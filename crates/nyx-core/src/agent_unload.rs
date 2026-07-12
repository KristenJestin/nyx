//! IDLE agent-session UNLOAD policy (v2.5.5 — task `01KX8Z6T1J2AC65DHHZTCQ26Z4`).
//!
//! The PURE decision — "may this agent session be unloaded to free resources?" — kept
//! separate from the sidecar plumbing that closes the PTY, exactly as
//! [`crate::agent_resume::decide_resume`] keeps the resume verdict pure. This is where the
//! CENTRAL SAFETY INVARIANT lives and is unit-tested: **a session that is not `Idle` is
//! NEVER unloaded.**
//!
//! The signal it consumes was validated by the phase-4 spike
//! (`docs/spikes/v2.5.5-agent-activity-signal.md`): the runtime
//! [`Activity`](crate::agent_activity::Activity) is the reliable idle-vs-active
//! discriminator, and PTY I/O inactivity is the temporal threshold layered on top.
//!
//! # The three gates (in order)
//!
//! 1. **Safety gate — `Activity`.** `Working` (Claude is generating OR a tool/sub-agent is in
//!    flight — the "action en cours" exception) and `Waiting` (blocked on the user, e.g. a
//!    permission prompt the user still has to answer) both mean NEVER unload. Only `Idle` — a
//!    turn that has cleanly ended — passes. Because the activity signal is 0-stale (a long
//!    in-flight action keeps `Working` with no timer), this gate alone guarantees an active
//!    session is never unloaded.
//! 2. **Focus gate — `visited`.** The terminal the user is currently looking at is never
//!    unloaded (the "réveil sur ouverture" / never-yank-what-you-see rule).
//! 3. **Threshold gate — `idle_for`.** Only a session idle (no PTY output nor keystroke) for
//!    at least `threshold` is unloaded, so a session merely set aside for a moment survives.

use std::time::Duration;

use crate::agent_activity::Activity;

/// The default inactivity threshold before an idle, unvisited agent session is unloaded —
/// **15 minutes**, the value the spike recommends
/// (`docs/spikes/v2.5.5-agent-activity-signal.md`). Long enough not to yank a session just
/// set aside, short enough to reclaim the resources of a genuinely dormant one. Callers may
/// override it (the `unload_idle_sessions` db-command accepts a `thresholdSecs`).
pub const DEFAULT_UNLOAD_THRESHOLD_SECS: u64 = 15 * 60;

/// The verdict for ONE candidate: unload it, or keep it (with the reason, for logs/tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnloadDecision {
    /// Safe to unload: close the PTY, free the resources; the DB session stays `active` so a
    /// revisit resumes it via the lazy park+inject.
    Unload,
    /// Keep it loaded, for this reason.
    Keep(KeepReason),
}

impl UnloadDecision {
    /// Whether the verdict is to unload (the caller then closes the PTY).
    pub fn is_unload(self) -> bool {
        matches!(self, UnloadDecision::Unload)
    }
}

/// Why a candidate is KEPT (never unloaded). Ordered by the gate that stopped it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    /// `Activity::Working` — Claude is generating OR a tool/sub-agent is in flight. This is
    /// the CENTRAL invariant plus the "action en cours" exception in one: an active session
    /// is never unloaded.
    Active,
    /// `Activity::Waiting` — blocked on the user (a permission prompt / `AskUserQuestion`).
    /// Unloading would drop the prompt the user still has to answer, so it is kept.
    Waiting,
    /// The terminal is currently visited/focused — never unload what the user is looking at.
    Visited,
    /// Idle and unvisited, but not yet idle for `threshold` — still within the grace window.
    WithinThreshold,
}

/// One session weighed for unload: its runtime activity, whether it is currently visited, and
/// how long it has had NO PTY I/O.
#[derive(Debug, Clone)]
pub struct UnloadCandidate {
    /// The terminal RECORD id (`NYX_TERMINAL_ID`) — the key shared by the activity store and
    /// the resume parks.
    pub record_id: String,
    /// The live runtime activity (`Idle`/`Working`/`Waiting`) from the agent-activity store.
    pub activity: Activity,
    /// Whether the user is currently viewing this terminal (focused/visited).
    pub visited: bool,
    /// Time since the last PTY I/O (output or keystroke) for this terminal.
    pub idle_for: Duration,
}

