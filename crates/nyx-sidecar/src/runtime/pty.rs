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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nyx_core::agent::AgentRegistry;
use nyx_core::db::{self, Db};
use nyx_core::osc133::{self, ExecOutcome};
use nyx_core::osc7;
use nyx_core::pty::{PauseGate, Pty, SHELL_KILL_GRACE};
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

/// `(cols, rows)` of a shell spawned with no pane behind it
/// ([`PtyManager::spawn_headless`]). The classic terminal default: nothing on screen dictates a
/// geometry, and the mount that later attaches resizes the shell to its real pane.
const HEADLESS_SIZE: (u16, u16) = (80, 24);

/// Minimum delay between two SAVES of a view-less terminal's scrollback
/// ([`ScrollbackOwner::save_headless`]). The bytes are accumulated on every chunk, so nothing
/// waits on this — only the SQLite write does, which is why a flood must not trigger one per
/// 16ms flush. Mirrors the renderer's own debounce (750ms) for a mounted terminal, so a read is
/// no less fresh off-screen than on it.
const HEADLESS_SAVE_INTERVAL: Duration = Duration::from_millis(750);

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

/// The SIDECAR-owned scrollback of a terminal that has NO renderer view.
///
/// `terminals.scrollback` is normally written by the RENDERER (xterm's `SerializeAddon`,
/// debounced) and only while the terminal's pane is mounted. A shell opened over MCP has no pane
/// anywhere, so nothing ever saved it and `read_terminal` answered an empty buffer: an agent
/// could drive a terminal it could not read. A read must mean the same thing whether or not a
/// human happens to be looking at that project, so the sidecar persists what it pumps for as
/// long as — and ONLY as long as — nobody else does.
struct HeadlessScrollback {
    db: Arc<Db>,
    /// The persistent terminal RECORD id whose `scrollback` column this writes.
    record_id: String,
    /// Everything the shell has printed since the spawn, bounded to the column's own cap.
    ///
    /// This lock is also the HAND-OVER seam: [`ScrollbackOwner::hand_over_to_view`] takes it to
    /// make the final save and flip `has_view` in ONE critical section, so the pump can neither
    /// write after the renderer has become the owner (a double-write, clobbering its save) nor
    /// leave its accumulated history unsaved (a lost pre-mount history).
    buffer: Mutex<Vec<u8>>,
}

impl HeadlessScrollback {
    /// Overwrite the record's `scrollback` with the accumulated buffer. `persist_scrollback` is
    /// an overwrite (the same call the renderer's save makes), which is why the WHOLE history is
    /// kept in memory rather than appended blindly. Best-effort: a failed write leaves the
    /// previous buffer in place and the next save carries the same bytes again.
    fn save(&self, buffer: &[u8]) {
        let text = String::from_utf8_lossy(buffer);
        let _ = self
            .db
            .with_conn(|c| db::persist_scrollback(c, &self.record_id, &text));
    }
}

/// WHO owns a terminal's scrollback right now, and therefore where the pump's bytes go.
///
/// A pane attached (`has_view`) ⇒ the RENDERER owns it: the bytes are streamed to it and it
/// saves the serialized buffer, exactly as before. No pane ⇒ the SIDECAR owns it: nothing
/// crosses the pipe (no view means no ack, see [`LiveTerminal::has_view`]), so the pump keeps
/// the bytes and saves them itself. The bit is the single authority for that question, read by
/// the pump and flipped once, by the mount that takes the shell over.
#[derive(Clone)]
struct ScrollbackOwner {
    /// Whether a RENDERER VIEW is wired to this PTY — see [`LiveTerminal::has_view`].
    has_view: Arc<AtomicBool>,
    /// Whether the persisted `terminals.scrollback` is BEHIND the shell — see
    /// [`LiveTerminal::scrollback_dirty`].
    dirty: Arc<AtomicBool>,
    /// The sidecar's own persistence, present only for a shell spawned WITHOUT a view and
    /// carrying a record id (there is nothing to write into otherwise). `None` once — and
    /// because — the renderer has owned the buffer from the first byte.
    sidecar: Option<Arc<HeadlessScrollback>>,
}

impl ScrollbackOwner {
    /// Whether a renderer pane consumes (and acks) this terminal's output frames.
    fn has_view(&self) -> bool {
        self.has_view.load(Ordering::Relaxed)
    }

