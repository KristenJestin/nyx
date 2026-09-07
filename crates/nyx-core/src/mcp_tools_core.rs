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

use diesel::sqlite::SqliteConnection;
use serde_json::{json, Value};

use crate::agent::{AgentEvent, AgentRegistry};
use crate::agent_activity::{ActivityEvent, AgentActivityStore, HookFacts};
use crate::command::{CommandRunner, RunState, RunnerSink};
use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{
    internal_db, map_content_write_err, optional_bool, optional_env_map, optional_str,
    optional_usize, require_str, status_json,
};

/// The default `tail_bytes` window for the output/scrollback reads — the token-safe
/// 12 KiB the Tauri dispatcher uses.
pub const DEFAULT_TAIL_BYTES: usize = 12 * 1024;
/// Hard ceiling on a single output/scrollback window: a request for more than 1 MiB is
/// refused with `output_too_large` rather than served.
pub const MAX_TAIL_BYTES: usize = 1024 * 1024;

/// The pause a [`TerminalHost`] leaves BETWEEN two consecutive `send_keys` chunks.
///
/// Segmenting the writes is only half the fix: a `write`+`flush` pair issued immediately after
/// another can still land in the SAME `read()` on the child side (both the Unix line discipline
/// and ConPTY hand over whatever has accumulated), which glues `"texte"` and `"\r"` back into the
/// one chunk Ink misreads as a paste. A short gap makes the child's read return between them.
///
/// Sized to be invisible to a human (a 3-element `keys` array costs ~2× this) while being an
/// eternity for a process that is already parked in `read()`.
pub const KEY_CHUNK_PACING: std::time::Duration = std::time::Duration::from_millis(12);

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
    /// The IDEAS backlog changed (v2.6 — carried by the `idea_*` MCP tools so any open
    /// Idées view re-pulls after an agent capture / maintenance mutation). The wire string
    /// is `ideas`; the front's re-pull on it is wired in Phase 4 (the Knowledge UI), but the
    /// mutation authority NAMES the topic here so the seam is single-authority end to end.
    Ideas,
    /// The ORCHESTRATION graph changed (v2.8 — carried by the `workflow_*` / `node_*` authoring
    /// MCP tools so any open Engine visualiser re-pulls the re-built tree after an agent authoring
    /// mutation). The wire string is `workflows`; the front's re-pull on it is wired in Phase 6
    /// (the React Flow graph UI), but the mutation authority NAMES the topic here so the seam is
    /// single-authority end to end (the same forward-declared pattern as `Specs` / `Ideas`).
    Workflows,
    /// The PROTOTYPE annexe changed (v2.7 — carried by the `proto_*` MCP tools so any open
    /// proto view re-pulls after an agent action: present a round, elect a variant, raise or
    /// resolve a comment). The wire string is `prototypes`; the front's re-pull on it (and the
    /// future proto overlay, PDR-0009) is a later phase, but the mutation authority NAMES the
    /// topic here so the seam is single-authority end to end.
    Prototypes,
    // NO `SystemApprovals` VARIANT, and its absence is the honest shape (review
    // 01M15QP3HVYV9Q729ZYTNJB8T2, finding 01M15RFKAPVSBCA9XGZP4DZCYC).
    //
    // This enum names what a SUCCESSFUL mutation changed — it rides on `ToolOutcome::effects`, and
    // effects exist only on the `Ok` path. The Système approval queue moves on the opposite event:
    // a protected call mutates nothing and comes back as `approval_required`, and THAT is when the
    // human has something to do. A variant here could therefore never be constructed, and it was
    // not: both real emitters spell the topic from
    // [`crate::system_approvals::SYSTEM_APPROVALS_CHANGED_TOPIC`] directly — the MCP dispatcher off
    // the refusal's code, and the UI's grant/deny routes after a decision. That constant is the
    // single authority for the wire string; a second, unreachable one would only be a place for the
    // two to drift.
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
            ChangedTopic::Ideas => "ideas",
            ChangedTopic::Workflows => "workflows",
            ChangedTopic::Prototypes => "prototypes",
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
    /// A mutating tool that touches MORE THAN ONE domain names every topic it invalidated, so the
    /// shell pushes each `*://changed` (`emit_effects` iterates them). Used by the review FINDING
    /// mutations, which touch BOTH the review feed (`Repos`, the viewer) AND the Spec's task list
    /// (`Specs`, the terminal-spec overlay: a finding is a `tasks kind=review` row).
    pub(crate) fn changed_many(result: Value, effects: Vec<ChangedTopic>) -> Self {
        Self { result, effects }
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
    /// Called by `create_terminal` for a terminal opened with a `command`, BEFORE
    /// [`Self::open_terminal_shell`] — that spawn is what drains the park.
    fn park_opening_command(&self, terminal_id: &str, command: &str);

    /// START the record's shell NOW, without waiting for anything on screen.
    ///
    /// The shell's front end mounts a terminal pane only for the project it is DISPLAYING, and
    /// that mount is what spawns the PTY — so a terminal an agent opened anywhere else (another
    /// project, or loose) never got a shell at all: it sat in the sidebar as an empty record,
    /// `send_to_terminal` answered `invalid_state` forever, and the opening command stayed parked
    /// for a spawn that would only come if the human happened to switch to that project. An agent
    /// driving nyx has to be able to open a session where the work is, so `create_terminal` asks
    /// for the shell itself instead of hoping a pane appears.
    ///
    /// `cwd` is the record's stored cwd; `"."` means no directory was asked for — the shell's own
    /// default. A host that owns no PTY does nothing (the record exists, with no shell).
    fn open_terminal_shell(&self, terminal_id: &str, cwd: &str);

    /// Write `command + "\r"` into the terminal's live shell (the SAME path as
    /// `pty_write`). Returns `Ok(true)` when written, `Ok(false)` when the terminal has
    /// no live PTY (unknown/closed/not-yet-spawned), or `Err` on a real write failure.
    fn send_to_terminal(&self, terminal_id: &str, command: &str) -> Result<bool, String>;

    /// Write the RAW key `chunks` into the terminal's live shell WITHOUT any appended newline
    /// (the `send_keys` path). Returns `Ok(true)` when written, `Ok(false)` when the terminal
    /// has no live PTY (unknown/closed/not-yet-spawned), or `Err` on a real write failure. Same
    /// liveness contract as [`Self::send_to_terminal`]; the ONLY difference is no forced `\r`.
    ///
    /// # Why a LIST of chunks and not one flat buffer (the submit bug)
    ///
    /// A raw-mode TUI does not read bytes, it reads **chunks**: Ink (Claude Code, and every
    /// other Ink app) classifies each `data` event from stdin AS A WHOLE — a chunk that is not
    /// exactly one known key sequence is treated as PASTED TEXT, and a `\r` at the end of it
    /// becomes a literal newline inside the input box instead of a submit. So a single
    /// `write_all(b"texte\r")` types the prompt and never sends it, while a `\r` arriving ALONE
    /// (a one-byte chunk) submits immediately — measured on the 2026-09-01 pilot run, where
    /// `send_keys(["texte","enter"])` answered `sent:true` and left the text sitting in the box.
    ///
    /// The FIX is therefore about GRANULARITY, not about the bytes: [`send_keys`] resolves the
    /// `keys` array into one chunk PER ELEMENT, and a host writes each chunk with its own
    /// `write` + `flush` so the named keys reach the application on their own. Delivery is
    /// still verbatim and nothing is added — `enter` is still `\r` (0x0D), never `\n`.
    ///
    /// A host must also **space the writes out** by [`KEY_CHUNK_PACING`]: two flushes issued
    /// back to back can still be coalesced into ONE `read()` on the child side, which would put
    /// the flat buffer back together and re-open the very bug. The pacing is the host's job
    /// because only the host owns the real PTY (the DB half of the tool has no I/O to pace).
    fn send_keys(&self, terminal_id: &str, chunks: &[Vec<u8>]) -> Result<bool, String>;

    /// Kill the terminal's live PTY if one is registered (the SAME path as `pty_close`),
    /// and drop any parked opening command. Idempotent — a no-op when nothing is live.
    fn close_terminal_pty(&self, terminal_id: &str);

    /// Close SEVERAL terminals' live PTYs — the BULK twin of [`Self::close_terminal_pty`],
    /// used by the `remove_workspace` teardown.
    ///
    /// It exists because a close is not free: the sidecar host raises each pump's explicit
    /// close flag and then JOINS that pump, and an idle pump only reads the flag when it wakes
    /// from its poll — so a LOOP of `close_terminal_pty` pays that window once per terminal,
    /// serially. A host that can signal every terminal BEFORE joining the first pump overrides
    /// this to pay it once for the whole set (the sidecar's `PtyManager::close_records`).
    ///
    /// The default is the loop, so a host with no bulk path (and every test double) keeps
    /// working unchanged — and the semantics are identical either way, only the waiting differs.
    fn close_terminal_ptys(&self, terminal_ids: &[String]) {
        for id in terminal_ids {
            self.close_terminal_pty(id);
        }
    }

    /// The live `(live, busy)` bits for a terminal record: `live` = a PTY is registered
    /// (its shell started), `busy` = a command is running in the foreground (the OS dot
    /// authority), or `None` when it cannot be derived. A record with no live PTY → `(false,
    /// None)`. Used to enrich `list_terminals`.
    fn terminal_liveness(&self, terminal_id: &str) -> (bool, Option<bool>);

    /// Whether the record's PERSISTED scrollback is BEHIND its live shell: output has arrived
    /// that has not been saved into `terminals.scrollback` yet.
    ///
    /// The persisted scrollback is written by the SHELL's front end, and only for a terminal it
    /// currently has mounted — so a live terminal the user is not looking at keeps running into a
    /// buffer nobody updates. `read_terminal` reads that buffer, so without this bit it would hand
    /// back an arbitrarily old snapshot with nothing marking it as past. `false` for a record with
    /// no live PTY (nothing is producing) and for a host that owns no PTY at all.
    fn scrollback_is_behind(&self, terminal_id: &str) -> bool;
}

/// A [`TerminalHost`] that owns no live PTY — every terminal looks closed/idle and a
/// `send_to_terminal` finds no live shell. Lets a shell that has not wired its PTY
/// manager (or a test) still serve the terminal tools' DB half without a `None` branch:
/// the record-level operations work, and the live-PTY operations degrade to the same
/// `invalid_state` an unknown-but-not-yet-spawned terminal would give.
pub struct NoTerminalHost;

impl TerminalHost for NoTerminalHost {
    fn park_opening_command(&self, _terminal_id: &str, _command: &str) {}
    fn open_terminal_shell(&self, _terminal_id: &str, _cwd: &str) {}
    fn send_to_terminal(&self, _terminal_id: &str, _command: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn send_keys(&self, _terminal_id: &str, _chunks: &[Vec<u8>]) -> Result<bool, String> {
        Ok(false)
    }
    fn close_terminal_pty(&self, _terminal_id: &str) {}
    fn terminal_liveness(&self, _terminal_id: &str) -> (bool, Option<bool>) {
        (false, None)
    }
    fn scrollback_is_behind(&self, _terminal_id: &str) -> bool {
        false
    }
}

// --- Argument helpers specific to this module ------------------------------

/// A CLEARABLE optional string argument — the three-state read a partial-update tool needs:
/// `None` (omitted ⇒ leave it alone), `Some(None)` (explicit `null` ⇒ CLEAR it) and
/// `Some(Some(s))` (a value ⇒ set it). Distinct from [`optional_str`], whose `null → None` would
/// make "clear it" and "leave it alone" the same request on a setting whose absence is a state.
/// Anything that is not a string or `null` is `invalid_argument`.
fn clearable_str<'a>(args: &'a Value, key: &str) -> Result<Option<Option<&'a str>>, RpcError> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => Ok(Some(Some(s.as_str()))),
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("argument '{key}' must be a string (or null to clear it)"),
        )),
    }
}

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
pub(crate) fn require_str_vec(args: &Value, key: &str) -> Result<Vec<String>, RpcError> {
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
/// CONNECTION-SCOPED, like every command-CRUD helper below it.
///
/// The protected Système path builds its errors from INSIDE the transaction the claim shares with
/// the mutation ([`crate::system_approvals::claim_and_mutate`]), where a `Db::with_conn` would ask
/// the pool for a SECOND connection — and the in-memory pool the tests run on holds exactly one.
/// Every `*_on_conn` twin in this file exists for that same reason, and each public `fn(db, …)` is
/// now a one-line `with_conn` over its twin, so the two surfaces cannot drift.
fn bad_command_id_error_on_conn(conn: &mut SqliteConnection, id: &str) -> RpcError {
    let is_instance = db::get_instance(conn, id).ok().flatten().is_some();
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
    // `env_keys`, never the env itself: an env VALUE is a secret by default (the runner's
    // "values are never logged" invariant), and the KEYS are what an agent needs to see —
    // what the command declares, and whether its last write landed (N31).
    let mut env_keys: Vec<String> = crate::command::decode_command_env(t.env.as_deref())
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    env_keys.sort();
    json!({
        "command_id": t.id,
        "project_id": t.project_id,
        "name": t.name,
        "command": t.command,
        "subfolder": t.subfolder,
        "source_kind": t.source_kind,
        "package_manager": t.package_manager,
        "env_keys": env_keys,
        // The group this command belongs to, or `null` when it is SOLO — the READ side of
        // `manage_command_group` (review finding `01M11XM66VTB30VCZRA0RMPMRP`). Without it the only
        // way to observe the grouping was to MUTATE it and read the `groups` of the reply, which is
        // no way for an agent to inspect before acting.
        "group_id": t.group_id,
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
            // A DIVERGENCE and a schema merely BEHIND need different words: "restart nyx to apply
            // them" is actionable for the second and misleading for the first — nothing will be
            // applied, because the registry already records the migration whose object is gone.
            let warning = health.drift_refusal().unwrap_or_else(|| {
                "schema has pending migrations — restart nyx to apply them".to_string()
            });
            map.insert("schema_warning".to_string(), json!(warning));
            match health.pending_count {
                Some(count) => {
                    map.insert("pending_migrations".to_string(), json!(count));
                }
                None => {
                    map.insert("schema_check_failed".to_string(), json!(true));
                }
            }
            if !health.missing_objects.is_empty() {
                map.insert(
                    "missing_schema_objects".to_string(),
                    json!(health
                        .missing_objects
                        .iter()
                        .map(|missing| missing.object.clone())
                        .collect::<Vec<_>>()),
                );
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
        // The use case can refuse (the Système singleton is not a USER container to enumerate);
        // `From<SystemScopeError>` carries that as `system_scope_forbidden`, a DB failure as
        // `internal` — never as `invalid_id`, which would claim the project does not exist.
        .map_err(RpcError::from)?;
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
///
/// THE GROUPING IS READABLE HERE (review finding `01M11XM66VTB30VCZRA0RMPMRP`). Every command entry
/// carries its `group_id` (`null` = solo), and the `{ project_id }` form additionally returns the
/// project's `groups` — the SAME `db::CommandGroup` rows, from the SAME `db::list_command_groups`
/// reader, that `manage_command_group` echoes after a mutation, so the read surface and the write
/// surface cannot describe two different worlds. Before this, `manage_command_group`'s description
/// pointed agents HERE to inspect the state and neither this tool nor `workspace_runtime` carried a
/// single byte of membership: the only way to observe the grouping was to provoke a WRITE and read
/// the `groups` of its reply.
///
/// The `{ workspace_id }` form lists INSTANCES, which are per workspace while a group is project
/// data; each instance carries its template's `group_id` (`null` = solo) so membership is readable
/// WITHOUT a second call — and still names its `command_id`, so an agent that wants the group's
/// name, colour and member order resolves those with one `{ project_id }` call.
pub fn list_commands<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<Value, RpcError> {
    if let Some(workspace_id) = optional_str(args, "workspace_id")? {
        // The NATURE of every entry of this listing, resolved ONCE: the instances of one workspace
        // all share that workspace, so they all share its container's nature.
        let nature = db
            .with_conn(|c| crate::system_scope::is_system_workspace(c, workspace_id))
            .map_err(internal_db)?;
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
                    // The multi-pane GROUP the instance's template belongs to (`null` = solo). The
                    // ambient session context names it on every grouped command so the agent can
                    // start a whole disposition with ONE `start_command { group_id, workspace_id }`
                    // — so the tool the context sends it back to must carry it too, or "go check
                    // list_commands" would mean "go read something poorer".
                    "group_id": row.group_id,
                    "nature": command_nature(nature),
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
        // Same nature, read from the container itself this time — one project, one answer.
        let nature = db
            .with_conn(|c| crate::system_scope::is_system_project(c, project_id))
            .map_err(internal_db)?;
        let templates = db
            .with_conn(|c| db::list_templates(c, project_id))
            .map_err(internal_db)?;
        let commands: Vec<Value> = templates
            .iter()
            .map(|t| {
                let mut entry = template_json(t);
                if let Some(map) = entry.as_object_mut() {
                    map.insert("nature".to_string(), json!(command_nature(nature)));
                }
                entry
            })
            .collect();
        // The project's command GROUPS, each with its id, name, colour and its member ids in
        // DISPOSITION order — the read half of `manage_command_group`, from the very reader that
        // tool echoes after a mutation.
        let groups = db
            .with_conn(|c| db::list_command_groups(c, project_id))
            .map_err(internal_db)?;
        return Ok(json!({ "commands": commands, "groups": groups }));
    }
    Err(RpcError::new(
        RpcCode::InvalidArgument,
        "list_commands requires either workspace_id (instances) or project_id (templates)",
    ))
}

/// The `nature` an agent-facing command entry carries: which KIND of container it belongs to.
///
/// The SAME vocabulary as [`db::PROJECT_KINDS`] and not a second one, because it answers the same
/// question — `projects.kind` is the single source of truth for a container's nature, and this is
/// that value travelling to the wire rather than a parallel label. Every authorized entry of
/// `list_commands` carries it, and every entry of `list_system_commands` carries `system`: one
/// reader tells a project command from a Système service without a second call.
fn command_nature(is_system: bool) -> &'static str {
    if is_system {
        db::PROJECT_KIND_SYSTEM
    } else {
        db::PROJECT_KIND_USER
    }
}

/// `list_system_commands` — `{}` → `{ scope, commands, groups }`. THE agent's dedicated discovery
/// of the Système scope's commands (PRD « Scope Système »).
///
/// ## Why it exists at all
///
/// The Système container is a singleton project deliberately ABSENT from `list_projects` /
/// `list_workspaces` — it is not a container an agent may work IN (no Spec, no repository, no
/// workspace lifecycle), and offering it there would invite exactly the operations the domain
/// boundary refuses. But its COMMANDS have to be discoverable: they are the user's cross-project
/// services, and an agent that cannot name `services` can neither read its log nor start it. So
/// discovery gets its own door, and that door hands back everything needed to ACT on a command
/// through the ordinary runtime tools — the launchable `instance_id` above all — and nothing else
/// of the scope.
///
/// ## The one thing that ALSO names them, and why that is not a second door
///
/// The ambient `SessionStart` context lists the Système commands next to the session's own
/// (`session_context_line`, review `01M1EJ84VCWRD6F4NDVD2M0QWJ`): an agent that can act on them has
/// to be TOLD they exist and that they are shared, and waiting for it to guess this tool's name was
/// the failure the user observed on a real agent. So this door is no longer the only place the
/// names appear — a deviation from "discovery gets its own door", assumed and written down (see
/// [`crate::mcp_context::session_commands_block`], which spells out the trade in full).
///
/// It stays the DOOR because the block deliberately stops at `instance_id`: `scope.project_id` and
/// `scope.workspace_id` — the pair `add_command` and a whole-group `start_command` take — come from
/// HERE and from nowhere else, as does anything past the block's cap. And nothing about either path
/// changes what an id is worth: the matrix authorizes the same verbs whichever way it was learned.
///
/// ## What it does NOT open
///
/// The Système TERMINALS. They are absent from this answer, absent from `list_terminals`, and
/// refused by id on `read`/`send_to`/`send_keys`/`close_terminal` (`refuse_system_terminal`).
/// Discovering the commands is not a step towards them.
///
/// Nor does it grant anything: an id learned here goes through the very same authorization as an
/// id learned anywhere else. The scope is never defended by hiding an id
/// ([`crate::system_scope::guard_payload`]), which is precisely what makes it safe to hand these
/// ones over.
///
/// ## Shape
///
/// `scope` names the container (`project_id`, `workspace_id`) and the `home` every Système command
/// runs from — the workspace's `root_path`, so what is advertised here IS what the runner will use.
/// `commands` reuses the entry shape of `list_commands(workspace_id=…)` (same fields, same live
/// status overlay from the runner) plus `group_id` and `nature`. `groups` is the SAME
/// `db::list_command_groups` reader `list_commands(project_id=…)` and `manage_command_group`
/// return, so the three cannot describe different worlds.
///
/// An un-bootstrapped scope is an `internal` error, not an empty answer: `Db::open` always
/// bootstraps, so its absence is a failure of nyx and saying "there are no Système commands" would
/// be a lie an agent could act on.
pub fn list_system_commands<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    _args: &Value,
) -> Result<Value, RpcError> {
    let scope = db
        .with_conn(crate::system_scope::load)
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::from(crate::system_scope::SystemScopeError::NotBootstrapped)
        })?;

    // The instances of the ONE workspace — the same reader, the same entry shape and the same live
    // overlay `list_commands(workspace_id=…)` serves, so an agent that knows one knows the other.
    let mut listed = list_commands(
        db,
        runner,
        &json!({ "workspace_id": scope.workspace.id }),
    )?;
    let commands = listed
        .get_mut("commands")
        .map(Value::take)
        .unwrap_or_else(|| json!([]));
    let groups = db
        .with_conn(|c| db::list_command_groups(c, &scope.project.id))
        .map_err(internal_db)?;
    // The GROUP each command belongs to, folded onto its entry: the instance listing is per
    // workspace while a group is project data, and making the agent re-join them by hand is the
    // shape `list_commands` already regretted.
    let commands: Vec<Value> = commands
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut entry| {
            let group_id = entry
                .get("command_id")
                .and_then(Value::as_str)
                .and_then(|cid| {
                    groups
                        .iter()
                        .find(|g| g.command_ids.iter().any(|m| m == cid))
                        .map(|g| g.id.clone())
                });
            if let Some(map) = entry.as_object_mut() {
                map.insert("group_id".to_string(), json!(group_id));
            }
            entry
        })
        .collect();

    Ok(json!({
        "scope": {
            "project_id": scope.project.id,
            "workspace_id": scope.workspace.id,
            "name": scope.project.name,
            "home": scope.home(),
        },
        "commands": commands,
        "groups": groups,
    }))
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
    let (offset, limit) = crate::mcp_tools_spec::page_args(args)?;
    let overlay = |instance_id: &str| runner.outcome(instance_id);
    let snapshot = db
        .with_conn(|c| crate::workspace_runtime::snapshot(c, workspace_id, &overlay))
        .map_err(internal_db)?;
    let Some(runtime) = snapshot else {
        return Err(RpcError::new(
            RpcCode::InvalidId,
            format!(
                "unknown workspace {workspace_id} (use a workspace_id from \
                 list_workspaces(project_id=…))"
            ),
        ));
    };
    Ok(json!({ "runtime": runtime_for_agent(&runtime, offset, limit) }))
}

/// PROJECT the shared runtime snapshot for an AGENT — review A10.
///
/// The snapshot is a UI read model and the UI is right to want it whole: it renders one workspace on
/// screen and holds the bytes anyway. An MCP caller pays for every byte of it in context, and what it
/// was handed was the entire history inline — ~35 TERMINATED agent sessions, each terminal record's
/// full `scrollback` (up to 256 KiB apiece), each service's `scrollback` and its retained prior run —
/// with no bound and no way to ask for less. Measured on the audit's workspace: a single call cost
/// more than the whole of `tools/list`.
///
/// This is the discipline phase 4 posed for `tools/list` and `read_terminal`, applied here:
///
///   * NO SCROLLBACK IS INLINED. Not a terminal's, not a service's, not a retained prior run's. Each
///     is replaced by its BYTE LENGTH, so a caller can see there is output and knows whether it is
///     worth a read — and the reads that serve it (`read_terminal`, `get_command_output`) are one
///     call away, bounded, resumable, and are what phase 4 rewrote around the gesture that works.
///   * WHAT IS LIVE COMES FIRST. Agent sessions are ordered active-first, so a default call — one
///     page, no arguments — is about the workspace as it stands, not about its archaeology.
///   * AN ENDED SESSION CARRIES ITS POINTER, NOT ITS PAYLOAD. `transcript_path` (phase 1) already
///     names the file holding its full output; `metadata_json` is an adapter bag nobody reads from a
///     runtime view, so it is dropped for a session that has ended and kept for a live one.
///   * THE REST IS PAGED with the SAME grammar as every other paged read of this surface —
///     [`crate::mcp_tools_spec::page_of`] itself, not a lookalike: `offset` / `limit` in,
///     `agent_sessions_total` / `_returned` / `_next_offset` / `_truncated` out.
fn runtime_for_agent(
    runtime: &crate::workspace_runtime::WorkspaceRuntimeSnapshot,
    offset: usize,
    limit: usize,
) -> Value {
    let terminals: Vec<Value> = runtime
        .terminals
        .iter()
        .map(|t| {
            json!({
                "terminal_id": t.terminal.id,
                "label": t.terminal.label,
                "cwd": t.terminal.cwd,
                "status": t.terminal.status,
                "exec_state": t.terminal.exec_state,
                "exec_exit_code": t.terminal.exec_exit_code,
                "exec_state_unread": t.terminal.exec_state_unread,
                // NULL, not `false`: this snapshot carries no live overlay (only the UI bridge
                // applies `apply_terminal_live_overlay`), so the OS busy bit is UNKNOWN here — the
                // same `null` list_terminals reports, never a negative asserted as a fact.
                "busy": Value::Null,
                "detected_agent_kind": Value::Null,
                // The BYTES, not the bytes: read_terminal(terminal_id=…) serves them bounded.
                "scrollback_bytes": t.terminal.scrollback.len(),
                "live_sessions": t
                    .sessions
                    .iter()
                    .filter(|s| s.state == db::SESSION_STATE_ACTIVE)
                    .count(),
            })
        })
        .collect();
    let services: Vec<Value> = runtime
        .services
        .iter()
        .map(|s| {
            json!({
                "instance_id": s.instance.id,
                "name": s.instance.name,
                "command": s.instance.command,
                "run_state": s.run_state,
                "exit_code": s.exit_code,
                "unread": s.unread,
                "group_id": s.instance.group_id,
                // Same rule as a terminal's: get_command_output(instance_id=…) serves them.
                "scrollback_bytes": s.instance.scrollback.len(),
            })
        })
        .collect();

    // LIVE FIRST, then the ended ones most-recently-started first: a default page is about now.
    let mut sessions: Vec<&db::AgentSession> = runtime.agent_sessions.iter().collect();
    sessions.sort_by(|a, b| {
        let live = |s: &db::AgentSession| s.state == db::SESSION_STATE_ACTIVE;
        live(b)
            .cmp(&live(a))
            .then_with(|| b.started_at.cmp(&a.started_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    // Every row goes through `bounded_row`: `page_of` pages under the STRICT budget, whose "at
    // least one item fits" guarantee — and so "the paging can never stall" — holds only when each
    // item is itself bounded. A live session's `metadata_json` is caller-supplied text.
    let rows: Vec<Value> = sessions
        .iter()
        .map(|s| {
            let live = s.state == db::SESSION_STATE_ACTIVE;
            crate::mcp_runtime::bounded_row(&json!({
                "session_id": s.id,
                "terminal_id": s.terminal_id,
                "agent_kind": s.agent_kind,
                "external_session_id": s.external_session_id,
                "state": s.state,
                "cwd": s.cwd,
                "started_at": s.started_at,
                "ended_at": s.ended_at,
                "last_seen_at": s.last_seen_at,
                // The POINTER to the full output, always — this is what phase 1 exposed it for.
                "transcript_path": s.transcript_path,
                // The adapter bag is carried only while the session is LIVE: on an ended one it is
                // history nobody reads from a runtime view, and there are dozens of them.
                "metadata_json": if live { json!(s.metadata_json) } else { Value::Null },
            }))
        })
        .collect();

    let head = json!({
        "workspace": {
            "workspace_id": runtime.workspace.id,
            "project_id": runtime.workspace.project_id,
            "name": runtime.workspace.name,
            "root_path": runtime.workspace.root_path,
            "is_root": runtime.workspace.is_root,
        },
        "terminals": terminals,
        "services": services,
        "live_agent_sessions": runtime
            .agent_sessions
            .iter()
            .filter(|s| s.state == db::SESSION_STATE_ACTIVE)
            .count(),
    });
    let page = crate::mcp_tools_spec::page_of(
        "agent_sessions",
        rows,
        offset,
        limit,
        crate::mcp_runtime::json_len(&head),
    );
    let mut result = head;
    if let (Some(result), Some(page)) = (result.as_object_mut(), page.as_object()) {
        for (key, value) in page {
            // Prefixed for the same reason `spec_get` prefixes its own: a bare `total` beside a
            // workspace, its terminals and its services says nothing about WHAT was counted.
            let key = if key == "agent_sessions" {
                key.clone()
            } else {
                format!("agent_sessions_{key}")
            };
            result.insert(key, value.clone());
        }
    }
    result
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
    let spec_id = crate::mcp_context::optional_spec_arg(db, args, "spec_id")?;
    let spec_id = spec_id.as_deref();
    let comments = db
        .with_conn(|c| crate::db::list_review_comments(c, workspace_id, repository_id, spec_id))
        .map_err(internal_db)?;
    Ok(json!({ "comments": comments }))
}

/// `review_comment_post` — `{ workspace_id, repository_id, body, spec_id?, parent_id?,
/// file_path?, side?, line_start?, line_end?, blob?, snippet?, context? }` → `{ comment }`. The
/// v2.4 AGENT façade to annotate a review (task `01KX0F6HEBPK34CV49EREHK3Y9`). The author is
/// ALWAYS `agent` — the human posts through the UI. The agent may leave a global/anchored
/// comment OR a REPLY (`parent_id`), and may resolve ITS OWN (agent) comments via
/// [`review_comment_resolve`]; a HUMAN comment stays human-only. Routes through the SHARED
/// [`crate::db::insert_review_comment`] (PDR-0005). An anchor is captured when a `file_path` is
/// given; `line_end` defaults to `line_start`. Carries a `Repos` changed effect so an open
/// review UI re-pulls the comment feed live (the agent's reply appears without a reload).
pub fn review_comment_post(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let repository_id = require_str(args, "repository_id")?;
    let body = require_str(args, "body")?;
    let spec_id = crate::mcp_context::optional_spec_arg(db, args, "spec_id")?;
    let spec_id = spec_id.as_deref();
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
        .map_err(map_content_write_err)?;
    Ok(ToolOutcome::changed(
        json!({ "comment": comment }),
        ChangedTopic::Repos,
    ))
}

/// `review_comment_resolve` — `{ id }` → `{ comment }`. The AGENT resolves ITS OWN
/// (`author_type=agent`) comment (v2.5.11 review follow-up finding `01KXZYWGBQEJZ9BM39NYGFKHMJ`). A
/// comment is resolved BY ITS AUTHOR: this closes an agent comment (stamped `resolved_by_type=agent`)
/// and REFUSES a HUMAN comment (`invalid_state`) — a human comment is human-only, closed through the
/// human/UI façade (`review_comment_resolve` db-command). Routes through the SHARED
/// [`crate::db::resolve_agent_comment`]. Carries a `Repos` changed effect so an open review UI
/// re-pulls the feed live.
pub fn review_comment_resolve(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let id = require_str(args, "id")?;
    let comment = db
        .with_conn(|c| crate::db::resolve_agent_comment(c, id))
        .map_err(|e| match e {
            crate::db::ResolveAgentCommentError::NotFound => {
                RpcError::new(RpcCode::InvalidId, format!("unknown review comment {id}"))
            }
            crate::db::ResolveAgentCommentError::HumanOnly => RpcError::new(
                RpcCode::InvalidState,
                "this comment is human-authored: its resolution is HUMAN-ONLY. You resolve only \
                 your OWN (agent) comments; reply to a human comment instead — the human validates \
                 and closes it.",
            ),
            crate::db::ResolveAgentCommentError::Db(e) => internal_db(e),
        })?;
    Ok(ToolOutcome::changed(
        json!({ "comment": comment }),
        ChangedTopic::Repos,
    ))
}

/// `review_task_list` — `{ workspace_id, repository_id, spec_id? }` → `{ tasks }`. The review
/// FINDING read model, cut over to the CANONICAL `tasks kind=review` model (v2.5.11 Phase 2,
/// PDR-0016 — NOT the deprecated `review_tasks` table): the GRILLED findings of a Spec, each with
/// its `from_comment_id`, `title`, optional `severity`, and `grill_state`
/// (proposed/structured; `structured` = coder-ready). Routes through the SHARED
/// [`crate::db::list_grilled_findings_for_spec`], projected into the viewer wire shape
/// ([`crate::db::ReviewFinding`]) — the SAME list the UI bridge reads. Read-only. Findings are
/// SPEC-scoped, so the agent passes the `spec_id` it is reviewing; an unscoped call (no `spec_id`)
/// returns an empty list (there is no repo-wide finding bucket once findings are `tasks`).
pub fn review_task_list(db: &Db, args: &Value) -> Result<Value, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let repository_id = require_str(args, "repository_id")?;
    let spec_id = crate::mcp_context::optional_spec_arg(db, args, "spec_id")?;
    let spec_id = spec_id.as_deref();
    let tasks: Vec<crate::db::ReviewFinding> = match spec_id {
        Some(sid) => {
            // The Spec's plan, read ONCE for the whole list, so each finding can carry the
            // `phase_name` its numeric rank stands for (#K13) at the cost of a single read.
            let plan = db
                .with_conn(|c| crate::db::get_spec(c, sid))
                .ok()
                .flatten()
                .map(|s| crate::db::parse_phase_plan(&s.phases))
                .unwrap_or_default();
            db.with_conn(|c| crate::db::list_grilled_findings_for_spec(c, sid))
                .map_err(internal_db)?
                .iter()
                .map(|t| crate::db::ReviewFinding::from_task(t, workspace_id, repository_id, &plan))
                .collect()
        }
        None => Vec::new(),
    };
    Ok(json!({ "tasks": tasks }))
}

/// `review_task_structure` — `{ id, title, details?, severity? }` → `{ task }`. The AGENT's
/// step of the promotion "grill" (task `01KX0F6JEKCEV2JCTMEBMM3P4Z`) — and its FINAL step: the agent
/// captured the retour raw (a `proposed` finding), STRUCTURES it into a clean coder task (title +
/// details + optional severity) and it moves to `structured`, which is READY for the coder. The grill
/// collapsed to two states (review follow-up #31): there is no `validated` state and DELIBERATELY no
/// validate tool. Routes through the SHARED [`crate::db::structure_finding`] — the CANONICAL finding
/// model (`tasks kind=review`, v2.5.11 cutover, PDR-0016; NOT the deprecated `review_tasks` table).
/// Carries `Repos` + `Specs` changed effects so BOTH the open review UI (viewer, `repos://changed`)
/// AND the terminal-spec OVERLAY (`specs://changed` — a finding is a `tasks kind=review` row the
/// overlay renders) re-pull the updated finding WITHOUT a reload (review F3). `invalid_id` when the
/// finding is unknown or not a `kind=review` finding.
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
        .with_conn(|c| crate::db::structure_finding(c, id, title, details, severity))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!("no structurable finding {id} (unknown or not a review finding)"),
            )
        })?;
    Ok(ToolOutcome::changed_many(
        // Through `task_json_with_plan`, like every other task on the wire (#K13): a raw `db::Task`
        // carries the phase RANK and no `phase_name`, so an agent that had just written "Wiring"
        // read `phase: 2` back and `phase_name: undefined`.
        json!({ "task": crate::mcp_tools_spec::task_json_with_plan(db, &task) }),
        vec![ChangedTopic::Repos, ChangedTopic::Specs],
    ))
}

