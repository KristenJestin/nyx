//! The sidecar's per-terminal CPU%/RAM STATS POLL (FEEDBACK #28 — the producer for
//! `terminal://stats`, v2.5.6 task `01KXBNX51EPQ69MVKYG4W960HB`).
//!
//! ## Why this exists
//!
//! [`nyx_core::proc_stats`] was ported to the v2 core (the PROCESS-TREE CPU%/RAM summation over a
//! shell + all its descendants — a `claude`, an `npm run dev`, a `cargo build`, …), but NOTHING
//! emitted it: the `terminal://stats` renderer channel had a consumer ([`use-terminal-stats`]) and
//! no producer. This module is that missing producer — the sidecar analog of [`super::busy_poll`],
//! re-câbled onto the pipe [`EventSink`].
//!
//! ## What it does
//!
//! [`run`] ticks every [`STATS_POLL_INTERVAL`], samples every record-backed live terminal's SHELL
//! pid ([`PtyManager::shell_pids_by_record`]) and sums each one's process tree in a SINGLE process-
//! table refresh ([`ProcStats::tree_stats_batch`]), then emits one [`Event::TerminalStats`] per
//! terminal keyed by the persistent RECORD id. The renderer folds each reading onto a per-terminal
//! map its rows read on hover (NON-inline — the CPU%/RAM never masks the terminal name).
//!
//! ## Why a REUSED `ProcStats` + a stream (not transition-gated)
//!
//! `sysinfo` computes CPU% as the work done BETWEEN two refreshes, so the owner must be kept ALIVE
//! across ticks (a fresh `System` per tick always reads 0%). The poll thread therefore owns ONE
//! [`ProcStats`] for its whole life. Unlike the busy poll (a boolean that only changes on a
//! foreground transition) CPU% moves constantly, so there is nothing to diff — we stream a fresh
//! reading for every live terminal each tick (the ~1.5s cadence the renderer hook documents).

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use nyx_core::proc_stats::{ProcStats, TreeStats};

use super::events::EventSink;
use super::pty::PtyManager;
use crate::pipe::wire::Event;

/// Cadence of the stats poll (~1.5s): frequent enough to feel live, cheap enough that the single
/// full process-table refresh per tick stays negligible. Matches the renderer hook's documented
/// cadence (`use-terminal-stats.tsx`).
pub const STATS_POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// Convert a core [`TreeStats`] CPU percentage to the wire's integer MILLI-percent (thousandths of
/// a percent, `cpu_pct * 1000` rounded), saturating into `u32`. Integer-on-the-wire keeps the
/// MessagePack envelope float-free (the `Event`/`Message` `Eq` derive and the TS `msgpack-mini`
/// subset both reject floats); the supervisor divides by 1000 back to the float `cpu_pct` the UI
/// formats. A negative/NaN CPU% (never expected) clamps to `0`.
pub fn cpu_milli(stats: &TreeStats) -> u32 {
    let scaled = (stats.cpu_pct * 1000.0).round();
    if scaled.is_finite() && scaled > 0.0 {
        // `f32::MAX` well exceeds `u32::MAX`, so clamp before the cast (a saturating cast would do
        // it too, but the explicit min keeps intent obvious).
        scaled.min(u32::MAX as f32) as u32
    } else {
        0
    }
}

/// One PASS: given a `sample` of `(record_id, shell_pid)`, refresh the shared [`ProcStats`] ONCE,
/// sum each terminal's process tree, and emit an [`Event::TerminalStats`] per record (in sample
/// order). Split from the IO ([`sweep`]) so it is unit-testable with a live `ProcStats` + a
/// capturing sink. An empty sample refreshes nothing and emits nothing.
pub fn sample_and_emit(sample: &[(String, u32)], stats: &mut ProcStats, events: &EventSink) {
    if sample.is_empty() {
        return;
    }
    let roots: Vec<u32> = sample.iter().map(|(_, pid)| *pid).collect();
    // ONE refresh + table build for the whole batch; one pure summation per root (FEEDBACK #28
    // perf), returned in the SAME order as `roots`.
    let readings = stats.tree_stats_batch(&roots);
    for ((record_id, _), reading) in sample.iter().zip(readings.iter()) {
        events.emit(Event::TerminalStats {
            record_id: record_id.clone(),
            cpu_milli: cpu_milli(reading),
            mem_bytes: reading.mem_bytes,
        });
    }
}

/// ONE sweep: sample every record-backed live terminal's shell pid and emit each tree's stats.
pub fn sweep(pty: &PtyManager, stats: &mut ProcStats, events: &EventSink) {
    let sample = pty.shell_pids_by_record();
    sample_and_emit(&sample, stats, events);
}

/// The stats-poll driver thread: sweep every [`STATS_POLL_INTERVAL`] for the life of the process,
/// over ONE reused [`ProcStats`] (so CPU% deltas are meaningful). The sidecar owns exactly one
/// (started in [`super::Runtime::attach_events`]); it exits when the process does (no teardown
/// handle — the process exit reaps it, parity with the busy poll).
fn run(pty: Arc<PtyManager>, events: EventSink) {
    let mut stats = ProcStats::new();
    loop {
        std::thread::sleep(STATS_POLL_INTERVAL);
        sweep(&pty, &mut stats, &events);
    }
}