    /// The shell just printed: whatever is persisted is behind it, until a save says otherwise.
    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Whether the persisted scrollback is behind the shell (output has arrived since the last
    /// save) — what `read_terminal` reports as `stale`.
    fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    /// ACCUMULATE one raw output chunk while no pane is attached. Called on every chunk (not
    /// only on a flush) so the hand-over below has the FULL history to save — a mount must never
    /// cost the terminal the last bytes it printed before the pane appeared.
    fn record_headless(&self, chunk: &[u8]) {
        let Some(sidecar) = &self.sidecar else { return };
        let mut buffer = sidecar.buffer.lock().unwrap();
        // A pane may have attached between the caller's `has_view` check and this lock: from
        // that instant the renderer owns the buffer, so drop the chunk rather than grow a
        // history nobody will save.
        if self.has_view() {
            return;
        }
        buffer.extend_from_slice(chunk);
        // Keep only the tail the column can hold, so a chatty view-less shell cannot grow this
        // without bound (the DB write bounds too, but the memory must be bounded here).
        if buffer.len() > db::MAX_SCROLLBACK_BYTES {
            let cut = buffer.len() - db::MAX_SCROLLBACK_BYTES;
            buffer.drain(..cut);
        }
    }

    /// SAVE the accumulated buffer into the record. Separate from
    /// [`record_headless`](Self::record_headless) because the accumulation is per-chunk while
    /// the SQLite write is debounced ([`HEADLESS_SAVE_INTERVAL`]).
    fn save_headless(&self) {
        let Some(sidecar) = &self.sidecar else { return };
        let buffer = sidecar.buffer.lock().unwrap();
        if self.has_view() {
            return;
        }
        sidecar.save(&buffer);
        // The record now carries what the shell has printed, so `read_terminal` reports it as
        // the terminal's present state — the same answer a mounted terminal gives.
        self.dirty.store(false, Ordering::Relaxed);
    }

    /// A pane TOOK THE TERMINAL OVER: make the last sidecar save, then hand ownership to the
    /// renderer — both under the buffer lock, so a concurrent pump save can land neither in the
    /// middle of the flip nor after it. From here the renderer's debounced `persist_scrollback`
    /// is the only writer, exactly as for a terminal that was mounted from the start.
    fn hand_over_to_view(&self) {
        match &self.sidecar {
            Some(sidecar) => {
                let buffer = sidecar.buffer.lock().unwrap();
                sidecar.save(&buffer);
                self.dirty.store(false, Ordering::Relaxed);
                self.has_view.store(true, Ordering::Relaxed);
            }
            None => self.has_view.store(true, Ordering::Relaxed),
        }
    }
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
    /// The instant of the LAST PTY I/O — updated on every output chunk (the pump) and on every
    /// keystroke/write ([`PtyManager::write`]). Its elapsed time is the "inactivité I/O du PTY"
    /// signal the idle-session UNLOAD (v2.5.5) uses as its inactivity THRESHOLD: a session with
    /// no output nor keystroke for N minutes is a candidate to unload (gated first by the
    /// `agent_activity == Idle` safety check). Shared with the pump thread by `Arc`.
    last_io: Arc<Mutex<Instant>>,
    /// WHO owns this terminal's scrollback (and therefore whether its output is streamed or
    /// persisted here) — see [`ScrollbackOwner`]. Holds:
    ///
    /// * `has_view` — whether a RENDERER VIEW is wired to this PTY, i.e. whether anyone consumes
    ///   the [`Event::TerminalOutput`] frames and ACKS them back. `true` for a PTY the renderer
    ///   spawned by mounting its pane, `false` for one
    ///   [`spawn_headless`](PtyManager::spawn_headless) opened for an MCP `create_terminal` with
    ///   no pane on screen, until a mount [`attaches`](PtyManager::attach_view). It gates the
    ///   pump's EMIT because the flow quota is credited by the renderer alone: with no view,
    ///   every frame crosses the pipe to be dropped by a renderer that has no instance for that
    ///   id, so its bytes are never acked — the backlog would climb to `HIGH_WATER` and pause the
    ///   reader FOREVER, hanging the very shell an agent opened. A view-less terminal therefore
    ///   emits nothing and reserves nothing; its bytes reach the VT screen through the feeder and
    ///   the record through the sidecar's own save.
    /// * `dirty` — whether OUTPUT has arrived since this terminal's scrollback was last saved,
    ///   i.e. whether `terminals.scrollback` is BEHIND the shell right now. Both halves of that
    ///   question cross the sidecar (the pump sees the output; the save is either its own or the
    ///   `persist_scrollback` db-command), so it holds the EXACT bit instead of guessing from a
    ///   timestamp. Set by the pump on every chunk, cleared by whichever save happens.
    /// * `sidecar` — the view-less persistence itself.
    ///
    /// Shared with the pump thread by `Arc`.
    scrollback: ScrollbackOwner,
}

/// The live interactive-PTY registry + pump driver. The [`super::Runtime`] holds one;
/// the pipe dispatch calls [`spawn`](PtyManager::spawn) / [`write`](PtyManager::write)
/// / [`resize`](PtyManager::resize) / [`close`](PtyManager::close) /
/// [`ack`](PtyManager::ack) on it.
pub struct PtyManager {
    /// The SHARED pool, so a terminal with no renderer view can have its scrollback persisted
    /// HERE (see [`HeadlessScrollback`]) — the sidecar owns that buffer for exactly as long as
    /// no pane does.
    db: Arc<Db>,
    terminals: Mutex<HashMap<u32, LiveTerminal>>,
    /// record-id -> live pty id. The sidecar registers this join itself when a PTY is
    /// spawned WITH a `record_id` (the v1 `register_terminal_pty`), so the MCP terminal
    /// tools can resolve a record to a LIVE shell synchronously: `send_to_terminal`
    /// returns `invalid_state` when no PTY is live (parity), and `list_terminals`
    /// reports the true `live` bit. Wrapped in its OWN `Arc<Mutex>` so the MCP
    /// dispatcher (a separate thread) shares the SAME registry.
    live_records: Arc<Mutex<HashMap<String, u32>>>,
    /// record-id -> the command line to type into that record's shell the moment its PTY spawns
    /// (the v1 core-host `PendingTerminalCommands`). `create_terminal` over MCP accepts a
    /// `command`, but the record it creates has NO shell yet — the renderer owns terminal
    /// mounting, and the PTY only exists once it mounts the pane. So the command waits HERE and
    /// [`spawn`](PtyManager::spawn) drains it. Kept in the PTY manager rather than in a policy
    /// module (unlike the agent [`resume parks`](super::resume_parks)) because it carries no
    /// policy at all: it is just bytes to type into a record's shell as soon as one exists.
    opening_commands: Mutex<HashMap<String, String>>,
}

impl PtyManager {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            terminals: Mutex::new(HashMap::new()),
            live_records: Arc::new(Mutex::new(HashMap::new())),
            opening_commands: Mutex::new(HashMap::new()),
        }
    }

    /// PARK the command a terminal was OPENED with, to be typed into its shell as soon as that
    /// shell exists. The record is brand new when this is called (`create_terminal` just wrote
    /// it), so no PTY can be live for it yet — the only consumer is the upcoming
    /// [`spawn`](PtyManager::spawn). Dropped un-run if the terminal is closed before it ever
    /// mounts ([`close_record`](PtyManager::close_record)).
    pub fn park_opening_command(&self, record_id: &str, command: &str) {
        if record_id.is_empty() {
            return;
        }
        self.opening_commands
            .lock()
            .unwrap()
            .insert(record_id.to_string(), command.to_string());
    }

    /// Number of live terminals currently registered.
    pub fn live_count(&self) -> usize {
        self.terminals.lock().unwrap().len()
    }

    /// Whether a terminal id is live.
    pub fn is_live(&self, terminal_id: u32) -> bool {
        self.terminals.lock().unwrap().contains_key(&terminal_id)
    }

    /// Whether a live PTY is currently registered for a terminal RECORD id (the persistent
    /// `terminals.id`). Used by the lazy-resume park path to inject immediately into an
    /// already-spawned terminal (vs. waiting for its next spawn).
    pub fn is_live_record(&self, record_id: &str) -> bool {
        self.live_records.lock().unwrap().contains_key(record_id)
    }

    /// Whether the persisted `terminals.scrollback` for `record_id` is BEHIND its live shell —
    /// output has arrived that the renderer has not saved. `false` for a record with no live PTY:
    /// nothing is producing, so the buffer is not falling further behind (the caller still knows
    /// there is no shell, from `live`).
    pub fn scrollback_is_behind(&self, record_id: &str) -> bool {
        self.with_live_terminal(record_id, |t| t.scrollback.dirty.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// The renderer just SAVED this record's scrollback: the persisted buffer is level with the
    /// shell again. Called from the `persist_scrollback` db-command — the renderer's half of the
    /// save, the only one for a terminal whose pane is mounted (a view-less one saves itself,
    /// see [`ScrollbackOwner::save_headless`]).
    pub fn mark_scrollback_persisted(&self, record_id: &str) {
        self.with_live_terminal(record_id, |t| {
            t.scrollback.dirty.store(false, Ordering::Relaxed)
        });
    }

    /// Run `f` against the LIVE terminal backing a record id, if there is one. Resolves the record
    /// through `live_records` (the same join the terminal tools use) under both registry locks.
    fn with_live_terminal<T>(
        &self,
        record_id: &str,
        f: impl FnOnce(&LiveTerminal) -> T,
    ) -> Option<T> {
        let pty_id = self.live_records.lock().unwrap().get(record_id).copied()?;
        let terms = self.terminals.lock().unwrap();
        terms.get(&pty_id).map(f)
    }

    /// Spawn an interactive shell PTY of `cols`x`rows` FOR A RENDERER VIEW (the mount of a
    /// terminal pane), start its pump (binary output over `events` + OSC cwd/exec-state + exit),
    /// and return the live terminal id.
    ///
    /// `cwd` empty -> the sidecar's cwd; `record_id` empty -> no `NYX_TERMINAL_ID`
    /// export. `mcp_port` is the MCP HTTP port THIS instance's sidecar bound (`0` when the MCP
    /// server has not started yet); `> 0` exports it as `NYX_MCP_PORT` so the Claude plugin's
    /// SessionStart hook reaches THIS instance (review `01KXAXQN32GASMEB05PDW5BZSR`). A spawn
    /// failure is a readable error (never a half-spawn).
    ///
    /// ATTACHES rather than spawns when a PTY is ALREADY live for `record_id`: a terminal opened
    /// over MCP got its shell here without any pane (see
    /// [`spawn_headless`](Self::spawn_headless)), so the mount that comes later — the user finally
    /// switching to that project — must take over THAT shell. A second `Pty::spawn` would leave the
    /// first one orphaned behind the record and show the user a fresh, wrong prompt.
    pub fn spawn(
        &self,
        cols: u16,
        rows: u16,
        cwd: &str,
        record_id: &str,
        mcp_port: u16,
        events: EventSink,
    ) -> anyhow::Result<u32> {
        if let Some(live) = self.live_pty_for(record_id) {
            self.attach_view(live, cols, rows);
            return Ok(live);
        }
        self.spawn_pty(cols, rows, cwd, record_id, mcp_port, events, true)
    }

    /// Spawn a shell for `record_id` with NO renderer view behind it — the MCP `create_terminal`
    /// path. The terminal RECORD exists but nothing on screen shows it (its project is not the
    /// displayed one, or it is a loose terminal), and the renderer only ever mounts the displayed
    /// project's panes; without this the record would sit in the sidebar as an empty shell and
    /// every `send_to_terminal` would answer `invalid_state`, so an agent could only drive nyx in
    /// whatever project the human happened to be looking at.
    ///
    /// The shell is real from this instant: the parked opening command runs, `send_to_terminal`
    /// writes, `list_terminals` reports it live. It just emits no output frames until a pane
    /// [`attaches`](Self::attach_view) — see [`LiveTerminal::has_view`].
    ///
    /// Sized at [`HEADLESS_SIZE`]: no pane means no geometry to fit, and the attaching mount
    /// resizes it to the real one.
    pub fn spawn_headless(
        &self,
        cwd: &str,
        record_id: &str,
        mcp_port: u16,
        events: EventSink,
    ) -> anyhow::Result<u32> {
        if let Some(live) = self.live_pty_for(record_id) {
            return Ok(live);
        }
        let (cols, rows) = HEADLESS_SIZE;
        self.spawn_pty(cols, rows, cwd, record_id, mcp_port, events, false)
    }

    /// The live pty id backing a RECORD id, or `None` for an empty/unbound record.
    fn live_pty_for(&self, record_id: &str) -> Option<u32> {
        if record_id.is_empty() {
            return None;
        }
        self.live_records.lock().unwrap().get(record_id).copied()
    }

    /// A renderer pane took over an ALREADY-live PTY (the mount of a terminal whose shell was
    /// opened headlessly over MCP): hand the scrollback back to the renderer, start emitting to
    /// it, and resize the shell to the pane.
    ///
    /// The scrollback hand-over is what keeps the ownership unambiguous in BOTH directions: the
    /// sidecar makes its final save and stops in one critical section
    /// ([`ScrollbackOwner::hand_over_to_view`]), so the pre-mount history is in the record and no
    /// second writer can clobber the renderer's saves afterwards.
    ///
    /// The flow quota is RESET rather than carried over: what the pump produced before the pane
    /// existed was never sent to anyone, so counting it would charge the new view for bytes it
    /// will never receive and could leave the reader paused with nobody able to credit it back.
    fn attach_view(&self, terminal_id: u32, cols: u16, rows: u16) {
        {
            let terms = self.terminals.lock().unwrap();
            let Some(t) = terms.get(&terminal_id) else {
                return;
            };
            t.scrollback.hand_over_to_view();
            let mut flow = t.flow.lock().unwrap();
            flow.in_flight = 0;
            if flow.paused {
                flow.paused = false;
                t.pause_gate.set_paused(false);
            }
        }
        let _ = self.resize(terminal_id, cols, rows);
    }

    /// The shared spawn body of [`spawn`](Self::spawn) / [`spawn_headless`](Self::spawn_headless);
    /// `has_view` says whether a renderer pane consumes (and acks) this PTY's output frames.
    #[allow(clippy::too_many_arguments)]
    fn spawn_pty(
        &self,
        cols: u16,
        rows: u16,
        cwd: &str,
        record_id: &str,
        mcp_port: u16,
        events: EventSink,
        has_view: bool,
    ) -> anyhow::Result<u32> {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let cwd_opt = (!cwd.is_empty()).then_some(cwd);
        let record_opt = (!record_id.is_empty()).then_some(record_id);
        let (pty, rx) = Pty::spawn(size, cwd_opt, record_opt, mcp_port)?;
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
        // Last-I/O clock (v2.5.5 unload): starts "now" at spawn — a fresh terminal is not idle.
        let last_io = Arc::new(Mutex::new(Instant::now()));
        // WHO owns the scrollback. A shell spawned WITHOUT a view carries the sidecar's own
        // persistence: the record it belongs to was created by the `create_terminal` that opened
        // this very shell, so its `scrollback` column is empty and accumulating from nothing
        // rewrites no history. Freshness starts level: nothing has been printed yet, so the
        // (empty) persisted scrollback is not behind anything.
        let scrollback = ScrollbackOwner {
            has_view: Arc::new(AtomicBool::new(has_view)),
            dirty: Arc::new(AtomicBool::new(false)),
            sidecar: (!has_view && !record_id.is_empty()).then(|| {
                Arc::new(HeadlessScrollback {
                    db: Arc::clone(&self.db),
                    record_id: record_id.to_string(),
                    buffer: Mutex::new(Vec::new()),
                })
            }),
        };

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
        let pump_last_io = Arc::clone(&last_io);
        let pump_scrollback = scrollback.clone();
        let pump = std::thread::Builder::new()
            .name(format!("nyx-sidecar-pty-pump-{terminal_id}"))
            .spawn(move || {
                run_pump(PumpParams {
                    terminal_id,
                    rx,
                    events,
                    flow: pump_flow,
                    gate: pump_gate,
                    pty: pump_pty,
                    feed_tx: pump_feed_tx,
                    last_io: pump_last_io,
                    scrollback: pump_scrollback,
                });
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
                last_io,
                scrollback,
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
            // OPENING COMMAND: a terminal opened over MCP with a `command` parked it when it had
            // no shell to run it in. Its shell exists NOW, so type the line in — the same write a
            // keystroke takes, `command` + a carriage return, so the shell executes it and stays
            // interactive. Drained one-shot: re-opening the terminal later gives a bare shell.
            let opening = self.opening_commands.lock().unwrap().remove(record_id);
            if let Some(command) = opening {
                let mut bytes = command.into_bytes();
                bytes.push(b'\r');
                let _ = self.write(terminal_id, &bytes);
            }
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

    /// Close a terminal by its RECORD id (the MCP `close_terminal` live-PTY half). Also drops any
    /// opening command still waiting for a shell, so closing a terminal before it ever mounted
    /// cannot leave a command that a later record with the same id would inherit.
    pub fn close_record(&self, record_id: &str) {
        self.opening_commands.lock().unwrap().remove(record_id);
        let pty_id = self.live_records.lock().unwrap().get(record_id).copied();
        if let Some(id) = pty_id {
            self.close(id);
        }
    }

    /// Write raw bytes to a live PTY (keystrokes / paste). Clones the writer handle
    /// under the registry lock then writes WITHOUT it, so a stuck child stalls only
    /// its own pty (parity with the v1 `PtyManager::write_to`). Unknown id -> error.
    pub fn write(&self, terminal_id: u32, data: &[u8]) -> anyhow::Result<()> {
        // Clone the pty + last-I/O `Arc`s out from under the registry lock, then grab the writer
        // handle WITHOUT the registry lock so a stuck child stalls only its own pty.
        let (pty, last_io) = {
            let terms = self.terminals.lock().unwrap();
            let t = terms
                .get(&terminal_id)
                .ok_or_else(|| anyhow::anyhow!("unknown terminal {terminal_id}"))?;
            (Arc::clone(&t.pty), Arc::clone(&t.last_io))
        };
        // A keystroke/paste/inject is I/O too — refresh the idle clock so a terminal the user is
        // typing into (or one we just injected a resume line into) is never seen as idle.
        *last_io.lock().unwrap() = Instant::now();
        let writer = pty.lock().unwrap().writer_handle();
        let mut w = writer.lock().unwrap();
        use std::io::Write as _;
        w.write_all(data)?;
        w.flush()?;
        Ok(())
    }

    /// The per-record PTY INACTIVITY durations (v2.5.5 unload): for every live terminal that
    /// carries a persistent RECORD id, `(record_id, time since its last PTY I/O)`. The idle-
    /// session unload sweep reads this as the "pas de frappe ni d'output depuis N min" threshold
    /// signal (gated first by `agent_activity == Idle`). Record-less terminals are omitted (they
    /// cannot be correlated back to a resumable session).
    pub fn agent_idle_durations(&self) -> Vec<(String, Duration)> {
        let terms = self.terminals.lock().unwrap();
        terms
            .values()
            .filter_map(|t| {
                let rec = t.record_id.clone()?;
                let idle_for = t.last_io.lock().unwrap().elapsed();
                Some((rec, idle_for))
            })
            .collect()
    }

    /// The OS BUSY bit of a live terminal by its persistent RECORD id (v2.5.5 review T1):
    /// `true` while a command runs in its FOREGROUND process group, `false` at the idle
    /// prompt. Resolves the record -> live pty -> [`nyx_core::pty::Pty::is_busy`]
    /// (`foreground_pgid != shell_pid`). Returns `false` (not busy) when no live PTY backs
    /// the record OR the OS signal is unavailable (non-Unix / master closed) — a terminal
    /// with no derivable foreground group is not running anything (parity with the v1 poll's
    /// null-is-idle treatment). This is the LIVE, non-persisted running source that
    /// `apply_terminal_live_overlay` overlays onto the runtime snapshot's terminal LINE.
    pub fn is_busy_record(&self, record_id: &str) -> bool {
        let Some(pty) = self.pty_for_record(record_id) else {
            return false;
        };
        let busy = pty.lock().unwrap().is_busy();
        busy.unwrap_or(false)
    }

    /// Resolve a persistent RECORD id to its LIVE pty handle (record → live-pty join → registry),
    /// cloning the `Arc` out from under BOTH registry locks so every per-record reader below does
    /// its (possibly blocking) work WITHOUT holding them. `None` when no live PTY backs the record.
    /// The single-record counterpart of the `busy_by_record` / `shell_pids_by_record` batch joins.
    fn pty_for_record(&self, record_id: &str) -> Option<Arc<Mutex<Pty>>> {
        let terms = self.terminals.lock().unwrap();
        let pty_id = self.live_records.lock().unwrap().get(record_id).copied()?;
        Some(Arc::clone(&terms.get(&pty_id)?.pty))
    }

    /// The OS busy bit of EVERY live terminal that carries a persistent RECORD id, as
    /// `(record_id, busy)`. This is the per-tick sample the busy poll
    /// ([`super::busy_poll`]) diffs to emit `terminal://busy-state` only on a TRANSITION
    /// (parity with the v1 `BusyStatePoller` sweep). Record-less terminals are omitted (they
    /// cannot be correlated back to a snapshot line). Never errors: a terminal whose busy bit
    /// cannot be derived reports `false` (idle).
    pub fn busy_by_record(&self) -> Vec<(String, bool)> {
        // Snapshot the (record -> pty) joins first, then read each `is_busy` WITHOUT holding
        // the registry lock across the (cheap) tcgetpgrp syscalls.
        let joins: Vec<(String, Arc<Mutex<Pty>>)> = {
            let terms = self.terminals.lock().unwrap();
            self.live_records
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(rec, pty_id)| {
                    terms.get(pty_id).map(|t| (rec.clone(), Arc::clone(&t.pty)))
                })
                .collect()
        };
        joins
            .into_iter()
            .map(|(rec, pty)| {
                let busy = pty.lock().unwrap().is_busy().unwrap_or(false);
                (rec, busy)
            })
            .collect()
    }

    /// The SHELL (root) pid of every live terminal that carries a persistent RECORD id, as
    /// `(record_id, shell_pid)` (FEEDBACK #28 / v2.5.6). This is the per-tick input the stats poll
    /// ([`super::stats_poll`]) feeds to [`nyx_core::proc_stats::ProcStats::tree_stats_batch`] to
    /// sum each terminal's PROCESS-TREE CPU%/RAM (the shell + every descendant). A terminal whose
    /// shell pid is unavailable (already exited, or the platform did not report one) is omitted —
    /// it consumes nothing to report. Mirrors the [`busy_by_record`](Self::busy_by_record) sample
    /// shape (record id → live PTY), reading the shell pid instead of the busy bit.
    pub fn shell_pids_by_record(&self) -> Vec<(String, u32)> {
        // Snapshot the (record -> pty) joins first, then read each `shell_pid` WITHOUT holding the
        // registry lock (parity with `busy_by_record`).
        let joins: Vec<(String, Arc<Mutex<Pty>>)> = {
            let terms = self.terminals.lock().unwrap();
            self.live_records
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(rec, pty_id)| {
                    terms.get(pty_id).map(|t| (rec.clone(), Arc::clone(&t.pty)))
                })
                .collect()
        };
        joins
            .into_iter()
            .filter_map(|(rec, pty)| {
                let pid = pty.lock().unwrap().shell_pid()?;
                Some((rec, pid))
            })
            .collect()
    }

    /// The FOREGROUND program name (`comm`) of a live terminal by its persistent RECORD id
    /// (v2.5.5 review T3): `/proc/<foreground_pgid>/comm` — the running program the shell handed
    /// the terminal to (`claude` while an agent runs, the shell name at the prompt). This is the
    /// input the provider-agnostic `AgentRegistry::detect` resolves to an `agent_kind` for PASSIVE
    /// agent adoption. `None` when no live PTY backs the record, on a non-Linux platform (no
    /// `/proc`), or when the foreground cannot be read (e.g. the master already closed on exit) —
    /// so the caller simply detects no agent. Reuses the SAME `read_terminal_info` foreground read
    /// the auto-label path uses (its `.1` is the foreground `comm`).
    pub fn foreground_comm_record(&self, record_id: &str) -> Option<String> {
        let pty = self.pty_for_record(record_id)?;
        let pty = pty.lock().unwrap();
        read_terminal_info(pty.shell_pid(), foreground_pgid(&pty)).1
    }

    /// Whether the terminal `record_id` currently hosts a LIVE agent PROCESS in its PTY tree
    /// (PRD `01KY9PM1GK30FPVZVWQ3A7ZMJH` — the real liveness signal). Resolves the record → live
    /// pty → shell pid, then asks [`nyx_core::agent_liveness::agent_alive_in_tree`] whether the
    /// process tree rooted at that shell pid contains a process a registered agent adapter
    /// recognizes (`claude`, extensibly `codex`/`opencode`). CROSS-PLATFORM (a `sysinfo` PPID
    /// walk, not `/proc`).
    ///
    /// Returns `false` when NO live PTY backs the record (a bare/unloaded/absent terminal — no
    /// hosted agent) OR the shell pid is unavailable, so the lazy-resume gate treats such a
    /// terminal as resumable. The point: a `record` whose PTY still runs a live `claude` (Idle
    /// BETWEEN turns, or Working/Waiting) reads `true` here — distinguishing it from a crashed
    /// agent whose stale `Idle` activity looks identical, which reads `false`.
    pub fn agent_alive_in_record(&self, record_id: &str, registry: &AgentRegistry) -> bool {
        let Some(shell_pid) = self.shell_pid_for_record(record_id) else {
            return false;
        };
        nyx_core::agent_liveness::agent_alive_in_tree(shell_pid, registry)
    }

    /// The SHELL (root) pid of a single live terminal by its persistent RECORD id, or `None` when
    /// no live PTY backs the record (or the platform reported no pid). The single-record analogue
    /// of [`shell_pids_by_record`](Self::shell_pids_by_record), used by the liveness probe to root
    /// its process-tree walk.
    pub fn shell_pid_for_record(&self, record_id: &str) -> Option<u32> {
        let pty = self.pty_for_record(record_id)?;
        let pid = pty.lock().unwrap().shell_pid();
        pid
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
            // Kill the session-leader shell and make the kill STICK (`kill_and_reap`: SIGHUP,
            // resume the gate so a paused reader can loop and hit EOF, then SIGKILL after the
            // grace) BEFORE joining the pump. A bare `kill` is one SIGHUP an interactive shell
            // may not honour — and the pump only exits when the master EOFs, i.e. when the shell
            // dies, so the join below would block FOREVER on the sidecar's REQUEST LOOP: the pipe
            // stops answering and the whole app freezes. `Drop for Pty` runs the same escalation,
            // but it can never rescue us here — the `Pty` is behind an `Arc` the pump thread we
            // are joining also holds, so `Drop` cannot run until that join returns.
            live.pty.lock().unwrap().kill_and_reap(SHELL_KILL_GRACE);
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

/// The channels + shared handles ONE pump thread drives, grouped into a single params struct so
/// [`run_pump`] takes one argument instead of eight positional ones (`clippy::too_many_arguments`
/// — the `last_io` idle clock added in v2.5.5 pushed the old flat signature to 8/7). Built at
/// [`PtyManager::spawn`] and moved wholesale into the pump thread.
struct PumpParams {
    /// The live pty id (the wire terminal id all emitted frames carry).
    terminal_id: u32,
    /// The core reader channel: raw PTY chunks arrive here until the child exits / master closes.
    rx: Receiver<Vec<u8>>,
    /// The unsolicited-event sink the coalesced binary output / OSC state / exit ride.
    events: EventSink,
    /// The per-terminal flow-control accounting (in-flight bytes + pause state).
    flow: Arc<Mutex<FlowState>>,
    /// The lossless pause-gate the pump pauses/resumes for OS backpressure.
    gate: PauseGate,
    /// The live pty handle (reaped for the exit code on disconnect).
    pty: Arc<Mutex<Pty>>,
    /// The O(1) hand-off into the headless-VT feeder thread.
    feed_tx: Sender<Vec<u8>>,
    /// The last-PTY-I/O clock (v2.5.5 unload): refreshed on every output chunk.
    last_io: Arc<Mutex<Instant>>,
    /// WHO owns the scrollback ([`ScrollbackOwner`]): the freshness bit the pump RAISES on every
    /// chunk, and — while no pane is attached — the sidecar's own accumulation + save. Its
    /// `has_view` is read at each flush, so a pane that attaches mid-stream starts receiving from
    /// its next flush on.
    scrollback: ScrollbackOwner,
}

/// The pump body (ported from the v1 napi `run_pump`): leading-edge coalescing, the
/// OSC 7 / OSC 133 scan with a tail-carry across chunk boundaries, the flow-control
/// reserve + pause-gate, and the exit reap. Runs until the reader channel disconnects
/// (child exit / master close).
fn run_pump(params: PumpParams) {
    let PumpParams {
        terminal_id,
        rx,
        events,
        flow,
        gate,
        pty,
        feed_tx,
        last_io,
        scrollback,
    } = params;
    let mut pending: Vec<u8> = Vec::new();
    let mut last_flush = Instant::now();
    let mut last_save = Instant::now();
    let mut osc133_tail: Vec<u8> = Vec::new();
    let mut exec_sm = osc133::ExecStateMachine::new();

    loop {
        // LEADING-EDGE coalescing, identical strategy to the v1 napi pump:
        //   - `pending` EMPTY: BLOCK on `recv()` so the next byte flushes immediately.
        //   - `pending` NON-EMPTY: wait at most until the next scheduled flush so a
        //     steady flood coalesces on the 16ms cadence.
        let recv = if pending.is_empty() {
            // About to block until the shell prints again: save its scrollback NOW if it is
            // behind, so a one-shot output (the usual case an agent reads back) is in the record
            // immediately instead of waiting for a chunk that may never come.
            save_headless(&scrollback, &mut last_save, true);
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            let wait = FLUSH_INTERVAL.saturating_sub(last_flush.elapsed());
            rx.recv_timeout(wait)
        };
        match recv {
            Ok(chunk) => {
                // OUTPUT is I/O: refresh the idle clock so a terminal actively printing (Claude
                // streaming its answer, a build logging) is never seen as idle by the unload sweep.
                *last_io.lock().unwrap() = Instant::now();
                // The screen just moved on: whatever was last SAVED as this terminal's scrollback
                // is now behind it, until the next save says otherwise.
                scrollback.mark_dirty();
                // With NO pane attached the sidecar is this terminal's only scrollback writer, so
                // it keeps every chunk (the SAVE below is debounced, the accumulation is not — a
                // mount must never cost the terminal the bytes it printed just before the pane
                // appeared). A no-op once a pane owns the buffer.
                scrollback.record_headless(&chunk);
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
                    flush(terminal_id, &mut pending, &events, &flow, &gate, &scrollback);
                    last_flush = Instant::now();
                }
                // A shell that never stops printing never reaches the trailing save above, so
                // the debounced one keeps the record within one interval of the truth.
                save_headless(&scrollback, &mut last_save, false);
            }
            Err(RecvTimeoutError::Timeout) => {
                flush(terminal_id, &mut pending, &events, &flow, &gate, &scrollback);
                last_flush = Instant::now();
                save_headless(&scrollback, &mut last_save, false);
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Child exited / master closed: flush the tail, save what it last printed (the
                // record is all that is left of it), reap the code, emit `pty-exit`, exit the
                // pump. The `feed_tx` clone drops here, so once close also drops the stored
                // sender the feeder disconnects and exits.
                flush(terminal_id, &mut pending, &events, &flow, &gate, &scrollback);
                save_headless(&scrollback, &mut last_save, true);
                let code = pty.lock().unwrap().wait();
                events.emit(Event::PtyExit { terminal_id, code });
                break;
            }
        }
    }
}

/// Save a VIEW-LESS terminal's accumulated scrollback into its record, at most once per
/// [`HEADLESS_SAVE_INTERVAL`] — the sidecar's twin of the renderer's debounced
/// `persist_scrollback`, so a flood costs the shared pool one bounded write per interval instead
/// of one per 16ms flush.
///
/// `force` bypasses the debounce for the two edges where waiting would leave the record wrong for
/// an unbounded time: the pump is about to BLOCK on an idle shell, and the child has EXITED.
/// A no-op for a terminal whose pane owns the buffer, and for one that has printed nothing since
/// its last save.
fn save_headless(scrollback: &ScrollbackOwner, last_save: &mut Instant, force: bool) {
    if scrollback.has_view() || !scrollback.is_dirty() {
        return;
    }
    if !force && last_save.elapsed() < HEADLESS_SAVE_INTERVAL {
        return;
    }
    scrollback.save_headless();
    *last_save = Instant::now();
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
///
/// With NO view attached (`has_view`), the buffer is DROPPED instead of emitted: the quota's only
/// creditor is the renderer, so sending to a pane that does not exist would consume the whole
/// high-water budget and then pause the reader with nobody able to ack it back — a headless shell
/// that hangs after its first half-megabyte of output. Nothing is lost by dropping it: the feeder
/// has already parsed the same bytes into the VT screen, and the pump has already handed them to
/// [`ScrollbackOwner::record_headless`] for the record.
fn flush(
    terminal_id: u32,
    pending: &mut Vec<u8>,
    events: &EventSink,
    flow: &Arc<Mutex<FlowState>>,
    gate: &PauseGate,
    scrollback: &ScrollbackOwner,
) {
    if pending.is_empty() {
        return;
    }
    if !scrollback.has_view() {
        pending.clear();
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

    /// A manager over a fresh on-disk migrated `Db` under a unique temp dir (the sidecar cannot
    /// use nyx-core's `#[cfg(test)]` `Db::in_memory`). The pool is what a view-less terminal's
    /// scrollback is persisted into; the tests below that never look at a record still need one.
    fn manager() -> PtyManager {
        let dir = std::env::temp_dir().join(format!(
            "nyx-pty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("nyx.db")).expect("open db");
        PtyManager::new(Arc::new(db))
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
        let mgr = manager();
        let id = mgr
            .spawn(80, 24, "", "", 0, discard_sink())
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
        let mgr = manager();
        let id = mgr
            .spawn(80, 24, "", "", 0, discard_sink())
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
        let mgr = manager();
        assert!(mgr.snapshot(999_999).is_none());
    }

    /// Poll `is_busy_record` until it equals `want` or we time out, returning whether it hit.
    #[cfg(unix)]
    fn busy_until(mgr: &PtyManager, record: &str, timeout: Duration, want: bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if mgr.is_busy_record(record) == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        mgr.is_busy_record(record) == want
    }

    /// v2.5.5 review T1 — the OS BUSY bit, end to end through a REAL PTY: a foreground command
    /// (`sleep`) run in a record-backed terminal makes `is_busy_record` report `true` (its
    /// foreground process group differs from the shell), and it returns to `false` once the
    /// command hands the terminal back to the shell prompt. `busy_by_record` reports the SAME
    /// record while busy. This is the "un process foreground rallume un point running, puis
    /// s'éteint quand il rend la main au shell" criterion at the sidecar source (the poll's
    /// sample). Reuses `nyx_core::pty::Pty::is_busy` (`foreground_pgid != shell_pid`).
    #[cfg(unix)]
    #[test]
    fn busy_record_flips_true_while_a_foreground_command_runs() {
        let mgr = manager();
        let record = "rec-busy";
        let id = mgr
            .spawn(80, 24, "", record, 0, discard_sink())
            .expect("spawn pty");
        // Run a foreground command for a moment; the shell puts it in its own process group and
        // hands it the terminal → busy. `exec 2>/dev/null` keeps the row text irrelevant.
        mgr.write(id, b"sleep 2\n").expect("write");
        assert!(
            busy_until(&mgr, record, Duration::from_secs(5), true),
            "a foreground `sleep` must make the terminal report BUSY"
        );
        // While busy, the poll's sweep sample carries this record as busy.
        assert!(
            mgr.busy_by_record().iter().any(|(r, b)| r == record && *b),
            "busy_by_record reports the busy record"
        );
        // Once `sleep` returns, the foreground group is the shell again → not busy.
        assert!(
            busy_until(&mgr, record, Duration::from_secs(6), false),
            "the dot must clear when the command hands the terminal back to the shell"
        );
        // An unknown record is never busy (no live PTY backs it).
        assert!(!mgr.is_busy_record("no-such-record"));
        mgr.close(id);
    }

    /// FEEDBACK #28 / v2.5.6 — the STATS sample through a REAL PTY: a record-backed live terminal
    /// is reported by `shell_pids_by_record` with a REAL shell pid, so the stats poll can sum its
    /// process tree. A record with no live PTY is absent from the sample.
    #[cfg(unix)]
    #[test]
    fn shell_pids_by_record_reports_live_record_backed_terminals() {
        let mgr = manager();
        let record = "rec-stats";
        let id = mgr
            .spawn(80, 24, "", record, 0, discard_sink())
            .expect("spawn pty");
        let sample = mgr.shell_pids_by_record();
        let entry = sample.iter().find(|(r, _)| r == record);
        let (_, pid) = entry.expect("the record-backed terminal appears in the stats sample");
        assert!(
            *pid > 0,
            "a live shell has a real pid the tree walk can root at"
        );
        // A record with no live PTY contributes nothing to the sample.
        assert!(!sample.iter().any(|(r, _)| r == "no-such-record"));
        mgr.close(id);
    }

    /// v2.5.5 review T3 — the PASSIVE-adoption plumbing through a REAL PTY: while a foreground
    /// command runs, `foreground_comm_record` returns its program name (`comm`), the input the
    /// provider-agnostic `AgentRegistry::detect` resolves to an `agent_kind`. Here the foreground
    /// is `sleep` (not an agent), so the read yields `"sleep"` and the default registry detects
    /// nothing — proving the wire (foreground `comm` → registry) works and stays quiet for a
    /// plain command. The POSITIVE, provider-agnostic detection is proven by the nyx-core
    /// mock-adapter unit test; here we also pin that the SAME registry WOULD light for a real
    /// agent name (`claude` → `claude_code`), so the only missing piece in production is the
    /// process actually being that agent.
    #[cfg(unix)]
    #[test]
    fn foreground_comm_feeds_provider_agnostic_detection() {
        use nyx_core::agent::AgentRegistry;

        let mgr = manager();
        let record = "rec-detect";
        let id = mgr
            .spawn(80, 24, "", record, 0, discard_sink())
            .expect("spawn pty");
        mgr.write(id, b"sleep 2\n").expect("write");
        // Wait until the foreground is the `sleep` child (its comm becomes readable).
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut comm = None;
        while Instant::now() < deadline {
            comm = mgr.foreground_comm_record(record);
            if comm.as_deref() == Some("sleep") {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(
            comm.as_deref(),
            Some("sleep"),
            "the foreground program name is read from the live PTY"
        );

        // The provider-agnostic registry: a plain command is NOT an agent (no passive icon)...
        let registry = AgentRegistry::default();
        assert!(
            registry.detect("sleep").is_none(),
            "a plain foreground command adopts no agent"
        );
        // ...but the SAME registry path WOULD resolve a real agent's foreground name — the wire is
        // keyed on the registry, not on any hardcoded provider.
        assert_eq!(
            registry.detect("claude").map(|a| a.kind()),
            Some("claude_code"),
            "the registry resolves a known agent's comm to its kind (no `if claude` in the wire)"
        );

        // An unknown record has no foreground comm (no live PTY).
        assert!(mgr.foreground_comm_record("no-such-record").is_none());
        mgr.close(id);
    }

    // --- close never blocks the caller ---------------------------------------------------

    /// [`PtyManager::close`] runs ON THE SIDECAR'S REQUEST LOOP (`close_terminal`, the idle-unload
    /// sweep, `close_all` at shutdown), so a close that blocks freezes the whole pipe — no reply
    /// to any request, ever.
    ///
    /// It CAN block: `close` joins the pump, the pump only exits when the PTY master EOFs, and the
    /// master only EOFs when the shell dies — while `portable-pty`'s Unix killer sends exactly one
    /// `SIGHUP`, which a shell is free to ignore. The `Drop for Pty` escalation cannot save the
    /// caller either: the `Pty` sits behind an `Arc` the very pump thread being joined also holds,
    /// so `Drop` cannot run until that join returns.
    ///
    /// `trap '' HUP` makes that state DETERMINISTIC — the shell explicitly ignores the hangup,
    /// which a freshly-spawned interactive shell does by accident whenever the kill lands before it
    /// reaches the point where it honours one (the real-world flake). The close must still return,
    /// because it escalates to `SIGKILL` past the grace.
    #[cfg(unix)]
    #[test]
    fn close_returns_even_when_the_shell_ignores_sighup() {
        let mgr = Arc::new(manager());
        let id = mgr
            .spawn(80, 24, "", "rec-hup", 0, discard_sink())
            .expect("spawn pty");
        // Install the trap, then have the shell CONFIRM it is in place. The marker is assembled by
        // `printf` so the confirmation cannot be satisfied by the terminal's echo of the typed line
        // itself — only real post-`trap` output spells `HUP_TRAPPED`.
        mgr.write(id, b"trap '' HUP; printf 'HUP_%s' TRAPPED\n")
            .expect("write");
        let snap = snapshot_until(&mgr, id, Duration::from_secs(5), |s| {
            (0..s.main.rows).any(|r| s.main.row_text(r).contains("HUP_TRAPPED"))
        })
        .expect("a snapshot");
        assert!(
            (0..snap.main.rows).any(|r| snap.main.row_text(r).contains("HUP_TRAPPED")),
            "the shell must confirm the SIGHUP trap is installed before we close; got rows: {:?}",
            (0..snap.main.rows)
                .map(|r| snap.main.row_text(r))
                .collect::<Vec<_>>()
        );

        // Close from another thread so an unbounded `join` inside `close` FAILS this test with a
        // readable timeout instead of hanging the whole test binary.
        let (tx, rx) = std::sync::mpsc::channel();
        let closer = Arc::clone(&mgr);
        std::thread::spawn(move || {
            closer.close(id);
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(15)).expect(
            "close must RETURN (escalating past the ignored SIGHUP) instead of blocking the \
             request loop on the pump join",
        );
        assert!(!mgr.is_live(id), "the closed terminal left the registry");
    }

    // --- a shell with no pane behind it (H1) ----------------------------------------------

    /// A sink that KEEPS the framed bytes, so a test can ask what actually crossed the pipe.
    fn recording_sink() -> (EventSink, Arc<Mutex<Vec<u8>>>) {
        let wire = Arc::new(Mutex::new(Vec::new()));
        (EventSink::new(Box::new(Sink(Arc::clone(&wire)))), wire)
    }

    /// Whether the framed wire bytes carry `marker` anywhere. Output frames are BINARY
    /// MessagePack of the raw PTY bytes, so a marker the shell printed appears verbatim.
    fn wire_carries(wire: &Arc<Mutex<Vec<u8>>>, marker: &str) -> bool {
        last_subslice(&wire.lock().unwrap(), marker.as_bytes()).is_some()
    }

    /// A terminal opened over MCP gets its shell HERE, with no pane anywhere; the renderer's
    /// mount, whenever it comes, must take THAT shell over instead of starting a second one.
    /// A second `Pty::spawn` would strand the first shell — still running, still holding the
    /// agent's session — behind a record now pointing at a fresh, empty prompt.
    ///
    /// Also pins the resize the attach owes the pane: the headless shell was sized at
    /// [`HEADLESS_SIZE`] because nothing on screen dictated a geometry.
    #[cfg(unix)]
    #[test]
    fn a_mount_takes_over_the_headless_shell_instead_of_spawning_a_second() {
        let mgr = manager();
        let record = "rec-attach";
        let headless = mgr
            .spawn_headless("", record, 0, discard_sink())
            .expect("the MCP-opened shell");
        // What the renderer does when the user finally switches to that project.
        let mounted = mgr
            .spawn(120, 40, "", record, 0, discard_sink())
            .expect("the mount");
        assert_eq!(
            mounted, headless,
            "the mount must take over the shell already backing the record"
        );
        assert_eq!(
            mgr.live_count(),
            1,
            "one record, one shell — the mount must not leave an orphan running"
        );
        let snap = snapshot_until(&mgr, mounted, Duration::from_secs(5), |s| {
            (s.main.cols, s.main.rows) == (120, 40)
        })
        .expect("a snapshot");
        assert_eq!(
            (snap.main.cols, snap.main.rows),
            (120, 40),
            "the attach must resize the shell to the pane it is now shown in"
        );
        mgr.close(mounted);
    }

    /// A shell with NO view streams nothing over the pipe, and starts streaming the moment one
    /// attaches.
    ///
    /// This is not an optimisation: the flow quota is credited by the RENDERER alone
    /// ([`PtyManager::ack`]). Frames sent to a pane that does not exist are dropped by a renderer
    /// with no instance for that id, so their bytes are never acked — the backlog would climb to
    /// [`HIGH_WATER`] and pause the reader with nobody left to release it, hanging the very shell
    /// an agent opened as soon as it printed half a megabyte.
    ///
    /// The output is not lost to the sidecar: the marker is asserted PRESENT on the VT screen at
    /// the same moment it is absent from the wire, so the test cannot pass by the shell simply
    /// having printed nothing.
    #[cfg(unix)]
    #[test]
    fn a_shell_with_no_view_streams_nothing_until_a_pane_attaches() {
        let mgr = manager();
        let record = "rec-noview";
        let (sink, wire) = recording_sink();
        let id = mgr
            .spawn_headless("", record, 0, sink)
            .expect("the MCP-opened shell");

        // The shell PRINTS while nothing is watching: the marker reaches the VT screen…
        mgr.write(id, b"printf 'NOVIEW_%s' MARKER\n").expect("write");
        snapshot_until(&mgr, id, Duration::from_secs(5), |s| {
            (0..s.main.rows).any(|r| s.main.row_text(r).contains("NOVIEW_MARKER"))
        })
        .filter(|s| (0..s.main.rows).any(|r| s.main.row_text(r).contains("NOVIEW_MARKER")))
        .expect("the shell must have printed the marker (the feeder sees it)");
        // …and NOT the pipe: not one byte of a view-less terminal's output crosses it.
        assert!(
            !wire_carries(&wire, "NOVIEW"),
            "a shell with no pane must not stream (its bytes could never be acked back)"
        );

        // A pane attaches: from here the terminal streams like any other.
        mgr.spawn(80, 24, "", record, 0, discard_sink())
            .expect("the mount");
        mgr.write(id, b"printf 'ATTACHED_%s' MARKER\n")
            .expect("write");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !wire_carries(&wire, "ATTACHED_MARKER") {
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(
            wire_carries(&wire, "ATTACHED_MARKER"),
            "once a pane is attached the shell's output must reach the renderer"
        );
        mgr.close(id);
    }
}
