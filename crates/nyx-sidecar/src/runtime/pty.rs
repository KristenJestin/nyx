//! Interactive-PTY path for the sidecar (HHC — `01KWCJPFP72FP5CDVWSB8DTHHC`).
//!
//! Ports v1's `portable-pty` integration + the lossless pause-gate flow control from
//! `nyx-core`, but streams raw PTY buffers as BINARY over the [`crate::pipe`]
//! transport ([`Event::TerminalOutput`], MessagePack `bin` — NO base64 per chunk),
//! with end-to-end backpressure and kill-by-process-group.
//!
//! ## What moved out of v1 / Node
//!
//! In v1 the interactive PTY pump lived in the `nyx-napi` `NyxPty` class (the Node
//! core-host owned it): a Rust pump thread coalesced output and delivered it to Node
//! through a `ThreadsafeFunction`, and the host drove the pause-gate from its
//! xterm-acknowledged backlog. This module is the faithful port of that pump
//! (`run_pump` + the OSC scan + the leading-edge coalescing), with two changes:
//!   * the per-chunk delivery is a BINARY [`Event::TerminalOutput`] frame on the pipe
//!     instead of a napi `Buffer` callback — no base64 anywhere;
//!   * the flow-control accounting (the per-terminal quota + pause/resume) lives HERE
//!     in the sidecar, driven by the renderer's [`Request::PtyAck`] credit, so the
//!     pause-gate holds under flood with bounded memory and zero loss.
//!
//! ## Lossless backpressure (the pause-gate)
//!
//! The pump reserves quota in a [`FlowController`] BEFORE emitting each chunk. When a
//! terminal's in-flight (unacked) bytes reach the high-water mark, the pump pauses the
//! [`nyx_core::pty::PauseGate`]: the core reader thread blocks at its gate, the kernel
//! PTY buffer fills, and the child blocks on its own `write` — real OS backpressure,
//! so nothing is dropped or reordered. A [`Request::PtyAck`] frees quota; once the
//! backlog drops below the low-water mark the pump resumes the gate. The hysteresis
//! gap avoids pause/resume thrash.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nyx_core::osc133::{self, ExecOutcome};
use nyx_core::osc7;
use nyx_core::pty::{PauseGate, Pty};
use nyx_core::vt_screen::{ScreenSnapshot, VtScreen};
use portable_pty::PtySize;

use super::events::EventSink;
use crate::pipe::wire::Event;

/// Output coalescing cadence: ~60fps. Mirrors the v1 napi pump's `FLUSH_INTERVAL` so a
/// flood (`yes`) never fans out one event per chunk/line.
const FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// Max bytes per emitted [`Event::TerminalOutput`] frame. A coalesced flood chunk is
/// split into pieces no larger than this before crossing the pipe, so the backlog is
/// bounded to `HIGH_WATER + CHUNK_BYTES` and one frame never carries a multi-MiB
/// payload. Mirrors the v1 host's `CHUNK_BYTES`.
const CHUNK_BYTES: usize = 64 * 1024;

/// High-water mark: at or above this many UNACKED output bytes for a terminal, the
/// pump pauses that terminal's reader (OS backpressure to the child). Mirrors the v1
/// host's `HIGH_WATER`.
const HIGH_WATER: usize = 512 * 1024;

/// Low-water mark: once the unacked backlog drops below this, the pump resumes the
/// reader. The hysteresis gap (HIGH → LOW) avoids pause/resume thrashing under flood.
/// Mirrors the v1 host's `LOW_WATER`.
const LOW_WATER: usize = 128 * 1024;

/// Per-terminal flow-control accounting: the in-flight (sent-but-unacked) byte count
/// and the live pause state. Shared between the pump thread (which reserves on emit
/// and pauses at the high-water mark) and the ack handler (which frees quota and
/// resumes under the low-water mark).
struct FlowState {
    /// Bytes sent to the renderer that have not yet been acked.
    in_flight: usize,
    /// Whether the reader is currently paused by backpressure.
    paused: bool,
    /// Total bytes emitted so far (the monotonically increasing `seq` base — actually
    /// we use a separate per-chunk seq below; this is informational).
    seq: u64,
}

