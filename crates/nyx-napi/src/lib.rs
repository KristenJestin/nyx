//! nyx-napi — napi-rs binding that exposes `nyx-core` to Node (the Electron
//! core-host).
//!
//! Phase 3 — the FULL interactive-PTY surface. The skeleton (phase 1) proved the
//! napi chain + ABI with `version()` + a minimal streaming `NyxPty`; this exposes
//! the whole terminal core of `nyx-core` to Node at parity with the Tauri bridge:
//!
//! - **lifecycle**: `new(cols, rows, cwd?, terminalId?, callbacks)` spawns the
//!   default shell in a PTY; `write`, `resize`, `kill`, `id` drive it.
//! - **streaming**: a Rust pump thread coalesces output (leading-edge, ~60fps
//!   cadence) and delivers it to Node through a `ThreadsafeFunction` (the
//!   [`nyx_core::frontier::EventSink`] frontier on the Node loop). Exit is a
//!   second callback.
//! - **OSC observation** (parity with the Tauri pump): the SAME raw bytes are
//!   scanned for OSC 7 (cwd) and OSC 133 (exec-state) and surfaced as their own
//!   callbacks — WITHOUT stripping anything from the output stream (xterm renders
//!   the full bytes; these are purely observed). The DB/persistence half of
//!   exec-state stays phase 5; here the core only EMITS the decoded transitions.
//! - **lossless flow control** (PRD frozen decision / annexe §E): `setPaused`
//!   pauses the core reader thread at its gate, applying real OS backpressure so a
//!   flood is never dropped or reordered. The bridge (the host) drives pause/resume
//!   from its xterm-acknowledged backlog; the per-terminal byte thresholds live in
//!   the host (512 KiB high / 128 KiB low), the mechanism here.
//!
//! ABI: the `.node` is built against the Node ABI EMBEDDED BY THE PINNED ELECTRON
//! (`process.versions.modules` of that Electron), not the system Node, and is loaded
//! for validation via the Electron binary in `ELECTRON_RUN_AS_NODE=1`. napi-rs v2
//! is pinned (a known, stable `ThreadsafeFunction` API) — see `Cargo.toml`.
//!
//! Per the POC's mandatory architecture decision, this addon is hosted in a
//! DEDICATED Node process (`ELECTRON_RUN_AS_NODE`), NEVER `fork`ed from the Electron
//! main/renderer (a PTY fork from the Chromium main process SIGSEGVs). That hosting
//! is the Electron core-host's job; here we expose the core.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ErrorStrategy, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

use nyx_core::osc133;
use nyx_core::osc7;
use nyx_core::pty::{PauseGate, Pty};
use portable_pty::PtySize;

mod command;
mod core_db;
mod mcp;

pub use command::NyxCommandRunner;
pub use core_db::NyxCore;

/// Output coalescing cadence: ~60fps. Mirrors the Tauri bridge's `FLUSH_INTERVAL`
/// so a flood (`yes`) never fans out one IPC event per chunk/line.
const FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// The nyx-core version string this native addon was built against. The Electron
/// core-host calls this right after loading the `.node` as a liveness + ABI-match
/// probe (if the addon loaded under the wrong ABI it would fail to load at all, so
/// a successful `version()` call proves the ABI matches the host Electron).
#[napi]
pub fn version() -> String {
    // `CARGO_PKG_VERSION` of nyx-core, resolved at compile time. A single source of
    // truth the host can log to confirm which core a packaged build embeds.
    env!("CARGO_PKG_VERSION").to_string()
}

