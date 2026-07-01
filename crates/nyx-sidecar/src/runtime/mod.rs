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

pub mod command;
pub mod db;
pub mod db_command;
pub mod events;
pub mod mcp;
pub mod pty;
pub mod recovery;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nyx_core::agent_activity::AgentActivityStore;
use nyx_core::command::CommandRunner;
use nyx_core::db::Db;

use crate::pipe::wire::{Request, Response};
use command::{CommandRuntime, SidecarRunnerSink};
use events::EventSink;
use mcp::McpRuntime;

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
    /// The sidecar-owned crash-recovery state (Phase 4): the clean-shutdown marker, the
    /// instance ids flagged `interrupted` after a crash, and the idempotency-key journal for
    /// mutating ops. Persisted to `<data_dir>/sidecar-recovery.json` (NOT the frozen v1 DB).
    /// Behind a `Mutex` so the boot reconcile, the mutating db-commands' idempotency guard,
    /// and the ordered shutdown all serialize their reads/writes through one cell.
    recovery: Arc<Mutex<recovery::RecoveryState>>,
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
        Self {
            db,
            pty: Arc::new(pty::PtyManager::new()),
            command: Arc::new(Mutex::new(None)),
            mcp: Arc::new(Mutex::new(None)),
            runner_cell: Arc::new(Mutex::new(None)),
            activity: Arc::new(AgentActivityStore::new()),
            data_dir,
            resource_dir,
            recovery: Arc::new(Mutex::new(recovery::RecoveryState::default())),
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
        ));
        *self.command.lock().unwrap() = Some(Arc::clone(&command));
        *self.mcp.lock().unwrap() = Some(mcp);

        // CRASH RECONCILE (Phase 4 — `01KWCJPFP9JSJMRDZGW2E0N905`): BEFORE the boot restore,
        // detect whether the PREVIOUS run shut down cleanly. If it CRASHED, flag every
        // instance persisted as `running` as `interrupted` (its liveness can't be
        // re-verified), clear its shutdown snapshot so the snapshot-keyed restore below
        // cannot blindly relaunch it, and flip every still-`active` agent session to
        // `unknown`. The user resumes interrupted instances explicitly (no blind resume —
        // the PRD non-goal). The reconcile re-arms the clean-shutdown marker to false for the
        // run we are starting.
        let outcome = recovery::reconcile_after_crash(&self.db, &self.data_dir);
        // Latch the interrupted set into the runtime so the `recovery_snapshot` db-command
        // surfaces it to the renderer and the idempotency journal is the loaded one.
        {
            let mut guard = self.recovery.lock().unwrap();
            *guard = recovery::RecoveryState::load(&self.data_dir);
        }
        if outcome.was_crash {
            events.log(
                "warn",
                format!(
                    "crash recovery: previous run did not shut down cleanly — {} command(s) flagged interrupted, {} agent session(s) marked unknown (user chooses what to resume)",
                    outcome.interrupted_instances.len(),
                    outcome.sessions_marked_unknown
                ),
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

    /// The shared SQLite pool. The runtime's single connection authority.
    pub fn db(&self) -> &Arc<Db> {
        &self.db
    }

    /// The live interactive-PTY manager.
    pub fn pty(&self) -> &Arc<pty::PtyManager> {
        &self.pty
    }

    /// The managed-command runtime (once [`attach_events`](Runtime::attach_events) ran).
    pub fn command(&self) -> Option<Arc<CommandRuntime>> {
        self.command.lock().unwrap().clone()
    }

    /// The MCP runtime (once [`attach_events`](Runtime::attach_events) ran).
    pub fn mcp(&self) -> Option<Arc<McpRuntime>> {
        self.mcp.lock().unwrap().clone()
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
    /// A crash never runs `shutdown`, so the next boot reads it still `false` and the crash
    /// reconcile fires. We persist the clean marker WHILE keeping the idempotency journal, and
    /// clear the now-consumed interrupted set (the next clean run starts with nothing doubtful).
    pub fn shutdown(&self) {
        if let Some(command) = self.command() {
            if command.begin_shutdown() {
                command.snapshot_on_shutdown();
                command.kill_all_running();
            }
        }
        self.pty.close_all();
        // Mark this an ORDERED (clean) shutdown so the next boot does NOT run the crash
        // reconcile. Best-effort persistence (the file is an optimization over the DB).
        {
            let mut guard = self.recovery.lock().unwrap();
            guard.clean_shutdown = true;
            guard.interrupted.clear();
            guard.save(&self.data_dir);
        }
    }

    /// The command-instance ids flagged `interrupted` by the boot crash reconcile (empty
    /// after a clean prior shutdown). The `recovery_snapshot` db-command surfaces these so
    /// the renderer badges them and the user chooses what to resume.
    pub fn interrupted_instances(&self) -> Vec<String> {
        self.recovery.lock().unwrap().interrupted.clone()
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
            } => match self.pty.spawn(*cols, *rows, cwd, record_id, events.clone()) {
                Ok(terminal_id) => Response::PtySpawned {
                    id: *id,
                    terminal_id,
                },
                Err(e) => err(*id, format!("pty spawn failed: {e}")),
            },
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
            Request::CommandStart { id, instance_id } => match self.command() {
                Some(cmd) => match cmd.start(instance_id) {
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
                Some(cmd) => match cmd.relaunch(instance_id) {
                    Ok(status) => status.into_response(*id),
                    Err(e) => err(*id, format!("command relaunch failed: {e}")),
                },
                None => err(*id, "managed-command runtime not started".into()),
            },
            // --- MCP HTTP server (HHD) -------------------------------------------
            Request::McpStart { id } => match self.mcp() {
                Some(mcp) => Response::McpStarted {
                    id: *id,
                    port: mcp.start(),
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