/// One live interactive terminal: the kill/write/resize handles plus its flow state.
struct LiveTerminal {
    pty: Arc<Mutex<Pty>>,
    pause_gate: PauseGate,
    flow: Arc<Mutex<FlowState>>,
    /// The pump thread handle, joined on close so the thread never leaks.
    pump: Option<JoinHandle<()>>,
    /// The headless-VT feeder thread handle, joined on close so it never leaks.
    feeder: Option<JoinHandle<()>>,
    /// The persistent terminal RECORD id this PTY was spawned for, if any (so close can
    /// retract the record -> live-pty join from `live_records`).
    record_id: Option<String>,
    /// The HEADLESS VT screen model (v2.2.5): a dedicated FEEDER thread parses the SAME raw PTY
    /// bytes the pump streams to the renderer into this grid (main + alt-screen), so
    /// [`PtyManager::snapshot`] can capture a serializable [`ScreenSnapshot`] for lossless
    /// re-hydration at restart — a full-screen TUI included, not just scrollback.
    ///
    /// CRITICAL: the parsing runs on the feeder thread, NOT on the pump's read→emit path. The
    /// pump only does an O(1) channel send of each chunk to the feeder, so the (slow, byte-wise)
    /// VT parse can never widen the pump's read-ahead window and thereby weaken the lossless
    /// pause-gate backpressure. The correctness of live RENDERING stays with ghostty in the
    /// renderer (this is a restore-time snapshot, per the PRD).
    screen: Arc<Mutex<VtScreen>>,
    /// The channel INTO the feeder thread. The pump sends each raw chunk here (O(1)); dropping
    /// this sender (on close) disconnects the feeder so it drains and exits. `None` once close
    /// has taken it to trigger that shutdown.
    feed_tx: Option<Sender<Vec<u8>>>,
}

/// The live interactive-PTY registry + pump driver. The [`super::Runtime`] holds one;
/// the pipe dispatch calls [`spawn`](PtyManager::spawn) / [`write`](PtyManager::write)
/// / [`resize`](PtyManager::resize) / [`close`](PtyManager::close) /
/// [`ack`](PtyManager::ack) on it.
#[derive(Default)]
pub struct PtyManager {
    terminals: Mutex<HashMap<u32, LiveTerminal>>,
    /// record-id -> live pty id. The sidecar registers this join itself when a PTY is
    /// spawned WITH a `record_id` (the v1 `register_terminal_pty`), so the MCP terminal
    /// tools can resolve a record to a LIVE shell synchronously: `send_to_terminal`
    /// returns `invalid_state` when no PTY is live (parity), and `list_terminals`
    /// reports the true `live` bit. Wrapped in its OWN `Arc<Mutex>` so the MCP
    /// dispatcher (a separate thread) shares the SAME registry.
    live_records: Arc<Mutex<HashMap<String, u32>>>,
}

