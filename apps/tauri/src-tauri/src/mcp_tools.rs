//! MCP tool implementations (PRD-4 phase 2, ADR-0003).
//!
//! This module is the phase-2 [`crate::mcp::ToolDispatcher`]: it plugs the frozen
//! v1 tool surface into the SAME PRD-2/PRD-3 runtime + DB layer the Tauri UI drives,
//! so an agent and the UI share one source of truth. Nothing here owns a second
//! command lifecycle (ADR-0003 D6): every command tool delegates to the managed
//! [`crate::command::CommandRunner`] (`start`/`stop`/`relaunch`/`live_output`) and
//! the [`crate::db`] helpers, exactly like the bridge commands
//! `command_start`/`command_stop`/`command_relaunch`/`command_output`.
//!
//! Design notes honoring the ADR:
//! - **D4 / D5 — explicit ids, no cwd magic.** Action tools take explicit ids
//!   (`instance_id`, `project_id`, `workspace_id`). A `cwd` argument is accepted by
//!   listing tools ONLY as an optional *filter* — never to resolve "the current"
//!   project/workspace. Absent `cwd` → the listing returns everything and the agent
//!   chooses; an ambiguous `cwd` (matching several workspaces) returns ALL matches,
//!   never a silently-picked one.
//! - **D6 — one lifecycle.** The dispatcher reaches the managed `Db` and
//!   `ManagedCommandRunner` over the held `AppHandle`, so a command launched via MCP
//!   is the SAME instance as the UI's (same `command://state`/`command://output`
//!   events, same persistence, same sidebar dot).
//! - **D7 — bounded output.** `get_command_output` returns a bounded WINDOW
//!   (`tail_bytes`/`since`), never the whole scrollback. See [`bound_output`].
//! - **D8 — standardized errors.** Every failure is a [`RpcError`] with one of the
//!   ADR string codes (`invalid_id`, `invalid_argument`, `invalid_state`,
//!   `output_too_large`, `mcp_unavailable`, `internal`).

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

use std::time::Duration;

use crate::bridge::{ManagedCommandRunner, PendingTerminalCommands, PtyManager, TerminalPtyMap};
use crate::command::{poll_until, RunState, WAIT_MAX_TIMEOUT, WAIT_POLL_INTERVAL};
use crate::db::{self, Db};
use crate::agent::{AgentEvent, AgentRegistry};
use crate::mcp::{
    RpcCode, RpcError, ToolDispatcher, ADD_COMMAND_TOOL, AGENT_SESSION_EVENT_TOOL, CLEAR_COMMAND_OUTPUT_TOOL,
    CLOSE_TERMINAL_TOOL, CREATE_TERMINAL_TOOL, IMPORT_COMMANDS_TOOL, LIST_IMPORTABLE_SCRIPTS_TOOL,
    LIST_TERMINALS_TOOL, PROBE_TOOL, READ_TERMINAL_TOOL, REMOVE_COMMANDS_TOOL, REMOVE_COMMAND_TOOL,
    REMOVE_WORKSPACE_TOOL, SEND_TO_TERMINAL_TOOL, UPDATE_COMMAND_TOOL, WAIT_FOR_COMMAND_TOOL,
};

/// Default `tail_bytes` window for `get_command_output` / `wait_for_command`
/// (ADR-0003 D7, review R-OUTPUT): a TOKEN-SAFE last-12 KiB of the scrollback when
/// the caller does not ask for a specific window. The prior 64 KiB default produced
/// ~80k JSON-escaped chars on a busy dev server, which blew an agent's MCP token
/// budget on a single default read (the dogfood P0). 12 KiB of cleaned text sits
/// comfortably inside the budget while still showing the meaningful tail; an agent
/// that genuinely needs more raises `tail_bytes` explicitly (capped at
/// [`MAX_TAIL_BYTES`]).
pub const DEFAULT_TAIL_BYTES: usize = 12 * 1024;
/// Hard ceiling on a single `get_command_output` window (ADR-0003 D7): a request
/// for more than 1 MiB is refused with `output_too_large` rather than served.
pub const MAX_TAIL_BYTES: usize = 1024 * 1024;

/// Default `timeout_ms` for `wait_for_command` (ADR-0003 D12) when the caller omits
/// it: a 30 s bounded wait. The effective wait is always clamped to
/// [`crate::command::WAIT_MAX_TIMEOUT`] so the long-poll is never unbounded.
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;

/// The phase-2 [`ToolDispatcher`]: routes every v1 tool to the managed PRD-2/PRD-3
/// layer over the held `AppHandle`. Generic over the Tauri `Runtime` so the same
/// implementation backs the production runtime and the mock test runtime.
pub struct NyxToolDispatcher<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> NyxToolDispatcher<R> {
    /// Build a dispatcher over `app`. The `Db` and `ManagedCommandRunner` it routes
    /// to must already be managed on `app` (they are, from the setup hook, before the
    /// dispatcher is installed onto the MCP server).
    pub fn new(app: AppHandle<R>) -> Self {
        Self { app }
    }

    /// The managed single-connection [`Db`]. `mcp_unavailable` if the runtime is not
    /// fully set up (the managed state is absent) — the explicit degradation D8 asks
    /// for instead of a panic.
    fn db(&self) -> Result<tauri::State<'_, Db>, RpcError> {
        self.app
            .try_state::<Db>()
            .ok_or_else(|| RpcError::new(RpcCode::McpUnavailable, "nyx runtime not reachable: db"))
    }

    /// The managed command runner (same instance the UI lifecycle commands use).
    fn runner(&self) -> Result<tauri::State<'_, ManagedCommandRunner<R>>, RpcError> {
        self.app.try_state::<ManagedCommandRunner<R>>().ok_or_else(|| {
            RpcError::new(RpcCode::McpUnavailable, "nyx runtime not reachable: command runner")
        })
    }

    /// The managed terminal RECORD ↔ live PTY map (the front registers it via
    /// `register_terminal_pty`). `mcp_unavailable` if the runtime is not yet set up.
    fn terminal_pty_map(&self) -> Result<tauri::State<'_, TerminalPtyMap>, RpcError> {
        self.app.try_state::<TerminalPtyMap>().ok_or_else(|| {
            RpcError::new(RpcCode::McpUnavailable, "nyx runtime not reachable: terminal map")
        })
    }

    /// The managed park for MCP-supplied terminal opening commands (drained by
    /// `register_terminal_pty` once the front spawns the PTY). `mcp_unavailable` if absent.
    fn pending_terminal_commands(&self) -> Result<tauri::State<'_, PendingTerminalCommands>, RpcError> {
        self.app.try_state::<PendingTerminalCommands>().ok_or_else(|| {
            RpcError::new(RpcCode::McpUnavailable, "nyx runtime not reachable: terminal command park")
        })
    }

    /// The managed live PTY registry (the terminal write/close path goes through it,
    /// the SAME state the `pty_write`/`pty_close` commands use). `mcp_unavailable` if absent.
    fn pty_manager(&self) -> Result<tauri::State<'_, PtyManager>, RpcError> {
        self.app
            .try_state::<PtyManager>()
            .ok_or_else(|| RpcError::new(RpcCode::McpUnavailable, "nyx runtime not reachable: pty manager"))
    }

    /// The FACTUAL run status of an instance as [`status_json`], reported the way the
    /// v4 split requires: the runner's LIVE outcome is authoritative when it backs the
    /// instance this session, else the PERSISTED outcome from the DB row is used as a
    /// fallback (so a finished run reports `state=error`/`exit_code` correctly even
    /// after a restart, when the in-memory map is empty). Either way the outcome is
    /// the FACTUAL one — a UI acknowledge flips only `unread`, never the state/code —
    /// so an agent always sees a crash (`exit_code ≠ 0`) it would otherwise have lost
    /// when the UI acknowledged it. `inst` is the already-loaded DB row (the listing
    /// has it in hand); the action tools that lack a row read it once via the runner.
    fn factual_status_from_row(
        runner: &ManagedCommandRunner<R>,
        instance_id: &str,
        persisted_state: &str,
        persisted_exit_code: Option<i32>,
        persisted_unread: bool,
    ) -> Value {
        match runner.outcome(instance_id) {
            // Live entry this session: the runner is authoritative.
            Some((state, exit_code, unread)) => status_json(state, exit_code, unread),
            // No live entry (e.g. cold after a restart): fall back to the persisted
            // factual outcome so the crash signal is not lost.
            None => status_json(
                RunState::from_db_str(persisted_state),
                persisted_exit_code,
                persisted_unread,
            ),
        }
    }

    /// The run status read straight off the LIVE runner — used by the action tools
    /// (start/stop/relaunch) right after they mutate the runner, when a live entry is
    /// guaranteed to exist. Falls back to a neutral idle status only if the entry is
    /// somehow absent (e.g. a stop that left no entry), keeping the uniform shape.
    fn runner_status(&self, runner: &ManagedCommandRunner<R>, instance_id: &str) -> Value {
        match runner.outcome(instance_id) {
            Some((state, exit_code, unread)) => status_json(state, exit_code, unread),
            None => status_json(RunState::Idle, None, false),
        }
    }

    /// [`Self::factual_status_from_row`] for a tool that holds only an id: read the
    /// persisted outcome from the DB row, then prefer the live runner outcome over it.
    fn factual_status(&self, instance_id: &str) -> Result<Value, RpcError> {
        let runner = self.runner()?;
        // Fast path: a live entry needs no DB read at all.
        if let Some((state, exit_code, unread)) = runner.outcome(instance_id) {
            return Ok(status_json(state, exit_code, unread));
        }
        // Cold path: rehydrate the persisted factual outcome.
        let db = self.db()?;
        let inst = db
            .with_conn(|c| db::get_instance(c, instance_id))
            .map_err(internal_db("read command status"))?;
        Ok(match inst {
            Some(inst) => status_json(
                RunState::from_db_str(&inst.last_state),
                inst.last_exit_code,
                inst.unread,
            ),
            None => status_json(RunState::Idle, None, false),
        })
    }

    // --- PRD-2 context tools (#4) ----------------------------------------

    /// `list_projects` — `{}` → `{ projects: Project[] }`. The discovery entry point
    /// (ADR-0003 D4): the agent enumerates projects and picks an id; it never guesses.
    fn list_projects(&self) -> Result<Value, RpcError> {
        let db = self.db()?;
        let projects = db
            .with_conn(db::list_projects)
            .map_err(internal_db("list projects"))?;
        Ok(json!({ "projects": projects }))
    }

    /// `list_workspaces` — `{ project_id, cwd? }` → `{ workspaces: Workspace[] }`.
    /// `cwd` is the OPTIONAL filter of ADR-0003 D5: it narrows the listing to
    /// workspaces whose `path` matches, but never resolves "the" current workspace.
    /// Absent `cwd` returns every workspace of the project; an ambiguous `cwd`
    /// returns ALL matches (the agent disambiguates by id), never a guessed one.
    ///
    /// **Live git branch (dogfood finding):** each workspace's `branch` is resolved
    /// LIVE at read time via [`db::detect_branch`], NOT served from the stale value
    /// stored at workspace-add time (which goes wrong the moment the user switches
    /// branches — the finding's two-worktrees-on-`main`-show-`null` symptom). A
    /// non-git folder resolves to `null`, same as at add time. The branch resolution
    /// runs AFTER the `cwd` filter so only the returned rows pay the (cheap, read-only)
    /// `git` call.
    fn list_workspaces(&self, args: &Value) -> Result<Value, RpcError> {
        let project_id = require_str(args, "project_id")?;
        let cwd = optional_str(args, "cwd")?;
        let db = self.db()?;
        let mut workspaces = db
            .with_conn(|c| db::list_workspaces(c, project_id))
            .map_err(internal_db("list workspaces"))?;
        if let Some(cwd) = cwd {
            let needle = crate::pathnorm::normalize(cwd);
            workspaces.retain(|w| path_matches(&w.path, &needle));
        }
        // Refresh `branch` LIVE so the displayed value tracks the work tree's current
        // HEAD rather than the (possibly stale) value captured at add time. Non-git
        // folders resolve to None → serialized as null.
        for w in &mut workspaces {
            w.branch = db::detect_branch(&w.path);
        }
        Ok(json!({ "workspaces": workspaces }))
    }

    /// `workspace_add` — `{ project_id, path, name? }` → `{ workspace }`. Registers an
    /// EXISTING on-disk folder as a workspace (the *register an existing dir* tool —
    /// contrast `create_workspace`, which CREATES the folder first, D2). The path is
    /// VALIDATED on disk BEFORE the DB write (dogfood finding): a path that does not
    /// exist, or that exists but is NOT a directory (a file), is rejected with the D8
    /// `invalid_argument` vocabulary and an actionable message, so a typo'd path can no
    /// longer register a phantom workspace. Then delegates to `db::create_workspace`
    /// (ADR-0003 §8): an unknown project (FK) or a duplicate path in the same project
    /// (UNIQUE) surfaces as the D8 error vocabulary. `name` defaults to the path's
    /// last segment when omitted.
    fn workspace_add(&self, args: &Value) -> Result<Value, RpcError> {
        let project_id = require_str(args, "project_id")?;
        let path = require_str(args, "path")?;
        let name = match optional_str(args, "name")? {
            Some(n) => n.to_string(),
            None => {
                // Derive the name from the path's last segment — but reject an EMPTY
                // result (e.g. path "/" or all-separators), which would register a
                // workspace named "" rather than failing.
                let derived = basename(path);
                if derived.is_empty() {
                    return Err(RpcError::new(
                        RpcCode::InvalidArgument,
                        format!(
                            "could not derive a workspace name from path '{path}' — \
                             pass an explicit `name`"
                        ),
                    ));
                }
                derived
            }
        };
        // The contract: workspace_add registers an EXISTING directory. Validate that
        // on disk before touching the DB so a non-existent / non-dir path is rejected
        // with an actionable invalid_argument rather than silently creating a phantom
        // workspace row that points nowhere (the dogfood finding).
        validate_existing_dir(path)?;
        self.create_workspace_inner(project_id, &name, path)
    }

    /// `create_workspace` — `{ project_id, name, path }` → `{ workspace }`. The
    /// *creating-intent* sibling of `workspace_add` (D2): it `mkdir -p`s the folder
    /// (creating it AND any missing parents) BEFORE registering, so an agent can ask
    /// nyx to track a folder that does not exist on disk yet. `workspace_add` instead
    /// requires the folder to already exist. Both then share the SAME
    /// `db::create_workspace` write (one persistence path, ADR-0003 §8/§9). A path that
    /// cannot be created (e.g. a component is a file, or permission denied) → the D8
    /// `invalid_argument` vocabulary. `name` is required here.
    fn create_workspace(&self, args: &Value) -> Result<Value, RpcError> {
        let project_id = require_str(args, "project_id")?;
        let name = require_str(args, "name")?;
        let path = require_str(args, "path")?;
        // Creating intent (D2): make the directory tree first, then register. A path
        // that already exists as a directory is a no-op create (idempotent); a path
        // that exists as a FILE, or that cannot be created, is invalid_argument.
        ensure_dir_created(path)?;
        self.create_workspace_inner(project_id, name, path)
    }

    /// Shared body of `workspace_add` / `create_workspace`: one `db::create_workspace`
    /// call, mapping its failure to the D8 vocabulary. A FK violation (unknown
    /// project) → `invalid_id`; a UNIQUE violation (duplicate path) → `invalid_state`;
    /// anything else → `internal`. The on-disk path handling (validate-existing vs
    /// mkdir-p) is done by the caller BEFORE this, so the two tools differ only in
    /// their filesystem precondition, not in the persistence path.
    fn create_workspace_inner(
        &self,
        project_id: &str,
        name: &str,
        path: &str,
    ) -> Result<Value, RpcError> {
        let db = self.db()?;
        match db.with_conn(|c| db::create_workspace(c, project_id, name, path)) {
            Ok(workspace) => {
                // MUTATING tool → emit the shared structural-refresh event so the
                // sidebar re-pulls its projects/workspaces tree WITHOUT a manual
                // reload (review 01KV9611923NKX3JPR5V6MN44F). This is the SAME
                // `workspaces://changed` signal the UI's own `create_workspace`
                // command emits, so a UI- and an MCP-driven add converge on one
                // refresh path — the principle "every mutating MCP tool emits a
                // frontend event" (cf. the command tools' `command://state`). Emitted
                // only on a SUCCESSFUL mutation (after the row commits), never on the
                // error branch. Future mutating tools (command-template CRUD) reuse
                // `bridge::emit_workspaces_changed` the same way.
                crate::bridge::emit_workspaces_changed(&self.app);
                Ok(json!({ "workspace": workspace }))
            }
            Err(e) => Err(map_create_workspace_err(project_id, e)),
        }
    }

    // --- PRD-3 command tools (#3 + the listing of #4) --------------------

    /// `list_commands` — `{ workspace_id }` (instances with live state, the nominal
    /// form) OR `{ project_id }` (templates) → `{ commands: [...] }`. Routes to
    /// `db::list_instances_for_workspace` / `db::list_templates`. For the instance
    /// form, `last_state` is overlaid with the runner's LIVE state (D6): the DB row is
    /// only the debounced mirror, the runner map is the truth. Each instance row ALSO
    /// carries the `{ running, finished, exit_code }` run-status fields (#19/#20),
    /// derived from the SAME live runner via [`status_json`] — the identical shape the
    /// action tools and `get_command_output` already surface (#13) — so an agent that
    /// merely LISTS commands can already tell a crash (`exit_code ≠ 0`, `state: error`)
    /// from a clean run (`exit_code: 0`, `state: success`) per row, without a follow-up
    /// `get_command_output` call.
    fn list_commands(&self, args: &Value) -> Result<Value, RpcError> {
        let db = self.db()?;
        // Instance form (nominal): the pilotable instances of a workspace.
        if let Some(workspace_id) = optional_str(args, "workspace_id")? {
            let runner = self.runner()?;
            let rows = db
                .with_conn(|c| db::list_instances_for_workspace(c, workspace_id))
                .map_err(internal_db("list command instances"))?;
            let commands: Vec<Value> = rows
                .into_iter()
                .map(|row| {
                    // The FACTUAL outcome: the live runner state when it backs the
                    // instance this session (authoritative for "running right now"),
                    // else the PERSISTED outcome from the row (so a finished run's
                    // crash signal survives a restart AND a UI acknowledge — which now
                    // flips only `unread`, never the state/code). `status_json` then
                    // yields one consistent `{ state, running, finished, exit_code,
                    // unread }` snapshot (#19/#20 + the v4 split).
                    let status = Self::factual_status_from_row(
                        &runner,
                        &row.id,
                        &row.last_state,
                        row.last_exit_code,
                        row.unread,
                    );
                    // The back-compat `last_state` field mirrors the factual state.
                    let state_str = status
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or(db::STATE_IDLE)
                        .to_string();
                    let cwd = crate::subfolder::resolve_run_dir_lossy(
                        &row.workspace_path,
                        row.subfolder.as_deref(),
                    );
                    let mut entry = json!({
                        "instance_id": row.id,
                        "command_id": row.command_id,
                        "workspace_id": row.workspace_id,
                        "name": row.name,
                        "command": row.command,
                        "subfolder": row.subfolder,
                        // `last_state` mirrors the FACTUAL state (back-compat field).
                        "last_state": state_str,
                        "cwd": cwd,
                        "source_kind": row.source_kind,
                        "package_manager": row.package_manager,
                    });
                    // Splat `{ state, running, finished, exit_code }` into the row so a
                    // listing carries the crash-vs-clean signal per instance, matching
                    // start/stop/relaunch/get_command_output and ADR-0003 §3.
                    if let (Some(map), Some(status_map)) =
                        (entry.as_object_mut(), status.as_object())
                    {
                        for (k, v) in status_map {
                            map.insert(k.clone(), v.clone());
                        }
                    }
                    entry
                })
                .collect();
            return Ok(json!({ "commands": commands }));
        }
        // Template form: a project's command templates (no live instances).
        if let Some(project_id) = optional_str(args, "project_id")? {
            let templates = db
                .with_conn(|c| db::list_templates(c, project_id))
                .map_err(internal_db("list command templates"))?;
            let commands: Vec<Value> = templates
                .into_iter()
                .map(|t| {
                    json!({
                        "command_id": t.id,
                        "project_id": t.project_id,
                        "name": t.name,
                        "command": t.command,
                        "subfolder": t.subfolder,
                        "source_kind": t.source_kind,
                        "package_manager": t.package_manager,
                    })
                })
                .collect();
            return Ok(json!({ "commands": commands }));
        }
        Err(RpcError::new(
            RpcCode::InvalidArgument,
            "list_commands requires either workspace_id (instances) or project_id (templates)",
        ))
    }

    /// `start_command` — `{ instance_id | (name, workspace_id), env? }` →
    /// `{ instance_id, running, was_running, restarted, state, ... }`. Delegates to
    /// `CommandRunner::start_with_env` (ADR-0003 D4/D6), resolving the command line +
    /// cwd from the DB exactly like `bridge::command_start`. An unknown instance →
    /// `invalid_id` BEFORE any spawn. The `{ name, workspace_id }` form (finding #16)
    /// resolves the named instance within the workspace; an unknown or ambiguous name →
    /// a clear error.
    ///
    /// **Double-start semantics (R-WSCMD #5):** `start_command` on an already-running
    /// instance is a NO-OP — it does NOT spawn a second process (the guard lives at the
    /// runner boundary). The ack reports `was_running:true, restarted:false` so the
    /// agent can tell the no-op apart from a fresh start (`was_running:false`). A fresh
    /// start is NOT a restart (`restarted:false`); `relaunch_command` is the explicit
    /// restart. `start_command` never restarts a running instance.
    ///
    /// **Per-run env (R-WSCMD #7):** an OPTIONAL `env` map (`{ KEY: VALUE }`) is MERGED
    /// onto the inherited environment for THIS run (e.g. `VAULT_ENV`, values lifted from
    /// a `.env`), plumbed through to the PRD-3 runner spawn. Secret VALUES are never
    /// logged. On a no-op (already running) the env is ignored — the live process keeps
    /// the env it was started with; relaunch to apply a new env.
    fn start_command(&self, args: &Value) -> Result<Value, RpcError> {
        let instance_id = self.resolve_instance_id(args)?;
        let instance_id = instance_id.as_str();
        let env = optional_env(args, "env")?;
        let (command, cwd) = self.resolve_command_and_cwd(instance_id)?;
        let runner = self.runner()?;
        let outcome = runner
            .start_with_env(instance_id, &command, Some(&cwd), &env)
            .map_err(|e| RpcError::new(RpcCode::Internal, format!("start failed: {e}")))?;
        // Explicit mutation ack (R-WSCMD #4/#5): `was_running` (was it already running
        // when start was called → the call was a no-op) and `restarted` (always false
        // for start — a fresh start is not a restart, and a running instance is a no-op,
        // never a restart; relaunch is the restart). `running` is the live state flag.
        let mut result = status_result(instance_id, self.runner_status(&runner, instance_id));
        if let Some(map) = result.as_object_mut() {
            map.insert("was_running".to_string(), json!(outcome.was_running));
            map.insert("restarted".to_string(), json!(false));
        }
        Ok(result)
    }

    /// `stop_command` — `{ instance_id }` →
    /// `{ instance_id, changed, was_running, state, ... }`. Delegates to
    /// `CommandRunner::stop`. Idempotent on a non-running instance (returns the
    /// current state, not an error — ADR-0003 D8 idempotent rule). Validates the
    /// instance id first so an unknown id is `invalid_id`, not a silent idle.
    ///
    /// **Explicit ack (R-WSCMD #4):** `was_running` reports whether the instance was
    /// running BEFORE the stop, and `changed` reports whether the stop actually did
    /// something (killed a live process) vs was a silent no-op on an already-idle
    /// instance — so a stop on an idle command no longer looks like a successful stop.
    fn stop_command(&self, args: &Value) -> Result<Value, RpcError> {
        let instance_id = require_str(args, "instance_id")?;
        self.assert_instance_exists(instance_id)?;
        let runner = self.runner()?;
        // Capture liveness BEFORE the stop so we can report changed/was_running. The
        // runner is the source of truth for "running right now".
        let was_running = runner.is_running(instance_id);
        let state_after = runner
            .stop(instance_id)
            .map_err(|e| RpcError::new(RpcCode::Internal, format!("stop failed: {e}")))?;
        // `changed` ⇔ OUR stop actually killed a live process. Derive it from stop()'s own
        // result (the state AFTER the stop), not just the pre-stop liveness snapshot: if
        // the command finished NATURALLY in the race window between the snapshot and the
        // stop, stop() is a no-op that returns the settled success/error state — reporting
        // that as a user-initiated stop (changed:true) would mislabel a natural exit. Only
        // a running snapshot that the stop transitioned to `idle` counts as a real change.
        let changed = was_running && state_after == RunState::Idle;
        // Surface the run status (finding #13 + v4). A stop is a kill, not a natural
        // exit, so it transitions to `idle` with no `exit_code` — distinct from a
        // `success`/`error` finish, which carries its code.
        let mut result = status_result(instance_id, self.runner_status(&runner, instance_id));
        if let Some(map) = result.as_object_mut() {
            map.insert("changed".to_string(), json!(changed));
            map.insert("was_running".to_string(), json!(was_running));
        }
        Ok(result)
    }

    /// `relaunch_command` — `{ instance_id, env? }` →
    /// `{ instance_id, running, was_running, restarted, state, ... }`. Delegates to
    /// `CommandRunner::relaunch_with_env` (stop-then-start if running, else a direct
    /// start); never leaves two live processes. Resolves command + cwd like
    /// `bridge::command_relaunch`. Unknown instance → `invalid_id` before any spawn.
    ///
    /// **Restart semantics (R-WSCMD #5):** `relaunch_command` is the EXPLICIT restart —
    /// it ALWAYS spawns a fresh process (in contrast to a second `start_command`, which
    /// is a no-op on a running instance). So the ack reports `restarted:true`, and
    /// `was_running` reports whether a live process was stopped first.
    ///
    /// **Per-run env (R-WSCMD #7):** the OPTIONAL `env` map is MERGED onto the inherited
    /// environment for the fresh run, plumbed to the runner spawn; secret VALUES are
    /// never logged. Because relaunch always re-spawns, the env IS applied (unlike a
    /// no-op start).
    fn relaunch_command(&self, args: &Value) -> Result<Value, RpcError> {
        let instance_id = require_str(args, "instance_id")?;
        let env = optional_env(args, "env")?;
        let (command, cwd) = self.resolve_command_and_cwd(instance_id)?;
        let runner = self.runner()?;
        let outcome = runner
            .relaunch_with_env(instance_id, &command, Some(&cwd), &env)
            .map_err(|e| RpcError::new(RpcCode::Internal, format!("relaunch failed: {e}")))?;
        // Explicit mutation ack (R-WSCMD #4/#5): a relaunch always restarts
        // (`restarted:true`); `was_running` reports whether a live process was stopped.
        let mut result = status_result(instance_id, self.runner_status(&runner, instance_id));
        if let Some(map) = result.as_object_mut() {
            map.insert("was_running".to_string(), json!(outcome.was_running));
            map.insert("restarted".to_string(), json!(true));
        }
        Ok(result)
    }

    /// `get_command_output` — `{ instance_id | (name, workspace_id), tail_bytes?,
    /// since?, max_bytes?, strip_ansi?, grep?, tail_lines? }` →
    /// `{ instance_id, output, total_bytes, returned_bytes, truncated, cursor,
    ///   state, running, finished, exit_code }`.
    ///
    /// The source mirrors `bridge::command_output` (ADR-0003 D7): the runner's LIVE
    /// in-memory tail while running, else the persisted scrollback rehydrated from the
    /// DB. The full text is then BOUNDED to a window by [`bound_output`] so the tool
    /// never pushes the whole scrollback. `since` (a byte offset returned as the
    /// previous call's `cursor`) supports incremental polling.
    ///
    /// **Token-safety (review R-OUTPUT):** the default `tail_bytes` is the token-safe
    /// [`DEFAULT_TAIL_BYTES`] (12 KiB) — small enough to fit an agent's MCP budget on a
    /// default read of a busy dev server — and `strip_ansi` defaults to **`true`**. When
    /// `strip_ansi` is true the returned `output` is the SINGLE cleaned view (the window
    /// run through [`strip_ansi`]); there is NO parallel raw `output`+`text` pair (the
    /// old duplication doubled the payload). The RAW window is returned in `output` only
    /// when `strip_ansi:false`. Either way `cursor`/`total_bytes` are computed on the RAW
    /// bytes, so the byte cursor/round-trip stays exact regardless of stripping.
    ///
    /// **Line modes (review R-OUTPUT, task #4):** `grep` (a regex, matched on the
    /// ANSI-stripped text) returns only the matching lines; `tail_lines` keeps the last N
    /// lines of the (stripped) window as an alternative to the byte window. Both are
    /// token-safe (bounded by the byte window first) and leave the byte cursor intact.
    /// The `name` form (finding #16) resolves the instance like `start_command`.
    fn get_command_output(&self, args: &Value) -> Result<Value, RpcError> {
        let instance_id = self.resolve_instance_id(args)?;
        let instance_id = instance_id.as_str();
        // Token-safe windowing knobs (`tail_bytes`/`max_bytes`/`since`/`strip_ansi`),
        // parsed + ceiling-checked by the SHARED helper so the contract (default sizes,
        // the 1 MiB cap, min(tail,max), output_too_large) is identical across the three
        // output flows. `effective_tail = min(tail_bytes, max_bytes)`.
        let knobs = parse_window_knobs(args)?;
        let since = knobs.since;
        let strip = knobs.strip;
        // Line modes (task #4): optional regex `grep` (matched on the stripped text) and
        // a `tail_lines` line-window. Both are applied to the rendered text AFTER the
        // byte window, so they never widen the token cost beyond the byte bound.
        let grep = optional_regex(args, "grep")?;
        let tail_lines = optional_usize(args, "tail_lines")?;
        // The bounded inter-run selector (review 01KV90QCKZ8BXZ4DTYZRJK56EZ): default =
        // the CURRENT/latest run; `run=-1` or `run="previous"` reads the ONE retained
        // prior run (bounded N=1). An out-of-range run → `invalid_argument`.
        let run = optional_run_selector(args, "run")?;
        // `mark_read` (PRD-4.1 task #5): an EXPLICIT, opt-in consumption flag (default
        // FALSE) so passive polling never consumes the `unread` notification. When true,
        // the read ALSO acknowledges the instance's unseen result — flipping `unread` to
        // false EXACTLY as a UI acknowledge does (the SHARED `acknowledge_unread` core) —
        // after the output window is computed, so the read still returns the output it
        // observed. A default (or false) read is purely observational and leaves `unread`
        // intact. Only meaningful for the CURRENT run (the `previous` selector reads a
        // settled prior run that carries no live `unread`).
        let mark_read = optional_bool(args, "mark_read")?.unwrap_or(false);

        // Resolve the output text + status for the SELECTED run. The previous-run
        // selector is served entirely from the persisted `prev_*` columns (it is, by
        // construction, a finished run — never live), so it never reads the runner's
        // live buffer; the current run keeps the existing live-then-cold path. The
        // runner is acquired only when the Current branch needs the LIVE tail, so a bad
        // instance_id surfaces its actionable `invalid_id` (from the cold DB read) ahead
        // of `mcp_unavailable`, matching the action tools' error precedence.
        let (full, status, previous) = match run {
            RunSelector::Previous => {
                let db = self.db()?;
                let inst = match db
                    .with_conn(|c| db::get_instance(c, instance_id))
                    .map_err(internal_db("read command output"))?
                {
                    Some(inst) => inst,
                    // Disambiguate a template command_id from an unknown id (finding
                    // #14), same as the action tools.
                    None => return Err(self.bad_instance_id_error(instance_id)),
                };
                // No prior run retained yet (idle never-run, or only one run so far):
                // an explicit empty window + a null prior outcome, NOT an error — the
                // agent asked a valid question that simply has no answer yet.
                let prev_state = inst.prev_last_state.as_deref().map(RunState::from_db_str);
                let status =
                    status_json(prev_state.unwrap_or(RunState::Idle), inst.prev_exit_code, false);
                (inst.prev_scrollback, status, true)
            }
            RunSelector::Current => {
                // Live path: a running instance returns the runner's in-memory tail. A
                // missing runner (very early boot) degrades to the cold DB path instead
                // of failing the read.
                let runner = self.runner().ok();
                let live = runner.as_ref().and_then(|r| r.live_output(instance_id));
                if let Some(live) = live {
                    // Running: the live tail + the runner-backed status (a no-DB fast path
                    // in `factual_status` when the entry is live).
                    let status = self
                        .factual_status(instance_id)
                        .unwrap_or_else(|_| status_json(RunState::Idle, None, false));
                    (live, status, false)
                } else {
                    // Cold path: idle/success/error (or absent live map) rehydrates the
                    // persisted scrollback row ONCE and derives the run status from that
                    // SAME row — unless the runner still holds a finished-this-session
                    // outcome, which is preferred (it carries the authoritative in-memory
                    // `unread` before persistence). Reusing the row we just loaded avoids
                    // the redundant re-read `factual_status` would do here (finding #13).
                    // An unknown instance → `invalid_id` (disambiguates a template id).
                    let db = self.db()?;
                    let inst = match db
                        .with_conn(|c| db::get_instance(c, instance_id))
                        .map_err(internal_db("read command output"))?
                    {
                        Some(inst) => inst,
                        None => return Err(self.bad_instance_id_error(instance_id)),
                    };
                    // Surface the run status alongside the output (finding #13 + v4): an
                    // agent reading output also needs to know if the command is still
                    // `running` or `finished` and, if finished, whether it crashed
                    // (`exit_code ≠ 0`) and whether the UI has acknowledged it (`unread`).
                    let status = match runner.as_ref().and_then(|r| r.outcome(instance_id)) {
                        Some((state, exit_code, unread)) => status_json(state, exit_code, unread),
                        None => status_json(
                            RunState::from_db_str(&inst.last_state),
                            inst.last_exit_code,
                            inst.unread,
                        ),
                    };
                    (inst.scrollback, status, false)
                }
            }
        };

        // `since` is a CURRENT-run byte cursor. The previous-run buffer is a DIFFERENT
        // byte stream, so applying a current-run cursor to it would slice at a meaningless
        // offset — typically reporting the prior run as empty (or spuriously "reset"). A
        // previous read is therefore a one-shot bounded tail; `since` is ignored for it
        // (cursors are only meaningful within the current run, per the mark_read note).
        let effective_since = if previous { None } else { since };
        let window = bound_output(&full, knobs.effective_tail, effective_since);
        // Render the `output` field per the token-safe contract (review R-OUTPUT):
        // strip_ansi=true → ONE cleaned view (no raw output+text duplication);
        // strip_ansi=false → the raw byte window. `grep`/`tail_lines` further reduce
        // the rendered text to matching/last-N lines. `cursor`/`total_bytes`/`returned_
        // bytes`/`truncated` stay computed on the RAW bytes so the byte cursor is exact.
        let output = render_output(&window.output, strip, grep.as_ref(), tail_lines, window.head_truncated);
        let mut result = json!({
            "instance_id": instance_id,
            // Echo which run this window is for, so a polling agent can tell a
            // `run="previous"` read apart from the default current-run read.
            "run": if previous { "previous" } else { "current" },
            "output": output,
            "total_bytes": window.total_bytes,
            "returned_bytes": window.returned_bytes,
            "truncated": window.truncated,
            // `reset` is true when the supplied `since` was beyond the current end —
            // the buffer shrank (a `clear`/reset) so the prior cursor is stale; the
            // window then carries the fresh content rather than an empty result.
            "reset": window.reset,
            // `cursor` is an integer byte offset (one past the end of what was
            // returned), so it round-trips verbatim as the next `since` — both are
            // integers per ADR-0003 §7 and the advertised descriptor schema. It is the
            // RAW-byte cursor even when `output` is stripped/grepped, so a follow-up
            // get_command_output(since=cursor) resumes with no gap/dup.
            "cursor": window.cursor,
        });
        // Splat the `{ state, running, finished, exit_code }` status fields in.
        if let (Some(map), Some(status_map)) = (result.as_object_mut(), status.as_object()) {
            for (k, v) in status_map {
                map.insert(k.clone(), v.clone());
            }
        }
        // Explicit consumption (task #5): only when `mark_read:true` AND this is the
        // CURRENT run (a `previous`-selector read targets a settled prior run with no live
        // `unread` to consume). The acknowledge reuses the SHARED `bridge::acknowledge_unread`
        // — the IDENTICAL operation the UI's `command_acknowledge` runs — so it clears only
        // `unread` (+ emits `command://ack`) and NEVER erases the factual outcome the read
        // just reported. The `unread` field already splatted into `result` reflects the
        // PRE-mark snapshot, which is the honest answer for "what did this read observe?".
        if mark_read && !previous {
            // Best-effort consumption, SYMMETRIC to the passive read above: a missing
            // runner/db STATE (e.g. very early boot) must not fail a read that already
            // produced output — the passive path uses `self.runner().ok()`, so the
            // consuming path degrades the same way instead of propagating with `?`
            // (finding #12). When the state IS present, a genuine DB error during the
            // acknowledge still propagates, exactly as the passive read's DB reads do.
            if let (Ok(runner), Ok(db)) = (self.runner(), self.db()) {
                crate::bridge::acknowledge_unread(&self.app, &db, &runner, instance_id)
                    .map_err(internal_db("mark command output read"))?;
            }
        }
        Ok(result)
    }

    /// `wait_for_command` (PRD-4 dogfood, ADR-0003 D12) —
    /// `{ instance_id, until?: string[], timeout_ms?: number, since?: number,
    ///   tail_bytes?: number, max_bytes?: number, strip_ansi?: bool }` →
    /// `{ instance_id, resolved, state, exit_code, ended_at, waited_ms, cursor,
    ///   output_tail }`.
    ///
    /// A BOUNDED long-poll: it returns at the FIRST of (a) the instance's FACTUAL state
    /// entering the `until` set → `resolved:true`, or (b) `timeout_ms` elapsing →
    /// `resolved:false` (a NORMAL result, NOT a protocol error — the agent re-polls
    /// with the returned `cursor`). It is purely OBSERVATIONAL: the wait only READS the
    /// runner/db state via the SAME paths as the v1 tools (`runner.outcome` → DB row),
    /// so any number of clients may wait the SAME instance concurrently, and waiting
    /// NEVER acknowledges / clears the `unread` flag (waiting ≠ acknowledging).
    ///
    /// **Token-safe `output_tail` (review R-OUTPUT, D12):** on the FIRST call (no
    /// `since`), `since` DEFAULTS to the current end-of-buffer captured BEFORE the wait,
    /// so `output_tail` carries only the output produced AFTER the call — not the entire
    /// pre-existing scrollback (which, on a busy dev server, was an ~80k-char token bomb
    /// for a supposedly light long-poll). A subsequent call passes the returned `cursor`
    /// back as `since` to keep streaming incrementally. `tail_bytes`/`max_bytes` bound the
    /// window exactly as `get_command_output` does, and `strip_ansi` (default `true`)
    /// runs `output_tail` through the SAME [`render_output`] cleaning, so the wait's
    /// output surface matches the read tool's.
    ///
    /// Bounding is enforced by [`poll_until`]: `timeout_ms` defaults to
    /// [`DEFAULT_WAIT_TIMEOUT_MS`] and is clamped to [`crate::command::WAIT_MAX_TIMEOUT`]
    /// (~60 s), and the loop re-reads on a short [`WAIT_POLL_INTERVAL`] — never an
    /// infinite block. `until` aligns to the runner vocabulary (`idle`/`running`/
    /// `success`/`error`); `"exited"` is an alias for `success`+`error`; the default is
    /// the settled set `success`+`error`. The returned `cursor` is computed off the
    /// SAME `bound_output(since)` path as `get_command_output`, so an agent can chain
    /// `get_command_output(since=cursor)` with no gap or duplicate.
    fn wait_for_command(&self, args: &Value) -> Result<Value, RpcError> {
        let instance_id = require_str(args, "instance_id")?.to_string();
        // Validate the id up front (same actionable template-vs-instance error as the
        // action tools) so waiting on a bad id fails fast rather than spinning.
        self.assert_instance_exists(&instance_id)?;
        let until = parse_until(args)?;
        // Default 30 s, clamped to the ~60 s hard ceiling so the wait is bounded.
        let timeout_ms = optional_u64(args, "timeout_ms")?.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
        let timeout = Duration::from_millis(timeout_ms).min(WAIT_MAX_TIMEOUT);
        // Token-safe windowing knobs, identical to get_command_output (D7/R-OUTPUT),
        // parsed + ceiling-checked by the SHARED helper. `since` is taken un-resolved
        // here because wait supplies its own first-call default below.
        let knobs = parse_window_knobs(args)?;
        let strip = knobs.strip;
        let effective_tail = knobs.effective_tail;

        // FIRST-CALL BOUNDING (D12): when the caller passes no `since`, default it to the
        // CURRENT end-of-buffer captured BEFORE the wait, so `output_tail` returns only
        // the bytes produced AFTER this call — never the whole pre-existing scrollback.
        // A caller resuming a poll passes the prior `cursor` back as `since` and that
        // takes precedence. Measuring on the same `current_output` source the result
        // window reads keeps the byte cursor consistent.
        let since = match knobs.since {
            Some(s) => s,
            None => self.current_output(&instance_id)?.len(),
        };

        // The BOUNDED, observational poll: re-read the FACTUAL state each interval and
        // resolve as soon as it enters `until`, else give up at `timeout`. Reading via
        // `factual_state` reuses the runner→DB read path (no second source of truth),
        // and reads ONLY — it never mutates the runner or touches `unread`.
        let outcome = poll_until(
            &until,
            timeout,
            WAIT_POLL_INTERVAL,
            || self.factual_state(&instance_id),
            std::thread::sleep,
        );

        // Read the FACTUAL outcome ONCE after the wait (runner-first, DB fallback) for
        // the reported state/exit_code/ended_at. exit_code/ended_at are surfaced only
        // for a finished run, null otherwise. This read is observational too.
        let (state, exit_code, ended_at) = self.factual_outcome(&instance_id)?;
        let finished = matches!(state, RunState::Success | RunState::Error);

        // Compute the output_tail + cursor off the SAME bounded path as
        // get_command_output(since), so `cursor` chains there with no gap/dup. Live
        // tail while running, else the persisted current scrollback. `output_tail` is
        // rendered through the SAME token-safe path (strip/clean) as get_command_output.
        let full = self.current_output(&instance_id)?;
        let window = bound_output(&full, effective_tail, Some(since));
        let output_tail = render_output(&window.output, strip, None, None, window.head_truncated);

        Ok(json!({
            "instance_id": instance_id,
            // resolved:false is a NORMAL status (timeout), never a protocol error.
            "resolved": outcome.resolved,
            "state": state.as_db_str(),
            // exit_code / ended_at are the finished run's; null while idle/running.
            "exit_code": if finished { exit_code } else { None },
            "ended_at": if finished { ended_at } else { None },
            "waited_ms": outcome.waited.as_millis() as u64,
            // Chains verbatim into get_command_output(since=cursor).
            "cursor": window.cursor,
            // True when the scrollback shrank below our start cursor during the wait
            // (a `clear`): `output_tail` then carries the fresh content from the new start.
            "reset": window.reset,
            "output_tail": output_tail,
        }))
    }

    /// The FACTUAL current [`RunState`] of an instance: the live runner outcome when it
    /// backs the instance this session, else the persisted `last_state` from the DB row
    /// (so a run that finished before a restart still reports `success`/`error`). The
    /// per-iteration read of the `wait_for_command` poll — purely observational, the
    /// same runner→DB precedence as [`Self::factual_status`], and it NEVER mutates the
    /// runner or clears `unread`. A transient DB miss degrades to `Idle` rather than
    /// aborting the bounded wait.
    fn factual_state(&self, instance_id: &str) -> RunState {
        if let Ok(runner) = self.runner() {
            if let Some((state, _exit, _unread)) = runner.outcome(instance_id) {
                return state;
            }
        }
        match self.db() {
            Ok(db) => db
                .with_conn(|c| db::get_instance(c, instance_id))
                .ok()
                .flatten()
                .map(|inst| RunState::from_db_str(&inst.last_state))
                .unwrap_or(RunState::Idle),
            Err(_) => RunState::Idle,
        }
    }

    /// The FACTUAL outcome triple `(state, exit_code, ended_at)` for the wait result.
    /// Runner-first for the live `(state, exit_code)`, then the DB row supplies
    /// `ended_at` (and is the cold-path fallback for everything after a restart). Like
    /// [`Self::factual_status`] this is observational — it never acknowledges.
    fn factual_outcome(
        &self,
        instance_id: &str,
    ) -> Result<(RunState, Option<i32>, Option<i64>), RpcError> {
        let runner = self.runner()?;
        let live = runner.outcome(instance_id);
        // `ended_at` is not held in memory by the runner, so read the DB row for it
        // (and as the cold-path source of state/exit_code when there is no live entry).
        let db = self.db()?;
        let inst = db
            .with_conn(|c| db::get_instance(c, instance_id))
            .map_err(internal_db("read command outcome"))?;
        match live {
            Some((state, exit_code, _unread)) => {
                // `ended_at` is persisted to the DB row asynchronously (the pump flips the
                // in-memory state BEFORE on_state persists ended_at), so a run that JUST
                // finished can be live-success here while the row's ended_at is still null.
                // Don't report a finished run with ended_at:null — fall back to "now" for a
                // settled state (a close approximation; the precise persisted timestamp
                // lands a moment later). A still-running run legitimately has no ended_at.
                let ended_at = inst.as_ref().and_then(|i| i.ended_at).or_else(|| {
                    matches!(state, RunState::Success | RunState::Error).then(db::now_millis)
                });
                Ok((state, exit_code, ended_at))
            }
            None => match inst {
                Some(inst) => Ok((
                    RunState::from_db_str(&inst.last_state),
                    inst.last_exit_code,
                    inst.ended_at,
                )),
                None => Ok((RunState::Idle, None, None)),
            },
        }
    }

    /// The CURRENT run's full output text (pre-bounding): the runner's live in-memory
    /// tail while running, else the persisted scrollback rehydrated from the DB row.
    /// The SAME source the current-run branch of [`Self::get_command_output`] uses, so
    /// the `wait_for_command` cursor lines up with a follow-up `get_command_output`.
    fn current_output(&self, instance_id: &str) -> Result<String, RpcError> {
        let runner = self.runner()?;
        if let Some(live) = runner.live_output(instance_id) {
            return Ok(live);
        }
        let db = self.db()?;
        let scrollback = db
            .with_conn(|c| db::get_instance(c, instance_id))
            .map_err(internal_db("read command output"))?
            .map(|inst| inst.scrollback)
            .unwrap_or_default();
        Ok(scrollback)
    }

    /// Resolve the target instance id for a command tool, accepting EITHER an explicit
    /// `instance_id` (the canonical path) OR `{ name, workspace_id }` (finding #16, the
    /// ergonomic shortcut so launching "dev" does not need a `list_commands` round-trip
    /// first). Rules:
    /// - `instance_id` present → used verbatim (the name form is ignored); existence is
    ///   validated downstream by `resolve_command_and_cwd`/the output read.
    /// - else `{ name, workspace_id }` → resolve to the single instance of that
    ///   workspace whose template `name` matches. No match → `invalid_id` naming the
    ///   name+workspace; MORE than one match → `invalid_state` (ambiguous) listing the
    ///   instance_ids so the agent can disambiguate by id.
    /// - neither → `invalid_argument`.
    ///
    /// Returns an owned `String` so the borrow of `args` does not leak into the runner
    /// calls. The `name` form never silently picks one of several matches (mirrors the
    /// D5 cwd-filter rule: the server filters, the agent disambiguates).
    fn resolve_instance_id(&self, args: &Value) -> Result<String, RpcError> {
        if let Some(instance_id) = optional_str(args, "instance_id")? {
            return Ok(instance_id.to_string());
        }
        let name = match optional_str(args, "name")? {
            Some(name) => name,
            None => {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    "provide instance_id, or { name, workspace_id } to resolve by name",
                ))
            }
        };
        let workspace_id = match optional_str(args, "workspace_id")? {
            Some(ws) => ws,
            None => {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    "resolving a command by name requires workspace_id alongside name",
                ))
            }
        };
        let db = self.db()?;
        let rows = db
            .with_conn(|c| db::list_instances_for_workspace(c, workspace_id))
            .map_err(internal_db("resolve command by name"))?;
        let mut matches = rows.into_iter().filter(|r| r.name == name);
        let first = matches.next().ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!("no command named '{name}' in workspace {workspace_id}"),
            )
        })?;
        // More than one instance shares the name → ambiguous; list the ids so the
        // agent re-calls with an explicit instance_id rather than us guessing one.
        if let Some(second) = matches.next() {
            let mut ids = vec![first.id, second.id];
            ids.extend(matches.map(|r| r.id));
            return Err(RpcError::new(
                RpcCode::InvalidState,
                format!(
                    "command name '{name}' is ambiguous in workspace {workspace_id} \
                     ({} instances: {}); pass an explicit instance_id",
                    ids.len(),
                    ids.join(", ")
                ),
            ));
        }
        Ok(first.id)
    }

    /// Resolve an instance's command line + run cwd (the SAME logic as
    /// `bridge::resolve_command_and_cwd`): the template `command` and the workspace
    /// path joined with the VALIDATED subfolder. Maps to the D8 vocabulary — an
    /// unknown instance is `invalid_id`, an invalid/missing subfolder is
    /// `invalid_argument` — and errors BEFORE any spawn.
    fn resolve_command_and_cwd(&self, instance_id: &str) -> Result<(String, String), RpcError> {
        let db = self.db()?;
        let ctx = db
            .with_conn(|c| db::instance_run_context(c, instance_id))
            .map_err(internal_db("resolve command"))?;
        let ctx = match ctx {
            Some(ctx) => ctx,
            // No instance with this id: it may be a TEMPLATE command_id (a common
            // confusion — finding #14). Disambiguate the error so the agent knows
            // which id to pass instead of just "unknown".
            None => return Err(self.bad_instance_id_error(instance_id)),
        };
        let cwd = crate::subfolder::resolve_run_dir(&ctx.workspace_path, ctx.subfolder.as_deref())
            .map_err(|e| RpcError::new(RpcCode::InvalidArgument, e))?;
        Ok((ctx.command, cwd))
    }

    /// Build the `invalid_id` error for an id that is NOT a launchable instance
    /// (finding #14). `command_id` (templates) and `instance_id` are both UUID-shaped,
    /// and only `instance_id` is pilotable, so agents routinely pass a template
    /// `command_id` from `list_commands(project_id=…)` to `start_command`. When the id
    /// turns out to be a known template, the message NAMES the correct path
    /// (`list_commands(workspace_id=…)`) instead of a bare "unknown"; otherwise it is
    /// the generic unknown-id error. A DB hiccup degrades to the generic error rather
    /// than masking the real failure.
    fn bad_instance_id_error(&self, id: &str) -> RpcError {
        let is_template = self
            .db()
            .ok()
            .and_then(|db| db.with_conn(|c| db::get_template(c, id)).ok())
            .flatten()
            .is_some();
        if is_template {
            RpcError::new(
                RpcCode::InvalidId,
                format!(
                    "'{id}' is a command TEMPLATE id (command_id), which is not launchable. \
                     Pass an instance_id from list_commands(workspace_id=…) — command_id names \
                     a project template, instance_id names a workspace's launchable instance."
                ),
            )
        } else {
            RpcError::new(
                RpcCode::InvalidId,
                format!(
                    "unknown command instance {id} (if this is a command_id from \
                     list_commands(project_id=…), pass instead an instance_id from \
                     list_commands(workspace_id=…))"
                ),
            )
        }
    }

    /// `probe` (PRD-4 #7 spike, ADR-0004) — `{}` → `{ ok: true, server, version }`.
    /// A trivial no-op liveness tool: it deliberately touches NO managed state, so it
    /// answers even while the Db/runner are still warming up or unreachable. This is
    /// exactly what a `SessionStart` `mcp_tool` hook needs — "is nyx's MCP
    /// surface up?" — without depending on PRD-2/PRD-3 being fully initialized. Its
    /// trivial-ok contract is the minimal `mcp_tool` payload PRD 5 would build on.
    fn probe(&self) -> Result<Value, RpcError> {
        // D1: include schema health in the probe result so a client can tell whether
        // the schema is in a good state. `ok` stays `true` — the probe is a liveness
        // check that must NOT fail on a warm-but-schema-lagging runtime — but
        // `schema_ok` signals any pending migrations. If the Db is not yet managed
        // (e.g. very early boot), we report schema_ok: true (no evidence of a problem)
        // rather than blocking the liveness check on managed state.
        // Compute the full health snapshot once (when the Db is managed) so we can
        // both gate `schema_ok` and surface the pending-migration count in the result.
        let health = self.db().ok().map(|db| db.with_conn(db::schema_health));
        let schema_ok = health.as_ref().map(|h| h.up_to_date).unwrap_or(true);
        let mut result = json!({
            "ok": true,
            "server": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            // Short git SHA injected at build time by build.rs (best-effort; "unknown"
            // when git is unavailable at build time, e.g. a clean CI checkout without .git).
            // INTACT — no `-dirty` suffix (that would corrupt the sha); dirtiness is the
            // separate `build_dirty` boolean below (PRD-4.1 task #6).
            "build_sha": env!("NYX_BUILD_SHA"),
            // Whether the working tree was DIRTY when this binary was built (baked by
            // build.rs from `git status --porcelain`). A clean committed build reports
            // false; a build off an uncommitted working tree reports true — so an agent
            // can tell a reproducible release build from a local dev build WITHOUT the
            // `build_sha` ever being polluted. Parsed from the baked "true"/"false" env.
            "build_dirty": env!("NYX_BUILD_DIRTY") == "true",
            // D1 schema health: false when pending migrations are detected. A probe
            // returning schema_ok:false means the binary was upgraded without running
            // migrations — callers should not rely on any data-dependent tools.
            "schema_ok": schema_ok,
        });
        if !schema_ok {
            // Surface a clear warning so the agent / operator knows why tools fail.
            // `pending_count` is `Some(n)` for a real pending count, or `None` when the
            // health check itself failed — surfaced as a distinct `schema_check_failed`
            // flag rather than an absurd sentinel count on the wire.
            if let Some(map) = result.as_object_mut() {
                map.insert(
                    "schema_warning".to_string(),
                    json!("schema has pending migrations — restart nyx to apply them"),
                );
                match health.as_ref().and_then(|h| h.pending_count) {
                    Some(count) => {
                        map.insert("pending_migrations".to_string(), json!(count));
                    }
                    None => {
                        map.insert("schema_check_failed".to_string(), json!(true));
                    }
                }
            }
        }
        Ok(result)
    }

    /// `agent_session_event` — the Claude Code SessionStart/SessionEnd channel
    /// (PRD-5 #4, ADR-0004 / ADR-0010). A `mcp_tool` (or best-effort `command`) hook
    /// forwards its raw payload here; nyx normalizes it through the `claude_code`
    /// adapter ([`AgentRegistry`]) and persists the `agent_sessions` row:
    ///
    ///   - **SessionStart** → [`db::record_session_start`]: UPSERT the single ACTIVE
    ///     session for `(terminal_id, agent_kind)`, capturing `external_session_id`,
    ///     `cwd`, `transcript_path`, `metadata_json` (at least `source`) and
    ///     refreshing `last_seen_at = now`. The session anchors to the terminal's
    ///     workspace (so the project is derivable, ADR-0010) — read live from the
    ///     terminal row, NOT trusted from the hook payload.
    ///   - **SessionEnd** → [`db::mark_session_ended`]: mark the matching ACTIVE
    ///     session `ended` (the clean-end case). SQLite stays the authority: a brutal
    ///     kill never fires SessionEnd, and the row simply stays `active` (resumable)
    ///     until the staleness sweep — nyx never depends on SessionEnd for cleanup.
    ///
    /// Correlation is by `NYX_TERMINAL_ID` (the terminal record id nyx exports into
    /// the shell). An unknown terminal → `invalid_id`; a payload that is not a
    /// recognizable session event (or lacks a `session_id`) → `invalid_argument`.
    /// Like [`Self::probe`], a not-yet-warm runtime degrades to `mcp_unavailable`
    /// (via [`Self::db`]) so a best-effort hook never wedges the agent's startup.
    fn agent_session_event(&self, args: &Value) -> Result<Value, RpcError> {
        // The agent kind selects the adapter; default to claude_code (the only v1
        // production adapter, and the only one that installs this hook).
        let agent_kind = optional_str(args, "agent_kind")?.unwrap_or(db::AGENT_KIND_CLAUDE_CODE);
        let registry = AgentRegistry::default();
        let adapter = registry.get(agent_kind).ok_or_else(|| {
            RpcError::new(RpcCode::InvalidArgument, format!("unknown agent_kind '{agent_kind}'"))
        })?;

        // The terminal correlation id is required: without it nyx cannot bind the
        // session to a terminal (and resume would have no shell to respawn into).
        let terminal_id = require_str(args, "NYX_TERMINAL_ID")?;

        // Normalize the raw hook payload through the adapter (start / end / None).
        let event = adapter.parse_event(args).ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidArgument,
                "payload is not a recognizable agent session event (need hook_event_name + session_id)",
            )
        })?;

        let db = self.db()?;
        match event {
            AgentEvent::Start(start) => {
                // Resolve the terminal's CURRENT workspace anchor live from the row —
                // the project is derived from it (ADR-0010), and the hook must not be
                // trusted to assert a workspace. An unknown terminal → invalid_id.
                let session = db
                    .with_conn(|c| -> Result<Option<db::AgentSession>, diesel::result::Error> {
                        let Some(terminal) = db::get_terminal(c, terminal_id)? else {
                            return Ok(None);
                        };
                        let capture = db::SessionCapture {
                            workspace_id: terminal.workspace_id,
                            external_session_id: start.external_session_id,
                            cwd: start.cwd,
                            transcript_path: start.transcript_path,
                            metadata_json: start.metadata_json,
                        };
                        let row = db::record_session_start(c, terminal_id, agent_kind, capture)?;
                        Ok(Some(row))
                    })
                    .map_err(internal_db("record agent session start"))?;
                let Some(session) = session else {
                    return Err(RpcError::new(
                        RpcCode::InvalidId,
                        format!("unknown terminal {terminal_id}"),
                    ));
                };
                // A new ACTIVE session exists → the sidebar should swap this terminal's
                // row to the agent's icon (finding #55). Emit the coalescing refresh so
                // the front re-pulls `agent_active_sessions`.
                crate::bridge::emit_agent_sessions_changed(&self.app);
                Ok(json!({
                    "event": "SessionStart",
                    "session_id": session.id,
                    "terminal_id": session.terminal_id,
                    "agent_kind": session.agent_kind,
                    "external_session_id": session.external_session_id,
                    "state": session.state,
                    "workspace_id": session.workspace_id,
                }))
            }
            AgentEvent::End(end) => {
                // Find the ACTIVE session for this terminal+agent and, only when its
                // external id matches the ended one, mark it ended. A mismatch (or no
                // active row) is reported as ended:false — SQLite is the authority, so
                // a stray/duplicate end never corrupts state.
                let outcome = db
                    .with_conn(|c| -> Result<Option<(String, bool)>, diesel::result::Error> {
                        let Some(active) = db::active_session_for(c, terminal_id, agent_kind)? else {
                            return Ok(None);
                        };
                        if active.external_session_id != end.external_session_id {
                            // The active session is a different one — do not end it.
                            return Ok(Some((active.id, false)));
                        }
                        db::mark_session_ended(c, &active.id)?;
                        Ok(Some((active.id, true)))
                    })
                    .map_err(internal_db("end agent session"))?;
                match outcome {
                    Some((session_id, true)) => {
                        // The active session ended → the terminal row reverts to the
                        // generic terminal icon (finding #55). Refresh the sidebar.
                        crate::bridge::emit_agent_sessions_changed(&self.app);
                        Ok(json!({
                            "event": "SessionEnd",
                            "session_id": session_id,
                            "terminal_id": terminal_id,
                            "ended": true,
                        }))
                    }
                    Some((session_id, false)) => Ok(json!({
                        "event": "SessionEnd",
                        "session_id": session_id,
                        "terminal_id": terminal_id,
                        "ended": false,
                        "reason": "active session id does not match the ended session",
                    })),
                    None => Ok(json!({
                        "event": "SessionEnd",
                        "terminal_id": terminal_id,
                        "ended": false,
                        "reason": "no active session for this terminal",
                    })),
                }
            }
        }
    }

    /// Confirm `instance_id` names a real instance, mapping an unknown id to
    /// `invalid_id`. Used by tools (e.g. `stop_command`) whose runner call is
    /// idempotent on an absent instance and so would otherwise silently succeed. An
    /// unknown id gets the disambiguating template-vs-instance error (finding #14).
    fn assert_instance_exists(&self, instance_id: &str) -> Result<(), RpcError> {
        let db = self.db()?;
        let found = db
            .with_conn(|c| db::get_instance(c, instance_id))
            .map_err(internal_db("read command instance"))?
            .is_some();
        if found {
            Ok(())
        } else {
            Err(self.bad_instance_id_error(instance_id))
        }
    }

    // --- command CRUD tools (PRD-4 dogfood, review 01KV9614CHC4092P05DV9R5KPG) ---
    //
    // The MUTATING command tools the read/lifecycle v1 surface lacked. Each delegates
    // to the EXISTING PRD-3 layer the UI's bridge commands drive — NO parallel command
    // logic (ADR-0003 D6/D13): `add_command` reuses `bridge::infer_command_source` +
    // `db::create_template` (the `command_create` path); `update_command` reuses
    // `bridge::command_detaches_source` + `db::update_template`/`set_template_source`
    // (the `command_update` path); `import_commands` reuses
    // `pkgjson::discover_package_scripts` + `pkgjson::import_command` (the
    // `command_import_scripts`/`command_import_create` path). Explicit ids, D8 errors.

    /// `add_command` — `{ project_id, name, command, subfolder? }` → `{ command }`.
    /// Create a per-project command TEMPLATE via the SAME path as the UI's
    /// `bridge::command_create`: infer package.json provenance for a PM-invocation
    /// command line (reusing [`crate::bridge::infer_command_source`]), then
    /// [`db::create_template`] (which materializes one instance per existing workspace
    /// of the project). A name already used in the project surfaces as the D8
    /// vocabulary (`invalid_state`); an unknown project (FK) as `invalid_id`.
    fn add_command(&self, args: &Value) -> Result<Value, RpcError> {
        let project_id = require_str(args, "project_id")?;
        let name = require_str(args, "name")?;
        let command = require_str(args, "command")?;
        let subfolder = optional_str(args, "subfolder")?;
        // Reuse the UI's provenance inference so a manually-added `pnpm dev` reads the
        // same as a detected one — NOT a parallel inference path (ADR-0003 D6).
        let (source_kind, package_manager) =
            crate::bridge::infer_command_source(command, None, None);
        let source = db::CommandSource {
            source_kind,
            source_package_json_path: None,
            source_script_name: None,
            source_script_command_snapshot: None,
            package_manager,
        };
        let db = self.db()?;
        match db.with_conn(|c| db::create_template(c, project_id, name, command, subfolder, source))
        {
            Ok(template) => {
                // A template was created (+ one instance per workspace materialized) →
                // emit the shared command-band refresh so the UI re-pulls WITHOUT a
                // manual reload. Same `commands://changed` signal the UI's own
                // `command_create` emits; only on a SUCCESSFUL mutation.
                crate::bridge::emit_commands_changed(&self.app);
                Ok(json!({ "command": template_json(&template) }))
            }
            Err(e) => Err(map_template_write_err(project_id, e)),
        }
    }

    /// `update_command` — `{ command_id, name?, command?, subfolder? }` → `{ command }`.
    /// Modify an existing TEMPLATE's editable fields via the SAME path as the UI's
    /// `bridge::command_update`: the current row supplies the value for every OMITTED
    /// field (so a partial update never blanks the others), the package.json
    /// source-detach rule ([`crate::bridge::command_detaches_source`]) runs when the
    /// `command` drifts from its canonical call, and the write is [`db::update_template`]
    /// (+ [`db::set_template_source`] on detach). Refused while any instance is running
    /// (`invalid_state`) — exactly like the UI guards an edit of a live service.
    fn update_command(&self, args: &Value) -> Result<Value, RpcError> {
        let command_id = require_str(args, "command_id")?;
        // Partial-update fields: a present value overrides, an absent one keeps current.
        let new_name = optional_str(args, "name")?;
        let new_command = optional_str(args, "command")?;
        // `subfolder` is tri-state: absent = keep; "" = clear to root; value = set.
        // optional_str maps "" → None, so distinguish "absent" from "present empty".
        let subfolder_present = args.get("subfolder").map(|v| !v.is_null()).unwrap_or(false);
        let new_subfolder = optional_str(args, "subfolder")?;

        let db = self.db()?;
        // Guard: refuse the edit while any of the template's instances is running, the
        // SAME precondition the UI's command_update enforces (the user must stop the
        // service before editing what affects its runtime). Reuse the runner outcome.
        self.assert_template_not_running(command_id)?;

        let updated = db
            .with_conn(|c| -> Result<Option<db::ManagedCommand>, diesel::result::Error> {
                let Some(current) = db::get_template(c, command_id)? else {
                    return Ok(None);
                };
                // Fill omitted fields from the current row (partial update semantics).
                let name = new_name.unwrap_or(current.name.as_str());
                let command = new_command.unwrap_or(current.command.as_str());
                let subfolder: Option<&str> = if subfolder_present {
                    new_subfolder // present: set (or clear when "")
                } else {
                    current.subfolder.as_deref() // absent: keep current
                };
                // Source-detach: only when the template IS sourced and the (possibly
                // new) command drifts from BOTH the runner call and the raw snapshot —
                // the IDENTICAL rule as bridge::command_update.
                let detach = current.source_script_name.is_some()
                    && crate::bridge::command_detaches_source(&current, command);
                db::update_template(c, command_id, name, command, subfolder)?;
                if detach {
                    db::set_template_source(c, command_id, db::CommandSource::default())?;
                }
                // Return the fresh row so the result reflects the persisted state.
                db::get_template(c, command_id)
            })
            .map_err(map_template_write_err_generic)?;
        match updated {
            Some(template) => {
                // A template was modified → emit the shared command-band refresh so the
                // UI re-pulls to the new fields. Same `commands://changed` signal the
                // UI's own `command_update` emits; only on a SUCCESSFUL update.
                crate::bridge::emit_commands_changed(&self.app);
                Ok(json!({ "command": template_json(&template) }))
            }
            // No row with this id: it may be an instance_id (the inverse confusion of
            // the action tools). Surface an actionable invalid_id.
            None => Err(self.bad_command_id_error(command_id)),
        }
    }

    /// `import_commands` — `{ project_id?, workspace_id?, names? }` →
    /// `{ imported: [...], skipped: [...] }`. Import the project's package.json scripts
    /// as templates, reusing the EXISTING import logic with NO parallel discovery:
    /// [`crate::pkgjson::discover_package_scripts`] per workspace +
    /// [`crate::pkgjson::import_command`] per script (the SAME calls
    /// `command_import_scripts`/`command_import_create` make). A script whose proposed
    /// name is already used in the project is SKIPPED (reported, not an error) — the
    /// import is idempotent and re-runnable. `project_id` scans every workspace;
    /// `workspace_id` scans that single one (and resolves its project from the row).
    ///
    /// **B1 — selective import**: the optional `names` array filters which scripts to
    /// import. A `names` entry matches a script by its PROPOSED NAME (`pkg:script` in a
    /// multi-package repo) OR its RAW `script_name` (the bare `build`), so `names:["build"]`
    /// matches a `build` script in EVERY package even when the proposed name is prefixed
    /// (R-IMPORT #2 — before, only the prefixed name matched and `build` silently matched
    /// nothing). Scripts discovered but NOT requested are silently bypassed (not in
    /// `skipped` — the caller excluded them on purpose). Any REQUESTED name that matches
    /// NO discovered script is reported in `skipped` with `reason:"not_found"` so the
    /// agent can tell "not found" from "already imported". Absent or null `names` → full
    /// import (backwards-compatible default).
    ///
    /// **R-IMPORT #3 — discovery summary**: the result carries `manifests_found` (the
    /// count of `package.json` files the filtered, monorepo-aware discovery retained
    /// across the scanned workspace[s]). When it is `0`, a `skipped` entry with
    /// `reason:"no_manifest"` is added so `{imported:[],skipped:[]}` is no longer a mute
    /// "found nothing" indistinguishable from "all already imported".
    ///
    /// **R-IMPORT #4 — preview**: `preview:true` lists the discoverable scripts (name,
    /// package, script_name, body) WITHOUT creating any template (no DB write, no event).
    fn import_commands(&self, args: &Value) -> Result<Value, RpcError> {
        // B1: optional name-selection filter. `names` is an array of names to import;
        // absent/null = import everything (backward-compatible default).
        let name_filter: Option<std::collections::HashSet<String>> =
            optional_str_array(args, "names")?;
        // R-IMPORT #4: dry-run preview flag — list discoverable scripts, create nothing.
        let preview = optional_bool(args, "preview")?.unwrap_or(false);

        let (project_id, scripts, manifests_found) = self.discover_importable(args)?;

        // Track which requested `names` matched at least one discovered script (by
        // proposed name OR raw script_name), so an unmatched request becomes not_found.
        let mut matched_requests: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        let db = self.db()?;
        // In preview mode this collects the WOULD-import rows (no DB write); in a real
        // import it collects the ACTUAL created templates. The result field is named
        // accordingly below (`would_import` for a preview, `imported` for a real import)
        // so a preview never falsely claims it `imported` anything (PRD-4.1 task #6).
        let mut imported: Vec<Value> = Vec::new();
        let mut skipped: Vec<Value> = Vec::new();
        // De-duplicate by proposed name so two workspaces exposing the same script do not
        // both try to import it (the second would collide anyway).
        let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for script in &scripts {
            if !seen_names.insert(script.proposed_name.clone()) {
                continue; // already handled this proposed name in this run
            }
            // B1: when a name filter is active, a script is selected if its PROPOSED name
            // OR its RAW script_name appears in the filter (R-IMPORT #2). Record which
            // request strings matched so unmatched ones become not_found below.
            if let Some(ref filter) = name_filter {
                let by_proposed = filter.contains(&script.proposed_name);
                let by_raw = filter.contains(&script.script_name);
                if by_proposed {
                    matched_requests.insert(script.proposed_name.clone());
                }
                if by_raw {
                    matched_requests.insert(script.script_name.clone());
                }
                if !by_proposed && !by_raw {
                    continue; // not requested → silently bypassed
                }
            }
            // Preview mode: list, never create (R-IMPORT #4).
            if preview {
                imported.push(preview_script_json(script));
                continue;
            }
            let source = db::CommandSource {
                source_kind: Some(db::SOURCE_KIND_PACKAGE_JSON.to_string()),
                source_package_json_path: Some(script.package_json_path.clone()),
                source_script_name: Some(script.script_name.clone()),
                source_script_command_snapshot: Some(script.script_command_snapshot.clone()),
                package_manager: Some(script.package_manager.clone()),
            };
            // Reuse pkgjson::import_command — the EXACT path command_import_create takes
            // (name-collision check + db::create_template). A collision is a SKIP
            // (reported with reason:"already_exists"), never a hard error: the import
            // stays re-runnable.
            let result = db.with_conn(|c| {
                crate::pkgjson::import_command(
                    c,
                    &project_id,
                    &script.proposed_name,
                    &script.default_command,
                    &script.subfolder,
                    source,
                )
            });
            match result {
                Ok(template) => imported.push(template_json(&template)),
                Err(detail) => skipped.push(json!({
                    "name": script.proposed_name,
                    "script_name": script.script_name,
                    "reason": "already_exists",
                    "detail": detail,
                })),
            }
        }

        // R-IMPORT #2: any REQUESTED name (from the filter) that matched NO discovered
        // script is a not_found skip, so a typo'd or absent name is an explicit signal
        // rather than a silent miss.
        if let Some(ref filter) = name_filter {
            let mut not_found: Vec<&String> =
                filter.iter().filter(|n| !matched_requests.contains(*n)).collect();
            not_found.sort(); // stable order for deterministic results/tests
            for name in not_found {
                skipped.push(json!({
                    "name": name,
                    "reason": "not_found",
                }));
            }
        }

        // R-IMPORT #3: when the (filtered) discovery found NO manifest at all, say so
        // explicitly — distinct from "found manifests but all already imported".
        if manifests_found == 0 {
            skipped.push(json!({ "reason": "no_manifest" }));
        }

        // Emit the shared command-band refresh ONLY when at least one template was
        // actually imported (instances materialized) — a preview, a run that imports
        // nothing (all names skipped/not-found, or no scripts found) changed no row, so it
        // stays silent. Same `commands://changed` signal the UI's import path emits.
        if !preview && !imported.is_empty() {
            crate::bridge::emit_commands_changed(&self.app);
        }
        // PRD-4.1 task #6: a PREVIEW created nothing, so it reports its candidate rows
        // under `would_import` and leaves `imported` an EMPTY array (it truthfully imported
        // nothing); a REAL import reports its creations under `imported` and leaves
        // `would_import` empty. BOTH keys are ALWAYS present as arrays — the unused one is
        // `[]`, never absent — so a consumer that reads `result["imported"]` never gets
        // null, while `preview` plus the populated field still disambiguate the two cases
        // unambiguously. (Keeping the keys stable avoids breaking any reader of `imported`.)
        let (imported_rows, would_import_rows) = if preview {
            (json!([]), json!(imported))
        } else {
            (json!(imported), json!([]))
        };
        Ok(json!({
            "imported": imported_rows,
            "would_import": would_import_rows,
            "skipped": skipped,
            "manifests_found": manifests_found,
            "preview": preview,
        }))
    }

    /// Resolve the import target `(project_id, [workspace paths])` from EITHER a
    /// `workspace_id` (scan one) or a `project_id` (scan every workspace of the project),
    /// then run the FILTERED, monorepo-aware discovery ([`crate::pkgjson::discover_scripts`])
    /// across those paths. Returns `(project_id, discovered scripts, manifests_found)`.
    /// `manifests_found` sums the retained `package.json` count across the scanned
    /// workspaces (the R-IMPORT #3 discovery summary). Shared by `import_commands` and
    /// `list_importable_scripts` so both surfaces use the IDENTICAL discovery.
    fn discover_importable(
        &self,
        args: &Value,
    ) -> Result<(String, Vec<crate::pkgjson::DiscoveredScript>, usize), RpcError> {
        let db = self.db()?;
        let (project_id, workspace_paths): (String, Vec<String>) =
            match (optional_str(args, "workspace_id")?, optional_str(args, "project_id")?) {
                (Some(workspace_id), _) => {
                    let ws = db
                        .with_conn(|c| db::get_workspace(c, workspace_id))
                        .map_err(internal_db("resolve workspace for import"))?
                        .ok_or_else(|| {
                            RpcError::new(RpcCode::InvalidId, format!("unknown workspace {workspace_id}"))
                        })?;
                    (ws.project_id, vec![ws.path])
                }
                (None, Some(project_id)) => {
                    let workspaces = db
                        .with_conn(|c| db::list_workspaces(c, project_id))
                        .map_err(internal_db("list workspaces for import"))?;
                    if workspaces.is_empty() {
                        return Err(RpcError::new(
                            RpcCode::InvalidId,
                            format!(
                                "unknown project {project_id} (or it has no workspaces to scan)"
                            ),
                        ));
                    }
                    let paths = workspaces.into_iter().map(|w| w.path).collect();
                    (project_id.to_string(), paths)
                }
                (None, None) => {
                    return Err(RpcError::new(
                        RpcCode::InvalidArgument,
                        "import_commands requires project_id (scan all workspaces) or \
                         workspace_id (scan one)",
                    ))
                }
            };
        let mut scripts = Vec::new();
        let mut manifests_found = 0usize;
        for path in &workspace_paths {
            let result = crate::pkgjson::discover_scripts(path);
            manifests_found += result.manifests_found;
            scripts.extend(result.scripts);
        }
        Ok((project_id, scripts, manifests_found))
    }

    /// `list_importable_scripts` — `{ project_id? | workspace_id? }` →
    /// `{ scripts: [...], manifests_found }`. The READ-ONLY import-preview surface
    /// (R-IMPORT #5): the discoverable package.json scripts via the SAME filtered,
    /// monorepo-aware discovery [`Self::discover_importable`] uses, WITHOUT creating any
    /// template or emitting any event. Each entry carries the proposed `name`, its
    /// `package` (subfolder), the raw `script_name`, the script `body`, the runner
    /// `command` an import would create, and the `package_manager`. De-duplicated by
    /// proposed name (same as `import_commands`). `manifests_found:0` ⇔ no manifest found.
    fn list_importable_scripts(&self, args: &Value) -> Result<Value, RpcError> {
        let (_project_id, scripts, manifests_found) = self.discover_importable(args)?;
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let listed: Vec<Value> = scripts
            .iter()
            .filter(|s| seen.insert(s.proposed_name.as_str()))
            .map(preview_script_json)
            .collect();
        Ok(json!({ "scripts": listed, "manifests_found": manifests_found }))
    }

    /// Refuse a template edit while any of its instances is running — the SAME
    /// precondition `bridge::command_update` enforces via `guard_template_not_running`,
    /// reusing the IDENTICAL path: [`db::instance_ids_for_template`] +
    /// `ManagedCommandRunner::any_running` (the live runner is the source of truth for
    /// "running now"). An unknown `command_id` is NOT rejected here (the caller's
    /// `get_template` lookup handles that with the actionable error); this only blocks
    /// a live edit.
    fn assert_template_not_running(&self, command_id: &str) -> Result<(), RpcError> {
        let db = self.db()?;
        let instance_ids = db
            .with_conn(|c| db::instance_ids_for_template(c, command_id))
            .map_err(internal_db("read template instances"))?;
        let runner = self.runner()?;
        if runner.any_running(&instance_ids) {
            return Err(RpcError::new(
                RpcCode::InvalidState,
                format!(
                    "command {command_id} is running in at least one workspace; stop it \
                     before editing the command"
                ),
            ));
        }
        Ok(())
    }

    /// Build the `invalid_id` error for an id that is NOT a known TEMPLATE (the inverse
    /// of [`Self::bad_instance_id_error`]): the command-CRUD tools take a `command_id`,
    /// and an agent may pass a launchable `instance_id` by mistake. When the id turns
    /// out to be a known instance, the message NAMES the correct id; otherwise it is the
    /// generic unknown-template error.
    fn bad_command_id_error(&self, id: &str) -> RpcError {
        let is_instance = self
            .db()
            .ok()
            .and_then(|db| db.with_conn(|c| db::get_instance(c, id)).ok())
            .flatten()
            .is_some();
        if is_instance {
            RpcError::new(
                RpcCode::InvalidId,
                format!(
                    "'{id}' is a launchable INSTANCE id (instance_id), not a command TEMPLATE. \
                     Pass a command_id from list_commands(project_id=…) — add_command/\
                     update_command operate on the project template, not a workspace instance."
                ),
            )
        } else {
            RpcError::new(
                RpcCode::InvalidId,
                format!("unknown command template {id} (command_id from list_commands(project_id=…))"),
            )
        }
    }

    // --- A2: remove_workspace + remove_command (the D of CRUD) ----------

    /// `remove_workspace` — `{ workspace_id }` → `{}`. Delete a workspace and its
    /// command instances (ON DELETE CASCADE in the DB). Terminals bound to the workspace
    /// are DETACHED (SET NULL), not killed — they survive as loose terminals, same as the
    /// UI's project-delete behaviour. REFUSED while any instance of the workspace is
    /// running (the agent must stop services before deleting). Delegates to
    /// `db::delete_workspace` (no parallel lifecycle). Emits `workspaces://changed` on
    /// success so the sidebar re-pulls.
    fn remove_workspace(&self, args: &Value) -> Result<Value, RpcError> {
        let workspace_id = require_str(args, "workspace_id")?;
        let db = self.db()?;
        // Guard: refuse to delete a project's ROOT workspace. A project requires exactly
        // one root (idx_one_root_per_project) and the root anchors its folder; deleting it
        // would leave a rootless project husk (no path, list_workspaces empty, no way to
        // materialize instances). The UI has no per-workspace delete — only whole-project
        // delete — so this MCP tool is the only path that could reach a root row.
        let workspace = db
            .with_conn(|c| db::get_workspace(c, workspace_id))
            .map_err(internal_db("read workspace"))?;
        match workspace {
            None => {
                return Err(RpcError::new(
                    RpcCode::InvalidId,
                    format!("unknown workspace {workspace_id}"),
                ));
            }
            Some(ws) if ws.is_root => {
                return Err(RpcError::new(
                    RpcCode::InvalidState,
                    format!(
                        "workspace {workspace_id} is the project's root — it cannot be \
                         removed on its own; delete the whole project instead"
                    ),
                ));
            }
            Some(_) => {}
        }
        // Guard: refuse if any instance in this workspace is running — same as the
        // project-delete guard in bridge::delete_project.
        let instance_ids = db
            .with_conn(|c| db::instance_ids_for_workspace(c, workspace_id))
            .map_err(internal_db("read workspace instances"))?;
        let runner = self.runner()?;
        if runner.any_running(&instance_ids) {
            return Err(RpcError::new(
                RpcCode::InvalidState,
                format!(
                    "workspace {workspace_id} has a running command — stop it before \
                     removing the workspace"
                ),
            ));
        }
        // The instances cascade-deleted with the workspace — counted for the ack.
        let removed_instances = instance_ids.len();
        // Delete (cascade removes instances; SET NULL detaches terminals).
        let deleted = db
            .with_conn(|c| db::delete_workspace(c, workspace_id))
            .map_err(internal_db("delete workspace"))?;
        if deleted == 0 {
            return Err(RpcError::new(
                RpcCode::InvalidId,
                format!("unknown workspace {workspace_id}"),
            ));
        }
        crate::bridge::emit_workspaces_changed(&self.app);
        // Explicit mutation ack (R-WSCMD #4): `removed:true` + the count of command
        // instances that cascade-deleted with the workspace, so the agent gets a
        // confirmation with substance rather than a bare `{}`.
        Ok(json!({ "removed": true, "removed_instances": removed_instances }))
    }

    /// `remove_command` — `{ command_id }` → `{}`. Delete a command TEMPLATE and all its
    /// workspace instances (ON DELETE CASCADE). REFUSED if any instance is running (the
    /// agent must stop first). Passing an `instance_id` instead of a `command_id` returns
    /// an actionable `invalid_id` (the same per-tool disambiguation as the CRUD tools).
    /// Delegates to `bridge::command_delete`'s underlying path (`db::delete_template`).
    /// Emits `commands://changed` on success.
    fn remove_command(&self, args: &Value) -> Result<Value, RpcError> {
        let command_id = require_str(args, "command_id")?;
        // The single-id removal (id validation + running-guard + cascade delete), shared
        // verbatim with the grouped remove_commands so the two stay in lockstep.
        let removed_instances = self.remove_one_command(command_id)?;
        crate::bridge::emit_commands_changed(&self.app);
        // Explicit mutation ack (R-WSCMD #4): `removed:true` + the count of instances
        // that cascade-deleted with the template.
        Ok(json!({ "removed": true, "removed_instances": removed_instances }))
    }

    /// Remove ONE command template by id (no event emission — the caller emits once).
    /// Validates the id (an `instance_id` or unknown id → the actionable `invalid_id`),
    /// refuses while any instance is running (`invalid_state`), then cascade-deletes the
    /// template + its instances and returns the count of instances removed. The SAME path
    /// `remove_command` (single) and `remove_commands` (grouped) both run.
    fn remove_one_command(&self, command_id: &str) -> Result<usize, RpcError> {
        let db = self.db()?;
        // Validate the id first: if it's an instance_id, return the actionable error;
        // if it's unknown entirely, return a generic invalid_id.
        let template = db
            .with_conn(|c| db::get_template(c, command_id))
            .map_err(internal_db("read command template"))?;
        if template.is_none() {
            return Err(self.bad_command_id_error(command_id));
        }
        // The instances that will cascade-delete with the template — both the running
        // guard input AND the ack count.
        let instance_ids = db
            .with_conn(|c| db::instance_ids_for_template(c, command_id))
            .map_err(internal_db("read template instances"))?;
        // Guard: refuse while any instance is running (same as the UI's command_delete).
        // The message says "removing" (not "editing") — the dogfood finding: it was
        // copy-pasted from update_command's edit guard (R-WSCMD #6).
        let runner = self.runner()?;
        if runner.any_running(&instance_ids) {
            return Err(RpcError::new(
                RpcCode::InvalidState,
                format!(
                    "command {command_id} is running in at least one workspace; stop it \
                     before removing the command"
                ),
            ));
        }
        let removed_instances = instance_ids.len();
        db.with_conn(|c| db::delete_template(c, command_id))
            .map_err(internal_db("delete command template"))?;
        Ok(removed_instances)
    }

    /// `remove_commands` — `{ command_ids: [...] }` → `{ removed, removed_instances,
    /// results: [...] }`. GROUPED deletion of command TEMPLATES (R-IMPORT #5): the batch
    /// mirror of `remove_command` so a mass import can be undone in ONE call. Each id runs
    /// the SAME [`Self::remove_one_command`] path (id validation + running-guard +
    /// cascade delete). A failure on one id (unknown, an instance_id, or a running
    /// template) is reported in that id's `results` ack and does NOT abort the others.
    /// Returns `removed` (count of templates actually deleted) + `removed_instances`
    /// (their cascaded instances) + a per-id `results` list (`{ command_id, removed }`,
    /// plus `error` on failure). Emits `commands://changed` ONCE when at least one
    /// template was removed. Out of V1_TOOLS; D8 errors (the `command_ids` shape).
    fn remove_commands(&self, args: &Value) -> Result<Value, RpcError> {
        // `command_ids` is required and must be a (possibly empty) array of strings.
        let ids = require_str_array(args, "command_ids")?;
        let mut removed = 0usize;
        let mut removed_instances = 0usize;
        let mut results: Vec<Value> = Vec::new();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for id in &ids {
            // De-duplicate: a repeated id would 404 on the second pass (already deleted);
            // collapse it to a single ack so the batch is idempotent on dupes.
            if !seen.insert(id.as_str()) {
                continue;
            }
            match self.remove_one_command(id) {
                Ok(n) => {
                    removed += 1;
                    removed_instances += n;
                    results.push(json!({ "command_id": id, "removed": true }));
                }
                Err(e) => results.push(json!({
                    "command_id": id,
                    "removed": false,
                    "error": { "code": e.code.as_str(), "message": e.message },
                })),
            }
        }
        // Emit the shared command-band refresh ONCE if anything was actually removed.
        if removed > 0 {
            crate::bridge::emit_commands_changed(&self.app);
        }
        // Explicit grouped ack (mirrors remove_command's): the removed count + the
        // cascaded-instance count + a per-id results list.
        Ok(json!({
            "removed": removed,
            "removed_instances": removed_instances,
            "results": results,
        }))
    }

    /// `clear_command_output` — `{ instance_id }` → `{ instance_id, cleared: true }`
    /// (PRD-4 review R-OUTPUT). Clear the captured output BUFFER of an instance so a
    /// long-running instance's accumulated scrollback (e.g. 160 KiB) does not stay
    /// attached indefinitely with no way for an agent to reset it. Delegates to the
    /// PRD-3 runner ([`crate::command::CommandRunner::clear_output`]), which empties the
    /// LIVE in-memory tail (if running) and the persisted `scrollback`/`prev_scrollback`
    /// via the sink, then emits the refresh event so the UI output panel wipes its
    /// xterm. The FACTUAL run outcome (state/exit_code/unread) is LEFT INTACT — a clear
    /// wipes the bytes, not the result. Validates the id first so an unknown id (or a
    /// template `command_id`) is the actionable `invalid_id` (finding #14) rather than a
    /// silent no-op. Out of V1_TOOLS; D8 errors.
    fn clear_command_output(&self, args: &Value) -> Result<Value, RpcError> {
        let instance_id = require_str(args, "instance_id")?;
        // An unknown / template id → the disambiguating invalid_id, before any clear.
        self.assert_instance_exists(instance_id)?;
        let runner = self.runner()?;
        runner.clear_output(instance_id);
        Ok(json!({ "instance_id": instance_id, "cleared": true }))
    }

    // --- Interactive terminal tools (PRD-4 review R-TERM) ----------------
    //
    // Let an agent drive the SAME interactive terminals the user sees. These reuse the
    // EXISTING terminal/PTY primitives — NO second terminal lifecycle (ADR-0003 D6 spirit):
    //   - `create_terminal` writes the terminal RECORD (`db::create_terminal`), optionally
    //     auto-attaches it to a workspace (the SAME `resolve::decide_attachment` rule the UI
    //     auto-attach uses), parks any opening `command` for the front to inject when its
    //     PTY spawns, and emits `terminals://changed` so the FRONT mounts the xterm + spawns
    //     the PTY (the front still owns the PTY, exactly as for a UI-created terminal).
    //   - `send_to_terminal` resolves the record id → live PTY (TerminalPtyMap) and writes
    //     `command + "\r"` via the SAME PtyManager write path as `pty_write`.
    //   - `list_terminals` reads the alive records + the live record↔pty map.
    //   - `close_terminal` flips the record closed (`db::close_terminal`) + kills the PTY
    //     (the SAME PtyManager close path as `pty_close`) + emits `terminals://changed`.
    // Explicit ids, D8 errors, out of V1_TOOLS.

    /// `create_terminal` — `{ cwd?, command?, label? }` → `{ terminal_id, cwd, workspace_id,
    /// has_command }`. Create an INTERACTIVE terminal. `cwd` is optional: when it sits inside a
    /// known workspace the terminal is auto-attached to that workspace (the SAME longest-ancestor
    /// rule as the UI auto-attach), otherwise it opens loose AT that cwd; absent → the user's
    /// home (resolved on the front when it spawns the shell, so the backend stores a sensible
    /// default). `command` is optional: present = parked for injection at opening (the front
    /// types `command + "\r"` once the PTY spawns, then the terminal stays interactive); absent =
    /// a bare shell. Emits `terminals://changed` so the front reconciles, mounts the xterm and
    /// spawns the PTY.
    fn create_terminal(&self, args: &Value) -> Result<Value, RpcError> {
        let cwd = optional_str(args, "cwd")?;
        let command = optional_str(args, "command")?;
        let label = optional_str(args, "label")?.map(|s| s.to_string());
        let db = self.db()?;

        // The record's stored cwd: the explicit cwd if given, else "." — the front's
        // `<Terminal>` resolves "." to the user's home / nyx's cwd when it spawns the shell
        // (the same default a UI loose terminal uses). A non-empty cwd is normalized so the
        // auto-attach match and the stored value agree.
        let stored_cwd = match cwd {
            Some(c) => crate::pathnorm::normalize(c),
            None => ".".to_string(),
        };

        // Create the record (+ resolve auto-attach to a known workspace from the cwd) in one
        // DB pass, so the new row already carries its workspace binding before the front
        // reconciles. Auto-attach reuses the SAME resolver the UI's `auto_attach_terminal`
        // uses — a cwd that matches no known workspace leaves the terminal loose (no guessing,
        // creates nothing).
        let (terminal_id, workspace_id) = db
            .with_conn(|c| -> Result<(String, Option<String>), diesel::result::Error> {
                let record = db::create_terminal(c, &stored_cwd, label.clone())?;
                let workspace_id = Self::resolve_attach_for_new_terminal(c, &record.id, cwd)?;
                Ok((record.id, workspace_id))
            })
            .map_err(internal_db("create terminal"))?;

        // Park the opening command (if any) so `register_terminal_pty` injects it once the
        // front's PTY for this record spawns. A bare terminal parks nothing.
        let has_command = command.is_some();
        if let Some(command) = command {
            self.pending_terminal_commands()?.set(&terminal_id, command.to_string());
        }

        // Broadcast the deck refresh so the FRONT mounts the xterm + spawns the PTY. The
        // same `terminals://changed` signal the close tool emits.
        crate::bridge::emit_terminals_changed(&self.app);

        Ok(json!({
            "terminal_id": terminal_id,
            "cwd": stored_cwd,
            "workspace_id": workspace_id,
            "has_command": has_command,
        }))
    }

    /// Resolve + apply the auto-attach for a freshly-created terminal record, reusing the
    /// SAME hybrid rule as `bridge::auto_attach_terminal` (longest-ancestor KNOWN workspace,
    /// creates nothing, no guessing). Returns the attached workspace id, or `None` when the
    /// cwd is absent / matches no known workspace (a loose terminal). Runs inside the create
    /// transaction so the row carries its binding before the front reconciles.
    fn resolve_attach_for_new_terminal(
        conn: &mut diesel::SqliteConnection,
        terminal_id: &str,
        cwd: Option<&str>,
    ) -> diesel::QueryResult<Option<String>> {
        use crate::resolve::{decide_attachment, Attachment, BindingMode, CurrentBinding, WorkspaceMatch};
        let Some(cwd) = cwd else {
            return Ok(None); // no cwd → loose terminal.
        };
        let normalized = crate::pathnorm::normalize(cwd);
        // A fresh terminal is unattached + auto-mode.
        let current = CurrentBinding { workspace_id: None, mode: BindingMode::Auto };
        let known: Vec<WorkspaceMatch> = db::all_workspaces(conn)?
            .into_iter()
            .map(|w| WorkspaceMatch { id: w.id, path: w.path })
            .collect();
        match decide_attachment(&current, Some(&normalized), &known) {
            Attachment::AttachAuto(ws) => {
                db::attach_terminal(conn, terminal_id, &ws, db::BINDING_AUTO)?;
                Ok(Some(ws))
            }
            Attachment::Unchanged => Ok(None),
        }
    }

    /// `send_to_terminal` — `{ terminal_id, command }` → `{ terminal_id, sent: true }`. Run a
    /// command in an OPEN terminal: resolve the record id → live PTY (TerminalPtyMap) and write
    /// `command + "\r"` via the SAME PtyManager write path as `pty_write`. The output streams
    /// back through `pty://output` (nothing to add on the display side). An unknown terminal id,
    /// or one whose PTY has not (yet) registered / has exited, is the actionable `invalid_id`.
    fn send_to_terminal(&self, args: &Value) -> Result<Value, RpcError> {
        let terminal_id = require_str(args, "terminal_id")?;
        let command = require_str(args, "command")?;
        let pty_id = self.resolve_live_pty(terminal_id)?;
        let mut bytes = command.as_bytes().to_vec();
        bytes.push(b'\r');
        let written = self
            .pty_manager()?
            .write_to(pty_id, &bytes)
            .map_err(|e| RpcError::new(RpcCode::Internal, format!("write to terminal failed: {e}")))?;
        if !written {
            // The map had a pty id but it is no longer live (raced an exit). Surface the
            // same actionable invalid_id as an unknown id.
            return Err(self.bad_terminal_id_error(terminal_id));
        }
        Ok(json!({ "terminal_id": terminal_id, "sent": true }))
    }

    /// `list_terminals` — `{ include_closed? }` → `{ terminals: [{ terminal_id, cwd, label,
    /// workspace_id, status, pty_id, live, busy, exec_state, exec_state_updated_at,
    /// exec_exit_code }] }`. List terminal records with their id + the live record↔PTY mapping
    /// (so the agent knows which it can write to). `live` is true when a PTY is registered for
    /// the record (its shell has started); `pty_id` is that live id or null; `status` is
    /// `"alive"` or `"closed"`. By default only OPEN (alive) terminals are listed; pass
    /// `include_closed:true` to ALSO list closed terminals, so an agent can rediscover a
    /// finished terminal's id and read its last scrollback via `read_terminal` (arbitration C).
    ///
    /// `busy` is the OS-AUTHORITATIVE bit (task #4) — `true` when a command is running in the
    /// terminal's foreground (the kernel foreground process group differs from the shell's),
    /// `false` at an idle prompt. It is the SAME signal that drives the UI running dot, so the
    /// agent sees exactly what the user sees; `null` when it cannot be derived (no live PTY, or
    /// a platform without a foreground process group). `exec_state` (`idle`|`running`|`success`
    /// |`error`), `exec_exit_code`, and `exec_state_updated_at` (epoch ms of the last transition)
    /// mirror the settled OSC-133 ANNOTATION (success/error + exit code), the same vocabulary the
    /// managed-command tools expose. Read-only.
    fn list_terminals(&self, args: &Value) -> Result<Value, RpcError> {
        let include_closed = optional_bool(args, "include_closed")?.unwrap_or(false);
        let db = self.db()?;
        let records = db
            .with_conn(db::list_terminals)
            .map_err(internal_db("list terminals"))?;
        let map = self.terminal_pty_map()?.snapshot();
        let pty_manager = self.pty_manager()?;
        let terminals: Vec<Value> = records
            .into_iter()
            .filter(|t| include_closed || t.status == db::STATUS_ALIVE)
            .map(|t| {
                let pty_id = map.get(&t.id).copied();
                // OS busy bit (task #4): the SAME `foreground_pgid != shell pgid` authority the
                // UI dot reads, derived live from the kernel via the PTY manager. `null` for a
                // record with no live PTY (or a non-Unix platform) — never a stale/phantom bit.
                let busy = pty_id.and_then(|id| pty_manager.busy_of(id));
                json!({
                    "terminal_id": t.id,
                    "cwd": t.cwd,
                    "label": t.label,
                    "workspace_id": t.workspace_id,
                    // "alive" | "closed" — a closed terminal has no live PTY but its record
                    // (and last scrollback) survives, so read_terminal still serves it.
                    "status": t.status,
                    "pty_id": pty_id,
                    "live": pty_id.is_some(),
                    // OS-derived running bit (authority for the dot) — see fn doc.
                    "busy": busy,
                    // Settled OSC-133 annotation (success/error + exit code) + the monotone
                    // signal. `exec_state_updated_at` is the epoch-ms an agent captures BEFORE
                    // send_to_terminal and polls past to detect a command's end (task #4).
                    "exec_state": t.exec_state,
                    "exec_exit_code": t.exec_exit_code,
                    "exec_state_updated_at": t.exec_state_updated_at,
                })
            })
            .collect();
        Ok(json!({ "terminals": terminals }))
    }

    /// `close_terminal` — `{ terminal_id }` → `{ terminal_id, closed: true }`. Close a terminal
    /// by id: flip the record `closed` (`db::close_terminal`, so it is not re-spawned), kill its
    /// live PTY if one is registered (the SAME PtyManager close path as `pty_close`), drop the
    /// record↔pty link, and emit `terminals://changed` so the front retires the pane. An unknown
    /// terminal id (no alive record) is the actionable `invalid_id`.
    fn close_terminal(&self, args: &Value) -> Result<Value, RpcError> {
        let terminal_id = require_str(args, "terminal_id")?;
        let db = self.db()?;
        // Validate against the alive records: an unknown / already-closed id → invalid_id.
        let record = db
            .with_conn(|c| db::get_terminal(c, terminal_id))
            .map_err(internal_db("read terminal"))?;
        match record {
            Some(r) if r.status == db::STATUS_ALIVE => {}
            _ => return Err(self.bad_terminal_id_error(terminal_id)),
        }
        // Flip the record closed (the SAME `close_terminal` record helper the UI uses).
        db.with_conn(|c| db::close_terminal(c, terminal_id))
            .map_err(internal_db("close terminal"))?;
        // Kill the live PTY if the front registered one (the SAME PtyManager close path as
        // `pty_close`), then drop the link. Both are idempotent if the PTY already exited.
        if let Some(pty_id) = self.terminal_pty_map()?.get(terminal_id) {
            let _ = self.pty_manager()?.close_id(pty_id);
        }
        self.terminal_pty_map()?.clear(terminal_id);
        // Drain any command parked by `create_terminal` for this record. If the terminal
        // is closed BEFORE its PTY ever registered (the front never mounted the xterm),
        // `register_terminal_pty` never runs, so the only other drain never fires and the
        // parked command would leak in PendingTerminalCommands forever.
        let _ = self.pending_terminal_commands()?.take(terminal_id);
        // Retire the pane on the front (the SAME signal create emits).
        crate::bridge::emit_terminals_changed(&self.app);
        Ok(json!({ "terminal_id": terminal_id, "closed": true }))
    }

    /// `read_terminal` — `{ terminal_id, tail_bytes?, max_bytes?, since?, strip_ansi? }` →
    /// `{ terminal_id, output, total_bytes, returned_bytes, truncated, reset, cursor }`. The READ
    /// counterpart to `send_to_terminal` (PRD-4.1 task #1): a BOUNDED window over a terminal's
    /// scrollback, symmetric to [`Self::get_command_output`].
    ///
    /// **Source — the EXISTING front scrollback, no second buffer.** The text read is the
    /// terminal record's persisted `scrollback` column (`db::get_terminal(...).scrollback`),
    /// i.e. the blob the front xterm `SerializeAddon` serializes and `db::persist_scrollback`
    /// stores (PRD 1). `read_terminal` introduces NO parallel backend PTY capture and NO second
    /// buffer; it reads the LAST front-persisted snapshot. Because that persistence is DEBOUNCED
    /// on the front, a read immediately after `send_to_terminal` may reflect a slightly stale
    /// snapshot — a documented trade-off of reusing the serializer (the agent re-reads to catch
    /// up), not a bug.
    ///
    /// **Bounding — identical knobs to `get_command_output`.** `tail_bytes` defaults to the
    /// token-safe [`DEFAULT_TAIL_BYTES`] (12 KiB) and `max_bytes`/`tail_bytes` over
    /// [`MAX_TAIL_BYTES`] (1 MiB) are refused with `output_too_large` (the SAME bound as the two
    /// command flows). `strip_ansi` defaults to **`true`** so the single `output` field is the
    /// cleaned view.
    ///
    /// **`since` is BEST-EFFORT here, not the strict cursor it is for `get_command_output`.**
    /// A command's captured output is an APPEND-ONLY stream, so a byte `cursor` is a stable
    /// resume point. A terminal's `scrollback` is instead the front `SerializeAddon` dump of the
    /// xterm GRID, RE-serialized in full on each debounced persist — it grows by append in the
    /// common line-echo case, but a reflow (resize), an alt-screen repaint (a TUI opening/closing),
    /// a `clear`, or scrollback eviction can REWRITE it so a prior byte offset no longer denotes
    /// the same content. We make the common cases safe — an incremental read pages forward
    /// CONTIGUOUSLY (it never silently drops the head of a burst), and a `since` beyond the new end
    /// (the buffer shrank) returns `reset:true` with the fresh content instead of an empty window —
    /// but on a same-length rewrite a `since=cursor` read can still return shifted/garbled content.
    /// So: `since` is a polling convenience for steady line output; whenever `reset` is true, or the
    /// output looks misaligned, re-read WITHOUT `since` to resync. Pixel-perfect cross-snapshot
    /// incremental fidelity is out of scope (see the arbitration note in the PR review).
    ///
    /// **Fidelity limit on alt-screen TUI output (documented, not a bug).** This reads the front
    /// `SerializeAddon` dump of the terminal grid. For NORMAL command output (line-based echo) the
    /// text is reproduced faithfully. For a full-screen (alt-screen) TUI app — which paints by
    /// cursor positioning rather than emitting literal spaces — a `strip_ansi` read can COALESCE
    /// runs of spaces (so adjacent on-screen words may join), because the serialized grid lacks the
    /// real whitespace the TUI never wrote. Pixel-perfect TUI rendering is explicitly OUT of scope
    /// (PRD-4.1); normal command output is unaffected.
    ///
    /// **Closed vs unknown (ADR-0003 D8).** As long as the terminal RECORD exists, the last
    /// persisted scrollback is returned — even for a CLOSED terminal (the record outlives the
    /// PTY). An UNKNOWN / no-longer-existing record is the structured `invalid_id`. Out of
    /// `V1_TOOLS`; D8 errors.
    fn read_terminal(&self, args: &Value) -> Result<Value, RpcError> {
        let terminal_id = require_str(args, "terminal_id")?;
        // Token-safe windowing knobs (`tail_bytes`/`max_bytes`/`since`/`strip_ansi`),
        // parsed + ceiling-checked by the SHARED helper — identical contract and
        // structured `output_too_large` payload as the two command flows.
        let knobs = parse_window_knobs(args)?;
        let since = knobs.since;
        let strip = knobs.strip;
        let effective_tail = knobs.effective_tail;

        // Read the EXISTING front-serialized scrollback for the record. An unknown record (the
        // row is gone / id was never valid) is the disambiguating invalid_id — symmetric to the
        // command flows' invalid_id on an unknown instance. A CLOSED terminal whose record still
        // exists falls through and returns its last persisted scrollback (decided: the record
        // outlives the PTY).
        let db = self.db()?;
        let scrollback = match db
            .with_conn(|c| db::get_terminal(c, terminal_id))
            .map_err(internal_db("read terminal scrollback"))?
        {
            Some(record) => record.scrollback,
            // A read targets the RECORD, which a closed terminal still has — so unlike
            // send_to_terminal (which needs a LIVE pty and rejects "unknown or closed"),
            // the ONLY way to reach here is a record that genuinely does not exist. Use a
            // message that says exactly that, not the live-tool's "or closed" wording.
            None => {
                return Err(RpcError::new(
                    RpcCode::InvalidId,
                    format!(
                        "unknown terminal {terminal_id} (no such terminal record; use a \
                         terminal_id from list_terminals, or one returned by create_terminal)"
                    ),
                ))
            }
        };

        // Bound + render through the SAME path as get_command_output (single output field).
        let window = bound_output(&scrollback, effective_tail, since);
        let output = render_output(&window.output, strip, None, None, window.head_truncated);
        Ok(json!({
            "terminal_id": terminal_id,
            "output": output,
            "total_bytes": window.total_bytes,
            "returned_bytes": window.returned_bytes,
            "truncated": window.truncated,
            // `reset` is true when the supplied `since` was beyond the new end — the
            // serialized scrollback shrank (a `clear`/reflow), so the prior cursor is stale
            // and `output` carries the fresh content rather than an empty window.
            "reset": window.reset,
            // `cursor` is the RAW-byte offset one past the end of what was returned. It
            // round-trips as the next `since` for steady line output; see the doc comment for
            // the best-effort caveat on a re-serialized (reflowed/cleared) scrollback.
            "cursor": window.cursor,
        }))
    }

    /// Resolve an OPEN terminal record id to its live PTY id, or the actionable `invalid_id`
    /// when the id is unknown / not alive / has no live PTY registered. Used by
    /// `send_to_terminal`.
    fn resolve_live_pty(&self, terminal_id: &str) -> Result<u64, RpcError> {
        // The id must name an alive record first (so "unknown id" is distinct from "no PTY").
        let db = self.db()?;
        let record = db
            .with_conn(|c| db::get_terminal(c, terminal_id))
            .map_err(internal_db("read terminal"))?;
        match record {
            Some(r) if r.status == db::STATUS_ALIVE => {}
            _ => return Err(self.bad_terminal_id_error(terminal_id)),
        }
        self.terminal_pty_map()?
            .get(terminal_id)
            .ok_or_else(|| {
                RpcError::new(
                    RpcCode::InvalidState,
                    format!(
                        "terminal {terminal_id} has no live shell yet (it may still be \
                         starting up); try again, or open one with create_terminal"
                    ),
                )
            })
    }

    /// The actionable `invalid_id` for an unknown / non-open terminal id, naming where a
    /// valid id comes from (mirrors the command tools' disambiguating errors).
    fn bad_terminal_id_error(&self, terminal_id: &str) -> RpcError {
        RpcError::new(
            RpcCode::InvalidId,
            format!(
                "unknown or closed terminal {terminal_id} (use a terminal_id from \
                 list_terminals)"
            ),
        )
    }
}

impl<R: Runtime> ToolDispatcher for NyxToolDispatcher<R> {
    fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
        match name {
            // Spike probe (PRD-4 #7): handled first and without any managed-state
            // lookup, so a liveness hook succeeds even before the runtime is warm.
            PROBE_TOOL => self.probe(),
            "list_projects" => self.list_projects(),
            "list_workspaces" => self.list_workspaces(arguments),
            "list_commands" => self.list_commands(arguments),
            "start_command" => self.start_command(arguments),
            "stop_command" => self.stop_command(arguments),
            "relaunch_command" => self.relaunch_command(arguments),
            "get_command_output" => self.get_command_output(arguments),
            // Advertised extension (NOT in V1_TOOLS): the bounded long-poll (D12).
            WAIT_FOR_COMMAND_TOOL => self.wait_for_command(arguments),
            // Advertised command-CRUD extension (NOT in V1_TOOLS, ADR-0003 D13): the
            // mutating tools that delegate to the existing PRD-3 create/update/import
            // layer (review 01KV9614CHC4092P05DV9R5KPG).
            ADD_COMMAND_TOOL => self.add_command(arguments),
            UPDATE_COMMAND_TOOL => self.update_command(arguments),
            IMPORT_COMMANDS_TOOL => self.import_commands(arguments),
            REMOVE_WORKSPACE_TOOL => self.remove_workspace(arguments),
            REMOVE_COMMAND_TOOL => self.remove_command(arguments),
            // Advertised import-preview + grouped-delete extension (NOT in V1_TOOLS,
            // review R-IMPORT #5): the read-only import-discovery preview and the batch
            // mirror of remove_command, over the SAME discovery/delete paths.
            LIST_IMPORTABLE_SCRIPTS_TOOL => self.list_importable_scripts(arguments),
            REMOVE_COMMANDS_TOOL => self.remove_commands(arguments),
            // Advertised output-buffer reset (NOT in V1_TOOLS, review R-OUTPUT): delegates
            // to the PRD-3 runner buffer clear + the refresh event.
            CLEAR_COMMAND_OUTPUT_TOOL => self.clear_command_output(arguments),
            // Advertised interactive-terminal extension (NOT in V1_TOOLS, review R-TERM):
            // create / write to / list / close an interactive terminal, reusing the existing
            // terminal record + PTY primitives (no second terminal lifecycle).
            CREATE_TERMINAL_TOOL => self.create_terminal(arguments),
            SEND_TO_TERMINAL_TOOL => self.send_to_terminal(arguments),
            LIST_TERMINALS_TOOL => self.list_terminals(arguments),
            CLOSE_TERMINAL_TOOL => self.close_terminal(arguments),
            // Advertised scrollback-read extension (NOT in V1_TOOLS, PRD-4.1 #1): the read
            // counterpart to send_to_terminal, over the EXISTING front-serialized scrollback.
            READ_TERMINAL_TOOL => self.read_terminal(arguments),
            // Advertised agent-session channel (NOT in V1_TOOLS, PRD-5 #4 / ADR-0004):
            // the Claude Code SessionStart/SessionEnd hook target. Normalizes the raw
            // hook payload through the claude_code adapter and persists the
            // `agent_sessions` row (capture on start, end on SessionEnd).
            AGENT_SESSION_EVENT_TOOL => self.agent_session_event(arguments),
            "workspace_add" => self.workspace_add(arguments),
            "create_workspace" => self.create_workspace(arguments),
            other => Err(RpcError::new(
                RpcCode::MethodNotFound,
                format!("unknown tool '{other}'"),
            )),
        }
    }
}

/// The token-safe windowing knobs shared by the THREE output flows
/// (`get_command_output`, `wait_for_command`, `read_terminal`): the parsed
/// `tail_bytes`/`since`/`strip_ansi`, plus the resolved `effective_tail`
/// (`min(tail_bytes, max_bytes)`). Built by [`parse_window_knobs`].
struct WindowKnobs {
    /// Raw `since` byte offset (a prior `cursor`), or `None` to read the tail.
    /// `wait_for_command` supplies its own default (the current end-of-buffer) when
    /// this is `None`, so it is exposed un-resolved.
    since: Option<usize>,
    /// `strip_ansi`, defaulting to `true` (one cleaned `output` field).
    strip: bool,
    /// The effective window size = `min(tail_bytes, max_bytes)`, after the ceiling guard.
    effective_tail: usize,
}

/// Parse + validate the windowing knobs common to the three output flows so the
/// token-safe contract lives in ONE place (D7/D8): `tail_bytes` (default
/// [`DEFAULT_TAIL_BYTES`]), `max_bytes` (alternative ceiling), `since`, and
/// `strip_ansi` (default `true`). A `tail_bytes` OR `max_bytes` above
/// [`MAX_TAIL_BYTES`] is refused with the shared `output_too_large` error (carrying
/// structured `requested`/`limit`) rather than silently clamped. Returns the resolved
/// [`WindowKnobs`] (incl. `effective_tail = min(tail_bytes, ceiling)`).
fn parse_window_knobs(args: &Value) -> Result<WindowKnobs, RpcError> {
    let tail_bytes = optional_usize(args, "tail_bytes")?.unwrap_or(DEFAULT_TAIL_BYTES);
    let since = optional_usize(args, "since")?;
    let max_bytes = optional_usize(args, "max_bytes")?;
    let strip = optional_bool(args, "strip_ansi")?.unwrap_or(true);
    let ceiling = max_bytes.unwrap_or(MAX_TAIL_BYTES);
    if tail_bytes > MAX_TAIL_BYTES || ceiling > MAX_TAIL_BYTES {
        return Err(output_too_large_error(tail_bytes, ceiling));
    }
    Ok(WindowKnobs { since, strip, effective_tail: tail_bytes.min(ceiling) })
}

/// A bounded output window (ADR-0003 D7). `output` is at most `tail_bytes`;
/// `total_bytes` is the full size before bounding; `cursor` is the byte offset one
/// past the end of what was returned, fed back as the next `since` for incremental
/// polling. `reset` flags that the caller's `since` was beyond the current end —
/// the buffer SHRANK (a `clear`/reset) so the old offsets no longer apply.
struct OutputWindow {
    output: String,
    total_bytes: usize,
    returned_bytes: usize,
    truncated: bool,
    reset: bool,
    cursor: usize,
    /// Whether the window begins PAST byte 0 of the full buffer (a tail/`since` cut at
    /// the head). Only then can the first bytes be the orphaned tail of an ANSI escape
    /// whose `ESC[` introducer fell before the window — i.e. only then may the
    /// leading-orphan guard run. A full-buffer read (`head_truncated == false`) must keep
    /// its first bytes verbatim.
    head_truncated: bool,
}

/// Compute the bounded output window (ADR-0003 D7). Operates on BYTES so the bound
/// is exact, then snaps cut points to UTF-8 char boundaries so the returned string
/// stays decodable.
///
/// - `since` (a byte offset from a previous `cursor`): only bytes at/after it are
///   considered, so polling never re-returns what the agent already read. A `since`
///   exactly AT the end yields an empty window with the cursor pinned there. A
///   `since` BEYOND the end means the buffer shrank since the caller's last read (a
///   `clear`/reset, or a terminal re-serialized to a smaller grid): the old offsets
///   are gone, so we set `reset` and fall back to a fresh read (`since`-less) of the
///   new content instead of silently returning empty with a backward-moving cursor.
/// - `tail_bytes` bounds the window. Without a `since` (a one-shot/tail read) keep
///   the LAST `tail_bytes` (most recent). WITH a `since` (incremental paging) keep
///   the FIRST `tail_bytes` of the new region instead, so repeated `since=cursor`
///   reads page forward CONTIGUOUSLY — no gap (the head of a burst is never
///   silently dropped) and no duplicate. Either way `truncated` flags that the
///   window did not cover the whole remaining region.
/// - `cursor`: one past the end of what was ACTUALLY returned, so the next call's
///   `since` resumes exactly there (== `total_bytes` when the window reaches the end).
fn bound_output(full: &str, tail_bytes: usize, since: Option<usize>) -> OutputWindow {
    let bytes = full.as_bytes();
    let total_bytes = bytes.len();

    // A `since` PAST the current end means the buffer shrank since the caller last
    // read (a `clear`/reset, or a terminal re-serialized smaller): the old byte
    // offsets are stale. Signal it and fall back to a since-less tail read so the
    // caller sees the post-reset content rather than an empty window + a cursor that
    // silently moved backward. (`since == total_bytes` is the ordinary "nothing new
    // yet" case, NOT a reset.)
    let reset = since.is_some_and(|s| s > total_bytes);
    let since = if reset { None } else { since };

    // Apply the incremental cursor first: drop everything the caller already saw.
    // Snap `since` up to a char boundary so the slice below is valid UTF-8.
    let start_after = since.unwrap_or(0).min(total_bytes);
    let start_after = ceil_char_boundary(full, start_after);
    let remaining = &bytes[start_after..];

    // Bound the remaining region to `tail_bytes`. Forward-page on an incremental
    // (`since`) read so the head of a burst is never dropped; tail on a one-shot read
    // so the most-recent output is what's returned.
    let (raw_start, raw_end, truncated) = if remaining.len() > tail_bytes {
        if since.is_some() {
            // Incremental: keep the FIRST `tail_bytes` of the new region; the cursor
            // then advances only to the end of what we returned, so the next
            // `since=cursor` continues contiguously.
            (start_after, start_after + tail_bytes, true)
        } else {
            // One-shot: keep the LAST `tail_bytes` (the most recent), dropping the head.
            (start_after + (remaining.len() - tail_bytes), total_bytes, true)
        }
    } else {
        (start_after, total_bytes, false)
    };
    // Snap both cut points up to char boundaries (only matters when truncated), and
    // keep end ≥ start after snapping.
    let window_start = ceil_char_boundary(full, raw_start);
    let window_end = ceil_char_boundary(full, raw_end).min(total_bytes).max(window_start);

    let slice = &full[window_start..window_end];
    OutputWindow {
        output: slice.to_string(),
        total_bytes,
        returned_bytes: slice.len(),
        truncated,
        reset,
        // The next `since`: one past the end of what we ACTUALLY returned.
        cursor: window_end,
        // The window starts past byte 0 ⇒ its head may be a chopped escape's orphan tail.
        head_truncated: window_start > 0,
    }
}

/// Round `idx` UP to the next UTF-8 char boundary in `s` (clamped to `s.len()`), so
/// slicing at the result never splits a multibyte char. `str::is_char_boundary` is
/// stable; `str::ceil_char_boundary` is not, so we roll our own.
fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Whether a workspace `path` matches a normalized `cwd` filter (ADR-0003 D5). A
/// match is "the cwd is at or under the workspace, or vice versa" — both already
/// normalized — so an exact path, a subdir of the workspace, or the workspace's own
/// parent all match. This is a FILTER convenience only; it never resolves a single
/// "current" workspace (an ambiguous cwd matching several rows returns them all).
fn path_matches(workspace_path: &str, cwd_normalized: &str) -> bool {
    if workspace_path == cwd_normalized {
        return true;
    }
    let under = |child: &str, parent: &str| {
        child.len() > parent.len()
            && child.starts_with(parent)
            && child[parent.len()..].starts_with(['/', '\\'])
    };
    under(workspace_path, cwd_normalized) || under(cwd_normalized, workspace_path)
}

/// Validate that `path` exists on disk AND is a directory (the `workspace_add`
/// precondition — dogfood finding). A non-existent path or one that resolves to a
/// FILE (not a dir) is rejected with the D8 `invalid_argument` vocabulary and an
/// actionable message naming the path, so a typo can no longer register a phantom
/// workspace that points nowhere. Symlinks are followed (`std::fs::metadata`), so a
/// symlink to a real directory is accepted — that is still "an existing folder".
fn validate_existing_dir(path: &str) -> Result<(), RpcError> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "path '{path}' exists but is not a directory; workspace_add registers an \
                 existing folder (use create_workspace to create a new folder)"
            ),
        )),
        Err(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "path '{path}' does not exist; workspace_add registers an EXISTING folder \
                 (use create_workspace to create the folder first)"
            ),
        )),
    }
}

/// Ensure the directory at `path` exists, creating it AND any missing parents
/// (`mkdir -p` semantics) — the `create_workspace` creating-intent precondition (D2).
/// Already a directory → a no-op success (idempotent). A path that exists as a FILE,
/// or that cannot be created (e.g. a parent component is a file, or permission
/// denied), is rejected with the D8 `invalid_argument` vocabulary. The error message
/// names the path + the OS reason but never the surrounding environment.
fn ensure_dir_created(path: &str) -> Result<(), RpcError> {
    // A pre-existing FILE at the path is not creatable into a directory — surface the
    // same clear distinction as workspace_add rather than a confusing mkdir error.
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_dir() {
            return Ok(()); // already a directory: idempotent create.
        }
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "path '{path}' exists but is not a directory; cannot create a workspace folder there"
            ),
        ));
    }
    std::fs::create_dir_all(path).map_err(|e| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!("could not create directory '{path}': {e}"),
        )
    })
}

/// Last path segment of `path` (for a `workspace_add` default name): the basename
/// after the final `/` or `\`, or the whole string if it has no separator.
fn basename(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string()
}

/// Map a `db::create_workspace` failure to the ADR-0003 D8 vocabulary. SQLite
/// surfaces a FK violation (unknown project) and a UNIQUE violation (duplicate path
/// in the project) as `DatabaseError`s whose message we classify; anything else is
/// `internal`.
fn map_create_workspace_err(project_id: &str, e: diesel::result::Error) -> RpcError {
    use diesel::result::{DatabaseErrorKind, Error as DieselError};
    match &e {
        DieselError::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, _) => RpcError::new(
            RpcCode::InvalidId,
            format!("unknown project {project_id}"),
        ),
        DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => RpcError::new(
            RpcCode::InvalidState,
            "a workspace with this path already exists in the project",
        ),
        // SQLite sometimes reports the FK failure as a generic constraint message
        // rather than the typed kind; classify on the message as a fallback.
        DieselError::DatabaseError(_, info) => {
            let msg = info.message().to_ascii_lowercase();
            if msg.contains("foreign key") {
                RpcError::new(RpcCode::InvalidId, format!("unknown project {project_id}"))
            } else if msg.contains("unique") {
                RpcError::new(
                    RpcCode::InvalidState,
                    "a workspace with this path already exists in the project",
                )
            } else {
                RpcError::new(RpcCode::Internal, format!("create workspace failed: {e}"))
            }
        }
        _ => RpcError::new(RpcCode::Internal, format!("create workspace failed: {e}")),
    }
}

/// A closure that maps a DB error to an `internal` [`RpcError`] tagged with the
/// failing operation, for the listing tools whose failures are never the caller's
/// fault (a bad query/connection, not a bad id).
fn internal_db(op: &'static str) -> impl Fn(diesel::result::Error) -> RpcError {
    move |e| RpcError::new(RpcCode::Internal, format!("{op}: {e}"))
}

/// The shared `output_too_large` error for the THREE output flows (`get_command_output`,
/// `wait_for_command`, `read_terminal`) when a requested window exceeds the hard ceiling
/// (PRD-4.1 task #3). The structured `data` carries machine-readable `requested` (the larger
/// of the offending `tail_bytes`/`max_bytes`, i.e. what the caller asked for) and `limit`
/// ([`MAX_TAIL_BYTES`]) so an agent can retry a smaller window WITHOUT parsing the prose
/// message. The error `code` (`output_too_large`) and the prose are kept EXACTLY as the two
/// pre-existing flows emitted them (non-regression); only the structured `data` is added.
fn output_too_large_error(tail_bytes: usize, ceiling: usize) -> RpcError {
    // `requested` reflects what the caller asked for that tripped the ceiling: the larger of
    // the two knobs they set (either one above MAX_TAIL_BYTES is the trigger).
    let requested = tail_bytes.max(ceiling);
    RpcError::new(
        RpcCode::OutputTooLarge,
        format!("requested window exceeds max_bytes ({MAX_TAIL_BYTES})"),
    )
    .with_data(json!({ "requested": requested, "limit": MAX_TAIL_BYTES }))
}

/// The JSON view of a command TEMPLATE returned by the command-CRUD tools
/// (`add_command`/`update_command`/`import_commands`). Matches the `project_id`
/// (template) form of `list_commands` so an agent sees a consistent template shape
/// across read and write: `command_id` is the template id (NOT launchable — pass it
/// to `update_command`, or use the instance_id from `list_commands(workspace_id=…)`
/// to act on a workspace's running instance).
fn template_json(t: &db::ManagedCommand) -> Value {
    json!({
        "command_id": t.id,
        "project_id": t.project_id,
        "name": t.name,
        "command": t.command,
        "subfolder": t.subfolder,
        "source_kind": t.source_kind,
        "package_manager": t.package_manager,
    })
}

/// The JSON view of ONE discoverable script for the preview / `list_importable_scripts`
/// surface (R-IMPORT #4/#5): the proposed `name` (the editable template name), the
/// owning `package` (the subfolder, `""` = root), the raw `script_name`, the script
/// `body` (the raw `package.json` script command), plus the default runner `command`
/// the import would create and the detected `package_manager`. NO template id — nothing
/// is created in preview.
fn preview_script_json(s: &crate::pkgjson::DiscoveredScript) -> Value {
    json!({
        "name": s.proposed_name,
        "package": s.subfolder,
        "script_name": s.script_name,
        "body": s.script_command_snapshot,
        "command": s.default_command,
        "package_manager": s.package_manager,
    })
}

/// Map a `db::create_template` failure (the `add_command` write) to the ADR-0003 D8
/// vocabulary. A UNIQUE violation (`UNIQUE(project_id, name)`) means the name is
/// already used in the project → `invalid_state`; a FK violation (unknown project) →
/// `invalid_id`; anything else → `internal`. Same message-fallback classification as
/// [`map_create_workspace_err`], since SQLite sometimes reports a typed constraint as
/// a generic message.
fn map_template_write_err(project_id: &str, e: diesel::result::Error) -> RpcError {
    use diesel::result::{DatabaseErrorKind, Error as DieselError};
    match &e {
        DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => RpcError::new(
            RpcCode::InvalidState,
            "a command with this name already exists in the project — choose a unique name",
        ),
        DieselError::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, _) => {
            RpcError::new(RpcCode::InvalidId, format!("unknown project {project_id}"))
        }
        DieselError::DatabaseError(_, info) => {
            let msg = info.message().to_ascii_lowercase();
            if msg.contains("unique") {
                RpcError::new(
                    RpcCode::InvalidState,
                    "a command with this name already exists in the project — choose a unique name",
                )
            } else if msg.contains("foreign key") {
                RpcError::new(RpcCode::InvalidId, format!("unknown project {project_id}"))
            } else {
                RpcError::new(RpcCode::Internal, format!("create command failed: {e}"))
            }
        }
        _ => RpcError::new(RpcCode::Internal, format!("create command failed: {e}")),
    }
}

/// Map an `update_command` write failure to the D8 vocabulary when no project id is in
/// hand (the update is keyed by `command_id`). A UNIQUE violation (renaming to a name
/// already used in the project) → `invalid_state`; anything else → `internal`.
fn map_template_write_err_generic(e: diesel::result::Error) -> RpcError {
    use diesel::result::{DatabaseErrorKind, Error as DieselError};
    match &e {
        DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => RpcError::new(
            RpcCode::InvalidState,
            "a command with this name already exists in the project — choose a unique name",
        ),
        DieselError::DatabaseError(_, info)
            if info.message().to_ascii_lowercase().contains("unique") =>
        {
            RpcError::new(
                RpcCode::InvalidState,
                "a command with this name already exists in the project — choose a unique name",
            )
        }
        _ => RpcError::new(RpcCode::Internal, format!("update command failed: {e}")),
    }
}

/// Build the run-status JSON fields a command tool surfaces (finding #13 + the v4
/// outcome/unread split) from a FACTUAL [`RunState`], its `last_exit_code`, and the
/// `unread` notification flag: `{ state, running, finished, exit_code, unread }`.
///
/// - `running` ⇔ `state == Running` (a live process is streaming now).
/// - `finished` ⇔ `state ∈ {Success, Error}` (the last run ended naturally).
/// - `exit_code` is surfaced ONLY for a finished run (`Some` natural code): `0` for
///   `Success`, non-zero for a crash under `Error`. While `idle`/`running` it is
///   `null` — there is no completed run to report. This is what lets an agent tell a
///   crash (`exit_code ≠ 0`, `state:error`) from a clean run (`exit_code:0`,
///   `state:success`) instead of a bare `idle`.
/// - `unread` (v4) is the separate "unseen result" flag: `true` while a finished
///   run has not been acknowledged in the UI, `false` once acknowledged (or while
///   running/idle). It is REPORTED but never gates the factual fields — a UI
///   acknowledge flips ONLY `unread`, so `state`/`exit_code` survive the ack (the
///   finding's crux: an ack no longer erases the error the MCP sees).
fn status_json(state: RunState, last_exit_code: Option<i32>, unread: bool) -> Value {
    let running = state == RunState::Running;
    let finished = matches!(state, RunState::Success | RunState::Error);
    // Only a finished run has a meaningful exit code; while idle/running it is null
    // even if a PRIOR run left a code (the new run has not produced one yet).
    let exit_code = if finished { last_exit_code } else { None };
    json!({
        "state": state.as_db_str(),
        "running": running,
        "finished": finished,
        "exit_code": exit_code,
        "unread": unread,
    })
}

/// Wrap the status fields of [`status_json`] into a `start`/`stop`/`relaunch_command`
/// result by prefixing `instance_id`: `{ instance_id, state, running, finished,
/// exit_code }`. Keeps the legacy `{ instance_id, state }` shape (back-compat) while
/// adding finding #13's `running`/`finished`/`exit_code`.
fn status_result(instance_id: &str, status: Value) -> Value {
    let mut obj = json!({ "instance_id": instance_id });
    if let (Some(map), Some(status_map)) = (obj.as_object_mut(), status.as_object()) {
        for (k, v) in status_map {
            map.insert(k.clone(), v.clone());
        }
    }
    obj
}

/// Read a REQUIRED string argument, erroring `invalid_argument` (D8) when it is
/// missing or not a string. Empty strings are rejected too — an empty id is never a
/// valid reference.
fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    match args.get(key).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("missing or empty required argument '{key}'"),
        )),
    }
}

/// Read an OPTIONAL string argument. `None` when absent or JSON null; an empty
/// string is treated as absent (so `cwd: ""` is "no filter"). A present non-string
/// is an `invalid_argument` error.
fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument '{key}' must be a string"),
        )),
    }
}

/// Read an OPTIONAL non-negative integer argument (for `tail_bytes`/`since`/
/// `max_bytes`). `None` when absent/null. A negative or non-integer value is an
/// `invalid_argument` error (the D8 example: `tail_bytes must be >= 0`).
fn optional_usize(args: &Value, key: &str) -> Result<Option<usize>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_i64().ok_or_else(|| {
                RpcError::new(RpcCode::InvalidArgument, format!("argument '{key}' must be an integer"))
            })?;
            if n < 0 {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!("argument '{key}' must be >= 0"),
                ));
            }
            Ok(Some(n as usize))
        }
    }
}

/// Read an OPTIONAL non-negative integer argument as a `u64` (for `timeout_ms`).
/// `None` when absent/null; a negative or non-integer value is `invalid_argument`.
/// Distinct from [`optional_usize`] only in its return width — `timeout_ms` is a
/// duration in milliseconds, naturally a `u64`.
fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_i64().ok_or_else(|| {
                RpcError::new(RpcCode::InvalidArgument, format!("argument '{key}' must be an integer"))
            })?;
            if n < 0 {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!("argument '{key}' must be >= 0"),
                ));
            }
            Ok(Some(n as u64))
        }
    }
}

/// Parse the OPTIONAL `until` argument of `wait_for_command` into the set of
/// [`RunState`]s that resolve the wait (ADR-0003 D12). Aligns to the runner
/// vocabulary `idle`/`running`/`success`/`error`; `"exited"` is an ALIAS expanding to
/// both settled states (`success`+`error`). Absent / null / an empty array →
/// the DEFAULT settled set `success`+`error` (the common "await completion" case).
///
/// A non-array value, a non-string element, or an unknown state string is
/// `invalid_argument` (the D8 vocabulary) — the contract names the accepted values
/// rather than silently ignoring a typo. Duplicates (incl. those introduced by
/// `"exited"`) are de-duplicated so the target set stays minimal.
fn parse_until(args: &Value) -> Result<Vec<RunState>, RpcError> {
    let default = || vec![RunState::Success, RunState::Error];
    let raw = match args.get("until") {
        None | Some(Value::Null) => return Ok(default()),
        Some(Value::Array(items)) => items,
        Some(_) => {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                "argument 'until' must be an array of state strings \
                 (idle|running|success|error|exited)",
            ))
        }
    };
    let mut states: Vec<RunState> = Vec::new();
    let push = |s: RunState, states: &mut Vec<RunState>| {
        if !states.contains(&s) {
            states.push(s);
        }
    };
    for item in raw {
        let s = item.as_str().ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidArgument,
                "each 'until' entry must be a state string \
                 (idle|running|success|error|exited)",
            )
        })?;
        match s.trim().to_ascii_lowercase().as_str() {
            "idle" => push(RunState::Idle, &mut states),
            "running" => push(RunState::Running, &mut states),
            "success" => push(RunState::Success, &mut states),
            "error" => push(RunState::Error, &mut states),
            // "exited" is the alias for "finished either way": success OR error.
            "exited" => {
                push(RunState::Success, &mut states);
                push(RunState::Error, &mut states);
            }
            other => {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!(
                        "unknown 'until' state '{other}' \
                         (accepted: idle|running|success|error|exited)"
                    ),
                ))
            }
        }
    }
    // An empty array means "no explicit targets" → fall back to the settled default,
    // so a caller that passes `until: []` still gets the await-completion behaviour.
    if states.is_empty() {
        return Ok(default());
    }
    Ok(states)
}

/// Which run `get_command_output` reads (review 01KV90QCKZ8BXZ4DTYZRJK56EZ). Bounded
/// to N=1 of history, so there are exactly two selectable runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunSelector {
    /// The CURRENT/latest run (the default — live tail if running, else the persisted
    /// current scrollback).
    Current,
    /// The single RETAINED prior run (its archived `prev_*` scrollback + outcome).
    Previous,
}

/// Read the OPTIONAL `run` selector for `get_command_output`. Bounded to N=1 of
/// retained history, so the accepted values are deliberately small. Absent / null /
/// `0` / `"current"` / `"latest"` select [`RunSelector::Current`]; `-1` / `"previous"`
/// / `"prev"` select [`RunSelector::Previous`]. Any other integer (e.g. `-2`, `1`) or
/// string is `invalid_argument`: there is no run beyond the immediately-prior one
/// (history is bounded), so the contract refuses it rather than silently clamping.
fn optional_run_selector(args: &Value, key: &str) -> Result<RunSelector, RpcError> {
    let too_far = || {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "argument '{key}' selects a run: 0/\"current\" (default, latest) or \
                 -1/\"previous\" (the one retained prior run); history is bounded to N=1"
            ),
        )
    };
    match args.get(key) {
        None | Some(Value::Null) => Ok(RunSelector::Current),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "" | "current" | "latest" => Ok(RunSelector::Current),
            "previous" | "prev" => Ok(RunSelector::Previous),
            _ => Err(too_far()),
        },
        Some(v) => match v.as_i64() {
            Some(0) => Ok(RunSelector::Current),
            Some(-1) => Ok(RunSelector::Previous),
            Some(_) => Err(too_far()),
            None => Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!("argument '{key}' must be an integer (0/-1) or a string"),
            )),
        },
    }
}

/// Read an OPTIONAL boolean argument (for `strip_ansi`). `None` when absent/null. A
/// present non-boolean value is an `invalid_argument` error (D8).
fn optional_bool(args: &Value, key: &str) -> Result<Option<bool>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument '{key}' must be a boolean"),
        )),
    }
}

/// Read an optional array-of-strings argument (for `names` in `import_commands` B1).
/// `None` when absent or null (→ no filter, import everything). An empty array is
/// accepted as `Some([])` (filter that matches nothing). A non-array or an array with
/// a non-string element → `invalid_argument`.
fn optional_str_array(
    args: &Value,
    key: &str,
) -> Result<Option<std::collections::HashSet<String>>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(arr)) => {
            let mut set = std::collections::HashSet::with_capacity(arr.len());
            for (i, v) in arr.iter().enumerate() {
                match v.as_str() {
                    Some(s) => {
                        set.insert(s.to_string());
                    }
                    None => {
                        return Err(RpcError::new(
                            RpcCode::InvalidArgument,
                            format!("argument '{key}[{i}]' must be a string"),
                        ));
                    }
                }
            }
            Ok(Some(set))
        }
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument '{key}' must be an array of strings"),
        )),
    }
}

/// Read a REQUIRED array-of-strings argument, preserving ORDER (for `command_ids` in
/// `remove_commands`). Missing / not-an-array / a non-string element →
/// `invalid_argument`. An empty array is accepted (a no-op batch). Empty-string ids are
/// rejected (an empty id is never a valid reference).
fn require_str_array(args: &Value, key: &str) -> Result<Vec<String>, RpcError> {
    match args.get(key) {
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, v) in arr.iter().enumerate() {
                match v.as_str() {
                    Some(s) if !s.is_empty() => out.push(s.to_string()),
                    _ => {
                        return Err(RpcError::new(
                            RpcCode::InvalidArgument,
                            format!("argument '{key}[{i}]' must be a non-empty string"),
                        ))
                    }
                }
            }
            Ok(out)
        }
        _ => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("missing or invalid required argument '{key}' (expected an array of strings)"),
        )),
    }
}

/// Read the OPTIONAL `env` map argument for `start_command` / `relaunch_command`
/// (R-WSCMD #7): a JSON object `{ KEY: VALUE }` of per-run environment overrides
/// MERGED onto the inherited environment at spawn. Returns the pairs as a `Vec` so the
/// runner can hand them to the PTY spawn in order. `None`/absent/null → an empty Vec
/// (a plain inherited-env spawn).
///
/// Validation (D8 vocabulary): a non-object value is `invalid_argument`; each VALUE
/// must be a string (a number/bool/object is rejected — env values are strings). A key
/// may not be empty. **Secret VALUES are NEVER included in any error message** — an
/// error names only the offending KEY, never its value, so a secret can never leak into
/// a log or an error payload.
fn optional_env(args: &Value, key: &str) -> Result<Vec<(String, String)>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Object(map)) => {
            let mut pairs = Vec::with_capacity(map.len());
            for (k, v) in map {
                if k.is_empty() {
                    return Err(RpcError::new(
                        RpcCode::InvalidArgument,
                        format!("argument '{key}' has an empty environment variable name"),
                    ));
                }
                match v {
                    Value::String(s) => pairs.push((k.clone(), s.clone())),
                    // Reject non-string values WITHOUT echoing the value (which may be a
                    // secret): name only the key and its JSON type.
                    other => {
                        return Err(RpcError::new(
                            RpcCode::InvalidArgument,
                            format!(
                                "environment variable '{k}' in '{key}' must be a string \
                                 (got a {})",
                                json_type_name(other)
                            ),
                        ))
                    }
                }
            }
            Ok(pairs)
        }
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument '{key}' must be an object mapping KEY to VALUE strings"),
        )),
    }
}

/// The JSON type name of a value, for an `invalid_argument` message that must NOT echo
/// the value itself (e.g. a non-string env value that could be a secret).
fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Strip ANSI/VT control sequences from terminal output (finding #15), leaving the
/// readable text an agent actually wants. Removes CSI sequences (`ESC [ … final`,
/// incl. SGR colors, cursor moves, mode sets, and DSR/DA queries), OSC sequences
/// (`ESC ] … BEL`/`ST`), and the common two-byte escapes (`ESC (` charset selects,
/// etc.). Bytes that are not part of an escape — including normal newlines/tabs — are
/// preserved verbatim. Operates on `char`s so it never splits a multibyte UTF-8 char.
///
/// This is intentionally a pragmatic stripper (not a full terminal emulator): it
/// drops the escape framing that buries the message, which is exactly the finding's
/// ask, without trying to interpret cursor motion into a reconstructed screen.
///
/// Note: the leading-orphan guard ([`strip_leading_csi_orphan`]) is NOT applied here —
/// it is only valid on a window that was cut at the head (see [`render_output`], which
/// applies it conditionally on `head_truncated`). Applying it unconditionally would
/// silently eat legitimate leading text like `2K…`/`0m…` from a full-buffer read.
/// Thin DELEGATION to the shared [`nyx_core::ansi::strip_ansi`] — the ONE implementation
/// both shells (and the shared MCP tool dispatch) clean output through, so the rule lives
/// in exactly one place (no duplicated CSI/OSC scanner).
fn strip_ansi(input: &str) -> String {
    nyx_core::ansi::strip_ansi(input)
}

/// Drop a leading orphan CSI/SGR fragment from the FRONT of a tail window
/// (review `01KV9618K0YEPER69A05CF52R6`). When [`bound_output`] slices the tail by
/// byte offset, the cut can land after a sequence's `ESC[` introducer, leaving its
/// bare tail — e.g. `1m`, `0;31m`, `?25l`, `2K` — at offset 0 with no `ESC` for
/// [`strip_ansi`] to anchor on. We consume an optional CSI private/param prefix
/// (`?`/`<`/`=`/`>` then `[0-9;]*`) followed by EXACTLY ONE CSI final byte
/// (`0x40..=0x7E`) and return the remainder.
///
/// Conservative on purpose, to avoid eating legitimate leading text:
/// - the fragment must end in a CSI final byte, so `100; done` (params but no
///   final letter) survives;
/// - and it must contain at least one DIGIT in its parameter run, so prose like
///   `;leading semicolon` (a `;` then a letter that merely falls in the final-byte
///   range) is NOT mistaken for a chopped `ESC[;l`.
///
/// All the real orphans the finding cites (`1m`, `0;31m`, `?25l`, `2K`) carry a
/// digit, so this loses nothing real. Anything starting with a true `ESC` is left
/// for `strip_ansi`'s main loop to handle normally.
fn strip_leading_csi_orphan(input: &str) -> &str {
    // A real, intact escape: hand it back untouched so the main loop strips it.
    if input.starts_with('\u{1b}') {
        return input;
    }
    let mut it = input.char_indices().peekable();

    // Optional CSI private-marker / parameter prefix.
    if let Some(&(_, c)) = it.peek() {
        if matches!(c, '?' | '<' | '=' | '>') {
            it.next();
        }
    }
    // Parameter bytes: digits and ';' separators. Track whether we saw a digit —
    // a digit-free param run (e.g. a bare leading `;`) is treated as plain text.
    let mut saw_digit = false;
    while let Some(&(_, c)) = it.peek() {
        if c.is_ascii_digit() {
            saw_digit = true;
            it.next();
        } else if c == ';' {
            it.next();
        } else {
            break;
        }
    }
    // Require a single CSI final byte AND a digit in the params to confirm this is
    // a chopped escape tail rather than ordinary text that happens to lead with
    // a separator and a letter.
    match it.peek() {
        Some(&(idx, c)) if saw_digit && ('\u{40}'..='\u{7e}').contains(&c) => {
            &input[idx + c.len_utf8()..]
        }
        // No digit-backed final byte ⇒ not a recognizable orphan; leave text intact.
        _ => input,
    }
}

/// Render the `output` field of `get_command_output` / `wait_for_command` from a RAW
/// byte window per the token-safe contract (review R-OUTPUT). This is the SINGLE place
/// the windowed text is shaped, so both tools agree.
///
/// - `strip` (default true): when set, the window is run through [`strip_ansi`] and the
///   result REPLACES `output` (one cleaned view, no parallel raw+text). When false the
///   raw window is returned verbatim. The line modes below ALWAYS operate on the
///   ANSI-stripped text (regex/line work on readable text), but the emitted lines are
///   taken from the stripped or raw form to honor `strip`.
/// - `grep`: keep only the lines that match the regex (matched against the stripped
///   text, so escapes never break the pattern). Applied before `tail_lines`.
/// - `tail_lines`: keep at most the last N lines (after any `grep`).
///
/// The byte cursor/total are computed by the caller on the RAW window, so reducing the
/// rendered text here never desyncs incremental polling.
fn render_output(
    raw_window: &str,
    strip: bool,
    grep: Option<&regex::Regex>,
    tail_lines: Option<usize>,
    head_truncated: bool,
) -> String {
    // The leading-orphan guard only makes sense when the window was cut at the HEAD (a
    // tail/`since` slice can begin mid-escape) AND we are cleaning. A full-buffer read, or
    // a raw (strip=false) read, keeps its first bytes verbatim — otherwise legitimate
    // leading text such as `2K…`/`0m…` (a digit + a final-byte letter) is silently eaten.
    let cleaned_source = if strip && head_truncated {
        strip_leading_csi_orphan(raw_window)
    } else {
        raw_window
    };
    // Cleaned text drives matching AND, when `strip`, the emitted text. When NOT
    // stripping we still match on cleaned lines but emit the corresponding raw lines.
    let cleaned = strip_ansi(cleaned_source);

    // Fast path: no line modes → just honor `strip`.
    if grep.is_none() && tail_lines.is_none() {
        return if strip { cleaned } else { raw_window.to_string() };
    }

    // Line modes work line-by-line. We pair each RAW line with its cleaned form so the
    // regex anchors on readable text while we can still emit the raw line when
    // strip=false. We re-join with '\n' and preserve a final newline if the source had
    // one. In RAW mode we split on '\n' WITHOUT dropping a preceding '\r' — `str::lines`
    // strips it, which would silently rewrite CRLF→LF and break the raw-verbatim promise;
    // a trailing '\n' yields a final empty element we drop here. In strip mode the
    // cleaned text may normalize line endings, which is acceptable.
    let emit_source = if strip { cleaned.as_str() } else { raw_window };
    let raw_lines: Vec<&str> = if strip {
        emit_source.lines().collect()
    } else {
        let mut v: Vec<&str> = emit_source.split('\n').collect();
        if emit_source.ends_with('\n') {
            v.pop();
        }
        v
    };
    // For matching we need the cleaned form of each emitted line. When strip=true the
    // emitted lines ARE the cleaned lines; when strip=false strip each raw line so the
    // pattern is not foiled by escapes embedded in that line.
    let mut kept: Vec<String> = Vec::new();
    for line in &raw_lines {
        let hay = if strip {
            (*line).to_string()
        } else {
            strip_ansi(line)
        };
        if let Some(re) = grep {
            if !re.is_match(&hay) {
                continue;
            }
        }
        kept.push((*line).to_string());
    }
    // tail_lines: keep at most the last N matching lines. `n == 0` means "no line cap"
    // (keep all), NOT "drop everything": draining to zero lines would return empty output
    // while returned_bytes>0 — a misleading "no output" on a non-empty window.
    if let Some(n) = tail_lines {
        if n > 0 && kept.len() > n {
            kept.drain(0..kept.len() - n);
        }
    }
    let trailing_nl = emit_source.ends_with('\n');
    let mut out = kept.join("\n");
    if trailing_nl && !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Read the OPTIONAL `grep` regex argument for `get_command_output` (task #4). `None`
/// when absent/null/empty. A present non-string, or an invalid regex, is an
/// `invalid_argument` error (D8) naming the compile failure so the agent can fix the
/// pattern. The pattern is matched per-line on the ANSI-stripped text.
fn optional_regex(args: &Value, key: &str) -> Result<Option<regex::Regex>, RpcError> {
    match optional_str(args, key)? {
        None => Ok(None),
        Some(pat) => regex::Regex::new(pat).map(Some).map_err(|e| {
            RpcError::new(
                RpcCode::InvalidArgument,
                format!("argument '{key}' is not a valid regex: {e}"),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the pure tool helpers — argument validation, the D8 error
    //! mapping, the cwd filter (absent / matching / ambiguous), and the output
    //! bounding (`tail_bytes` / `since` / `truncated` / UTF-8 safety). The
    //! dispatcher's end-to-end routing over a live runner + DB is covered by the
    //! `tauri::test` mock-runtime suite in `bridge` / CI (phase 5 owns running the
    //! lib tests; this machine cannot launch them — see the PRD env caveat).

    use super::*;

    // --- output bounding (D7) --------------------------------------------

    #[test]
    fn bound_output_under_tail_returns_all_untruncated() {
        let w = bound_output("hello world", 1024, None);
        assert_eq!(w.output, "hello world");
        assert_eq!(w.total_bytes, 11);
        assert_eq!(w.returned_bytes, 11);
        assert!(!w.truncated, "fits under tail → not truncated");
        assert_eq!(w.cursor, 11, "cursor is one past the end");
    }

    #[test]
    fn bound_output_keeps_the_tail_and_flags_truncation() {
        // 10 bytes, ask for the last 4: keep "6789", drop the head, flag truncated.
        let w = bound_output("0123456789", 4, None);
        assert_eq!(w.output, "6789", "keeps the most-recent tail");
        assert_eq!(w.total_bytes, 10);
        assert_eq!(w.returned_bytes, 4);
        assert!(w.truncated, "head was dropped → truncated");
        assert_eq!(w.cursor, 10);
    }

    #[test]
    fn bound_output_since_returns_only_new_bytes() {
        // The agent already read up to offset 6; only "6789" is new.
        let w = bound_output("0123456789", 1024, Some(6));
        assert_eq!(w.output, "6789", "since drops already-seen bytes");
        assert_eq!(w.total_bytes, 10, "total is the FULL size, not the window");
        assert_eq!(w.returned_bytes, 4);
        assert!(!w.truncated, "the tail reaches the new bytes → not truncated");
        assert_eq!(w.cursor, 10, "cursor advances so the next since resumes here");
    }

    #[test]
    fn bound_output_since_past_end_is_empty() {
        // Polling after the last cursor with no new output: an empty window, cursor
        // pinned at the end so a subsequent call still resumes correctly.
        let w = bound_output("0123456789", 1024, Some(10));
        assert_eq!(w.output, "");
        assert_eq!(w.returned_bytes, 0);
        assert!(!w.truncated);
        assert_eq!(w.cursor, 10);
    }

    #[test]
    fn bound_output_since_and_tail_page_forward_without_gap() {
        // Skip the first 2 bytes (since=2 → remaining "23456789"); the remaining
        // region (8 bytes) exceeds tail_bytes=3. With a `since`, we page FORWARD —
        // keep the FIRST 3 bytes ("234") and advance the cursor to 5, NOT tail to
        // "789" and jump the cursor to 10 (which would silently drop "234567").
        let w = bound_output("0123456789", 3, Some(2));
        assert_eq!(w.output, "234", "incremental read keeps the HEAD of the new region");
        assert!(w.truncated, "more remains after this window");
        assert_eq!(w.cursor, 5, "cursor advances only past what was returned");
        // The next since=cursor read continues contiguously: no gap, no dup.
        let w2 = bound_output("0123456789", 3, Some(w.cursor));
        assert_eq!(w2.output, "567", "resumes exactly where the last window ended");
        assert_eq!(w2.cursor, 8);
        let w3 = bound_output("0123456789", 3, Some(w2.cursor));
        assert_eq!(w3.output, "89", "final partial window");
        assert!(!w3.truncated, "the tail now fits → not truncated");
        assert_eq!(w3.cursor, 10);
    }

    #[test]
    fn bound_output_since_past_end_signals_reset_and_returns_new_content() {
        // The buffer SHRANK below the caller's cursor (a `clear`/reset): a stale
        // since=20 over a 10-byte buffer must NOT silently return empty. We flag
        // `reset` and fall back to a fresh tail read so the caller sees the new
        // content rather than nothing.
        let w = bound_output("0123456789", 1024, Some(20));
        assert!(w.reset, "since beyond the end is a reset signal");
        assert_eq!(w.output, "0123456789", "falls back to the new content, not empty");
        assert_eq!(w.cursor, 10, "cursor reflects the new (smaller) end");
        // The ordinary 'nothing new yet' case (since == total) is NOT a reset.
        let at_end = bound_output("0123456789", 1024, Some(10));
        assert!(!at_end.reset, "since exactly at the end is not a reset");
        assert_eq!(at_end.output, "");
    }

    #[test]
    fn bound_output_never_splits_a_utf8_char() {
        // "éé" is 4 bytes (2 each). A 3-byte tail would cut mid-char; we snap UP to a
        // boundary so the result is the last whole char (still valid UTF-8).
        let s = "éé"; // bytes: C3 A9 C3 A9
        let w = bound_output(s, 3, None);
        assert!(w.output.is_char_boundary(0));
        assert_eq!(w.output, "é", "snapped up to a whole char");
        assert!(w.truncated);
    }

    #[test]
    fn bound_output_since_snaps_to_char_boundary() {
        // since=1 lands mid-"é"; snapping UP to byte 2 yields the second whole char.
        let w = bound_output("éé", 1024, Some(1));
        assert_eq!(w.output, "é");
    }

    #[test]
    fn bound_output_never_splits_a_4byte_emoji() {
        // B2: 4-byte emoji (🔥 = F0 9F 94 A5) at a tail boundary. A 3-byte window
        // would land mid-codepoint; ceil_char_boundary must snap up to exclude it.
        let s = "ok🔥";  // "ok" = 2 bytes; "🔥" = 4 bytes → total 6 bytes
        // Request 3 bytes from the tail: the emoji starts at byte 2, so a naive
        // 3-byte window would span bytes 2..5, right in the middle of the emoji.
        let w = bound_output(s, 3, None);
        // The emoji must NOT be split: we should get just "🔥" (snapped up to 4).
        assert!(std::str::from_utf8(w.output.as_bytes()).is_ok(), "output is valid UTF-8");
        assert!(w.truncated, "the head was dropped");
        // Verify the emoji is either included whole or excluded: no partial encoding.
        assert!(
            w.output == "🔥" || w.output.is_empty(),
            "emoji is either whole or absent, got: {:?}",
            w.output
        );
    }

    #[test]
    fn bound_output_since_snaps_past_4byte_emoji() {
        // B2: a `since` that lands mid-emoji snaps UP to the next char boundary,
        // so the resumed window starts at a clean codepoint.
        let s = "A🔥B";  // A=1 byte, 🔥=4 bytes, B=1 byte → total 6 bytes
        // since=2 lands at byte 2, which is the 2nd byte of 🔥 (not a char boundary).
        let w = bound_output(s, 1024, Some(2));
        assert!(std::str::from_utf8(w.output.as_bytes()).is_ok(), "output is valid UTF-8");
        // After snapping since=2 UP to byte 5 (past the emoji), we get "B".
        assert_eq!(w.output, "B", "snapped since past the emoji codepoint");
    }

    #[test]
    fn bound_output_tail_and_since_both_valid_utf8_with_mixed_content() {
        // B2: mixed ASCII + 2-byte accents + 4-byte emoji; both tail cut and since
        // snap must produce valid UTF-8, never a broken codepoint.
        let s = "café🎉done"; // c(1) a(1) f(1) é(2) 🎉(4) d(1) o(1) n(1) e(1) = 12 bytes
        // Tail cut: request 10 bytes from the tail (should snap to a char boundary).
        let w = bound_output(s, 10, None);
        assert!(std::str::from_utf8(w.output.as_bytes()).is_ok(), "tail is valid UTF-8");
        // Since cut: start at byte 3 (mid-é) — must snap UP.
        let w2 = bound_output(s, 1024, Some(3));
        assert!(std::str::from_utf8(w2.output.as_bytes()).is_ok(), "since-resumed is valid UTF-8");
    }

    #[test]
    fn cursor_round_trips_verbatim_as_next_since() {
        // The incremental-poll contract (ADR-0003 §7): the integer `cursor`
        // emitted by one call is accepted VERBATIM as the next `since`. We
        // reproduce the exact seam the impl uses — emit `cursor` into the result
        // JSON exactly as `get_command_output` does (`window.cursor`), then read
        // that same JSON value back through `optional_usize("since")` (no
        // `invalid_argument`) and feed it into the next `bound_output`.
        let full = "0123456789";

        // First poll: read the whole buffer; capture the emitted cursor.
        let first = bound_output(full, 1024, None);
        assert_eq!(first.output, "0123456789");
        let result = json!({ "cursor": first.cursor });

        // Echo the opaque cursor back as the next `since`, exactly as a client
        // would. It must parse without error (this is what was broken when the
        // cursor was emitted as a String).
        let since = optional_usize(&result, "cursor")
            .expect("emitted cursor must be accepted verbatim as an integer since");
        assert_eq!(since, Some(10), "cursor parsed back to the byte offset");

        // Second poll resumes right after the first window: no new bytes yet,
        // cursor still pinned at the end so the loop keeps working.
        let second = bound_output(full, 1024, since);
        assert_eq!(second.output, "", "nothing new since the last cursor");
        assert_eq!(second.returned_bytes, 0);
        assert!(!second.truncated);
        assert_eq!(second.cursor, 10, "cursor stays put for the next round-trip");

        // And once more output arrives, the resumed window returns only the new
        // bytes — the round-trip resumes correctly rather than re-sending all.
        let grown = "0123456789ABCDE";
        let third = bound_output(grown, 1024, since);
        assert_eq!(third.output, "ABCDE", "resumes from the round-tripped cursor");
        assert_eq!(third.cursor, 15);
    }

    // --- cwd filter (D5): absent / matching / ambiguous -------------------

    #[test]
    fn path_matches_exact_and_nested() {
        assert!(path_matches("/home/u/app", "/home/u/app"), "exact match");
        assert!(
            path_matches("/home/u/app", "/home/u/app/src"),
            "cwd under the workspace matches"
        );
        assert!(
            path_matches("/home/u/app/pkg", "/home/u/app"),
            "workspace under the cwd matches"
        );
        assert!(
            !path_matches("/home/u/app", "/home/u/apple"),
            "a prefix that is not a path segment must NOT match"
        );
        assert!(
            !path_matches("/home/u/app", "/var/other"),
            "an unrelated path must not match"
        );
    }

    #[test]
    fn path_matches_is_ambiguous_when_several_workspaces_share_a_root() {
        // The cwd filter is a FILTER, not a resolver: two workspaces both under the
        // same cwd BOTH match, so the listing stays ambiguous and the agent picks by
        // id — it is never silently resolved to one. (ADR-0003 D5.)
        let cwd = "/home/u/monorepo";
        let a = "/home/u/monorepo/app-a";
        let b = "/home/u/monorepo/app-b";
        assert!(path_matches(a, cwd));
        assert!(path_matches(b, cwd));
    }

    // --- argument validation (D8) ----------------------------------------

    #[test]
    fn require_str_rejects_missing_and_empty() {
        let args = json!({ "project_id": "p1", "blank": "" });
        assert_eq!(require_str(&args, "project_id").unwrap(), "p1");
        assert_eq!(
            require_str(&args, "missing").unwrap_err().code,
            "invalid_argument"
        );
        assert_eq!(
            require_str(&args, "blank").unwrap_err().code,
            "invalid_argument",
            "an empty id is never a valid reference"
        );
    }

    #[test]
    fn optional_str_treats_empty_and_null_as_absent() {
        let args = json!({ "cwd": "", "other": null, "set": "/x" });
        assert_eq!(optional_str(&args, "cwd").unwrap(), None, "empty → no filter");
        assert_eq!(optional_str(&args, "other").unwrap(), None, "null → absent");
        assert_eq!(optional_str(&args, "missing").unwrap(), None, "absent");
        assert_eq!(optional_str(&args, "set").unwrap(), Some("/x"));
        let bad = json!({ "cwd": 5 });
        assert_eq!(
            optional_str(&bad, "cwd").unwrap_err().code,
            "invalid_argument",
            "a non-string filter is rejected"
        );
    }

    #[test]
    fn optional_usize_rejects_negative_and_non_integer() {
        let args = json!({ "tail_bytes": 4096, "neg": -1, "txt": "x" });
        assert_eq!(optional_usize(&args, "tail_bytes").unwrap(), Some(4096));
        assert_eq!(optional_usize(&args, "absent").unwrap(), None);
        assert_eq!(
            optional_usize(&args, "neg").unwrap_err().code,
            "invalid_argument",
            "tail_bytes must be >= 0"
        );
        assert_eq!(
            optional_usize(&args, "txt").unwrap_err().code,
            "invalid_argument"
        );
    }

    #[test]
    fn optional_bool_parses_and_rejects_non_bool() {
        let args = json!({ "strip_ansi": true, "off": false, "nul": null, "bad": "yes" });
        assert_eq!(optional_bool(&args, "strip_ansi").unwrap(), Some(true));
        assert_eq!(optional_bool(&args, "off").unwrap(), Some(false));
        assert_eq!(optional_bool(&args, "nul").unwrap(), None, "null → absent");
        assert_eq!(optional_bool(&args, "absent").unwrap(), None);
        assert_eq!(
            optional_bool(&args, "bad").unwrap_err().code,
            "invalid_argument",
            "a non-boolean strip_ansi is rejected"
        );
    }

    // --- run status surfacing (#13): exit_code / running / finished -------

    #[test]
    fn status_json_running_has_null_exit_code() {
        // A live run: running=true, finished=false, exit_code=null even though no
        // code exists yet (the run has not ended). A running command is not unread.
        let s = status_json(RunState::Running, None, false);
        assert_eq!(s["state"], "running");
        assert_eq!(s["running"], true);
        assert_eq!(s["finished"], false);
        assert!(s["exit_code"].is_null(), "no exit code while running");
        assert_eq!(s["unread"], false, "a running command is not an unseen result");
    }

    #[test]
    fn status_json_clean_exit_is_success_with_zero() {
        // A clean run: state=success, finished=true, exit_code=0 — distinguishable
        // from a bare idle (the gap finding #13 reported). A fresh finish is unread.
        let s = status_json(RunState::Success, Some(0), true);
        assert_eq!(s["state"], "success");
        assert_eq!(s["running"], false);
        assert_eq!(s["finished"], true);
        assert_eq!(s["exit_code"], 0, "a clean finish surfaces exit_code 0");
        assert_eq!(s["unread"], true, "a fresh finish is an unseen result");
    }

    #[test]
    fn status_json_non_zero_exit_surfaces_the_crash_code() {
        // The crux of finding #13 (done_criterion: "a non-zero-exit command surfaces
        // its exit_code and a success/ERROR outcome, not idle"): a command that exits
        // non-zero is `error` + finished + carries its non-zero code, NOT a silent
        // idle — so an agent can tell a crash from a clean run.
        let s = status_json(RunState::Error, Some(2), true);
        assert_eq!(s["state"], "error", "a non-zero exit is an error outcome, not idle");
        assert_eq!(s["running"], false);
        assert_eq!(s["finished"], true);
        assert_eq!(s["exit_code"], 2, "the crash exit code is surfaced");

        // And the same fields, prefixed with instance_id, are what the action tools
        // return — proving the start/stop/relaunch result shape carries the crash too.
        let wrapped = status_result("inst-7", s);
        assert_eq!(wrapped["instance_id"], "inst-7");
        assert_eq!(wrapped["state"], "error");
        assert_eq!(wrapped["exit_code"], 2);
    }

    #[test]
    fn status_json_error_after_ack_keeps_the_crash_code_only_unread_flips() {
        // The v4 finding: after a UI acknowledge the FACTUAL outcome is unchanged —
        // state=error + exit_code=2 — and only `unread` flips to false. status_json
        // is the projection the MCP reports, so an acked error still reads as a crash.
        let s = status_json(RunState::Error, Some(2), false);
        assert_eq!(s["state"], "error", "ack does NOT erase the factual error state");
        assert_eq!(s["finished"], true);
        assert_eq!(s["exit_code"], 2, "ack does NOT erase the crash exit code");
        assert_eq!(s["unread"], false, "ack flipped only the unread flag");
    }

    #[test]
    fn status_json_idle_has_no_exit_code_even_with_prior_code() {
        // A prior run left a code, but a fresh idle/never-finished state reports null:
        // the new (non-)run has produced no completion of its own.
        let s = status_json(RunState::Idle, Some(1), false);
        assert_eq!(s["state"], "idle");
        assert_eq!(s["finished"], false);
        assert!(s["exit_code"].is_null(), "idle reports no exit code");
    }

    // --- strip_ansi (#15) -------------------------------------------------

    #[test]
    fn strip_ansi_removes_sgr_colors_keeping_text() {
        // The useful message buried under SGR color codes (finding #15).
        let raw = "\u{1b}[31merror:\u{1b}[0m build \u{1b}[1mfailed\u{1b}[22m\n";
        assert_eq!(strip_ansi(raw), "error: build failed\n");
    }

    #[test]
    fn strip_ansi_removes_cursor_and_mode_and_query_sequences() {
        // Cursor moves (CSI H/J/K), private mode sets (CSI ? … h/l), and a DSR query
        // (CSI 6n) — exactly the noise the finding cites — all stripped; text kept.
        let raw = "\u{1b}[2J\u{1b}[H\u{1b}[?25lhello\u{1b}[?25h\u{1b}[6n world";
        assert_eq!(strip_ansi(raw), "hello world");
    }

    #[test]
    fn strip_ansi_removes_osc_title_sequence() {
        // OSC (set window title), terminated by BEL — dropped, surrounding text kept.
        let raw = "\u{1b}]0;my title\u{7}done";
        assert_eq!(strip_ansi(raw), "done");
        // OSC terminated by ST (ESC \) is handled too.
        let raw_st = "\u{1b}]0;t\u{1b}\\done";
        assert_eq!(strip_ansi(raw_st), "done");
    }

    #[test]
    fn strip_ansi_is_a_noop_on_clean_text_and_preserves_utf8() {
        assert_eq!(strip_ansi("plain text\nline2\t end"), "plain text\nline2\t end");
        // Multibyte chars adjacent to escapes survive intact.
        assert_eq!(strip_ansi("\u{1b}[32mréussi é\u{1b}[0m"), "réussi é");
    }

    #[test]
    fn render_output_drops_leading_orphan_only_on_a_head_cut() {
        // The finding: a tail/`since` window can begin right after an `ESC[` introducer,
        // leaving its orphan tail (`1m`, `0;31m`, `?25l`, `2K`) at offset 0. The slice
        // helper drops exactly that fragment...
        assert_eq!(strip_leading_csi_orphan("1mbuild failed\n"), "build failed\n");
        assert_eq!(strip_leading_csi_orphan("0;31merror here"), "error here");
        assert_eq!(strip_leading_csi_orphan("?25lhidden cursor"), "hidden cursor");
        assert_eq!(strip_leading_csi_orphan("2Kcleared line"), "cleared line");
        // ...and render_output applies it ONLY on a head-cut window (head_truncated=true),
        // composing with the rest of the window being stripped normally.
        assert_eq!(
            render_output("1mfailed\u{1b}[0m done", true, None, None, true),
            "failed done"
        );
    }

    #[test]
    fn full_buffer_read_never_eats_leading_digit_letter_text() {
        // Finding #11: the orphan guard must NOT run on a full-buffer (head_truncated=false)
        // read. strip_ansi alone never strips a leading orphan, so legitimate leading text
        // like "2K…"/"0m…"/"4K…" (a digit + a final-byte letter) survives verbatim.
        assert_eq!(strip_ansi("2Kcleared line"), "2Kcleared line");
        assert_eq!(strip_ansi("0m left on the clock"), "0m left on the clock");
        assert_eq!(strip_ansi("4K video ready\n"), "4K video ready\n");
        assert_eq!(
            render_output("2Kcleared line", true, None, None, false),
            "2Kcleared line"
        );
        // Real (intact) escapes are still stripped on a full-buffer read, and ordinary
        // leading text always survives.
        assert_eq!(strip_ansi("\u{1b}[1mbold\u{1b}[0m tail"), "bold tail");
        assert_eq!(strip_ansi("mostly clean"), "mostly clean");
        assert_eq!(strip_ansi("12345 widgets"), "12345 widgets");
    }

    #[test]
    fn bound_output_tail_cut_mid_escape_keeps_raw_exact_but_text_has_no_orphan() {
        // ADR-0003 D7 windowing slices by BYTE, so a tail cut can land right after an
        // `ESC[` introducer, orphaning its parameter/final tail (`1m`). The raw
        // `output` must stay byte-exact (cursor math unchanged); only `text` is cleaned.
        let full = "noise\u{1b}[1mfailed\n"; // ESC[ is at byte index 5..7.
        // Choose a tail that starts INSIDE the escape: keep the last bytes such that
        // the window begins at the `1m...` (one past the `ESC[`).
        let from_one_m = full.find('1').unwrap(); // index of the orphan `1m`.
        let tail = full.len() - from_one_m;
        let window = bound_output(full, tail, None);

        // Raw output is byte-exact: it still starts with the orphan `1m`.
        assert_eq!(window.output, "1mfailed\n");
        assert!(window.output.starts_with("1m"));
        assert!(window.truncated);
        // The window was cut at the head, so the orphan guard is eligible...
        assert!(window.head_truncated);
        // ...and render_output drops the orphan — no leading `1m` residue.
        let text = render_output(&window.output, true, None, None, window.head_truncated);
        assert_eq!(text, "failed\n");
        assert!(!text.starts_with("1m"));
    }

    // --- render_output: token-safe strip-replaces-output + line modes (R-OUTPUT) ---

    #[test]
    fn render_output_default_size_is_token_safe() {
        // Review R-OUTPUT task #1: a DEFAULT read of a large buffer stays well under the
        // MCP token cap. The default tail window is 12 KiB (cleaned), an order of
        // magnitude below the old 64 KiB that blew the budget.
        // The default tail window sits in the token-safe 8-16 KiB band.
        assert_eq!(DEFAULT_TAIL_BYTES, 12 * 1024, "default tail window is 12 KiB");
        // A buffer far larger than the default is bounded to the default tail.
        let big = "x".repeat(200 * 1024);
        let w = bound_output(&big, DEFAULT_TAIL_BYTES, None);
        assert_eq!(w.returned_bytes, DEFAULT_TAIL_BYTES, "bounded to the token-safe tail");
        assert!(w.truncated, "the head was dropped");
        assert_eq!(w.total_bytes, 200 * 1024, "total_bytes is the FULL size (byte-exact)");
    }

    #[test]
    fn render_output_strip_true_replaces_output_with_one_cleaned_field() {
        // strip_ansi=true (the default) returns ONE cleaned `output` — NOT a raw output
        // plus a parallel `text`. The escapes are gone; the readable text remains.
        let raw = "\u{1b}[31merror:\u{1b}[0m boom\n";
        let out = render_output(raw, true, None, None, false);
        assert_eq!(out, "error: boom\n", "stripped output is the single cleaned view");
        assert!(!out.contains('\u{1b}'), "no escape bytes leak into the cleaned output");
    }

    #[test]
    fn render_output_strip_false_returns_raw_window_verbatim() {
        // strip_ansi=false returns the RAW window byte-for-byte (escapes preserved), so
        // an agent that wants the raw bytes can still get them.
        let raw = "\u{1b}[31merror:\u{1b}[0m boom\n";
        let out = render_output(raw, false, None, None, false);
        assert_eq!(out, raw, "raw output is byte-exact when strip_ansi=false");
    }

    #[test]
    fn render_output_grep_keeps_only_matching_lines_on_stripped_text() {
        // task #4: a regex `grep` returns only the matching lines, matched on the
        // ANSI-stripped text so color codes never foil the pattern.
        let raw = "\u{1b}[32mstarting up\u{1b}[0m\n\u{1b}[31mERROR: boom\u{1b}[0m\nlistening on :3000\n";
        let re = regex::Regex::new("ERROR").unwrap();
        let out = render_output(raw, true, Some(&re), None, false);
        assert_eq!(out, "ERROR: boom\n", "only the matching line, cleaned");
    }

    #[test]
    fn render_output_grep_matches_even_when_not_stripping_output() {
        // strip_ansi=false still matches on the cleaned per-line text, but EMITS the raw
        // matching line (escapes preserved in the emitted output).
        let raw = "ok\n\u{1b}[31mERROR boom\u{1b}[0m\n";
        let re = regex::Regex::new("ERROR").unwrap();
        let out = render_output(raw, false, Some(&re), None, false);
        assert_eq!(
            out, "\u{1b}[31mERROR boom\u{1b}[0m\n",
            "raw matching line is emitted (escapes preserved), trailing newline kept"
        );
    }

    #[test]
    fn render_output_tail_lines_keeps_last_n_lines() {
        // task #4: `tail_lines` keeps the last N lines of the window.
        let raw = "l1\nl2\nl3\nl4\nl5\n";
        let out = render_output(raw, true, None, Some(2), false);
        assert_eq!(out, "l4\nl5\n", "keeps the last 2 lines, trailing newline preserved");
    }

    #[test]
    fn render_output_grep_then_tail_lines_compose() {
        // grep first (keep matching), then tail_lines (last N of the matches).
        let raw = "a: ok\nb: ERR\nc: ok\nd: ERR\ne: ERR\n";
        let re = regex::Regex::new("ERR").unwrap();
        let out = render_output(raw, true, Some(&re), Some(2), false);
        assert_eq!(out, "d: ERR\ne: ERR\n", "last 2 of the matching lines");
    }

    #[test]
    fn optional_regex_compiles_or_rejects() {
        let ok = json!({ "grep": "err.*boom" });
        assert!(optional_regex(&ok, "grep").unwrap().is_some(), "a valid regex compiles");
        assert!(optional_regex(&json!({}), "grep").unwrap().is_none(), "absent → None");
        assert!(
            optional_regex(&json!({ "grep": "" }), "grep").unwrap().is_none(),
            "empty → None (no filter)"
        );
        let bad = json!({ "grep": "(" });
        assert_eq!(
            optional_regex(&bad, "grep").unwrap_err().code,
            "invalid_argument",
            "an invalid regex is invalid_argument, not internal"
        );
    }

    // --- create_workspace error mapping (D8) ------------------------------

    #[test]
    fn create_workspace_err_maps_fk_to_invalid_id() {
        use diesel::result::{DatabaseErrorKind, Error as DieselError};
        // A real diesel error carries a boxed message; the typed kind drives the map.
        let err = DieselError::DatabaseError(
            DatabaseErrorKind::ForeignKeyViolation,
            Box::new("FOREIGN KEY constraint failed".to_string()),
        );
        assert_eq!(map_create_workspace_err("proj-x", err).code, "invalid_id");
    }

    #[test]
    fn create_workspace_err_maps_unique_to_invalid_state() {
        use diesel::result::{DatabaseErrorKind, Error as DieselError};
        let err = DieselError::DatabaseError(
            DatabaseErrorKind::UniqueViolation,
            Box::new("UNIQUE constraint failed: workspaces.project_id, workspaces.path".to_string()),
        );
        assert_eq!(map_create_workspace_err("proj-x", err).code, "invalid_state");
    }

    #[test]
    fn create_workspace_err_classifies_generic_constraint_on_message() {
        use diesel::result::{DatabaseErrorKind, Error as DieselError};
        // SQLite sometimes reports a FK failure as a generic constraint; the message
        // fallback must still classify it as invalid_id.
        let err = DieselError::DatabaseError(
            DatabaseErrorKind::Unknown,
            Box::new("FOREIGN KEY constraint failed".to_string()),
        );
        assert_eq!(map_create_workspace_err("proj-x", err).code, "invalid_id");
    }

    #[test]
    fn basename_takes_last_segment() {
        assert_eq!(basename("/home/u/my-app"), "my-app");
        assert_eq!(basename(r"C:\Users\k\proj"), "proj");
        assert_eq!(basename("/home/u/app/"), "app", "trailing slash ignored");
        assert_eq!(basename("solo"), "solo");
    }

    // --- probe spike tool (PRD-4 #7, ADR-0004) ---------------------------

    /// The `probe` tool routes through the REAL [`NyxToolDispatcher`] on a
    /// `tauri::test` mock app that has NO managed `Db`/runner — proving the probe
    /// answers `{ ok: true }` even while the runtime layer is unreachable (the
    /// SessionStart-hook / "MCP just came up" case). This is the loopback-style
    /// proof the PRD env caveat asks for; it does NOT spawn any real session.
    ///
    /// (Lives behind the same mock-runtime seam as the `bridge` suite. If
    /// `cargo test --lib` cannot launch on this machine — the known
    /// `STATUS_ENTRYPOINT_NOT_FOUND` ConPTY gap — `cargo check --tests` still
    /// type-checks it, and CI runs it out-of-band.)
    #[test]
    fn probe_returns_trivial_ok_without_managed_state() {
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app builds");
        // Deliberately do NOT manage a Db or ManagedCommandRunner: the probe must
        // not depend on them (unlike every other tool, which returns mcp_unavailable
        // when the runtime is absent).
        let dispatcher = NyxToolDispatcher::new(app.handle().clone());

        let result = dispatcher
            .call(PROBE_TOOL, &json!({}))
            .expect("probe never errors — it is a no-op liveness tool");
        assert_eq!(result["ok"], true, "probe reports liveness");
        assert_eq!(result["server"], env!("CARGO_PKG_NAME"));
        assert!(result["version"].is_string(), "probe carries the nyx version");
        // D1: probe must carry build_sha (C1 + D1) and schema_ok.
        assert!(result["build_sha"].is_string(), "probe carries build_sha");
        // PRD-4.1 task #6: probe exposes a BOOLEAN build_dirty, and build_sha is INTACT
        // (no `-dirty` suffix that would corrupt the sha). The value is build-env
        // dependent (true off an uncommitted tree, false off a clean commit), so we
        // assert only the TYPE + the sha's non-corruption here.
        assert!(
            result["build_dirty"].is_boolean(),
            "probe exposes build_dirty as a boolean, got {:?}",
            result["build_dirty"]
        );
        assert!(
            !result["build_sha"].as_str().unwrap().ends_with("-dirty"),
            "build_sha must stay intact — dirtiness lives in build_dirty, not a sha suffix"
        );
        // Without a managed Db, schema_ok defaults to true (no evidence of lag).
        assert_eq!(result["schema_ok"], true, "no Db → schema_ok defaults to true");
    }

    #[test]
    fn probe_reports_schema_ok_true_when_migrations_are_applied() {
        // D1: probe schema_ok=true when the DB is fully migrated (in_memory always is).
        use tauri::test::{mock_builder, mock_context, noop_assets};
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app builds");
        let db = Db::in_memory();
        app.manage(db);
        let dispatcher = NyxToolDispatcher::new(app.handle().clone());
        let result = dispatcher
            .call(PROBE_TOOL, &json!({}))
            .expect("probe succeeds");
        assert_eq!(result["ok"], true, "probe is always ok");
        assert_eq!(
            result["schema_ok"], true,
            "a fully-migrated in-memory DB → schema_ok:true"
        );
        assert!(
            result["schema_warning"].is_null(),
            "no warning when schema is up to date"
        );
    }

    // --- id resolution against a live Db (findings #2 + #4) ---------------
    //
    // `bad_instance_id_error` (finding #2: template-vs-instance disambiguation)
    // and `resolve_instance_id` (finding #4: `{ name, workspace_id }` resolution)
    // both reach the managed `Db` — the unit suite above only covered the PURE
    // helpers, leaving these DB-backed paths to the heavier `tauri::test` suite.
    // These tests close that gap with a self-contained in-memory `Db`: they build
    // the REAL `NyxToolDispatcher` over a mock app managing `Db::in_memory()`
    // (neither method touches the runner), seed a project/workspace/template — which
    // auto-materializes one instance per workspace — and exercise success + every
    // error branch. (Same mock-runtime seam as `probe`/`bridge`; `cargo test --lib`
    // can't launch on this machine — see the env caveat — but `--no-run` type-checks
    // them and CI runs them.)

    // `db` is already in scope via `use super::*`; bring in the extra types the
    // seeding helpers need (CommandSource for hand-authored templates).
    use crate::db::CommandSource;
    use diesel::RunQueryDsl;
    use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};

    /// A mock dispatcher backed by an in-memory migrated `Db` and the ids of a
    /// seeded graph: `project_id`, `workspace_id`, the template `command_id`, and the
    /// auto-materialized `instance_id`. `create_template` materializes exactly one
    /// instance into the project's single workspace (`create_project`'s root), so the
    /// graph is project → workspace → template → one instance.
    struct Seeded {
        dispatcher: NyxToolDispatcher<MockRuntime>,
        workspace_id: String,
        command_id: String,
        instance_id: String,
        // Held so the managed `Db` outlives the dispatcher's `AppHandle` borrows.
        _app: tauri::App<MockRuntime>,
    }

    fn seed_dispatcher(template_name: &str) -> Seeded {
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app builds");
        let db = Db::in_memory();
        let (workspace_id, command_id, instance_id) = db.with_conn(|c| {
            let (_project, workspace) =
                db::create_project(c, "proj", "/tmp/nyx-test-ws", None).expect("create project");
            let template = db::create_template(
                c,
                &workspace.project_id,
                template_name,
                "npm run dev",
                None,
                CommandSource::default(),
            )
            .expect("create template");
            // create_template materialized one instance into the root workspace.
            let instances = db::list_instances_for_workspace(c, &workspace.id)
                .expect("list seeded instances");
            assert_eq!(instances.len(), 1, "one instance materialized per workspace");
            (workspace.id, template.id, instances[0].id.clone())
        });
        app.manage(db);
        let dispatcher = NyxToolDispatcher::new(app.handle().clone());
        Seeded {
            dispatcher,
            workspace_id,
            command_id,
            instance_id,
            _app: app,
        }
    }

    // --- finding #2: bad_instance_id_error template-vs-instance -----------

    #[test]
    fn bad_instance_id_error_names_template_path_for_a_command_id() {
        // Passing a template `command_id` (e.g. from list_commands(project_id=…)) to
        // an action tool must yield an ACTIONABLE invalid_id that names the correct
        // path, not a bare "unknown" — the crux of finding #2.
        let s = seed_dispatcher("dev");
        let err = s.dispatcher.bad_instance_id_error(&s.command_id);
        assert_eq!(err.code, "invalid_id");
        assert!(
            err.message.contains("TEMPLATE"),
            "a known command_id is flagged as a template, got: {}",
            err.message
        );
        assert!(
            err.message.contains("list_commands(workspace_id"),
            "the message names the launchable path, got: {}",
            err.message
        );
    }

    #[test]
    fn bad_instance_id_error_is_generic_for_a_truly_unknown_id() {
        // An id that is neither an instance nor a template gets the generic unknown
        // error — still actionable (it hints at the workspace_id form) but not the
        // template-specific message.
        let s = seed_dispatcher("dev");
        let err = s.dispatcher.bad_instance_id_error("totally-unknown-id");
        assert_eq!(err.code, "invalid_id");
        assert!(
            !err.message.contains("TEMPLATE"),
            "an unknown (non-template) id is not flagged as a template, got: {}",
            err.message
        );
        assert!(
            err.message.contains("unknown command instance"),
            "the generic branch names the unknown instance, got: {}",
            err.message
        );
    }

    #[test]
    fn assert_instance_exists_rejects_a_template_id_with_the_actionable_error() {
        // The stop_command guard path: a template command_id is NOT a launchable
        // instance, so assert_instance_exists surfaces the same actionable invalid_id
        // (finding #2) rather than a silent idempotent success.
        let s = seed_dispatcher("dev");
        // A real instance passes.
        s.dispatcher
            .assert_instance_exists(&s.instance_id)
            .expect("a materialized instance exists");
        // A template id is rejected with the template-vs-instance error.
        let err = s
            .dispatcher
            .assert_instance_exists(&s.command_id)
            .expect_err("a template command_id is not a launchable instance");
        assert_eq!(err.code, "invalid_id");
        assert!(err.message.contains("TEMPLATE"), "got: {}", err.message);
    }

    // --- finding #4: resolve_instance_id { name, workspace_id } -----------

    #[test]
    fn resolve_instance_id_prefers_an_explicit_instance_id() {
        // The canonical path: an explicit instance_id is used verbatim, the name form
        // ignored (existence is validated downstream, not here).
        let s = seed_dispatcher("dev");
        let resolved = s
            .dispatcher
            .resolve_instance_id(&json!({ "instance_id": "verbatim-id" }))
            .expect("explicit instance_id passes through");
        assert_eq!(resolved, "verbatim-id");
    }

    #[test]
    fn resolve_instance_id_resolves_a_unique_name_in_the_workspace() {
        // Finding #4 success: { name, workspace_id } resolves to the single matching
        // instance's id, so launching "dev" needs no list_commands round-trip first.
        let s = seed_dispatcher("dev");
        let resolved = s
            .dispatcher
            .resolve_instance_id(&json!({ "name": "dev", "workspace_id": s.workspace_id }))
            .expect("a unique name resolves");
        assert_eq!(resolved, s.instance_id, "resolved to the materialized instance");
    }

    #[test]
    fn resolve_instance_id_unknown_name_is_invalid_id() {
        // An unknown name in a real workspace → invalid_id (no silent pick).
        let s = seed_dispatcher("dev");
        let err = s
            .dispatcher
            .resolve_instance_id(&json!({ "name": "nope", "workspace_id": s.workspace_id }))
            .expect_err("an unknown name does not resolve");
        assert_eq!(err.code, "invalid_id");
        assert!(
            err.message.contains("nope"),
            "the error names the missing command, got: {}",
            err.message
        );
    }

    #[test]
    fn resolve_instance_id_missing_workspace_id_is_invalid_argument() {
        // The name form REQUIRES workspace_id alongside name; absent it is an
        // invalid_argument (not a guess across all workspaces).
        let s = seed_dispatcher("dev");
        let err = s
            .dispatcher
            .resolve_instance_id(&json!({ "name": "dev" }))
            .expect_err("name without workspace_id is rejected");
        assert_eq!(err.code, "invalid_argument");
        assert!(
            err.message.contains("workspace_id"),
            "the error explains the missing workspace_id, got: {}",
            err.message
        );
    }

    #[test]
    fn resolve_instance_id_neither_form_is_invalid_argument() {
        // Neither instance_id nor { name, workspace_id } → invalid_argument.
        let s = seed_dispatcher("dev");
        let err = s
            .dispatcher
            .resolve_instance_id(&json!({}))
            .expect_err("an empty arg set is rejected");
        assert_eq!(err.code, "invalid_argument");
    }

    #[test]
    fn resolve_instance_id_ambiguous_name_is_invalid_state_listing_ids() {
        // Finding #4 ambiguity: when two instances of a workspace share a template
        // `name`, the name is ambiguous → invalid_state that LISTS the candidate
        // instance_ids (never a silent pick — mirrors the D5 cwd rule).
        //
        // The schema's `UNIQUE(managed_commands.project_id, name)` +
        // `UNIQUE(command_instances.command_id, workspace_id)` mean the public
        // create/materialize API never yields two same-named instances in ONE
        // workspace — the ambiguity branch is a DEFENSIVE guard. To reach it we craft
        // exactly that state: a SECOND project owns a template ALSO named "dup"
        // (allowed — uniqueness is per-project), and we pair THAT template with the
        // first workspace via a raw `command_instances` insert (the FK only requires
        // command_id + workspace_id to exist; there is no project-match constraint, and
        // `list_instances_for_workspace` filters purely by workspace_id). This is the
        // only in-scope way to construct the guarded state without a db.rs test helper.
        let s = seed_dispatcher("dup");
        let second_instance_id = s.dispatcher.db().unwrap().with_conn(|c| {
            // A second project with a template of the SAME name (per-project unique).
            let (p2, _w2) =
                db::create_project(c, "proj2", "/tmp/nyx-test-ws2", None).expect("project 2");
            let t2 = db::create_template(
                c,
                &p2.id,
                "dup",
                "other cmd",
                None,
                CommandSource::default(),
            )
            .expect("second 'dup' template in another project");
            // Pair t2 with the FIRST workspace via a raw insert (no project-match FK),
            // so the workspace now has two instances whose template name is "dup".
            let second_id = uuid_like();
            let stmt = format!(
                "INSERT INTO command_instances \
                 (id, command_id, workspace_id, last_state, scrollback, \
                  was_running_on_shutdown, created_at, updated_at) \
                 VALUES ('{second_id}', '{}', '{}', 'idle', '', 0, 0, 0)",
                t2.id, s.workspace_id
            );
            diesel::sql_query(stmt)
                .execute(c)
                .expect("raw insert of the cross-project instance");
            let instances = db::list_instances_for_workspace(c, &s.workspace_id).unwrap();
            assert_eq!(instances.len(), 2, "two instances now share the name 'dup'");
            second_id
        });

        let err = s
            .dispatcher
            .resolve_instance_id(&json!({ "name": "dup", "workspace_id": s.workspace_id }))
            .expect_err("an ambiguous name does not resolve to one");
        assert_eq!(err.code, "invalid_state", "ambiguity is invalid_state");
        assert!(
            err.message.contains(&s.instance_id) && err.message.contains(&second_instance_id),
            "the ambiguity error lists BOTH candidate instance_ids, got: {}",
            err.message
        );
    }

    /// A throwaway unique id for the raw-insert test row (avoids depending on the
    /// `uuid` crate from this module — any distinct string the FKs accept is fine).
    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("test-instance-{n}")
    }

    // --- list_commands instance-row shape (#19/#20) -----------------------

    /// `list_commands(workspace_id)` rows carry the full run-status fieldset
    /// (`state`/`running`/`finished`/`exit_code`) alongside `last_state`, matching
    /// `status_json` and the action tools — the row-shape assertion finding #20 asks
    /// for. The instance is never started, so the runner reports `idle`: the row must
    /// read `state: idle`, `running: false`, `finished: false`, `exit_code: null` (a
    /// never-run instance), distinguishable from a future crash/clean finish. This
    /// uses the REAL dispatcher over a managed `Db` + runner (the runner is needed
    /// because `list_commands` overlays its live state) and spawns NO process — so it
    /// runs even under the ConPTY gap.
    #[test]
    fn list_commands_instance_rows_carry_running_finished_exit_code() {
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app builds");
        let db = Db::in_memory();
        let (workspace_id, instance_id) = db.with_conn(|c| {
            let (_p, w) =
                db::create_project(c, "proj", "/tmp/nyx-listcmd-ws", None).expect("project");
            db::create_template(c, &w.project_id, "dev", "npm run dev", None, CommandSource::default())
                .expect("template");
            let instances = db::list_instances_for_workspace(c, &w.id).expect("instances");
            (w.id, instances[0].id.clone())
        });
        app.manage(db);
        // list_commands overlays the LIVE runner state, so a runner must be managed.
        crate::bridge::manage_command_runner(&app.handle().clone());
        let dispatcher = NyxToolDispatcher::new(app.handle().clone());

        let listed = dispatcher
            .call("list_commands", &json!({ "workspace_id": workspace_id }))
            .expect("list_commands over the real dispatcher");
        let rows = listed["commands"].as_array().expect("commands array");
        let row = rows
            .iter()
            .find(|r| r["instance_id"] == json!(instance_id))
            .expect("the seeded instance is listed");

        // Back-compat field still present.
        assert_eq!(row["last_state"], "idle", "last_state stays the live state");
        // The #19/#20 additions, consistent with status_json on an idle instance.
        assert_eq!(row["state"], "idle", "row carries the status `state`");
        assert_eq!(row["running"], false, "an unstarted instance is not running");
        assert_eq!(row["finished"], false, "a never-run instance is not finished");
        assert!(
            row["exit_code"].is_null(),
            "a never-run instance reports a null exit_code, got: {}",
            row["exit_code"]
        );
        // All four status keys are present (the splat, not just last_state).
        for k in ["state", "running", "finished", "exit_code"] {
            assert!(
                row.get(k).is_some(),
                "row is missing the `{k}` status field added by #19/#20"
            );
        }
    }

    // --- wait_for_command argument parsing (ADR-0003 D12, pure) -----------

    #[test]
    fn parse_until_defaults_to_settled_states() {
        // Absent / null / empty array all fall back to the settled set success+error
        // (the common "await completion" case).
        assert_eq!(
            parse_until(&json!({})).unwrap(),
            vec![RunState::Success, RunState::Error],
            "absent until → settled default"
        );
        assert_eq!(
            parse_until(&json!({ "until": null })).unwrap(),
            vec![RunState::Success, RunState::Error],
            "null until → settled default"
        );
        assert_eq!(
            parse_until(&json!({ "until": [] })).unwrap(),
            vec![RunState::Success, RunState::Error],
            "empty until → settled default"
        );
    }

    #[test]
    fn parse_until_accepts_runner_vocabulary_and_running() {
        // The runner vocabulary, including "running" (wait for start).
        assert_eq!(
            parse_until(&json!({ "until": ["running"] })).unwrap(),
            vec![RunState::Running]
        );
        assert_eq!(
            parse_until(&json!({ "until": ["idle"] })).unwrap(),
            vec![RunState::Idle]
        );
        assert_eq!(
            parse_until(&json!({ "until": ["success", "error"] })).unwrap(),
            vec![RunState::Success, RunState::Error]
        );
    }

    #[test]
    fn parse_until_expands_exited_alias_and_dedupes() {
        // "exited" aliases to success+error; duplicates collapse so the set is minimal.
        assert_eq!(
            parse_until(&json!({ "until": ["exited"] })).unwrap(),
            vec![RunState::Success, RunState::Error],
            "exited → success+error"
        );
        assert_eq!(
            parse_until(&json!({ "until": ["success", "exited", "error"] })).unwrap(),
            vec![RunState::Success, RunState::Error],
            "overlap with the alias de-duplicates"
        );
    }

    #[test]
    fn parse_until_rejects_unknown_and_non_string() {
        assert_eq!(
            parse_until(&json!({ "until": ["done"] })).unwrap_err().code,
            "invalid_argument",
            "an unknown state string is rejected"
        );
        assert_eq!(
            parse_until(&json!({ "until": [5] })).unwrap_err().code,
            "invalid_argument",
            "a non-string element is rejected"
        );
        assert_eq!(
            parse_until(&json!({ "until": "success" })).unwrap_err().code,
            "invalid_argument",
            "a non-array until is rejected"
        );
    }

    #[test]
    fn optional_u64_parses_and_rejects_negative() {
        let args = json!({ "timeout_ms": 5000, "neg": -1, "txt": "x" });
        assert_eq!(optional_u64(&args, "timeout_ms").unwrap(), Some(5000));
        assert_eq!(optional_u64(&args, "absent").unwrap(), None);
        assert_eq!(optional_u64(&args, "neg").unwrap_err().code, "invalid_argument");
        assert_eq!(optional_u64(&args, "txt").unwrap_err().code, "invalid_argument");
    }

    // --- wait_for_command behaviour against a live Db + runner (D12) -------
    //
    // These drive the REAL dispatcher over a mock app managing an in-memory `Db` AND
    // a real `CommandRunner`, but spawn NO process: a finished run is simulated by
    // writing the FACTUAL outcome straight to the DB (`db::set_run_state`) — exactly
    // the cold path `factual_state`/`factual_outcome` read when the in-memory runner
    // has no live entry (a run that finished before a restart). This keeps the suite
    // free of the ConPTY gap (`cargo test --lib` can't launch here — see the env
    // caveat) while exercising resolved-true / resolved-false / no-ack / cursor.

    /// Seed a dispatcher over an in-memory `Db` + a managed runner, returning the
    /// dispatcher and the seeded `instance_id`. Mirrors `seed_dispatcher` but also
    /// manages a `CommandRunner` (which `wait_for_command` reads via `runner()`).
    fn seed_wait_dispatcher() -> (NyxToolDispatcher<MockRuntime>, String, tauri::App<MockRuntime>) {
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app builds");
        let db = Db::in_memory();
        let instance_id = db.with_conn(|c| {
            let (_p, w) =
                db::create_project(c, "proj", "/tmp/nyx-wait-ws", None).expect("project");
            db::create_template(c, &w.project_id, "dev", "npm run dev", None, CommandSource::default())
                .expect("template");
            let instances = db::list_instances_for_workspace(c, &w.id).expect("instances");
            instances[0].id.clone()
        });
        app.manage(db);
        crate::bridge::manage_command_runner(&app.handle().clone());
        let dispatcher = NyxToolDispatcher::new(app.handle().clone());
        (dispatcher, instance_id, app)
    }

    #[test]
    fn wait_for_command_resolves_true_when_already_finished() {
        // resolved-true: a command that has finished (success, exit 0) within the wait
        // window resolves immediately with the settled state + exit_code + ended_at.
        let (d, instance_id, app) = seed_wait_dispatcher();
        // Simulate a finished run via the DB cold path (no live runner entry needed).
        app.state::<Db>()
            .with_conn(|c| db::set_run_state(c, &instance_id, db::STATE_SUCCESS, Some(0)))
            .expect("record a finished run");

        let r = d
            .wait_for_command(&json!({ "instance_id": instance_id, "timeout_ms": 200 }))
            .expect("wait_for_command runs");
        assert_eq!(r["resolved"], true, "an already-finished command resolves");
        assert_eq!(r["state"], "success", "reports the settled state");
        assert_eq!(r["exit_code"], 0, "a clean finish surfaces exit_code 0");
        assert!(r["ended_at"].is_i64(), "a finished run carries ended_at");
        assert!(r["cursor"].is_u64(), "cursor is an integer offset");
        // It resolved fast (well under the timeout) — no blind-poll latency.
        assert!(r["waited_ms"].as_u64().unwrap() < 200, "resolved before timeout");
    }

    #[test]
    fn wait_for_command_resolves_false_on_timeout_with_state_and_cursor() {
        // resolved-false: an instance that never leaves idle within a TINY timeout
        // returns resolved:false (a NORMAL result), the current state, and a cursor the
        // client re-polls with. The tiny timeout keeps the test fast.
        let (d, instance_id, _app) = seed_wait_dispatcher();
        let r = d
            .wait_for_command(&json!({ "instance_id": instance_id, "timeout_ms": 30 }))
            .expect("wait_for_command runs");
        assert_eq!(r["resolved"], false, "a non-settling wait times out (NOT an error)");
        assert_eq!(r["state"], "idle", "reports the current (idle) state on timeout");
        assert!(r["exit_code"].is_null(), "an unfinished run has no exit_code");
        assert!(r["ended_at"].is_null(), "an unfinished run has no ended_at");
        assert!(r["cursor"].is_u64(), "a cursor is returned for the client to re-poll");
        assert!(
            r["waited_ms"].as_u64().unwrap() >= 30,
            "the wait blocked at least the timeout, got {}",
            r["waited_ms"]
        );
    }

    #[test]
    fn wait_for_command_does_not_acknowledge_unread() {
        // no-ack: a finished, UNREAD run stays unread after a wait — waiting is purely
        // observational and must NEVER clear the unread flag (waiting ≠ acknowledging).
        let (d, instance_id, app) = seed_wait_dispatcher();
        app.state::<Db>()
            .with_conn(|c| db::set_run_state(c, &instance_id, db::STATE_ERROR, Some(2)))
            .expect("record a crashed run (unread=true)");
        // Precondition: the row is unread before the wait.
        let before = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .unwrap();
        assert!(before.unread, "a fresh finish is unread before the wait");

        let r = d
            .wait_for_command(&json!({ "instance_id": instance_id, "timeout_ms": 100 }))
            .expect("wait_for_command runs");
        assert_eq!(r["resolved"], true);
        assert_eq!(r["state"], "error");
        assert_eq!(r["exit_code"], 2, "the crash code survives (factual outcome intact)");

        // The crux: unread is STILL set — the wait did not acknowledge it.
        let after = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .unwrap();
        assert!(after.unread, "waiting must NOT clear the unread flag");
    }

    #[test]
    fn wait_for_command_first_call_returns_only_new_output_then_cursor_chains() {
        // Task #2 (D12): on the FIRST call WITHOUT `since`, output_tail returns only
        // output produced AFTER the call — NOT the pre-existing scrollback (the token
        // bomb). The cursor still points one past the end so a follow-up
        // get_command_output(since=cursor) lines up with no gap/dup.
        let (d, instance_id, app) = seed_wait_dispatcher();
        let scrollback = "line one\nline two\n";
        app.state::<Db>()
            .with_conn(|c| {
                db::set_run_state(c, &instance_id, db::STATE_SUCCESS, Some(0))?;
                db::persist_instance_scrollback(c, &instance_id, scrollback)
            })
            .expect("seed a finished run with scrollback");

        let waited = d
            .wait_for_command(&json!({ "instance_id": instance_id, "timeout_ms": 100 }))
            .expect("wait runs");
        assert_eq!(waited["resolved"], true);
        // The crux: the buffer existed BEFORE the call, so the first poll returns NOTHING
        // (default since = end-of-buffer), instead of dumping the whole scrollback.
        assert_eq!(
            waited["output_tail"], "",
            "first wait without since returns only output produced after the call"
        );
        let cursor = waited["cursor"].as_u64().expect("an integer cursor");
        assert_eq!(cursor as usize, scrollback.len(), "cursor is one past the end");

        // The returned cursor lines up with a follow-up get_command_output(since=cursor).
        let out = d
            .get_command_output(&json!({ "instance_id": instance_id, "since": cursor }))
            .expect("get_command_output runs");
        assert_eq!(out["output"], "", "nothing new since the wait's cursor (no dup)");
        assert_eq!(
            out["cursor"].as_u64().unwrap(),
            cursor,
            "the cursor round-trips: get_command_output resumes from it with no gap"
        );
    }

    #[test]
    fn wait_for_command_honors_explicit_since_and_returns_new_bytes() {
        // A resuming poll passes the prior cursor back as `since`; bytes appended AFTER
        // that offset are returned (bounded), proving the cursor chains across calls.
        let (d, instance_id, app) = seed_wait_dispatcher();
        let first = "old output\n"; // 11 bytes
        app.state::<Db>()
            .with_conn(|c| {
                db::set_run_state(c, &instance_id, db::STATE_SUCCESS, Some(0))?;
                db::persist_instance_scrollback(c, &instance_id, first)
            })
            .expect("seed initial scrollback");
        // Now MORE output lands; an agent resuming from since=first.len() sees only it.
        let grown = "old output\nNEW LINE\n";
        app.state::<Db>()
            .with_conn(|c| db::persist_instance_scrollback(c, &instance_id, grown))
            .expect("append output");
        let waited = d
            .wait_for_command(&json!({
                "instance_id": instance_id, "timeout_ms": 50, "since": first.len(),
            }))
            .expect("wait runs");
        assert_eq!(
            waited["output_tail"], "NEW LINE\n",
            "explicit since returns only the bytes after it"
        );
        assert_eq!(waited["cursor"].as_u64().unwrap() as usize, grown.len());
    }

    #[test]
    fn wait_for_command_rejects_tail_bytes_over_the_ceiling() {
        // Task #2: tail_bytes/max_bytes over MAX_TAIL_BYTES is output_too_large, the same
        // D7/D8 guard as get_command_output.
        let (d, instance_id, _app) = seed_wait_dispatcher();
        let err = d
            .wait_for_command(&json!({
                "instance_id": instance_id, "timeout_ms": 10, "tail_bytes": MAX_TAIL_BYTES + 1,
            }))
            .expect_err("an over-ceiling window is refused");
        assert_eq!(err.code, "output_too_large");
        // PRD-4.1 #3: the payload carries structured requested/limit (not only prose).
        let data = err.data.as_ref().expect("output_too_large carries structured data");
        assert_eq!(data["requested"], json!(MAX_TAIL_BYTES + 1), "requested echoes the ask");
        assert_eq!(data["limit"], json!(MAX_TAIL_BYTES), "limit is the hard ceiling");
    }

    #[test]
    fn get_command_output_over_the_ceiling_is_output_too_large_with_requested_limit() {
        // PRD-4.1 #3: the pre-existing output_too_large code on get_command_output stays, and
        // its payload now carries machine-readable requested/limit (non-regression on code/prose).
        let (d, instance_id, _app) = seed_wait_dispatcher();
        let err = d
            .get_command_output(&json!({
                "instance_id": instance_id, "max_bytes": MAX_TAIL_BYTES + 7,
            }))
            .expect_err("an over-ceiling window is refused");
        assert_eq!(err.code, "output_too_large", "the code is unchanged");
        let data = err.data.as_ref().expect("output_too_large carries structured data");
        assert_eq!(data["requested"], json!(MAX_TAIL_BYTES + 7), "requested echoes the ask");
        assert_eq!(data["limit"], json!(MAX_TAIL_BYTES), "limit is the hard ceiling");
    }

    // --- mark_read: explicit, opt-in unread consumption (PRD-4.1 task #5) ------
    //
    // A passive read (default / mark_read:false) must NEVER consume the `unread`
    // notification, so an agent polling output does not silently acknowledge a result
    // the UI has not seen. Only an explicit `mark_read:true` flips `unread=false`,
    // EXACTLY as a UI acknowledge does (via the shared `bridge::acknowledge_unread`),
    // while the factual outcome (state/exit_code) survives.

    #[test]
    fn get_command_output_passive_read_does_not_consume_unread() {
        // The default read (no mark_read) is purely observational: a finished, UNREAD run
        // stays unread after the read. This is the crux of #5 — passive polling must not
        // acknowledge a result the UI has not seen.
        let (d, instance_id, app) = seed_wait_dispatcher();
        app.state::<Db>()
            .with_conn(|c| {
                db::set_run_state(c, &instance_id, db::STATE_ERROR, Some(2))?;
                db::persist_instance_scrollback(c, &instance_id, "boom\n")
            })
            .expect("seed a crashed, unread run");
        // Precondition: the row is unread before the read.
        let before = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .unwrap();
        assert!(before.unread, "a fresh finish is unread before the read");

        let out = d
            .get_command_output(&json!({ "instance_id": instance_id }))
            .expect("default get_command_output runs");
        // The read still SEES the unread flag (the pre-mark snapshot) and the factual crash.
        assert_eq!(out["unread"], json!(true), "the read reports the still-unread result");
        assert_eq!(out["exit_code"], json!(2), "the factual crash code is reported");

        // The crux: a passive read did NOT clear `unread`.
        let after = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .unwrap();
        assert!(after.unread, "a passive (default) read must NOT consume unread");
        // Belt-and-braces: mark_read:false is the same as omitting it.
        d.get_command_output(&json!({ "instance_id": instance_id, "mark_read": false }))
            .expect("explicit mark_read:false runs");
        let after_false = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .unwrap();
        assert!(after_false.unread, "mark_read:false also leaves unread intact");
    }

    #[test]
    fn get_command_output_mark_read_true_consumes_unread_but_keeps_outcome() {
        // An EXPLICIT mark_read:true flips unread=false (an acknowledge), exactly as a UI
        // acknowledge does — while the FACTUAL outcome (state=error + exit_code=2) survives.
        let (d, instance_id, app) = seed_wait_dispatcher();
        app.state::<Db>()
            .with_conn(|c| {
                db::set_run_state(c, &instance_id, db::STATE_ERROR, Some(2))?;
                db::persist_instance_scrollback(c, &instance_id, "boom\n")
            })
            .expect("seed a crashed, unread run");
        let before = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .unwrap();
        assert!(before.unread, "unread before the consuming read");

        d.get_command_output(&json!({ "instance_id": instance_id, "mark_read": true }))
            .expect("consuming get_command_output runs");

        let after = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .unwrap();
        assert!(!after.unread, "mark_read:true consumes the unread flag");
        // The factual outcome is UNTOUCHED — an acknowledge never erases the crash.
        assert_eq!(after.last_state, db::STATE_ERROR, "the error state survives the mark");
        assert_eq!(after.last_exit_code, Some(2), "the crash exit code survives the mark");
    }

    // --- get_command_output token-safe contract end-to-end (review R-OUTPUT) ---
    //
    // These drive the REAL dispatcher over a cold (persisted-scrollback) instance — no
    // PTY spawn, so they run under the ConPTY gap — and assert the over-the-wire result
    // shape of tasks #1/#4: strip-replaces-output by default, raw when asked, byte-exact
    // cursor regardless of stripping, and the server-side grep.

    #[test]
    fn get_command_output_default_strips_ansi_and_replaces_output_no_text_field() {
        // Task #1: a DEFAULT read returns ONE cleaned `output` (no parallel `text`),
        // with the byte cursor/total computed on the RAW bytes.
        let (d, instance_id, app) = seed_wait_dispatcher();
        let raw = "\u{1b}[31mERROR:\u{1b}[0m boom\n";
        app.state::<Db>()
            .with_conn(|c| {
                db::set_run_state(c, &instance_id, db::STATE_ERROR, Some(1))?;
                db::persist_instance_scrollback(c, &instance_id, raw)
            })
            .expect("seed scrollback");
        let out = d
            .get_command_output(&json!({ "instance_id": instance_id }))
            .expect("get_command_output runs");
        assert_eq!(out["output"], "ERROR: boom\n", "default output is the cleaned text");
        assert!(out.get("text").is_none(), "no parallel `text` field (single cleaned output)");
        assert_eq!(
            out["total_bytes"].as_u64().unwrap() as usize,
            raw.len(),
            "total_bytes is the RAW byte count, byte-exact"
        );
        assert_eq!(
            out["cursor"].as_u64().unwrap() as usize,
            raw.len(),
            "cursor is the RAW byte offset even though output was stripped"
        );
    }

    #[test]
    fn get_command_output_strip_false_returns_raw_window() {
        // Task #1: strip_ansi:false returns the RAW bytes verbatim in `output`.
        let (d, instance_id, app) = seed_wait_dispatcher();
        let raw = "\u{1b}[31mERROR:\u{1b}[0m boom\n";
        app.state::<Db>()
            .with_conn(|c| {
                db::set_run_state(c, &instance_id, db::STATE_ERROR, Some(1))?;
                db::persist_instance_scrollback(c, &instance_id, raw)
            })
            .expect("seed scrollback");
        let out = d
            .get_command_output(&json!({ "instance_id": instance_id, "strip_ansi": false }))
            .expect("get_command_output runs");
        assert_eq!(out["output"], raw, "raw window verbatim when strip_ansi=false");
    }

    #[test]
    fn get_command_output_grep_returns_only_matching_lines() {
        // Task #4: a server-side regex `grep` returns only the matching lines.
        let (d, instance_id, app) = seed_wait_dispatcher();
        let raw = "starting\n\u{1b}[31mERROR: boom\u{1b}[0m\nlistening :3000\nERROR: again\n";
        app.state::<Db>()
            .with_conn(|c| {
                db::set_run_state(c, &instance_id, db::STATE_SUCCESS, Some(0))?;
                db::persist_instance_scrollback(c, &instance_id, raw)
            })
            .expect("seed scrollback");
        let out = d
            .get_command_output(&json!({ "instance_id": instance_id, "grep": "ERROR" }))
            .expect("get_command_output runs");
        assert_eq!(out["output"], "ERROR: boom\nERROR: again\n", "only the matching lines");
        // The byte cursor is unaffected by the line filter (byte-exact resume).
        assert_eq!(
            out["cursor"].as_u64().unwrap() as usize,
            raw.len(),
            "cursor stays the RAW byte offset under grep"
        );
    }

    #[test]
    fn get_command_output_invalid_grep_is_invalid_argument() {
        // Task #4: an uncompilable regex is a clean invalid_argument (D8), not internal.
        let (d, instance_id, _app) = seed_wait_dispatcher();
        let err = d
            .get_command_output(&json!({ "instance_id": instance_id, "grep": "(" }))
            .expect_err("a bad regex is rejected");
        assert_eq!(err.code, "invalid_argument", "bad grep → invalid_argument");
    }

    #[test]
    fn get_command_output_tail_lines_keeps_last_n_lines() {
        // Task #4: `tail_lines` keeps only the last N lines of the window (a line-based
        // alternative to tail_bytes), token-safe and byte-cursor-exact.
        let (d, instance_id, app) = seed_wait_dispatcher();
        let raw = "l1\nl2\nl3\nl4\nl5\n";
        app.state::<Db>()
            .with_conn(|c| {
                db::set_run_state(c, &instance_id, db::STATE_SUCCESS, Some(0))?;
                db::persist_instance_scrollback(c, &instance_id, raw)
            })
            .expect("seed scrollback");
        let out = d
            .get_command_output(&json!({ "instance_id": instance_id, "tail_lines": 2 }))
            .expect("get_command_output runs");
        assert_eq!(out["output"], "l4\nl5\n", "keeps only the last 2 lines");
        assert_eq!(
            out["cursor"].as_u64().unwrap() as usize,
            raw.len(),
            "cursor stays the RAW byte offset under tail_lines"
        );
    }

    #[test]
    fn get_command_output_byte_mode_still_works_alongside_line_modes() {
        // Task #4: byte-mode is unaffected when no line filter is given — a tail_bytes
        // window returns the last N bytes (cleaned by default) with truncated flagged.
        let (d, instance_id, app) = seed_wait_dispatcher();
        let raw = "0123456789";
        app.state::<Db>()
            .with_conn(|c| {
                db::set_run_state(c, &instance_id, db::STATE_SUCCESS, Some(0))?;
                db::persist_instance_scrollback(c, &instance_id, raw)
            })
            .expect("seed scrollback");
        let out = d
            .get_command_output(&json!({ "instance_id": instance_id, "tail_bytes": 4 }))
            .expect("get_command_output runs");
        assert_eq!(out["output"], "6789", "byte window keeps the last 4 bytes");
        assert_eq!(out["truncated"], true, "the head was dropped → truncated");
        assert_eq!(out["returned_bytes"].as_u64().unwrap(), 4);
    }

    // --- clear_command_output (review R-OUTPUT, task #5) -------------------

    #[test]
    fn clear_command_output_empties_buffer_then_get_returns_empty() {
        // Task #5: clearing an instance's buffer makes a subsequent get_command_output
        // return empty/new-only, while the factual outcome (state/exit_code) survives.
        let (d, instance_id, app) = seed_wait_dispatcher();
        let raw = "lots of stale output\n";
        app.state::<Db>()
            .with_conn(|c| {
                db::set_run_state(c, &instance_id, db::STATE_ERROR, Some(1))?;
                db::persist_instance_scrollback(c, &instance_id, raw)
            })
            .expect("seed scrollback");
        // Before: the buffer has content.
        let before = d
            .get_command_output(&json!({ "instance_id": instance_id }))
            .expect("read before clear");
        assert_eq!(before["output"], "lots of stale output\n");

        // Clear it.
        let cleared = d
            .clear_command_output(&json!({ "instance_id": instance_id }))
            .expect("clear runs");
        assert_eq!(cleared["cleared"], true);
        assert_eq!(cleared["instance_id"], json!(instance_id));

        // After: the buffer is empty, but the factual crash outcome is intact.
        let after = d
            .get_command_output(&json!({ "instance_id": instance_id }))
            .expect("read after clear");
        assert_eq!(after["output"], "", "buffer is empty after clear");
        assert_eq!(after["total_bytes"].as_u64().unwrap(), 0, "no bytes remain");
        assert_eq!(after["state"], "error", "the clear preserved the factual run state");
        assert_eq!(after["exit_code"], 1, "the clear preserved the crash exit code");
    }

    #[test]
    fn clear_command_output_rejects_template_id_with_actionable_error() {
        // Task #5 (D8): a template command_id (or unknown id) is rejected with the
        // actionable invalid_id, before any clear — same disambiguation as the other
        // instance-id tools.
        let s = seed_dispatcher("clear-c2");
        let err = s
            .dispatcher
            .call("clear_command_output", &json!({ "instance_id": s.command_id }))
            .expect_err("clear_command_output rejects a template command_id");
        assert_eq!(err.code, "invalid_id");
        assert!(err.message.contains("TEMPLATE"), "names the template path, got: {}", err.message);
    }

    // --- workspace MUTATIONS emit the structural-refresh event (review
    //     01KV9611923NKX3JPR5V6MN44F) -------------------------------------------
    //
    // The finding: an agent added a workspace over MCP but it never appeared in the
    // sidebar — the workspace/project mutations emitted NO frontend event (unlike the
    // command tools' `command://state`), and the UI only updated its OWN tree
    // optimistically after its OWN invoke. The fix routes every mutating path through
    // the shared `bridge::WORKSPACES_CHANGED_EVENT` so the sidebar (which listens on
    // it — see `useProjects`) re-pulls the tree regardless of WHO mutated. These tests
    // prove the MCP tools emit that event on a SUCCESSFUL mutation and stay silent on a
    // rejected one. (Same `tauri::test` mock-runtime seam as the rest of the suite; no
    // process spawns, so it runs under the local ConPTY gap; `--no-run` type-checks it
    // and CI runs it.)

    /// Build a mock app managing an in-memory `Db` with one seeded project, plus the
    /// REAL dispatcher over its handle and a shared counter wired to the
    /// `workspaces://changed` event — so a test can drive a mutation and assert the
    /// sidebar's refresh signal fired. Returns `(dispatcher, project_id, counter, app)`;
    /// the app is held so its managed `Db` outlives the dispatcher's handle borrow.
    fn seed_workspace_change_listener() -> (
        NyxToolDispatcher<MockRuntime>,
        String,
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tauri::App<MockRuntime>,
    ) {
        use tauri::Listener;
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app builds");
        let db = Db::in_memory();
        // A REAL temp dir as the project root, so the workspace path-validation
        // (workspace_add requires an existing dir) is satisfied without touching a
        // hard-coded /tmp path that may not exist.
        let root = std::env::temp_dir().join(format!("nyx-ws-evt-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).expect("seed project root dir");
        let root_str = root.to_str().expect("utf8 root path").to_string();
        let project_id = db
            .with_conn(|c| db::create_project(c, "proj", &root_str, None))
            .expect("seed project")
            .0
            .id;
        app.manage(db);

        // Count every `workspaces://changed` tick the dispatcher emits — the SAME
        // event name the sidebar listens on (`bridge::WORKSPACES_CHANGED_EVENT`).
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let count = std::sync::Arc::clone(&count);
            app.listen(crate::bridge::WORKSPACES_CHANGED_EVENT, move |_event| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }

        let dispatcher = NyxToolDispatcher::new(app.handle().clone());
        (dispatcher, project_id, root_str, count, app)
    }

    #[test]
    fn workspace_add_emits_the_sidebar_refresh_event() {
        // The finding's exact path: an agent calls `workspace_add` over MCP. The add
        // must succeed AND emit `workspaces://changed` so the sidebar re-pulls its tree
        // without a manual reload — the listener (`useProjects`) refetches on it.
        let (d, project_id, root, count, _app) = seed_workspace_change_listener();
        // workspace_add requires an EXISTING dir, so materialize it on disk first.
        let feat = format!("{root}/feat");
        std::fs::create_dir_all(&feat).expect("create feat dir");
        let res = d
            .call("workspace_add", &json!({ "project_id": project_id, "path": feat }))
            .expect("workspace_add over MCP succeeds");
        assert_eq!(
            res["workspace"]["name"], "feat",
            "the added workspace defaults its name to the path's basename"
        );
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "an MCP workspace_add emits exactly one workspaces://changed tick so the sidebar refreshes"
        );
    }

    #[test]
    fn create_workspace_emits_the_sidebar_refresh_event() {
        // The aliased tool shares the SAME mutating path, so it emits the SAME refresh
        // event — both MCP names converge on `bridge::emit_workspaces_changed`. The
        // folder does NOT exist yet; create_workspace mkdir -p's it (D2).
        let (d, project_id, root, count, _app) = seed_workspace_change_listener();
        let feat = format!("{root}/feat");
        d.call(
            "create_workspace",
            &json!({ "project_id": project_id, "name": "feat", "path": feat }),
        )
        .expect("create_workspace over MCP succeeds");
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "an MCP create_workspace emits the workspaces://changed refresh tick too"
        );
    }

    #[test]
    fn rejected_workspace_add_emits_no_refresh_event() {
        // A FAILED mutation (duplicate path → invalid_state) commits no row, so it must
        // emit NO refresh event — the signal fires only when the tree actually changed.
        let (d, project_id, root, count, _app) = seed_workspace_change_listener();
        let feat = format!("{root}/feat");
        std::fs::create_dir_all(&feat).expect("create feat dir");
        d.call("workspace_add", &json!({ "project_id": project_id, "path": feat.clone() }))
            .expect("first add succeeds");
        let dup = d
            .call("workspace_add", &json!({ "project_id": project_id, "path": feat }))
            .expect_err("a duplicate path is rejected");
        assert_eq!(dup.code, "invalid_state", "duplicate path → invalid_state");
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "only the SUCCESSFUL add emitted; the rejected duplicate emitted nothing"
        );
    }

    // --- workspace_add path validation (#1) + create_workspace mkdir (#2, D2) -----

    #[test]
    fn workspace_add_rejects_a_nonexistent_path() {
        // The dogfood finding: a typo'd path used to register a phantom workspace.
        // Now a non-existent path is rejected with an actionable invalid_argument and
        // NO workspace row / refresh event is produced.
        let (d, project_id, root, count, _app) = seed_workspace_change_listener();
        let missing = format!("{root}/does-not-exist-{}", uuid::Uuid::now_v7());
        let err = d
            .call("workspace_add", &json!({ "project_id": project_id, "path": missing }))
            .expect_err("a non-existent path is rejected");
        assert_eq!(err.code, "invalid_argument", "non-existent path → invalid_argument");
        assert!(
            err.message.contains("does not exist"),
            "the message is actionable, got: {}",
            err.message
        );
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a rejected workspace_add commits no row and emits no refresh"
        );
    }

    #[test]
    fn workspace_add_rejects_a_file_path() {
        // A path that exists but is a FILE (not a directory) is also rejected.
        let (d, project_id, root, _count, _app) = seed_workspace_change_listener();
        let file = format!("{root}/a-file");
        std::fs::write(&file, b"x").expect("write file");
        let err = d
            .call("workspace_add", &json!({ "project_id": project_id, "path": file }))
            .expect_err("a file path is rejected");
        assert_eq!(err.code, "invalid_argument", "a file path → invalid_argument");
        assert!(
            err.message.contains("not a directory"),
            "the message names the not-a-directory problem, got: {}",
            err.message
        );
    }

    #[test]
    fn workspace_add_succeeds_on_an_existing_dir() {
        // The happy path: an existing directory registers fine and emits the refresh.
        let (d, project_id, root, count, _app) = seed_workspace_change_listener();
        let dir = format!("{root}/existing");
        std::fs::create_dir_all(&dir).expect("create dir");
        d.call("workspace_add", &json!({ "project_id": project_id, "path": dir }))
            .expect("workspace_add on an existing dir succeeds");
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the successful add emitted exactly one refresh"
        );
    }

    #[test]
    fn create_workspace_creates_a_missing_dir_then_registers() {
        // D2: create_workspace mkdir -p's a folder that does not exist yet (including a
        // missing PARENT) and then registers it — the creating-intent tool.
        let (d, project_id, root, _count, _app) = seed_workspace_change_listener();
        let nested = format!("{root}/new-parent/new-child");
        assert!(!std::path::Path::new(&nested).exists(), "precondition: missing");
        d.call(
            "create_workspace",
            &json!({ "project_id": project_id, "name": "child", "path": nested.clone() }),
        )
        .expect("create_workspace creates the dir then registers");
        assert!(
            std::path::Path::new(&nested).is_dir(),
            "create_workspace must have created the directory tree (mkdir -p)"
        );
    }

    /// Run a git subcommand in `dir`, asserting success, with a deterministic identity
    /// so the test never depends on the host's git config. Mirrors `db::tests::git_in`.
    /// Commit signing is forced OFF: a developer host may set `commit.gpgsign=true`
    /// globally, which would make the non-interactive test commit prompt for a key
    /// passphrase and fail. The test only needs a commit to exist, not a signed one.
    fn git_in(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@nyx",
                "-c",
                "user.name=nyx-test",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn list_workspaces_resolves_the_live_branch_for_git_and_null_otherwise() {
        // The dogfood finding: branch detection works at add time but list_workspaces
        // served a STALE value (branch:null). This proves list_workspaces resolves the
        // branch LIVE at read time — and that switching the branch is reflected on the
        // next read (the stored value would have stayed wrong). Non-git folders → null.
        if !git_available() {
            eprintln!("skipping live-branch test: git not available");
            return;
        }
        let (d, project_id, root, _count, _app) = seed_workspace_change_listener();

        // A real git work tree on a known branch, registered as a workspace.
        let repo = format!("{root}/repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        let repo_path = std::path::Path::new(&repo);
        git_in(repo_path, &["init", "-q"]);
        git_in(repo_path, &["symbolic-ref", "HEAD", "refs/heads/work-a"]);
        std::fs::write(repo_path.join("README.md"), b"nyx").expect("seed file");
        git_in(repo_path, &["add", "README.md"]);
        git_in(repo_path, &["commit", "-q", "-m", "init"]);
        d.call("workspace_add", &json!({ "project_id": project_id, "path": repo.clone() }))
            .expect("add git workspace");

        // A plain (non-git) folder, registered as a second workspace.
        let plain = format!("{root}/plain");
        std::fs::create_dir_all(&plain).expect("create plain dir");
        d.call("workspace_add", &json!({ "project_id": project_id, "path": plain }))
            .expect("add plain workspace");

        let branch_of = |d: &NyxToolDispatcher<MockRuntime>, name: &str| -> Option<String> {
            let res = d
                .call("list_workspaces", &json!({ "project_id": project_id }))
                .expect("list_workspaces");
            res["workspaces"]
                .as_array()
                .expect("workspaces array")
                .iter()
                .find(|w| w["name"] == name)
                .map(|w| w["branch"].as_str().map(str::to_string))
                .expect("workspace present")
        };

        assert_eq!(
            branch_of(&d, "repo").as_deref(),
            Some("work-a"),
            "the git workspace reports its live HEAD branch"
        );
        assert_eq!(
            branch_of(&d, "plain"),
            None,
            "a non-git folder reports null branch"
        );

        // Switch the repo's branch: a STORED value would now be stale, but the read
        // resolves live, so the next list reflects the NEW branch.
        git_in(repo_path, &["checkout", "-q", "-b", "work-b"]);
        assert_eq!(
            branch_of(&d, "repo").as_deref(),
            Some("work-b"),
            "list_workspaces tracks the branch switch live (not the stale stored value)"
        );
    }

    #[test]
    fn create_workspace_on_an_existing_dir_is_idempotent() {
        // D2: create_workspace on an already-existing directory is a no-op create that
        // still registers (no error from the mkdir step).
        let (d, project_id, root, _count, _app) = seed_workspace_change_listener();
        let dir = format!("{root}/already-there");
        std::fs::create_dir_all(&dir).expect("pre-create dir");
        d.call(
            "create_workspace",
            &json!({ "project_id": project_id, "name": "there", "path": dir.clone() }),
        )
        .expect("create_workspace on an existing dir succeeds");
        assert!(std::path::Path::new(&dir).is_dir(), "the dir still exists");
    }

    // --- command CRUD tools (review 01KV9614CHC4092P05DV9R5KPG) -----------
    //
    // These drive the REAL dispatcher over a mock app managing an in-memory `Db` AND a
    // managed runner (update_command's running-guard reads the runner), and reuse the
    // EXISTING PRD-3 layer (db::create_template / pkgjson::import_command). They create
    // NO process, so they stay free of the ConPTY gap — `cargo test --lib --no-run`
    // type-checks them and CI runs them out-of-band (see the env caveat).

    /// A mock dispatcher backed by an in-memory `Db` + a managed runner, exposing the
    /// seeded `project_id` and `workspace_id` so the CRUD tools can be exercised by id.
    struct CrudSeed {
        dispatcher: NyxToolDispatcher<MockRuntime>,
        project_id: String,
        workspace_id: String,
        _app: tauri::App<MockRuntime>,
    }

    fn seed_crud_dispatcher(workspace_root: &str) -> CrudSeed {
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app builds");
        let db = Db::in_memory();
        let (project_id, workspace_id) = db.with_conn(|c| {
            let (project, workspace) =
                db::create_project(c, "proj", workspace_root, None).expect("create project");
            (project.id, workspace.id)
        });
        app.manage(db);
        crate::bridge::manage_command_runner(&app.handle().clone());
        let dispatcher = NyxToolDispatcher::new(app.handle().clone());
        CrudSeed {
            dispatcher,
            project_id,
            workspace_id,
            _app: app,
        }
    }

    #[test]
    fn add_command_creates_a_template_via_the_prd3_path() {
        // add_command delegates to db::create_template (the command_create path): the
        // returned template carries the project's command_id, and listing the project's
        // templates afterwards shows it — proving it went through the real layer, not a
        // parallel write.
        let s = seed_crud_dispatcher("/tmp/nyx-crud-add");
        let created = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "dev", "command": "vite" }),
            )
            .expect("add_command creates the template");
        let cmd = &created["command"];
        assert_eq!(cmd["name"], "dev");
        assert_eq!(cmd["command"], "vite");
        assert_eq!(cmd["project_id"], json!(s.project_id));
        let command_id = cmd["command_id"].as_str().expect("a command_id").to_string();

        // It is visible through the SAME read path the agent uses (list templates).
        let listed = s
            .dispatcher
            .call("list_commands", &json!({ "project_id": s.project_id }))
            .expect("list templates");
        assert!(
            listed["commands"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["command_id"] == json!(command_id)),
            "the created template is listed via the existing read path"
        );
    }

    #[test]
    fn add_command_infers_package_manager_provenance() {
        // A PM-invocation command line gets its provenance inferred through the SAME
        // bridge::infer_command_source path the UI's command_create uses.
        let s = seed_crud_dispatcher("/tmp/nyx-crud-infer");
        let created = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "build", "command": "pnpm build" }),
            )
            .expect("add_command");
        assert_eq!(
            created["command"]["package_manager"], "pnpm",
            "a `pnpm build` command line infers the pnpm manager (reused inference)"
        );
        assert_eq!(created["command"]["source_kind"], "package_json");
    }

    #[test]
    fn add_command_duplicate_name_is_invalid_state() {
        // The UNIQUE(project_id, name) backstop surfaces as the D8 invalid_state.
        let s = seed_crud_dispatcher("/tmp/nyx-crud-dup");
        s.dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "dev", "command": "vite" }),
            )
            .expect("first create");
        let err = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "dev", "command": "next dev" }),
            )
            .expect_err("a duplicate name is refused");
        assert_eq!(err.code, "invalid_state", "duplicate name → invalid_state");
    }

    #[test]
    fn add_command_unknown_project_is_invalid_id() {
        let s = seed_crud_dispatcher("/tmp/nyx-crud-noproj");
        let err = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": "no-such-project", "name": "dev", "command": "vite" }),
            )
            .expect_err("an unknown project is refused");
        assert_eq!(err.code, "invalid_id", "unknown project (FK) → invalid_id");
    }

    #[test]
    fn add_command_missing_required_args_is_invalid_argument() {
        let s = seed_crud_dispatcher("/tmp/nyx-crud-req");
        let err = s
            .dispatcher
            .call("add_command", &json!({ "project_id": s.project_id, "name": "dev" }))
            .expect_err("a missing command is rejected");
        assert_eq!(err.code, "invalid_argument", "missing `command` → invalid_argument");
    }

    #[test]
    fn update_command_modifies_an_existing_template() {
        // update_command delegates to db::update_template (the command_update path),
        // applying ONLY the supplied fields — omitted fields keep their current value.
        let s = seed_crud_dispatcher("/tmp/nyx-crud-update");
        let created = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "dev", "command": "vite" }),
            )
            .expect("create");
        let command_id = created["command"]["command_id"].as_str().unwrap().to_string();

        // Update only the command; the name must be preserved (partial update).
        let updated = s
            .dispatcher
            .call(
                "update_command",
                &json!({ "command_id": command_id, "command": "vite --host" }),
            )
            .expect("update_command modifies the template");
        assert_eq!(updated["command"]["command"], "vite --host", "command changed");
        assert_eq!(
            updated["command"]["name"], "dev",
            "an omitted field keeps its current value (partial update)"
        );
    }

    #[test]
    fn update_command_subfolder_tristate_clears_with_empty_string() {
        // Tri-state subfolder: a present "" clears it; an omitted subfolder keeps it.
        let s = seed_crud_dispatcher("/tmp/nyx-crud-sub");
        let created = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "api", "command": "node x", "subfolder": "packages/api" }),
            )
            .expect("create with subfolder");
        let command_id = created["command"]["command_id"].as_str().unwrap().to_string();
        assert_eq!(created["command"]["subfolder"], "packages/api");

        // Omitting subfolder keeps it.
        let kept = s
            .dispatcher
            .call("update_command", &json!({ "command_id": command_id, "name": "api2" }))
            .expect("update without subfolder");
        assert_eq!(kept["command"]["subfolder"], "packages/api", "omitted → kept");

        // An explicit empty string clears it to the workspace root.
        let cleared = s
            .dispatcher
            .call("update_command", &json!({ "command_id": command_id, "subfolder": "" }))
            .expect("update clearing subfolder");
        assert!(
            cleared["command"]["subfolder"].is_null(),
            "an empty-string subfolder clears it, got: {}",
            cleared["command"]["subfolder"]
        );
    }

    // --- A1: update_command source-detach rule (3 cases) ---------------------
    //
    // The detach rule (bridge::command_detaches_source): edit the command away from
    // BOTH the canonical runner call AND the raw script body → detach (null source).
    // Case 1: update to the canonical runner call → keep source.
    // Case 2: update to the raw script body → keep source.
    // Case 3: update to something completely different → detach source.

    fn seed_sourced_template_via_import(tag: &str) -> (CrudSeed, String) {
        // Create a temp workspace with a package.json, then import it to get a
        // package.json-sourced template for the detachment tests.
        let tmp = std::env::temp_dir().join(format!("nyx-detach-{tag}"));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        // Use a script body that is NOT a runner invocation, so the canonical runner
        // call ("npm run build") and the script body ("tsc --out dist") are distinct.
        std::fs::write(
            tmp.join("package.json"),
            r#"{ "scripts": { "build": "tsc --out dist" } }"#,
        )
        .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);
        let imported = s
            .dispatcher
            .call("import_commands", &json!({ "project_id": s.project_id }))
            .expect("import runs");
        let command_id = imported["imported"][0]["command_id"]
            .as_str()
            .expect("imported a template")
            .to_string();
        (s, command_id)
    }

    #[test]
    fn update_command_detaches_source_when_command_drifts_from_both_canonical_and_snapshot() {
        // A1 case 3: a command completely unrelated to the canonical runner call and
        // the raw script body → source_kind and package_manager are cleared to null.
        let (s, command_id) = seed_sourced_template_via_import("drift");
        // Confirm the template was imported with a source.
        let listed = s
            .dispatcher
            .call("list_commands", &json!({ "project_id": s.project_id }))
            .expect("list templates");
        let before = &listed["commands"][0];
        assert_eq!(before["source_kind"], "package_json", "imported with provenance");

        // Update to something totally unrelated.
        let updated = s
            .dispatcher
            .call("update_command", &json!({ "command_id": command_id, "command": "echo detach-test" }))
            .expect("update to unrelated command");
        assert!(
            updated["command"]["source_kind"].is_null(),
            "source_kind must be null after a drift update, got: {}",
            updated["command"]["source_kind"]
        );
        assert!(
            updated["command"]["package_manager"].is_null(),
            "package_manager must be null after a drift update, got: {}",
            updated["command"]["package_manager"]
        );
    }

    #[test]
    fn update_command_keeps_source_when_command_equals_canonical_runner_call() {
        // A1 case 1: the canonical runner call (e.g. "npm run build") keeps the source.
        let (s, command_id) = seed_sourced_template_via_import("canonical");
        // Update to the exact canonical npm runner call.
        let updated = s
            .dispatcher
            .call(
                "update_command",
                &json!({ "command_id": command_id, "command": "npm run build" }),
            )
            .expect("update to canonical runner call");
        assert_eq!(
            updated["command"]["source_kind"], "package_json",
            "canonical runner call keeps the source link, got: {}",
            updated["command"]["source_kind"]
        );
    }

    #[test]
    fn update_command_keeps_source_when_command_equals_raw_script_body() {
        // A1 case 2: the raw script body from the package.json snapshot keeps the source.
        let (s, command_id) = seed_sourced_template_via_import("snapshot");
        // Update to the exact raw script body (as it was when imported).
        let updated = s
            .dispatcher
            .call(
                "update_command",
                &json!({ "command_id": command_id, "command": "tsc --out dist" }),
            )
            .expect("update to raw script body");
        assert_eq!(
            updated["command"]["source_kind"], "package_json",
            "raw script body keeps the source link, got: {}",
            updated["command"]["source_kind"]
        );
    }

    #[test]
    fn update_command_unknown_id_is_invalid_id() {
        // An unknown command_id → invalid_id; passing an instance_id gets the
        // actionable inverse error (names the template path).
        let s = seed_crud_dispatcher("/tmp/nyx-crud-badid");
        let err = s
            .dispatcher
            .call("update_command", &json!({ "command_id": "no-such-template", "name": "x" }))
            .expect_err("unknown template id");
        assert_eq!(err.code, "invalid_id");

        // Seed a template → its instance; passing the INSTANCE id to update_command is
        // the inverse confusion and gets the actionable message.
        let created = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "dev", "command": "vite" }),
            )
            .expect("create");
        let _command_id = created["command"]["command_id"].as_str().unwrap().to_string();
        let instance_id = s
            .dispatcher
            .db()
            .unwrap()
            .with_conn(|c| db::list_instances_for_workspace(c, &s.workspace_id))
            .unwrap()[0]
            .id
            .clone();
        let err = s
            .dispatcher
            .call("update_command", &json!({ "command_id": instance_id, "name": "x" }))
            .expect_err("an instance_id is not a template");
        assert_eq!(err.code, "invalid_id");
        assert!(
            err.message.contains("INSTANCE"),
            "the inverse error names the instance/template distinction, got: {}",
            err.message
        );
    }

    #[test]
    fn import_commands_imports_package_json_scripts_via_existing_logic() {
        // import_commands reuses pkgjson::discover_package_scripts + import_command (the
        // command_import_scripts/command_import_create path). Seed a REAL package.json
        // in a temp workspace, point a project's workspace at it, and import: the
        // scripts become templates, and a re-run skips the now-existing names.
        let tmp = std::env::temp_dir().join(format!("nyx-import-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(
            tmp.join("package.json"),
            r#"{ "scripts": { "dev": "vite", "build": "tsc" } }"#,
        )
        .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        let result = s
            .dispatcher
            .call("import_commands", &json!({ "project_id": s.project_id }))
            .expect("import_commands runs");
        let imported = result["imported"].as_array().expect("imported array");
        let names: Vec<&str> = imported
            .iter()
            .filter_map(|c| c["name"].as_str())
            .collect();
        assert!(names.contains(&"dev"), "the `dev` script was imported, got {names:?}");
        assert!(names.contains(&"build"), "the `build` script was imported");
        // Provenance was linked through the reused import path.
        assert_eq!(imported[0]["source_kind"], "package_json");

        // Idempotent re-run: the existing names are SKIPPED (reported), not errored.
        let again = s
            .dispatcher
            .call("import_commands", &json!({ "project_id": s.project_id }))
            .expect("second import runs");
        assert!(
            again["imported"].as_array().unwrap().is_empty(),
            "a re-run imports nothing new"
        );
        assert_eq!(
            again["skipped"].as_array().unwrap().len(),
            2,
            "both existing scripts are skipped on the re-run"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn import_commands_workspace_id_form_scans_one_workspace() {
        // The workspace_id form resolves the project from the row and scans that one
        // workspace (db::get_workspace + the SAME discovery/import path).
        let tmp = std::env::temp_dir().join(format!("nyx-import-ws-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(tmp.join("package.json"), r#"{ "scripts": { "start": "node ." } }"#)
            .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        let result = s
            .dispatcher
            .call("import_commands", &json!({ "workspace_id": s.workspace_id }))
            .expect("import by workspace_id");
        let names: Vec<&str> = result["imported"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["name"].as_str())
            .collect();
        assert!(names.contains(&"start"), "imported the workspace's script, got {names:?}");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn import_commands_requires_a_target() {
        let s = seed_crud_dispatcher("/tmp/nyx-import-none");
        let err = s
            .dispatcher
            .call("import_commands", &json!({}))
            .expect_err("neither project_id nor workspace_id");
        assert_eq!(err.code, "invalid_argument");
    }

    #[test]
    fn import_commands_unknown_workspace_is_invalid_id() {
        let s = seed_crud_dispatcher("/tmp/nyx-import-badws");
        let err = s
            .dispatcher
            .call("import_commands", &json!({ "workspace_id": "no-such-ws" }))
            .expect_err("unknown workspace");
        assert_eq!(err.code, "invalid_id");
    }

    // --- A2: remove_workspace + remove_command tests -------------------------

    #[test]
    fn remove_command_deletes_template_and_emits_event() {
        let s = seed_crud_dispatcher("/tmp/nyx-rm-cmd");
        let created = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "dev", "command": "vite" }),
            )
            .expect("create template");
        let command_id = created["command"]["command_id"].as_str().unwrap().to_string();

        // remove_command succeeds on an idle template + returns the explicit ack
        // (R-WSCMD #4): removed:true + the count of instances that cascade-deleted (one
        // per workspace; this project's single root workspace → 1).
        let ack = s
            .dispatcher
            .call("remove_command", &json!({ "command_id": command_id }))
            .expect("remove_command removes an idle template");
        assert_eq!(ack["removed"], true, "remove_command acks removed:true");
        assert_eq!(
            ack["removed_instances"], 1,
            "one instance (the root workspace's) cascade-deleted with the template"
        );

        // Template is gone from the listing.
        let listed = s
            .dispatcher
            .call("list_commands", &json!({ "project_id": s.project_id }))
            .expect("list after remove");
        assert!(
            listed["commands"].as_array().unwrap().is_empty(),
            "template is removed from the listing"
        );
    }

    #[test]
    fn remove_command_rejects_unknown_id_with_invalid_id() {
        let s = seed_crud_dispatcher("/tmp/nyx-rm-cmd-bad");
        let err = s
            .dispatcher
            .call("remove_command", &json!({ "command_id": "no-such-template" }))
            .expect_err("unknown template");
        assert_eq!(err.code, "invalid_id");
    }

    #[test]
    fn remove_command_rejects_instance_id_with_actionable_error() {
        // Passing an instance_id to remove_command returns the actionable template-vs-
        // instance disambiguation error (D8), not a generic unknown.
        let s = seed_crud_dispatcher("/tmp/nyx-rm-cmd-inst");
        let created = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "dev", "command": "vite" }),
            )
            .expect("create template");
        let _command_id = created["command"]["command_id"].as_str().unwrap().to_string();
        // Get the instance_id.
        let instance_id = s
            .dispatcher
            .db()
            .unwrap()
            .with_conn(|c| db::list_instances_for_workspace(c, &s.workspace_id))
            .unwrap()[0]
            .id
            .clone();
        let err = s
            .dispatcher
            .call("remove_command", &json!({ "command_id": instance_id }))
            .expect_err("instance_id passed to remove_command");
        assert_eq!(err.code, "invalid_id");
        assert!(
            err.message.contains("INSTANCE"),
            "the error must name the instance/template distinction, got: {}",
            err.message
        );
    }

    #[test]
    fn remove_workspace_deletes_workspace_and_emits_event() {
        // A real temp project root so workspace_add's existing-dir validation passes.
        let root = std::env::temp_dir().join(format!("nyx-rm-ws-{}", uuid_like()));
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).expect("create sub dir");
        let s = seed_crud_dispatcher(root.to_str().expect("utf8 root"));
        // Add a second workspace at the existing sub-folder, then a command so an
        // instance materializes into it (proving removed_instances counts cascades).
        let ws = s
            .dispatcher
            .call(
                "workspace_add",
                &json!({ "project_id": s.project_id, "path": sub.to_str().unwrap() }),
            )
            .expect("add workspace");
        let ws_id = ws["workspace"]["id"].as_str().unwrap().to_string();
        s.dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "dev", "command": "vite" }),
            )
            .expect("add command materializes an instance per workspace");

        // remove_workspace removes it + acks removed:true with the cascade-deleted
        // instance count (the one instance materialized into this workspace).
        let ack = s
            .dispatcher
            .call("remove_workspace", &json!({ "workspace_id": ws_id }))
            .expect("remove_workspace removes an idle workspace");
        assert_eq!(ack["removed"], true, "remove_workspace acks removed:true");
        assert_eq!(
            ack["removed_instances"], 1,
            "the workspace's one command instance cascade-deleted with it"
        );
    }

    #[test]
    fn remove_command_running_guard_message_says_removing_not_editing() {
        // R-WSCMD #6: the running-guard message on remove_command must reference
        // REMOVING, not EDITING (it was copy-pasted from update_command's edit guard).
        // We start the materialized instance so the guard trips, then assert the wording.
        let s = seed_crud_dispatcher("/tmp/nyx-rm-running");
        let created = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "dev", "command": "sleep 30" }),
            )
            .expect("create template");
        let command_id = created["command"]["command_id"].as_str().unwrap().to_string();
        // Start the materialized instance so the template has a running instance.
        let instance_id = s
            .dispatcher
            .db()
            .unwrap()
            .with_conn(|c| db::list_instances_for_workspace(c, &s.workspace_id))
            .unwrap()[0]
            .id
            .clone();
        s.dispatcher
            .call("start_command", &json!({ "instance_id": instance_id }))
            .expect("start the instance");

        let err = s
            .dispatcher
            .call("remove_command", &json!({ "command_id": command_id }))
            .expect_err("remove_command refused while an instance runs");
        assert_eq!(err.code, "invalid_state");
        assert!(
            err.message.contains("removing"),
            "the guard message must say 'removing', got: {}",
            err.message
        );
        assert!(
            !err.message.contains("editing"),
            "the guard message must NOT say 'editing' (copy-paste bug), got: {}",
            err.message
        );
        // Cleanup: stop the running instance.
        s.dispatcher
            .call("stop_command", &json!({ "instance_id": instance_id }))
            .expect("cleanup stop");
    }

    #[test]
    fn remove_workspace_rejects_unknown_id_with_invalid_id() {
        let s = seed_crud_dispatcher("/tmp/nyx-rm-ws-bad");
        let err = s
            .dispatcher
            .call("remove_workspace", &json!({ "workspace_id": "no-such-ws" }))
            .expect_err("unknown workspace");
        assert_eq!(err.code, "invalid_id");
    }

    // --- R-WSCMD #4/#5/#7: lifecycle acks + double-start + per-run env --------
    //
    // These drive the REAL dispatcher over a mock app with a managed runner + Db,
    // spawning real (short-lived) processes — the SAME mock-runtime seam as the other
    // command tests. They prove the explicit mutation acks (was_running/restarted/
    // changed), the no-double-spawn semantics, and that a per-run `env` reaches the
    // spawned process.

    /// Seed a CRUD dispatcher whose project root is a REAL temp dir (so a command's run
    /// cwd resolves) with one materialized instance for `command`. Returns the seed +
    /// the instance_id.
    fn seed_runnable_instance(tag: &str, command: &str) -> (CrudSeed, String) {
        let root = std::env::temp_dir().join(format!("nyx-life-{tag}-{}", uuid_like()));
        std::fs::create_dir_all(&root).expect("create project root");
        let s = seed_crud_dispatcher(root.to_str().expect("utf8 root"));
        s.dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "svc", "command": command }),
            )
            .expect("add command materializes an instance");
        let instance_id = s
            .dispatcher
            .db()
            .unwrap()
            .with_conn(|c| db::list_instances_for_workspace(c, &s.workspace_id))
            .unwrap()[0]
            .id
            .clone();
        (s, instance_id)
    }

    #[test]
    fn start_command_ack_reports_was_running_and_restarted() {
        // A fresh start: running:true, was_running:false, restarted:false.
        let (s, instance_id) = seed_runnable_instance("start-ack", "sleep 30");
        let ack = s
            .dispatcher
            .call("start_command", &json!({ "instance_id": instance_id }))
            .expect("start_command");
        assert_eq!(ack["running"], true, "the instance is running after start");
        assert_eq!(ack["was_running"], false, "it was not already running");
        assert_eq!(ack["restarted"], false, "a fresh start is not a restart");
        s.dispatcher
            .call("stop_command", &json!({ "instance_id": instance_id }))
            .expect("cleanup stop");
    }

    #[test]
    fn double_start_command_is_a_noop_was_running_true_no_second_process() {
        // R-WSCMD #5: a second start on a running instance is a NO-OP — was_running:true,
        // restarted:false, and it does NOT spawn a second process. We assert the ack
        // shape (the no-second-process guarantee is proven at the runner level).
        let (s, instance_id) = seed_runnable_instance("double-start", "sleep 30");
        s.dispatcher
            .call("start_command", &json!({ "instance_id": instance_id }))
            .expect("first start");
        let second = s
            .dispatcher
            .call("start_command", &json!({ "instance_id": instance_id }))
            .expect("second start is a no-op, not an error");
        assert_eq!(second["running"], true, "still running after the no-op");
        assert_eq!(
            second["was_running"], true,
            "the second start saw an already-running instance (no-op)"
        );
        assert_eq!(
            second["restarted"], false,
            "a second start NEVER restarts — relaunch is the explicit restart"
        );
        s.dispatcher
            .call("stop_command", &json!({ "instance_id": instance_id }))
            .expect("cleanup stop");
    }

    #[test]
    fn stop_command_ack_reports_changed_and_was_running() {
        // A stop on a RUNNING instance: changed:true, was_running:true. A stop on an
        // already-idle instance: changed:false, was_running:false (a clear no-op).
        let (s, instance_id) = seed_runnable_instance("stop-ack", "sleep 30");
        s.dispatcher
            .call("start_command", &json!({ "instance_id": instance_id }))
            .expect("start");
        let stopped = s
            .dispatcher
            .call("stop_command", &json!({ "instance_id": instance_id }))
            .expect("stop");
        assert_eq!(stopped["changed"], true, "stopping a live process changed something");
        assert_eq!(stopped["was_running"], true, "it was running before the stop");

        // A second stop is a no-op.
        let again = s
            .dispatcher
            .call("stop_command", &json!({ "instance_id": instance_id }))
            .expect("idempotent second stop");
        assert_eq!(again["changed"], false, "a stop on an idle instance changes nothing");
        assert_eq!(again["was_running"], false, "it was not running");
    }

    #[test]
    fn relaunch_command_ack_reports_restarted_true() {
        // R-WSCMD #5: relaunch ALWAYS restarts (restarted:true); was_running reports
        // whether a live process was stopped first.
        let (s, instance_id) = seed_runnable_instance("relaunch-ack", "sleep 30");
        s.dispatcher
            .call("start_command", &json!({ "instance_id": instance_id }))
            .expect("start");
        let ack = s
            .dispatcher
            .call("relaunch_command", &json!({ "instance_id": instance_id }))
            .expect("relaunch");
        assert_eq!(ack["restarted"], true, "relaunch always restarts");
        assert_eq!(ack["was_running"], true, "it was running, so relaunch stopped it first");
        s.dispatcher
            .call("stop_command", &json!({ "instance_id": instance_id }))
            .expect("cleanup stop");
    }

    #[test]
    fn start_command_passes_env_to_the_spawned_process() {
        // R-WSCMD #7: an `env` map on start_command reaches the spawned process, proven
        // by a command that echoes the var into output read back via get_command_output.
        // The command exits quickly so the output settles.
        let (s, instance_id) =
            seed_runnable_instance("start-env", "printf 'GOTVAR=%s\\n' \"$NYX_MCP_ENV_TEST\"; true");
        s.dispatcher
            .call(
                "start_command",
                &json!({
                    "instance_id": instance_id,
                    "env": { "NYX_MCP_ENV_TEST": "from-mcp" }
                }),
            )
            .expect("start_command with env");
        // Poll get_command_output until the var shows up (or a short deadline).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut found = false;
        while std::time::Instant::now() < deadline && !found {
            std::thread::sleep(Duration::from_millis(50));
            let out = s
                .dispatcher
                .call("get_command_output", &json!({ "instance_id": instance_id }))
                .expect("get_command_output");
            if out["output"].as_str().unwrap_or("").contains("GOTVAR=from-mcp") {
                found = true;
            }
        }
        assert!(found, "the per-run env var must reach the spawned process");
    }

    #[test]
    fn start_command_rejects_a_non_string_env_value_without_leaking_it() {
        // The env value must be a string; a non-string is invalid_argument, and the
        // error names only the KEY + type, never the value (secret-safety).
        let (s, instance_id) = seed_runnable_instance("env-bad", "true");
        let err = s
            .dispatcher
            .call(
                "start_command",
                &json!({ "instance_id": instance_id, "env": { "SECRET": 12345 } }),
            )
            .expect_err("a non-string env value is rejected");
        assert_eq!(err.code, "invalid_argument");
        assert!(err.message.contains("SECRET"), "names the offending key, got: {}", err.message);
        assert!(
            !err.message.contains("12345"),
            "the error must NOT echo the value (secret-safety), got: {}",
            err.message
        );
    }

    #[test]
    fn start_command_rejects_a_non_object_env() {
        let (s, instance_id) = seed_runnable_instance("env-shape", "true");
        let err = s
            .dispatcher
            .call(
                "start_command",
                &json!({ "instance_id": instance_id, "env": "KEY=VALUE" }),
            )
            .expect_err("a non-object env is rejected");
        assert_eq!(err.code, "invalid_argument");
        assert!(err.message.contains("object"), "got: {}", err.message);
    }

    // --- B1: selective import with `names` filter ----------------------------

    #[test]
    fn import_commands_names_filter_imports_only_selected_scripts() {
        // B1 done-criterion: a partial import with names:["dev"] imports only "dev"
        // and silently bypasses "build" (not in skipped, not an error).
        let tmp = std::env::temp_dir().join(format!("nyx-import-sel-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(
            tmp.join("package.json"),
            r#"{ "scripts": { "dev": "vite", "build": "tsc", "preview": "vite preview" } }"#,
        )
        .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        // Import only "dev".
        let result = s
            .dispatcher
            .call(
                "import_commands",
                &json!({ "project_id": s.project_id, "names": ["dev"] }),
            )
            .expect("selective import runs");

        let imported = result["imported"].as_array().expect("imported array");
        let names: Vec<&str> = imported.iter().filter_map(|c| c["name"].as_str()).collect();
        assert_eq!(names, vec!["dev"], "only 'dev' was imported, got {names:?}");

        // "build" and "preview" are not in skipped — they were simply not requested.
        let skipped = result["skipped"].as_array().expect("skipped array");
        assert!(
            skipped.is_empty(),
            "scripts bypassed by the filter are NOT in skipped, got {skipped:?}"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn import_commands_no_names_filter_imports_all_backward_compat() {
        // B1 backward-compat: absent `names` → every script is a candidate (same as
        // before the filter was added).
        let tmp = std::env::temp_dir().join(format!("nyx-import-nosel-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(
            tmp.join("package.json"),
            r#"{ "scripts": { "dev": "vite", "build": "tsc" } }"#,
        )
        .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        let result = s
            .dispatcher
            .call("import_commands", &json!({ "project_id": s.project_id }))
            .expect("import without filter runs");

        let imported = result["imported"].as_array().expect("imported array");
        let names: Vec<&str> = imported.iter().filter_map(|c| c["name"].as_str()).collect();
        assert!(names.contains(&"dev") && names.contains(&"build"),
            "all scripts imported when no filter, got {names:?}");

        std::fs::remove_dir_all(&tmp).ok();
    }

    // --- R-IMPORT #2: names matches raw script_name + skipped{not_found} -----

    #[test]
    fn import_commands_names_matches_raw_script_name_across_packages() {
        // R-IMPORT #2 done-criterion: names:["build"] matches a `build` script in EVERY
        // package even when the proposed name is prefixed (pkg:build) by a collision.
        let tmp = std::env::temp_dir().join(format!("nyx-import-raw-{}", uuid_like()));
        std::fs::create_dir_all(tmp.join("packages/api")).expect("mkdir api");
        std::fs::create_dir_all(tmp.join("packages/web")).expect("mkdir web");
        // Two packages BOTH with a `build` script → proposed names collide to api:build /
        // web:build. names:["build"] must still match both via the raw script_name.
        std::fs::write(
            tmp.join("packages/api/package.json"),
            r#"{ "name": "api", "scripts": { "build": "tsc", "dev": "node ." } }"#,
        )
        .expect("write api pkg");
        std::fs::write(
            tmp.join("packages/web/package.json"),
            r#"{ "name": "web", "scripts": { "build": "next build", "serve": "next" } }"#,
        )
        .expect("write web pkg");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        let result = s
            .dispatcher
            .call(
                "import_commands",
                &json!({ "project_id": s.project_id, "names": ["build"] }),
            )
            .expect("import by raw script_name");
        let imported = result["imported"].as_array().expect("imported array");
        // Both packages' build scripts imported (proposed names api:build / web:build),
        // matched via the raw script_name `build`.
        let script_names: Vec<&str> = imported
            .iter()
            .filter_map(|c| c["source_kind"].as_str().map(|_| c))
            .filter_map(|c| c["name"].as_str())
            .collect();
        assert_eq!(
            imported.len(),
            2,
            "names:[build] matched the build script in BOTH packages, got {script_names:?}"
        );
        assert!(
            script_names.iter().all(|n| n.ends_with(":build")),
            "the colliding builds are imported under their prefixed proposed names, got {script_names:?}"
        );
        // The non-build scripts (dev, serve) were not requested → not imported, not skipped.
        assert!(
            result["skipped"].as_array().unwrap().is_empty(),
            "unrequested scripts are not in skipped, got {:?}",
            result["skipped"]
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn import_commands_unknown_requested_name_is_skipped_not_found() {
        // R-IMPORT #2 done-criterion: an unknown requested name appears in
        // skipped{reason:"not_found"} (not silently swallowed).
        let tmp = std::env::temp_dir().join(format!("nyx-import-nf-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(
            tmp.join("package.json"),
            r#"{ "scripts": { "dev": "vite" } }"#,
        )
        .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        let result = s
            .dispatcher
            .call(
                "import_commands",
                &json!({ "project_id": s.project_id, "names": ["dev", "does-not-exist"] }),
            )
            .expect("import with one unknown name");
        // "dev" imported.
        let names: Vec<&str> = result["imported"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["name"].as_str())
            .collect();
        assert_eq!(names, vec!["dev"], "the discovered name imported, got {names:?}");
        // "does-not-exist" reported as not_found.
        let skipped = result["skipped"].as_array().expect("skipped array");
        let nf: Vec<&Value> = skipped
            .iter()
            .filter(|e| e["reason"] == "not_found")
            .collect();
        assert_eq!(nf.len(), 1, "exactly one not_found, got {skipped:?}");
        assert_eq!(nf[0]["name"], "does-not-exist");

        std::fs::remove_dir_all(&tmp).ok();
    }

    // --- R-IMPORT #3: explicit no-manifest signal ----------------------------

    #[test]
    fn import_commands_reports_no_manifest_when_none_found() {
        // R-IMPORT #3 done-criterion: importing where NO package.json exists returns an
        // explicit signal (manifests_found:0 + a skipped entry reason:"no_manifest"),
        // distinct from {imported:[],skipped:[]}.
        let tmp = std::env::temp_dir().join(format!("nyx-import-nomanifest-{}", uuid_like()));
        std::fs::create_dir_all(tmp.join("src")).expect("temp workspace dir, no manifest");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        let result = s
            .dispatcher
            .call("import_commands", &json!({ "project_id": s.project_id }))
            .expect("import on a manifest-less workspace");
        assert_eq!(result["manifests_found"], 0, "explicit manifests_found:0");
        assert!(result["imported"].as_array().unwrap().is_empty());
        let skipped = result["skipped"].as_array().expect("skipped array");
        assert!(
            skipped.iter().any(|e| e["reason"] == "no_manifest"),
            "a no_manifest skip entry is present, got {skipped:?}"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn import_commands_no_manifest_distinguishable_from_all_imported() {
        // The two "imported nothing" cases now differ: no_manifest carries the explicit
        // reason; an all-already-imported re-run reports manifests_found>0 and NO
        // no_manifest entry (its skips are already_exists).
        let tmp = std::env::temp_dir().join(format!("nyx-import-distinct-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(tmp.join("package.json"), r#"{ "scripts": { "dev": "vite" } }"#)
            .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        // First import creates `dev`.
        s.dispatcher
            .call("import_commands", &json!({ "project_id": s.project_id }))
            .expect("first import");
        // Re-run: dev already exists → skipped already_exists, manifest still found.
        let again = s
            .dispatcher
            .call("import_commands", &json!({ "project_id": s.project_id }))
            .expect("re-run import");
        assert_eq!(again["manifests_found"], 1, "the manifest is still found on re-run");
        let skipped = again["skipped"].as_array().unwrap();
        assert!(
            skipped.iter().all(|e| e["reason"] != "no_manifest"),
            "an all-already-imported re-run is NOT reported as no_manifest, got {skipped:?}"
        );
        assert!(
            skipped.iter().any(|e| e["reason"] == "already_exists"),
            "the re-run skip carries reason:already_exists, got {skipped:?}"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    // --- R-IMPORT #4: dry-run / preview --------------------------------------

    #[test]
    fn import_commands_preview_lists_without_creating() {
        // R-IMPORT #4 done-criterion: a preview call lists importable scripts WITHOUT
        // creating any template (asserted by listing the project's templates after).
        let tmp = std::env::temp_dir().join(format!("nyx-import-preview-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(
            tmp.join("package.json"),
            r#"{ "scripts": { "dev": "vite --host", "build": "tsc" } }"#,
        )
        .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        let result = s
            .dispatcher
            .call(
                "import_commands",
                &json!({ "project_id": s.project_id, "preview": true }),
            )
            .expect("preview import runs");
        assert_eq!(result["preview"], true, "the result echoes preview:true");
        // PRD-4.1 task #6: a preview reports its rows under `would_import` and leaves
        // `imported` an EMPTY array (it created nothing) — both keys are always present so
        // a consumer reading `imported` never gets null, while `would_import` carries the rows.
        assert_eq!(
            result["imported"].as_array().map(|a| a.len()),
            Some(0),
            "a preview's `imported` is present but empty (it imported nothing), got {:?}",
            result.get("imported")
        );
        let listed = result["would_import"].as_array().expect("would_import (preview) array");
        // Preview rows carry name/package/script_name/body/command — NOT a command_id.
        let dev = listed
            .iter()
            .find(|c| c["name"] == "dev")
            .expect("dev in preview");
        assert_eq!(dev["script_name"], "dev");
        assert_eq!(dev["body"], "vite --host", "the raw script body is surfaced");
        assert_eq!(dev["command"], "npm run dev", "the runner command is surfaced");
        assert!(dev["command_id"].is_null(), "no template id in preview (nothing created)");

        // NO template was created in the project.
        let templates = s
            .dispatcher
            .call("list_commands", &json!({ "project_id": s.project_id }))
            .expect("list templates after preview");
        assert!(
            templates["commands"].as_array().unwrap().is_empty(),
            "preview created NO template, got {:?}",
            templates["commands"]
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn import_commands_real_import_reports_creations_under_imported_not_would_import() {
        // PRD-4.1 task #6 (the real-import half): a REAL import (no preview) reports its
        // actual creations under `imported` and leaves `would_import` an EMPTY array — the
        // inverse of the preview. Both keys are always present (the unused one is `[]`), so
        // `preview` plus the populated field disambiguate a dry-run from a real run.
        let tmp = std::env::temp_dir().join(format!("nyx-import-real-field-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(tmp.join("package.json"), r#"{ "scripts": { "dev": "vite" } }"#)
            .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        let result = s
            .dispatcher
            .call("import_commands", &json!({ "project_id": s.project_id }))
            .expect("real import runs");
        assert_eq!(result["preview"], false, "a real import echoes preview:false");
        // A real import carries `imported` (with the created template) and an EMPTY `would_import`.
        assert_eq!(
            result["would_import"].as_array().map(|a| a.len()),
            Some(0),
            "a real import's `would_import` is present but empty, got {:?}",
            result.get("would_import")
        );
        let imported = result["imported"].as_array().expect("imported array on a real import");
        let dev = imported
            .iter()
            .find(|c| c["name"] == "dev")
            .expect("dev imported");
        assert!(
            dev["command_id"].is_string(),
            "a real import row carries the created template's command_id"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn import_commands_preview_emits_no_refresh_event() {
        // A preview must not emit commands://changed (it changes no row).
        let tmp = std::env::temp_dir().join(format!("nyx-import-preview-noev-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(tmp.join("package.json"), r#"{ "scripts": { "dev": "vite" } }"#)
            .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let (d, project_id, _ws, count, _app) = seed_command_change_listener(&root);
        d.call(
            "import_commands",
            &json!({ "project_id": project_id, "preview": true }),
        )
        .expect("preview");
        assert_eq!(ticks(&count), 0, "preview emits no commands://changed");
        std::fs::remove_dir_all(&tmp).ok();
    }

    // --- R-IMPORT #5: list_importable_scripts + remove_commands --------------

    #[test]
    fn list_importable_scripts_returns_the_discoverable_set() {
        // R-IMPORT #5 done-criterion (a): list_importable_scripts returns the filtered,
        // monorepo-aware discoverable set WITHOUT creating any template.
        let tmp = std::env::temp_dir().join(format!("nyx-list-import-{}", uuid_like()));
        std::fs::create_dir_all(tmp.join("packages/api")).expect("mkdir api");
        std::fs::write(
            tmp.join("package.json"),
            r#"{ "name": "root", "scripts": { "lint": "eslint" } }"#,
        )
        .expect("root pkg");
        std::fs::write(
            tmp.join("packages/api/package.json"),
            r#"{ "name": "api", "scripts": { "dev": "node ." } }"#,
        )
        .expect("api pkg");
        // node_modules must be filtered out by the shared discovery.
        std::fs::create_dir_all(tmp.join("node_modules/dep")).expect("mkdir nm");
        std::fs::write(
            tmp.join("node_modules/dep/package.json"),
            r#"{ "scripts": { "leak": "x" } }"#,
        )
        .expect("nm pkg");
        let root = tmp.to_string_lossy().to_string();
        let s = seed_crud_dispatcher(&root);

        let result = s
            .dispatcher
            .call("list_importable_scripts", &json!({ "project_id": s.project_id }))
            .expect("list_importable_scripts runs");
        assert_eq!(result["manifests_found"], 2, "root + api manifests, node_modules excluded");
        let scripts = result["scripts"].as_array().expect("scripts array");
        let names: Vec<&str> = scripts.iter().filter_map(|c| c["name"].as_str()).collect();
        assert!(names.contains(&"lint"), "root lint listed, got {names:?}");
        assert!(names.contains(&"dev"), "api dev listed, got {names:?}");
        assert!(names.iter().all(|n| *n != "leak"), "node_modules script not listed");
        // Each entry carries the preview fields, not a command_id.
        let lint = scripts.iter().find(|c| c["name"] == "lint").unwrap();
        assert_eq!(lint["script_name"], "lint");
        assert_eq!(lint["body"], "eslint");
        assert!(lint["command_id"].is_null(), "no template id (nothing created)");

        // NOTHING was created.
        let templates = s
            .dispatcher
            .call("list_commands", &json!({ "project_id": s.project_id }))
            .expect("list templates");
        assert!(
            templates["commands"].as_array().unwrap().is_empty(),
            "list_importable_scripts created NO template"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn list_importable_scripts_requires_a_target() {
        let s = seed_crud_dispatcher("/tmp/nyx-list-import-none");
        let err = s
            .dispatcher
            .call("list_importable_scripts", &json!({}))
            .expect_err("neither project_id nor workspace_id");
        assert_eq!(err.code, "invalid_argument");
    }

    #[test]
    fn list_importable_scripts_unknown_workspace_is_invalid_id() {
        let s = seed_crud_dispatcher("/tmp/nyx-list-import-badws");
        let err = s
            .dispatcher
            .call("list_importable_scripts", &json!({ "workspace_id": "no-such-ws" }))
            .expect_err("unknown workspace");
        assert_eq!(err.code, "invalid_id");
    }

    #[test]
    fn remove_commands_removes_a_batch_and_returns_the_count() {
        // R-IMPORT #5 done-criterion (b): remove_commands removes a batch and returns the
        // removed count (+ per-id acks, mirror of remove_command).
        let s = seed_crud_dispatcher("/tmp/nyx-rm-batch");
        let mut ids = Vec::new();
        for (name, cmd) in [("dev", "vite"), ("build", "tsc"), ("test", "vitest")] {
            let created = s
                .dispatcher
                .call(
                    "add_command",
                    &json!({ "project_id": s.project_id, "name": name, "command": cmd }),
                )
                .expect("add");
            ids.push(created["command"]["command_id"].as_str().unwrap().to_string());
        }
        let result = s
            .dispatcher
            .call("remove_commands", &json!({ "command_ids": ids }))
            .expect("remove_commands runs");
        assert_eq!(result["removed"], 3, "all three templates removed");
        let results = result["results"].as_array().expect("results array");
        assert_eq!(results.len(), 3);
        assert!(
            results.iter().all(|r| r["removed"] == true),
            "every id acked removed:true, got {results:?}"
        );
        // The project now has no templates.
        let listed = s
            .dispatcher
            .call("list_commands", &json!({ "project_id": s.project_id }))
            .expect("list templates");
        assert!(listed["commands"].as_array().unwrap().is_empty(), "all removed");
    }

    #[test]
    fn remove_commands_partial_failure_does_not_abort_the_batch() {
        // An unknown id in the batch is reported in its ack but does NOT stop the valid
        // ids from being removed.
        let s = seed_crud_dispatcher("/tmp/nyx-rm-batch-partial");
        let created = s
            .dispatcher
            .call(
                "add_command",
                &json!({ "project_id": s.project_id, "name": "dev", "command": "vite" }),
            )
            .expect("add");
        let good = created["command"]["command_id"].as_str().unwrap().to_string();
        let result = s
            .dispatcher
            .call(
                "remove_commands",
                &json!({ "command_ids": [good, "no-such-template"] }),
            )
            .expect("remove_commands runs");
        assert_eq!(result["removed"], 1, "only the valid id removed");
        let results = result["results"].as_array().unwrap();
        let bad = results
            .iter()
            .find(|r| r["command_id"] == "no-such-template")
            .expect("the unknown id has an ack");
        assert_eq!(bad["removed"], false);
        assert_eq!(bad["error"]["code"], "invalid_id", "unknown id → invalid_id in its ack");
    }

    #[test]
    fn remove_commands_requires_command_ids_array() {
        let s = seed_crud_dispatcher("/tmp/nyx-rm-batch-bad");
        let err = s
            .dispatcher
            .call("remove_commands", &json!({}))
            .expect_err("missing command_ids");
        assert_eq!(err.code, "invalid_argument");
        let err2 = s
            .dispatcher
            .call("remove_commands", &json!({ "command_ids": "not-an-array" }))
            .expect_err("command_ids not an array");
        assert_eq!(err2.code, "invalid_argument");
    }

    #[test]
    fn remove_commands_emits_one_event_for_the_batch() {
        // The grouped delete emits exactly ONE commands://changed for the whole batch.
        let (d, project_id, _ws, count, _app) = seed_command_change_listener("/tmp/nyx-rm-batch-ev");
        let mut ids = Vec::new();
        for (name, cmd) in [("dev", "vite"), ("build", "tsc")] {
            let created = d
                .call(
                    "add_command",
                    &json!({ "project_id": project_id, "name": name, "command": cmd }),
                )
                .expect("add");
            ids.push(created["command"]["command_id"].as_str().unwrap().to_string());
        }
        // Each add already emitted one tick; reset our reasoning to the delta.
        let before = ticks(&count);
        d.call("remove_commands", &json!({ "command_ids": ids }))
            .expect("remove batch");
        assert_eq!(
            ticks(&count) - before,
            1,
            "the grouped delete emits exactly one commands://changed for the batch"
        );
    }

    #[test]
    fn remove_commands_empty_batch_is_a_noop_with_no_event() {
        let (d, _project_id, _ws, count, _app) =
            seed_command_change_listener("/tmp/nyx-rm-batch-empty");
        let result = d
            .call("remove_commands", &json!({ "command_ids": [] }))
            .expect("empty batch runs");
        assert_eq!(result["removed"], 0, "nothing removed");
        assert_eq!(ticks(&count), 0, "an empty batch emits no event");
    }

    // --- command-band refresh event (commands://changed) ------------------
    //
    // A template mutation feeds the sidebar COMMANDS band (`useCommandInstances`) and
    // the Manage Commands modal (`useCommands`); NEITHER re-loads on a template added
    // to an EXISTING workspace, so an MCP-driven add/update/import never appeared live.
    // The fix routes every mutating tool through `bridge::emit_commands_changed`, the
    // SAME `commands://changed` signal the UI's own command mutations emit. These tests
    // (mirroring the workspaces `seed_workspace_change_listener` ones) prove each tool
    // emits exactly ONE tick on a SUCCESSFUL mutation and stays silent on a rejected /
    // no-op call. Same mock-runtime seam as the rest of the suite; no process spawns.

    /// A mock dispatcher backed by an in-memory `Db` + a managed runner, plus a shared
    /// counter wired to the `commands://changed` event — so a test can drive a template
    /// mutation and assert the band's refresh signal fired exactly N times. Returns the
    /// seeded `(dispatcher, project_id, workspace_id, counter, app)`; the app is held so
    /// its managed state outlives the dispatcher's handle borrow.
    fn seed_command_change_listener(
        workspace_root: &str,
    ) -> (
        NyxToolDispatcher<MockRuntime>,
        String,
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tauri::App<MockRuntime>,
    ) {
        use tauri::Listener;
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app builds");
        let db = Db::in_memory();
        let (project_id, workspace_id) = db.with_conn(|c| {
            let (project, workspace) =
                db::create_project(c, "proj", workspace_root, None).expect("create project");
            (project.id, workspace.id)
        });
        app.manage(db);
        crate::bridge::manage_command_runner(&app.handle().clone());

        // Count every `commands://changed` tick — the SAME event the band listens on
        // (`bridge::COMMANDS_CHANGED_EVENT`).
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let count = std::sync::Arc::clone(&count);
            app.listen(crate::bridge::COMMANDS_CHANGED_EVENT, move |_event| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }

        let dispatcher = NyxToolDispatcher::new(app.handle().clone());
        (dispatcher, project_id, workspace_id, count, app)
    }

    /// Read the current `commands://changed` tick count.
    fn ticks(count: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> usize {
        count.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn add_command_emits_one_command_change_event() {
        // A successful add_command emits exactly one commands://changed so the band
        // re-pulls without a manual reload.
        let (d, project_id, _ws, count, _app) = seed_command_change_listener("/tmp/nyx-cmd-evt-add");
        d.call(
            "add_command",
            &json!({ "project_id": project_id, "name": "dev", "command": "vite" }),
        )
        .expect("add_command succeeds");
        assert_eq!(ticks(&count), 1, "a successful add emits exactly one refresh tick");
    }

    #[test]
    fn rejected_add_command_emits_no_command_change_event() {
        // A duplicate name (UNIQUE backstop → invalid_state) commits no row, so it must
        // emit NO refresh event — the signal fires only when a template actually changed.
        let (d, project_id, _ws, count, _app) = seed_command_change_listener("/tmp/nyx-cmd-evt-dup");
        d.call(
            "add_command",
            &json!({ "project_id": project_id, "name": "dev", "command": "vite" }),
        )
        .expect("first add succeeds");
        let dup = d
            .call(
                "add_command",
                &json!({ "project_id": project_id, "name": "dev", "command": "next dev" }),
            )
            .expect_err("a duplicate name is refused");
        assert_eq!(dup.code, "invalid_state", "duplicate name → invalid_state");
        assert_eq!(
            ticks(&count),
            1,
            "only the SUCCESSFUL add emitted; the rejected duplicate emitted nothing"
        );
    }

    #[test]
    fn update_command_emits_one_command_change_event() {
        // A successful update_command emits exactly one commands://changed (the add that
        // seeds the template emits its own, so the update is the SECOND tick).
        let (d, project_id, _ws, count, _app) =
            seed_command_change_listener("/tmp/nyx-cmd-evt-update");
        let created = d
            .call(
                "add_command",
                &json!({ "project_id": project_id, "name": "dev", "command": "vite" }),
            )
            .expect("seed template");
        assert_eq!(ticks(&count), 1, "the seeding add emitted its own tick");
        let command_id = created["command"]["command_id"].as_str().unwrap().to_string();

        d.call(
            "update_command",
            &json!({ "command_id": command_id, "command": "vite --host" }),
        )
        .expect("update succeeds");
        assert_eq!(ticks(&count), 2, "a successful update emits exactly one more tick");
    }

    #[test]
    fn rejected_update_command_emits_no_command_change_event() {
        // An unknown command_id is a no-op (invalid_id), so it must emit NO refresh.
        let (d, _project_id, _ws, count, _app) =
            seed_command_change_listener("/tmp/nyx-cmd-evt-badupd");
        let err = d
            .call("update_command", &json!({ "command_id": "no-such-template", "name": "x" }))
            .expect_err("unknown template id");
        assert_eq!(err.code, "invalid_id");
        assert_eq!(ticks(&count), 0, "a no-op update emits nothing");
    }

    #[test]
    fn import_commands_emits_one_command_change_event_when_something_imported() {
        // A real package.json import (scripts → templates) emits exactly one tick.
        let tmp = std::env::temp_dir().join(format!("nyx-cmd-evt-import-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(
            tmp.join("package.json"),
            r#"{ "scripts": { "dev": "vite", "build": "tsc" } }"#,
        )
        .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let (d, project_id, _ws, count, _app) = seed_command_change_listener(&root);

        let result = d
            .call("import_commands", &json!({ "project_id": project_id }))
            .expect("import runs");
        assert!(
            !result["imported"].as_array().unwrap().is_empty(),
            "the run imported at least one template"
        );
        assert_eq!(
            ticks(&count),
            1,
            "a single tick covers the whole import (one re-pull), not one per template"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn import_commands_emits_no_event_when_nothing_imported() {
        // An import that imports nothing (all names already exist → skipped) changed no
        // row, so it must emit NO refresh — the second run is a pure no-op.
        let tmp = std::env::temp_dir().join(format!("nyx-cmd-evt-import-none-{}", uuid_like()));
        std::fs::create_dir_all(&tmp).expect("temp workspace dir");
        std::fs::write(tmp.join("package.json"), r#"{ "scripts": { "dev": "vite" } }"#)
            .expect("write package.json");
        let root = tmp.to_string_lossy().to_string();
        let (d, project_id, _ws, count, _app) = seed_command_change_listener(&root);

        d.call("import_commands", &json!({ "project_id": project_id }))
            .expect("first import runs");
        assert_eq!(ticks(&count), 1, "the first import emitted its tick");

        let again = d
            .call("import_commands", &json!({ "project_id": project_id }))
            .expect("second import runs");
        assert!(
            again["imported"].as_array().unwrap().is_empty(),
            "the re-run imports nothing new"
        );
        assert_eq!(
            ticks(&count),
            1,
            "a re-run that imports nothing emits no further tick"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    // --- C2: per-tool actionable errors when a command_id (template) is passed ---
    //
    // The four instance-action tools (start/stop/relaunch/get_command_output) must
    // return an actionable `invalid_id` naming the correct path when the caller
    // passes a template `command_id` instead of a launchable `instance_id`.

    #[test]
    fn start_command_rejects_template_id_with_actionable_error() {
        let s = seed_dispatcher("start-c2");
        // Pass the template command_id to start_command — it resolves via
        // resolve_instance_id (explicit id path) → resolve_command_and_cwd → bad_instance_id_error.
        let err = s
            .dispatcher
            .call("start_command", &json!({ "instance_id": s.command_id }))
            .expect_err("start_command rejects a template command_id");
        assert_eq!(err.code, "invalid_id", "start_command: wrong code, got: {}", err.code);
        assert!(
            err.message.contains("TEMPLATE"),
            "start_command: message must name the template path, got: {}",
            err.message
        );
    }

    #[test]
    fn stop_command_rejects_template_id_with_actionable_error() {
        let s = seed_dispatcher("stop-c2");
        // stop_command validates via assert_instance_exists → bad_instance_id_error.
        let err = s
            .dispatcher
            .call("stop_command", &json!({ "instance_id": s.command_id }))
            .expect_err("stop_command rejects a template command_id");
        assert_eq!(err.code, "invalid_id", "stop_command: wrong code, got: {}", err.code);
        assert!(
            err.message.contains("TEMPLATE"),
            "stop_command: message must name the template path, got: {}",
            err.message
        );
    }

    #[test]
    fn relaunch_command_rejects_template_id_with_actionable_error() {
        let s = seed_dispatcher("relaunch-c2");
        // relaunch_command resolves via resolve_command_and_cwd → bad_instance_id_error.
        let err = s
            .dispatcher
            .call("relaunch_command", &json!({ "instance_id": s.command_id }))
            .expect_err("relaunch_command rejects a template command_id");
        assert_eq!(err.code, "invalid_id", "relaunch_command: wrong code, got: {}", err.code);
        assert!(
            err.message.contains("TEMPLATE"),
            "relaunch_command: message must name the template path, got: {}",
            err.message
        );
    }

    #[test]
    fn get_command_output_rejects_template_id_with_actionable_error() {
        let s = seed_dispatcher("output-c2");
        // get_command_output resolves via resolve_instance_id (explicit id path) →
        // the cold DB branch → bad_instance_id_error.
        let err = s
            .dispatcher
            .call("get_command_output", &json!({ "instance_id": s.command_id }))
            .expect_err("get_command_output rejects a template command_id");
        assert_eq!(err.code, "invalid_id", "get_command_output: wrong code, got: {}", err.code);
        assert!(
            err.message.contains("TEMPLATE"),
            "get_command_output: message must name the template path, got: {}",
            err.message
        );
    }

    // --- Interactive terminal tools (PRD-4 review R-TERM) -----------------
    //
    // These drive the REAL dispatcher over a mock app whose terminal managed state is wired
    // via `bridge::init` (so TerminalPtyMap / PendingTerminalCommands / PtyManager exist),
    // plus a managed in-memory `Db`. They assert the 4 tools' contracts AND the
    // `terminals://changed` emission, WITHOUT spawning any real PTY (the front owns that),
    // so they run under the ConPTY gap.

    use tauri::Listener;

    /// A mock app with the terminal managed state wired (`bridge::init`) + an in-memory `Db`,
    /// plus a counter on `terminals://changed`. Returns `(dispatcher, db-app, counter)`; the
    /// app is held so its managed state outlives the dispatcher's handle borrow.
    fn seed_terminal_app() -> (
        NyxToolDispatcher<MockRuntime>,
        tauri::App<MockRuntime>,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let app = crate::bridge::init(mock_builder())
            .build(mock_context(noop_assets()))
            .expect("mock app builds with terminal state");
        app.manage(Db::in_memory());
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let count = std::sync::Arc::clone(&count);
            app.listen(crate::bridge::TERMINALS_CHANGED_EVENT, move |_event| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
        let dispatcher = NyxToolDispatcher::new(app.handle().clone());
        (dispatcher, app, count)
    }

    fn term_ticks(count: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> usize {
        count.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn create_terminal_bare_makes_a_record_and_emits_changed() {
        // Without a command: a terminal record is created (a bare shell), has_command:false,
        // and exactly one terminals://changed is emitted so the front mounts the xterm.
        let (d, app, count) = seed_terminal_app();
        let res = d.call("create_terminal", &json!({})).expect("create bare terminal");
        let terminal_id = res["terminal_id"].as_str().expect("terminal_id").to_string();
        assert_eq!(res["has_command"], json!(false), "no command parked → bare shell");
        assert!(res["workspace_id"].is_null(), "no cwd → loose terminal");
        assert_eq!(term_ticks(&count), 1, "one terminals://changed emitted");
        // The record is persisted + alive (the front will reconcile it in).
        let alive = app
            .state::<Db>()
            .with_conn(db::list_terminals)
            .unwrap()
            .into_iter()
            .any(|t| t.id == terminal_id && t.status == db::STATUS_ALIVE);
        assert!(alive, "a fresh alive terminal record exists");
        // No command was parked for a bare terminal.
        assert_eq!(
            app.state::<PendingTerminalCommands>().take(&terminal_id),
            None,
            "a bare create parks no command"
        );
    }

    #[test]
    fn create_terminal_with_command_parks_it_for_injection() {
        // With a command: the same record is created, has_command:true, and the command is
        // PARKED keyed by the record id so register_terminal_pty injects it at opening.
        let (d, app, count) = seed_terminal_app();
        let res = d
            .call("create_terminal", &json!({ "command": "echo hi" }))
            .expect("create terminal with command");
        let terminal_id = res["terminal_id"].as_str().unwrap().to_string();
        assert_eq!(res["has_command"], json!(true), "a command was supplied");
        assert_eq!(term_ticks(&count), 1, "one terminals://changed emitted");
        assert_eq!(
            app.state::<PendingTerminalCommands>().take(&terminal_id).as_deref(),
            Some("echo hi"),
            "the opening command is parked for the front's PTY to run"
        );
    }

    #[test]
    fn create_terminal_attaches_to_a_known_workspace_via_cwd() {
        // A cwd inside a known workspace auto-attaches the terminal to it; a cwd matching no
        // workspace leaves it loose (no guessing, creates nothing).
        let (d, app, _count) = seed_terminal_app();
        let ws_path = std::env::temp_dir().join(format!("nyx-term-ws-{}", uuid_like()));
        std::fs::create_dir_all(&ws_path).unwrap();
        let ws_path = ws_path.to_string_lossy().to_string();
        let workspace_id = app
            .state::<Db>()
            .with_conn(|c| {
                let (_p, w) = db::create_project(c, "proj", &ws_path, None)?;
                Ok::<_, diesel::result::Error>(w.id)
            })
            .unwrap();
        // A cwd UNDER the workspace path → attaches to it.
        let inside = format!("{ws_path}/src");
        let res = d
            .call("create_terminal", &json!({ "cwd": inside }))
            .expect("create terminal in workspace");
        assert_eq!(
            res["workspace_id"].as_str(),
            Some(workspace_id.as_str()),
            "cwd inside a known workspace auto-attaches the terminal"
        );
        // A cwd matching NO workspace → loose.
        let res2 = d
            .call("create_terminal", &json!({ "cwd": "/nowhere/known" }))
            .expect("create loose terminal");
        assert!(res2["workspace_id"].is_null(), "an unmatched cwd leaves the terminal loose");
    }

    #[test]
    fn list_terminals_returns_open_terminals_and_the_pty_mapping() {
        // list_terminals lists the alive records with the live record↔pty mapping. Before the
        // front registers a PTY, `live` is false / pty_id null; after register_terminal_pty it
        // carries the id.
        let (d, app, _count) = seed_terminal_app();
        let res = d.call("create_terminal", &json!({})).expect("create");
        let terminal_id = res["terminal_id"].as_str().unwrap().to_string();

        let listed = d.call("list_terminals", &json!({})).expect("list_terminals");
        let rows = listed["terminals"].as_array().expect("terminals array");
        let row = rows
            .iter()
            .find(|r| r["terminal_id"] == json!(terminal_id))
            .expect("the created terminal is listed");
        assert_eq!(row["live"], json!(false), "no PTY registered yet → not live");
        assert!(row["pty_id"].is_null(), "no pty id before the front spawns it");

        // The front registers the record↔pty link (as its <Terminal> spawns).
        app.state::<TerminalPtyMap>().set(&terminal_id, 99);
        let listed2 = d.call("list_terminals", &json!({})).expect("list again");
        let row2 = listed2["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["terminal_id"] == json!(terminal_id))
            .unwrap()
            .clone();
        assert_eq!(row2["pty_id"], json!(99), "the live pty id is surfaced");
        assert_eq!(row2["live"], json!(true), "a registered PTY → live");
    }

    #[test]
    fn list_terminals_include_closed_surfaces_closed_records_for_rediscovery() {
        // Arbitration C: by default list_terminals hides CLOSED terminals, but
        // include_closed:true surfaces them (each carrying status:"closed") so an agent can
        // rediscover a finished terminal's id and read its last scrollback via read_terminal.
        let (d, _app, _count) = seed_terminal_app();
        let terminal_id = d
            .call("create_terminal", &json!({}))
            .expect("create")["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        // An open terminal carries status:"alive".
        let open = d.call("list_terminals", &json!({})).expect("list");
        let alive_row = open["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["terminal_id"] == json!(terminal_id))
            .expect("the alive terminal is listed by default");
        assert_eq!(alive_row["status"], json!("alive"), "an open terminal is alive");

        // Close it: the record stays but flips to closed.
        d.call("close_terminal", &json!({ "terminal_id": terminal_id }))
            .expect("close");

        // Default list HIDES the closed terminal...
        let default_list = d.call("list_terminals", &json!({})).expect("default list");
        assert!(
            default_list["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["terminal_id"] != json!(terminal_id)),
            "a closed terminal is hidden from the default list"
        );
        // ...but include_closed:true surfaces it with status:"closed".
        let with_closed = d
            .call("list_terminals", &json!({ "include_closed": true }))
            .expect("list including closed");
        let closed_row = with_closed["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["terminal_id"] == json!(terminal_id))
            .expect("include_closed surfaces the closed terminal");
        assert_eq!(closed_row["status"], json!("closed"), "it is reported as closed");
        assert_eq!(closed_row["live"], json!(false), "a closed terminal has no live PTY");
    }

    #[test]
    fn list_terminals_carries_busy_and_the_exec_state_marker_fields() {
        // Task #4: every listed terminal carries the OS `busy` bit + the settled exec-state
        // marker (`exec_state`, `exec_exit_code`, `exec_state_updated_at`), the SAME vocabulary
        // the managed-command tools expose. A fresh terminal with no live PTY has busy:null
        // (cannot be derived) and idle exec_state with a stamped timestamp.
        let (d, _app, _count) = seed_terminal_app();
        let terminal_id = d
            .call("create_terminal", &json!({}))
            .expect("create")["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();

        let listed = d.call("list_terminals", &json!({})).expect("list_terminals");
        let row = listed["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["terminal_id"] == json!(terminal_id))
            .expect("the created terminal is listed")
            .clone();
        // No live PTY yet → busy cannot be derived from the OS → null (never a phantom bit).
        assert!(row["busy"].is_null(), "no live PTY → busy is null, got {}", row["busy"]);
        // The settled-result marker fields are present, defaulting to idle for a fresh terminal.
        assert_eq!(row["exec_state"], json!("idle"), "fresh terminal is idle");
        assert!(row["exec_exit_code"].is_null(), "no exit code yet");
        assert!(
            row["exec_state_updated_at"].as_i64().is_some_and(|t| t > 0),
            "exec_state_updated_at is a stamped epoch-ms, got {}",
            row["exec_state_updated_at"]
        );
    }

    #[test]
    fn agent_detects_short_command_end_via_advancing_exec_state_marker() {
        // Task #4 DONE criterion (the integration test): an agent detects that a command sent
        // via send_to_terminal has FINISHED — even an ULTRA-SHORT one that starts and finishes
        // between two polls — by capturing `exec_state_updated_at` BEFORE the send and polling
        // list_terminals until it ADVANCES to a settled state. A `busy` boolean alone would race
        // (the short command could be entirely missed between polls → false negative); the
        // monotone timestamp catches it. We drive the SAME exec-state persistence path the
        // OSC-133 annotation uses (`db::set_exec_state`) to model the command's running→success
        // transition, decoupled from any real shell emission.
        let (d, app, _count) = seed_terminal_app();
        let res = d.call("create_terminal", &json!({})).expect("create");
        let terminal_id = res["terminal_id"].as_str().unwrap().to_string();
        // A live PTY backs the record so send_to_terminal has somewhere to write (the OS touch).
        let pty_id = crate::bridge::tests_spawn_pty(&app);
        app.state::<TerminalPtyMap>().set(&terminal_id, pty_id);

        // The agent's protocol step 1: snapshot the marker BEFORE sending.
        let before = d.call("list_terminals", &json!({})).expect("list before");
        let baseline = before["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["terminal_id"] == json!(terminal_id))
            .unwrap()["exec_state_updated_at"]
            .as_i64()
            .expect("baseline exec_state_updated_at");

        // Send the command. (The newline write is the real send path; the output/exec-state
        // would be driven by the pump+annotation in production.)
        d.call("send_to_terminal", &json!({ "terminal_id": terminal_id, "command": "true" }))
            .expect("send_to_terminal");

        // Model an ULTRA-SHORT command: it ran and finished settling to success ENTIRELY between
        // two polls — so a busy snapshot taken now reads idle/false as if nothing happened. The
        // settle stamps a NEW exec_state_updated_at (a small wait guarantees the wall-clock ms
        // advances even on a fast machine — the same time the send + run consumes in practice).
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.state::<Db>()
            .with_conn(|c| db::set_exec_state(c, &terminal_id, db::STATE_RUNNING, None, false))
            .expect("running");
        app.state::<Db>()
            .with_conn(|c| db::set_exec_state(c, &terminal_id, db::STATE_SUCCESS, Some(0), true))
            .expect("settle success");

        // The agent's protocol step 2: poll until the marker ADVANCES past the baseline AND the
        // state is settled. It sees the finish despite never catching busy:true.
        let after = d.call("list_terminals", &json!({})).expect("list after");
        let row = after["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["terminal_id"] == json!(terminal_id))
            .unwrap()
            .clone();
        let updated = row["exec_state_updated_at"].as_i64().expect("updated at");
        assert!(
            updated > baseline,
            "the monotone marker advanced (baseline {baseline} → {updated}), so the agent \
             detects the command finished even though it was missed by a busy snapshot"
        );
        assert_eq!(row["exec_state"], json!("success"), "settled to success");
        assert_eq!(row["exec_exit_code"], json!(0), "carries the exit code");

        let _ = app.state::<PtyManager>().close_id(pty_id);
    }

    #[test]
    fn send_to_terminal_writes_a_command_to_the_live_pty() {
        // send_to_terminal resolves the record id → live PTY and writes the command+newline.
        // We spawn a REAL pty (the write path needs a live PTY) and register the link; the
        // tool then succeeds. (A pty spawn is the only OS touch; no ConPTY interactive grid.)
        let (d, app, _count) = seed_terminal_app();
        let res = d.call("create_terminal", &json!({})).expect("create");
        let terminal_id = res["terminal_id"].as_str().unwrap().to_string();
        // Spawn a pty and register it as this record's live shell (what the front does).
        let pty_id = crate::bridge::tests_spawn_pty(&app);
        app.state::<TerminalPtyMap>().set(&terminal_id, pty_id);

        let sent = d
            .call("send_to_terminal", &json!({ "terminal_id": terminal_id, "command": "ls" }))
            .expect("send_to_terminal writes to the live pty");
        assert_eq!(sent["sent"], json!(true), "the command was written");
        let _ = app.state::<PtyManager>().close_id(pty_id);
    }

    #[test]
    fn send_to_terminal_unknown_id_is_invalid_id() {
        let (d, _app, _count) = seed_terminal_app();
        let err = d
            .call("send_to_terminal", &json!({ "terminal_id": "no-such", "command": "ls" }))
            .expect_err("unknown terminal id");
        assert_eq!(err.code, "invalid_id", "unknown id → invalid_id");
    }

    #[test]
    fn send_to_terminal_with_no_live_pty_is_invalid_state() {
        // An alive record whose PTY has not registered yet → invalid_state (it is starting up),
        // distinct from an unknown id.
        let (d, _app, _count) = seed_terminal_app();
        let res = d.call("create_terminal", &json!({})).expect("create");
        let terminal_id = res["terminal_id"].as_str().unwrap().to_string();
        let err = d
            .call("send_to_terminal", &json!({ "terminal_id": terminal_id, "command": "ls" }))
            .expect_err("no live pty yet");
        assert_eq!(err.code, "invalid_state", "no live shell yet → invalid_state");
    }

    #[test]
    fn close_terminal_closes_the_record_and_emits_changed() {
        // close_terminal flips the record closed, drops the link, and emits terminals://changed
        // so the front retires the pane.
        let (d, app, count) = seed_terminal_app();
        let res = d.call("create_terminal", &json!({})).expect("create");
        let terminal_id = res["terminal_id"].as_str().unwrap().to_string();
        assert_eq!(term_ticks(&count), 1, "create emitted one tick");
        // Pretend the front registered a (non-live) link; close must clear it.
        app.state::<TerminalPtyMap>().set(&terminal_id, 5);

        let closed = d
            .call("close_terminal", &json!({ "terminal_id": terminal_id }))
            .expect("close_terminal");
        assert_eq!(closed["closed"], json!(true), "the terminal was closed");
        assert_eq!(term_ticks(&count), 2, "close emitted a second tick");
        // The record is now closed (not alive) and the link is dropped.
        let still_alive = app
            .state::<Db>()
            .with_conn(db::list_terminals)
            .unwrap()
            .into_iter()
            .any(|t| t.id == terminal_id && t.status == db::STATUS_ALIVE);
        assert!(!still_alive, "the record is flipped to closed");
        assert_eq!(app.state::<TerminalPtyMap>().get(&terminal_id), None, "the link is cleared");
    }

    #[test]
    fn close_terminal_unknown_id_is_invalid_id_and_emits_nothing() {
        let (d, _app, count) = seed_terminal_app();
        let err = d
            .call("close_terminal", &json!({ "terminal_id": "no-such" }))
            .expect_err("unknown terminal id");
        assert_eq!(err.code, "invalid_id", "unknown id → invalid_id");
        assert_eq!(term_ticks(&count), 0, "a rejected close emits nothing");
    }

    // --- read_terminal (PRD-4.1 #1): read the front-serialized scrollback -------
    //
    // These drive the REAL dispatcher over a terminal record whose persisted scrollback was
    // seeded via the SAME `db::persist_scrollback` the front uses. No second buffer, no PTY:
    // read_terminal reads exactly the last persisted snapshot.

    #[test]
    fn read_terminal_unknown_id_is_invalid_id() {
        // An unknown / no-longer-existing terminal record → structured invalid_id (D8).
        let (d, _app, _count) = seed_terminal_app();
        let err = d
            .call("read_terminal", &json!({ "terminal_id": "no-such" }))
            .expect_err("unknown terminal id");
        assert_eq!(err.code, "invalid_id", "unknown record → invalid_id");
    }

    #[test]
    fn read_terminal_default_strips_ansi_and_returns_single_output_field() {
        // A default read (no strip_ansi) returns ONE cleaned `output` field — no raw escapes.
        let (d, app, _count) = seed_terminal_app();
        let res = d.call("create_terminal", &json!({})).expect("create");
        let terminal_id = res["terminal_id"].as_str().unwrap().to_string();
        // Seed the persisted scrollback with ANSI-laced text (what SerializeAddon emits).
        let raw = "\x1b[32mgreen\x1b[0m\nplain\n";
        app.state::<Db>()
            .with_conn(|c| db::persist_scrollback(c, &terminal_id, raw))
            .expect("seed scrollback");

        let out = d
            .call("read_terminal", &json!({ "terminal_id": terminal_id }))
            .expect("read_terminal");
        assert_eq!(
            out["output"].as_str().unwrap(),
            "green\nplain\n",
            "default read strips ANSI to one cleaned output field"
        );
        // total_bytes is the RAW length (cursor/round-trip is byte-exact even when stripped).
        assert_eq!(out["total_bytes"], json!(raw.len()));
        assert_eq!(out["cursor"], json!(raw.len()), "cursor is one past the raw end");
        assert!(out.get("text").is_none(), "no parallel raw output+text pair");
    }

    #[test]
    fn read_terminal_strip_false_returns_raw_window() {
        let (d, app, _count) = seed_terminal_app();
        let terminal_id = d
            .call("create_terminal", &json!({}))
            .expect("create")["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        let raw = "\x1b[31mred\x1b[0m\n";
        app.state::<Db>()
            .with_conn(|c| db::persist_scrollback(c, &terminal_id, raw))
            .expect("seed scrollback");
        let out = d
            .call("read_terminal", &json!({ "terminal_id": terminal_id, "strip_ansi": false }))
            .expect("read raw");
        assert_eq!(out["output"].as_str().unwrap(), raw, "strip_ansi:false returns the raw window");
    }

    #[test]
    fn read_terminal_tail_bytes_bounds_the_window_from_the_end() {
        // tail_bytes keeps only the LAST N bytes and marks truncated.
        let (d, app, _count) = seed_terminal_app();
        let terminal_id = d
            .call("create_terminal", &json!({}))
            .expect("create")["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        let raw = "0123456789"; // 10 bytes, ANSI-free so strip is a no-op
        app.state::<Db>()
            .with_conn(|c| db::persist_scrollback(c, &terminal_id, raw))
            .expect("seed scrollback");
        let out = d
            .call("read_terminal", &json!({ "terminal_id": terminal_id, "tail_bytes": 4 }))
            .expect("bounded read");
        assert_eq!(out["output"].as_str().unwrap(), "6789", "tail keeps the last 4 bytes");
        assert_eq!(out["truncated"], json!(true), "earlier bytes were dropped → truncated");
        assert_eq!(out["total_bytes"], json!(10), "total is the full scrollback length");
        assert_eq!(out["cursor"], json!(10), "cursor still points one past the full end");
    }

    #[test]
    fn read_terminal_over_the_ceiling_is_output_too_large() {
        // tail_bytes/max_bytes over MAX_TAIL_BYTES → output_too_large, the SAME bound as the
        // command flows (PRD-4.1 #3 ports the code onto read_terminal).
        let (d, _app, _count) = seed_terminal_app();
        let terminal_id = d
            .call("create_terminal", &json!({}))
            .expect("create")["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        let err = d
            .call(
                "read_terminal",
                &json!({ "terminal_id": terminal_id, "tail_bytes": MAX_TAIL_BYTES + 1 }),
            )
            .expect_err("over the ceiling");
        assert_eq!(err.code, "output_too_large", "over-cap read is output_too_large");
        // The enriched payload (PRD-4.1 #3) carries machine-readable requested/limit.
        let data = err.data.as_ref().expect("output_too_large carries structured data");
        assert_eq!(data["requested"], json!(MAX_TAIL_BYTES + 1), "requested echoes the ask");
        assert_eq!(data["limit"], json!(MAX_TAIL_BYTES), "limit is the hard ceiling");
    }

    #[test]
    fn read_terminal_since_is_incremental_without_gap_or_dup() {
        // since (a prior cursor) reads only what is NEW: chaining read_terminal(since=cursor)
        // returns the bytes appended after, with no gap and no duplicate.
        let (d, app, _count) = seed_terminal_app();
        let terminal_id = d
            .call("create_terminal", &json!({}))
            .expect("create")["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        let first = "line one\n";
        app.state::<Db>()
            .with_conn(|c| db::persist_scrollback(c, &terminal_id, first))
            .expect("seed initial scrollback");
        let r1 = d
            .call("read_terminal", &json!({ "terminal_id": terminal_id }))
            .expect("first read");
        let cursor = r1["cursor"].as_u64().unwrap();
        assert_eq!(cursor as usize, first.len(), "cursor is one past the seeded end");

        // The terminal produces more output; the front persists the GROWN scrollback.
        let grown = "line one\nline two\n";
        app.state::<Db>()
            .with_conn(|c| db::persist_scrollback(c, &terminal_id, grown))
            .expect("persist grown scrollback");
        let r2 = d
            .call("read_terminal", &json!({ "terminal_id": terminal_id, "since": cursor }))
            .expect("incremental read");
        assert_eq!(
            r2["output"].as_str().unwrap(),
            "line two\n",
            "since=cursor returns only the newly-appended bytes (no gap, no dup)"
        );
        assert_eq!(r2["cursor"], json!(grown.len()), "cursor advances to the new end");
    }

    #[test]
    fn read_terminal_closed_record_returns_last_persisted_scrollback() {
        // DECIDED: as long as the record EXISTS, a CLOSED terminal returns its last persisted
        // scrollback (the record outlives the PTY). Only a missing record → invalid_id.
        let (d, app, _count) = seed_terminal_app();
        let terminal_id = d
            .call("create_terminal", &json!({}))
            .expect("create")["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        let raw = "final output before close\n";
        app.state::<Db>()
            .with_conn(|c| db::persist_scrollback(c, &terminal_id, raw))
            .expect("seed scrollback");
        // Close the terminal — the record stays, flipped to `closed`.
        d.call("close_terminal", &json!({ "terminal_id": terminal_id }))
            .expect("close");
        // The read still succeeds and returns the last persisted scrollback.
        let out = d
            .call("read_terminal", &json!({ "terminal_id": terminal_id }))
            .expect("read a closed terminal's last scrollback");
        assert_eq!(
            out["output"].as_str().unwrap(),
            raw,
            "a closed-but-still-recorded terminal returns its last persisted scrollback"
        );
    }

    // --- MCP terminal loop integration: create → send → read (PRD-4.1 task #3) ----
    //
    // The AUTOMATED regression for the exact hole the PRD-4 dogfood found: an agent could
    // open a terminal and send a command via MCP but had NO way to READ its output
    // (`send_to_terminal` returned only `{sent:true}`). This test drives the WHOLE loop
    // through the MCP dispatcher — `create_terminal` → `send_to_terminal` → `read_terminal`
    // — and asserts the command's output is verifiable from the `read_terminal` MCP
    // response ALONE, with NO UI inspection.
    //
    // Front dependency, made EXPLICIT (the PRD's assumed reserve): a terminal's PTY is
    // spawned by the FRONT when its `<Terminal>` mounts, and the scrollback is serialized
    // by the front's xterm `SerializeAddon` and persisted (debounced) via
    // `persist_scrollback`. A pure-Rust test has no xterm, so — exactly as the other
    // `read_terminal_*` tests and the PRD testing decision sanction — this test MODELS the
    // two front-owned steps: it registers a real live PTY as the terminal's shell (what
    // `register_terminal_pty` does on mount) and persists the rendered scrollback (what the
    // front does after output streams back). The MCP path itself — create, send, read — is
    // exercised end-to-end through the real dispatcher; only the xterm rendering is modeled.

    #[test]
    fn mcp_create_send_read_terminal_loop_is_verifiable_from_the_response_alone() {
        let (d, app, _count) = seed_terminal_app();

        // 1) An agent OPENS a terminal via MCP. This is the dogfood entry point: the
        //    response carries the terminal_id the agent then drives.
        let created = d
            .call("create_terminal", &json!({}))
            .expect("create_terminal via MCP");
        let terminal_id = created["terminal_id"]
            .as_str()
            .expect("the MCP response carries a terminal_id")
            .to_string();

        // FRONT DEPENDENCY (modeled): the front mounts <Terminal>, spawns the PTY, and
        // registers the record→pty link via `register_terminal_pty`. We do the same with a
        // REAL live PTY so the MCP write path has a live shell to target.
        let pty_id = crate::bridge::tests_spawn_pty(&app);
        app.state::<TerminalPtyMap>().set(&terminal_id, pty_id);

        // 2) The agent SENDS a command via MCP. Before the fix this was the dead end —
        //    `{sent:true}` with no way to observe the result. The write goes to the live PTY
        //    through the SAME PtyManager path `pty_write` uses.
        let marker = "nyx-loop-marker-7f3a";
        let sent = d
            .call(
                "send_to_terminal",
                &json!({ "terminal_id": terminal_id, "command": format!("echo {marker}") }),
            )
            .expect("send_to_terminal via MCP");
        assert_eq!(sent["sent"], json!(true), "the command was written to the live PTY");

        // FRONT DEPENDENCY (modeled): the PTY's output streams back through `pty://output`;
        // the front's xterm renders it and its SerializeAddon serializes the scrollback,
        // which is persisted (debounced) via `persist_scrollback`. We persist the scrollback
        // the front WOULD have captured — the echoed marker line — so the read has the last
        // front-persisted snapshot to serve (the documented debounce/staleness reserve).
        let rendered_scrollback = format!("$ echo {marker}\n{marker}\n");
        app.state::<Db>()
            .with_conn(|c| db::persist_scrollback(c, &terminal_id, &rendered_scrollback))
            .expect("front persists the rendered scrollback");

        // 3) The agent READS the output via MCP — the new capability that closes the loop.
        let read = d
            .call("read_terminal", &json!({ "terminal_id": terminal_id }))
            .expect("read_terminal via MCP");

        // The crux: the command's output is verifiable from the MCP `read_terminal`
        // response ALONE — no UI inspection. The agent sees the marker it just echoed.
        let output = read["output"].as_str().expect("read_terminal returns an output string");
        assert!(
            output.contains(marker),
            "the sent command's output is readable from the MCP response alone, got {output:?}"
        );
        // And the read carries the bounded-window metadata an agent needs to keep reading
        // (cursor for the next incremental `since`), all from the same MCP response.
        assert_eq!(
            read["total_bytes"].as_u64().unwrap() as usize,
            rendered_scrollback.len(),
            "total_bytes reflects the full persisted scrollback"
        );
        assert!(read["cursor"].is_u64(), "the response carries a cursor for incremental reads");

        // Clean up the live PTY (the only OS resource this test holds).
        let _ = app.state::<PtyManager>().close_id(pty_id);
    }

    // --- agent_session_event channel (PRD-5 #4, ADR-0004 / ADR-0010) -------
    //
    // The Claude Code SessionStart/SessionEnd hook target, driven through the REAL
    // NyxToolDispatcher over an in-memory Db (same mock-runtime seam as the rest of
    // this suite; `cargo test --lib` can't launch on this host — the ConPTY gap — but
    // `--no-run` type-checks them and CI runs them).

    /// Make a real terminal record and return its id (the NYX_TERMINAL_ID a hook sends).
    fn new_terminal(app: &tauri::App<MockRuntime>) -> String {
        app.state::<Db>()
            .with_conn(|c| db::create_terminal(c, "/work", None).map(|t| t.id))
            .expect("create terminal")
    }

    /// SessionStart creates an `agent_sessions` row (state=active), capturing the
    /// external id / cwd / transcript / source, bound to the terminal.
    #[test]
    fn session_start_creates_active_row() {
        let (d, app, _c) = seed_terminal_app();
        let tid = new_terminal(&app);
        let res = d
            .call(
                AGENT_SESSION_EVENT_TOOL,
                &json!({
                    "hook_event_name": "SessionStart",
                    "session_id": "claude-sid-1",
                    "cwd": "/work/proj",
                    "transcript_path": "/home/u/.claude/projects/h/claude-sid-1.jsonl",
                    "source": "startup",
                    "NYX_TERMINAL_ID": tid,
                }),
            )
            .expect("session start recorded");
        assert_eq!(res["event"], "SessionStart");
        assert_eq!(res["state"], db::SESSION_STATE_ACTIVE);
        assert_eq!(res["external_session_id"], "claude-sid-1");

        // The active row exists with the captured fields + source metadata.
        let row = app
            .state::<Db>()
            .with_conn(|c| db::active_session_for(c, &tid, db::AGENT_KIND_CLAUDE_CODE))
            .unwrap()
            .expect("an active session for the terminal");
        assert_eq!(row.external_session_id, "claude-sid-1");
        assert_eq!(row.cwd, "/work/proj");
        assert_eq!(row.transcript_path.as_deref(), Some("/home/u/.claude/projects/h/claude-sid-1.jsonl"));
        assert!(row.metadata_json.contains("startup"), "source stashed in metadata");
    }

    /// A second SessionStart on the SAME terminal (a resume) UPSERTS the one active
    /// row and pushes `last_seen_at` forward — never a duplicate active row.
    #[test]
    fn session_start_upserts_and_refreshes_last_seen() {
        let (d, app, _c) = seed_terminal_app();
        let tid = new_terminal(&app);
        d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "SessionStart", "session_id": "sid-a", "cwd": "/w", "source": "startup", "NYX_TERMINAL_ID": tid,
        })).expect("first start");
        let first = app.state::<Db>()
            .with_conn(|c| db::active_session_for(c, &tid, db::AGENT_KIND_CLAUDE_CODE)).unwrap().unwrap();

        // Sleep a hair so the millisecond clock advances, then resume.
        std::thread::sleep(std::time::Duration::from_millis(2));
        d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "SessionStart", "session_id": "sid-b", "cwd": "/w", "source": "resume", "NYX_TERMINAL_ID": tid,
        })).expect("resume start");

        let all = app.state::<Db>().with_conn(|c| db::sessions_for_terminal(c, &tid)).unwrap();
        assert_eq!(all.len(), 1, "upsert keeps ONE row, not two");
        let after = &all[0];
        assert_eq!(after.id, first.id, "same row updated in place");
        assert_eq!(after.external_session_id, "sid-b", "external id refreshed");
        assert!(after.last_seen_at >= first.last_seen_at, "last_seen_at moved forward");
    }

    /// SessionEnd on the clean case marks the matching active session `ended`.
    #[test]
    fn session_end_marks_session_ended() {
        let (d, app, _c) = seed_terminal_app();
        let tid = new_terminal(&app);
        d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "SessionStart", "session_id": "sid-end", "cwd": "/w", "NYX_TERMINAL_ID": tid,
        })).expect("start");
        let res = d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "SessionEnd", "session_id": "sid-end", "NYX_TERMINAL_ID": tid,
        })).expect("end");
        assert_eq!(res["ended"], json!(true));
        // No active row remains; the row is now `ended` with an ended_at stamp.
        assert!(app.state::<Db>()
            .with_conn(|c| db::active_session_for(c, &tid, db::AGENT_KIND_CLAUDE_CODE)).unwrap().is_none(),
            "no active session after a clean end");
        let row = app.state::<Db>().with_conn(|c| db::sessions_for_terminal(c, &tid)).unwrap().pop().unwrap();
        assert_eq!(row.state, db::SESSION_STATE_ENDED);
        assert!(row.ended_at.is_some(), "clean end stamps ended_at");
    }

    /// A brutal kill never fires SessionEnd: with no end event, the row stays `active`
    /// and resumable. We simulate "kill" as the absence of an end event and assert the
    /// row is still active (SQLite is the authority, ADR-0004 / ADR-0010).
    #[test]
    fn brutal_kill_leaves_session_resumable() {
        let (d, app, _c) = seed_terminal_app();
        let tid = new_terminal(&app);
        d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "SessionStart", "session_id": "sid-kill", "cwd": "/w", "NYX_TERMINAL_ID": tid,
        })).expect("start");
        // No SessionEnd is delivered (a SIGKILL / app crash). The row must remain active.
        let row = app.state::<Db>()
            .with_conn(|c| db::active_session_for(c, &tid, db::AGENT_KIND_CLAUDE_CODE)).unwrap()
            .expect("session still active after a kill (no clean end)");
        assert_eq!(row.state, db::SESSION_STATE_ACTIVE, "a killed session stays a resume candidate");
        assert!(row.ended_at.is_none(), "no ended_at without a clean SessionEnd");
    }

    /// A SessionEnd carrying a DIFFERENT external id does not end the active session
    /// (SQLite stays the authority; a stray end never corrupts state).
    #[test]
    fn session_end_with_mismatched_id_does_not_end() {
        let (d, app, _c) = seed_terminal_app();
        let tid = new_terminal(&app);
        d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "SessionStart", "session_id": "sid-live", "cwd": "/w", "NYX_TERMINAL_ID": tid,
        })).expect("start");
        let res = d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "SessionEnd", "session_id": "some-other-id", "NYX_TERMINAL_ID": tid,
        })).expect("end call returns");
        assert_eq!(res["ended"], json!(false), "a mismatched end does not end the live session");
        assert_eq!(app.state::<Db>()
            .with_conn(|c| db::active_session_for(c, &tid, db::AGENT_KIND_CLAUDE_CODE)).unwrap().unwrap().state,
            db::SESSION_STATE_ACTIVE);
    }

    /// An unknown terminal id → invalid_id (the D8 vocabulary).
    #[test]
    fn session_start_unknown_terminal_is_invalid_id() {
        let (d, _app, _c) = seed_terminal_app();
        let err = d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "SessionStart", "session_id": "x", "cwd": "/w", "NYX_TERMINAL_ID": "no-such-terminal",
        })).expect_err("unknown terminal");
        assert_eq!(err.code, "invalid_id");
    }

    /// A payload without NYX_TERMINAL_ID, or that is not a recognizable event, is an
    /// invalid_argument (the contract's error path).
    #[test]
    fn session_event_invalid_payloads_are_invalid_argument() {
        let (d, app, _c) = seed_terminal_app();
        let tid = new_terminal(&app);
        // Missing NYX_TERMINAL_ID.
        let e1 = d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "SessionStart", "session_id": "x", "cwd": "/w",
        })).expect_err("missing terminal id");
        assert_eq!(e1.code, "invalid_argument");
        // Unrecognized hook event.
        let e2 = d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "PreToolUse", "session_id": "x", "NYX_TERMINAL_ID": tid,
        })).expect_err("not a session event");
        assert_eq!(e2.code, "invalid_argument");
    }

    /// The session anchors to the terminal's CURRENT workspace (so the project is
    /// derivable, ADR-0010) — read live from the terminal row, not the hook payload.
    #[test]
    fn session_start_anchors_to_terminal_workspace() {
        let (d, app, _c) = seed_terminal_app();
        // A workspace + a terminal attached to it.
        let ws_path = std::env::temp_dir().join(format!("nyx-sess-ws-{}", uuid_like()));
        std::fs::create_dir_all(&ws_path).unwrap();
        let (tid, workspace_id) = app.state::<Db>().with_conn(|c| {
            let (_p, w) = db::create_project(c, "proj", &ws_path.to_string_lossy(), None)?;
            let t = db::create_terminal(c, "/work", None)?;
            db::attach_terminal(c, &t.id, &w.id, db::BINDING_AUTO)?;
            Ok::<_, diesel::result::Error>((t.id, w.id))
        }).unwrap();

        d.call(AGENT_SESSION_EVENT_TOOL, &json!({
            "hook_event_name": "SessionStart", "session_id": "sid-ws", "cwd": "/work", "NYX_TERMINAL_ID": tid,
        })).expect("start");

        let row = app.state::<Db>()
            .with_conn(|c| db::active_session_for(c, &tid, db::AGENT_KIND_CLAUDE_CODE)).unwrap().unwrap();
        assert_eq!(row.workspace_id.as_deref(), Some(workspace_id.as_str()),
            "session anchors to the terminal's workspace");
        // And the project is derivable via the workspace (no denormalized project_id).
        let project = app.state::<Db>().with_conn(|c| db::project_id_for_session(c, &row.id)).unwrap();
        assert!(project.is_some(), "project derivable from the session's workspace");
    }
}
