//! AGENT-SESSION RESUME PARKS (v2.5.5 — the LAZY resume execution).
//!
//! The sidecar-side re-port of the v1 core-host `ResumeParks` + the injection half of
//! `register_terminal_pty` (`apps/electron/src/core-host/{resume-parks,pty-manager}.ts`),
//! but driven LAZILY rather than eager-at-boot: the renderer consumes the
//! `workspace_restore_plan` read model (which REUSES `agent_resume::decide_resume` /
//! `workspace_resume_plan` — no new policy) and PUSHES each resumable session's
//! `claude --resume <id>` here, keyed by its terminal RECORD id. The command is then
//! injected EXACTLY ONCE into that terminal's shell:
//!
//!   * at the terminal's PTY SPAWN (the front mounts/opens the terminal → drain the park), or
//!   * IMMEDIATELY when the park is set if the terminal's PTY is already live (the deck
//!     eager-mounts every project terminal, so the spawn usually precedes the park at
//!     relaunch — handling both orderings makes the resume race-free).
//!
//! A one-shot `attempted` guard (per record id, per app run) makes the injection idempotent:
//! a re-park or a terminal re-open never injects twice. An injection whose PTY write FAILS
//! marks its session `resume_failed` (via `nyx_core::db::mark_session_resume_failed`) so the
//! resume is NOT retried in a loop (a clean `resume-failed` state — `decide_resume` then skips
//! it with `AlreadyResumeFailed`).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// One parked resume: the exact shell line to inject plus the session it resumes (so a failed
/// injection can mark THAT session `resume_failed`).
#[derive(Debug, Clone)]
pub struct ResumePark {
    /// The `agent_sessions.id` this resume concerns — the key `mark_session_resume_failed` needs.
    pub session_id: String,
    /// The exact command line to inject (`claude --resume <external_id>`), from the restore plan.
    pub command: String,
}

/// The in-memory resume-park registry (one per [`super::Runtime`], shared by `Arc`). Holds the
/// pending parks awaiting injection and the one-shot `attempted` guard.
#[derive(Default)]
pub struct ResumeParks {
    /// record_id → pending resume, not yet injected. Drained one-shot on the first injection.
    pending: Mutex<HashMap<String, ResumePark>>,
    /// record_ids a resume was already ATTEMPTED for this run — the one-shot guard: no re-park,
    /// no re-inject on a terminal re-open. Set the moment an injection is taken (success or not).
    attempted: Mutex<HashSet<String>>,
}

impl ResumeParks {
    pub fn new() -> Self {
        Self::default()
    }

    /// PARK a resume for `record_id`. A no-op when a resume was already attempted for this record
    /// this run (the one-shot guard) — so a renderer that re-pushes the restore plan (the resumed
    /// session is still `active`, so it re-appears) never re-injects. Overwrites any earlier
    /// pending park for the same record (the newest plan wins).
    pub fn park(&self, record_id: &str, park: ResumePark) {
        if record_id.is_empty() {
            return;
        }
        if self.attempted.lock().unwrap().contains(record_id) {
            return;
        }
        self.pending
            .lock()
            .unwrap()
            .insert(record_id.to_string(), park);
    }

    /// TAKE the pending resume for `record_id` for injection (one-shot): removes it from `pending`
    /// and records the record id as `attempted` so it is never taken again this run. Returns `None`
    /// when nothing is pending (or a resume was already attempted) — in which case the caller does
    /// NOT mark the record attempted, so a park that arrives LATER can still be injected.
    pub fn take_for_inject(&self, record_id: &str) -> Option<ResumePark> {
        if self.attempted.lock().unwrap().contains(record_id) {
            return None;
        }
        let park = self.pending.lock().unwrap().remove(record_id)?;
        self.attempted.lock().unwrap().insert(record_id.to_string());
        Some(park)
    }

    /// RE-ARM a record with a FRESH resume (v2.5.5 UNLOAD — review `01KX9CP96KVK6BTHY3ZSWY1E67`):
    /// clear the one-shot `attempted` flag, drop any stale pending park, and set `park` as the new
    /// pending resume. Called when an idle session is UNLOADED — the session stays `active`, so the
    /// sidecar itself re-parks its `claude --resume <id>` HERE (it knows the session id and rebuilds
    /// the command via the SAME `decide_resume` policy), and the next spawn's drain re-injects it,
    /// REUSING the phase-2 lazy path. Doing the re-park at unload (rather than only `forget`ting and
    /// hoping the renderer re-pushes) is what makes an IN-RUN revisit resume: the renderer pushes a
    /// workspace's plan at most ONCE per run, so nothing else would re-park within the same run.
    ///
    /// It intentionally BYPASSES the `attempted` guard `park` honours (the session was already
    /// resumed once before being unloaded), but the one-shot invariant is preserved: the injecting
    /// `take_for_inject` re-sets `attempted`, so exactly ONE inject happens per revisit. A no-op for
    /// an empty record id (a record-less terminal cannot be correlated back).
    pub fn rearm(&self, record_id: &str, park: ResumePark) {
        if record_id.is_empty() {
            return;
        }
        self.attempted.lock().unwrap().remove(record_id);
        self.pending
            .lock()
            .unwrap()
            .insert(record_id.to_string(), park);
    }