/// A decoded exec-state transition surfaced to Node (PRD-2.1, OSC 133). At parity
/// with the Tauri bridge's `drive_exec_state`: only a command END produces a
/// transition — a `D;0` is `success`, any other code (or a missing one) is `error`.
/// `running` is NOT driven from OSC 133 (the OS busy signal owns it); the
/// pre-exec/prompt markers are inert. The host maps this onto `terminal://exec-state`
/// and (phase 5) the DB record. `state` is `"success"` or `"error"`.
#[napi(object)]
pub struct ExecStateEvent {
    /// `"success"` (exit 0) or `"error"` (non-zero / unknown).
    pub state: String,
    /// The exit code when the `D` carried a parseable one; `None` otherwise (still
    /// `error`-colored — finished but unknown).
    pub exit_code: Option<i32>,
}

/// The live auto-label introspection of a terminal (the `terminal_info` command result),
/// at parity with the Tauri bridge's `TerminalInfo`. Both fields are `null` on a platform
/// without `/proc` (Windows) — a clean degradation, never an error.
#[napi(object)]
pub struct TerminalInfo {
    /// The shell's LIVE working directory (`readlink /proc/<shell_pid>/cwd`), or `null`
    /// when it cannot be read (process gone, no `/proc`).
    pub cwd: Option<String>,
    /// The foreground program name (`/proc/<foreground_pgid>/comm`) — `htop` while a
    /// program runs, the shell name at an idle prompt, `null` when undeterminable.
    pub foreground: Option<String>,
}

/// The full interactive PTY exposed to Node. One instance == one shell in a PTY.
/// Spawn it with output/exit/cwd/exec-state callbacks; drive it with
/// write/resize/kill/setPaused. The host keys these by terminal id (the live
/// `pty_id → record id` map stays on the JS side, exactly like the Tauri
/// `TerminalIdMap`), so this class carries no multi-terminal registry itself.
#[napi]
pub struct NyxPty {
    pty: Arc<Mutex<Pty>>,
    pause_gate: PauseGate,
}