impl PtyManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live terminals currently registered.
    pub fn live_count(&self) -> usize {
        self.terminals.lock().unwrap().len()
    }

    /// Whether a terminal id is live.
    pub fn is_live(&self, terminal_id: u32) -> bool {
        self.terminals.lock().unwrap().contains_key(&terminal_id)
    }

    /// Spawn an interactive shell PTY of `cols`x`rows`, start its pump (binary output
    /// over `events` + OSC cwd/exec-state + exit), and return the live terminal id.
    ///
    /// `cwd` empty -> the sidecar's cwd; `record_id` empty -> no `NYX_TERMINAL_ID`
    /// export. A spawn failure is a readable error (never a half-spawn).
    pub fn spawn(
        &self,
        cols: u16,
        rows: u16,
        cwd: &str,
        record_id: &str,
        events: EventSink,
    ) -> anyhow::Result<u32> {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let cwd_opt = (!cwd.is_empty()).then_some(cwd);
        let record_opt = (!record_id.is_empty()).then_some(record_id);
        let (pty, rx) = Pty::spawn(size, cwd_opt, record_opt)?;
        let terminal_id = pty.id() as u32;
        let pause_gate = pty.pause_gate();
        let flow = Arc::new(Mutex::new(FlowState {
            in_flight: 0,
            paused: false,
            seq: 0,
        }));
        let pty = Arc::new(Mutex::new(pty));
        // Headless VT screen model (v2.2.5): sized to match the spawn geometry so its grid is
        // the same shape the renderer paints.
        let screen = Arc::new(Mutex::new(VtScreen::new(rows, cols)));

        // FEEDER thread: owns the slow, byte-wise VT parse OFF the pump's read→emit path. The
        // pump sends each raw chunk here (O(1)); this thread parses it into `screen`. Dropping
        // the sender (on close) disconnects `feed_rx` so the feeder drains and exits.
        let (feed_tx, feed_rx) = mpsc::channel::<Vec<u8>>();
        let feeder_screen = Arc::clone(&screen);
        let feeder = std::thread::Builder::new()
            .name(format!("nyx-sidecar-pty-feeder-{terminal_id}"))
            .spawn(move || {
                run_screen_feeder(feed_rx, feeder_screen);
            })?;

        // Spawn the pump thread: it OWNS the receiver and drives coalescing + OSC scan
        // + flow control until the child exits (the receiver disconnects on EOF). It also
        // forwards each chunk to the feeder for the VT screen model.
        let pump_flow = Arc::clone(&flow);
        let pump_gate = pause_gate.clone();
        let pump_pty = Arc::clone(&pty);
        let pump_feed_tx = feed_tx.clone();
        let pump = std::thread::Builder::new()
            .name(format!("nyx-sidecar-pty-pump-{terminal_id}"))
            .spawn(move || {
                run_pump(
                    terminal_id,
                    rx,
                    events,
                    pump_flow,
                    pump_gate,
                    pump_pty,
                    pump_feed_tx,
                );
            })?;

        self.terminals.lock().unwrap().insert(
            terminal_id,
            LiveTerminal {
                pty,
                pause_gate,
                flow,
                pump: Some(pump),
                feeder: Some(feeder),
                record_id: (!record_id.is_empty()).then(|| record_id.to_string()),
                screen,
                feed_tx: Some(feed_tx),
            },
        );
        // Register the record -> live pty join so the MCP terminal tools resolve this
        // record to a LIVE shell synchronously (the sidecar's own
        // `register_terminal_pty`, since the sidecar now OWNS the PTY).
        if !record_id.is_empty() {
            self.live_records
                .lock()
                .unwrap()
                .insert(record_id.to_string(), terminal_id);
        }
        Ok(terminal_id)
    }

    /// The shared record -> live-pty registry (so the MCP dispatcher's `TerminalHost`
    /// reads the same liveness the spawn/close maintain).
    pub fn live_records(&self) -> Arc<Mutex<HashMap<String, u32>>> {
        Arc::clone(&self.live_records)
    }

    /// Write a command LINE to a terminal by its RECORD id (the MCP `send_to_terminal`
    /// live-PTY half). Returns `Ok(false)` when no live PTY is registered for the record
    /// (the caller surfaces `invalid_state` — parity with v1), `Ok(true)` on a write.
    pub fn write_to_record(&self, record_id: &str, data: &[u8]) -> bool {
        let pty_id = self.live_records.lock().unwrap().get(record_id).copied();
        match pty_id {
            Some(id) => self.write(id, data).is_ok(),
            None => false,
        }
    }

    /// Close a terminal by its RECORD id (the MCP `close_terminal` live-PTY half).
    pub fn close_record(&self, record_id: &str) {
        let pty_id = self.live_records.lock().unwrap().get(record_id).copied();
        if let Some(id) = pty_id {
            self.close(id);
        }
    }

    /// Write raw bytes to a live PTY (keystrokes / paste). Clones the writer handle
    /// under the registry lock then writes WITHOUT it, so a stuck child stalls only
    /// its own pty (parity with the v1 `PtyManager::write_to`). Unknown id -> error.
    pub fn write(&self, terminal_id: u32, data: &[u8]) -> anyhow::Result<()> {
        // Clone the pty `Arc` out from under the registry lock, then grab the writer
        // handle WITHOUT the registry lock so a stuck child stalls only its own pty.
        let pty = {
            let terms = self.terminals.lock().unwrap();
            terms
                .get(&terminal_id)
                .map(|t| Arc::clone(&t.pty))
                .ok_or_else(|| anyhow::anyhow!("unknown terminal {terminal_id}"))?
        };
        let writer = pty.lock().unwrap().writer_handle();
        let mut w = writer.lock().unwrap();
        use std::io::Write as _;
        w.write_all(data)?;
        w.flush()?;
        Ok(())
    }

    /// Resize a live PTY (SIGWINCH to the child). Unknown id -> error.
    pub fn resize(&self, terminal_id: u32, cols: u16, rows: u16) -> anyhow::Result<()> {
        // Clone the pty + screen `Arc`s out from under the registry lock so the (potentially
        // blocking) resize never holds the whole-registry lock.
        let (pty, screen) = {
            let terms = self.terminals.lock().unwrap();
            let t = terms
                .get(&terminal_id)
                .ok_or_else(|| anyhow::anyhow!("unknown terminal {terminal_id}"))?;
            (Arc::clone(&t.pty), Arc::clone(&t.screen))
        };
        // Keep the headless VT grid the same shape as the PTY window so its snapshot matches
        // what the renderer paints after a SIGWINCH.
        screen.lock().unwrap().resize(rows, cols);
        let result = pty.lock().unwrap().resize(cols, rows, 0, 0);
        result
    }

    /// The serializable VT screen SNAPSHOT of a live terminal by its live pty id — the
    /// current on-screen grid (main + alt-screen), for lossless re-hydration at restart
    /// (v2.2.5). `None` for an unknown / already-closed terminal.
    pub fn snapshot(&self, terminal_id: u32) -> Option<ScreenSnapshot> {
        let screen = {
            let terms = self.terminals.lock().unwrap();
            Arc::clone(&terms.get(&terminal_id)?.screen)
        };
        let snap = screen.lock().unwrap().snapshot();
        Some(snap)
    }

    /// The VT screen snapshot of a live terminal by its persistent RECORD id (the MCP / restore
    /// path keys terminals by record id). `None` when no live PTY backs the record.
    pub fn snapshot_by_record(&self, record_id: &str) -> Option<ScreenSnapshot> {
        let pty_id = self.live_records.lock().unwrap().get(record_id).copied()?;
        self.snapshot(pty_id)
    }

    /// Every live terminal that carries a persistent RECORD id, as `(record_id, snapshot)` of
    /// its current VT screen. This is what the ordered shutdown persists so the next boot can
    /// re-hydrate each terminal (a TUI on the alt-screen included). Record-less terminals are
    /// omitted (they cannot be correlated back at restart).
    pub fn live_snapshots_by_record(&self) -> Vec<(String, ScreenSnapshot)> {
        let terms = self.terminals.lock().unwrap();
        terms
            .values()
            .filter_map(|t| {
                let rec = t.record_id.clone()?;
                let snap = t.screen.lock().unwrap().snapshot();
                Some((rec, snap))
            })
            .collect()
    }

    /// Close a live PTY: kill the interactive shell and join its pump. The pump
    /// flushes its tail + emits `pty-exit` as the reader EOFs. Idempotent (closing an
    /// unknown / already-closed terminal is a no-op).
    ///
    /// This is the INTERACTIVE-terminal kill, faithful to v1: `Pty::kill` calls the
    /// `portable-pty` killer on the session-leader shell (it is spawned via `setsid`),
    /// so it terminates the shell directly; foreground descendants then receive the
    /// PTY session hangup. This is NOT the explicit negative-pgid tree kill — that
    /// `kill(-pgid, SIGTERM/SIGKILL)` path is reserved for MANAGED commands
    /// (`nyx-core` `command.rs`), not interactive terminals.
    pub fn close(&self, terminal_id: u32) {
        let live = self.terminals.lock().unwrap().remove(&terminal_id);
        if let Some(mut live) = live {
            // Retract the record -> live-pty join (so the MCP tools see it go offline).
            if let Some(rec) = &live.record_id {
                self.live_records.lock().unwrap().remove(rec);
            }
            // Kill the session-leader shell (portable-pty killer), then resume the gate
            // so a paused reader can loop, hit EOF and the pump exits.
            let _ = live.pty.lock().unwrap().kill();
            live.pause_gate.set_paused(false);
            if let Some(handle) = live.pump.take() {
                let _ = handle.join();
            }
            // Drop the feeder's sender so its channel disconnects, then join it (the pump also
            // held a sender clone, but it dropped when the pump thread exited above). The feeder
            // drains any remaining chunks and exits — it never leaks past close.
            drop(live.feed_tx.take());
            if let Some(handle) = live.feeder.take() {
                let _ = handle.join();
            }
        }
    }

    /// FLOW-CONTROL ACK: the renderer consumed `bytes` of `terminal_id`'s output. Free
    /// that much quota and, if the backlog dropped below the low-water mark, RESUME the
    /// reader. The credit half of the lossless pause-gate loop. Unknown id -> no-op.
    pub fn ack(&self, terminal_id: u32, bytes: u64) {
        let terms = self.terminals.lock().unwrap();
        let Some(t) = terms.get(&terminal_id) else {
            return;
        };
        let mut flow = t.flow.lock().unwrap();
        flow.in_flight = flow.in_flight.saturating_sub(bytes as usize);
        if flow.paused && flow.in_flight < LOW_WATER {
            flow.paused = false;
            t.pause_gate.set_paused(false);
        }
    }

    /// LIVE auto-label introspection of a terminal by its live pty id (the v1
    /// `terminal_info` command backend — auto-label / shell-suffix). Reads `{ cwd,
    /// foreground }` straight from the kernel (Linux `/proc`), keyed by the live PTY's
    /// `shell_pid` + `foreground_pgid` — NOT from OSC 7. On a non-Linux platform (no
    /// `/proc`) both are `None` WITHOUT erroring, so the per-second poll never spams.
    /// Unknown / exited pty -> `{ None, None }`.
    pub fn terminal_info(&self, terminal_id: u32) -> (Option<String>, Option<String>) {
        let pty = {
            let terms = self.terminals.lock().unwrap();
            match terms.get(&terminal_id) {
                Some(t) => Arc::clone(&t.pty),
                None => return (None, None),
            }
        };
        let pty = pty.lock().unwrap();
        read_terminal_info(pty.shell_pid(), foreground_pgid(&pty))
    }

    /// Close every live terminal (shutdown). Kills each session-leader shell (the
    /// interactive killer, as in [`close`](Self::close)) and joins its pump so nothing
    /// is orphaned past exit.
    pub fn close_all(&self) {
        let ids: Vec<u32> = self.terminals.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.close(id);
        }
    }
}

