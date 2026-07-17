//! Shell-agnostic MCP **extension tools** — the DB-backed dispatch logic for every
//! advertised tool that is NOT one of the four runtime command tools (those live in
//! [`crate::mcp_runtime`]).
//!
//! This module is the SECOND half of lifting the Tauri `mcp_tools::NyxToolDispatcher`
//! into `nyx-core`, so BOTH shells route the FULL advertised surface through ONE
//! implementation over a [`Db`] (+ a [`CommandRunner<S>`] for the tools that read live
//! run state, + an optional [`TerminalHost`] for the live-PTY terminal tools). The
//! Tauri dispatcher held an `AppHandle<R>` and reached `tauri::State`; these free
//! functions take the `nyx-core` handles directly, so NO shell type crosses them (the
//! frozen "0 Tauri in nyx-core" rule).
//!
//! ## What is extracted here
//!
//! - **Pure DB reads** — `probe`, `list_projects`, `list_workspaces`, `list_commands`
//!   (the template/instance forms, with live run state overlaid from the runner),
//!   `list_importable_scripts`.
//! - **Command-template CRUD** — `add_command`, `update_command`, `import_commands`,
//!   `remove_command`, `remove_commands`, `remove_workspace`, `clear_command_output`.
//!   Each delegates to the SAME `db`/`pkgjson`/`command` helpers the UI bridge drives.
//! - **Agent-session channel** — `agent_session_event` (the Claude Code
//!   SessionStart/SessionEnd hook target), over the shared `agent`/`db` layer.
//! - **Interactive-terminal tools** — `create_terminal`, `send_to_terminal`,
//!   `list_terminals`, `close_terminal`, `read_terminal`. The DB-record half (create
//!   the record + auto-attach, list the records, read the persisted scrollback, flip a
//!   record closed) is shell-agnostic and lives here; the LIVE-PTY half (write into a
//!   terminal's shell, kill its PTY, the live `live`/`busy` bits) is delegated to the
//!   shell's [`TerminalHost`] — the PTY is owned by the shell (the Tauri `PtyManager`
//!   / the Electron core-host's Node PTY manager), never by `nyx-core`.
//!
//! ## Event seams
//!
//! A mutating tool returns a [`ToolEffect`] alongside its JSON result, naming the
//! coarse `changed` topic the shell should broadcast (`terminals` / `workspaces` /
//! `commands` / `agent-sessions`) so the front re-pulls — exactly the
//! `workspaces://changed` seam [`crate::mcp_runtime::dispatch_workspace_tool`] already
//! documents. A shell with a front fires the matching event AFTER a successful
//! dispatch; the core-host's renderer re-pulls on its own invalidations.

use serde_json::{json, Value};

use crate::agent::{AgentEvent, AgentRegistry};
use crate::agent_activity::{ActivityEvent, AgentActivityStore};
use crate::command::{CommandRunner, RunState, RunnerSink};
use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{
    internal_db, optional_bool, optional_str, optional_usize, require_str, status_json,
};

/// The default `tail_bytes` window for the output/scrollback reads — the token-safe
/// 12 KiB the Tauri dispatcher uses.
pub const DEFAULT_TAIL_BYTES: usize = 12 * 1024;
/// Hard ceiling on a single output/scrollback window: a request for more than 1 MiB is
/// refused with `output_too_large` rather than served.
pub const MAX_TAIL_BYTES: usize = 1024 * 1024;

/// The coarse `changed` topic a mutating tool asks the shell to broadcast so the front
/// re-pulls (the same closed set as `nyx_core::frontier::ChangedTopic`). Mirrors the
/// `*://changed` events the Tauri dispatcher emits after a successful mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangedTopic {
    Terminals,
    Workspaces,
    Commands,
    AgentSessions,
    /// The Spec domain (Specs / revisions / tasks / dependencies) — carried by the v2.4
    /// authoring + transition MCP tools so any open Specs board / PRD detail re-pulls after
    /// an agent mutation. The wire string is `specs`; the front's re-pull on it is wired in
    /// phase 6 (real-time UI), but the mutation authority already NAMES the topic here so the
    /// seam is single-authority end to end.
    Specs,
    /// A watched repository's Git state changed (v2.4 Q-015 — the real-time git
    /// watcher recomputed a derived view). Mirrors `frontier::ChangedTopic::Repos`.
    Repos,
}

impl ChangedTopic {
    /// The wire string the shell maps to its event (`terminals` / `workspaces` /
    /// `commands` / `agent-sessions` / `specs` / `repos`).
    pub fn as_str(self) -> &'static str {
        match self {
            ChangedTopic::Terminals => "terminals",
            ChangedTopic::Workspaces => "workspaces",
            ChangedTopic::Commands => "commands",
            ChangedTopic::AgentSessions => "agent-sessions",
            ChangedTopic::Specs => "specs",
            ChangedTopic::Repos => "repos",
        }
    }
}

/// A tool's JSON result plus the (optional) `changed` topic the shell should broadcast.
/// A read tool returns `effects: vec![]`; a mutating tool names the topic(s) it touched.
#[derive(Debug)]
pub struct ToolOutcome {
    pub result: Value,
    pub effects: Vec<ChangedTopic>,
}

impl ToolOutcome {
    pub(crate) fn read(result: Value) -> Self {
        Self {
            result,
            effects: vec![],
        }
    }
    pub(crate) fn changed(result: Value, topic: ChangedTopic) -> Self {
        Self {
            result,
            effects: vec![topic],
        }
    }
}

// --- The shell's live-PTY terminal capability ------------------------------

/// The LIVE-PTY half of the interactive-terminal tools the shell owns (the Tauri
/// `PtyManager` + `TerminalPtyMap` + `PendingTerminalCommands`; the Electron core-host's
/// Node PTY manager). `nyx-core` owns the terminal RECORDS (DB) but never the live PTY,
/// so it delegates the four PTY-touching operations to whatever the shell wires here.
///
/// All methods are best-effort/idempotent on an unknown id: the DB-record validation has
/// already run in the tool, so a `None`/`false` here means "no live shell for this
/// record" (the actionable `invalid_id`/`invalid_state` is built by the caller).
pub trait TerminalHost: Send + Sync {
    /// Park `command` to be injected into the terminal's shell once its PTY spawns (the
    /// SAME role as Tauri's `PendingTerminalCommands` + the Electron host's command park).
    /// Called by `create_terminal` for a terminal opened with a `command`.
    fn park_opening_command(&self, terminal_id: &str, command: &str);

    /// Write `command + "\r"` into the terminal's live shell (the SAME path as
    /// `pty_write`). Returns `Ok(true)` when written, `Ok(false)` when the terminal has
    /// no live PTY (unknown/closed/not-yet-spawned), or `Err` on a real write failure.
    fn send_to_terminal(&self, terminal_id: &str, command: &str) -> Result<bool, String>;

    /// Write RAW `bytes` into the terminal's live shell WITHOUT any appended newline (the
    /// `send_keys` path). The byte sequence the caller already resolved from literal text and
    /// named keys is delivered verbatim, so it can drive a raw-mode TUI (arrows, Esc, Ctrl+C,
    /// typing without submitting). Returns `Ok(true)` when written, `Ok(false)` when the
    /// terminal has no live PTY (unknown/closed/not-yet-spawned), or `Err` on a real write
    /// failure. Same liveness contract as [`Self::send_to_terminal`]; the ONLY difference is
    /// no forced `\r`.
    fn send_keys(&self, terminal_id: &str, bytes: &[u8]) -> Result<bool, String>;

    /// Kill the terminal's live PTY if one is registered (the SAME path as `pty_close`),
    /// and drop any parked opening command. Idempotent — a no-op when nothing is live.
    fn close_terminal_pty(&self, terminal_id: &str);

    /// The live `(live, busy)` bits for a terminal record: `live` = a PTY is registered
    /// (its shell started), `busy` = a command is running in the foreground (the OS dot
    /// authority), or `None` when it cannot be derived. A record with no live PTY → `(false,
    /// None)`. Used to enrich `list_terminals`.
    fn terminal_liveness(&self, terminal_id: &str) -> (bool, Option<bool>);
}

/// A [`TerminalHost`] that owns no live PTY — every terminal looks closed/idle and a
/// `send_to_terminal` finds no live shell. Lets a shell that has not wired its PTY
/// manager (or a test) still serve the terminal tools' DB half without a `None` branch:
/// the record-level operations work, and the live-PTY operations degrade to the same
/// `invalid_state` an unknown-but-not-yet-spawned terminal would give.
pub struct NoTerminalHost;

impl TerminalHost for NoTerminalHost {
    fn park_opening_command(&self, _terminal_id: &str, _command: &str) {}
    fn send_to_terminal(&self, _terminal_id: &str, _command: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn send_keys(&self, _terminal_id: &str, _bytes: &[u8]) -> Result<bool, String> {
        Ok(false)
    }
    fn close_terminal_pty(&self, _terminal_id: &str) {}
    fn terminal_liveness(&self, _terminal_id: &str) -> (bool, Option<bool>) {
        (false, None)
    }
}

// --- Argument helpers specific to this module ------------------------------

/// An OPTIONAL array-of-strings argument as a de-duplicating set (`names`). Absent/null
/// → `None`; non-array / non-string element → `invalid_argument`.
fn optional_str_set(
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
                        ))
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

/// A REQUIRED array-of-strings argument, ORDER-preserving (`command_ids`). Missing /
/// not-an-array / non-string element → `invalid_argument`. An empty array is accepted (a
/// no-op batch). Empty-string ids are rejected.
fn require_str_vec(args: &Value, key: &str) -> Result<Vec<String>, RpcError> {
    match args.get(key) {
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, v) in arr.iter().enumerate() {
                match v.as_str() {
                    Some(s) if !s.is_empty() => out.push(s.to_string()),
                    Some(_) => {
                        return Err(RpcError::new(
                            RpcCode::InvalidArgument,
                            format!("argument '{key}[{i}]' must be a non-empty string"),
                        ))
                    }
                    None => {
                        return Err(RpcError::new(
                            RpcCode::InvalidArgument,
                            format!("argument '{key}[{i}]' must be a string"),
                        ))
                    }
                }
            }
            Ok(out)
        }
        _ => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("missing or invalid required array argument '{key}'"),
        )),
    }
}

/// The shared `output_too_large` error: structured `requested`/`limit` (bytes) so an
/// agent can retry a smaller window without parsing the prose. Parity with the Tauri
/// `output_too_large_error`.
fn output_too_large_error(tail_bytes: usize, ceiling: usize) -> RpcError {
    let requested = tail_bytes.max(ceiling);
    RpcError::new(
        RpcCode::OutputTooLarge,
        format!("requested window exceeds max_bytes ({MAX_TAIL_BYTES})"),
    )
    .with_data(json!({ "requested": requested, "limit": MAX_TAIL_BYTES }))
}

/// Parse + validate the windowing knobs for the scrollback read (`tail_bytes` default
/// [`DEFAULT_TAIL_BYTES`], `max_bytes` ceiling, `since`, `strip_ansi` default `true`),
/// refusing a window above [`MAX_TAIL_BYTES`] with `output_too_large`. Returns
/// `(effective_tail, since, strip)`.
fn parse_window_knobs(args: &Value) -> Result<(usize, Option<usize>, bool), RpcError> {
    let tail_bytes = optional_usize(args, "tail_bytes")?.unwrap_or(DEFAULT_TAIL_BYTES);
    let since = optional_usize(args, "since")?;
    let max_bytes = optional_usize(args, "max_bytes")?;
    let strip = optional_bool(args, "strip_ansi")?.unwrap_or(true);
    let ceiling = max_bytes.unwrap_or(MAX_TAIL_BYTES);
    if tail_bytes > MAX_TAIL_BYTES || ceiling > MAX_TAIL_BYTES {
        return Err(output_too_large_error(tail_bytes, ceiling));
    }
    Ok((tail_bytes.min(ceiling), since, strip))
}

// --- Error builders (parity with the Tauri dispatcher) ---------------------

/// The actionable `invalid_id` for an id that is NOT a known TEMPLATE: if it turns out to
/// be a launchable instance, NAME the correct path; else the generic unknown-template
/// error. Parity with the Tauri `bad_command_id_error`.
fn bad_command_id_error(db: &Db, id: &str) -> RpcError {
    let is_instance = db
        .with_conn(|c| db::get_instance(c, id))
        .ok()
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

/// The actionable `invalid_id` for an unknown / non-open terminal id (parity with the
/// Tauri `bad_terminal_id_error`).
fn bad_terminal_id_error(terminal_id: &str) -> RpcError {
    RpcError::new(
        RpcCode::InvalidId,
        format!("unknown or closed terminal {terminal_id} (use a terminal_id from list_terminals)"),
    )
}

/// Map a `db::create_template` failure to the D8 vocabulary (parity with the Tauri
/// `map_template_write_err`): UNIQUE → `invalid_state` (name taken), FK → `invalid_id`
/// (unknown project), else `internal`.
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

/// Map an `update_command` write failure (keyed by `command_id`, no project in hand) to
/// the D8 vocabulary (parity with the Tauri `map_template_write_err_generic`).
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

// --- JSON views (parity with the Tauri dispatcher) -------------------------

/// The JSON view of a command TEMPLATE (parity with the Tauri `template_json`).
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

/// The JSON view of ONE discoverable script (parity with the Tauri `preview_script_json`).
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

// --- Pure DB read tools ----------------------------------------------------

/// `probe` — `{}` → `{ ok, server, version, build_sha, build_dirty, schema_ok, … }`. The
/// trivial liveness tool: it touches NO runtime, only the DB schema health (best-effort),
/// so it answers even before the runtime is warm. Parity with the Tauri `probe`.
pub fn probe(db: &Db) -> Result<Value, RpcError> {
    let health = db.with_conn(db::schema_health);
    let schema_ok = health.up_to_date;
    let mut result = json!({
        "ok": true,
        "server": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "schema_ok": schema_ok,
    });
    if !schema_ok {
        if let Some(map) = result.as_object_mut() {
            map.insert(
                "schema_warning".to_string(),
                json!("schema has pending migrations — restart nyx to apply them"),
            );
            match health.pending_count {
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

/// `list_projects` — `{}` → `{ projects }`. Routes through the SHARED
/// [`crate::usecase::list_projects`] (v2.1 task #8) — the SAME identity use case the UI
/// bridge's `list_projects` db-command calls — so the read surface is aligned on the
/// authority too, not just the mutations.
pub fn list_projects(db: &Db) -> Result<Value, RpcError> {
    let projects = db
        .with_conn(crate::usecase::list_projects)
        .map_err(internal_db)?;
    Ok(json!({ "projects": projects }))
}

/// `list_workspaces` — `{ project_id, cwd? }` → `{ workspaces }`. `cwd` is the OPTIONAL
/// filter; each returned workspace's `branch` is resolved LIVE. Routes the base list
/// through the SHARED [`crate::usecase::list_workspaces`] (v2.1 task #8) — the SAME use
/// case the UI bridge drives — then applies the MCP-only `cwd` filter + live-branch overlay.
pub fn list_workspaces(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let cwd = optional_str(args, "cwd")?;
    let mut workspaces = db
        .with_conn(|c| crate::usecase::list_workspaces(c, project_id))
        .map_err(internal_db)?;
    if let Some(cwd) = cwd {
        let needle = crate::pathnorm::normalize(cwd);
        workspaces.retain(|w| path_matches(&w.root_path, &needle));
    }
    for w in &mut workspaces {
        w.branch = db::detect_branch(&w.root_path);
    }
    Ok(json!({ "workspaces": workspaces }))
}

/// `list_commands` — `{ workspace_id }` (instances, the NOMINAL form, with live run
/// state overlaid from the runner) OR `{ project_id }` (templates) → `{ commands }`.
/// THIS is the routing the Electron dispatcher got wrong (it returned terminals). Parity
/// with the Tauri `list_commands`.
pub fn list_commands<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    if let Some(workspace_id) = optional_str(args, "workspace_id")? {
        let rows = db
            .with_conn(|c| db::list_instances_for_workspace(c, workspace_id))
            .map_err(internal_db)?;
        let commands: Vec<Value> = rows
            .into_iter()
            .map(|row| {
                // The FACTUAL outcome: the live runner state when it backs the instance
                // this session, else the PERSISTED outcome from the row.
                let status = match runner.outcome(&row.id) {
                    Some((state, exit_code, unread)) => status_json(state, exit_code, unread),
                    None => status_json(
                        RunState::from_db_str(&row.last_state),
                        row.last_exit_code,
                        row.unread,
                    ),
                };
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
                    "last_state": state_str,
                    "cwd": cwd,
                    "source_kind": row.source_kind,
                    "package_manager": row.package_manager,
                });
                if let (Some(map), Some(status_map)) = (entry.as_object_mut(), status.as_object()) {
                    for (k, v) in status_map {
                        map.insert(k.clone(), v.clone());
                    }
                }
                entry
            })
            .collect();
        return Ok(json!({ "commands": commands }));
    }
    if let Some(project_id) = optional_str(args, "project_id")? {
        let templates = db
            .with_conn(|c| db::list_templates(c, project_id))
            .map_err(internal_db)?;
        let commands: Vec<Value> = templates.iter().map(template_json).collect();
        return Ok(json!({ "commands": commands }));
    }
    Err(RpcError::new(
        RpcCode::InvalidArgument,
        "list_commands requires either workspace_id (instances) or project_id (templates)",
    ))
}

/// `workspace_runtime` — `{ workspace_id }` → `{ runtime }`. The v2.2 cockpit read model
/// (task `01KWHPV32YVCRBNY1JW5CDY4AY`): the LIVE runtime aggregate of ONE workspace —
/// its terminals (with the agent sessions on them), its cross-terminal agent sessions,
/// and its managed-command SERVICES with their current run state. Routes through the
/// SHARED [`crate::workspace_runtime::snapshot`] — the SAME read model the UI bridge's
/// `workspace_runtime_snapshot` db-command calls (PDR-0005: one authority, UI + MCP) —
/// overlaying each service's FACTUAL run state from THIS dispatcher's live `runner`
/// (`runner.outcome`), exactly as `list_commands` does. An unknown workspace id is a
/// D8 `invalid_id` error.
pub fn workspace_runtime<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let overlay = |instance_id: &str| runner.outcome(instance_id);
    let snapshot = db
        .with_conn(|c| crate::workspace_runtime::snapshot(c, workspace_id, &overlay))
        .map_err(internal_db)?;
    match snapshot {
        Some(runtime) => Ok(json!({ "runtime": runtime })),
        None => Err(RpcError::new(
            RpcCode::InvalidId,
            format!(
                "unknown workspace {workspace_id} (use a workspace_id from \
                 list_workspaces(project_id=…))"
            ),
        )),
    }
}

/// `workspace_git` — `{ workspace_id }` → `{ git }`. The v2.2 Git CHANGES read model
/// (task `01KWHPV33EGTB3Z41J0RQR1BAK`, PDR-0013): per NON-OPTIONAL repository of the
/// workspace, the current branch, dirty flag, and changed-file SUMMARY — RE-READ from
/// Git, never authoritative in the DB. Routes through the SHARED
/// [`crate::workspace_git::summary`] — the SAME read model the UI bridge's
/// `workspace_git_summary` db-command calls. NOT a line-level diff (v2.3). The only DB
/// read resolves the workspace + derives its repos; every Git fact is transient. An
/// unknown workspace id is a D8 `invalid_id` error.
pub fn workspace_git(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let summary = db
        .with_conn(|c| crate::workspace_git::summary(c, workspace_id))
        .map_err(internal_db)?;
    match summary {
        Some(git) => Ok(json!({ "git": git })),
        None => Err(RpcError::new(
            RpcCode::InvalidId,
            format!(
                "unknown workspace {workspace_id} (use a workspace_id from \
                 list_workspaces(project_id=…))"
            ),
        )),
    }
}

/// `workspace_timeline` — `{ workspace_id, limit? }` → `{ timeline }`. The v2.2 workspace
/// TIMELINE read model (task `01KWHPV33SAV9NV1STSY403CF0`): the workspace's most recent
/// `domain_events` (v2.1) as a BOUNDED, oldest-first activity fil — the business
/// "changements notables" (workspace / repository / project lifecycle), NEVER a terminal
/// byte (the outbox carries none by construction). Routes through the SHARED
/// [`crate::workspace_timeline::timeline`] — the SAME read model the UI bridge's
/// `workspace_timeline` db-command calls (PDR-0005). `limit` bounds the read (omitted / 0 →
/// the default ceiling). NOT the full contextual activity timeline (v2.5). An unknown
/// workspace id is a D8 `invalid_id` error.
pub fn workspace_timeline(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    // Bound the read: an omitted / 0 limit lets the read model apply its default ceiling.
    let limit = optional_usize(args, "limit")?.unwrap_or(0) as i64;
    let timeline = db
        .with_conn(|c| crate::workspace_timeline::timeline(c, workspace_id, limit))
        .map_err(internal_db)?;
    match timeline {
        Some(timeline) => Ok(json!({ "timeline": timeline })),
        None => Err(RpcError::new(
            RpcCode::InvalidId,
            format!(
                "unknown workspace {workspace_id} (use a workspace_id from \
                 list_workspaces(project_id=…))"
            ),
        )),
    }
}

/// `repo_diff` — `{ workspace_id, repository_id, kind, rev?, base?, target?, mode? }` →
/// `{ diff }`. The v2.4 GIT DIFF read model (task `01KX0F6FD836YASVYT17YV3FTW`, PDR-0007
/// / PDR-0015): the line-level diff of ONE repository within a workspace, for one of the
/// review surfaces (working_tree / staged / unstaged / untracked / commit / range /
/// branches), in LIVING or PINNED mode. Routes through the SHARED
/// [`crate::git_diff::diff_repo_with_mode`] — the SAME engine the UI bridge's `repo_diff`
/// db-command drives (PDR-0005: one authority for UI + MCP). gix is the primary backend;
/// the `git` CLI is a coherent fallback. The `(workspace_id, repository_id)` pair resolves
/// to exactly ONE repo's absolute path via [`crate::repository::member_absolute_path`], so
/// the diff never mixes Git identities across repos. Nothing is persisted (PDR-0013).
pub fn repo_diff(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let repository_id = require_str(args, "repository_id")?;
    let kind = require_str(args, "kind")?;
    let rev = optional_str(args, "rev")?;
    let base = optional_str(args, "base")?;
    let target = optional_str(args, "target")?;
    let mode = optional_str(args, "mode")?;

    let diff_target = crate::git_diff::target_from_params(kind, rev, base, target)
        .map_err(|m| RpcError::new(RpcCode::InvalidArgument, m))?;
    let mode_override = crate::git_diff::mode_from_str(mode)
        .map_err(|m| RpcError::new(RpcCode::InvalidArgument, m))?;

    // Resolve the ONE repository's absolute path (multi-repo identity isolation).
    let absolute_path = db
        .with_conn(|c| crate::repository::member_absolute_path(c, workspace_id, repository_id))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!(
                    "no repository {repository_id} in workspace {workspace_id} (use a \
                     repository_id from workspace_git(workspace_id=…))"
                ),
            )
        })?;

    let effective_mode = mode_override.unwrap_or_else(|| diff_target.default_mode());
    let diff = crate::git_diff::diff_repo_with_mode(&absolute_path, &diff_target, effective_mode)
        .map_err(|e| RpcError::new(RpcCode::InvalidArgument, e.to_string()))?;
    Ok(json!({ "diff": diff }))
}