#[napi]
impl NyxPty {
    /// Spawn the default shell in a `cols`x`rows` PTY and stream it to Node.
    ///
    /// - `cwd`: working dir for the shell (`None`/`undefined` → the host's cwd).
    /// - `terminal_id`: nyx's PERSISTENT terminal record id, exported into the shell
    ///   as `NYX_TERMINAL_ID` for agent-session correlation (PRD-5 task #3). `None`
    ///   for a record-less terminal.
    /// - `on_data`: each coalesced output chunk, as a `Buffer`, on the Node loop.
    /// - `on_exit`: fired once when the shell exits, with the exit code (or `null`).
    /// - `on_cwd`: the decoded OSC 7 cwd whenever the shell reports one (most-recent
    ///   per chunk). Lets auto-attach work portably (Windows/macOS have no `/proc`).
    /// - `on_exec_state`: a decoded OSC 133 command-END transition (success/error).
    ///
    /// The OSC callbacks are PURELY OBSERVATIONAL: the control bytes still flow to
    /// `on_data` (xterm renders the full stream); we never strip them.
    #[napi(constructor)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cols: u16,
        rows: u16,
        cwd: Option<String>,
        terminal_id: Option<String>,
        #[napi(ts_arg_type = "(err: null | Error, bytes: Buffer) => void")] on_data: JsFunction,
        #[napi(ts_arg_type = "(err: null | Error, code: number | null) => void")]
        on_exit: JsFunction,
        #[napi(ts_arg_type = "(err: null | Error, cwd: string) => void")] on_cwd: JsFunction,
        #[napi(ts_arg_type = "(err: null | Error, ev: ExecStateEvent) => void")]
        on_exec_state: JsFunction,
    ) -> Result<Self> {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let (pty, rx) = Pty::spawn(size, cwd.as_deref(), terminal_id.as_deref())
            .map_err(|e| Error::from_reason(format!("pty spawn failed: {e}")))?;
        let pause_gate = pty.pause_gate();

        // Bridge the four Rust→Node channels. `ErrorStrategy::Fatal` matches the
        // POC's validated streaming shape: each call carries the value as the only
        // (non-error) argument, delivered NonBlocking on the Node loop.
        let data_tsfn: ThreadsafeFunction<Vec<u8>, ErrorStrategy::Fatal> =
            on_data.create_threadsafe_function(0, |ctx| Ok(vec![Buffer::from(ctx.value)]))?;
        let exit_tsfn: ThreadsafeFunction<Option<i32>, ErrorStrategy::Fatal> =
            on_exit.create_threadsafe_function(0, |ctx| Ok(vec![ctx.value]))?;
        let cwd_tsfn: ThreadsafeFunction<String, ErrorStrategy::Fatal> =
            on_cwd.create_threadsafe_function(0, |ctx| Ok(vec![ctx.value]))?;
        let exec_tsfn: ThreadsafeFunction<ExecStateEvent, ErrorStrategy::Fatal> =
            on_exec_state.create_threadsafe_function(0, |ctx| Ok(vec![ctx.value]))?;

        let pty = Arc::new(Mutex::new(pty));
        let pump_pty = Arc::clone(&pty);

        // The output pump: coalesce + scan OSC + emit, on a dedicated thread. The
        // channel disconnects (loop ends) when the PTY's reader EOFs on child exit,
        // so the thread exits cleanly without a separate stop signal.
        std::thread::Builder::new()
            .name("nyx-napi-pty-pump".into())
            .spawn(move || {
                run_pump(rx, data_tsfn, exit_tsfn, cwd_tsfn, exec_tsfn, pump_pty);
            })
            .map_err(|e| Error::from_reason(format!("pump thread spawn failed: {e}")))?;

        Ok(NyxPty { pty, pause_gate })
    }

    /// Write bytes (keystrokes / paste) to the PTY. Flushes immediately so echo is
    /// not buffered.
    #[napi]
    pub fn write(&self, data: Buffer) -> Result<()> {
        self.pty
            .lock()
            .unwrap()
            .write(&data)
            .map_err(|e| Error::from_reason(format!("pty write failed: {e}")))
    }

    /// Resize the PTY window (delivers SIGWINCH to the child). Idempotent — a resize
    /// to the current size is a harmless no-op. Pixel dims are best-effort (0 here).
    #[napi]
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.pty
            .lock()
            .unwrap()
            .resize(cols, rows, 0, 0)
            .map_err(|e| Error::from_reason(format!("pty resize failed: {e}")))
    }

    /// Terminate the child shell. Idempotent: killing an already-dead child is a
    /// no-op. After this the reader EOFs, the pump flushes its tail and fires
    /// `on_exit`.
    #[napi]
    pub fn kill(&self) -> Result<()> {
        self.pty
            .lock()
            .unwrap()
            .kill()
            .map_err(|e| Error::from_reason(format!("pty kill failed: {e}")))
    }

    /// LOSSLESS FLOW CONTROL (PRD frozen decision / annexe §E). Pause or resume the
    /// reader thread. Paused → the core reader blocks at its gate, the kernel PTY
    /// buffer fills, and the child blocks on write (real OS backpressure — nothing
    /// is dropped or reordered). The host calls `setPaused(true)` when the renderer's
    /// xterm-acknowledged backlog crosses the high-water mark and `setPaused(false)`
    /// when it drains below the low-water mark.
    #[napi(js_name = "setPaused")]
    pub fn set_paused(&self, paused: bool) {
        self.pause_gate.set_paused(paused);
    }

    /// The live PTY id (so the host can correlate streams in the multi-terminal case).
    #[napi]
    pub fn id(&self) -> u32 {
        self.pty.lock().unwrap().id() as u32
    }

    /// The OS-derived BUSY bit (PRD-5 task #1 / decision 1-B): `Some(true)` when a
    /// command runs in this PTY's foreground (`foreground_pgid != shell pgid`),
    /// `Some(false)` at an idle prompt, `None` when the signal cannot be derived
    /// (non-Unix — ConPTY has no `tcgetpgrp` — or the master is already closed).
    ///
    /// This is the AUTHORITY for the running dot, read by the host's busy-state poll
    /// loop — the SAME `Pty::is_busy` derivation the Tauri bridge polls, exposed here
    /// so the Electron core-host can re-host the `start_busy_state_loop` over napi at
    /// parity. It is INDEPENDENT of OSC 133 (a force-quit/restore can never leave a
    /// phantom running: a restored terminal with no foreground command samples idle by
    /// construction). One cheap `tcgetpgrp` per call; the host polls on a bounded
    /// cadence and emits `terminal://busy-state` only on a TRANSITION.
    #[napi]
    pub fn busy(&self) -> Option<bool> {
        self.pty.lock().unwrap().is_busy()
    }

    /// This terminal's ROOT pid — the shell process id, the anchor of its process tree
    /// (FEEDBACK #28). The host's per-terminal stats poll passes this to
    /// `NyxProcStats.treeStats` to sum the shell + all descendants' CPU%/RAM. `None`
    /// when the shell pid is unknown (a spawn that did not yield a pid). Portable: every
    /// OS exposes the child's pid.
    #[napi]
    pub fn shell_pid(&self) -> Option<u32> {
        self.pty.lock().unwrap().shell_pid()
    }

    /// The LIVE auto-label introspection of this terminal (the `terminal_info` command's
    /// backend — PRD-5 auto-label / auto-attach revival): a fresh `{ cwd, foreground }`
    /// read straight from the kernel, NOT from OSC 7. Anchored on the live PTY:
    ///  - `cwd` is `readlink /proc/<shell_pid>/cwd` — the shell's real cwd, reflecting
    ///    every `cd` the user typed, instantly;
    ///  - `foreground` is `/proc/<foreground_pgid>/comm` — the program currently in the
    ///    PTY's foreground (`htop`/`vim`), or the shell name at an idle prompt.
    ///
    /// Both come from `nyx_core::proc`, keyed by the SAME `shell_pid` / `foreground_pgid`
    /// the Tauri `terminal_info` uses (parity). On a platform without `/proc` (Windows —
    /// no `/proc`, no `tcgetpgrp`) both fields are `null` WITHOUT erroring: a clean
    /// degradation (the front's auto-label simply yields no live label), never an error
    /// the per-second poll would spam. The host polls this on a bounded ~1s cadence
    /// (the front debounces), so it is two cheap syscalls, never per output byte.
    #[napi]
    pub fn terminal_info(&self) -> TerminalInfo {
        let pty = self.pty.lock().unwrap();
        read_terminal_info(pty.shell_pid(), foreground_pgid(&pty))
    }
}

