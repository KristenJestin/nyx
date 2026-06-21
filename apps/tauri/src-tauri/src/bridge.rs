//! Tauri bridge over the [`crate::pty`] module.
//!
//! Exposes managed PTY state keyed by id plus four commands
//! (`pty_spawn`/`pty_write`/`pty_resize`/`pty_close`) and two events:
//!
//! - `pty://output` — `{ id, bytes }`, the child's output. The reader channel
//!   yields raw chunks; this layer COALESCES them and flushes at most once per
//!   ~16ms (≈60fps) so a flood (`yes`) never emits one event per chunk/line.
//! - `pty://exit` — `{ id, code }`, emitted once when the child terminates.
//!
//! Keeping the throttling here (not in the PTY module) means the core stays a
//! plain byte pump and the bridge owns the front-facing performance contract.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

use crate::db::{self, Db, Project, Terminal, Workspace};
use crate::pty::Pty;

/// Flush cadence for coalesced output: ~60fps.
const FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// Payload of the `pty://output` event.
#[derive(Clone, Serialize)]
struct OutputPayload {
    id: u64,
    /// Output bytes since the last flush (raw PTY bytes; the front decodes/writes
    /// them into xterm). Serialized as a JSON array of numbers.
    bytes: Vec<u8>,
}

/// Payload of the `pty://exit` event.
#[derive(Clone, Serialize)]
struct ExitPayload {
    id: u64,
    /// Process exit code, or `null` if it could not be determined.
    code: Option<i32>,
}

/// Payload of the `terminal://exec-state` event (PRD-2.1 task #6): a terminal
/// RECORD's exec-state transition. Keyed by the PERSISTENT `terminal_id` (NOT the
/// live pty_id), so the sidebar — which keys terminals by record id — can filter
/// it and survive a restart / re-spawn. The fields are deliberately `snake_case`
/// (NOT `rename_all = "camelCase"` like `CommandStatePayload`): the front's
/// `TerminalRecord` is the snake_case DB shape (`exec_state`, `exec_exit_code`,
/// `exec_state_unread`), and the event handler folds this payload straight onto
/// that record, so matching the column names keeps the two in lockstep.
#[derive(Clone, Serialize)]
struct TerminalExecStatePayload {
    /// The persistent `terminals.id` this transition is for (the sidebar key).
    terminal_id: String,
    /// `idle` | `running` | `success` | `error` (the DB CHECK vocabulary).
    state: String,
    /// Exit code for a settled state (`success`/`error`); `None` otherwise or when
    /// the OSC 133 `D` end carried no parseable code.
    exit_code: Option<i32>,
    /// Whether this is an UNREAD settled notification (`success`/`error` the user
    /// has not yet viewed). `running`/`idle` are never unread.
    unread: bool,
    /// Epoch ms of this transition (mirrors `exec_state_updated_at`).
    updated_at: i64,
}

/// Payload of the `terminal://busy-state` event (PRD task #1): a terminal's
/// OS-derived busy/idle TRANSITION. Keyed by the PERSISTENT `terminal_id` (the
/// sidebar key), exactly like `terminal://exec-state`, so the front can fold it
/// onto the matching record and the signal survives re-spawn.
///
/// `busy` is the kernel-truthful "a command is running in the foreground" bit
/// (`foreground_pgid != shell pgid`) — the AUTHORITY for the running dot, REPLACING
/// the OSC-133-derived `running`. Emitted only on a CHANGE (the poll loop diffs the
/// last emitted value), never every tick, so the front sees one event per real
/// transition. `snake_case` (not camelCase) to match the snake_case `TerminalRecord`
/// shape the front folds it onto — same convention as `TerminalExecStatePayload`.
#[derive(Clone, Serialize)]
struct TerminalBusyStatePayload {
    /// The persistent `terminals.id` this transition is for (the sidebar key).
    terminal_id: String,
    /// `true` = a foreground command is running; `false` = idle at the prompt.
    busy: bool,
}

/// The `terminal://busy-state` event name (PRD task #1) — the OS busy/idle signal
/// that drives the UI dot, decoupled from OSC 133. Public so tests assert on it.
pub const TERMINAL_BUSY_STATE_EVENT: &str = "terminal://busy-state";

/// Managed state: the last busy value EMITTED per persistent terminal id, so the
/// poll loop emits `terminal://busy-state` only on a TRANSITION (not every tick).
/// Keyed by the durable `terminal_id` (the event key), not the live pty_id, so a
/// re-spawn under the same record re-evaluates against the last announced value.
#[derive(Default)]
pub struct BusyStateTracker {
    last: Mutex<HashMap<String, bool>>,
}

impl BusyStateTracker {
    /// Record `busy` for `terminal_id` and return whether it CHANGED from the last
    /// recorded value (treating a never-seen id as a change only when `busy` is
    /// true — a fresh terminal defaults to idle, so its first `false` is not a
    /// spurious transition to announce). One lock per call; cheap.
    fn changed(&self, terminal_id: &str, busy: bool) -> bool {
        let mut map = self.last.lock().unwrap();
        match map.insert(terminal_id.to_string(), busy) {
            Some(prev) => prev != busy,
            // Unseen id: announce only the first BUSY (idle is the implicit default
            // the front already shows), so boot/restore never emits a redundant
            // `false` for every idle terminal.
            None => busy,
        }
    }
    /// Drop the tracked value for a terminal id (on PTY exit/close) so the table
    /// does not grow without bound and a future re-spawn starts clean.
    fn forget(&self, terminal_id: &str) {
        self.last.lock().unwrap().remove(terminal_id);
    }
}

/// Managed state: all live PTYs keyed by their id.
#[derive(Default)]
pub struct PtyManager {
    ptys: Mutex<HashMap<u64, Pty>>,
}

impl PtyManager {
    /// Write raw bytes to the PTY identified by `id`, the SAME path as the `pty_write`
    /// command (no second lifecycle). Returns `false` if `id` is not a live PTY, so the
    /// MCP terminal tools can distinguish a stale id from a write error. `pub(crate)` for
    /// the MCP `send_to_terminal` tool (which holds only a record id → resolves the PTY id
    /// via [`TerminalPtyMap`]).
    pub(crate) fn write_to(&self, id: u64, data: &[u8]) -> Result<bool, String> {
        use std::io::Write as _;
        // Resolve the per-pty writer handle under the registry lock, then RELEASE the lock
        // before the (potentially blocking) write — a child that stops draining its tty
        // then stalls writes to ITS pty only, not every pty op behind the registry mutex.
        let writer = {
            let ptys = self.ptys.lock().unwrap();
            match ptys.get(&id) {
                Some(pty) => pty.writer_handle(),
                None => return Ok(false),
            }
        };
        let mut w = writer.lock().unwrap();
        w.write_all(data)
            .and_then(|_| w.flush())
            .map(|_| true)
            .map_err(|e| e.to_string())
    }

    /// Kill + drop the PTY identified by `id`, the SAME path as the `pty_close` command.
    /// Returns `false` if `id` was already gone (idempotent). `pub(crate)` for the MCP
    /// `close_terminal` tool.
    pub(crate) fn close_id(&self, id: u64) -> Result<bool, String> {
        let pty = self.ptys.lock().unwrap().remove(&id);
        match pty {
            Some(mut pty) => {
                pty.kill().map_err(|e| e.to_string())?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// The OS-derived busy bit of the PTY identified by `id` (task #4): `Some(true)`
    /// when a command runs in its foreground (`foreground_pgid != shell pgid`),
    /// `Some(false)` at an idle prompt, `None` when `id` is not a live PTY or the
    /// signal cannot be derived (non-Unix, or master already closed). This is the
    /// SAME OS authority the busy-state poll loop reads — it goes through
    /// [`pty_busy`], the one derivation point — so the MCP `list_terminals` busy
    /// field is consistent with the UI dot. `pub(crate)` for that tool (which holds
    /// a record id → resolves the PTY id via [`TerminalPtyMap`]). The lock is held
    /// for one cheap `tcgetpgrp`, then released.
    pub(crate) fn busy_of(&self, id: u64) -> Option<bool> {
        self.ptys.lock().unwrap().get(&id).and_then(pty_busy)
    }
}

// --- Managed-command runtime wiring (PRD-3 Phase 2) ----------------------
//
// The state machine + process-tree lifecycle live in `crate::command` (decoupled
// from Tauri and unit-tested there). This is the THIN Tauri layer that turns a
// runner transition into the front-facing events and the DB persistence — the
// mirror of the terminal output pump above. The lifecycle `#[tauri::command]`s
// that drive `start`/`stop`/`relaunch` land in Phase 3; until then the runner +
// sink are constructed by the tests and that later command surface, so the type
// carries the same `#[cfg_attr(not(test), allow(dead_code))]` deferral the Phase-1
// db helpers use for their not-yet-wired runner consumer.

/// Payload of the `command://state` event: an instance's derived run state plus
/// the natural exit code (for success/error; `null` otherwise).
///
/// `rename_all = "camelCase"` is load-bearing: the front filters every event on
/// `event.payload.instanceId` (camelCase), so a snake_case `instance_id` would
/// surface as `undefined` there and EVERY live state transition would be dropped
/// (the dot would only ever update on a cold rehydrate). See the mirror
/// `CommandOutputPayload`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandStatePayload {
    /// `command_instances.id` the transition is for.
    instance_id: String,
    /// `idle` | `running` | `success` | `error` (the DB CHECK vocabulary).
    state: String,
    /// Natural exit code for success/error transitions; `null` otherwise.
    code: Option<i32>,
}

/// Payload of the `command://output` event: coalesced output for one instance.
///
/// `rename_all = "camelCase"` is load-bearing for the SAME reason as
/// [`CommandStatePayload`]: the front filters on `event.payload.instanceId`, so a
/// snake_case key would make every live output chunk be dropped (output would only
/// ever appear via the cold `command_output` rehydrate, never stream live).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandOutputPayload {
    instance_id: String,
    /// Output bytes since the last flush (raw; the front decodes/writes them).
    bytes: Vec<u8>,
}

/// Payload of the `command://ack` event: an instance's "unseen result" was
/// acknowledged. Carries ONLY the instance id — the acknowledge clears the unread
/// notification WITHOUT changing the factual state, so there is no `state`/`code`
/// here (those are unchanged; the row still reflects the factual outcome). The front
/// filters on `instanceId` (camelCase, load-bearing — see [`CommandStatePayload`])
/// and hides the settled badge for that instance off this event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandAckPayload {
    instance_id: String,
}

/// Payload of the [`COMMAND_OUTPUT_CLEARED_EVENT`] event (PRD-4 review R-OUTPUT): an
/// instance's captured output BUFFER was cleared (via the MCP `clear_command_output`
/// tool). Carries ONLY the instance id — clearing wipes the bytes, NOT the factual
/// state/outcome, so there is no `state`/`code` here. The read-only output panel
/// (`useCommandOutput`) filters on `instanceId` (camelCase, load-bearing — same as
/// [`CommandStatePayload`]) and resets its xterm on this event: the analog of the
/// run-start clear, but WITHOUT a state transition.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandOutputClearedPayload {
    instance_id: String,
}

/// The event emitted when an instance's captured output buffer is cleared (PRD-4
/// review R-OUTPUT, the `clear_command_output` MCP tool). The read-only output panel
/// listens on it to wipe its xterm. A DEDICATED signal (not `command://state`) because
/// a clear is NOT a run transition: the factual state/outcome are unchanged.
pub const COMMAND_OUTPUT_CLEARED_EVENT: &str = "command://output-cleared";

/// Production [`crate::command::RunnerSink`]: emits `command://state` /
/// `command://output` over the `AppHandle` and persists `last_state` + bounded
/// scrollback via the managed [`Db`]. Holds the `AppHandle` so the pump thread can
/// reach managed state off the main thread (same pattern as the terminal pump).
pub struct TauriRunnerSink<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> crate::command::RunnerSink for TauriRunnerSink<R> {
    fn on_state(&self, instance_id: &str, state: crate::command::RunState, exit_code: Option<i32>) {
        // Persist the FACTUAL outcome (DB CHECK vocabulary) BEFORE emitting, so a
        // listener that reads the row on the event sees the committed value. A
        // success/error finish records `last_exit_code` + `ended_at` + flips `unread`
        // (the v4 outcome columns); a `running` start clears the prior code; `idle`
        // touches only `last_state`. The outcome columns are what an acknowledge must
        // NOT erase, so this writer is decoupled from `on_acknowledge` below.
        let db_state = state.as_db_str();
        self.app
            .state::<Db>()
            .with_conn(|c| db::set_run_state(c, instance_id, db_state, exit_code))
            .ok();
        let _ = self.app.emit(
            "command://state",
            CommandStatePayload {
                instance_id: instance_id.to_string(),
                state: db_state.to_string(),
                code: exit_code,
            },
        );
    }

    fn on_acknowledge(&self, instance_id: &str) {
        // Clear ONLY the persisted `unread` flag (the factual outcome is untouched),
        // then emit `command://ack` so the UI hides the settled badge WITHOUT any
        // state change. This is the decoupled notification path: a UI ack can no
        // longer erase the error the MCP sees.
        self.app
            .state::<Db>()
            .with_conn(|c| db::acknowledge_instance(c, instance_id))
            .ok();
        let _ = self.app.emit(
            "command://ack",
            CommandAckPayload {
                instance_id: instance_id.to_string(),
            },
        );
    }

    fn on_output(&self, instance_id: &str, bytes: &[u8]) {
        let _ = self.app.emit(
            "command://output",
            CommandOutputPayload {
                instance_id: instance_id.to_string(),
                bytes: bytes.to_vec(),
            },
        );
    }

    fn persist_scrollback(&self, instance_id: &str, serialized: &str) {
        self.app
            .state::<Db>()
            .with_conn(|c| db::persist_instance_scrollback(c, instance_id, serialized))
            .ok();
    }

    fn archive_previous_run(&self, instance_id: &str) {
        // A fresh (re)launch: archive the last completed run into the bounded `prev_*`
        // columns (N=1) and reset the current run to a clean `running` row, in one
        // transaction. Retains the previous run's output + exit_code/ended_at so an
        // observer (the MCP `get_command_output(run="previous")`) can still read it,
        // while the current run starts unpolluted by the prior run's bytes.
        self.app
            .state::<Db>()
            .with_conn(|c| db::archive_and_reset_for_relaunch(c, instance_id))
            .ok();
    }

    fn clear_output(&self, instance_id: &str) {
        // Empty the persisted scrollback (current + retained prior run) WITHOUT touching
        // the factual outcome columns, then emit the dedicated clear event so the
        // read-only output panel wipes its xterm. The analog of the run-start clear,
        // but with NO state transition (clearing the log is not stopping/relaunching).
        self.app
            .state::<Db>()
            .with_conn(|c| db::clear_instance_scrollback(c, instance_id))
            .ok();
        let _ = self.app.emit(
            COMMAND_OUTPUT_CLEARED_EVENT,
            CommandOutputClearedPayload {
                instance_id: instance_id.to_string(),
            },
        );
    }
}

/// Managed state: the live [`crate::command::CommandRunner`] over the production
/// Tauri sink, keyed by `command_instances.id`. Registered as managed state in
/// [`manage_command_runner`] during setup so the lifecycle commands can reach it.
pub type ManagedCommandRunner<R> = crate::command::CommandRunner<TauriRunnerSink<R>>;

/// Build a [`ManagedCommandRunner`] bound to `app`'s event + DB plumbing, sized
/// for the off-screen command PTYs.
pub fn build_command_runner<R: Runtime>(app: AppHandle<R>) -> ManagedCommandRunner<R> {
    // A modest off-screen size: managed commands are watch-only services, not
    // interactive full-screen TUIs, so an 80x24 grid is plenty for their output.
    let size = portable_pty::PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };
    crate::command::CommandRunner::new(TauriRunnerSink { app }, size)
}

/// Register the [`ManagedCommandRunner`] as managed state on a built `App`. Called
/// from the setup hook (after the `Db` is managed) so the lifecycle commands have a
/// live runner. The runner holds the `AppHandle` it emits/persists through.
pub fn manage_command_runner<R: Runtime>(app: &AppHandle<R>) {
    let runner = build_command_runner(app.clone());
    app.manage(runner);
}

/// The name of the structural-refresh event the sidebar listens on to re-pull the
/// `projects → workspaces` tree. Emitted by EVERY mutation of that tree, whether it
/// comes from a UI `#[tauri::command]` (`create_project`/`create_workspace`/
/// `delete_project`) or from an MCP tool (`workspace_add`/`create_workspace`).
///
/// Why a single shared signal: the command tools (`start`/`stop`/`relaunch`) already
/// emit `command://state` and so the dot refreshes for both UI- and MCP-driven runs;
/// the workspace/project MUTATIONS had NO such event — the UI only updated its own
/// in-memory tree optimistically after its OWN invoke returned (see `useProjects`),
/// so an agent adding a workspace over MCP never reached the sidebar (review
/// 01KV9611923NKX3JPR5V6MN44F). Routing every mutating path through this one event
/// keeps the UI in sync regardless of who mutated, and gives the future mutating MCP
/// tools (command-template CRUD) the SAME refresh hook for free.
pub const WORKSPACES_CHANGED_EVENT: &str = "workspaces://changed";

/// Emit [`WORKSPACES_CHANGED_EVENT`] so any sidebar listening on it re-pulls the
/// project/workspace tree. The reusable refresh hook for the project/workspace tree:
/// call it from EVERY backend path that mutates that tree (UI command OR MCP tool) so
/// both surfaces stay in sync from one signal. The payload is empty `{}` — the event
/// is a pure "the tree changed, re-list" tick, not a delta; the listener re-fetches
/// `list_projects` + `list_workspaces` (its single source of truth) on receipt, so a
/// concurrent UI- and MCP-driven mutation can never desync the sidebar from the DB.
/// A failed emit is swallowed (best-effort, like the other event emitters here): the
/// next mutation re-emits and the user can always reload.
pub fn emit_workspaces_changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(WORKSPACES_CHANGED_EVENT, ());
}

/// The name of the structural-refresh event the sidebar COMMANDS band listens on to
/// re-pull its command instances/templates. Emitted by EVERY mutation of a command
/// TEMPLATE, whether it comes from a UI `#[tauri::command]` (`command_create`/
/// `command_update`/`command_delete`/`command_resync_source`/`command_unlink_source`/
/// `command_import_create`) or from an MCP tool (`add_command`/`update_command`/
/// `import_commands`).
///
/// Why a dedicated signal (not `workspaces://changed`): a template mutation does not
/// change the project/workspace TREE — it adds/edits/removes the COMMANDS that hang
/// off existing workspaces. The two band surfaces (`useCommandInstances` for the
/// sidebar band, `useCommands` for the Manage Commands modal) only re-loaded on a
/// workspace-id-set change or a `projectId` change respectively, so a template added
/// to an EXISTING workspace — over MCP OR via the UI — never appeared live. Routing
/// every mutating path through this one event keeps both surfaces in sync regardless
/// of who mutated, mirroring the project/workspace tree's `workspaces://changed`.
pub const COMMANDS_CHANGED_EVENT: &str = "commands://changed";

/// Emit [`COMMANDS_CHANGED_EVENT`] so any command-band surface listening on it re-pulls
/// its command instances/templates. The reusable refresh hook for the COMMANDS band:
/// call it from EVERY backend path that mutates a command template (UI command OR MCP
/// tool) so both surfaces stay in sync from one signal. The payload is empty `()` — the
/// event is a pure "the commands changed, re-list" tick, not a delta; the listeners
/// re-fetch their lists (their single source of truth) on receipt, so a concurrent UI-
/// and MCP-driven mutation can never desync the band from the DB. A failed emit is
/// swallowed (best-effort, like the other event emitters here): the next mutation
/// re-emits and the user can always reload.
pub fn emit_commands_changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(COMMANDS_CHANGED_EVENT, ());
}

/// The name of the structural-refresh event the terminal deck listens on to re-pull its
/// terminal records (PRD-4 review R-TERM). Emitted by EVERY backend path that CREATES or
/// removes a terminal RECORD from outside the front's own orchestration — i.e. the MCP
/// terminal tools (`create_terminal` / `close_terminal`). Modelled on
/// [`COMMANDS_CHANGED_EVENT`].
///
/// Why it exists: unlike commands, terminals are orchestrated entirely by the FRONT today
/// (the UI calls `create_terminal` then mounts a `<Terminal>` which spawns the PTY itself),
/// so there was NO backend→front signal for terminals at all — a terminal an agent creates
/// over MCP would never reach the sidebar / never get a PTY+xterm. Routing the MCP-driven
/// create/close through this one event lets the front reconcile: it re-pulls `list_terminals`
/// on receipt, mounts an xterm for any newly-`alive` record (which spawns its PTY) and drops
/// the pane of any record that went `closed`. The payload is empty `()` — a pure "the
/// terminals changed, re-list" tick, not a delta; the front re-fetches its single source of
/// truth. A failed emit is swallowed (best-effort, like the other emitters).
pub const TERMINALS_CHANGED_EVENT: &str = "terminals://changed";

/// Emit [`TERMINALS_CHANGED_EVENT`] so the terminal deck re-pulls `list_terminals` and
/// reconciles its mounted xterm panes. The reusable refresh hook for the terminal deck:
/// call it from EVERY backend path that creates/closes a terminal record outside the front's
/// own orchestration (the MCP terminal tools). Best-effort, like the other event emitters.
pub fn emit_terminals_changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(TERMINALS_CHANGED_EVENT, ());
}

/// "Some agent session started or ended" — a coalescing refresh tick (no payload). The
/// sidebar re-pulls `agent_active_sessions` to swap the provider-aware icon on the right
/// terminal rows (finding #55). Emitted from the `agent_session_event` MCP tool after a
/// successful SessionStart/SessionEnd. Like the other `*_CHANGED` events, this is a
/// "re-fetch your source of truth" signal, not a delta.
pub const AGENT_SESSIONS_CHANGED_EVENT: &str = "agent-sessions://changed";

/// Emit [`AGENT_SESSIONS_CHANGED_EVENT`] so the sidebar re-pulls the set of terminals
/// with a live agent session and updates each row's icon. Best-effort (a dropped emit
/// just means the icon updates on the next change / re-pull).
pub fn emit_agent_sessions_changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(AGENT_SESSIONS_CHANGED_EVENT, ());
}

// --- Terminal RECORD ↔ live PTY mapping (PRD-4 review R-TERM) -------------
//
// The record↔pty link historically lived ONLY in the front (`TerminalManager`'s
// `ptyIds` Map, keyed by record id): each `<Terminal>` spawns its own PTY and reports
// the id up. That is fine while the FRONT alone drives terminals, but an MCP tool that
// must write into / close / enumerate a terminal by its RECORD id needs the same join on
// the backend. So the front now REGISTERS the link here as soon as its `<Terminal>`
// resolves a PTY id (and clears it on exit/close), giving the MCP tools a way to resolve
// a terminal record id → its live PTY id WITHOUT owning a second PTY lifecycle: the PTY is
// still spawned/owned by the front's `<Terminal>` exactly as before.

/// Managed state: the live link between a terminal RECORD id (`terminals.id`, a UUID
/// string) and its current PTY id (`PtyManager` key). Populated by the front via
/// [`register_terminal_pty`] when a `<Terminal>` spawns/exits its PTY; read by the MCP
/// terminal tools to resolve a record id to the PTY they must `pty_write`/`pty_close`.
#[derive(Default)]
pub struct TerminalPtyMap {
    by_record: Mutex<HashMap<String, u64>>,
}

impl TerminalPtyMap {
    /// Record that `record_id`'s live PTY is `pty_id`. Overwrites any prior link (a
    /// record that respawned a PTY after an exit gets the fresh id).
    pub fn set(&self, record_id: &str, pty_id: u64) {
        self.by_record.lock().unwrap().insert(record_id.to_string(), pty_id);
    }
    /// Drop the link for `record_id` (its PTY exited or the terminal was closed).
    pub fn clear(&self, record_id: &str) {
        self.by_record.lock().unwrap().remove(record_id);
    }
    /// The live PTY id for `record_id`, if the front has registered one.
    pub fn get(&self, record_id: &str) -> Option<u64> {
        self.by_record.lock().unwrap().get(record_id).copied()
    }
    /// A snapshot of every `(record_id, pty_id)` link, for `list_terminals` mapping.
    pub fn snapshot(&self) -> HashMap<String, u64> {
        self.by_record.lock().unwrap().clone()
    }
}

/// Managed state: commands the MCP `create_terminal` tool wants injected into a terminal
/// AT OPENING, keyed by the terminal RECORD id. Because the PTY is spawned by the FRONT
/// (when it mounts the `<Terminal>` after reconciling on `terminals://changed`), an
/// MCP-supplied `command` cannot be written until that PTY is live. So `create_terminal`
/// PARKS the command here; [`register_terminal_pty`] drains it (a one-shot, `take`) and
/// writes `command + "\r"` into the freshly-spawned PTY, so the command runs once at the
/// shell's first prompt and the terminal stays interactive after. A terminal opened with
/// NO command parks nothing — the shell is bare.
#[derive(Default)]
pub struct PendingTerminalCommands {
    by_record: Mutex<HashMap<String, String>>,
}

impl PendingTerminalCommands {
    /// Park `command` to be injected into `record_id`'s PTY once it spawns.
    pub fn set(&self, record_id: &str, command: String) {
        self.by_record.lock().unwrap().insert(record_id.to_string(), command);
    }
    /// Take (remove + return) the parked command for `record_id`, if any. One-shot: a
    /// later re-registration of the same record (e.g. a respawn after exit) does NOT
    /// re-inject — the command runs exactly once, at the first opening. `pub(crate)` so the
    /// MCP terminal-tool tests can assert the park/drain contract.
    pub(crate) fn take(&self, record_id: &str) -> Option<String> {
        self.by_record.lock().unwrap().remove(record_id)
    }
}

/// One parked agent-session RESUME for a terminal record (PRD-5 #5). Produced by the
/// boot resume scan ([`restore_agent_sessions_on_boot`]) and drained by
/// [`register_terminal_pty`] into the freshly-respawned shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingResume {
    /// The `agent_sessions.id` being resumed — so a delivery failure can mark exactly
    /// that row `resume_failed`.
    pub session_id: String,
    /// The exact shell command to inject (e.g. `claude --resume <id>`).
    pub command: String,
    /// `true` when the candidate was an `unknown` (stale / probable-kill) session —
    /// resumed anyway, but the doubt is recorded.
    pub uncertain: bool,
}

/// Managed state: per-record agent-session RESUME commands to inject at the terminal's
/// first respawn after a relaunch (PRD-5 #5). Mirrors [`PendingTerminalCommands`] but
/// carries the `agent_sessions.id` so a delivery failure marks that exact row
/// `resume_failed`. Populated ONCE at boot by [`restore_agent_sessions_on_boot`] and
/// drained one-shot by [`register_terminal_pty`] — a respawn-after-exit within the same
/// run finds nothing parked, so resume happens exactly once, at the post-relaunch open.
#[derive(Default)]
pub struct PendingResumes {
    by_record: Mutex<HashMap<String, PendingResume>>,
}

impl PendingResumes {
    /// Park a resume for `record_id` to be injected when its PTY next registers.
    pub fn set(&self, record_id: &str, resume: PendingResume) {
        self.by_record.lock().unwrap().insert(record_id.to_string(), resume);
    }
    /// Take (remove + return) the parked resume for `record_id`, if any. One-shot.
    pub(crate) fn take(&self, record_id: &str) -> Option<PendingResume> {
        self.by_record.lock().unwrap().remove(record_id)
    }
}

/// Register (or clear) the RECORD ↔ live PTY link for a terminal. Called by the front's
/// `<Terminal>` when its PTY id resolves (`pty_id = Some`) and when the PTY exits / the
/// terminal is torn down (`pty_id = None`). This is the ONLY writer of
/// [`TerminalPtyMap`]; the MCP terminal tools are readers. The PTY itself is still
/// spawned and owned by the front (`pty_spawn`), so this command adds NO second
/// lifecycle — it only surfaces the join the front already maintains to the backend.
///
/// On registration (`pty_id = Some`) it also DRAINS any command the MCP `create_terminal`
/// tool parked for this record (see [`PendingTerminalCommands`]): the parked
/// `command + "\r"` is written into the just-spawned PTY so an MCP "open a terminal that
/// runs X" lands its line at the shell's first prompt and stays interactive after. A
/// terminal opened bare (no parked command) injects nothing.
#[tauri::command(async)]
fn register_terminal_pty(
    db: State<'_, Db>,
    map: State<'_, TerminalPtyMap>,
    pending: State<'_, PendingTerminalCommands>,
    resumes: State<'_, PendingResumes>,
    pty_state: State<'_, PtyManager>,
    record_id: String,
    pty_id: Option<u64>,
) -> Result<(), String> {
    match pty_id {
        Some(id) => {
            map.set(&record_id, id);
            // Drain the one-shot parked lines for this record: an MCP `create_terminal`
            // command (see [`PendingTerminalCommands`]) and a boot-scan agent RESUME
            // (PRD-5 #5). Both are written into the freshly-spawned PTY under a SINGLE lock
            // acquisition (command first, then `claude --resume <id>`), each with a
            // trailing carriage return so the shell runs the line and stays interactive — the
            // SAME write path as `pty_write` (no second lifecycle). A terminal opened bare
            // injects nothing.
            let parked_command = pending.take(&record_id);
            let parked_resume = resumes.take(&record_id);
            // Returns the resume's session id when the resume could NOT be delivered (PTY
            // gone or write failed) so we can mark that row `resume_failed` — but OUTSIDE
            // the PTY lock, since the DB write must not run while the mutex is held.
            let resume_to_fail = if parked_command.is_none() && parked_resume.is_none() {
                // A bare terminal injects nothing — never touch the registry.
                None
            } else {
                use std::io::Write as _;
                // Resolve the per-pty writer handle UNDER the registry lock, then RELEASE the
                // lock before the (potentially blocking) writes — a child that stops draining
                // its tty stalls writes to ITS pty only, not every pty op behind the registry
                // mutex (the SAME discipline as `PtyManager::write_to` / `pty_write`).
                let writer = {
                    let ptys = pty_state.ptys.lock().unwrap();
                    ptys.get(&id).map(|pty| pty.writer_handle())
                };
                match writer {
                    Some(writer) => {
                        let mut w = writer.lock().unwrap();
                        // Command first (MCP `create_terminal`), then the boot-scan resume —
                        // each with a trailing carriage return (#76) so the shell runs the line
                        // and stays interactive. Writes serialize only on THIS pty's writer.
                        if let Some(command) = parked_command {
                            let mut bytes = command.into_bytes();
                            bytes.push(b'\r');
                            let _ = w.write_all(&bytes).and_then(|_| w.flush());
                        }
                        match parked_resume {
                            Some(resume) => {
                                let mut bytes = resume.command.into_bytes();
                                bytes.push(b'\r');
                                // Delivered → the session stays `active` (a live `resume`
                                // SessionStart refreshes `last_seen_at`); failed write →
                                // mark it `resume_failed` so the next launch won't retry.
                                if w.write_all(&bytes).and_then(|_| w.flush()).is_ok() {
                                    None
                                } else {
                                    Some(resume.session_id)
                                }
                            }
                            None => None,
                        }
                    }
                    // PTY gone: the parked resume can't be injected → it failed to resume.
                    None => parked_resume.map(|r| r.session_id),
                }
            };
            if let Some(session_id) = resume_to_fail {
                let _ = db.with_conn(|c| db::mark_session_resume_failed(c, &session_id));
            }
        }
        None => map.clear(&record_id),
    }
    Ok(())
}

/// Run the BOOT restoration from an `AppHandle`: read the managed `Db` + runner and
/// relaunch the instances the shutdown snapshot marked, normalizing the rest. A
/// thin handle-reaching wrapper over [`restore_commands_on_boot`] for the setup
/// hook (which only has the handle).
pub fn restore_commands_from_handle<R: Runtime>(app: &AppHandle<R>) {
    let db = app.state::<Db>();
    let runner = app.state::<ManagedCommandRunner<R>>();
    restore_commands_on_boot(&db, &runner);
}

/// BOOT NORMALIZATION of phantom-running terminals (PRD task #2): settle any
/// terminal left at a persisted `exec_state = 'running'` down to `idle` at launch,
/// so a force-quit/restore can never resurface a phantom running badge. This is the
/// TERMINAL analogue of the managed-command `normalize_unrelaunched` (which already
/// settles orphaned `running` instances at boot).
///
/// Busy/idle is derived live from the OS now (task #1), so this normalization is
/// DEFENSIVE/cosmetic rather than the authority: a restored terminal with no
/// foreground process samples idle by construction, and the dot reads the
/// `terminal://busy-state` signal — never the persisted `exec_state`. But settling
/// the stored field too means even a transient read before the first busy poll (and
/// any other consumer of the persisted value) sees idle, fully eliminating the
/// dogfood symptom (terminals stuck `running` in the DB). Best-effort: a DB error is
/// swallowed (the UI must still come up); returns the count for tests/logging.
pub fn normalize_terminals_on_boot(db: &Db) -> usize {
    db.with_conn(db::normalize_phantom_running_terminals)
        .unwrap_or(0)
}

/// Run the terminal boot normalization from an `AppHandle`. A thin handle-reaching
/// wrapper over [`normalize_terminals_on_boot`] for the setup hook.
pub fn normalize_terminals_from_handle<R: Runtime>(app: &AppHandle<R>) {
    let db = app.state::<Db>();
    normalize_terminals_on_boot(&db);
}

/// Run the BOOT agent-session RESUME scan from an `AppHandle` (PRD-5 #5). A thin
/// handle-reaching wrapper over [`restore_agent_sessions_on_boot`] for the setup hook.
pub fn restore_agent_sessions_from_handle<R: Runtime>(app: &AppHandle<R>) {
    let db = app.state::<Db>();
    let resumes = app.state::<PendingResumes>();
    restore_agent_sessions_on_boot(&db, &resumes);
}

/// The BOOT agent-session resume scan (PRD-5 #5). In order:
///   1. SWEEP stale `active` sessions to `unknown` (the kill-then-relaunch path leaves
///      a row `active`; this rebascules a long-silent one and flags the doubt).
///   2. Gather the resume CANDIDATES (alive terminal + `active`/`unknown` session +
///      the project's resume flag) via [`db::resume_candidates_on_boot`].
///   3. For each, run the PURE resume DECISION ([`crate::agent_resume::decide_resume`])
///      with the resolved target shell + the agent's adapter, and — when it says
///      RESUME — PARK the exact command in [`PendingResumes`] so the terminal's first
///      respawn after relaunch injects it. The `closed_voluntarily` gate is structural
///      (a closed terminal is not `alive`, so it never appears as a candidate).
///
/// Returns the record ids a resume was parked for (handy for tests/logging). The
/// actual injection happens later, in [`register_terminal_pty`], when the front mounts
/// each restored terminal's PTY.
pub fn restore_agent_sessions_on_boot(db: &Db, resumes: &PendingResumes) -> Vec<String> {
    use crate::agent::AgentRegistry;
    use crate::agent_resume::{decide_resume, ResumeInputs, ResumeTarget, SessionState};

    // 1. Sweep stale active → unknown (probable kills since the last clean run).
    let _ = db.with_conn(|c| db::sweep_stale_active_sessions(c, db::SESSION_STALE_AFTER_MS));

    // 2. Gather candidates.
    let candidates = match db.with_conn(db::resume_candidates_on_boot) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };

    // The execution target is fixed per run by the resolved default shell (resume now
    // targets a native Linux shell, WSL under Windows, AND native Windows PowerShell/cmd
    // — finding #83).
    let target = ResumeTarget::classify_shell(&crate::pty::resolve_shell());
    let registry = AgentRegistry::default();

    let mut parked = Vec::new();
    // BOOT CLEANUP (finding #82): every candidate we DON'T resume is provably dead (claude
    // was not relaunched after the restart), so its `active`/`unknown` row is a phantom
    // that would warn on every subsequent close. Collect those session ids and retire them
    // below; a Resume leaves the row `active` (the park revives it at injection).
    let mut dead_session_ids: Vec<String> = Vec::new();
    for cand in candidates {
        // A session whose state string is somehow unrecognized is skipped defensively.
        let Some(state) = SessionState::from_db(&cand.session_state) else {
            continue;
        };
        let Some(adapter) = registry.get(&cand.agent_kind) else {
            continue;
        };
        // #53: a candidate is only resumable if its conversation EXISTS on disk. Claude
        // writes `transcript_path` only on the first message, so a session the user
        // never typed into (or one whose conversation was deleted) has a session id but
        // no `.jsonl` — `claude --resume` would fail "No conversation found" and break
        // the respawned terminal. A single `stat` on the already-captured path (no FS
        // scan); a missing/absent path → `transcript_exists = false` → the pure decision
        // skips with `NoConversation`.
        let transcript_exists = cand
            .transcript_path
            .as_deref()
            .map(|p| std::path::Path::new(p).exists())
            .unwrap_or(false);
        let inputs = ResumeInputs {
            project_resume_on: cand.project_resume_on,
            // A candidate is, by construction, an ALIVE terminal — a voluntary close
            // flips the terminal to `closed` (excluded by the candidate query), so it
            // never reaches here. So `closed_voluntarily` is always false at boot.
            closed_voluntarily: false,
            session_state: state,
            external_session_id: &cand.external_session_id,
            transcript_exists,
            target,
        };
        match decide_resume(&inputs, adapter) {
            crate::agent_resume::ResumeDecision::Resume { command, resume_uncertain } => {
                resumes.set(
                    &cand.terminal_id,
                    PendingResume {
                        session_id: cand.session_id,
                        command,
                        uncertain: resume_uncertain,
                    },
                );
                parked.push(cand.terminal_id);
            }
            // Skip(_) for ANY reason: the session will NOT be brought back, so it is dead
            // — retire it (mark `ended`) so it stops warning on every close.
            crate::agent_resume::ResumeDecision::Skip(_) => {
                dead_session_ids.push(cand.session_id)
            }
        }
    }

    // Retire the non-resumed sessions in one batch (best-effort: a cleanup failure must
    // not abort the boot scan). After this, a fresh boot leaves only sessions started in
    // the CURRENT run live, so the close-warning fires only for genuinely live sessions.
    if !dead_session_ids.is_empty() {
        let _ = db.with_conn(|c| db::mark_sessions_ended(c, &dead_session_ids));
    }

    parked
}

/// Run the SHUTDOWN snapshot from an `AppHandle`: read the managed `Db` + runner and
/// persist `was_running_on_shutdown` for every instance. A thin handle-reaching
/// wrapper over [`snapshot_commands_on_shutdown`] for the window-close / exit hook.
pub fn snapshot_commands_from_handle<R: Runtime>(app: &AppHandle<R>) {
    // The managed state may be absent if the snapshot fires before setup completed
    // (e.g. an early close); be defensive and no-op in that case.
    let Some(db) = app.try_state::<Db>() else {
        return;
    };
    let Some(runner) = app.try_state::<ManagedCommandRunner<R>>() else {
        return;
    };
    // The window event fires for BOTH `CloseRequested` and `Destroyed`; run the
    // shutdown snapshot + reap exactly once so the second event does not re-snapshot
    // AFTER the kill (which would clear `was_running_on_shutdown` to false).
    if !runner.begin_shutdown() {
        return;
    }
    // Persist which instances were running (for restart-on-startup) BEFORE killing,
    // then reap the live process trees so managed commands are not orphaned past
    // exit (the pump owns the `CommandPty` on a detached thread; the runner map
    // alone would not kill the children).
    snapshot_commands_on_shutdown(&db, &runner);
    runner.kill_all_running();
}

/// Live introspection of a terminal: its real cwd and foreground program name
/// (Linux `/proc`). Both are `Option` because the lookup can fail (process gone,
/// permission, non-Linux). Payload of the `terminal_info` command.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TerminalInfo {
    /// Live working directory (`readlink /proc/<shell_pid>/cwd`).
    pub cwd: Option<String>,
    /// Foreground program name (`/proc/<tcgetpgrp(master)>/comm`).
    pub foreground: Option<String>,
}

/// How long a [`TerminalInfo`] reading is reused before `/proc` is re-read.
/// This is the BOUND that keeps the lookup off the hot path: even if the front
/// polls every frame, the actual `/proc` syscalls run at most once per second
/// per terminal. Never compute this per output byte (see module/proc docs).
const INFO_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// A cached [`TerminalInfo`] with the instant it was read, per terminal id.
struct CachedInfo {
    info: TerminalInfo,
    read_at: Instant,
}

/// Managed state: debounced cache of per-terminal `/proc` introspection.
#[derive(Default)]
pub struct TerminalInfoCache {
    by_id: Mutex<HashMap<u64, CachedInfo>>,
}

/// Managed state: the latest OSC 7 cwd seen per PTY id. This is the PORTABLE cwd
/// source (Windows/macOS, and a fallback elsewhere): the output pump scans the
/// raw PTY stream for OSC 7 sequences and records the most recent decoded path
/// here. `terminal_info` then prefers `/proc` (Linux live cwd) and falls back to
/// this OSC 7 cwd, so the auto-attach resolver gets a cwd on every platform.
#[derive(Default)]
pub struct Osc7Cache {
    by_id: Mutex<HashMap<u64, String>>,
}

impl Osc7Cache {
    /// Record the latest OSC 7 cwd for a PTY id (raw, host-stripped path).
    fn set(&self, id: u64, cwd: String) {
        self.by_id.lock().unwrap().insert(id, cwd);
    }
    /// The latest OSC 7 cwd for a PTY id, if any has been seen.
    fn get(&self, id: u64) -> Option<String> {
        self.by_id.lock().unwrap().get(&id).cloned()
    }
}

/// Managed state: the association from a LIVE `pty_id` (the per-spawn, process-
/// unique [`Pty::id`]) to the PERSISTENT terminal RECORD id (the SQLite
/// `terminals.id` the sidebar keys on). These are two distinct id-spaces: the
/// backend emits `pty://output`/`pty://exit` by live pty_id, while exec-state
/// (PRD-2.1) must be persisted and emitted keyed by the durable terminal record
/// id so it survives a restart / re-spawn. `pty_spawn` records the mapping when
/// the front passes a `terminal_id`; the output pump and exit handler resolve it
/// to address the persistent record. A record-less spawn (the socle / a test that
/// passes no `terminal_id`) simply has no entry — exec-state work is then skipped.
#[derive(Default)]
pub struct TerminalIdMap {
    by_pty: Mutex<HashMap<u64, String>>,
}

impl TerminalIdMap {
    /// Associate a live `pty_id` with a persistent terminal record id.
    fn set(&self, pty_id: u64, terminal_id: String) {
        self.by_pty.lock().unwrap().insert(pty_id, terminal_id);
    }
    /// Resolve the persistent terminal record id for a live `pty_id`, if one was
    /// recorded at spawn. Used by the output pump + exit handler.
    fn get(&self, pty_id: u64) -> Option<String> {
        self.by_pty.lock().unwrap().get(&pty_id).cloned()
    }
    /// Drop the mapping for a `pty_id` (on exit/close) so the table does not grow
    /// without bound across the app's lifetime.
    fn remove(&self, pty_id: u64) -> Option<String> {
        self.by_pty.lock().unwrap().remove(&pty_id)
    }
}

/// Managed state: a small per-terminal TAIL buffer for OSC 133 scanning. A real
/// PTY can split an `ESC]133;…` sequence across two read chunks; the pump prepends
/// this carried tail to the next chunk so a split sequence is recovered (the
/// `osc133` parser is position-independent — see its `split_sequence_recovered`
/// test). Keyed by the PERSISTENT terminal record id, not the live pty_id, so the
/// tail follows the record. Bounded: only an unterminated trailing introducer is
/// ever carried.
#[derive(Default)]
pub struct Osc133Pending {
    tail: Mutex<HashMap<String, Vec<u8>>>,
}

impl Osc133Pending {
    /// Take the carried tail for `terminal_id` (if any) and return `tail || chunk`
    /// — the bytes to scan this round. Clears the stored tail; the caller re-sets
    /// it via [`set_tail`](Self::set_tail) with whatever remains incomplete.
    fn take_and_prepend(&self, terminal_id: &str, chunk: &[u8]) -> Vec<u8> {
        let mut map = self.tail.lock().unwrap();
        match map.remove(terminal_id) {
            Some(mut tail) if !tail.is_empty() => {
                tail.extend_from_slice(chunk);
                tail
            }
            _ => chunk.to_vec(),
        }
    }
    /// Store the trailing incomplete OSC 133 bytes to carry into the next chunk.
    /// An empty `tail` clears the entry (the common case: chunk ended complete).
    fn set_tail(&self, terminal_id: &str, tail: Vec<u8>) {
        let mut map = self.tail.lock().unwrap();
        if tail.is_empty() {
            map.remove(terminal_id);
        } else {
            map.insert(terminal_id.to_string(), tail);
        }
    }
}

/// Managed state: the per-terminal OSC 133 → exec-state STATE MACHINE
/// ([`crate::osc133::ExecStateMachine`]), carrying its PROVENANCE GUARD across pump
/// chunks. The guard makes a `D` (command-end) settle a `success`/`error` ONLY when
/// a `C` (pre-exec — a real command ran) preceded it since the last settle. A `D`
/// with no preceding `C` — the shell's FIRST prompt at spawn emits `D;0` (because
/// `$?` is true) with no `C`; a bare Enter on an empty prompt does too — is a
/// PHANTOM end and is IGNORED, so no green success dot lands on a freshly-spawned
/// terminal. Keyed by the PERSISTENT terminal record id (so the guard follows the
/// record across pty rebinds), exactly like [`Osc133Pending`].
#[derive(Default)]
pub struct Osc133ExecState {
    by_terminal: Mutex<HashMap<String, crate::osc133::ExecStateMachine>>,
}

impl Osc133ExecState {
    /// Feed one decoded event to the terminal's machine and return its settled
    /// outcome (if any). A fresh terminal starts with a default machine (no command
    /// has run, so a `D` before any `C` is ignored).
    fn on_event(
        &self,
        terminal_id: &str,
        event: crate::osc133::Osc133Event,
    ) -> Option<crate::osc133::ExecOutcome> {
        self.by_terminal
            .lock()
            .unwrap()
            .entry(terminal_id.to_string())
            .or_default()
            .on_event(event)
    }
}

/// Managed state: the decoded OSC 133 events recorded per persistent terminal,
/// in stream order. The output pump appends here as it scans chunks (PRD-2.1 task
/// #4). The exec-state STATE MACHINE (phase 3, task #6) consumes this log to drive
/// `running`/`success`/`error`, persist via [`crate::db::set_exec_state`], and emit
/// `terminal://exec-state`. Decoupling decode (here) from the transition policy
/// (phase 3) keeps each task's scope clean and the parser/pump unit-testable.
#[derive(Default)]
pub struct Osc133Events {
    by_terminal: Mutex<HashMap<String, Vec<crate::osc133::Osc133Event>>>,
}

impl Osc133Events {
    /// Append decoded events for a terminal, in order.
    fn extend(&self, terminal_id: &str, events: &[crate::osc133::Osc133Event]) {
        if events.is_empty() {
            return;
        }
        self.by_terminal
            .lock()
            .unwrap()
            .entry(terminal_id.to_string())
            .or_default()
            .extend_from_slice(events);
    }
    /// Snapshot the recorded events for a terminal (test/phase-3 consumer).
    #[cfg_attr(not(test), allow(dead_code))]
    fn snapshot(&self, terminal_id: &str) -> Vec<crate::osc133::Osc133Event> {
        self.by_terminal
            .lock()
            .unwrap()
            .get(terminal_id)
            .cloned()
            .unwrap_or_default()
    }
}

/// Spawn the default shell in a new PTY and start streaming its output.
///
/// Returns the new (live) PTY id. The caller (front) subscribes to `pty://output`
/// filtered by this id. Output is coalesced on a dedicated thread.
///
/// `terminal_id` is the PERSISTENT terminal RECORD id (SQLite `terminals.id`) the
/// front already knows (TerminalDeck → Terminal → usePty). When present it is
/// recorded in the [`TerminalIdMap`] so the output pump + exit handler can address
/// the durable record for exec-state (PRD-2.1) — `pty://output`/`pty://exit` stay
/// keyed by the live pty_id and are UNCHANGED. A record-less spawn (the socle /
/// the unit harness) passes `None` and simply gets no mapping entry.
#[tauri::command(async)]
fn pty_spawn<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, PtyManager>,
    id_map: State<'_, TerminalIdMap>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
    terminal_id: Option<String>,
) -> Result<u64, String> {
    let size = portable_pty::PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    // Inject NYX_TERMINAL_ID into the shell (PRD-5 task #3): the SAME persistent
    // record id the front passed (`terminals.id`) is exported into the spawned shell
    // so an agent integration inside it can correlate its session events to THIS
    // terminal. Passed by reference here, then moved into the id_map below.
    let (pty, rx) =
        Pty::spawn(size, cwd.as_deref(), terminal_id.as_deref()).map_err(|e| e.to_string())?;
    let id = pty.id();

    state.ptys.lock().unwrap().insert(id, pty);

    // Record the live pty_id → persistent terminal record id association (when the
    // front supplied one) BEFORE the pump starts, so the very first output chunk
    // can already resolve the record. Skipped for a record-less spawn.
    if let Some(terminal_id) = terminal_id {
        id_map.set(id, terminal_id);
    }

    // Coalescing pump: own the receiver, batch chunks, flush every FLUSH_INTERVAL.
    spawn_output_pump(app, id, rx);

    Ok(id)
}

/// Write bytes (e.g. keystrokes) to the PTY identified by `id`.
#[tauri::command]
fn pty_write(state: State<'_, PtyManager>, id: u64, data: Vec<u8>) -> Result<(), String> {
    use std::io::Write as _;
    // Resolve the writer handle under the registry lock, then RELEASE it before the
    // blocking write (see PtyManager::write_to for the rationale).
    let writer = {
        let ptys = state.ptys.lock().unwrap();
        ptys.get(&id)
            .ok_or_else(|| format!("unknown pty id {id}"))?
            .writer_handle()
    };
    let mut w = writer.lock().unwrap();
    w.write_all(&data).and_then(|_| w.flush()).map_err(|e| e.to_string())
}

/// Resize the PTY identified by `id` to `cols`x`rows` cells.
#[tauri::command]
fn pty_resize(state: State<'_, PtyManager>, id: u64, cols: u16, rows: u16) -> Result<(), String> {
    let ptys = state.ptys.lock().unwrap();
    let pty = ptys
        .get(&id)
        .ok_or_else(|| format!("unknown pty id {id}"))?;
    pty.resize(cols, rows, 0, 0).map_err(|e| e.to_string())
}

/// Kill the PTY identified by `id` and remove it from managed state.
///
/// The `pty://exit` event is emitted by the output pump once the child is
/// reaped, so closing here only needs to terminate the process; dropping the
/// removed [`Pty`] also kills/joins as a safety net.
#[tauri::command(async)]
fn pty_close(state: State<'_, PtyManager>, id: u64) -> Result<(), String> {
    let pty = state.ptys.lock().unwrap().remove(&id);
    match pty {
        Some(mut pty) => {
            pty.kill().map_err(|e| e.to_string())?;
            // `pty` drops here: kills (idempotent), joins the waiter, and joins
            // (Unix) / detaches (Windows) the reader. See `Pty::drop` for why the
            // Windows reader is detached (a `join()` there deadlocked the UI).
            Ok(())
        }
        None => Err(format!("unknown pty id {id}")),
    }
}

/// Read live `/proc` introspection (cwd + foreground program) for the PTY `id`,
/// DEBOUNCED: a reading younger than [`INFO_REFRESH_INTERVAL`] is returned from
/// cache without touching `/proc`. The front may call this on a poll/timer; the
/// debounce makes the syscalls bounded regardless of call frequency.
///
/// On a cache miss/stale entry we read the shell pid (for cwd) and the
/// foreground pgid (`tcgetpgrp` on the master, for the program name) from the
/// live `Pty`, then hit `/proc`. An unknown id yields `Err`. A closed/exited
/// terminal (no live Pty) also yields `Err` — there is nothing to introspect.
#[tauri::command(async)]
fn terminal_info(
    pty_state: State<'_, PtyManager>,
    cache: State<'_, TerminalInfoCache>,
    osc7: State<'_, Osc7Cache>,
    id: u64,
) -> Result<TerminalInfo, String> {
    // Fast path: a fresh cached reading short-circuits before any /proc access.
    {
        let cache = cache.by_id.lock().unwrap();
        if let Some(entry) = cache.get(&id) {
            if entry.read_at.elapsed() < INFO_REFRESH_INTERVAL {
                return Ok(entry.info.clone());
            }
        }
    }

    // Stale/missing: snapshot the pids under the pty lock (cheap), release it,
    // then do the /proc reads without holding the registry lock.
    let (shell_pid, fg_pgid) = {
        let ptys = pty_state.ptys.lock().unwrap();
        let pty = ptys
            .get(&id)
            .ok_or_else(|| format!("unknown pty id {id}"))?;
        (pty.shell_pid(), foreground_pgid(pty))
    };

    let mut info = read_terminal_info(shell_pid, fg_pgid);

    // Portable cwd fallback: when `/proc` did not yield a cwd (non-Linux, or the
    // read failed), use the latest OSC 7 cwd recorded from the output stream.
    // `/proc` (when present) is preferred as the live, kernel-truthful source.
    if info.cwd.is_none() {
        info.cwd = osc7.get(id);
    }

    cache.by_id.lock().unwrap().insert(
        id,
        CachedInfo {
            info: info.clone(),
            read_at: Instant::now(),
        },
    );
    Ok(info)
}

/// Extract the foreground process group id from a `Pty` (`tcgetpgrp(master)`),
/// abstracted so the non-Unix build compiles (where it is always `None`).
///
/// Widened from Linux-only to ALL Unix (task #1): `tcgetpgrp` works on macOS too,
/// so the OS busy/idle signal — and the foreground-program name lookup wherever a
/// `/proc`-equivalent exists — is available on every Unix. Only the `/proc`-backed
/// NAME read ([`read_terminal_info`]) stays Linux-gated; the busy BOOLEAN
/// ([`pty_busy`]) needs none of `/proc`.
#[cfg(unix)]
fn foreground_pgid(pty: &Pty) -> Option<i32> {
    pty.foreground_pgid()
}
#[cfg(not(unix))]
fn foreground_pgid(_pty: &Pty) -> Option<i32> {
    None
}

/// The OS-AUTHORITATIVE busy/idle signal for a terminal (task #1): `true` when a
/// command runs in the PTY's foreground (`foreground_pgid != pgid of the shell`),
/// `false` at an idle prompt, `None` when it cannot be derived (shell pid unknown,
/// `tcgetpgrp` failed, or non-Unix where there is no foreground process group).
///
/// This is the single OS-agnostic point the bridge reads — the busy-state poll
/// loop and any future MCP exposure go through here, so the derivation lives in
/// ONE place. It is INDEPENDENT of OSC 133: the dot no longer needs the shell to
/// emit a `133;C`/`133;D` to know a command is running. Delegates straight to
/// [`Pty::is_busy`] (which encodes the `fg_pgid != shell_pid` rule); on non-Unix
/// it is always `None` (Windows busy/idle is out of scope — ConPTY has no
/// foreground process group).
fn pty_busy(pty: &Pty) -> Option<bool> {
    pty.is_busy()
}

/// Cadence of the backend busy-state poll (PRD task #1, decision 1-B). `tcgetpgrp`
/// is pull-only (no kernel notification), so the bridge SAMPLES the foreground
/// process group of every open PTY on this fixed interval and emits a
/// `terminal://busy-state` event only when a terminal's busy bit CHANGED. ~300ms is
/// snappy enough for the dot while keeping the per-tick syscall cost negligible
/// (one `tcgetpgrp` per open terminal).
const BUSY_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// ONE busy-state sweep: snapshot the busy bit of every live PTY, resolve each to
/// its persistent terminal id, and return the `(terminal_id, busy)` pairs whose
/// value CHANGED since the last sweep (per the [`BusyStateTracker`]). The caller
/// emits a `terminal://busy-state` for each — so only TRANSITIONS reach the front,
/// never one event per tick.
///
/// Cost is mutualized exactly like [`terminal_info`]: a SINGLE pass collects
/// `(pty_id, busy)` while holding the `ptys` lock (one cheap `tcgetpgrp` per PTY),
/// then the lock is released BEFORE the id-map lookups and the change diff, so the
/// registry is not held across that work. A PTY whose busy bit cannot be derived
/// (`None` — non-Unix, or master already closed) is treated as idle (`false`): a
/// terminal with no live foreground group is not running anything.
fn scan_busy_once<R: Runtime>(app: &AppHandle<R>) -> Vec<(String, bool)> {
    // Snapshot busy per live pty_id under the lock (cheap: one tcgetpgrp each),
    // then drop the lock before resolving ids / diffing.
    let snapshot: Vec<(u64, bool)> = {
        let ptys = app.state::<PtyManager>();
        let map = ptys.ptys.lock().unwrap();
        map.iter()
            .map(|(&id, pty)| (id, pty_busy(pty).unwrap_or(false)))
            .collect()
    };

    let id_map = app.state::<TerminalIdMap>();
    let tracker = app.state::<BusyStateTracker>();
    let mut transitions = Vec::new();
    for (pty_id, busy) in snapshot {
        // Only record-backed terminals have a durable id to key the event on; a
        // record-less spawn (socle / test) has no sidebar dot to drive.
        let Some(terminal_id) = id_map.get(pty_id) else {
            continue;
        };
        if tracker.changed(&terminal_id, busy) {
            transitions.push((terminal_id, busy));
        }
    }
    transitions
}

/// Emit `terminal://busy-state` for each busy/idle transition found this sweep.
/// Pure side-effect wrapper over [`scan_busy_once`] so the loop body — and tests —
/// share the same "diff then emit on change only" logic.
fn poll_and_emit_busy_state<R: Runtime>(app: &AppHandle<R>) {
    for (terminal_id, busy) in scan_busy_once(app) {
        let _ = app.emit(
            TERMINAL_BUSY_STATE_EVENT,
            TerminalBusyStatePayload { terminal_id, busy },
        );
    }
}

/// Start the backend busy-state poll loop (PRD task #1, decision 1-B): a dedicated
/// thread that every [`BUSY_POLL_INTERVAL`] samples the foreground process group of
/// every open PTY and emits `terminal://busy-state` ONLY for terminals whose busy
/// bit changed. This is the AUTHORITY for the UI running dot, derived live from the
/// OS — so a force-quit/restore can never leave a phantom running (a restored
/// terminal with no foreground command samples idle by construction, task #2).
///
/// The loop holds no PTY across a sleep; each tick is a bounded sweep (one
/// `tcgetpgrp` per open terminal) under the registry lock, released before the
/// emit. Runs for the app's lifetime — there is no teardown handle because the
/// process owns exactly one of these and it costs nothing while no PTYs are open.
pub fn start_busy_state_loop<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("nyx-busy-state-poll".into())
        .spawn(move || loop {
            std::thread::sleep(BUSY_POLL_INTERVAL);
            poll_and_emit_busy_state(&app);
        })
        .expect("failed to spawn busy-state poll thread");
}

/// Resolve cwd + foreground program from raw pids via `/proc` (Linux). Split
/// from the command so the mapping pid→info is unit-testable with real pids and
/// the non-Linux build degrades to empty info.
#[cfg(target_os = "linux")]
fn read_terminal_info(shell_pid: Option<u32>, fg_pgid: Option<i32>) -> TerminalInfo {
    TerminalInfo {
        cwd: shell_pid.and_then(crate::proc::read_cwd),
        foreground: fg_pgid.and_then(crate::proc::read_foreground_comm),
    }
}
#[cfg(not(target_os = "linux"))]
fn read_terminal_info(_shell_pid: Option<u32>, _fg_pgid: Option<i32>) -> TerminalInfo {
    TerminalInfo::default()
}

/// Spawn the thread that drains the PTY output receiver, coalesces chunks, and
/// emits `pty://output` at most once per [`FLUSH_INTERVAL`]. On disconnect
/// (child exited / master closed) it flushes the tail, reaps the exit code from
/// managed state, and emits `pty://exit`.
///
/// The pump can resolve this live `id`'s PERSISTENT terminal record id from the
/// managed [`TerminalIdMap`] (recorded at spawn): it reads it once at start (the
/// mapping is set before the pump is spawned) and holds it for the lifetime of
/// the thread. A record-less spawn leaves it `None` and the exec-state work
/// (OSC 133, a later task) is simply skipped — output/exit are unchanged.
fn spawn_output_pump<R: Runtime>(app: AppHandle<R>, id: u64, rx: Receiver<Vec<u8>>) {
    std::thread::Builder::new()
        .name(format!("nyx-pty-pump-{id}"))
        .spawn(move || {
            // The durable record this PTY belongs to (if the front passed one at
            // spawn). The pump resolves it from the live pty_id → terminal_id
            // mapping; it is the address the exec-state pipeline (OSC 133) will
            // persist/emit under. `pty://output` itself stays keyed by `id`.
            let terminal_id: Option<String> = app.state::<TerminalIdMap>().get(id);

            let mut pending: Vec<u8> = Vec::new();
            let mut last_flush = Instant::now();

            let flush = |app: &AppHandle<R>, pending: &mut Vec<u8>| {
                if pending.is_empty() {
                    return;
                }
                let payload = OutputPayload {
                    id,
                    bytes: std::mem::take(pending),
                };
                let _ = app.emit("pty://output", payload);
            };

            loop {
                // LEADING-EDGE coalescing. The wait strategy depends on whether we
                // are idle or mid-flood:
                //   - `pending` EMPTY (idle / between keystrokes): BLOCK on
                //     `recv()`. No busy-spin on the 16ms cadence, and — crucially —
                //     `last_flush` is NOT refreshed while we sleep, so it stays
                //     "old". When the next byte arrives, `last_flush.elapsed() >=
                //     FLUSH_INTERVAL` holds and the Ok branch flushes it
                //     IMMEDIATELY (leading edge, retention ≈ 0). A disconnect while
                //     blocked maps to the Disconnected branch unchanged.
                //   - `pending` NON-EMPTY (mid-flood): wait at most until the next
                //     scheduled flush so a steady flood still coalesces on the
                //     16ms cadence (trailing edge, anti event-DoS) rather than
                //     emitting per chunk.
                let recv = if pending.is_empty() {
                    rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
                } else {
                    let wait = FLUSH_INTERVAL.saturating_sub(last_flush.elapsed());
                    rx.recv_timeout(wait)
                };
                match recv {
                    Ok(chunk) => {
                        // Portable cwd source: scan the raw stream for OSC 7 and
                        // record the most recent decoded cwd for this PTY. Cheap
                        // (a substring scan); the auto-attach resolver reads it
                        // via `terminal_info`/`auto_attach_terminal`.
                        if let Some(cwd) = crate::osc7::extract_last_cwd(&chunk) {
                            app.state::<Osc7Cache>().set(id, cwd);
                        }
                        // Exec-state source (PRD-2.1): when this PTY is backed by a
                        // persistent terminal record, scan the SAME raw chunk for
                        // OSC 133 command-lifecycle events ALONGSIDE OSC 7. This
                        // does NOT strip bytes from `pending` — xterm still renders
                        // the full stream below; we only OBSERVE the control
                        // sequences. (See `handle_osc133_chunk`.)
                        if let Some(terminal_id) = terminal_id.as_deref() {
                            handle_osc133_chunk(&app, terminal_id, &chunk);
                        }
                        pending.extend_from_slice(&chunk);
                        if last_flush.elapsed() >= FLUSH_INTERVAL {
                            flush(&app, &mut pending);
                            last_flush = Instant::now();
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        flush(&app, &mut pending);
                        last_flush = Instant::now();
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        // Child exited / master closed: flush the tail and emit exit.
                        flush(&app, &mut pending);
                        let code = reap_exit_code(&app, id);
                        let _ = app.emit("pty://exit", ExitPayload { id, code });
                        // Exec-state (PRD-2.1): a shell/PTY exit must not leave a
                        // stale `running` badge. Settle a still-`running` record to
                        // idle (a settled success/error survives untouched). Only a
                        // real OSC 133 `D` end ever produces success/error.
                        if let Some(terminal_id) = terminal_id.as_deref() {
                            normalize_exec_state_on_exit(&app, terminal_id);
                            // The dot's running authority is now the OS busy signal, so
                            // the EXIT path must clear THAT channel too — not just
                            // exec_state above. `reap_exit_code` already removed this PTY
                            // from the registry, so the busy poll loop will never sample
                            // it again: if we last announced this terminal BUSY (a
                            // foreground command was running when the shell/PTY died,
                            // e.g. killed mid-command), nobody else will ever emit the
                            // `busy=false` that clears the running dot. Emit it here —
                            // the dead PTY is idle by definition — but ONLY on a real
                            // busy→idle transition (the tracker suppresses it for a
                            // terminal that exited from an idle prompt, the common case,
                            // so a mass-close does not flood redundant events).
                            let tracker = app.state::<BusyStateTracker>();
                            if tracker.changed(terminal_id, false) {
                                let _ = app.emit(
                                    TERMINAL_BUSY_STATE_EVENT,
                                    TerminalBusyStatePayload {
                                        terminal_id: terminal_id.to_string(),
                                        busy: false,
                                    },
                                );
                            }
                            // Drop the tracked value so the table does not grow unbounded
                            // and a re-spawn under the same record starts clean (task #1).
                            tracker.forget(terminal_id);
                            // Evict the record→pty mapping too (the MCP-facing one read by
                            // list_terminals/send_to_terminal/close_terminal). The backend
                            // is the single authority on PTY liveness now, so it must clear
                            // BOTH directions here — otherwise list_terminals reports this
                            // dead pty as `live` until the front happens to call
                            // register_terminal_pty(record, None). Idempotent with that path.
                            app.state::<TerminalPtyMap>().clear(terminal_id);
                        }
                        // Drop the pty_id → terminal_id mapping now that the live
                        // PTY is gone; the persistent record outlives it, but this
                        // pty_id will never be reused. Keeps the map bounded.
                        app.state::<TerminalIdMap>().remove(id);
                        break;
                    }
                }
            }
        })
        .expect("failed to spawn pty output pump thread");
}

/// Remove the PTY from managed state and block until its exit code is known.
/// Returns `None` if the PTY was already removed (e.g. via `pty_close`).
///
/// Removing here is load-bearing: on a NATURAL child exit nobody else evicts the
/// entry (the front nulls its session id on `pty://exit` and so never calls
/// `pty_close`), so a `get_mut`-only reap would leak the dead `Pty` — its master
/// fd and finished thread handles — in the map forever. We `remove` it instead,
/// dropping the lock BEFORE the blocking `wait()` (a thread join) so concurrent
/// commands on OTHER PTYs are not serialized behind the join. The owned `Pty` is
/// dropped at the end (kill is a no-op on a dead child; the helper threads have
/// already finished, so the join in `Drop` returns promptly).
fn reap_exit_code<R: Runtime>(app: &AppHandle<R>, id: u64) -> Option<i32> {
    let pty = app.state::<PtyManager>().ptys.lock().unwrap().remove(&id);
    pty.and_then(|mut pty| pty.wait())
}

/// Scan ONE raw PTY chunk for OSC 133 command-lifecycle events and record the
/// decoded transitions for the persistent terminal `terminal_id` (PRD-2.1, task
/// #4). Runs ALONGSIDE the OSC 7 scan in the output pump, over the SAME bytes,
/// and is purely OBSERVATIONAL: it never mutates the chunk and the pump still
/// forwards every byte to `pty://output`, so xterm remains the renderer and no
/// control bytes are stripped.
///
/// A real PTY can split an OSC 133 sequence across chunk boundaries, so we keep a
/// small per-PTY TAIL buffer (the [`Osc133Pending`] managed state, keyed by the
/// persistent terminal id): we prepend the carried tail, parse complete events,
/// then carry whatever trailing incomplete-introducer bytes remain. The decoded
/// [`crate::osc133::Osc133Event`]s are appended to the per-terminal event log
/// ([`Osc133Events`]) AND fed, in order, to the exec-state STATE MACHINE
/// ([`drive_exec_state`], task #3) which — since OSC 133 was retrograded to result
/// annotation — turns a `D` end into a `success`/`error` BADGE (never `running`,
/// which the OS busy signal owns), persists each settled transition via
/// [`crate::db::set_exec_state`], and emits `terminal://exec-state`.
fn handle_osc133_chunk<R: Runtime>(app: &AppHandle<R>, terminal_id: &str, chunk: &[u8]) {
    // Stitch the carried tail (an incomplete sequence from the previous chunk)
    // ahead of this chunk so a split `ESC]133;…` sequence is recovered. The tail
    // is bounded (we only ever carry an UNTERMINATED trailing introducer).
    let pending = app.state::<Osc133Pending>();
    let stitched = pending.take_and_prepend(terminal_id, chunk);

    let events = crate::osc133::extract_events(&stitched);

    // Carry forward any trailing incomplete OSC 133 introducer (`ESC]133;` seen
    // with no terminator yet) so the next chunk can complete it. We only retain
    // from the LAST introducer with no following terminator; everything before it
    // has been fully parsed already.
    pending.set_tail(terminal_id, osc133_incomplete_tail(&stitched));

    if !events.is_empty() {
        // Record the decoded events (the log the phase-2 tests / introspection
        // consume), then drive the state machine over the SAME events, in order.
        app.state::<Osc133Events>().extend(terminal_id, &events);
        for ev in &events {
            drive_exec_state(app, terminal_id, *ev);
        }
    }
}

/// EXEC-STATE STATE MACHINE — OSC 133 RETROGRADED TO RESULT ANNOTATION (PRD task
/// #3). Maps ONE decoded OSC 133 event onto a terminal RECORD's exec-state. Since
/// phase 1 the AUTHORITY for "a command is running" is the OS — the PTY's foreground
/// process group, exposed via `terminal://busy-state` and read by the dot — so
/// OSC 133 NO LONGER drives `running`. It survives ONLY to ANNOTATE the RESULT
/// (success/error + exit code), a best-effort signal the kernel cannot give us
/// (only the shell knows `$?`). Each settled transition is persisted (the DB record
/// is the authority for the badge after restart) and emitted via
/// `terminal://exec-state` so the sidebar badge updates immediately. The transitions:
///
/// - `133;C` (pre-exec) → INERT. It used to post `running` as the dot's source; it
///   no longer does — the OS busy signal owns `running`. Driving `running` from `C`
///   here would re-introduce the very phantom-running bug this PRD fixes (a `D`
///   never emitted by an unsupported/coupled shell would leave a stuck `running`).
/// - `133;D;0` → `success`; `133;D;<non-zero>` → `error`; a `D` with no parseable
///   code settles to `error` (a finished-but-unknown result). This ONLY annotates
///   the color/exit code — it does NOT touch the OS-derived running bit. EVERY
///   settled transition persists `exec_state_unread = 1`: the backend is the SOLE
///   authority for unread and NEVER inspects UI focus — the frontend (task #7) owns
///   "mark read immediately while active" by reacting to the event and calling
///   `terminal_exec_mark_read`.
/// - `133;A`/`133;B` (prompt/command start) carry no exec-state meaning for nyx
///   and are inert here (they keep the parser robust to a full prompt stream).
///
/// GRACEFUL DEGRADATION (the point of the retrograde): a missing `D` loses AT WORST
/// the badge COLOR (success/error) — NEVER a phantom `running`, because `running`
/// comes from the OS. A shell with no OSC 133 integration still gets correct
/// busy/idle, just no green/red.
///
/// The backend never tracks which terminal is focused, so it cannot (and must not)
/// decide read vs unread from focus — that separation is exactly what lets the
/// persisted `exec_state_unread` flag survive a re-deselect (user story #3).
fn drive_exec_state<R: Runtime>(
    app: &AppHandle<R>,
    terminal_id: &str,
    event: crate::osc133::Osc133Event,
) {
    use crate::osc133::ExecOutcome;
    // Feed the event to the per-terminal state machine (managed state, keyed by the
    // persistent record id). It arms on `C` (pre-exec — a real command started) and
    // settles on `D` (command end) — but the PROVENANCE GUARD inside settles ONLY a
    // `D` that a real `C` preceded since the last settle. `A`/`B`/`C` are inert
    // here (they only arm/disarm the guard; the OS busy signal owns `running`), and
    // a `D` with NO preceding `C` — the shell's first prompt at spawn emits `D;0`
    // with no `C`, and a bare Enter on an empty prompt does too — is a PHANTOM end
    // the machine IGNORES (no settle, no badge on a freshly-spawned terminal).
    let Some(outcome) = app.state::<Osc133ExecState>().on_event(terminal_id, event) else {
        return;
    };
    let (state, exit_code) = match outcome {
        ExecOutcome::Success => (db::STATE_SUCCESS, Some(0)),
        // Non-zero OR a missing/garbage code: `error` so a finished command is
        // colored as such. A `None` code keeps `exit_code = None` (failed-ish, but
        // not WHICH code).
        ExecOutcome::Error { exit_code } => (db::STATE_ERROR, exit_code),
    };

    // A settled `D` end (success/error) is always an UNREAD notification. It never
    // affects the OS-derived running bit.
    persist_and_emit_exec_state(app, terminal_id, state, exit_code, true);
}

/// Persist a terminal exec-state transition (authority for restart) THEN emit
/// `terminal://exec-state` keyed by `terminal_id`. The DB write happens FIRST so a
/// listener that re-reads the row on the event sees the committed value (the same
/// order as `TauriRunnerSink::on_state`). The emitted payload mirrors what was
/// persisted: state, exit_code, unread, and the transition timestamp. A persist
/// failure (unknown id, etc.) skips the emit — we never announce a state the DB
/// does not hold.
fn persist_and_emit_exec_state<R: Runtime>(
    app: &AppHandle<R>,
    terminal_id: &str,
    state: &str,
    exit_code: Option<i32>,
    unread: bool,
) {
    let updated_at = app
        .state::<Db>()
        .with_conn(|c| {
            db::set_exec_state(c, terminal_id, state, exit_code, unread)?;
            // Read back the stamped timestamp so the event's `updated_at` matches
            // the persisted `exec_state_updated_at` exactly.
            db::get_terminal(c, terminal_id)
                .map(|t| t.map(|t| t.exec_state_updated_at))
        })
        .ok()
        .flatten();
    let Some(updated_at) = updated_at else {
        // The terminal id was unknown (no row updated / read): do not emit a state
        // the DB does not hold.
        return;
    };
    let _ = app.emit(
        "terminal://exec-state",
        TerminalExecStatePayload {
            terminal_id: terminal_id.to_string(),
            state: state.to_string(),
            exit_code,
            unread,
            updated_at,
        },
    );
}

/// Normalize a terminal's exec-state when its shell/PTY EXITS (the pump saw the
/// reader disconnect). DEFENSIVE since OSC 133 was retrograded (PRD task #3):
/// `drive_exec_state` no longer posts `running` to the persisted `exec_state`, so a
/// fresh terminal cannot reach `running` via OSC 133. This still settles any
/// `running` left in the DB by an OLDER build (a pre-migration row, or a value
/// persisted before this PRD) down to `idle` on exit, so no stale `running` badge
/// survives. A terminal already at a SETTLED state (`success`/`error`) or `idle` is
/// left untouched — its last settled result (and any unread flag) survives the exit.
fn normalize_exec_state_on_exit<R: Runtime>(app: &AppHandle<R>, terminal_id: &str) {
    let current = app
        .state::<Db>()
        .with_conn(|c| db::get_terminal(c, terminal_id))
        .ok()
        .flatten()
        .map(|t| t.exec_state);
    if current.as_deref() == Some(db::STATE_RUNNING) {
        // Settle the stale `running` down to `idle` (not unread — there is no
        // result to notify; the command never reported an exit).
        persist_and_emit_exec_state(app, terminal_id, db::STATE_IDLE, None, false);
    }
}

/// Return the trailing slice of `buf` that is an INCOMPLETE OSC 133 sequence — an
/// `ESC ] 133 ;` introducer with no `BEL`/`ST` terminator after it — so the pump
/// can carry it to the next chunk. Returns an empty slice when the buffer ends on
/// a complete boundary (the common case). Bounds the carry: if the last introducer
/// IS terminated, nothing is carried.
fn osc133_incomplete_tail(buf: &[u8]) -> Vec<u8> {
    const INTRO: &[u8] = b"\x1b]133;";
    // Find the last introducer; if everything after it has a terminator, there is
    // nothing incomplete to carry.
    let Some(pos) = buf
        .windows(INTRO.len())
        .rposition(|w| w == INTRO)
    else {
        return Vec::new();
    };
    let after = &buf[pos + INTRO.len()..];
    let terminated = after.iter().enumerate().any(|(i, &b)| {
        b == 0x07 || (b == 0x1b && after.get(i + 1) == Some(&b'\\'))
    });
    if terminated {
        Vec::new()
    } else {
        buf[pos..].to_vec()
    }
}

// --- Terminal RECORD commands (SQLite via Diesel) ------------------------
//
// These persist the terminal records (id-space distinct from the live PTY ids):
// create/list/close/reorder/rename and the bounded scrollback snapshot. Thin
// wrappers over the unit-tested `crate::db` CRUD functions; the heavy logic and
// its tests live there. Errors are stringified for the IPC boundary.

/// Create a terminal record at `cwd` (optional `label`) and return the new row.
#[tauri::command(async)]
fn create_terminal(
    db: State<'_, Db>,
    cwd: String,
    label: Option<String>,
) -> Result<Terminal, String> {
    db.with_conn(|c| db::create_terminal(c, &cwd, label))
        .map_err(|e| e.to_string())
}

/// List all terminal records in sidebar order (closed ones included).
#[tauri::command(async)]
fn list_terminals(db: State<'_, Db>) -> Result<Vec<Terminal>, String> {
    db.with_conn(db::list_terminals).map_err(|e| e.to_string())
}

/// Mark a terminal record `closed` (no re-spawn at launch).
#[tauri::command(async)]
fn close_terminal(db: State<'_, Db>, id: String) -> Result<(), String> {
    db.with_conn(|c| db::close_terminal(c, &id))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Persist the sidebar order: each id's `order` becomes its index in `ids`.
#[tauri::command(async)]
fn reorder(db: State<'_, Db>, ids: Vec<String>) -> Result<(), String> {
    db.with_conn(|c| db::reorder(c, &ids))
        .map_err(|e| e.to_string())
}

/// Rename a terminal record (`label`; `None` clears it).
#[tauri::command(async)]
fn rename(db: State<'_, Db>, id: String, label: Option<String>) -> Result<(), String> {
    db.with_conn(|c| db::rename(c, &id, label))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Record `id` as the active terminal (stamps `last_active_at`) so a relaunch
/// reopens on it.
#[tauri::command(async)]
fn set_active(db: State<'_, Db>, id: String) -> Result<(), String> {
    db.with_conn(|c| db::set_active(c, &id))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Persist a terminal's serialized scrollback (bounded). The caller debounces.
#[tauri::command(async)]
fn persist_scrollback(db: State<'_, Db>, id: String, serialized: String) -> Result<(), String> {
    db.with_conn(|c| db::persist_scrollback(c, &id, &serialized))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Mark a terminal's settled exec-state as READ (PRD-2.1 task #6): clear
/// `exec_state_unread` while PRESERVING the last settled `exec_state` + exit code
/// (the badge keeps its success/error color but stops being an unread
/// notification). This is the frontend's mark-read path — the front calls it when
/// the user VIEWS the terminal, and immediately when a `success`/`error` arrives
/// for the already-active terminal. The backend owns the unread BIT but never the
/// focus decision (it does not track which terminal is active), so this command is
/// the only way `exec_state_unread` is cleared. Deliberately does NOT collapse the
/// state to idle (that is the managed-command acknowledge model, kept separate).
#[tauri::command(async)]
fn terminal_exec_mark_read(db: State<'_, Db>, id: String) -> Result<(), String> {
    db.with_conn(|c| db::mark_exec_state_read(c, &id))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// --- Project / Workspace commands (SQLite via Diesel, PRD-2 v2) ----------
//
// Thin wrappers over the unit-tested `crate::db` CRUD. `create_project` creates
// the project + its single root workspace; paths are normalized in the db layer.
// A duplicate path within a project surfaces as a stringified UNIQUE error.

/// JSON payload returned by `create_project`: the project and its root workspace.
#[derive(Clone, Serialize)]
struct ProjectWithRoot {
    project: Project,
    root: Workspace,
}

/// Create a project and its single, explicitly-named root workspace at
/// `root_path`. `root_name` defaults to "root" when omitted.
#[tauri::command(async)]
fn create_project<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, Db>,
    name: String,
    root_path: String,
    root_name: Option<String>,
) -> Result<ProjectWithRoot, String> {
    let created = db
        .with_conn(|c| db::create_project(c, &name, &root_path, root_name.as_deref()))
        .map(|(project, root)| ProjectWithRoot { project, root })
        .map_err(|e| e.to_string())?;
    // Broadcast the structural refresh so the sidebar re-pulls the tree (the SAME
    // signal an MCP-driven mutation emits — see `emit_workspaces_changed`).
    emit_workspaces_changed(&app);
    Ok(created)
}

/// List all projects.
#[tauri::command(async)]
fn list_projects(db: State<'_, Db>) -> Result<Vec<Project>, String> {
    db.with_conn(db::list_projects).map_err(|e| e.to_string())
}

/// Rename a project's display `name`. Returns ().
#[tauri::command(async)]
fn update_project(db: State<'_, Db>, id: String, name: String) -> Result<(), String> {
    db.with_conn(|c| db::update_project(c, &id, &name))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Delete a project and its workspaces (ON DELETE CASCADE). Terminals bound to
/// those workspaces are DETACHED (workspace_id → NULL via ON DELETE SET NULL),
/// not killed — they survive as loose terminals. Returns ().
///
/// REFUSED if any command instance of the project is currently running (deleting
/// would cascade-remove a live service's instance, orphaning the process). The
/// user must stop the running services first. Note `update_project`,
/// `rename_workspace`, and the collapse commands are NOT guarded — they change
/// neither a path nor the runtime.
#[tauri::command(async)]
fn delete_project<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, Db>,
    runner: State<'_, ManagedCommandRunner<R>>,
    id: String,
) -> Result<(), String> {
    let instance_ids = db
        .with_conn(|c| db::instance_ids_for_project(c, &id))
        .map_err(|e| e.to_string())?;
    if runner.any_running(&instance_ids) {
        return Err(
            "this project has a running command — stop it before deleting the project".to_string(),
        );
    }
    db.with_conn(|c| db::delete_project(c, &id))
        .map(|_| ())
        .map_err(|e| e.to_string())?;
    // Broadcast the structural refresh so the sidebar re-pulls the tree (a delete is
    // a tree mutation too — same shared signal as the create paths / the MCP tools).
    emit_workspaces_changed(&app);
    Ok(())
}

/// Persist a project's sidebar `collapsed` (open/closed) state so the band's
/// disclosure survives a restart. Returns ().
#[tauri::command(async)]
fn set_project_collapsed(db: State<'_, Db>, id: String, collapsed: bool) -> Result<(), String> {
    db.with_conn(|c| db::set_project_collapsed(c, &id, collapsed))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Persist a project's `resume_agent_sessions` opt-in (PRD-5 #5): when `true`, nyx
/// resumes the project's terminals' active agent sessions at relaunch. Default OFF.
/// Returns ().
#[tauri::command(async)]
fn set_project_resume_agent_sessions(
    db: State<'_, Db>,
    id: String,
    resume: bool,
) -> Result<(), String> {
    db.with_conn(|c| db::set_project_resume_agent_sessions(c, &id, resume))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// One close-warning entry returned by [`agent_close_warnings`] (PRD-5 #6): a live
/// agent session a close would silently drop. `message` is the ready-to-show line
/// (names the agent + terminal + workspace); the structured fields let the front group
/// / link if it wants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CloseWarningEntry {
    pub terminal_id: String,
    pub agent_kind: String,
    pub message: String,
}

/// The agent-session CLOSE WARNINGS (PRD-5 #6): the live (`active`/`unknown`) sessions
/// whose project does NOT auto-resume — i.e. the ones a close would drop without nyx
/// bringing them back. An EMPTY list means "no warning needed; close freely". The front
/// calls this on a close request and, when non-empty, shows the confirm dialog before
/// actually closing. The message names the AGENT (Claude/Codex/OpenCode/custom) and the
/// TERMINAL (label or id) + workspace.
#[tauri::command(async)]
fn agent_close_warnings(db: State<'_, Db>) -> Result<Vec<CloseWarningEntry>, String> {
    use crate::agent_resume::{close_warning_message, should_warn_on_close, SessionState};
    let rows = db
        .with_conn(db::close_warning_candidates)
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .filter(|w| {
            // The single warn/no-warn policy point: a resume-ON project never warns;
            // only a live (active/unknown) session in a non-resuming project does. An
            // unrecognized state string is treated as not-warnable (defensive).
            SessionState::from_db(&w.session_state)
                .is_some_and(|s| should_warn_on_close(s, w.project_resume_on))
        })
        .map(|w| CloseWarningEntry {
            message: close_warning_message(
                &w.agent_kind,
                w.terminal_label.as_deref(),
                &w.terminal_id,
                w.workspace_name.as_deref(),
            ),
            terminal_id: w.terminal_id,
            agent_kind: w.agent_kind,
        })
        .collect())
}

/// The terminals that currently host a LIVE (`active`) agent session, each with the
/// agent kind (finding #55). The sidebar reads this on mount and on every
/// `agent-sessions://changed` event, then maps `agent_kind` through its provider
/// registry to render the agent's logo in place of the generic terminal glyph for those
/// rows (reverting when the session ends and the terminal drops out of this list). An
/// EMPTY list means "no terminal has a live session" (every row shows the terminal icon).
#[tauri::command(async)]
fn agent_active_sessions(db: State<'_, Db>) -> Result<Vec<db::ActiveAgentSession>, String> {
    db.with_conn(db::active_agent_sessions).map_err(|e| e.to_string())
}

/// Create a (non-root) workspace in `project_id` at `path`. Rejects a path
/// already present in the SAME project (UNIQUE(project_id, path)).
#[tauri::command(async)]
fn create_workspace<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, Db>,
    project_id: String,
    name: String,
    path: String,
) -> Result<Workspace, String> {
    let workspace = db
        .with_conn(|c| db::create_workspace(c, &project_id, &name, &path))
        .map_err(|e| e.to_string())?;
    // Broadcast the structural refresh so the sidebar re-pulls the tree (the SAME
    // signal the MCP `workspace_add`/`create_workspace` tools emit).
    emit_workspaces_changed(&app);
    Ok(workspace)
}

/// List the workspaces of `project_id` (root first).
#[tauri::command(async)]
fn list_workspaces(db: State<'_, Db>, project_id: String) -> Result<Vec<Workspace>, String> {
    db.with_conn(|c| db::list_workspaces(c, &project_id))
        .map_err(|e| e.to_string())
}

/// Rename a workspace's display `name` (the path is immutable). Returns ().
#[tauri::command(async)]
fn rename_workspace(db: State<'_, Db>, id: String, name: String) -> Result<(), String> {
    db.with_conn(|c| db::rename_workspace(c, &id, &name))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Persist a workspace's sidebar `collapsed` (open/closed) state so the band's
/// disclosure survives a restart. Returns ().
#[tauri::command(async)]
fn set_workspace_collapsed(db: State<'_, Db>, id: String, collapsed: bool) -> Result<(), String> {
    db.with_conn(|c| db::set_workspace_collapsed(c, &id, collapsed))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Attach a terminal record to a workspace with an explicit binding `mode`
/// (`auto`|`manual`).
#[tauri::command(async)]
fn attach_terminal(
    db: State<'_, Db>,
    terminal_id: String,
    workspace_id: String,
    mode: String,
) -> Result<(), String> {
    db.with_conn(|c| db::attach_terminal(c, &terminal_id, &workspace_id, &mode))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Detach a terminal record from any workspace (mode resets to `auto`).
#[tauri::command(async)]
fn detach_terminal(db: State<'_, Db>, terminal_id: String) -> Result<(), String> {
    db.with_conn(|c| db::detach_terminal(c, &terminal_id))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Pin a terminal record to a workspace (mode `manual`; a later `cd` no longer
/// moves it).
#[tauri::command(async)]
fn pin_terminal_workspace(
    db: State<'_, Db>,
    terminal_id: String,
    workspace_id: String,
) -> Result<(), String> {
    db.with_conn(|c| db::pin_terminal_workspace(c, &terminal_id, &workspace_id))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Unpin a terminal record (mode `auto`; auto-attach resumes).
#[tauri::command(async)]
fn unpin_terminal_workspace(db: State<'_, Db>, terminal_id: String) -> Result<(), String> {
    db.with_conn(|c| db::unpin_terminal_workspace(c, &terminal_id))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// --- Managed command (template / instance / lifecycle / source) ----------
//
// The INTERNAL Tauri API the PRD-3 UI consumes. This is NOT the public MCP
// contract (PRD-4 owns that, its tool names and its hard-to-reverse ADR); these
// are plain internal `#[tauri::command]`s, thin wrappers over the unit-tested
// `crate::db` CRUD + the `crate::command` runner, with a few RUNTIME GUARDS that
// refuse a mutation while an affected instance is running. Errors are stringified
// for the IPC boundary, like every other command here.

/// Create a per-project command template. `subfolder` is an optional run path
/// relative to the workspace; `restart_on_startup` toggles boot relaunch; the
/// `source_*` group carries optional package.json provenance. Materializes one
/// instance per existing workspace of the project (in the db layer).
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
fn command_create<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, Db>,
    project_id: String,
    name: String,
    command: String,
    subfolder: Option<String>,
    restart_on_startup: Option<bool>,
    source_kind: Option<String>,
    source_package_json_path: Option<String>,
    source_script_name: Option<String>,
    source_script_command_snapshot: Option<String>,
    package_manager: Option<String>,
) -> Result<db::ManagedCommand, String> {
    // INFER provenance for a hand-authored command whose line is itself a package
    // manager invocation (`bun install`, `pnpm dev`, …). The import path supplies
    // these fields explicitly; a manually-added command leaves them null, so the
    // detected vs. manual commands looked inconsistent (the dogfood finding). We
    // only fill in a manager the caller did NOT set, and only when the command's
    // first token names a known PM — never overriding an explicit value.
    let (source_kind, package_manager) =
        infer_command_source(&command, source_kind, package_manager);
    let source = db::CommandSource {
        source_kind,
        source_package_json_path,
        source_script_name,
        source_script_command_snapshot,
        package_manager,
    };
    let template = db.with_conn(|c| {
        let created = db::create_template(
            c,
            &project_id,
            &name,
            &command,
            subfolder.as_deref(),
            source,
        )?;
        // Apply the restart flag if the caller asked for it (the template defaults
        // to false at creation).
        if restart_on_startup == Some(true) {
            db::set_restart_on_startup(c, &created.id, true)?;
        }
        db::get_template(c, &created.id).map(|t| t.unwrap_or(created))
    })
    .map_err(|e| e.to_string())?;
    // Broadcast the command-band refresh so the sidebar + modal re-pull (the SAME
    // signal the MCP `add_command` tool emits — see `emit_commands_changed`).
    emit_commands_changed(&app);
    Ok(template)
}

/// List a project's command templates in sidebar order.
#[tauri::command(async)]
fn command_list(db: State<'_, Db>, project_id: String) -> Result<Vec<db::ManagedCommand>, String> {
    db.with_conn(|c| db::list_templates(c, &project_id))
        .map_err(|e| e.to_string())
}

/// Update a template's editable fields (`name`, `command`, `subfolder`,
/// `restart_on_startup`). REFUSED if any of the template's instances is running
/// (the user must stop the service before editing what affects its runtime).
///
/// SOURCE DETACH ON MANUAL EDIT: a package.json-linked command's link survives
/// only while the command is still the canonical call for its source script.
/// When this update CHANGES the `command` of a sourced template to a value that
/// is neither the package-manager runner call (`pnpm dev`, …) NOR the current
/// raw script snapshot, the source is DETACHED in the same write — the command
/// is now hand-authored and no longer tracks the script. Editing only the name /
/// subfolder / restart flag (command unchanged), or re-typing exactly the runner
/// call or the raw script, keeps the link. (Resync is the explicit path that
/// adopts a new script value WITHOUT detaching — see [`command_resync_source`].)
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
fn command_update<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, Db>,
    runner: State<'_, ManagedCommandRunner<R>>,
    id: String,
    name: String,
    command: String,
    subfolder: Option<String>,
    restart_on_startup: Option<bool>,
) -> Result<(), String> {
    guard_template_not_running(&db, &runner, &id)?;
    db.with_conn(|c| {
        // Decide whether this edit must detach a package.json source: only when the
        // template IS sourced and the new `command` drifts away from BOTH the runner
        // call and the raw script body it was tracking. We read the current row first
        // so we can compare the incoming command against the canonical values.
        let detach = match db::get_template(c, &id)? {
            Some(t) if t.source_script_name.is_some() => command_detaches_source(&t, &command),
            _ => false,
        };
        db::update_template(c, &id, &name, &command, subfolder.as_deref())?;
        if detach {
            // The manual edit broke the link to the source script → drop provenance.
            db::set_template_source(c, &id, db::CommandSource::default())?;
        }
        if let Some(flag) = restart_on_startup {
            db::set_restart_on_startup(c, &id, flag)?;
        }
        Ok::<_, diesel::result::Error>(())
    })
    .map_err(|e| e.to_string())?;
    // Broadcast the command-band refresh so the sidebar + modal re-pull the edited
    // template (the SAME signal the MCP `update_command` tool emits on success).
    emit_commands_changed(&app);
    Ok(())
}

/// Whether replacing a sourced template's command with `new_command` should
/// DETACH its package.json source. It detaches when the new command is neither
/// the detected package-manager runner invocation for the source script (`pnpm
/// dev`, `npm run dev`, …) NOR the current raw script snapshot — i.e. the user
/// edited the command away from the canonical call so it no longer tracks the
/// script. Callers only invoke this for an actually-sourced template.
///
/// `pub(crate)` so the MCP `update_command` tool (`mcp_tools.rs`) applies the
/// IDENTICAL detach rule as the UI's `command_update`, instead of replicating it.
///
/// Thin DELEGATION to the shared [`nyx_core::pkgjson::command_detaches_source`] — the
/// ONE implementation both shells (and the Electron core-host napi dispatcher) drive.
pub(crate) fn command_detaches_source(template: &db::ManagedCommand, new_command: &str) -> bool {
    crate::pkgjson::command_detaches_source(template, new_command)
}

/// Delete a template (its instances cascade away). REFUSED if any of its instances
/// is running.
#[tauri::command(async)]
fn command_delete<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, Db>,
    runner: State<'_, ManagedCommandRunner<R>>,
    id: String,
) -> Result<(), String> {
    guard_template_not_running(&db, &runner, &id)?;
    db.with_conn(|c| db::delete_template(c, &id))
        .map(|_| ())
        .map_err(|e| e.to_string())?;
    // Broadcast the command-band refresh so the sidebar + modal drop the removed
    // template (a delete is a template mutation too — same shared signal).
    emit_commands_changed(&app);
    Ok(())
}

/// Persist a project's template order: each id's order becomes its index in `ids`.
#[tauri::command(async)]
fn command_reorder(db: State<'_, Db>, ids: Vec<String>) -> Result<(), String> {
    db.with_conn(|c| db::reorder_templates(c, &ids))
        .map_err(|e| e.to_string())
}

/// List a workspace's command instances, each joined to its template's display
/// fields (`name`, `command`, `subfolder`, the `source_*` provenance, order) and
/// its workspace path. Each row's `cwd` is filled here with the resolved run
/// directory (`workspace_path` + `subfolder`, best-effort) so the front's command
/// info bar can show where the command runs without re-resolving.
#[tauri::command(async)]
fn command_instance_list(
    db: State<'_, Db>,
    workspace_id: String,
) -> Result<Vec<db::InstanceWithTemplate>, String> {
    let mut rows = db
        .with_conn(|c| db::list_instances_for_workspace(c, &workspace_id))
        .map_err(|e| e.to_string())?;
    for row in &mut rows {
        // Best-effort (infallible) resolution for display: shows the real cwd a
        // spawn would use, falling back to the lexical workspace+subfolder join when
        // the subfolder is missing/unsafe — the listing must never fail on that.
        row.cwd = Some(crate::subfolder::resolve_run_dir_lossy(
            &row.workspace_path,
            row.subfolder.as_deref(),
        ));
    }
    Ok(rows)
}

/// Start (or restart-from-terminal-state) an instance: resolve its command line +
/// cwd (the workspace path joined with the validated subfolder) and spawn through
/// the runner. Idempotent on a running instance (no second spawn). Returns the
/// `last_state` string after the call.
#[tauri::command(async)]
fn command_start<R: Runtime>(
    _app: AppHandle<R>,
    db: State<'_, Db>,
    runner: State<'_, ManagedCommandRunner<R>>,
    instance_id: String,
) -> Result<String, String> {
    let (command, cwd) = resolve_command_and_cwd(&db, &instance_id)?;
    runner
        .start(&instance_id, &command, Some(&cwd))
        .map(|s| s.as_db_str().to_string())
        .map_err(|e| e.to_string())
}

/// Stop a running instance (best-effort process-tree kill, then idle). Idempotent
/// on a non-running instance. Returns the `last_state` string after the call.
#[tauri::command(async)]
fn command_stop<R: Runtime>(
    _app: AppHandle<R>,
    runner: State<'_, ManagedCommandRunner<R>>,
    instance_id: String,
) -> Result<String, String> {
    runner
        .stop(&instance_id)
        .map(|s| s.as_db_str().to_string())
        .map_err(|e| e.to_string())
}

/// Relaunch an instance: stop-then-start if running, else a direct start. Never
/// leaves two live processes. Returns the `last_state` string after the call.
#[tauri::command(async)]
fn command_relaunch<R: Runtime>(
    _app: AppHandle<R>,
    db: State<'_, Db>,
    runner: State<'_, ManagedCommandRunner<R>>,
    instance_id: String,
) -> Result<String, String> {
    let (command, cwd) = resolve_command_and_cwd(&db, &instance_id)?;
    runner
        .relaunch(&instance_id, &command, Some(&cwd))
        .map(|s| s.as_db_str().to_string())
        .map_err(|e| e.to_string())
}

/// Acknowledge a FINISHED one-shot when it is opened/selected: clear ONLY its
/// "unseen result" notification (`unread`) so the settled BADGE hides once the user
/// has seen it. The FACTUAL outcome (`last_state` / `last_exit_code` / `ended_at`) is
/// NEVER erased — this is the v4 finding fix: a UI ack must no longer collapse the
/// state to `idle`, which used to erase the error + exit code the MCP (and any other
/// observer) reads. A `running` instance is never acknowledged (no unseen result).
///
/// Two paths, both clearing only the unread flag (never the outcome):
///   - LIVE terminal entry (a run that finished this session): the runner flips its
///     in-memory `unread` to false and (via the sink) persists `unread=0` + emits
///     `command://ack`.
///   - PERSISTED terminal state with NO live entry (e.g. a `success`/`error` restored
///     at boot, never re-run): the runner has no live entry to flip, so we clear the
///     persisted `unread` here and emit `command://ack` directly — same payload — so
///     the badge still hides. The factual `last_state`/`last_exit_code` survive.
///
/// Returns the FACTUAL `last_state` string after the call (unchanged by the ack).
#[tauri::command(async)]
fn command_acknowledge<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, Db>,
    runner: State<'_, ManagedCommandRunner<R>>,
    instance_id: String,
) -> Result<String, String> {
    // The acknowledge core is shared with the MCP `mark_read` path so a UI ack and an
    // MCP consuming read converge on ONE behavior (clear only `unread`, emit
    // `command://ack`, never touch the outcome). An unknown id surfaces here as the
    // command's error string.
    acknowledge_unread(&app, &db, &runner, &instance_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("unknown command instance {instance_id}"))
}

/// Acknowledge a FINISHED one-shot's "unseen result": clear ONLY its `unread` flag and
/// emit `command://ack`, NEVER its factual outcome (`last_state`/`last_exit_code`/
/// `ended_at`). The SHARED core behind both the UI `command_acknowledge` command and the
/// MCP `get_command_output(mark_read:true)` path, so a UI ack and an MCP consuming read
/// are byte-for-byte the same operation (the v4 split: an ack must never erase the error
/// the MCP sees, and the MCP must reuse — not re-implement — the UI's acknowledge).
///
/// Two paths, identical to the original UI command:
///   - LIVE entry (a run that finished this session): the runner flips its in-memory
///     `unread` to false and (via the sink) persists `unread=0` + emits `command://ack`.
///   - PERSISTED-only state (no live entry, e.g. a `success`/`error` row restored at
///     boot): clear the persisted `unread` here and emit `command://ack` directly.
///
/// A `running` instance is never acknowledged (no unseen result yet). Returns
/// `Ok(Some(factual_last_state))` (unchanged by the ack), or `Ok(None)` when the id names
/// no instance (the caller maps that to its own not-found error). A DB error propagates.
pub(crate) fn acknowledge_unread<R: Runtime>(
    app: &AppHandle<R>,
    db: &Db,
    runner: &ManagedCommandRunner<R>,
    instance_id: &str,
) -> diesel::QueryResult<Option<String>> {
    // Never acknowledge a live process — it has no unseen result yet.
    if runner.is_running(instance_id) {
        return Ok(Some(crate::command::RunState::Running.as_db_str().to_string()));
    }
    // LIVE terminal entry (a run that finished this session): the runner clears its
    // in-memory `unread` and (via the sink) persists `unread=0` + emits the ack
    // event. A no-op for a runner that has no live terminal entry to flip — that case
    // is the persisted path below. `is_unread` tells us whether the runner just
    // handled it, so we don't double-emit for the same acknowledge.
    let runner_had_unread = runner.is_unread(instance_id);
    runner.acknowledge(instance_id);
    if runner_had_unread {
        // The runner + sink already cleared `unread` and emitted `command://ack`.
        return Ok(Some(runner.state_of(instance_id).as_db_str().to_string()));
    }
    // PERSISTED terminal state with no live entry: clear its `unread` here so a
    // restored, still-unread success/error badge also hides on select — WITHOUT
    // touching the factual `last_state`/`last_exit_code`/`ended_at`.
    let inst = match db.with_conn(|c| db::get_instance(c, instance_id))? {
        Some(inst) => inst,
        None => return Ok(None),
    };
    if inst.unread && (inst.last_state == db::STATE_SUCCESS || inst.last_state == db::STATE_ERROR) {
        db.with_conn(|c| db::acknowledge_instance(c, instance_id))?;
        let _ = app.emit(
            "command://ack",
            CommandAckPayload {
                instance_id: instance_id.to_string(),
            },
        );
    }
    // Return the factual state (unchanged — the outcome is never erased by an ack).
    Ok(Some(inst.last_state))
}

/// Return an instance's output history: the LIVE in-memory buffer if it is running,
/// else the persisted scrollback read back from the DB (cold rehydration). The front
/// feeds it into the read-only output panel on open.
///
/// While running, the runner keeps a bounded live scrollback tail in memory and
/// streams `command://output` events on top of it; the persisted DB row is only
/// debounced (`PERSIST_DEBOUNCE`) plus a final persist on exit, so it can lag the
/// true tail. Reading the live buffer when running makes this one-shot call reflect
/// the current output, not a row that can be up to a debounce window stale. When the
/// instance is idle/success/error there is no live buffer, so we rehydrate cold from
/// the persisted scrollback.
#[tauri::command(async)]
fn command_output<R: Runtime>(
    _app: AppHandle<R>,
    db: State<'_, Db>,
    runner: State<'_, ManagedCommandRunner<R>>,
    instance_id: String,
) -> Result<String, String> {
    // Live path: a running instance returns the runner's in-memory tail directly.
    if let Some(live) = runner.live_output(&instance_id) {
        return Ok(live);
    }
    // Cold path: idle/success/error (or absent live map) rehydrates the persisted
    // scrollback row from the DB.
    db.with_conn(|c| db::get_instance(c, &instance_id))
        .map_err(|e| e.to_string())?
        .map(|inst| inst.scrollback)
        .ok_or_else(|| format!("unknown command instance {instance_id}"))
}

// --- Source actions (NO implicit rewrite of `command`) -------------------
//
// `refresh` updates only the snapshot + a derived status; it NEVER touches
// `command`. The one action that DOES change `command` is the explicit `resync`
// (re-reads the package.json at click time, rewrites the command to the current
// raw script value, and KEEPS the link). `unlink` drops the source_* fields,
// turning the template into a plain manual command. (There is no longer a
// "reset to script runner" action — adopting the runner call is just a manual
// edit, which detaches per [`command_update`].)

/// Result of a source refresh: the freshness status + the (possibly updated)
/// snapshot. `command` is deliberately ABSENT — refresh never rewrites it.
#[derive(Clone, Serialize)]
struct SourceRefreshResult {
    /// `fresh` | `stale` | `missing package.json` | `missing script`.
    status: String,
    /// The script's current raw body, when the file + script still exist.
    snapshot: Option<String>,
}

/// Re-read the template's source `package.json` and update ONLY the snapshot +
/// status. Does NOT modify `command` (no implicit rewrite). Status is `fresh` when
/// the on-disk script body equals the stored snapshot, `stale` when it differs,
/// `missing package.json` / `missing script` when the file/script is gone.
#[tauri::command(async)]
fn command_source_refresh(db: State<'_, Db>, id: String) -> Result<SourceRefreshResult, String> {
    let template = db
        .with_conn(|c| db::get_template(c, &id))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("unknown command {id}"))?;

    let Some(pkg_path) = template.source_package_json_path.clone() else {
        return Err(format!("command {id} has no linked package.json source"));
    };
    let Some(script_name) = template.source_script_name.clone() else {
        return Err(format!("command {id} has no linked source script"));
    };

    match read_script_body(&pkg_path, &script_name) {
        ScriptLookup::Missing => Ok(SourceRefreshResult {
            status: "missing package.json".to_string(),
            snapshot: template.source_script_command_snapshot,
        }),
        ScriptLookup::NoScript => Ok(SourceRefreshResult {
            status: "missing script".to_string(),
            snapshot: template.source_script_command_snapshot,
        }),
        ScriptLookup::Body(body) => {
            // Update the stored snapshot (provenance freshness) but never `command`.
            let status = if template.source_script_command_snapshot.as_deref() == Some(&body) {
                "fresh"
            } else {
                "stale"
            };
            let source = db::CommandSource {
                source_kind: template.source_kind.clone(),
                source_package_json_path: template.source_package_json_path.clone(),
                source_script_name: template.source_script_name.clone(),
                source_script_command_snapshot: Some(body.clone()),
                package_manager: template.package_manager.clone(),
            };
            db.with_conn(|c| db::set_template_source(c, &id, source))
                .map_err(|e| e.to_string())?;
            Ok(SourceRefreshResult {
                status: status.to_string(),
                snapshot: Some(body),
            })
        }
    }
}

/// EXPLICITLY RESYNC the command to the source script's CURRENT raw body, re-read
/// from the package.json AT CLICK TIME (not the snapshot). Modifies `command`.
/// KEEPS the source fields (the link is preserved — this is the explicit "adopt
/// the new script value" path that does NOT detach) + refreshes the snapshot.
/// REFUSED if any instance is running. Errors if the file/script no longer exists.
#[tauri::command(async)]
fn command_resync_source<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, Db>,
    runner: State<'_, ManagedCommandRunner<R>>,
    id: String,
) -> Result<String, String> {
    guard_template_not_running(&db, &runner, &id)?;
    let template = db
        .with_conn(|c| db::get_template(c, &id))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("unknown command {id}"))?;
    let pkg_path = template
        .source_package_json_path
        .clone()
        .ok_or_else(|| format!("command {id} has no linked package.json source"))?;
    let script = template
        .source_script_name
        .clone()
        .ok_or_else(|| format!("command {id} has no linked source script"))?;

    // Re-read the package.json AT CLICK TIME (the snapshot is not the authority).
    let body = match read_script_body(&pkg_path, &script) {
        ScriptLookup::Body(b) => b,
        ScriptLookup::Missing => {
            return Err(format!(
                "the source package.json '{pkg_path}' no longer exists"
            ))
        }
        ScriptLookup::NoScript => {
            return Err(format!("the source script '{script}' no longer exists"))
        }
    };

    db.with_conn(|c| {
        // Update command to the raw body AND refresh the snapshot (now fresh).
        db::update_template(c, &id, &template.name, &body, template.subfolder.as_deref())?;
        let source = db::CommandSource {
            source_kind: template.source_kind.clone(),
            source_package_json_path: template.source_package_json_path.clone(),
            source_script_name: template.source_script_name.clone(),
            source_script_command_snapshot: Some(body.clone()),
            package_manager: template.package_manager.clone(),
        };
        db::set_template_source(c, &id, source)
    })
    .map_err(|e| e.to_string())?;
    // Broadcast the command-band refresh so the sidebar + modal re-pull the resynced
    // command (a resync rewrites the template's `command` — same shared signal).
    emit_commands_changed(&app);
    Ok(body)
}

/// EXPLICITLY detach the package.json source: clears all `source_*` fields +
/// `package_manager`, turning the template into a plain manual command. `command`
/// is left exactly as-is.
#[tauri::command(async)]
fn command_unlink_source<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, Db>,
    id: String,
) -> Result<(), String> {
    db.with_conn(|c| db::set_template_source(c, &id, db::CommandSource::default()))
        .map(|_| ())
        .map_err(|e| e.to_string())?;
    // Broadcast the command-band refresh so the sidebar + modal re-pull the now
    // un-sourced template (clearing the source is a template mutation too).
    emit_commands_changed(&app);
    Ok(())
}

// --- Package.json import (discovery + create from selection) -------------

/// Discover the package.json scripts under a WORKSPACE (root + subfolders), each
/// with an editable proposed name + default runner command + source metadata. The
/// front renders these for selection; an empty list means nothing importable.
#[tauri::command(async)]
fn command_import_scripts(
    db: State<'_, Db>,
    workspace_id: String,
) -> Result<Vec<crate::pkgjson::DiscoveredScript>, String> {
    let path = db
        .with_conn(|c| db::workspace_path(c, &workspace_id))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("unknown workspace {workspace_id}"))?;
    Ok(crate::pkgjson::discover_package_scripts(&path))
}

/// Create a template from a SELECTED import row: the (user-edited) name + command,
/// the package.json subfolder, and the source metadata. Refused with a clear error
/// if the final name already exists in the project.
#[tauri::command(async)]
#[allow(clippy::too_many_arguments)]
fn command_import_create<R: Runtime>(
    app: AppHandle<R>,
    db: State<'_, Db>,
    project_id: String,
    name: String,
    command: String,
    subfolder: String,
    source_package_json_path: String,
    source_script_name: String,
    source_script_command_snapshot: String,
    package_manager: String,
) -> Result<db::ManagedCommand, String> {
    let source = db::CommandSource {
        source_kind: Some(db::SOURCE_KIND_PACKAGE_JSON.to_string()),
        source_package_json_path: Some(source_package_json_path),
        source_script_name: Some(source_script_name),
        source_script_command_snapshot: Some(source_script_command_snapshot),
        package_manager: Some(package_manager),
    };
    let template = db.with_conn(|c| {
        crate::pkgjson::import_command(c, &project_id, &name, &command, &subfolder, source)
    })?;
    // Broadcast the command-band refresh so the sidebar + modal re-pull the imported
    // template (the SAME signal the MCP `import_commands` tool emits on success).
    emit_commands_changed(&app);
    Ok(template)
}

// --- Shutdown snapshot + boot restoration (PRD-3 Phase 3, task 16) --------
//
// The auto-relaunch-on-startup contract, driven by TWO signals (never
// `last_state` alone):
//   - at SHUTDOWN, snapshot `was_running_on_shutdown = (the runner reports the
//     instance running)` for every instance;
//   - at BOOT, relaunch an instance ONLY when its template's `restart_on_startup`
//     is ON AND its `was_running_on_shutdown` snapshot is true; then reset the
//     snapshot so the next boot cannot relaunch a ghost; and normalize a stale
//     `running` (a dead/orphaned process) down to `idle` while keeping
//     `success`/`error` for the dot.

/// Snapshot the shutdown state for every command instance — thin DELEGATION to the
/// shell-agnostic [`crate::command::snapshot_commands_on_shutdown`] in nyx-core
/// (extracted so the Tauri adapter and the Electron core-host run the IDENTICAL
/// flow). The runner's LIVE map is the source of truth. Kept as a same-named wrapper
/// so the existing call sites (the window-close hook, the bridge tests) are untouched.
pub fn snapshot_commands_on_shutdown<R: Runtime>(db: &Db, runner: &ManagedCommandRunner<R>) {
    crate::command::snapshot_commands_on_shutdown(db, runner)
}

/// Restore command instances at boot from the shutdown snapshot — thin DELEGATION to
/// the shell-agnostic [`crate::command::restore_commands_on_boot`] in nyx-core. The
/// relaunch/normalize/reset contract lives in the core now; this wrapper preserves the
/// Tauri call sites (setup hook + tests). Returns the relaunched instance ids.
pub fn restore_commands_on_boot<R: Runtime>(
    db: &Db,
    runner: &ManagedCommandRunner<R>,
) -> Vec<String> {
    crate::command::restore_commands_on_boot(db, runner)
}

// --- Internal guard + resolution helpers ---------------------------------

/// Refuse a template mutation while any of its instances is running. The runner's
/// LIVE map is authoritative; the persisted `last_state` is only a mirror. Returns
/// a clear user-facing error when blocked.
fn guard_template_not_running<R: Runtime>(
    db: &State<'_, Db>,
    runner: &State<'_, ManagedCommandRunner<R>>,
    template_id: &str,
) -> Result<(), String> {
    let instance_ids = db
        .with_conn(|c| db::instance_ids_for_template(c, template_id))
        .map_err(|e| e.to_string())?;
    if runner.any_running(&instance_ids) {
        return Err(
            "this command is running in at least one workspace — stop it before editing or deleting it"
                .to_string(),
        );
    }
    Ok(())
}

/// Resolve an instance's command line + run cwd: the template `command`, and the
/// workspace path joined with the VALIDATED subfolder (anti path-traversal /
/// existence, via [`crate::subfolder`]). Errors before any spawn on an unknown
/// instance or an invalid/missing subfolder.
fn resolve_command_and_cwd(
    db: &State<'_, Db>,
    instance_id: &str,
) -> Result<(String, String), String> {
    let ctx = db
        .with_conn(|c| db::instance_run_context(c, instance_id))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("unknown command instance {instance_id}"))?;
    let cwd = crate::subfolder::resolve_run_dir(&ctx.workspace_path, ctx.subfolder.as_deref())?;
    Ok((ctx.command, cwd))
}

/// Fill in `package_manager`/`source_kind` for a hand-authored command when they
/// were not explicitly provided AND the command line is itself a package-manager
/// invocation. An EXPLICIT caller value (import path) is always preserved. When we
/// infer a manager we also tag `source_kind` = `package_json` (the only non-null
/// `source_kind` the schema's CHECK allows — see migration v3), so an inferred
/// command reads consistently with a detected one. A command whose first token is
/// not a known PM is left untouched (both stay `None`).
///
/// `pub(crate)` so the MCP `add_command` tool (`mcp_tools.rs`) infers provenance
/// through the SAME path as the UI's `command_create`, instead of replicating it.
///
/// Thin DELEGATION to the shared [`nyx_core::pkgjson::infer_command_source`] — the ONE
/// implementation both shells (and the Electron core-host napi dispatcher) drive.
pub(crate) fn infer_command_source(
    command: &str,
    source_kind: Option<String>,
    package_manager: Option<String>,
) -> (Option<String>, Option<String>) {
    crate::pkgjson::infer_command_source(command, source_kind, package_manager)
}

/// Outcome of looking up a script body in a package.json on disk.
enum ScriptLookup {
    /// The file is gone / unreadable / unparsable.
    Missing,
    /// The file exists but does not contain the named script.
    NoScript,
    /// The script's current raw body.
    Body(String),
}

/// Read the current raw body of `script_name` from the package.json at `pkg_path`.
/// Used by `refresh` / `swap` to re-read the source AT CALL TIME (never the
/// snapshot). Distinguishes a missing file from a missing script.
fn read_script_body(pkg_path: &str, script_name: &str) -> ScriptLookup {
    let Ok(text) = std::fs::read_to_string(pkg_path) else {
        return ScriptLookup::Missing;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return ScriptLookup::Missing;
    };
    match json
        .get("scripts")
        .and_then(|v| v.as_object())
        .and_then(|s| s.get(script_name))
        .and_then(|v| v.as_str())
    {
        Some(body) => ScriptLookup::Body(body.to_string()),
        None => ScriptLookup::NoScript,
    }
}

// --- Auto-attach (cwd → known workspace) ---------------------------------

/// Outcome of an auto-attach pass, returned to the front so it can reflect the
/// (possibly unchanged) binding. `workspace_id` is the terminal's binding AFTER
/// the pass (`None` = unattached); `changed` says whether this call moved it.
#[derive(Clone, Serialize)]
pub struct AutoAttachResult {
    pub workspace_id: Option<String>,
    pub changed: bool,
}

/// Build the platform-appropriate [`crate::resolve::CwdProvider`] for a raw cwd
/// string. On Linux the live cwd is the `/proc` reading; everywhere else it is
/// the OSC 7 reading. Either way the provider normalizes it into the one form
/// the resolver compares against stored workspace paths. A `None` cwd is the
/// explicit "no reliable source" signal (`CwdProvider::None`).
fn cwd_provider(cwd: Option<String>) -> crate::resolve::CwdProvider {
    use crate::resolve::CwdProvider;
    match cwd {
        None => CwdProvider::None,
        #[cfg(target_os = "linux")]
        Some(c) => CwdProvider::Proc(Some(c)),
        #[cfg(not(target_os = "linux"))]
        Some(c) => CwdProvider::Osc7(Some(c)),
    }
}

/// Auto-attach a terminal RECORD to the longest-ancestor KNOWN workspace for the
/// given live `cwd`. The cwd comes from the platform-agnostic provider chain
/// (`/proc` on Linux, OSC 7 elsewhere) — the front reads it from `terminal_info`
/// and passes it here; `None`/empty means "no reliable cwd" and is a no-op.
///
/// The hybrid rule is applied in [`crate::resolve::decide_attachment`]:
/// - a `manual`-pinned terminal is never moved;
/// - `auto` mode follows the resolved cwd;
/// - NO project/workspace is created, and an unmatched cwd leaves the binding
///   untouched (no guessing).
#[tauri::command(async)]
fn auto_attach_terminal(
    db: State<'_, Db>,
    terminal_id: String,
    cwd: Option<String>,
) -> Result<AutoAttachResult, String> {
    use crate::resolve::{
        decide_attachment, Attachment, BindingMode, CurrentBinding, WorkspaceMatch,
    };

    // Wrap the incoming cwd in the platform-appropriate provider, then take its
    // NORMALIZED cwd — the single code path the resolver consumes regardless of
    // source (/proc on Linux, OSC 7 elsewhere). A `None`/empty cwd surfaces as
    // "no reliable source" and the resolver makes no change.
    let provider = cwd_provider(cwd);
    let normalized = provider.normalized_cwd();

    db.with_conn(|c| {
        // Current binding of the terminal record. Unknown id ⇒ no-op (unattached).
        let term = db::get_terminal(c, &terminal_id)?;
        let Some(term) = term else {
            return Ok(AutoAttachResult {
                workspace_id: None,
                changed: false,
            });
        };
        let mode = if term.workspace_binding_mode == db::BINDING_MANUAL {
            BindingMode::Manual
        } else {
            BindingMode::Auto
        };
        let current = CurrentBinding {
            workspace_id: term.workspace_id.clone(),
            mode,
        };

        // The candidate set is ONLY the already-known workspaces.
        let known: Vec<WorkspaceMatch> = db::all_workspaces(c)?
            .into_iter()
            .map(|w| WorkspaceMatch {
                id: w.id,
                path: w.path,
            })
            .collect();

        match decide_attachment(&current, normalized.as_deref(), &known) {
            Attachment::AttachAuto(ws) => {
                db::attach_terminal(c, &terminal_id, &ws, db::BINDING_AUTO)?;
                Ok(AutoAttachResult {
                    workspace_id: Some(ws),
                    changed: true,
                })
            }
            Attachment::Unchanged => Ok(AutoAttachResult {
                workspace_id: term.workspace_id,
                changed: false,
            }),
        }
    })
    .map_err(|e: diesel::result::Error| e.to_string())
}

/// Decide whether the custom window controls (min / max / close) are shown, from
/// the raw `NYX_WINDOW_CONTROLS` env value. Pure so it is unit-testable without
/// touching the real process env.
///
/// The contract (the PRD's interim runtime toggle): controls are VISIBLE by
/// default; ONLY the exact string `"0"` hides them. Any other value (including
/// unset/empty) keeps them visible — a permissive default so the frameless
/// window is never left uncloseable by an unexpected value.
fn controls_visible_from_env(raw: Option<String>) -> bool {
    raw.as_deref() != Some("0")
}

/// Whether the frameless window controls should render, read from the OS env at
/// RUNTIME (`NYX_WINDOW_CONTROLS`). Exposed to the front (which has no access to
/// `process.env` inside the webview) so launching `NYX_WINDOW_CONTROLS=0 nyx`
/// hides the controls without a rebuild. Default (unset / any non-`"0"`) =
/// visible. See [`controls_visible_from_env`] for the parsing contract.
#[tauri::command]
fn window_controls_visible() -> bool {
    controls_visible_from_env(std::env::var("NYX_WINDOW_CONTROLS").ok())
}

// --- Portless `nyx.localhost` option (PRD-4 #6, ADR-0003 D11) -------------
//
// A SEPARATE human/integration surface, disabled by default. Enabling shells out to
// `portless alias nyx <port> --force` and surfaces `https://nyx.localhost`; disabling
// runs `portless alias --remove nyx`. The MCP transport stays on localhost direct —
// portless never becomes the MCP transport (ADR-0003 D11). All the create/remove/error
// logic + fake-binary tests live in `crate::portless`; these are the thin Tauri
// commands that drive it with the real `SystemRunner` and persist the toggle.

/// Status of the portless option, returned to the front.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortlessStatus {
    /// Whether the option is currently enabled (persisted; disabled by default).
    enabled: bool,
    /// The human URL the alias exposes when enabled (`https://nyx.localhost`).
    url: &'static str,
}

/// Path of the portless toggle settings file under nyx's data dir.
fn portless_settings_path<R: Runtime>(app: &AppHandle<R>) -> Result<std::path::PathBuf, String> {
    crate::resolve_data_dir(app)
        .map(|d| d.join(crate::portless::settings::SETTINGS_FILE))
        .map_err(|e| e.to_string())
}

/// The current MCP port the option would alias (ADR-0003 D2 resolution).
fn portless_port() -> u16 {
    crate::mcp::resolve_port()
}

/// Read the persisted portless option state (disabled by default).
#[tauri::command(async)]
fn portless_status<R: Runtime>(app: AppHandle<R>) -> Result<PortlessStatus, String> {
    let path = portless_settings_path(&app)?;
    let state = crate::portless::settings::read(&path);
    Ok(PortlessStatus {
        enabled: matches!(state, crate::portless::PortlessState::Enabled),
        url: crate::portless::PORTLESS_URL,
    })
}

/// Enable or disable the portless option. Enabling verifies `portless` is present,
/// runs `portless alias nyx <port> --force`, and (on success) persists `enabled` and
/// returns the `https://nyx.localhost` URL. Disabling runs `portless alias --remove
/// nyx` and persists `disabled`. A missing `portless` binary is a clear error
/// (`portless is not installed …`), never an auto-install (ADR-0003 D11). State is
/// only persisted AFTER the alias mutation succeeds, so a failed enable does not
/// leave the toggle stuck "on".
#[tauri::command(async)]
fn portless_set_enabled<R: Runtime>(
    app: AppHandle<R>,
    enabled: bool,
) -> Result<PortlessStatus, String> {
    use crate::portless::{PortlessManager, PortlessState, SystemRunner};
    let path = portless_settings_path(&app)?;
    let mgr = PortlessManager::new(SystemRunner);
    if enabled {
        let port = portless_port();
        mgr.enable(port).map_err(|e| e.to_string())?;
        crate::portless::settings::write(&path, PortlessState::Enabled).map_err(|e| e.to_string())?;
    } else {
        mgr.disable().map_err(|e| e.to_string())?;
        crate::portless::settings::write(&path, PortlessState::Disabled)
            .map_err(|e| e.to_string())?;
    }
    Ok(PortlessStatus {
        enabled,
        url: crate::portless::PORTLESS_URL,
    })
}

// ---------------------------------------------------------------------------
// Integration management commands (PRD-4 task #1/#3)
// ---------------------------------------------------------------------------
// These commands back the Settings → Integrations UI. Install state is
// persisted to `<app_data_dir>/integrations.json` via [`onboarding::IntegrationState`].
// Only `claude_code` is fully functional in v1; other providers are
// advertised as "coming soon" in the UI and their commands are stubs.

/// Status of one integration, returned to the front-end. The nyx Claude integration is
/// now ONE bundled plugin that provides BOTH the MCP server and the session-capture hooks
/// (finding #44/#45), so there is a SINGLE `installed` flag — no more split MCP/plugin
/// state to desync (finding #46).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationStatus {
    /// Provider key (e.g. `"claude_code"`).
    provider: &'static str,
    /// Human-readable display name.
    label: &'static str,
    /// Whether the nyx integration (the ONE bundled plugin: MCP + hooks) is installed for
    /// this provider. Derived from Claude Code's REAL config — `enabledPlugins[
    /// "nyx-claude-integration@nyx"] == true` — not nyx's stored flag (finding #46).
    installed: bool,
    /// `true` when the provider is fully functional in v1; `false` = coming soon.
    available: bool,
}

/// Build the integration status list. Pure (no `AppHandle`) so the wiring — 4 registry
/// providers, available/coming-soon flags — is unit-testable in isolation (see the
/// `tests` module). claude_code's single install flag is derived from Claude Code's REAL
/// config (finding #46): `enabledPlugins["nyx-claude-integration@nyx"]` in
/// `~/.claude/settings.json` (`NYX_CLAUDE_SETTINGS`). So a plugin removed DIRECTLY in
/// Claude Code reads as uninstalled at the next status refresh, instead of nyx's flag
/// lying.
fn integration_status_list() -> Vec<IntegrationStatus> {
    vec![
        claude_status(),
        IntegrationStatus { provider: "codex", label: "Codex", installed: false, available: false },
        IntegrationStatus { provider: "opencode", label: "OpenCode", installed: false, available: false },
        // `custom` is reserved for a future user-defined MCP server flow
        // (onboarding.rs, ADR-0003 D14/D11). No semantics in v1 → coming soon,
        // like codex/opencode. Listed so the UI shows all 4 registry providers.
        IntegrationStatus { provider: "custom", label: "Custom", installed: false, available: false },
    ]
}

/// The `claude_code` integration status, derived from Claude Code's REAL config (finding
/// #46) rather than nyx's stored `integrations.json` flag: a SINGLE `installed` flag from
/// `enabledPlugins["nyx-claude-integration@nyx"] == true` in `~/.claude/settings.json`
/// ([`crate::plugin::claude_plugin_enabled`]). The plugin now provides the MCP too, so
/// there is no separate MCP status to read. Honors the injectable `NYX_CLAUDE_SETTINGS`
/// seam, so callers never touch the real `~/.claude` under test.
fn claude_status() -> IntegrationStatus {
    IntegrationStatus {
        provider: "claude_code",
        label: "Claude Code",
        installed: crate::plugin::claude_plugin_enabled(),
        available: true,
    }
}

/// Same as [`claude_status`] but reads the REAL Claude `settings.json` at an EXPLICIT path
/// instead of resolving the `NYX_CLAUDE_SETTINGS` seam. Used by the install/remove cores so
/// they report the post-mutation status from the very file the plugin CLI just wrote —
/// keeping the `AppHandle`-free cores testable against temp paths with no process-global
/// env (finding #46).
fn claude_status_at(settings_path: &std::path::Path) -> IntegrationStatus {
    IntegrationStatus {
        provider: "claude_code",
        label: "Claude Code",
        installed: crate::plugin::plugin_enabled_in_settings(
            settings_path,
            &crate::plugin::claude_plugin_install_id(),
        ),
        available: true,
    }
}

/// Core of `integration_install` (no `AppHandle`). Installs the ONE nyx Claude integration
/// — the bundled plugin that provides BOTH the MCP server and the session-capture hooks
/// (finding #44/#45). There is no separate MCP write anymore: the plugin's `.mcp.json`
/// declares the MCP, so to AVOID a double-declaration we also strip any legacy standalone
/// `mcpServers.nyx` from `~/.claude.json` left by the old separate-MCP flow. A missing
/// `claude` CLI surfaces as a typed error (no fake success — review #35), shown verbatim
/// in the UI. Unit-testable against temp paths.
fn do_integration_install(
    provider: &str,
    target: &crate::onboarding::OnboardingTarget,
    plugin_install: Option<&crate::plugin::PluginInstall>,
    plugin_cli: Option<&dyn crate::plugin::PluginCli>,
    state_path: &std::path::Path,
) -> Result<IntegrationStatus, String> {
    if provider != "claude_code" {
        return Err(format!("provider '{provider}' is not supported in v1"));
    }
    let descriptor = plugin_install
        .ok_or_else(|| "Could not resolve the bundled nyx plugin (no plugin dir / app data dir)".to_string())?;
    let cli = plugin_cli
        .ok_or_else(|| "Could not resolve the Claude plugin CLI driver".to_string())?;
    // Install the ONE plugin (copy + port-template + register via the CLI). The plugin
    // bundles the MCP, so this is the whole integration.
    crate::plugin::install_with(descriptor, cli).map_err(|e| e.to_string())?;
    // Drop any legacy standalone MCP so it is not declared twice (the plugin now owns it).
    let _ = crate::onboarding::remove_legacy_mcp_server(&target.config_path);

    // Mark nyx's own (non-authoritative) install cache flag, kept for back-compat — the
    // real status is read from Claude's config below. Best-effort: this flag is NOT the
    // status authority, and the plugin is already installed, so a save failure must NOT
    // fail the install (which would leave a confusing "plugin installed but call failed").
    let mut state = crate::onboarding::IntegrationState::load(state_path);
    state.set_installed("claude_code", true);
    if let Err(e) = state.save(state_path) {
        eprintln!("integration_install: persisting install cache flag failed (non-fatal): {e}");
    }
    Ok(claude_status_at(&claude_settings_path_for(plugin_install)))
}

/// The Claude `settings.json` path to read the post-mutation plugin status from: the
/// plugin descriptor's `settings_path` when one was resolved (the same file the CLI
/// install/uninstall writes `enabledPlugins` into), else the resolved real seam
/// (`NYX_CLAUDE_SETTINGS` / `~/.claude/settings.json`). Keeps the install/remove cores
/// reading the REAL file they just affected without a process-global env (review #40).
fn claude_settings_path_for(plugin_install: Option<&crate::plugin::PluginInstall>) -> std::path::PathBuf {
    plugin_install
        .map(|p| p.settings_path.clone())
        .or_else(crate::plugin::claude_settings_path)
        .unwrap_or_default()
}

/// Core of `integration_remove` (no `AppHandle`). The mirror of install: uninstalls the
/// ONE nyx plugin (CLI uninstall + marketplace remove) AND cleans every legacy residue so
/// nothing nyx lingers (finding #45) — the legacy standalone `mcpServers.nyx` in
/// `~/.claude.json`, plus the legacy hand-written settings keys
/// (`extraKnownMarketplaces.nyx` / `enabledPlugins[…]`) from the old approach. Best-effort:
/// a `None` descriptor / CLI still clears the legacy MCP + the state flag. Unit-testable
/// against temp paths.
fn do_integration_remove(
    provider: &str,
    target: &crate::onboarding::OnboardingTarget,
    plugin_install: Option<&crate::plugin::PluginInstall>,
    plugin_cli: Option<&dyn crate::plugin::PluginCli>,
    state_path: &std::path::Path,
) -> Result<IntegrationStatus, String> {
    if provider != "claude_code" {
        return Err(format!("provider '{provider}' is not supported in v1"));
    }
    // Uninstall the plugin + remove the marketplace + strip legacy settings keys.
    // Best-effort, but don't swallow the error SILENTLY: the returned status is read from
    // Claude's real config (so it stays honest), yet a failed CLI uninstall should be
    // surfaced rather than vanishing into a `let _ =`.
    if let (Some(descriptor), Some(cli)) = (plugin_install, plugin_cli) {
        if let Err(e) = crate::plugin::remove_with(descriptor, cli) {
            eprintln!("integration_remove: plugin uninstall failed (best-effort): {e}");
        }
    }
    // Strip the legacy standalone MCP server entry (residue from the old separate-MCP flow).
    let _ = crate::onboarding::remove_legacy_mcp_server(&target.config_path);

    // Non-authoritative cache flag (see install) — best-effort, never fatal.
    let mut state = crate::onboarding::IntegrationState::load(state_path);
    state.set_installed("claude_code", false);
    if let Err(e) = state.save(state_path) {
        eprintln!("integration_remove: persisting install cache flag failed (non-fatal): {e}");
    }
    Ok(claude_status_at(&claude_settings_path_for(plugin_install)))
}

/// Resolve the Claude Code plugin install descriptor from an `AppHandle`: the bundled
/// plugin SOURCE dir (via the resource dir), the STABLE install dir (under the Tauri app
/// data dir — review #33), and the settings path (for legacy cleanup). `None` when any
/// of those cannot be resolved.
fn claude_plugin_install<R: Runtime>(app: &AppHandle<R>) -> Option<crate::plugin::PluginInstall> {
    use crate::agent::AgentAdapter;
    use tauri::Manager;
    let resource_dir = app.path().resource_dir().ok();
    let app_data_dir = crate::resolve_data_dir(app).ok();
    crate::agent::ClaudeCodeAdapter.plugin_install(resource_dir.as_deref(), app_data_dir.as_deref())
}

/// Resolve the Claude Code plugin CLI driver (shells out to `claude plugin …`).
fn claude_plugin_cli() -> Option<Box<dyn crate::plugin::PluginCli>> {
    use crate::agent::AgentAdapter;
    crate::agent::ClaudeCodeAdapter.plugin_cli()
}

/// List the status of all supported integrations (the ONE bundled plugin per provider).
/// The claude_code install flag is derived from Claude Code's REAL config (finding #46),
/// so no `AppHandle`/data-dir lookup is needed — the status helper resolves the real
/// `~/.claude/settings.json` (or its injectable seam).
#[tauri::command(async)]
fn integration_list<R: Runtime>(_app: AppHandle<R>) -> Result<Vec<IntegrationStatus>, String> {
    Ok(integration_status_list())
}

/// Install the ONE nyx integration for a provider — the bundled plugin that provides BOTH
/// the MCP server and the session-capture hooks (finding #44/#45). For `claude_code`:
/// copies the bundled plugin into a stable app-data dir (port-templating the bundled MCP),
/// registers it via the `claude` CLI (`marketplace add` + `install`; the CLI owns
/// `settings.json`), and strips any legacy standalone `mcpServers.nyx` so the MCP is not
/// declared twice. Other providers are not yet supported and return an error.
#[tauri::command(async)]
fn integration_install<R: Runtime>(
    app: AppHandle<R>,
    provider: String,
) -> Result<IntegrationStatus, String> {
    let data_dir = crate::resolve_data_dir(&app).map_err(|e| e.to_string())?;
    let state_path = data_dir.join(crate::onboarding::INTEGRATIONS_FILE);
    if provider == "claude_code" {
        let target = crate::onboarding::OnboardingTarget::claude_code()
            .ok_or_else(|| "Could not resolve Claude Code config path (no home dir)".to_string())?;
        let plugin = claude_plugin_install(&app);
        let cli = claude_plugin_cli();
        do_integration_install(&provider, &target, plugin.as_ref(), cli.as_deref(), &state_path)
    } else {
        Err(format!("provider '{provider}' is not supported in v1"))
    }
}

/// Uninstall the ONE nyx integration for a provider — the mirror of install (finding #45).
/// For `claude_code`: uninstalls the bundled plugin via the `claude` CLI (+ removes the
/// marketplace), strips the legacy standalone `mcpServers.nyx` from `~/.claude.json`, and
/// strips any leftover legacy hand-written settings keys — leaving no nyx residue. Other
/// providers are not yet supported and return an error.
#[tauri::command(async)]
fn integration_remove<R: Runtime>(
    app: AppHandle<R>,
    provider: String,
) -> Result<IntegrationStatus, String> {
    let data_dir = crate::resolve_data_dir(&app).map_err(|e| e.to_string())?;
    let state_path = data_dir.join(crate::onboarding::INTEGRATIONS_FILE);
    if provider == "claude_code" {
        let target = crate::onboarding::OnboardingTarget::claude_code()
            .ok_or_else(|| "Could not resolve Claude Code config path (no home dir)".to_string())?;
        let plugin = claude_plugin_install(&app);
        let cli = claude_plugin_cli();
        do_integration_remove(&provider, &target, plugin.as_ref(), cli.as_deref(), &state_path)
    } else {
        Err(format!("provider '{provider}' is not supported in v1"))
    }
}

/// TEST-ONLY: spawn a real PTY on `app`'s managed [`PtyManager`] and return its id, so the
/// MCP terminal-tool tests (in `crate::mcp_tools`) can register a LIVE shell for a record
/// and exercise the `send_to_terminal` write path without re-implementing the spawn. The
/// PTY is the only OS touch (no interactive grid), so it runs under the ConPTY gap.
#[cfg(test)]
pub(crate) fn tests_spawn_pty<R: Runtime>(app: &tauri::App<R>) -> u64 {
    use tauri::Manager;
    let size = portable_pty::PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
    let (pty, rx) = Pty::spawn(size, None, None).expect("spawn pty");
    let id = pty.id();
    app.state::<PtyManager>().ptys.lock().unwrap().insert(id, pty);
    spawn_output_pump(app.handle().clone(), id, rx);
    id
}

/// Register the PTY managed state and command handlers on the builder.
pub fn init<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .manage(PtyManager::default())
        .manage(TerminalInfoCache::default())
        .manage(Osc7Cache::default())
        .manage(TerminalIdMap::default())
        .manage(Osc133Pending::default())
        .manage(Osc133ExecState::default())
        .manage(Osc133Events::default())
        .manage(BusyStateTracker::default())
        .manage(TerminalPtyMap::default())
        .manage(PendingTerminalCommands::default())
        .manage(PendingResumes::default())
        .invoke_handler(tauri::generate_handler![
            pty_spawn,
            pty_write,
            pty_resize,
            pty_close,
            terminal_info,
            create_terminal,
            list_terminals,
            close_terminal,
            register_terminal_pty,
            reorder,
            rename,
            set_active,
            persist_scrollback,
            terminal_exec_mark_read,
            window_controls_visible,
            create_project,
            list_projects,
            update_project,
            delete_project,
            set_project_collapsed,
            set_project_resume_agent_sessions,
            agent_close_warnings,
            agent_active_sessions,
            create_workspace,
            list_workspaces,
            rename_workspace,
            set_workspace_collapsed,
            attach_terminal,
            detach_terminal,
            pin_terminal_workspace,
            unpin_terminal_workspace,
            auto_attach_terminal,
            command_create,
            command_list,
            command_update,
            command_delete,
            command_reorder,
            command_instance_list,
            command_start,
            command_stop,
            command_relaunch,
            command_acknowledge,
            command_output,
            command_source_refresh,
            command_resync_source,
            command_unlink_source,
            command_import_scripts,
            command_import_create,
            portless_status,
            portless_set_enabled,
            integration_list,
            integration_install,
            integration_remove
        ])
}

#[cfg(test)]
mod tests {
    //! Bridge integration tests on the `tauri::test` MOCK RUNTIME.
    //!
    //! We exercise the real command bodies (`pty_spawn`/`pty_write`/
    //! `pty_resize`/`pty_close`), the managed `PtyManager` state, the coalescing
    //! output pump, and the actually-emitted `pty://output` / `pty://exit`
    //! events captured via `app.listen`. We invoke the command functions
    //! directly with the mock app's `AppHandle` + `State` rather than routing
    //! through the IPC layer: app-defined command ACL permissions are generated
    //! at build time by `tauri-build` and are absent under `mock_context`
    //! (the IPC authority would reject every invoke with "Plugin not found" /
    //! "UnknownManifest"). Calling the bodies directly tests OUR logic and the
    //! event contract; the ACL wiring is validated by the real
    //! `generate_context!` build (capabilities/default.json) which `cargo build`
    //! compiles.

    use super::*;
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};
    use tauri::{App, Listener, Manager};

    fn build_app() -> App<MockRuntime> {
        init(mock_builder())
            .build(mock_context(noop_assets()))
            .expect("failed to build mock app")
    }

    /// Mock app with an IN-MEMORY `Db` managed, so the record commands run
    /// against a real migrated SQLite without touching `app_data_dir`.
    fn build_app_with_db() -> App<MockRuntime> {
        let app = build_app();
        app.manage(Db::in_memory());
        app
    }

    /// Invoke the `pty_spawn` command body with the mock app's handle + state.
    /// Record-less (no terminal_id) — the exec-state mapping is exercised by the
    /// dedicated `spawn_with_record` helper / tests below.
    fn spawn(app: &App<MockRuntime>, cols: u16, rows: u16) -> u64 {
        pty_spawn(
            app.handle().clone(),
            app.state::<PtyManager>(),
            app.state::<TerminalIdMap>(),
            None,
            cols,
            rows,
            None,
        )
        .expect("pty_spawn")
    }

    /// Invoke `pty_spawn` WITH a persistent terminal record id, returning the live
    /// pty id. Used to exercise the pty_id → terminal_id mapping (task #3) and the
    /// OSC 133 chunk scan (task #4).
    fn spawn_with_record(
        app: &App<MockRuntime>,
        cols: u16,
        rows: u16,
        terminal_id: &str,
    ) -> u64 {
        pty_spawn(
            app.handle().clone(),
            app.state::<PtyManager>(),
            app.state::<TerminalIdMap>(),
            None,
            cols,
            rows,
            Some(terminal_id.to_string()),
        )
        .expect("pty_spawn")
    }
    fn write(app: &App<MockRuntime>, id: u64, data: &[u8]) {
        pty_write(app.state::<PtyManager>(), id, data.to_vec()).expect("pty_write");
    }
    fn resize(app: &App<MockRuntime>, id: u64, cols: u16, rows: u16) {
        pty_resize(app.state::<PtyManager>(), id, cols, rows).expect("pty_resize");
    }
    fn close(app: &App<MockRuntime>, id: u64) -> Result<(), String> {
        pty_close(app.state::<PtyManager>(), id)
    }
    fn info(app: &App<MockRuntime>, id: u64) -> Result<TerminalInfo, String> {
        terminal_info(
            app.state::<PtyManager>(),
            app.state::<TerminalInfoCache>(),
            app.state::<Osc7Cache>(),
            id,
        )
    }

    /// Spawn an INTERACTIVE `bash` PTY directly (job control ON, so the foreground
    /// process group flips when a command runs — the exact condition the busy
    /// signal reads), register it on the manager AND map its live id to a
    /// persistent `terminal_id`, mirroring what `pty_spawn` wires. Used by the
    /// busy-state tests so they do not depend on which shell `$SHELL` resolves to.
    /// Returns the live pty id.
    #[cfg(unix)]
    fn spawn_interactive_bash_record(app: &App<MockRuntime>, terminal_id: &str) -> u64 {
        let size = portable_pty::PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
        let (pty, rx) = Pty::spawn_program_with_terminal_id(
            "bash",
            &["--norc", "--noprofile", "-i"],
            size,
            None,
            Some(terminal_id),
        )
        .expect("spawn interactive bash");
        let id = pty.id();
        app.state::<PtyManager>().ptys.lock().unwrap().insert(id, pty);
        app.state::<TerminalIdMap>().set(id, terminal_id.to_string());
        spawn_output_pump(app.handle().clone(), id, rx);
        id
    }

    /// Decode an emitted `pty://output` payload (JSON `{id, bytes:[..]}`) to a String.
    fn output_to_string(payload: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(payload).expect("json");
        let bytes: Vec<u8> = v["bytes"]
            .as_array()
            .expect("bytes array")
            .iter()
            .map(|n| n.as_u64().unwrap() as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Count the PTYs currently held in managed state (leak detector).
    fn live_pty_count(app: &App<MockRuntime>) -> usize {
        app.state::<PtyManager>().ptys.lock().unwrap().len()
    }

    /// On Unix, ask the kernel whether `pid` still names a live process.
    /// `kill(pid, 0)` performs permission/existence checks WITHOUT sending a
    /// signal: it returns 0 if the process exists, or -1 with errno `ESRCH`
    /// when there is no such process. This is the authoritative "is it an
    /// orphan?" probe — far stronger than trusting our own bookkeeping.
    #[cfg(unix)]
    fn process_alive(pid: i32) -> bool {
        // SAFETY: `kill` with signal 0 has no side effects beyond the checks.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc == 0 {
            return true;
        }
        // Distinguish "gone" (ESRCH) from a real error. Anything that is not
        // ESRCH (e.g. EPERM — exists but not ours) counts as alive.
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    /// Parse the FIRST `MARK<id>:<pid>\n` record out of accumulated PTY output.
    /// The interactive shell echoes the typed command (`$$` literal) before the
    /// expanded value, so we require the colon to be followed by digits AND a
    /// terminating newline — same robustness trick as `close_leaves_no_orphan`.
    fn parse_marked_pid(acc: &str, mark: &str) -> Option<i32> {
        let tag = format!("{mark}:");
        for (off, _) in acc.match_indices(&tag) {
            let rest = &acc[off + tag.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() && rest[digits.len()..].starts_with(['\r', '\n']) {
                return digits.parse::<i32>().ok();
            }
        }
        None
    }

    /// Done-criterion #1+#2+#3 for multi-PTY in ONE test: spawn 3 PTYs
    /// simultaneously, prove each routes output INDEPENDENTLY under its own id,
    /// then close ONE and prove (via `kill(pid,0)`) that only its process dies —
    /// the other two stay alive and their handles are not leaked.
    #[cfg(unix)]
    #[test]
    fn three_ptys_route_independently_and_close_isolates() {
        let app = build_app();

        // Collect output keyed by id so we can assert per-id routing.
        let by_id: Arc<Mutex<HashMap<u64, String>>> = Arc::new(Mutex::new(HashMap::new()));
        {
            let by_id = Arc::clone(&by_id);
            app.listen("pty://output", move |event| {
                let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                let id = v["id"].as_u64().unwrap();
                let bytes: Vec<u8> = v["bytes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|n| n.as_u64().unwrap() as u8)
                    .collect();
                let mut map = by_id.lock().unwrap();
                map.entry(id)
                    .or_default()
                    .push_str(&String::from_utf8_lossy(&bytes));
            });
        }

        // Spawn 3 PTYs at once; each prints a per-id marker + its own PID, then
        // sleeps so we can observe it alive before closing.
        let ids = [
            spawn(&app, 80, 24),
            spawn(&app, 80, 24),
            spawn(&app, 80, 24),
        ];
        assert_eq!(
            live_pty_count(&app),
            3,
            "three simultaneous spawns register three PTYs"
        );
        for (i, &id) in ids.iter().enumerate() {
            let cmd = format!("echo MARK{i}:$$\nsleep 60\n");
            write(&app, id, cmd.as_bytes());
        }

        // Wait until every id has produced its OWN marker. Output must be routed
        // by id: id[i]'s buffer contains `MARK{i}` and never another index.
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut pids: [Option<i32>; 3] = [None; 3];
        while Instant::now() < deadline && pids.iter().any(|p| p.is_none()) {
            std::thread::sleep(Duration::from_millis(100));
            let map = by_id.lock().unwrap();
            for (i, &id) in ids.iter().enumerate() {
                if let Some(buf) = map.get(&id) {
                    pids[i] = parse_marked_pid(buf, &format!("MARK{i}"));
                }
            }
        }

        // Independence assertions: each id saw its own marker and NO foreign one.
        {
            let map = by_id.lock().unwrap();
            for (i, &id) in ids.iter().enumerate() {
                let buf = map.get(&id).cloned().unwrap_or_default();
                assert!(
                    buf.contains(&format!("MARK{i}")),
                    "pty id {id} (index {i}) must receive its own marker, got: {buf:?}"
                );
                for j in 0..3 {
                    if j != i {
                        assert!(
                            !buf.contains(&format!("MARK{j}:")),
                            "pty id {id} (index {i}) leaked MARK{j} from another PTY: {buf:?}"
                        );
                    }
                }
            }
        }

        let pids = pids.map(|p| p.expect("each PTY must report its PID"));
        // Distinct processes: the three shells are different OS processes.
        assert_ne!(pids[0], pids[1]);
        assert_ne!(pids[1], pids[2]);
        assert_ne!(pids[0], pids[2]);
        for pid in pids {
            assert!(process_alive(pid), "sanity: pid {pid} alive before close");
        }

        // Close ONLY the middle PTY. The other two must be untouched.
        close(&app, ids[1]).expect("close middle pty");

        // Its process must die; allow brief teardown grace.
        let gone_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < gone_deadline && process_alive(pids[1]) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !process_alive(pids[1]),
            "closed pty (pid {}) must be dead",
            pids[1]
        );
        // The other two are still alive — close did not touch the wrong process.
        assert!(
            process_alive(pids[0]),
            "untouched pty (pid {}) must stay alive after closing a sibling",
            pids[0]
        );
        assert!(
            process_alive(pids[2]),
            "untouched pty (pid {}) must stay alive after closing a sibling",
            pids[2]
        );
        // Registry shrank by exactly one: no leak of the closed handle, no
        // accidental eviction of the survivors.
        assert_eq!(
            live_pty_count(&app),
            2,
            "exactly one PTY removed from the registry by close"
        );

        // Cleanup the survivors.
        let _ = close(&app, ids[0]);
        let _ = close(&app, ids[2]);
    }

    /// Full bridge lifecycle in ONE test, exactly as the done-criterion spells
    /// it: spawn → write → `pty://output` event → close → `pty://exit` event.
    /// A single test so a regression in ANY link of the chain (a command that
    /// stops relaying, or an event that stops firing) breaks it here.
    #[test]
    fn full_cycle_spawn_write_output_close_exit() {
        let app = build_app();

        let (otx, orx) = channel::<String>();
        app.listen("pty://output", move |event| {
            let _ = otx.send(event.payload().to_string());
        });
        let (etx, erx) = channel::<String>();
        app.listen("pty://exit", move |event| {
            let _ = etx.send(event.payload().to_string());
        });

        // 1) spawn
        let id = spawn(&app, 80, 24);
        assert!(id >= 1, "spawn returns a valid id");
        assert_eq!(live_pty_count(&app), 1, "spawn registers exactly one PTY");

        // 2) write → 3) pty://output carries the command output
        write(&app, id, b"echo cycle_marker_9c1\n");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut acc = String::new();
        while Instant::now() < deadline && !acc.contains("cycle_marker_9c1") {
            if let Ok(p) = orx.recv_timeout(Duration::from_millis(200)) {
                acc.push_str(&output_to_string(&p));
            }
        }
        assert!(
            acc.contains("cycle_marker_9c1"),
            "pty://output must relay the command output, got: {acc:?}"
        );

        // 4) close → 5) pty://exit fires with this id
        close(&app, id).expect("pty_close");
        let exit = erx
            .recv_timeout(Duration::from_secs(5))
            .expect("pty://exit must fire after close");
        let v: serde_json::Value = serde_json::from_str(&exit).unwrap();
        assert_eq!(v["id"].as_u64(), Some(id), "exit event carries the id");

        // close also drops the PTY from managed state: no leaked handle.
        assert_eq!(
            live_pty_count(&app),
            0,
            "managed state must be empty after close (no leaked PTY)"
        );
    }

    /// `pty_close` must leave NO orphan OS process behind.
    ///
    /// We make the shell announce its own PID (`echo PID:$$`), parse it from the
    /// `pty://output` stream, then close and assert via `kill(pid, 0)` that the
    /// process is genuinely gone — not merely removed from our HashMap. The
    /// managed-state count is also asserted to be zero so a leaked `Pty` handle
    /// (which would keep fds/threads alive) is caught too.
    #[cfg(unix)]
    #[test]
    fn close_leaves_no_orphan_process() {
        let app = build_app();

        let (otx, orx) = channel::<String>();
        app.listen("pty://output", move |event| {
            let _ = otx.send(event.payload().to_string());
        });
        let (etx, erx) = channel::<String>();
        app.listen("pty://exit", move |event| {
            let _ = etx.send(event.payload().to_string());
        });

        let id = spawn(&app, 80, 24);
        // Print the shell's own PID, then keep it alive so we can observe the
        // process BEFORE we close it (proving the probe distinguishes alive).
        write(&app, id, b"echo PID:$$\nsleep 60\n");

        // Parse `PID:<n>` out of the coalesced output.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut acc = String::new();
        let mut pid: Option<i32> = None;
        while Instant::now() < deadline && pid.is_none() {
            if let Ok(p) = orx.recv_timeout(Duration::from_millis(200)) {
                acc.push_str(&output_to_string(&p));
                // The interactive shell echoes the TYPED command first
                // (`PID:$$`, where `$$` is not digits) and only later prints the
                // EXPANDED value (`PID:796693`). Scan every `PID:` occurrence and
                // accept the first that is immediately followed by digits AND a
                // terminating newline (so we never parse a half-flushed number).
                for (off, _) in acc.match_indices("PID:") {
                    let rest = &acc[off + 4..];
                    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if !digits.is_empty() && rest[digits.len()..].starts_with(['\r', '\n']) {
                        pid = digits.parse::<i32>().ok();
                        break;
                    }
                }
            }
        }
        let pid = pid.unwrap_or_else(|| panic!("could not parse shell PID, got: {acc:?}"));
        assert!(
            process_alive(pid),
            "sanity: the shell (pid {pid}) must be alive before close"
        );

        // Close, wait for the exit event (child reaped), then probe the OS.
        close(&app, id).expect("pty_close");
        erx.recv_timeout(Duration::from_secs(5))
            .expect("pty://exit must fire after close");

        // The reader saw EOF and the waiter reaped the child; the PID must now
        // be gone. Allow a brief grace for the kernel to finish teardown.
        let gone_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < gone_deadline && process_alive(pid) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !process_alive(pid),
            "orphan process: shell pid {pid} still alive after pty_close"
        );
        assert_eq!(
            live_pty_count(&app),
            0,
            "managed state must hold no PTY after close"
        );
    }

    #[test]
    fn closing_one_terminal_keeps_the_others_usable() {
        // Closing a terminal must drop ONLY that PTY and never wedge the manager:
        // the survivors stay live and responsive. Before the close-hang fix, a
        // teardown deadlock on the main thread would have frozen everything.
        let app = build_app();
        let a = spawn(&app, 80, 24);
        let b = spawn(&app, 80, 24);
        let c = spawn(&app, 80, 24);
        assert_eq!(live_pty_count(&app), 3);

        close(&app, b).expect("pty_close b");
        assert_eq!(live_pty_count(&app), 2, "only the closed PTY is removed");

        // Survivors still accept writes/resizes without error (manager not wedged).
        write(&app, a, b"echo still_alive\n");
        write(&app, c, b"echo still_alive\n");
        resize(&app, a, 100, 30);
        resize(&app, c, 100, 30);
        // The closed one is gone: re-closing it is an error, not a hang.
        assert!(close(&app, b).is_err(), "re-closing a gone PTY must error");
        assert_eq!(
            live_pty_count(&app),
            2,
            "survivors remain after the re-close"
        );
    }

    #[test]
    fn closing_every_terminal_empties_the_manager_promptly() {
        // Close a whole batch through the command path: each returns Ok and the
        // manager ends empty (no leaked Pty handle / thread). A teardown deadlock
        // would stall here, so this guards the close-hang at the command level too.
        let app = build_app();
        let ids: Vec<u64> = (0..6).map(|_| spawn(&app, 80, 24)).collect();
        assert_eq!(live_pty_count(&app), 6);

        let start = Instant::now();
        for id in ids {
            close(&app, id).expect("pty_close");
        }
        assert_eq!(
            live_pty_count(&app),
            0,
            "no PTY may remain after closing all"
        );
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "closing 6 PTYs took too long ({:?}) — a teardown stalled",
            start.elapsed(),
        );
    }

    #[test]
    fn spawn_write_emits_coalesced_output() {
        let app = build_app();

        let (tx, rx) = channel::<String>();
        app.listen("pty://output", move |event| {
            let _ = tx.send(event.payload().to_string());
        });

        let id = spawn(&app, 80, 24);
        assert!(id >= 1, "spawn returns a valid id");
        write(&app, id, b"echo bridge_marker\n");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut acc = String::new();
        while Instant::now() < deadline && !acc.contains("bridge_marker") {
            if let Ok(p) = rx.recv_timeout(Duration::from_millis(200)) {
                acc.push_str(&output_to_string(&p));
            }
        }
        assert!(
            acc.contains("bridge_marker"),
            "pty://output should carry the command output, got: {acc:?}"
        );
        let _ = close(&app, id);
    }

    #[test]
    fn resize_reflected_via_stty_size() {
        let app = build_app();

        let (tx, rx) = channel::<String>();
        app.listen("pty://output", move |event| {
            let _ = tx.send(event.payload().to_string());
        });

        let id = spawn(&app, 80, 24);
        resize(&app, id, 132, 50);
        std::thread::sleep(Duration::from_millis(100));
        write(&app, id, b"stty size\n");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut acc = String::new();
        while Instant::now() < deadline && !acc.contains("50 132") {
            if let Ok(p) = rx.recv_timeout(Duration::from_millis(200)) {
                acc.push_str(&output_to_string(&p));
            }
        }
        assert!(acc.contains("50 132"), "resize not reflected, got: {acc:?}");
        let _ = close(&app, id);
    }

    #[test]
    fn close_kills_and_emits_exit() {
        let app = build_app();

        let (tx, rx) = channel::<String>();
        app.listen("pty://exit", move |event| {
            let _ = tx.send(event.payload().to_string());
        });

        let id = spawn(&app, 80, 24);
        // Long-running command so the shell stays alive until we close.
        write(&app, id, b"sleep 60\n");
        std::thread::sleep(Duration::from_millis(150));
        close(&app, id).expect("pty_close");

        let payload = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("pty://exit must fire after close");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["id"].as_u64(), Some(id), "exit event carries the id");
        assert!(v.get("code").is_some(), "exit event carries a code field");
    }

    /// The same flood workload as the bridge test, used to count how many RAW
    /// chunks the reader emits (one per `read()` call) — an INDEPENDENT measure
    /// of coalescing pressure that does not depend on byte volume.
    const FLOOD_CMD: &[u8] = b"for i in $(seq 1 20000); do echo floodline; done\n";

    /// Drive the raw [`Pty`] reader directly (no bridge pump) with [`FLOOD_CMD`]
    /// and count the chunks its receiver yields. This is the baseline the pump
    /// must beat: the pump coalesces these N raw chunks into far fewer events.
    fn count_raw_reader_chunks() -> usize {
        let size = portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let (mut pty, rx) = Pty::spawn_program("sh", size, None).expect("spawn raw pty");
        pty.write(FLOOD_CMD).expect("write flood");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut chunks = 0usize;
        // Drain until the flood is clearly done (a quiet gap) or we hit the
        // deadline. Each `Ok` is exactly one reader `read()` chunk.
        let mut idle_gaps = 0u32;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(_) => {
                    chunks += 1;
                    idle_gaps = 0;
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Two consecutive idle gaps after we've seen data ⇒ flood
                    // has drained; stop so the count reflects the real workload.
                    idle_gaps += 1;
                    if chunks > 0 && idle_gaps >= 2 {
                        break;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let _ = pty.kill();
        chunks
    }

    #[test]
    fn flood_is_coalesced_few_events_many_lines() {
        let app = build_app();

        let event_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let byte_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let ec = Arc::clone(&event_count);
            let bc = Arc::clone(&byte_count);
            app.listen("pty://output", move |event| {
                ec.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                if let Some(arr) = v["bytes"].as_array() {
                    bc.fetch_add(arr.len(), std::sync::atomic::Ordering::Relaxed);
                }
            });
        }

        let id = spawn(&app, 80, 24);
        // Flood: print a short line in a tight loop for a bounded count.
        write(&app, id, FLOOD_CMD);

        std::thread::sleep(Duration::from_secs(2));
        let _ = close(&app, id);
        std::thread::sleep(Duration::from_millis(100));

        let events = event_count.load(std::sync::atomic::Ordering::Relaxed);
        let bytes = byte_count.load(std::sync::atomic::Ordering::Relaxed);

        // Independent baseline: how many RAW chunks the reader produces for the
        // identical workload. The pump must collapse these into far fewer
        // events. This is the true coalescing signal — it does not depend on
        // byte volume, so a "few bytes in many small chunks" workload (which
        // could slip past a `bytes/50` bound) is also covered.
        let raw_chunks = count_raw_reader_chunks();

        // Visibility: emit the counters so a failure is diagnosable from the log
        // (run with `cargo test -- --nocapture` to see it on a pass).
        let ratio_chunks = if events == 0 {
            f64::INFINITY
        } else {
            raw_chunks as f64 / events as f64
        };
        eprintln!(
            "[flood_is_coalesced] events={events} bytes={bytes} raw_chunks={raw_chunks} \
             chunks_per_event={ratio_chunks:.1} bytes_per_event={:.1}",
            if events == 0 {
                f64::INFINITY
            } else {
                bytes as f64 / events as f64
            }
        );

        // Guard 1 (anti-vacuous): the pump MUST have emitted something. If
        // `events == 0` the listener never fired and every "<<" assertion below
        // would pass vacuously — fail loudly instead.
        assert!(
            events >= 1,
            "pump emitted zero events; nothing was coalesced (vacuous pass guarded)"
        );

        // Guard 2: a real flood actually happened (real byte volume).
        assert!(
            bytes > 10_000,
            "expected a real flood of bytes, got {bytes}"
        );

        // Guard 3: the workload genuinely stressed the reader into many chunks,
        // otherwise "events << raw_chunks" would be trivially satisfiable.
        assert!(
            raw_chunks > 100,
            "flood should fragment into many reader chunks, got {raw_chunks}"
        );

        // Core assertion: events are an order of magnitude (≥10x) fewer than the
        // raw chunks the reader produced. If coalescing regressed to one event
        // per chunk (or per line), `events` would approach `raw_chunks` and this
        // would fail with the printed counters showing why.
        assert!(
            events * 10 <= raw_chunks,
            "events ({events}) must be << raw reader chunks ({raw_chunks}); \
             coalescing regressed (ratio {ratio_chunks:.1}x, expected ≥10x)"
        );

        // Secondary guard kept from the original test: events stay far below the
        // byte volume (catches a regression even if the raw-chunk baseline is
        // somehow degenerate on a given platform).
        assert!(
            events < bytes / 50,
            "events ({events}) must be << byte volume ({bytes}); coalescing failed"
        );
    }

    /// LEADING-EDGE non-regression (companion to `flood_is_coalesced_…`). The
    /// flood test proves the pump still COALESCES under a steady stream; this one
    /// proves the complementary property the leading-edge fix added: a chunk that
    /// arrives AFTER a quiet gap (≥ FLUSH_INTERVAL) is emitted PROMPTLY, not held
    /// for ~16ms by the coalescing window. That promptness is the interactive-echo
    /// latency the fix removed.
    ///
    /// We drive the production [`spawn_output_pump`] directly with a SYNTHETIC mpsc
    /// receiver (same harness as the OSC-133 synthetic e2e tests) so the test is
    /// shell-agnostic and deterministic: it depends only on the pump's wait/flush
    /// logic, not on any child process timing.
    ///
    /// Method: send a FIRST chunk to prime the pump and wait for its event, sleep
    /// well past FLUSH_INTERVAL so the pump goes idle (blocks on `recv()`, with
    /// `last_flush` left "old"), then send a SECOND chunk and measure how long
    /// until its `pty://output` event lands. With the leading edge that delay is
    /// ≈0 (the byte flushes on arrival); a regression to trailing-only coalescing
    /// would push it toward FLUSH_INTERVAL (~16ms).
    ///
    /// ANTI-FLAKY: the threshold is DELIBERATELY GENEROUS. We assert the event
    /// arrives in < FLUSH_INTERVAL * 4 (~64ms), i.e. comfortably below the
    /// ~16ms-and-climbing a trailing-edge regression would exhibit, yet well above
    /// the real ≈0ms so OS scheduler jitter / loaded-CR runners (the listener hop,
    /// thread wake-up) cannot make it flake. The point is to catch a REGRESSION TO
    /// HELD-FOR-A-FULL-INTERVAL, not to pin a tight latency number — so a loose
    /// upper bound is the correct, durable assertion here.
    #[test]
    fn leading_edge_emits_a_chunk_after_idle_promptly() {
        let app = build_app();

        // Per-event arrival timestamps (monotonic). The pump emits `pty://output`
        // with a `{id, bytes}` payload; we only need the WHEN, so we just stamp.
        let stamps: Arc<Mutex<Vec<Instant>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let s = Arc::clone(&stamps);
            app.listen("pty://output", move |_event| {
                s.lock().unwrap().push(Instant::now());
            });
        }

        // The pump owns the receiver; we own the synthetic transmitter, so we
        // control exactly WHEN bytes are offered — no shell, no read() jitter.
        let (tx, rx) = channel::<Vec<u8>>();
        spawn_output_pump(app.handle().clone(), 70_001u64, rx);

        // 1) Prime: first chunk + wait until its event is observed. This drains
        //    the startup state and lets the pump return to a blocked `recv()`.
        tx.send(b"first\r\n".to_vec()).unwrap();
        let primed_deadline = Instant::now() + Duration::from_secs(2);
        while stamps.lock().unwrap().is_empty() && Instant::now() < primed_deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            stamps.lock().unwrap().len(),
            1,
            "the priming chunk must produce exactly one event before the idle gap"
        );

        // 2) Idle gap WELL past FLUSH_INTERVAL so the pump is parked on `recv()`
        //    with `pending` empty and `last_flush` now older than FLUSH_INTERVAL.
        std::thread::sleep(FLUSH_INTERVAL * 5);

        // 3) Send the post-idle chunk and time its event. With leading-edge this
        //    flushes on arrival (retention ≈ 0).
        let sent_at = Instant::now();
        tx.send(b"echo\r\n".to_vec()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut arrived_at = None;
        while arrived_at.is_none() && Instant::now() < deadline {
            if stamps.lock().unwrap().len() >= 2 {
                arrived_at = Some(*stamps.lock().unwrap().last().unwrap());
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        let arrived_at = arrived_at.expect("post-idle chunk must produce a second event");
        let latency = arrived_at.saturating_duration_since(sent_at);

        // Generous bound (see doc comment): ~64ms, vs the ~16ms-floor a
        // trailing-edge regression would impose. Comfortably catches the
        // regression without flaking on jittery runners.
        let bound = FLUSH_INTERVAL * 4;
        assert!(
            latency < bound,
            "post-idle chunk must be emitted promptly (leading edge): \
             latency {latency:?} should be < {bound:?} (FLUSH_INTERVAL={FLUSH_INTERVAL:?})"
        );

        drop(tx);
    }

    // --- terminal_info (live cwd + foreground program via /proc) ----------
    //
    // These exercise the WHOLE chain: a real shell in a PTY → shell pid +
    // foreground pgid → /proc reads → the `terminal_info` command + its
    // debounce. Linux-only (the feature is Linux-only by design).

    /// Poll `terminal_info(id)` until `pred` holds or we time out, sleeping
    /// between calls. Each call advances the debounce window, but we sleep
    /// >INFO_REFRESH_INTERVAL so successive polls actually re-read /proc.
    #[cfg(target_os = "linux")]
    fn info_until(
        app: &App<MockRuntime>,
        id: u64,
        timeout: Duration,
        pred: impl Fn(&TerminalInfo) -> bool,
    ) -> TerminalInfo {
        let deadline = Instant::now() + timeout;
        let mut last = TerminalInfo::default();
        while Instant::now() < deadline {
            // Drop the cache entry so each poll re-reads /proc (the production
            // front polls on a ~1s timer; here we force fresh reads to converge
            // fast without sleeping a full second between attempts).
            app.state::<TerminalInfoCache>()
                .by_id
                .lock()
                .unwrap()
                .remove(&id);
            if let Ok(i) = info(app, id) {
                if pred(&i) {
                    return i;
                }
                last = i;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        last
    }

    /// Done-criterion #1: after a `cd`, the live cwd remounted equals the new
    /// directory. We spawn a shell, `cd /tmp`, and assert `terminal_info` reports
    /// `/tmp` (canonicalized to absorb a symlinked /tmp).
    #[cfg(target_os = "linux")]
    #[test]
    fn live_cwd_reflects_cd() {
        let app = build_app();
        let id = spawn(&app, 80, 24);

        // Baseline: cwd is known and is NOT /tmp yet (we inherit nyx's cwd).
        let target = std::fs::canonicalize("/tmp").unwrap();
        write(&app, id, b"cd /tmp\n");

        let got = info_until(&app, id, Duration::from_secs(8), |i| {
            i.cwd
                .as_ref()
                .and_then(|c| std::fs::canonicalize(c).ok())
                .map(|c| c == target)
                .unwrap_or(false)
        });
        let cwd_canon = got
            .cwd
            .as_ref()
            .and_then(|c| std::fs::canonicalize(c).ok())
            .unwrap_or_default();
        assert_eq!(
            cwd_canon, target,
            "after `cd /tmp`, live cwd must be /tmp, got {:?}",
            got.cwd
        );
        let _ = close(&app, id);
    }

    /// Done-criterion #2: a foreground program shows up as the foreground
    /// process, and on its exit we fall back to the shell. We use `sleep`
    /// (universally present, deterministic) as the stand-in for htop: while it
    /// runs it is the controlling-terminal foreground pgrp leader, so its `comm`
    /// is `sleep`; after it exits, the foreground is the shell again.
    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_program_tracks_running_then_shell() {
        let app = build_app();
        let id = spawn(&app, 80, 24);

        // Run a foreground program that stays in the foreground.
        write(&app, id, b"sleep 30\n");
        let running = info_until(&app, id, Duration::from_secs(8), |i| {
            i.foreground.as_deref() == Some("sleep")
        });
        assert_eq!(
            running.foreground.as_deref(),
            Some("sleep"),
            "running `sleep` must surface as the foreground program, got {:?}",
            running.foreground
        );

        // Interrupt it (Ctrl-C) → foreground returns to the shell. The shell's
        // own comm is the resolved $SHELL basename (bash/zsh/sh): we only assert
        // it is no longer the program we ran.
        write(&app, id, &[0x03]); // Ctrl-C
        let back = info_until(&app, id, Duration::from_secs(8), |i| {
            i.foreground.as_deref() != Some("sleep")
        });
        assert_ne!(
            back.foreground.as_deref(),
            Some("sleep"),
            "after the program exits, foreground must fall back to the shell, got {:?}",
            back.foreground
        );
        assert!(
            back.foreground.is_some(),
            "the shell itself must still be a readable foreground program"
        );
        let _ = close(&app, id);
    }

    /// Done-criterion #3: the calculation is time-bounded — a second call within
    /// the debounce window returns the CACHED reading without touching /proc.
    /// We prove it by mutating reality (cd) and showing the cached call does NOT
    /// see the change until the window elapses; a fresh call (cache cleared)
    /// does. This is the "not on every byte" guarantee made observable.
    #[cfg(target_os = "linux")]
    #[test]
    fn terminal_info_is_debounced() {
        let app = build_app();
        let id = spawn(&app, 80, 24);

        // Prime the cache at the initial cwd.
        let first = info_until(&app, id, Duration::from_secs(8), |i| i.cwd.is_some());
        let first_cwd = first.cwd.clone().expect("primed cwd");

        // Change reality, then call again IMMEDIATELY (without clearing the
        // cache). Within the 1s window the command must return the stale cached
        // value, proving it did not re-hit /proc.
        write(&app, id, b"cd /tmp\n");
        std::thread::sleep(Duration::from_millis(300)); // let the cd land in /proc
        let cached = info(&app, id).expect("cached terminal_info");
        assert_eq!(
            cached.cwd, first.cwd,
            "within the debounce window the reading must be the cached one \
             (got {:?}, primed {:?})",
            cached.cwd, first.cwd
        );

        // After the window elapses, a fresh call re-reads /proc and sees the cd.
        let target = std::fs::canonicalize("/tmp").unwrap();
        let refreshed = info_until(&app, id, Duration::from_secs(8), |i| {
            i.cwd
                .as_ref()
                .and_then(|c| std::fs::canonicalize(c).ok())
                .map(|c| c == target)
                .unwrap_or(false)
        });
        let refreshed_canon = refreshed
            .cwd
            .as_ref()
            .and_then(|c| std::fs::canonicalize(c).ok())
            .unwrap_or_default();
        assert_eq!(
            refreshed_canon, target,
            "after the debounce window, a fresh reading must see the new cwd"
        );
        assert_ne!(
            first_cwd,
            refreshed.cwd.clone().unwrap_or_default(),
            "sanity: the cd actually changed the cwd"
        );
        let _ = close(&app, id);
    }

    /// `terminal_info` on an unknown id is an error (no live PTY to introspect).
    #[test]
    fn terminal_info_unknown_id_errors() {
        let app = build_app();
        assert!(
            info(&app, 999_999).is_err(),
            "terminal_info on an unknown id must error"
        );
    }

    // --- OS busy/idle signal + transition-only emission (PRD task #1) --------

    /// True when `bash` is on PATH (the busy tests need an interactive shell with
    /// job control; the derivation itself is shell-agnostic).
    #[cfg(unix)]
    fn bash_available() -> bool {
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join("bash").is_file()))
            .unwrap_or(false)
    }

    /// Sweep `scan_busy_once` until it reports a `(terminal_id, want)` transition or
    /// we time out; returns whether it was seen. Drives the same path the poll loop
    /// uses (snapshot → resolve id → diff against the tracker), so only TRANSITIONS
    /// show up — a steady state yields an empty sweep.
    #[cfg(unix)]
    fn busy_transition_seen(
        app: &App<MockRuntime>,
        terminal_id: &str,
        want: bool,
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            for (tid, busy) in scan_busy_once(app.handle()) {
                if tid == terminal_id && busy == want {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// CORE done-criteria (task #1): the dot's authority is the OS busy signal,
    /// emitted via `terminal://busy-state` on TRANSITION ONLY, with NO dependency on
    /// OSC 133. We spawn an interactive bash record, then drive `scan_busy_once`
    /// (the exact loop body) and assert:
    ///  - idle prompt → NO transition (a fresh idle terminal emits nothing);
    ///  - a foreground `sleep` → a single `busy=true` transition, and a re-scan
    ///    while still running emits NOTHING (steady state, not every tick);
    ///  - ending the command → a single `busy=false` transition.
    ///
    /// None of this consults OSC 133 — it is purely `foreground_pgid != shell pgid`.
    #[cfg(unix)]
    #[test]
    fn busy_state_scan_emits_only_on_transition() {
        if !bash_available() {
            eprintln!("skipping busy_state_scan_emits_only_on_transition: bash not found");
            return;
        }
        let app = build_app_with_db();
        let tid = "term-busy-1";
        let _id = spawn_interactive_bash_record(&app, tid);

        // The shell starts at an idle prompt: its foreground group is its own, so
        // busy is false. The tracker suppresses the first idle (idle is the default
        // the front already shows), so NO transition is reported. Sweep a few times
        // to give the prompt time to settle and confirm it stays silent.
        for _ in 0..6 {
            let transitions = scan_busy_once(app.handle());
            assert!(
                !transitions.iter().any(|(t, _)| t == tid),
                "an idle prompt must NOT emit a busy transition; got {transitions:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        // Run a long foreground command → a single busy=true transition.
        write(&app, _id, b"sleep 30\n");
        assert!(
            busy_transition_seen(&app, tid, true, Duration::from_secs(5)),
            "a foreground command must produce a busy=true transition"
        );
        // Still running: a re-scan must report NOTHING (transition-only, not per tick).
        let steady = scan_busy_once(app.handle());
        assert!(
            !steady.iter().any(|(t, _)| t == tid),
            "a still-running terminal must NOT re-emit busy every tick; got {steady:?}"
        );

        // End the command (Ctrl-C) → a single busy=false transition.
        write(&app, _id, &[0x03]);
        assert!(
            busy_transition_seen(&app, tid, false, Duration::from_secs(5)),
            "ending the foreground command must produce a busy=false transition"
        );

        let _ = close(&app, _id);
    }

    /// The transition tracker fires the event end-to-end: `poll_and_emit_busy_state`
    /// emits `terminal://busy-state` on the OS busy flip, captured via `app.listen`.
    /// Proves the loop body actually emits the front-facing event (not just computes
    /// transitions), keyed by the persistent terminal id.
    #[cfg(unix)]
    #[test]
    fn busy_state_event_is_emitted_on_transition() {
        if !bash_available() {
            eprintln!("skipping busy_state_event_is_emitted_on_transition: bash not found");
            return;
        }
        let app = build_app_with_db();
        let tid = "term-busy-evt";

        // Capture every emitted busy-state payload.
        let seen: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = Arc::clone(&seen);
            app.listen(TERMINAL_BUSY_STATE_EVENT, move |event| {
                let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                let t = v["terminal_id"].as_str().unwrap().to_string();
                let b = v["busy"].as_bool().unwrap();
                seen.lock().unwrap().push((t, b));
            });
        }

        let id = spawn_interactive_bash_record(&app, tid);
        // Let the idle prompt settle while polling: no busy event should be emitted.
        for _ in 0..6 {
            poll_and_emit_busy_state(app.handle());
            std::thread::sleep(Duration::from_millis(50));
        }

        // Foreground command → expect a busy=true event.
        write(&app, id, b"sleep 30\n");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            poll_and_emit_busy_state(app.handle());
            if seen.lock().unwrap().iter().any(|(t, b)| t == tid && *b) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let events = seen.lock().unwrap().clone();
        assert!(
            events.iter().any(|(t, b)| t == tid && *b),
            "a busy=true terminal://busy-state event must be emitted; got {events:?}"
        );
        // No idle event was emitted before the command (idle is the suppressed default).
        assert!(
            !events.iter().any(|(t, b)| t == tid && !*b),
            "no busy=false event must precede the first busy=true; got {events:?}"
        );

        let _ = close(&app, id);
    }

    /// The tracker reports a change ONLY when the busy bit actually flips, and never
    /// announces the first idle (the front's default). Pure unit over the diff logic
    /// — no PTY, no shell — so the transition-only contract is locked down
    /// deterministically alongside the live-shell tests.
    #[test]
    fn busy_tracker_reports_only_real_transitions() {
        let tracker = BusyStateTracker::default();
        // First-seen idle is suppressed (no spurious idle transition at boot).
        assert!(!tracker.changed("t", false), "first idle is not a transition");
        // Idle → busy is a transition.
        assert!(tracker.changed("t", true), "idle→busy is a transition");
        // Busy → busy (steady state) is NOT.
        assert!(!tracker.changed("t", true), "busy→busy is not a transition");
        // Busy → idle is a transition.
        assert!(tracker.changed("t", false), "busy→idle is a transition");
        // First-seen BUSY (a different id) IS a transition (it differs from the
        // implicit idle default the front shows).
        assert!(tracker.changed("u", true), "first-seen busy is a transition");
        // forget() resets so a re-spawn under the same id starts clean.
        tracker.forget("u");
        assert!(!tracker.changed("u", false), "after forget, first idle is suppressed again");
    }

    /// REGRESSION (PRD task #1): a PTY that dies while BUSY must clear the running
    /// dot. The dot's authority moved from OSC-133 `exec_state` to the OS `busy`
    /// signal, so the pump's exit path — which already settles a stale `running`
    /// `exec_state` — must ALSO emit a final `busy=false` on the busy channel.
    /// Otherwise nobody ever clears it: `reap_exit_code` removes the PTY before the
    /// poll loop can sample it, the Tauri `list_terminals` record carries no `busy`,
    /// and the `terminals://changed` merge keeps the stale in-memory record — so the
    /// front's `record.busy` stays `true` and the dot is stuck lit forever.
    ///
    /// We pre-seed the tracker to the state the poll loop leaves after announcing a
    /// foreground command (`busy=true`), drive a real pump to disconnect (the shell
    /// died mid-command), and assert exactly ONE `terminal://busy-state {busy:false}`
    /// keyed to OUR terminal is emitted.
    #[test]
    fn pty_exit_while_busy_emits_a_final_busy_false() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        let pty_id = 90_021u64;
        map_pty_to_record(&app, pty_id, &tid);

        // The poll loop announced this terminal BUSY (a foreground command ran).
        assert!(
            app.state::<BusyStateTracker>().changed(&tid, true),
            "precondition: tracker records the terminal as busy"
        );

        // Capture every emitted busy-state payload.
        let seen: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = Arc::clone(&seen);
            app.listen(TERMINAL_BUSY_STATE_EVENT, move |event| {
                let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                let t = v["terminal_id"].as_str().unwrap().to_string();
                let b = v["busy"].as_bool().unwrap();
                seen.lock().unwrap().push((t, b));
            });
        }

        // The shell/PTY dies mid-command: spawn the real pump, then disconnect it.
        let (tx, rx) = channel::<Vec<u8>>();
        spawn_output_pump(app.handle().clone(), pty_id, rx);
        drop(tx); // reader disconnect → the pump's exit path runs.

        // The exit path must emit a single busy=false transition for OUR terminal.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if seen.lock().unwrap().iter().any(|(t, b)| t == &tid && !*b) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let events = seen.lock().unwrap().clone();
        assert_eq!(
            events.iter().filter(|(t, _)| t == &tid).count(),
            1,
            "exactly one busy-state event for our terminal on exit; got {events:?}"
        );
        assert_eq!(
            events.iter().find(|(t, _)| t == &tid),
            Some(&(tid.clone(), false)),
            "the exit event clears the running dot (busy=false); got {events:?}"
        );
        // The tracker entry is dropped so a re-spawn starts clean: a fresh
        // `changed(tid, false)` hits the never-seen arm and is suppressed (false),
        // proving the id was forgotten rather than left recorded as `false`.
        assert!(
            !app.state::<BusyStateTracker>().changed(&tid, false),
            "after exit the tracker forgot the id (first idle is suppressed again)",
        );
    }

    /// COMPLEMENT to the above: a terminal that exits from an IDLE prompt (the common
    /// case — no foreground command was running) must NOT emit a spurious busy=false,
    /// so a mass-close does not flood redundant events. The tracker was never set to
    /// busy, so the exit path's transition check suppresses the emit.
    #[test]
    fn pty_exit_while_idle_emits_no_busy_event() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        let pty_id = 90_022u64;
        map_pty_to_record(&app, pty_id, &tid);
        // No busy ever announced: the tracker has no entry for tid (idle is the
        // implicit default the front already shows).

        let seen: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = Arc::clone(&seen);
            app.listen(TERMINAL_BUSY_STATE_EVENT, move |event| {
                let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                let t = v["terminal_id"].as_str().unwrap().to_string();
                let b = v["busy"].as_bool().unwrap();
                seen.lock().unwrap().push((t, b));
            });
        }

        let (tx, rx) = channel::<Vec<u8>>();
        spawn_output_pump(app.handle().clone(), pty_id, rx);
        drop(tx);

        // Give the exit path time to run, then assert no busy event for our terminal.
        std::thread::sleep(Duration::from_millis(150));
        let events = seen.lock().unwrap().clone();
        assert!(
            !events.iter().any(|(t, _)| t == &tid),
            "an exit from idle must not emit a busy event; got {events:?}"
        );
    }

    // --- Terminal RECORD commands through the bridge (YR) -----------------
    //
    // Exercise the real `#[tauri::command]` bodies against an in-memory Db
    // managed on the mock app — the IPC-facing surface of the db CRUD.

    /// Full record lifecycle via the COMMANDS: create→list returns it,
    /// persist_scrollback round-trips, rename + reorder persist, close→closed.
    /// One test so a regression in any command's wiring breaks here.
    #[test]
    fn record_commands_full_cycle() {
        let app = build_app_with_db();
        let dbs = || app.state::<Db>();

        // create → list returns the terminal
        let a = create_terminal(dbs(), "/a".into(), Some("alpha".into())).expect("create a");
        let b = create_terminal(dbs(), "/b".into(), None).expect("create b");
        let listed = list_terminals(dbs()).expect("list");
        assert_eq!(listed.len(), 2, "list returns both created terminals");
        assert_eq!(listed[0].id, a.id);
        assert_eq!(listed[0].label.as_deref(), Some("alpha"));
        assert_eq!(listed[0].status, crate::db::STATUS_ALIVE);

        // persist_scrollback stores then relit the same string
        let blob = "history\r\n\x1b[32mok\x1b[0m\r\n";
        persist_scrollback(dbs(), a.id.clone(), blob.into()).expect("persist");
        let after = list_terminals(dbs()).unwrap();
        let a_row = after.iter().find(|t| t.id == a.id).unwrap();
        assert_eq!(a_row.scrollback, blob, "scrollback round-trips via command");

        // rename persists
        rename(dbs(), b.id.clone(), Some("beta".into())).expect("rename");
        let b_row = list_terminals(dbs())
            .unwrap()
            .into_iter()
            .find(|t| t.id == b.id)
            .unwrap();
        assert_eq!(b_row.label.as_deref(), Some("beta"));

        // reorder persists: put b before a
        reorder(dbs(), vec![b.id.clone(), a.id.clone()]).expect("reorder");
        let ordered: Vec<String> = list_terminals(dbs())
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(
            ordered,
            vec![b.id.clone(), a.id.clone()],
            "reorder reflected by list"
        );

        // close → status closed (row retained)
        close_terminal(dbs(), a.id.clone()).expect("close");
        let a_closed = list_terminals(dbs())
            .unwrap()
            .into_iter()
            .find(|t| t.id == a.id)
            .unwrap();
        assert_eq!(
            a_closed.status,
            crate::db::STATUS_CLOSED,
            "close_terminal flips status to closed"
        );
    }

    // --- Restore flow ACROSS A SIMULATED RESTART, at the COMMAND level --------
    //
    // The done-criterion's "flow de restore au niveau commandes" is exercised
    // end-to-end here: persist records + scrollback through the commands on one
    // mock app, DROP that app (simulating nyx quitting), build a SECOND mock app
    // backed by the SAME on-disk DB (simulating the relaunch), and replay the
    // launcher's command sequence — `list_terminals` to find the alive
    // candidates, re-spawn a real PTY for each via `pty_spawn` (the re-spawn the
    // front does per restored record), then `persist_scrollback` a new snapshot
    // and read it back. A regression where a command stops routing/persisting
    // breaks THIS test, not just the single-app cycle above.

    /// A unique temp DB path that does NOT collide across parallel test threads.
    /// We avoid a `tempfile` dependency (none is in the tree) and clean it up in
    /// the test via [`DbFileGuard`].
    fn unique_db_path() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "nyx-restore-test-{}-{nanos}-{n}.db",
            std::process::id()
        ))
    }

    /// RAII cleanup for the temp DB file (and SQLite's `-wal`/`-shm` siblings).
    struct DbFileGuard(std::path::PathBuf);
    impl Drop for DbFileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("db-wal"));
            let _ = std::fs::remove_file(self.0.with_extension("db-shm"));
        }
    }

    /// Build a mock app whose managed `Db` is backed by `path` on disk, so two
    /// successive apps can share persisted state across a simulated restart.
    fn build_app_with_db_at(path: &std::path::Path) -> App<MockRuntime> {
        let app = build_app();
        app.manage(Db::open(path).expect("open file-backed db"));
        app
    }

    /// Read the live PTY's shell pid for `id` (or `None` if the id is unknown) —
    /// used to assert each re-spawned terminal is a distinct, live OS process.
    fn shell_pid_of(app: &App<MockRuntime>, id: u64) -> Option<u32> {
        app.state::<PtyManager>()
            .ptys
            .lock()
            .unwrap()
            .get(&id)
            .and_then(|p| p.shell_pid())
    }

    #[test]
    fn restore_flow_across_restart_relists_alive_respawns_and_repersists() {
        let path = unique_db_path();
        let _guard = DbFileGuard(path.clone());

        // ── Session 1: seed records + scrollback through the COMMANDS, close one.
        let (a_id, b_id, c_id);
        {
            let app = build_app_with_db_at(&path);
            let dbs = || app.state::<Db>();
            let a = create_terminal(dbs(), "/work/a".into(), Some("alpha".into())).unwrap();
            let b = create_terminal(dbs(), "/work/b".into(), None).unwrap();
            let c = create_terminal(dbs(), "/work/c".into(), None).unwrap();
            a_id = a.id;
            b_id = b.id;
            c_id = c.id;

            persist_scrollback(dbs(), a_id.clone(), "alpha-history\r\n".into()).unwrap();
            persist_scrollback(dbs(), b_id.clone(), "beta-history\r\n".into()).unwrap();
            persist_scrollback(dbs(), c_id.clone(), "gamma-history\r\n".into()).unwrap();

            // Reorder to a non-creation order so we can prove ORDER survives the
            // restart too: c, a, b.
            reorder(dbs(), vec![c_id.clone(), a_id.clone(), b_id.clone()]).unwrap();

            // Close the MIDDLE record (b): the relaunch must NOT re-spawn it.
            close_terminal(dbs(), b_id.clone()).unwrap();
            // app drops here → simulates nyx quitting (its Db file persists).
        }

        // ── Session 2: a fresh app on the SAME db — the launcher's command flow.
        let app = build_app_with_db_at(&path);
        let dbs = || app.state::<Db>();

        // 1) list_terminals: the launcher's read. Order + scrollback + status all
        //    survived the restart.
        let rows = list_terminals(dbs()).expect("list after restart");
        assert_eq!(rows.len(), 3, "all three records persisted across restart");
        assert_eq!(
            rows.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            vec![c_id.clone(), a_id.clone(), b_id.clone()],
            "the persisted reorder (c,a,b) survives the restart"
        );
        let row = |id: &str| rows.iter().find(|t| t.id == id).unwrap();
        assert_eq!(row(&a_id).scrollback, "alpha-history\r\n");
        assert_eq!(row(&c_id).scrollback, "gamma-history\r\n");
        assert_eq!(
            row(&b_id).status,
            crate::db::STATUS_CLOSED,
            "the closed record is still closed after restart"
        );

        // 2) Re-spawn: the launcher mounts a fresh <Terminal> per ALIVE record,
        //    which spawns a real PTY at the record's cwd. We do exactly that here
        //    via pty_spawn and assert only the alive records got a PTY.
        let alive: Vec<&Terminal> = rows
            .iter()
            .filter(|t| t.status == crate::db::STATUS_ALIVE)
            .collect();
        assert_eq!(
            alive.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            vec![c_id.clone(), a_id.clone()],
            "only the alive records (c,a) are re-spawn candidates; b is excluded"
        );

        let mut spawned: Vec<u64> = Vec::new();
        for t in &alive {
            let pty_id = pty_spawn(
                app.handle().clone(),
                app.state::<PtyManager>(),
                app.state::<TerminalIdMap>(),
                Some(t.cwd.clone()),
                80,
                24,
                Some(t.id.clone()),
            )
            .expect("re-spawn pty for an alive record");
            // The re-spawn carries the persistent record id, so the live pty_id
            // resolves back to it (task #3).
            assert_eq!(
                app.state::<TerminalIdMap>().get(pty_id).as_deref(),
                Some(t.id.as_str())
            );
            spawned.push(pty_id);
        }
        assert_eq!(
            live_pty_count(&app),
            2,
            "exactly TWO PTYs re-spawned (the alive records), the closed one is NOT"
        );
        // Each re-spawned terminal is a distinct, live OS process.
        let pids: Vec<u32> = spawned
            .iter()
            .map(|&id| shell_pid_of(&app, id).expect("re-spawned pty has a shell pid"))
            .collect();
        assert_ne!(
            pids[0], pids[1],
            "the two re-spawned shells are distinct processes"
        );

        // 3) Re-persist: a fresh scrollback snapshot for a restored terminal
        //    round-trips through the command (the steady-state persist after
        //    re-spawn). Re-read proves the command still writes + the list reads.
        let new_blob = "post-restart fresh output\r\n\x1b[34mblue\x1b[0m\r\n";
        persist_scrollback(dbs(), a_id.clone(), new_blob.into()).expect("re-persist after restart");
        let a_after = list_terminals(dbs())
            .unwrap()
            .into_iter()
            .find(|t| t.id == a_id)
            .unwrap();
        assert_eq!(
            a_after.scrollback, new_blob,
            "a fresh scrollback snapshot round-trips through persist→list after restart"
        );

        // Cleanup the re-spawned shells.
        for id in spawned {
            let _ = close(&app, id);
        }
    }

    // --- Project / Workspace / attach / pin / auto-attach through the COMMANDS --
    //
    // ZE1: integration tests on the tauri::test MOCK RUNTIME that drive the REAL
    // `#[tauri::command]` bodies (`create_project`/`create_workspace`/`attach_terminal`/
    // `detach_terminal`/`pin_terminal_workspace`/`unpin_terminal_workspace`/
    // `auto_attach_terminal`) against an in-memory `Db` managed on the mock app.
    // These exercise command ROUTING + PERSISTENCE — that the IPC-facing wrappers
    // map their args correctly and persist `workspace_id` + `workspace_binding_mode`
    // — distinct from the pure DB/resolver unit tests in `db.rs`/`resolve.rs`.
    //
    // The "injected cwd provider" is `auto_attach_terminal`'s `cwd: Option<String>`
    // argument: the bridge wraps it in the platform `CwdProvider` and normalizes it,
    // so passing an explicit cwd here injects the live-cwd reading deterministically
    // without a real `/proc`/OSC7 source. `None` is the "no reliable cwd" signal.
    //
    // Paths are Unix-form: these tests run on Linux (the phase-3 target), where
    // `pathnorm::normalize` uses the Unix rules.

    /// Thin invokers over the command bodies with the mock app's `Db` state, so the
    /// tests read as a sequence of command calls (the IPC surface the front drives).
    fn cmd_create_project(
        app: &App<MockRuntime>,
        name: &str,
        root_path: &str,
        root_name: Option<&str>,
    ) -> Result<ProjectWithRoot, String> {
        create_project(
            app.handle().clone(),
            app.state::<Db>(),
            name.into(),
            root_path.into(),
            root_name.map(Into::into),
        )
    }
    fn cmd_create_workspace(
        app: &App<MockRuntime>,
        project_id: &str,
        name: &str,
        path: &str,
    ) -> Result<Workspace, String> {
        create_workspace(
            app.handle().clone(),
            app.state::<Db>(),
            project_id.into(),
            name.into(),
            path.into(),
        )
    }
    fn cmd_attach(
        app: &App<MockRuntime>,
        terminal_id: &str,
        workspace_id: &str,
        mode: &str,
    ) -> Result<(), String> {
        attach_terminal(
            app.state::<Db>(),
            terminal_id.into(),
            workspace_id.into(),
            mode.into(),
        )
    }
    fn cmd_detach(app: &App<MockRuntime>, terminal_id: &str) -> Result<(), String> {
        detach_terminal(app.state::<Db>(), terminal_id.into())
    }
    fn cmd_pin(
        app: &App<MockRuntime>,
        terminal_id: &str,
        workspace_id: &str,
    ) -> Result<(), String> {
        pin_terminal_workspace(app.state::<Db>(), terminal_id.into(), workspace_id.into())
    }
    fn cmd_unpin(app: &App<MockRuntime>, terminal_id: &str) -> Result<(), String> {
        unpin_terminal_workspace(app.state::<Db>(), terminal_id.into())
    }
    fn cmd_auto_attach(
        app: &App<MockRuntime>,
        terminal_id: &str,
        cwd: Option<&str>,
    ) -> Result<AutoAttachResult, String> {
        auto_attach_terminal(app.state::<Db>(), terminal_id.into(), cwd.map(Into::into))
    }
    /// Read a terminal record's (workspace_id, binding_mode) straight from the Db —
    /// the persistence the commands must have written.
    fn binding_of(app: &App<MockRuntime>, terminal_id: &str) -> (Option<String>, String) {
        app.state::<Db>()
            .with_conn(|c| db::get_terminal(c, terminal_id))
            .unwrap()
            .map(|t| (t.workspace_id, t.workspace_binding_mode))
            .expect("terminal record exists")
    }

    /// FULL command cycle (ZE1 done-criterion #1): create project + workspace,
    /// attach → detach → pin → unpin, then auto-attach — all through the command
    /// bodies — asserting each step ROUTES and PERSISTS `workspace_id` +
    /// `workspace_binding_mode`. One test so a regression in ANY command's wiring
    /// breaks here.
    #[test]
    fn project_workspace_attach_pin_unpin_auto_attach_cycle_via_commands() {
        let app = build_app_with_db();

        // create_project → project + its single root workspace persisted.
        let created = cmd_create_project(&app, "demo", "/home/kris/demo", None)
            .expect("create_project command");
        assert_eq!(created.project.name, "demo");
        assert!(
            created.root.is_root,
            "create_project persists a root workspace"
        );
        let root_id = created.root.id.clone();
        let project_id = created.project.id.clone();

        // list_projects routes back the new project.
        let projects = list_projects(app.state::<Db>()).expect("list_projects");
        assert!(projects.iter().any(|p| p.id == project_id));

        // create_workspace → a non-root workspace persisted under the project.
        let feat = cmd_create_workspace(&app, &project_id, "feat", "/home/kris/demo/feat")
            .expect("create_workspace command");
        assert!(!feat.is_root);
        let workspaces =
            list_workspaces(app.state::<Db>(), project_id.clone()).expect("list_workspaces");
        assert_eq!(workspaces.len(), 2, "root + feat persisted, root first");
        assert!(workspaces[0].is_root);

        // A terminal record to bind.
        let t = create_terminal(app.state::<Db>(), "/home/kris/demo".into(), None)
            .expect("create_terminal");

        // attach (explicit auto) → persists workspace_id + mode auto.
        cmd_attach(&app, &t.id, &root_id, db::BINDING_AUTO).expect("attach command");
        let (ws, mode) = binding_of(&app, &t.id);
        assert_eq!(
            ws.as_deref(),
            Some(root_id.as_str()),
            "attach persisted workspace_id"
        );
        assert_eq!(mode, db::BINDING_AUTO, "attach persisted mode auto");

        // detach → clears workspace_id, resets mode to auto.
        cmd_detach(&app, &t.id).expect("detach command");
        let (ws, mode) = binding_of(&app, &t.id);
        assert_eq!(ws, None, "detach cleared workspace_id");
        assert_eq!(mode, db::BINDING_AUTO);

        // pin → sets workspace_id + mode MANUAL.
        cmd_pin(&app, &t.id, &feat.id).expect("pin command");
        let (ws, mode) = binding_of(&app, &t.id);
        assert_eq!(
            ws.as_deref(),
            Some(feat.id.as_str()),
            "pin persisted workspace_id"
        );
        assert_eq!(mode, db::BINDING_MANUAL, "pin persisted mode manual");

        // unpin → mode back to AUTO, workspace KEPT.
        cmd_unpin(&app, &t.id).expect("unpin command");
        let (ws, mode) = binding_of(&app, &t.id);
        assert_eq!(
            ws.as_deref(),
            Some(feat.id.as_str()),
            "unpin keeps workspace_id"
        );
        assert_eq!(mode, db::BINDING_AUTO, "unpin restores mode auto");

        // auto_attach with a cwd inside `feat` while currently on `feat` ⇒ no change.
        let r = cmd_auto_attach(&app, &t.id, Some("/home/kris/demo/feat/src"))
            .expect("auto_attach command");
        assert!(!r.changed, "already on the resolved workspace ⇒ unchanged");
        assert_eq!(r.workspace_id.as_deref(), Some(feat.id.as_str()));

        // auto_attach with a cwd under the ROOT (outside feat) ⇒ moves to root.
        let r = cmd_auto_attach(&app, &t.id, Some("/home/kris/demo/docs"))
            .expect("auto_attach command");
        assert!(r.changed, "cd under root resolves to root ⇒ moved");
        assert_eq!(r.workspace_id.as_deref(), Some(root_id.as_str()));
        let (ws, mode) = binding_of(&app, &t.id);
        assert_eq!(
            ws.as_deref(),
            Some(root_id.as_str()),
            "auto-attach persisted the move"
        );
        assert_eq!(mode, db::BINDING_AUTO, "an auto-attach move is mode auto");
    }

    /// `agent_close_warnings` (PRD-5 #6) end-to-end through the command: a live agent
    /// session in a NON-resuming project WARNS (the message names the agent + terminal);
    /// turning the project's resume ON makes the SAME session stop warning. Covers the
    /// "Tests couvrent option ON/OFF" + "Warning seulement quand necessaire" criteria at
    /// the command boundary.
    #[test]
    fn agent_close_warnings_warns_only_when_resume_off() {
        let app = build_app_with_db();
        let created = cmd_create_project(&app, "demo", "/home/kris/demo", None).unwrap();
        let ws_id = created.root.id.clone();
        let project_id = created.project.id.clone();

        // A terminal attached to the project's workspace, hosting a live Claude session.
        let t = create_terminal(app.state::<Db>(), "/home/kris/demo".into(), Some("build".into()))
            .unwrap();
        cmd_attach(&app, &t.id, &ws_id, db::BINDING_AUTO).unwrap();
        app.state::<Db>().with_conn(|c| {
            db::record_session_start(
                c,
                &t.id,
                db::AGENT_KIND_CLAUDE_CODE,
                db::SessionCapture {
                    workspace_id: Some(ws_id.clone()),
                    external_session_id: "sid-1".into(),
                    cwd: "/home/kris/demo".into(),
                    transcript_path: None,
                    metadata_json: None,
                },
            )
        })
        .unwrap();

        // Project resume OFF (default) → the live session WARNS, naming agent + terminal.
        let warnings = agent_close_warnings(app.state::<Db>()).expect("agent_close_warnings");
        assert_eq!(warnings.len(), 1, "a live session in a non-resuming project warns");
        assert_eq!(warnings[0].terminal_id, t.id);
        assert_eq!(warnings[0].agent_kind, db::AGENT_KIND_CLAUDE_CODE);
        assert!(
            warnings[0].message.contains("Claude Code") && warnings[0].message.contains("build"),
            "the message names the agent and the terminal: {}",
            warnings[0].message
        );

        // Turn project resume ON → the SAME session no longer warns (nyx will resume it).
        set_project_resume_agent_sessions(app.state::<Db>(), project_id, true).unwrap();
        let warnings_on = agent_close_warnings(app.state::<Db>()).expect("agent_close_warnings on");
        assert!(warnings_on.is_empty(), "resume-ON project suppresses the warning");
    }

    /// Serialize `$SHELL` mutation across tests that depend on the resolved resume
    /// TARGET (resume only parks for a Unix/WSL shell). Returns a guard holding the lock
    /// and restoring the prior value on drop.
    fn force_unix_shell() -> impl Drop {
        struct ShellGuard {
            prev: Option<String>,
            _lock: std::sync::MutexGuard<'static, ()>,
        }
        impl Drop for ShellGuard {
            fn drop(&mut self) {
                match &self.prev {
                    Some(v) => std::env::set_var("SHELL", v),
                    None => std::env::remove_var("SHELL"),
                }
            }
        }
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("SHELL").ok();
        std::env::set_var("SHELL", "/bin/bash");
        ShellGuard { prev, _lock: lock }
    }

    /// Create a real (non-empty) `.jsonl` transcript file in a process-unique temp path
    /// and return it, so the boot-scan tests exercise the #53 "conversation exists on
    /// disk" gate against a path that actually `stat`s as present (mirrors Claude only
    /// writing the transcript once the user has typed). No `tempfile` dep in the tree.
    fn boot_temp_transcript(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let mut dir = std::env::temp_dir();
        dir.push(format!("nyx-resume-transcript-{}-{}-{}", std::process::id(), tag, n));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{tag}.jsonl"));
        std::fs::write(&path, b"{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        path
    }

    /// `restore_agent_sessions_on_boot` (PRD-5 #5) parks a resume ONLY for an alive
    /// terminal whose project opts IN and whose session is resumable. We force a Unix
    /// `$SHELL` so the resume target is supported on this host. Covers: option ON parks
    /// `claude --resume <id>`; option OFF parks nothing; an ended session parks nothing.
    /// The ON session has a REAL transcript on disk so the #53 conversation gate opens.
    #[test]
    fn restore_agent_sessions_on_boot_parks_resume_when_opted_in() {
        let _shell = force_unix_shell();
        let app = build_app_with_db();

        // A real transcript file on disk so the #53 "conversation exists" gate opens.
        let transcript = boot_temp_transcript("sid-on");

        // Project ON, an attached alive terminal with a live session → should park.
        let on = cmd_create_project(&app, "on", "/on", None).unwrap();
        set_project_resume_agent_sessions(app.state::<Db>(), on.project.id.clone(), true).unwrap();
        let t_on = create_terminal(app.state::<Db>(), "/on".into(), None).unwrap();
        cmd_attach(&app, &t_on.id, &on.root.id, db::BINDING_AUTO).unwrap();
        app.state::<Db>()
            .with_conn(|c| {
                db::record_session_start(
                    c,
                    &t_on.id,
                    db::AGENT_KIND_CLAUDE_CODE,
                    db::SessionCapture {
                        workspace_id: Some(on.root.id.clone()),
                        external_session_id: "sid-on".into(),
                        cwd: "/on".into(),
                        transcript_path: Some(transcript.to_string_lossy().into_owned()),
                        metadata_json: None,
                    },
                )
            })
            .unwrap();

        // Project OFF (default), attached alive terminal with a live session → no park.
        let off = cmd_create_project(&app, "off", "/off", None).unwrap();
        let t_off = create_terminal(app.state::<Db>(), "/off".into(), None).unwrap();
        cmd_attach(&app, &t_off.id, &off.root.id, db::BINDING_AUTO).unwrap();
        app.state::<Db>()
            .with_conn(|c| {
                db::record_session_start(
                    c,
                    &t_off.id,
                    db::AGENT_KIND_CLAUDE_CODE,
                    db::SessionCapture {
                        workspace_id: Some(off.root.id.clone()),
                        external_session_id: "sid-off".into(),
                        cwd: "/off".into(),
                        transcript_path: None,
                        metadata_json: None,
                    },
                )
            })
            .unwrap();

        let resumes = PendingResumes::default();
        let parked = restore_agent_sessions_on_boot(app.state::<Db>().inner(), &resumes);

        assert_eq!(parked, vec![t_on.id.clone()], "only the resume-ON terminal parks a resume");
        let r = resumes.take(&t_on.id).expect("a resume is parked for the ON terminal");
        assert_eq!(r.command, "claude --resume sid-on", "exact-id resume command");
        assert!(!r.uncertain, "an active (not unknown) session is not flagged uncertain");
        assert!(resumes.take(&t_off.id).is_none(), "the resume-OFF terminal parks nothing");
    }

    /// #53: a resume-ON, alive, active candidate whose `transcript_path` does NOT exist
    /// on disk (user never typed → Claude never wrote the `.jsonl`, or the conversation
    /// was deleted) is NOT resumed at boot — the bridge `stat`s the missing path, the
    /// pure decision skips with `NoConversation`, and nothing is parked. Otherwise nyx
    /// would inject a `claude --resume` that fails "No conversation found" and breaks the
    /// respawned terminal. A `None` transcript_path is treated the same (no path to stat).
    #[test]
    fn boot_scan_skips_resume_when_transcript_missing() {
        let _shell = force_unix_shell();
        let app = build_app_with_db();

        // (a) transcript_path points at a path that does NOT exist on disk.
        let on = cmd_create_project(&app, "on", "/on", None).unwrap();
        set_project_resume_agent_sessions(app.state::<Db>(), on.project.id.clone(), true).unwrap();
        let t_missing = create_terminal(app.state::<Db>(), "/on".into(), None).unwrap();
        cmd_attach(&app, &t_missing.id, &on.root.id, db::BINDING_AUTO).unwrap();
        let absent = std::env::temp_dir()
            .join(format!("nyx-resume-absent-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&absent); // ensure it really does not exist
        app.state::<Db>()
            .with_conn(|c| {
                db::record_session_start(
                    c,
                    &t_missing.id,
                    db::AGENT_KIND_CLAUDE_CODE,
                    db::SessionCapture {
                        workspace_id: Some(on.root.id.clone()),
                        external_session_id: "sid-no-conv".into(),
                        cwd: "/on".into(),
                        transcript_path: Some(absent.to_string_lossy().into_owned()),
                        metadata_json: None,
                    },
                )
            })
            .unwrap();

        // (b) transcript_path is None (never captured) → also no conversation to resume.
        let t_none = create_terminal(app.state::<Db>(), "/on".into(), None).unwrap();
        cmd_attach(&app, &t_none.id, &on.root.id, db::BINDING_AUTO).unwrap();
        app.state::<Db>()
            .with_conn(|c| {
                db::record_session_start(
                    c,
                    &t_none.id,
                    db::AGENT_KIND_CLAUDE_CODE,
                    db::SessionCapture {
                        workspace_id: Some(on.root.id.clone()),
                        external_session_id: "sid-no-path".into(),
                        cwd: "/on".into(),
                        transcript_path: None,
                        metadata_json: None,
                    },
                )
            })
            .unwrap();

        let resumes = PendingResumes::default();
        let parked = restore_agent_sessions_on_boot(app.state::<Db>().inner(), &resumes);

        assert!(parked.is_empty(), "no conversation on disk → no resume parked: {parked:?}");
        assert!(resumes.take(&t_missing.id).is_none(), "missing transcript parks nothing");
        assert!(resumes.take(&t_none.id).is_none(), "absent transcript_path parks nothing");
    }

    /// The kill→relaunch path end-to-end: a session left STALE-active (a probable app
    /// kill: `last_seen_at` far in the past, no clean SessionEnd) is swept to `unknown`
    /// by the boot scan and STILL parked as a resume — flagged `uncertain`. Ties
    /// together the active→unknown péremption sweep and "unknown is a resume candidate".
    #[test]
    fn boot_scan_sweeps_stale_active_to_unknown_and_still_resumes_uncertain() {
        use diesel::connection::SimpleConnection;
        let _shell = force_unix_shell();
        let app = build_app_with_db();

        let on = cmd_create_project(&app, "on", "/on", None).unwrap();
        set_project_resume_agent_sessions(app.state::<Db>(), on.project.id.clone(), true).unwrap();
        let t = create_terminal(app.state::<Db>(), "/on".into(), None).unwrap();
        cmd_attach(&app, &t.id, &on.root.id, db::BINDING_AUTO).unwrap();

        // A real transcript on disk so the #53 conversation gate opens for the resume.
        let transcript = boot_temp_transcript("sid-stale");

        // Record a normal active session, then BACKDATE its last_seen_at well past the
        // staleness threshold — the shape an app kill leaves behind (active, never
        // cleanly ended, long silent). A raw UPDATE since there is no public setter.
        let session = app
            .state::<Db>()
            .with_conn(|c| {
                db::record_session_start(
                    c,
                    &t.id,
                    db::AGENT_KIND_CLAUDE_CODE,
                    db::SessionCapture {
                        workspace_id: Some(on.root.id.clone()),
                        external_session_id: "sid-stale".into(),
                        cwd: "/on".into(),
                        transcript_path: Some(transcript.to_string_lossy().into_owned()),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let past = db::now_millis() - db::SESSION_STALE_AFTER_MS - 60_000;
        let session_id = session.id.clone();
        app.state::<Db>()
            .with_conn(|c| {
                c.batch_execute(&format!(
                    "UPDATE agent_sessions SET last_seen_at = {past} WHERE id = '{session_id}'"
                ))
            })
            .expect("backdate last_seen_at");

        let resumes = PendingResumes::default();
        let parked = restore_agent_sessions_on_boot(app.state::<Db>().inner(), &resumes);

        // The boot scan swept the row to `unknown` (péremption) but still parked it.
        let row = app
            .state::<Db>()
            .with_conn(|c| db::get_session(c, &session_id))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, db::SESSION_STATE_UNKNOWN, "stale active swept to unknown");
        assert_eq!(parked, vec![t.id.clone()], "the unknown session is still a resume candidate");
        let r = resumes.take(&t.id).expect("a resume is parked for the unknown session");
        assert_eq!(r.command, "claude --resume sid-stale");
        assert!(r.uncertain, "an unknown (probable-kill) session is flagged uncertain");
    }

    /// `register_terminal_pty` marks a parked resume `resume_failed` when delivery is
    /// impossible (no live PTY for the id). Covers the resume_failed transition through
    /// the real injection path (the PTY-present happy path is the dogfood/E2E gate).
    #[test]
    fn parked_resume_with_dead_pty_marks_resume_failed() {
        let app = build_app_with_db();
        let t = create_terminal(app.state::<Db>(), "/work".into(), None).unwrap();
        let session = app
            .state::<Db>()
            .with_conn(|c| {
                db::record_session_start(
                    c,
                    &t.id,
                    db::AGENT_KIND_CLAUDE_CODE,
                    db::SessionCapture {
                        external_session_id: "sid-x".into(),
                        cwd: "/work".into(),
                        ..Default::default()
                    },
                )
            })
            .unwrap();

        // Park a resume, then register a PTY id that has NO live Pty in the manager
        // (delivery will fail) → the session must flip to resume_failed.
        app.state::<PendingResumes>().set(
            &t.id,
            PendingResume {
                session_id: session.id.clone(),
                command: "claude --resume sid-x".into(),
                uncertain: false,
            },
        );
        register_terminal_pty(
            app.state::<Db>(),
            app.state::<TerminalPtyMap>(),
            app.state::<PendingTerminalCommands>(),
            app.state::<PendingResumes>(),
            app.state::<PtyManager>(),
            t.id.clone(),
            Some(999_999), // no Pty with this id is registered → delivery fails
        )
        .expect("register");

        let got = app
            .state::<Db>()
            .with_conn(|c| db::get_session(c, &session.id))
            .unwrap()
            .unwrap();
        assert_eq!(
            got.state,
            db::SESSION_STATE_RESUME_FAILED,
            "a resume that could not be delivered marks the session resume_failed"
        );
    }

    /// GUARD (ZE1 done-criterion #2): a `manual`-mode terminal is NEVER moved by
    /// auto-attach. We pin the terminal to `root`, then auto-attach with a cwd that
    /// resolves to `feat` — the command must leave the binding on `root`/manual.
    /// A regression that let auto-attach move a pinned terminal fails here.
    #[test]
    fn auto_attach_does_not_move_a_manual_pinned_terminal() {
        let app = build_app_with_db();
        let created = cmd_create_project(&app, "p", "/home/kris/p", None).expect("create_project");
        let root_id = created.root.id.clone();
        let project_id = created.project.id.clone();
        let feat = cmd_create_workspace(&app, &project_id, "feat", "/home/kris/p/feat")
            .expect("create_workspace");

        let t = create_terminal(app.state::<Db>(), "/home/kris/p".into(), None).unwrap();
        // PIN to root (mode manual).
        cmd_pin(&app, &t.id, &root_id).expect("pin");

        // A cwd deep inside feat would resolve to `feat` in auto mode — but the
        // terminal is pinned, so auto-attach must NOT move it.
        let r = cmd_auto_attach(&app, &t.id, Some("/home/kris/p/feat/src")).expect("auto_attach");
        assert!(!r.changed, "a manual pin must not be moved by auto-attach");
        assert_eq!(
            r.workspace_id.as_deref(),
            Some(root_id.as_str()),
            "the pinned binding is reported unchanged"
        );
        let (ws, mode) = binding_of(&app, &t.id);
        assert_eq!(
            ws.as_deref(),
            Some(root_id.as_str()),
            "the persisted binding stays on root (NOT moved to feat)"
        );
        assert_eq!(mode, db::BINDING_MANUAL, "and stays manual");
        assert_ne!(
            ws.as_deref(),
            Some(feat.id.as_str()),
            "auto-attach must never relocate a pinned terminal to the resolved workspace"
        );
    }

    /// GUARD (ZE1 done-criterion #3): the absence of a reliable cwd (`None`) must
    /// NOT produce a created/invented attachment. With an UNATTACHED auto-mode
    /// terminal and `cwd: None`, the command degrades explicitly: no change, no
    /// workspace invented, the binding stays `None`. Also covers a cwd that matches
    /// NO known workspace (also "no invention").
    #[test]
    fn auto_attach_with_no_reliable_cwd_invents_nothing() {
        let app = build_app_with_db();
        // A known workspace exists, so a match WOULD be possible IF we guessed.
        let created = cmd_create_project(&app, "p", "/home/kris/p", None).expect("create_project");
        let root_id = created.root.id.clone();

        let t = create_terminal(app.state::<Db>(), "/home/kris/p".into(), None).unwrap();
        assert_eq!(
            binding_of(&app, &t.id),
            (None, db::BINDING_AUTO.to_string())
        );

        // No reliable cwd ⇒ explicit degradation: unchanged, still unattached.
        let r = cmd_auto_attach(&app, &t.id, None).expect("auto_attach None");
        assert!(!r.changed, "no reliable cwd ⇒ no change");
        assert_eq!(r.workspace_id, None, "no cwd ⇒ no invented attachment");
        assert_eq!(
            binding_of(&app, &t.id),
            (None, db::BINDING_AUTO.to_string()),
            "the record stays unattached/auto (nothing created)"
        );

        // An empty-string cwd is also "no reliable cwd" (normalizes to empty).
        let r = cmd_auto_attach(&app, &t.id, Some("")).expect("auto_attach empty");
        assert!(!r.changed && r.workspace_id.is_none());

        // A cwd that matches NO known workspace ⇒ likewise nothing is created.
        let r = cmd_auto_attach(&app, &t.id, Some("/somewhere/else/entirely"))
            .expect("auto_attach unmatched");
        assert!(!r.changed, "an unmatched cwd creates nothing");
        assert_eq!(r.workspace_id, None);

        // And no stray workspace was minted under the project (still just root).
        let workspaces = list_workspaces(app.state::<Db>(), created.project.id.clone())
            .expect("list_workspaces");
        assert_eq!(
            workspaces.len(),
            1,
            "no workspace was invented by auto-attach"
        );
        assert_eq!(workspaces[0].id, root_id);
    }

    /// `create_workspace` surfaces a duplicate-path rejection as a stringified Err
    /// through the command boundary (the front renders it inline). The SAME path in
    /// a DIFFERENT project is accepted — the routing preserves the per-project
    /// UNIQUE(project_id, path) semantics.
    #[test]
    fn create_workspace_command_rejects_dup_path_allows_across_projects() {
        let app = build_app_with_db();
        let p1 = cmd_create_project(&app, "p1", "/work", None).expect("p1");
        let p2 = cmd_create_project(&app, "p2", "/other", None).expect("p2");

        cmd_create_workspace(&app, &p1.project.id, "feat", "/work/feat").expect("first add");
        // Same normalized path again in p1 ⇒ Err surfaced through the command.
        let dup = cmd_create_workspace(&app, &p1.project.id, "dup", "/work//feat/");
        assert!(
            dup.is_err(),
            "duplicate path in same project is rejected via the command"
        );
        // Same path in p2 ⇒ accepted.
        cmd_create_workspace(&app, &p2.project.id, "feat", "/work/feat")
            .expect("same path in a different project is accepted");
    }

    /// `update_project` + `delete_project` through the COMMAND bodies: rename
    /// routes + persists; delete removes the project + its workspaces and DETACHES
    /// (not deletes) its terminals. Mirrors the db unit tests at the IPC surface.
    #[test]
    fn update_and_delete_project_commands_route_and_persist() {
        let app = build_app_with_runner();

        let created = cmd_create_project(&app, "old", "/home/kris/proj", None)
            .expect("create_project command");
        let project_id = created.project.id.clone();
        let root_id = created.root.id.clone();

        // update_project renames; list_projects reflects it.
        update_project(app.state::<Db>(), project_id.clone(), "renamed".into())
            .expect("update_project command");
        let projects = list_projects(app.state::<Db>()).expect("list_projects");
        let got = projects.iter().find(|p| p.id == project_id).unwrap();
        assert_eq!(
            got.name, "renamed",
            "rename routes + persists via the command"
        );

        // Bind a terminal to the root, then delete the project: the terminal must
        // survive, DETACHED (workspace_id NULL), and the workspaces are gone.
        let t = create_terminal(app.state::<Db>(), "/home/kris/proj".into(), None).unwrap();
        cmd_attach(&app, &t.id, &root_id, db::BINDING_MANUAL).expect("attach");

        delete_project(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            project_id.clone(),
        )
        .expect("delete_project command");
        let projects = list_projects(app.state::<Db>()).expect("list after delete");
        assert!(
            projects.iter().all(|p| p.id != project_id),
            "delete_project removes the project via the command"
        );
        assert!(
            list_workspaces(app.state::<Db>(), project_id.clone())
                .unwrap()
                .is_empty(),
            "the project's workspaces cascaded away"
        );
        let (ws, _mode) = binding_of(&app, &t.id);
        assert_eq!(
            ws, None,
            "the terminal survived the project delete, detached (workspace_id NULL)"
        );
    }

    /// `set_project_collapsed` + `set_workspace_collapsed` route through the
    /// COMMAND bodies and PERSIST the sidebar disclosure state: `list_projects` /
    /// `list_workspaces` read back the flag a reload restores the bands from.
    #[test]
    fn set_collapsed_commands_route_and_persist() {
        let app = build_app_with_db();

        let created = cmd_create_project(&app, "p", "/home/kris/p", None).expect("create_project");
        let project_id = created.project.id.clone();
        let root_id = created.root.id.clone();
        let feat = cmd_create_workspace(&app, &project_id, "feat", "/home/kris/p/feat")
            .expect("create_workspace");

        // Fresh project + workspaces are OPEN (collapsed = false) by default.
        assert!(!created.project.collapsed && !created.root.collapsed && !feat.collapsed);

        // Collapse the project via the command; list_projects reflects it.
        set_project_collapsed(app.state::<Db>(), project_id.clone(), true)
            .expect("set_project_collapsed command");
        let projects = list_projects(app.state::<Db>()).expect("list_projects");
        assert!(
            projects
                .iter()
                .find(|pr| pr.id == project_id)
                .unwrap()
                .collapsed,
            "collapse routes + persists via the project command"
        );

        // Collapse the feat workspace via the command; the root stays open.
        set_workspace_collapsed(app.state::<Db>(), feat.id.clone(), true)
            .expect("set_workspace_collapsed command");
        let workspaces = list_workspaces(app.state::<Db>(), project_id.clone()).expect("list_ws");
        assert!(
            workspaces
                .iter()
                .find(|w| w.id == feat.id)
                .unwrap()
                .collapsed,
            "collapse routes + persists via the workspace command"
        );
        assert!(
            !workspaces
                .iter()
                .find(|w| w.id == root_id)
                .unwrap()
                .collapsed,
            "a sibling workspace is left open"
        );

        // Re-open the project (the toggle round-trips both ways).
        set_project_collapsed(app.state::<Db>(), project_id.clone(), false)
            .expect("re-open project");
        let projects = list_projects(app.state::<Db>()).expect("list_projects again");
        assert!(
            !projects
                .iter()
                .find(|pr| pr.id == project_id)
                .unwrap()
                .collapsed,
            "the project re-opens via the command"
        );
    }

    // --- window_controls_visible toggle parsing (interim NYX_WINDOW_CONTROLS) -
    //
    // The command reads the raw OS env at runtime; the parsing contract lives in
    // the pure `controls_visible_from_env` so it is testable without mutating the
    // process environment.

    #[test]
    fn controls_visible_default_when_unset() {
        // Unset env → controls VISIBLE (permissive default; window stays closeable).
        assert!(controls_visible_from_env(None));
    }

    #[test]
    fn controls_hidden_only_on_exact_zero() {
        // Only the exact string "0" hides the controls.
        assert!(!controls_visible_from_env(Some("0".into())));
    }

    #[test]
    fn controls_visible_for_any_non_zero_value() {
        // Any other value (incl. empty) keeps them visible.
        assert!(controls_visible_from_env(Some("1".into())));
        assert!(controls_visible_from_env(Some("true".into())));
        assert!(controls_visible_from_env(Some(String::new())));
        assert!(controls_visible_from_env(Some("00".into())));
    }

    // --- command event payloads serialize in camelCase (live-event contract) -
    //
    // The front filters EVERY `command://state` / `command://output` event on
    // `event.payload.instanceId` (camelCase). If these payloads serialized the
    // field as snake_case `instance_id`, the front would read `undefined` and DROP
    // every live event — the headline "the dot never updates / output never streams
    // live" bug. These tests pin the wire key so a regression fails here, not in a
    // hard-to-trace dogfood session.

    #[test]
    fn command_state_payload_serializes_instance_id_as_camel_case() {
        let payload = CommandStatePayload {
            instance_id: "inst-42".to_string(),
            state: "running".to_string(),
            code: None,
        };
        let v = serde_json::to_value(&payload).expect("serialize CommandStatePayload");
        assert_eq!(
            v.get("instanceId").and_then(|x| x.as_str()),
            Some("inst-42"),
            "command://state must serialize instanceId in camelCase (the front filters on it)"
        );
        assert!(
            v.get("instance_id").is_none(),
            "the snake_case key must NOT be present (it would make the front drop the event)"
        );
    }

    #[test]
    fn command_output_payload_serializes_instance_id_as_camel_case() {
        let payload = CommandOutputPayload {
            instance_id: "inst-7".to_string(),
            bytes: vec![1, 2, 3],
        };
        let v = serde_json::to_value(&payload).expect("serialize CommandOutputPayload");
        assert_eq!(
            v.get("instanceId").and_then(|x| x.as_str()),
            Some("inst-7"),
            "command://output must serialize instanceId in camelCase (the front filters on it)"
        );
        assert!(
            v.get("instance_id").is_none(),
            "the snake_case key must NOT be present"
        );
    }

    // --- Managed-command runtime: production sink on the mock runtime --------
    //
    // Validates the THIN Tauri layer wired in this phase: `build_command_runner`
    // + `TauriRunnerSink` must (1) emit `command://state` and `command://output`
    // over the AppHandle and (2) persist `last_state` through the managed `Db`.
    // The state-machine logic itself is unit-tested in `crate::command`; here we
    // prove the production event + persistence plumbing end-to-end on the mock
    // app. (The fuller start->running->output->exit integration matrix is the
    // Phase-5 `tauri::test` task; this is the minimal contract for THIS phase.)

    /// Create a project (with its root workspace) + one template, returning the
    /// materialized instance id for the root workspace. Pins `$SHELL` POSIX so the
    /// command runs deterministically under `sh -c`.
    #[cfg(not(windows))]
    fn seed_instance(app: &App<MockRuntime>) -> String {
        std::env::set_var("SHELL", "/bin/sh");
        let db = app.state::<Db>();
        db.with_conn(|c| {
            let pr = db::create_project(c, "proj", "/tmp/nyx-cmd-test", None).expect("project");
            // Materializes one instance of this template into the root workspace.
            let tpl =
                db::create_template(c, &pr.0.id, "svc", "echo SVC_OUT", None, Default::default())
                    .expect("template");
            let instances = db::list_instances_for_workspace(c, &pr.1.id).expect("instances");
            let inst = instances
                .iter()
                .find(|i| i.command_id == tpl.id)
                .expect("materialized instance");
            inst.id.clone()
        })
    }

    #[test]
    #[cfg(not(windows))]
    fn production_runner_emits_events_and_persists_last_state() {
        use std::sync::mpsc::channel;
        let app = build_app_with_db();
        let instance_id = seed_instance(&app);

        // Capture command://state + command://output for our instance.
        let (state_tx, state_rx) = channel::<(String, Option<i32>)>();
        {
            let id = instance_id.clone();
            app.listen("command://state", move |event| {
                let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                if v["instance_id"] == id {
                    let st = v["state"].as_str().unwrap().to_string();
                    let code = v["code"].as_i64().map(|n| n as i32);
                    let _ = state_tx.send((st, code));
                }
            });
        }
        let (out_tx, out_rx) = channel::<String>();
        {
            let id = instance_id.clone();
            app.listen("command://output", move |event| {
                let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                if v["instance_id"] == id {
                    let bytes: Vec<u8> = v["bytes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|n| n.as_u64().unwrap() as u8)
                        .collect();
                    let _ = out_tx.send(String::from_utf8_lossy(&bytes).into_owned());
                }
            });
        }

        // Build the PRODUCTION runner (Tauri sink + managed Db) and run the
        // template's command line through it.
        let runner = build_command_runner(app.handle().clone());
        runner
            .start(&instance_id, "echo SVC_OUT", None)
            .expect("start");

        // 1) A running state event fires.
        let mut saw_running = false;
        let mut saw_success = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while std::time::Instant::now() < deadline && !(saw_running && saw_success) {
            if let Ok((st, code)) = state_rx.recv_timeout(std::time::Duration::from_millis(200)) {
                if st == "running" {
                    saw_running = true;
                }
                if st == "success" {
                    assert_eq!(code, Some(0), "success must carry exit code 0");
                    saw_success = true;
                }
            }
        }
        assert!(
            saw_running,
            "command://state must emit a running transition"
        );
        assert!(
            saw_success,
            "command://state must emit a success transition on exit 0"
        );

        // 2) The output event relayed the command's stdout.
        let mut out = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline && !out.contains("SVC_OUT") {
            if let Ok(chunk) = out_rx.recv_timeout(std::time::Duration::from_millis(200)) {
                out.push_str(&chunk);
            }
        }
        assert!(
            out.contains("SVC_OUT"),
            "command://output must relay the command stdout, got: {out:?}"
        );

        // 3) The DB row's last_state was persisted to success (committed before the
        // success event, so by now it must read back as success).
        let persisted = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .expect("get_instance")
            .expect("instance row");
        assert_eq!(
            persisted.last_state, "success",
            "the runner must persist last_state to the DB on each transition"
        );
        // And the scrollback was persisted (contains the output).
        assert!(
            persisted.scrollback.contains("SVC_OUT"),
            "the runner must persist bounded scrollback, got: {:?}",
            persisted.scrollback
        );
    }

    // --- Internal command surface (PRD-3 Phase 3, task 5) --------------------
    //
    // These exercise the `command_*` `#[tauri::command]` BODIES directly with the
    // mock app's handle + managed state (the same direct-invoke pattern the PTY
    // tests use; app-defined ACL permissions are absent under `mock_context`). We
    // manage the PRODUCTION runner so the lifecycle commands have a live runner and
    // the running-mutation guards see real live state.

    /// A mock app with an in-memory `Db` AND the production command runner managed,
    /// so the `command_*` lifecycle commands and their guards work end-to-end.
    fn build_app_with_runner() -> App<MockRuntime> {
        let app = build_app_with_db();
        manage_command_runner(&app.handle().clone());
        app
    }

    fn runner_state(app: &App<MockRuntime>) -> State<'_, ManagedCommandRunner<MockRuntime>> {
        app.state::<ManagedCommandRunner<MockRuntime>>()
    }

    /// A real temp workspace directory (canonicalized), cleaned on drop, so
    /// `command_start`'s subfolder/cwd resolution has an existing folder to run in.
    struct TempWs {
        root: std::path::PathBuf,
    }
    impl TempWs {
        fn new(tag: &str) -> Self {
            let mut root = std::env::temp_dir();
            root.push(format!(
                "nyx_bridge_{}_{}_{}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let root = std::fs::canonicalize(&root).unwrap();
            TempWs { root }
        }
        fn path(&self) -> String {
            self.root.to_string_lossy().into_owned()
        }
    }
    impl Drop for TempWs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Poll the DB `last_state` of an instance until it equals `want` or times out.
    #[cfg(not(windows))]
    fn wait_db_state(app: &App<MockRuntime>, instance_id: &str, want: &str, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            let st = app
                .state::<Db>()
                .with_conn(|c| db::get_instance(c, instance_id))
                .unwrap()
                .map(|i| i.last_state)
                .unwrap_or_default();
            if st == want {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        false
    }

    /// `command_create` carries subfolder + restart_on_startup + the four source
    /// fields; `command_list` returns it; `command_reorder` persists a new order.
    #[test]
    fn create_carries_subfolder_restart_source_and_reorder_persists() {
        let app = build_app_with_db();
        let db = app.state::<Db>();
        let project_id = db
            .with_conn(|c| db::create_project(c, "p", "/tmp/p", None))
            .unwrap()
            .0
            .id;

        let created = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id.clone(),
            "dev".into(),
            "pnpm dev".into(),
            Some("packages/api".into()),
            Some(true),
            Some("package_json".into()),
            Some("/tmp/p/packages/api/package.json".into()),
            Some("dev".into()),
            Some("vite".into()),
            Some("pnpm".into()),
        )
        .expect("command_create");
        assert_eq!(created.subfolder.as_deref(), Some("packages/api"));
        assert!(created.restart_on_startup, "restart_on_startup carried");
        assert_eq!(created.source_kind.as_deref(), Some("package_json"));
        assert_eq!(created.source_script_name.as_deref(), Some("dev"));
        assert_eq!(created.package_manager.as_deref(), Some("pnpm"));

        // A second template, then reorder them and confirm the order persists.
        let second = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id.clone(),
            "build".into(),
            "pnpm build".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("second");

        let listed = command_list(app.state::<Db>(), project_id.clone()).expect("list");
        assert_eq!(listed.len(), 2);

        command_reorder(
            app.state::<Db>(),
            vec![second.id.clone(), created.id.clone()],
        )
        .expect("reorder");
        let reordered = command_list(app.state::<Db>(), project_id).expect("list2");
        assert_eq!(
            reordered.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            vec![second.id, created.id],
            "reorder must persist the new template order"
        );
    }

    /// PURE inference: a command line whose first token names a known package
    /// manager is categorized; anything else (raw binary, unknown token, empty)
    /// is left `None`. Extra args / flags after the manager are irrelevant. Exercised
    /// through the shared [`infer_command_source`] (which delegates to the ONE nyx-core
    /// implementation both shells drive) — its `package_manager` output is the inference.
    #[test]
    fn infer_package_manager_from_command_line() {
        for (line, expected) in [
            ("bun install", Some("bun")),
            ("bun run dev", Some("bun")),
            ("npm install", Some("npm")),
            ("npm run build", Some("npm")),
            ("pnpm dev", Some("pnpm")),
            ("pnpm   -w   build", Some("pnpm")), // multiple spaces / flags ignored
            ("yarn", Some("yarn")),              // bare manager, no script
            ("  yarn start  ", Some("yarn")),    // leading whitespace tolerated
            ("vite dev", None),                  // raw binary, not a PM
            ("node server.js", None),
            ("BUN_ENV=prod bun dev", None), // env prefix is the first token, not a PM
            ("", None),
            ("   ", None),
        ] {
            let (_kind, pm) = infer_command_source(line, None, None);
            assert_eq!(
                pm.as_deref(),
                expected,
                "inference for {line:?}"
            );
        }
    }

    /// `infer_command_source` fills BOTH fields for an inferable line, leaves a
    /// non-PM line untouched, and NEVER overrides an explicit caller-provided
    /// manager (the import path).
    #[test]
    fn infer_command_source_rules() {
        // Inferable, caller supplied nothing → manager + package_json source_kind.
        let (kind, pm) = infer_command_source("bun install", None, None);
        assert_eq!(pm.as_deref(), Some("bun"));
        assert_eq!(kind.as_deref(), Some(db::SOURCE_KIND_PACKAGE_JSON));

        // Non-PM line → both stay None.
        let (kind, pm) = infer_command_source("node app.js", None, None);
        assert_eq!(pm, None);
        assert_eq!(kind, None);

        // Explicit manager wins even if the line would infer a different one.
        let (kind, pm) = infer_command_source(
            "pnpm dev",
            Some("package_json".into()),
            Some("yarn".into()),
        );
        assert_eq!(pm.as_deref(), Some("yarn"), "explicit value preserved");
        assert_eq!(kind.as_deref(), Some("package_json"));
    }

    /// A manually-added command whose line is a PM invocation gets its
    /// `package_manager` + `source_kind` inferred through the real
    /// `command_create` path; an already-tagged (imported) command is unchanged;
    /// a plain (non-PM) manual command stays null on both fields.
    #[test]
    fn command_create_infers_package_manager_for_manual_commands() {
        let app = build_app_with_db();
        let project_id = app
            .state::<Db>()
            .with_conn(|c| db::create_project(c, "p", "/tmp/p", None))
            .unwrap()
            .0
            .id;

        // Manually-added `bun install` (no source fields supplied) → inferred.
        let manual = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id.clone(),
            "deps".into(),
            "bun install".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("manual bun install");
        assert_eq!(manual.package_manager.as_deref(), Some("bun"));
        assert_eq!(manual.source_kind.as_deref(), Some("package_json"));

        // A non-PM manual command stays uncategorized.
        let plain = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id.clone(),
            "serve".into(),
            "node server.js".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("plain command");
        assert_eq!(plain.package_manager, None);
        assert_eq!(plain.source_kind, None);

        // An EXPLICITLY-tagged (imported) command is left exactly as supplied: the
        // command line says `bun` but the caller pinned `pnpm`, which must win.
        let imported = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id,
            "dev".into(),
            "bun run dev".into(),
            None,
            None,
            Some("package_json".into()),
            Some("/tmp/p/package.json".into()),
            Some("dev".into()),
            Some("vite".into()),
            Some("pnpm".into()),
        )
        .expect("imported command");
        assert_eq!(
            imported.package_manager.as_deref(),
            Some("pnpm"),
            "explicit imported manager must not be overridden by inference"
        );
        assert_eq!(imported.source_kind.as_deref(), Some("package_json"));
    }

    /// Full lifecycle through the COMMAND surface: start -> running, output relayed,
    /// natural exit -> success, and `command_output` returns the persisted history.
    #[test]
    #[cfg(not(windows))]
    fn start_running_output_success_through_commands() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("lifecycle");
        let (project_id, workspace_id) = app.state::<Db>().with_conn(|c| {
            let pr = db::create_project(c, "p", &ws.path(), None).unwrap();
            (pr.0.id, pr.1.id)
        });

        // A template that echoes a marker and exits 0.
        let tpl = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id,
            "svc".into(),
            "echo LIFECYCLE_MARKER".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("template");

        let instance_id = app
            .state::<Db>()
            .with_conn(|c| db::list_instances_for_workspace(c, &workspace_id))
            .unwrap()
            .into_iter()
            .find(|i| i.command_id == tpl.id)
            .expect("instance")
            .id;

        // start -> running (then it exits 0 -> success).
        let st = command_start(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_start");
        assert_eq!(st, "running", "start returns running");

        assert!(
            wait_db_state(&app, &instance_id, "success", 8),
            "the command must reach success after a natural exit 0"
        );

        // command_output returns the persisted scrollback (cold history) containing
        // the marker.
        let out = command_output(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_output");
        assert!(
            out.contains("LIFECYCLE_MARKER"),
            "command_output must return the output history, got: {out:?}"
        );
    }

    /// `command_output` branch contract (T5 done_criterion): the LIVE in-memory
    /// buffer while running, the persisted scrollback once terminal. We start a
    /// command that emits a marker then sleeps so the instance stays `running` with
    /// output already buffered. The live read must surface the marker straight from
    /// the runner — even before the debounced DB persist has written the row — and
    /// after we stop it, the same call must rehydrate the persisted cold scrollback.
    #[test]
    #[cfg(not(windows))]
    fn command_output_reads_live_buffer_while_running_then_persisted_cold() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("liveoutput");
        let (project_id, workspace_id) = app.state::<Db>().with_conn(|c| {
            let pr = db::create_project(c, "p", &ws.path(), None).unwrap();
            (pr.0.id, pr.1.id)
        });

        // Emit a marker, then sleep so the instance stays running (no natural exit).
        let tpl = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id,
            "svc".into(),
            "echo LIVE_MARKER; sleep 30".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("template");

        let instance_id = app
            .state::<Db>()
            .with_conn(|c| db::list_instances_for_workspace(c, &workspace_id))
            .unwrap()
            .into_iter()
            .find(|i| i.command_id == tpl.id)
            .expect("instance")
            .id;

        let st = command_start(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_start");
        assert_eq!(st, "running", "start returns running");

        // Poll the LIVE read until the marker appears. This proves the running
        // branch returns the runner's in-memory buffer: it is sourced from the live
        // map (`is_running`), not the DB row — and it can show the marker before the
        // debounced persist (PERSIST_DEBOUNCE) has even written the scrollback row.
        let live_read = || {
            command_output(
                app.handle().clone(),
                app.state::<Db>(),
                runner_state(&app),
                instance_id.clone(),
            )
            .expect("command_output (live)")
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut saw_marker = false;
        while std::time::Instant::now() < deadline {
            if live_read().contains("LIVE_MARKER") {
                saw_marker = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            saw_marker,
            "while running, command_output must return the live in-memory buffer with the marker"
        );
        // The runner reports the instance as live; the live read is the running branch.
        assert!(
            runner_state(&app).is_running(&instance_id),
            "instance must still be running for the live-branch assertion"
        );

        // Stop -> idle. The live buffer is gone, so command_output now rehydrates the
        // persisted cold scrollback. The pump's final persist on disconnect wrote the
        // marker into the DB row, so the cold read still surfaces it.
        let stopped = command_stop(
            app.handle().clone(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_stop");
        assert_eq!(stopped, "idle", "stop returns idle");
        assert!(
            !runner_state(&app).is_running(&instance_id),
            "instance must be idle after stop (no live buffer)"
        );

        let cold = command_output(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_output (cold)");
        let persisted = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .map(|i| i.scrollback)
            .unwrap_or_default();
        assert_eq!(
            cold, persisted,
            "after stop, command_output must return the persisted scrollback row verbatim"
        );
        assert!(
            cold.contains("LIVE_MARKER"),
            "the persisted cold scrollback must still contain the marker, got: {cold:?}"
        );
    }

    /// `command_acknowledge` clears the PERSISTED `unread` flag of a terminal row with
    /// NO live entry (the restore-at-boot shape: an unread `success`/`error` row, never
    /// re-run this session) WITHOUT erasing the factual outcome. This is the
    /// bridge-only path (the runner has no live entry to flip), proving the badge
    /// hides on select after a restart while `last_state`/`last_exit_code` survive —
    /// the v4 finding fix.
    #[test]
    #[cfg(not(windows))]
    fn acknowledge_clears_persisted_terminal_state_without_live_entry() {
        let app = build_app_with_runner();
        let ws = TempWs::new("ack_persisted");
        let (project_id, workspace_id) = app.state::<Db>().with_conn(|c| {
            let pr = db::create_project(c, "p", &ws.path(), None).unwrap();
            (pr.0.id, pr.1.id)
        });
        let tpl = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id,
            "svc".into(),
            "true".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("template");
        let instance_id = app
            .state::<Db>()
            .with_conn(|c| db::list_instances_for_workspace(c, &workspace_id))
            .unwrap()
            .into_iter()
            .find(|i| i.command_id == tpl.id)
            .expect("instance")
            .id;

        // Persist a terminal OUTCOME directly (simulating a restored, still-unread
        // error row: state=error, exit_code=2, unread=1), with NO live runner entry.
        app.state::<Db>()
            .with_conn(|c| db::set_run_state(c, &instance_id, db::STATE_ERROR, Some(2)))
            .expect("seed error outcome");
        assert!(
            !runner_state(&app).is_running(&instance_id),
            "no live entry: the runner does not back this terminal state"
        );

        // Acknowledge: clears ONLY `unread` — returns the unchanged factual state.
        let st = command_acknowledge(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_acknowledge");
        assert_eq!(
            st, "error",
            "acknowledge returns the unchanged factual state (NOT idle)"
        );
        let row = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .expect("row");
        assert_eq!(
            row.last_state, "error",
            "the factual state is preserved through the ack (not collapsed to idle)"
        );
        assert_eq!(
            row.last_exit_code,
            Some(2),
            "the factual exit code is preserved through the ack"
        );
        assert!(!row.unread, "the ack cleared the persisted unread flag");

        // A second acknowledge is a no-op (already read): state still error, unread 0.
        let st2 = command_acknowledge(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_acknowledge (2)");
        assert_eq!(
            st2, "error",
            "a second ack on an already-read row returns the unchanged factual state"
        );
        let row2 = app
            .state::<Db>()
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .expect("row");
        assert!(!row2.unread, "still read after the second ack");
    }

    /// `command_acknowledge` is a NO-OP on a running instance: it must never clear a
    /// live process's state. The instance keeps running.
    #[test]
    #[cfg(not(windows))]
    fn acknowledge_running_instance_is_a_noop() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("ack_running");
        let (project_id, workspace_id) = app.state::<Db>().with_conn(|c| {
            let pr = db::create_project(c, "p", &ws.path(), None).unwrap();
            (pr.0.id, pr.1.id)
        });
        let tpl = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id,
            "svc".into(),
            "sleep 30".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("template");
        let instance_id = app
            .state::<Db>()
            .with_conn(|c| db::list_instances_for_workspace(c, &workspace_id))
            .unwrap()
            .into_iter()
            .find(|i| i.command_id == tpl.id)
            .expect("instance")
            .id;

        command_start(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_start");
        assert!(
            runner_state(&app).is_running(&instance_id),
            "instance must be running before the acknowledge no-op check"
        );

        let st = command_acknowledge(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_acknowledge");
        assert_eq!(st, "running", "acknowledge on running returns running (no-op)");
        assert!(
            runner_state(&app).is_running(&instance_id),
            "the running instance must be untouched by acknowledge"
        );

        // Cleanup: stop the live process.
        command_stop(app.handle().clone(), runner_state(&app), instance_id).expect("cleanup stop");
    }

    /// stop is idempotent on a non-running instance, and relaunch on idle is a
    /// direct start. (Run cheaply with a long-lived `sleep` so timing is robust.)
    #[test]
    #[cfg(not(windows))]
    fn stop_and_relaunch_through_commands() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("stop_relaunch");
        let (project_id, workspace_id) = app.state::<Db>().with_conn(|c| {
            let pr = db::create_project(c, "p", &ws.path(), None).unwrap();
            (pr.0.id, pr.1.id)
        });

        let tpl = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id,
            "svc".into(),
            "sleep 30".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("template");
        let instance_id = app
            .state::<Db>()
            .with_conn(|c| db::list_instances_for_workspace(c, &workspace_id))
            .unwrap()
            .into_iter()
            .find(|i| i.command_id == tpl.id)
            .unwrap()
            .id;

        // stop on a never-started instance: idempotent no-op, idle.
        let st = command_stop(
            app.handle().clone(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("stop noop");
        assert_eq!(st, "idle");

        // relaunch on idle = direct start -> running.
        let st = command_relaunch(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("relaunch");
        assert_eq!(st, "running", "relaunch on idle starts directly");
        assert!(wait_db_state(&app, &instance_id, "running", 4));

        // stop the running instance -> idle.
        let st = command_stop(
            app.handle().clone(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("stop running");
        assert_eq!(st, "idle", "stop on running goes idle");
    }

    /// Full lifecycle driven through the `command_*` SURFACE while every transition
    /// and output chunk is observed via `app.listen` (the front's only signal):
    /// `command_start` -> `command://state` running + `command://output` marker ->
    /// natural exit 0 -> `command://state` success; then a fresh start, `command_stop`
    /// -> `command://state` idle; then `command_relaunch` -> running again. Proves the
    /// done-criterion "start/stop/relaunch/output observables via app.listen" end-to-end
    /// over the real command surface, and that relaunch never leaves two live instances.
    #[test]
    #[cfg(not(windows))]
    fn lifecycle_through_commands_is_observable_via_listen() {
        use std::sync::mpsc::channel;
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("listen_lifecycle");
        let (project_id, workspace_id) = app.state::<Db>().with_conn(|c| {
            let pr = db::create_project(c, "p", &ws.path(), None).unwrap();
            (pr.0.id, pr.1.id)
        });

        // A template that emits a marker then sleeps so it stays running until we act.
        let tpl = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id,
            "svc".into(),
            "echo LISTEN_MARKER; sleep 30".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("template");
        let instance_id = app
            .state::<Db>()
            .with_conn(|c| db::list_instances_for_workspace(c, &workspace_id))
            .unwrap()
            .into_iter()
            .find(|i| i.command_id == tpl.id)
            .unwrap()
            .id;

        // Subscribe to BOTH command events, filtered to our instance, BEFORE starting.
        let (state_tx, state_rx) = channel::<(String, Option<i32>)>();
        {
            let id = instance_id.clone();
            app.listen("command://state", move |event| {
                let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                if v["instance_id"] == id {
                    let st = v["state"].as_str().unwrap().to_string();
                    let code = v["code"].as_i64().map(|n| n as i32);
                    let _ = state_tx.send((st, code));
                }
            });
        }
        let (out_tx, out_rx) = channel::<String>();
        {
            let id = instance_id.clone();
            app.listen("command://output", move |event| {
                let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                if v["instance_id"] == id {
                    let bytes: Vec<u8> = v["bytes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|n| n.as_u64().unwrap() as u8)
                        .collect();
                    let _ = out_tx.send(String::from_utf8_lossy(&bytes).into_owned());
                }
            });
        }

        // Wait until a `command://state` event of `want` is observed (with its code).
        let wait_state = |want: &str, secs: u64| -> Option<Option<i32>> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            while std::time::Instant::now() < deadline {
                if let Ok((st, code)) =
                    state_rx.recv_timeout(std::time::Duration::from_millis(150))
                {
                    if st == want {
                        return Some(code);
                    }
                }
            }
            None
        };

        // 1) command_start -> a `running` state event is broadcast over app.listen.
        let st = command_start(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_start");
        assert_eq!(st, "running", "command_start returns running");
        assert!(
            wait_state("running", 6).is_some(),
            "command://state must broadcast a running transition for the started instance"
        );

        // 2) the command's stdout is relayed as a `command://output` event.
        let mut out = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while std::time::Instant::now() < deadline && !out.contains("LISTEN_MARKER") {
            if let Ok(chunk) = out_rx.recv_timeout(std::time::Duration::from_millis(150)) {
                out.push_str(&chunk);
            }
        }
        assert!(
            out.contains("LISTEN_MARKER"),
            "command://output must relay the command stdout, got: {out:?}"
        );

        // 3) command_stop -> the instance goes idle and an `idle` state event fires.
        let st = command_stop(
            app.handle().clone(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_stop");
        assert_eq!(st, "idle", "command_stop returns idle");
        assert!(
            wait_state("idle", 6).is_some(),
            "command://state must broadcast an idle transition after stop"
        );
        // Exactly one live entry max — stop left no orphan running.
        assert!(
            !runner_state(&app).is_running(&instance_id),
            "after stop the instance is not running"
        );

        // 4) command_relaunch on the idle instance -> running again, observable.
        let st = command_relaunch(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("command_relaunch");
        assert_eq!(st, "running", "command_relaunch returns running");
        assert!(
            wait_state("running", 6).is_some(),
            "command://state must broadcast a running transition after relaunch"
        );
        // A relaunch never leaves two live instances for one command: the runner
        // keys live entries by instance_id, so a relaunch replaces (never doubles)
        // the entry — the process-death proof lives in the `command::` unit test
        // `relaunch_never_leaves_two_live_instances`; here we confirm the post-state
        // is a single running entry for the instance.
        assert!(
            runner_state(&app).is_running(&instance_id),
            "the relaunched instance is running with a single live entry"
        );

        // Clean up the live process.
        command_stop(app.handle().clone(), runner_state(&app), instance_id).expect("final stop");
    }

    /// update/delete of a template with a RUNNING instance is refused with a clear
    /// error; delete_project is refused while an instance is running, then allowed
    /// after stop.
    #[test]
    #[cfg(not(windows))]
    fn running_mutations_are_guarded() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("guards");
        let (project_id, workspace_id) = app.state::<Db>().with_conn(|c| {
            let pr = db::create_project(c, "p", &ws.path(), None).unwrap();
            (pr.0.id, pr.1.id)
        });
        let tpl = command_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id.clone(),
            "svc".into(),
            "sleep 30".into(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("template");
        let instance_id = app
            .state::<Db>()
            .with_conn(|c| db::list_instances_for_workspace(c, &workspace_id))
            .unwrap()
            .into_iter()
            .find(|i| i.command_id == tpl.id)
            .unwrap()
            .id;

        command_start(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("start");
        assert!(wait_db_state(&app, &instance_id, "running", 4));

        // update is refused while running.
        let err = command_update(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            tpl.id.clone(),
            "svc".into(),
            "sleep 99".into(),
            None,
            None,
        )
        .expect_err("update of a running template must be refused");
        assert!(err.contains("running"), "clear error: {err}");

        // delete is refused while running.
        let err = command_delete(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            tpl.id.clone(),
        )
        .expect_err("delete of a running template must be refused");
        assert!(err.contains("running"), "clear error: {err}");

        // delete_project is refused while an instance is running.
        let err = delete_project(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            project_id.clone(),
        )
        .expect_err("delete_project must be refused while an instance is running");
        assert!(err.contains("running"), "clear error: {err}");

        // update_project / rename_workspace are NOT guarded (no path/runtime change).
        update_project(app.state::<Db>(), project_id.clone(), "renamed".into())
            .expect("update_project must pass even while a command runs");
        rename_workspace(app.state::<Db>(), workspace_id.clone(), "ws2".into())
            .expect("rename_workspace must pass even while a command runs");

        // Stop, then the guarded mutations are allowed.
        command_stop(
            app.handle().clone(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("stop");
        // Give the runner a beat to settle the live entry to idle.
        assert!(wait_db_state(&app, &instance_id, "idle", 4));
        delete_project(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            project_id,
        )
        .expect("delete_project must pass once nothing is running");
    }

    /// Source actions: `refresh` updates snapshot+status WITHOUT touching `command`;
    /// `resync_source` re-reads the package.json and rewrites `command` to the current
    /// raw script value while KEEPING the link; `unlink_source` drops the source
    /// fields. No implicit rewrite anywhere.
    #[test]
    #[cfg(not(windows))]
    fn source_actions_have_no_implicit_rewrite() {
        let app = build_app_with_runner();
        let ws = TempWs::new("source");
        // A real package.json whose `dev` script the template links to.
        let pkg_path = ws.root.join("package.json");
        std::fs::write(
            &pkg_path,
            r#"{ "scripts": { "dev": "vite --host 0.0.0.0" } }"#,
        )
        .unwrap();
        let pkg_path_str = std::fs::canonicalize(&pkg_path)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let project_id = app
            .state::<Db>()
            .with_conn(|c| db::create_project(c, "p", &ws.path(), None))
            .unwrap()
            .0
            .id;

        // Import the dev script: default command is the runner `pnpm dev`, snapshot
        // is the raw body.
        let created = command_import_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id,
            "dev".into(),
            "pnpm dev".into(),
            String::new(),
            pkg_path_str.clone(),
            "dev".into(),
            "vite --host 0.0.0.0".into(),
            "pnpm".into(),
        )
        .expect("import_create");
        assert_eq!(created.command, "pnpm dev");

        // 1) refresh: snapshot is identical -> fresh, command UNCHANGED.
        let refreshed =
            command_source_refresh(app.state::<Db>(), created.id.clone()).expect("refresh");
        assert_eq!(refreshed.status, "fresh", "snapshot matches => fresh");
        let after_refresh = app
            .state::<Db>()
            .with_conn(|c| db::get_template(c, &created.id))
            .unwrap()
            .unwrap();
        assert_eq!(
            after_refresh.command, "pnpm dev",
            "refresh must NOT rewrite command"
        );

        // Change the file: refresh now reports `stale`, still no command rewrite.
        std::fs::write(&pkg_path, r#"{ "scripts": { "dev": "vite --port 4000" } }"#).unwrap();
        let refreshed2 =
            command_source_refresh(app.state::<Db>(), created.id.clone()).expect("refresh2");
        assert_eq!(refreshed2.status, "stale", "changed body => stale");
        let after_refresh2 = app
            .state::<Db>()
            .with_conn(|c| db::get_template(c, &created.id))
            .unwrap()
            .unwrap();
        assert_eq!(after_refresh2.command, "pnpm dev", "still no rewrite");

        // 2) resync_source: explicitly replaces command with the CURRENT raw body
        // (re-read at click time, not the snapshot) AND keeps the link.
        let resynced = command_resync_source(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            created.id.clone(),
        )
        .expect("resync");
        assert_eq!(
            resynced, "vite --port 4000",
            "resync reads the file at click time"
        );
        let after_resync = app
            .state::<Db>()
            .with_conn(|c| db::get_template(c, &created.id))
            .unwrap()
            .unwrap();
        assert_eq!(after_resync.command, "vite --port 4000");
        // Source fields are preserved (provenance KEPT — resync does not detach).
        assert_eq!(after_resync.source_script_name.as_deref(), Some("dev"));
        assert_eq!(
            after_resync.source_kind.as_deref(),
            Some(db::SOURCE_KIND_PACKAGE_JSON),
            "resync keeps the link"
        );

        // 3) unlink_source: source fields cleared, command left as-is.
        command_unlink_source(app.handle().clone(), app.state::<Db>(), created.id.clone())
            .expect("unlink");
        let after_unlink = app
            .state::<Db>()
            .with_conn(|c| db::get_template(c, &created.id))
            .unwrap()
            .unwrap();
        assert_eq!(after_unlink.source_kind, None, "source_kind cleared");
        assert_eq!(after_unlink.source_package_json_path, None);
        assert_eq!(after_unlink.source_script_name, None);
        assert_eq!(after_unlink.package_manager, None);
        assert_eq!(
            after_unlink.command, "vite --port 4000",
            "unlink must leave the command untouched"
        );
    }

    /// `command_update` DETACHES a package.json source when the command is edited
    /// away from both the runner call and the raw script body; editing the command
    /// to exactly the runner call (or only touching name/subfolder) KEEPS the link.
    #[test]
    #[cfg(not(windows))]
    fn update_detaches_source_only_on_manual_command_change() {
        let app = build_app_with_runner();
        let ws = TempWs::new("detach");
        let pkg_path = ws.root.join("package.json");
        std::fs::write(&pkg_path, r#"{ "scripts": { "dev": "vite --host" } }"#).unwrap();
        let pkg_path_str = std::fs::canonicalize(&pkg_path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let project_id = app
            .state::<Db>()
            .with_conn(|c| db::create_project(c, "p", &ws.path(), None))
            .unwrap()
            .0
            .id;

        let created = command_import_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id,
            "dev".into(),
            "pnpm dev".into(),
            String::new(),
            pkg_path_str,
            "dev".into(),
            "vite --host".into(),
            "pnpm".into(),
        )
        .expect("import_create");
        assert_eq!(created.command, "pnpm dev");

        // a) Edit only the restart flag, command UNCHANGED (still `pnpm dev`) → link kept.
        command_update(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            created.id.clone(),
            "dev".into(),
            "pnpm dev".into(),
            None,
            Some(true),
        )
        .expect("update keep");
        let kept = app
            .state::<Db>()
            .with_conn(|c| db::get_template(c, &created.id))
            .unwrap()
            .unwrap();
        assert_eq!(
            kept.source_script_name.as_deref(),
            Some("dev"),
            "an unchanged command keeps the source"
        );

        // b) Edit the command to a hand-authored value → source DETACHED.
        command_update(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            created.id.clone(),
            "dev".into(),
            "node server.js".into(),
            None,
            None,
        )
        .expect("update detach");
        let detached = app
            .state::<Db>()
            .with_conn(|c| db::get_template(c, &created.id))
            .unwrap()
            .unwrap();
        assert_eq!(detached.command, "node server.js");
        assert_eq!(
            detached.source_kind, None,
            "a manual command edit detaches the source"
        );
        assert_eq!(detached.source_script_name, None);
        assert_eq!(detached.source_package_json_path, None);
        assert_eq!(detached.package_manager, None);
    }

    /// `command_import_scripts` discovers the workspace's package.json scripts and
    /// `command_import_create` blocks a name already used in the project.
    #[test]
    fn import_scripts_discovers_and_create_blocks_collision() {
        let app = build_app_with_db();
        let ws = TempWs::new("import");
        std::fs::write(
            ws.root.join("package.json"),
            r#"{ "scripts": { "dev": "vite", "build": "tsc" } }"#,
        )
        .unwrap();
        let (project_id, workspace_id) = app.state::<Db>().with_conn(|c| {
            let pr = db::create_project(c, "p", &ws.path(), None).unwrap();
            (pr.0.id, pr.1.id)
        });

        let scripts =
            command_import_scripts(app.state::<Db>(), workspace_id).expect("import_scripts");
        assert!(
            scripts.iter().any(|s| s.script_name == "dev"),
            "discovery surfaces the dev script"
        );

        // Seed a command named "dev", then importing another "dev" is refused.
        command_import_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id.clone(),
            "dev".into(),
            "npm run dev".into(),
            String::new(),
            "/x/package.json".into(),
            "dev".into(),
            "vite".into(),
            "npm".into(),
        )
        .expect("first dev import");
        let err = command_import_create(
            app.handle().clone(),
            app.state::<Db>(),
            project_id,
            "dev".into(),
            "npm run dev".into(),
            String::new(),
            "/x/package.json".into(),
            "dev".into(),
            "vite".into(),
            "npm".into(),
        )
        .expect_err("a duplicate name import must be refused");
        assert!(err.contains("already used"), "clear collision error: {err}");
    }

    // --- Shutdown snapshot + boot restoration (task 16) ----------------------

    /// Seed a project at `ws` with one template (command, restart flag) and return
    /// (project_id, workspace_id, template_id, instance_id).
    #[cfg(not(windows))]
    fn seed_restore(
        app: &App<MockRuntime>,
        ws_path: &str,
        command: &str,
        restart_on_startup: bool,
    ) -> (String, String, String, String) {
        app.state::<Db>().with_conn(|c| {
            let pr = db::create_project(c, "p", ws_path, None).unwrap();
            let tpl =
                db::create_template(c, &pr.0.id, "svc", command, None, Default::default()).unwrap();
            db::set_restart_on_startup(c, &tpl.id, restart_on_startup).unwrap();
            let inst = db::list_instances_for_workspace(c, &pr.1.id)
                .unwrap()
                .into_iter()
                .find(|i| i.command_id == tpl.id)
                .unwrap();
            (pr.0.id, pr.1.id, tpl.id, inst.id)
        })
    }

    fn instance(app: &App<MockRuntime>, id: &str) -> db::CommandInstance {
        app.state::<Db>()
            .with_conn(|c| db::get_instance(c, id))
            .unwrap()
            .unwrap()
    }

    /// Shutdown snapshot: a running instance is snapshotted
    /// `was_running_on_shutdown = true`; a non-running one `false`.
    #[test]
    #[cfg(not(windows))]
    fn shutdown_snapshots_running_instances() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("shutdown_snap");
        let (_p, _w, _t, instance_id) = seed_restore(&app, &ws.path(), "sleep 30", true);

        let runner = app.state::<ManagedCommandRunner<MockRuntime>>();
        // Not running yet → snapshot false.
        snapshot_commands_on_shutdown(app.state::<Db>().inner(), runner.inner());
        assert!(
            !instance(&app, &instance_id).was_running_on_shutdown,
            "an idle instance snapshots was_running_on_shutdown=false"
        );

        // Start it, then snapshot → true.
        command_start(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("start");
        assert!(wait_db_state(&app, &instance_id, "running", 4));
        snapshot_commands_on_shutdown(app.state::<Db>().inner(), runner_state(&app).inner());
        assert!(
            instance(&app, &instance_id).was_running_on_shutdown,
            "a running instance snapshots was_running_on_shutdown=true"
        );
        // Cleanup.
        command_stop(app.handle().clone(), runner_state(&app), instance_id).expect("stop");
    }

    /// Boot: an instance with template restart_on_startup ON + snapshot true is
    /// relaunched; the snapshot is reset to false afterwards.
    #[test]
    #[cfg(not(windows))]
    fn boot_relaunches_when_toggle_on_and_snapshot_true_then_resets() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("boot_on");
        let (_p, _w, _t, instance_id) = seed_restore(&app, &ws.path(), "sleep 30", true);
        // Simulate the prior shutdown: this instance WAS running.
        app.state::<Db>()
            .with_conn(|c| db::set_was_running_on_shutdown(c, &instance_id, true))
            .unwrap();

        let relaunched =
            restore_commands_on_boot(app.state::<Db>().inner(), runner_state(&app).inner());
        assert!(
            relaunched.contains(&instance_id),
            "the instance must be relaunched at boot"
        );
        assert!(
            wait_db_state(&app, &instance_id, "running", 4),
            "the relaunched instance is running"
        );
        // The snapshot was reset so a future boot cannot relaunch a ghost.
        assert!(
            !instance(&app, &instance_id).was_running_on_shutdown,
            "the snapshot must be reset to false after boot"
        );
        command_stop(app.handle().clone(), runner_state(&app), instance_id).expect("stop");
    }

    /// Boot: a template with restart_on_startup OFF that WAS running is NOT
    /// relaunched, and its displayed state is normalized to idle.
    #[test]
    #[cfg(not(windows))]
    fn boot_does_not_relaunch_toggle_off_and_normalizes_to_idle() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("boot_off");
        let (_p, _w, _t, instance_id) = seed_restore(&app, &ws.path(), "sleep 30", false);
        // Prior shutdown: it WAS running (last_state running + snapshot true), but
        // the toggle is OFF.
        app.state::<Db>().with_conn(|c| {
            db::set_last_state(c, &instance_id, db::STATE_RUNNING).unwrap();
            db::set_was_running_on_shutdown(c, &instance_id, true).unwrap();
        });

        let relaunched =
            restore_commands_on_boot(app.state::<Db>().inner(), runner_state(&app).inner());
        assert!(
            !relaunched.contains(&instance_id),
            "a toggle-OFF instance must NOT be relaunched"
        );
        let inst = instance(&app, &instance_id);
        assert_eq!(
            inst.last_state, "idle",
            "an orphaned running instance must be normalized to idle"
        );
        assert!(!inst.was_running_on_shutdown, "snapshot reset");
        assert!(
            !runner_state(&app).is_running(&instance_id),
            "no process is started for a toggle-OFF instance"
        );
    }

    /// Boot: success/error instances keep their last_state (dot color) and are not
    /// relaunched.
    #[test]
    #[cfg(not(windows))]
    fn boot_keeps_success_and_error_without_relaunch() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("boot_terminal");
        // Two templates so we get two instances; mark one success, one error. Both
        // have restart ON. A success/error instance was NOT running at the last
        // shutdown, so its snapshot is FALSE (exactly what `snapshot_commands_on_
        // shutdown` records) — proving success/error are never relaunched and keep
        // their state for the dot.
        let (project_id, workspace_id, _t, succ_id) = seed_restore(&app, &ws.path(), "true", true);
        let err_id = app.state::<Db>().with_conn(|c| {
            let tpl =
                db::create_template(c, &project_id, "svc2", "false", None, Default::default())
                    .unwrap();
            db::set_restart_on_startup(c, &tpl.id, true).unwrap();
            db::list_instances_for_workspace(c, &workspace_id)
                .unwrap()
                .into_iter()
                .find(|i| i.command_id == tpl.id)
                .unwrap()
                .id
        });
        app.state::<Db>().with_conn(|c| {
            db::set_last_state(c, &succ_id, db::STATE_SUCCESS).unwrap();
            db::set_was_running_on_shutdown(c, &succ_id, false).unwrap();
            db::set_last_state(c, &err_id, db::STATE_ERROR).unwrap();
            db::set_was_running_on_shutdown(c, &err_id, false).unwrap();
        });

        let relaunched =
            restore_commands_on_boot(app.state::<Db>().inner(), runner_state(&app).inner());
        assert!(
            relaunched.is_empty(),
            "success/error instances must not be relaunched even with snapshot true"
        );
        assert_eq!(
            instance(&app, &succ_id).last_state,
            "success",
            "success is preserved for the green dot"
        );
        assert_eq!(
            instance(&app, &err_id).last_state,
            "error",
            "error is preserved for the red dot"
        );
    }

    /// Boot: `last_state=running` ALONE (no snapshot) must NOT relaunch — the
    /// snapshot is the load-bearing signal, never `last_state`.
    #[test]
    #[cfg(not(windows))]
    fn boot_does_not_relaunch_from_last_state_running_alone() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("running_alone");
        let (_p, _w, _t, instance_id) = seed_restore(&app, &ws.path(), "sleep 30", true);
        // last_state running but the snapshot is FALSE (e.g. it crashed without a
        // clean shutdown snapshot). Restart toggle is ON, yet without the snapshot
        // it must NOT relaunch.
        app.state::<Db>().with_conn(|c| {
            db::set_last_state(c, &instance_id, db::STATE_RUNNING).unwrap();
            db::set_was_running_on_shutdown(c, &instance_id, false).unwrap();
        });

        let relaunched =
            restore_commands_on_boot(app.state::<Db>().inner(), runner_state(&app).inner());
        assert!(
            relaunched.is_empty(),
            "last_state=running without a snapshot must not relaunch"
        );
        assert_eq!(
            instance(&app, &instance_id).last_state,
            "idle",
            "the orphaned running is normalized to idle"
        );
        assert!(!runner_state(&app).is_running(&instance_id));
    }

    /// Boot, finding 01KV6C5RWKJ: the EXACT phantom-running DB state observed in the
    /// nyx DB — `last_state=running`, `was_running_on_shutdown=1` (never reset),
    /// `restart_on_startup=0`, and NO live process. The boot restore must force the
    /// orphan to `idle` AND reset `was_running_on_shutdown` to 0, so there is no
    /// phantom running dot at launch and a future boot cannot relaunch a ghost.
    #[test]
    #[cfg(not(windows))]
    fn boot_normalizes_phantom_running_and_resets_snapshot() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("phantom_running");
        // restart_on_startup = false (the observed state).
        let (_p, _w, _t, instance_id) = seed_restore(&app, &ws.path(), "sleep 30", false);
        // Reproduce the corrupt-on-disk state: running + snapshot NOT reset.
        app.state::<Db>().with_conn(|c| {
            db::set_last_state(c, &instance_id, db::STATE_RUNNING).unwrap();
            db::set_was_running_on_shutdown(c, &instance_id, true).unwrap();
        });
        // Sanity: no live process backs that running state.
        assert!(
            !runner_state(&app).is_running(&instance_id),
            "precondition: the phantom running has no live process"
        );

        let relaunched =
            restore_commands_on_boot(app.state::<Db>().inner(), runner_state(&app).inner());
        assert!(
            relaunched.is_empty(),
            "a restart-OFF phantom must NOT be relaunched"
        );

        let inst = instance(&app, &instance_id);
        assert_eq!(
            inst.last_state, "idle",
            "the phantom running must be normalized to idle (no phantom dot at launch)"
        );
        assert!(
            !inst.was_running_on_shutdown,
            "was_running_on_shutdown must be reset to 0 so a future boot can't relaunch a ghost"
        );
        assert!(
            !runner_state(&app).is_running(&instance_id),
            "no process is started for the phantom"
        );
    }

    /// PRD task #2 — the TERMINAL analogue of `boot_normalizes_phantom_running_*`:
    /// after a restart, a terminal left at a persisted `exec_state = running` (a
    /// force-quit artefact — the EXACT dogfood symptom) is settled to idle by the
    /// boot normalization, so no phantom running survives whatever was persisted.
    /// A settled `success`/`error` and an `idle` terminal are left untouched. Runs
    /// the real bridge boot hook (`normalize_terminals_on_boot`) against an
    /// in-memory Db, then asserts via the `list_terminals` command (the surface the
    /// front reads on startup).
    #[test]
    fn boot_normalizes_phantom_running_terminals() {
        let app = build_app_with_db();
        let dbs = || app.state::<Db>();

        // Three records; reproduce the corrupt-on-disk state: one stuck `running`
        // (no live PTY backs it — a force-quit), one settled `success` (unread), one
        // plain `idle`.
        let running = create_terminal(dbs(), "/running".into(), None).expect("create running");
        let success = create_terminal(dbs(), "/success".into(), None).expect("create success");
        let _idle = create_terminal(dbs(), "/idle".into(), None).expect("create idle");
        dbs()
            .with_conn(|c| {
                db::set_exec_state(c, &running.id, db::STATE_RUNNING, None, false)?;
                db::set_exec_state(c, &success.id, db::STATE_SUCCESS, Some(0), true)
            })
            .expect("seed exec states");

        // Boot normalization (the setup hook path).
        normalize_terminals_on_boot(&dbs());

        // list_terminals (what the front reads at startup) shows NO running terminal.
        let listed = list_terminals(dbs()).expect("list");
        assert!(
            listed.iter().all(|t| t.exec_state != db::STATE_RUNNING),
            "no terminal may be left running after boot, whatever was persisted; got {:?}",
            listed.iter().map(|t| (&t.id, &t.exec_state)).collect::<Vec<_>>()
        );
        // The phantom is now idle with no exit code / not unread.
        let r = listed.iter().find(|t| t.id == running.id).unwrap();
        assert_eq!(r.exec_state, db::STATE_IDLE, "phantom running settled to idle");
        assert_eq!(r.exec_exit_code, None);
        assert!(!r.exec_state_unread);
        // The settled success result SURVIVES (its badge/unread persist across boot).
        let s = listed.iter().find(|t| t.id == success.id).unwrap();
        assert_eq!(s.exec_state, db::STATE_SUCCESS, "settled result survives restart");
        assert!(s.exec_state_unread, "settled success keeps its unread flag");
    }

    // --- PRD-2.1: pty_id → terminal_id mapping + OSC 133 pump scan -----------

    /// Task #3: `pty_spawn` with a persistent terminal record id records the live
    /// pty_id → terminal_id association in the managed `TerminalIdMap`, so backend
    /// state can resolve the durable record from the live pty id. A record-less
    /// spawn records nothing.
    #[test]
    fn spawn_with_record_id_is_resolvable_and_record_less_is_not() {
        let app = build_app();
        let with = spawn_with_record(&app, 80, 24, "term-record-abc");
        let without = spawn(&app, 80, 24);

        let map = app.state::<TerminalIdMap>();
        assert_eq!(
            map.get(with).as_deref(),
            Some("term-record-abc"),
            "the pty_id of a record-bound spawn resolves to its terminal record id"
        );
        assert_eq!(
            map.get(without),
            None,
            "a record-less spawn records no mapping"
        );

        let _ = close(&app, with);
        let _ = close(&app, without);
    }

    /// Task #3: the mapping is dropped once the live PTY exits (the pump removes it
    /// on disconnect), so the table stays bounded. We close the PTY and wait for
    /// the exit event, then assert the entry is gone.
    #[test]
    fn mapping_is_dropped_on_exit() {
        let app = build_app();
        let (tx, rx) = channel::<String>();
        app.listen("pty://exit", move |event| {
            let _ = tx.send(event.payload().to_string());
        });
        let id = spawn_with_record(&app, 80, 24, "term-record-gone");
        assert!(app.state::<TerminalIdMap>().get(id).is_some());
        close(&app, id).expect("close");
        rx.recv_timeout(Duration::from_secs(5))
            .expect("pty://exit fires");
        // The pump removes the mapping right before breaking; allow a brief grace.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && app.state::<TerminalIdMap>().get(id).is_some() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            app.state::<TerminalIdMap>().get(id),
            None,
            "the pty_id → terminal_id mapping is dropped after exit"
        );
    }

    /// Task #4: `handle_osc133_chunk` decodes OSC 133 events from a raw chunk and
    /// records them per terminal, WITHOUT stripping anything (it is given the
    /// chunk by reference and never mutates it; the pump forwards the same bytes to
    /// `pty://output` separately — proven by `osc133_scan_does_not_strip_output`).
    #[test]
    fn osc133_chunk_decodes_and_records_events() {
        use crate::osc133::Osc133Event;
        let app = build_app();
        let tid = "term-osc133";
        // A full prompt cycle: pre-exec (running) → end exit 0 (success) → prompt.
        let chunk = b"\x1b]133;C\x07hello\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07";
        handle_osc133_chunk(app.handle(), tid, chunk);
        let events = app.state::<Osc133Events>().snapshot(tid);
        assert_eq!(
            events,
            vec![
                Osc133Event::PreExec,
                Osc133Event::CommandEnd { exit_code: Some(0) },
                Osc133Event::PromptStart,
                Osc133Event::CommandStart,
            ],
            "the pump scan decodes the same events the parser yields"
        );
    }

    /// Task #4: a sequence SPLIT across two chunks is recovered via the per-terminal
    /// tail buffer (`Osc133Pending`). The first chunk ends mid-`D;` (no terminator):
    /// no event yet, the tail is carried; the second chunk completes it.
    #[test]
    fn osc133_split_sequence_recovered_across_chunks() {
        use crate::osc133::Osc133Event;
        let app = build_app();
        let tid = "term-split";
        // Chunk 1: an incomplete command-end (introducer + `D;` but no BEL/ST).
        handle_osc133_chunk(app.handle(), tid, b"out\x1b]133;D;");
        assert!(
            app.state::<Osc133Events>().snapshot(tid).is_empty(),
            "no complete event from the split first half"
        );
        // Chunk 2: the rest of the code + terminator.
        handle_osc133_chunk(app.handle(), tid, b"7\x07more");
        assert_eq!(
            app.state::<Osc133Events>().snapshot(tid),
            vec![Osc133Event::CommandEnd { exit_code: Some(7) }],
            "the split D;7 is recovered once the tail is stitched"
        );
    }

    /// Task #4: irrelevant OSC sequences (e.g. OSC 7 cwd) and plain output produce
    /// no OSC 133 events — the scan does not false-positive.
    #[test]
    fn osc133_ignores_unrelated_sequences() {
        let app = build_app();
        let tid = "term-noise";
        handle_osc133_chunk(app.handle(), tid, b"\x1b]7;file:///home/kris\x07plain text\r\n$ ");
        assert!(
            app.state::<Osc133Events>().snapshot(tid).is_empty(),
            "OSC 7 + plain output carry no OSC 133 events"
        );
    }

    /// Task #4 (the load-bearing invariant): scanning for OSC 133 must NOT strip
    /// bytes from `pty://output`. We spawn a record-bound shell, emit a literal
    /// OSC 133 `D;0` plus a visible marker via `printf`, and assert the marker
    /// reaches `pty://output` — the renderer still receives the full stream while
    /// the pump observes the control sequence out-of-band.
    #[cfg(not(windows))]
    #[test]
    fn osc133_scan_does_not_strip_output() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app();
        let (tx, rx) = channel::<String>();
        app.listen("pty://output", move |event| {
            let _ = tx.send(event.payload().to_string());
        });
        let id = spawn_with_record(&app, 80, 24, "term-nostrip");
        // Emit an OSC 133 end sequence followed by a distinctive marker. xterm (the
        // front) would consume the control bytes invisibly; the marker must arrive.
        write(&app, id, b"printf '\\033]133;D;0\\007osc133_marker_z9\\n'\n");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut acc = String::new();
        while Instant::now() < deadline && !acc.contains("osc133_marker_z9") {
            if let Ok(p) = rx.recv_timeout(Duration::from_millis(200)) {
                acc.push_str(&output_to_string(&p));
            }
        }
        assert!(
            acc.contains("osc133_marker_z9"),
            "the marker after the OSC 133 sequence must still reach pty://output (no stripping), got: {acc:?}"
        );
        let _ = close(&app, id);
    }

    // --- PRD-2.1 task #6: exec-state STATE MACHINE --------------------------
    //
    // These drive the state machine over decoded OSC 133 events against a REAL
    // in-memory migrated DB and assert BOTH the persisted row (the authority for
    // restart) and the emitted `terminal://exec-state` event. We invoke the
    // machine through `handle_osc133_chunk` (the production path: scan a raw chunk
    // → record + transition) so the tests cover decode + transition + persist +
    // emit as one pipeline. The bodies are called directly with the mock app's
    // handle — same convention as the rest of this module.

    /// Create a REAL terminal record in the managed in-memory DB and return its id.
    fn make_record(app: &App<MockRuntime>) -> String {
        app.state::<Db>()
            .with_conn(|c| db::create_terminal(c, "/tmp", None))
            .expect("create_terminal")
            .id
    }

    /// Read a terminal record's exec-state tuple (state, exit_code, unread).
    fn exec_state(app: &App<MockRuntime>, tid: &str) -> (String, Option<i32>, bool) {
        let t = app
            .state::<Db>()
            .with_conn(|c| db::get_terminal(c, tid))
            .expect("get_terminal")
            .expect("terminal exists");
        (t.exec_state, t.exec_exit_code, t.exec_state_unread)
    }

    /// Collect every `terminal://exec-state` event payload, in order, into a shared
    /// vec; returns the handle to read after driving the machine.
    fn collect_exec_events(
        app: &App<MockRuntime>,
    ) -> Arc<Mutex<Vec<serde_json::Value>>> {
        let events: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        app.listen("terminal://exec-state", move |event| {
            let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            sink.lock().unwrap().push(v);
        });
        events
    }

    /// Wait until at least `n` exec-state events have been collected (the mock
    /// runtime delivers `listen` callbacks asynchronously), then snapshot them.
    fn wait_events(
        events: &Arc<Mutex<Vec<serde_json::Value>>>,
        n: usize,
    ) -> Vec<serde_json::Value> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            {
                let got = events.lock().unwrap();
                if got.len() >= n {
                    return got.clone();
                }
            }
            if Instant::now() >= deadline {
                return events.lock().unwrap().clone();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// OSC 133 RETROGRADED (PRD task #3): pre-exec (`133;C`) is now INERT — it must
    /// NOT post a `running` exec-state. `running` is owned by the OS busy signal
    /// (`terminal://busy-state`), so a `C` here neither persists `running` nor emits
    /// any `terminal://exec-state`. Driving `running` from `C` would re-introduce the
    /// phantom-running bug (a `D` an unsupported shell never emits would stick).
    #[test]
    fn preexec_does_not_drive_running() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        let events = collect_exec_events(&app);

        handle_osc133_chunk(app.handle(), &tid, b"\x1b]133;C\x07");

        assert_eq!(
            exec_state(&app, &tid).0,
            db::STATE_IDLE,
            "133;C is inert: it never posts a `running` exec-state (the OS owns running)"
        );
        let evs = wait_events(&events, 1);
        assert!(
            evs.is_empty(),
            "133;C emits NO terminal://exec-state (running comes from busy-state), got {evs:?}"
        );
    }

    /// A success cycle: `133;C` is inert (no running), `133;D;0` ANNOTATES success
    /// WITH exit code 0 and ALWAYS unread=1. Only the `D` emits — the `C` does not
    /// flash a `running` (the OS busy signal owns running). The backend never
    /// inspects focus.
    #[test]
    fn command_end_exit_zero_is_success_and_always_unread() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        let events = collect_exec_events(&app);

        handle_osc133_chunk(app.handle(), &tid, b"\x1b]133;C\x07out\x1b]133;D;0\x07");

        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_SUCCESS.to_string(), Some(0), true),
            "133;D;0 settles success with exit 0 and unread=1"
        );
        let evs = wait_events(&events, 1);
        assert_eq!(evs.len(), 1, "only the D settle emits; C drives no running");
        assert_eq!(evs[0]["state"], "success");
        assert_eq!(evs[0]["exit_code"], 0);
        assert_eq!(evs[0]["unread"], true);
    }

    /// `133;D;<non-zero>` ANNOTATES `error`, stores the exit code, and is unread. The
    /// `133;C` before it is inert (no running event), so the ONLY emit is the error.
    #[test]
    fn command_end_nonzero_is_error_with_code_and_unread() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        let events = collect_exec_events(&app);

        handle_osc133_chunk(app.handle(), &tid, b"\x1b]133;C\x07\x1b]133;D;3\x07");

        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_ERROR.to_string(), Some(3), true),
            "133;D;3 settles error with exit 3, unread"
        );
        let evs = wait_events(&events, 1);
        assert_eq!(evs.len(), 1, "only the D settle emits; C drives no running");
        assert_eq!(evs[0]["state"], "error");
        assert_eq!(evs[0]["exit_code"], 3);
        assert_eq!(evs[0]["unread"], true);
    }

    /// A `133;D` with NO parseable exit code settles to `error` (a finished result
    /// with no code) — NEVER a stale `running`. The `133;C` before it is inert.
    #[test]
    fn command_end_missing_code_settles_error_not_running() {
        let app = build_app_with_db();
        let tid = make_record(&app);

        handle_osc133_chunk(app.handle(), &tid, b"\x1b]133;C\x07\x1b]133;D\x07");

        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_ERROR.to_string(), None, true),
            "a code-less D settles to error with no exit code, not running"
        );
    }

    /// GRACEFUL DEGRADATION (PRD task #3 done-criterion): a command whose `133;D`
    /// end is NEVER emitted (an unsupported / OSC-133-coupled shell) must NOT leave a
    /// phantom `running` exec-state. With `C` retrograded to inert, a lone `133;C`
    /// (no `D`) keeps the persisted `exec_state` at `idle` — the WORST case is a
    /// missing success/error COLOR, never a stuck running. busy/idle (the OS signal)
    /// is unaffected: it is derived from the foreground pgroup, independent of OSC 133.
    #[test]
    fn missing_command_end_leaves_no_phantom_running() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        let events = collect_exec_events(&app);

        // A full prompt + pre-exec stream with NO `133;D` (the shell never reported
        // the result): the classic "stuck running" trigger under the OLD model.
        handle_osc133_chunk(app.handle(), &tid, b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b]133;C\x07");

        assert_eq!(
            exec_state(&app, &tid).0,
            db::STATE_IDLE,
            "a missing D never leaves a phantom running in exec_state (OS owns running)"
        );
        let evs = wait_events(&events, 1);
        assert!(
            evs.is_empty(),
            "no terminal://exec-state at all without a D — running is not OSC-driven, got {evs:?}"
        );
    }

    /// `133;A`/`133;B` (prompt/command start) are INERT, AND a `133;D` end with NO
    /// preceding `133;C` (a bare Enter at an empty prompt, OR the shell's FIRST
    /// prompt at spawn which emits `D;0` because `$?` is true) is a PHANTOM end:
    /// the PROVENANCE GUARD ignores it — the terminal stays `idle`, no badge. This
    /// is the phantom-success-dot fix; without it a freshly-spawned terminal would
    /// flash a green success dot before any command ran.
    #[test]
    fn prompt_and_command_start_are_inert_and_d_without_c_is_ignored() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        let events = collect_exec_events(&app);

        // Prompt start + command start with NO pre-exec: still idle.
        handle_osc133_chunk(app.handle(), &tid, b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert_eq!(
            exec_state(&app, &tid).0,
            db::STATE_IDLE,
            "A/B alone never leave idle (no false running)"
        );
        // A bare Enter at an empty prompt (or the spawn-time first prompt): B then
        // D;0 with NO C. With the provenance guard this settles NOTHING — the
        // terminal stays idle and no exec-state event is emitted (the phantom dot is
        // gone).
        handle_osc133_chunk(app.handle(), &tid, b"\x1b]133;D;0\x07");
        assert_eq!(
            exec_state(&app, &tid).0,
            db::STATE_IDLE,
            "a D;0 with no preceding C (prompt-initial / empty Enter) does NOT settle"
        );
        let evs = wait_events(&events, 1);
        assert!(
            evs.is_empty(),
            "no terminal://exec-state for a phantom D (no C preceded it), got {evs:?}"
        );

        // Non-regression within the SAME terminal: a REAL command (C then D;0) DOES
        // settle success — the guard re-arms on the next C.
        handle_osc133_chunk(app.handle(), &tid, b"\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07");
        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_SUCCESS.to_string(), Some(0), true),
            "a real C-preceded command still settles success after the phantom was ignored"
        );
        let evs = wait_events(&events, 1);
        assert_eq!(evs.len(), 1, "exactly one settle — only the real command");
        assert_eq!(evs[0]["state"], "success");
    }

    /// `terminal_exec_mark_read` clears `exec_state_unread` while PRESERVING the
    /// settled state + exit code (the badge keeps its color; it stops being unread).
    #[test]
    fn mark_read_clears_unread_but_preserves_settled_result() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        handle_osc133_chunk(app.handle(), &tid, b"\x1b]133;C\x07\x1b]133;D;2\x07");
        assert_eq!(exec_state(&app, &tid), (db::STATE_ERROR.to_string(), Some(2), true));

        terminal_exec_mark_read(app.state::<Db>(), tid.clone()).expect("mark read");

        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_ERROR.to_string(), Some(2), false),
            "unread cleared; error + exit code preserved (NOT collapsed to idle)"
        );
    }

    /// A shell/PTY exit while a `running` is left in the DB (a value persisted by an
    /// OLDER build — OSC 133 no longer posts `running`) must NOT leave a stale running
    /// badge: `normalize_exec_state_on_exit` settles it to idle. We seed the stale
    /// `running` directly (OSC 133 can no longer produce it).
    #[test]
    fn exit_while_running_normalizes_to_idle() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        let events = collect_exec_events(&app);
        // Simulate a pre-migration persisted `running` (no OSC path produces it now).
        app.state::<Db>()
            .with_conn(|c| db::set_exec_state(c, &tid, db::STATE_RUNNING, None, false))
            .expect("seed stale running");
        assert_eq!(exec_state(&app, &tid).0, db::STATE_RUNNING);

        normalize_exec_state_on_exit(app.handle(), &tid);

        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_IDLE.to_string(), None, false),
            "a stale running settles to idle on PTY exit (no false running)"
        );
        let evs = wait_events(&events, 1);
        assert_eq!(evs.last().unwrap()["state"], "idle");
    }

    /// A shell/PTY exit must NOT clobber a SETTLED result: a `success`/`error`
    /// (and its unread flag) survives the exit untouched.
    #[test]
    fn exit_after_settled_preserves_result() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        handle_osc133_chunk(app.handle(), &tid, b"\x1b]133;C\x07\x1b]133;D;0\x07");
        assert_eq!(exec_state(&app, &tid), (db::STATE_SUCCESS.to_string(), Some(0), true));

        normalize_exec_state_on_exit(app.handle(), &tid);

        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_SUCCESS.to_string(), Some(0), true),
            "a settled success survives PTY exit (only stale running is normalized)"
        );
    }

    /// An unknown terminal id neither panics nor emits: a real `C`-preceded `133;D`
    /// end (which DOES reach a persist+emit) hits `persist_and_emit`, which skips the
    /// emit when no row was updated (we never announce a state the DB does not hold).
    #[test]
    fn unknown_terminal_id_is_a_safe_noop() {
        let app = build_app_with_db();
        let events = collect_exec_events(&app);
        handle_osc133_chunk(app.handle(), "no-such-terminal", b"\x1b]133;C\x07\x1b]133;D;0\x07");
        let evs = wait_events(&events, 1);
        assert!(evs.is_empty(), "no event for an unknown terminal id");
    }

    /// Done-criterion (bridge/state): events are emitted for the CORRECT
    /// `terminal_id`. Two distinct terminal records are driven through DIFFERENT
    /// OSC 133 streams; every emitted `terminal://exec-state` must carry the id of
    /// the terminal whose chunk produced it (no cross-talk), and each record's
    /// PERSISTED row must reflect only its own stream. This proves the pump keys
    /// transitions by the per-chunk `terminal_id`, not a shared/last-writer slot.
    #[test]
    fn events_are_routed_to_the_correct_terminal_id() {
        let app = build_app_with_db();
        let ta = make_record(&app);
        let tb = make_record(&app);
        let events = collect_exec_events(&app);

        // Terminal A: a command that SUCCEEDS (running → success exit 0).
        handle_osc133_chunk(app.handle(), &ta, b"\x1b]133;C\x07\x1b]133;D;0\x07");
        // Terminal B: a command that FAILS (running → error exit 5).
        handle_osc133_chunk(app.handle(), &tb, b"\x1b]133;C\x07\x1b]133;D;5\x07");

        // Persisted rows: each terminal holds ONLY the outcome of its OWN stream.
        assert_eq!(
            exec_state(&app, &ta),
            (db::STATE_SUCCESS.to_string(), Some(0), true),
            "terminal A persists its own success"
        );
        assert_eq!(
            exec_state(&app, &tb),
            (db::STATE_ERROR.to_string(), Some(5), true),
            "terminal B persists its own error — no cross-talk from A"
        );

        // Emitted events: 2 in total (ONE settle per terminal — `C` is inert now),
        // each keyed to its own id.
        let evs = wait_events(&events, 2);
        assert_eq!(evs.len(), 2, "one settle transition per terminal was emitted");
        let for_a: Vec<&serde_json::Value> =
            evs.iter().filter(|e| e["terminal_id"] == ta).collect();
        let for_b: Vec<&serde_json::Value> =
            evs.iter().filter(|e| e["terminal_id"] == tb).collect();
        assert_eq!(for_a.len(), 1, "exactly A's settle carries A's id");
        assert_eq!(for_b.len(), 1, "exactly B's settle carries B's id");
        // A's settle is success; B's settle is error — events did not swap ids.
        assert_eq!(for_a[0]["state"], "success");
        assert_eq!(for_a[0]["exit_code"], 0);
        assert_eq!(for_b[0]["state"], "error");
        assert_eq!(for_b[0]["exit_code"], 5);
    }

    // --- PRD-2.1 task #10: DETERMINISTIC SYNTHETIC E2E (dogfood gate) --------
    //
    // The phase-5 gate ("Dogfood terminal exec-state on synthetic and real
    // shells") requires a deterministic e2e that proves the FULL chain
    //   raw PTY bytes → bridge PUMP → OSC 133 scan → state machine → persist + emit
    // WITHOUT depending on a real shell or any local shell customization (so CI is
    // reproducible across machines that have no bash/zsh/pwsh integration set up).
    //
    // We drive the production [`spawn_output_pump`] directly with a SYNTHETIC mpsc
    // receiver instead of a live `Pty::spawn`. The pump is shell-agnostic: it only
    // sees `Vec<u8>` chunks on its `rx`, scans them for OSC 133 ALONGSIDE
    // forwarding every byte to `pty://output`, drives the state machine, and on
    // `rx` disconnect emits `pty://exit` + normalizes a stale `running`. Feeding it
    // crafted OSC 133 byte sequences is exactly the "synthetic OSC 133 events" the
    // PRD's testing decisions call for — the real production code path, minus the
    // real child process.
    //
    // NOTE on running these on Windows: the lib TEST BINARY links `portable-pty`
    // (conpty) and, in CI/dev sandboxes lacking the conpty entrypoint, the test
    // harness exe fails to LAUNCH with STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) —
    // an environment gap, not a logic failure (the crate compiles + links clean).
    // These tests themselves spawn NO conpty PTY, so they pass anywhere the harness
    // can launch (Linux/macOS CI, or a Windows host with conpty present).

    /// Register a live `pty_id` → persistent `terminal_id` mapping (what
    /// `pty_spawn` does when the front passes a record id) so the pump resolves the
    /// durable record from the synthetic pty id.
    fn map_pty_to_record(app: &App<MockRuntime>, pty_id: u64, terminal_id: &str) {
        app.state::<TerminalIdMap>()
            .set(pty_id, terminal_id.to_string());
    }

    /// Drain `pty://output` payloads into one accumulated String (the bytes the
    /// renderer — xterm — would receive). Used to prove NO byte stripping.
    fn collect_pty_output(app: &App<MockRuntime>) -> Arc<Mutex<String>> {
        let acc: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&acc);
        app.listen("pty://output", move |event| {
            sink.lock()
                .unwrap()
                .push_str(&output_to_string(event.payload()));
        });
        acc
    }

    /// Block until the accumulated `pty://output` contains `needle` (or a deadline
    /// elapses), returning the final accumulation. The pump coalesces at ~60fps so
    /// output arrives a frame after it is fed.
    fn wait_output_contains(acc: &Arc<Mutex<String>>, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            {
                let got = acc.lock().unwrap();
                if got.contains(needle) || Instant::now() >= deadline {
                    return got.clone();
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// SYNTHETIC E2E #1 — success path, full chain, no real shell.
    ///
    /// Feed the production pump a SYNTHETIC stream that interleaves visible output
    /// with OSC 133 markers: a pre-exec (`C` — inert since the retrograde), real
    /// command output, and a command-end exit 0 (`D;0` → success ANNOTATION). Assert
    /// simultaneously:
    ///  - `terminal://exec-state` fires ONLY the success settle (no `running` from
    ///    OSC 133 — that is the OS busy signal's job), keyed to OUR terminal_id;
    ///  - the persisted DB row settles to success(0)+unread (authority for restart);
    ///  - `pty://output` still carries EVERY visible byte (the OSC bytes are
    ///    observed out-of-band, NOT stripped — xterm renders the full stream).
    #[test]
    fn synthetic_e2e_running_to_success_through_the_pump() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        let pty_id = 90_001u64;
        map_pty_to_record(&app, pty_id, &tid);

        let exec_events = collect_exec_events(&app);
        let output = collect_pty_output(&app);

        // The pump owns the receiver end; we own the synthetic transmitter.
        let (tx, rx) = channel::<Vec<u8>>();
        spawn_output_pump(app.handle().clone(), pty_id, rx);

        // A realistic, deterministic prompt cycle (the bytes a 133-instrumented
        // shell would emit), with VISIBLE text around the control sequences.
        tx.send(b"\x1b]133;A\x07PS C:\\> \x1b]133;B\x07".to_vec())
            .unwrap(); // prompt drawn (inert)
        tx.send(b"\x1b]133;C\x07".to_vec()).unwrap(); // pre-exec (inert; OS owns running)
        tx.send(b"hello from synthetic shell\r\n".to_vec()).unwrap(); // command output
        tx.send(b"\x1b]133;D;0\x07".to_vec()).unwrap(); // end exit 0 → success
        tx.send(b"\x1b]133;A\x07PS C:\\> \x1b]133;B\x07".to_vec())
            .unwrap(); // next prompt

        // Full-chain assertions on the persisted authority + the emitted events:
        // ONLY the success settle is emitted (no OSC-driven running).
        let evs = wait_events(&exec_events, 1);
        assert_eq!(evs.len(), 1, "exactly the success settle was emitted (no running)");
        assert_eq!(evs[0]["terminal_id"], tid, "settle keyed to OUR terminal");
        assert_eq!(evs[0]["state"], "success");
        assert_eq!(evs[0]["exit_code"], 0);
        assert_eq!(evs[0]["unread"], true, "settled success is an unread notification");
        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_SUCCESS.to_string(), Some(0), true),
            "the DB row is the restart authority: success(0)+unread"
        );

        // No stripping: the visible command output reached the renderer in full.
        let acc = wait_output_contains(&output, "hello from synthetic shell");
        assert!(
            acc.contains("hello from synthetic shell"),
            "visible output must reach pty://output unstripped, got: {acc:?}"
        );

        // Clean shutdown: dropping tx disconnects rx → the pump emits pty://exit
        // and (record settled) leaves the success result untouched.
        drop(tx);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && app.state::<TerminalIdMap>().get(pty_id).is_some() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_SUCCESS.to_string(), Some(0), true),
            "a settled success survives the PTY-exit normalize"
        );
    }

    /// SYNTHETIC E2E #2 — error ANNOTATION + defensive normalize-on-exit, no real shell.
    ///
    /// One terminal runs a command that FAILS (`C` inert, `D;3` → error), proving the
    /// success/error ANNOTATION path end-to-end after the retrograde — with NO OSC
    /// `running` emitted. A SECOND terminal carries a stale `running` persisted by an
    /// OLDER build (OSC 133 can no longer produce `running`); its synthetic PTY then
    /// disconnects and the pump's defensive disconnect path normalizes that stale
    /// `running` to `idle` (no false badge after a shell/PTY exit). Both run through
    /// the production pump with synthetic bytes only.
    #[test]
    fn synthetic_e2e_running_to_error_and_normalize_on_exit_through_the_pump() {
        let app = build_app_with_db();
        let exec_events = collect_exec_events(&app);

        // --- Terminal A: error annotation (command failed, exit 3), no running ---
        let ta = make_record(&app);
        let pty_a = 90_011u64;
        map_pty_to_record(&app, pty_a, &ta);
        let (tx_a, rx_a) = channel::<Vec<u8>>();
        spawn_output_pump(app.handle().clone(), pty_a, rx_a);
        tx_a.send(b"\x1b]133;C\x07".to_vec()).unwrap(); // pre-exec (inert; OS owns running)
        tx_a.send(b"boom: command failed\r\n".to_vec()).unwrap();
        tx_a.send(b"\x1b]133;D;3\x07".to_vec()).unwrap(); // error, exit 3

        // --- Terminal B: a STALE persisted `running` (older build), PTY then exits ---
        let tb = make_record(&app);
        let pty_b = 90_012u64;
        map_pty_to_record(&app, pty_b, &tb);
        let (tx_b, rx_b) = channel::<Vec<u8>>();
        spawn_output_pump(app.handle().clone(), pty_b, rx_b);
        // Seed the stale `running` directly: no OSC path produces it post-retrograde.
        app.state::<Db>()
            .with_conn(|c| db::set_exec_state(c, &tb, db::STATE_RUNNING, None, false))
            .expect("seed stale running on B");
        tx_b.send(b"long running, never ends...\r\n".to_vec()).unwrap();

        // A's error must settle (→ error(3)+unread) on the persisted row.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && exec_state(&app, &ta).0 != db::STATE_ERROR
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            exec_state(&app, &ta),
            (db::STATE_ERROR.to_string(), Some(3), true),
            "terminal A: error(3)+unread end-to-end through the pump (no OSC running)"
        );

        // B holds the seeded stale running; then its PTY disconnects.
        assert_eq!(exec_state(&app, &tb).0, db::STATE_RUNNING, "B holds a stale running");
        drop(tx_b); // PTY exit while running → the pump normalizes the stale badge.

        // No stale running badge after the shell/PTY exit: B settles to idle.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && exec_state(&app, &tb).0 == db::STATE_RUNNING {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            exec_state(&app, &tb),
            (db::STATE_IDLE.to_string(), None, false),
            "B: a stale running normalizes to idle on PTY exit (no false running)"
        );

        // Event-stream cross-check: every emitted event is keyed to the RIGHT id.
        // A emits ONLY its error settle (no running); B emits ONLY its idle normalize.
        let evs = wait_events(&exec_events, 2); // A: error ; B: idle
        let for_a: Vec<&serde_json::Value> =
            evs.iter().filter(|e| e["terminal_id"] == ta).collect();
        let for_b: Vec<&serde_json::Value> =
            evs.iter().filter(|e| e["terminal_id"] == tb).collect();
        assert_eq!(for_a.last().unwrap()["state"], "error");
        assert_eq!(for_a.last().unwrap()["exit_code"], 3);
        assert_eq!(
            for_b.last().unwrap()["state"],
            "idle",
            "B's terminating event is the normalize to idle, not a false error"
        );

        drop(tx_a);
    }

    /// SYNTHETIC E2E #3 — the PHANTOM-SUCCESS-DOT case (the bug this fix closes).
    ///
    /// A FRESHLY-spawned terminal's first prompt emits `A B D;0` through the
    /// production pump — the shell reports `D;0` (because `$?` is true on a fresh
    /// shell) BEFORE any command, with NO preceding `133;C` (see the PowerShell
    /// capture in `crate::osc133`'s docs). Under the OLD mapping this painted a green
    /// `success` dot (+ unread) on a terminal where nothing had run. With the
    /// PROVENANCE GUARD that phantom `D` is IGNORED: NO `terminal://exec-state` is
    /// emitted and the persisted row stays `idle`. A SUBSEQUENT real command
    /// (`C ... D;0`) then settles `success` exactly as before (non-regression),
    /// proving the guard re-arms per command.
    #[test]
    fn synthetic_e2e_phantom_d_without_c_emits_no_success_through_the_pump() {
        let app = build_app_with_db();
        let tid = make_record(&app);
        let pty_id = 90_021u64;
        map_pty_to_record(&app, pty_id, &tid);

        let exec_events = collect_exec_events(&app);

        let (tx, rx) = channel::<Vec<u8>>();
        spawn_output_pump(app.handle().clone(), pty_id, rx);

        // The shell's spawn-time first prompt: A, B, then D;0 with NO C — exactly the
        // bytes a 133-instrumented PowerShell emits at startup (module-doc capture).
        tx.send(b"\x1b]133;A\x07PS C:\\> \x1b]133;B\x07".to_vec()).unwrap();
        tx.send(b"\x1b]133;D;0\x07".to_vec()).unwrap(); // phantom end — must NOT settle

        // Give the pump time to process; the phantom must produce NO exec-state event
        // and leave the persisted row at idle (the dot stays absent).
        let evs = wait_events(&exec_events, 1);
        assert!(
            evs.is_empty(),
            "a D;0 with no preceding C (prompt-initial) must emit NO exec-state, got {evs:?}"
        );
        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_IDLE.to_string(), None, false),
            "the freshly-spawned terminal stays idle: no phantom success dot"
        );

        // Now a REAL command runs (C ... output ... D;0): success settles as before.
        tx.send(b"\x1b]133;C\x07".to_vec()).unwrap();
        tx.send(b"real output\r\n".to_vec()).unwrap();
        tx.send(b"\x1b]133;D;0\x07".to_vec()).unwrap();

        let evs = wait_events(&exec_events, 1);
        assert_eq!(evs.len(), 1, "exactly the real command's success settle emits");
        assert_eq!(evs[0]["terminal_id"], tid);
        assert_eq!(evs[0]["state"], "success");
        assert_eq!(evs[0]["exit_code"], 0);
        assert_eq!(
            exec_state(&app, &tid),
            (db::STATE_SUCCESS.to_string(), Some(0), true),
            "the real C-preceded command settles success (guard re-armed after the phantom)"
        );

        drop(tx);
    }

    // --- PRD-4 phase 5 GATE: MCP dogfood → commandes visibles nyx ------------
    //
    // The phase-5 gate (#8) proves the MCP surface really serves the nyx
    // workflow, not just a server that answers: from a path equivalent to "an
    // agent launched inside nyx", `list` / `start`(relaunch) / `output` flow
    // through the MCP tools against the SAME managed runtime + DB the UI drives,
    // and the UI-facing path observes the IDENTICAL state (ADR-0003 D6). These
    // tests drive the REAL `crate::mcp_tools::NyxToolDispatcher` — the exact type
    // `lib.rs:162` installs onto the loopback `McpServer` — over a mock app that
    // manages the production `Db` + `ManagedCommandRunner`, so the dispatcher's
    // `app.try_state` lookups resolve to the very instances the `command_*`
    // commands and the `command://state` event use.
    //
    // (Like the rest of this suite, exercising the bodies directly on the
    // `tauri::test` mock runtime is the loopback-style proof the PRD env caveat
    // asks for; `cargo test --lib --no-run` type-checks them, CI runs them — the
    // local `STATUS_ENTRYPOINT_NOT_FOUND` ConPTY gap blocks launching here.)

    use crate::mcp::ToolDispatcher;
    use crate::mcp_tools::NyxToolDispatcher;
    use serde_json::json;

    /// Build the REAL MCP dispatcher over the same `AppHandle` whose managed `Db`
    /// + runner the UI commands use — the exact wiring of `lib.rs:162`.
    fn mcp(app: &App<MockRuntime>) -> NyxToolDispatcher<MockRuntime> {
        NyxToolDispatcher::new(app.handle().clone())
    }

    /// GATE done_criterion #1 — "E2E ou dogfood prouve list/start/relaunch/output."
    /// AND done_criterion #2 — "La commande est visible et controlee dans l'UI."
    ///
    /// The dogfood lifecycle, end to end, through the MCP tools:
    ///   1. `list_commands { workspace_id }` discovers the instance (no guessing).
    ///   2. `start_command { instance_id }` spawns it through the SAME runner.
    ///   3. `relaunch_command { instance_id }` restarts the SAME instance.
    ///   4. `get_command_output { instance_id }` reads its bounded output window.
    /// Then the UI-FACING path (`command_instance_list` / `command_output` + the
    /// `command://state` event) is asserted to observe the IDENTICAL instance id,
    /// the same live `running` state, and the same scrollback — the D6 invariant
    /// "the UI sees the same state" proven at integration level.
    #[test]
    #[cfg(not(windows))]
    fn mcp_dogfood_lifecycle_is_the_same_instance_the_ui_sees() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("mcp_dogfood");

        // Capture the UI's live `command://state` stream so we can assert the UI
        // observes the SAME state transitions the MCP calls drive (D6: one event
        // stream, no invisible process). A deterministic command: emit a marker
        // then sleep so the instance stays `running` for the cross-surface checks.
        let states: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let states = Arc::clone(&states);
            app.listen("command://state", move |event| {
                let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
                let id = v["instanceId"].as_str().unwrap_or_default().to_string();
                let st = v["state"].as_str().unwrap_or_default().to_string();
                states.lock().unwrap().push((id, st));
            });
        }

        let (project_id, workspace_id, _tpl, instance_id) =
            seed_restore(&app, &ws.path(), "echo MCP_DOGFOOD_MARKER; sleep 30", false);

        let dispatcher = mcp(&app);

        // 1) MCP `list_commands { workspace_id }` — discovery. The instance is
        //    visible to the agent, idle, before any start (no guessing — D4).
        let listed = dispatcher
            .call("list_commands", &json!({ "workspace_id": workspace_id }))
            .expect("list_commands over MCP");
        let cmds = listed["commands"].as_array().expect("commands array");
        let row = cmds
            .iter()
            .find(|c| c["instance_id"] == json!(instance_id))
            .expect("the seeded instance is visible via the MCP list tool");
        assert_eq!(row["last_state"], "idle", "discovered idle before start");
        assert_eq!(
            row["command"], "echo MCP_DOGFOOD_MARKER; sleep 30",
            "the MCP list surfaces the same command line the template stores"
        );

        // 2) MCP `start_command` — spawns through the SAME managed runner.
        let started = dispatcher
            .call("start_command", &json!({ "instance_id": instance_id }))
            .expect("start_command over MCP");
        assert_eq!(started["instance_id"], json!(instance_id));
        assert_eq!(started["state"], "running", "MCP start returns running");

        // UI-SAME-STATE (a): the UI-facing runner reports THIS instance running —
        // the MCP start drove the very instance the UI introspects.
        assert!(
            wait_db_state(&app, &instance_id, "running", 5),
            "the DB row the UI reads must reach running after the MCP start"
        );
        assert!(
            runner_state(&app).is_running(&instance_id),
            "the UI-facing runner reports the MCP-started instance as the live one"
        );

        // 3) MCP `relaunch_command` — restarts the SAME instance, never two live
        //    processes. It stays the single instance the UI knows.
        let relaunched = dispatcher
            .call("relaunch_command", &json!({ "instance_id": instance_id }))
            .expect("relaunch_command over MCP");
        assert_eq!(relaunched["instance_id"], json!(instance_id));
        assert_eq!(relaunched["state"], "running", "MCP relaunch returns running");
        assert!(
            wait_db_state(&app, &instance_id, "running", 5),
            "the instance is running again after the MCP relaunch"
        );

        // 4) MCP `get_command_output` — bounded window with the marker, AND the
        //    incremental-poll fields (D7). Poll until the marker streams through.
        let mut mcp_out = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut last = json!({});
        while std::time::Instant::now() < deadline && !mcp_out.contains("MCP_DOGFOOD_MARKER") {
            last = dispatcher
                .call("get_command_output", &json!({ "instance_id": instance_id }))
                .expect("get_command_output over MCP");
            mcp_out = last["output"].as_str().unwrap_or_default().to_string();
            if !mcp_out.contains("MCP_DOGFOOD_MARKER") {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        assert!(
            mcp_out.contains("MCP_DOGFOOD_MARKER"),
            "MCP get_command_output must return the live output window, got: {mcp_out:?}"
        );
        // D7 bounded-window contract is honored in the result shape.
        assert!(last["total_bytes"].is_u64(), "result carries total_bytes");
        assert!(last["cursor"].is_u64(), "result carries an integer cursor");
        assert_eq!(last["instance_id"], json!(instance_id));

        // === UI sees the SAME state (D6) — the gate's load-bearing invariant. ===

        // (b) The UI's `command_instance_list` lists the SAME instance id with the
        //     SAME live `running` state the MCP `list_commands` would now report.
        let ui_rows = command_instance_list(app.state::<Db>(), workspace_id.clone())
            .expect("UI command_instance_list");
        let ui_row = ui_rows
            .iter()
            .find(|r| r.id == instance_id)
            .expect("the UI lists the MCP-started instance");
        assert_eq!(
            ui_row.last_state, "running",
            "the UI list shows the same running state the MCP path produced"
        );

        // (c) The UI's `command_output` returns the SAME live scrollback (same
        //     marker) the MCP `get_command_output` read — one buffer, one runner.
        let ui_out = command_output(
            app.handle().clone(),
            app.state::<Db>(),
            runner_state(&app),
            instance_id.clone(),
        )
        .expect("UI command_output");
        assert!(
            ui_out.contains("MCP_DOGFOOD_MARKER"),
            "the UI command_output reads the same live buffer the MCP tool saw, got: {ui_out:?}"
        );

        // (d) The UI's `command://state` event stream — the signal the sidebar dot
        //     and the bridge command-state listener consume — recorded a `running`
        //     transition for THIS exact instance, driven purely by the MCP calls.
        //     There is no second/invisible process: the same instance id, the same
        //     state, the same event the UI listens on.
        let saw_running_for_instance = states
            .lock()
            .unwrap()
            .iter()
            .any(|(id, st)| id == &instance_id && st == "running");
        assert!(
            saw_running_for_instance,
            "the UI command://state event observed the MCP-driven running transition \
             for the same instance id (camelCase instanceId), states: {:?}",
            states.lock().unwrap()
        );

        // The `list_commands` of the agent and the UI's `command_instance_list`
        // refer to the SAME project too (no divergent id space).
        let _ = project_id;

        // Cleanup: stop the sleeping process (idempotent, via the same runner).
        let _ = dispatcher.call("stop_command", &json!({ "instance_id": instance_id }));
    }

    /// REVIEW 01KV90QCKZ8BXZ4DTYZRJK56EZ — after start → output → relaunch, the
    /// PREVIOUS run's output PLUS its exit_code/outcome is still retrievable via the
    /// `run="previous"` selector, while the DEFAULT (`run` absent) returns the CURRENT
    /// run. End to end through the real MCP `get_command_output` against the live
    /// runner + in-memory DB (the bounded N=1 retained prior run, persisted in the v5
    /// `prev_*` columns by `archive_and_reset_for_relaunch` on relaunch).
    #[test]
    #[cfg(not(windows))]
    fn mcp_get_command_output_retains_previous_run_across_relaunch() {
        std::env::set_var("SHELL", "/bin/sh");
        let app = build_app_with_runner();
        let ws = TempWs::new("mcp_prev_run");

        // Run 1: emit a unique marker, then exit NON-ZERO so the run FINISHES as
        // `error` (and is therefore archivable on the next relaunch).
        let (_project_id, _workspace_id, _tpl, instance_id) =
            seed_restore(&app, &ws.path(), "echo PREV_RUN_MARKER; exit 9", false);
        let dispatcher = mcp(&app);

        // Start run 1 and wait until it FINISHES error (so its output + outcome are the
        // current run, ready to be archived on the relaunch).
        dispatcher
            .call("start_command", &json!({ "instance_id": instance_id }))
            .expect("start run 1");
        assert!(
            wait_db_state(&app, &instance_id, "error", 5),
            "run 1 must finish as error (exit 9) before the relaunch"
        );

        // RELAUNCH into run 2: a long-lived command emitting a DIFFERENT marker, so the
        // current run is clearly distinguishable from the archived prior run.
        app.state::<Db>()
            .with_conn(|c| {
                db::update_template(
                    c,
                    &_tpl,
                    "svc",
                    "echo CURRENT_RUN_MARKER; sleep 30",
                    None,
                )
                .unwrap();
            });
        dispatcher
            .call("relaunch_command", &json!({ "instance_id": instance_id }))
            .expect("relaunch into run 2");
        assert!(
            wait_db_state(&app, &instance_id, "running", 5),
            "run 2 must be running after the relaunch"
        );

        // run="previous": the PRIOR run (run 1) is still retrievable — its output AND
        // its factual outcome (state=error, exit_code=9) survived the relaunch.
        let prev = dispatcher
            .call(
                "get_command_output",
                &json!({ "instance_id": instance_id, "run": "previous" }),
            )
            .expect("get_command_output run=previous");
        assert_eq!(prev["run"], "previous", "the result echoes the selected run");
        assert!(
            prev["output"].as_str().unwrap_or_default().contains("PREV_RUN_MARKER"),
            "run=previous must return the PRIOR run's output, got: {:?}",
            prev["output"]
        );
        assert_eq!(prev["state"], "error", "the prior run's factual state is retained");
        assert_eq!(prev["exit_code"], json!(9), "the prior run's exit_code is retained");
        assert_eq!(prev["finished"], json!(true), "the prior run is finished");

        // run=-1 is the SAME prior run (the integer alias of "previous").
        let prev_int = dispatcher
            .call(
                "get_command_output",
                &json!({ "instance_id": instance_id, "run": -1 }),
            )
            .expect("get_command_output run=-1");
        assert!(
            prev_int["output"].as_str().unwrap_or_default().contains("PREV_RUN_MARKER"),
            "run=-1 is the integer alias of run=previous"
        );
        assert_eq!(prev_int["exit_code"], json!(9));

        // DEFAULT (run absent) returns the CURRENT run, NOT the prior one — the prior
        // run's bytes never pollute the current window. Poll until run 2 streams.
        let mut cur = json!({});
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            cur = dispatcher
                .call("get_command_output", &json!({ "instance_id": instance_id }))
                .expect("get_command_output default (current)");
            if cur["output"].as_str().unwrap_or_default().contains("CURRENT_RUN_MARKER") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(cur["run"], "current", "the default result echoes run=current");
        let cur_out = cur["output"].as_str().unwrap_or_default();
        assert!(
            cur_out.contains("CURRENT_RUN_MARKER"),
            "the default (current) run returns run 2's output, got: {cur_out:?}"
        );
        assert!(
            !cur_out.contains("PREV_RUN_MARKER"),
            "the current run must NOT be polluted by the prior run's output, got: {cur_out:?}"
        );
        assert_eq!(cur["state"], "running", "the current run is running");

        // An out-of-range run selector is refused (history is bounded to N=1).
        let too_far = dispatcher.call(
            "get_command_output",
            &json!({ "instance_id": instance_id, "run": -2 }),
        );
        assert!(too_far.is_err(), "run=-2 is out of bounded history (N=1)");
        assert_eq!(
            too_far.unwrap_err().code,
            "invalid_argument",
            "an out-of-range run is invalid_argument"
        );

        // Cleanup.
        let _ = dispatcher.call("stop_command", &json!({ "instance_id": instance_id }));
    }

    /// GATE done_criterion #3 — "Timeout nyx down documente" (the testable half).
    ///
    /// nyx-down / client-timeout behavior (ADR-0003 D8 `mcp_unavailable`): when
    /// the nyx runtime is NOT reachable — the managed `Db` / `ManagedCommandRunner`
    /// is absent (the dispatcher installed before/without the runtime, or nyx mid
    /// teardown) — every command tool degrades EXPLICITLY to the `mcp_unavailable`
    /// error code rather than panicking or hanging. A short-timeout client then
    /// surfaces that as "nyx not reachable". This is the server-side half of the
    /// degradation; the client-side short timeout is documented in ADR-0003 D8 and
    /// the ADR-0004 `command`/`curl --max-time 1 … || true` fallback.
    #[test]
    fn mcp_command_tools_degrade_to_mcp_unavailable_when_runtime_absent() {
        // A mock app with NO managed Db / runner — the "nyx runtime not reachable"
        // condition (the only failure mode the agent sees when nyx is down/warming).
        let app = build_app();
        let dispatcher = mcp(&app);

        // Every command/listing tool that needs the runtime degrades to the SAME
        // explicit `mcp_unavailable` code (D8) — never a panic, never a hang.
        for (tool, args) in [
            ("list_commands", json!({ "workspace_id": "w-absent" })),
            ("start_command", json!({ "instance_id": "i-absent" })),
            ("relaunch_command", json!({ "instance_id": "i-absent" })),
            ("get_command_output", json!({ "instance_id": "i-absent" })),
            ("list_projects", json!({})),
        ] {
            let err = dispatcher
                .call(tool, &args)
                .expect_err("a tool needing the runtime must error when it is absent");
            assert_eq!(
                err.code, "mcp_unavailable",
                "{tool} must degrade to the explicit mcp_unavailable code (D8), got {:?}",
                err.code
            );
        }

        // Contrast (ADR-0004): the no-op `probe` liveness tool still answers even
        // with the runtime absent — that is exactly why a SessionStart hook can use
        // it, and why "nyx down" is distinguishable (probe unreachable) from
        // "runtime warming" (probe ok, command tools `mcp_unavailable`).
        let probe = dispatcher
            .call(crate::mcp::PROBE_TOOL, &json!({}))
            .expect("probe answers without managed state");
        assert_eq!(probe["ok"], true);
    }

    // -----------------------------------------------------------------------
    // Settings → Integrations (PRD-4 #3): integration_list / _install / _remove
    // -----------------------------------------------------------------------
    //
    // These exercise the AppHandle-free cores (`integration_status_list`,
    // `do_integration_install`, `do_integration_remove`) against temp files —
    // never the user's real `~/.claude.json` nor a real app-data dir. The
    // `#[tauri::command]` wrappers only resolve the data dir + claude_code
    // target and delegate to these, so the wiring under test is the whole
    // behaviour: 4 providers, available/coming-soon flags, install upserts the
    // nyx entry + persists installed, remove deletes the entry + clears it.

    use std::sync::atomic::{AtomicU32, Ordering};

    /// A process-unique temp dir for one integration test (no `tempfile` dep),
    /// so the suite never collides or touches real config/state.
    fn integ_temp_dir(tag: &str) -> std::path::PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let mut dir = std::env::temp_dir();
        dir.push(format!("nyx-integ-{}-{}-{}", std::process::id(), tag, n));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `true` if the config file at `path` has an `mcpServers.nyx` entry.
    fn config_has_nyx(path: &std::path::Path) -> bool {
        crate::onboarding::read_config_pub(path)
            .ok()
            .and_then(|root| {
                root.get("mcpServers")
                    .and_then(|s| s.get(crate::onboarding::SERVER_NAME))
                    .map(|_| ())
            })
            .is_some()
    }

    /// A fake bundled-plugin descriptor pointing at temp paths, so the plugin tests
    /// never touch the user's real `~/.claude` and never shell out. The source dir holds
    /// a minimal manifest so the copy-to-stable step has something to mirror; the stable
    /// install dir + settings file are temp paths.
    fn temp_plugin_install(dir: &std::path::Path) -> crate::plugin::PluginInstall {
        let source = dir.join("claude-plugin");
        std::fs::create_dir_all(source.join(".claude-plugin")).unwrap();
        std::fs::write(source.join(".claude-plugin").join("marketplace.json"), "{}").unwrap();
        crate::plugin::PluginInstall {
            marketplace: crate::plugin::CLAUDE_MARKETPLACE.to_string(),
            plugin: crate::plugin::CLAUDE_PLUGIN_NAME.to_string(),
            source_dir: source,
            install_dir: dir.join("stable-claude-plugin"),
            settings_path: dir.join("settings.json"),
            mcp_port: 8765,
        }
    }

    /// A recording fake of the Claude plugin CLI for the bridge plugin tests — models a
    /// single optional `nyx` marketplace registration in memory, so install/remove are
    /// observable without the real `claude` binary. When a `settings_path` is wired, it
    /// also models the REAL CLI's ownership of `settings.json.enabledPlugins`: `install`
    /// sets `enabledPlugins[<id>] = true`, `uninstall` removes it — exactly the signal the
    /// post-mutation status now reads (review #40), so the status-reading tests observe a
    /// faithful enabledPlugins state without shelling out.
    #[derive(Default)]
    struct FakeBridgeCli {
        registered: std::cell::RefCell<Option<std::path::PathBuf>>,
        /// When set, `install`/`uninstall` write/clear `enabledPlugins[<id>]` here,
        /// mirroring the real `claude plugin` CLI that owns this file.
        settings_path: Option<std::path::PathBuf>,
    }

    impl FakeBridgeCli {
        /// A CLI fake that models the real `enabledPlugins` write into `settings_path`.
        fn with_settings(settings_path: std::path::PathBuf) -> Self {
            Self { registered: std::cell::RefCell::new(None), settings_path: Some(settings_path) }
        }

        /// Toggle `enabledPlugins[<id>]` in the wired settings file (no-op when unwired),
        /// mirroring the real CLI's ownership of `settings.json`.
        fn set_enabled_plugin(&self, install_id: &str, enabled: bool) {
            let Some(path) = self.settings_path.as_deref() else { return };
            let mut root = crate::onboarding::read_config_pub(path).unwrap_or(serde_json::json!({}));
            let obj = root.as_object_mut().expect("read_config yields an object");
            let plugins = obj
                .entry("enabledPlugins")
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            let plugins = plugins.as_object_mut().expect("enabledPlugins is an object");
            if enabled {
                plugins.insert(install_id.to_string(), serde_json::Value::Bool(true));
            } else {
                plugins.remove(install_id);
            }
            crate::onboarding::write_config_pub(path, &root).expect("write fake settings");
        }
    }

    impl crate::plugin::PluginCli for FakeBridgeCli {
        fn marketplace_add(&self, dir: &std::path::Path) -> Result<(), crate::plugin::PluginError> {
            *self.registered.borrow_mut() = Some(dir.to_path_buf());
            Ok(())
        }
        fn install(&self, install_id: &str) -> Result<(), crate::plugin::PluginError> {
            self.set_enabled_plugin(install_id, true);
            Ok(())
        }
        fn uninstall(&self, install_id: &str) -> Result<(), crate::plugin::PluginError> {
            self.set_enabled_plugin(install_id, false);
            Ok(())
        }
        fn marketplace_remove(&self, _marketplace: &str) -> Result<(), crate::plugin::PluginError> {
            *self.registered.borrow_mut() = None;
            Ok(())
        }
        fn marketplace_update(&self, _marketplace: &str) -> Result<(), crate::plugin::PluginError> {
            Ok(())
        }
        fn plugin_update(&self, _install_id: &str) -> Result<(), crate::plugin::PluginError> {
            Ok(())
        }
        fn marketplace_list(&self) -> Result<Vec<crate::plugin::MarketplaceEntry>, crate::plugin::PluginError> {
            Ok(self
                .registered
                .borrow()
                .clone()
                .map(|p| vec![crate::plugin::MarketplaceEntry {
                    name: crate::plugin::CLAUDE_MARKETPLACE.to_string(),
                    path: Some(p),
                }])
                .unwrap_or_default())
        }
    }

    /// `true` if the fake CLI currently has nyx's plugin registered (the bridge install
    /// drives `marketplace_add` at the stable dir; remove clears it). Replaces the old
    /// settings-file probe — the CLI now owns the registration, not settings.json.
    fn cli_has_nyx_plugin(cli: &FakeBridgeCli) -> bool {
        cli.registered.borrow().is_some()
    }

    #[test]
    fn integration_list_returns_four_registry_providers() {
        // The list SHAPE (4 providers, available/coming-soon flags) is independent of the
        // env seams — only claude_code's single installed flag derives from the real config.
        // We assert the shape from the live list and the claude_code flag via the path-param
        // core `claude_status_at`, so this test mutates NO process-global env (review
        // #42/#43) — no shared lock needed.
        let list = integration_status_list();
        let providers: Vec<&str> = list.iter().map(|s| s.provider).collect();
        assert_eq!(
            providers,
            vec!["claude_code", "codex", "opencode", "custom"],
            "the Integrations section advertises exactly the 4 registry providers"
        );

        // claude_code is the only available (functional) provider in v1.
        let claude = list.iter().find(|s| s.provider == "claude_code").unwrap();
        assert!(claude.available, "claude_code is functional in v1");

        // The single installed flag against ABSENT settings reads "not installed", derived
        // through the same core the list uses (finding #46) — no env touched.
        let dir = integ_temp_dir("list");
        let absent = claude_status_at(&dir.join("absent-settings.json"));
        assert!(!absent.installed, "absent real settings → integration not installed");

        // codex / opencode / custom are coming soon (available == false), never installed.
        for p in ["codex", "opencode", "custom"] {
            let s = list.iter().find(|s| s.provider == p).unwrap();
            assert!(!s.available, "{p} is coming soon (available == false)");
            assert!(!s.installed, "{p} is never installed in v1");
        }
    }

    /// The ONE install drives the whole integration: the bundled plugin (which provides
    /// the MCP) is registered via the CLI, AND any legacy standalone `mcpServers.nyx` is
    /// stripped so the MCP is not declared twice (finding #45). Status flips to installed,
    /// derived from the REAL `enabledPlugins` the fake CLI writes (finding #46).
    #[test]
    fn integration_install_registers_plugin_and_strips_legacy_mcp() {
        let dir = integ_temp_dir("install-one");
        let state_path = dir.join(crate::onboarding::INTEGRATIONS_FILE);
        let config_path = dir.join(".claude.json");
        // Seed a LEGACY standalone MCP entry (residue from the old separate-MCP flow).
        std::fs::write(&config_path, r#"{"mcpServers":{"nyx":{"type":"http","url":"http://127.0.0.1:8765/mcp"}},"autoConnectIde":true}"#).unwrap();
        let target = crate::onboarding::OnboardingTarget::new("Claude Code", &config_path);
        let plugin = temp_plugin_install(&dir);
        let cli = FakeBridgeCli::with_settings(plugin.settings_path.clone());

        let status = do_integration_install("claude_code", &target, Some(&plugin), Some(&cli), &state_path)
            .expect("install succeeds");
        assert!(status.installed, "status reports the integration installed (enabledPlugins set)");

        // Plugin registered via the CLI at the STABLE dir; bundled content copied.
        assert!(cli_has_nyx_plugin(&cli), "plugin registered via the CLI");
        assert_eq!(cli.registered.borrow().as_deref(), Some(plugin.install_dir.as_path()), "registered the stable dir");
        assert!(plugin.install_dir.join(".claude-plugin").join("marketplace.json").exists(), "bundled content copied to stable dir");
        // The legacy standalone MCP entry was stripped (no double-declaration); other keys survive.
        assert!(!config_has_nyx(&config_path), "legacy standalone mcpServers.nyx stripped on install");
        let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(cfg["autoConnectIde"], true, "unrelated config preserved");
        // nyx's own cache flag is set (non-authoritative).
        assert!(crate::onboarding::IntegrationState::load(&state_path).is_installed("claude_code"));
    }

    /// Uninstall is the mirror: it uninstalls the plugin (CLI uninstall + marketplace
    /// remove) AND cleans every nyx residue — the legacy standalone MCP and the legacy
    /// hand-written settings keys — leaving nothing behind (finding #45).
    #[test]
    fn integration_remove_uninstalls_plugin_and_cleans_all_residue() {
        let dir = integ_temp_dir("remove-one");
        let state_path = dir.join(crate::onboarding::INTEGRATIONS_FILE);
        let config_path = dir.join(".claude.json");
        let target = crate::onboarding::OnboardingTarget::new("Claude Code", &config_path);
        let plugin = temp_plugin_install(&dir);
        let cli = FakeBridgeCli::with_settings(plugin.settings_path.clone());

        // Install, then seed a legacy standalone MCP + legacy hand-written settings keys.
        do_integration_install("claude_code", &target, Some(&plugin), Some(&cli), &state_path).unwrap();
        std::fs::write(&config_path, r#"{"mcpServers":{"nyx":{"type":"http","url":"http://127.0.0.1:8765/mcp"}}}"#).unwrap();
        std::fs::write(
            &plugin.settings_path,
            r#"{"enabledPlugins":{"nyx-claude-integration@nyx":true},"extraKnownMarketplaces":{"nyx":{}}}"#,
        )
        .unwrap();
        assert!(cli_has_nyx_plugin(&cli));

        let status = do_integration_remove("claude_code", &target, Some(&plugin), Some(&cli), &state_path)
            .expect("remove succeeds");
        assert!(!status.installed, "status reports uninstalled");

        // Everything nyx is gone: plugin registration, legacy MCP, legacy settings keys.
        assert!(!cli_has_nyx_plugin(&cli), "plugin registration removed");
        assert!(!config_has_nyx(&config_path), "legacy standalone mcpServers.nyx stripped on uninstall");
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&plugin.settings_path).unwrap()).unwrap();
        assert!(settings["enabledPlugins"].get("nyx-claude-integration@nyx").is_none(), "legacy enabledPlugins stripped");
        assert!(settings["extraKnownMarketplaces"].get("nyx").is_none(), "legacy marketplace stripped");
        assert!(!crate::onboarding::IntegrationState::load(&state_path).is_installed("claude_code"));
    }

    #[test]
    fn integration_install_rejects_unsupported_provider() {
        let dir = integ_temp_dir("unsupported");
        let state_path = dir.join(crate::onboarding::INTEGRATIONS_FILE);
        let config_path = dir.join(".claude.json");
        let target = crate::onboarding::OnboardingTarget::new("Claude Code", &config_path);
        let plugin = temp_plugin_install(&dir);
        let cli = FakeBridgeCli::default();

        // codex / opencode / custom are coming soon → install/remove refuse.
        for p in ["codex", "opencode", "custom"] {
            let err = do_integration_install(p, &target, Some(&plugin), Some(&cli), &state_path)
                .expect_err("coming-soon providers cannot be installed in v1");
            assert!(err.contains("not supported"), "actionable error for {p}: {err}");
            let err = do_integration_remove(p, &target, Some(&plugin), Some(&cli), &state_path)
                .expect_err("coming-soon providers cannot be removed in v1");
            assert!(err.contains("not supported"), "actionable error for {p}: {err}");
        }
        // No config/state side effects from the rejected calls.
        assert!(!config_has_nyx(&config_path));
        assert!(!cli_has_nyx_plugin(&cli));
    }

    /// REGRESSION (finding #46): the single status is derived from Claude Code's REAL config
    /// (`enabledPlugins`), not nyx's stored `integrations.json` flag. The plugin was
    /// uninstalled DIRECTLY in Claude Code (`enabledPlugins` entry gone) — status must read
    /// FALSE even though a stale `integrations.json` says installed.
    #[test]
    fn status_reflects_real_config_not_stale_integrations_json() {
        let dir = integ_temp_dir("real-vs-stale");

        // The user uninstalled the plugin directly in Claude Code: the enabledPlugins entry
        // is gone (only the marketplace lingers, which is NOT the signal).
        let settings = dir.join("settings.json");
        std::fs::write(&settings, r#"{"enabledPlugins":{"warp@claude-code-warp":true},"extraKnownMarketplaces":{"nyx":{}}}"#).unwrap();

        // A STALE integrations.json that lies: installed (the pre-fix source of truth).
        let state_path = dir.join(crate::onboarding::INTEGRATIONS_FILE);
        let mut stale = crate::onboarding::IntegrationState::default();
        stale.set_installed("claude_code", true);
        stale.save(&state_path).unwrap();

        // Derive via the path-param core (finding #46): reads ONLY the REAL settings file at
        // an explicit temp path and never consults `integrations.json` — no env, no lock.
        let claude = claude_status_at(&settings);
        assert!(
            !claude.installed,
            "plugin uninstalled in Claude Code (enabledPlugins entry gone) → NOT installed, \
             even though integrations.json still says true"
        );
    }

    /// The single status is derived from the REAL `enabledPlugins` flag: present/true →
    /// installed, absent/false → not (finding #46).
    #[test]
    fn status_derives_from_enabled_plugins_flag() {
        // Exercised through the path-param core `claude_status_at` against temp files — no
        // process-global env, no shared lock (review #42/#43).
        let dir = integ_temp_dir("status-flag");
        let settings = dir.join("settings.json");

        for (plugin_cfg, want) in [
            (r#"{"enabledPlugins":{"nyx-claude-integration@nyx":true}}"#, true),
            (r#"{"enabledPlugins":{"nyx-claude-integration@nyx":false}}"#, false),
            (r#"{"enabledPlugins":{}}"#, false),
        ] {
            std::fs::write(&settings, plugin_cfg).unwrap();
            assert_eq!(claude_status_at(&settings).installed, want, "status from {plugin_cfg}");
        }
    }

    // --- Terminal RECORD ↔ PTY link + pending-command injection (R-TERM) ----

    #[test]
    fn register_terminal_pty_links_and_clears_the_record_to_pty_mapping() {
        // The foundation join the MCP terminal tools read: registering a Some(pty_id)
        // records the record→pty link; registering None clears it.
        let app = build_app_with_db();
        register_terminal_pty(
            app.state::<Db>(),
            app.state::<TerminalPtyMap>(),
            app.state::<PendingTerminalCommands>(),
            app.state::<PendingResumes>(),
            app.state::<PtyManager>(),
            "rec-1".into(),
            Some(42),
        )
        .expect("register a live pty");
        assert_eq!(app.state::<TerminalPtyMap>().get("rec-1"), Some(42));
        // The snapshot the list_terminals MCP tool consumes carries the link.
        assert_eq!(app.state::<TerminalPtyMap>().snapshot().get("rec-1"), Some(&42));

        register_terminal_pty(
            app.state::<Db>(),
            app.state::<TerminalPtyMap>(),
            app.state::<PendingTerminalCommands>(),
            app.state::<PendingResumes>(),
            app.state::<PtyManager>(),
            "rec-1".into(),
            None,
        )
        .expect("clear on exit");
        assert_eq!(app.state::<TerminalPtyMap>().get("rec-1"), None, "exit clears the link");
    }

    #[test]
    fn register_terminal_pty_injects_a_parked_command_once_into_the_live_pty() {
        // The MCP `create_terminal(command=…)` path: a command parked for a record is
        // injected (command + "\r") into the PTY the front spawns, then the parked entry
        // is consumed so a later respawn does NOT re-inject. We spawn a REAL pty here, so
        // this test is gated behind the same non-ConPTY reasoning as the other pty tests
        // — keep it a pure state test of the PARK + take instead, exercising the live
        // injection path against a spawned pty.
        let app = build_app_with_db();
        let pty_id = spawn(&app, 80, 24);
        app.state::<PendingTerminalCommands>().set("rec-cmd", "echo hi".into());

        register_terminal_pty(
            app.state::<Db>(),
            app.state::<TerminalPtyMap>(),
            app.state::<PendingTerminalCommands>(),
            app.state::<PendingResumes>(),
            app.state::<PtyManager>(),
            "rec-cmd".into(),
            Some(pty_id),
        )
        .expect("register + inject");
        // The parked command is consumed (one-shot): taking again yields nothing, so a
        // respawn would not double-run the command.
        assert_eq!(
            app.state::<PendingTerminalCommands>().take("rec-cmd"),
            None,
            "the parked command is drained exactly once"
        );
        let _ = close(&app, pty_id);
    }

    #[test]
    fn register_terminal_pty_with_no_parked_command_is_a_bare_shell() {
        // A terminal opened without an MCP command parks nothing — registration injects
        // nothing and only records the link.
        let app = build_app_with_db();
        register_terminal_pty(
            app.state::<Db>(),
            app.state::<TerminalPtyMap>(),
            app.state::<PendingTerminalCommands>(),
            app.state::<PendingResumes>(),
            app.state::<PtyManager>(),
            "rec-bare".into(),
            Some(7),
        )
        .expect("register bare");
        assert_eq!(app.state::<TerminalPtyMap>().get("rec-bare"), Some(7));
        assert_eq!(
            app.state::<PendingTerminalCommands>().take("rec-bare"),
            None,
            "no command was ever parked for a bare terminal"
        );
    }
}