/// The foreground process group id of a live PTY (`tcgetpgrp(master)`), abstracted so
/// the non-Unix build compiles (no foreground pgid -> always `None`). Parity with the v1
/// `foreground_pgid` helper.
#[cfg(unix)]
fn foreground_pgid(pty: &Pty) -> Option<i32> {
    pty.foreground_pgid()
}
#[cfg(not(unix))]
fn foreground_pgid(_pty: &Pty) -> Option<i32> {
    None
}

/// Read `{ cwd, foreground }` from `/proc`, keyed by the live PTY's `shell_pid` +
/// `foreground_pgid` — the Linux auto-label source. Every other platform returns the
/// empty reading (`{ None, None }`): a clean degradation, never an error (so the
/// per-second poll never spams). Mirrors the v1 `read_terminal_info`.
#[cfg(target_os = "linux")]
fn read_terminal_info(
    shell_pid: Option<u32>,
    fg_pgid: Option<i32>,
) -> (Option<String>, Option<String>) {
    (
        shell_pid.and_then(nyx_core::proc::read_cwd),
        fg_pgid.and_then(nyx_core::proc::read_foreground_comm),
    )
}
#[cfg(not(target_os = "linux"))]
fn read_terminal_info(
    _shell_pid: Option<u32>,
    _fg_pgid: Option<i32>,
) -> (Option<String>, Option<String>) {
    (None, None)
}