/// The foreground process group id of a live PTY (`tcgetpgrp(master)`), abstracted so
/// the non-Unix build compiles (where there is no foreground process group, so it is
/// always `None`). Parity with the Tauri bridge's `foreground_pgid`.
#[cfg(unix)]
fn foreground_pgid(pty: &Pty) -> Option<i32> {
    pty.foreground_pgid()
}
#[cfg(not(unix))]
fn foreground_pgid(_pty: &Pty) -> Option<i32> {
    None
}

/// Read `{ cwd, foreground }` from `/proc`, keyed by the live PTY's shell pid +
/// foreground pgid. Linux-only; every other platform returns the empty reading
/// (`{ null, null }`) — a clean degradation, never an error. Mirrors the Tauri
/// `read_terminal_info`.
#[cfg(target_os = "linux")]
fn read_terminal_info(shell_pid: Option<u32>, fg_pgid: Option<i32>) -> TerminalInfo {
    TerminalInfo {
        cwd: shell_pid.and_then(nyx_core::proc::read_cwd),
        foreground: fg_pgid.and_then(nyx_core::proc::read_foreground_comm),
    }
}
#[cfg(not(target_os = "linux"))]
fn read_terminal_info(_shell_pid: Option<u32>, _fg_pgid: Option<i32>) -> TerminalInfo {
    TerminalInfo {
        cwd: None,
        foreground: None,
    }
}

