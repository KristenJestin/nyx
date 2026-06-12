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
            // `pty` drops here: kills (idempotent) + joins helper threads.
            Ok(())
        }
        None => Err(format!("unknown pty id {id}")),
    }
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

/// Register the PTY managed state and command handlers on the builder.
pub fn init<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .manage(PtyManager::default())
        .invoke_handler(tauri::generate_handler![
            pty_spawn, pty_write, pty_resize, pty_close
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
}