/// `repo_git_view` — `{ workspace_id, repository_id, base?, target? }` → `{ view }`. The
/// v2.4 DERIVED GIT VIEWS read model (task `01KX0F6FXE6AJ9BR4BYHRX6B1R`, Q-015 /
/// PDR-0013): the per-repo dirty state (staged / modified / untracked), current branch +
/// base/target, ahead/behind, and a change summary — RE-READ from Git on demand, NEVER
/// persisted. Routes through the SHARED [`crate::git_views::repo_view_with_target`] — the
/// SAME derivation the UI bridge's `repo_git_view` db-command drives (PDR-0005: one
/// authority for UI + MCP). The `(workspace_id, repository_id)` pair resolves to exactly
/// ONE repo's absolute path (multi-repo identity isolation). `base` is an OPTIONAL review
/// endpoint (a local branch or SHA — no remote assumed); `target` optionally pins the
/// compared endpoint (defaults to the current branch).
pub fn repo_git_view(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let repository_id = require_str(args, "repository_id")?;
    let base = optional_str(args, "base")?;
    let target = optional_str(args, "target")?;

    let absolute_path = db
        .with_conn(|c| crate::repository::member_absolute_path(c, workspace_id, repository_id))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!(
                    "no repository {repository_id} in workspace {workspace_id} (use a \
                     repository_id from workspace_git(workspace_id=…))"
                ),
            )
        })?;

    let view = crate::git_views::repo_view_with_target(&absolute_path, base, target);
    Ok(json!({ "view": view }))
}

/// `review_comment_list` — `{ workspace_id, repository_id, spec_id? }` → `{ comments }`. The v2.4
/// review COMMENT feed read model (task `01KX0F6HEBPK34CV49EREHK3Y9`, PDR-0012/0015): the whole
/// auto-threaded set of a `(workspace, repository)` review — roots + replies, anchored + global,
/// with each comment's `outdated`/`status`. Routes through the SHARED
/// [`crate::db::list_review_comments`] — the SAME model the UI bridge's `review_comment_list`
/// db-command reads (PDR-0005). Optional `spec_id` SCOPES the feed to one Spec (Finding 41); the
/// agent reviewing a Spec passes it, an unscoped call omits it and gets the whole repo feed.
/// Read-only.
pub fn review_comment_list(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let repository_id = require_str(args, "repository_id")?;
    let spec_id = optional_str(args, "spec_id")?;
    let comments = db
        .with_conn(|c| crate::db::list_review_comments(c, workspace_id, repository_id, spec_id))
        .map_err(internal_db)?;
    Ok(json!({ "comments": comments }))
}

/// `review_comment_post` — `{ workspace_id, repository_id, body, spec_id?, parent_id?,
/// file_path?, side?, line_start?, line_end?, blob?, snippet?, context? }` → `{ comment }`. The
/// v2.4 AGENT façade to annotate a review (task `01KX0F6HEBPK34CV49EREHK3Y9`). The author is
/// ALWAYS `agent` — the human posts through the UI. The agent may leave a global/anchored
/// comment OR a REPLY (`parent_id`), but there is DELIBERATELY no resolve tool: the agent NEVER
/// resolves (resolution is human-only). Routes through the SHARED
/// [`crate::db::insert_review_comment`] (PDR-0005). An anchor is captured when a `file_path` is
/// given; `line_end` defaults to `line_start`. Carries a `Repos` changed effect so an open
/// review UI re-pulls the comment feed live (the agent's reply appears without a reload).
pub fn review_comment_post(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let repository_id = require_str(args, "repository_id")?;
    let body = require_str(args, "body")?;
    let spec_id = optional_str(args, "spec_id")?;
    let parent_id = optional_str(args, "parent_id")?;
    let file_path = optional_str(args, "file_path")?;
    let side = optional_str(args, "side")?;
    let line_start = optional_usize(args, "line_start")?;
    let line_end = optional_usize(args, "line_end")?;
    let blob = optional_str(args, "blob")?;
    let snippet = optional_str(args, "snippet")?;
    let context = optional_str(args, "context")?;

    // An anchor is present iff a file_path is given. Default side = new; line defaults to 1.
    let anchor = file_path.map(|fp| {
        let start = line_start.unwrap_or(1) as i32;
        let end = line_end.map(|e| e as i32).unwrap_or(start).max(start);
        crate::db::CommentAnchor {
            file_path: fp.to_string(),
            side: side.unwrap_or(crate::db::SIDE_NEW).to_string(),
            line_start: start,
            line_end: end,
            blob: blob.map(|b| b.to_string()),
            snippet: snippet.map(|s| s.to_string()),
            context: context.map(|c| c.to_string()),
        }
    });

    let comment = db
        .with_conn(|c| {
            crate::db::insert_review_comment(
                c,
                crate::db::NewComment {
                    workspace_id: workspace_id.to_string(),
                    repository_id: repository_id.to_string(),
                    spec_id: spec_id.map(|s| s.to_string()),
                    parent_id: parent_id.map(|p| p.to_string()),
                    // The MCP façade ALWAYS writes as the agent (the human posts via the UI).
                    author_type: crate::db::AUTHOR_AGENT.to_string(),
                    author_name: "nyx-agent".to_string(),
                    anchor,
                    body: body.to_string(),
                },
            )
        })
        .map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({ "comment": comment }),
        ChangedTopic::Repos,
    ))
}

/// `review_task_list` — `{ workspace_id, repository_id }` → `{ tasks }`. The v2.4 review
/// TASK (finding = task) read model (task `01KX0F6JEKCEV2JCTMEBMM3P4Z`, PDR-0012 — finding = task):
/// the promoted-task list of a `(workspace, repository)` review, each with its `from_comment_id`,
/// `title`, optional `severity`, and `grill_state` (proposed/structured/validated). Routes through
/// the SHARED [`crate::db::list_review_tasks`] — the SAME model the UI bridge reads (PDR-0005).
/// Read-only. The agent façade reads the whole repo's task list UNSCOPED (`spec_id = None`), like
/// its unscoped `review_comment_list` — the per-Spec scoping (finding #53) is the UI façade's read.
pub fn review_task_list(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let repository_id = require_str(args, "repository_id")?;
    let tasks = db
        .with_conn(|c| crate::db::list_review_tasks(c, workspace_id, repository_id, None))
        .map_err(internal_db)?;
    Ok(json!({ "tasks": tasks }))
}

/// `review_task_structure` — `{ id, title, details?, severity? }` → `{ task }`. The AGENT's
/// step of the promotion "grill" (task `01KX0F6JEKCEV2JCTMEBMM3P4Z`): the human lâche brut (a
/// `proposed` task), the agent STRUCTURES it into a clean coder task (title + details + optional
/// severity) and it moves to `structured`. The agent may structure but NEVER validate — there is
/// DELIBERATELY no validate tool here (validation is human-only, symmetric to phase-4 human-only
/// comment resolution). Routes through the SHARED [`crate::db::structure_task`] (PDR-0005),
/// which also refuses to reopen an already-VALIDATED task. Carries a `Repos` changed effect so
/// an open review UI re-pulls the task list + unread badge live. `invalid_id` when the task
/// is unknown OR already validated (nothing to structure).
pub fn review_task_structure(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let id = require_str(args, "id")?;
    let title = require_str(args, "title")?;
    let details = optional_str(args, "details")?;
    // `severity` present (even null) → set it; absent → leave unchanged. The MCP arg carries a
    // string when set, so a present key means "set to this severity".
    let severity = match args.get("severity") {
        Some(Value::Null) | None => None,
        Some(_) => Some(optional_str(args, "severity")?),
    };
    let task = db
        .with_conn(|c| crate::db::structure_task(c, id, title, details, severity))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!("no structurable task {id} (unknown or already validated)"),
            )
        })?;
    Ok(ToolOutcome::changed(
        json!({ "task": task }),
        ChangedTopic::Repos,
    ))
}

/// Whether a workspace `path` matches a normalized `cwd` filter (parity with the Tauri
/// `path_matches`).
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

// --- Command-template CRUD tools -------------------------------------------

/// `add_command` — `{ project_id, name, command, subfolder? }` → `{ command }`. Parity
/// with the Tauri `add_command` (reuses `pkgjson::infer_command_source` + `db::create_template`).
pub fn add_command(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let name = require_str(args, "name")?;
    let command = require_str(args, "command")?;
    let subfolder = optional_str(args, "subfolder")?;
    let (source_kind, package_manager) = crate::pkgjson::infer_command_source(command, None, None);
    let source = db::CommandSource {
        source_kind,
        source_package_json_path: None,
        source_script_name: None,
        source_script_command_snapshot: None,
        package_manager,
    };
    let template = db
        .with_conn(|c| db::create_template(c, project_id, name, command, subfolder, source))
        .map_err(|e| map_template_write_err(project_id, e))?;
    Ok(ToolOutcome::changed(
        json!({ "command": template_json(&template) }),
        ChangedTopic::Commands,
    ))
}

/// `set_project_settings` — `{ project_id, name?, resume_agent_sessions? }` → `{ project }`.
/// Modify an EXISTING project's mutable settings (rename + the PRD-5 #5 resume opt-in),
/// the MCP mirror of the UI's project-settings modal. It reuses the SAME `db::update_project`
/// / `db::set_project_resume_agent_sessions` writes the UI bridge drives, so both surfaces
/// converge on one code path. Partial update: only the supplied fields change. Returns the
/// project's post-write state and a `workspaces` changed effect so any open front re-pulls
/// (the agent-driven mutation the UI never invoked is reflected without a manual reload).
pub fn set_project_settings(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let new_name = optional_str(args, "name")?;
    let new_resume = optional_bool(args, "resume_agent_sessions")?;

    // Validate the id up front so an unknown project yields a clean `invalid_id` rather
    // than a silent zero-row no-op (the writes return rows-updated, not Err, on a bad id).
    if db
        .with_conn(|c| db::get_project(c, project_id))
        .map_err(internal_db)?
        .is_none()
    {
        return Err(RpcError::new(
            RpcCode::InvalidId,
            format!("unknown project {project_id}"),
        ));
    }

    // A rename to an empty/blank name is refused (parity with the UI's non-empty + trim
    // validation) so the project never loses its label.
    if let Some(name) = new_name {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                "name must not be blank",
            ));
        }
        db.with_conn(|c| db::update_project(c, project_id, trimmed))
            .map_err(internal_db)?;
    }

    if let Some(resume) = new_resume {
        db.with_conn(|c| db::set_project_resume_agent_sessions(c, project_id, resume))
            .map_err(internal_db)?;
    }

    // Re-read so the result reflects the persisted state (and any unchanged fields).
    let project = db
        .with_conn(|c| db::get_project(c, project_id))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(RpcCode::InvalidId, format!("unknown project {project_id}"))
        })?;
    Ok(ToolOutcome::changed(
        json!({ "project": project }),
        ChangedTopic::Workspaces,
    ))
}

/// Parse the REQUIRED `repos` argument of `project_set_repos`: an array of
/// `{ relative_path, name? }` objects into [`db::RepoConfig`]s. Missing/not-an-array →
/// `invalid_argument`; an element that is not an object, or lacks a non-empty
/// `relative_path`, is rejected. An empty array is accepted (resets to the fallback).
fn parse_repo_configs(args: &Value) -> Result<Vec<db::RepoConfig>, RpcError> {
    let arr = match args.get("repos") {
        Some(Value::Array(arr)) => arr,
        _ => {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                "missing or invalid required array argument 'repos'",
            ))
        }
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let obj = v.as_object().ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidArgument,
                format!("argument 'repos[{i}]' must be an object {{ relative_path, name? }}"),
            )
        })?;
        let relative_path = obj
            .get("relative_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                RpcError::new(
                    RpcCode::InvalidArgument,
                    format!("argument 'repos[{i}].relative_path' must be a non-empty string"),
                )
            })?
            .to_string();
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        out.push(db::RepoConfig {
            relative_path,
            name,
        });
    }
    Ok(out)
}

/// `project_set_repos` — `{ project_id, repos: [{ relative_path, name? }] }` → `{ repos }`.
/// Finding #27 (PDR-0013): CONFIGURE a project's repositories EXPLICITLY — the association
/// that replaces the removed disk auto-detection. A full RECONCILE: the listed repos become
/// the project's active logical identities (keyed on `(project_id, relative_path)`), and a
/// repo previously configured but no longer listed is archived. Every workspace/worktree of
/// the project then DERIVES these repos at `root_path + relative_path` (so
/// workspace_git/repo_diff/review see each sub-repo). Routes through the SHARED
/// [`crate::db::set_project_repositories`] — the single write path (PDR-0005). Mutating → a
/// `repos` changed effect so any open review/cockpit re-pulls. NEVER scans the disk. An
/// unknown project id is a D8 `invalid_id` error.
pub fn project_set_repos(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let repos = parse_repo_configs(args)?;

    // Validate the id up front so an unknown project yields a clean `invalid_id` rather than
    // a silent config on nothing.
    if db
        .with_conn(|c| db::get_project(c, project_id))
        .map_err(internal_db)?
        .is_none()
    {
        return Err(RpcError::new(
            RpcCode::InvalidId,
            format!("unknown project {project_id}"),
        ));
    }

    let configured = db
        .with_conn(|c| db::set_project_repositories(c, project_id, &repos))
        .map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({ "repos": configured }),
        ChangedTopic::Repos,
    ))
}

/// `update_command` — `{ command_id, name?, command?, subfolder? }` → `{ command }`.
/// Partial update; the package.json source-detach rule runs; refused while any instance
/// is running. Parity with the Tauri `update_command`.
pub fn update_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let command_id = require_str(args, "command_id")?;
    let new_name = optional_str(args, "name")?;
    let new_command = optional_str(args, "command")?;
    let subfolder_present = args.get("subfolder").map(|v| !v.is_null()).unwrap_or(false);
    let new_subfolder = optional_str(args, "subfolder")?;

    assert_template_not_running(db, runner, command_id)?;

    let updated = db
        .with_conn(
            |c| -> Result<Option<db::ManagedCommand>, diesel::result::Error> {
                let Some(current) = db::get_template(c, command_id)? else {
                    return Ok(None);
                };
                let name = new_name.unwrap_or(current.name.as_str());
                let command = new_command.unwrap_or(current.command.as_str());
                let subfolder: Option<&str> = if subfolder_present {
                    new_subfolder
                } else {
                    current.subfolder.as_deref()
                };
                let detach = current.source_script_name.is_some()
                    && crate::pkgjson::command_detaches_source(&current, command);
                db::update_template(c, command_id, name, command, subfolder)?;
                if detach {
                    db::set_template_source(c, command_id, db::CommandSource::default())?;
                }
                db::get_template(c, command_id)
            },
        )
        .map_err(map_template_write_err_generic)?;
    match updated {
        Some(template) => Ok(ToolOutcome::changed(
            json!({ "command": template_json(&template) }),
            ChangedTopic::Commands,
        )),
        None => Err(bad_command_id_error(db, command_id)),
    }
}

