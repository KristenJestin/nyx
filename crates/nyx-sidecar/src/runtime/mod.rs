//! The sidecar's RUNTIME — the nyx-core services the supervised sidecar owns and
//! drives, plus the adapters that bridge them to the [`crate::pipe`] transport.
//!
//! ## What moved here (Phase 2)
//!
//! In v1 the runtime/business logic was OWNED by the TS core-host (via the
//! `nyx-napi` binding): the `nyx-core` services (the SQLite pool, the interactive
//! PTY pump, the managed-command runner, the MCP HTTP server) were instantiated and
//! sequenced from Node. The frontier traits ([`nyx_core::frontier`]) were
//! implemented in TS/napi adapters.
//!
//! Phase 2 moves that ownership INTO this Rust sidecar. The sidecar is the runtime
//! authority now:
//!   * [`db`] — open + migrate the SQLite database at boot (PDR-0023; HHE). The
//!     embedded migrations apply on the pooled connection before any query serves.
//!   * [`pty`] — the interactive PTY pump, streaming raw binary buffers over the
//!     pipe with the lossless pause-gate flow control (HHC).
//!   * [`command`] — the managed-command runner (start/stop/relaunch/snapshot),
//!     busy-state, reprises (restart_on_startup, agent sessions) and the MCP HTTP
//!     server (HHD).
//!
//! The [`Runtime`] struct is the SERVICE / STATE CONTAINER (frontier 3): it holds
//! the long-lived `nyx-core` state the sidecar owns and hands the protocol dispatch
//! typed access. The TS `core-host` is no longer solicited for any of this.

pub mod busy_poll;
pub mod command;
pub mod db;
pub mod db_command;
pub mod events;
pub mod git_watch;
pub mod mcp;
pub mod pty;
pub mod recovery;
pub mod resume_parks;
pub mod snapshot_store;
pub mod stats_poll;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nyx_core::agent_activity::AgentActivityStore;
use nyx_core::command::CommandRunner;
use nyx_core::db::Db;

use crate::pipe::wire::{Event, Request, Response};
use command::{CommandRuntime, SidecarRunnerSink};
use events::EventSink;
use git_watch::GitWatchRuntime;
use mcp::McpRuntime;

/// The env flag that OPTS IN the boot-time Claude-plugin reconcile (`NYX_RECONCILE_CLAUDE_PLUGIN`).
/// Set to a non-empty, non-`0` value by the app that OWNS the real Claude integration (the
/// Electron main passes it when spawning the packaged/dev sidecar); UNSET everywhere else.
///
/// Why an explicit opt-in rather than "always on": the reconcile drives the REAL `claude plugin`
/// CLI against the user's REAL `~/.claude` registry (re-copying + re-registering the nyx-v2
/// marketplace). A bare sidecar test spawns with only `NYX_DATA_DIR` pointing at a TEMP dir — if
/// the reconcile fired there and the dev's machine has nyx-v2 registered, it would re-register the
/// marketplace at the temp path (deleted after the test), corrupting the user's real config. The
/// flag keeps every existing test hermetic (they never set it) while production turns it on. The
/// dedicated `integration_reconcile` test sets it AND the `NYX_CLAUDE_*` seams so it exercises the
/// path against a FAKE claude + temp dirs, touching nothing real.
const RECONCILE_PLUGIN_ENV: &str = "NYX_RECONCILE_CLAUDE_PLUGIN";

/// Whether the boot-time Claude-plugin reconcile is opted in (see [`RECONCILE_PLUGIN_ENV`]). A
/// value of `0`/empty reads as OFF so the flag can be unset by exporting `NYX_RECONCILE_CLAUDE_PLUGIN=0`.
fn reconcile_claude_plugin_enabled() -> bool {
    match std::env::var_os(RECONCILE_PLUGIN_ENV) {
        Some(v) => !v.is_empty() && v != "0",
        None => false,
    }
}

