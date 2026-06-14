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

/// Managed state: all live PTYs keyed by their id.
#[derive(Default)]
pub struct PtyManager {
    ptys: Mutex<HashMap<u64, Pty>>,
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

/// Spawn the default shell in a new PTY and start streaming its output.
///
/// Returns the new PTY id. The caller (front) subscribes to `pty://output`
/// filtered by this id. Output is coalesced on a dedicated thread.
#[tauri::command]
fn pty_spawn<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, PtyManager>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
) -> Result<u64, String> {
    let size = portable_pty::PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    let (pty, rx) = Pty::spawn(size, cwd.as_deref()).map_err(|e| e.to_string())?;
    let id = pty.id();

    state.ptys.lock().unwrap().insert(id, pty);

    // Coalescing pump: own the receiver, batch chunks, flush every FLUSH_INTERVAL.
    spawn_output_pump(app, id, rx);

    Ok(id)
}

/// Write bytes (e.g. keystrokes) to the PTY identified by `id`.
#[tauri::command]
fn pty_write(state: State<'_, PtyManager>, id: u64, data: Vec<u8>) -> Result<(), String> {
    let mut ptys = state.ptys.lock().unwrap();
    let pty = ptys
        .get_mut(&id)
        .ok_or_else(|| format!("unknown pty id {id}"))?;
    pty.write(&data).map_err(|e| e.to_string())
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
#[tauri::command]
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
#[tauri::command]
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
/// abstracted so the non-Linux build compiles (where it is always `None`).
#[cfg(target_os = "linux")]
fn foreground_pgid(pty: &Pty) -> Option<i32> {
    pty.foreground_pgid()
}
#[cfg(not(target_os = "linux"))]
fn foreground_pgid(_pty: &Pty) -> Option<i32> {
    None
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
fn spawn_output_pump<R: Runtime>(app: AppHandle<R>, id: u64, rx: Receiver<Vec<u8>>) {
    std::thread::Builder::new()
        .name(format!("nyx-pty-pump-{id}"))
        .spawn(move || {
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
                // Wait at most until the next scheduled flush so a steady flood
                // still flushes on cadence rather than only when idle.
                let since = last_flush.elapsed();
                let wait = FLUSH_INTERVAL.saturating_sub(since);
                match rx.recv_timeout(wait) {
                    Ok(chunk) => {
                        // Portable cwd source: scan the raw stream for OSC 7 and
                        // record the most recent decoded cwd for this PTY. Cheap
                        // (a substring scan); the auto-attach resolver reads it
                        // via `terminal_info`/`auto_attach_terminal`.
                        if let Some(cwd) = crate::osc7::extract_last_cwd(&chunk) {
                            app.state::<Osc7Cache>().set(id, cwd);
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

// --- Terminal RECORD commands (SQLite via Diesel) ------------------------
//
// These persist the terminal records (id-space distinct from the live PTY ids):
// create/list/close/reorder/rename and the bounded scrollback snapshot. Thin
// wrappers over the unit-tested `crate::db` CRUD functions; the heavy logic and
// its tests live there. Errors are stringified for the IPC boundary.

/// Create a terminal record at `cwd` (optional `label`) and return the new row.
#[tauri::command]
fn create_terminal(
    db: State<'_, Db>,
    cwd: String,
    label: Option<String>,
) -> Result<Terminal, String> {
    db.with_conn(|c| db::create_terminal(c, &cwd, label))
        .map_err(|e| e.to_string())
}

/// List all terminal records in sidebar order (closed ones included).
#[tauri::command]
fn list_terminals(db: State<'_, Db>) -> Result<Vec<Terminal>, String> {
    db.with_conn(db::list_terminals).map_err(|e| e.to_string())
}

/// Mark a terminal record `closed` (no re-spawn at launch).
#[tauri::command]
fn close_terminal(db: State<'_, Db>, id: String) -> Result<(), String> {
    db.with_conn(|c| db::close_terminal(c, &id))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Persist the sidebar order: each id's `order` becomes its index in `ids`.
#[tauri::command]
fn reorder(db: State<'_, Db>, ids: Vec<String>) -> Result<(), String> {
    db.with_conn(|c| db::reorder(c, &ids))
        .map_err(|e| e.to_string())
}

/// Rename a terminal record (`label`; `None` clears it).
#[tauri::command]
fn rename(db: State<'_, Db>, id: String, label: Option<String>) -> Result<(), String> {
    db.with_conn(|c| db::rename(c, &id, label))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Record `id` as the active terminal (stamps `last_active_at`) so a relaunch
/// reopens on it.
#[tauri::command]
fn set_active(db: State<'_, Db>, id: String) -> Result<(), String> {
    db.with_conn(|c| db::set_active(c, &id))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Persist a terminal's serialized scrollback (bounded). The caller debounces.
#[tauri::command]
fn persist_scrollback(db: State<'_, Db>, id: String, serialized: String) -> Result<(), String> {
    db.with_conn(|c| db::persist_scrollback(c, &id, &serialized))
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
#[tauri::command]
fn create_project(
    db: State<'_, Db>,
    name: String,
    root_path: String,
    root_name: Option<String>,
) -> Result<ProjectWithRoot, String> {
    db.with_conn(|c| db::create_project(c, &name, &root_path, root_name.as_deref()))
        .map(|(project, root)| ProjectWithRoot { project, root })
        .map_err(|e| e.to_string())
}

/// List all projects.
#[tauri::command]
fn list_projects(db: State<'_, Db>) -> Result<Vec<Project>, String> {
    db.with_conn(db::list_projects).map_err(|e| e.to_string())
}

/// Rename a project's display `name`. Returns ().
#[tauri::command]
fn update_project(db: State<'_, Db>, id: String, name: String) -> Result<(), String> {
    db.with_conn(|c| db::update_project(c, &id, &name))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Delete a project and its workspaces (ON DELETE CASCADE). Terminals bound to
/// those workspaces are DETACHED (workspace_id → NULL via ON DELETE SET NULL),
/// not killed — they survive as loose terminals. Returns ().
#[tauri::command]
fn delete_project(db: State<'_, Db>, id: String) -> Result<(), String> {
    db.with_conn(|c| db::delete_project(c, &id))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Persist a project's sidebar `collapsed` (open/closed) state so the band's
/// disclosure survives a restart. Returns ().
#[tauri::command]
fn set_project_collapsed(db: State<'_, Db>, id: String, collapsed: bool) -> Result<(), String> {
    db.with_conn(|c| db::set_project_collapsed(c, &id, collapsed))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Create a (non-root) workspace in `project_id` at `path`. Rejects a path
/// already present in the SAME project (UNIQUE(project_id, path)).
#[tauri::command]
fn create_workspace(
    db: State<'_, Db>,
    project_id: String,
    name: String,
    path: String,
) -> Result<Workspace, String> {
    db.with_conn(|c| db::create_workspace(c, &project_id, &name, &path))
        .map_err(|e| e.to_string())
}

/// List the workspaces of `project_id` (root first).
#[tauri::command]
fn list_workspaces(db: State<'_, Db>, project_id: String) -> Result<Vec<Workspace>, String> {
    db.with_conn(|c| db::list_workspaces(c, &project_id))
        .map_err(|e| e.to_string())
}

/// Rename a workspace's display `name` (the path is immutable). Returns ().
#[tauri::command]
fn rename_workspace(db: State<'_, Db>, id: String, name: String) -> Result<(), String> {
    db.with_conn(|c| db::rename_workspace(c, &id, &name))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Persist a workspace's sidebar `collapsed` (open/closed) state so the band's
/// disclosure survives a restart. Returns ().
#[tauri::command]
fn set_workspace_collapsed(db: State<'_, Db>, id: String, collapsed: bool) -> Result<(), String> {
    db.with_conn(|c| db::set_workspace_collapsed(c, &id, collapsed))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Attach a terminal record to a workspace with an explicit binding `mode`
/// (`auto`|`manual`).
#[tauri::command]
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
#[tauri::command]
fn detach_terminal(db: State<'_, Db>, terminal_id: String) -> Result<(), String> {
    db.with_conn(|c| db::detach_terminal(c, &terminal_id))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Pin a terminal record to a workspace (mode `manual`; a later `cd` no longer
/// moves it).
#[tauri::command]
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
#[tauri::command]
fn unpin_terminal_workspace(db: State<'_, Db>, terminal_id: String) -> Result<(), String> {
    db.with_conn(|c| db::unpin_terminal_workspace(c, &terminal_id))
        .map(|_| ())
        .map_err(|e| e.to_string())
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
#[tauri::command]
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

/// Register the PTY managed state and command handlers on the builder.
pub fn init<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .manage(PtyManager::default())
        .manage(TerminalInfoCache::default())
        .manage(Osc7Cache::default())
        .invoke_handler(tauri::generate_handler![
            pty_spawn,
            pty_write,
            pty_resize,
            pty_close,
            terminal_info,
            create_terminal,
            list_terminals,
            close_terminal,
            reorder,
            rename,
            set_active,
            persist_scrollback,
            window_controls_visible,
            create_project,
            list_projects,
            update_project,
            delete_project,
            set_project_collapsed,
            create_workspace,
            list_workspaces,
            rename_workspace,
            set_workspace_collapsed,
            attach_terminal,
            detach_terminal,
            pin_terminal_workspace,
            unpin_terminal_workspace,
            auto_attach_terminal
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
    fn spawn(app: &App<MockRuntime>, cols: u16, rows: u16) -> u64 {
        pty_spawn(
            app.handle().clone(),
            app.state::<PtyManager>(),
            None,
            cols,
            rows,
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
        assert_eq!(live_pty_count(&app), 2, "survivors remain after the re-close");
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
        assert_eq!(live_pty_count(&app), 0, "no PTY may remain after closing all");
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
        assert_eq!(ordered, vec![b.id.clone(), a.id.clone()], "reorder reflected by list");

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
                Some(t.cwd.clone()),
                80,
                24,
            )
            .expect("re-spawn pty for an alive record");
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
        let app = build_app_with_db();

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

        delete_project(app.state::<Db>(), project_id.clone()).expect("delete_project command");
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
            projects.iter().find(|pr| pr.id == project_id).unwrap().collapsed,
            "collapse routes + persists via the project command"
        );

        // Collapse the feat workspace via the command; the root stays open.
        set_workspace_collapsed(app.state::<Db>(), feat.id.clone(), true)
            .expect("set_workspace_collapsed command");
        let workspaces = list_workspaces(app.state::<Db>(), project_id.clone()).expect("list_ws");
        assert!(
            workspaces.iter().find(|w| w.id == feat.id).unwrap().collapsed,
            "collapse routes + persists via the workspace command"
        );
        assert!(
            !workspaces.iter().find(|w| w.id == root_id).unwrap().collapsed,
            "a sibling workspace is left open"
        );

        // Re-open the project (the toggle round-trips both ways).
        set_project_collapsed(app.state::<Db>(), project_id.clone(), false)
            .expect("re-open project");
        let projects = list_projects(app.state::<Db>()).expect("list_projects again");
        assert!(
            !projects.iter().find(|pr| pr.id == project_id).unwrap().collapsed,
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
}