/// One terminal's process-tree resource usage (FEEDBACK #28), at the napi frontier:
/// the summed CPU% + resident memory of the shell AND all its transitive descendants.
/// Mirrors `nyx_core::proc_stats::TreeStats`.
#[napi(object)]
pub struct TreeStats {
    /// Summed CPU usage of the tree, in PERCENT (relative to a single core, so a tree
    /// busy on N cores can exceed 100 — e.g. a parallel build). The host/UI may clamp.
    pub cpu_pct: f64,
    /// Summed resident memory of the tree, in BYTES (RSS). A JS `number` holds this
    /// exactly up to 2^53 bytes (8 PiB), far beyond any real terminal's footprint.
    pub mem_bytes: f64,
}

/// Cross-platform per-terminal CPU%/RAM introspector (FEEDBACK #28). Owns ONE live
/// `sysinfo::System` kept ALIVE across calls so per-process CPU% deltas are meaningful
/// (a fresh System per call would always read 0% — see `nyx_core::proc_stats`). The
/// host builds exactly ONE of these at boot and calls [`tree_stats_batch`](Self::tree_stats_batch)
/// ONCE per poll tick with EVERY live terminal's shell pid.
///
/// ## Off the Node main thread (FEEDBACK #28 perf)
///
/// The full `/proc` scan is EXPENSIVE; with N terminals the old `tree_stats`-per-terminal
/// loop ran N full scans SYNCHRONOUSLY on the Node main thread, blocking the event loop for
/// hundreds of ms and freezing keystroke IPC. So the live `ProcStats` (the one `System`) now
/// lives behind an `Arc<Mutex<…>>`, and [`tree_stats_batch`](Self::tree_stats_batch) returns
/// an [`AsyncTask`]: the scan runs in `compute()` on a libuv WORKER thread (mirroring the DB
/// tasks in `core_db.rs`), never the main loop. The single `System` is preserved for CPU%
/// deltas — it is just shared through the mutex now (only the poll touches it, so there is
/// no contention).
///
/// Cross-platform via `sysinfo` (Linux/macOS/Windows) — the descendant set is found by
/// walking PARENT pids, the one portable signal, never `/proc` session ids.
#[napi]
pub struct NyxProcStats {
    /// The live `ProcStats` (the single reused `System`), behind a mutex so the libuv
    /// worker thread running [`TreeStatsBatchTask`] can lock it for the scan. Cloning this
    /// `Arc` into the task hands the worker the SAME `System` (the CPU%-delta authority).
    inner: Arc<Mutex<nyx_core::proc_stats::ProcStats>>,
}

#[napi]
impl NyxProcStats {
    /// Build the introspector. CPU% is meaningful from the SECOND `treeStats*` call for a
    /// given pid on (the first has no prior sample to diff — it reads ~0%).
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        NyxProcStats {
            inner: Arc::new(Mutex::new(nyx_core::proc_stats::ProcStats::new())),
        }
    }

    /// Refresh the process table and return the summed CPU%/RAM of the tree rooted at
    /// `root_pid` (a terminal's shell pid). A pid that is GONE (the shell exited) yields
    /// the all-zero reading — NEVER an error.
    ///
    /// SYNCHRONOUS, kept for parity/single-pid callers; the host's per-tick poll uses the
    /// ASYNC [`tree_stats_batch`](Self::tree_stats_batch) so the scan never blocks the loop.
    #[napi]
    pub fn tree_stats(&self, root_pid: u32) -> TreeStats {
        let s = self.inner.lock().unwrap().tree_stats(root_pid);
        TreeStats {
            cpu_pct: s.cpu_pct as f64,
            mem_bytes: s.mem_bytes as f64,
        }
    }

    /// Refresh the process table ONCE, then return the summed CPU%/RAM of the tree rooted
    /// at EACH pid in `roots`, one [`TreeStats`] per root IN THE SAME ORDER — OFF the Node
    /// main thread (FEEDBACK #28 perf).
    ///
    /// The host calls this ONCE per poll tick with every live terminal's shell pid. The
    /// expensive `/proc` scan happens EXACTLY ONCE per tick (not once per terminal) and on
    /// a libuv WORKER thread (the returned [`AsyncTask`] is a `Promise` in JS), so the Node
    /// event loop keeps servicing keystroke IPC + PTY output while the scan is in flight.
    #[napi(ts_return_type = "Promise<TreeStats[]>")]
    pub fn tree_stats_batch(&self, roots: Vec<u32>) -> AsyncTask<TreeStatsBatchTask> {
        AsyncTask::new(TreeStatsBatchTask {
            inner: Arc::clone(&self.inner),
            roots,
        })
    }
}