/// The long-lived runtime state the sidecar owns (frontier 3 — the service/state
/// container). Built ONCE at boot ([`Runtime::boot`]); cloned cheaply (every field is
/// an `Arc`/handle) so the pipe dispatch, the PTY pump threads and the MCP server all
/// share the SAME services.
#[derive(Clone)]
pub struct Runtime {
    /// The SHARED SQLite pool (WAL + foreign_keys + busy_timeout per connection;
    /// migrations applied at boot). The single connection authority for every reader:
    /// the pipe dispatch, the managed-command runner's persistence, and the MCP server
    /// all check connections out of THIS pool — no second authority.
    db: Arc<Db>,
    /// The live interactive-PTY manager (HHC): record-id → live shell, streaming raw
    /// binary output over the pipe with the lossless pause-gate.
    pty: Arc<pty::PtyManager>,
    /// The managed-command runtime (HHD), built once the pipe event sink exists
    /// ([`Runtime::attach_events`]). `None` before then.
    command: Arc<Mutex<Option<Arc<CommandRuntime>>>>,
    /// The MCP HTTP runtime (HHD), built with the command runtime.
    mcp: Arc<Mutex<Option<Arc<McpRuntime>>>>,
    /// The v2.4 real-time GIT-WATCH runtime (Q-015): the fs-watch + debounce engine that
    /// recomputes the derived Git views on a work-tree/ref change and PUSHES `repos://changed`
    /// over the pipe. Built once the pipe [`EventSink`] exists ([`Runtime::attach_events`]);
    /// `None` before then. The `repo_watch` / `repo_unwatch` db-commands (+ MCP tools)
    /// register/unregister a `(workspace, repository)` as its review surface opens/closes.
    git_watch: Arc<Mutex<Option<Arc<GitWatchRuntime>>>>,
    /// The shared runner cell the MCP dispatcher routes runtime command tools onto (the
    /// SAME cell the command runtime fills) — built before OR after the MCP server.
    runner_cell: Arc<Mutex<Option<Arc<CommandRunner<SidecarRunnerSink>>>>>,
    /// The SHARED runtime agent-activity store (the live "what is Claude doing" dot). The
    /// MCP per-turn hooks WRITE it; the pipe `agent_activity_snapshot` READS it and
    /// `agent_mark_ready_read` CLEARS it. Built once at boot, shared by value (`Arc`) with
    /// the MCP runtime so there is exactly ONE store. EMPTY at boot (the anti-phantom
    /// guarantee — never persisted).
    activity: Arc<AgentActivityStore>,
    /// The writable data dir this runtime persists into (`<data_dir>/nyx.db`,
    /// `integrations.json`, and the stable Claude-plugin install target). Resolved at boot
    /// (`NYX_DATA_DIR` override else the OS-conventional dir) so the `integration_*`
    /// db-commands resolve their paths — parity with the v1 `data_dir` the core-host passed.
    data_dir: PathBuf,
    /// The read-only bundled-resources dir (the bundled Claude-plugin SOURCE in a packaged
    /// build), or `None` in dev/bare — then the integrations core falls back to the source
    /// tree. From `NYX_RESOURCE_DIR` (the supervisor passes the unpacked resources path).
    resource_dir: Option<PathBuf>,
    /// The sidecar-owned crash-recovery state (Phase 4): the clean-shutdown marker and the
    /// idempotency-key journal for mutating ops. (The v2.5.5 crash `interrupted` set was
    /// reverted in v2.5.8 — a crash is treated as a clean shutdown, so there is nothing to
    /// flag.) Persisted to `<data_dir>/sidecar-recovery.json` (NOT the frozen v1 DB).
    /// Behind a `Mutex` so the boot reconcile, the mutating db-commands' idempotency guard,
    /// and the ordered shutdown all serialize their reads/writes through one cell.
    recovery: Arc<Mutex<recovery::RecoveryState>>,
    /// The sidecar-owned per-terminal VT SCREEN SNAPSHOT store (v2.2.5): the last serializable
    /// screen (main + alt) of each terminal, persisted to `<data_dir>/terminal-snapshots.json`
    /// on the ordered shutdown and read at boot. The `terminal_screen_rehydration` db-command
    /// serves its ANSI re-hydration bytes so a restarted TUI is repainted faithfully — not just
    /// its scrollback. Behind a `Mutex` so boot-load and shutdown-save serialize through one cell.
    /// NOT in the frozen v1 DB (same precedent as [`recovery::RecoveryState`]).
    snapshots: Arc<Mutex<snapshot_store::SnapshotStore>>,
    /// Latches `true` the first time the Claude-plugin boot reconcile runs (on the first
    /// `McpStart`, once the MCP server has actually bound its port). A `Runtime` is cloned
    /// cheaply and shared across the pipe dispatch, so this `Arc<AtomicBool>` guards against
    /// re-reconciling on every subsequent `McpStart` (start-once returns the same bound port,
    /// so a second reconcile would be redundant CLI churn). See [`Runtime::start_mcp`].
    plugin_reconciled: Arc<AtomicBool>,
    /// The LAZY agent-session resume parks (v2.5.5): `claude --resume <id>` commands the renderer
    /// pushed from the `workspace_restore_plan` read model, injected ONCE into a terminal's shell
    /// at its PTY spawn (or immediately if already live). In-memory, per app run — a sidecar
    /// restart re-evaluates the restore plan afresh. See [`Runtime::park_resumes`] /
    /// [`Runtime::inject_resume_on_spawn`].
    resume_parks: Arc<resume_parks::ResumeParks>,
    /// The OS BUSY-STATE POLL (v2.5.5 review T1): the periodic `Pty::is_busy` sweep that pushes
    /// `terminal://busy-state` transitions so a foreground command lights the runtime tree's
    /// `running` dot (the v1 `BusyStatePoller` port). Started in [`Runtime::attach_events`] once
    /// the pipe [`EventSink`] exists; `None` before then (a bare test build spawns no poll).
    busy_poll: Arc<Mutex<Option<Arc<busy_poll::BusyPoll>>>>,
    /// The per-terminal CPU%/RAM STATS POLL (FEEDBACK #28 / v2.5.6): the periodic process-tree
    /// sweep that pushes `terminal://stats` readings so the runtime tree can show a terminal's
    /// live CPU%/RAM on hover (NON-inline — never masking the name). The missing producer for the
    /// already-ported [`nyx_core::proc_stats`]. Started in [`Runtime::attach_events`] once the
    /// pipe [`EventSink`] exists; `None` before then (a bare test build spawns no poll).
    stats_poll: Arc<Mutex<Option<Arc<stats_poll::StatsPoll>>>>,
}