/// The stats-poll runtime component: owns the poll thread. Built once at boot over the live
/// [`PtyManager`] + the pipe [`EventSink`], analogous to [`super::busy_poll::BusyPoll`].
pub struct StatsPoll {
    /// The poll thread handle (kept so the spawn never reads as leaked; the loop runs for the
    /// process lifetime, so it is not joined — the process exit reaps it).
    _handle: JoinHandle<()>,
}

impl StatsPoll {
    /// Start the stats poll over the live PTY manager, pushing `terminal://stats` readings on the
    /// pipe `events`. Returns the owning handle (held by the runtime).
    pub fn start(pty: Arc<PtyManager>, events: EventSink) -> Arc<Self> {
        let handle = std::thread::Builder::new()
            .name("nyx-sidecar-stats-poll".into())
            .spawn(move || run(pty, events))
            .expect("spawn stats-poll thread");
        Arc::new(Self { _handle: handle })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::codec::FrameDecoder;
    use crate::pipe::wire::{decode_message, Message};
    use std::sync::{Arc, Mutex};

    /// A `Write` that appends to a shared buffer so a test reads back the frames the sink pushed.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn captured_events(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<Event> {
        let bytes = buf.lock().unwrap().clone();
        let mut dec = FrameDecoder::new();
        dec.push(&bytes);
        let mut events = Vec::new();
        while let Some(payload) = dec.next_frame().expect("decode frame") {
            if let Ok(Message::Event(e)) = decode_message(&payload) {
                events.push(e);
            }
        }
        events
    }

    /// The milli-percent conversion: a 1.234% tree → 1234 milli; a 0% / degenerate reading → 0.
    #[test]
    fn cpu_milli_scales_and_clamps() {
        assert_eq!(
            cpu_milli(&TreeStats {
                cpu_pct: 1.234,
                mem_bytes: 0
            }),
            1234
        );
        assert_eq!(
            cpu_milli(&TreeStats {
                cpu_pct: 0.0,
                mem_bytes: 0
            }),
            0
        );
        // A parallel build over many cores can exceed 100% — represented verbatim (380% → 380000).
        assert_eq!(
            cpu_milli(&TreeStats {
                cpu_pct: 380.0,
                mem_bytes: 0
            }),
            380_000
        );
        // A negative/NaN (never expected) clamps to 0 rather than wrapping.
        assert_eq!(
            cpu_milli(&TreeStats {
                cpu_pct: -5.0,
                mem_bytes: 0
            }),
            0
        );
        assert_eq!(
            cpu_milli(&TreeStats {
                cpu_pct: f32::NAN,
                mem_bytes: 0
            }),
            0
        );
    }

    /// The wiring EMISSION→WIRE: a sample of `(record, shell_pid)` driven through `sample_and_emit`
    /// pushes one `TerminalStats` frame per record, keyed by record id, over a LIVE `ProcStats`.
    /// The current TEST process is a real root, so its tree has a non-zero RSS; a dead pid
    /// (`u32::MAX`) emits a ZERO reading at its slot — so a live terminal always gets a reading and
    /// the order is preserved. Proven without any live PTY.
    #[test]
    fn sample_and_emit_pushes_stats_per_record() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let events = EventSink::new(Box::new(SharedBuf(buf.clone())));
        let mut stats = ProcStats::new();

        let me = std::process::id();
        let dead = u32::MAX;
        sample_and_emit(
            &[("live".to_string(), me), ("dead".to_string(), dead)],
            &mut stats,
            &events,
        );

        let evs = captured_events(&buf);
        assert_eq!(evs.len(), 2, "one reading per sampled record, in order");
        match &evs[0] {
            Event::TerminalStats {
                record_id,
                mem_bytes,
                ..
            } => {
                assert_eq!(record_id, "live");
                assert!(*mem_bytes > 0, "the test process's own tree consumes RAM");
            }
            other => panic!("expected TerminalStats for the live record, got {other:?}"),
        }
        match &evs[1] {
            Event::TerminalStats {
                record_id,
                cpu_milli,
                mem_bytes,
            } => {
                assert_eq!(record_id, "dead");
                assert_eq!(
                    (*cpu_milli, *mem_bytes),
                    (0, 0),
                    "a gone shell reports zero"
                );
            }
            other => panic!("expected TerminalStats for the dead record, got {other:?}"),
        }
    }

    /// An empty sample refreshes nothing and emits nothing (the host skips the poll entirely when
    /// there are no terminals, but the pass must be a clean no-op).
    #[test]
    fn empty_sample_emits_nothing() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let events = EventSink::new(Box::new(SharedBuf(buf.clone())));
        let mut stats = ProcStats::new();
        sample_and_emit(&[], &mut stats, &events);
        assert!(captured_events(&buf).is_empty());
    }
}