/// Refuse a template edit while any of its instances is running (parity with the Tauri
/// `assert_template_not_running`).
fn assert_template_not_running<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    command_id: &str,
) -> Result<(), RpcError> {
    let instance_ids = db
        .with_conn(|c| db::instance_ids_for_template(c, command_id))
        .map_err(internal_db)?;
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

/// Resolve the import target `(project_id, [workspace paths])` + run the discovery
/// (parity with the Tauri `discover_importable`).
fn discover_importable(
    db: &Db,
    args: &Value,
) -> Result<(String, Vec<crate::pkgjson::DiscoveredScript>, usize), RpcError> {
    let (project_id, workspace_paths): (String, Vec<String>) = match (
        optional_str(args, "workspace_id")?,
        optional_str(args, "project_id")?,
    ) {
        (Some(workspace_id), _) => {
            let ws = db
                .with_conn(|c| db::get_workspace(c, workspace_id))
                .map_err(internal_db)?
                .ok_or_else(|| {
                    RpcError::new(
                        RpcCode::InvalidId,
                        format!("unknown workspace {workspace_id}"),
                    )
                })?;
            (ws.project_id, vec![ws.root_path])
        }
        (None, Some(project_id)) => {
            let workspaces = db
                .with_conn(|c| db::list_workspaces(c, project_id))
                .map_err(internal_db)?;
            if workspaces.is_empty() {
                return Err(RpcError::new(
                    RpcCode::InvalidId,
                    format!("unknown project {project_id} (or it has no workspaces to scan)"),
                ));
            }
            let paths = workspaces.into_iter().map(|w| w.root_path).collect();
            (project_id.to_string(), paths)
        }
        (None, None) => return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "import_commands requires project_id (scan all workspaces) or workspace_id (scan one)",
        )),
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

/// `list_importable_scripts` — `{ project_id? | workspace_id? }` → `{ scripts,
/// manifests_found }`. Read-only import preview. Parity with the Tauri tool.
pub fn list_importable_scripts(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let (_project_id, scripts, manifests_found) = discover_importable(db, args)?;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let listed: Vec<Value> = scripts
        .iter()
        .filter(|s| seen.insert(s.proposed_name.as_str()))
        .map(preview_script_json)
        .collect();
    Ok(json!({ "scripts": listed, "manifests_found": manifests_found }))
}

/// `import_commands` — `{ project_id? | workspace_id?, names?, preview? }` → `{ imported,
/// would_import, skipped, manifests_found, preview }`. Parity with the Tauri `import_commands`.
pub fn import_commands(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let name_filter = optional_str_set(args, "names")?;
    let preview = optional_bool(args, "preview")?.unwrap_or(false);
    let (project_id, scripts, manifests_found) = discover_importable(db, args)?;

    let mut matched_requests: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut imported: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for script in &scripts {
        if !seen_names.insert(script.proposed_name.clone()) {
            continue;
        }
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
                continue;
            }
        }
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

    if let Some(ref filter) = name_filter {
        let mut not_found: Vec<&String> = filter
            .iter()
            .filter(|n| !matched_requests.contains(*n))
            .collect();
        not_found.sort();
        for name in not_found {
            skipped.push(json!({ "name": name, "reason": "not_found" }));
        }
    }
    if manifests_found == 0 {
        skipped.push(json!({ "reason": "no_manifest" }));
    }

    let effects = if !preview && !imported.is_empty() {
        vec![ChangedTopic::Commands]
    } else {
        vec![]
    };
    let (imported_rows, would_import_rows) = if preview {
        (json!([]), json!(imported))
    } else {
        (json!(imported), json!([]))
    };
    Ok(ToolOutcome {
        result: json!({
            "imported": imported_rows,
            "would_import": would_import_rows,
            "skipped": skipped,
            "manifests_found": manifests_found,
            "preview": preview,
        }),
        effects,
    })
}

/// Kill a workspace's LIVE runtime processes as part of the destructive `remove_workspace`
/// cascade: STOP (tree-kill) every running managed-command instance, and CLOSE the live PTY
/// (kill the interactive shell) of every terminal record. Both operations are IDEMPOTENT —
/// a non-running instance / a terminal with no live PTY is a no-op — so this is safe to call
/// with the full membership lists (the DB rows) without pre-filtering to only the live ones.
/// SHARED by the UI bridge and the MCP route so the "kill the vivants" behaviour is defined
/// once. Runs BEFORE the DB delete (the runner/host are the live-process authorities; the DB
/// rows only record membership).
pub fn kill_workspace_runtime<S: RunnerSink>(
    runner: &CommandRunner<S>,
    host: &dyn TerminalHost,
    instance_ids: &[String],
    terminal_ids: &[String],
) {
    for id in instance_ids {
        // Best-effort tree-kill; a stop failure must not abort the removal (the user asked to
        // clear everything). `stop` is a no-op on an already-idle instance.
        let _ = runner.stop(id);
    }
    for id in terminal_ids {
        // Kill the interactive shell if a PTY is live; no-op otherwise.
        host.close_terminal_pty(id);
    }
}

/// `remove_workspace` — `{ workspace_id }` → `{ removed, removed_instances }`. Refused on
/// the root workspace. DESTRUCTIVE: any LIVE process in the workspace (running command PTY
/// trees + interactive terminal shells) is KILLED, then the workspace and its full runtime
/// (command instances + terminals + agent sessions) are deleted. Parity with the Tauri
/// `remove_workspace`, updated for review 01KWEX11 ("Tout supprimer + tuer les vivants").
pub fn remove_workspace<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    host: &dyn TerminalHost,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    // RUNTIME kill owned by this adapter: STOP every running command instance + CLOSE every
    // interactive terminal PTY of the workspace BEFORE the delete. The runner + PTY host are
    // the live-process authorities (not the DB), unreachable from the nyx-core use case's
    // `&mut SqliteConnection`, so the kill runs HERE — the same split as the FS precondition
    // of `create_workspace`. This REPLACES the former "refuse while a command runs" guard
    // (per the review decision, the user wants everything cleared, not to be blocked). The
    // `instance_ids` len also feeds the `removed_instances` count the MCP contract returns.
    let instance_ids = db
        .with_conn(|c| db::instance_ids_for_workspace(c, workspace_id))
        .map_err(internal_db)?;
    let terminal_ids = db
        .with_conn(|c| db::terminal_ids_for_workspace(c, workspace_id))
        .map_err(internal_db)?;
    kill_workspace_runtime(runner, host, &instance_ids, &terminal_ids);
    let removed_instances = instance_ids.len();
    // The DB-expressible business decision (root invariant + delete + zero-row + the
    // `workspace.removed` domain event) is the SINGLE `usecase::remove_workspace`, shared with
    // the UI bridge — no invariant is duplicated here.
    db.with_conn(|c| {
        crate::usecase::remove_workspace(
            c,
            &crate::usecase::NullEventSink,
            workspace_id,
            crate::usecase::DomainEventSource::Mcp,
        )
    })
    .map_err(|e| match e {
        crate::usecase::RemoveWorkspaceError::NotFound => RpcError::new(
            RpcCode::InvalidId,
            format!("unknown workspace {workspace_id}"),
        ),
        crate::usecase::RemoveWorkspaceError::IsRoot => RpcError::new(
            RpcCode::InvalidState,
            format!(
                "workspace {workspace_id} is the project's root — it cannot be removed on \
                 its own; delete the whole project instead"
            ),
        ),
        crate::usecase::RemoveWorkspaceError::Db(e) => internal_db(e),
    })?;
    Ok(ToolOutcome::changed(
        json!({ "removed": true, "removed_instances": removed_instances }),
        ChangedTopic::Workspaces,
    ))
}

/// Remove ONE command template (id validation + running-guard + cascade delete), returning
/// the cascaded-instance count. Parity with the Tauri `remove_one_command`.
fn remove_one_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    command_id: &str,
) -> Result<usize, RpcError> {
    let template = db
        .with_conn(|c| db::get_template(c, command_id))
        .map_err(internal_db)?;
    if template.is_none() {
        return Err(bad_command_id_error(db, command_id));
    }
    let instance_ids = db
        .with_conn(|c| db::instance_ids_for_template(c, command_id))
        .map_err(internal_db)?;
    if runner.any_running(&instance_ids) {
        return Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "command {command_id} is running in at least one workspace; stop it before \
                 removing the command"
            ),
        ));
    }
    let removed_instances = instance_ids.len();
    db.with_conn(|c| db::delete_template(c, command_id))
        .map_err(internal_db)?;
    Ok(removed_instances)
}

/// `remove_command` — `{ command_id }` → `{ removed, removed_instances }`. Parity with the
/// Tauri `remove_command`.
pub fn remove_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let command_id = require_str(args, "command_id")?;
    let removed_instances = remove_one_command(db, runner, command_id)?;
    Ok(ToolOutcome::changed(
        json!({ "removed": true, "removed_instances": removed_instances }),
        ChangedTopic::Commands,
    ))
}

/// `remove_commands` — `{ command_ids }` → `{ removed, removed_instances, results }`.
/// GROUPED delete; a failure on one id does not abort the others. Parity with the Tauri
/// `remove_commands`.
pub fn remove_commands<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let ids = require_str_vec(args, "command_ids")?;
    let mut removed = 0usize;
    let mut removed_instances = 0usize;
    let mut results: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for id in &ids {
        if !seen.insert(id.as_str()) {
            continue;
        }
        match remove_one_command(db, runner, id) {
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
    let effects = if removed > 0 {
        vec![ChangedTopic::Commands]
    } else {
        vec![]
    };
    Ok(ToolOutcome {
        result: json!({
            "removed": removed,
            "removed_instances": removed_instances,
            "results": results,
        }),
        effects,
    })
}

/// `clear_command_output` — `{ instance_id }` → `{ instance_id, cleared: true }`. Parity
/// with the Tauri `clear_command_output` (validates the id, then clears the runner buffer).
pub fn clear_command_output<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let instance_id = require_str(args, "instance_id")?;
    let exists = db
        .with_conn(|c| db::get_instance(c, instance_id))
        .map_err(internal_db)?
        .is_some();
    if !exists {
        // Disambiguate a template command_id from an unknown id.
        let is_template = db
            .with_conn(|c| db::get_template(c, instance_id))
            .ok()
            .flatten()
            .is_some();
        return Err(if is_template {
            RpcError::new(
                RpcCode::InvalidId,
                format!(
                    "'{instance_id}' is a command TEMPLATE id (command_id), which has no live \
                     output. Pass an instance_id from list_commands(workspace_id=…)."
                ),
            )
        } else {
            RpcError::new(
                RpcCode::InvalidId,
                format!("unknown command instance {instance_id}"),
            )
        });
    }
    runner.clear_output(instance_id);
    Ok(ToolOutcome::changed(
        json!({ "instance_id": instance_id, "cleared": true }),
        ChangedTopic::Commands,
    ))
}

// --- Agent-session channel -------------------------------------------------

/// The version string nyx BUNDLES for its Claude plugin — the SINGLE SOURCE OF TRUTH for
/// "what the plugin should be" (#18b). Parsed ONCE (lazily) from the bundled
/// `plugin.json` compiled into the binary via `include_str!`, so there is NO hand-kept
/// constant to drift from the manifest: bumping `plugin.json` (which the reconcile #18
/// re-caches on a version change) automatically bumps the expected version here too.
///
/// A malformed/blank manifest version yields `None`, which DISABLES the stale check
/// (everything reads as "unknown ⇒ not stale") rather than mis-flagging every session —
/// the manifest is ours and always valid, so `None` is only a defensive floor.
static EXPECTED_PLUGIN_VERSION: std::sync::LazyLock<Option<String>> =
    std::sync::LazyLock::new(|| {
        const MANIFEST: &str =
            include_str!("../resources/claude-plugin/.claude-plugin/plugin.json");
        serde_json::from_str::<Value>(MANIFEST).ok().and_then(|v| {
            v.get("version")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    });

/// Decide, from the hook-reported `plugin_version`, whether the session's loaded plugin is
/// STALE relative to the version nyx bundles (#18b). The rule (per the spec): a reported
/// version that is PRESENT and DIFFERS from the expected version is stale ("older or
/// different" — we do not parse semver, a mismatch is enough). A MISSING/blank reported
/// version is UNKNOWN ⇒ NOT stale (an old hook that does not report its version must not be
/// flagged). When the expected version cannot be resolved (defensive `None`), nothing is
/// ever flagged.
fn plugin_is_outdated(reported: Option<&str>) -> bool {
    let reported = match reported.map(str::trim) {
        Some(v) if !v.is_empty() => v,
        _ => return false, // unknown reported version ⇒ not stale.
    };
    match EXPECTED_PLUGIN_VERSION.as_deref() {
        Some(expected) => reported != expected,
        None => false, // expected version unresolved ⇒ never flag.
    }
}

/// Whether `hook_event_name` is a per-turn ACTIVITY hook (the live dot), as opposed to a
/// SESSION-lifecycle hook (`SessionStart`/`SessionEnd`). The activity hooks are resolved
/// + short-circuited in [`agent_session_event`] so they never fall through to the
/// session-row parse (which would reject them). Kept in sync with
/// [`crate::agent_activity::ActivityEvent::from_hook`]'s recognized names — listing them
/// here lets a recognized-but-no-dot-transition hook (e.g. a `Notification` of an ignored
/// type) be acknowledged as a no-op instead of erroring.
fn is_activity_hook(name: &str) -> bool {
    matches!(
        name,
        "UserPromptSubmit"
            | "PreToolUse"
            | "PostToolUse"
            | "PostToolUseFailure"
            | "SubagentStart"
            | "SubagentStop"
            | "Notification"
            | "Stop"
            | "StopFailure"
    )
}

/// Build the MINIMAL one-line nyx context injected into a STARTING agent session as the
/// SessionStart hook's `additionalContext` (FEEDBACK #22), so the agent situates itself
/// inside nyx AND can act on the MCP tools WITHOUT discovery:
/// `You're in nyx — project "<P>" (project_id=<pid>) · workspace "<W>" (workspace_id=<wid>)
/// · terminal_id=<tid>`. Read-only, resolved from the session's workspace → project; a
/// LOOSE terminal (no workspace) yields `You're in nyx — terminal_id=<tid>`.
///
/// The IDs are the point: they let the agent call e.g. `list_commands(workspace_id=…)`
/// directly instead of `list_projects` + `list_workspaces` first (cutting useless calls).
/// Deliberately LEAN — NO path (already in the agent's cwd/context) and NO sibling-workspace
/// list (rarely useful every session); the generic "what/when/how to use nyx" stays in the
/// MCP server instructions, not re-injected here each turn.
fn session_context_line(db: &Db, session: &db::AgentSession) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ws_id) = session.workspace_id.as_deref() {
        if let Some(ws) = db.with_conn(|c| db::get_workspace(c, ws_id)).ok().flatten() {
            if let Some(proj) = db
                .with_conn(|c| db::get_project(c, &ws.project_id))
                .ok()
                .flatten()
            {
                parts.push(format!(
                    "project \"{}\" (project_id={})",
                    proj.name, proj.id
                ));
            }
            parts.push(format!(
                "workspace \"{}\" (workspace_id={})",
                ws.name, ws.id
            ));
        }
    }
    parts.push(format!("terminal_id={}", session.terminal_id));
    format!("You're in nyx — {}", parts.join(" · "))
}

