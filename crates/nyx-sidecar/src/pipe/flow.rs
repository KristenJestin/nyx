//! Backpressure and per-terminal quotas for sidecar -> UI event flow (PDR-0007).
//!
//! Terminal output is high-volume and bursty. Without flow control a runaway
//! process (e.g. `yes`) would let the sidecar enqueue unbounded
//! [`Event::TerminalOutput`](super::wire::Event) frames faster than the UI can
//! drain them, growing memory without bound. The [`FlowController`] enforces a
//! **per-terminal byte quota** of in-flight (sent-but-unacked) output:
//!
//! * `try_reserve(terminal, len)` is called before emitting a chunk. If the
//!   terminal already has `quota` bytes in flight, the call is refused and the
//!   producer must PAUSE that terminal (apply backpressure) until the UI acks.
//! * `ack(terminal, len)` is called when the UI confirms it consumed `len` bytes,
//!   freeing that much of the quota so the terminal can RESUME.
//!
//! Quotas are independent per terminal, so one noisy terminal cannot starve the
//! others. This is the bounded-frames + backpressure contract; chunks are raw
//! binary frames, never JSON/base64 per chunk (explicit non-goal).

use std::collections::HashMap;

/// Default per-terminal in-flight byte quota (256 KiB). Beyond this the terminal
/// is paused until the UI acks.
pub const DEFAULT_TERMINAL_QUOTA: usize = 256 * 1024;

/// Tracks per-terminal in-flight (unacked) output bytes and enforces the quota.
pub struct FlowController {
    quota: usize,
    in_flight: HashMap<u32, usize>,
}

impl Default for FlowController {
    fn default() -> Self {
        Self::with_quota(DEFAULT_TERMINAL_QUOTA)
    }
}

impl FlowController {
    pub fn with_quota(quota: usize) -> Self {
        Self {
            quota,
            in_flight: HashMap::new(),
        }
    }

    /// Try to reserve `len` bytes of quota for `terminal` before sending a chunk.
    /// Returns `true` if reserved (send allowed); `false` if the terminal is at or
    /// over quota (the producer must pause it and wait for [`ack`]).
    ///
    /// A single chunk larger than the whole quota is allowed through only when the
    /// terminal currently has nothing in flight, so forward progress is always
    /// possible (we never wedge on an oversized first chunk).
    pub fn try_reserve(&mut self, terminal: u32, len: usize) -> bool {
        let cur = *self.in_flight.get(&terminal).unwrap_or(&0);
        if cur == 0 || cur + len <= self.quota {
            self.in_flight.insert(terminal, cur + len);
            true
        } else {
            false
        }
    }

    /// Acknowledge that the UI consumed `len` bytes of `terminal`'s output,
    /// freeing that much quota. Saturates at zero (never underflows).
    pub fn ack(&mut self, terminal: u32, len: usize) {
        if let Some(cur) = self.in_flight.get_mut(&terminal) {
            *cur = cur.saturating_sub(len);
            if *cur == 0 {
                self.in_flight.remove(&terminal);
            }
        }
    }

    /// Whether `terminal` is currently paused by backpressure (at/over quota).
    pub fn is_paused(&self, terminal: u32) -> bool {
        self.in_flight
            .get(&terminal)
            .is_some_and(|&c| c >= self.quota)
    }

    /// In-flight (unacked) bytes for `terminal`.
    pub fn in_flight(&self, terminal: u32) -> usize {
        *self.in_flight.get(&terminal).unwrap_or(&0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserves_until_quota_then_applies_backpressure() {
        let mut fc = FlowController::with_quota(100);
        assert!(fc.try_reserve(1, 60)); // 60 in flight
        assert!(fc.try_reserve(1, 40)); // 100 in flight (== quota)
                                        // Now at quota: a further reserve is refused -> producer must pause.
        assert!(!fc.try_reserve(1, 1));
        assert!(fc.is_paused(1));
    }

    #[test]
    fn ack_frees_quota_and_resumes() {
        let mut fc = FlowController::with_quota(100);
        assert!(fc.try_reserve(1, 100));
        assert!(!fc.try_reserve(1, 50)); // paused
        fc.ack(1, 80); // 20 in flight now
        assert!(!fc.is_paused(1));
        assert!(fc.try_reserve(1, 50)); // 70 in flight, allowed again
    }

    #[test]
    fn quotas_are_independent_per_terminal() {
        let mut fc = FlowController::with_quota(100);
        assert!(fc.try_reserve(1, 100));
        assert!(fc.is_paused(1));
        // Terminal 2 is unaffected by terminal 1's saturation.
        assert!(!fc.is_paused(2));
        assert!(fc.try_reserve(2, 100));
    }

    #[test]
    fn oversized_first_chunk_makes_progress() {
        let mut fc = FlowController::with_quota(100);
        // A 500-byte chunk on an idle terminal is allowed (forward progress).
        assert!(fc.try_reserve(7, 500));
        assert_eq!(fc.in_flight(7), 500);
        // But while it is unacked, further sends are refused.
        assert!(!fc.try_reserve(7, 1));
    }

    #[test]
    fn ack_saturates_at_zero() {
        let mut fc = FlowController::with_quota(100);
        fc.try_reserve(1, 30);
        fc.ack(1, 1000); // over-ack must not underflow
        assert_eq!(fc.in_flight(1), 0);
    }
}