impl Runtime {
    /// Boot the runtime: resolve the data dir, open + migrate the SQLite database, and
    /// build the live PTY manager. Returns once the core is ready to serve.
    ///
    /// This is the sidecar's slice of the [`nyx_core::frontier::Lifecycle::boot`]
    /// sequence: "open DB + migrate" runs HERE (HHE), in the Rust sidecar, instead of
    /// the TS core-host's `NyxCore::open`. A failure (bad path / migration error)
    /// propagates so the sidecar refuses to start rather than serving a broken schema.
    pub fn boot() -> anyhow::Result<Self> {
        let data_dir = db::resolve_data_dir()?;
        // HARD GUARD (review #38): BEFORE opening/migrating SQLite, refuse a data dir that
        // belongs to another nyx version (a v1 `nyx.db` with no `nyx-v2` ownership marker).
        // A fresh dir is claimed (marker written) and proceeds; a foreign dir fails loudly so
        // we can NEVER accidentally migrate/clobber a live v1 DB.
        db::guard_data_dir_ownership(&data_dir)?;
        let db = db::open_database(&data_dir)?;
        // The read-only bundled-resources dir the supervisor passes (the unpacked,
        // outside-asar resources in a packaged build); absent in dev — the integrations
        // core then falls back to the source tree.
        let resource_dir = std::env::var_os("NYX_RESOURCE_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        Ok(Self::with_db_and_paths(
            Arc::new(db),
            data_dir,
            resource_dir,
        ))
    }

    /// Build a runtime around an already-open [`Db`] (used by [`Runtime::boot`] and by
    /// the integration tests, which open an isolated DB under a temp dir). The data dir
    /// defaults to the DB's parent and there is no resource dir (the tests don't exercise
    /// the integration install paths).
    pub fn with_db(db: Arc<Db>) -> Self {
        Self::with_db_and_paths(db, PathBuf::new(), None)
    }

    /// Build a runtime around an already-open [`Db`] plus the resolved data/resource dirs
    /// the `integration_*` db-commands need.
    pub fn with_db_and_paths(
        db: Arc<Db>,
        data_dir: PathBuf,
        resource_dir: Option<PathBuf>,
    ) -> Self {
        // The PTY manager shares the pool: a terminal with NO renderer view has its scrollback
        // persisted by the sidecar itself, into the SAME record `read_terminal` reads.
        let pty = Arc::new(pty::PtyManager::new(Arc::clone(&db)));
        Self {
            db,
            pty,
            command: Arc::new(Mutex::new(None)),
            mcp: Arc::new(Mutex::new(None)),
            git_watch: Arc::new(Mutex::new(None)),
            runner_cell: Arc::new(Mutex::new(None)),
            activity: Arc::new(AgentActivityStore::new()),
            data_dir,
            resource_dir,
            recovery: Arc::new(Mutex::new(recovery::RecoveryState::default())),
            snapshots: Arc::new(Mutex::new(snapshot_store::SnapshotStore::default())),
            plugin_reconciled: Arc::new(AtomicBool::new(false)),
            resume_parks: Arc::new(resume_parks::ResumeParks::new()),
            busy_poll: Arc::new(Mutex::new(None)),
            stats_poll: Arc::new(Mutex::new(None)),
        }
    }

    /// Attach the pipe event sink and finish the event-driven boot (HHD): build the
    /// managed-command runtime + the MCP runtime over the shared pool + the sink, then
    /// run the BOOT RESTORE (reprises) — relaunch the instances whose template
    /// `restart_on_startup` is ON and that were running at the last shutdown.
    ///
    /// Called ONCE from the binary's run loop, after the shared `stdout` sink exists.
    /// This is the rest of the [`nyx_core::frontier::Lifecycle::boot`] sequence the
    /// sidecar now owns: restore commands, then (lazily, on the McpStart request) start
    /// the MCP server.
    pub fn attach_events(&self, events: &EventSink) {
        let command = Arc::new(CommandRuntime::new(Arc::clone(&self.db), events.clone()));
        // Share the runner so the MCP dispatcher routes runtime command tools onto it.
        *self.runner_cell.lock().unwrap() = Some(command.runner());
        let mcp = Arc::new(McpRuntime::new(
            Arc::clone(&self.db),
            Arc::clone(&self.runner_cell),
            Arc::clone(&self.pty),
            // Share the SAME activity store with the MCP runtime so the live dot the MCP
            // per-turn hooks write is the one the pipe `agent_activity_snapshot` reads.
            Arc::clone(&self.activity),
            events.clone(),
            // Share the git-watch CELL (filled just below) so the MCP `repo_watch` /
            // `repo_unwatch` tools drive the SAME watcher the UI db-commands do. The
            // dispatcher reads the cell lazily on each call, so filling it after this is fine.
            Arc::clone(&self.git_watch),
        ));
        *self.command.lock().unwrap() = Some(Arc::clone(&command));
        *self.mcp.lock().unwrap() = Some(mcp);

        // THE APPROVAL PUSH, installed at its SOURCE (PDR-0009). A protected MCP call now blocks on
        // the human for up to forty-five seconds, so the decision surface has to learn about the
        // request the instant the ledger writes it — not when the call it belongs to finally
        // returns, which is precisely too late to be decided. The ledger owns that instant; this is
        // the sink it pushes through, and it is the SAME `system-approvals` topic the UI's
        // grant/deny routes emit, so one topic drives the surface whichever side moved it.
        {
            let events = events.clone();
            nyx_core::system_approvals::set_pending_notifier(move || {
                events.emit(Event::Changed {
                    topic: nyx_core::system_approvals::SYSTEM_APPROVALS_CHANGED_TOPIC.to_string(),
                });
            });
        }

        // Start the v2.4 real-time GIT-WATCH runtime (Q-015) over the SAME pipe `events`
        // sink the domain events ride: an fs-watch + debounce engine that recomputes the
        // derived Git views on a work-tree/ref change and pushes `repos://changed`. No repo
        // is watched until a review surface registers one via `repo_watch` — so a bare boot
        // starts zero watches (nothing to thrash on).
        *self.git_watch.lock().unwrap() = Some(GitWatchRuntime::start(events.clone()));

        // Start the OS BUSY-STATE POLL (v2.5.5 review T1): the periodic `Pty::is_busy` sweep over
        // every record-backed live PTY, pushing `terminal://busy-state` on each foreground
        // transition so the runtime tree lights a `running` dot for ANY foreground command (a
        // build, a `sleep`, a hand-launched agent) WITHOUT a hook — the v1 `BusyStatePoller` port
        // re-câbled onto the sidecar pipe. Independent of OSC 133; a restored idle terminal
        // samples idle by construction (no phantom running).
        *self.busy_poll.lock().unwrap() = Some(busy_poll::BusyPoll::start(
            Arc::clone(&self.pty),
            events.clone(),
        ));

        // Start the per-terminal CPU%/RAM STATS POLL (FEEDBACK #28 / v2.5.6): the periodic process-
        // tree sweep (`ProcStats::tree_stats_batch` over each record-backed shell pid) that pushes
        // `terminal://stats` readings — the MISSING producer for the already-ported
        // `nyx_core::proc_stats`. The renderer shows the reading on hover of a terminal LINE
        // (NON-inline — the CPU%/RAM never masks the terminal name). Independent of the busy poll:
        // that diffs a boolean transition, this streams a fresh reading each tick (CPU% is
        // continuous), over its OWN reused `ProcStats` so per-process CPU% deltas stay meaningful.
        *self.stats_poll.lock().unwrap() = Some(stats_poll::StatsPoll::start(
            Arc::clone(&self.pty),
            events.clone(),
        ));

        // BOOT RECONCILE (Phase 4 — `01KWCJPFP9JSJMRDZGW2E0N905`; crash-UI reverted in v2.5.8):
        // BEFORE the boot restore, re-arm the clean-shutdown marker to false for the run we are
        // starting. A crash is now treated exactly like a CLEAN shutdown — the reconcile flags
        // NOTHING interrupted and flips NO session to `unknown`; the normal SILENT resume path
        // (lazy agent-session resume, gated by the project toggle + the process-liveness gate) takes
        // over. `was_crash` is returned only so the boot can log that the previous run crashed.
        let outcome = recovery::reconcile_after_crash(&self.data_dir);
        // Reload the (re-armed) recovery state into the runtime so the idempotency journal is the
        // loaded one.
        {
            let mut guard = self.recovery.lock().unwrap();
            *guard = recovery::RecoveryState::load(&self.data_dir);
        }
        // Load the per-terminal VT screen snapshots persisted at the last shutdown so the
        // `terminal_screen_rehydration` db-command can hand the renderer each restarted
        // terminal's re-hydration bytes (a TUI's alt-screen included, not just scrollback).
        {
            let mut guard = self.snapshots.lock().unwrap();
            *guard = snapshot_store::SnapshotStore::load(&self.data_dir);
            if !guard.is_empty() {
                events.log(
                    "info",
                    format!(
                        "loaded {} terminal screen snapshot(s) for restart re-hydration",
                        guard.len()
                    ),
                );
            }
        }
        if outcome.was_crash {
            events.log(
                "info",
                "recovery: previous run did not shut down cleanly — treating it as a clean shutdown (silent resume via the normal toggle-gated path; no interrupted UI)".to_string(),
            );
        }

        // BOOT RESTORE (reprises): relaunch the commands that were running at the last
        // CLEAN shutdown and whose template opts into restart_on_startup, and normalize any
        // orphaned persisted-running. After a crash the reconcile above cleared the
        // snapshots, so this relaunches NOTHING blindly.
        let restored = command.restore_on_boot();
        if !restored.is_empty() {
            events.log(
                "info",
                format!("restored {} managed command(s) on boot", restored.len()),
            );
        }
    }

    /// Start the MCP HTTP server (idempotent — start-once) and, on the FIRST successful
    /// start of this process, RECONCILE the bundled Claude plugin against the port the server
    /// actually bound. Returns that bound port (0 on a bind failure).
    ///
    /// This is the missing wiring the bug report describes. Production reaches it exactly once
    /// at boot: the Electron main calls `sidecar.start()` then `sidecar.startMcp()`
    /// (`apps/desktop/src/main/index.ts`), which sends `McpStart`. The plugin's bundled
    /// `.mcp.json` declares `http://127.0.0.1:<port>/mcp`, so for Claude Code to CONNECT that
    /// MCP the port written into the INSTALLED `.mcp.json` must be the port nyx is truly
    /// listening on — which is only known once `McpServer::start` has bound. So we bind FIRST,
    /// then reconcile with the bound port. Doing the reconcile HERE (rather than in
    /// `attach_events`) also means it fires only when the MCP is actually started — never in a
    /// bare test harness that opens no MCP surface, keeping those tests hermetic (no stray
    /// shell-out to the real `claude`).
    ///
    /// The reconcile runs at most ONCE per process (the `plugin_reconciled` latch): a repeated
    /// `McpStart` returns the same start-once port and must not re-drive the CLI.
    fn start_mcp(&self) -> u16 {
        let Some(mcp) = self.mcp() else {
            return 0;
        };
        let port = mcp.start();
        // Only reconcile once, and only when the server actually bound a live port.
        if port != 0 && !self.plugin_reconciled.swap(true, Ordering::SeqCst) {
            eprintln!("[nyx-sidecar] MCP HTTP server listening on 127.0.0.1:{port}/mcp");
            if reconcile_claude_plugin_enabled() {
                self.reconcile_claude_plugin(port);
            }
        }
        port
    }

    /// RECONCILE the ONE bundled Claude Code plugin, keyed on the port the MCP server actually
    /// bound (finding #44/#45/#47). This is the wiring the design intended ("Called from
    /// lib.rs after the MCP server is bound") but that was never hooked up in the
    /// Tauri→Electron move. Invoked once from [`Runtime::start_mcp`].
    ///
    /// It resolves the plugin descriptor the SAME way the Settings → Integrations install
    /// does ([`nyx_core::agent::ClaudeCodeAdapter::plugin_install`] over the runtime's
    /// `resource_dir` SOURCE + `data_dir` as the app-data dir → the stable
    /// `<data_dir>/claude-plugin` install dir + the `claude` CLI driver), then delegates to
    /// [`nyx_core::onboarding::reconcile_installed_providers`]. Because it reuses the EXACT
    /// same descriptor builder as install, the stable dir it re-templates is the SAME dir the
    /// marketplace was registered at (`known_marketplaces.json` → `<data_dir>/claude-plugin`),
    /// so the re-template actually takes effect.
    ///
    /// CRUCIAL: the reconcile is given the REAL bound MCP port (not the default guess), so the
    /// `__NYX_MCP_PORT__` placeholder in the installed `.mcp.json` is replaced with the port
    /// nyx is truly listening on. A `mcp_port` of 0 (the MCP server failed to bind) skips the
    /// reconcile — re-templating a `.mcp.json` to a dead port would only entrench the broken
    /// state.
    ///
    /// Conservative + best-effort: a plugin the user removed in Claude Code is a no-op (never
    /// re-installed at boot); an unresolvable descriptor / CLI or any CLI error is logged and
    /// skipped, never a boot failure.
    fn reconcile_claude_plugin(&self, mcp_port: u16) {
        use nyx_core::agent::AgentAdapter;

        if mcp_port == 0 {
            eprintln!(
                "[nyx-sidecar] skipping Claude plugin reconcile: MCP server is not bound (port 0)"
            );
            return;
        }

        // The descriptor carries the LIVE bound port so the copied `.mcp.json` is templated to
        // the port nyx actually listens on — NOT `mcp::resolve_port()`'s guess (they normally
        // match, but the bound port is the source of truth after `McpServer::start`).
        let resource_dir = self.resource_dir.as_deref();
        let app_data_dir = self.data_dir.clone();
        let state_path = nyx_core::integrations::state_path_in(&self.data_dir);

        // The per-provider resolver `reconcile_installed_providers` calls for each provider key.
        // Only `claude_code` resolves to a descriptor; every other key → `None` (skipped). The
        // descriptor's `mcp_port` is overridden to the REAL bound port so the re-template is
        // correct even if `NYX_MCP_PORT` was unset and a non-default port was chosen at bind.
        let resolve_plugin = |key: &str| -> nyx_core::onboarding::PluginReconcileEntry {
            if key != "claude_code" {
                return None;
            }
            let mut install = nyx_core::agent::ClaudeCodeAdapter
                .plugin_install(resource_dir, Some(&app_data_dir))?;
            install.mcp_port = mcp_port;
            let cli = nyx_core::agent::ClaudeCodeAdapter.plugin_cli()?;
            Some((install, cli))
        };

        nyx_core::onboarding::reconcile_installed_providers(mcp_port, &state_path, resolve_plugin);
    }

    /// The shared SQLite pool. The runtime's single connection authority.
    pub fn db(&self) -> &Arc<Db> {
        &self.db
    }

    /// The live interactive-PTY manager.
    pub fn pty(&self) -> &Arc<pty::PtyManager> {
        &self.pty
    }

    /// PARK a batch of LAZY agent-session resumes (v2.5.5): the renderer pushes each resumable
    /// session's `claude --resume <id>` (computed by the `workspace_restore_plan` read model,
    /// which REUSES `agent_resume::decide_resume`) keyed by its terminal RECORD id. Each park is
    /// stored one-shot; a resume whose terminal PTY is ALREADY live is injected IMMEDIATELY (the
    /// deck eager-mounts every project terminal, so at relaunch the spawn usually precedes the
    /// plan), otherwise it waits for [`inject_resume_on_spawn`](Runtime::inject_resume_on_spawn).
    /// The `attempted` guard inside [`resume_parks::ResumeParks`] makes a re-pushed plan idempotent.
    ///
    /// FILTER — an already-LOADED terminal is never offered a resume (PRD
    /// `01KY9PM1GK30FPVZVWQ3A7ZMJH`): a record whose PTY ALREADY hosts a live agent process is
    /// SKIPPED outright — not parked, not injected. This is the belt-and-suspenders on top of the
    /// [`inject_resume`](Runtime::inject_resume) liveness gate: it stops a stale `claude --resume
    /// <external_id>` (a session id from a PREVIOUS run) from ever sitting parked against a terminal
    /// that is already running its agent — so it can never be injected into that live agent, even by
    /// a later drain. Only a terminal with NO live agent (a bare/relaunch shell, or an unloaded one
    /// whose PTY is closed) is parked/offered. Keyed on the real process tree, so a live-idle Claude
    /// (Idle BETWEEN turns) is filtered exactly like a Working one.
    ///
    /// WHEN THIS RUNS — the answer to "why is there no resume in my BOOT log". Never at boot: the
    /// resume is LAZY. Nothing is parked until the RENDERER pushes a plan, and it pushes one only
    /// when its `use-lazy-resume` hook is mounted — i.e. when the Terminals page is up AND a project
    /// with workspaces is entered (see `apps/desktop/renderer/src/components/terminals/
    /// use-lazy-resume.ts`, which states the two conditions). A startup that shows no
    /// `[nyx-sidecar] resume …` line is therefore not evidence of a broken resume; it is a log taken
    /// before the resume was asked for.
    ///
    /// WHAT IT SAYS — the refusal below is a DECISION, not an absence, so it is written down HERE,
    /// where it is taken. It used to be a bare `continue`: a resume protected by the #28 gate and a
    /// resume that never existed produced the same empty log, which is precisely what makes every
    /// failed relaunch look like the previous one.
    pub fn park_resumes(&self, parks: &[(String, String, String)], events: &EventSink) {
        let registry = nyx_core::agent::AgentRegistry::default();
        for (record_id, session_id, command) in parks {
            // Skip a terminal that is ALREADY loaded (its PTY hosts a live agent process): offering
            // it a resume would only risk typing a stale session id into the running agent.
            if self.pty.agent_alive_in_record(record_id, &registry) {
                eprintln!(
                    "[nyx-sidecar] resume NOT parked for terminal {record_id} (session \
                     {session_id}): a live agent process already occupies it — the #28 gate \
                     protects the running session. This is a REFUSAL, not a failure."
                );
                continue;
            }
            self.resume_parks.park(
                record_id,
                resume_parks::ResumePark {
                    session_id: session_id.clone(),
                    command: command.clone(),
                },
            );
            // If the terminal is already live (spawned before the plan arrived) but hosts NO agent
            // yet — a fresh shell awaiting its `claude --resume` — inject now: the shell prompt is
            // ready by this point, so the resume line runs cleanly (the inject re-checks liveness).
            if self.pty.is_live_record(record_id) {
                self.inject_resume(record_id, events);
            }
        }
    }

    /// Drain and inject the parked resume for a terminal AT ITS PTY SPAWN (v2.5.5): the front
    /// mounted/opened the terminal and its PTY is now live, so a parked `claude --resume <id>` is
    /// written into the fresh shell EXACTLY ONCE. A no-op when nothing is parked for this record
    /// (a bare terminal, or the plan has not arrived yet — then the `park_resumes` immediate path
    /// covers it once it does).
    pub fn inject_resume_on_spawn(&self, record_id: &str, events: &EventSink) {
        if record_id.is_empty() {
            return;
        }
        self.inject_resume(record_id, events);
    }

    /// Take the pending resume for `record_id` (one-shot) and INJECT it into the live shell — the
    /// SAME write path as a keystroke, `command` + a trailing carriage return (`\r`) so the shell
    /// executes it and stays interactive (parity with the v1 injection). A no-op when nothing is
    /// pending. A failed write marks the session `resume_failed` (no retry loop) and nudges the
    /// renderer to re-pull the agent sessions so the clean `resume-failed` state surfaces.
    fn inject_resume(&self, record_id: &str, events: &EventSink) {
        // NOTHING PARKED → nothing to gate. This runs BEFORE the liveness probe because that probe
        // is a FULL process-table scan (hundreds of `/proc` reads), while this is one hash lookup:
        // `inject_resume_on_spawn` fires on EVERY PtySpawn (`dispatch_runtime`), and opening a
        // project eager-mounts every terminal — so without this the sidecar's request loop paid one
        // whole scan per bare terminal for a drain that would have found nothing. (Same reasoning as
        // `proc_stats::tree_stats_batch`'s one-scan-per-tick fix.)
        if !self.resume_parks.has_pending(record_id) {
            return;
        }
        // GATE ON REAL PROCESS LIVENESS (PRD `01KY9PM1GK30FPVZVWQ3A7ZMJH`) — the authoritative
        // check that holds AT THE MOMENT OF INJECT. A parked `claude --resume <id>` must NEVER be
        // typed into a terminal that ALREADY hosts a live agent process: doing so lands the resume
        // line as keystrokes inside the running agent (the dogfood bug — "resume relancé sur une
        // session vivante"), often with a STALE external session id from a previous run.
        //
        // This REPLACES the old `agent_activity`-only gate (which skipped only `Working`/`Waiting`).
        // That proxy had a fatal hole: an `Idle` activity means EITHER a live agent waiting between
        // two turns OR a crashed/exited agent whose last hook was a `TurnFinished` — indistinguishable
        // from the activity alone, so a live-idle Claude slipped through and got the resume typed into
        // it. The reliable discriminator is whether an agent PROCESS is alive in the terminal's PTY
        // tree, which the sidecar can read because it OWNS the PTY (see
        // [`pty::PtyManager::agent_alive_in_record`] / [`nyx_core::agent_liveness`]): a live agent in
        // ANY state (Working, Waiting OR Idle-between-turns) reads `true` here and is SKIPPED, while a
        // crashed/absent agent (the stale-`Idle` case the old gate mis-injected) — and a genuinely
        // bare terminal (a fresh relaunch shell, or a revisit after an idle-unload closed the PTY) —
        // reads `false` and IS injected. On a skip we DELIBERATELY leave the park PENDING and
        // un-attempted (we do NOT `take_for_inject`), so a later bare spawn can still drain it — the
        // resume is deferred, never lost.
        if self
            .pty
            .agent_alive_in_record(record_id, &nyx_core::agent::AgentRegistry::default())
        {
            // Same REFUSAL as in `park_resumes`, taken at the other moment (the park was made
            // against a bare terminal that has since become occupied). Said here for the same
            // reason: a deferred resume and a lost one are indistinguishable from silence.
            eprintln!(
                "[nyx-sidecar] resume HELD BACK for terminal {record_id}: a live agent process \
                 occupies it — the #28 gate protects the running session. The park stays pending \
                 for a later bare spawn; nothing is lost."
            );
            return;
        }
        let Some(park) = self.resume_parks.take_for_inject(record_id) else {
            return;
        };
        let session_id = park.session_id.clone();
        let mut bytes = park.command.into_bytes();
        bytes.push(b'\r');
        // `write_to_record` returns false when no live PTY backs the record OR the write errored.
        // We only reach here for a live record (spawn just registered it, or `park_resumes`
        // checked liveness), so a false is a genuine injection failure → mark the session failed.
        if !self.pty.write_to_record(record_id, &bytes) {
            // A genuine FAILURE — the third fact, distinct from "nothing to resume" (which the
            // renderer reports over its own plan) and from the refusals above.
            eprintln!(
                "[nyx-sidecar] resume INJECTION FAILED for terminal {record_id} (session \
                 {session_id}): the PTY write did not go through; the session is marked \
                 resume_failed and is not retried automatically."
            );
            let _ = self
                .db
                .with_conn(|c| nyx_core::db::mark_session_resume_failed(c, &session_id));
            events.emit(crate::pipe::wire::Event::Changed {
                topic: "agent-sessions".to_string(),
            });
            return;
        }
        eprintln!(
            "[nyx-sidecar] resume INJECTED into terminal {record_id} (session {session_id})."
        );
    }

    /// UNLOAD the idle agent sessions (v2.5.5 — task `01KX8Z6T1J2AC65DHHZTCQ26Z4`): close the PTY
    /// of every agent-hosting terminal that is safe to unload, freeing its resources / dropping it
    /// out of the TUI, and RETURN the record ids unloaded. The renderer drives this on a timer,
    /// passing the currently-`visited` (focused) terminal record ids and an inactivity `threshold`.
    ///
    /// A terminal is unloaded ONLY when BOTH gates pass:
    ///   1. [`nyx_core::agent_unload::decide_unload`] says so — its runtime activity is `Idle`
    ///      (NEVER `Working`/`Waiting` — the central "une session ACTIVE n'est JAMAIS unloadée"
    ///      invariant AND the "action en cours" exception, both enforced by the activity gate since
    ///      a long in-flight action keeps `Working` 0-stale), it is NOT visited, and it has had NO
    ///      PTY I/O for at least `threshold`; AND
    ///   2. the session is RESUME-ELIGIBLE — the SAME `agent_resume::decide_resume` policy the
    ///      restore plan uses would resume it (the project toggle is ON, the session is a live
    ///      candidate with a transcript on disk, the shell/adapter can host the resume). This is the
    ///      "toggle = interrupteur maître" decision extended to unload (review
    ///      `01KX9CP96KVK6BTHY3ZSWY1E67`, finding 2): a session we could NOT bring back on revisit
    ///      (e.g. a `resume_agent_sessions`-OFF project, the default) is NEVER unloaded — it stays
    ///      loaded rather than being killed without recourse.
    ///
    /// Only ACTIVE/UNKNOWN agent sessions on `alive` terminals are candidates (a bare shell is not a
    /// resumable session), read from `resume_candidates_on_boot` (which also carries the toggle +
    /// transcript + external id gate 2 needs).
    ///
    /// The unload does NOT end the DB session (it stays `active`, exactly like a session left after
    /// an app kill), so a revisit RESUMES it through the SAME phase-2 lazy path: closing the PTY +
    /// [`resume_parks::ResumeParks::rearm`] RE-PARKS the exact `claude --resume <id>` (rebuilt HERE
    /// via `decide_resume`), so on the terminal's next spawn — an IN-RUN revisit — the resume is
    /// re-injected automatically, WITHOUT depending on the renderer re-pushing the plan (it pushes a
    /// workspace's plan at most once per run — review finding 1). The runtime activity entry is
    /// cleared (the anti-phantom reflex on unload).
    pub fn unload_idle_sessions(
        &self,
        threshold: Duration,
        visited: &HashSet<String>,
        events: &EventSink,
    ) -> Vec<String> {
        // Only ACTIVE/UNKNOWN agent sessions on `alive` terminals are unload candidates — a plain
        // shell with no session is not a resumable thing to unload. Read them via
        // `resume_candidates_on_boot` (keyed by record id), which ALSO carries the resume-eligibility
        // inputs (project toggle, transcript path, external id, agent kind) gate 2 below needs — so
        // the unload never kills a session it could not bring back.
        let candidates: std::collections::HashMap<String, nyx_core::db::ResumeCandidate> = self
            .db
            .with_conn(nyx_core::db::resume_candidates_on_boot)
            .map(|rows| {
                rows.into_iter()
                    .map(|c| (c.terminal_id.clone(), c))
                    .collect()
            })
            .unwrap_or_default();

        // Resolve the run-constant resume inputs ONCE: the kind→adapter registry (the single source
        // of truth) and the target shell classification. Reused per candidate by `decide_resume`.
        let registry = nyx_core::agent::AgentRegistry::default();
        let target =
            nyx_core::agent_resume::ResumeTarget::classify_shell(&nyx_core::pty::resolve_shell());

        let mut unloaded = Vec::new();
        for (record_id, idle_for) in self.pty.agent_idle_durations() {
            let Some(candidate) = candidates.get(&record_id) else {
                continue; // a live terminal that does not host an active agent session — skip.
            };
            // GATE 1 — safety/focus/threshold. The authoritative live activity (missing entry ⇒
            // Idle — the anti-phantom default). Never unload Working/Waiting, the visited terminal,
            // or one still within the grace window.
            let activity = self
                .activity
                .snapshot(&record_id)
                .map(|s| s.activity)
                .unwrap_or(nyx_core::agent_activity::Activity::Idle);
            let unload_candidate = nyx_core::agent_unload::UnloadCandidate {
                record_id: record_id.clone(),
                activity,
                visited: visited.contains(&record_id),
                idle_for,
            };
            if !nyx_core::agent_unload::decide_unload(&unload_candidate, threshold).is_unload() {
                continue;
            }
            // GATE 2 — resume-eligibility (master switch). Only unload a session the SAME
            // `decide_resume` policy would resume on revisit; else keep it loaded (never kill
            // without recourse — a toggle-OFF project is the common case).
            let Some(command) = resume_command_for_candidate(candidate, target, &registry) else {
                continue;
            };
            // GATE 1b — RE-CHECK the LIVE activity IMMEDIATELY before the irreversible close
            // (finding #28, the idle-unload ↔ reactivation race). Gate 1's `decide_unload` ran on a
            // snapshot taken BEFORE gate 2's command build — and gate 2 does real work (an FS `stat`
            // of the transcript, an adapter lookup), so a concurrent MCP hook thread (a new
            // `UserPromptSubmit`, a tool/sub-agent starting) could have flipped the session to
            // `Working`/`Waiting` in that window. Re-read the activity as tightly as possible here so
            // a session that reactivated mid-sweep is SPARED rather than killed — the "une session
            // qui travaille n'est JAMAIS déchargée" invariant holds even under a race, not just for
            // an in-order snapshot.
            let still_idle = self
                .activity
                .snapshot(&record_id)
                .map(|s| s.activity)
                .unwrap_or(nyx_core::agent_activity::Activity::Idle)
                == nyx_core::agent_activity::Activity::Idle;
            if !still_idle {
                continue;
            }
            // Free the resources / take it out of the TUI. The DB session stays `active`.
            self.pty.close_record(&record_id);
            // Anti-phantom reflex: drop the (Idle) runtime activity entry for the gone PTY.
            self.activity.clear(&record_id);
            // RE-PARK the exact resume so an IN-RUN revisit re-injects `claude --resume <id>` via
            // the phase-2 spawn-drain — no dependence on the renderer re-pushing the plan.
            self.resume_parks.rearm(
                &record_id,
                resume_parks::ResumePark {
                    session_id: candidate.session_id.clone(),
                    command,
                },
            );
            unloaded.push(record_id);
        }
        if !unloaded.is_empty() {
            // Nudge the renderer to re-pull the agent sessions / terminals so the unloaded ones
            // drop out of the deck (and can be re-mounted → resumed on revisit).
            events.emit(crate::pipe::wire::Event::Changed {
                topic: "agent-sessions".to_string(),
            });
        }
        unloaded
    }

    /// The managed-command runtime (once [`attach_events`](Runtime::attach_events) ran).
    pub fn command(&self) -> Option<Arc<CommandRuntime>> {
        self.command.lock().unwrap().clone()
    }

    /// The MCP runtime (once [`attach_events`](Runtime::attach_events) ran).
    pub fn mcp(&self) -> Option<Arc<McpRuntime>> {
        self.mcp.lock().unwrap().clone()
    }

    /// The v2.4 real-time git-watch runtime (once [`attach_events`](Runtime::attach_events)
    /// ran). The `repo_watch` / `repo_unwatch` db-commands register a `(workspace,
    /// repository)` with it as a review surface opens/closes.
    pub fn git_watch(&self) -> Option<Arc<GitWatchRuntime>> {
        self.git_watch.lock().unwrap().clone()
    }

    /// The SHARED runtime agent-activity store (the live dot the MCP hooks write + the
    /// pipe `agent_activity_snapshot` reads / `agent_mark_ready_read` clears).
    pub fn activity(&self) -> &Arc<AgentActivityStore> {
        &self.activity
    }

    /// The shared managed-command runner cell — the SAME `Arc` the command runtime fills
    /// (used by the template-mutation db-commands' running-guard).
    pub fn runner_cell(&self) -> &Arc<Mutex<Option<Arc<CommandRunner<SidecarRunnerSink>>>>> {
        &self.runner_cell
    }

    /// The writable data dir (for the `integration_*` db-commands).
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// The read-only bundled-resources dir, if any.
    pub fn resource_dir(&self) -> Option<&std::path::Path> {
        self.resource_dir.as_deref()
    }

    /// Ordered SHUTDOWN (reprises + reaping): snapshot which commands are running so the
    /// next boot relaunches exactly those, then tree-kill every live managed command and
    /// every interactive PTY so nothing is orphaned past exit. Idempotent / latched.
    ///
    /// Phase 4: this is the ONLY path that marks the recovery file `clean_shutdown = true`.
    /// A crash never runs `shutdown`, so the next boot reads it still `false` and logs that the
    /// previous run crashed (handled like a clean shutdown since v2.5.8). We persist the clean
    /// marker WHILE keeping the idempotency journal.
    pub fn shutdown(&self) {
        // R1 — STOP marking sessions `ended` for the rest of this shutdown, FIRST of all (before
        // mcp.stop + close_all): the dying Claude PTYs fire `SessionEnd(prompt_input_exit)` that
        // reaches the handler even after the listener is torn down, and marking `ended` there kills
        // the next-boot resume. Armed here, that late SessionEnd leaves the row `active` instead.
        nyx_core::mcp_tools_core::suppress_session_end_marking();
        // STOP THE MCP HTTP LISTENER FIRST — BEFORE any PTY is closed (`self.pty.close_all()`
        // below). Closing the interactive Claude PTYs makes each dying Claude fire a
        // `SessionEnd(reason=other)` hook that POSTs `agent_session_event` to our MCP server. If
        // the server were still listening (as it was until this fix — it only died when the
        // process exited), that POST would `mark_session_ended` and the session would NEVER be
        // resumed. Tearing the listener down here means the late POST hits a refused socket
        // (parity with v1, where the in-process MCP died WITH the host, and with the crash reaper,
        // which SIGKILLs Claude+MCP together): the killed session stays `active` and lazy-resume
        // reclaims it next boot. A Claude ended INTENTIONALLY while the app runs (logout /
        // prompt_input_exit) still reaches the LIVE server before this shutdown, so it stays
        // `ended` (v1 parity, untouched). Bounded (unblock + join), idempotent.
        if let Some(mcp) = self.mcp() {
            mcp.stop();
        }
        if let Some(command) = self.command() {
            if command.begin_shutdown() {
                command.snapshot_on_shutdown();
                command.kill_all_running();
            }
        }
        // Persist every live terminal's current VT screen (main + alt) BEFORE killing the PTYs,
        // so the next boot can re-hydrate each terminal faithfully — a full-screen TUI included,
        // not just its scrollback. Keyed by the persistent record id; record-less terminals are
        // omitted (they cannot be correlated back at restart).
        {
            let live = self.pty.live_snapshots_by_record();
            let mut store = self.snapshots.lock().unwrap();
            store.save_live(&self.data_dir, live);
        }
        self.pty.close_all();
        // Mark this an ORDERED (clean) shutdown so the next boot does NOT run the crash
        // reconcile. Best-effort persistence (the file is an optimization over the DB).
        {
            let mut guard = self.recovery.lock().unwrap();
            guard.clean_shutdown = true;
            guard.save(&self.data_dir);
        }
    }

    /// TEST-ONLY: load the persisted terminal-snapshot store from `data_dir` into this runtime,
    /// mirroring what [`attach_events`](Runtime::attach_events) does at boot. Tests build a
    /// runtime with [`with_db_and_paths`](Runtime::with_db_and_paths) (no full event-driven
    /// boot), so this is the seam that populates the store for a restart-re-hydration assertion.
    #[cfg(test)]
    pub fn load_snapshots_for_test(&self, data_dir: &std::path::Path) {
        *self.snapshots.lock().unwrap() = snapshot_store::SnapshotStore::load(data_dir);
    }

    /// The ANSI re-hydration byte stream for a terminal RECORD id, for restart re-hydration
    /// (v2.2.5). Prefers a LIVE terminal's current screen (still-running after a hot reload of
    /// the renderer) and falls back to the snapshot persisted at the last shutdown. `None` when
    /// neither exists. The renderer writes these bytes verbatim into a fresh wterm/ghostty
    /// terminal; ghostty reconstructs the exact grid (alt-screen included). Reading does not
    /// consume the snapshot, and the byte stream is self-resetting, so re-hydration is idempotent.
    pub fn terminal_rehydration_bytes(&self, record_id: &str) -> Option<Vec<u8>> {
        if let Some(live) = self.pty.snapshot_by_record(record_id) {
            return Some(live.to_rehydration_bytes());
        }
        self.snapshots.lock().unwrap().rehydration_bytes(record_id)
    }

    /// IDEMPOTENCY CHECK for a mutating operation: returns `true` if `key` was ALREADY
    /// applied (a replayed/duplicate mutation — the caller treats it as a no-op success). A
    /// PURE read: it does NOT record the key, so a mutation that subsequently FAILS stays
    /// retriable. An empty key opts out (always `false`). Pair with
    /// [`Runtime::idempotency_record`] AFTER a successful apply.
    pub fn idempotency_seen(&self, key: &str) -> bool {
        if key.is_empty() {
            return false;
        }
        self.recovery.lock().unwrap().is_duplicate(key)
    }

    /// Record an idempotency `key` as applied AFTER its mutation succeeded, persisting the
    /// bounded journal so a retry after a crash recognises the key. A no-op for an empty key
    /// or an already-recorded one.
    pub fn idempotency_record(&self, key: &str) {
        if key.is_empty() {
            return;
        }
        let mut guard = self.recovery.lock().unwrap();
        if guard.is_duplicate(key) {
            return;
        }
        guard.record_key(key);
        guard.save(&self.data_dir);
    }

    /// Dispatch a runtime request (the PTY ops of HHC + the managed-command/MCP ops of
    /// HHD) onto the live services, producing the response the supervisor receives.
    /// `events` is the shared unsolicited-event sink the spawned terminal's pump streams
    /// binary output / cwd / exec-state / exit through (and the command runner streams
    /// command-state / command-output through).
    ///
    /// An error in any op becomes a structured [`Response::Error`] (`MalformedRequest`
    /// is reused as the generic op-failed code so the supervisor surfaces a readable
    /// reason rather than a hang). Only runtime requests are passed here — the binary's
    /// run loop routes them via [`crate::pipe::handle_request_stateless`].
    pub fn dispatch_runtime(&self, req: &Request, events: &EventSink) -> Response {
        use crate::pipe::wire::ErrorCode;
        let err = |id: u64, msg: String| Response::Error {
            id,
            code: ErrorCode::MalformedRequest,
            message: msg,
        };
        match req {
            Request::PtySpawn {
                id,
                cols,
                rows,
                cwd,
                record_id,
            } => {
                // Route the Claude plugin's SessionStart hook to THIS instance (review
                // `01KXAXQN32GASMEB05PDW5BZSR`): read the port the MCP server ACTUALLY bound and
                // export it into the spawned shell as `NYX_MCP_PORT`. The bound port is the single
                // source of truth (the same value `reconcile_claude_plugin` writes into the
                // installed `.mcp.json`), read live so it is always the real, dynamically bound
                // port — never a hard-coded 8765/8766, correct even under a v1/v2 collision.
                // `0` (the MCP server has not started yet — a terminal racing the boot McpStart)
                // exports nothing, so the hook keeps its own fallback rather than a wrong port.
                let mcp_port = self.mcp().map(|m| m.bound_port()).unwrap_or(0);
                match self
                    .pty
                    .spawn(*cols, *rows, cwd, record_id, mcp_port, events.clone())
                {
                    Ok(terminal_id) => {
                        // LAZY agent-session resume (v2.5.5): the terminal's PTY is now live — drain
                        // and inject a parked `claude --resume <id>` exactly once (a bare terminal
                        // with no park is untouched). Mirrors the v1 injection at `register_terminal_pty`.
                        self.inject_resume_on_spawn(record_id, events);
                        Response::PtySpawned {
                            id: *id,
                            terminal_id,
                        }
                    }
                    Err(e) => err(*id, format!("pty spawn failed: {e}")),
                }
            }
            Request::PtyWrite {
                id,
                terminal_id,
                data,
            } => match self.pty.write(*terminal_id, data) {
                Ok(()) => Response::Ok { id: *id },
                Err(e) => err(*id, format!("pty write failed: {e}")),
            },
            Request::PtyResize {
                id,
                terminal_id,
                cols,
                rows,
            } => match self.pty.resize(*terminal_id, *cols, *rows) {
                Ok(()) => Response::Ok { id: *id },
                Err(e) => err(*id, format!("pty resize failed: {e}")),
            },
            Request::PtyClose { id, terminal_id } => {
                self.pty.close(*terminal_id);
                Response::Ok { id: *id }
            }
            Request::PtyAck {
                id,
                terminal_id,
                bytes,
            } => {
                self.pty.ack(*terminal_id, *bytes);
                Response::Ok { id: *id }
            }
            // --- Managed-command lifecycle (HHD) ---------------------------------
            // The UI lifecycle requests carry NO per-run env (the map is an MCP-tool argument,
            // which travels to the SAME runner through `mcp_runtime::start_command`), so these
            // pass what the request holds: nothing.
            Request::CommandStart { id, instance_id } => match self.command() {
                Some(cmd) => match cmd.start(instance_id, &[]) {
                    Ok(status) => status.into_response(*id),
                    Err(e) => err(*id, format!("command start failed: {e}")),
                },
                None => err(*id, "managed-command runtime not started".into()),
            },
            Request::CommandStop { id, instance_id } => match self.command() {
                Some(cmd) => match cmd.stop(instance_id) {
                    Ok(status) => status.into_response(*id),
                    Err(e) => err(*id, format!("command stop failed: {e}")),
                },
                None => err(*id, "managed-command runtime not started".into()),
            },
            Request::CommandRelaunch { id, instance_id } => match self.command() {
                Some(cmd) => match cmd.relaunch(instance_id, &[]) {
                    Ok(status) => status.into_response(*id),
                    Err(e) => err(*id, format!("command relaunch failed: {e}")),
                },
                None => err(*id, "managed-command runtime not started".into()),
            },
            // --- MCP HTTP server (HHD) -------------------------------------------
            // Start-once + (on the first start of the process) reconcile the bundled Claude
            // plugin against the bound port, so the installed `.mcp.json` is re-templated to
            // the port nyx is actually listening on and Claude Code can CONNECT the MCP
            // (finding #44/#45/#47). `start_mcp` returns 0 when the MCP runtime is not built
            // yet, which the supervisor surfaces as a non-fatal "did not start".
            Request::McpStart { id } => match self.mcp() {
                Some(_) => Response::McpStarted {
                    id: *id,
                    port: self.start_mcp(),
                },
                None => err(*id, "mcp runtime not started".into()),
            },
            // --- The GENERIC non-PTY backend command (G0) ------------------------
            // The full DB-backed + integrations + live-PTY-introspection + activity +
            // command-output/acknowledge long tail. Dispatched by name, emitting the
            // `commands://changed`/`workspaces://changed` invalidations on a successful
            // template/tree mutation (parity with the v1 core-command router). The error
            // path is a structured `Response::Error` the supervisor surfaces as a `command`
            // BridgeError, never a hang.
            Request::DbCommand {
                id,
                command,
                args_json,
            } => match db_command::dispatch(self, command, args_json, events) {
                Ok(result_json) => Response::DbResult {
                    id: *id,
                    result_json,
                },
                Err(e) => err(*id, e),
            },
            // Stateless requests never reach here (routed by handle_request_stateless).
            _ => err(0, "stateless request routed to dispatch_runtime".into()),
        }
    }
}

