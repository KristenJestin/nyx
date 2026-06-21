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
}

impl ChangedTopic {
    /// The wire string the shell maps to its event (`terminals` / `workspaces` /
    /// `commands` / `agent-sessions`).
    pub fn as_str(self) -> &'static str {
        match self {
            ChangedTopic::Terminals => "terminals",
            ChangedTopic::Workspaces => "workspaces",
            ChangedTopic::Commands => "commands",
            ChangedTopic::AgentSessions => "agent-sessions",
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
    fn read(result: Value) -> Self {
        Self {
            result,
            effects: vec![],
        }
    }
    fn changed(result: Value, topic: ChangedTopic) -> Self {
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

/// `list_projects` — `{}` → `{ projects }`.
pub fn list_projects(db: &Db) -> Result<Value, RpcError> {
    let projects = db.with_conn(db::list_projects).map_err(internal_db)?;
    Ok(json!({ "projects": projects }))
}

/// `list_workspaces` — `{ project_id, cwd? }` → `{ workspaces }`. `cwd` is the OPTIONAL
/// filter; each returned workspace's `branch` is resolved LIVE. Parity with the Tauri
/// `list_workspaces`.
pub fn list_workspaces(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let cwd = optional_str(args, "cwd")?;
    let mut workspaces = db
        .with_conn(|c| db::list_workspaces(c, project_id))
        .map_err(internal_db)?;
    if let Some(cwd) = cwd {
        let needle = crate::pathnorm::normalize(cwd);
        workspaces.retain(|w| path_matches(&w.path, &needle));
    }
    for w in &mut workspaces {
        w.branch = db::detect_branch(&w.path);
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
            (ws.project_id, vec![ws.path])
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
            let paths = workspaces.into_iter().map(|w| w.path).collect();
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

/// `remove_workspace` — `{ workspace_id }` → `{ removed, removed_instances }`. Refused on
/// the root workspace, or while any instance runs. Parity with the Tauri `remove_workspace`.
pub fn remove_workspace<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let workspace = db
        .with_conn(|c| db::get_workspace(c, workspace_id))
        .map_err(internal_db)?;
    match workspace {
        None => {
            return Err(RpcError::new(
                RpcCode::InvalidId,
                format!("unknown workspace {workspace_id}"),
            ))
        }
        Some(ws) if ws.is_root => {
            return Err(RpcError::new(
                RpcCode::InvalidState,
                format!(
                    "workspace {workspace_id} is the project's root — it cannot be removed on \
                     its own; delete the whole project instead"
                ),
            ))
        }
        Some(_) => {}
    }
    let instance_ids = db
        .with_conn(|c| db::instance_ids_for_workspace(c, workspace_id))
        .map_err(internal_db)?;
    if runner.any_running(&instance_ids) {
        return Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "workspace {workspace_id} has a running command — stop it before removing the \
                 workspace"
            ),
        ));
    }
    let removed_instances = instance_ids.len();
    let deleted = db
        .with_conn(|c| db::delete_workspace(c, workspace_id))
        .map_err(internal_db)?;
    if deleted == 0 {
        return Err(RpcError::new(
            RpcCode::InvalidId,
            format!("unknown workspace {workspace_id}"),
        ));
    }
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

/// `agent_session_event` — the Claude Code SessionStart/SessionEnd hook target. Parity
/// with the Tauri `agent_session_event` (over the shared `agent`/`db` layer).
pub fn agent_session_event(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let agent_kind = optional_str(args, "agent_kind")?.unwrap_or(db::AGENT_KIND_CLAUDE_CODE);
    let registry = AgentRegistry::default();
    let adapter = registry.get(agent_kind).ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown agent_kind '{agent_kind}'"),
        )
    })?;
    let terminal_id = require_str(args, "NYX_TERMINAL_ID")?;
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
                    },
                )
                .map_err(internal_db)?;
            let Some(session) = session else {
                return Err(RpcError::new(
                    RpcCode::InvalidId,
                    format!("unknown terminal {terminal_id}"),
                ));
            };
            Ok(ToolOutcome::changed(
                json!({
                    "event": "SessionStart",
                    "session_id": session.id,
                    "terminal_id": session.terminal_id,
                    "agent_kind": session.agent_kind,
                    "external_session_id": session.external_session_id,
                    "state": session.state,
                    "workspace_id": session.workspace_id,
                }),
                ChangedTopic::AgentSessions,
            ))
        }
        AgentEvent::End(end) => {
            let outcome = db
                .with_conn(
                    |c| -> Result<Option<(String, bool)>, diesel::result::Error> {
                        let Some(active) = db::active_session_for(c, terminal_id, agent_kind)?
                        else {
                            return Ok(None);
                        };
                        if active.external_session_id != end.external_session_id {
                            return Ok(Some((active.id, false)));
                        }
                        db::mark_session_ended(c, &active.id)?;
                        Ok(Some((active.id, true)))
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
            path: w.path,
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
    name: &str,
    args: &Value,
) -> Option<Result<ToolOutcome, RpcError>> {
    // Read tools wrap their plain JSON result into a no-effect `ToolOutcome`.
    let read = |r: Result<Value, RpcError>| Some(r.map(ToolOutcome::read));
    match name {
        // Pure DB reads.
        crate::mcp::PROBE_TOOL => read(probe(db)),
        "list_projects" => read(list_projects(db)),
        "list_workspaces" => read(list_workspaces(db, args)),
        "list_commands" => read(list_commands(db, runner, args)),
        crate::mcp::LIST_IMPORTABLE_SCRIPTS_TOOL => read(list_importable_scripts(db, args)),
        // Command-template CRUD (mutating → carry a `changed` effect).
        crate::mcp::ADD_COMMAND_TOOL => Some(add_command(db, args)),
        crate::mcp::UPDATE_COMMAND_TOOL => Some(update_command(db, runner, args)),
        crate::mcp::IMPORT_COMMANDS_TOOL => Some(import_commands(db, args)),
        crate::mcp::REMOVE_WORKSPACE_TOOL => Some(remove_workspace(db, runner, args)),
        crate::mcp::REMOVE_COMMAND_TOOL => Some(remove_command(db, runner, args)),
        crate::mcp::REMOVE_COMMANDS_TOOL => Some(remove_commands(db, runner, args)),
        crate::mcp::CLEAR_COMMAND_OUTPUT_TOOL => Some(clear_command_output(db, runner, args)),
        // Agent-session channel.
        crate::mcp::AGENT_SESSION_EVENT_TOOL => Some(agent_session_event(db, args)),
        // Interactive-terminal tools (DB-record half + the shell's TerminalHost).
        crate::mcp::CREATE_TERMINAL_TOOL => Some(create_terminal(db, host, args)),
        crate::mcp::SEND_TO_TERMINAL_TOOL => Some(send_to_terminal(db, host, args)),
        crate::mcp::LIST_TERMINALS_TOOL => read(list_terminals(db, host, args)),
        crate::mcp::CLOSE_TERMINAL_TOOL => Some(close_terminal(db, host, args)),
        crate::mcp::READ_TERMINAL_TOOL => read(read_terminal(db, args)),
        // Not one of ours (runtime / workspace tools live in mcp_runtime).
        _ => None,
    }
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
}