/// `review_submit_feedback` — `{ workspace_id, repository_id, spec_id, body, title?, severity? }` →
/// `{ comment, finding }`. The v2.5.11 RETOUR-CHAT channel (task `01KXR1CZ68C5S9CY2KFB073KDM`): the
/// agent RELAYS the human's chat feedback into the review. It writes a HUMAN, GLOBAL (non-anchored)
/// `review_comments` row — the human instructed the agent directly, so presence = authorization, no
/// diff anchor needed — then immediately GRILLS it into a `proposed` `tasks kind=review` finding
/// (agent-owned, closable by the coder loop like any finding) linked back via `from_comment_id`.
/// This gives a chat retour the SAME path to review as the diff viewer (which stays a valid channel
/// in parallel).
/// Routes through the SHARED [`crate::db::insert_review_comment`] + [`crate::db::promote_comment_to_finding`].
/// The comment is `author_type=human` even though it comes through the MCP (agent) façade — that is
/// the whole point: the human is the author, the agent is only the relay. Carries `Repos` + `Specs`
/// changed effects so BOTH the open review UI (viewer, `repos://changed` — the comment feed) AND the
/// terminal-spec OVERLAY (`specs://changed` — the new finding is a `tasks kind=review` row it renders)
/// re-pull WITHOUT a reload (review F3: a chat retour appears in the overlay live). `title` defaults to
/// the feedback `body`.
pub fn review_submit_feedback(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let workspace_id = require_str(args, "workspace_id")?;
    let repository_id = require_str(args, "repository_id")?;
    let spec_id = crate::mcp_context::require_spec_arg(db, args, "spec_id")?;
    let spec_id = spec_id.as_str();
    let body = require_str(args, "body")?;
    let title = optional_str(args, "title")?;
    let severity = optional_str(args, "severity")?;
    // OPTIONAL explicit phase attribution (F5): a positive phase number that OVERRIDES the stored
    // `reviewing_phase` / derived last-delivered phase when rattaching the finding. Omitted = the
    // default precedence applies.
    let phase_override = optional_usize(args, "phase")?.map(|n| n as i32);

    let (comment, finding) = db
        .with_conn(
            |c| -> Result<(crate::db::ReviewComment, crate::db::Task), crate::db::ContentWriteError> {
                use diesel::Connection;
                // The comment + its finding commit ATOMICALLY (the inner `create_task` transaction
                // nests as a savepoint) — a chat retour never leaves an orphan comment on failure.
                c.transaction(|c| {
                    // 1. The human's chat feedback → a HUMAN, GLOBAL (no anchor) review comment.
                    let comment = crate::db::insert_review_comment(
                        c,
                        crate::db::NewComment {
                            workspace_id: workspace_id.to_string(),
                            repository_id: repository_id.to_string(),
                            spec_id: Some(spec_id.to_string()),
                            parent_id: None,
                            author_type: crate::db::AUTHOR_HUMAN.to_string(),
                            author_name: "You".to_string(),
                            // GLOBAL: no anchor (file_path/line null) — presence = authorization.
                            anchor: None,
                            body: body.to_string(),
                        },
                    )?;
                    // 2. Grill it into a `proposed` finding (agent-owned; the human provenance is on
                    //    the comment). The optional `phase` overrides the finding's phase attribution (F5).
                    let seed = title.unwrap_or(body);
                    let finding = crate::db::promote_comment_to_finding_on_phase(
                        c,
                        &comment,
                        seed,
                        severity,
                        phase_override,
                    )?;
                    Ok((comment, finding))
                })
            },
        )
        .map_err(map_content_write_err)?;

    Ok(ToolOutcome::changed_many(
        // The finding goes out through `task_json_with_plan` — one form of a task on the wire,
        // `phase_name` included (#K13).
        json!({
            "comment": comment,
            "finding": crate::mcp_tools_spec::task_json_with_plan(db, &finding),
        }),
        vec![ChangedTopic::Repos, ChangedTopic::Specs],
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

/// Decode the optional `env` argument of `add_command`/`update_command` into what to WRITE in
/// `managed_commands.env` (v44, N31). Two nested options, because a partial update needs three
/// answers, not two:
///   - `None` — the argument is ABSENT: leave the stored env exactly as it is;
///   - `Some(None)` — an explicit `null` or an EMPTY object `{}`: CLEAR the column (the command
///     goes back to the inherited environment alone);
///   - `Some(Some(json))` — the serialized KEY→VALUE object to store.
///
/// The parse is the STRICT one ([`optional_env_map`]): a non-object, or ANY non-string value, is
/// `invalid_argument`. This is the WRITE boundary, the one place a caller mistake can still be
/// named — past it the reader is deliberately total, so a stored env can never make a command
/// unresolvable. SECRET HYGIENE: a refusal names the offending KEY and never echoes its value.
fn optional_env_column(args: &Value, key: &str) -> Result<Option<Option<String>>, RpcError> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(_) => {
            let pairs = optional_env_map(args, key)?;
            if pairs.is_empty() {
                return Ok(Some(None));
            }
            let map: serde_json::Map<String, Value> = pairs
                .into_iter()
                .map(|(k, v)| (k, Value::from(v)))
                .collect();
            Ok(Some(Some(Value::Object(map).to_string())))
        }
    }
}

/// `add_command` — `{ project_id, name, command, subfolder?, env? }` → `{ command }`. Parity
/// with the Tauri `add_command` (reuses `pkgjson::infer_command_source` + `db::create_template`).
///
/// `env` persists the command's OWN environment (N31): the base every spawn path applies — an
/// agent start/relaunch, the boot restore, and a `gate_check` nyx runs itself. Omitted → none.
pub fn add_command(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    db.with_conn(|c| add_command_on_conn(c, args))
}

/// The CONNECTION-SCOPED body of [`add_command`] — see [`bad_command_id_error_on_conn`] for why
/// every protected command tool has one.
pub(crate) fn add_command_on_conn(
    conn: &mut SqliteConnection,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let name = require_str(args, "name")?;
    let command = require_str(args, "command")?;
    let subfolder = optional_str(args, "subfolder")?;
    let env = optional_env_column(args, "env")?;
    let (source_kind, package_manager) = crate::pkgjson::infer_command_source(command, None, None);
    let source = db::CommandSource {
        source_kind,
        source_package_json_path: None,
        source_script_name: None,
        source_script_command_snapshot: None,
        package_manager,
    };
    let template = (|| -> Result<db::ManagedCommand, diesel::result::Error> {
        let created = db::create_template(conn, project_id, name, command, subfolder, source)?;
        match env.flatten() {
            // Only a non-empty map is written: an absent / cleared env leaves the column NULL,
            // which is already the state a fresh row is in.
            Some(env) => {
                db::set_template_env(conn, &created.id, Some(&env))?;
                Ok(db::get_template(conn, &created.id)?.unwrap_or(created))
            }
            None => Ok(created),
        }
    })()
    .map_err(|e| map_template_write_err(project_id, e))?;
    Ok(ToolOutcome::changed(
        json!({ "command": template_json(&template) }),
        ChangedTopic::Commands,
    ))
}

/// `set_project_settings` — `{ project_id, name?, resume_agent_sessions?, worktrees_root?,
/// accent?, icon?, image? }` → `{ project }`. Modify an EXISTING project's mutable settings
/// (rename, the PRD-5 #5 resume opt-in, the worktrees root and the project's visual identity), the
/// MCP mirror of the UI's project-settings modal. It reuses the SAME `db::update_project` /
/// `db::set_project_resume_agent_sessions` / `db::set_project_worktrees_root` /
/// `db::set_project_accent` / `db::set_project_mark` writes the UI bridge drives, so both surfaces
/// converge on one code path. Partial update: only the supplied fields change. Returns the
/// project's post-write state and a `workspaces` changed effect so any open front re-pulls
/// (the agent-driven mutation the UI never invoked is reflected without a manual reload).
///
/// `worktrees_root`, `accent`, `icon` and `image` are the arguments here where an EMPTY value is a
/// VALUE and not a silence: passing `null` CLEARS the setting (for the root, the state a flow
/// needing a workspace reads as "ask the human where"; for the identity, the fallback tile). They
/// are read through [`clearable_str`] rather than [`optional_str`], whose `null → None` would make
/// "clear it" and "leave it alone" the same request.
///
/// `icon` and `image` are the SAME slot in two spellings (the `projects` CHECK enforces it):
/// supplying one clears the other, and supplying both is `invalid_argument` rather than an
/// arbitrary winner.
pub fn set_project_settings(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let new_name = optional_str(args, "name")?;
    let new_resume = optional_bool(args, "resume_agent_sessions")?;
    let new_worktrees_root = clearable_str(args, "worktrees_root")?;
    let new_accent = clearable_str(args, "accent")?;
    let new_icon = clearable_str(args, "icon")?;
    let new_image = clearable_str(args, "image")?;
    // The mark is ONE slot in two spellings (the `projects` CHECK enforces it), so a call that
    // supplies BOTH is a request nyx cannot honour — refused up front rather than silently
    // letting one win.
    if matches!(new_icon, Some(Some(_))) && matches!(new_image, Some(Some(_))) {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "a project carries an 'icon' OR an 'image', never both — pass one of them",
        ));
    }

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

    if let Some(root) = new_worktrees_root {
        db.with_conn(|c| db::set_project_worktrees_root(c, project_id, root))
            .map_err(internal_db)?;
    }

    if let Some(accent) = new_accent {
        db.with_conn(|c| db::set_project_accent(c, project_id, accent))
            .map_err(internal_db)?;
    }

    // ONE write for the mark whichever spelling arrived, so setting one ALWAYS clears the other
    // (`db::set_project_mark` writes both columns). `Some(None)` on either side is an explicit
    // "drop the mark" and lands the same way.
    let new_mark = match (new_icon, new_image) {
        (Some(Some(icon)), _) => Some(Some(db::ProjectMark::Icon(icon.to_string()))),
        (_, Some(Some(image))) => Some(Some(db::ProjectMark::Image(image.to_string()))),
        (Some(None), _) | (_, Some(None)) => Some(None),
        (None, None) => None,
    };
    if let Some(mark) = new_mark {
        db.with_conn(|c| db::set_project_mark(c, project_id, mark.as_ref()))
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

/// `update_command` — `{ command_id, name?, command?, subfolder?, env? }` → `{ command }`.
/// Partial update; the package.json source-detach rule runs; refused while any instance
/// is running. Parity with the Tauri `update_command`.
///
/// `env` REPLACES the command's persisted environment WHOLE (it is not a per-key patch — a partial
/// merge would leave no way to remove a key), and an explicit `null` or `{}` CLEARS it. Omitted, it
/// is left untouched like every other field.
pub fn update_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    db.with_conn(|c| update_command_on_conn(c, runner, args))
}

/// The CONNECTION-SCOPED body of [`update_command`] — see [`bad_command_id_error_on_conn`].
pub(crate) fn update_command_on_conn<S: RunnerSink>(
    conn: &mut SqliteConnection,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let command_id = require_str(args, "command_id")?;
    let new_name = optional_str(args, "name")?;
    let new_command = optional_str(args, "command")?;
    let subfolder_present = args.get("subfolder").map(|v| !v.is_null()).unwrap_or(false);
    let new_subfolder = optional_str(args, "subfolder")?;
    let new_env = optional_env_column(args, "env")?;

    assert_template_not_running(conn, runner, command_id)?;

    let updated = (|| -> Result<Option<db::ManagedCommand>, diesel::result::Error> {
        let Some(current) = db::get_template(conn, command_id)? else {
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
        db::update_template(conn, command_id, name, command, subfolder)?;
        if detach {
            db::set_template_source(conn, command_id, db::CommandSource::default())?;
        }
        if let Some(env) = new_env {
            db::set_template_env(conn, command_id, env.as_deref())?;
        }
        db::get_template(conn, command_id)
    })()
    .map_err(map_template_write_err_generic)?;
    match updated {
        Some(template) => Ok(ToolOutcome::changed(
            json!({ "command": template_json(&template) }),
            ChangedTopic::Commands,
        )),
        None => Err(bad_command_id_error_on_conn(conn, command_id)),
    }
}

/// Refuse a template edit while any of its instances is running (parity with the Tauri
/// `assert_template_not_running`).
///
/// Connection-scoped, because both its callers now are: the runner's live map is consulted
/// AFTER the id read, and consulting it needs no connection of its own.
fn assert_template_not_running<S: RunnerSink>(
    conn: &mut SqliteConnection,
    runner: &CommandRunner<S>,
    command_id: &str,
) -> Result<(), RpcError> {
    let instance_ids = db::instance_ids_for_template(conn, command_id).map_err(internal_db)?;
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
    conn: &mut SqliteConnection,
    args: &Value,
) -> Result<(String, Vec<crate::pkgjson::DiscoveredScript>, usize), RpcError> {
    let (project_id, workspace_paths): (String, Vec<String>) = match (
        optional_str(args, "workspace_id")?,
        optional_str(args, "project_id")?,
    ) {
        (Some(workspace_id), _) => {
            let ws = db::get_workspace(conn, workspace_id)
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
            let workspaces = db::list_workspaces(conn, project_id).map_err(internal_db)?;
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
    let (_project_id, scripts, manifests_found) =
        db.with_conn(|c| discover_importable(c, args))?;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let listed: Vec<Value> = scripts
        .iter()
        .filter(|s| seen.insert(s.proposed_name.as_str()))
        .map(preview_script_json)
        .collect();
    Ok(json!({ "scripts": listed, "manifests_found": manifests_found }))
}

/// What `import_commands` needs BEFORE it may write anything: the target project, the scripts the
/// discovery found, and how many manifests it walked.
pub(crate) type ImportDiscovery = (String, Vec<crate::pkgjson::DiscoveredScript>, usize);

/// The CONNECTION-SCOPED body of `import_commands` — see [`bad_command_id_error_on_conn`].
///
/// THE DISCOVERY IS NOT DONE HERE, and that is the point of the argument (review
/// 01M15QP3HVYV9Q729ZYTNJB8T2, finding 01M15REGB574G6HD7WZ0MRQVXC). This body runs INSIDE the
/// approval claim's transaction, and the claim writes before it — so SQLite's single write lock is
/// held from the first statement onwards. A four-level filesystem walk in there holds that lock for
/// as long as the walk takes, which over a large tree is past the five-second `busy_timeout` every
/// other connection waits on: a "database is locked" under the user's hands, bought by a scan that
/// writes nothing.
///
/// So the walk is hoisted to the guard's PREPARE step
/// ([`crate::system_command_guard::run_db_mutation_prepared`]), which runs it on the same
/// connection with no transaction open — after the credential check, so an unapproved attempt still
/// buys no scan. What the transaction keeps is the half worth having: an import of twelve scripts
/// lands whole with its claim, or not at all.
pub(crate) fn import_commands_on_conn(
    conn: &mut SqliteConnection,
    args: &Value,
    discovery: ImportDiscovery,
) -> Result<ToolOutcome, RpcError> {
    let name_filter = optional_str_set(args, "names")?;
    let preview = optional_bool(args, "preview")?.unwrap_or(false);
    let (project_id, scripts, manifests_found) = discovery;

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
        let result = crate::pkgjson::import_command(
            conn,
            &project_id,
            &script.proposed_name,
            &script.default_command,
            &script.subfolder,
            source,
        );
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
    // Bulk stop: SIGTERM every target, then ONE grace and ONE join window for the whole set
    // (`stop_many`), instead of paying `TERM_GRACE + PUMP_JOIN_TIMEOUT` per instance in a loop.
    // Same split as the bridge route: this runs on the caller thread, so a workspace holding
    // several live commands would otherwise serialise seconds of pure waiting before the delete.
    // Best-effort by contract — a stop failure must not abort the removal (the user asked to
    // clear everything), and an instance the runner does not back is skipped rather than forced
    // through a phantom idle transition.
    runner.stop_many(instance_ids);
    // …and the SAME split for the interactive shells: one bulk close, not a loop. A host that
    // can signal every pump before joining the first one pays ONE poll window for the whole
    // workspace instead of N (`TerminalHost::close_terminal_ptys`); a host that cannot falls
    // back to the loop this replaced. Idempotent either way — a terminal with no live PTY is a
    // no-op, which is what lets the full membership list be passed with no live-filter.
    host.close_terminal_ptys(terminal_ids);
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
    // A12 — THE FOLDER STAYS ON DISK, AND THE ANSWER SAYS SO, BY PATH.
    //
    // Detaching never touches the contents: nyx does not destroy the user's work, and that is the
    // right behaviour. It was simply IMPLICIT — the response said `removed: true` and named nothing,
    // so an agent that detached a scratch workspace had no way to know it had left bytes behind, nor
    // where. Observed in the audit as a `_worktrees/audit-k9-ws` cleaned up by hand.
    //
    // Read BEFORE the delete: after it, the row carrying `root_path` is gone. Best-effort — a
    // workspace that does not resolve is the use case's refusal to make, not this read's.
    let retained_path = db
        .with_conn(|c| db::get_workspace(c, workspace_id))
        .ok()
        .flatten()
        .map(|ws| ws.root_path);
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
        // The domain boundary keeps its OWN code on the wire (`system_scope_forbidden`) instead of
        // borrowing `invalid_state` from the root rule next to it: the Système workspace is not a
        // workspace in a state that forbids removal, it is a container with no removal at all.
        crate::usecase::RemoveWorkspaceError::SystemScope(e) => RpcError::from(e),
        crate::usecase::RemoveWorkspaceError::Db(e) => internal_db(e),
    })?;
    Ok(ToolOutcome::changed(
        json!({
            "removed": true,
            "removed_instances": removed_instances,
            // A12 — WHAT WAS *NOT* REMOVED. The folder and everything in it are still there; the
            // path is named so the caller can act on it (or tell the human to), instead of
            // discovering it later.
            "folder_kept": retained_path,
        }),
        ChangedTopic::Workspaces,
    ))
}

/// Remove ONE command template (id validation + running-guard + cascade delete), returning
/// the cascaded-instance count. Parity with the Tauri `remove_one_command`.
fn remove_one_command<S: RunnerSink>(
    conn: &mut SqliteConnection,
    runner: &CommandRunner<S>,
    command_id: &str,
) -> Result<usize, RpcError> {
    let template = db::get_template(conn, command_id).map_err(internal_db)?;
    if template.is_none() {
        return Err(bad_command_id_error_on_conn(conn, command_id));
    }
    let instance_ids = db::instance_ids_for_template(conn, command_id).map_err(internal_db)?;
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
    db::delete_template(conn, command_id).map_err(internal_db)?;
    Ok(removed_instances)
}

/// `remove_command` — `{ command_id }` → `{ removed, removed_instances }`. Parity with the
/// Tauri `remove_command`.
pub fn remove_command<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    db.with_conn(|c| remove_command_on_conn(c, runner, args))
}

/// The CONNECTION-SCOPED body of [`remove_command`] — see [`bad_command_id_error_on_conn`].
pub(crate) fn remove_command_on_conn<S: RunnerSink>(
    conn: &mut SqliteConnection,
    runner: &CommandRunner<S>,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let command_id = require_str(args, "command_id")?;
    let removed_instances = remove_one_command(conn, runner, command_id)?;
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
    db.with_conn(|c| remove_commands_on_conn(c, runner, args))
}

/// The CONNECTION-SCOPED body of [`remove_commands`] — see [`bad_command_id_error_on_conn`].
pub(crate) fn remove_commands_on_conn<S: RunnerSink>(
    conn: &mut SqliteConnection,
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
        match remove_one_command(conn, runner, id) {
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

/// `reorder_commands` — `{ project_id, command_ids }` → `{ project_id, commands }`.
///
/// The AGENT's half of the command LIST order (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding
/// 01M11XTRXVVPZ9XMDW5ARWKWHA), over the SAME `db::reorder_templates` the UI's `command_reorder`
/// IPC route calls.
///
/// `command_ids` must be EXACTLY the project's templates, as a set. A partial list is refused for
/// the reason `set_command_group_layout` refuses one: positions are absolute (`order` = index), so
/// re-numbering three of five commands leaves the other two on indices that now collide, and the
/// resulting order is neither the old one nor the asked one. Refusing names the mistake; accepting
/// would silently scramble the list.
pub fn reorder_commands(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    db.with_conn(|c| reorder_commands_on_conn(c, args))
}

/// The CONNECTION-SCOPED body of [`reorder_commands`] — see [`bad_command_id_error_on_conn`].
pub(crate) fn reorder_commands_on_conn(
    conn: &mut SqliteConnection,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?.to_string();
    let command_ids = require_str_vec(args, "command_ids")?;
    let current = db::list_templates(conn, &project_id).map_err(internal_db)?;

    let mut wanted: Vec<&str> = command_ids.iter().map(String::as_str).collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut have: Vec<&str> = current.iter().map(|t| t.id.as_str()).collect();
    have.sort_unstable();
    if wanted != have || command_ids.len() != current.len() {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "command_ids must list EXACTLY the {} command(s) of project {project_id}, in the \
                 wanted order (got {}); read them with list_commands(project_id=…) and send them \
                 back permuted",
                current.len(),
                command_ids.len()
            ),
        ));
    }

    db::reorder_templates(conn, &command_ids).map_err(internal_db)?;
    let commands = db::list_templates(conn, &project_id).map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({
            "project_id": project_id,
            "commands": commands
                .iter()
                .map(|t| json!({
                    "command_id": t.id,
                    "name": t.name,
                    "group_id": t.group_id,
                }))
                .collect::<Vec<_>>(),
        }),
        ChangedTopic::Commands,
    ))
}

/// Map a [`db::CommandGroupError`] onto the D8 error vocabulary: a bad ID is `invalid_id`, a
/// refused SHAPE (too few members, a colour out of range, a layout that is not a re-order) is
/// `invalid_argument`, and only a real DB failure is `internal`. The agent can therefore tell
/// "you named something that does not exist" from "what you asked for is not a legal group".
fn command_group_err(e: db::CommandGroupError) -> RpcError {
    match e {
        db::CommandGroupError::GroupNotFound { .. }
        | db::CommandGroupError::CommandNotFound { .. } => {
            RpcError::new(RpcCode::InvalidId, e.to_string())
        }
        db::CommandGroupError::TooFewMembers { .. }
        | db::CommandGroupError::ColorOutOfRange { .. }
        | db::CommandGroupError::UnknownShape { .. }
        | db::CommandGroupError::LayoutMismatch => {
            RpcError::new(RpcCode::InvalidArgument, e.to_string())
        }
        db::CommandGroupError::Db(err) => internal_db(err),
    }
}

/// `manage_command_group` — `{ action, … }` → `{ action, group, groups }`.
///
/// The AGENT's half of the multi-pane grouping of managed commands (review
/// `01M0ABR6V501D0SPR2BYBVZC4A` #90). Every branch delegates to the SHARED `nyx_core::db`
/// transitions the UI's `command_group_*` IPC routes call — one mutation path, so an agent's
/// grouping and a hand gesture are the same write and produce the same state. Mutating actions
/// carry a `Commands` changed effect so an open sidebar/settings screen repaints without polling;
/// nothing here polls or reads `localStorage` (which is precisely why this model lives in the DB).
///
/// The reply always carries the project's `groups` AFTER the change, so an agent that chains
/// several actions never has to re-list between them.
pub fn manage_command_group(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    db.with_conn(|c| manage_command_group_on_conn(c, args))
}

/// The CONNECTION-SCOPED body of [`manage_command_group`] — see [`bad_command_id_error_on_conn`].
pub(crate) fn manage_command_group_on_conn(
    conn: &mut SqliteConnection,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let action = require_str(args, "action")?;

    /// The project a group (or a command) belongs to — needed to return the post-change list even
    /// when the action's own target has just ceased to exist.
    fn project_of_group(conn: &mut SqliteConnection, group_id: &str) -> Result<String, RpcError> {
        db::get_command_group(conn, group_id)
            .map_err(internal_db)?
            .map(|g| g.project_id)
            .ok_or_else(|| {
                RpcError::new(
                    RpcCode::InvalidId,
                    format!("unknown command group {group_id}"),
                )
            })
    }
    fn project_of_command(
        conn: &mut SqliteConnection,
        command_id: &str,
    ) -> Result<String, RpcError> {
        db::get_template(conn, command_id)
            .map_err(internal_db)?
            .map(|t| t.project_id)
            .ok_or_else(|| {
                RpcError::new(RpcCode::InvalidId, format!("unknown command {command_id}"))
            })
    }

    // `(the group after the action — None once it is gone, the project to list)`.
    let (group, project_id): (Option<db::CommandGroup>, String) = match action {
        "create" => {
            let project_id = require_str(args, "project_id")?.to_string();
            let command_ids = require_str_vec(args, "command_ids")?;
            let group = db::create_command_group(conn, &project_id, &command_ids)
                .map_err(command_group_err)?;
            (Some(group), project_id)
        }
        "add" => {
            let group_id = require_str(args, "group_id")?.to_string();
            let command_id = require_str(args, "command_id")?.to_string();
            let project_id = project_of_group(conn, &group_id)?;
            let group = db::add_to_command_group(conn, &group_id, &command_id)
                .map_err(command_group_err)?;
            (Some(group), project_id)
        }
        "remove" => {
            let command_id = require_str(args, "command_id")?.to_string();
            let project_id = project_of_command(conn, &command_id)?;
            // The group SURVIVES most removals — it only dissolves when the removal takes it under
            // two members — and the reply must say which happened. It used to answer `group: null`
            // in BOTH cases (review finding `01M11XKKFVQWSHZQ4TGT3P3Z06`), so a caller reading
            // `group === null` as "the group is gone" concluded a dissolution while the group was
            // alive with its remaining members, listed two fields away in `groups`. The DB
            // transition now returns the survivor (or `None` on a real dissolution) from INSIDE its
            // own transaction, which is the only place the two can be told apart without a race.
            let survivor =
                db::remove_from_command_group(conn, &command_id).map_err(command_group_err)?;
            (survivor, project_id)
        }
        "dissolve" => {
            let group_id = require_str(args, "group_id")?.to_string();
            let project_id = project_of_group(conn, &group_id)?;
            db::dissolve_command_group(conn, &group_id).map_err(command_group_err)?;
            (None, project_id)
        }
        "name" | "color" => {
            let group_id = require_str(args, "group_id")?.to_string();
            let project_id = project_of_group(conn, &group_id)?;
            // An OMITTED value CLEARS the label — the tool has one action per label, so "name
            // without a name" can only mean "remove the name". (The DB layer's third state,
            // "leave it alone", is what the OTHER action uses on this same call.)
            let group = if action == "name" {
                let name = optional_str(args, "name")?.map(str::to_string);
                db::set_command_group_labels(conn, &group_id, Some(name.as_deref()), None)
                    .map_err(command_group_err)?
            } else {
                let color = match args.get("color") {
                    None | Some(Value::Null) => None,
                    // `try_from`, NOT `as i32`: the cast WRAPS, so `4294967297` came out as `1`
                    // and passed the 1..=6 check downstream — the boundary that exists to refuse
                    // an out-of-palette index would have invented a valid one instead.
                    Some(v) => Some(v.as_i64().and_then(|n| i32::try_from(n).ok()).ok_or_else(
                        || {
                            RpcError::new(
                                RpcCode::InvalidArgument,
                                "argument 'color' must be a 32-bit integer",
                            )
                        },
                    )?),
                };
                db::set_command_group_labels(conn, &group_id, None, Some(color))
                    .map_err(command_group_err)?
            };
            (Some(group), project_id)
        }
        "layout" => {
            let group_id = require_str(args, "group_id")?.to_string();
            let command_ids = require_str_vec(args, "command_ids")?;
            let project_id = project_of_group(conn, &group_id)?;
            let group = db::set_command_group_layout(conn, &group_id, &command_ids)
                .map_err(command_group_err)?;
            (Some(group), project_id)
        }
        // The EIGHTH action (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding 01M11XTRXVVPZ9XMDW5ARWKWHA).
        // Separate from `layout` on purpose: `layout` says WHICH member goes where, `shape` says
        // what the grid looks like. Folding them would make "re-order" and "re-shape"
        // indistinguishable on the wire, exactly as folding `name` and `color` would.
        "shape" => {
            let group_id = require_str(args, "group_id")?.to_string();
            let shape = require_str(args, "shape")?.to_string();
            let project_id = project_of_group(conn, &group_id)?;
            let group =
                db::set_command_group_shape(conn, &group_id, &shape).map_err(command_group_err)?;
            (Some(group), project_id)
        }
        other => {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "unknown action '{other}' — expected one of create | add | remove | dissolve \
                     | name | color | layout | shape"
                ),
            ))
        }
    };

    let groups = db::list_command_groups(conn, &project_id).map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({ "action": action, "group": group, "groups": groups }),
        ChangedTopic::Commands,
    ))
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