    /// Whether a resume is currently PENDING (parked, not yet injected) for `record_id`.
    #[cfg(test)]
    pub fn has_pending(&self, record_id: &str) -> bool {
        self.pending.lock().unwrap().contains_key(record_id)
    }

    /// Whether a resume was already ATTEMPTED for `record_id` this run (the one-shot guard).
    #[cfg(test)]
    pub fn was_attempted(&self, record_id: &str) -> bool {
        self.attempted.lock().unwrap().contains(record_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn park(session: &str, command: &str) -> ResumePark {
        ResumePark {
            session_id: session.into(),
            command: command.into(),
        }
    }

    /// A parked resume is taken exactly ONCE; the second take is `None` (one-shot drain), and the
    /// record is flagged `attempted` so a re-park is refused.
    #[test]
    fn take_is_one_shot_and_guards_reparking() {
        let parks = ResumeParks::new();
        parks.park("t1", park("s1", "claude --resume ext-1"));
        assert!(parks.has_pending("t1"));

        let taken = parks
            .take_for_inject("t1")
            .expect("first take drains the park");
        assert_eq!(taken.command, "claude --resume ext-1");
        assert_eq!(taken.session_id, "s1");
        assert!(!parks.has_pending("t1"), "the park was drained");
        assert!(
            parks.was_attempted("t1"),
            "the record is now flagged attempted"
        );

        // A second take yields nothing (one-shot).
        assert!(parks.take_for_inject("t1").is_none());
        // Re-parking the SAME record after an attempt is refused (idempotent — no double resume).
        parks.park("t1", park("s1", "claude --resume ext-1"));
        assert!(!parks.has_pending("t1"), "re-park after attempt is a no-op");
    }

    /// A `take_for_inject` on a record with NO pending park returns `None` WITHOUT flagging it
    /// attempted — so a park set LATER (the eager-mount race: spawn precedes the restore plan)
    /// can still be injected.
    #[test]
    fn take_without_pending_does_not_flag_attempted() {
        let parks = ResumeParks::new();
        assert!(parks.take_for_inject("t1").is_none());
        assert!(
            !parks.was_attempted("t1"),
            "an empty take never blocks a later park"
        );

        // A park set afterwards is still injectable.
        parks.park("t1", park("s1", "claude --resume ext-1"));
        assert!(parks.take_for_inject("t1").is_some());
    }

    /// UNLOAD → RESUME: after a resume was attempted (one-shot), unloading the session `rearm`s the
    /// record with a FRESH resume in ONE step — clearing the guard AND parking the new command — so
    /// the next spawn (an in-run revisit) re-injects it. Without `rearm` the `attempted` guard would
    /// refuse a plain re-park and the unloaded session could never come back within the same run.
    #[test]
    fn rearm_re_enables_resume_after_unload() {
        let parks = ResumeParks::new();
        parks.park("t1", park("s1", "claude --resume ext-1"));
        // First resume happens (the session is loaded then resumed).
        assert!(parks.take_for_inject("t1").is_some());
        assert!(parks.was_attempted("t1"));
        // A plain re-park while attempted is refused (the one-shot guard).
        parks.park("t1", park("s1", "claude --resume ext-1"));
        assert!(!parks.has_pending("t1"), "re-park refused while attempted");

        // UNLOAD re-arms the record → the guard is cleared AND the fresh resume is parked at once.
        parks.rearm("t1", park("s1", "claude --resume ext-1"));
        assert!(
            !parks.was_attempted("t1"),
            "rearm clears the attempted guard"
        );
        assert!(parks.has_pending("t1"), "rearm parks the fresh resume");

        // The revisit drains it exactly once (resume-au-retour works, one inject per visit).
        assert!(
            parks.take_for_inject("t1").is_some(),
            "after rearm the unloaded session can resume again"
        );
        assert!(
            parks.was_attempted("t1"),
            "the re-injection is one-shot again"
        );
    }

    /// An empty record id never parks (a record-less terminal cannot be correlated back).
    #[test]
    fn empty_record_id_never_parks() {
        let parks = ResumeParks::new();
        parks.park("", park("s1", "claude --resume ext-1"));
        assert!(!parks.has_pending(""));
    }
}