/// `agent_session_event` — the Claude Code hook target. Handles BOTH:
///   * SESSION lifecycle (`SessionStart`/`SessionEnd`) → persisted `agent_sessions`
///     rows (the icon / resume candidate authority) + the `agent-sessions` topic;
///   * per-turn ACTIVITY (`UserPromptSubmit`/`Pre`/`PostToolUse`/`Subagent*`/
///     `Notification`/`Stop`/`StopFailure`) → the RUNTIME [`AgentActivityStore`] (the
///     live dot), which is NEVER persisted (the anti-phantom contract — see
///     [`crate::agent_activity`]).
///
/// The activity path is checked FIRST (via [`is_activity_hook`]) and resolved off
/// `hook_event_name` + the `tool_name`/`notification_type` discriminators, then
/// short-circuited (the per-turn hooks are NOT session events, so the adapter's
/// `parse_event` would reject them). A `SubagentStop` only decrements the in-flight
/// counter (a sub-agent finishing is not the main turn finishing); a `Notification` of an
/// ignored type is acknowledged as a no-op rather than rejected. A `SessionEnd` also
/// CLEARS the runtime activity (the clean-end anti-phantom reflex) on top of marking the
/// row `ended`.
///
/// `activity` is the host's runtime store. Parity with the Tauri `agent_session_event`
/// for the session half (over the shared `agent`/`db` layer).
pub fn agent_session_event(
    db: &Db,
    activity: &AgentActivityStore,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let agent_kind = optional_str(args, "agent_kind")?.unwrap_or(db::AGENT_KIND_CLAUDE_CODE);
    let registry = AgentRegistry::default();
    let adapter = registry.get(agent_kind).ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown agent_kind '{agent_kind}'"),
        )
    })?;
    let terminal_id = require_str(args, "NYX_TERMINAL_ID")?;

    // --- Per-turn ACTIVITY hooks (the live dot; never persisted) --------------
    // Resolve the runtime activity transition from the raw hook name + the relevant
    // payload discriminators BEFORE the session-event parse, because these hooks are not
    // session lifecycle events (the adapter would reject them). The `.cjs` hook forwards
    // the full Claude stdin, so `tool_name` (PreToolUse/PostToolUse) and
    // `notification_type` (Notification) are available here and let one hook name resolve
    // to different states (AskUserQuestion → Waiting, permission_prompt → Waiting,
    // idle_prompt → turn-end, …). A recognized activity hook updates the store and returns
    // the `agent-sessions` change topic so the front re-pulls the live dot.
    if let Some(hook_name) = args.get("hook_event_name").and_then(|v| v.as_str()) {
        // The hooks that move the live dot are NOT session events — resolve them here and
        // short-circuit so they never fall through to the session-row parse (which would
        // reject them). `is_activity_hook` covers the recognized-but-no-op case (e.g. a
        // `Notification` whose type we ignore) so it is still acknowledged, not errored.
        if is_activity_hook(hook_name) {
            let tool_name = args.get("tool_name").and_then(|v| v.as_str());
            let notification_type = args.get("notification_type").and_then(|v| v.as_str());
            if let Some(activity_event) =
                ActivityEvent::from_hook(hook_name, tool_name, notification_type)
            {
                activity.apply(terminal_id, activity_event);
                return Ok(ToolOutcome::changed(
                    json!({
                        "event": hook_name,
                        "terminal_id": terminal_id,
                        "activity": true,
                    }),
                    ChangedTopic::AgentSessions,
                ));
            }
            // A recognized activity hook that resolves to no transition (e.g. a
            // `Notification` of an ignored type, or a `SubagentStop` — kept as a no-op
            // dot-wise even though the counter logic lives in `from_hook`). Acknowledge it
            // so the best-effort hook never sees an error, but emit no change.
            return Ok(ToolOutcome::read(json!({
                "event": hook_name,
                "terminal_id": terminal_id,
                "activity": false,
                "reason": "activity hook with no dot transition",
            })));
        }
    }

    let event = adapter.parse_event(args).ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            "payload is not a recognizable agent session event (need hook_event_name + session_id)",
        )
    })?;
    match event {
        AgentEvent::Start(start) => {
            let session = db
                .with_conn(
                    |c| -> Result<Option<db::AgentSession>, diesel::result::Error> {
                        // The session upsert AND its `agent_session.started` outbox event commit
                        // ATOMICALLY (finding 01KWMCT58832JZW7M5GQDYZZ4B): `record_session_start`
                        // nests as a savepoint, and `record_agent_session_event` writes the
                        // correlated `domain_events` row in the SAME transaction — so the
                        // workspace timeline surfaces the session start, and the timeline row can
                        // never disagree with the session row (a rollback erases both).
                        use diesel::Connection;
                        c.transaction(|c| {
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
                            let row =
                                db::record_session_start(c, terminal_id, agent_kind, capture)?;
                            crate::usecase::record_agent_session_event(c, &row, true)?;
                            Ok(Some(row))
                        })
                    },
                )
                .map_err(internal_db)?;
            let Some(session) = session else {
                return Err(RpcError::new(
                    RpcCode::InvalidId,
                    format!("unknown terminal {terminal_id}"),
                ));
            };
            // STALE-PLUGIN verdict (#18b): the session reports the plugin version it loaded
            // at start; compare it to the version nyx bundles and record a RUNTIME (never
            // persisted) per-terminal flag on the same activity store the sidebar already
            // reads. A session keeps its loaded hooks until it restarts, so this is the only
            // moment we learn the loaded version — set it ONCE here. A missing/unknown
            // reported version reads as "not stale" (an old hook simply omits the field).
            let plugin_version = optional_str(args, "plugin_version")?;
            let plugin_outdated = plugin_is_outdated(plugin_version);
            activity.set_plugin_outdated(terminal_id, plugin_outdated);
            // The MINIMAL nyx-context line the SessionStart hook injects as
            // `additionalContext` (#22). Returned in `structuredContent.context` so the
            // `.cjs` hook can relay it WITHOUT any new per-spawn env injection.
            let context = session_context_line(db, &session);
            Ok(ToolOutcome::changed(
                json!({
                    "event": "SessionStart",
                    "session_id": session.id,
                    "terminal_id": session.terminal_id,
                    "agent_kind": session.agent_kind,
                    "external_session_id": session.external_session_id,
                    "state": session.state,
                    "workspace_id": session.workspace_id,
                    "plugin_outdated": plugin_outdated,
                    "context": context,
                }),
                ChangedTopic::AgentSessions,
            ))
        }
        AgentEvent::End(end) => {
            // INTERNAL-TRANSITION GUARD (the "icône qui saute après /clear" fix): a
            // `/clear` or `/resume` makes Claude fire SessionEnd { reason } and then,
            // immediately, SessionStart { source } on the SAME terminal — the session is
            // replaced in place, it does not actually end. Marking the row `ended` here
            // would vacate the `active` slot for the gap until the SessionStart lands, so
            // `active_agent_sessions` would briefly omit this terminal and the sidebar
            // icon would fall back to the generic terminal glyph (and the running dot
            // vanish) for a frame. So for these we do NOTHING: the row stays `active`
            // (the following SessionStart refreshes its external id in place), and the
            // live activity is left untouched. A real end (logout, prompt_input_exit, …)
            // falls through to the normal end path below.
            if end.is_internal_transition() {
                // UN-MAP (v2.5.10 Phase 1, task 01KXN0HTXFMZAJ0GTVWVBN2PGZ): a `/clear` RESETS
                // the session's working context — vacate its Spec SET so the terminal overlay
                // disappears — WITHOUT ending the row (the following SessionStart refreshes it
                // in place, keeping the sidebar icon/dot steady). A `/resume` is also an
                // internal transition but CONTINUES the same work, so it keeps the set; a
                // `/compact` fires no SessionEnd at all, so it never reaches here. GENERIC: the
                // reset DETECTION is per-provider (`end.resets_working_context()`), the CLEARING
                // is provider-agnostic (`db::clear_session_specs`). Best-effort — a DB hiccup or
                // no active session simply un-maps nothing.
                if end.resets_working_context() {
                    let cleared = db
                        .with_conn(|c| -> Result<usize, diesel::result::Error> {
                            let Some(session) = db::active_session_for(c, terminal_id, agent_kind)?
                            else {
                                return Ok(0);
                            };
                            db::clear_session_specs(c, &session.id)
                        })
                        .unwrap_or(0);
                    if cleared > 0 {
                        // The set actually changed → push `specs://changed` so the terminal
                        // overlay re-pulls (and, now empty, disappears).
                        return Ok(ToolOutcome::changed(
                            json!({
                                "event": "SessionEnd",
                                "terminal_id": terminal_id,
                                "ended": false,
                                "reason": "clear — session Spec set vacated (overlay reset)",
                                "specs_unmapped": cleared,
                            }),
                            ChangedTopic::Specs,
                        ));
                    }
                }
                return Ok(ToolOutcome::read(json!({
                    "event": "SessionEnd",
                    "terminal_id": terminal_id,
                    "ended": false,
                    "reason": "internal transition (clear/resume) — session kept active",
                })));
            }
            // A real SessionEnd ALSO clears the runtime activity (the anti-phantom
            // reflex on the clean-end path — no live turn survives the session ending).
            activity.clear(terminal_id);
            let outcome = db
                .with_conn(
                    |c| -> Result<Option<(String, bool)>, diesel::result::Error> {
                        // The end mutation AND its `agent_session.ended` outbox event commit
                        // ATOMICALLY (finding 01KWMCT58832JZW7M5GQDYZZ4B) so the workspace
                        // timeline surfaces the session end and can never disagree with the row.
                        use diesel::Connection;
                        c.transaction(|c| {
                            let Some(active) = db::active_session_for(c, terminal_id, agent_kind)?
                            else {
                                return Ok(None);
                            };
                            if active.external_session_id != end.external_session_id {
                                return Ok(Some((active.id, false)));
                            }
                            db::mark_session_ended(c, &active.id)?;
                            // Re-read the row so the outbox event carries the ENDED state
                            // (its `ended_at` is now stamped); a missing re-read is impossible
                            // right after the update, but guard defensively.
                            if let Some(ended) = db::get_session(c, &active.id)? {
                                crate::usecase::record_agent_session_event(c, &ended, false)?;
                            }
                            Ok(Some((active.id, true)))
                        })
                    },
                )
                .map_err(internal_db)?;
            match outcome {
                Some((session_id, true)) => Ok(ToolOutcome::changed(
                    json!({
                        "event": "SessionEnd",
                        "session_id": session_id,
                        "terminal_id": terminal_id,
                        "ended": true,
                    }),
                    ChangedTopic::AgentSessions,
                )),
                Some((session_id, false)) => Ok(ToolOutcome::read(json!({
                    "event": "SessionEnd",
                    "session_id": session_id,
                    "terminal_id": terminal_id,
                    "ended": false,
                    "reason": "active session id does not match the ended session",
                }))),
                None => Ok(ToolOutcome::read(json!({
                    "event": "SessionEnd",
                    "terminal_id": terminal_id,
                    "ended": false,
                    "reason": "no active session for this terminal",
                }))),
            }
        }
    }
}

// --- Interactive-terminal tools (DB-record half + TerminalHost) ------------

/// `create_terminal` — `{ cwd?, command?, label? }` → `{ terminal_id, cwd, workspace_id,
/// has_command }`. Writes the record + resolves auto-attach + parks any opening command,
/// then asks the shell to refresh its terminal deck (the front mounts the xterm + spawns
/// the PTY). Parity with the Tauri `create_terminal`.
pub fn create_terminal(
    db: &Db,
    host: &dyn TerminalHost,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let cwd = optional_str(args, "cwd")?;
    let command = optional_str(args, "command")?;
    let label = optional_str(args, "label")?.map(|s| s.to_string());

    let stored_cwd = match cwd {
        Some(c) => crate::pathnorm::normalize(c),
        None => ".".to_string(),
    };
    let (terminal_id, workspace_id) = db
        .with_conn(
            |c| -> Result<(String, Option<String>), diesel::result::Error> {
                let record = db::create_terminal(c, &stored_cwd, label.clone())?;
                let workspace_id = resolve_attach_for_new_terminal(c, &record.id, cwd)?;
                Ok((record.id, workspace_id))
            },
        )
        .map_err(internal_db)?;

    let has_command = command.is_some();
    if let Some(command) = command {
        host.park_opening_command(&terminal_id, command);
    }
    Ok(ToolOutcome::changed(
        json!({
            "terminal_id": terminal_id,
            "cwd": stored_cwd,
            "workspace_id": workspace_id,
            "has_command": has_command,
        }),
        ChangedTopic::Terminals,
    ))
}

/// Resolve + apply the auto-attach for a freshly-created terminal (parity with the Tauri
/// `resolve_attach_for_new_terminal`).
fn resolve_attach_for_new_terminal(
    conn: &mut diesel::SqliteConnection,
    terminal_id: &str,
    cwd: Option<&str>,
) -> diesel::QueryResult<Option<String>> {
    use crate::resolve::{
        decide_attachment, Attachment, BindingMode, CurrentBinding, WorkspaceMatch,
    };
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    let normalized = crate::pathnorm::normalize(cwd);
    let current = CurrentBinding {
        workspace_id: None,
        mode: BindingMode::Auto,
    };
    let known: Vec<WorkspaceMatch> = db::all_workspaces(conn)?
        .into_iter()
        .map(|w| WorkspaceMatch {
            id: w.id,
            path: w.root_path,
        })
        .collect();
    match decide_attachment(&current, Some(&normalized), &known) {
        Attachment::AttachAuto(ws) => {
            db::attach_terminal(conn, terminal_id, &ws, db::BINDING_AUTO)?;
            Ok(Some(ws))
        }
        Attachment::Unchanged => Ok(None),
    }
}

/// `send_to_terminal` — `{ terminal_id, command }` → `{ terminal_id, sent: true }`.
/// Validates the alive record, then writes `command + "\r"` into the live shell via the
/// shell's [`TerminalHost`]. Parity with the Tauri `send_to_terminal`.
pub fn send_to_terminal(
    db: &Db,
    host: &dyn TerminalHost,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let terminal_id = require_str(args, "terminal_id")?;
    let command = require_str(args, "command")?;
    // The id must name an ALIVE record first (so "unknown id" is distinct from "no PTY").
    let record = db
        .with_conn(|c| db::get_terminal(c, terminal_id))
        .map_err(internal_db)?;
    match record {
        Some(r) if r.status == db::STATUS_ALIVE => {}
        _ => return Err(bad_terminal_id_error(terminal_id)),
    }
    let written = host
        .send_to_terminal(terminal_id, command)
        .map_err(|e| RpcError::new(RpcCode::Internal, format!("write to terminal failed: {e}")))?;
    if !written {
        return Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "terminal {terminal_id} has no live shell yet (it may still be starting up, or \
                 has already exited); try again, or open one with create_terminal"
            ),
        ));
    }
    Ok(ToolOutcome::read(
        json!({ "terminal_id": terminal_id, "sent": true }),
    ))
}

/// Resolve ONE `keys` array element to the control bytes it injects. An element is treated
/// as a NAMED KEY only when it matches the vocabulary EXACTLY (case-insensitive) — `enter`,
/// the arrows/navigation keys, `ctrl+<letter>`, … ; ANY other string is LITERAL text and is
/// written verbatim (its own UTF-8 bytes, no newline added). This is the seam that lets
/// `send_keys` both TYPE into a TUI and send bare keystrokes, with no forced `\r`.
///
/// Kept as a PURE function (no I/O, no host) so the whole vocabulary is unit-testable.
fn resolve_key_element(element: &str) -> Vec<u8> {
    let lower = element.to_ascii_lowercase();
    // A generic `ctrl+<letter>` → the control byte (ascii & 0x1f), covering the named
    // ctrl+c/d/u/l/a/e below AND any other single letter (ctrl+x, ctrl+w, …).
    if let Some(rest) = lower.strip_prefix("ctrl+") {
        let bytes = rest.as_bytes();
        if bytes.len() == 1 && bytes[0].is_ascii_alphabetic() {
            return vec![bytes[0] & 0x1f];
        }
    }
    let named: &[u8] = match lower.as_str() {
        "enter" => b"\r",
        "tab" => b"\t",
        "escape" | "esc" => b"\x1b",
        "backspace" => b"\x7f",
        "space" => b" ",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "pageup" => b"\x1b[5~",
        "pagedown" => b"\x1b[6~",
        "delete" => b"\x1b[3~",
        // Explicit ctrl shortcuts also covered by the generic ctrl+<letter> above; listed
        // for completeness/clarity (they resolve identically).
        "ctrl+c" => b"\x03",
        "ctrl+d" => b"\x04",
        "ctrl+u" => b"\x15",
        "ctrl+l" => b"\x0c",
        "ctrl+a" => b"\x01",
        "ctrl+e" => b"\x05",
        // Not a named key → LITERAL text, written verbatim (its own UTF-8 bytes, no newline).
        _ => return element.as_bytes().to_vec(),
    };
    named.to_vec()
}

/// Resolve a `keys` array (left-to-right) into ONE raw byte sequence: each element is either
/// LITERAL text (written verbatim) or a NAMED KEY resolved to its control bytes (see
/// [`resolve_key_element`]). NO trailing `\r` is ever appended — the caller asks for `enter`
/// explicitly when it wants to submit. Pure (testable) companion to [`send_keys`].
fn resolve_keys(elements: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for element in elements {
        out.extend_from_slice(&resolve_key_element(element));
    }
    out
}

/// `send_keys` — `{ terminal_id, keys }` (keys: an ARRAY of literal-text / named-key
/// strings) → `{ terminal_id, sent: true, bytes }`. Validates the alive record, resolves
/// `keys` to RAW bytes (named keys → control bytes; literal text → verbatim), then writes
/// them into the live shell WITHOUT any appended `\r` via the shell's [`TerminalHost`]. The
/// raw-key counterpart to `send_to_terminal` (which always appends a newline): this drives a
/// raw-mode TUI (arrows, Esc, Ctrl+C, typing without submitting). Same liveness /
/// `invalid_state` semantics as `send_to_terminal`.
pub fn send_keys(db: &Db, host: &dyn TerminalHost, args: &Value) -> Result<ToolOutcome, RpcError> {
    let terminal_id = require_str(args, "terminal_id")?;
    let keys = require_str_vec(args, "keys")?;
    // The id must name an ALIVE record first (so "unknown id" is distinct from "no PTY").
    let record = db
        .with_conn(|c| db::get_terminal(c, terminal_id))
        .map_err(internal_db)?;
    match record {
        Some(r) if r.status == db::STATUS_ALIVE => {}
        _ => return Err(bad_terminal_id_error(terminal_id)),
    }
    let bytes = resolve_keys(&keys);
    let written = host
        .send_keys(terminal_id, &bytes)
        .map_err(|e| RpcError::new(RpcCode::Internal, format!("write to terminal failed: {e}")))?;
    if !written {
        return Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "terminal {terminal_id} has no live shell yet (it may still be starting up, or \
                 has already exited); try again, or open one with create_terminal"
            ),
        ));
    }
    Ok(ToolOutcome::read(json!({
        "terminal_id": terminal_id,
        "sent": true,
        "bytes": bytes.len(),
    })))
}

/// `list_terminals` — `{ include_closed? }` → `{ terminals }`. Lists records + the live
/// `live`/`busy` bits from the shell's [`TerminalHost`]. Parity with the Tauri
/// `list_terminals`.
pub fn list_terminals(db: &Db, host: &dyn TerminalHost, args: &Value) -> Result<Value, RpcError> {
    let include_closed = optional_bool(args, "include_closed")?.unwrap_or(false);
    let records = db.with_conn(db::list_terminals).map_err(internal_db)?;
    let terminals: Vec<Value> = records
        .into_iter()
        .filter(|t| include_closed || t.status == db::STATUS_ALIVE)
        .map(|t| {
            let (live, busy) = host.terminal_liveness(&t.id);
            json!({
                "terminal_id": t.id,
                "cwd": t.cwd,
                "label": t.label,
                "workspace_id": t.workspace_id,
                "status": t.status,
                "live": live,
                "busy": busy,
                "exec_state": t.exec_state,
                "exec_exit_code": t.exec_exit_code,
                "exec_state_updated_at": t.exec_state_updated_at,
            })
        })
        .collect();
    Ok(json!({ "terminals": terminals }))
}

/// `close_terminal` — `{ terminal_id }` → `{ terminal_id, closed: true }`. Flips the record
/// closed + asks the shell to kill the live PTY. Parity with the Tauri `close_terminal`.
pub fn close_terminal(
    db: &Db,
    host: &dyn TerminalHost,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let terminal_id = require_str(args, "terminal_id")?;
    let record = db
        .with_conn(|c| db::get_terminal(c, terminal_id))
        .map_err(internal_db)?;
    match record {
        Some(r) if r.status == db::STATUS_ALIVE => {}
        _ => return Err(bad_terminal_id_error(terminal_id)),
    }
    db.with_conn(|c| db::close_terminal(c, terminal_id))
        .map_err(internal_db)?;
    host.close_terminal_pty(terminal_id);
    Ok(ToolOutcome::changed(
        json!({ "terminal_id": terminal_id, "closed": true }),
        ChangedTopic::Terminals,
    ))
}

/// `read_terminal` — `{ terminal_id, tail_bytes?, max_bytes?, since?, strip_ansi? }` →
/// `{ terminal_id, output, total_bytes, returned_bytes, truncated, reset, cursor }`. Reads
/// the persisted (front-serialized) scrollback. Parity with the Tauri `read_terminal`
/// (returning the bounded window; `strip_ansi` post-processing is the same byte-exact
/// window — the shell may further strip if it wants the exact rendered field).
pub fn read_terminal(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let terminal_id = require_str(args, "terminal_id")?;
    let (effective_tail, since, strip) = parse_window_knobs(args)?;
    let scrollback = match db
        .with_conn(|c| db::get_terminal(c, terminal_id))
        .map_err(internal_db)?
    {
        Some(record) => record.scrollback,
        None => {
            return Err(RpcError::new(
                RpcCode::InvalidId,
                format!(
                    "unknown terminal {terminal_id} (no such terminal record; use a terminal_id \
                     from list_terminals, or one returned by create_terminal)"
                ),
            ))
        }
    };
    let window = crate::mcp_runtime::bound_output(&scrollback, effective_tail, since);
    let output = if strip {
        crate::ansi::strip_ansi(&window.output)
    } else {
        window.output.clone()
    };
    Ok(json!({
        "terminal_id": terminal_id,
        "output": output,
        "total_bytes": window.total_bytes,
        "returned_bytes": window.returned_bytes,
        "truncated": window.truncated,
        "reset": window.reset,
        "cursor": window.cursor,
    }))
}

// --- Unified dispatch ------------------------------------------------------

