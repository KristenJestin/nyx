//! The Spec ACTIVITY read model (v2.4) — the PRD detail's Activity fil, read from the
//! `domain_events` outbox (v2.1) and nothing else.
//!
//! ## Why this module exists
//!
//! The PRD detail shows a short, glanceable fil of a Spec's activity — the business mutations
//! of the Spec itself and of its tasks (its creation, its section edits, its lifecycle
//! transitions, the add/transition of its tasks, the dependency edges added). Those mutations
//! land, atomically, in the `domain_events` outbox from the SINGLE db authority every caller
//! funnels into ([`crate::db::create_spec`], [`crate::db::transition_spec`], … — see
//! [`crate::db::record_domain_event`]). This module assembles the ONE [`SpecActivity`] both
//! the UI bridge and (potentially) an MCP tool consume, so the UI and an agent read the SAME
//! bounded projection from the SAME authority (PDR-0005), exactly as [`crate::workspace_timeline`]
//! does for the workspace fil.
//!
//! ```text
//! UI (db_command.rs `spec_activity`) --> spec_activity::activity
//!                                          -> db::recent_domain_events_for_spec
//! ```
//!
//! ## Bounded, and NEVER terminal bytes
//!
//! Mirrors [`crate::workspace_timeline`]: the read is capped at [`DEFAULT_ACTIVITY_LIMIT`] rows
//! taken newest-first at the SQL layer, then re-ordered oldest-first for a natural chronology;
//! the ONLY source is `domain_events`, whose payload is a tiny business JSON bag by construction
//! (no terminal scrollback, no PTY buffer, no command output).

use diesel::sqlite::SqliteConnection;
use diesel::QueryResult;

use crate::db::{self, DomainEvent};

/// The default bound on the Spec activity fil — the cap on how many recent Spec/task
/// `domain_events` the PRD detail loads. A small, fixed ceiling that keeps the read (and the
/// render) bounded, matching the workspace timeline's default.
pub const DEFAULT_ACTIVITY_LIMIT: i64 = 50;

/// One entry of the Spec activity fil — a projection of a single [`DomainEvent`] row onto the
/// small presentational shape the detail renders (the SAME shape as a workspace-timeline entry,
/// so the Activity tab renders both uniformly). Carries ONLY the business fields — NEVER a
/// terminal byte (the source row has none by construction).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SpecActivityEntry {
    /// The event's monotonic global ordering key (the `domain_events.sequence` rowid).
    pub sequence: i64,
    /// The dotted `entity.action` wire string (`spec.created`, `task.transitioned`, …).
    pub event_type: String,
    /// The entity CLASS the event concerns: `spec` | `task`.
    pub aggregate: String,
    /// WHERE the mutation originated: `ui` | `mcp` | `system`.
    pub source: String,
    /// The event's small business JSON payload (at least the concerned `spec_id`; a `task.*`
    /// event also carries `task_id`, a transition its `from`/`to`, a section edit its section).
    pub payload: String,
    /// Epoch ms the event was recorded.
    pub created_at: i64,
}

impl From<DomainEvent> for SpecActivityEntry {
    fn from(e: DomainEvent) -> Self {
        Self {
            sequence: e.sequence,
            event_type: e.event_type,
            aggregate: e.aggregate,
            source: e.source,
            payload: e.payload,
            created_at: e.created_at,
        }
    }
}

/// The Spec activity read model: the ONE bounded fil the PRD detail renders — the Spec's most
/// recent `spec`/`task` `domain_events` (its own lifecycle + its tasks' events), oldest first,
/// capped at `limit`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SpecActivity {
    /// The Spec this activity belongs to.
    pub spec_id: String,
    /// The cap that was applied to the underlying read (the fil is BOUNDED to this).
    pub limit: i64,
    /// The bounded slice of correlated events, OLDEST first (chronological order for display).
    pub entries: Vec<SpecActivityEntry>,
}

