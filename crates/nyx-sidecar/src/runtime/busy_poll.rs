//! The sidecar's OS BUSY-STATE POLL (v2.5.5 review T1 — task `01KXB3EKYXRJBJMYRVK4E0NV16`).
//!
//! ## Why this exists
//!
//! In v1 a poll SAMPLED the foreground process group of every record-backed PTY on a fixed
//! cadence and emitted `terminal://busy-state` on a TRANSITION, so the UI's `running` dot lit
//! for ANY foreground command (a build, a `sleep`, a `claude` launched by hand) WITHOUT any
//! shell integration or agent hook — the OS-authoritative running signal that OSC 133 cannot
//! express (`exec_state` never carries `running`). In the v2 port this poll/emitter was lost:
//! nothing sampled [`nyx_core::pty::Pty::is_busy`], so a foreground process in a v2 terminal lit
//! nothing on its line. This module re-câbles that poll onto the sidecar's pipe [`EventSink`].
//!
//! ## What it does
//!
//! [`run`] ticks every [`BUSY_POLL_INTERVAL`], sampling [`PtyManager::busy_by_record`] and, via
//! a [`BusyTracker`] that diffs against the last EMITTED value, emitting an
//! [`Event::PtyBusyState`] ONLY on a transition (busy `false → true` or `true → false`). The
//! renderer folds that onto the terminal RECORD and — because `terminal://busy-state` is a
//! `RUNTIME_CHANGED_EVENTS` topic — re-pulls the workspace runtime snapshot, whose overlaid
//! `busy` bit (`apply_terminal_live_overlay`) then lights the tree LINE's `running` dot.
//!
//! `tcgetpgrp` (behind `is_busy`) is PULL-ONLY — there is no kernel notification when the
//! foreground group changes — so a bounded poll is the right shape (one cheap syscall per open
//! terminal per tick), exactly as in v1. It is INDEPENDENT of OSC 133: a restored terminal with
//! no foreground command samples idle by construction, so this source can never leave a phantom.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use super::events::EventSink;
use super::pty::PtyManager;
use crate::pipe::wire::Event;

/// Cadence of the busy-state poll — mirrors the v1 `BUSY_POLL_INTERVAL` (300ms): snappy for
/// the dot while keeping the per-tick syscall cost negligible (one `tcgetpgrp` per terminal).
pub const BUSY_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Tracks the last busy value EMITTED per persistent terminal RECORD id, so a
/// `terminal://busy-state` is emitted only on a CHANGE (mirrors the v1 `BusyStateTracker`). A
/// never-seen record is announced ONLY on its first BUSY — idle is the implicit default the
/// front already shows, so boot/restore never emits a redundant `false` for every idle terminal.
#[derive(Default)]
pub struct BusyTracker {
    last: HashMap<String, bool>,
}

impl BusyTracker {
    /// Whether `busy` differs from the last EMITTED value for `record_id` (recording the new
    /// value). A never-seen record returns `busy` (announce only the first BUSY); a known
    /// record returns whether the value flipped.
    pub fn changed(&mut self, record_id: &str, busy: bool) -> bool {
        match self.last.insert(record_id.to_string(), busy) {
            None => busy,
            Some(prev) => prev != busy,
        }
    }

    /// Drop tracked records that are no longer in `live` (a closed terminal) so the table does
    /// not grow unbounded across a long session. A closed terminal drops out of the snapshot
    /// entirely (its `pty-exit` tears down the row), so there is no last `false` to emit for it.
    pub fn forget_absent(&mut self, live: &[(String, bool)]) {
        self.last
            .retain(|rec, _| live.iter().any(|(r, _)| r == rec));
    }
}

/// One diff PASS over an already-collected `sample` of `(record_id, busy)`: forget records that
/// vanished, then emit an [`Event::PtyBusyState`] for every record whose busy value TRANSITIONED
/// since it was last emitted. Split from the IO ([`sweep`]) so the transition logic is unit-
/// testable with synthetic samples + a capturing sink (no live PTY needed).
pub fn diff_and_emit(sample: &[(String, bool)], tracker: &mut BusyTracker, events: &EventSink) {
    tracker.forget_absent(sample);
    for (record_id, busy) in sample {
        if tracker.changed(record_id, *busy) {
            events.emit(Event::PtyBusyState {
                record_id: record_id.clone(),
                busy: *busy,
            });
        }
    }
}

/// ONE sweep: sample the busy bit of every record-backed live PTY and emit each TRANSITION.
pub fn sweep(pty: &PtyManager, tracker: &mut BusyTracker, events: &EventSink) {
    let sample = pty.busy_by_record();
    diff_and_emit(&sample, tracker, events);
}

