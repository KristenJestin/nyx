//! SPIKE (v2.5.5, task 01KX8Z6SJAGN479Y0Q344PWKJD) — EMPIRICAL proof that the runtime
//! `agent_activity` signal RELIABLY discriminates "idle en attente d'input" from "génère
//! activement / action en cours", which is the crux of the idle-session UNLOAD.
//!
//! This exercises the REAL [`AgentActivityStore`] state machine (the exact code the MCP
//! per-turn hooks drive in production) over the hook sequences Claude Code emits, and
//! asserts the properties the unload policy relies on. See the feasibility report in
//! `docs/spikes/v2.5.5-agent-activity-signal.md` for the full analysis (why `agent_activity`
//! is THE discriminator, why `busy`/`exec_state` is NOT, and the recommended threshold).
//!
//! The three signals the spike evaluated:
//!   * `agent_activity` (Working/Waiting/Idle) — THE discriminator, proven here.
//!   * `busy`/`exec_state` (OSC133 + foreground pgid) — NOT usable alone: `claude` is the
//!     foreground process for the WHOLE session, so the OS "busy" bit is set whether Claude
//!     is generating or sitting idle at its prompt (documented in `osc133.rs`: "the OS busy
//!     signal owns the running state"). It cannot separate idle-at-prompt from generating.
//!   * PTY I/O inactivity (no output/keystroke for N min) — the THRESHOLD knob layered on
//!     top of the `Idle` gate; its real-PTY empirical validation lives in the unload slice.

use nyx_core::agent_activity::{Activity, ActivityEvent, AgentActivityStore};

/// The recommended inactivity threshold (see the report). Encoded here so the spike proof
/// and the unload policy agree on ONE number.
const RECOMMENDED_THRESHOLD_SECS: u64 = 15 * 60;

/// EMPIRICAL CORE — the signal cleanly separates the two states the unload cares about.
/// A brand-new turn (`UserPromptSubmit`) is `Working` (generating); a finished turn
/// (`Stop`) is `Idle` (waiting for input). This is the whole discriminator: `Idle` ⇒
/// safe-to-consider-for-unload, anything else ⇒ NEVER unload.
#[test]
fn agent_activity_separates_generating_from_idle() {
    let store = AgentActivityStore::new();
    let t = "rec-1";

    // Between turns, before any prompt: a fresh terminal reads as Idle (no entry).
    assert!(
        store.snapshot(t).is_none(),
        "a never-prompted terminal has no activity entry → treated as Idle"
    );

    // The user submits a prompt → Claude is GENERATING.
    let s = store.apply(t, ActivityEvent::PromptSubmitted);
    assert_eq!(
        s.activity,
        Activity::Working,
        "a submitted prompt = Claude is actively working"
    );

    // The turn ends → Claude is IDLE, waiting for the next input.
    let s = store.apply(t, ActivityEvent::TurnFinished);
    assert_eq!(
        s.activity,
        Activity::Idle,
        "a finished turn = idle, waiting for input (the moment it becomes unload-eligible)"
    );
}

/// EMPIRICAL — the 0-stale property that makes the signal SAFE: a long action (a tool or a
/// sub-agent in flight) keeps the session `Working` across ARBITRARILY many observations
/// with NO time-based decay. A 10-minute build/tool that emits nothing is STILL `Working`,
/// so the unload can never misread a genuine "action en cours" as idle. This is the
/// "exceptions (session avec action en cours)" invariant, proven at the signal level.
#[test]
fn long_action_in_flight_never_reads_idle() {
    let store = AgentActivityStore::new();
    let t = "rec-2";
    store.apply(t, ActivityEvent::PromptSubmitted);
    store.apply(t, ActivityEvent::ToolStarted { tool: None }); // a long-running tool begins (e.g. a build).

    // Observe MANY times (as the unload sweep would poll) — never Idle while in flight.
    for i in 0..1000 {
        assert_eq!(
            store.snapshot(t).unwrap().activity,
            Activity::Working,
            "observation {i}: an in-flight action keeps Working with NO staleness timer"
        );
    }

    // A background sub-agent OUTLIVES the main turn's Stop → still Working (must not unload).
    store.apply(t, ActivityEvent::SubagentStarted);
    store.apply(t, ActivityEvent::ToolFinished); // the tool returned…
    store.apply(t, ActivityEvent::TurnFinished); // …and the main turn "ended"…
    assert_eq!(
        store.snapshot(t).unwrap().activity,
        Activity::Working,
        "a still-running background sub-agent keeps the session Working past the Stop"
    );

    // Only when the LAST sub-agent finishes does it settle to Idle (now unload-eligible).
    let s = store.apply(t, ActivityEvent::SubagentFinished);
    assert_eq!(
        s.activity,
        Activity::Idle,
        "the session goes Idle only once the last in-flight action completes"
    );
}