/// The pump body (ported from the v1 napi `run_pump`): leading-edge coalescing, the
/// OSC 7 / OSC 133 scan with a tail-carry across chunk boundaries, the flow-control
/// reserve + pause-gate, and the exit reap. Runs until the reader channel disconnects
/// (child exit / master close).
fn run_pump(
    terminal_id: u32,
    rx: Receiver<Vec<u8>>,
    events: EventSink,
    flow: Arc<Mutex<FlowState>>,
    gate: PauseGate,
    pty: Arc<Mutex<Pty>>,
    feed_tx: Sender<Vec<u8>>,
) {
    let mut pending: Vec<u8> = Vec::new();
    let mut last_flush = Instant::now();
    let mut osc133_tail: Vec<u8> = Vec::new();
    let mut exec_sm = osc133::ExecStateMachine::new();

    loop {
        // LEADING-EDGE coalescing, identical strategy to the v1 napi pump:
        //   - `pending` EMPTY: BLOCK on `recv()` so the next byte flushes immediately.
        //   - `pending` NON-EMPTY: wait at most until the next scheduled flush so a
        //     steady flood coalesces on the 16ms cadence.
        let recv = if pending.is_empty() {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            let wait = FLUSH_INTERVAL.saturating_sub(last_flush.elapsed());
            rx.recv_timeout(wait)
        };
        match recv {
            Ok(chunk) => {
                // Forward the SAME raw bytes to the headless VT feeder (v2.2.5) with an O(1)
                // channel send — the slow byte-wise parse runs on the FEEDER thread, so it never
                // slows this read→emit loop and can never widen the read-ahead window that the
                // lossless pause-gate backpressure depends on. A `send` error means the feeder is
                // gone (close); we simply stop forwarding then. Observational — the bytes still
                // flow unchanged to TerminalOutput below.
                let _ = feed_tx.send(chunk.clone());
                // OSC 7 cwd (observational; bytes still flow to TerminalOutput below).
                if let Some(cwd) = osc7::extract_last_cwd(&chunk) {
                    events.emit(Event::PtyCwd { terminal_id, cwd });
                }
                // OSC 133 exec-state (stitch the carried tail, emit each command-END
                // transition a real `C` preceded). Never strips bytes.
                handle_osc133(terminal_id, &chunk, &mut osc133_tail, &mut exec_sm, &events);

                pending.extend_from_slice(&chunk);
                if last_flush.elapsed() >= FLUSH_INTERVAL {
                    flush(terminal_id, &mut pending, &events, &flow, &gate);
                    last_flush = Instant::now();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                flush(terminal_id, &mut pending, &events, &flow, &gate);
                last_flush = Instant::now();
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Child exited / master closed: flush the tail, reap the code, emit
                // `pty-exit`, exit the pump. The `feed_tx` clone drops here, so once close also
                // drops the stored sender the feeder disconnects and exits.
                flush(terminal_id, &mut pending, &events, &flow, &gate);
                let code = pty.lock().unwrap().wait();
                events.emit(Event::PtyExit { terminal_id, code });
                break;
            }
        }
    }
}

/// The headless-VT FEEDER loop (v2.2.5): drain raw output chunks off the pump and parse them
/// into the [`VtScreen`], entirely OFF the pump's latency-critical read→emit path.
///
/// Why a separate thread: the VT parse is byte-wise and, under a pathological flood (`yes`),
/// parsing megabytes is slow. If it ran inline on the pump, it would slow the pump's drain of
/// the reader channel and let the core reader race ahead — widening the read-ahead window and
/// weakening the lossless pause-gate backpressure (the unacked backlog would blow past the
/// high-water mark). Here the pump only does an O(1) `send`, so backpressure is unaffected.
///
/// Coalescing: the screen is EVENTUALLY-CONSISTENT — only the *final* grid matters for a
/// restart snapshot. So under a burst we drain everything currently queued and feed it in one
/// batch, which lets the parser skip nothing but avoids per-chunk lock churn. Exits when the
/// channel disconnects (the pump exited AND close dropped the stored sender).
fn run_screen_feeder(rx: Receiver<Vec<u8>>, screen: Arc<Mutex<VtScreen>>) {
    while let Ok(first) = rx.recv() {
        // Coalesce whatever else is already queued so a flood is fed in one locked batch.
        let mut batch = first;
        while let Ok(more) = rx.try_recv() {
            batch.extend_from_slice(&more);
        }
        screen.lock().unwrap().feed(&batch);
    }
}

/// Flush the coalesced `pending` buffer as one or more bounded BINARY
/// [`Event::TerminalOutput`] frames, accounting each frame's bytes against the
/// per-terminal flow quota and PAUSING the reader at the high-water mark.
///
/// Splitting at [`CHUNK_BYTES`] bounds each frame's payload; the per-chunk `seq` lets
/// the UI detect gaps. The reserve-then-pause is the producer half of the lossless
/// pause-gate: once the in-flight backlog reaches the high-water mark we pause the
/// gate, so the core reader blocks and the child gets OS backpressure — nothing is
/// dropped, and memory is bounded to `HIGH_WATER + CHUNK_BYTES`.
fn flush(
    terminal_id: u32,
    pending: &mut Vec<u8>,
    events: &EventSink,
    flow: &Arc<Mutex<FlowState>>,
    gate: &PauseGate,
) {
    if pending.is_empty() {
        return;
    }
    let chunk = std::mem::take(pending);
    for piece in chunk.chunks(CHUNK_BYTES) {
        let seq = {
            let mut f = flow.lock().unwrap();
            f.in_flight += piece.len();
            let seq = f.seq;
            f.seq += 1;
            // At/over the high-water mark: pause the reader (backpressure). The ack
            // handler resumes it once the backlog drops below the low-water mark.
            if !f.paused && f.in_flight >= HIGH_WATER {
                f.paused = true;
                gate.set_paused(true);
            }
            seq
        };
        events.emit(Event::TerminalOutput {
            terminal_id,
            seq,
            data: piece.to_vec(),
        });
    }
}

/// Scan one raw chunk for OSC 133 command-lifecycle events (stitching the carried
/// `tail` ahead of it so a split sequence is recovered), emit each command-END
/// transition THAT A REAL `C` PRECEDED as a [`Event::PtyExecState`], and update `tail`
/// with the new trailing incomplete introducer. Ported from the v1 napi
/// `handle_osc133`.
fn handle_osc133(
    terminal_id: u32,
    chunk: &[u8],
    tail: &mut Vec<u8>,
    sm: &mut osc133::ExecStateMachine,
    events: &EventSink,
) {
    let stitched: Vec<u8> = if tail.is_empty() {
        chunk.to_vec()
    } else {
        let mut s = std::mem::take(tail);
        s.extend_from_slice(chunk);
        s
    };

    for ev in osc133::extract_events(&stitched) {
        if let Some(outcome) = sm.on_event(ev) {
            let (state, exit_code) = match outcome {
                ExecOutcome::Success => ("success", Some(0)),
                ExecOutcome::Error { exit_code } => ("error", exit_code),
            };
            events.emit(Event::PtyExecState {
                terminal_id,
                state: state.to_string(),
                exit_code,
            });
        }
    }

    *tail = osc133_incomplete_tail(&stitched);
}

/// Return the trailing bytes of `buf` that begin an OSC 133 introducer (`ESC]133;`)
/// with NO terminator yet — the incomplete tail to carry into the next chunk. Ported
/// verbatim from the v1 napi `osc133_incomplete_tail`.
fn osc133_incomplete_tail(buf: &[u8]) -> Vec<u8> {
    const INTRO: &[u8] = b"\x1b]133;";
    if let Some(pos) = last_subslice(buf, INTRO) {
        let after = &buf[pos..];
        if !has_terminator(&after[INTRO.len().min(after.len())..]) {
            return after.to_vec();
        }
    }
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
        assert_eq!(osc133_incomplete_tail(b"out\x1b]13"), b"\x1b]13".to_vec());
        assert_eq!(
            osc133_incomplete_tail(b"\x1b]133;D;0"),
            b"\x1b]133;D;0".to_vec()
        );
    }

    #[test]
    fn incomplete_tail_empty_when_complete() {
        assert!(osc133_incomplete_tail(b"\x1b]133;D;0\x07").is_empty());
        assert!(osc133_incomplete_tail(b"just output\r\n").is_empty());
    }

    #[test]
    fn has_terminator_detects_bel_and_st() {
        assert!(has_terminator(b"abc\x07"));
        assert!(has_terminator(b"abc\x1b\\"));
        assert!(!has_terminator(b"abc"));
        assert!(!has_terminator(b"abc\x1b"));
    }

    // --- per-terminal VT screen snapshot (v2.2.5, PRD 01KWMDBKW7GZ0X310TK64QEN32) ---------

    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::runtime::events::EventSink;

    /// A discard sink: the pump's `TerminalOutput`/exit frames go nowhere in these tests — we
    /// assert on the SNAPSHOT, not the wire, so the events are irrelevant here.
    #[derive(Clone)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn discard_sink() -> EventSink {
        EventSink::new(Box::new(Sink(Arc::new(Mutex::new(Vec::new())))))
    }

    /// Poll the snapshot until `pred` holds or we time out, returning the last snapshot.
    fn snapshot_until<F>(
        mgr: &PtyManager,
        id: u32,
        timeout: Duration,
        pred: F,
    ) -> Option<ScreenSnapshot>
    where
        F: Fn(&ScreenSnapshot) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut last = None;
        while Instant::now() < deadline {
            if let Some(s) = mgr.snapshot(id) {
                let hit = pred(&s);
                last = Some(s);
                if hit {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        last
    }

    /// The pump feeds the per-terminal VT model, so a shell that prints text has a faithful
    /// on-screen snapshot (the main-screen half of the done-criteria). We echo a marker via
    /// `printf` (no trailing newline in the marker so the row text is exact) and read it back
    /// off the snapshot grid — proving the sidecar EXPOSES a per-terminal screen snapshot.
    #[cfg(unix)]
    #[test]
    fn snapshot_reflects_shell_output() {
        let mgr = PtyManager::new();
        let id = mgr
            .spawn(80, 24, "", "", discard_sink())
            .expect("spawn pty");
        // Print a distinctive marker to the terminal's own stdout (flows through the pump).
        mgr.write(id, b"printf 'NYX_SNAP_MARKER'\n").expect("write");
        let snap = snapshot_until(&mgr, id, Duration::from_secs(5), |s| {
            (0..s.main.rows).any(|r| s.main.row_text(r).contains("NYX_SNAP_MARKER"))
        })
        .expect("a snapshot");
        assert!(
            (0..snap.main.rows).any(|r| snap.main.row_text(r).contains("NYX_SNAP_MARKER")),
            "the VT snapshot must contain the shell's printed marker; got rows: {:?}",
            (0..snap.main.rows)
                .map(|r| snap.main.row_text(r))
                .collect::<Vec<_>>()
        );
        assert!(!snap.alt_active, "still on the main screen");
        mgr.close(id);
    }

    /// The crux of the PRD: a TUI paints into the ALTERNATE screen, which is invisible to the
    /// scrollback. Driving the alt-screen enter sequence + a painted line, the sidecar's
    /// snapshot must show `alt_active` with the alt-grid content — the "restore a TUI, not just
    /// the scrollback" capability. We emit the raw sequences via `printf` from the shell.
    #[cfg(unix)]
    #[test]
    fn snapshot_captures_alt_screen_tui() {
        let mgr = PtyManager::new();
        let id = mgr
            .spawn(80, 24, "", "", discard_sink())
            .expect("spawn pty");
        // Enter alt-screen (1049h), home, clear, paint a TUI line — all via one printf. Octal
        // \033 is ESC in POSIX printf. No newline after the payload so the row is exact.
        mgr.write(id, b"printf '\\033[?1049h\\033[H\\033[2JTUI_ALT_LINE'\n")
            .expect("write");
        let snap = snapshot_until(&mgr, id, Duration::from_secs(5), |s| {
            s.alt_active
                && s.alt
                    .as_ref()
                    .map(|a| (0..a.rows).any(|r| a.row_text(r).contains("TUI_ALT_LINE")))
                    .unwrap_or(false)
        })
        .expect("a snapshot");
        assert!(
            snap.alt_active,
            "the snapshot must show the alternate screen active"
        );
        let alt = snap.alt.expect("alt buffer present while in alt-screen");
        assert!(
            (0..alt.rows).any(|r| alt.row_text(r).contains("TUI_ALT_LINE")),
            "the alt-screen grid must carry the TUI's painted line; got rows: {:?}",
            (0..alt.rows).map(|r| alt.row_text(r)).collect::<Vec<_>>()
        );
        mgr.close(id);
    }

    /// An unknown / closed terminal has no snapshot.
    #[test]
    fn snapshot_none_for_unknown_terminal() {
        let mgr = PtyManager::new();
        assert!(mgr.snapshot(999_999).is_none());
    }
}