/// The SINGLE entry point a shell's MCP dispatcher calls for EVERY advertised extension
/// tool — the read tools, the command-template CRUD, the agent-session channel, and the
/// interactive-terminal tools — over a [`Db`] + a [`CommandRunner<S>`] + the shell's
/// [`TerminalHost`]. Returns:
///
/// - `Some(Ok(outcome))` — the tool ran; `outcome.effects` names the `changed` topics the
///   shell should broadcast after this returns (the event seam).
/// - `Some(Err(e))` — the tool ran but produced a domain error (`invalid_id`/…); it is
///   STILL served (never `method_not_found`/`unknown tool`).
/// - `None` — `name` is NOT one of this module's tools (it belongs to
///   [`crate::mcp_runtime`]: the runtime command tools `start_command`/… or the workspace
///   registration tools `workspace_add`/`create_workspace`), so the caller falls through.
///
/// This is what guarantees that NO tool advertised by `tools/list` falls into the
/// dispatcher's `unknown tool` arm: every advertised name resolves to either this module
/// or [`crate::mcp_runtime`].
pub fn dispatch_extension_tool<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    host: &dyn TerminalHost,
    activity: &AgentActivityStore,
    name: &str,
    args: &Value,
    caller_terminal_id: Option<&str>,
) -> Option<Result<ToolOutcome, RpcError>> {
    // Read tools wrap their plain JSON result into a no-effect `ToolOutcome`.
    let read = |r: Result<Value, RpcError>| Some(r.map(ToolOutcome::read));
    let dispatched = match name {
        // Pure DB reads.
        crate::mcp::PROBE_TOOL => read(probe(db)),
        "list_projects" => read(list_projects(db)),
        "list_workspaces" => read(list_workspaces(db, args)),
        "list_commands" => read(list_commands(db, runner, args)),
        // The v2.2 cockpit read model: a workspace's LIVE runtime aggregate (terminals +
        // sessions + services), overlaying live run state from `runner` — the SAME
        // `workspace_runtime::snapshot` the UI bridge drives (PDR-0005).
        crate::mcp::WORKSPACE_RUNTIME_TOOL => read(workspace_runtime(db, runner, args)),
        // The v2.2 Git CHANGES read model: branch/dirty/changed-files per non-optional
        // repo, RE-READ from Git — the SAME `workspace_git::summary` the UI bridge drives.
        crate::mcp::WORKSPACE_GIT_TOOL => read(workspace_git(db, args)),
        // The v2.2 TIMELINE read model: a bounded, oldest-first fil of a workspace's recent
        // domain_events — the SAME `workspace_timeline::timeline` the UI bridge drives.
        crate::mcp::WORKSPACE_TIMELINE_TOOL => read(workspace_timeline(db, args)),
        // The v2.4 Git DIFF read model: the line-level diff of ONE repo in a workspace for a
        // review surface (living/pinned) — the SAME `git_diff::diff_repo_with_mode` the UI
        // bridge's `repo_diff` db-command drives (gix primary, git CLI fallback).
        crate::mcp::REPO_DIFF_TOOL => read(repo_diff(db, args)),
        // The v2.4 DERIVED GIT VIEWS read model: per-repo dirty/branch/ahead-behind/summary,
        // on demand — the SAME `git_views::repo_view_with_target` the UI bridge drives.
        crate::mcp::REPO_GIT_VIEW_TOOL => read(repo_git_view(db, args)),
        // The v2.4 review COMMENT feed read model: the auto-threaded comments of a
        // (workspace, repository) — the SAME `db::list_review_comments` the UI bridge drives.
        crate::mcp::REVIEW_COMMENT_LIST_TOOL => read(review_comment_list(db, args)),
        // The v2.4 review COMMENT post: the AGENT leaves a comment/reply (never resolves —
        // there is no resolve tool). Mutating → carries a `Repos` changed effect so the UI
        // re-pulls the feed. The SAME `db::insert_review_comment` the UI bridge drives.
        crate::mcp::REVIEW_COMMENT_POST_TOOL => Some(review_comment_post(db, args)),
        // The v2.4 review TASK (finding = task) list: the promoted-task list of a (workspace,
        // repository) — the SAME `db::list_review_tasks` the UI bridge drives. Read-only.
        crate::mcp::REVIEW_TASK_LIST_TOOL => read(review_task_list(db, args)),
        // The v2.4 review TASK structure: the AGENT structures a proposed promotion (never
        // validates — there is no validate tool). Mutating → carries a `Repos` changed effect so
        // the UI re-pulls the task list + unread badge. The SAME `db::structure_task`.
        crate::mcp::REVIEW_TASK_STRUCTURE_TOOL => Some(review_task_structure(db, args)),
        crate::mcp::LIST_IMPORTABLE_SCRIPTS_TOOL => read(list_importable_scripts(db, args)),
        // Project settings (rename + resume opt-in) — mutating → workspaces changed.
        crate::mcp::SET_PROJECT_SETTINGS_TOOL => Some(set_project_settings(db, args)),
        // Explicit repository config (finding #27): SET a project's repos — mutating → repos
        // changed so any open review/cockpit re-pulls. The SAME `db::set_project_repositories`.
        crate::mcp::PROJECT_SET_REPOS_TOOL => Some(project_set_repos(db, args)),
        // Command-template CRUD (mutating → carry a `changed` effect).
        crate::mcp::ADD_COMMAND_TOOL => Some(add_command(db, args)),
        crate::mcp::UPDATE_COMMAND_TOOL => Some(update_command(db, runner, args)),
        crate::mcp::IMPORT_COMMANDS_TOOL => Some(import_commands(db, args)),
        crate::mcp::REMOVE_WORKSPACE_TOOL => Some(remove_workspace(db, runner, host, args)),
        crate::mcp::REMOVE_COMMAND_TOOL => Some(remove_command(db, runner, args)),
        crate::mcp::REMOVE_COMMANDS_TOOL => Some(remove_commands(db, runner, args)),
        crate::mcp::CLEAR_COMMAND_OUTPUT_TOOL => Some(clear_command_output(db, runner, args)),
        // Agent-session channel (session lifecycle + runtime activity).
        crate::mcp::AGENT_SESSION_EVENT_TOOL => Some(agent_session_event(db, activity, args)),
        // Interactive-terminal tools (DB-record half + the shell's TerminalHost).
        crate::mcp::CREATE_TERMINAL_TOOL => Some(create_terminal(db, host, args)),
        crate::mcp::SEND_TO_TERMINAL_TOOL => Some(send_to_terminal(db, host, args)),
        crate::mcp::SEND_KEYS_TOOL => Some(send_keys(db, host, args)),
        crate::mcp::LIST_TERMINALS_TOOL => read(list_terminals(db, host, args)),
        crate::mcp::CLOSE_TERMINAL_TOOL => Some(close_terminal(db, host, args)),
        crate::mcp::READ_TERMINAL_TOOL => read(read_terminal(db, args)),
        // The v2.4 Phase-5 tools (routed by their own sub-dispatchers so a Spec/context tool
        // is added in exactly one place): the Spec authoring + transition tools
        // (create/update/fork/transition Spec, add/transition/depend tasks) and the
        // agent-context render — each delegating to the SHARED `nyx_core::db` authority
        // (single mutation path, no double implementation).
        _ => crate::mcp_tools_spec::dispatch_spec_tool(db, name, args)
            .or_else(|| crate::mcp_context::dispatch_context_tool(db, name, args)),
    };

    // AUTO-MAP (v2.5.10 Phase 1, task 01KXN0HTAXXMXF3GXF4ACM2970): when a spec-scoped MCP
    // MUTATION succeeds, attribute it to the CALLING terminal's live session and ADD the
    // worked Spec to that session's set. `caller_terminal_id` is the terminal the MCP
    // connection is bound to (the `X-Nyx-Terminal-Id` header the plugin `.mcp.json` carries,
    // resolved by `pty.rs`'s `NYX_TERMINAL_ID`); `None` for an un-attributable call (a bare
    // client, a test). GENERIC + CIBLÉE: `worked_spec_id` returns `Some` ONLY for the mutating
    // Spec authoring/transition tools (a READ returns `None`, so a read never maps), and the
    // link resolves through the session substrate (no provider named). BEST-EFFORT: a terminal
    // with no active session, or any DB hiccup, silently maps nothing — the tool result the
    // agent sees is never affected.
    if let (Some(caller), Some(Ok(outcome))) = (caller_terminal_id, dispatched.as_ref()) {
        if let Some(spec_id) = crate::mcp_tools_spec::worked_spec_id(name, &outcome.result) {
            let _ = db.with_conn(|c| db::link_terminal_active_session_spec(c, caller, &spec_id));
        }
    }
    dispatched
}

#[cfg(test)]
mod tests {
    use super::*;
    use portable_pty::PtySize;

    /// A no-op [`RunnerSink`]: nothing is running, nothing to persist. Enough for the
    /// CRUD/list tools whose only runner use is the `any_running`/`outcome` read.
    struct NoopSink;
    impl RunnerSink for NoopSink {
        fn on_state(&self, _: &str, _: RunState, _: Option<i32>) {}
        fn on_acknowledge(&self, _: &str) {}
        fn on_output(&self, _: &str, _: &[u8]) {}
        fn persist_scrollback(&self, _: &str, _: &str) {}
        fn archive_previous_run(&self, _: &str) {}
        fn clear_output(&self, _: &str) {}
    }

    fn noop_runner() -> CommandRunner<NoopSink> {
        CommandRunner::new(
            NoopSink,
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
        )
    }