/// Build the [`SpecActivity`] for `spec_id`, or `None` when the id does not resolve to a Spec.
/// Pure projection — reads only; NEVER writes, NEVER polls, and reads ONLY the `domain_events`
/// outbox (no terminal bytes anywhere). `limit` bounds the read (falling back to
/// [`DEFAULT_ACTIVITY_LIMIT`] when non-positive); the cap is applied at the SQL layer
/// (newest-first), then the entries are re-ordered oldest-first for a natural chronology.
pub fn activity(
    conn: &mut SqliteConnection,
    spec_id: &str,
    limit: i64,
) -> QueryResult<Option<SpecActivity>> {
    // A missing Spec short-circuits to `None` (the caller maps it to a readable "unknown spec"
    // error) rather than an empty-but-present activity fil.
    if db::get_spec(conn, spec_id)?.is_none() {
        return Ok(None);
    }

    // Bound the read: a non-positive `limit` falls back to the default ceiling.
    let bound = if limit > 0 {
        limit
    } else {
        DEFAULT_ACTIVITY_LIMIT
    };

    // The most recent correlated events, newest-first at the SQL `LIMIT` (the WHOLE input;
    // there is no unbounded scan, and the source is the small business outbox only).
    let mut rows = db::recent_domain_events_for_spec(conn, spec_id, bound)?;

    // Re-order oldest-first for a natural top-to-bottom chronology (the SQL returned
    // newest-first to make the `LIMIT` keep the RECENT tail).
    rows.reverse();

    Ok(Some(SpecActivity {
        spec_id: spec_id.to_string(),
        limit: bound,
        entries: rows.into_iter().map(SpecActivityEntry::from).collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        self, create_spec, create_task, open_in_memory, transition_spec, transition_task,
        NewTaskInput, SpecSection, SpecSections, SpecTransition, TaskTransition,
    };
    use crate::repository;

    fn root_path(tag: &str) -> String {
        #[cfg(windows)]
        {
            format!("C:\\sa-{tag}")
        }
        #[cfg(not(windows))]
        {
            format!("/sa-{tag}")
        }
    }

    fn empty_sections() -> SpecSections {
        SpecSections {
            problem: String::new(),
            solution: String::new(),
            implementation_decisions: String::new(),
            testing_decisions: String::new(),
            user_stories: String::new(),
            out_of_scope: String::new(),
        }
    }

    /// The read model surfaces a Spec's own + its tasks' `domain_events` as a bounded,
    /// chronological activity fil (creation → section edit → transition → task add →
    /// task transition), oldest-first, and folds in NOTHING that belongs to another Spec.
    #[test]
    fn activity_reads_spec_and_task_events_oldest_first() {
        let mut conn = open_in_memory();
        let (project, _root) =
            repository::register_project(&mut conn, "Nyx", &root_path("read"), None).unwrap();

        // Spec A gets a full lifecycle worth of activity.
        let (spec, _rev) = create_spec(&mut conn, &project.id, "A", empty_sections()).unwrap();
        db::update_active_revision_section(&mut conn, &spec.id, SpecSection::Problem, "p").unwrap();
        db::update_active_revision_section(&mut conn, &spec.id, SpecSection::Solution, "s")
            .unwrap();
        let task = create_task(
            &mut conn,
            &project.id,
            "T1",
            NewTaskInput {
                spec_id: Some(spec.id.clone()),
                ..NewTaskInput::default()
            },
        )
        .unwrap();
        transition_task(&mut conn, &task.id, TaskTransition::Start, None).unwrap();
        transition_spec(&mut conn, &spec.id, SpecTransition::Ready, true).unwrap();

        // A DIFFERENT Spec's activity must NOT leak into A's fil.
        let (other, _r) = create_spec(&mut conn, &project.id, "B", empty_sections()).unwrap();
        create_task(
            &mut conn,
            &project.id,
            "OtherTask",
            NewTaskInput {
                spec_id: Some(other.id.clone()),
                ..NewTaskInput::default()
            },
        )
        .unwrap();

        let act = activity(&mut conn, &spec.id, DEFAULT_ACTIVITY_LIMIT)
            .unwrap()
            .expect("spec resolves");
        assert_eq!(act.spec_id, spec.id);

        let types: Vec<&str> = act.entries.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "spec.created",
                "spec.section_updated",
                "spec.section_updated",
                "task.added",
                "task.transitioned",
                "spec.transitioned",
            ],
            "the Spec's own + its task's events, oldest-first"
        );

        // Chronology: sequences strictly increasing (oldest-first).
        let seqs: Vec<i64> = act.entries.iter().map(|e| e.sequence).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "oldest-first");

        // No cross-Spec leak: every entry's payload carries THIS spec_id.
        assert!(act
            .entries
            .iter()
            .all(|e| e.payload.contains(&spec.id) && !e.payload.contains(&other.id)));

        // Every entry is a business event with a small JSON payload (never a terminal byte).
        assert!(act.entries.iter().all(|e| e.payload.contains("spec_id")));
    }

    /// The fil is BOUNDED — with more events than the cap, only the MOST RECENT survive
    /// (newest tail), still returned oldest-first.
    #[test]
    fn activity_is_bounded_keeping_the_recent_tail() {
        let mut conn = open_in_memory();
        let (project, _root) =
            repository::register_project(&mut conn, "Nyx", &root_path("bound"), None).unwrap();
        let (spec, _rev) = create_spec(&mut conn, &project.id, "A", empty_sections()).unwrap();
        for i in 0..5 {
            db::update_active_revision_section(
                &mut conn,
                &spec.id,
                SpecSection::Problem,
                &format!("p{i}"),
            )
            .unwrap();
        }

        let act = activity(&mut conn, &spec.id, 2).unwrap().unwrap();
        assert_eq!(act.limit, 2);
        assert_eq!(act.entries.len(), 2, "bounded to the cap");
        let seqs: Vec<i64> = act.entries.iter().map(|e| e.sequence).collect();
        assert!(seqs[0] < seqs[1], "the bounded slice is still oldest-first");
    }

    /// A non-positive `limit` falls back to the default bound.
    #[test]
    fn activity_non_positive_limit_falls_back_to_the_default_bound() {
        let mut conn = open_in_memory();
        let (project, _root) =
            repository::register_project(&mut conn, "Nyx", &root_path("dflt"), None).unwrap();
        let (spec, _rev) = create_spec(&mut conn, &project.id, "A", empty_sections()).unwrap();
        let act = activity(&mut conn, &spec.id, 0).unwrap().unwrap();
        assert_eq!(act.limit, DEFAULT_ACTIVITY_LIMIT);
    }

    /// An unknown Spec id resolves to `None` (not an empty fil).
    #[test]
    fn activity_unknown_spec_is_none() {
        let mut conn = open_in_memory();
        assert!(activity(&mut conn, "nope", DEFAULT_ACTIVITY_LIMIT)
            .unwrap()
            .is_none());
    }
}