/// The off-main-thread process-tree scan (FEEDBACK #28 perf). `compute()` locks the shared
/// `ProcStats`, runs the SINGLE-refresh `tree_stats_batch` (so the whole `/proc` scan + the
/// per-root summation happen HERE, on a libuv worker thread, NOT the Node main loop), and
/// `resolve()` maps each `nyx_core::TreeStats` to the napi `TreeStats` — order preserved, so
/// the host can zip the result back to its `roots` input. Mirrors the `Task` pattern in
/// `core_db.rs`.
pub struct TreeStatsBatchTask {
    inner: Arc<Mutex<nyx_core::proc_stats::ProcStats>>,
    roots: Vec<u32>,
}

impl Task for TreeStatsBatchTask {
    type Output = Vec<nyx_core::proc_stats::TreeStats>;
    type JsValue = Vec<TreeStats>;

    fn compute(&mut self) -> Result<Self::Output> {
        // The blocking /proc scan happens HERE, on the libuv worker thread. The mutex is
        // held only for the scan (the only contender is the poll itself, one tick at a
        // time, so there is no real contention).
        Ok(self.inner.lock().unwrap().tree_stats_batch(&self.roots))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output
            .into_iter()
            .map(|s| TreeStats {
                cpu_pct: s.cpu_pct as f64,
                mem_bytes: s.mem_bytes as f64,
            })
            .collect())
    }
}