/// The exact resume command for a candidate IFF the SHARED `agent_resume::decide_resume` policy
/// would resume it (project toggle ON, live candidate, transcript on disk, resumable target +
/// adapter), else `None`. It REUSES `decide_resume` verbatim — the SAME gate the workspace restore
/// plan applies (no new policy, no duplication) — so the idle-session UNLOAD (v2.5.5, review
/// `01KX9CP96KVK6BTHY3ZSWY1E67`):
///   * gates unloading on resume-eligibility (finding 2 — never kill a session it could not bring
///     back: a `resume_agent_sessions`-OFF project yields `Skip(OptionOff)` → `None` → kept), and
///   * rebuilds the `claude --resume <id>` to RE-PARK at unload (finding 1 — the in-run revisit
///     re-injects it) from the SAME string `decide_resume` returns.
///
/// A free fn (not a method): it needs none of the runtime state, only the plain candidate + the
/// run-constant target/registry the caller resolves once.
fn resume_command_for_candidate(
    candidate: &nyx_core::db::ResumeCandidate,
    target: nyx_core::agent_resume::ResumeTarget,
    registry: &nyx_core::agent::AgentRegistry,
) -> Option<String> {
    let session_state = nyx_core::agent_resume::SessionState::from_db(&candidate.session_state)?;
    // `stat` the captured transcript (finding #53 parity): a missing file → not resumable.
    let transcript_exists = candidate
        .transcript_path
        .as_deref()
        .map(|p| std::path::Path::new(p).exists())
        .unwrap_or(false);
    let inputs = nyx_core::agent_resume::ResumeInputs {
        project_resume_on: candidate.project_resume_on,
        // A terminal reached by the sweep is `alive` (a voluntary close leaves it `closed`, so it
        // never reaches here) — the voluntary-close gate is structural, exactly as the restore plan.
        closed_voluntarily: false,
        session_state,
        external_session_id: &candidate.external_session_id,
        transcript_exists,
        target,
    };
    let adapter = registry.get(&candidate.agent_kind)?;
    match nyx_core::agent_resume::decide_resume(&inputs, adapter) {
        nyx_core::agent_resume::ResumeDecision::Resume { command, .. } => Some(command),
        nyx_core::agent_resume::ResumeDecision::Skip(_) => None,
    }
}
