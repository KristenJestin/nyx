//! The workspace TIMELINE read model (v2.2 Phase 4, task
//! `01KWHPV33SAV9NV1STSY403CF0`) — the cockpit's BOUNDED activity fil, read from the
//! `domain_events` outbox (v2.1) and nothing else.
//!
//! ## Why this module exists
//!
//! The v2.2 cockpit shows a short, glanceable fil of a workspace's runtime "changements
//! notables" — the business mutations that already land, atomically, in the v2.1
//! [`domain_events`](crate::db::DomainEvent) outbox (workspace created / renamed /
//! archived / primary-set, the repository lifecycle of its project, …). This module
//! assembles the ONE [`WorkspaceTimeline`] both the UI bridge and the MCP tool consume,
//! so the UI and an agent read the SAME bounded projection from the SAME authority
//! (PDR-0005), exactly as [`crate::workspace_runtime`] and [`crate::workspace_git`] do.
//!
//! ```text
//! UI  (db_command.rs `workspace_timeline`) --\
//!                                             -> workspace_timeline::timeline
//! MCP (mcp_tools_core `workspace_timeline`) --/   -> db::recent_domain_events_for_workspace
//! ```
//!
//! ## Bounded, and NEVER terminal bytes (the two non-goals, enforced here)
//!
//! - BOUNDED: the read is capped at [`DEFAULT_TIMELINE_LIMIT`] rows, taken newest-first at
//!   the SQL layer ([`crate::db::recent_domain_events_for_workspace`]) — it never loads the
//!   whole unbounded log, and it is deliberately NOT the full contextual activity timeline
//!   (that is v2.5). The bound is a hard `LIMIT`, not a client-side slice.
//! - NEVER terminal bytes: the ONLY source is `domain_events`, whose payload is a tiny
//!   business JSON bag by construction (the v12 migration forbids the terminal byte stream in
//!   `payload` — see its docstring). This module reads NO terminal scrollback, NO PTY buffer,
//!   NO command output; there is no code path here that could persist or surface a terminal
//!   byte. Q-008 (no per-byte terminal persistence) holds trivially: we read the outbox, not
//!   the terminal.
//!
//! ## Read-only, no new authority, no polling
//!
//! This module issues ONLY reads and never writes. The LIVE update of the timeline (re-pull
//! on a `domain_event`-driven push, no polling) rides the SAME unsolicited channel the rest
//! of the cockpit uses — a new `domain_events` row is announced on the existing coarse topic
//! and the renderer re-pulls; there is no timer here.

use diesel::sqlite::SqliteConnection;
use diesel::QueryResult;

use crate::db::{self, DomainEvent};

/// The default bound on the timeline — the cap on how many recent `domain_events` the cockpit
/// fil loads. A small, fixed ceiling that keeps the read (and the render) bounded; the full
/// contextual activity timeline is v2.5, so this is deliberately short.
pub const DEFAULT_TIMELINE_LIMIT: i64 = 50;

/// One entry of the workspace timeline — a projection of a single [`DomainEvent`] row onto the
/// small, presentational shape the cockpit fil renders. It carries ONLY the business fields of
/// the event (its ordering key, dotted type, aggregate class, source, correlation, small JSON
/// payload, timestamp) — NEVER any terminal byte (the source row has none by construction).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkspaceTimelineEntry {
    /// The event's monotonic global ordering key (the `domain_events.sequence` rowid).
    pub sequence: i64,
    /// The dotted `entity.action` wire string (`workspace.created`, `repository.renamed`, …).
    pub event_type: String,
    /// The entity CLASS the event concerns: `project` | `workspace` | `repository`.
    pub aggregate: String,
    /// WHERE the mutation originated: `ui` | `mcp`.
    pub source: String,
    /// The project the event is correlated to.
    pub project_id: String,
    /// The workspace the event is correlated to, or `None` for a project-/repository-scoped
    /// event of the workspace's project that carries no single workspace.
    pub workspace_id: Option<String>,
    /// The event's small business JSON payload (at least the concerned identity id). NEVER a
    /// terminal byte stream — the `domain_events` row it comes from cannot carry one.
    pub payload: String,
    /// Epoch ms the event was recorded.
    pub created_at: i64,
}