/// The output pump body (extracted so the spawn site stays readable). Owns the
/// leading-edge coalescing, the OSC 7 / OSC 133 scan (with a tail-carry across chunk
/// boundaries, mirroring the Tauri pump), and the exit reap. Runs until the reader
/// channel disconnects (child exit / master close).
fn run_pump(
    rx: Receiver<Vec<u8>>,
    data_tsfn: ThreadsafeFunction<Vec<u8>, ErrorStrategy::Fatal>,
    exit_tsfn: ThreadsafeFunction<Option<i32>, ErrorStrategy::Fatal>,
    cwd_tsfn: ThreadsafeFunction<String, ErrorStrategy::Fatal>,
    exec_tsfn: ThreadsafeFunction<ExecStateEvent, ErrorStrategy::Fatal>,
    pty: Arc<Mutex<Pty>>,
) {
    let mut pending: Vec<u8> = Vec::new();
    let mut last_flush = Instant::now();
    // Carry an incomplete trailing OSC 133 introducer across chunk boundaries so a
    // split `ESC]133;…` sequence is recovered (parity with the Tauri pump's
    // per-terminal tail buffer).
    let mut osc133_tail: Vec<u8> = Vec::new();
    // The OSC 133 → exec-state machine, carried across chunks so a `C` (pre-exec)
    // in one chunk arms the settle for its matching `D` (command-end) in a later
    // one. The provenance guard inside ensures a `D` with NO preceding `C` (the
    // shell's first prompt at spawn emits `D;0` with no `C`; a bare Enter too) is
    // IGNORED — no phantom success badge on a freshly-spawned terminal.
    let mut exec_sm = osc133::ExecStateMachine::new();

    let flush = |data_tsfn: &ThreadsafeFunction<Vec<u8>, ErrorStrategy::Fatal>,
                 pending: &mut Vec<u8>| {
        if pending.is_empty() {
            return;
        }
        let chunk = std::mem::take(pending);
        data_tsfn.call(chunk, ThreadsafeFunctionCallMode::NonBlocking);
    };

    loop {
        // LEADING-EDGE coalescing, identical strategy to the Tauri pump:
        //   - `pending` EMPTY (idle / between keystrokes): BLOCK on `recv()`. The
        //     clock is not refreshed while we sleep, so the next byte flushes
        //     immediately (leading edge, ~0 retention).
        //   - `pending` NON-EMPTY (mid-flood): wait at most until the next scheduled
        //     flush so a steady flood coalesces on the 16ms cadence (anti event-DoS).
        let recv = if pending.is_empty() {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            let wait = FLUSH_INTERVAL.saturating_sub(last_flush.elapsed());
            rx.recv_timeout(wait)
        };
        match recv {
            Ok(chunk) => {
                // Portable cwd source: surface the most-recent OSC 7 cwd in this
                // chunk (a cheap substring scan). Observational — bytes still flow
                // to `on_data` below.
                if let Some(cwd) = osc7::extract_last_cwd(&chunk) {
                    cwd_tsfn.call(cwd, ThreadsafeFunctionCallMode::NonBlocking);
                }
                // Exec-state source (OSC 133): stitch the carried tail, decode every
                // complete event, carry the new incomplete tail, and emit each
                // command-END transition THAT A REAL `C` PRECEDED. Never strips
                // bytes from the output stream.
                handle_osc133(&chunk, &mut osc133_tail, &mut exec_sm, &exec_tsfn);

                pending.extend_from_slice(&chunk);
                if last_flush.elapsed() >= FLUSH_INTERVAL {
                    flush(&data_tsfn, &mut pending);
                    last_flush = Instant::now();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                flush(&data_tsfn, &mut pending);
                last_flush = Instant::now();
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Child exited / master closed: flush the tail, reap the code, exit.
                flush(&data_tsfn, &mut pending);
                let code = pty.lock().unwrap().wait();
                exit_tsfn.call(code, ThreadsafeFunctionCallMode::NonBlocking);
                break;
            }
        }
    }
}

/// Scan one raw chunk for OSC 133 command-lifecycle events (stitching the carried
/// `tail` ahead of it so a split sequence is recovered), emit each command-END
/// transition THAT A REAL `C` (pre-exec) PRECEDED, and update `tail` with the new
/// trailing incomplete introducer. Mirrors the Tauri bridge's `handle_osc133_chunk`
/// + `drive_exec_state`, minus the DB write (phase 5): here we only EMIT the decoded
/// transition.
///
/// The `sm` ([`osc133::ExecStateMachine`]) carries the PROVENANCE GUARD across
/// chunks: a `D` (command-end) only settles a `success`/`error` when a `C`
/// (pre-exec — a real command ran) has been seen since the last settle. A `D` with
/// no preceding `C` (the shell's FIRST prompt at spawn emits `D;0` because `$?` is
/// true; a bare Enter on an empty prompt does too) is a PHANTOM end and is IGNORED —
/// no exec-state event, so no green dot on a freshly-spawned terminal.
fn handle_osc133(
    chunk: &[u8],
    tail: &mut Vec<u8>,
    sm: &mut osc133::ExecStateMachine,
    exec_tsfn: &ThreadsafeFunction<ExecStateEvent, ErrorStrategy::Fatal>,
) {
    // Stitch tail || chunk. The tail is bounded (only an unterminated trailing
    // introducer is ever carried).
    let stitched: Vec<u8> = if tail.is_empty() {
        chunk.to_vec()
    } else {
        let mut s = std::mem::take(tail);
        s.extend_from_slice(chunk);
        s
    };

    for ev in osc133::extract_events(&stitched) {
        // The state machine arms on `C` and settles on `D` — but ONLY a `D` that a
        // real `C` preceded yields an outcome. A/B and a prompt-initial / empty-Enter
        // `D` (no preceding `C`) yield `None` (inert; the OS busy signal owns
        // `running`, exactly as in the Tauri bridge).
        if let Some(outcome) = sm.on_event(ev) {
            let (state, exit_code) = match outcome {
                osc133::ExecOutcome::Success => ("success", Some(0)),
                osc133::ExecOutcome::Error { exit_code } => ("error", exit_code),
            };
            exec_tsfn.call(
                ExecStateEvent {
                    state: state.to_string(),
                    exit_code,
                },
                ThreadsafeFunctionCallMode::NonBlocking,
            );
        }
    }

    // Carry forward any trailing incomplete OSC 133 introducer so the next chunk
    // can complete it.
    *tail = osc133_incomplete_tail(&stitched);
}

/// Return the trailing bytes of `buf` that begin an OSC 133 introducer (`ESC]133;`)
/// with NO terminator yet — the incomplete tail to carry into the next chunk. An
/// empty result means the buffer ended on a complete boundary. Mirrors the Tauri
/// bridge's `osc133_incomplete_tail`.
fn osc133_incomplete_tail(buf: &[u8]) -> Vec<u8> {
    const INTRO: &[u8] = b"\x1b]133;";
    // Find the LAST introducer; if everything after it lacks a terminator, that is
    // the incomplete tail. We also handle a PARTIAL introducer at the very end
    // (e.g. the chunk ended mid-`ESC]13`) by retaining a trailing run that could be
    // the start of an introducer.
    if let Some(pos) = last_subslice(buf, INTRO) {
        let after = &buf[pos..];
        // Is there a terminator (BEL or ST) somewhere after the introducer?
        if !has_terminator(&after[INTRO.len().min(after.len())..]) {
            return after.to_vec();
        }
    }
    // No full introducer pending. Guard the split-introducer case: if the buffer
    // ends with a prefix of `ESC]133;`, keep it so the next chunk can complete it.
    for n in (1..INTRO.len()).rev() {
        if buf.len() >= n && buf[buf.len() - n..] == INTRO[..n] {
            return buf[buf.len() - n..].to_vec();
        }
    }
    Vec::new()
}

/// Whether `bytes` contains an OSC terminator (`BEL` = 0x07 or `ST` = ESC `\`).
fn has_terminator(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x07 {
            return true;
        }
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            return true;
        }
        i += 1;
    }
    false
}

/// Last index of `needle` in `haystack`, or `None`.
fn last_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_tail_carries_split_introducer() {
        // A chunk ending mid-introducer keeps the prefix.
        assert_eq!(osc133_incomplete_tail(b"out\x1b]13"), b"\x1b]13".to_vec());
        // A chunk ending mid-sequence (introducer complete, no terminator) keeps it.
        assert_eq!(
            osc133_incomplete_tail(b"\x1b]133;D;0"),
            b"\x1b]133;D;0".to_vec()
        );
    }

    #[test]
    fn incomplete_tail_empty_when_complete() {
        // A fully-terminated sequence leaves nothing to carry.
        assert!(osc133_incomplete_tail(b"\x1b]133;D;0\x07").is_empty());
        // Plain output with no introducer carries nothing.
        assert!(osc133_incomplete_tail(b"just output\r\n").is_empty());
    }

    #[test]
    fn has_terminator_detects_bel_and_st() {
        assert!(has_terminator(b"abc\x07"));
        assert!(has_terminator(b"abc\x1b\\"));
        assert!(!has_terminator(b"abc"));
        assert!(!has_terminator(b"abc\x1b")); // lone ESC, not ST
    }
}