/// The busy-poll driver thread: sweep every [`BUSY_POLL_INTERVAL`] for the life of the process.
/// The sidecar owns exactly one (started in [`super::Runtime::attach_events`]); it exits when
/// the process does (no teardown handle, parity with the v1 loop). A tick is a bounded sweep.
fn run(pty: Arc<PtyManager>, events: EventSink) {
    let mut tracker = BusyTracker::default();
    loop {
        std::thread::sleep(BUSY_POLL_INTERVAL);
        sweep(&pty, &mut tracker, &events);
    }
}

/// The busy-poll runtime component: owns the poll thread. Built once at boot over the live
/// [`PtyManager`] + the pipe [`EventSink`], analogous to the git-watch runtime.
pub struct BusyPoll {
    /// The poll thread handle (kept so it never reads as an unused/leaked spawn; the loop runs
    /// for the process lifetime, so it is not joined — the process exit reaps it).
    _handle: JoinHandle<()>,
}

impl BusyPoll {
    /// Start the busy poll over the live PTY manager, pushing `terminal://busy-state`
    /// transitions on the pipe `events`. Returns the owning handle (held by the runtime).
    pub fn start(pty: Arc<PtyManager>, events: EventSink) -> Arc<Self> {
        let handle = std::thread::Builder::new()
            .name("nyx-sidecar-busy-poll".into())
            .spawn(move || run(pty, events))
            .expect("spawn busy-poll thread");
        Arc::new(Self { _handle: handle })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::codec::FrameDecoder;
    use crate::pipe::wire::{decode_message, Message};
    use std::sync::Mutex;

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

    /// The tracker announces a record's FIRST busy, both edges of a transition, and NEVER a
    /// no-change repeat — the exact "emit on transition only" contract of the v1 poll.
    #[test]
    fn tracker_emits_only_on_transition() {
        let mut t = BusyTracker::default();
        // First sight idle → NOT announced (idle is the implicit default the UI already shows).
        assert!(!t.changed("a", false));
        // First sight busy → announced.
        assert!(t.changed("b", true));
        // Repeat busy → no change.
        assert!(!t.changed("b", true));
        // b goes idle → transition.
        assert!(t.changed("b", false));
        // a goes busy → transition.
        assert!(t.changed("a", true));
        // a repeat busy → no change.
        assert!(!t.changed("a", true));
    }

    /// A closed terminal is forgotten, so a record REUSED later re-announces its first busy
    /// (the tracker never carries a stale value for a gone terminal).
    #[test]
    fn tracker_forgets_absent_records() {
        let mut t = BusyTracker::default();
        assert!(t.changed("a", true));
        // Sweep sees only "b" now → "a" is dropped.
        t.forget_absent(&[("b".to_string(), false)]);
        // "a" comes back busy: with the stale entry forgotten this counts as a first-busy again.
        assert!(t.changed("a", true));
    }

    /// The wiring EMISSION→MODEL: a synthetic sample driven through `diff_and_emit` pushes a
    /// `PtyBusyState` on the pipe for exactly the transitioned records, keyed by record id —
    /// the frame the renderer folds onto the terminal LINE (proven without a live PTY).
    #[test]
    fn diff_and_emit_pushes_busy_transitions_on_the_pipe() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let events = EventSink::new(Box::new(SharedBuf(buf.clone())));
        let mut tracker = BusyTracker::default();

        // Tick 1: t1 busy (foreground command), t2 idle → one emission for t1 only.
        diff_and_emit(
            &[("t1".to_string(), true), ("t2".to_string(), false)],
            &mut tracker,
            &events,
        );
        // Tick 2: t1 still busy (no change), t2 now busy → one emission for t2 only.
        diff_and_emit(
            &[("t1".to_string(), true), ("t2".to_string(), true)],
            &mut tracker,
            &events,
        );
        // Tick 3: t1 back to idle → one emission for t1=false.
        diff_and_emit(
            &[("t1".to_string(), false), ("t2".to_string(), true)],
            &mut tracker,
            &events,
        );

        let evs = captured_events(&buf);
        assert_eq!(
            evs,
            vec![
                Event::PtyBusyState {
                    record_id: "t1".into(),
                    busy: true
                },
                Event::PtyBusyState {
                    record_id: "t2".into(),
                    busy: true
                },
                Event::PtyBusyState {
                    record_id: "t1".into(),
                    busy: false
                },
            ],
            "exactly the transitions, in order, keyed by record id"
        );
    }
}