/// The SHARED half of the ambient block's data: the Système scope's command lines, or an EMPTY vec.
///
/// A FUNCTION, and one that returns no error, because "best-effort" is a property this path has to
/// HOLD, not merely intend. The `SessionStart` hook drops the WHOLE injected context — orientation
/// line included — on an error or on a timeout, so the shared read must be incapable of costing
/// anything but its own lines. Every way it can go wrong ends on the same empty vec:
///
/// * the scope is NOT BOOTSTRAPPED (`load` ⇒ `None`) — every test `Db` that never called
///   `ensure_system_scope`, which is most of them;
/// * the scope is HALF THERE (a singleton whose primary workspace row is gone) — `load` answers
///   `None` rather than a half-scope, by contract;
/// * the query FAILS — `unwrap_or_default()`, the same swallow the workspace half already applies.
///
/// It is also where the DOUBLE-RENDER is refused: a session whose own workspace IS the Système one
/// gets its commands ONCE, under its own header, not twice under two.
///
/// THE ID COMES FROM THE SCOPE'S OWN READ. [`crate::system_scope::load`], never
/// `db::resolvable_workspaces` — which excludes the singleton deliberately (phase 1) so the cwd
/// auto-attach can never capture it. Injecting the commands does not touch that exclusion: no
/// resolver learns the workspace, and the block itself never renders its id.
///
/// THE READER IS THE LEAN ONE, like the workspace half: this runs on the hook's 1000 ms budget, and
/// `db::list_command_lines_for_workspace` is the reader that never selects `scrollback`.
fn system_command_lines(
    db: &Db,
    session_workspace_id: Option<&str>,
) -> Vec<db::WorkspaceCommandLine> {
    db.with_conn(|c| -> diesel::QueryResult<Vec<db::WorkspaceCommandLine>> {
        let Some(scope) = crate::system_scope::load(c)? else {
            return Ok(Vec::new());
        };
        if session_workspace_id == Some(scope.workspace.id.as_str()) {
            return Ok(Vec::new());
        }
        db::list_command_lines_for_workspace(c, &scope.workspace.id)
    })
    .unwrap_or_default()
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
///
/// AND, under that line, the MANAGED COMMANDS this session can act on — name, command, FACTUAL
/// state, `instance_id` (plus `group_id` on a grouped one) — followed by the directive that says
/// what to do with them. Rendered by [`crate::mcp_context::session_commands_block`], which takes the
/// run-state overlay as a CLOSURE over THIS dispatcher's live `runner` — the same `runner.outcome(…)`
/// else persisted `last_state` fallback `list_commands` applies, so the block cannot claim a command
/// is up when it is not.
///
/// TWO SOURCES feed that block, read here and never merged into one query: the session's OWN
/// workspace, and the Système scope's — the machine-wide services every agent on this machine
/// shares, which the block marks as such (review `01M1EJ84VCWRD6F4NDVD2M0QWJ`). The Système
/// workspace id comes from [`crate::system_scope::load`], the scope's own read, and NEVER from a
/// resolver: `db::resolvable_workspaces` excludes the singleton on purpose since phase 1, and
/// nothing here undoes that.
///
/// A session with NOTHING on either side — a loose terminal on a machine with no Système command —
/// gets no block at all: the line stays exactly what it was.
fn session_context_line<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    session: &db::AgentSession,
) -> String {
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
    let mut context = format!("You're in nyx — {}", parts.join(" · "));
    // The managed-command block. Best-effort by construction: a read that fails yields no
    // instances, hence no block — this line must never fail a SessionStart (the hook drops the
    // WHOLE context, orientation included, on an error or a timeout).
    //
    // The LEAN reader, not `list_instances_for_workspace` (review finding
    // `01M152NMXPWKD59QFK95CMSZWQ`): the full row carries each instance's `scrollback`, bounded
    // at 256 KiB apiece — up to 3 MiB read, UTF-8 validated and allocated on a workspace whose
    // commands have actually run — for a block that renders name/command/state/instance_id and
    // nothing else. On the ONE path with a hard budget, the dominant term is the one that is
    // never read. The SHARED half below takes the same reader, for the same reason.
    let workspace_lines = match session.workspace_id.as_deref() {
        Some(ws_id) => db
            .with_conn(|c| db::list_command_lines_for_workspace(c, ws_id))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    // The SHARED half — the Système scope's commands, which every agent on this machine can act on
    // (review `01M1EJ84VCWRD6F4NDVD2M0QWJ`). SECOND, and it CANNOT subtract anything but itself:
    // the workspace read above has already produced its rows before this one starts.
    let system_lines = system_command_lines(db, session.workspace_id.as_deref());
    let overlay = |instance_id: &str| runner.outcome(instance_id).map(|(state, _, _)| state);
    let block =
        crate::mcp_context::session_commands_block(&workspace_lines, &system_lines, overlay);
    if !block.is_empty() {
        context.push('\n');
        context.push_str(&block);
    }
    context
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
///
/// `runner` is this dispatcher's live [`CommandRunner`] — the handler itself never starts anything;
/// it READS the runner so a `SessionStart`'s injected context can state the FACTUAL run state of the
/// workspace's managed commands (see [`session_context_line`]). That is the whole reason this
/// handler is generic over `S`.
/// R1 — set by the HOST at the START of an ordered shutdown (before it stops the MCP listener and
/// kills the PTYs). While true, a real `SessionEnd` does NOT mark its session `ended`: the row stays
/// `active` so lazy-resume reclaims it next boot. This is the fix for R1 — on a normal close, the
/// dying Claude PTYs fire `SessionEnd(prompt_input_exit)` that reaches this handler even AFTER
/// `mcp.stop()` (proven by diag: the POST lands after `shutdown()` began), ending the session and
/// killing the resume. This is the SECOND deliberate process-global on the crate's dispatch path
/// (after the engine runtime): the handler is a free fn, and threading a flag through the entire MCP
/// dispatch would be far more invasive; the sidecar owns the single toggle.
static SESSION_END_SUPPRESSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The host calls this at ordered-shutdown START so in-flight / late `SessionEnd`s are IGNORED
/// (they must not end a session we want to resume — R1). Idempotent; never reset within a process
/// (a shutdown is terminal for the process).
pub fn suppress_session_end_marking() {
    SESSION_END_SUPPRESSED.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn session_end_marking_suppressed() -> bool {
    SESSION_END_SUPPRESSED.load(std::sync::atomic::Ordering::SeqCst)
}

/// TEST-ONLY: clear the (process-global) suppression flag so a test that toggles it does not leak
/// into the next (the tests serialize on `db_guard`, so a reset inside the lock is isolated).
#[cfg(test)]
fn reset_session_end_suppression() {
    SESSION_END_SUPPRESSED.store(false, std::sync::atomic::Ordering::SeqCst);
}

pub fn agent_session_event<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
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

    // --- A slash-command INJECTION that failed (H8) ----------------------------
    // The hook reports its own failure here rather than only on stderr, which Claude Code hands to
    // nobody nyx can read. Handled BEFORE the activity resolution and returned from: this is a
    // REPORT about the turn, not a beat of it, and re-asserting "running" on the way past would make
    // the live dot depend on whether an injection happened to fail.
    if let Some(reason) = optional_str(args, "nyx_injection_failed")? {
        let command = optional_str(args, "nyx_injection_command")?.unwrap_or("");
        let filed = db
            .with_conn(|c| db::record_injection_failure(c, terminal_id, command, reason))
            .map_err(internal_db)?;
        return Ok(ToolOutcome::changed(
            json!({
                "terminal_id": terminal_id,
                "injection_failed": reason,
                "recorded": filed,
            }),
            ChangedTopic::Specs,
        ));
    }

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
            // The HUMAN spoke. Recorded off the SAME hook that moves the dot, but for a different
            // reader and into a different store: the orchestration engine has to know that a PERSON
            // — not the session — took a turn on this terminal, and this hook is the only signal on
            // the whole dispatch path an agent cannot produce (H6, see [`db::HumanTurn`]). It goes
            // to the DB, not to the runtime activity map, because the answer it authorises can
            // arrive in a LATER session than the question (`claude -p` opens and closes a whole
            // session per prompt). The raw text rides along so a consent answer is journaled with
            // the words that authorised it.
            //
            // The turn's IDENTITY is Claude Code's own `prompt_id` when the hook carries one; when
            // it does not, `record_human_turn` mints one. BEST-EFFORT: this hook must never fail a
            // prompt, so a DB hiccup records nothing rather than erroring (nyx then simply proves
            // no turn, which is the safe answer).
            if hook_name == "UserPromptSubmit" {
                let prompt = optional_str(args, "prompt")?.unwrap_or_default();
                let turn_id = optional_str(args, "prompt_id")?;
                let _ = db.with_conn(|c| db::record_human_turn(c, terminal_id, turn_id, prompt));
            }
            // The FACTS off the raw payload. `tool_input` and `message` are the SECONDARY
            // INFORMATION half (the question Claude is asking, the permission it is blocked on):
            // the `.cjs` hook already forwards Claude's whole stdin, so both are simply read here
            // instead of being dropped. Every one is optional and read defensively — a payload
            // shaped differently yields an event with no text, never an error.
            let facts = HookFacts {
                tool_name: args.get("tool_name").and_then(|v| v.as_str()),
                tool_input: args.get("tool_input"),
                notification_type: args.get("notification_type").and_then(|v| v.as_str()),
                message: args.get("message").and_then(|v| v.as_str()),
            };
            if let Some(activity_event) = ActivityEvent::from_hook(hook_name, facts) {
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
            let context = session_context_line(db, runner, &session);
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
                // UN-MAP (v2.5.10 Phase 1, task 01KXN0HTXFMZAJ0GTVWVBN2PGZ ; proto set added v2.7
                // Phase 2, task 01KY50ZPVSMZPSY351VJNR7Z0F): a `/clear` RESETS the session's working
                // context — vacate BOTH its working sets (its Specs AND its prototypes) so the
                // terminal overlays disappear — WITHOUT ending the row (the following SessionStart
                // refreshes it in place, keeping the sidebar icon/dot steady). The prototype set is
                // vacated "COMME les Specs" (PDR-0009: the proto hangs off the SAME agent session):
                // its link table (`agent_session_prototypes`) survives a `/clear` (the row stays
                // active), so it must be emptied explicitly too. A `/resume` is also an internal
                // transition but CONTINUES the same work, so it keeps both sets; a `/compact` fires
                // no SessionEnd at all, so it never reaches here. GENERIC: the reset DETECTION is
                // per-provider (`end.resets_working_context()`), the CLEARING is provider-agnostic
                // (`db::clear_session_specs` / `db::clear_session_prototypes`). Best-effort — a DB
                // hiccup or no active session simply un-maps nothing.
                if end.resets_working_context() {
                    let (specs_unmapped, protos_unmapped) = db
                        .with_conn(|c| -> Result<(usize, usize), diesel::result::Error> {
                            let Some(session) = db::active_session_for(c, terminal_id, agent_kind)?
                            else {
                                return Ok((0, 0));
                            };
                            let specs = db::clear_session_specs(c, &session.id)?;
                            let protos = db::clear_session_prototypes(c, &session.id)?;
                            Ok((specs, protos))
                        })
                        .unwrap_or((0, 0));
                    // Push a `*://changed` for EACH set that actually emptied so the matching
                    // terminal overlay re-pulls (and, now empty, disappears).
                    let mut effects = Vec::new();
                    if specs_unmapped > 0 {
                        effects.push(ChangedTopic::Specs);
                    }
                    if protos_unmapped > 0 {
                        effects.push(ChangedTopic::Prototypes);
                    }
                    if !effects.is_empty() {
                        return Ok(ToolOutcome::changed_many(
                            json!({
                                "event": "SessionEnd",
                                "terminal_id": terminal_id,
                                "ended": false,
                                "reason": "clear — session working sets vacated (overlays reset)",
                                "specs_unmapped": specs_unmapped,
                                "prototypes_unmapped": protos_unmapped,
                            }),
                            effects,
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
            // R1 — the HOST is shutting down: a dying PTY's `SessionEnd` must NOT end the session,
            // or lazy-resume cannot reclaim it next boot. Leave the row `active` (and the activity
            // untouched — the process is going away anyway). This catches the late POST that
            // outlives `mcp.stop()`, which closing the listener alone did not.
            if session_end_marking_suppressed() {
                return Ok(ToolOutcome::read(json!({
                    "event": "SessionEnd",
                    "terminal_id": terminal_id,
                    "ended": false,
                    "reason": "host shutting down — session kept active for resume (R1)",
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

/// THE TERMINAL-ID HALF OF THE SYSTÈME BOUNDARY, for the four generic terminal VERBS an agent can
/// aim at one id: `read_terminal`, `send_to_terminal`, `send_keys`, `close_terminal`.
///
/// ## Why these four cannot be covered by the allow-list
///
/// [`crate::system_scope::guard_payload`] keys on the singleton's PROJECT and WORKSPACE ids
/// appearing in the payload. These tools carry a `terminal_id` and nothing else, so that guard
/// sees nothing to match — the membership is one join away
/// ([`crate::system_scope::is_system_terminal`]), and only a read can answer it. Hence a second,
/// narrower door rather than a new allow-list entry.
///
/// ## Refused, not hidden
///
/// The refusal is [`RpcCode::SystemScopeForbidden`] and it NAMES the terminal — never `invalid_id`,
/// which would claim the record does not exist. Same rule as everywhere else in this scope: the
/// singleton is not defended by concealment, so an agent that learns a Système terminal's id (from
/// a log, from a human, by guessing) is refused for what it is asking, not left to wonder whether
/// it mistyped. `list_terminals` omits those terminals for ERGONOMICS — an agent should not be
/// offered ids it may not use — and that omission is not the defense either: these four verbs
/// refuse the id whether or not the list ever showed it.
///
/// A terminal of a user project, an unattached (orphan) one, and an id that names nothing all pass
/// through untouched: `None`.
fn refuse_system_terminal(db: &Db, terminal_id: &str, capability: &str) -> Option<RpcError> {
    // The SAME guard the binding writers pose (`db::detach_terminal`,
    // `db::unpin_terminal_workspace`), not a second reading of the same question — so "what is a
    // Système terminal" has one answer in the crate, and the refusal carries the same entity, id
    // and capability whichever door it comes out of.
    //
    // A DB failure is REPORTED, never swallowed (`From<SystemScopeError>` maps it to `internal`):
    // failing open here would turn a transient SQL hiccup into a hole in the boundary.
    db.with_conn(|c| crate::system_scope::require_user_terminal(c, terminal_id, capability))
        .err()
        .map(RpcError::from)
}

/// `create_terminal` — `{ cwd?, command?, label? }` → `{ terminal_id, cwd, workspace_id,
/// has_command }`. Writes the record + resolves auto-attach + parks any opening command, then
/// STARTS the shell — see [`TerminalHost::open_terminal_shell`] for why the tool opens it rather
/// than leaving it to the front's mount.
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
    // PARK BEFORE OPENING: the shell that `open_terminal_shell` starts is what drains the park and
    // types the opening command in, so the command must already be waiting when it comes up.
    if let Some(command) = command {
        host.park_opening_command(&terminal_id, command);
    }
    host.open_terminal_shell(&terminal_id, &stored_cwd);
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
    // The SAME candidate set as `resolve::auto_attach_terminal` — `db::resolvable_workspaces`, the
    // single reader that excludes the Système workspace by join. An agent-created terminal must
    // never auto-attach into the Système scope any more than a UI-created one can.
    let known: Vec<WorkspaceMatch> = db::resolvable_workspaces(conn)?
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
/// Validates the alive record, then writes `command` + a carriage return into the live shell via
/// the shell's [`TerminalHost`]. Parity with the Tauri `send_to_terminal`.
///
/// # ONE write for a shell, TWO for an agent (#K4a)
///
/// On a terminal that hosts NO agent session the command goes out as a single write, exactly as it
/// always has: a line-mode shell reads bytes, not chunks, and gluing the `\r` to the command is
/// what a paste into `bash` looks like anyway.
///
/// On a terminal that DOES host an active agent session the same single write is the measured
/// submit bug: an Ink TUI classifies each stdin chunk AS A WHOLE, so `"commande\r"` arriving as one
/// chunk is read as PASTED TEXT whose trailing CR is a literal newline — the prompt is typed into
/// the box and never sent (see [`TerminalHost::send_keys`], where the same measurement is written
/// up for `send_keys`). This tool nevertheless ARMED the submission window on that write, so a
/// following `wait_for_terminal` answered `resolved: true` on a turn that had not started: a
/// prompt asleep in the input box, indistinguishable from a fast answer.
///
/// So an agent-hosting terminal takes the SAME segmented path `send_keys` takes — `[commande]`
/// then `[CR]`, two writes spaced by the host's pacing. The arming condition is untouched; what
/// changes is that the write it stands for can now actually produce the submission it claims.
pub fn send_to_terminal(
    db: &Db,
    host: &dyn TerminalHost,
    activity: &AgentActivityStore,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let terminal_id = require_str(args, "terminal_id")?;
    let command = require_str(args, "command")?;
    // A Système terminal is not the agent's to type into — refused BEFORE the liveness read, so
    // the refusal never depends on whether a shell happens to be up.
    if let Some(refusal) = refuse_system_terminal(db, terminal_id, crate::mcp::SEND_TO_TERMINAL_TOOL)
    {
        return Err(refusal);
    }
    // The id must name an ALIVE record first (so "unknown id" is distinct from "no PTY").
    let record = db
        .with_conn(|c| db::get_terminal(c, terminal_id))
        .map_err(internal_db)?;
    match record {
        Some(r) if r.status == db::STATUS_ALIVE => {}
        _ => return Err(bad_terminal_id_error(terminal_id)),
    }
    // WHICH WRITE SHAPE — decided BEFORE the write, from the same fact that decides the arming, so
    // the two can never disagree: an agent-hosting terminal gets the segmented submit, a plain
    // shell the single write it has always had.
    let hosts_agent = hosts_active_agent_session(db, terminal_id)?;
    let written = if hosts_agent {
        host.send_keys(
            terminal_id,
            &[command.as_bytes().to_vec(), SUBMIT_CHUNK.to_vec()],
        )
    } else {
        host.send_to_terminal(terminal_id, command)
    }
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
    // A `send_to_terminal` ALWAYS submits, so on an agent-hosting terminal it is always a turn
    // submission — arm the wait window (see `hosts_active_agent_session`).
    if hosts_agent {
        activity.note_prompt_submitted(terminal_id);
    }
    Ok(ToolOutcome::read(
        json!({ "terminal_id": terminal_id, "sent": true }),
    ))
}

/// The bytes that SUBMIT a turn, written on their own. `\r` (0x0D) — the same byte `send_keys`
/// resolves `"enter"` to, and never `\n`: what a raw-mode TUI recognises as Enter is a lone CR
/// arriving as its own chunk.
const SUBMIT_CHUNK: &[u8] = b"\r";

/// Whether `terminal_id` hosts an ACTIVE agent session — the fact both the write shape and the
/// submission arming hang off. Shared by `send_to_terminal` and `send_keys`.
///
/// # Why the session check is not optional
///
/// The arming window makes a subsequent `wait_for_terminal` withhold its answer
/// ([`crate::agent_activity::AgentActivityStore::note_prompt_submitted`]). On a PLAIN terminal —
/// a shell with no agent — there is no turn to wait for and no hook will ever come, so arming
/// would buy nothing and cost the next waiter a full window against a state that was already
/// final. The check keeps the reticence where it has a payer: a terminal whose activity is reported
/// by somebody else's hooks.
///
/// A DB error here is surfaced rather than swallowed: it is the same connection the caller already
/// read the terminal record on, so a failure means something the caller needs to hear about, not a
/// missing nicety.
fn hosts_active_agent_session(db: &Db, terminal_id: &str) -> Result<bool, RpcError> {
    Ok(db
        .with_conn(|c| db::active_session_for_terminal(c, terminal_id))
        .map_err(internal_db)?
        .is_some())
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

/// Resolve a `keys` array (left-to-right) into ONE raw byte CHUNK PER ELEMENT: each element is
/// either LITERAL text (written verbatim) or a NAMED KEY resolved to its control bytes (see
/// [`resolve_key_element`]). NO trailing `\r` is ever appended — the caller asks for `enter`
/// explicitly when it wants to submit. Pure (testable) companion to [`send_keys`].
///
/// # Why a Vec of chunks rather than one concatenated buffer
///
/// The chunk boundary IS the fix (see [`TerminalHost::send_keys`]): a raw-mode TUI classifies each
/// stdin `data` event as a whole, so `"texte" + "\r"` delivered as ONE chunk reads as a paste whose
/// trailing CR is a literal newline — the prompt gets typed and never submitted. Keeping one chunk
/// per element is what lets the host write `\r` ALONE, which is what Ink recognises as Enter.
///
/// An EMPTY element resolves to no bytes and is dropped rather than kept as a zero-length write:
/// a `write_all(&[])` is a no-op that would only cost the host a pacing gap.
fn resolve_keys(elements: &[String]) -> Vec<Vec<u8>> {
    elements
        .iter()
        .map(|element| resolve_key_element(element))
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

/// `send_keys` — `{ terminal_id, keys }` (keys: an ARRAY of literal-text / named-key
/// strings) → `{ terminal_id, sent: true, bytes, writes }`. Validates the alive record, resolves
/// `keys` to RAW bytes (named keys → control bytes; literal text → verbatim) as ONE CHUNK PER
/// ELEMENT, then writes those chunks into the live shell — one PTY write each, no appended `\r` —
/// via the shell's [`TerminalHost`]. The raw-key counterpart to `send_to_terminal` (which always
/// appends a newline): this drives a raw-mode TUI (arrows, Esc, Ctrl+C, typing without
/// submitting). Same liveness / `invalid_state` semantics as `send_to_terminal`.
///
/// `writes` reports how many PTY writes the call produced; `bytes` stays the TOTAL across them,
/// so the answer says the thing the 2026-09-01 run could not see — that `["texte","enter"]` is
/// two writes and not one (see [`TerminalHost::send_keys`] for why that is the whole fix).
pub fn send_keys(
    db: &Db,
    host: &dyn TerminalHost,
    activity: &AgentActivityStore,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let terminal_id = require_str(args, "terminal_id")?;
    let keys = require_str_vec(args, "keys")?;
    // Same rule as `send_to_terminal`: raw keystrokes into a Système terminal are the human's.
    if let Some(refusal) = refuse_system_terminal(db, terminal_id, crate::mcp::SEND_KEYS_TOOL) {
        return Err(refusal);
    }
    // The id must name an ALIVE record first (so "unknown id" is distinct from "no PTY").
    let record = db
        .with_conn(|c| db::get_terminal(c, terminal_id))
        .map_err(internal_db)?;
    match record {
        Some(r) if r.status == db::STATUS_ALIVE => {}
        _ => return Err(bad_terminal_id_error(terminal_id)),
    }
    let chunks = resolve_keys(&keys);
    let total_bytes: usize = chunks.iter().map(Vec::len).sum();
    let written = host
        .send_keys(terminal_id, &chunks)
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
    // A `keys` array that carries a SUBMIT chunk (`enter`, or a literal string containing a
    // newline) is a turn submission on an agent-hosting terminal — arm the wait window. A pure
    // navigation/edit burst (`["up"]`, `["escape"]`, text with no `enter`) submits nothing and is
    // deliberately NOT armed: it must not make the next `wait_for_terminal` block on a turn that
    // was never started.
    if chunks
        .iter()
        .any(|c| c.iter().any(|b| *b == b'\r' || *b == b'\n'))
        && hosts_active_agent_session(db, terminal_id)?
    {
        activity.note_prompt_submitted(terminal_id);
    }
    Ok(ToolOutcome::read(json!({
        "terminal_id": terminal_id,
        "sent": true,
        "bytes": total_bytes,
        "writes": chunks.len(),
    })))
}

/// The AGENT half of a terminal row: the live `agent_activity` and the `agent_session` link, or
/// two `null`s when this terminal hosts no agent session.
///
/// # What this fixes
///
/// The 2026-09-01 pilot run measured that NEITHER of the two signals `list_terminals` already
/// carried can say whether an agent is mid-turn: on Windows `busy` is `null` for the whole
/// session, and `exec_state` never leaves `idle` with an `exec_state_updated_at` that does not
/// move once — because `claude` IS the foreground process from the moment it starts to the moment
/// it exits, so the OS bit cannot separate "generating" from "sitting at its prompt" (the project's
/// own spike, `tests/agent_activity_signal_spike.rs`, had already concluded this).
///
/// nyx nevertheless HOLDS the right signal: [`AgentActivityStore`], fed by the session's own hooks,
/// is what drives the coloured dot in the UI. This surfaces the SAME value to an agent, read-only.
///
/// # Read, never written — the anti-phantom contract is untouched
///
/// The store is RUNTIME-ONLY by design (a persisted `working` would be a dot stuck forever on a
/// session that died mid-turn), and nothing here changes that: no column, no migration, no write.
/// A terminal the store has never heard of — including one revived from the DB at boot — reads
/// `idle`, and the PTY-death / `SessionEnd` clear drops the entry, so the value can never outlive
/// the process it describes.
fn agent_fields_for(
    activity: &AgentActivityStore,
    session: Option<&db::AgentSession>,
) -> (Value, Value) {
    let Some(session) = session else {
        // NO agent session on this terminal: both fields are explicitly null rather than absent,
        // so a reader tells "not an agent terminal" from "an agent terminal that is idle".
        return (Value::Null, Value::Null);
    };
    // A session with no store entry is `Idle` — the boot state, and the state after a clear.
    let current = activity
        .snapshot(&session.terminal_id)
        .map(|s| s.activity)
        .unwrap_or(crate::agent_activity::Activity::Idle);
    (
        json!(current.as_str()),
        json!({
            "session_id": session.id,
            "agent_kind": session.agent_kind,
            "external_session_id": session.external_session_id,
            "state": session.state,
            "transcript_path": session.transcript_path,
        }),
    )
}

/// `list_terminals` — `{ include_closed? }` → `{ terminals }`. Lists records + the live
/// `live`/`busy` bits from the shell's [`TerminalHost`], plus the agent half of each row
/// (`agent_activity` / `agent_session`, see [`agent_fields_for`]). Parity with the Tauri
/// `list_terminals`.
pub fn list_terminals(
    db: &Db,
    host: &dyn TerminalHost,
    activity: &AgentActivityStore,
    args: &Value,
) -> Result<Value, RpcError> {
    let include_closed = optional_bool(args, "include_closed")?.unwrap_or(false);
    // The Système workspace's id, read ONCE, so the filter below is a string comparison per row
    // rather than a join per row. `None` when the scope is not bootstrapped — nothing to exclude.
    let system_workspace = db
        .with_conn(|c| -> Result<Option<String>, diesel::result::Error> {
            Ok(crate::system_scope::load(c)?.map(|s| s.workspace.id))
        })
        .map_err(internal_db)?;
    let records = db.with_conn(db::list_terminals).map_err(internal_db)?;
    // The ACTIVE agent sessions, read ONCE for the whole list and keyed by host terminal — a
    // per-row query would cost one round trip per terminal for a link that is a single scan.
    //
    // FIRST ROW PER TERMINAL WINS, and the query's order is the one
    // [`db::active_session_for_terminal`] applies (`started_at` desc, `id` desc) — so the session
    // this list names for a terminal is the SAME one `read_terminal` and `wait_for_terminal` name
    // for it (#K6). Collecting into the map with `collect()` used to let the LAST row win, i.e. the
    // highest `agent_kind`, which is a THIRD answer to a question that has one.
    let mut sessions: std::collections::HashMap<String, db::AgentSession> =
        std::collections::HashMap::new();
    for session in db
        .with_conn(db::active_agent_sessions_full)
        .map_err(internal_db)?
    {
        sessions.entry(session.terminal_id.clone()).or_insert(session);
    }
    let terminals: Vec<Value> = records
        .into_iter()
        // A SYSTÈME terminal is not offered to the agent. ERGONOMICS, not the defense: the four
        // verbs (`read`/`send_to`/`send_keys`/`close_terminal`) refuse its id whether or not this
        // list ever showed it (`refuse_system_terminal`). What the omission does buy is that an
        // agent is never handed an id it may not use.
        //
        // Keyed on the POSITIVE membership — the row's `workspace_id` naming the singleton's
        // workspace — so an ORPHAN (`workspace_id IS NULL`) is untouched: the two sets stay
        // disjoint here as everywhere.
        .filter(|t| match (&t.workspace_id, &system_workspace) {
            (Some(ws), Some(sys)) => ws != sys,
            _ => true,
        })
        .filter(|t| include_closed || t.status == db::STATUS_ALIVE)
        .map(|t| {
            let (live, busy) = host.terminal_liveness(&t.id);
            let (agent_activity, agent_session) =
                agent_fields_for(activity, sessions.get(t.id.as_str()));
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
                "agent_activity": agent_activity,
                "agent_session": agent_session,
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
    // Closing a Système terminal is the human's gesture, from the UI — refused here BEFORE the
    // record is even read, so no agent can end a service shell it cannot see.
    if let Some(refusal) = refuse_system_terminal(db, terminal_id, crate::mcp::CLOSE_TERMINAL_TOOL) {
        return Err(refusal);
    }
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
/// `{ terminal_id, output, total_bytes, returned_bytes, truncated, reset, cursor, live, stale }`.
/// Reads the persisted (front-serialized) scrollback.
///
/// It ALSO states the buffer's FRESHNESS, because that buffer is not always the terminal's present
/// state and the difference is invisible in the text: the scrollback is serialized by the shell's
/// front end and only for a terminal it currently has MOUNTED. A terminal in another project, or
/// one whose shell has exited, keeps its last saved buffer for as long as it stays out of view —
/// arbitrarily long. So the answer carries:
///   * `live` — a shell is running behind this record right now;
///   * `stale` — what you are reading is NOT the terminal's present state, either because no shell
///     is running any more (`live: false`, a closed or exited terminal: the buffer is its final
///     saved output) or because its shell has printed things nyx has not saved yet
///     (`scrollback_is_behind`: the terminal is not mounted in the UI, or the save debounce is
///     still in flight right after a `send_to_terminal`).
///
/// `stale: false` is therefore the only reading a caller may treat as "this is what the terminal
/// shows now"; anything else is a past snapshot, said out loud rather than passed off as current.
///
/// When the terminal HOSTS an agent session the answer also carries `agent_activity` — so a caller
/// that has just read the screen can tell, in the SAME round trip, whether the agent was still
/// mid-turn while it read (see [`agent_fields_for`]). `null` on a terminal with no session.
pub fn read_terminal(
    db: &Db,
    host: &dyn TerminalHost,
    activity: &AgentActivityStore,
    args: &Value,
) -> Result<Value, RpcError> {
    let terminal_id = require_str(args, "terminal_id")?;
    // A Système terminal's scrollback is not an agent read: refused before the buffer is loaded.
    if let Some(refusal) = refuse_system_terminal(db, terminal_id, crate::mcp::READ_TERMINAL_TOOL) {
        return Err(refusal);
    }
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
    // The FRAME reading (`strip_ansi_frame`, not `strip_ansi`): a terminal's scrollback is a
    // re-serialized GRID, so the blanks a `\t` skipped over are part of what the screen showed
    // (#K8).
    let output = if strip {
        crate::ansi::strip_ansi_frame(&window.output)
    } else {
        window.output.clone()
    };
    let (live, _busy) = host.terminal_liveness(terminal_id);
    let stale = !live || host.scrollback_is_behind(terminal_id);
    let session = db
        .with_conn(|c| db::active_session_for_terminal(c, terminal_id))
        .map_err(internal_db)?;
    let (agent_activity, _agent_session) = agent_fields_for(activity, session.as_ref());
    Ok(json!({
        "terminal_id": terminal_id,
        "output": output,
        "total_bytes": window.total_bytes,
        "returned_bytes": window.returned_bytes,
        "truncated": window.truncated,
        "reset": window.reset,
        "cursor": window.cursor,
        "live": live,
        "stale": stale,
        "agent_activity": agent_activity,
        // The SAME honesty as wait_for_terminal: `agent_activity` above is the hooks' word, and
        // while nyx has just written an enter no hook has confirmed, that word describes the
        // PREVIOUS turn. A poller refused a blocking wait is sent here — it must see this bit.
        "submission_pending": activity.submission_pending(terminal_id),
    }))
}

/// Parse the OPTIONAL `until` argument of [`wait_for_terminal`] into the set of
/// [`crate::agent_activity::Activity`] values that resolve the wait — the activity twin of
/// `mcp_runtime::parse_until`, down to the alias.
///
/// The DEFAULT is `idle` + `waiting`, i.e. **the agent has given you the hand back**: a turn that
/// finished normally lands on `idle`, one that stopped to ask a question or for a permission lands
/// on `waiting`, and a caller that waited only for `idle` would sit through the whole timeout on a
/// session that is blocked and will not move until a human answers. `"settled"` names that same
/// pair explicitly, the way `"exited"` names success+error for a command.
fn parse_activity_until(args: &Value) -> Result<Vec<crate::agent_activity::Activity>, RpcError> {
    use crate::agent_activity::Activity;
    let default = || vec![Activity::Idle, Activity::Waiting];
    let raw = match args.get("until") {
        None | Some(Value::Null) => return Ok(default()),
        Some(Value::Array(items)) if items.is_empty() => return Ok(default()),
        Some(Value::Array(items)) => items,
        Some(_) => {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                "argument 'until' must be an array of activity strings \
                 (idle|working|waiting|settled)",
            ))
        }
    };
    let mut states: Vec<Activity> = Vec::new();
    let push = |s: Activity, states: &mut Vec<Activity>| {
        if !states.contains(&s) {
            states.push(s);
        }
    };
    for item in raw {
        let s = item.as_str().ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidArgument,
                "each 'until' entry must be an activity string (idle|working|waiting|settled)",
            )
        })?;
        match s.trim().to_ascii_lowercase().as_str() {
            "idle" => push(Activity::Idle, &mut states),
            "working" => push(Activity::Working, &mut states),
            "waiting" => push(Activity::Waiting, &mut states),
            "settled" => {
                push(Activity::Idle, &mut states);
                push(Activity::Waiting, &mut states);
            }
            other => {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!(
                        "unknown 'until' activity '{other}' \
                         (accepted: idle|working|waiting|settled)"
                    ),
                ))
            }
        }
    }
    Ok(states)
}

/// The ceiling [`wait_for_terminal`] gives the persisted scrollback to catch up with a terminal
/// whose turn has just ended (see the call site for why it has to).
///
/// It bounds ONE latency: the debounced SAVE of `terminals.scrollback`. The sidecar's pump saves a
/// view-less terminal every 750 ms, and a mounted one is saved by the renderer on a debounce of
/// the same order, so two seconds clears both with room for a loaded machine — while staying far
/// under the shortest wait a caller would think of setting. It is a CEILING, not a delay: the loop
/// ends the instant `scrollback_is_behind` clears, which for a terminal that has stopped printing
/// is the first read.
const SCROLLBACK_SETTLE_BUDGET: std::time::Duration = std::time::Duration::from_millis(2_000);

/// How many `wait_for_terminal` calls may BLOCK at once, process-wide (#K18).
///
/// The MCP surface serves requests on a fixed pool of [`crate::mcp::REQUEST_WORKERS`] threads, and
/// a blocking wait parks one of them for up to [`crate::command::WAIT_MAX_TIMEOUT`] plus
/// [`SCROLLBACK_SETTLE_BUDGET`]. That is fine for `wait_for_command`, whose signal comes from the
/// in-process runner — but a terminal's activity is reported by the agent's own hooks, which reach
/// nyx as a `POST /mcp` served by THAT SAME POOL. So a wait can only be resolved by a request that
/// needs a worker the wait itself is holding.
///
/// Piloting several sessions at once — the very thing these tools exist for — reaches that in four
/// calls: the hook POSTs queue behind the waits, each hook abandons at its own 1 s timeout with no
/// replay, and all four waits come back `resolved: false` after a minute on turns that ended
/// immediately. Raising [`crate::mcp::REQUEST_WORKERS`] would only move the number at which it
/// happens; keeping one worker permanently free for short requests removes the circularity.
///
/// THE BUDGET COVERS EVERY LONG BLOCK ON THIS POOL, not just terminal waits. `wait_for_command`
/// blocks the same way for the same minute, and a `nyx_next` gate runs INLINE for up to 90 s; a cap
/// that counted only `wait_for_terminal` left the free worker to be taken by either of them, and
/// the deadlock came back through the side door. One budget, shared.
pub(crate) const MAX_CONCURRENT_TERMINAL_WAITS: usize = crate::mcp::REQUEST_WORKERS - 1;

/// How many LONG BLOCKING CALLS are in flight RIGHT NOW, process-wide. Paired with
/// [`TerminalWaitSlot`], which is the only thing that touches it.
static LIVE_TERMINAL_WAITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One of the [`MAX_CONCURRENT_TERMINAL_WAITS`] blocking slots, held for the duration of the block
/// and released on EVERY exit path (including a panic) by its `Drop`.
pub(crate) struct TerminalWaitSlot;

impl TerminalWaitSlot {
    /// Take a slot, or `None` when the process is already blocking on
    /// [`MAX_CONCURRENT_TERMINAL_WAITS`] terminals. A compare-and-swap loop rather than a
    /// fetch-add-then-check: an over-increment would be visible to a concurrent caller as a
    /// refusal it did not earn.
    pub(crate) fn acquire() -> Option<Self> {
        use std::sync::atomic::Ordering;
        let mut live = LIVE_TERMINAL_WAITS.load(Ordering::Acquire);
        loop {
            if live >= MAX_CONCURRENT_TERMINAL_WAITS {
                return None;
            }
            match LIVE_TERMINAL_WAITS.compare_exchange_weak(
                live,
                live + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self),
                Err(actual) => live = actual,
            }
        }
    }
}

impl Drop for TerminalWaitSlot {
    fn drop(&mut self) {
        LIVE_TERMINAL_WAITS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// The refusal a caller gets instead of a slot — NAMED and actionable, which is the whole point of
/// the cap: the alternative was a wait that looked accepted and came back `resolved: false` a
/// minute later having observed nothing.
pub(crate) fn too_many_terminal_waits_error() -> RpcError {
    RpcError::new(
        RpcCode::InvalidState,
        format!(
            "too many blocking calls at once (limit {MAX_CONCURRENT_TERMINAL_WAITS}): one \
             nyx worker must stay free to receive the agent hooks that END these waits, so a \
             {n}th concurrent block (wait_for_terminal, wait_for_command or a nyx_next gate) \
             would only starve the others. Let one of them return first, or poll with \
             read_terminal / get_command_output — and while a terminal's submission_pending \
             is true, its agent_activity still describes the PREVIOUS turn: keep polling.",
            n = MAX_CONCURRENT_TERMINAL_WAITS + 1
        ),
    )
    .with_data(json!({
        "waits_in_flight": MAX_CONCURRENT_TERMINAL_WAITS,
        "limit": MAX_CONCURRENT_TERMINAL_WAITS,
    }))
}

/// Block until this terminal's persisted scrollback is level with its shell again, or until
/// [`SCROLLBACK_SETTLE_BUDGET`] runs out. Returns how long that took (zero when it was already
/// level, which is the common case).
///
/// Reuses [`crate::command::WAIT_POLL_INTERVAL`] so the settle has the same resolution as the wait
/// it extends, and returns EARLY the moment the bit clears — the budget is only spent on a
/// terminal that keeps printing.
fn settle_scrollback(host: &dyn TerminalHost, terminal_id: &str) -> std::time::Duration {
    let start = std::time::Instant::now();
    while host.scrollback_is_behind(terminal_id) {
        if start.elapsed() >= SCROLLBACK_SETTLE_BUDGET {
            break;
        }
        std::thread::sleep(crate::command::WAIT_POLL_INTERVAL);
    }
    start.elapsed()
}

/// `wait_for_terminal` — `{ terminal_id, until?, timeout_ms?, since?, tail_bytes?, max_bytes?,
/// strip_ansi? }` → `{ terminal_id, resolved, agent_activity, submission_pending, agent_session,
/// waited_ms, cursor, reset, output_tail, live, stale }`. A BOUNDED, observational long-poll on a
/// terminal's AGENT activity — the twin of `wait_for_command`, built the same way and with the same
/// vocabulary.
///
/// # `agent_activity` is the SNAPSHOT, and only the snapshot (#K3)
///
/// The field carries what the agent's own hooks asserted — the SAME value `list_terminals` and
/// `read_terminal` report for that terminal, read after the wait. It never carries the submission
/// arming window's supposition: that lives inside the poll loop, where its only power is to WITHHOLD
/// a resolution (see [`crate::agent_activity::WaitActivity`]), and it is reported separately as
/// `submission_pending`.
///
/// # The asymmetry it closes
///
/// A managed command had `wait_for_command`; a terminal had nothing, so an agent driving a second
/// Claude session polled blind — one MCP round trip per attempt, over a signal that could not
/// answer: on the 2026-09-01 run ten probes across thirty seconds showed `busy: null` and an
/// `exec_state` frozen on `idle` the entire time the session was generating, because the agent IS
/// the foreground process from start to exit. Worse, the fallback of grepping the screen for a
/// sentinel matched on the FIRST read — the prompt that had just been SENT was on screen too, and
/// nothing distinguishes it from the answer. This waits on the one signal that means what it says.
///
/// # Timeout is an OUTCOME, not an error
///
/// `resolved: false` after `timeout_ms` (default 30 s, clamped to [`crate::command::WAIT_MAX_TIMEOUT`])
/// is a normal answer: call again to keep waiting. Nothing is consumed by a wait — it is a pure
/// reader, so any number of callers may wait the same terminal and none of them acknowledges
/// anything.
///
/// # No agent session is not an error either
///
/// A terminal hosting no agent session reads `Idle` forever (the store has no entry), so the
/// default `until` resolves immediately with `agent_session: null`. That is deliberate: refusing
/// would make the tool unusable in exactly the moment a caller cannot yet know whether a session
/// has registered, and the null link says what happened far better than an error code.
pub fn wait_for_terminal(
    db: &Db,
    host: &dyn TerminalHost,
    activity: &AgentActivityStore,
    args: &Value,
) -> Result<Value, RpcError> {
    let terminal_id = require_str(args, "terminal_id")?;
    // Same rule as the other terminal verbs: a Système terminal is the human's.
    if let Some(refusal) =
        refuse_system_terminal(db, terminal_id, crate::mcp::WAIT_FOR_TERMINAL_TOOL)
    {
        return Err(refusal);
    }
    // The id must name an ALIVE record: waiting on a closed terminal can only ever time out.
    match db
        .with_conn(|c| db::get_terminal(c, terminal_id))
        .map_err(internal_db)?
    {
        Some(r) if r.status == db::STATUS_ALIVE => {}
        _ => return Err(bad_terminal_id_error(terminal_id)),
    }

    let until = parse_activity_until(args)?;
    let timeout_ms = optional_usize(args, "timeout_ms")?.unwrap_or(
        crate::mcp_runtime::DEFAULT_WAIT_TIMEOUT_MS as usize,
    );
    let timeout =
        std::time::Duration::from_millis(timeout_ms as u64).min(crate::command::WAIT_MAX_TIMEOUT);
    let (effective_tail, since_arg, strip) = parse_window_knobs(args)?;

    // The scrollback as it stands RIGHT NOW, read once so a first call (no `since`) can bound its
    // window to what appears AFTER it — the same first-call bounding `wait_for_command` applies,
    // and the reason `output_tail` is never the whole existing buffer.
    let scrollback_now = |db: &Db| -> Result<String, RpcError> {
        Ok(db
            .with_conn(|c| db::get_terminal(c, terminal_id))
            .map_err(internal_db)?
            .map(|r| r.scrollback)
            .unwrap_or_default())
    };
    let since = match since_arg {
        Some(s) => s,
        None => scrollback_now(db)?.len(),
    };

    // The target set, lifted into the wait's own vocabulary: every `until` state is something the
    // STORE must have OBSERVED. `WaitActivity::Unconfirmed` — nyx submitted and no hook has
    // confirmed — is by construction absent from this vector, which is the whole of the #K3 fix:
    // the arming window can WITHHOLD a resolution but can never PRODUCE one, so `until: ["working"]`
    // waits for a hook-asserted `working` instead of resolving instantly on nyx's own guess.
    let until: Vec<crate::agent_activity::WaitActivity> = until
        .into_iter()
        .map(crate::agent_activity::WaitActivity::Observed)
        .collect();
    // TAKE A BLOCKING SLOT (#K18) — refused, by name, rather than accepted into a pool where this
    // wait would starve the hook that ends it. Held across the poll AND the scrollback settle (both
    // block a worker), released by `Drop` on every exit path below.
    let Some(_slot) = TerminalWaitSlot::acquire() else {
        return Err(too_many_terminal_waits_error());
    };

    let outcome = crate::command::poll_until(
        &until,
        timeout,
        crate::command::WAIT_POLL_INTERVAL,
        // `wait_activity`, NOT `snapshot`: it is the one reader that honours the submission
        // arming window, so a turn nyx just wrote an `enter` for reads `Unconfirmed` until its
        // `UserPromptSubmit` hook lands. Without it this wait resolved on the PREVIOUS turn's
        // `Idle` — `resolved: true`, `waited_ms: 0`, an empty `output_tail` (gate 2026-09-02).
        || activity.wait_activity(terminal_id),
        std::thread::sleep,
    );

    // THE TURN IS OVER — THE RECORD IS NOT YET LEVEL WITH IT. The activity signal comes from the
    // agent's `Stop` hook; the `output_tail` below comes from `terminals.scrollback`, which a
    // view-less terminal saves on its own debounce (750 ms in the sidecar's pump). Measured on the
    // gate run of 2026-09-02: the wait resolved on the real end of turn, and for the next ~780 ms
    // `read_terminal` answered `stale: true` with the answer still missing — so a caller that did
    // the obvious thing got an EMPTY tail and had to read again. That second read is exactly the
    // catch-up call this tool exists to remove, so the wait absorbs the lag itself.
    //
    // `scrollback_is_behind` is an EXACT bit, not a timestamp guess (the pump sets it on every
    // chunk, whichever save clears it), so this loop ends as soon as the save lands — for a quiet
    // terminal, immediately. The budget is the ceiling for the case where it never clears: a shell
    // that never stops printing (a `tail -f`) is never level, and waiting on it forever would turn
    // a bounded tool into an unbounded one. Past the budget the answer goes out as it is, with
    // `stale: true` saying so.
    //
    // Only on a RESOLVED wait: a timeout means "call me back", and adding a settle to it would
    // only make giving up slower.
    let settled_for = if outcome.resolved {
        settle_scrollback(host, terminal_id)
    } else {
        std::time::Duration::ZERO
    };

    // Everything below is read AFTER the wait, so the answer describes the terminal as it is when
    // the caller gets it back — not as it was when the wait started.
    let window =
        crate::mcp_runtime::bound_output(&scrollback_now(db)?, effective_tail, Some(since));
    // Same FRAME reading as `read_terminal` — this tail comes from the same scrollback (#K8).
    let output_tail = if strip {
        crate::ansi::strip_ansi_frame(&window.output)
    } else {
        window.output.clone()
    };
    let (live, _busy) = host.terminal_liveness(terminal_id);
    let stale = !live || host.scrollback_is_behind(terminal_id);
    let session = db
        .with_conn(|c| db::active_session_for_terminal(c, terminal_id))
        .map_err(internal_db)?;
    let (_activity_json, agent_session) = agent_fields_for(activity, session.as_ref());
    // THE STORE'S OWN VALUE, not the wait's internal reading (#K3). `outcome.state` carries the
    // `Unconfirmed` sentinel and, before this fix, the window's SUPPOSED `Working` — a guess put on
    // the wire under the same field name `list_terminals` and `read_terminal` use for the SNAPSHOT,
    // so two tools contradicted each other on one terminal for four seconds. The wait's optimism
    // belongs to the wait; what goes out is what the hooks asserted, read here exactly as
    // [`agent_fields_for`] reads it.
    let observed = activity
        .snapshot(terminal_id)
        .map(|s| s.activity)
        .unwrap_or(crate::agent_activity::Activity::Idle);

    Ok(json!({
        "terminal_id": terminal_id,
        "resolved": outcome.resolved,
        "agent_activity": observed.as_str(),
        // The arming mark, said OUT LOUD instead of smuggled into `agent_activity`: nyx submitted a
        // turn into this terminal and no hook has confirmed it yet. `true` alongside a
        // `resolved: false` is the honest reading of "the enter went in, the agent has not spoken".
        "submission_pending": activity.submission_pending(terminal_id),
        "agent_session": agent_session,
        // The WHOLE call, the activity wait plus the scrollback settle — what the caller actually
        // spent, not the part of it that was spent on the agent.
        "waited_ms": (outcome.waited + settled_for).as_millis() as u64,
        "cursor": window.cursor,
        "reset": window.reset,
        "output_tail": output_tail,
        "live": live,
        "stale": stale,
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
/// - `Some(Err(system_scope_forbidden))` — the SYSTÈME BOUNDARY refused the call BEFORE any
///   routing happened (the guard below). The exact condition: `name` is on NEITHER half of the
///   allow-list ([`crate::system_scope::SYSTEM_SCOPE_MCP_CAPABILITIES`] /
///   [`crate::system_scope::SYSTEM_SCOPE_IPC_CAPABILITIES`], both read by the guard) AND its
///   payload aims the singleton — including a name this module does not serve, and a name that
///   exists nowhere. It is therefore the one case where a `Some(_)` does not mean "this module owns
///   the tool". Symmetrically, an ALLOW-LISTED name is never refused here whatever it aims:
///   `start_command` or `command_list` carrying the singleton's `project_id` goes straight past.
/// - `None` — `name` is not one of this module's tools AND the guard above did not fire, so the
///   caller falls through. In practice that is [`crate::mcp_runtime`]'s surface, in two groups: the
///   runtime command tools (`start_command`, `stop_command`, `relaunch_command`,
///   `get_command_output`, `wait_for_command`) are ON the allow-list, so they fall through even
///   when their payload names the singleton; the workspace registration tools (`workspace_add`,
///   `create_workspace`) are NOT, so they fall through only while their payload aims elsewhere.
///
/// ## The routing consequence: call this LAST
///
/// Because the refusal precedes the routing, a `Some(_)` here does not mean "this module owns the
/// tool" — it may be a refusal issued on behalf of a name belonging to a sibling dispatcher, or to
/// nobody. So this must be the caller's LAST step, which is what `nyx_sidecar::runtime::mcp` does
/// (`dispatch_repo_watch`, then the runtime command tools, then `dispatch_workspace_tool`, then
/// this).
///
/// WHAT THE ORDER DOES NOT BUY, since the allow-list was split in two. It is NOT that
/// [`crate::mcp_runtime`]'s tools would be swallowed: five of its seven names are on the allow-list
/// (above), so this guard never fires on them whatever they aim. The two that are not,
/// `workspace_add` and `create_workspace`, must be refused on the singleton anyway, and their own
/// dispatcher refuses them through THIS VERY function — so for them the order changes the verdict
/// not at all. The order is a STRUCTURAL precaution, not a live fix: it preserves "each sibling
/// dispatcher decides its own names" for the day a sibling wants a verdict other than this guard's.
///
/// Every advertised name still resolves to either this module or [`crate::mcp_runtime`], so no
/// advertised tool can fall into the dispatcher's `unknown tool` arm — the guarantee is unchanged,
/// it is simply no longer expressible as "`None` means fall through".
///
/// ## THE SECOND GATE, on the command arms
///
/// "Allow-listed" answers « le scope Système peut-il être la cible de ce verbe » and stops there. A
/// SECOND, different question — « ce geste exige-t-il une main humaine » — is asked per call by
/// [`crate::system_command_guard`], on the command-CRUD arms below and on
/// [`crate::mcp_runtime`]'s three lifecycle verbs. For a USER project it is inert (one
/// set-membership walk, then the identical body on the identical connection); on the Système
/// singleton it is the approval protocol, and a protected call comes back as
/// [`crate::mcp::RpcCode::ApprovalRequired`] carrying its ticket.
///
/// The two gates are kept apart because they are not decided the same way: the first reads the
/// PAYLOAD, the second reads the payload AND the live run state (`stop` on an idle service is free,
/// `stop` on a running one is not). Folding them would make "the scope can hold commands" and "an
/// agent may create one" the same sentence — the window this phase closed.
///
/// SIDE EFFECT WORTH KNOWING. An UNKNOWN method whose payload aims the singleton now answers
/// JSON-RPC `-32000` (`system_scope_forbidden`) instead of `-32601` (`method_not_found`). That is
/// deliberate: the call was refused either way, and answering "forbidden" removes the incentive to
/// probe the surface for a name that slipped through the allow-list. A caller distinguishing
/// "unknown method" from "refused" must therefore not aim the singleton to find out.
///
/// ONE EXCEPTION TO THAT, and it is the allow-list's other half: an unknown name that happens to be
/// an IPC-ONLY entry (`command_list`, `attach_terminal`, `workspace_runtime_snapshot`…) is NOT
/// refused — it traverses the guard, matches no arm here, and comes back out of the shell's
/// dispatcher as `method_not_found`. Nothing leaks, because none of those names is an advertised
/// tool — which `system_scope::tests::the_two_halves_of_the_allow_list_do_not_share_a_name_with_the_live_mcp_surface`
/// pins — but the probe-resistance above does not extend to them.
/// The three-argument spelling of [`crate::system_command_guard::run_db_mutation`] for the arms
/// whose conn-scoped body needs no runner — written once so the dispatcher's table stays readable.
fn guarded_db<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    tool: &str,
    args: &Value,
    caller_terminal_id: Option<&str>,
    body: impl FnOnce(&mut SqliteConnection, &Value) -> Result<ToolOutcome, RpcError>,
) -> Result<ToolOutcome, RpcError> {
    crate::system_command_guard::run_db_mutation(
        db,
        runner,
        &crate::system_approvals::SystemClock,
        tool,
        args,
        caller_terminal_id,
        body,
    )
}

pub fn dispatch_extension_tool<S: RunnerSink>(
    db: &Db,
    runner: &CommandRunner<S>,
    host: &dyn TerminalHost,
    activity: &AgentActivityStore,
    name: &str,
    args: &Value,
    caller_terminal_id: Option<&str>,
) -> Option<Result<ToolOutcome, RpcError>> {
    // THE SYSTÈME DOMAIN BOUNDARY, checked BEFORE any tool body runs (a refusal must cost nothing,
    // and must certainly not happen after a half-applied write).
    //
    // This dispatcher is the LAST step of the shell's routing and the entry to every sub-dispatcher
    // (Spec, context, ideas, annexes, graph authoring, prototypes, design, engine), so one check
    // here covers the whole remaining surface — instead of one `require_user_project` per tool,
    // which is exactly the shape that ends up with a hole in it.
    //
    // It fires for any name that is on NEITHER half of the allow-list and whose payload aims the
    // singleton, including one this dispatcher does not serve: answering `system_scope_forbidden` to
    // `nonexistent_tool(project_id: <système>)` rather than `method_not_found` costs nothing (the
    // call was refused either way) and removes the temptation to probe the surface for a name that
    // slipped through the table.
    if let Some(refusal) = crate::mcp_context::refuse_system_scope(db, name, args) {
        return Some(Err(refusal));
    }
    // Read tools wrap their plain JSON result into a no-effect `ToolOutcome`.
    let read = |r: Result<Value, RpcError>| Some(r.map(ToolOutcome::read));
    let dispatched = match name {
        // Pure DB reads.
        crate::mcp::PROBE_TOOL => read(probe(db)),
        "list_projects" => read(list_projects(db)),
        "list_workspaces" => read(list_workspaces(db, args)),
        "list_commands" => read(list_commands(db, runner, args)),
        // The agent's DEDICATED discovery of the Système scope's commands — the one door onto a
        // container `list_projects` / `list_workspaces` deliberately omit. Read-only.
        crate::mcp::LIST_SYSTEM_COMMANDS_TOOL => read(list_system_commands(db, runner, args)),
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
        // The v2.4 review COMMENT post: the AGENT leaves a comment/reply. Mutating → carries a
        // `Repos` changed effect so the UI re-pulls the feed. The SAME `db::insert_review_comment`
        // the UI bridge drives.
        crate::mcp::REVIEW_COMMENT_POST_TOOL => Some(review_comment_post(db, args)),
        // The v2.5.11 review COMMENT resolve: the AGENT closes ITS OWN (agent) comment; a HUMAN
        // comment is refused (human-only). Mutating → `Repos` changed. Routes through
        // `db::resolve_agent_comment` (the MCP counterpart of the human-only UI resolve).
        crate::mcp::REVIEW_COMMENT_RESOLVE_TOOL => Some(review_comment_resolve(db, args)),
        // The review FINDING list (v2.5.11 cutover): the GRILLED findings of a Spec — the SAME
        // `db::list_grilled_findings_for_spec` (`tasks kind=review`, NOT `review_tasks`) the UI
        // bridge drives. Read-only.
        crate::mcp::REVIEW_TASK_LIST_TOOL => read(review_task_list(db, args)),
        // The review FINDING structure: the AGENT structures a proposed finding (never validates —
        // there is no validate tool). Mutating → carries a `Repos` changed effect so the UI
        // re-pulls the task list + unread badge. The SAME `db::structure_finding` (`tasks kind=review`).
        crate::mcp::REVIEW_TASK_STRUCTURE_TOOL => Some(review_task_structure(db, args)),
        // The v2.5.11 RETOUR-CHAT channel: the agent relays the human's chat feedback into the
        // review — a HUMAN global comment + a proposed finding. Mutating → `Repos` changed.
        crate::mcp::REVIEW_SUBMIT_FEEDBACK_TOOL => Some(review_submit_feedback(db, args)),
        crate::mcp::LIST_IMPORTABLE_SCRIPTS_TOOL => read(list_importable_scripts(db, args)),
        // Project settings (rename + resume opt-in) — mutating → workspaces changed.
        crate::mcp::SET_PROJECT_SETTINGS_TOOL => Some(set_project_settings(db, args)),
        // Explicit repository config (finding #27): SET a project's repos — mutating → repos
        // changed so any open review/cockpit re-pulls. The SAME `db::set_project_repositories`.
        crate::mcp::PROJECT_SET_REPOS_TOOL => Some(project_set_repos(db, args)),
        // The Système approval protocol's ONE extra tool: the TOKEN-OWNED read of a single
        // request. Grant/deny are NEVER here — they are UI-only db-commands, because a tool that
        // lets the caller approve its own request is not an approval.
        crate::mcp::SYSTEM_APPROVAL_STATUS_TOOL => read(
            crate::system_command_guard::system_approval_status(
                db,
                &crate::system_approvals::SystemClock,
                args,
            ),
        ),
        // Command-template CRUD (mutating → carry a `changed` effect).
        //
        // EACH goes through `system_command_guard::run_db_mutation`, which is a NO-OP for a user
        // project (one set-membership walk, then the very same body on the very same connection)
        // and, on the Système singleton, is the approval protocol: raise a request and refuse with
        // its ticket, or claim the ticket INSIDE the mutation's own transaction. That placement is
        // what makes "impossible de créer une commande arbitraire puis la démarrer librement" true
        // — the creation never happens without a human.
        crate::mcp::ADD_COMMAND_TOOL => Some(guarded_db(db, runner, name, args, caller_terminal_id, add_command_on_conn)),
        crate::mcp::UPDATE_COMMAND_TOOL => Some(crate::system_command_guard::run_db_mutation(
            db,
            runner,
            &crate::system_approvals::SystemClock,
            name,
            args,
            caller_terminal_id,
            |c, a| update_command_on_conn(c, runner, a),
        )),
        // `import_commands` is the ONE arm with a PREPARE step: its package.json discovery is a
        // filesystem walk, and a walk inside the claim's transaction would hold SQLite's write lock
        // for its whole duration (finding 01M15REGB574G6HD7WZ0MRQVXC). The walk runs on the same
        // connection, after the credential check and before the transaction opens.
        crate::mcp::IMPORT_COMMANDS_TOOL => Some(crate::system_command_guard::run_db_mutation_prepared(
            db,
            runner,
            &crate::system_approvals::SystemClock,
            name,
            args,
            caller_terminal_id,
            |c, a| discover_importable(c, a),
            import_commands_on_conn,
        )),
        crate::mcp::REMOVE_WORKSPACE_TOOL => Some(remove_workspace(db, runner, host, args)),
        crate::mcp::REMOVE_COMMAND_TOOL => Some(crate::system_command_guard::run_db_mutation(
            db,
            runner,
            &crate::system_approvals::SystemClock,
            name,
            args,
            caller_terminal_id,
            |c, a| remove_command_on_conn(c, runner, a),
        )),
        crate::mcp::REMOVE_COMMANDS_TOOL => Some(crate::system_command_guard::run_db_mutation(
            db,
            runner,
            &crate::system_approvals::SystemClock,
            name,
            args,
            caller_terminal_id,
            |c, a| remove_commands_on_conn(c, runner, a),
        )),
        // The multi-pane GROUPING of commands (review #90): one tool, eight actions, delegating
        // to the SAME `db::` transitions the UI's `command_group_*` routes call.
        crate::mcp::MANAGE_COMMAND_GROUP_TOOL => Some(guarded_db(db, runner, name, args, caller_terminal_id, manage_command_group_on_conn)),
        // The ORDER of the command LIST — the agent's half of `command_reorder`, over the same
        // `db::reorder_templates` (review 01M11WE6H9E5D5FCS6RT0ZXAXN).
        crate::mcp::REORDER_COMMANDS_TOOL => Some(guarded_db(db, runner, name, args, caller_terminal_id, reorder_commands_on_conn)),
        // Clearing a captured buffer is a RUNNER effect (it empties an in-memory tail), so it takes
        // the other discipline: claim BEFORE the I/O, one attempt.
        crate::mcp::CLEAR_COMMAND_OUTPUT_TOOL => {
            Some(crate::system_command_guard::run_runner_effect(
                db,
                runner,
                &crate::system_approvals::SystemClock,
                name,
                args,
                caller_terminal_id,
                || clear_command_output(db, runner, args),
            ))
        }
        // Agent-session channel (session lifecycle + runtime activity).
        crate::mcp::AGENT_SESSION_EVENT_TOOL => {
            Some(agent_session_event(db, runner, activity, args))
        }
        // The v2.8 ENGINE run-lifecycle entry: OPEN a run and get its run_id (the dispatcher calls
        // this once, then loops on nyx_next). Resolves the workflow + optional Spec binding. It
        // reaches the SAME process-global engine registry the tick below does, because opening a run
        // can REPLACE a dead one and deleting that row takes the very tick guard a live tick holds.
        crate::mcp_engine::RUN_START_TOOL => Some(crate::mcp_engine::run_start(
            db,
            crate::mcp_engine::shared_runtime(),
            args,
        )),
        // The RECOVERY twin of the entry above: a run already open, and no Spec on it. It reaches
        // the SAME process-global registry because "is this run yours?" is a question only that
        // registry answers — `run_list` enumerates the whole installation's runs.
        crate::mcp_engine::RUN_BIND_SPEC_TOOL => Some(crate::mcp_engine::run_bind_spec(
            db,
            crate::mcp_engine::shared_runtime(),
            args,
        )),
        // The v2.8 ENGINE runtime tick: the thin dispatcher calls this in a loop. It advances the
        // run's behavior tree, runs the gate_check managed commands ITSELF over the PTY (refusing
        // to advance while a blocking gate is red), imposes state transitions through the guarded
        // Spec authority, and returns the next atomic action. Uses the session-global engine
        // registry so the signature stays stable across shells. The CALLER SESSION rides along —
        // the terminal this MCP connection is bound to — because the consent check (H6) is a
        // question about who is on the other end (it reads that terminal's persisted human-turn
        // ledger itself, from the same `db`).
        crate::mcp_engine::NYX_NEXT_TOOL => Some(crate::mcp_engine::nyx_next(
            db,
            crate::mcp_engine::shared_runtime(),
            crate::mcp_engine::CallerSession {
                terminal_id: caller_terminal_id,
            },
            args,
        )),
        // Interactive-terminal tools (DB-record half + the shell's TerminalHost).
        crate::mcp::CREATE_TERMINAL_TOOL => Some(create_terminal(db, host, args)),
        crate::mcp::SEND_TO_TERMINAL_TOOL => Some(send_to_terminal(db, host, activity, args)),
        crate::mcp::SEND_KEYS_TOOL => Some(send_keys(db, host, activity, args)),
        crate::mcp::LIST_TERMINALS_TOOL => read(list_terminals(db, host, activity, args)),
        crate::mcp::CLOSE_TERMINAL_TOOL => Some(close_terminal(db, host, args)),
        crate::mcp::READ_TERMINAL_TOOL => read(read_terminal(db, host, activity, args)),
        crate::mcp::WAIT_FOR_TERMINAL_TOOL => read(wait_for_terminal(db, host, activity, args)),
        // The v2.4 Phase-5 tools (routed by their own sub-dispatchers so a Spec/context tool
        // is added in exactly one place): the Spec authoring + transition tools
        // (create/update/fork/transition Spec, add/transition/depend tasks) and the
        // agent-context render — each delegating to the SHARED `nyx_core::db` authority
        // (single mutation path, no double implementation).
        _ => crate::mcp_tools_spec::dispatch_spec_tool(db, name, args)
            .or_else(|| crate::mcp_context::dispatch_context_tool(db, name, args))
            // The v2.6 IDEAS tools (capture + search + maintenance + Spec links) — routed by
            // their own sub-dispatcher so an idea tool is added in exactly one place, each
            // delegating to the SHARED `nyx_core::db` idea authority (single mutation path).
            .or_else(|| crate::mcp_tools_idea::dispatch_idea_tool(db, name, args))
            // The v2.6 ANNEXE tools (attach doc/artefact/url/prototype to a Spec, dedup by
            // content_hash) — same one-place sub-dispatcher, delegating to the SHARED
            // `nyx_core::db` annexe authority.
            .or_else(|| crate::mcp_tools_annexe::dispatch_annexe_tool(db, name, args))
            // The v2.8 GRAPH-AUTHORING tools (workflow_create/fork/assign + node CRUD) — the MCP
            // layer over the Phase-2 workflow/node CRUD, adding the tree-coherence guard that
            // refuses an incoherent graph. Same one-place sub-dispatcher.
            .or_else(|| crate::mcp_authoring::dispatch_authoring_tool(db, name, args))
            // The v2.7 PROTOTYPE tools (the agent's constrained proto contract: read the explicit
            // state, present a round, elect a variant, raise/resolve a pre-dev comment) — same
            // one-place sub-dispatcher, delegating to the SHARED `nyx_core::db` prototype
            // authority (which records the `proto.*` domain events in-transaction).
            .or_else(|| crate::mcp_tools_proto::dispatch_proto_tool(db, name, args))
            // The v2.7 DESIGN-SYSTEM trigger (review #23): `design_inventory` populates the
            // component catalogue from the project's repos + re-syncs the visual hashes — same
            // one-place sub-dispatcher, delegating to the SHARED `nyx_core::design::inventory_project`.
            .or_else(|| crate::mcp_tools_design::dispatch_design_tool(db, name, args)),
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
        // The PROTO twin (v2.7 — the proto ↔ session seam, task 01KY50ZPVSMZPSY351VJNR7Z0F): a
        // `proto_*` MUTATION attributes the worked PROTOTYPE to the calling terminal's live
        // session, so the future proto overlay resolves "the prototype of this terminal" the same
        // way it resolves its Specs. Best-effort + ciblée: `worked_prototype_id` returns `Some`
        // ONLY for the mutating proto tools (a `proto_get` read returns `None`).
        if let Some(prototype_id) =
            crate::mcp_tools_proto::worked_prototype_id(name, &outcome.result)
        {
            let _ = db.with_conn(|c| {
                db::link_terminal_active_session_prototype(c, caller, &prototype_id)
            });
        }
    }
    dispatched
}

#[cfg(test)]
pub(crate) mod tests {
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

    /// SERIALIZE the tests that touch process-global state: the in-memory `Db` singleton, and the
    /// [`LIVE_TERMINAL_WAITS`] slot counter every `wait_for_terminal` competes for. `pub(crate)`
    /// because the #K18 pool test lives with the HTTP harness in [`crate::mcp`] and must take the
    /// SAME lock — it parks blocking waits for seconds, and any wait running beside it would be
    /// refused a slot it had every right to.
    pub(crate) fn db_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// [`agent_session_event`] over a runner with NOTHING live on it — the shape every
    /// session-lifecycle / activity test wants: the handler only READS the runner (to state the
    /// FACTUAL run state of the workspace's commands in a `SessionStart`'s context), so an empty one
    /// makes those tests fall back on the persisted `last_state`, which is what they seed. A test
    /// that needs a LIVE state passes its own runner to `agent_session_event` directly.
    fn session_event(
        db: &Db,
        activity: &AgentActivityStore,
        args: &Value,
    ) -> Result<ToolOutcome, RpcError> {
        agent_session_event(db, &noop_runner(), activity, args)
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
        assert_eq!(runtime["workspace"]["workspace_id"], json!(ws.id));
        // Terminal (A10: identified and stated, its bytes not inlined).
        let terminals = runtime["terminals"].as_array().unwrap();
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["terminal_id"], json!(term.id));
        assert_eq!(terminals[0]["live_sessions"], json!(1));
        // Cross-terminal agent view.
        let sessions = runtime["agent_sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["external_session_id"], json!("sess-mcp"));
        // Service with its (persisted-idle) run state.
        let services = runtime["services"].as_array().unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0]["name"], json!("dev"));
        assert_eq!(services[0]["run_state"], json!("idle"));

        // An unknown workspace id is a D8 invalid_id error (not an empty snapshot).
        let err = workspace_runtime(&db, &runner, &json!({ "workspace_id": "nope" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    /// A10 — THE RUNTIME VIEW IS BOUNDED ON WHAT IS LIVE, AND THE HISTORY IS PAGED.
    ///
    /// The shared snapshot is a UI read model, and the UI is right to want it whole. An MCP caller
    /// pays for every byte of it in context, and what it got was the entire history inline: ~35
    /// TERMINATED agent sessions, every terminal record's full `scrollback`, every service's too,
    /// with no bound and no way to ask for less.
    ///
    /// MEASURED HERE, before and after, on the shape the audit met: 12 finished sessions over 12
    /// terminals each holding 64 KiB of scrollback, plus a service with 64 KiB of its own. The
    /// assertion is the ratio and the cap, not a fixed number — the point is that a naive call is
    /// safe by construction.
    #[test]
    fn workspace_runtime_does_not_inline_the_history_and_pages_it() {
        use crate::usecase::{self, test_support::RecordingEventSink, DomainEventSource};
        let _g = db_guard();
        let db = Db::in_memory();
        let sink = RecordingEventSink::new();
        let (project, _root) = db
            .with_conn(|c| {
                usecase::create_project(c, &sink, "P", "/tmp/wr-big", None, DomainEventSource::Mcp)
            })
            .unwrap();
        let ws = db
            .with_conn(|c| {
                usecase::register_workspace(
                    c,
                    &sink,
                    &project.id,
                    "wt",
                    "/tmp/wr-big-wt",
                    DomainEventSource::Mcp,
                )
            })
            .unwrap();

        // A service carrying real output.
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
        let instance = db
            .with_conn(|c| db::instance_ids_for_workspace(c, &ws.id))
            .unwrap()
            .pop()
            .expect("one instance");
        let fat = "s".repeat(64 * 1024);
        db.with_conn(|c| db::persist_instance_scrollback(c, &instance, &fat))
            .unwrap();

        // Twelve terminals, each with 64 KiB of scrollback and one ENDED agent session.
        for i in 0..12 {
            let term = db
                .with_conn(|c| db::create_terminal(c, "/tmp/wr-big-wt", None))
                .unwrap();
            db.with_conn(|c| db::attach_terminal(c, &term.id, &ws.id, "auto"))
                .unwrap();
            db.with_conn(|c| db::persist_scrollback(c, &term.id, &fat))
                .unwrap();
            let session = db
                .with_conn(|c| {
                    db::record_session_start(
                        c,
                        &term.id,
                        "claude_code",
                        db::SessionCapture {
                            workspace_id: Some(ws.id.clone()),
                            external_session_id: format!("sess-{i}"),
                            cwd: "/tmp/wr-big-wt".to_string(),
                            transcript_path: Some(format!("/tmp/transcripts/{i}.jsonl")),
                            metadata_json: Some("{\"noise\":\"x\"}".to_string()),
                        },
                    )
                })
                .unwrap();
            db.with_conn(|c| db::mark_session_ended(c, &session.id))
                .unwrap();
        }

        // BEFORE: the raw shared snapshot, the shape the tool used to echo verbatim.
        let raw = db
            .with_conn(|c| {
                crate::workspace_runtime::snapshot(
                    c,
                    &ws.id,
                    &crate::workspace_runtime::NoRunStateOverlay,
                )
            })
            .unwrap()
            .expect("snapshot");
        let before = crate::mcp_runtime::json_len(&json!({ "runtime": raw }));

        // AFTER: the tool's own answer, with no arguments at all.
        let runner = noop_runner();
        let res = workspace_runtime(&db, &runner, &json!({ "workspace_id": ws.id })).unwrap();
        let after = crate::mcp_runtime::json_len(&res);

        // CONSIGNED, the way phase 4 consigned its own: run this test with `--nocapture` to read it.
        eprintln!("A10 workspace_runtime — before: {before} bytes, after: {after} bytes");
        assert!(
            before > 800_000,
            "the raw snapshot really is the elephant this finding measured: {before} bytes"
        );
        assert!(
            after < crate::mcp_runtime::MAX_RESPONSE_BYTES,
            "a naive call fits the response budget: {after} bytes (was {before})"
        );
        assert!(
            after * 20 < before,
            "…and by a wide margin, not a haircut: {after} bytes vs {before}"
        );

        // NOT ONE BYTE OF SCROLLBACK IS INLINED — the whole payload holds no run of the filler.
        let serialized = serde_json::to_string(&res).unwrap();
        assert!(
            !serialized.contains(&"s".repeat(1024)),
            "no scrollback is carried inline"
        );
        let runtime = &res["runtime"];
        assert_eq!(
            runtime["terminals"][0]["scrollback_bytes"],
            json!(64 * 1024),
            "…but its SIZE is stated, so a caller knows whether a read_terminal is worth it"
        );
        assert_eq!(runtime["services"][0]["scrollback_bytes"], json!(64 * 1024));

        // The ENDED sessions carry their POINTER, never their bag.
        let page = runtime["agent_sessions"].as_array().unwrap();
        assert!(!page.is_empty());
        for s in page {
            assert_eq!(s["state"], "ended");
            assert!(
                s["transcript_path"].is_string(),
                "an ended session names the file holding its full output: {s}"
            );
            assert_eq!(
                s["metadata_json"],
                Value::Null,
                "…and drops the adapter bag nobody reads from a runtime view: {s}"
            );
        }

        // PAGED with the SAME grammar as the other paged reads of this surface.
        assert_eq!(runtime["agent_sessions_total"], json!(12));
        assert_eq!(runtime["agent_sessions_offset"], json!(0));
        assert_eq!(runtime["live_agent_sessions"], json!(0));
        let one = workspace_runtime(
            &db,
            &runner,
            &json!({ "workspace_id": ws.id, "limit": 5 }),
        )
        .unwrap();
        let one = &one["runtime"];
        assert_eq!(one["agent_sessions"].as_array().unwrap().len(), 5);
        assert_eq!(one["agent_sessions_returned"], json!(5));
        assert_eq!(one["agent_sessions_truncated"], json!(true));
        assert_eq!(one["agent_sessions_next_offset"], json!(5));
        let two = workspace_runtime(
            &db,
            &runner,
            &json!({ "workspace_id": ws.id, "limit": 5, "offset": 5 }),
        )
        .unwrap();
        assert_eq!(two["runtime"]["agent_sessions_next_offset"], json!(10));
        let three = workspace_runtime(
            &db,
            &runner,
            &json!({ "workspace_id": ws.id, "limit": 5, "offset": 10 }),
        )
        .unwrap();
        assert_eq!(
            three["runtime"]["agent_sessions_next_offset"],
            Value::Null,
            "following next_offset terminates"
        );
    }

    /// A10 — A LIVE SESSION COMES FIRST AND KEEPS WHAT A LIVE SESSION IS FOR.
    #[test]
    fn workspace_runtime_puts_the_live_sessions_first() {
        use crate::usecase::{self, test_support::RecordingEventSink, DomainEventSource};
        let _g = db_guard();
        let db = Db::in_memory();
        let sink = RecordingEventSink::new();
        let (project, _root) = db
            .with_conn(|c| {
                usecase::create_project(c, &sink, "P", "/tmp/wr-live", None, DomainEventSource::Mcp)
            })
            .unwrap();
        let ws = db
            .with_conn(|c| {
                usecase::register_workspace(
                    c,
                    &sink,
                    &project.id,
                    "wt",
                    "/tmp/wr-live-wt",
                    DomainEventSource::Mcp,
                )
            })
            .unwrap();
        let start = |external: &str, end: bool| {
            let term = db
                .with_conn(|c| db::create_terminal(c, "/tmp/wr-live-wt", None))
                .unwrap();
            db.with_conn(|c| db::attach_terminal(c, &term.id, &ws.id, "auto"))
                .unwrap();
            let session = db
                .with_conn(|c| {
                    db::record_session_start(
                        c,
                        &term.id,
                        "claude_code",
                        db::SessionCapture {
                            workspace_id: Some(ws.id.clone()),
                            external_session_id: external.to_string(),
                            cwd: "/tmp/wr-live-wt".to_string(),
                            transcript_path: None,
                            metadata_json: Some("{\"live\":true}".to_string()),
                        },
                    )
                })
                .unwrap();
            if end {
                db.with_conn(|c| db::mark_session_ended(c, &session.id))
                    .unwrap();
            }
        };
        // Three ended sessions opened BEFORE the live one, so ordering by time alone would bury it.
        start("dead-1", true);
        start("dead-2", true);
        start("dead-3", true);
        start("alive", false);

        let res =
            workspace_runtime(&db, &noop_runner(), &json!({ "workspace_id": ws.id })).unwrap();
        let sessions = res["runtime"]["agent_sessions"].as_array().unwrap();
        assert_eq!(
            sessions[0]["external_session_id"], "alive",
            "the LIVE session leads the page: a default read is about now, not archaeology"
        );
        assert_eq!(
            sessions[0]["metadata_json"], "{\"live\":true}",
            "…and a live session keeps its bag"
        );
        assert_eq!(res["runtime"]["live_agent_sessions"], json!(1));
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

    /// DONE-CRITERION (v2.5.11 review follow-up finding 01KXZYWGBQEJZ9BM39NYGFKHMJ): a comment is
    /// resolved BY ITS AUTHOR. The agent's `review_comment_resolve` MCP tool closes the agent's OWN
    /// (author=agent) comment (stamped `resolved_by_type=agent`) and REFUSES a HUMAN comment
    /// (`invalid_state` — human-only, closed through the UI façade). `review_comment_reopen` stays
    /// NOT an MCP tool (a human-only undo).
    #[test]
    fn mcp_agent_resolves_its_own_comment_and_is_refused_on_a_human_comment() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();

        // The agent posts its OWN comment (author=agent) …
        let posted = dispatch_extension_tool(
            &db,
            &runner,
            &host,
            &activity,
            crate::mcp::REVIEW_COMMENT_POST_TOOL,
            &json!({ "workspace_id": "ws", "repository_id": "repo", "body": "nit: rename this" }),
            None,
        )
        .expect("review_comment_post IS an MCP tool")
        .expect("post ok")
        .result;
        let agent_comment_id = posted["comment"]["id"].as_str().unwrap().to_string();

        // … and resolves it via the MCP tool (author-owned resolution → resolved_by_type=agent).
        let resolved = dispatch_extension_tool(
            &db,
            &runner,
            &host,
            &activity,
            crate::mcp::REVIEW_COMMENT_RESOLVE_TOOL,
            &json!({ "id": agent_comment_id }),
            None,
        )
        .expect("review_comment_resolve IS an MCP tool")
        .expect("resolve ok")
        .result;
        assert_eq!(resolved["comment"]["status"], json!("resolved"));
        assert_eq!(resolved["comment"]["resolved_by_type"], json!("agent"));

        // A HUMAN comment is REFUSED (human-only). Seed one as the human directly.
        let human_id = db
            .with_conn(|c| {
                crate::db::insert_review_comment(
                    c,
                    crate::db::NewComment {
                        workspace_id: "ws".into(),
                        repository_id: "repo".into(),
                        spec_id: None,
                        parent_id: None,
                        author_type: crate::db::AUTHOR_HUMAN.into(),
                        author_name: "You".into(),
                        anchor: None,
                        body: "human note".into(),
                    },
                )
            })
            .unwrap()
            .id;
        let refused = dispatch_extension_tool(
            &db,
            &runner,
            &host,
            &activity,
            crate::mcp::REVIEW_COMMENT_RESOLVE_TOOL,
            &json!({ "id": human_id }),
            None,
        )
        .expect("review_comment_resolve IS an MCP tool")
        .expect_err("a human comment refuses agent resolution");
        assert_eq!(
            refused.code,
            RpcCode::InvalidState,
            "a human comment is human-only (invalid_state)"
        );

        // The human-only UNDO stays off the MCP façade (no reopen tool).
        assert!(
            dispatch_extension_tool(
                &db,
                &runner,
                &host,
                &activity,
                "review_comment_reopen",
                &json!({ "id": "whatever" }),
                None,
            )
            .is_none(),
            "review_comment_reopen must NOT be an MCP tool (human-only undo)"
        );
    }

    /// The GRILL over MCP, cut over to `tasks kind=review` (v2.5.11 Phase 2, PDR-0016): the agent
    /// STRUCTURES a proposed FINDING via `review_task_structure` and lists it via
    /// `review_task_list` (spec-scoped). Proof: a human-promoted (proposed) finding is structured
    /// to a clean task, moves to `structured`, carries the `Repos` changed effect, and shows in the
    /// Spec's grilled-findings list — all on the CANONICAL finding model, no `review_tasks` row.
    #[test]
    fn mcp_review_task_structure_and_list() {
        let _g = db_guard();
        let db = Db::in_memory();

        // A project + Spec + a spec-scoped human comment, promoted to a `proposed` finding (the
        // human's "lâche brut" now creates a `tasks kind=review` row, not a `review_tasks` row).
        let (spec_id, finding) = db
            .with_conn(|c| -> Result<(String, crate::db::Task), crate::db::ContentWriteError> {
                let (project, root) = crate::db::create_project(c, "Proj", "/work", None)?;
                let (spec, _) = crate::db::create_spec(
                    c,
                    &project.id,
                    "Spec",
                    crate::db::SpecSections::empty(),
                )?;
                let comment = crate::db::insert_review_comment(
                    c,
                    crate::db::NewComment {
                        workspace_id: root.id.clone(),
                        repository_id: "repo".into(),
                        spec_id: Some(spec.id.clone()),
                        author_type: crate::db::AUTHOR_HUMAN.into(),
                        author_name: "Kris".into(),
                        body: "fragile".into(),
                        ..Default::default()
                    },
                )?;
                let finding = crate::db::promote_comment_to_finding(c, &comment, "fragile", None)?;
                Ok((spec.id, finding))
            })
            .expect("promote to a finding");
        assert_eq!(finding.kind, crate::db::TASK_KIND_REVIEW);
        assert_eq!(
            finding.grill_state.as_deref(),
            Some(crate::db::GRILL_PROPOSED)
        );

        // The AGENT structures it via MCP (proposed → structured).
        let structured = review_task_structure(
            &db,
            &json!({
                "id": finding.id,
                "title": "Harden the fragile path",
                "details": "Add validation + a test.",
                "severity": "high",
            }),
        )
        .expect("structure");
        // Mutating a finding invalidates BOTH the review viewer (Repos) AND the terminal-spec
        // overlay (Specs — the finding is a `tasks kind=review` row it renders) so both re-pull
        // WITHOUT a reload (review F3).
        assert_eq!(
            structured.effects,
            vec![ChangedTopic::Repos, ChangedTopic::Specs]
        );
        assert_eq!(
            structured.result["task"]["grill_state"],
            json!("structured")
        );
        assert_eq!(structured.result["task"]["severity"], json!("high"));

        // It shows in the Spec's grilled-findings list as `structured` — coder-ready (the grill has
        // no separate validate step, #31). The list is SPEC-scoped now (findings are `tasks`).
        let listed = review_task_list(
            &db,
            &json!({ "workspace_id": "ws", "repository_id": "repo", "spec_id": spec_id }),
        )
        .expect("list");
        let tasks = listed["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["grill_state"], json!("structured"));
        assert_eq!(
            tasks[0]["from_comment_id"],
            finding.from_comment_id.unwrap()
        );
    }

    /// A MALFORMED FINDING HAS A REPAIR PATH, MID-BUILD. Observed live: an auditor's tool call came
    /// out mangled and left a stray `<parameter name=…>` line in the finding's description. Nothing
    /// could clean it — `task_delete` is draft-only, and `skip` cancels the WORK when all that is
    /// wrong is the wording — so a badly-formed finding was permanent for the rest of the build.
    ///
    /// Rewriting a finding's TEXT is not the authority `task_delete` withholds: the freeze exists so
    /// work in progress cannot redefine its own target, and the words of a finding are not its target
    /// — the severity, the phase, the round and the status all stay put, so what the build owes is
    /// unchanged. And a finding raised DIRECTLY (no grill) is not filed into the retour grill by being
    /// corrected: a repair says nothing about a promotion that never happened.
    #[test]
    fn a_malformed_finding_is_repaired_mid_build_without_entering_the_grill() {
        let _g = db_guard();
        let db = Db::in_memory();

        // A Spec driven to in_progress — the FROZEN state where `task_delete` refuses — carrying an
        // auditor finding whose description came out malformed.
        let (spec_id, finding) = db
            .with_conn(|c| -> Result<(String, crate::db::Task), crate::db::ContentWriteError> {
                let (project, root) = crate::db::create_project(c, "Proj", "/work", None)?;
                let (spec, _) = crate::db::create_spec(
                    c,
                    &project.id,
                    "Spec",
                    crate::db::SpecSections {
                        problem: "p".into(),
                        solution: "s".into(),
                        ..Default::default()
                    },
                )?;
                let finding = crate::db::create_task(
                    c,
                    &project.id,
                    "Auditor: retry loop unbounded",
                    crate::db::NewTaskInput {
                        spec_id: Some(spec.id.clone()),
                        description: Some(
                            "Cap the retries.\n<parameter name=\"severity\">high</parameter>".into(),
                        ),
                        ..crate::db::NewTaskInput::review(crate::db::TASK_SEVERITY_HIGH)
                    },
                )?;
                crate::db::transition_spec(c, &spec.id, crate::db::SpecTransition::Ready, true)
                    .expect("ready");
                crate::db::transition_spec_with_workspace(
                    c,
                    &spec.id,
                    crate::db::SpecTransition::Start,
                    true,
                    Some(&root.id),
                )
                .expect("start");
                Ok((spec.id, finding))
            })
            .expect("a Spec under build carrying a malformed finding");
        assert_eq!(
            finding.grill_state, None,
            "a directly-raised auditor finding entered no grill"
        );

        // THE REPAIR — the same words, minus the markup the mangled call left behind.
        let repaired = review_task_structure(
            &db,
            &json!({
                "id": finding.id,
                "title": "Auditor: retry loop unbounded",
                "details": "Cap the retries.",
            }),
        )
        .expect("a finding's text is repairable while the Spec is frozen");
        assert_eq!(
            repaired.result["task"]["description"],
            json!("Cap the retries."),
            "the stray markup is gone"
        );

        // What the build OWES is untouched — the repair changed words, not obligations.
        let stored = db
            .with_conn(|c| crate::db::get_task(c, &finding.id))
            .unwrap()
            .unwrap();
        assert_eq!(stored.severity.as_deref(), Some(crate::db::TASK_SEVERITY_HIGH));
        assert_eq!(stored.status, finding.status);
        assert_eq!(stored.phase, finding.phase);
        assert_eq!(stored.round, finding.round);

        // And it did NOT enter the retour grill: the review viewer's promotion panel lists the
        // findings GRILLED from a comment, and this one was never promoted from one.
        assert_eq!(
            stored.grill_state, None,
            "repairing a directly-raised finding must not stamp a grill state on it"
        );
        let grilled = review_task_list(
            &db,
            &json!({ "workspace_id": "ws", "repository_id": "repo", "spec_id": spec_id }),
        )
        .expect("list");
        assert!(
            grilled["tasks"].as_array().unwrap().is_empty(),
            "a repaired auditor finding does not appear in the promotion panel as a grilled retour"
        );
    }

    /// DONE-CRITERION guard-rail (task 01KX0F6JEKCEV2JCTMEBMM3P4Z; grill collapse #31): the agent's
    /// MCP surface exposes NO promote/validate/mark-read tool of any name — the agent has ONLY
    /// `review_task_list` (read) + `review_task_structure` (structure, which makes a finding
    /// coder-ready; the grill has no separate validate step). Promotion + comment resolution stay on
    /// the UI façade. This is the task-side twin of `mcp_agent_cannot_resolve_a_comment` (phase-4
    /// human-only resolution).
    #[test]
    fn mcp_agent_cannot_promote_or_mark_read_a_task() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();

        // No promote/validate/mark-read tool of any name is dispatchable over MCP (the collapsed
        // grill has no validate tool at all).
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

    /// DONE-CRITERION (v2.5.11 Phase 2, task 01KXR1CZ68C5S9CY2KFB073KDM "Retour-chat = canal de
    /// review"): the agent relays the human's chat feedback via `review_submit_feedback` — it
    /// creates a HUMAN, GLOBAL (non-anchored) comment and GRILLS it into a `proposed`
    /// `tasks kind=review` finding (agent-owned; provenance lives on the comment's `author_type`, not
    /// on the finding). The finding shows in the Spec's grilled-findings list; the diff-viewer
    /// channel (`review_comment_post`) stays valid in parallel.
    #[test]
    fn mcp_review_submit_feedback_creates_a_human_global_grilled_finding() {
        let _g = db_guard();
        let db = Db::in_memory();

        // A project + Spec on a workspace (the finding lives on the Spec).
        let (spec_id, ws_id) = db
            .with_conn(|c| -> Result<(String, String), crate::db::ContentWriteError> {
                let (project, root) = crate::db::create_project(c, "Proj", "/work", None)?;
                let (spec, _) = crate::db::create_spec(
                    c,
                    &project.id,
                    "Spec",
                    crate::db::SpecSections::empty(),
                )?;
                Ok((spec.id, root.id))
            })
            .unwrap();

        // The agent relays the human's chat feedback.
        let out = review_submit_feedback(
            &db,
            &json!({
                "workspace_id": ws_id,
                "repository_id": "repo",
                "spec_id": spec_id,
                "body": "the empty state looks broken",
                "severity": "medium",
            }),
        )
        .expect("submit feedback");
        // The retour writes a comment (viewer, Repos) AND a finding = `tasks kind=review` (overlay,
        // Specs), so it names BOTH topics → both surfaces re-pull without a reload (review F3).
        assert_eq!(
            out.effects,
            vec![ChangedTopic::Repos, ChangedTopic::Specs],
            "mutating a finding → Repos (viewer) + Specs (overlay) changed"
        );

        // The COMMENT is HUMAN + GLOBAL (no anchor — presence = authorization).
        let comment = &out.result["comment"];
        assert_eq!(comment["author_type"], json!(crate::db::AUTHOR_HUMAN));
        assert!(
            comment["file_path"].is_null(),
            "global comment — no file anchor"
        );
        assert!(
            comment["line_start"].is_null(),
            "global comment — no line anchor"
        );
        assert_eq!(comment["body"], json!("the empty state looks broken"));

        // The FINDING is a `proposed` `tasks kind=review` linked to the comment (agent-owned — no
        // `origin`; the human provenance is on the comment's author_type).
        let finding = &out.result["finding"];
        assert_eq!(finding["kind"], json!(crate::db::TASK_KIND_REVIEW));
        assert_eq!(finding["grill_state"], json!(crate::db::GRILL_PROPOSED));
        assert!(
            finding.get("origin").is_none(),
            "a finding carries NO origin (v24 — always agent-created)"
        );
        assert_eq!(finding["severity"], json!("medium"));
        assert_eq!(finding["from_comment_id"], comment["id"]);
        assert_eq!(finding["spec_id"], json!(spec_id));

        // It shows in the Spec's grilled-findings list (the same list the viewer reads).
        let listed = review_task_list(
            &db,
            &json!({ "workspace_id": ws_id, "repository_id": "repo", "spec_id": spec_id }),
        )
        .expect("list");
        let tasks = listed["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["grill_state"], json!("proposed"));

        // COEXISTENCE: the diff-viewer channel (agent anchored comment) still works in parallel.
        let viewer = review_comment_post(
            &db,
            &json!({
                "workspace_id": ws_id,
                "repository_id": "repo",
                "spec_id": spec_id,
                "body": "nit on this line",
                "file_path": "src/x.rs",
                "line_start": 3,
            }),
        )
        .expect("viewer channel still posts");
        assert_eq!(
            viewer.result["comment"]["file_path"],
            json!("src/x.rs"),
            "the viewer stays a valid (anchored) review channel alongside the chat retour"
        );
    }

    /// DONE-CRITERION (v2.5.11 review follow-up, F5 01KXZP1PY71W5P4N3VYJBHP91G): the OPTIONAL `phase`
    /// param on `review_submit_feedback` OVERRIDES the phase nyx would infer (the last delivered), so
    /// a finding can be attributed to an explicit phase; omitting it falls back to the derived phase.
    #[test]
    fn mcp_review_submit_feedback_phase_param_overrides_the_derived_phase() {
        let _g = db_guard();
        let db = Db::in_memory();

        // A Spec with TWO plan phases: One(1) DELIVERED (done), Two(2) still open → the derived
        // last-delivered phase is 1.
        let (spec_id, ws_id) = db
            .with_conn(|c| -> Result<(String, String), crate::db::ContentWriteError> {
                let (project, root) = crate::db::create_project(c, "Proj", "/work", None)?;
                let (spec, _) = crate::db::create_spec(
                    c,
                    &project.id,
                    "Spec",
                    crate::db::SpecSections::empty(),
                )?;
                for (title, phase, done) in [("t1", "One", true), ("t2", "Two", false)] {
                    let t = crate::db::create_task(
                        c,
                        &project.id,
                        title,
                        crate::db::NewTaskInput {
                            spec_id: Some(spec.id.clone()),
                            phase_name: Some(phase.to_string()),
                            ..crate::db::NewTaskInput::slice()
                        },
                    )?;
                    if done {
                        crate::db::transition_task(
                            c,
                            &t.id,
                            crate::db::TaskTransition::Start,
                            None,
                        )
                        .expect("start");
                        crate::db::transition_task(c, &t.id, crate::db::TaskTransition::Done, None)
                            .expect("deliver");
                    }
                }
                Ok((spec.id, root.id))
            })
            .unwrap();

        // Without `phase`: the finding falls back to the DERIVED last-delivered phase (1).
        let derived = review_submit_feedback(
            &db,
            &json!({
                "workspace_id": ws_id,
                "repository_id": "repo",
                "spec_id": spec_id,
                "body": "derived",
            }),
        )
        .expect("submit");
        assert_eq!(
            derived.result["finding"]["phase"],
            json!(1),
            "no phase param → the finding falls back to the derived last-delivered phase (1)"
        );

        // With `phase=2`: the explicit param OVERRIDES the derived (1) → the finding lands on phase 2.
        let overridden = review_submit_feedback(
            &db,
            &json!({
                "workspace_id": ws_id,
                "repository_id": "repo",
                "spec_id": spec_id,
                "body": "explicit",
                "phase": 2,
            }),
        )
        .expect("submit with phase");
        assert_eq!(
            overridden.result["finding"]["phase"],
            json!(2),
            "an explicit phase param overrides the derived phase → the finding lands on phase 2"
        );
    }

    /// #K13 — A FINDING CARRIES ITS PHASE **NAME** ON EVERY REVIEW SURFACE.
    ///
    /// `task_json` is the single wire projection of a task, and what closes the rank/name ambiguity:
    /// the column holds a RANK, the write path takes a NAME, so a task read back raw says "phase 2"
    /// to a caller that wrote "Wiring". The three review surfaces handed back the `db::Task` (or, for
    /// `review_task_list`, a `ReviewFinding` that copied `phase` alone) — so an agent that structured
    /// a finding and re-read it got `phase_name: undefined`.
    ///
    /// This writes a phase by NAME, then walks all three surfaces and requires the SAME pair back.
    #[test]
    fn every_review_surface_returns_the_phase_name_the_finding_was_written_on() {
        let _g = db_guard();
        let db = Db::in_memory();

        // A Spec whose plan carries a NAMED phase 2 (a task written on "Wiring" makes it exist).
        let (spec_id, ws_id) = db
            .with_conn(|c| -> Result<(String, String), crate::db::ContentWriteError> {
                let (project, root) = crate::db::create_project(c, "Proj", "/work", None)?;
                let (spec, _) =
                    crate::db::create_spec(c, &project.id, "Spec", crate::db::SpecSections::empty())?;
                for phase in ["Socle", "Wiring"] {
                    crate::db::create_task(
                        c,
                        &project.id,
                        phase,
                        crate::db::NewTaskInput {
                            spec_id: Some(spec.id.clone()),
                            phase_name: Some(phase.to_string()),
                            ..crate::db::NewTaskInput::slice()
                        },
                    )?;
                }
                Ok((spec.id, root.id))
            })
            .unwrap();

        // 1. `review_submit_feedback` — the finding it hands back names its phase.
        let submitted = review_submit_feedback(
            &db,
            &json!({
                "workspace_id": ws_id,
                "repository_id": "repo",
                "spec_id": spec_id,
                "body": "le wiring ne branche pas le sweeper",
                "phase": 2,
            }),
        )
        .expect("submit");
        let finding = &submitted.result["finding"];
        assert_eq!(finding["phase"], json!(2));
        assert_eq!(
            finding["phase_name"],
            json!("Wiring"),
            "the phase the caller NAMED comes back named: {finding}"
        );
        let finding_id = finding["id"].as_str().expect("the finding has an id").to_string();

        // 2. `review_task_structure` — the same pair after the grill's structuring step.
        let structured = review_task_structure(
            &db,
            &json!({ "id": finding_id, "title": "Brancher le sweeper au boot" }),
        )
        .expect("structure");
        assert_eq!(structured.result["task"]["phase"], json!(2));
        assert_eq!(
            structured.result["task"]["phase_name"],
            json!("Wiring"),
            "structuring a finding must not drop the name: {}",
            structured.result
        );

        // 3. `review_task_list` — and the same pair on the read that lists them.
        let listed = review_task_list(
            &db,
            &json!({ "workspace_id": ws_id, "repository_id": "repo", "spec_id": spec_id }),
        )
        .expect("list");
        let row = listed["tasks"]
            .as_array()
            .expect("an array of findings")
            .iter()
            .find(|t| t["id"] == json!(finding_id))
            .expect("the finding is listed");
        assert_eq!(row["phase"], json!(2));
        assert_eq!(
            row["phase_name"],
            json!("Wiring"),
            "the list carries the name too — one form of a task on the wire: {row}"
        );
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

    /// DONE-CRITERION (task 01KXN0HTXFMZAJ0GTVWVBN2PGZ ; proto parity task
    /// 01KY50ZPVSMZPSY351VJNR7Z0F): "clear vide le set (overlay disparait) ; compact et resume ne
    /// demappent pas ; nouvelle session = set vide". The un-map: a `SessionEnd reason=clear`
    /// vacates BOTH the calling session's Spec set AND its prototype set (pushing `specs://changed`
    /// AND `prototypes://changed` so both overlays re-pull empty) WITHOUT ending the session, while
    /// `reason=resume` keeps both. The proto set is vacated "COMME les Specs" (PDR-0009 — the proto
    /// hangs off the same agent session). (`/compact` fires no `SessionEnd`, so it never reaches
    /// this path — no un-map by construction.)
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
            // A prototype hangs off Spec A; the session WORKS it (its proto set, PDR-0009 — the
            // proto is rattaché à la SESSION AGENT, COMME les Specs).
            let proto = crate::db::create_prototype(c, &a.id, Some("A's UI")).unwrap();
            crate::db::link_session_prototype(c, &session.id, &proto.id).unwrap();
            (session.id, terminal.id)
        });
        let set = |db: &Db| {
            db.with_conn(|c| crate::db::specs_for_terminal(c, &terminal_id))
                .unwrap()
        };
        let proto_set = |db: &Db| {
            db.with_conn(|c| crate::db::prototypes_for_terminal(c, &terminal_id))
                .unwrap()
        };
        assert_eq!(
            set(&db).len(),
            2,
            "the session starts with a set of 2 Specs"
        );
        assert_eq!(
            proto_set(&db).len(),
            1,
            "the session starts with a set of 1 prototype"
        );

        // A `/clear` (SessionEnd reason=clear) VACATES the set and pushes `specs://changed`,
        // but the session row stays ACTIVE (an internal transition — never ended).
        let cleared = session_event(
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
            vec![ChangedTopic::Specs, ChangedTopic::Prototypes],
            "a real un-map pushes specs://changed AND prototypes://changed so both overlays re-pull empty"
        );
        assert!(
            set(&db).is_empty(),
            "clear vacated the session's Spec set (overlay disappears)"
        );
        assert!(
            proto_set(&db).is_empty(),
            "clear ALSO vacated the session's prototype set (parité « comme les Specs », PDR-0009)"
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

        // Re-populate BOTH sets, then a `/resume` (SessionEnd reason=resume) must NOT un-map.
        let spec_c = db.with_conn(|c| {
            let (project, _) = crate::db::create_project(c, "P2", "/work2", None).unwrap();
            let (spec, _) =
                crate::db::create_spec(c, &project.id, "C", crate::db::SpecSections::empty())
                    .unwrap();
            crate::db::link_session_spec(c, &session_id, &spec.id).unwrap();
            let proto = crate::db::create_prototype(c, &spec.id, None).unwrap();
            crate::db::link_session_prototype(c, &session_id, &proto.id).unwrap();
            spec.id
        });
        assert_eq!(set(&db).len(), 1, "the set has one Spec again");
        assert_eq!(
            proto_set(&db).len(),
            1,
            "the proto set has one prototype again"
        );
        let resumed = session_event(
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
        assert_eq!(
            proto_set(&db).len(),
            1,
            "resume keeps the prototype set intact too"
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

    /// DONE-CRITERION (« Frontière de domaine ») — the boundary fires through the REAL dispatcher,
    /// not only through the guard read in isolation: an agent that holds the Système project's id
    /// gets `system_scope_forbidden` from the tools it may not use, and a normal answer from the
    /// ones it may.
    ///
    /// The Git family is here too (« Workspace Système hors des lectures Git »): `workspace_git`,
    /// `repo_diff` and `repo_git_view` are refused BEFORE they resolve a path, so no Git subprocess
    /// is even reached — the home is not a repository the agent may walk.
    #[test]
    fn the_dispatcher_refuses_the_system_scope_for_every_capability_it_does_not_have() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();

        let scope = db
            .with_conn(|c| {
                crate::system_scope::ensure_system_scope(
                    c,
                    &crate::system_scope::FixedHomeDirProvider::at(if cfg!(windows) {
                        "C:\\Users\\kris"
                    } else {
                        "/home/kris"
                    }),
                )
            })
            .expect("bootstrap the Système scope");
        let args = json!({
            "project_id": scope.project.id,
            "workspace_id": scope.workspace.id,
            "repository_id": "whatever",
            "kind": "working_tree",
        });

        for tool in [
            // the project's own lifecycle + settings
            "set_project_settings",
            "project_set_repos",
            // workspace lifecycle + inventory
            "list_workspaces",
            "remove_workspace",
            // GIT — refused before any path resolution, hence before any subprocess
            crate::mcp::WORKSPACE_GIT_TOOL,
            crate::mcp::REPO_DIFF_TOOL,
            crate::mcp::REPO_GIT_VIEW_TOOL,
            // the runtime aggregate is a UI read, not an agent one
            crate::mcp::WORKSPACE_RUNTIME_TOOL,
            // product aggregates
            "spec_create",
            "idea_capture",
            "workflow_create",
        ] {
            let err = dispatch_extension_tool(
                &db, &runner, &host, &activity, tool, &args, None,
            )
            .unwrap_or_else(|| panic!("`{tool}` must be served (refused), not fall through"))
            .expect_err("this capability is not the Système scope's");
            assert_eq!(
                err.code,
                RpcCode::SystemScopeForbidden,
                "`{tool}` must refuse with the dedicated code, got {err:?}"
            );
            // The WIRE spelling, not merely the variant: a caller discriminates on the string, so
            // "this exists and you may not" must reach it as `system_scope_forbidden` and never as
            // an unknown-id code. (Asserting `!= InvalidId` right after the `==` above would have
            // been a tautology — two variants of one enum — and would have proven nothing.)
            assert_eq!(
                err.code.as_str(),
                crate::system_scope::SYSTEM_SCOPE_FORBIDDEN,
                "`{tool}` must NOT pretend the container does not exist"
            );
        }

        // …while the scope's OWN capability answers normally: `list_commands` on the singleton is
        // a legitimate read (it is the container of the user's cross-project services).
        let listed = dispatch_extension_tool(
            &db,
            &runner,
            &host,
            &activity,
            "list_commands",
            &json!({ "project_id": scope.project.id }),
            None,
        )
        .expect("list_commands IS an MCP tool")
        .expect("and it is a capability of the Système scope");
        assert!(listed.result.get("commands").is_some());

        // …and a USER project passes every one of the refused tools' boundary (the refusal is
        // about the container's NATURE, never about the tool).
        let (user, _root) = db
            .with_conn(|c| crate::db::create_project(c, "app", "/tmp/sys-boundary", None))
            .unwrap();
        let ok = dispatch_extension_tool(
            &db,
            &runner,
            &host,
            &activity,
            "list_workspaces",
            &json!({ "project_id": user.id }),
            None,
        )
        .expect("list_workspaces IS an MCP tool")
        .expect("a user project is listable");
        assert!(ok.result.get("workspaces").is_some());
    }

    /// A [`crate::system_scope::SystemTerminalShell`] that opens nothing — this module's tests
    /// have no PTY, and the DB half is the whole subject here.
    struct NoShell;
    impl crate::system_scope::SystemTerminalShell for NoShell {
        fn open_at_home(&self, _terminal_id: &str, _home: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// DONE-CRITERION (« Terminal Système atomique ») — the generic MCP terminal tools neither
    /// LIST, READ, WRITE nor CLOSE a Système terminal.
    ///
    /// This is the half [`crate::system_scope::guard_payload`] structurally cannot cover: these
    /// five tools carry a `terminal_id`, and the payload guard only knows the singleton's project
    /// and workspace ids. So they read [`crate::system_scope::is_system_terminal`] instead
    /// (`refuse_system_terminal`), and this walks all five through the REAL dispatcher.
    ///
    /// Both directions, because only the pair means anything: the Système terminal must be
    /// refused, and an ORDINARY terminal must still work through the very same five tools.
    ///
    /// The refusal is `system_scope_forbidden` and NAMES the terminal — never `invalid_id`, which
    /// would claim the record does not exist. Omitting it from `list_terminals` is ergonomics (do
    /// not offer an id that cannot be used); the four verbs refuse the id regardless, which is why
    /// they are asserted with the id in hand.
    #[test]
    fn the_generic_terminal_tools_neither_list_read_write_nor_close_a_system_terminal() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        db.with_conn(|c| {
            crate::system_scope::ensure_system_scope(
                c,
                &crate::system_scope::FixedHomeDirProvider::at(if cfg!(windows) {
                    "C:\\Users\\kris"
                } else {
                    "/home/kris"
                }),
            )
        })
        .expect("bootstrap the Système scope");

        let sys = crate::system_scope::create_system_terminal(&db, Some("services".into()), &NoShell)
            .expect("the human's Système terminal");
        let sys_id = sys.terminal.id.clone();
        // An ORDINARY terminal beside it, through the agent's own tool.
        let user_id = create_terminal(&db, &host, &json!({ "label": "agent" }))
            .unwrap()
            .result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();

        let call = |name: &str, args: Value| {
            dispatch_extension_tool(&db, &runner, &host, &activity, name, &args, None)
                .unwrap_or_else(|| panic!("`{name}` is an MCP tool"))
        };

        // 1. LIST — the Système terminal is not offered, the ordinary one is. `include_closed`
        //    changes nothing: the filter is on MEMBERSHIP, not on status.
        for args in [json!({}), json!({ "include_closed": true })] {
            let listed = call(crate::mcp::LIST_TERMINALS_TOOL, args).unwrap();
            let ids: Vec<&str> = listed.result["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["terminal_id"].as_str().unwrap())
                .collect();
            assert!(
                !ids.contains(&sys_id.as_str()),
                "an agent must not be offered a Système terminal: {ids:?}"
            );
            assert!(ids.contains(&user_id.as_str()), "…while ordinary ones stay listed");
        }

        // 2. The four VERBS — refused BY NAME, with the stable code and the entity.
        for (tool, args) in [
            (crate::mcp::READ_TERMINAL_TOOL, json!({ "terminal_id": sys_id })),
            (
                crate::mcp::SEND_TO_TERMINAL_TOOL,
                json!({ "terminal_id": sys_id, "command": "rm -rf /" }),
            ),
            (
                crate::mcp::SEND_KEYS_TOOL,
                json!({ "terminal_id": sys_id, "keys": ["ctrl+c"] }),
            ),
            (crate::mcp::CLOSE_TERMINAL_TOOL, json!({ "terminal_id": sys_id })),
        ] {
            let err = call(tool, args).expect_err("a Système terminal is not the agent's");
            assert_eq!(
                err.code,
                RpcCode::SystemScopeForbidden,
                "`{tool}` must refuse with the dedicated code, got {err:?}"
            );
            assert_eq!(
                err.code.as_str(),
                crate::system_scope::SYSTEM_SCOPE_FORBIDDEN,
                "`{tool}` must NOT pretend the terminal does not exist"
            );
            let data = err.data.expect("the refusal is machine-readable");
            assert_eq!(data["entity"], "terminal");
            assert_eq!(data["id"], json!(sys_id));
            assert_eq!(data["capability"], json!(tool));
        }

        // Not one of those refusals touched the record: still alive, still a member.
        let row = db
            .with_conn(|c| crate::db::get_terminal(c, &sys_id))
            .unwrap()
            .expect("the record stands");
        assert_eq!(row.status, crate::db::STATUS_ALIVE);
        assert_eq!(row.workspace_binding_mode, crate::db::BINDING_MANUAL);
        assert!(db
            .with_conn(|c| crate::system_scope::is_system_terminal(c, &sys_id))
            .unwrap());

        // 3. NON-VACUOUS — the same four tools work on the ORDINARY terminal. The refusal is about
        //    the terminal's MEMBERSHIP, never about the tool.
        call(
            crate::mcp::READ_TERMINAL_TOOL,
            json!({ "terminal_id": user_id }),
        )
        .expect("an ordinary terminal is readable");
        // `send_to_terminal` / `send_keys` need a LIVE shell, which `NoTerminalHost` never gives —
        // so they answer `invalid_state` here. That is the point: they got PAST the boundary and
        // failed on liveness, which a `system_scope_forbidden` would have pre-empted.
        for (tool, args) in [
            (
                crate::mcp::SEND_TO_TERMINAL_TOOL,
                json!({ "terminal_id": user_id, "command": "ls" }),
            ),
            (
                crate::mcp::SEND_KEYS_TOOL,
                json!({ "terminal_id": user_id, "keys": ["enter"] }),
            ),
        ] {
            let err = call(tool, args).expect_err("no live shell behind a NoTerminalHost");
            assert_eq!(err.code, RpcCode::InvalidState, "`{tool}`: {err:?}");
        }
        call(
            crate::mcp::CLOSE_TERMINAL_TOOL,
            json!({ "terminal_id": user_id }),
        )
        .expect("an ordinary terminal is closable");
    }

    /// DONE-CRITERIA (« MCP : découverte dédiée et nature des commandes Système ») — the whole of
    /// what an agent may learn about the scope, and the whole of what it may do with it.
    ///
    ///  1. `list_system_commands` answers the SCOPE (project, workspace, home), the COMMANDS with
    ///     their `command_id` / launchable `instance_id` / `group_id` / `nature: "system"`, and the
    ///     GROUPS.
    ///  2. `list_commands` carries `nature` on BOTH its forms, for user commands as well — one
    ///     reader tells a project command from a Système service.
    ///  3. `list_projects` / `list_workspaces` stay USER-ONLY: the singleton is not in the first,
    ///     and the second refuses it outright.
    ///  4. An agent can go from the discovery response STRAIGHT to reading output and starting a
    ///     stopped command — and still cannot see, let alone drive, a Système TERMINAL.
    #[test]
    fn list_system_commands_discovers_the_scope_and_opens_nothing_else() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let scope = db
            .with_conn(|c| {
                crate::system_scope::ensure_system_scope(
                    c,
                    &crate::system_scope::FixedHomeDirProvider::at(if cfg!(windows) {
                        "C:\\Users\\kris"
                    } else {
                        "/home/kris"
                    }),
                )
            })
            .expect("bootstrap the Système scope");

        // Two Système services, grouped — plus a Système TERMINAL, which must stay invisible.
        let (grouped, group_id, user_project) = db.with_conn(|c| {
            let ids: Vec<String> = ["services", "docker"]
                .iter()
                .map(|name| {
                    crate::db::create_template(
                        c,
                        &scope.project.id,
                        name,
                        "echo hi",
                        None,
                        crate::db::CommandSource::default(),
                    )
                    .expect("Système command")
                    .id
                })
                .collect();
            let group = crate::db::create_command_group(c, &scope.project.id, &ids).expect("group");
            let (user, _root) =
                crate::db::create_project(c, "app", "/tmp/sys-mcp-user", None).expect("project");
            crate::db::create_template(
                c,
                &user.id,
                "dev",
                "npm run dev",
                None,
                crate::db::CommandSource::default(),
            )
            .expect("user command");
            (ids, group.id, user.id)
        });
        let sys_terminal =
            crate::system_scope::create_system_terminal(&db, Some("shell".into()), &NoShell)
                .expect("a Système terminal")
                .terminal
                .id;

        let call = |name: &str, args: Value| {
            dispatch_extension_tool(&db, &runner, &host, &activity, name, &args, None)
                .unwrap_or_else(|| panic!("`{name}` is an MCP tool"))
        };

        // 1. THE DISCOVERY.
        let out = call(crate::mcp::LIST_SYSTEM_COMMANDS_TOOL, json!({}))
            .expect("list_system_commands is a capability of the Système scope")
            .result;
        assert_eq!(out["scope"]["project_id"], json!(scope.project.id));
        assert_eq!(out["scope"]["workspace_id"], json!(scope.workspace.id));
        assert_eq!(
            out["scope"]["home"],
            json!(scope.home()),
            "the home an agent is told about IS the one the runner will spawn in"
        );
        let commands = out["commands"].as_array().expect("commands");
        assert_eq!(commands.len(), 2, "the singleton's ONE workspace ⇒ one instance each");
        for entry in commands {
            assert_eq!(entry["nature"], json!("system"));
            assert_eq!(entry["workspace_id"], json!(scope.workspace.id));
            assert_eq!(entry["group_id"], json!(group_id), "the grouping is readable here");
            assert!(entry["instance_id"].as_str().is_some(), "the LAUNCHABLE id");
            assert!(grouped.contains(&entry["command_id"].as_str().unwrap().to_string()));
            assert_eq!(entry["cwd"], json!(scope.home()));
        }
        let groups = out["groups"].as_array().expect("groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["id"], json!(group_id));
        assert_eq!(groups[0]["command_ids"], json!(grouped));

        // 2. `list_commands` CARRIES THE NATURE, on both forms and for both natures.
        for (args, expected) in [
            (json!({ "workspace_id": scope.workspace.id }), "system"),
            (json!({ "project_id": scope.project.id }), "system"),
            (json!({ "project_id": user_project }), "user"),
        ] {
            let listed = call("list_commands", args.clone()).unwrap().result;
            let rows = listed["commands"].as_array().unwrap();
            assert!(!rows.is_empty(), "non-vacuous for {args}");
            for row in rows {
                assert_eq!(row["nature"], json!(expected), "{args} → {row}");
            }
        }

        // 3. THE USER INVENTORIES stay user-only.
        let projects = call("list_projects", json!({})).unwrap().result;
        assert!(
            !projects["projects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"] == json!(scope.project.id)),
            "list_projects never shows the technical container: {projects}"
        );
        let refused = call("list_workspaces", json!({ "project_id": scope.project.id }))
            .expect_err("the singleton is not a USER container to enumerate");
        assert_eq!(refused.code, RpcCode::SystemScopeForbidden);

        // 4. FROM THE RESPONSE STRAIGHT TO ACTING — reading output and starting a stopped command,
        //    with nothing else discovered.
        //
        //    (Those two are served by `mcp_runtime`'s dispatcher, not by this module's — they are
        //    the ORDINARY runtime tools, which is the point: discovery hands back an id the
        //    existing surface already knows what to do with. They are on the Système allow-list,
        //    so the boundary lets them through with the singleton's ids in the payload.)
        let instance = commands[0]["instance_id"].as_str().unwrap().to_string();
        let runtime_call = |name: &str, args: Value| {
            crate::mcp_runtime::dispatch_command_tool(&db, &runner, name, &args, None)
                .unwrap_or_else(|| panic!("`{name}` is a runtime MCP tool"))
        };
        runtime_call("get_command_output", json!({ "instance_id": instance }))
            .expect("reading a Système command's output is free");
        runtime_call("start_command", json!({ "instance_id": instance }))
            .expect("starting a stopped Système command is free");
        let _ = runner.stop(&instance);

        // …and the Système TERMINAL was never part of the deal.
        let terminals = call(crate::mcp::LIST_TERMINALS_TOOL, json!({}))
            .unwrap()
            .result;
        assert!(
            terminals["terminals"].as_array().unwrap().is_empty(),
            "the discovery of the COMMANDS opens no terminal: {terminals}"
        );
        let err = call(
            crate::mcp::READ_TERMINAL_TOOL,
            json!({ "terminal_id": sys_terminal }),
        )
        .expect_err("even holding its id, an agent may not read a Système terminal");
        assert_eq!(err.code, RpcCode::SystemScopeForbidden);
    }

    #[test]
    fn create_then_list_then_close_terminal_roundtrips() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        // Create a loose terminal.
        let created = create_terminal(&db, &host, &json!({ "label": "t1" })).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        assert_eq!(created.effects, vec![ChangedTopic::Terminals]);
        // It lists as alive.
        let listed = list_terminals(&db, &host, &activity, &json!({})).unwrap();
        let arr = listed["terminals"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["terminal_id"], json!(tid));
        assert_eq!(arr[0]["status"], json!("alive"));
        // Close it → record flips, terminals changed.
        let closed = close_terminal(&db, &host, &json!({ "terminal_id": tid })).unwrap();
        assert_eq!(closed.result["closed"], json!(true));
        assert_eq!(closed.effects, vec![ChangedTopic::Terminals]);
        // Default list (alive only) is now empty.
        let after = list_terminals(&db, &host, &activity, &json!({})).unwrap();
        assert_eq!(after["terminals"].as_array().unwrap().len(), 0);
    }

    /// H1 — `create_terminal` OPENS the shell itself, for the record it just wrote and at the cwd
    /// it stored. Leaving that to the front's mount made the tool depend on what the human had on
    /// screen: the front mounts only the displayed project's panes, so a terminal opened anywhere
    /// else never got a shell at all.
    ///
    /// The ORDER is the other half of the contract: the opening command is parked BEFORE the
    /// shell is asked for, because that shell is what drains the park — park after and the
    /// command misses its own terminal.
    #[test]
    fn create_terminal_opens_the_shell_for_the_record_it_wrote() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = RecordingHost::default();
        let created = create_terminal(
            &db,
            &host,
            &json!({ "cwd": "/tmp/h1-open", "command": "echo hi" }),
        )
        .unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        assert_eq!(
            *host.opened.lock().unwrap(),
            // The cwd the tool STORED is the canonical one, so that is the cwd the host is
            // handed — the expectation goes through the same normalizer rather than assuming
            // the argument's spelling survives (on Windows it becomes `\tmp\h1-open`).
            vec![(tid.clone(), crate::pathnorm::normalize("/tmp/h1-open"))],
            "the tool must ask for the record's shell, at the cwd it stored"
        );
        assert_eq!(
            *host.parked.lock().unwrap(),
            vec![tid],
            "the opening command must be parked before the shell that drains it is asked for"
        );

        // With no cwd argument the record stores the `.` default, and that is what the host is
        // told — it is the host that spells "the shell's own working directory".
        let bare = create_terminal(&db, &host, &json!({})).unwrap();
        let bare_id = bare.result["terminal_id"].as_str().unwrap().to_string();
        assert_eq!(
            host.opened.lock().unwrap().last(),
            Some(&(bare_id, ".".to_string()))
        );
    }

    #[test]
    fn send_to_terminal_without_live_pty_is_invalid_state_not_unknown() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        // The record exists (alive) but no live PTY → invalid_state, NOT method_not_found.
        let err = send_to_terminal(
            &db,
            &host,
            &AgentActivityStore::new(),
            &json!({ "terminal_id": tid, "command": "ls" }),
        )
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
            &AgentActivityStore::new(),
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
    fn resolve_keys_mixed_array_segments_and_adds_no_forced_newline() {
        // ["my message", "enter"] → TWO chunks: the text, then a `\r` ALONE (the explicit
        // submit) — and NOTHING else: no second/forced \r is appended by the resolver.
        let chunks = resolve_keys(&["my message".to_string(), "enter".to_string()]);
        assert_eq!(chunks, vec![b"my message".to_vec(), b"\r".to_vec()]);

        // A bare keystroke array carries only that key's bytes (no newline), in one chunk.
        assert_eq!(resolve_keys(&["up".to_string()]), vec![b"\x1b[A".to_vec()]);
        assert_eq!(resolve_keys(&["ctrl+c".to_string()]), vec![b"\x03".to_vec()]);

        // Typing WITHOUT submitting: literal text alone never gains a trailing newline.
        assert_eq!(resolve_keys(&["draft".to_string()]), vec![b"draft".to_vec()]);

        // A richer mix resolves left-to-right, ONE chunk per element and in order.
        let chunks = resolve_keys(&[
            "ab".to_string(),
            "left".to_string(),
            "X".to_string(),
            "enter".to_string(),
        ]);
        assert_eq!(
            chunks,
            vec![
                b"ab".to_vec(),
                b"\x1b[D".to_vec(),
                b"X".to_vec(),
                b"\r".to_vec(),
            ]
        );
        // The BYTES are unchanged from the flat era — only the granularity moved.
        assert_eq!(chunks.concat(), b"ab\x1b[DX\r");

        // An EMPTY element writes nothing at all rather than costing a zero-length write.
        assert_eq!(
            resolve_keys(&["".to_string(), "enter".to_string()]),
            vec![b"\r".to_vec()]
        );
    }

    /// A [`TerminalHost`] that records the exact CHUNKS `send_keys` handed it — the shape of the
    /// PTY traffic, which is the whole subject of the submit fix — AND, separately, the command
    /// LINES that took the single-write [`TerminalHost::send_to_terminal`] path, so a test can tell
    /// the two write shapes apart rather than only counting bytes.
    #[derive(Default)]
    struct KeyRecordingHost {
        writes: std::sync::Mutex<Vec<Vec<u8>>>,
        lines: std::sync::Mutex<Vec<String>>,
    }
    impl TerminalHost for KeyRecordingHost {
        fn park_opening_command(&self, _: &str, _: &str) {}
        fn open_terminal_shell(&self, _: &str, _: &str) {}
        fn send_to_terminal(&self, _: &str, command: &str) -> Result<bool, String> {
            self.lines.lock().unwrap().push(command.to_string());
            Ok(true)
        }
        fn send_keys(&self, _: &str, chunks: &[Vec<u8>]) -> Result<bool, String> {
            self.writes.lock().unwrap().extend(chunks.iter().cloned());
            Ok(true)
        }
        fn close_terminal_pty(&self, _: &str) {}
        fn terminal_liveness(&self, _: &str) -> (bool, Option<bool>) {
            (true, None)
        }
        fn scrollback_is_behind(&self, _: &str) -> bool {
            false
        }
    }

    /// THE SUBMIT BUG (2026-09-01 pilot run) — `send_keys(["texte","enter"])` in ONE call must
    /// produce SEVERAL PTY writes, and the `\r` must leave in a write that contains NOTHING else.
    ///
    /// The flat era wrote `"texte\r"` in a single `write_all`, and Ink classified that whole chunk
    /// as a paste: the prompt was typed and the CR became a literal newline inside the input box,
    /// while the tool answered `sent:true`. A lone `\r` (a one-byte chunk) is recognised and
    /// submits — which is why this asserts on the SHAPE of the traffic, not on its bytes.
    #[test]
    fn send_keys_writes_the_text_and_the_enter_separately() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = KeyRecordingHost::default();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        let out = send_keys(
            &db,
            &host,
            &AgentActivityStore::new(),
            &json!({ "terminal_id": tid, "keys": ["texte", "enter"] }),
        )
        .expect("a live host accepts the keys");

        let writes = host.writes.lock().unwrap().clone();
        assert!(
            writes.len() > 1,
            "one call must produce SEVERAL PTY writes, got {writes:?}"
        );
        assert_eq!(writes, vec![b"texte".to_vec(), b"\r".to_vec()]);
        assert_eq!(
            writes.last().unwrap(),
            b"\r",
            "the submit leaves ALONE — and as \\r (0x0D), never \\n"
        );
        // The answer says the granularity out loud, so a caller can see the fix landed.
        assert_eq!(out.result["writes"], json!(2));
        assert_eq!(out.result["bytes"], json!(6));
        assert_eq!(out.result["sent"], json!(true));
    }

    /// NON-REGRESSION on the keystroke half: `ctrl+c` and the navigation keys still go out — each
    /// as its own write, so a TUI sees them as the discrete keypresses they are rather than as one
    /// pasted blob.
    #[test]
    fn send_keys_keeps_ctrl_and_navigation_keys_isolated() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = KeyRecordingHost::default();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        let activity = AgentActivityStore::new();
        send_keys(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "keys": ["ctrl+c"] }),
        )
        .unwrap();
        send_keys(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "keys": ["down", "down", "enter"] }),
        )
        .unwrap();

        assert_eq!(
            *host.writes.lock().unwrap(),
            vec![
                b"\x03".to_vec(),
                b"\x1b[B".to_vec(),
                b"\x1b[B".to_vec(),
                b"\r".to_vec(),
            ],
            "ctrl+c and each arrow keep their own write, and enter is still a lone \\r"
        );
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

    /// N31 — the persisted env is SETTABLE from the agent surface: `add_command` stores it,
    /// `update_command` replaces it, and an explicit `{}` (or `null`) CLEARS it. The read-back
    /// reports the KEYS only — an env value is a secret and is never echoed.
    #[test]
    fn add_and_update_command_set_then_clear_the_persisted_env() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/cmdenv", None))
            .unwrap();
        let runner = noop_runner();

        // add_command stores the map…
        let added = add_command(
            &db,
            &json!({
                "project_id": project.id,
                "name": "tests",
                "command": "cargo test",
                "env": { "DATABASE_URL": "postgres://secret", "RUST_LOG": "debug" },
            }),
        )
        .unwrap();
        let cmd_id = added.result["command"]["command_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            added.result["command"]["env_keys"],
            json!(["DATABASE_URL", "RUST_LOG"]),
            "the surface advertises the KEYS"
        );
        let stored = |db: &Db, id: &str| {
            db.with_conn(|c| db::get_template(c, id))
                .unwrap()
                .unwrap()
                .env
        };
        assert!(
            !added.result["command"].to_string().contains("secret"),
            "an env VALUE is never echoed back"
        );
        assert_eq!(
            crate::command::decode_command_env(stored(&db, &cmd_id).as_deref()).len(),
            2,
            "both entries are persisted and decodable"
        );

        // …update_command REPLACES it whole (the dropped key is gone, not merged)…
        let updated = update_command(
            &db,
            &runner,
            &json!({ "command_id": cmd_id, "env": { "RUST_LOG": "info" } }),
        )
        .unwrap();
        assert_eq!(updated.result["command"]["env_keys"], json!(["RUST_LOG"]));

        // …an omitted env leaves it untouched…
        let untouched = update_command(
            &db,
            &runner,
            &json!({ "command_id": cmd_id, "name": "tests-renamed" }),
        )
        .unwrap();
        assert_eq!(untouched.result["command"]["env_keys"], json!(["RUST_LOG"]));

        // …and an explicit {} clears the column outright.
        let cleared =
            update_command(&db, &runner, &json!({ "command_id": cmd_id, "env": {} })).unwrap();
        assert_eq!(cleared.result["command"]["env_keys"], json!([]));
        assert_eq!(stored(&db, &cmd_id), None, "the column is NULL, not '{{}}'");

        // `null` clears it too (the same "no environment" answer, spelled the other way).
        update_command(
            &db,
            &runner,
            &json!({ "command_id": cmd_id, "env": { "A": "1" } }),
        )
        .unwrap();
        update_command(&db, &runner, &json!({ "command_id": cmd_id, "env": null })).unwrap();
        assert_eq!(stored(&db, &cmd_id), None);
    }

    /// N31 — the WRITE boundary is where a caller mistake is still named: a non-string env value is
    /// refused as `invalid_argument` naming ONLY the key, never the value (which may be a secret).
    #[test]
    fn add_and_update_command_refuse_a_non_string_env_value_naming_only_the_key() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/cmdenvbad", None))
            .unwrap();
        let runner = noop_runner();

        let err = add_command(
            &db,
            &json!({
                "project_id": project.id,
                "name": "tests",
                "command": "cargo test",
                "env": { "PORT": 5433 },
            }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(err.message.contains("env.PORT"), "names the key: {err:?}");
        assert!(!err.message.contains("5433"), "never echoes the value");
        // The refusal happens BEFORE the write: no half-created template.
        assert!(db
            .with_conn(|c| db::list_templates(c, &project.id))
            .unwrap()
            .is_empty());

        // Same rule on update, and a non-object env is refused too.
        let added = add_command(
            &db,
            &json!({ "project_id": project.id, "name": "tests", "command": "cargo test" }),
        )
        .unwrap();
        let cmd_id = added.result["command"]["command_id"].as_str().unwrap();
        let err = update_command(
            &db,
            &runner,
            &json!({ "command_id": cmd_id, "env": { "PORT": 5433 } }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(err.message.contains("env.PORT"));
        let err = update_command(
            &db,
            &runner,
            &json!({ "command_id": cmd_id, "env": "DATABASE_URL=x" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
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

    /// The worktrees-root project setting: `worktrees_root` is OPTIONAL and EMPTY by default, set and cleared
    /// through the SAME `set_project_settings` `resume_agent_sessions` goes through, PERSISTED, and
    /// RENDERED by `list_projects` (which is where a flow — or a human — reads it back). Clearing it
    /// is an EXPLICIT empty string / null, never an omission: omitting the field leaves it alone.
    #[test]
    fn set_project_settings_carries_the_worktrees_root_and_list_projects_renders_it() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/wtr", None))
            .unwrap();
        assert_eq!(
            project.worktrees_root, None,
            "a fresh project configures no worktrees root"
        );

        let out = set_project_settings(
            &db,
            &json!({ "project_id": project.id, "worktrees_root": "D:/work/trees" }),
        )
        .unwrap();
        assert_eq!(
            out.result["project"]["worktrees_root"],
            json!("D:/work/trees")
        );
        assert_eq!(
            db.with_conn(|c| db::get_project(c, &project.id))
                .unwrap()
                .unwrap()
                .worktrees_root
                .as_deref(),
            Some("D:/work/trees"),
            "the setting is persisted"
        );
        // list_projects RENDERS it — the read surface a flow (and the UI) sees.
        let listed = list_projects(&db).unwrap();
        assert_eq!(
            listed["projects"][0]["worktrees_root"],
            json!("D:/work/trees")
        );

        // An OMITTED field is untouched…
        set_project_settings(&db, &json!({ "project_id": project.id, "name": "P2" })).unwrap();
        assert_eq!(
            db.with_conn(|c| db::get_project(c, &project.id))
                .unwrap()
                .unwrap()
                .worktrees_root
                .as_deref(),
            Some("D:/work/trees"),
            "omitting worktrees_root leaves it alone"
        );
        // …and an EXPLICIT empty string CLEARS it (back to "ask the human where").
        set_project_settings(
            &db,
            &json!({ "project_id": project.id, "worktrees_root": "" }),
        )
        .unwrap();
        assert_eq!(
            db.with_conn(|c| db::get_project(c, &project.id))
                .unwrap()
                .unwrap()
                .worktrees_root,
            None,
            "an empty string clears the setting"
        );
    }

    /// The project's VISUAL IDENTITY through the same tool: `accent` and the mark are independent,
    /// the mark is ONE slot in two spellings (`icon` / `image`), and `list_projects` renders all
    /// three — which is where the home reads them back. A project that sets nothing keeps the three
    /// columns null, which IS the repli.
    #[test]
    fn set_project_settings_carries_the_identity_and_the_mark_is_exclusive() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/identity-mcp", None))
            .unwrap();
        assert_eq!(project.accent, None, "a fresh project has no identity");
        assert_eq!(project.icon, None);
        assert_eq!(project.image_path, None);

        // A colour AND a glyph.
        let out = set_project_settings(
            &db,
            &json!({ "project_id": project.id, "accent": "#4f46e5", "icon": "rocket" }),
        )
        .unwrap();
        assert_eq!(out.result["project"]["accent"], json!("#4f46e5"));
        assert_eq!(out.result["project"]["icon"], json!("rocket"));
        assert_eq!(out.result["project"]["image_path"], json!(null));

        // Switching to an IMAGE clears the glyph; the accent is untouched (independent halves).
        set_project_settings(
            &db,
            &json!({ "project_id": project.id, "image": "D:/logo.png" }),
        )
        .unwrap();
        let listed = list_projects(&db).unwrap();
        assert_eq!(listed["projects"][0]["image_path"], json!("D:/logo.png"));
        assert_eq!(listed["projects"][0]["icon"], json!(null));
        assert_eq!(listed["projects"][0]["accent"], json!("#4f46e5"));

        // An explicit null CLEARS — back to the repli.
        set_project_settings(
            &db,
            &json!({ "project_id": project.id, "accent": null, "image": null }),
        )
        .unwrap();
        let bare = db
            .with_conn(|c| db::get_project(c, &project.id))
            .unwrap()
            .unwrap();
        assert_eq!(bare.accent, None);
        assert_eq!(bare.icon, None);
        assert_eq!(bare.image_path, None);

        // Asking for BOTH spellings at once is refused, not arbitrated.
        let err = set_project_settings(
            &db,
            &json!({ "project_id": project.id, "icon": "rocket", "image": "D:/logo.png" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
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
        let err = read_terminal(
            &db,
            &NoTerminalHost,
            &AgentActivityStore::new(),
            &json!({ "terminal_id": "nope" }),
        )
        .unwrap_err();
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

        let out = session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        assert_eq!(out.effects, vec![ChangedTopic::AgentSessions]);
        assert_eq!(out.result["activity"], json!(true));
        assert!(activity.snapshot(&tid).unwrap().activity == Activity::Working);
    }

    /// H6 — the same hook ALSO records that a HUMAN took a turn on this terminal: the raw text they
    /// typed and the turn's IDENTITY (Claude Code's `prompt_id`). This is the signal the
    /// orchestration engine checks before it lets an answer commit a Spec, and it is the only one on
    /// the whole dispatch path an agent cannot produce: no tool call writes this row, only a person
    /// pressing enter. A hook that is not a prompt must not write it either — otherwise the agent's
    /// own tool traffic would forge the very proof.
    ///
    /// And the row OUTLIVES THE SESSION, which is the fix: `SessionEnd` drops the live dot (no turn
    /// can still be running once the session is gone) and leaves the ledger standing. A `claude -p`
    /// invocation IS a whole session — start, one prompt, end — so a ledger that died with it was
    /// gone before the answer it authorises ever came back, and the consent check refused authentic
    /// answers for exactly that reason.
    #[test]
    fn a_user_prompt_hook_records_the_human_turn_and_it_outlives_the_session() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        let ledger = || db.with_conn(|c| db::last_human_turn(c, &tid)).unwrap();
        assert_eq!(ledger(), None, "no prompt, no turn");

        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "UserPromptSubmit",
                "NYX_TERMINAL_ID": tid,
                "prompt": "oui, passe-la en ready",
                "prompt_id": "prompt-1",
            }),
        )
        .unwrap();
        let turn = ledger().expect("the human's turn is recorded");
        assert_eq!(turn.turn_id, "prompt-1", "the turn is NAMED by the prompt id");
        assert_eq!(turn.prompt, "oui, passe-la en ready");

        // A tool the AGENT runs is a beat of its turn, not a turn of the human's.
        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "PreToolUse", "NYX_TERMINAL_ID": tid, "tool_name": "Bash" }),
        )
        .unwrap();
        assert_eq!(
            ledger().unwrap().turn_id,
            "prompt-1",
            "an agent's own tool traffic must never count as the human speaking"
        );

        // THE SESSION ENDS — the whole of a `claude -p` invocation. The dot goes (nothing is
        // running), the human's turn STAYS: they typed it, and it is still the last thing they said.
        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionEnd",
                "NYX_TERMINAL_ID": tid,
                "session_id": "sess-1",
                "reason": "prompt_input_exit",
            }),
        )
        .unwrap();
        assert!(
            activity.snapshot(&tid).is_none(),
            "the session ending clears the live dot"
        );
        assert_eq!(
            ledger().expect("the ledger survives the session").turn_id,
            "prompt-1",
            "the human turn outlives the session it was typed in"
        );

        // A provider that sends NO prompt id still produces a DISTINCT turn — nyx mints the identity
        // itself, because all the consent check asks is "is this another turn than the one I saw?".
        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "UserPromptSubmit",
                "NYX_TERMINAL_ID": tid,
                "prompt": "yes",
            }),
        )
        .unwrap();
        let next = ledger().expect("the next turn is recorded");
        assert_ne!(next.turn_id, "prompt-1", "a new prompt is a new turn");
        assert_eq!(next.prompt, "yes");
    }

    /// R1 — while the host is SHUTTING DOWN, a real `SessionEnd` (e.g. `prompt_input_exit` from a
    /// dying PTY) must NOT end the session: it stays `active` so lazy-resume reclaims it next boot.
    /// Without the suppression, that late SessionEnd (which outlives `mcp.stop()`) ended the session
    /// and killed the resume — the whole of R1. The mirror (no suppression → still ends) proves the
    /// guard is scoped to the shutdown window, not a blanket change.
    #[test]
    fn a_session_end_during_host_shutdown_keeps_the_session_active() {
        let _g = db_guard();
        reset_session_end_suppression(); // a prior test must not leak the flag in
        let db = Db::in_memory();
        let activity = AgentActivityStore::new();
        let (session_id, terminal_id) = db.with_conn(|c| {
            let terminal = crate::db::create_terminal(c, "/work", None).unwrap();
            let session = crate::db::record_session_start(
                c,
                &terminal.id,
                crate::db::AGENT_KIND_CLAUDE_CODE,
                crate::db::SessionCapture {
                    external_session_id: "ext-r1".into(),
                    cwd: "/work".into(),
                    ..Default::default()
                },
            )
            .unwrap();
            (session.id, terminal.id)
        });
        let end_event = json!({
            "hook_event_name": "SessionEnd",
            "session_id": "ext-r1",
            "reason": "prompt_input_exit",
            "NYX_TERMINAL_ID": terminal_id,
        });

        // The host begins its ordered shutdown → suppress. The dying PTY's SessionEnd is IGNORED.
        suppress_session_end_marking();
        let out = session_event(&db, &activity, &end_event).expect("handled");
        assert_eq!(out.result["ended"], json!(false));
        let survived = db
            .with_conn(|c| crate::db::get_session(c, &session_id))
            .unwrap()
            .unwrap();
        assert_eq!(
            survived.state,
            crate::db::SESSION_STATE_ACTIVE,
            "the session survives the shutdown SessionEnd → resumable next boot",
        );

        // Mirror: WITHOUT suppression (the app running normally) the SAME event DOES end it.
        reset_session_end_suppression();
        session_event(&db, &activity, &end_event).expect("handled");
        let ended = db
            .with_conn(|c| crate::db::get_session(c, &session_id))
            .unwrap()
            .unwrap();
        assert_eq!(
            ended.state,
            crate::db::SESSION_STATE_ENDED,
            "outside a shutdown, a real SessionEnd still ends the session (unchanged)",
        );
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

        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        session_event(
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

        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "SubagentStart", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        assert!(activity.snapshot(&tid).unwrap().activity == Activity::Working);
        let out = session_event(
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

        let send = |args: serde_json::Value| session_event(&db, &activity, &args).unwrap();

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

        let send = |args: serde_json::Value| session_event(&db, &activity, &args).unwrap();

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

        let send = |args: serde_json::Value| session_event(&db, &activity, &args).unwrap();

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

    /// THE SECONDARY INFORMATION, end to end over the real hook path: the data ALREADY arrives
    /// (the `.cjs` hook forwards Claude's whole stdin), so this proves the tool now RETAINS it —
    /// the `AskUserQuestion` `tool_input`, the permission `message`, the in-flight `tool_name` —
    /// and that a payload of an unexpected shape degrades to "no info" WITHOUT failing the hook.
    #[test]
    fn agent_session_event_captures_the_question_the_permission_and_the_tool() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        // Built ONCE and borrowed by every send: the hook path takes a runner since the managed
        // commands joined the agent context, and this test only ever needs it to exist.
        let runner = noop_runner();
        let send = |payload: Value| {
            agent_session_event(&db, &runner, &activity, &payload).expect("a hook is never failed")
        };

        send(json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }));

        // A TOOL in flight: its name is retained, not just used as a discriminant.
        send(json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": "cargo test" },
            "NYX_TERMINAL_ID": tid,
        }));
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(snap.activity, Activity::Working);
        assert_eq!(snap.tool.as_deref(), Some("Bash"));

        // A QUESTION: the tool_input is read and the row can say what is being asked.
        send(json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "AskUserQuestion",
            "tool_input": { "questions": [{ "question": "Ship it or hold?" }] },
            "NYX_TERMINAL_ID": tid,
        }));
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(snap.activity, Activity::Waiting);
        assert_eq!(snap.waiting_on.as_deref(), Some("Ship it or hold?"));

        // ANSWERED → back to Working, and the question is gone with the yellow.
        send(json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "AskUserQuestion",
            "NYX_TERMINAL_ID": tid,
        }));
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(snap.activity, Activity::Working);
        assert_eq!(snap.waiting_on, None);

        // A PERMISSION: Claude's own message is the text.
        send(json!({
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": "Claude needs your permission to use Bash",
            "NYX_TERMINAL_ID": tid,
        }));
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(snap.activity, Activity::Waiting);
        assert_eq!(
            snap.waiting_on.as_deref(),
            Some("Claude needs your permission to use Bash")
        );

        // AN UNEXPECTED PAYLOAD still blocks, with no info — and the hook does NOT fail (`send`
        // unwraps, so an Err here fails this test).
        send(json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }));
        let out = send(json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "AskUserQuestion",
            "tool_input": { "surprise": [1, 2, 3] },
            "NYX_TERMINAL_ID": tid,
        }));
        assert_eq!(out.result["activity"], json!(true));
        let snap = activity.snapshot(&tid).unwrap();
        assert_eq!(snap.activity, Activity::Waiting, "the block still happens");
        assert_eq!(snap.waiting_on, None, "…with no text rather than a guess");
        // A payload with NO `tool_input` at all is the same story.
        send(json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }));
        send(json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "AskUserQuestion",
            "NYX_TERMINAL_ID": tid,
        }));
        assert_eq!(activity.snapshot(&tid).unwrap().waiting_on, None);
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
        session_event(
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
        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        assert!(activity.snapshot(&tid).unwrap().activity == Activity::Working);
        // SessionEnd → clears the runtime activity (no phantom).
        session_event(
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

    /// THE BLIND-PILOT FIX (2026-09-01 pilot run, PRD annex §2 and §7) — `list_terminals` states
    /// the agent's LIVE activity and the session it belongs to, for a terminal that hosts one.
    ///
    /// The run measured that the two signals the row already carried are mute here: `busy` was
    /// `null` for the whole session and `exec_state` never left `idle` while `claude` was
    /// answering — because the agent IS the foreground process throughout. Meanwhile nyx held the
    /// right answer in [`AgentActivityStore`] (it drives the UI dot) and `tools/list` mentioned
    /// `agent_activity` exactly ZERO times. This walks the whole loop: a session starts, a turn
    /// runs, a turn ends, the session ends — and the row says so each time, without one row of
    /// anything being persisted.
    #[test]
    fn list_terminals_states_the_agent_activity_and_its_session_link() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();

        // Two terminals: one will host an agent session, the other never does.
        let agent_tid = create_terminal(&db, &host, &json!({ "label": "probe-claude" }))
            .unwrap()
            .result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        let plain_tid = create_terminal(&db, &host, &json!({ "label": "shell" }))
            .unwrap()
            .result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();

        let row_of = |listed: &Value, id: &str| -> Value {
            listed["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["terminal_id"] == json!(id))
                .expect("the terminal is listed")
                .clone()
        };

        // BEFORE any session: BOTH fields are explicitly null on both rows — "not an agent
        // terminal" is said, not left to be inferred from an absent key.
        let listed = list_terminals(&db, &host, &activity, &json!({})).unwrap();
        for id in [&agent_tid, &plain_tid] {
            let row = row_of(&listed, id);
            assert_eq!(row["agent_activity"], Value::Null);
            assert_eq!(row["agent_session"], Value::Null);
            // …and the pre-existing shape is untouched.
            assert_eq!(row["status"], json!("alive"));
            assert!(row.get("busy").is_some() && row.get("exec_state").is_some());
        }

        // A session starts. The row now carries the LINK — which agent, which of its sessions —
        // and an activity of `idle`: the store has no entry yet, and a session between turns is
        // idle, not "unknown".
        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "42d90a62-5f73-4a60-b55c-a48568b3e185",
                "cwd": "/work",
                "NYX_TERMINAL_ID": agent_tid,
            }),
        )
        .unwrap();
        let row = row_of(
            &list_terminals(&db, &host, &activity, &json!({})).unwrap(),
            &agent_tid,
        );
        assert_eq!(row["agent_activity"], json!("idle"));
        assert_eq!(row["agent_session"]["agent_kind"], json!("claude_code"));
        assert_eq!(
            row["agent_session"]["external_session_id"],
            json!("42d90a62-5f73-4a60-b55c-a48568b3e185")
        );
        assert_eq!(row["agent_session"]["state"], json!("active"));
        assert!(row["agent_session"]["session_id"].is_string());

        // A TURN RUNS — the exact window the pilot could not see. `working` here is the whole
        // point: nothing else on the row moves.
        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": agent_tid }),
        )
        .unwrap();
        let listed = list_terminals(&db, &host, &activity, &json!({})).unwrap();
        assert_eq!(row_of(&listed, &agent_tid)["agent_activity"], json!("working"));
        // The terminal with NO session is unaffected — its behaviour is exactly what it was.
        assert_eq!(row_of(&listed, &plain_tid)["agent_activity"], Value::Null);
        assert_eq!(row_of(&listed, &plain_tid)["agent_session"], Value::Null);

        // Blocked on the human (a permission prompt) → `waiting`, distinct from `working`.
        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "Notification",
                "notification_type": "permission_prompt",
                "NYX_TERMINAL_ID": agent_tid,
            }),
        )
        .unwrap();
        assert_eq!(
            row_of(
                &list_terminals(&db, &host, &activity, &json!({})).unwrap(),
                &agent_tid
            )["agent_activity"],
            json!("waiting")
        );

        // The turn ends → back to `idle`. This is what `wait_for_terminal` waits on.
        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "Stop", "NYX_TERMINAL_ID": agent_tid }),
        )
        .unwrap();
        assert_eq!(
            row_of(
                &list_terminals(&db, &host, &activity, &json!({})).unwrap(),
                &agent_tid
            )["agent_activity"],
            json!("idle")
        );

        // `read_terminal` says the same thing, so a caller that has just read the screen knows in
        // the SAME round trip whether the agent was still mid-turn while it read.
        let read = read_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": agent_tid }),
        )
        .unwrap();
        assert_eq!(read["agent_activity"], json!("idle"));
        let read_plain = read_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": plain_tid }),
        )
        .unwrap();
        assert_eq!(read_plain["agent_activity"], Value::Null);

        // NOTHING WAS PERSISTED — the anti-phantom contract holds. A brand-new store (what a
        // restart gives you) reads the SAME terminal as `idle`, never as a stuck `working`.
        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": agent_tid }),
        )
        .unwrap();
        let fresh = AgentActivityStore::new();
        assert_eq!(
            row_of(
                &list_terminals(&db, &host, &fresh, &json!({})).unwrap(),
                &agent_tid
            )["agent_activity"],
            json!("idle"),
            "a store rebuilt at boot reads idle — no dot survives a restart"
        );
    }

    /// THE ASYMMETRY CLOSED (PRD annex §2) — `wait_for_terminal` hands the caller back the moment
    /// the agent's turn ends, with NO polling on the caller's side.
    ///
    /// The wait is armed while the agent is `working`; a background thread ends the turn ~150 ms
    /// later; the call returns `resolved:true` having actually BLOCKED for that time. The timeout
    /// is a full 10 s, so a return at all is the proof: nothing here polls, and nothing here spent
    /// a round trip per attempt the way the 2026-09-01 run had to.
    #[test]
    fn wait_for_terminal_returns_when_the_agent_turn_ends() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = std::sync::Arc::new(AgentActivityStore::new());
        let tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-wait",
                "cwd": "/work",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        // A turn is running: the default `until` (idle+waiting) is NOT satisfied yet.
        activity.apply(&tid, crate::agent_activity::ActivityEvent::PromptSubmitted);

        // …and it ends while we are blocked, from another thread — the agent's `Stop` hook.
        let ender = {
            let activity = std::sync::Arc::clone(&activity);
            let tid = tid.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                activity.apply(&tid, crate::agent_activity::ActivityEvent::TurnFinished);
            })
        };

        let out = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "timeout_ms": 10_000 }),
        )
        .unwrap();
        ender.join().unwrap();

        assert_eq!(out["resolved"], json!(true), "the wait resolved: {out}");
        assert_eq!(out["agent_activity"], json!("idle"));
        assert!(
            out["waited_ms"].as_u64().unwrap() >= 100,
            "it really BLOCKED rather than returning on the first read: {out}"
        );
        // The link travels with the answer: one call says what finished, and whose turn it was.
        assert_eq!(out["agent_session"]["external_session_id"], json!("sid-wait"));
        assert_eq!(out["agent_session"]["state"], json!("active"));
    }

    /// THE RACE THE GATE FOUND (2026-09-02) — `send_keys([…,"enter"])` then `wait_for_terminal`,
    /// back to back, must NOT resolve on the state that predates the submission.
    ///
    /// On the real run the pair came back `resolved:true`, `agent_activity:"idle"`, `waited_ms:0`
    /// and an EMPTY `output_tail`: the write returns as soon as the bytes are in the PTY, while
    /// Claude's `UserPromptSubmit` hook is a `node` process that still has to start and POST. In
    /// that gap the store legitimately reads the PREVIOUS turn's `Idle`, the default `until`
    /// matches it, and the caller is handed back "the agent is done" about a turn that had not
    /// begun — then has to poll after all, which is the one thing this tool exists to remove.
    ///
    /// The submission arming window closes it: `send_keys` marks the terminal, and the wait reads
    /// `Working` until a hook lands. Here the hook NEVER lands within the short timeout, so the
    /// wait gives up honestly (`resolved:false`) instead of lying — and the SECOND call, after the
    /// hook, behaves exactly as before.
    #[test]
    fn wait_for_terminal_does_not_resolve_on_the_state_before_the_send_keys() {
        use crate::agent_activity::ActivityEvent;
        let _g = db_guard();
        let db = Db::in_memory();
        let host = KeyRecordingHost::default();
        let activity = std::sync::Arc::new(AgentActivityStore::new());
        let tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-race",
                "cwd": "/work",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        // The state the 2026-09-01 run measured for thirty seconds: idle, and nothing else to read.
        assert_eq!(
            list_terminals(&db, &host, &activity, &json!({})).unwrap()["terminals"][0]
                ["agent_activity"],
            json!("idle")
        );

        // ONE send_keys — the whole prompt and its `enter` — and then straight into the wait, with
        // NO hook in between. That is the real sequence, and the one that used to answer instantly.
        send_keys(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "keys": ["Resume ce projet", "enter"] }),
        )
        .unwrap();
        let armed = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "timeout_ms": 120 }),
        )
        .unwrap();
        assert_eq!(
            armed["resolved"],
            json!(false),
            "the turn we just submitted has not given the hand back: {armed}"
        );
        // WHAT THE ANSWER SAYS ABOUT IT (#K3). `agent_activity` is the SNAPSHOT — the same `idle`
        // `list_terminals` reports, because no hook has said anything yet — and the reason the wait
        // did not resolve on that `idle` is stated in its own field rather than smuggled in as a
        // `working` nobody asserted.
        assert_eq!(
            armed["agent_activity"],
            json!("idle"),
            "the field carries what the hooks said, not what the window guessed: {armed}"
        );
        assert_eq!(
            armed["agent_activity"],
            list_terminals(&db, &host, &activity, &json!({})).unwrap()["terminals"][0]
                ["agent_activity"],
            "and it agrees with list_terminals on the same terminal, at the same moment"
        );
        assert_eq!(
            armed["submission_pending"],
            json!(true),
            "the reason the wait held is named: nyx submitted, the agent has not spoken: {armed}"
        );

        // The hook lands (disarming the window), the turn runs, and the NEXT wait resolves on the
        // REAL end of turn — the ordinary behaviour, unchanged.
        activity.apply(&tid, ActivityEvent::PromptSubmitted);
        let ender = {
            let activity = std::sync::Arc::clone(&activity);
            let tid = tid.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(120));
                activity.apply(&tid, ActivityEvent::TurnFinished);
            })
        };
        let done = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "timeout_ms": 10_000 }),
        )
        .unwrap();
        ender.join().unwrap();
        assert_eq!(done["resolved"], json!(true));
        assert_eq!(done["agent_activity"], json!("idle"));
        assert!(
            done["waited_ms"].as_u64().unwrap() >= 100,
            "it blocked on the turn rather than on the window: {done}"
        );
    }

    /// THE OTHER HALF OF THE ARMING RULE — only a SUBMIT arms it, and only on a terminal that
    /// hosts an agent. A navigation burst (`["down"]`) types nothing to answer, and a plain shell
    /// has nobody to answer, so in both cases the very next wait must come straight back rather
    /// than sit through the window. Without this the fix would tax every non-prompt keystroke.
    #[test]
    fn send_keys_arms_the_wait_only_for_a_submit_on_an_agent_terminal() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = KeyRecordingHost::default();
        let activity = AgentActivityStore::new();
        let agent_tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        let plain_tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-arm",
                "cwd": "/work",
                "NYX_TERMINAL_ID": agent_tid,
            }),
        )
        .unwrap();

        // A navigation key on the AGENT terminal: no `\r`, nothing submitted, nothing armed.
        send_keys(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": agent_tid, "keys": ["down"] }),
        )
        .unwrap();
        let nav = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": agent_tid, "timeout_ms": 5_000 }),
        )
        .unwrap();
        assert_eq!(nav["resolved"], json!(true), "nothing was submitted: {nav}");
        assert!(
            nav["waited_ms"].as_u64().unwrap() < 1_000,
            "and the caller was not taxed the arming window: {nav}"
        );

        // A full submit on the PLAIN terminal: no agent session, so no turn to wait for.
        send_keys(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": plain_tid, "keys": ["ls", "enter"] }),
        )
        .unwrap();
        let plain = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": plain_tid, "timeout_ms": 5_000 }),
        )
        .unwrap();
        assert_eq!(plain["resolved"], json!(true));
        assert_eq!(plain["agent_session"], Value::Null);
        assert!(
            plain["waited_ms"].as_u64().unwrap() < 1_000,
            "a shell with no agent is never armed: {plain}"
        );
    }

    /// #K6 — ONE TERMINAL, ONE ANSWER TO "WHICH SESSION RUNS HERE".
    ///
    /// Three resolvers used to answer that question with three different tie-breaks:
    /// `list_terminals` collected `active_agent_sessions_full` (ordered by `agent_kind` asc) into a
    /// `HashMap`, where the LAST insert wins; `read_terminal` / `wait_for_terminal` /
    /// the submission arming used `active_agent_session_of_terminal`, where the FIRST by kind wins;
    /// and the auto-map used `active_session_for_terminal`, ordered by `started_at` desc. The
    /// partial-unique index only forbids two `active` rows per (terminal, AGENT KIND), and two
    /// tests consecrate two kinds coexisting on one terminal — so the disagreement is reachable,
    /// and the surfaces would name two different sessions for the same terminal in one breath.
    ///
    /// There is now ONE resolver. `read_terminal` exposes no `session_id` (only the activity, which
    /// is keyed by terminal), so what it proves here is that it resolved a session AT ALL through
    /// that same path.
    #[test]
    fn every_surface_names_the_same_session_for_a_terminal_hosting_two_kinds() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = KeyRecordingHost::default();
        let activity = AgentActivityStore::new();
        let tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();

        let start = |kind: &str, external: &str| {
            db.with_conn(|c| {
                db::record_session_start(
                    c,
                    &tid,
                    kind,
                    db::SessionCapture {
                        external_session_id: external.to_string(),
                        cwd: "/work".to_string(),
                        ..Default::default()
                    },
                )
            })
            .expect("seed session")
        };
        start(db::AGENT_KIND_CLAUDE_CODE, "sid-claude");
        // Two DISTINCT `started_at` values, so "the most recently started" is unambiguous.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let newest = start(db::AGENT_KIND_CODEX, "sid-codex");

        let resolved = db
            .with_conn(|c| db::active_session_for_terminal(c, &tid))
            .unwrap()
            .expect("the terminal hosts a live session");
        assert_eq!(
            resolved.id, newest.id,
            "the single resolver takes the most recently started"
        );

        let listed = list_terminals(&db, &host, &activity, &json!({})).unwrap();
        assert_eq!(
            listed["terminals"][0]["agent_session"]["session_id"],
            json!(resolved.id),
            "list_terminals must name the SAME session as the unit verbs: {listed}"
        );

        let waited = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "timeout_ms": 50 }),
        )
        .unwrap();
        assert_eq!(
            waited["agent_session"]["session_id"],
            json!(resolved.id),
            "and so must wait_for_terminal: {waited}"
        );

        let read = read_terminal(&db, &host, &activity, &json!({ "terminal_id": tid })).unwrap();
        assert_eq!(
            read["agent_activity"],
            json!("idle"),
            "read_terminal resolved a session through the same path (a null would mean it found \
             none): {read}"
        );
    }

    /// #K3 — THE ARMING WINDOW WITHHOLDS, IT NEVER SATISFIES.
    ///
    /// `until: ["working"]` used to resolve INSTANTLY on a freshly armed terminal (`waited_ms` ~ 0)
    /// because the window expressed itself by RETURNING `Working` — a supposition, handed to a
    /// caller that had explicitly asked to wait for the real thing, under the same field name
    /// `list_terminals` uses for the hook-asserted snapshot. Two tools, two answers, one terminal.
    ///
    /// Now the window answers a value no `until` set can contain, so it can only ever hold the wait
    /// open — and the answer's `agent_activity` is the snapshot, provably the same one
    /// `list_terminals` reports at the same moment.
    #[test]
    fn an_armed_submission_never_satisfies_an_explicit_until_working() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = KeyRecordingHost::default();
        let activity = AgentActivityStore::new();
        let tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-until-working",
                "cwd": "/work",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();

        send_keys(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "keys": ["un prompt", "enter"] }),
        )
        .unwrap();

        let out = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "until": ["working"], "timeout_ms": 250 }),
        )
        .unwrap();

        assert_eq!(
            out["resolved"],
            json!(false),
            "no hook has asserted `working`, so nothing may resolve a wait for it: {out}"
        );
        assert!(
            out["waited_ms"].as_u64().unwrap() >= 200,
            "it BLOCKED for its timeout instead of resolving on the window: {out}"
        );
        // The two surfaces agree, under the same field name, at the same moment.
        let listed = list_terminals(&db, &host, &activity, &json!({})).unwrap();
        assert_eq!(out["agent_activity"], json!("idle"));
        assert_eq!(out["agent_activity"], listed["terminals"][0]["agent_activity"]);
        assert_eq!(out["submission_pending"], json!(true));
    }

    /// #K4a — `send_to_terminal` SEGMENTS ITS SUBMIT ON AN AGENT TERMINAL, AND ONLY THERE.
    ///
    /// This tool arms the submission window unconditionally, on the grounds that it "always appends
    /// a newline". The host wrote `commande + CR` in ONE `write_to_record` — exactly the shape this
    /// branch measured as "types the prompt and never submits it" on an Ink TUI. So the window was
    /// armed for a turn that had not started and `wait_for_terminal` answered `resolved: true` on a
    /// prompt asleep in the input box.
    ///
    /// On an agent terminal the write now takes the same segmented path `send_keys` takes; on a
    /// plain shell it is the single write it has always been, byte for byte.
    #[test]
    fn send_to_terminal_segments_the_submit_only_on_an_agent_terminal() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = KeyRecordingHost::default();
        let activity = AgentActivityStore::new();
        let agent_tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        let plain_tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-send-to",
                "cwd": "/work",
                "NYX_TERMINAL_ID": agent_tid,
            }),
        )
        .unwrap();

        // AGENT TERMINAL — several PTY writes, and the CR leaves ALONE.
        send_to_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": agent_tid, "command": "resume ce projet" }),
        )
        .unwrap();
        let writes = host.writes.lock().unwrap().clone();
        assert!(
            writes.len() > 1,
            "an agent-hosting terminal must get the segmented submit, got {writes:?}"
        );
        assert_eq!(writes, vec![b"resume ce projet".to_vec(), b"\r".to_vec()]);
        assert_eq!(
            writes.last().unwrap(),
            b"\r",
            "the submit leaves as a LONE carriage return — the only chunk Ink reads as Enter"
        );
        assert!(
            host.lines.lock().unwrap().is_empty(),
            "and it did NOT take the single-write shell path"
        );
        // Which is what makes the arming honest: this write really can submit the turn.
        assert!(activity.submission_pending(&agent_tid));

        // PLAIN SHELL — the single write, unchanged, and nothing armed.
        host.writes.lock().unwrap().clear();
        send_to_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": plain_tid, "command": "ls" }),
        )
        .unwrap();
        assert_eq!(
            *host.lines.lock().unwrap(),
            vec!["ls".to_string()],
            "a shell with no agent keeps the ONE write it has always had"
        );
        assert!(
            host.writes.lock().unwrap().is_empty(),
            "no segmentation on a line-mode shell — it reads bytes, not chunks"
        );
        assert!(!activity.submission_pending(&plain_tid));
    }

    /// A TIMEOUT IS AN OUTCOME, NOT AN ERROR — and a second call picks the wait back up.
    ///
    /// Also pins the OTHER half of the `until` default: a turn that stops to ask the human
    /// (`waiting`) resolves the wait too. A caller that waited only for `idle` would sit through
    /// its whole timeout on a session that is blocked and cannot move without an answer.
    #[test]
    fn wait_for_terminal_times_out_without_erroring_and_resumes() {
        use crate::agent_activity::ActivityEvent;
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        activity.apply(&tid, ActivityEvent::PromptSubmitted); // working

        // A short wait on a turn that does not end: `resolved:false`, NO error.
        let timed_out = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "timeout_ms": 60 }),
        )
        .expect("a timeout is a normal answer, not an Err");
        assert_eq!(timed_out["resolved"], json!(false));
        assert_eq!(
            timed_out["agent_activity"],
            json!("working"),
            "it reports the state it gave up on, not a null"
        );

        // The agent now blocks on a permission prompt. A NEW call continues the wait and
        // resolves — `waiting` is part of "the agent gave you the hand back".
        activity.apply(
            &tid,
            ActivityEvent::AttentionNeeded {
                permission: Some("Claude needs your permission to use Bash".into()),
            },
        );
        let resumed = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "timeout_ms": 60 }),
        )
        .unwrap();
        assert_eq!(resumed["resolved"], json!(true));
        assert_eq!(resumed["agent_activity"], json!("waiting"));

        // `until` can name the states explicitly, including the `settled` alias…
        let explicit = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "until": ["settled"], "timeout_ms": 0 }),
        )
        .unwrap();
        assert_eq!(explicit["resolved"], json!(true));
        // …and an unknown one is an actionable invalid_argument, never a silent default.
        let err = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "until": ["finished"] }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains("idle|working|waiting|settled"),
            "the refusal names the vocabulary: {}",
            err.message
        );
    }

    /// THE CURSOR CHAINS — `wait_for_terminal`'s `cursor` goes straight into
    /// `read_terminal(since=…)` with no gap and no duplication, exactly as `wait_for_command`'s
    /// does into `get_command_output`.
    ///
    /// It also pins the FIRST-CALL BOUNDING: with no `since`, the window starts at the current end
    /// of the buffer, so `output_tail` carries what appeared AFTER the call and never the whole
    /// existing scrollback (the thing that made every "incremental" read of the pilot run return
    /// the entire history).
    #[test]
    fn wait_for_terminal_cursor_chains_into_read_terminal() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        // What the terminal had shown BEFORE the wait — the prompt we sent, among other things.
        db.with_conn(|c| db::persist_scrollback(c, &tid, "OLD-HISTORY\n"))
            .unwrap();

        // No agent session and no activity ⇒ idle ⇒ the default wait resolves at once. The point
        // here is the WINDOW, so a zero-length wait is what we want.
        let waited = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "timeout_ms": 0 }),
        )
        .unwrap();
        assert_eq!(waited["resolved"], json!(true));
        assert_eq!(
            waited["output_tail"], json!(""),
            "a first call returns what came AFTER it, not the existing scrollback: {waited}"
        );
        let cursor = waited["cursor"].as_u64().unwrap();
        assert_eq!(cursor, "OLD-HISTORY\n".len() as u64);

        // The answer lands.
        db.with_conn(|c| db::persist_scrollback(c, &tid, "OLD-HISTORY\nANSWER-ONE\n"))
            .unwrap();

        // The cursor chains into read_terminal: exactly the new bytes, once.
        let read = read_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "since": cursor }),
        )
        .unwrap();
        assert_eq!(read["output"], json!("ANSWER-ONE\n"));
        assert_eq!(read["reset"], json!(false));
        let next = read["cursor"].as_u64().unwrap();

        // …and chains ONWARD through a second wait, still with no gap and no duplicate.
        db.with_conn(|c| db::persist_scrollback(c, &tid, "OLD-HISTORY\nANSWER-ONE\nANSWER-TWO\n"))
            .unwrap();
        let again = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "since": next, "timeout_ms": 0 }),
        )
        .unwrap();
        assert_eq!(again["output_tail"], json!("ANSWER-TWO\n"));
        assert_eq!(again["reset"], json!(false));
    }

    /// THE SECOND HALF OF THE SAME GATE FINDING (2026-09-02) — resolving on the end of the turn is
    /// not enough if the ANSWER has not been saved yet.
    ///
    /// The activity comes from the agent's `Stop` hook; `output_tail` comes from
    /// `terminals.scrollback`, which the sidecar saves on a 750 ms debounce. Measured on the real
    /// run: the wait returned on the true end of turn, and for the next ~780 ms `read_terminal`
    /// answered `stale: true` with an EMPTY delta — so the caller read again, which is the
    /// catch-up call this tool exists to remove. The wait now absorbs that lag itself.
    ///
    /// The host here says "behind" until a background thread saves the answer, exactly as the pump
    /// does; the assertion is that the ANSWER is in the tool's own reply, with no second read.
    #[test]
    fn wait_for_terminal_waits_for_the_answer_to_be_saved_before_returning_it() {
        use std::sync::atomic::{AtomicBool, Ordering};
        /// A host whose scrollback is behind until `saved` flips — the pump's exact bit.
        struct LaggingHost {
            saved: std::sync::Arc<AtomicBool>,
        }
        impl TerminalHost for LaggingHost {
            fn park_opening_command(&self, _: &str, _: &str) {}
            fn open_terminal_shell(&self, _: &str, _: &str) {}
            fn send_to_terminal(&self, _: &str, _: &str) -> Result<bool, String> {
                Ok(true)
            }
            fn send_keys(&self, _: &str, _: &[Vec<u8>]) -> Result<bool, String> {
                Ok(true)
            }
            fn close_terminal_pty(&self, _: &str) {}
            fn terminal_liveness(&self, _: &str) -> (bool, Option<bool>) {
                (true, None)
            }
            fn scrollback_is_behind(&self, _: &str) -> bool {
                !self.saved.load(Ordering::SeqCst)
            }
        }

        let _g = db_guard();
        let db = std::sync::Arc::new(Db::in_memory());
        let saved = std::sync::Arc::new(AtomicBool::new(false));
        let host = LaggingHost {
            saved: std::sync::Arc::clone(&saved),
        };
        let activity = AgentActivityStore::new();
        let tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        db.with_conn(|c| db::persist_scrollback(c, &tid, "PROMPT\n"))
            .unwrap();

        // The debounced save lands a moment after the turn ended.
        let saver = {
            let db = std::sync::Arc::clone(&db);
            let saved = std::sync::Arc::clone(&saved);
            let tid = tid.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                db.with_conn(|c| db::persist_scrollback(c, &tid, "PROMPT\nLA REPONSE\n"))
                    .unwrap();
                saved.store(true, Ordering::SeqCst);
            })
        };

        // The turn is already over for the ACTIVITY (no session ⇒ idle), so without the settle this
        // would return instantly with an empty tail — which is precisely what the run measured.
        let out = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "since": "PROMPT\n".len(), "timeout_ms": 0 }),
        )
        .unwrap();
        saver.join().unwrap();

        assert_eq!(out["resolved"], json!(true));
        assert_eq!(
            out["output_tail"],
            json!("LA REPONSE\n"),
            "the answer travels with the wait — no second read: {out}"
        );
        assert_eq!(
            out["stale"],
            json!(false),
            "and it is not handed back as a snapshot known to be behind: {out}"
        );
        assert!(
            out["waited_ms"].as_u64().unwrap() >= 150,
            "waited_ms covers the whole call, settle included: {out}"
        );
    }

    /// The settle is a CEILING, not a delay: a terminal that never stops printing is never level,
    /// and the wait must still come back. It returns past [`SCROLLBACK_SETTLE_BUDGET`] saying
    /// `stale: true` rather than blocking on a bit that will never clear.
    #[test]
    fn a_terminal_that_never_settles_still_returns_and_says_it_is_stale() {
        struct AlwaysBehindHost;
        impl TerminalHost for AlwaysBehindHost {
            fn park_opening_command(&self, _: &str, _: &str) {}
            fn open_terminal_shell(&self, _: &str, _: &str) {}
            fn send_to_terminal(&self, _: &str, _: &str) -> Result<bool, String> {
                Ok(true)
            }
            fn send_keys(&self, _: &str, _: &[Vec<u8>]) -> Result<bool, String> {
                Ok(true)
            }
            fn close_terminal_pty(&self, _: &str) {}
            fn terminal_liveness(&self, _: &str) -> (bool, Option<bool>) {
                (true, None)
            }
            fn scrollback_is_behind(&self, _: &str) -> bool {
                true
            }
        }

        let _g = db_guard();
        let db = Db::in_memory();
        let host = AlwaysBehindHost;
        let activity = AgentActivityStore::new();
        let tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();

        let started = std::time::Instant::now();
        let out = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "timeout_ms": 0 }),
        )
        .unwrap();
        let spent = started.elapsed();

        assert_eq!(out["resolved"], json!(true));
        assert_eq!(out["stale"], json!(true), "it says what it handed back: {out}");
        assert!(
            spent >= SCROLLBACK_SETTLE_BUDGET,
            "it did give the save its budget: {spent:?}"
        );
        assert!(
            spent < SCROLLBACK_SETTLE_BUDGET * 3,
            "…and stopped there rather than blocking on a bit that never clears: {spent:?}"
        );
    }

    /// A TERMINAL WITH NO AGENT SESSION stays usable — the tool answers `agent_session: null` and
    /// an `idle` activity instead of refusing, so a caller that cannot yet know whether a session
    /// registered gets a fact rather than an error code. An UNKNOWN id, on the other hand, is the
    /// same actionable `invalid_id` the other terminal verbs give.
    #[test]
    fn wait_for_terminal_on_a_plain_terminal_is_usable_and_on_an_unknown_id_is_actionable() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();

        let out = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": tid, "timeout_ms": 0 }),
        )
        .expect("a terminal without an agent session is not an error");
        assert_eq!(out["resolved"], json!(true));
        assert_eq!(out["agent_activity"], json!("idle"));
        assert_eq!(out["agent_session"], Value::Null);

        let err = wait_for_terminal(
            &db,
            &host,
            &activity,
            &json!({ "terminal_id": "nope", "timeout_ms": 0 }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    /// NO PHANTOM AFTER THE FACT — the activity falls back to `idle` the moment the PTY dies or
    /// the session ends, whichever comes first. Both are the SAME clear, and both are asserted
    /// THROUGH `list_terminals` (the surface an agent reads), not on the store alone.
    #[test]
    fn list_terminals_activity_falls_back_to_idle_on_pty_death_and_session_end() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let tid = create_terminal(&db, &host, &json!({})).unwrap().result["terminal_id"]
            .as_str()
            .unwrap()
            .to_string();
        let activity_of = |store: &AgentActivityStore| -> Value {
            list_terminals(&db, &host, store, &json!({})).unwrap()["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["terminal_id"] == json!(tid))
                .unwrap()["agent_activity"]
                .clone()
        };

        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-phantom",
                "cwd": "/work",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        assert_eq!(activity_of(&activity), json!("working"));

        // PTY DEATH — the host's anti-phantom clear (the same call the shell makes when a shell
        // exits or a terminal closes). No `Stop` ever arrived, and yet nothing stays `working`.
        activity.clear(&tid);
        assert_eq!(
            activity_of(&activity),
            json!("idle"),
            "a killed agent that never sent Stop leaves no working dot behind"
        );

        // SESSION END — the same fallback, through the hook this time. The session row flips to
        // `ended`, so the LINK goes away with it and the row is a plain terminal again.
        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();
        assert_eq!(activity_of(&activity), json!("working"));
        session_event(
            &db,
            &activity,
            &json!({
                "hook_event_name": "SessionEnd",
                "session_id": "sid-phantom",
                "reason": "logout",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        assert_eq!(
            activity_of(&activity),
            Value::Null,
            "an ended session is no longer an agent terminal at all"
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
        session_event(
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
        session_event(
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

        let out = session_event(
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

        let out = session_event(
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

    /// A project + a workspace carrying `command_count` managed commands, and a terminal BOUND to
    /// that workspace — the shape every ambient-context test needs. `group` names the templates that
    /// are put in ONE multi-pane group (by index); fewer than two leaves every command solo.
    /// Returns `(project_id, workspace_id, terminal_id, group_id)`.
    fn workspace_with_commands(
        db: &Db,
        command_count: usize,
        group: &[usize],
    ) -> (String, String, String, Option<String>) {
        use crate::schema::terminals;
        use diesel::prelude::*;
        let host = NoTerminalHost;
        let (project, workspace) = db
            .with_conn(|c| db::create_project(c, "sandbox", "/tmp/ambient", None))
            .unwrap();
        let mut template_ids: Vec<String> = Vec::new();
        for i in 0..command_count {
            let t = db
                .with_conn(|c| {
                    db::create_template(
                        c,
                        &project.id,
                        &format!("cmd-{i}"),
                        &format!("bun run task-{i}"),
                        None,
                        db::CommandSource::default(),
                    )
                })
                .unwrap();
            template_ids.push(t.id);
        }
        let group_id = if group.len() >= 2 {
            let members: Vec<String> = group.iter().map(|i| template_ids[*i].clone()).collect();
            Some(
                db.with_conn(|c| db::create_command_group(c, &project.id, &members))
                    .expect("group")
                    .id,
            )
        } else {
            None
        };
        let created = create_terminal(db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        db.with_conn(|c| {
            diesel::update(terminals::table.find(&tid))
                .set(terminals::workspace_id.eq(Some(workspace.id.clone())))
                .execute(c)
        })
        .unwrap();
        (project.id, workspace.id, tid, group_id)
    }

    /// The BOOTSTRAPPED Système scope carrying `command_count` commands — the other half of every
    /// ambient-context test since the shared commands ride in the same block
    /// (review `01M1EJ84VCWRD6F4NDVD2M0QWJ`). Returns the scope, so a test can assert on the ids it
    /// must NOT find in the context as easily as on the instances it must.
    ///
    /// `Db::in_memory` does NOT bootstrap the singleton (only `Db::open` does), which is why every
    /// ambient test written before this one still measures a machine with no shared command at all —
    /// and why that case had to be re-proven rather than assumed.
    fn system_scope_with_commands(
        db: &Db,
        command_count: usize,
    ) -> crate::system_scope::SystemScope {
        let scope = db
            .with_conn(|c| {
                crate::system_scope::ensure_system_scope(
                    c,
                    &crate::system_scope::FixedHomeDirProvider::at(if cfg!(windows) {
                        "C:\\Users\\kris"
                    } else {
                        "/home/kris"
                    }),
                )
            })
            .expect("bootstrap the Système scope");
        for i in 0..command_count {
            db.with_conn(|c| {
                db::create_template(
                    c,
                    &scope.project.id,
                    &format!("svc-{i}"),
                    &format!("docker compose up svc-{i}"),
                    None,
                    db::CommandSource::default(),
                )
            })
            .expect("a Système command");
        }
        scope
    }

    /// THE HOOK BUDGET, MEASURED ON THE WORST REALISTIC WORKLOAD — not assumed, and no longer on a
    /// best case. The bundled `nyx-session-hook.cjs` bounds its `SessionStart` POST at
    /// `TIMEOUT_MS = 1000` ms, and a timeout drops the WHOLE injected context, orientation line
    /// included. The block adds TWO SQLite reads plus N `runner.outcome` lookups (each briefly
    /// taking the runner's `entries` mutex) to that request, so this times the FULL handler on the
    /// block's own upper bound: a 12-command workspace AND a Système scope at its own cap of eight
    /// shared commands — the twenty lines the renderer can put in front of a session, both halves
    /// carrying a full scrollback (review `01M1EJ84VCWRD6F4NDVD2M0QWJ`; the shared half doubles the
    /// number of reads on this path, so it is the half that had to be re-measured, not assumed).
    ///
    /// AND IT IS MEASURED ON THE SCROLLBACK-FULL WORKLOAD (review finding
    /// `01M152NMXPWKD59QFK95CMSZWQ`). A freshly created instance has an EMPTY `scrollback`, so a
    /// workspace that never ran anything measures the BEST case — while the workspace where the
    /// block is actually useful is the one whose commands HAVE run, i.e. the one carrying up to
    /// `12 × db::MAX_SCROLLBACK_BYTES` = 3 MiB of persisted output. The number this test constates
    /// is therefore the one taken AFTER seeding those 3 MiB.
    ///
    /// WHAT THIS TEST IS AND IS NOT (review finding `01M1580EK4BXWC4EP39GQBMDRM`). It carries ONE
    /// assert, and it is ABSOLUTE: the handler stays a small fraction of the hook's 1000 ms. That
    /// is the constat the PRD asked for, and a wide absolute bound cannot flake on a shared CI
    /// runner. It used to carry a SECOND assert comparing the full run against a ceiling derived
    /// from the empty run (`empty * 6 + 2 ms`) — an instrument that was simply wrong for the job:
    /// over 43 idle-machine runs `empty` spread by a factor 2.6 while `full` spread by a factor 19
    /// (0.74 ms → 14.3 ms), because the full phase runs right after writing 3 MiB into SQLite and
    /// eats the cold pages, the allocator and the journal alone. The two numbers do NOT move
    /// together, so the ratio flaked (1 failure in 43) — on a test replayed by `bun run test:core`,
    /// by `bun run test` and by the cross-OS gate.
    ///
    /// The property that assert was reaching for — "the SessionStart path never reads
    /// `scrollback`" — is not a matter of degree, so it is no longer measured at all: it is proven
    /// structurally, on the SQL the handler actually emits, by
    /// [`session_start_never_reads_the_scrollback_column`] below. The empty-scrollback timing is
    /// kept here as an OBSERVATION in the `eprintln!` (no assert derives from it), because seeing
    /// both numbers side by side is what makes the constat readable.
    #[test]
    fn session_start_on_a_twelve_command_workspace_stays_far_under_the_hook_budget() {
        let _g = db_guard();
        let db = Db::in_memory();
        let activity = AgentActivityStore::new();
        let runner = noop_runner();
        let (_p, ws, tid, _g_id) = workspace_with_commands(&db, 12, &[0, 1]);
        // …AND the shared half at its own cap: eight machine-wide services, read by a SECOND lean
        // query on the same request.
        let scope = system_scope_with_commands(&db, 8);
        let event = json!({
            "hook_event_name": "SessionStart",
            "session_id": "sid-budget",
            "cwd": "/tmp/ambient",
            "NYX_TERMINAL_ID": tid,
        });
        // The worst of 20 full `agent_session_event` calls, after one warm-up (the first call pays
        // the connection/statement setup, which the hook's real sidecar has long paid by the time
        // a session starts). Asserts on the way that what is timed really is the FULL block — the
        // twelve workspace commands AND the eight shared ones.
        fn worst_of_twenty<S: RunnerSink>(
            db: &Db,
            runner: &CommandRunner<S>,
            activity: &AgentActivityStore,
            event: &Value,
        ) -> std::time::Duration {
            agent_session_event(db, runner, activity, event).unwrap();
            let mut worst = std::time::Duration::ZERO;
            for _ in 0..20 {
                let t0 = std::time::Instant::now();
                let out = agent_session_event(db, runner, activity, event).unwrap();
                worst = worst.max(t0.elapsed());
                let ctx = out.result["context"].as_str().unwrap();
                assert_eq!(
                    ctx.lines().filter(|l| l.starts_with("- ")).count(),
                    20,
                    "what is being timed really is the full block, both halves: {ctx}"
                );
                assert_eq!(
                    ctx.lines().filter(|l| l.contains("nature=system")).count(),
                    8,
                    "…the shared half included: {ctx}"
                );
            }
            worst
        }

        // (1) The best case the previous constat measured: commands that never ran.
        let empty = worst_of_twenty(&db, &runner, &activity, &event);

        // (2) The realistic one: 256 KiB of persisted output on EVERY instance of BOTH workspaces —
        // 5 MiB sitting in the rows this SessionStart joins over.
        let full_scrollback = "z".repeat(db::MAX_SCROLLBACK_BYTES);
        let seed = |workspace_id: &str| -> Vec<db::InstanceWithTemplate> {
            db.with_conn(|c| {
                let rows = db::list_instances_for_workspace(c, workspace_id)?;
                for inst in &rows {
                    db::persist_instance_scrollback(c, &inst.id, &full_scrollback)?;
                }
                db::list_instances_for_workspace(c, workspace_id)
            })
            .expect("seeded scrollbacks")
        };
        let seeded = seed(&ws);
        let seeded_system = seed(&scope.workspace.id);
        assert_eq!(seeded.len(), 12, "twelve workspace instances were seeded");
        assert_eq!(seeded_system.len(), 8, "eight shared instances were seeded");
        assert!(
            seeded
                .iter()
                .chain(seeded_system.iter())
                .all(|r| r.scrollback.len() == db::MAX_SCROLLBACK_BYTES),
            "the workload really carries a full scrollback per instance, on both sides"
        );
        let full = worst_of_twenty(&db, &runner, &activity, &event);

        let budget = std::time::Duration::from_millis(1000);
        eprintln!(
            "SessionStart on a 12-command workspace + 8 shared System commands: worst of 20 = \
             {:?} with EMPTY scrollbacks, {:?} with a FULL {} KiB scrollback on each ({} MiB \
             persisted) — hook budget {:?}, margin {:?}",
            empty,
            full,
            db::MAX_SCROLLBACK_BYTES / 1024,
            (20 * db::MAX_SCROLLBACK_BYTES) / (1024 * 1024),
            budget,
            budget.saturating_sub(full),
        );
        assert!(
            full < std::time::Duration::from_millis(250),
            "the whole SessionStart must stay a small fraction of the hook's 1000 ms budget, took {full:?}",
        );
    }

    /// THE PROPERTY THE BUDGET RESTS ON, PROVEN STRUCTURALLY — no clock involved (review finding
    /// `01M1580EK4BXWC4EP39GQBMDRM`).
    ///
    /// `session_context_line` must render the ambient block WITHOUT loading
    /// `command_instances.scrollback`, bounded at [`db::MAX_SCROLLBACK_BYTES`] (256 KiB) per
    /// instance — up to 3 MiB on a twelve-command workspace, the dominant term of a request the
    /// hook bounds at 1000 ms. That is why `db::list_command_lines_for_workspace` exists next to
    /// `db::list_instances_for_workspace`: same JOIN, same order, six columns, no `scrollback`.
    ///
    /// "Does not load it" is a fact about the SQL, not about a duration, so it is asserted on the
    /// SQL: the connection is instrumented ([`diesel::connection::Instrumentation`]), a REAL
    /// `SessionStart` runs through the REAL handler, and every statement it emitted is inspected.
    /// `Db::in_memory` pools exactly ONE connection (`max_size(1)`), so the instrumentation
    /// installed here is still on the connection `agent_session_event` checks out — the evidence is
    /// what the handler ITSELF ran, not a query rebuilt in the test and free to drift from it.
    /// Route the block back through `db::list_instances_for_workspace` and this test fails on the
    /// `scrollback` in the projection — deterministically, on every machine, loaded or idle.
    #[test]
    fn session_start_never_reads_the_scrollback_column() {
        use diesel::connection::{Connection, InstrumentationEvent};
        use std::sync::{Arc, Mutex};

        let _g = db_guard();
        let db = Db::in_memory();
        let activity = AgentActivityStore::new();
        let runner = noop_runner();
        let (_p, ws, tid, _g_id) = workspace_with_commands(&db, 12, &[0, 1]);
        // The SHARED half is under the same property, and it is a SECOND read of the same shape —
        // so it is seeded and recorded here too rather than trusted to be lean by family
        // resemblance (review `01M1EJ84VCWRD6F4NDVD2M0QWJ`).
        let scope = system_scope_with_commands(&db, 8);

        // The realistic workload: 256 KiB of persisted output on every one of the twenty
        // instances, so a reader that DID select the column would have 5 MiB to show for it.
        let full_scrollback = "z".repeat(db::MAX_SCROLLBACK_BYTES);
        let seed = |workspace_id: &str| -> Vec<db::InstanceWithTemplate> {
            db.with_conn(|c| {
                let rows = db::list_instances_for_workspace(c, workspace_id)?;
                for inst in &rows {
                    db::persist_instance_scrollback(c, &inst.id, &full_scrollback)?;
                }
                db::list_instances_for_workspace(c, workspace_id)
            })
            .expect("seeded scrollbacks")
        };
        let seeded = seed(&ws);
        let seeded_system = seed(&scope.workspace.id);
        assert!(
            seeded.len() == 12
                && seeded_system.len() == 8
                && seeded
                    .iter()
                    .chain(seeded_system.iter())
                    .all(|r| r.scrollback.len() == db::MAX_SCROLLBACK_BYTES),
            "the column the handler must not read really is populated on all twenty instances"
        );

        // Record every statement the pooled connection runs from here on.
        let statements: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&statements);
        db.with_conn(move |c| {
            c.set_instrumentation(move |event: InstrumentationEvent<'_>| {
                if let InstrumentationEvent::StartQuery { query, .. } = event {
                    sink.lock().expect("sql sink").push(query.to_string());
                }
            });
        });

        let out = agent_session_event(
            &db,
            &runner,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-sql-probe",
                "cwd": "/tmp/ambient",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        // Detach before asserting: the sink must not outlive the recorded window.
        db.with_conn(|c| c.set_instrumentation(|_: InstrumentationEvent<'_>| {}));

        let ctx = out.result["context"].as_str().unwrap();
        assert_eq!(
            ctx.lines().filter(|l| l.starts_with("- ")).count(),
            20,
            "the recorded window really covers the full block, shared half included: {ctx}"
        );
        assert_eq!(
            ctx.lines().filter(|l| l.contains("nature=system")).count(),
            8,
            "…and the shared read really did happen inside it: {ctx}"
        );

        // A captured statement displays as `<SQL> -- binds: [...]`. Only the SQL half is evidence:
        // a BIND is DATA and can carry the word by accident (a `cwd`, a session id, a command
        // text), which would make this assert lie in either direction.
        let sql: Vec<String> = statements
            .lock()
            .expect("sql sink")
            .iter()
            .map(|s| s.split(" -- binds:").next().unwrap_or(s).to_string())
            .collect();
        assert!(
            sql.iter()
                .any(|s| s.contains("`command_instances`") && s.contains("`managed_commands`")),
            "the handler really did join the workspace's command instances: {sql:#?}"
        );
        // The QUALIFIED column, deliberately: the 3 MiB at stake is
        // `command_instances.scrollback`, twelve rows of it. `terminals.scrollback` is a
        // different, single-row column that the terminal resolution on this path has always
        // read — out of this property's scope, and naming the table keeps the two apart.
        let offenders: Vec<&String> = sql
            .iter()
            .filter(|s| s.contains("`command_instances`.`scrollback`"))
            .collect();
        assert!(
            offenders.is_empty(),
            "the SessionStart path selected `command_instances`.`scrollback` in {} of its {} \
             statements — the ambient block is back on a reader that drags up to {} MiB onto the \
             hook's 1000 ms budget: {offenders:#?}",
            offenders.len(),
            sql.len(),
            (12 * db::MAX_SCROLLBACK_BYTES) / (1024 * 1024),
        );
    }

    /// THE AMBIENT BLOCK, THROUGH THE REAL CHAIN. A `SessionStart` on a terminal bound to a
    /// workspace that HAS managed commands carries, under the untouched orientation line, one line
    /// per command with its `instance_id` and the directive naming the tools to act on it.
    #[test]
    fn session_start_context_carries_the_workspace_commands_and_the_directive() {
        let _g = db_guard();
        let db = Db::in_memory();
        let activity = AgentActivityStore::new();
        let runner = noop_runner();
        let (_p, ws, tid, group_id) = workspace_with_commands(&db, 3, &[0, 1]);
        let group_id = group_id.expect("two templates were grouped");

        let out = agent_session_event(
            &db,
            &runner,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-block",
                "cwd": "/tmp/ambient",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        let ctx = out.result["context"].as_str().expect("a context line");

        // The orientation line is untouched, and FIRST.
        assert!(
            ctx.lines().next().unwrap().starts_with("You're in nyx — "),
            "the orientation line still opens the context: {ctx}"
        );
        assert!(
            ctx.lines().next().unwrap().contains(&format!("workspace_id={ws}")),
            "…and still carries the workspace id: {ctx}"
        );
        // Then the block: one line per command, each with its instance id.
        assert_eq!(
            ctx.lines().filter(|l| l.starts_with("- ")).count(),
            3,
            "one line per managed command: {ctx}"
        );
        let instances = db
            .with_conn(|c| db::list_instances_for_workspace(c, &ws))
            .unwrap();
        for inst in &instances {
            assert!(
                ctx.contains(&format!(
                    "- {} · bun run task",
                    inst.name
                )),
                "{} is named with its command line: {ctx}",
                inst.name
            );
            assert!(
                ctx.contains(&format!("· idle · instance_id={}", inst.id)),
                "{} carries its factual state and its instance id: {ctx}",
                inst.name
            );
        }
        assert!(
            ctx.contains(&format!("· group_id={group_id}")),
            "the grouped commands name their group: {ctx}"
        );
        // And the directive, naming the tools.
        for tool in [
            "start_command",
            "relaunch_command",
            "list_commands",
            "get_command_output",
        ] {
            assert!(ctx.contains(tool), "the directive names {tool}: {ctx}");
        }
        assert!(
            ctx.contains("/clear") && ctx.contains("/compact"),
            "…and says the snapshot is re-taken at /clear and /compact: {ctx}"
        );
    }

    /// NOTHING WITHOUT COMMANDS. A workspace-bound session on a workspace that defines NO managed
    /// command gets the orientation line and STRICTLY nothing else — no empty header, no directive
    /// about a list that does not exist.
    ///
    /// "No command" means on EITHER side: this `Db` has no bootstrapped Système scope, so there is
    /// no shared half either (see `a_session_start_carries_the_shared_system_commands_next_to_the_workspaces_own`
    /// for the machine that has one).
    #[test]
    fn a_workspace_without_commands_gets_neither_block_nor_directive() {
        let _g = db_guard();
        let db = Db::in_memory();
        let activity = AgentActivityStore::new();
        let runner = noop_runner();
        let (_p, _ws, tid, _g_id) = workspace_with_commands(&db, 0, &[]);

        let out = agent_session_event(
            &db,
            &runner,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-empty",
                "cwd": "/tmp/ambient",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        let ctx = out.result["context"].as_str().unwrap();
        assert_eq!(
            ctx.lines().count(),
            1,
            "the context is the orientation line and nothing else: {ctx}"
        );
        assert!(
            !ctx.contains("Managed commands") && !ctx.contains("start_command"),
            "no block, no directive: {ctx}"
        );
    }

    /// A LOOSE terminal (no workspace) gets no block either — and the HARD constraint holds: its
    /// context still contains no `workspace_id=` at all. Twin of
    /// `agent_session_event_start_returns_minimal_nyx_context`, asserted here against the block.
    ///
    /// On a machine that HAS shared services the loose terminal does get those (they are its to act
    /// on) — `a_loose_terminal_gets_the_shared_half_and_still_names_no_workspace`. Here the Système
    /// scope is not bootstrapped, so what is pinned is the workspace half and it alone.
    #[test]
    fn a_loose_terminal_gets_no_block_and_still_names_no_workspace() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let runner = noop_runner();
        // A workspace WITH commands exists in the DB — the loose terminal must still see nothing.
        workspace_with_commands(&db, 3, &[]);
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        let out = agent_session_event(
            &db,
            &runner,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-loose",
                "cwd": "/work",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        let ctx = out.result["context"].as_str().unwrap();
        assert_eq!(
            ctx.lines().count(),
            1,
            "a loose terminal's context is one line: {ctx}"
        );
        assert!(
            !ctx.contains("workspace_id="),
            "a loose terminal names no workspace, block or no block: {ctx}"
        );
        assert!(
            !ctx.contains("Managed commands"),
            "…and lists the commands of no workspace: {ctx}"
        );
    }

    /// ISOLATION. Two workspaces of the SAME project each materialize their own instances of the
    /// project's templates; a session on one names ONLY its own instance ids. A block that leaked a
    /// sibling's ids would send `start_command` at the wrong worktree.
    #[test]
    fn the_block_names_only_the_instances_of_its_own_workspace() {
        use crate::schema::terminals;
        use diesel::prelude::*;
        let _g = db_guard();
        let db = Db::in_memory();
        let activity = AgentActivityStore::new();
        let runner = noop_runner();
        let (project, root, tid, _g_id) = workspace_with_commands(&db, 2, &[]);
        // A SECOND workspace on the same project — the templates materialize into it too.
        let feature = db
            .with_conn(|c| db::create_workspace(c, &project, "feature", "/tmp/ambient-feature"))
            .expect("second workspace");
        let created = create_terminal(&db, &NoTerminalHost, &json!({})).unwrap();
        let feature_tid = created.result["terminal_id"].as_str().unwrap().to_string();
        db.with_conn(|c| {
            diesel::update(terminals::table.find(&feature_tid))
                .set(terminals::workspace_id.eq(Some(feature.id.clone())))
                .execute(c)
        })
        .unwrap();

        let context_of = |terminal_id: &str, session_id: &str| -> String {
            agent_session_event(
                &db,
                &runner,
                &activity,
                &json!({
                    "hook_event_name": "SessionStart",
                    "session_id": session_id,
                    "cwd": "/tmp/ambient",
                    "NYX_TERMINAL_ID": terminal_id,
                }),
            )
            .unwrap()
            .result["context"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let root_ctx = context_of(&tid, "sid-root");
        let feature_ctx = context_of(&feature_tid, "sid-feature");

        let ids_of = |workspace_id: &str| -> Vec<String> {
            db.with_conn(|c| db::list_instances_for_workspace(c, workspace_id))
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect()
        };
        let root_ids = ids_of(&root);
        let feature_ids = ids_of(&feature.id);
        assert_eq!(root_ids.len(), 2);
        assert_eq!(feature_ids.len(), 2);
        for id in &root_ids {
            assert!(root_ctx.contains(id), "the root block names its own {id}");
            assert!(
                !feature_ctx.contains(id),
                "…and the feature block does NOT: {feature_ctx}"
            );
        }
        for id in &feature_ids {
            assert!(feature_ctx.contains(id), "…and vice versa");
            assert!(!root_ctx.contains(id), "…: {root_ctx}");
        }
    }

    // --- THE SHARED HALF (review `01M1EJ84VCWRD6F4NDVD2M0QWJ`) ----------------------
    //
    // The user watched a real agent start in one of his projects and see `fake-ticker`,
    // `fake-worker`, `fake-idle` — and NOT `Commande factice`, the Système command it was perfectly
    // authorized to read and to start. Nothing was refusing it: the block simply asked ONE
    // workspace. These tests are that gap, closed and pinned.

    /// WHAT AN AGENT NOW RECEIVES. A session started in a PROJECT gets its own commands AND the
    /// machine-wide ones, in the same block, told apart line by line, with the directive that says
    /// what sharing costs.
    #[test]
    fn a_session_start_carries_the_shared_system_commands_next_to_the_workspaces_own() {
        let _g = db_guard();
        let db = Db::in_memory();
        let activity = AgentActivityStore::new();
        let runner = noop_runner();
        let (_p, ws, tid, _g_id) = workspace_with_commands(&db, 2, &[]);
        let scope = system_scope_with_commands(&db, 2);

        let out = agent_session_event(
            &db,
            &runner,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-shared",
                "cwd": "/tmp/ambient",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap();
        let ctx = out.result["context"].as_str().unwrap();

        // The workspace's own half, unchanged.
        for inst in db
            .with_conn(|c| db::list_instances_for_workspace(c, &ws))
            .unwrap()
        {
            assert!(
                ctx.contains(&format!("· instance_id={}", inst.id)),
                "the workspace command {} is still there: {ctx}",
                inst.name
            );
        }
        // …and the shared one, marked, with the launchable id of each service.
        let shared = db
            .with_conn(|c| db::list_instances_for_workspace(c, &scope.workspace.id))
            .unwrap();
        assert_eq!(shared.len(), 2, "two Système services were seeded");
        for inst in &shared {
            assert!(
                ctx.contains(&format!(
                    "· instance_id={} · nature=system",
                    inst.id
                )),
                "the shared command {} rides in the block, marked as shared: {ctx}",
                inst.name
            );
        }
        assert!(
            ctx.contains("Shared System commands"),
            "the shared half announces itself: {ctx}"
        );
        // THE THREE FACTS THE DIRECTIVE OWES THE AGENT.
        assert!(
            ctx.contains("machine-wide")
                && ctx.contains("every project")
                && ctx.contains("every agent")
                && ctx.contains("ALL of them at once"),
            "1. these services are shared machine-wide, and stopping one stops it for all: {ctx}"
        );
        assert!(
            ctx.contains("Starting a stopped System command is free"),
            "2. starting is free, like a project command: {ctx}"
        );
        assert!(
            ctx.contains("needs the USER's approval") && ctx.contains("BLOCKS"),
            "3. every other verb waits for a human, and the call blocks meanwhile: {ctx}"
        );

        // THE DEVIATION STAYS BOUNDED: the block hands over what the lifecycle verbs take
        // (`instance_id`) and NOT the scope's own ids — `list_system_commands` is still where a
        // creation or a whole-group start gets its `project_id` / `workspace_id`.
        assert!(
            !ctx.contains(&scope.workspace.id) && !ctx.contains(&scope.project.id),
            "the Système scope's own ids are NOT injected: {ctx}"
        );
    }

    /// THE INVARIANT THAT MUST NOT BE UNDONE BY THIS. The Système workspace is excluded from
    /// `db::resolvable_workspaces` since phase 1 so the cwd auto-attach can never capture it — and
    /// the shared half does NOT go through a resolver: it reads the scope's own door
    /// (`system_scope::load`). Both facts are asserted in one place, because the cheap way to
    /// implement this finding would have been to widen the resolver, and that is exactly what the
    /// PRD forbids.
    #[test]
    fn the_shared_half_reads_the_scope_and_leaves_the_resolver_exclusion_alone() {
        let _g = db_guard();
        let db = Db::in_memory();
        let activity = AgentActivityStore::new();
        let runner = noop_runner();
        let (_p, _ws, tid, _g_id) = workspace_with_commands(&db, 1, &[]);
        let scope = system_scope_with_commands(&db, 1);

        let resolvable = db.with_conn(db::resolvable_workspaces).unwrap();
        assert!(
            !resolvable.iter().any(|w| w.id == scope.workspace.id),
            "the Système workspace is still out of the resolver's candidate set"
        );

        let ctx = agent_session_event(
            &db,
            &runner,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-resolver",
                "cwd": "/tmp/ambient",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap()
        .result["context"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            ctx.contains("nature=system"),
            "…and its commands reach the block all the same, through the scope's own read: {ctx}"
        );
    }

    /// BEST-EFFORT, PROVEN ON A READ THAT ACTUALLY FAILS. The `SessionStart` hook drops the WHOLE
    /// injected context — orientation line included — on an error, so the shared read may cost its
    /// own lines and NOTHING else. The DB is broken underneath it (the join's table is dropped) and
    /// the workspace half, already read, still renders whole.
    ///
    /// The order is the handler's own: workspace first, shared second. That is not a coincidence to
    /// be preserved by luck — it is why the shared half cannot subtract the workspace's.
    #[test]
    fn a_failing_shared_read_costs_its_own_lines_and_nothing_else() {
        use diesel::prelude::*;
        let _g = db_guard();
        let db = Db::in_memory();
        let (_p, ws, _tid, _g_id) = workspace_with_commands(&db, 3, &[]);
        let _scope = system_scope_with_commands(&db, 2);

        // HEALTHY first, so what follows is a regression of a real answer and not an empty that was
        // always empty.
        assert_eq!(
            system_command_lines(&db, Some(&ws)).len(),
            2,
            "the shared read answers on a healthy DB"
        );
        // The handler reads the workspace half FIRST.
        let workspace_lines = db
            .with_conn(|c| db::list_command_lines_for_workspace(c, &ws))
            .unwrap_or_default();
        assert_eq!(workspace_lines.len(), 3, "three workspace commands in hand");

        // …and the DB breaks under the shared read.
        db.with_conn(|c| {
            diesel::sql_query("PRAGMA foreign_keys = OFF")
                .execute(c)
                .expect("pragma off");
            diesel::sql_query("DROP TABLE command_instances")
                .execute(c)
                .expect("the join's table is gone");
        });

        let system_lines = system_command_lines(&db, Some(&ws));
        assert!(
            system_lines.is_empty(),
            "a failing shared read yields no line — and no panic, and no error to propagate"
        );
        let block =
            crate::mcp_context::session_commands_block(&workspace_lines, &system_lines, |_| None);
        assert!(
            block.contains("Managed commands of this workspace:")
                && block.lines().filter(|l| l.starts_with("- ")).count() == 3,
            "the workspace block is rendered WHOLE: {block}"
        );
        assert!(
            !block.contains("nature=system") && !block.contains("Shared System commands"),
            "…and not a word claims a shared command that could not be read: {block}"
        );
    }

    /// A LOOSE terminal (no workspace) on a machine that HAS shared services gets them — they are
    /// available to it exactly as they are to a project session, and the matrix says the same thing
    /// either way. The hard constraint still holds: no `workspace_id=` anywhere in its context, and
    /// no header claiming the commands of a workspace it does not have.
    #[test]
    fn a_loose_terminal_gets_the_shared_half_and_still_names_no_workspace() {
        let _g = db_guard();
        let db = Db::in_memory();
        let host = NoTerminalHost;
        let activity = AgentActivityStore::new();
        let runner = noop_runner();
        // A workspace WITH commands exists in the DB — the loose terminal must still see none of it.
        workspace_with_commands(&db, 3, &[]);
        let _scope = system_scope_with_commands(&db, 2);
        let created = create_terminal(&db, &host, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();

        let ctx = agent_session_event(
            &db,
            &runner,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-loose-shared",
                "cwd": "/work",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap()
        .result["context"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(
            !ctx.contains("workspace_id="),
            "a loose terminal names no workspace, block or no block: {ctx}"
        );
        assert!(
            !ctx.contains("Managed commands of this workspace:"),
            "…and no header claims the commands of a workspace it has not got: {ctx}"
        );
        assert_eq!(
            ctx.lines().filter(|l| l.contains("nature=system")).count(),
            2,
            "…while the shared services, which it CAN act on, are there: {ctx}"
        );
        assert_eq!(
            ctx.lines().filter(|l| l.starts_with("- ")).count(),
            2,
            "…and nothing of the other workspace leaked in: {ctx}"
        );
    }

    /// A session whose OWN workspace is the Système one lists its commands ONCE, under one header —
    /// not twice under two. A floor rather than a live case (the agent-facing terminal tools refuse
    /// a Système terminal outright), but a block that described the same list twice would be a
    /// block an agent has to reconcile.
    #[test]
    fn a_session_on_the_system_workspace_lists_its_commands_once() {
        use crate::schema::terminals;
        use diesel::prelude::*;
        let _g = db_guard();
        let db = Db::in_memory();
        let activity = AgentActivityStore::new();
        let runner = noop_runner();
        let scope = system_scope_with_commands(&db, 2);
        let created = create_terminal(&db, &NoTerminalHost, &json!({})).unwrap();
        let tid = created.result["terminal_id"].as_str().unwrap().to_string();
        db.with_conn(|c| {
            diesel::update(terminals::table.find(&tid))
                .set(terminals::workspace_id.eq(Some(scope.workspace.id.clone())))
                .execute(c)
        })
        .unwrap();

        let ctx = agent_session_event(
            &db,
            &runner,
            &activity,
            &json!({
                "hook_event_name": "SessionStart",
                "session_id": "sid-on-system",
                "cwd": "/tmp/ambient",
                "NYX_TERMINAL_ID": tid,
            }),
        )
        .unwrap()
        .result["context"]
            .as_str()
            .unwrap()
            .to_string();

        assert_eq!(
            ctx.lines().filter(|l| l.starts_with("- ")).count(),
            2,
            "two commands, two lines — not four: {ctx}"
        );
        for inst in db
            .with_conn(|c| db::list_instances_for_workspace(c, &scope.workspace.id))
            .unwrap()
        {
            assert_eq!(
                ctx.matches(&inst.id).count(),
                1,
                "{} is named exactly once: {ctx}",
                inst.name
            );
        }
        assert!(
            !ctx.contains("Shared System commands"),
            "no second header for a list that is already this session's own: {ctx}"
        );
    }

    /// `list_commands(workspace_id=…)` renders each instance's `group_id` (`null` = solo) — the tool
    /// the directive sends the agent back to for the LIVE state must carry at least what the block
    /// carried, or "go re-check list_commands" would mean "go read something poorer".
    #[test]
    fn list_commands_by_workspace_carries_the_group_id() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let (_p, ws, _tid, group_id) = workspace_with_commands(&db, 3, &[0, 1]);
        let group_id = group_id.expect("two templates were grouped");

        let out = list_commands(&db, &runner, &json!({ "workspace_id": ws })).unwrap();
        let commands = out["commands"].as_array().expect("commands array");
        assert_eq!(commands.len(), 3);
        let grouped: Vec<&Value> = commands
            .iter()
            .filter(|c| c["group_id"] == json!(group_id))
            .collect();
        assert_eq!(
            grouped.len(),
            2,
            "the two grouped instances name their group: {commands:?}"
        );
        assert!(
            commands
                .iter()
                .any(|c| c["group_id"].is_null() && c["name"] == json!("cmd-2")),
            "…and the solo one renders a null group_id: {commands:?}"
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
            let out = session_event(
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
            let out = session_event(
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
            let out = session_event(
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
        session_event(
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
        session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "UserPromptSubmit", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap();

        // `/clear` → SessionEnd { reason: clear }. The guard keeps everything live.
        let out = session_event(
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
        session_event(
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

    /// A [`TerminalHost`] that RECORDS what it was asked to do — which terminal PTYs to close
    /// (review 01KWEX11, done_criterion #2: the destructive `remove_workspace` cascade kills the
    /// workspace's live terminals through the host) and which shells to OPEN, in order, with the
    /// cwd asked for.
    #[derive(Default)]
    struct RecordingHost {
        closed: std::sync::Mutex<Vec<String>>,
        opened: std::sync::Mutex<Vec<(String, String)>>,
        parked: std::sync::Mutex<Vec<String>>,
    }
    impl TerminalHost for RecordingHost {
        fn park_opening_command(&self, terminal_id: &str, _: &str) {
            self.parked.lock().unwrap().push(terminal_id.to_string());
        }
        fn open_terminal_shell(&self, terminal_id: &str, cwd: &str) {
            self.opened
                .lock()
                .unwrap()
                .push((terminal_id.to_string(), cwd.to_string()));
        }
        fn send_to_terminal(&self, _: &str, _: &str) -> Result<bool, String> {
            Ok(false)
        }
        fn send_keys(&self, _: &str, _: &[Vec<u8>]) -> Result<bool, String> {
            Ok(false)
        }
        fn close_terminal_pty(&self, terminal_id: &str) {
            self.closed.lock().unwrap().push(terminal_id.to_string());
        }
        fn terminal_liveness(&self, _: &str) -> (bool, Option<bool>) {
            (false, None)
        }
        fn scrollback_is_behind(&self, _: &str) -> bool {
            false
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

    /// A12 — THE ANSWER NAMES THE FOLDER IT DID NOT DELETE.
    ///
    /// Detaching leaves the directory and everything in it exactly where they are, which is right —
    /// nyx does not destroy the user's work — but it was IMPLICIT: the reply said `removed: true`
    /// and named nothing, so an agent that detached a scratch workspace had no way to know it had
    /// left bytes behind, nor where. The audit found `_worktrees/audit-k9-ws` by hand.
    #[test]
    fn remove_workspace_names_the_folder_it_leaves_on_disk() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", "/tmp/rmw-path", None))
            .unwrap();
        let ws = db
            .with_conn(|c| db::create_workspace(c, &project.id, "scratch", "/tmp/rmw-path-scratch"))
            .unwrap();

        let host = NoTerminalHost;
        let outcome = remove_workspace(
            &db,
            &noop_runner(),
            &host,
            &json!({ "workspace_id": ws.id }),
        )
        .expect("remove");

        assert_eq!(outcome.result["removed"], json!(true));
        // Compared against the STORED `root_path` (nyx normalizes separators per platform), so the
        // assertion is "the reply names the folder", not "the reply spells it the way I typed it".
        assert_eq!(
            outcome.result["folder_kept"], ws.root_path,
            "the reply names the path that is STILL THERE: {}",
            outcome.result
        );
        // …and the tool's own description says so, so a caller knows before it calls.
        let listed = crate::mcp::tool_descriptors();
        let described = listed
            .iter()
            .find(|t| t["name"] == crate::mcp::REMOVE_WORKSPACE_TOOL)
            .expect("remove_workspace is advertised")["description"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            described.contains("FOLDER ON DISK IS NOT DELETED")
                && described.contains("folder_kept"),
            "the description states that the folder survives, and names the field: {described}"
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
        let err = session_event(
            &db,
            &activity,
            &json!({ "hook_event_name": "TotallyUnknownHook", "NYX_TERMINAL_ID": tid }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
    }

    /* ───────── COMMAND GROUPS — the MCP surface (review 01M11WE6H9E5D5FCS6RT0ZXAXN) ───────── */

    /// A project with `n` command templates, ready to be grouped.
    fn project_with_commands(db: &Db, n: usize, path: &str) -> (String, Vec<String>) {
        let (project, _root) = db
            .with_conn(|c| db::create_project(c, "P", path, None))
            .unwrap();
        let ids = (0..n)
            .map(|i| {
                db.with_conn(|c| {
                    db::create_template(
                        c,
                        &project.id,
                        &format!("cmd{i}"),
                        "echo hi",
                        None,
                        db::CommandSource::default(),
                    )
                })
                .unwrap()
                .id
            })
            .collect();
        (project.id, ids)
    }

    /// REMOVE MUST DISTINGUISH "the group survived" FROM "the group is gone" (review finding
    /// `01M11XKKFVQWSHZQ4TGT3P3Z06`).
    ///
    /// The contract says `group` is null "once the group is gone". The code returned null for
    /// EVERY removal, so a caller that trusted it concluded a dissolution while the group was
    /// alive — and listed, with its remaining members, two fields away in the same reply. Both
    /// halves are asserted here; the first one fails on the previous code.
    #[test]
    fn command_group_remove_returns_the_survivor_and_null_only_on_dissolution() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project_id, ids) = project_with_commands(&db, 4, "/tmp/cg-remove");

        let created = manage_command_group(
            &db,
            &json!({ "action": "create", "project_id": project_id, "command_ids": ids }),
        )
        .unwrap();
        let group_id = created.result["group"]["id"].as_str().unwrap().to_string();

        // A group of FOUR losing one SURVIVES: the reply carries it, with its three remaining
        // members in disposition order.
        let removed =
            manage_command_group(&db, &json!({ "action": "remove", "command_id": ids[3] }))
                .unwrap();
        assert_eq!(
            removed.result["group"]["id"],
            json!(group_id),
            "the surviving group is named, not nulled"
        );
        assert_eq!(
            removed.result["group"]["command_ids"],
            json!([ids[0], ids[1], ids[2]]),
            "with its remaining members, in order"
        );
        // …and it agrees with the project's list in the very same reply.
        assert_eq!(removed.result["groups"], json!([removed.result["group"]]));

        // Down to two, still alive.
        let two = manage_command_group(&db, &json!({ "action": "remove", "command_id": ids[2] }))
            .unwrap();
        assert_eq!(two.result["group"]["command_ids"], json!([ids[0], ids[1]]));

        // The SECOND-TO-LAST removal dissolves it — THAT is when `group` is null.
        let gone = manage_command_group(&db, &json!({ "action": "remove", "command_id": ids[1] }))
            .unwrap();
        assert_eq!(
            gone.result["group"],
            Value::Null,
            "the group really is gone"
        );
        assert_eq!(gone.result["groups"], json!([]));

        // A command that was already SOLO has no group to report either.
        let solo = manage_command_group(&db, &json!({ "action": "remove", "command_id": ids[0] }))
            .unwrap();
        assert_eq!(solo.result["group"], Value::Null);
    }

    /// NON-REGRESSION: the five other actions still answer with their group, unchanged.
    #[test]
    fn the_other_command_group_actions_still_return_their_group() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project_id, ids) = project_with_commands(&db, 3, "/tmp/cg-others");

        let created = manage_command_group(
            &db,
            &json!({ "action": "create", "project_id": project_id, "command_ids": [ids[0], ids[1]] }),
        )
        .unwrap();
        let group_id = created.result["group"]["id"].as_str().unwrap().to_string();
        assert_eq!(
            created.result["group"]["command_ids"],
            json!([ids[0], ids[1]])
        );

        let added = manage_command_group(
            &db,
            &json!({ "action": "add", "group_id": group_id, "command_id": ids[2] }),
        )
        .unwrap();
        assert_eq!(added.result["group"]["command_ids"], json!(ids));

        let named = manage_command_group(
            &db,
            &json!({ "action": "name", "group_id": group_id, "name": "build" }),
        )
        .unwrap();
        assert_eq!(named.result["group"]["name"], json!("build"));

        let coloured = manage_command_group(
            &db,
            &json!({ "action": "color", "group_id": group_id, "color": 3 }),
        )
        .unwrap();
        assert_eq!(coloured.result["group"]["color"], json!(3));

        let laid_out = manage_command_group(
            &db,
            &json!({ "action": "layout", "group_id": group_id, "command_ids": [ids[2], ids[0], ids[1]] }),
        )
        .unwrap();
        assert_eq!(
            laid_out.result["group"]["command_ids"],
            json!([ids[2], ids[0], ids[1]])
        );

        let dissolved =
            manage_command_group(&db, &json!({ "action": "dissolve", "group_id": group_id }))
                .unwrap();
        assert_eq!(dissolved.result["group"], Value::Null);
        assert_eq!(dissolved.result["groups"], json!([]));
    }

    /// THE GROUPING IS READABLE WITHOUT WRITING (review finding `01M11XM66VTB30VCZRA0RMPMRP`).
    ///
    /// `manage_command_group`'s description sends agents to `list_commands(project_id=…)` to
    /// inspect the state; that surface carried no membership at all, so the only way to observe
    /// the grouping was to provoke a mutation and read the `groups` of its reply. This asserts the
    /// read surface exists, carries everything the criterion names, and AGREES field-for-field
    /// with what a mutation returns.
    #[test]
    fn list_commands_exposes_the_grouping_without_a_write() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let (project_id, ids) = project_with_commands(&db, 3, "/tmp/cg-read");

        // BEFORE any grouping: the surface already answers — an empty list, and solo commands.
        let bare = list_commands(&db, &runner, &json!({ "project_id": project_id })).unwrap();
        assert_eq!(bare["groups"], json!([]));
        for entry in bare["commands"].as_array().unwrap() {
            assert_eq!(
                entry["group_id"],
                Value::Null,
                "every command reads as solo"
            );
        }

        let created = manage_command_group(
            &db,
            &json!({ "action": "create", "project_id": project_id, "command_ids": [ids[0], ids[1]] }),
        )
        .unwrap();
        manage_command_group(
            &db,
            &json!({ "action": "name", "group_id": created.result["group"]["id"], "name": "build" }),
        )
        .unwrap();
        let mutated = manage_command_group(
            &db,
            &json!({ "action": "color", "group_id": created.result["group"]["id"], "color": 4 }),
        )
        .unwrap();

        // THE READ, with no write of its own.
        let read = list_commands(&db, &runner, &json!({ "project_id": project_id })).unwrap();
        // Id, name, colour and the members IN DISPOSITION ORDER — the whole criterion, in one row.
        let group = &read["groups"][0];
        assert_eq!(group["id"], mutated.result["group"]["id"]);
        assert_eq!(group["name"], json!("build"));
        assert_eq!(group["color"], json!(4));
        assert_eq!(group["command_ids"], json!([ids[0], ids[1]]));
        // …and the two surfaces cannot describe two different worlds: same rows, same reader.
        assert_eq!(read["groups"], mutated.result["groups"]);

        // Membership is readable per command too — `null` is SOLO, never "unknown".
        let by_id: std::collections::HashMap<&str, &Value> = read["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["command_id"].as_str().unwrap(), c))
            .collect();
        assert_eq!(by_id[ids[0].as_str()]["group_id"], group["id"]);
        assert_eq!(by_id[ids[1].as_str()]["group_id"], group["id"]);
        assert_eq!(by_id[ids[2].as_str()]["group_id"], Value::Null);

        // NOTHING MOVED: reading twice leaves the state exactly where it was.
        let again = list_commands(&db, &runner, &json!({ "project_id": project_id })).unwrap();
        assert_eq!(again, read);
    }

    /// THE EIGHTH ACTION, END TO END (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding
    /// 01M11XTRXVVPZ9XMDW5ARWKWHA). The agent can CHOOSE the tiling shape — the symmetry rule
    /// (#90) requires it the moment the hand can — and the read surface reports it back without a
    /// write, so an agent inspecting a project sees the shape a user picked.
    #[test]
    fn the_shape_action_persists_and_the_read_surface_reports_it() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let (project_id, ids) = project_with_commands(&db, 3, "/tmp/cg-shape");

        let created = manage_command_group(
            &db,
            &json!({ "action": "create", "project_id": project_id, "command_ids": ids }),
        )
        .unwrap();
        let group_id = created.result["group"]["id"].as_str().unwrap().to_string();
        assert_eq!(
            created.result["group"]["layout_shape"],
            json!("auto"),
            "a group is born with the DERIVED shape"
        );

        let shaped = manage_command_group(
            &db,
            &json!({ "action": "shape", "group_id": group_id, "shape": "column" }),
        )
        .unwrap();
        assert_eq!(shaped.result["group"]["layout_shape"], json!("column"));
        assert_eq!(
            shaped.result["group"]["command_ids"],
            json!(ids),
            "a re-shape is not a re-order"
        );
        // Mutating → the sidebar/settings repaint on `commands://changed`, like every other action.
        assert_eq!(shaped.effects, vec![ChangedTopic::Commands]);

        // The READ surface carries it, from the very reader the mutation echoes.
        let read = list_commands(&db, &runner, &json!({ "project_id": project_id })).unwrap();
        assert_eq!(read["groups"][0]["layout_shape"], json!("column"));
        assert_eq!(read["groups"], shaped.result["groups"]);

        // A shape outside the closed vocabulary is `invalid_argument`, and names the vocabulary.
        let refused = manage_command_group(
            &db,
            &json!({ "action": "shape", "group_id": group_id, "shape": "diagonal" }),
        )
        .expect_err("an unknown shape is refused");
        assert_eq!(refused.code, RpcCode::InvalidArgument);
        assert!(refused.message.contains("main-top"), "{}", refused.message);
        // …and it changed nothing.
        let after = list_commands(&db, &runner, &json!({ "project_id": project_id })).unwrap();
        assert_eq!(after["groups"][0]["layout_shape"], json!("column"));
    }

    /// THE ORDER OF THE COMMAND LIST IS WRITABLE BY THE AGENT TOO (finding
    /// 01M11XTRXVVPZ9XMDW5ARWKWHA). It is the half that made a drag "work only halfway": with no
    /// writable order, a command taken out of a group fell back to its natural position instead of
    /// staying where it was dropped. A partial list is REFUSED — positions are absolute, so
    /// re-numbering a subset scrambles the rest.
    #[test]
    fn reorder_commands_permutes_the_whole_list_and_refuses_a_partial_one() {
        let _g = db_guard();
        let db = Db::in_memory();
        let runner = noop_runner();
        let (project_id, ids) = project_with_commands(&db, 3, "/tmp/cg-reorder");

        let names = |v: &Value| -> Vec<String> {
            v["commands"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["command_id"].as_str().unwrap().to_string())
                .collect()
        };
        let listed = list_commands(&db, &runner, &json!({ "project_id": project_id })).unwrap();
        assert_eq!(names(&listed), ids, "creation order to begin with");

        let wanted = vec![ids[2].clone(), ids[0].clone(), ids[1].clone()];
        let reordered = reorder_commands(
            &db,
            &json!({ "project_id": project_id, "command_ids": wanted }),
        )
        .unwrap();
        assert_eq!(
            reordered.result["commands"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["command_id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
            wanted,
            "the reply echoes the NEW order"
        );
        assert_eq!(reordered.effects, vec![ChangedTopic::Commands]);
        // …and it persisted: the read surface every other caller uses agrees.
        let after = list_commands(&db, &runner, &json!({ "project_id": project_id })).unwrap();
        assert_eq!(names(&after), wanted);

        // A PARTIAL list is refused by name rather than silently scrambling the rest.
        let refused = reorder_commands(
            &db,
            &json!({ "project_id": project_id, "command_ids": [ids[0]] }),
        )
        .expect_err("a partial list is refused");
        assert_eq!(refused.code, RpcCode::InvalidArgument);
        // A list holding a STRANGER is refused too (it is not the same set).
        assert_eq!(
            reorder_commands(
                &db,
                &json!({ "project_id": project_id, "command_ids": [ids[0], ids[1], "not-mine"] }),
            )
            .expect_err("a foreign id is refused")
            .code,
            RpcCode::InvalidArgument
        );
        // Neither refusal moved anything.
        let still = list_commands(&db, &runner, &json!({ "project_id": project_id })).unwrap();
        assert_eq!(names(&still), wanted);
    }
}