/// Decide whether `candidate` may be unloaded, applying the three gates in order (safety →
/// focus → threshold). The FIRST gate that stops it wins and reports its [`KeepReason`]; only
/// a candidate that clears all three is [`UnloadDecision::Unload`].
///
/// The safety gate is checked FIRST and unconditionally, so the "a session ACTIVE n'est
/// JAMAIS unloadée" invariant holds regardless of focus or how long it has been idle.
pub fn decide_unload(candidate: &UnloadCandidate, threshold: Duration) -> UnloadDecision {
    // 1. SAFETY GATE — never unload a session that is not cleanly Idle. `Working` (generating
    //    or an action in flight) and `Waiting` (blocked on the user) are both kept. This is
    //    THE invariant; it is first and unconditional.
    match candidate.activity {
        Activity::Working => return UnloadDecision::Keep(KeepReason::Active),
        Activity::Waiting => return UnloadDecision::Keep(KeepReason::Waiting),
        Activity::Idle => {}
    }
    // 2. FOCUS GATE — never unload the terminal the user is currently looking at.
    if candidate.visited {
        return UnloadDecision::Keep(KeepReason::Visited);
    }
    // 3. THRESHOLD GATE — only past the inactivity window.
    if candidate.idle_for < threshold {
        return UnloadDecision::Keep(KeepReason::WithinThreshold);
    }
    UnloadDecision::Unload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(activity: Activity, visited: bool, idle_secs: u64) -> UnloadCandidate {
        UnloadCandidate {
            record_id: "rec".into(),
            activity,
            visited,
            idle_for: Duration::from_secs(idle_secs),
        }
    }

    fn threshold() -> Duration {
        Duration::from_secs(DEFAULT_UNLOAD_THRESHOLD_SECS)
    }

    /// THE HAPPY PATH: an idle, unvisited session past the threshold is unloaded.
    #[test]
    fn idle_unvisited_beyond_threshold_unloads() {
        let c = candidate(Activity::Idle, false, DEFAULT_UNLOAD_THRESHOLD_SECS + 60);
        assert_eq!(decide_unload(&c, threshold()), UnloadDecision::Unload);
        assert!(decide_unload(&c, threshold()).is_unload());
    }

    /// THE CENTRAL INVARIANT: a session that is generating (`Working`) is NEVER unloaded —
    /// even unvisited and idle-clock well past the threshold (the clock is I/O, which a
    /// generating-but-silent turn may not refresh, so the ACTIVITY gate is what protects it).
    #[test]
    fn working_is_never_unloaded_even_past_threshold() {
        let c = candidate(Activity::Working, false, DEFAULT_UNLOAD_THRESHOLD_SECS * 10);
        assert_eq!(
            decide_unload(&c, threshold()),
            UnloadDecision::Keep(KeepReason::Active),
            "an ACTIVE session must never be unloaded — the safety invariant"
        );
    }

    /// The "action en cours" exception: a session with a long tool/sub-agent in flight reads
    /// `Working` (0-stale), so it is kept even though it looks quiescent on the I/O clock.
    #[test]
    fn action_in_flight_working_is_kept() {
        let c = candidate(Activity::Working, false, DEFAULT_UNLOAD_THRESHOLD_SECS + 1);
        assert!(!decide_unload(&c, threshold()).is_unload());
    }

    /// A session BLOCKED on the user (`Waiting`) is never unloaded — the prompt must survive.
    #[test]
    fn waiting_is_never_unloaded() {
        let c = candidate(Activity::Waiting, false, DEFAULT_UNLOAD_THRESHOLD_SECS + 60);
        assert_eq!(
            decide_unload(&c, threshold()),
            UnloadDecision::Keep(KeepReason::Waiting)
        );
    }

    /// The currently-visited terminal is never unloaded, even idle past the threshold.
    #[test]
    fn visited_is_never_unloaded() {
        let c = candidate(Activity::Idle, true, DEFAULT_UNLOAD_THRESHOLD_SECS + 60);
        assert_eq!(
            decide_unload(&c, threshold()),
            UnloadDecision::Keep(KeepReason::Visited)
        );
    }

    /// An idle, unvisited session still WITHIN the threshold is kept (the grace window).
    #[test]
    fn idle_within_threshold_is_kept() {
        let c = candidate(Activity::Idle, false, 30);
        assert_eq!(
            decide_unload(&c, threshold()),
            UnloadDecision::Keep(KeepReason::WithinThreshold)
        );
    }

    /// Exactly AT the threshold counts as reached (>=), so the boundary unloads.
    #[test]
    fn at_threshold_boundary_unloads() {
        let c = candidate(Activity::Idle, false, DEFAULT_UNLOAD_THRESHOLD_SECS);
        assert!(decide_unload(&c, threshold()).is_unload());
    }
}