impl From<DomainEvent> for WorkspaceTimelineEntry {
    fn from(e: DomainEvent) -> Self {
        Self {
            sequence: e.sequence,
            event_type: e.event_type,
            aggregate: e.aggregate,
            source: e.source,
            project_id: e.project_id,
            workspace_id: e.workspace_id,
            payload: e.payload,
            created_at: e.created_at,
        }
    }
}

/// The workspace timeline read model: the ONE bounded fil the cockpit renders and the MCP
/// `workspace_timeline` tool returns — the workspace's most recent `domain_events`, oldest
/// first, capped at `limit`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkspaceTimeline {
    /// The workspace this timeline belongs to.
    pub workspace_id: String,
    /// The cap that was applied to the underlying read (the timeline is BOUNDED to this).
    pub limit: i64,
    /// The bounded slice of correlated events, OLDEST first (chronological order for display).
    /// At most `limit` entries — the most recent ones when the log is longer than the bound.
    pub entries: Vec<WorkspaceTimelineEntry>,
}

/// Build the [`WorkspaceTimeline`] for `workspace_id`, or `None` when the id does not resolve
/// to a workspace. Pure projection — reads only; NEVER writes, NEVER polls, and reads ONLY the
/// `domain_events` outbox (no terminal bytes anywhere). `limit` bounds the read (falling back
/// to [`DEFAULT_TIMELINE_LIMIT`] when non-positive) — the cap is applied at the SQL layer
/// (newest-first), then the entries are re-ordered oldest-first for a natural chronology.
pub fn timeline(
    conn: &mut SqliteConnection,
    workspace_id: &str,
    limit: i64,
) -> QueryResult<Option<WorkspaceTimeline>> {
    // A missing workspace short-circuits to `None` (the caller maps it to a readable "unknown
    // workspace" error) rather than an empty-but-present timeline.
    let Some(workspace) = db::get_workspace(conn, workspace_id)? else {
        return Ok(None);
    };

    // Bound the read: a non-positive `limit` falls back to the default ceiling so a caller can
    // ask for "the default bounded slice" without knowing the number.
    let bound = if limit > 0 {
        limit
    } else {
        DEFAULT_TIMELINE_LIMIT
    };

    // The most recent correlated events, newest-first at the SQL `LIMIT`. This is the WHOLE
    // input — there is no unbounded scan, and the source is the small business outbox only.
    let mut rows =
        db::recent_domain_events_for_workspace(conn, workspace_id, &workspace.project_id, bound)?;

    // Re-order oldest-first for a natural top-to-bottom chronology in the cockpit fil (the SQL
    // returned newest-first to make the `LIMIT` keep the RECENT tail).
    rows.reverse();

    Ok(Some(WorkspaceTimeline {
        workspace_id: workspace_id.to_string(),
        limit: bound,
        entries: rows.into_iter().map(WorkspaceTimelineEntry::from).collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::usecase::{self, DomainEventSource, NullEventSink};

    fn root_path(tag: &str) -> String {
        #[cfg(windows)]
        {
            format!("C:\\tl-{tag}")
        }
        #[cfg(not(windows))]
        {
            format!("/tl-{tag}")
        }
    }

    /// The read model surfaces a workspace's `domain_events` as a bounded, chronological
    /// timeline — done_criterion #1. A create-project (project-scoped) + register-workspace +
    /// rename-workspace sequence lands three correlated events; the timeline returns them
    /// oldest-first, and every entry is a business event type (never a terminal byte).
    #[test]
    fn timeline_reads_workspace_domain_events_oldest_first() {
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("read"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = usecase::register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("read-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        usecase::rename_workspace(&mut conn, &sink, &ws.id, "renamed", DomainEventSource::Mcp)
            .unwrap();

        let tl = timeline(&mut conn, &ws.id, DEFAULT_TIMELINE_LIMIT)
            .unwrap()
            .expect("workspace resolves");
        assert_eq!(tl.workspace_id, ws.id);

        // The workspace-scoped events (created + renamed) are present, oldest-first.
        let ws_events: Vec<&str> = tl
            .entries
            .iter()
            .filter(|e| e.workspace_id.as_deref() == Some(ws.id.as_str()))
            .map(|e| e.event_type.as_str())
            .collect();
        assert_eq!(ws_events, vec!["workspace.created", "workspace.renamed"]);

        // The project-scoped `project.created` (workspace_id NULL, same project) is folded in
        // as a "changement notable" of the workspace's context.
        assert!(tl
            .entries
            .iter()
            .any(|e| e.event_type == "project.created" && e.workspace_id.is_none()));

        // Chronology: sequences are strictly increasing (oldest-first).
        let seqs: Vec<i64> = tl.entries.iter().map(|e| e.sequence).collect();
        assert!(
            seqs.windows(2).all(|w| w[0] < w[1]),
            "oldest-first by sequence"
        );

        // The root workspace's own `workspace.created` also belongs to this project — the
        // project-scoped fold is correlated by project id, not just the one workspace.
        let _ = root;

        // done_criterion #2: NO terminal byte anywhere — every entry is a business event with
        // a small JSON payload, and the ONLY payloads present are the identity bags.
        assert!(tl.entries.iter().all(|e| e.payload.contains("identity_id")));
    }

    /// The timeline is BOUNDED — done_criterion #1 ("bornée") and the "timeline minimale"
    /// intent. With more events than the cap, only the MOST RECENT `limit` survive (newest
    /// tail), still returned oldest-first.
    #[test]
    fn timeline_is_bounded_to_the_limit_keeping_the_recent_tail() {
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, _root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("bound"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = usecase::register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("bound-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        // Emit several rename events so the log is longer than a small cap.
        for i in 0..5 {
            usecase::rename_workspace(
                &mut conn,
                &sink,
                &ws.id,
                &format!("name-{i}"),
                DomainEventSource::Ui,
            )
            .unwrap();
        }

        // Cap at 2 — only the two most recent events survive, oldest-first among them.
        let tl = timeline(&mut conn, &ws.id, 2).unwrap().unwrap();
        assert_eq!(tl.limit, 2);
        assert_eq!(tl.entries.len(), 2, "bounded to the cap");
        let seqs: Vec<i64> = tl.entries.iter().map(|e| e.sequence).collect();
        assert!(seqs[0] < seqs[1], "the bounded slice is still oldest-first");

        // The kept entries are the MOST RECENT — their sequences exceed the earliest events.
        let all = db::list_domain_events(&mut conn).unwrap();
        let max_seq = all.iter().map(|e| e.sequence).max().unwrap();
        assert!(
            tl.entries.iter().any(|e| e.sequence == max_seq),
            "the newest event is retained by the bound"
        );
    }

    /// A non-positive `limit` falls back to the default bound (the caller can ask for "the
    /// default bounded slice" without knowing the number).
    #[test]
    fn timeline_non_positive_limit_falls_back_to_the_default_bound() {
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, _root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("dflt"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = usecase::register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("dflt-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        let tl = timeline(&mut conn, &ws.id, 0).unwrap().unwrap();
        assert_eq!(tl.limit, DEFAULT_TIMELINE_LIMIT);
    }

    /// An unknown workspace id resolves to `None` (not an empty timeline).
    #[test]
    fn timeline_unknown_workspace_is_none() {
        let mut conn = open_in_memory();
        assert!(timeline(&mut conn, "nope", DEFAULT_TIMELINE_LIMIT)
            .unwrap()
            .is_none());
    }
}