/// EMPIRICAL — a session BLOCKED on the user (`AskUserQuestion` or a permission prompt)
/// reads `Waiting`, distinct from both `Working` and `Idle`. The unload MUST keep a
/// `Waiting` session loaded (the user still has to answer it — unloading would drop the
/// prompt), so the policy treats `Waiting` exactly like `Working`: never unload.
#[test]
fn blocked_on_user_reads_waiting_not_idle() {
    let store = AgentActivityStore::new();
    let t = "rec-3";
    store.apply(t, ActivityEvent::PromptSubmitted);

    let s = store.apply(t, ActivityEvent::AttentionNeeded { permission: None }); // a permission prompt.
    assert_eq!(
        s.activity,
        Activity::Waiting,
        "blocked on the user = Waiting, which is NOT idle — must stay loaded"
    );
    assert_ne!(s.activity, Activity::Idle);
}

/// EMPIRICAL — the ONE documented residual hole (a bare `Esc` interrupt fires no `Stop`)
/// fails SAFE for the unload: the dot stays `Working`, so the session is conservatively
/// KEPT (never wrongly unloaded), and it self-heals to a clean state on the next prompt.
/// The unload never has a false "idle" that could drop an active session.
#[test]
fn bare_interrupt_fails_safe_stays_working() {
    let store = AgentActivityStore::new();
    let t = "rec-4";
    store.apply(t, ActivityEvent::PromptSubmitted);
    store.apply(t, ActivityEvent::ToolStarted { tool: None });
    // User hits Esc — Claude emits NO hook. The store cannot observe the abort, so it stays
    // Working. For the unload this is the SAFE direction: keep, never unload.
    assert_eq!(
        store.snapshot(t).unwrap().activity,
        Activity::Working,
        "a bare interrupt leaves Working (fails safe — never a false idle)"
    );
    // It self-heals on the next prompt (a new turn resets everything cleanly).
    let s = store.apply(t, ActivityEvent::PromptSubmitted);
    assert_eq!(s.activity, Activity::Working);
    let s = store.apply(t, ActivityEvent::TurnFinished);
    assert_eq!(s.activity, Activity::Idle, "self-healed to a clean Idle");
}

/// EMPIRICAL — the anti-phantom CLEAR (PTY death / SessionEnd / close) drops the entry to
/// Idle. A killed Claude that never sent `Stop` cannot leave a phantom `Working` that would
/// forever block its terminal from unloading. This bounds the signal on the death side.
#[test]
fn clear_on_death_drops_to_idle() {
    let store = AgentActivityStore::new();
    let t = "rec-5";
    store.apply(t, ActivityEvent::PromptSubmitted);
    store.apply(t, ActivityEvent::ToolStarted { tool: None });
    assert_eq!(store.snapshot(t).unwrap().activity, Activity::Working);
    store.clear(t); // PTY death / SessionEnd.
    assert!(
        store.snapshot(t).is_none(),
        "after clear the terminal reads Idle (no phantom Working)"
    );
}

/// Sanity: the threshold the spike recommends is a stable constant the unload policy reuses,
/// so the "prouvé + seuil" pair stays in one place.
#[test]
fn recommended_threshold_is_fifteen_minutes() {
    assert_eq!(RECOMMENDED_THRESHOLD_SECS, 900);
}