    fn db_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn list_commands_returns_templates_not_terminals() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/lc", None))
            .unwrap();
        // A template in the project.
        db.with_conn(|c| {
            db::create_template(
                c,
                &project.id,
                "build",
                "pnpm build",
                None,
                db::CommandSource::default(),
            )
        })
        .unwrap();
        let runner = noop_runner();
        // The project (template) form returns COMMANDS, never terminals.
        let res = list_commands(&db, &runner, &json!({ "project_id": project.id })).unwrap();
        let cmds = res["commands"].as_array().expect("commands array");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0]["name"], json!("build"));
        assert!(res.get("terminals").is_none(), "must not return terminals");
    }

    /// DONE-CRITERION (task #8): the MCP identity READ tools (`list_projects` /
    /// `list_workspaces`) pass through the SHARED `usecase` layer. Proof: a project +
    /// workspace created via the SHARED `usecase::create_project` / `usecase::register_workspace`
    /// (the exact use cases the UI bridge drives) surface through the MCP `list_projects` /
    /// `list_workspaces` tools — same authority on both the read and the write side.
    #[test]
    fn mcp_list_identity_tools_go_through_the_shared_use_case() {
        use crate::usecase::{
            self, test_support::RecordingEventSink, DomainEventKind, DomainEventSource,
        };
        let _g = db_guard();
        let db = Db::in_memory();
        let sink = RecordingEventSink::new();

        // WRITE via the shared use cases (the UI-and-MCP authority).
        let (project, _root) = db
            .with_conn(|c| {
                usecase::create_project(c, &sink, "P", "/tmp/uc-mcp", None, DomainEventSource::Mcp)
            })
            .unwrap();
        let ws = db
            .with_conn(|c| {
                usecase::register_workspace(
                    c,
                    &sink,
                    &project.id,
                    "wt",
                    "/tmp/uc-mcp-wt",
                    DomainEventSource::Mcp,
                )
            })
            .unwrap();

        // READ via the MCP identity tools → the same authority surfaces them.
        let projects = list_projects(&db).unwrap();
        let ids: Vec<&str> = projects["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&project.id.as_str()));

        let workspaces = list_workspaces(&db, &json!({ "project_id": project.id })).unwrap();
        let ws_ids: Vec<&str> = workspaces["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["id"].as_str().unwrap())
            .collect();
        assert!(ws_ids.contains(&ws.id.as_str()));

        // The WRITE-side use cases emitted the expected domain events (task #8 seam).
        assert_eq!(
            sink.kinds(),
            vec![
                DomainEventKind::ProjectCreated,
                DomainEventKind::WorkspaceCreated
            ]
        );
    }

    /// DONE-CRITERION (task 01KWHPV32YVCRBNY1JW5CDY4AY, criterion #2): the MCP
    /// `workspace_runtime` tool serves the SAME read model the UI bridge consumes — a
    /// workspace's terminals + agent sessions + services, aggregated by the SHARED
    /// `workspace_runtime::snapshot`. Proof: seed a workspace + a terminal with a live
    /// session + a materialized command instance, then read via the MCP tool and assert
    /// the aggregate shape, including the live-runner run-state overlay.
    #[test]
    fn mcp_workspace_runtime_tool_serves_the_shared_read_model() {
        use crate::usecase::{self, test_support::RecordingEventSink, DomainEventSource};
        let _g = db_guard();
        let db = Db::in_memory();
        let sink = RecordingEventSink::new();

        let (project, _root) = db
            .with_conn(|c| {
                usecase::create_project(c, &sink, "P", "/tmp/wr-mcp", None, DomainEventSource::Mcp)
            })
            .unwrap();
        let ws = db
            .with_conn(|c| {
                usecase::register_workspace(
                    c,
                    &sink,
                    &project.id,
                    "wt",
                    "/tmp/wr-mcp-wt",
                    DomainEventSource::Mcp,
                )
            })
            .unwrap();
        // A command template → one instance in the workspace.
        db.with_conn(|c| {
            db::create_template(
                c,
                &project.id,
                "dev",
                "npm run dev",
                None,
                db::CommandSource::default(),
            )
        })
        .unwrap();
        // A terminal on the workspace, hosting a live agent session.
        let term = db
            .with_conn(|c| db::create_terminal(c, "/tmp/wr-mcp-wt", None))
            .unwrap();
        db.with_conn(|c| db::attach_terminal(c, &term.id, &ws.id, "auto"))
            .unwrap();
        db.with_conn(|c| {
            db::record_session_start(
                c,
                &term.id,
                "claude_code",
                db::SessionCapture {
                    workspace_id: Some(ws.id.clone()),
                    external_session_id: "sess-mcp".to_string(),
                    cwd: "/tmp/wr-mcp-wt".to_string(),
                    transcript_path: None,
                    metadata_json: None,
                },
            )
        })
        .unwrap();

        // READ via the MCP tool (through the shared read model + the live runner overlay).
        let runner = noop_runner();
        let res = workspace_runtime(&db, &runner, &json!({ "workspace_id": ws.id })).unwrap();
        let runtime = &res["runtime"];
        assert_eq!(runtime["workspace"]["id"], json!(ws.id));
        // Terminal + its session.
        let terminals = runtime["terminals"].as_array().unwrap();
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["id"], json!(term.id));
        assert_eq!(
            terminals[0]["sessions"][0]["external_session_id"],
            json!("sess-mcp")
        );
        // Cross-terminal agent view.
        let sessions = runtime["agent_sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        // Service with its (persisted-idle) run state.
        let services = runtime["services"].as_array().unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0]["name"], json!("dev"));
        assert_eq!(services[0]["run_state"], json!("idle"));

        // An unknown workspace id is a D8 invalid_id error (not an empty snapshot).
        let err = workspace_runtime(&db, &runner, &json!({ "workspace_id": "nope" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    /// DONE-CRITERION (task 01KWHPV33EGTB3Z41J0RQR1BAK): the MCP `workspace_git` tool
    /// serves the SAME Git CHANGES read model the UI bridge consumes — branch/dirty/
    /// changed-files per non-optional repo, RE-READ from Git. Proof: register a real git
    /// repo as a workspace, dirty it, and assert the MCP tool reports the change (with NO
    /// DB write — Git is never authoritative in the DB). Git-guarded (skips if git absent).
    #[test]
    fn mcp_workspace_git_tool_serves_the_shared_git_read_model() {
        use crate::repository::register_project;
        let _g = db_guard();
        if crate::proc_util::command("git")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping mcp workspace_git test: git unavailable");
            return;
        }
        let root = std::env::temp_dir().join(format!("nyx-mcp-git-{}", uuid::Uuid::now_v7()));
        let repo = root.join("api");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str], dir: &std::path::Path| {
            let ok = crate::proc_util::command("git")
                .args([
                    "-c",
                    "user.email=t@nyx",
                    "-c",
                    "user.name=nyx",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"], &repo);
        git(&["symbolic-ref", "HEAD", "refs/heads/main"], &repo);
        std::fs::write(repo.join("README.md"), b"nyx").unwrap();
        git(&["add", "README.md"], &repo);
        git(&["commit", "-q", "-m", "init"], &repo);

        let db = Db::in_memory();
        let (project, root_ws) = db
            .with_conn(|c| register_project(c, "Nyx", root.to_str().unwrap(), None))
            .unwrap();
        // Finding #27: no auto-detection — configure the `api` sub-repo EXPLICITLY.
        db.with_conn(|c| {
            db::set_project_repositories(
                c,
                &project.id,
                &[db::RepoConfig {
                    relative_path: "api".into(),
                    name: Some("api".into()),
                }],
            )
        })
        .unwrap();

        // Dirty the repo with an untracked file.
        std::fs::write(repo.join("new.txt"), b"new").unwrap();

        let res = workspace_git(&db, &json!({ "workspace_id": root_ws.id })).unwrap();
        let repos = res["git"]["repos"].as_array().unwrap();
        let api = repos
            .iter()
            .find(|r| r["relative_path"] == "api")
            .expect("api repo summarized");
        assert_eq!(api["branch"], json!("main"), "branch re-read from Git");
        assert_eq!(api["dirty"], json!(true), "dirty re-read from Git");
        let files: Vec<&str> = api["changed_files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect();
        assert!(files.contains(&"new.txt"), "changed files: {files:?}");

        // Unknown workspace id is a D8 invalid_id error.
        let err = workspace_git(&db, &json!({ "workspace_id": "nope" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (task 01KX0F6FD836YASVYT17YV3FTW): the MCP `repo_diff` tool exposes
    /// the v2.4 diff engine to an agent — the line-level diff of ONE repository within a
    /// workspace, resolved by `(workspace_id, repository_id)` so identities never mix.
    /// Proof: register a git repo, modify a tracked file, then assert the tool returns a
    /// modified FileDiff with hunks for THAT repo's file. Git-guarded (skips if absent).
    #[test]
    fn mcp_repo_diff_tool_serves_the_v24_diff_engine() {
        use crate::repository::register_project;
        let _g = db_guard();
        if crate::proc_util::command("git")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping mcp repo_diff test: git unavailable");
            return;
        }
        let root = std::env::temp_dir().join(format!("nyx-mcp-diff-{}", uuid::Uuid::now_v7()));
        let repo = root.join("api");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str], dir: &std::path::Path| {
            let ok = crate::proc_util::command("git")
                .args([
                    "-c",
                    "user.email=t@nyx",
                    "-c",
                    "user.name=nyx",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"], &repo);
        git(&["symbolic-ref", "HEAD", "refs/heads/main"], &repo);
        std::fs::write(repo.join("README.md"), b"line1\nline2\nline3\n").unwrap();
        git(&["add", "README.md"], &repo);
        git(&["commit", "-q", "-m", "init"], &repo);

        let db = Db::in_memory();
        let (project, root_ws) = db
            .with_conn(|c| register_project(c, "Nyx", root.to_str().unwrap(), None))
            .unwrap();
        // Finding #27: configure the `api` sub-repo EXPLICITLY (no disk auto-detection).
        db.with_conn(|c| {
            db::set_project_repositories(
                c,
                &project.id,
                &[db::RepoConfig {
                    relative_path: "api".into(),
                    name: Some("api".into()),
                }],
            )
        })
        .unwrap();

        // The repository_id the tool needs comes from the workspace_git read model.
        let git_summary = workspace_git(&db, &json!({ "workspace_id": root_ws.id })).unwrap();
        let repository_id = git_summary["git"]["repos"][0]["repository_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Modify the tracked file → the working-tree diff shows a modification with hunks.
        std::fs::write(repo.join("README.md"), b"line1\nCHANGED\nline3\n").unwrap();

        let res = repo_diff(
            &db,
            &json!({
                "workspace_id": root_ws.id,
                "repository_id": repository_id,
                "kind": "working_tree",
            }),
        )
        .unwrap();
        let files = res["diff"]["files"].as_array().unwrap();
        let readme = files
            .iter()
            .find(|f| {
                f["new"]["path"] == json!("README.md") || f["old"]["path"] == json!("README.md")
            })
            .expect("README diff present");
        assert_eq!(readme["change"], json!("modified"));
        assert!(
            !readme["hunks"].as_array().unwrap().is_empty(),
            "the modification has hunks"
        );
        assert_eq!(res["diff"]["backend"], json!("gix"), "gix primary backend");
        assert_eq!(
            res["diff"]["mode"],
            json!("living"),
            "working tree is living"
        );

        // An unknown repository_id is a D8 invalid_id error.
        let err = repo_diff(
            &db,
            &json!({
                "workspace_id": root_ws.id,
                "repository_id": "nope",
                "kind": "working_tree",
            }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);

        // An unknown diff kind is an invalid_argument error.
        let err = repo_diff(
            &db,
            &json!({
                "workspace_id": root_ws.id,
                "repository_id": "whatever",
                "kind": "bogus",
            }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (task 01KX0F6FXE6AJ9BR4BYHRX6B1R): the MCP `repo_git_view` tool serves
    /// the v2.4 derived Git views — per-repo dirty/branch/ahead-behind/summary, on demand, in
    /// the stable shape the renderer also consumes. Proof: register a git repo, dirty it, and
    /// assert the tool reports the dirty partition + current branch. Git-guarded.
    #[test]
    fn mcp_repo_git_view_tool_serves_the_derived_views() {
        use crate::repository::register_project;
        let _g = db_guard();
        if crate::proc_util::command("git")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping mcp repo_git_view test: git unavailable");
            return;
        }
        let root = std::env::temp_dir().join(format!("nyx-mcp-view-{}", uuid::Uuid::now_v7()));
        let repo = root.join("api");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str], dir: &std::path::Path| {
            let ok = crate::proc_util::command("git")
                .args([
                    "-c",
                    "user.email=t@nyx",
                    "-c",
                    "user.name=nyx",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"], &repo);
        git(&["symbolic-ref", "HEAD", "refs/heads/main"], &repo);
        std::fs::write(repo.join("README.md"), b"line1\nline2\n").unwrap();
        git(&["add", "README.md"], &repo);
        git(&["commit", "-q", "-m", "init"], &repo);

        let db = Db::in_memory();
        let (project, root_ws) = db
            .with_conn(|c| register_project(c, "Nyx", root.to_str().unwrap(), None))
            .unwrap();
        // Finding #27: configure the `api` sub-repo EXPLICITLY (no disk auto-detection).
        db.with_conn(|c| {
            db::set_project_repositories(
                c,
                &project.id,
                &[db::RepoConfig {
                    relative_path: "api".into(),
                    name: Some("api".into()),
                }],
            )
        })
        .unwrap();
        let git_summary = workspace_git(&db, &json!({ "workspace_id": root_ws.id })).unwrap();
        let repository_id = git_summary["git"]["repos"][0]["repository_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Dirty it with an untracked file.
        std::fs::write(repo.join("scratch.txt"), b"scratch\n").unwrap();

        let res = repo_git_view(
            &db,
            &json!({
                "workspace_id": root_ws.id,
                "repository_id": repository_id,
            }),
        )
        .unwrap();
        assert_eq!(res["view"]["is_git"], json!(true));
        assert_eq!(res["view"]["branch"]["current"], json!("main"));
        assert_eq!(res["view"]["dirty"]["clean"], json!(false));
        let untracked: Vec<&str> = res["view"]["dirty"]["untracked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(untracked.contains(&"scratch.txt"));
        assert_eq!(res["view"]["summary"]["untracked"], json!(1));

        // An unknown repository_id is a D8 invalid_id error.
        let err = repo_git_view(
            &db,
            &json!({
                "workspace_id": root_ws.id,
                "repository_id": "nope",
            }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (task 01KX0F6HEBPK34CV49EREHK3Y9): the MCP review-comment tools let an
    /// AGENT post a comment and REPLY into a thread — and the reply is authored `agent` and
    /// appears in the list. Proof: post a root, reply to it via `review_comment_post`, and read
    /// the feed back with `review_comment_list`. No git repo needed (comments key on the
    /// (workspace, repository) strings).
    #[test]
    fn mcp_review_comment_tools_let_the_agent_post_and_reply() {
        let _g = db_guard();
        let db = Db::in_memory();

        // The agent posts an ANCHORED root comment.
        let posted = review_comment_post(
            &db,
            &json!({
                "workspace_id": "ws",
                "repository_id": "repo",
                "body": "this looks off",
                "file_path": "src/a.rs",
                "side": "new",
                "line_start": 4,
            }),
        )
        .expect("post root");
        // A mutating tool → it carries the Repos changed effect (the UI re-pulls live).
        assert_eq!(posted.effects, vec![ChangedTopic::Repos]);
        let root = &posted.result["comment"];
        assert_eq!(
            root["author_type"],
            json!("agent"),
            "MCP posts as the agent"
        );
        assert_eq!(root["file_path"], json!("src/a.rs"));
        assert_eq!(root["line_start"], json!(4));
        let root_id = root["id"].as_str().unwrap().to_string();

        // The agent REPLIES into the thread (parent_id).
        let reply = review_comment_post(
            &db,
            &json!({
                "workspace_id": "ws",
                "repository_id": "repo",
                "body": "on second thought it's fine",
                "parent_id": root_id,
            }),
        )
        .expect("post reply");
        assert_eq!(reply.result["comment"]["parent_id"], json!(root_id));
        assert_eq!(reply.result["comment"]["author_type"], json!("agent"));

        // The list shows the auto-threaded feed (root + reply), authored agent.
        let listed = review_comment_list(
            &db,
            &json!({ "workspace_id": "ws", "repository_id": "repo" }),
        )
        .expect("list");
        let comments = listed["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 2, "root + reply");
        assert_eq!(comments[0]["id"], json!(root_id), "oldest-first");
        assert_eq!(comments[1]["parent_id"], json!(root_id));
    }

    /// DONE-CRITERION (task 01KX0F6HEBPK34CV49EREHK3Y9): the agent NEVER resolves. Proof: the
    /// dispatcher exposes NO `review_comment_resolve` tool (and no resolve tool of any name) to
    /// MCP — the agent has post/reply, and resolution is reachable ONLY from the UI façade. An
    /// attempt to dispatch a resolve tool falls through as an unknown tool (`None`).
    #[test]
    fn mcp_agent_cannot_resolve_a_comment() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();

        // No resolve tool of any name is dispatchable over MCP.
        for name in ["review_comment_resolve", "review_comment_reopen"] {
            let out = dispatch_extension_tool(
                &db,
                &runner,
                &host,
                &activity,
                name,
                &json!({ "id": "whatever" }),
                None,
            );
            assert!(
                out.is_none(),
                "{name} must NOT be an MCP tool — the agent never resolves"
            );
        }

        // The agent CAN post/reply (sanity: the façade it IS allowed is dispatchable).
        let out = dispatch_extension_tool(
            &db,
            &runner,
            &host,
            &activity,
            crate::mcp::REVIEW_COMMENT_POST_TOOL,
            &json!({ "workspace_id": "ws", "repository_id": "repo", "body": "hi" }),
            None,
        );
        assert!(out.is_some(), "review_comment_post IS an MCP tool");
    }

    /// The GRILL over MCP (task 01KX0F6JEKCEV2JCTMEBMM3P4Z): the agent STRUCTURES a proposed
    /// task via `review_task_structure` and lists it via `review_task_list`. Proof: a
    /// human-promoted (proposed) task is structured to a clean task, moves to `structured`,
    /// carries the `Repos` changed effect, and shows in the list.
    #[test]
    fn mcp_review_task_structure_and_list() {
        let _g = db_guard();
        let db = Db::in_memory();

        // A human-promoted task (the DB layer models the human's "lâche brut").
        let task = db
            .with_conn(|c| {
                let comment = crate::db::insert_review_comment(
                    c,
                    crate::db::NewComment {
                        workspace_id: "ws".into(),
                        repository_id: "repo".into(),
                        author_type: crate::db::AUTHOR_HUMAN.into(),
                        author_name: "Kris".into(),
                        body: "fragile".into(),
                        ..Default::default()
                    },
                )?;
                crate::db::promote_comment_to_task(c, &comment, "fragile", None)
            })
            .expect("promote");
        assert_eq!(task.grill_state, crate::db::GRILL_PROPOSED);

        // The AGENT structures it via MCP.
        let structured = review_task_structure(
            &db,
            &json!({
                "id": task.id,
                "title": "Harden the fragile path",
                "details": "Add validation + a test.",
                "severity": "high",
            }),
        )
        .expect("structure");
        assert_eq!(structured.effects, vec![ChangedTopic::Repos]);
        assert_eq!(
            structured.result["task"]["grill_state"],
            json!("structured")
        );
        assert_eq!(structured.result["task"]["severity"], json!("high"));

        // It shows in the task list, still NOT validated (ready-for-coder is the human's call).
        let listed = review_task_list(
            &db,
            &json!({ "workspace_id": "ws", "repository_id": "repo" }),
        )
        .expect("list");
        let tasks = listed["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["grill_state"], json!("structured"));
    }

    /// DONE-CRITERION guard-rail (task 01KX0F6JEKCEV2JCTMEBMM3P4Z): the agent NEVER validates a
    /// task. Proof: the dispatcher exposes NO validate/promote tool of any name to MCP — the
    /// agent has ONLY `review_task_list` (read) + `review_task_structure` (structure).
    /// Validation (making a task ready for the coder) is reachable ONLY from the UI façade. This
    /// is the task-side twin of `mcp_agent_cannot_resolve_a_comment` (phase-4 human-only
    /// resolution).
    #[test]
    fn mcp_agent_cannot_validate_or_promote_a_task() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();

        // No validate/promote/mark-read tool of any name is dispatchable over MCP.
        for name in [
            "review_task_validate",
            "review_task_promote",
            "review_promote_comment",
            "review_mark_read",
        ] {
            let out = dispatch_extension_tool(
                &db,
                &runner,
                &host,
                &activity,
                name,
                &json!({ "id": "whatever" }),
                None,
            );
            assert!(
                out.is_none(),
                "{name} must NOT be an MCP tool — the agent never validates/promotes"
            );
        }

        // The agent CAN list + structure (the façade it IS allowed is dispatchable).
        for name in [
            crate::mcp::REVIEW_TASK_LIST_TOOL,
            crate::mcp::REVIEW_TASK_STRUCTURE_TOOL,
        ] {
            let out = dispatch_extension_tool(
                &db,
                &runner,
                &host,
                &activity,
                name,
                &json!({ "workspace_id": "ws", "repository_id": "repo", "id": "x", "title": "t" }),
                None,
            );
            assert!(out.is_some(), "{name} IS an MCP tool");
        }
    }

    /// DONE-CRITERION (task 01KXN0HTAXXMXF3GXF4ACM2970): "Creer/editer un Spec via MCP l'ajoute
    /// au set ; une lecture ne l'ajoute pas ; N Specs travailles = set de N". The auto-map:
    /// `dispatch_extension_tool`, given the CALLER TERMINAL, attributes a spec-scoped mutation
    /// to that terminal's live session and adds the worked Spec to its set — a read maps
    /// nothing, and an un-attributable call (no caller) maps nothing.
    #[test]
    fn auto_map_adds_worked_specs_to_the_caller_session_not_reads() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();

        // A project + a terminal hosting a LIVE agent session (the calling session).
        let (project_id, terminal_id) = db.with_conn(|c| {
            let (project, _root) = crate::db::create_project(c, "Proj", "/work", None).unwrap();
            let terminal = crate::db::create_terminal(c, "/work", None).unwrap();
            crate::db::record_session_start(
                c,
                &terminal.id,
                crate::db::AGENT_KIND_CLAUDE_CODE,
                crate::db::SessionCapture {
                    external_session_id: "ext-1".into(),
                    cwd: "/work".into(),
                    ..Default::default()
                },
            )
            .unwrap();
            (project.id, terminal.id)
        });
        let caller = Some(terminal_id.as_str());
        let set = |db: &Db| {
            db.with_conn(|c| crate::db::specs_for_terminal(c, &terminal_id))
                .unwrap()
        };

        // 1) spec_create attributed to the caller terminal → the Spec joins the session's set.
        let out = dispatch_extension_tool(
            &db,
            &runner,
            &host,
            &activity,
            crate::mcp_tools_spec::SPEC_CREATE_TOOL,
            &json!({ "project_id": project_id, "title": "Spec A" }),
            caller,
        )
        .expect("spec_create dispatched")
        .expect("spec_create ok");
        let spec_a = out.result["spec"]["id"].as_str().unwrap().to_string();
        assert_eq!(
            set(&db).len(),
            1,
            "creating a Spec via MCP adds it to the set"
        );
        assert_eq!(set(&db)[0].id, spec_a);

        // 2) A READ tool from the SAME caller → the set is UNCHANGED (a read never maps).
        dispatch_extension_tool(
            &db,
            &runner,
            &host,
            &activity,
            "list_projects",
            &json!({}),
            caller,
        )
        .expect("list_projects dispatched")
        .expect("ok");
        assert_eq!(set(&db).len(), 1, "a read never adds a Spec to the set");

        // 3) A SECOND Spec worked by the same session → the set is N=2.
        let out2 = dispatch_extension_tool(
            &db,
            &runner,
            &host,
            &activity,
            crate::mcp_tools_spec::SPEC_CREATE_TOOL,
            &json!({ "project_id": project_id, "title": "Spec B" }),
            caller,
        )
        .unwrap()
        .unwrap();
        let spec_b = out2.result["spec"]["id"].as_str().unwrap().to_string();
        let two = set(&db);
        assert_eq!(two.len(), 2, "N worked Specs → a set of N");
        let ids: std::collections::HashSet<&str> = two.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(spec_a.as_str()) && ids.contains(spec_b.as_str()));

        // 4) An UN-ATTRIBUTABLE call (no caller terminal) still creates a Spec but maps nothing.
        dispatch_extension_tool(
            &db,
            &runner,
            &host,
            &activity,
            crate::mcp_tools_spec::SPEC_CREATE_TOOL,
            &json!({ "project_id": project_id, "title": "Spec C" }),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            set(&db).len(),
            2,
            "a call with no caller terminal maps nothing"
        );
    }

    /// DONE-CRITERION (task 01KXN0HTXFMZAJ0GTVWVBN2PGZ): "clear vide le set (overlay disparait) ;
    /// compact et resume ne demappent pas ; nouvelle session = set vide". The un-map: a
    /// `SessionEnd reason=clear` vacates the calling session's Spec set (pushing `specs://changed`
    /// so the overlay re-pulls empty) WITHOUT ending the session, while `reason=resume` keeps the
    /// set. (`/compact` fires no `SessionEnd`, so it never reaches this path — no un-map by
    /// construction.)
    #[test]
    fn un_map_clears_the_set_on_clear_but_not_on_resume() {
        let _g = db_guard();
        let db = Db::in_memory();
        let activity = AgentActivityStore::new();

        // A project + a terminal hosting a LIVE session with TWO Specs in its set.
        let (session_id, terminal_id) = db.with_conn(|c| {
            let (project, _root) = crate::db::create_project(c, "Proj", "/work", None).unwrap();
            let terminal = crate::db::create_terminal(c, "/work", None).unwrap();
            let session = crate::db::record_session_start(
                c,
                &terminal.id,
                crate::db::AGENT_KIND_CLAUDE_CODE,
                crate::db::SessionCapture {
                    external_session_id: "ext-1".into(),
                    cwd: "/work".into(),
                    ..Default::default()
                },
            )
            .unwrap();
            let (a, _) =
                crate::db::create_spec(c, &project.id, "A", crate::db::SpecSections::empty())
                    .unwrap();
            let (b, _) =
                crate::db::create_spec(c, &project.id, "B", crate::db::SpecSections::empty())
                    .unwrap();
            crate::db::link_session_spec(c, &session.id, &a.id).unwrap();
            crate::db::link_session_spec(c, &session.id, &b.id).unwrap();
            (session.id, terminal.id)
        });
        let set = |db: &Db| {
            db.with_conn(|c| crate::db::specs_for_terminal(c, &terminal_id))
                .unwrap()
        };
        assert_eq!(
            set(&db).len(),
            2,
            "the session starts with a set of 2 Specs"
        );

        // A `/clear` (SessionEnd reason=clear) VACATES the set and pushes `specs://changed`,
        // but the session row stays ACTIVE (an internal transition — never ended).
        let cleared = agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionEnd",
                "session_id": "ext-1",
                "reason": "clear",
                "NYX_TERMINAL_ID": terminal_id,
            }),
        )
        .expect("clear handled");
        assert_eq!(
            cleared.effects,
            vec![ChangedTopic::Specs],
            "a real un-map pushes specs://changed so the overlay re-pulls empty"
        );
        assert!(
            set(&db).is_empty(),
            "clear vacated the session's Spec set (overlay disappears)"
        );
        let still_active = db
            .with_conn(|c| crate::db::get_session(c, &session_id))
            .unwrap()
            .unwrap();
        assert_eq!(
            still_active.state,
            crate::db::SESSION_STATE_ACTIVE,
            "clear keeps the session active (only the Spec set is reset)"
        );

        // Re-populate the set, then a `/resume` (SessionEnd reason=resume) must NOT un-map.
        let spec_c = db.with_conn(|c| {
            let (project, _) = crate::db::create_project(c, "P2", "/work2", None).unwrap();
            let (spec, _) =
                crate::db::create_spec(c, &project.id, "C", crate::db::SpecSections::empty())
                    .unwrap();
            crate::db::link_session_spec(c, &session_id, &spec.id).unwrap();
            spec.id
        });
        assert_eq!(set(&db).len(), 1, "the set has one Spec again");
        let resumed = agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionEnd",
                "session_id": "ext-1",
                "reason": "resume",
                "NYX_TERMINAL_ID": terminal_id,
            }),
        )
        .expect("resume handled");
        assert!(
            resumed.effects.is_empty(),
            "resume is an internal transition that does NOT un-map (no change pushed)"
        );
        let after_resume = set(&db);
        assert_eq!(after_resume.len(), 1, "resume keeps the Spec set intact");
        assert_eq!(
            after_resume[0].id, spec_c,
            "the same Spec survives a resume"
        );
    }

    /// DONE-CRITERION (task 01KWHPV33SAV9NV1STSY403CF0): the MCP `workspace_timeline` tool
    /// serves the SAME bounded read model the UI bridge consumes — a workspace's recent
    /// `domain_events` as an oldest-first, capped fil, with NO terminal byte. Proof: mutations
    /// through the shared use cases land correlated `domain_events`; the tool returns them
    /// bounded to `limit`, and every entry is a business event type (never terminal output).
    #[test]
    fn mcp_workspace_timeline_tool_serves_the_bounded_domain_events_read_model() {
        use crate::usecase::{self, test_support::RecordingEventSink, DomainEventSource};
        let _g = db_guard();
        let db = Db::in_memory();
        let sink = RecordingEventSink::new();

        let (project, _root) = db
            .with_conn(|c| {
                usecase::create_project(c, &sink, "P", "/tmp/tl-mcp", None, DomainEventSource::Mcp)
            })
            .unwrap();
        let ws = db
            .with_conn(|c| {
                usecase::register_workspace(
                    c,
                    &sink,
                    &project.id,
                    "wt",
                    "/tmp/tl-mcp-wt",
                    DomainEventSource::Mcp,
                )
            })
            .unwrap();
        db.with_conn(|c| {
            usecase::rename_workspace(c, &sink, &ws.id, "renamed", DomainEventSource::Ui)
        })
        .unwrap();

        // READ via the MCP tool (bounded, oldest-first).
        let res = workspace_timeline(&db, &json!({ "workspace_id": ws.id })).unwrap();
        let timeline = &res["timeline"];
        assert_eq!(timeline["workspace_id"], json!(ws.id));
        let entries = timeline["entries"].as_array().unwrap();
        // The workspace-scoped events surface, oldest-first, and every entry is a BUSINESS
        // event with a small identity payload — NEVER a terminal byte.
        let types: Vec<&str> = entries
            .iter()
            .map(|e| e["event_type"].as_str().unwrap())
            .collect();
        assert!(types.contains(&"workspace.created"));
        assert!(types.contains(&"workspace.renamed"));
        assert!(
            entries
                .iter()
                .all(|e| e["payload"].as_str().unwrap().contains("identity_id")),
            "every timeline entry is a business event payload, no terminal bytes"
        );

        // The `limit` bounds the read.
        let bounded =
            workspace_timeline(&db, &json!({ "workspace_id": ws.id, "limit": 1 })).unwrap();
        assert_eq!(bounded["timeline"]["limit"], json!(1));
        assert_eq!(bounded["timeline"]["entries"].as_array().unwrap().len(), 1);

        // Unknown workspace id is a D8 invalid_id error.
        let err = workspace_timeline(&db, &json!({ "workspace_id": "nope" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    #[test]
    fn create_then_list_then_close_terminal_roundtrips() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        // Create a loose terminal.
        let created = create_terminal(&db, &host, &json!({ "label": "t1" })).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        assert_eq!(created.effects, vec![ChangedTopic::Terminals]);
        // It lists as alive.
        let listed = list_terminals(&db, &host, &json!({})).unwrap();
        let arr = listed["terminals"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["terminal_id"], json!(tid));
        assert_eq!(arr[0]["status"], json!("alive"));
        // Close it → record flips, terminals changed.
        let closed = close_terminal(&db, &host, &json!({ "terminal_id": tid })).unwrap();
        assert_eq!(closed.result["closed"], json!(true));
        assert_eq!(closed.effects, vec![ChangedTopic::Terminals]);
        // Default list (alive only) is now empty.
        let after = list_terminals(&db, &host, &json!({})).unwrap();
        assert_eq!(after["terminals"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn send_to_terminal_without_live_pty_is_invalid_state_not_unknown() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        // The record exists (alive) but no live PTY → invalid_state, NOT method_not_found.
        let err = send_to_terminal(&db, &host, &json!({ "terminal_id": tid, "command": "ls" }))
            .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
    }

    #[test]
    fn send_to_terminal_unknown_id_is_invalid_id() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let err = send_to_terminal(
            &db,
            &host,
            &json!({ "terminal_id": "nope", "command": "ls" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    // --- send_keys: the PURE key resolver (named keys, ctrl+<x>, literal, mixed) -------

    #[test]
    fn resolve_keys_named_keys_map_to_control_bytes() {
        // Each named key (case-insensitive) resolves to its documented control bytes.
        assert_eq!(resolve_key_element("enter"), b"\r");
        assert_eq!(resolve_key_element("ENTER"), b"\r");
        assert_eq!(resolve_key_element("tab"), b"\t");
        assert_eq!(resolve_key_element("escape"), b"\x1b");
        assert_eq!(resolve_key_element("esc"), b"\x1b");
        assert_eq!(resolve_key_element("backspace"), b"\x7f");
        assert_eq!(resolve_key_element("space"), b" ");
        assert_eq!(resolve_key_element("up"), b"\x1b[A");
        assert_eq!(resolve_key_element("Down"), b"\x1b[B");
        assert_eq!(resolve_key_element("right"), b"\x1b[C");
        assert_eq!(resolve_key_element("left"), b"\x1b[D");
        assert_eq!(resolve_key_element("home"), b"\x1b[H");
        assert_eq!(resolve_key_element("end"), b"\x1b[F");
        assert_eq!(resolve_key_element("pageup"), b"\x1b[5~");
        assert_eq!(resolve_key_element("pagedown"), b"\x1b[6~");
        assert_eq!(resolve_key_element("delete"), b"\x1b[3~");
    }

    #[test]
    fn resolve_keys_ctrl_letter_is_generic_and_case_insensitive() {
        // The named ctrl shortcuts...
        assert_eq!(resolve_key_element("ctrl+c"), b"\x03");
        assert_eq!(resolve_key_element("ctrl+d"), b"\x04");
        assert_eq!(resolve_key_element("ctrl+u"), b"\x15");
        assert_eq!(resolve_key_element("ctrl+l"), b"\x0c");
        assert_eq!(resolve_key_element("ctrl+a"), b"\x01");
        assert_eq!(resolve_key_element("ctrl+e"), b"\x05");
        // ...are just the generic ctrl+<letter> rule (byte = letter ascii & 0x1f), which
        // also covers any other letter and is case-insensitive.
        assert_eq!(resolve_key_element("Ctrl+C"), b"\x03");
        assert_eq!(resolve_key_element("ctrl+x"), &[0x18]); // 'x' & 0x1f
        assert_eq!(resolve_key_element("ctrl+z"), &[0x1a]); // 'z' & 0x1f
    }

    #[test]
    fn resolve_keys_literal_text_passes_through_verbatim() {
        // Anything not in the vocabulary is literal text — its own UTF-8 bytes, no newline.
        assert_eq!(resolve_key_element("hello"), b"hello");
        assert_eq!(resolve_key_element("my message"), b"my message");
        // A near-miss is still literal (only an EXACT match is a named key).
        assert_eq!(resolve_key_element("enter "), b"enter ");
        assert_eq!(resolve_key_element("ctrl+"), b"ctrl+");
        assert_eq!(resolve_key_element("ctrl+cc"), b"ctrl+cc");
        // Multi-byte UTF-8 literals survive verbatim.
        assert_eq!(resolve_key_element("é"), "é".as_bytes());
    }

    #[test]
    fn resolve_keys_mixed_array_concatenates_and_adds_no_forced_newline() {
        // ["my message", "enter"] → the text bytes THEN a single \r (the explicit submit) —
        // and NOTHING else: no second/forced \r is appended by the resolver.
        let bytes = resolve_keys(&["my message".to_string(), "enter".to_string()]);
        assert_eq!(bytes, b"my message\r");

        // A bare keystroke array carries only that key's bytes (no newline).
        assert_eq!(resolve_keys(&["up".to_string()]), b"\x1b[A");
        assert_eq!(resolve_keys(&["ctrl+c".to_string()]), b"\x03");

        // Typing WITHOUT submitting: literal text alone never gains a trailing newline.
        assert_eq!(resolve_keys(&["draft".to_string()]), b"draft");

        // A richer mix resolves left-to-right into one contiguous sequence.
        let bytes = resolve_keys(&[
            "ab".to_string(),
            "left".to_string(),
            "X".to_string(),
            "enter".to_string(),
        ]);
        assert_eq!(bytes, b"ab\x1b[DX\r");
    }

    #[test]
    fn add_then_remove_command_roundtrips() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/arc", None))
            .unwrap();
        let runner = noop_runner();
        let added = add_command(
            &db,
            &json!({ "project_id": project.id, "name": "dev", "command": "pnpm dev" }),
        )
        .unwrap();
        let cmd_id = added.result["command"]["command_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(added.effects, vec![ChangedTopic::Commands]);
        // Remove it.
        let removed = remove_command(&db, &runner, &json!({ "command_id": cmd_id })).unwrap();
        assert_eq!(removed.result["removed"], json!(true));
        assert_eq!(removed.effects, vec![ChangedTopic::Commands]);
    }

    #[test]
    fn set_project_settings_renames_and_toggles_resume_and_persists() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "Old", "/tmp/sps", None))
            .unwrap();
        // A fresh project defaults to resume OFF.
        assert!(!project.resume_agent_sessions);

        // Rename + flip resume ON in one call.
        let out = set_project_settings(
            &db,
            &json!({ "project_id": project.id, "name": "New", "resume_agent_sessions": true }),
        )
        .unwrap();
        // The returned project reflects both writes…
        assert_eq!(out.result["project"]["name"], json!("New"));
        assert_eq!(out.result["project"]["resume_agent_sessions"], json!(true));
        // …and a workspaces refresh is requested so the front re-pulls.
        assert_eq!(out.effects, vec![ChangedTopic::Workspaces]);

        // The writes are PERSISTED (a fresh read sees them).
        let got = db
            .with_conn(|c| db::get_project(c, &project.id))
            .unwrap()
            .expect("project still exists");
        assert_eq!(got.name, "New");
        assert!(got.resume_agent_sessions);

        // A partial update touches only the supplied field (resume back OFF, name kept).
        set_project_settings(
            &db,
            &json!({ "project_id": project.id, "resume_agent_sessions": false }),
        )
        .unwrap();
        let after = db
            .with_conn(|c| db::get_project(c, &project.id))
            .unwrap()
            .unwrap();
        assert_eq!(after.name, "New", "omitted name is unchanged");
        assert!(!after.resume_agent_sessions, "resume toggled back OFF");
    }

    #[test]
    fn set_project_settings_unknown_id_is_invalid_id() {
        let _g = db_guard();
        let db = Db::in_memory();
        let err =
            set_project_settings(&db, &json!({ "project_id": "nope", "name": "X" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    #[test]
    fn set_project_settings_blank_name_is_invalid_argument() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "Keep", "/tmp/spsb", None))
            .unwrap();
        let err = set_project_settings(&db, &json!({ "project_id": project.id, "name": "   " }))
            .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        // The original name is untouched (the blank rename was refused before any write).
        let got = db
            .with_conn(|c| db::get_project(c, &project.id))
            .unwrap()
            .unwrap();
        assert_eq!(got.name, "Keep");
    }

    /// FINDING #27: the `project_set_repos` tool configures a project's repos EXPLICITLY,
    /// and the configured sub-repo is then SEEN by `workspace_git` (consequence #5 — the
    /// review/diff surface no longer sees only the root). Also: unknown project → invalid_id.
    #[test]
    fn mcp_project_set_repos_configures_repos_seen_by_workspace_git() {
        let _g = db_guard();
        if crate::proc_util::command("git")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping mcp project_set_repos test: git unavailable");
            return;
        }
        // A container root with a git sub-repo `api` (a multi-repo layout — the very case
        // the removed auto-detection used to guess and now the user declares explicitly).
        let root = std::env::temp_dir().join(format!("nyx-mcp-setrepos-{}", uuid::Uuid::now_v7()));
        let repo = root.join("api");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str], dir: &std::path::Path| {
            let ok = crate::proc_util::command("git")
                .args([
                    "-c",
                    "user.email=t@nyx",
                    "-c",
                    "user.name=nyx",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"], &repo);
        git(&["symbolic-ref", "HEAD", "refs/heads/main"], &repo);
        std::fs::write(repo.join("README.md"), b"nyx").unwrap();
        git(&["add", "README.md"], &repo);
        git(&["commit", "-q", "-m", "init"], &repo);

        let db = Db::in_memory();
        let (project, root_ws) = db
            .with_conn(|c| {
                crate::repository::register_project(c, "Nyx", root.to_str().unwrap(), None)
            })
            .unwrap();

        // BEFORE config: workspace_git sees only the root fallback (non-git container).
        let before = workspace_git(&db, &json!({ "workspace_id": root_ws.id })).unwrap();
        let before_repos = before["git"]["repos"].as_array().unwrap();
        assert_eq!(
            before_repos.len(),
            1,
            "fallback = the single current folder"
        );
        assert_eq!(before_repos[0]["relative_path"], json!("."));

        // Configure the `api` sub-repo via the TOOL.
        let out = project_set_repos(
            &db,
            &json!({
                "project_id": project.id,
                "repos": [{ "relative_path": "api", "name": "api" }],
            }),
        )
        .unwrap();
        assert!(
            out.effects.contains(&ChangedTopic::Repos),
            "a config mutation broadcasts a repos changed effect"
        );
        let repos = out.result["repos"].as_array().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["relative_path"], json!("api"));

        // AFTER config: workspace_git now sees the `api` sub-repo with its REAL Git state.
        std::fs::write(repo.join("new.txt"), b"new").unwrap();
        let after = workspace_git(&db, &json!({ "workspace_id": root_ws.id })).unwrap();
        let api = after["git"]["repos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["relative_path"] == json!("api"))
            .expect("api sub-repo now visible");
        assert_eq!(api["branch"], json!("main"), "the sub-repo's real branch");
        assert_eq!(api["dirty"], json!(true), "the sub-repo's real dirty state");

        // Unknown project id is a D8 invalid_id error.
        let err =
            project_set_repos(&db, &json!({ "project_id": "nope", "repos": [] })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);

        // A repo entry missing relative_path is invalid_argument.
        let err = project_set_repos(
            &db,
            &json!({ "project_id": project.id, "repos": [{ "name": "x" }] }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn probe_reports_ok() {
        let _g = db_guard();
        let db = Db::in_memory();
        let res = probe(&db).unwrap();
        assert_eq!(res["ok"], json!(true));
        assert_eq!(res["schema_ok"], json!(true));
    }

    #[test]
    fn read_terminal_unknown_id_is_invalid_id() {
        let _g = db_guard();
        let db = Db::in_memory();
        let err = read_terminal(&db, &json!({ "terminal_id": "nope" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    // --- agent_session_event: runtime ACTIVITY (the live dot) ----------------

    use crate::agent_activity::{Activity, AgentActivityStore};

    /// A `UserPromptSubmit` hook drives the runtime activity to `working` (the live dot)
    /// WITHOUT touching the persisted `agent_sessions` table, and emits the
    /// `agent-sessions` change topic so the front re-pulls. The terminal need not have a
    /// DB session row — activity is independent of the session lifecycle.
    #[test]
    fn agent_session_event_prompt_marks_working_and_changes() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        let out = agent_session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        assert_eq!(out.effects, vec![ChangedTopic::AgentSessions]);
        assert_eq!(out.result["activity"], json!(true));
        assert!(activity.snapshot(&tid).unwrap().activity == Activity::Working);
    }

    /// A `Stop` hook drives activity back to `idle` AND raises the focus-aware "response
    /// ready" notification (the green dot), again over the runtime store only.
    #[test]
    fn agent_session_event_stop_idles_and_raises_ready() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        agent_session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        agent_session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "Stop", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(snap.activity, Activity::Idle);
        assert!(
            snap.ready_unread,
            "Stop raises the response-ready notification"
        );
    }

    /// `SubagentStart`/`SubagentStop` move the in-flight counter but must NOT lower the
    /// main running dot: a sub-agent finishing is not the main turn finishing, so the dot
    /// stays `working` (and is never raised to `ready`). The best-effort hook never errors.
    #[test]
    fn agent_session_event_subagent_lifecycle_keeps_main_turn_working() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        agent_session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        agent_session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "SubagentStart", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        assert!(activity.snapshot(&tid).unwrap().activity == Activity::Working);
        let out = agent_session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "SubagentStop", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        // A recognized activity hook → change topic emitted (the front re-pulls), but the
        // resolved dot is still working (the main turn never ended).
        assert_eq!(out.result["activity"], json!(true));
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(snap.activity, Activity::Working);
        assert!(!snap.ready_unread, "a sub-agent stop never raises ready");
    }

    /// THE #21 FIX over the MCP boundary: a BACKGROUND sub-agent whose hook order is
    /// `SubagentStart` → `Stop` → `SubagentStop`. The `Stop` must NOT green the dot while the
    /// background sub-agent is still in flight — the dot stays `working` until the trailing
    /// `SubagentStop`, which then idles + raises ready. A SYNCHRONOUS sub-agent
    /// (`SubagentStart` → `SubagentStop` → `Stop`) greens normally at the `Stop`.
    #[test]
    fn agent_session_event_background_subagent_defers_ready_until_subagent_stop() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        let send = |args: serde_json::Value| agent_session_event(&db, &activity, &args).unwrap();

        // --- BACKGROUND: SubagentStart → Stop → SubagentStop ---
        send(json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }));
        send(json!({ "hook_event_name": "SubagentStart", "NYX_TERMINAL_ID": tid }));
        // The main turn ends while the background sub-agent runs: still working, NO green.
        send(json!({ "hook_event_name": "Stop", "NYX_TERMINAL_ID": tid }));
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(
            snap.activity,
            Activity::Working,
            "Stop with a background sub-agent in flight stays working — no premature green"
        );
        assert!(
            !snap.ready_unread,
            "the green is deferred while the background runs"
        );
        // The background sub-agent finishes → NOW idle + ready.
        send(json!({ "hook_event_name": "SubagentStop", "NYX_TERMINAL_ID": tid }));
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(
            snap.activity,
            Activity::Idle,
            "the last sub-agent settles the turn"
        );
        assert!(
            snap.ready_unread,
            "the green is raised on the trailing SubagentStop"
        );

        // --- SYNCHRONOUS: SubagentStart → SubagentStop → Stop (greens at the Stop) ---
        send(json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }));
        send(json!({ "hook_event_name": "SubagentStart", "NYX_TERMINAL_ID": tid }));
        send(json!({ "hook_event_name": "SubagentStop", "NYX_TERMINAL_ID": tid }));
        // Sub-agent already gone → the Stop idles + greens immediately.
        assert_eq!(activity.snapshot(&tid).unwrap().activity, Activity::Working);
        send(json!({ "hook_event_name": "Stop", "NYX_TERMINAL_ID": tid }));
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(snap.activity, Activity::Idle);
        assert!(
            snap.ready_unread,
            "a synchronous sub-agent greens at the Stop"
        );
    }

    /// A single long-running tool keeps the dot `working` between its `PreToolUse` and
    /// `PostToolUse` with NO timer, and `AskUserQuestion` drives the `waiting` (yellow)
    /// state end-to-end through the MCP tool. Also covers `permission_prompt` → waiting and
    /// `idle_prompt` → idle+ready (the notification discrimination via the payload).
    #[test]
    fn agent_session_event_tool_counter_and_waiting_states() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        let send = |args: serde_json::Value| agent_session_event(&db, &activity, &args).unwrap();

        send(json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }));
        // A long tool: PreToolUse with no Post → stays working across reads.
        send(
            json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash", "NYX_TERMINAL_ID": tid }),
        );
        assert_eq!(activity.snapshot(&tid).unwrap().activity, Activity::Working);
        send(
            json!({ "hook_event_name": "PostToolUse", "tool_name": "Bash", "NYX_TERMINAL_ID": tid }),
        );
        assert_eq!(activity.snapshot(&tid).unwrap().activity, Activity::Working);

        // AskUserQuestion → waiting (yellow), then its Post resumes working.
        send(
            json!({ "hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "NYX_TERMINAL_ID": tid }),
        );
        assert_eq!(activity.snapshot(&tid).unwrap().activity, Activity::Waiting);
        send(
            json!({ "hook_event_name": "PostToolUse", "tool_name": "AskUserQuestion", "NYX_TERMINAL_ID": tid }),
        );
        assert_eq!(activity.snapshot(&tid).unwrap().activity, Activity::Working);

        // A permission prompt → waiting.
        send(
            json!({ "hook_event_name": "Notification", "notification_type": "permission_prompt", "NYX_TERMINAL_ID": tid }),
        );
        assert_eq!(activity.snapshot(&tid).unwrap().activity, Activity::Waiting);

        // idle_prompt → idle + ready (a turn end via Notification).
        send(
            json!({ "hook_event_name": "Notification", "notification_type": "idle_prompt", "NYX_TERMINAL_ID": tid }),
        );
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(snap.activity, Activity::Idle);
        assert!(
            snap.ready_unread,
            "idle_prompt raises the ready notification"
        );

        // StopFailure also finishes the turn (a new prompt then a StopFailure).
        send(json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }));
        let out = send(json!({ "hook_event_name": "StopFailure", "NYX_TERMINAL_ID": tid }));
        assert_eq!(out.result["activity"], json!(true));
        assert_eq!(activity.snapshot(&tid).unwrap().activity, Activity::Idle);

        // A Notification of an unknown type is acknowledged as a no-op (no error, no
        // change), leaving the dot where it was.
        let out = send(
            json!({ "hook_event_name": "Notification", "notification_type": "weird_unknown", "NYX_TERMINAL_ID": tid }),
        );
        assert!(out.effects.is_empty());
        assert_eq!(out.result["activity"], json!(false));
    }

    /// #26 — the "chat about this" decline on an `AskUserQuestion` must NOT leave the dot
    /// stuck yellow. Drives the WHOLE flow through the real hook routing (`from_hook` +
    /// `apply`): a question raises `Waiting`, then the agent resumes working via a tool hook
    /// (the empirically-uncertain resume path) and the dot must read `Working`, not `Waiting`.
    /// Covered both for a tool's `PreToolUse` and a `PostToolUse`.
    #[test]
    fn agent_session_event_decline_question_clears_waiting() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        let send = |args: serde_json::Value| agent_session_event(&db, &activity, &args).unwrap();

        // A question blocks the agent → yellow.
        send(json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }));
        send(
            json!({ "hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "NYX_TERMINAL_ID": tid }),
        );
        assert_eq!(activity.snapshot(&tid).unwrap().activity, Activity::Waiting);
        // "chat about this": the agent resumes by running a tool — the dot must clear to blue
        // even though no AskUserQuestion PostToolUse arrived.
        send(
            json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash", "NYX_TERMINAL_ID": tid }),
        );
        assert_eq!(
            activity.snapshot(&tid).unwrap().activity,
            Activity::Working,
            "a tool starting after a declined question clears the stuck yellow (#26)"
        );

        // And via a UserPromptSubmit resume path (a fresh chat message) on a NEW terminal.
        let created2 = create_terminal(&db, &host, &json!({})).unwrap();
        let tid2 = created2.result["terminal_id"].as_str().unwrap().to_string();
        send(json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid2 }));
        send(
            json!({ "hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "NYX_TERMINAL_ID": tid2 }),
        );
        assert_eq!(
            activity.snapshot(&tid2).unwrap().activity,
            Activity::Waiting
        );
        send(json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid2 }));
        assert_eq!(
            activity.snapshot(&tid2).unwrap().activity,
            Activity::Working,
            "a new prompt after a declined question clears the stuck yellow (#26)"
        );
    }

    /// A clean `SessionEnd` clears the runtime activity too (the clean-end anti-phantom
    /// reflex): a session ending mid-`working` drops the live dot. Drives a real
    /// SessionStart→prompt→SessionEnd over a terminal with a DB session row.
    #[test]
    fn agent_session_event_session_end_clears_activity() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        // Record the session (so SessionEnd has a row to end).
        agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-1",
                "cwd": "/work",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        // A prompt → working.
        agent_session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        assert!(activity.snapshot(&tid).unwrap().activity == Activity::Working);
        // SessionEnd → clears the runtime activity (no phantom).
        agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionEnd",
                "session_id": "sid-1",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        assert!(
            activity.snapshot(&tid).is_none(),
            "SessionEnd clears the live dot"
        );
    }

    /// End-to-end (finding 01KWMCT58832JZW7M5GQDYZZ4B): the `agent_session_event` MCP tool
    /// emits a correlated `agent_session.started` / `agent_session.ended` `domain_events` row
    /// for a WORKSPACE-attached terminal — atomically with the `agent_sessions` mutation — so
    /// the workspace timeline surfaces the session lifecycle. Payload carries the business
    /// identity only, NEVER a terminal/transcript byte.
    #[test]
    fn agent_session_event_emits_timeline_domain_events_for_a_workspace_session() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();

        // A project + workspace + a terminal ATTACHED to that workspace.
        let (project, root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/tl-sess", None))
            .unwrap();
        let created = create_terminal(&db, &host, &json!({ "cwd": "/tmp/tl-sess" })).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        db.with_conn(|c| db::attach_terminal(c, &tid, &root.id, "auto"))
            .unwrap();
        let baseline = db.with_conn(db::list_domain_events).unwrap().len();

        // SessionStart → `agent_session.started`.
        agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "ext-sid-1",
                "cwd": "/tmp/tl-sess",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        // SessionEnd (a real end — logout) → `agent_session.ended`.
        agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionEnd",
                "session_id": "ext-sid-1",
                "reason": "logout",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();

        let events = db.with_conn(db::list_domain_events).unwrap();
        let new: Vec<_> = events[baseline..].iter().collect();
        let types: Vec<&str> = new.iter().map(|e| e.event_type.as_str()).collect();
        assert!(types.contains(&"agent_session.started"), "got {types:?}");
        assert!(types.contains(&"agent_session.ended"), "got {types:?}");
        for e in &new {
            assert_eq!(e.workspace_id.as_deref(), Some(root.id.as_str()));
            assert_eq!(e.project_id, project.id);
            assert!(!e.payload.contains("\\x1b"), "no terminal escape bytes");
            assert!(!e.payload.to_lowercase().contains("transcript"));
        }

        // The timeline read model surfaces both.
        let tl = db
            .with_conn(|c| crate::workspace_timeline::timeline(c, &root.id, 50))
            .unwrap()
            .unwrap();
        let kinds: Vec<&str> = tl.entries.iter().map(|e| e.event_type.as_str()).collect();
        assert!(kinds.contains(&"agent_session.started"));
        assert!(kinds.contains(&"agent_session.ended"));
    }

    /// SessionStart returns a MINIMAL nyx `context` line (#22) in `structuredContent` so the
    /// hook can inject it as `additionalContext`. A LOOSE terminal (no workspace) yields
    /// `You're in nyx — terminal_id=<id>` (no project/workspace segment).
    #[test]
    fn agent_session_event_start_returns_minimal_nyx_context() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        let out = agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-ctx",
                "cwd": "/work",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        let ctx = out.result["context"]
            .as_str()
            .expect("SessionStart carries a `context` line");
        assert!(
            ctx.starts_with("You're in nyx"),
            "context opens with the nyx orientation: {ctx}"
        );
        assert!(
            ctx.contains(&format!("terminal_id={tid}")),
            "context names the terminal id: {ctx}"
        );
        assert!(
            !ctx.contains("workspace_id="),
            "a loose terminal has no workspace segment: {ctx}"
        );
    }

    /// The #22 ENRICHMENT: SessionStart on a WORKSPACE-bound terminal carries the
    /// `project_id` + `workspace_id` (and names) in the context line, so the agent can call
    /// e.g. `list_commands(workspace_id=…)` directly — no `list_projects`/`list_workspaces`
    /// discovery first.
    #[test]
    fn agent_session_event_start_context_carries_project_and_workspace_ids() {
        use crate::schema::terminals;
        use diesel::prelude::*;
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let (project, workspace) = db
            .with_conn(|c| db::create_project(c, "sandbox", "/tmp/pb", None))
            .unwrap();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        // Bind the terminal to the project's root workspace.
        db.with_conn(|c| {
            diesel::update(terminals::table.find(&tid))
                .set(terminals::workspace_id.eq(Some(workspace.id.clone())))
                .execute(c)
        })
        .unwrap();

        let out = agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-ws",
                "cwd": "/tmp/pb",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        let ctx = out.result["context"].as_str().unwrap();
        assert!(
            ctx.contains(&format!("project_id={}", project.id)),
            "context carries the project_id: {ctx}"
        );
        assert!(
            ctx.contains(&format!("workspace_id={}", workspace.id)),
            "context carries the workspace_id: {ctx}"
        );
        assert!(
            ctx.contains("project \"sandbox\""),
            "context names the project: {ctx}"
        );
    }

    /// #18b — the STALE-PLUGIN verdict at SessionStart. A reported `plugin_version` that
    /// DIFFERS from the bundled (expected) version flags the terminal `plugin_outdated`
    /// (runtime, on the activity store + in the result); the CURRENT bundled version does
    /// NOT; and a MISSING reported version is treated as unknown ⇒ NOT stale. The expected
    /// version is sourced from the bundled `plugin.json` at runtime (the single source of
    /// truth) — this test reads it the SAME way so it never hard-codes a number to drift.
    #[test]
    fn agent_session_event_start_flags_stale_plugin() {
        let _g = db_guard();
        // The expected version is parsed from the bundled manifest (must resolve for the
        // check to be meaningful).
        let expected = EXPECTED_PLUGIN_VERSION
            .as_deref()
            .expect("bundled plugin.json carries a version");

        // 1) A STALE (older/different) reported version → flagged outdated.
        {
            let db = Db::in_memory();
            let host = NoTerminalHost;
            let activity = AgentActivityStore::new();
            let created = create_terminal(&db, &host, &json!({})).unwrap();
            let tid = created.result["terminal_id"].as_str().unwrap().to_string();
            let out = agent_session_event(
                &db,
                &activity,
                &json!({
                    "hook_event_name": "SessionStart",
                    "session_id": "sid-stale",
                    "cwd": "/work",
                    "NYX_TERMINAL_ID": tid,
                    "plugin_version": "0.0.1-old",
                }),
            )
            .unwrap();
            assert_eq!(
                out.result["plugin_outdated"],
                json!(true),
                "a stale reported version flags the session outdated"
            );
            assert!(
                activity.snapshot(&tid).unwrap().plugin_outdated,
                "the stale verdict is recorded on the runtime activity store"
            );
        }

        // 2) The CURRENT bundled version → NOT outdated.
        {
            let db = Db::in_memory();
            let host = NoTerminalHost;
            let activity = AgentActivityStore::new();
            let created = create_terminal(&db, &host, &json!({})).unwrap();
            let tid = created.result["terminal_id"].as_str().unwrap().to_string();
            let out = agent_session_event(
                &db,
                &activity,
                &json!({
                    "hook_event_name": "SessionStart",
                    "session_id": "sid-current",
                    "cwd": "/work",
                    "NYX_TERMINAL_ID": tid,
                    "plugin_version": expected,
                }),
            )
            .unwrap();
            assert_eq!(
                out.result["plugin_outdated"],
                json!(false),
                "the current bundled version is not outdated"
            );
            // No outdated entry materialized for a current (and otherwise idle) session.
            assert!(
                activity.snapshot(&tid).map(|s| s.plugin_outdated) != Some(true),
                "a current plugin leaves no stale badge"
            );
        }

        // 3) A MISSING reported version (an OLD hook that does not report it) → unknown ⇒
        // NOT outdated (must never be flagged on the absence of the field).
        {
            let db = Db::in_memory();
            let host = NoTerminalHost;
            let activity = AgentActivityStore::new();
            let created = create_terminal(&db, &host, &json!({})).unwrap();
            let tid = created.result["terminal_id"].as_str().unwrap().to_string();
            let out = agent_session_event(
                &db,
                &activity,
                &json!({
                    "hook_event_name": "SessionStart",
                    "session_id": "sid-unknown",
                    "cwd": "/work",
                    "NYX_TERMINAL_ID": tid,
                }),
            )
            .unwrap();
            assert_eq!(
                out.result["plugin_outdated"],
                json!(false),
                "a missing reported version is unknown ⇒ not stale"
            );
        }
    }

    /// THE `/clear` ICON FIX: a `SessionEnd { reason: "clear" }` is an INTERNAL transition
    /// (the session is immediately re-opened on the SAME terminal), so it must NOT mark the
    /// row `ended` and must NOT drop the runtime activity — otherwise the active-session row
    /// blinks out for the gap until the following SessionStart lands and the sidebar icon
    /// falls back to the generic terminal glyph (the "icône qui saute" bug). After the
    /// clear-end the terminal is STILL the host of an active session and STILL working.
    #[test]
    fn agent_session_event_clear_keeps_session_active_and_working() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        // A live session + an in-flight turn (the dot is on).
        agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-1",
                "cwd": "/work",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        agent_session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();

        // `/clear` → SessionEnd { reason: clear }. The guard keeps everything live.
        let out = agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionEnd",
                "session_id": "sid-1",
                "reason": "clear",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        assert_eq!(out.result["ended"], json!(false));
        assert!(
            out.effects.is_empty(),
            "an internal transition emits no agent-sessions change (no blink)"
        );
        // The DB row is STILL active → the icon never falls back.
        let actives = db.with_conn(db::active_agent_sessions).unwrap();
        assert!(
            actives.iter().any(|a| a.terminal_id == tid),
            "the session stays active across a /clear"
        );
        // The runtime activity is STILL working → the dot never blinks.
        assert_eq!(
            activity.snapshot(&tid).unwrap().activity,
            Activity::Working,
            "the live dot survives a /clear transition"
        );

        // The following SessionStart { source: clear } refreshes the row in place (new id),
        // keeping ONE active row — not two.
        agent_session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-2",
                "source": "clear",
                "cwd": "/work",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        let actives = db.with_conn(db::active_agent_sessions).unwrap();
        assert_eq!(
            actives.iter().filter(|a| a.terminal_id == tid).count(),
            1,
            "still exactly one active session after the clear→start cycle"
        );
    }

    /// A [`TerminalHost`] that RECORDS which terminal PTYs it was asked to close, so a test
    /// can assert the destructive `remove_workspace` cascade killed the workspace's live
    /// terminals via the host (review 01KWEX11, done_criterion #2).
    #[derive(Default)]
    struct RecordingHost {
        closed: std::sync::Mutex<Vec<String>>,
    }
    impl TerminalHost for RecordingHost {
        fn park_opening_command(&self, _: &str, _: &str) {}
        fn send_to_terminal(&self, _: &str, _: &str) -> Result<bool, String> {
            Ok(false)
        }
        fn send_keys(&self, _: &str, _: &[u8]) -> Result<bool, String> {
            Ok(false)
        }
        fn close_terminal_pty(&self, terminal_id: &str) {
            self.closed.lock().unwrap().push(terminal_id.to_string());
        }
        fn terminal_liveness(&self, _: &str) -> (bool, Option<bool>) {
            (false, None)
        }
    }

    /// Review 01KWEX11 done_criterion #2 — the MCP `remove_workspace` DOES NOT refuse while a
    /// command is live: it KILLS the workspace's running command (the runner stops it → idle)
    /// AND closes its terminal PTYs (via the host), then removes the workspace + its full
    /// runtime. Proves the "refuse if running" guard is REPLACED by "tuer puis supprimer".
    #[test]
    #[cfg(unix)]
    fn remove_workspace_kills_live_runtime_instead_of_refusing() {
        use std::time::{Duration, Instant};
        let _g = db_guard();
        let db = Db::in_memory();

        // Project + a non-root workspace + a template → one materialized instance in the ws.
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/rmw-kill", None))
            .unwrap();
        let ws = db
            .with_conn(|c| db::create_workspace(c, &project.id, "wt", "/tmp/rmw-kill-wt"))
            .unwrap();
        db.with_conn(|c| {
            db::create_template(
                c,
                &project.id,
                "dev",
                "sleep 120",
                None,
                db::CommandSource::default(),
            )
        })
        .unwrap();
        let instance_ids = db
            .with_conn(|c| db::instance_ids_for_workspace(c, &ws.id))
            .unwrap();
        let instance_id = instance_ids.first().expect("materialized instance").clone();

        // A terminal attached to the workspace (its PTY must be closed by the host).
        let terminal = db
            .with_conn(|c| db::create_terminal(c, "/tmp/rmw-kill-wt", None))
            .unwrap();
        db.with_conn(|c| db::attach_terminal(c, &terminal.id, &ws.id, "auto"))
            .unwrap();

        // GENUINELY start the instance so it is LIVE in the runner.
        let runner = noop_runner();
        runner
            .start(&instance_id, "sleep 120", None)
            .expect("start");
        let deadline = Instant::now() + Duration::from_secs(5);
        while runner.state_of(&instance_id) != RunState::Running && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            runner.state_of(&instance_id),
            RunState::Running,
            "the instance is live before removal"
        );

        // ACT: remove the workspace. The former guard would have REFUSED here.
        let host = RecordingHost::default();
        let outcome = remove_workspace(&db, &runner, &host, &json!({ "workspace_id": ws.id }))
            .expect("remove_workspace must NOT refuse a workspace with a live command");
        assert_eq!(outcome.result["removed"], json!(true));

        // The live command was KILLED (runner stopped it → idle), not left running.
        assert_eq!(
            runner.state_of(&instance_id),
            RunState::Idle,
            "the live command was killed by the runner"
        );
        // The workspace's terminal PTY was asked to close (kill the interactive shell).
        assert_eq!(
            *host.closed.lock().unwrap(),
            vec![terminal.id.clone()],
            "the workspace's terminal PTY was closed via the host"
        );
        // The workspace + its runtime rows are gone.
        assert!(
            db.with_conn(|c| db::get_workspace(c, &ws.id))
                .unwrap()
                .is_none(),
            "workspace row removed"
        );
        assert!(
            db.with_conn(|c| db::instance_ids_for_workspace(c, &ws.id))
                .unwrap()
                .is_empty(),
            "command instances cascaded"
        );
        assert!(
            db.with_conn(|c| db::get_terminal(c, &terminal.id))
                .unwrap()
                .is_none(),
            "the workspace's terminal row was deleted (not detached into the loose bucket)"
        );
    }

    /// A bogus hook (neither an activity hook nor a session event) still errors as before
    /// — the activity path does not swallow genuinely unrecognizable payloads. (Note:
    /// `PreToolUse` is now a recognized ACTIVITY hook, so it is NOT a valid bogus name; we
    /// use a hook name Claude never emits.)
    #[test]
    fn agent_session_event_unrecognized_payload_still_errors() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        let err = agent_session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "TotallyUnknownHook", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
    }
}
