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

use crate::db::{self, Db, Terminal};
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

    let info = read_terminal_info(shell_pid, fg_pgid);

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
            window_controls_visible
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
