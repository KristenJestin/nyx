//! The SHARED application use-case layer for the structuring identities
//! (project / workspace / repository), v2.1 Phase 3, tasks #7 + #8 (PDR-0005).
//!
//! ## Why this module exists
//!
//! PDR-0005: *"L'UI et le MCP appellent les mêmes services applicatifs."* Before this
//! module the UI bridge (`crates/nyx-sidecar/.../db_command.rs`) and the MCP surface
//! (`crate::mcp_runtime` / `crate::mcp_tools_core`) each reached the low-level
//! `crate::db` / `crate::repository` primitives INDEPENDENTLY — so the orchestration of
//! a business operation ("create a workspace" = insert + auto-detect repos, "remove a
//! workspace" = validate-not-root + running-guard + delete) lived in TWO places and
//! could drift. This module is the ONE application-service layer both adapters delegate
//! to: a thin orchestration over `db`/`repository` that owns the business decisions and
//! the invariants, so the guard rails, errors and (task #8) the emitted domain events
//! are identical whether an operation is driven from the UI or from an MCP tool.
//!
//! ```text
//! UI  (db_command.rs)      --\
//!                            -> usecase::* -> db / repository -> DomainEvent (EventSink)
//! MCP (mcp_runtime/…core)  --/
//! ```
//!
//! Each function here takes a `&mut SqliteConnection` (so a caller composes it inside its
//! own `Db::with_conn`, exactly like the `db`/`repository` primitives) and an injected
//! [`EventSink`] — the SEAM on which task #8 hangs domain-event emission. In Phase 3 the
//! only production `EventSink` is [`NullEventSink`] (a no-op) plus the test
//! [`RecordingEventSink`]; the PERSISTENT `domain_events` table and the real-time PUSH
//! pipeline are Phase 4 and will provide a real `EventSink` implementation WITHOUT
//! touching these use cases (the seam is already here).
//!
//! ## What is NOT here
//!
//! - No Spec/review/task use cases (v2.3/v2.4 — out of scope, PRD non-goal).
//! - No `domain_events` persistence and no push transport (Phase 4). The emission SEAM
//!   is here; the sink that persists/pushes is wired in Phase 4.
//! - No filesystem preconditions (validate-existing-dir vs `mkdir -p`): those stay in the
//!   MCP tool layer, which owns the on-disk intent distinction between `workspace_add`
//!   and `create_workspace`. The use case is the persistence-side orchestration both
//!   funnel into once the path exists.

use diesel::sqlite::SqliteConnection;
use diesel::Connection;
use diesel::QueryResult;

use crate::db::{self, NewDomainEvent, Project, Repository, Workspace};
use crate::system_scope::{require_user_project, require_user_workspace, SystemScopeError};

// --- Domain-event seam (task #8) -------------------------------------------
//
// ## Phase 4 design — outbox persistence + post-commit push (tasks #9 / #10)
//
// The Phase-3 seam had ONE job: let a mutating use case announce, through an injected
// [`EventSink`], that a business change happened — with a no-op prod sink and a recording
// test sink. Phase 4 gives that announcement two real destinations WITHOUT rewriting the
// call sites:
//
//   * **Persistence (task #9) is INTRA-TRANSACTION.** Each mutating use case now runs its
//     DB mutation AND the matching `domain_events` INSERT inside ONE
//     [`SqliteConnection::transaction`] (the outbox pattern): the event row is written on
//     the SAME connection, in the SAME transaction, as the state change. So a rollback
//     erases both and a commit persists both — the event can never disagree with the DB.
//     Diesel nests via SAVEPOINTs, so wrapping a `db::*` helper that already opens its own
//     transaction (e.g. `db::create_project`) is safe. The persisted row is CORRELATED
//     (`project_id` always, `workspace_id` when the event is workspace-scoped), resolved
//     from the mutated entity inside the same transaction (for a REMOVE, captured BEFORE
//     the row is deleted).
//   * **Push (task #10) is POST-COMMIT.** After the transaction commits, the use case
//     calls `events.emit(...)` on the injected [`EventSink`] exactly as in Phase 3 — but
//     now the production sink is the sidecar's real-time push (the coarse
//     `workspaces://changed` unsolicited event), so a committed mutation is reflected in
//     the UI without a reload or a poll. Emitting only AFTER commit means the sink never
//     sees an event for a change that rolled back.
//
// This keeps the seam's shape (a use case emits; the sink decides what to do) while making
// the persistence atomic with the mutation and the push a consequence of the commit.

/// The KIND of a domain event a mutating identity use case emits. A closed enum (not a
/// free string) so a new kind is a compile error at every emission and sink, never a
/// silently-mistyped topic. Minimal by design (task #8 asks for `kind + identité
/// concernée`): the RICH payload (before/after, actor, correlation) lands in Phase 4 with
/// the persistent `domain_events` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainEventKind {
    ProjectCreated,
    ProjectArchived,
    ProjectUnarchived,
    WorkspaceCreated,
    WorkspaceRenamed,
    WorkspaceArchived,
    WorkspaceUnarchived,
    WorkspaceRemoved,
    PrimaryWorkspaceSet,
    RepositoryCreated,
    RepositoryArchived,
    RepositoryUnarchived,
    RepositoryRenamed,
    RepositoryOptionalSet,
    // --- Runtime activity (task 01KWHPV33SAV9NV1STSY403CF0 follow-up) ----------
    // The BOUNDED runtime families the workspace timeline names as its content
    // ("démarrages/arrêts de commandes, sessions agents"). Emitted through the SAME
    // outbox (`persist_event`) IN the transaction that mutates `command_instances`
    // (start/stop) / `agent_sessions` (start/end), correlated to the workspace so
    // `recent_domain_events_for_workspace` surfaces them. Deliberately just these four
    // (+ the identity families above) — NOT the full contextual activity timeline (v2.5).
    /// A managed command instance transitioned to `running` (a start / relaunch spawn).
    CommandStarted,
    /// A managed command instance left `running` (a stop to idle, or a natural
    /// success/error exit).
    CommandStopped,
    /// An agent session started on a terminal (`SessionStart`).
    AgentSessionStarted,
    /// An agent session ended cleanly (`SessionEnd`).
    AgentSessionEnded,
    // --- Système approval ledger (PRD « Scope Système ») -----------------------
    // The SIX transitions of a one-shot approval ([`crate::system_approvals`]). Persisted through
    // the SAME outbox, INSIDE the transaction that performs each transition, so the audit trail of
    // "who authorised what, and was it used" survives the bounded retention of the ledger rows
    // themselves. The payloads carry the request id, the operation KIND and the fingerprint —
    // never the token, never the token hash, never the operation's contents.
    /// An MCP caller asked a human to authorise a protected Système operation.
    SystemApprovalRequested,
    /// A human said yes. The approval is now claimable ONCE, until it expires.
    SystemApprovalGranted,
    /// A human said no. Terminal.
    SystemApprovalDenied,
    /// The five-minute window closed before the decision or before the claim. Emitted exactly once
    /// per row, by the conditional update that performs the lazy expiry.
    SystemApprovalExpired,
    /// The approval was CONSUMED — the one-shot transition, immediately before (runner) or inside
    /// (DB mutation) the authorised effect.
    SystemApprovalClaimed,
    /// The authorised effect finished; the payload's `state` is unchanged and the outcome is on
    /// the row. A failure is journalled here too, without re-opening the approval.
    SystemApprovalCompleted,
}

impl DomainEventKind {
    /// The stable wire string for this kind (`project.created`, `workspace.archived`, …).
    /// The dotted `entity.action` shape is the identity the Phase-4 `domain_events` row
    /// and the push envelope will carry.
    pub fn as_str(self) -> &'static str {
        match self {
            DomainEventKind::ProjectCreated => "project.created",
            DomainEventKind::ProjectArchived => "project.archived",
            DomainEventKind::ProjectUnarchived => "project.unarchived",
            DomainEventKind::WorkspaceCreated => "workspace.created",
            DomainEventKind::WorkspaceRenamed => "workspace.renamed",
            DomainEventKind::WorkspaceArchived => "workspace.archived",
            DomainEventKind::WorkspaceUnarchived => "workspace.unarchived",
            DomainEventKind::WorkspaceRemoved => "workspace.removed",
            DomainEventKind::PrimaryWorkspaceSet => "workspace.primary_set",
            DomainEventKind::RepositoryCreated => "repository.created",
            DomainEventKind::RepositoryArchived => "repository.archived",
            DomainEventKind::RepositoryUnarchived => "repository.unarchived",
            DomainEventKind::RepositoryRenamed => "repository.renamed",
            DomainEventKind::RepositoryOptionalSet => "repository.optional_set",
            DomainEventKind::CommandStarted => "command.started",
            DomainEventKind::CommandStopped => "command.stopped",
            DomainEventKind::AgentSessionStarted => "agent_session.started",
            DomainEventKind::AgentSessionEnded => "agent_session.ended",
            DomainEventKind::SystemApprovalRequested => "system_approval.requested",
            DomainEventKind::SystemApprovalGranted => "system_approval.granted",
            DomainEventKind::SystemApprovalDenied => "system_approval.denied",
            DomainEventKind::SystemApprovalExpired => "system_approval.expired",
            DomainEventKind::SystemApprovalClaimed => "system_approval.claimed",
            DomainEventKind::SystemApprovalCompleted => "system_approval.completed",
        }
    }
}

/// The entity class an event's `identity` id refers to — so a sink (and, in Phase 4, the
/// `domain_events` row) knows WHICH table the stable id resolves against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainEntity {
    Project,
    Workspace,
    Repository,
    /// A managed command instance (`command_instances` row) — the entity a
    /// `command.started` / `command.stopped` event concerns.
    Command,
    /// An agent session (`agent_sessions` row) — the entity an
    /// `agent_session.started` / `agent_session.ended` event concerns.
    AgentSession,
    /// A Système approval request (`system_command_approvals` row) — the entity the six
    /// `system_approval.*` events concern.
    SystemApproval,
}

impl DomainEntity {
    pub fn as_str(self) -> &'static str {
        match self {
            DomainEntity::Project => "project",
            DomainEntity::Workspace => "workspace",
            DomainEntity::Repository => "repository",
            DomainEntity::Command => "command",
            DomainEntity::AgentSession => "agent_session",
            DomainEntity::SystemApproval => "system_approval",
        }
    }
}

/// WHERE a mutation originated — the two callers of the shared use-case layer (PDR-0005).
/// Persisted verbatim in the `domain_events.source` column so the audit log records
/// whether a change came from the UI or from an MCP tool (an agent). A closed enum (not a
/// free string) so a new source is a compile error at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainEventSource {
    /// Driven by the desktop UI bridge (`db_command.rs`).
    Ui,
    /// Driven by an MCP tool (an agent), through the MCP dispatcher.
    Mcp,
    /// Driven by the runtime lifecycle itself, not clearly attributable to a single UI or
    /// MCP call — a managed-command run-state transition (start / stop / natural exit) or an
    /// agent-session lifecycle hook the runner/plugin channel drives. The runner sink and the
    /// agent-session hook are shared by both surfaces, so a runtime event's origin is `system`.
    System,
}

impl DomainEventSource {
    /// The stable wire/DB string (`ui` | `mcp` | `system`). Free TEXT in `domain_events.source`
    /// (no CHECK), so a new source needs no migration.
    pub fn as_str(self) -> &'static str {
        match self {
            DomainEventSource::Ui => "ui",
            DomainEventSource::Mcp => "mcp",
            DomainEventSource::System => "system",
        }
    }
}

/// The correlation of a domain event: the `project_id` it belongs to (always) plus the
/// `workspace_id` when the event is workspace-scoped. Resolved from the mutated entity
/// INSIDE the mutation's transaction so the persisted `domain_events` row is correlated
/// even for a REMOVE (the ids are captured before the row is deleted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainEventCorrelation {
    pub project_id: String,
    pub workspace_id: Option<String>,
}

/// ONE domain event: a `kind` (what happened) plus the CONCERNED identity (its entity
/// class + stable id). Deliberately minimal (task #8 — the emission is observable at the
/// use-case level); the persistent/rich form is Phase 4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainEvent {
    pub kind: DomainEventKind,
    pub entity: DomainEntity,
    /// The stable internal id (UUIDv7) of the concerned identity.
    pub identity_id: String,
}

impl DomainEvent {
    fn new(kind: DomainEventKind, entity: DomainEntity, identity_id: impl Into<String>) -> Self {
        Self {
            kind,
            entity,
            identity_id: identity_id.into(),
        }
    }
}

/// The SEAM (task #8) through which a mutating identity use case emits its [`DomainEvent`].
/// The use cases depend on this abstraction, NOT on any concrete persistence/transport, so
/// Phase 4 plugs in the `domain_events`-persisting + real-time-push implementation here
/// WITHOUT editing a single use case. Phase 3 ships two implementations: [`NullEventSink`]
/// (production no-op — nothing consumes events yet) and the test [`RecordingEventSink`]
/// (collects the emitted events so a test can assert a mutation emitted the expected one).
pub trait EventSink {
    /// Emit a domain event. Called by a use case AFTER a mutation has committed to the DB
    /// (the row exists / is stamped), so a sink only ever sees events for applied changes.
    fn emit(&self, event: DomainEvent);
}

/// The production Phase-3 [`EventSink`]: a no-op. No consumer exists yet — the persistent
/// `domain_events` table and the push pipeline are Phase 4 — so emission is a black hole
/// here, but the CALL SITES in the use cases are already in place, so Phase 4 is a
/// sink-swap, not a use-case rewrite.
pub struct NullEventSink;

impl EventSink for NullEventSink {
    fn emit(&self, _event: DomainEvent) {}
}

// --- Outbox persistence (task #9) -------------------------------------------

/// The entity class an event's `identity` id refers to → the [`crate::db`] aggregate
/// string persisted in `domain_events.aggregate`.
fn aggregate_str(entity: DomainEntity) -> &'static str {
    entity.as_str()
}

/// Build the small JSON `payload` bag for a domain event. Deliberately minimal — at least
/// the concerned `identity_id` — and NEVER the terminal byte stream or a Git-derived view
/// (explicit non-goals). A future field lands here without a schema change.
fn build_payload(identity_id: &str) -> String {
    // serde_json over a fixed shape is infallible; a manual literal keeps it dependency-free
    // and escapes the id (a UUIDv7 is already safe, but escape defensively).
    serde_json::json!({ "identity_id": identity_id }).to_string()
}

/// PERSIST one `domain_events` outbox row for a just-applied mutation, INSIDE the caller's
/// transaction (task #9). The correlation (`project_id` always, `workspace_id` when the
/// event is workspace-scoped) is passed by the caller, which resolved it from the mutated
/// entity in the SAME transaction (captured BEFORE a delete). The persisted row's
/// `sequence` is not needed by callers, so this returns `()` — the row's presence/atomicity
/// is what matters. Kept crate-private: only the use cases below persist events. The payload
/// is the identity-only bag; a caller with a richer BUSINESS bag (a runtime event) uses
/// [`persist_event_with_payload`].
fn persist_event(
    conn: &mut SqliteConnection,
    kind: DomainEventKind,
    entity: DomainEntity,
    identity_id: &str,
    correlation: &DomainEventCorrelation,
    source: DomainEventSource,
) -> QueryResult<()> {
    persist_event_with_payload(
        conn,
        kind,
        entity,
        &build_payload(identity_id),
        correlation,
        source,
    )
}

/// PERSIST one `domain_events` outbox row with a caller-provided BUSINESS `payload` (a small
/// JSON bag — id/name/run-state/provider), INSIDE the caller's transaction. The generalization
/// of [`persist_event`] for the runtime families (`command.*` / `agent_session.*`) whose payload
/// carries more than the identity id. The `payload` MUST stay a small business bag — NEVER a
/// terminal byte stream or scrollback (Q-008; the `domain_events` table forbids it by design).
fn persist_event_with_payload(
    conn: &mut SqliteConnection,
    kind: DomainEventKind,
    entity: DomainEntity,
    payload: &str,
    correlation: &DomainEventCorrelation,
    source: DomainEventSource,
) -> QueryResult<()> {
    db::record_domain_event(
        conn,
        NewDomainEvent {
            project_id: correlation.project_id.clone(),
            workspace_id: correlation.workspace_id.clone(),
            aggregate: aggregate_str(entity).to_string(),
            event_type: kind.as_str().to_string(),
            source: source.as_str().to_string(),
            actor: None,
            payload: payload.to_string(),
            // 0 → `record_domain_event` stamps `now_millis()`.
            created_at: 0,
        },
    )?;
    Ok(())
}

/// Resolve a WORKSPACE's correlation (its `project_id` + itself as `workspace_id`) from the
/// workspace id, inside the current transaction. `None` when the id does not resolve.
fn correlate_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<Option<DomainEventCorrelation>> {
    Ok(
        db::get_workspace(conn, workspace_id)?.map(|ws| DomainEventCorrelation {
            project_id: ws.project_id,
            workspace_id: Some(ws.id),
        }),
    )
}

/// Resolve a REPOSITORY's correlation (its `project_id`; a repository is not
/// workspace-scoped, so `workspace_id` is `None`) from the repository id, inside the
/// current transaction. `None` when the id does not resolve.
fn correlate_repository(
    conn: &mut SqliteConnection,
    repository_id: &str,
) -> QueryResult<Option<DomainEventCorrelation>> {
    Ok(
        db::get_repository(conn, repository_id)?.map(|repo| DomainEventCorrelation {
            project_id: repo.project_id,
            workspace_id: None,
        }),
    )
}

/// The correlation of a PROJECT-scoped event (the project itself; no workspace).
fn correlate_project(project_id: &str) -> DomainEventCorrelation {
    DomainEventCorrelation {
        project_id: project_id.to_string(),
        workspace_id: None,
    }
}

// --- Project use cases ------------------------------------------------------

/// USE CASE — create a project on `root_path` (with its root workspace) and auto-detect
/// its repositories. The single orchestration both the UI (`create_project`) and any MCP
/// caller funnel into: it composes [`crate::repository::register_project`] (which creates
/// the project, its root workspace, and auto-detects the repos under the root) and emits a
/// [`DomainEventKind::ProjectCreated`] on success. Returns the new project and its root
/// workspace.
pub fn create_project(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    name: &str,
    root_path: &str,
    root_name: Option<&str>,
    source: DomainEventSource,
) -> QueryResult<(Project, Workspace)> {
    // Outbox (task #9): the project + root creation AND its `domain_events` row commit
    // atomically — a rollback erases both.
    let (project, root) = conn.transaction(|conn| {
        let (project, root) =
            crate::repository::register_project(conn, name, root_path, root_name)?;
        persist_event(
            conn,
            DomainEventKind::ProjectCreated,
            DomainEntity::Project,
            &project.id,
            &correlate_project(&project.id),
            source,
        )?;
        Ok::<_, diesel::result::Error>((project, root))
    })?;
    // Post-commit push (task #10): only reached after the transaction committed.
    events.emit(DomainEvent::new(
        DomainEventKind::ProjectCreated,
        DomainEntity::Project,
        &project.id,
    ));
    Ok((project, root))
}

/// USE CASE — list every project (sidebar order). The read companion; no event.
pub fn list_projects(conn: &mut SqliteConnection) -> QueryResult<Vec<Project>> {
    db::list_projects(conn)
}

/// USE CASE — resolve ONE project by its stable id, or `None`. Identity resolution keys on
/// the id, not a disk path. No event (a read).
pub fn resolve_project(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<Project>> {
    db::get_project(conn, id)
}

/// USE CASE — soft-archive a project (stamp `archived_at`, never a hard delete). Emits a
/// [`DomainEventKind::ProjectArchived`] ONLY when a row actually flipped (an
/// already-archived / unknown id is a no-op and emits nothing). Returns rows updated.
///
/// SYSTÈME: refused. Archiving the singleton would hide the container of the user's cross-project
/// services behind a lifecycle it does not have (see [`require_user_project`]).
pub fn archive_project(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> Result<usize, SystemScopeError> {
    require_user_project(conn, id, "archive_project")?;
    // The archive flip AND (when a row flipped) its outbox row are one transaction.
    let updated = conn.transaction(|conn| {
        let updated = db::archive_project(conn, id)?;
        if updated > 0 {
            persist_event(
                conn,
                DomainEventKind::ProjectArchived,
                DomainEntity::Project,
                id,
                &correlate_project(id),
                source,
            )?;
        }
        Ok::<_, diesel::result::Error>(updated)
    })?;
    if updated > 0 {
        events.emit(DomainEvent::new(
            DomainEventKind::ProjectArchived,
            DomainEntity::Project,
            id,
        ));
    }
    Ok(updated)
}

/// USE CASE — reverse [`archive_project`]. Emits [`DomainEventKind::ProjectUnarchived`]
/// only when a row flipped. Returns rows updated.
///
/// SYSTÈME: refused, like its twin — the singleton has no archive lifecycle to reverse.
pub fn unarchive_project(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> Result<usize, SystemScopeError> {
    require_user_project(conn, id, "unarchive_project")?;
    let updated = conn.transaction(|conn| {
        let updated = db::unarchive_project(conn, id)?;
        if updated > 0 {
            persist_event(
                conn,
                DomainEventKind::ProjectUnarchived,
                DomainEntity::Project,
                id,
                &correlate_project(id),
                source,
            )?;
        }
        Ok::<_, diesel::result::Error>(updated)
    })?;
    if updated > 0 {
        events.emit(DomainEvent::new(
            DomainEventKind::ProjectUnarchived,
            DomainEntity::Project,
            id,
        ));
    }
    Ok(updated)
}

/// USE CASE — set (or move) a project's canonical `primary_workspace_id`, enforcing the
/// cross-project coherence invariant (the target workspace must belong to the project).
/// Emits [`DomainEventKind::PrimaryWorkspaceSet`] (concerning the workspace) ONLY when the
/// pointer was actually set (`Ok(true)`); an invariant violation writes nothing and emits
/// nothing. Returns whether the pointer was set.
///
/// SYSTÈME: refused. The singleton's primary workspace is BOOTSTRAPPED and is the only workspace
/// it will ever have; moving the pointer is how every Système command would silently change cwd.
pub fn set_primary_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    project_id: &str,
    workspace_id: &str,
    source: DomainEventSource,
) -> Result<bool, SystemScopeError> {
    require_user_project(conn, project_id, "set_primary_workspace")?;
    require_user_workspace(conn, workspace_id, "set_primary_workspace")?;
    let set = conn.transaction(|conn| {
        let set = db::set_primary_workspace(conn, project_id, workspace_id)?;
        if set {
            // The event concerns the workspace now marked primary; its correlation is that
            // workspace (which belongs to `project_id`, the invariant just enforced).
            let correlation = DomainEventCorrelation {
                project_id: project_id.to_string(),
                workspace_id: Some(workspace_id.to_string()),
            };
            persist_event(
                conn,
                DomainEventKind::PrimaryWorkspaceSet,
                DomainEntity::Workspace,
                workspace_id,
                &correlation,
                source,
            )?;
        }
        Ok::<_, diesel::result::Error>(set)
    })?;
    if set {
        events.emit(DomainEvent::new(
            DomainEventKind::PrimaryWorkspaceSet,
            DomainEntity::Workspace,
            workspace_id,
        ));
    }
    Ok(set)
}

// --- Workspace use cases ----------------------------------------------------

/// USE CASE — register a non-root workspace at `path` in `project_id` and auto-detect its
/// repositories: the UNANCHORED entry point, for callers that have no Spec to anchor. It
/// DELEGATES to [`register_workspace_anchored`] with `spec_id = None` — it composes nothing
/// itself, so the insert + auto-detect ([`crate::repository::register_workspace`]) and the
/// [`DomainEventKind::WorkspaceCreated`] emission live in exactly ONE place, shared with the
/// anchored path. Returns the new workspace.
///
/// PRODUCTION CALLER: the sidecar/UI bridge's `create_workspace` DB command
/// (`nyx-sidecar/src/runtime/db_command.rs`) — plus tests. NOT the MCP: since v2.5
/// `workspace_add` / `create_workspace` go straight to [`register_workspace_anchored`]
/// (`mcp_runtime::create_workspace_inner`) so they can carry their optional `spec_id`. This
/// signature is load-bearing for that bridge and stays as it is; the on-disk path
/// precondition (validate-existing vs `mkdir -p`) is still the adapter's, not this layer's.
pub fn register_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    project_id: &str,
    name: &str,
    path: &str,
    source: DomainEventSource,
) -> Result<Workspace, SystemScopeError> {
    // The UNANCHORED contract: delegate with `spec_id = None`. With no Spec to anchor the
    // anchoring half cannot fail, so the only error shapes reachable here are `Db` and the
    // Système refusal the anchored path raises before it opens its transaction.
    match register_workspace_anchored(conn, events, project_id, name, path, None, source) {
        Ok((workspace, _)) => Ok(workspace),
        Err(RegisterWorkspaceError::Db(e)) => Err(SystemScopeError::Db(e)),
        Err(RegisterWorkspaceError::SystemScope(e)) => Err(e),
        // Unreachable (no `spec_id` ⇒ no anchor step). Collapse to a rollback rather than
        // panic, the same way `content_write_to_query_err` handles its impossible arms.
        Err(RegisterWorkspaceError::Anchor(_)) => Err(SystemScopeError::Db(
            diesel::result::Error::RollbackTransaction,
        )),
    }
}

/// Why [`register_workspace_anchored`] refused: either the workspace INSERT failed (`Db` — an
/// unknown project's FK violation, a duplicate path's UNIQUE violation…) or the Spec ANCHOR
/// was refused (`Anchor` — unknown/`draft`/terminal Spec, or a Spec of another project). Both
/// arms leave the DB untouched: the two writes share ONE transaction.
#[derive(Debug)]
pub enum RegisterWorkspaceError {
    /// The workspace registration itself failed.
    Db(diesel::result::Error),
    /// The Spec anchor was refused (the whole transaction rolled back).
    Anchor(db::SetSpecWorkspaceError),
    /// The target project is the SYSTÈME singleton, which has exactly one BOOTSTRAPPED workspace
    /// and no verb that adds a second (PRD « Scope Système »).
    SystemScope(SystemScopeError),
}

impl From<diesel::result::Error> for RegisterWorkspaceError {
    fn from(e: diesel::result::Error) -> Self {
        RegisterWorkspaceError::Db(e)
    }
}

impl From<SystemScopeError> for RegisterWorkspaceError {
    fn from(e: SystemScopeError) -> Self {
        RegisterWorkspaceError::SystemScope(e)
    }
}

impl From<db::SetSpecWorkspaceError> for RegisterWorkspaceError {
    fn from(e: db::SetSpecWorkspaceError) -> Self {
        match e {
            db::SetSpecWorkspaceError::Db(e) => RegisterWorkspaceError::Db(e),
            other => RegisterWorkspaceError::Anchor(other),
        }
    }
}

/// USE CASE — register a workspace AND (optionally) ANCHOR a Spec on it, in ONE transaction.
/// The v2.5 widening of [`register_workspace`]: a dev workspace is very often created/declared
/// FOR a Spec, so the two writes must be one coherent operation — either the workspace exists
/// AND the Spec points at it, or NEITHER happened (a refused anchor rolls the workspace insert
/// back, leaving no partial workspace behind).
///
/// The anchor goes through the SHARED [`db::set_spec_workspace`] authority, so it inherits its
/// guards verbatim (`ready`/`in_progress` only, same project, existing workspace) and its
/// `spec.workspace_changed` outbox row. It NEVER transitions the Spec — the status is read for
/// the guard, never written.
///
/// The caller is expected to have PREVALIDATED `spec_id` with [`db::check_spec_anchorable`]
/// before touching the filesystem; this function re-applies the guards anyway (the DB is the
/// authority, and the workspace only exists here). Returns the new workspace and the anchored
/// Spec (`None` when `spec_id` is absent).
pub fn register_workspace_anchored(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    project_id: &str,
    name: &str,
    path: &str,
    spec_id: Option<&str>,
    source: DomainEventSource,
) -> Result<(Workspace, Option<db::Spec>), RegisterWorkspaceError> {
    // SYSTÈME: refused before the transaction opens. The singleton owns exactly ONE workspace, the
    // bootstrapped one — a second would make "the Système home" ambiguous and is precisely the
    // state `ensure_system_scope` refuses to repair at boot.
    require_user_project(conn, project_id, "register_workspace")?;
    let (workspace, spec) = conn.transaction(|conn| {
        let workspace = crate::repository::register_workspace(conn, project_id, name, path)?;
        let correlation = DomainEventCorrelation {
            project_id: workspace.project_id.clone(),
            workspace_id: Some(workspace.id.clone()),
        };
        persist_event(
            conn,
            DomainEventKind::WorkspaceCreated,
            DomainEntity::Workspace,
            &workspace.id,
            &correlation,
            source,
        )?;
        // SAME transaction: a refused anchor rolls the workspace insert back with it.
        let spec = match spec_id {
            None => None,
            Some(spec_id) => Some(db::set_spec_workspace(conn, spec_id, Some(&workspace.id))?),
        };
        Ok::<_, RegisterWorkspaceError>((workspace, spec))
    })?;
    events.emit(DomainEvent::new(
        DomainEventKind::WorkspaceCreated,
        DomainEntity::Workspace,
        &workspace.id,
    ));
    Ok((workspace, spec))
}

/// USE CASE — RENAME a workspace's display `name` (a human label; the identity `path` is
/// untouched — it identifies the folder). The single orchestration both the UI
/// (`rename_workspace`) and any MCP caller funnel into, mirroring [`rename_repository`]:
/// inside ONE transaction it renames (`db::rename_workspace`) and, when a row was updated,
/// persists a correlated [`DomainEventKind::WorkspaceRenamed`] outbox row (task #9); after
/// commit it emits the domain event through the injected sink (task #10). An unknown id is a
/// no-op (0 rows) that writes and emits nothing. Returns rows updated.
///
/// SYSTÈME: refused. The scope's workspace is named by the bootstrap, not by a human.
pub fn rename_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    name: &str,
    source: DomainEventSource,
) -> Result<usize, SystemScopeError> {
    require_user_workspace(conn, id, "rename_workspace")?;
    let updated = conn.transaction(|conn| {
        let updated = db::rename_workspace(conn, id, name)?;
        if updated > 0 {
            // The row still exists (a rename is an in-place update), so correlate from it.
            if let Some(correlation) = correlate_workspace(conn, id)? {
                persist_event(
                    conn,
                    DomainEventKind::WorkspaceRenamed,
                    DomainEntity::Workspace,
                    id,
                    &correlation,
                    source,
                )?;
            }
        }
        Ok::<_, diesel::result::Error>(updated)
    })?;
    if updated > 0 {
        events.emit(DomainEvent::new(
            DomainEventKind::WorkspaceRenamed,
            DomainEntity::Workspace,
            id,
        ));
    }
    Ok(updated)
}

/// USE CASE — list the workspaces of `project_id` (root first). Read companion; no event.
///
/// SYSTÈME: refused. `list_projects` / `list_workspaces` are the USER containers' surface (the
/// sidebar tree, the MCP inventory); the singleton and its home workspace are read through their
/// own door instead ([`crate::system_scope::load`]). Refusing rather than answering an empty list
/// keeps the two apart: an empty list would say "this project has no workspaces", which is false.
pub fn list_workspaces(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> Result<Vec<Workspace>, SystemScopeError> {
    require_user_project(conn, project_id, "list_workspaces")?;
    Ok(db::list_workspaces(conn, project_id)?)
}

/// USE CASE — resolve ONE workspace by its stable id, or `None`. No event (a read).
pub fn resolve_workspace(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<Workspace>> {
    db::get_workspace(conn, id)
}

/// USE CASE — resolve a project's PRIMARY (free-work) workspace: the durable base workspace
/// where the v2.2 cockpit opens FREE WORK (a workspace with NO Spec — there is no `specs`
/// table in v2.2, so "sans Spec" is structural). This is the cockpit's default entry point
/// for a project ("le primary workspace est l'espace de travail libre par défaut", task
/// `01KWHPV33H0FX8NA4C94T5W46J`; glossary / domain-model.md §primary workspace).
///
/// Resolution keys on the CANONICAL source of truth first — `projects.primary_workspace_id`
/// (v10; the durable primary role, replacing reliance on `workspaces.is_root`) — and, only if
/// that pointer is absent or dangles (a project predating the v11 backfill, or a soft-archived
/// target), FALLS BACK to the project's root workspace (`db::list_workspaces` orders `is_root`
/// first). The pointer only wins when it resolves to a LIVE (non-archived) workspace of this
/// project: an archived workspace is not a valid default FREE-WORK space, so a pointer at a
/// soft-archived target is treated as dangling and the fallback prefers the first non-archived
/// candidate. Returns `None` only for an unknown project with no workspaces at all. No event
/// (a read); NEVER touches a Spec.
pub fn resolve_primary_workspace(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<Option<Workspace>> {
    // The canonical pointer wins when it resolves to a LIVE workspace of THIS project. A
    // soft-archived target (`archived_at` set) is NOT a valid free-work default, so it is
    // treated as a dangling pointer and skipped in favour of the fallback below.
    if let Some(project) = db::get_project(conn, project_id)? {
        if let Some(primary_id) = project.primary_workspace_id.as_deref() {
            if let Some(ws) = db::get_workspace(conn, primary_id)? {
                if ws.project_id == project_id && ws.archived_at.is_none() {
                    return Ok(Some(ws));
                }
            }
        }
    }
    // Fallback for a project whose canonical pointer is absent, dangles, or points at an
    // archived target: the first NON-ARCHIVED workspace (list_workspaces returns `is_root`
    // first, so this is the live root when present). Only if every workspace is archived do
    // we fall back to the first row overall, so an all-archived project still resolves rather
    // than reporting `None` (which stays reserved for a project with no workspaces at all).
    let workspaces = db::list_workspaces(conn, project_id)?;
    Ok(workspaces
        .iter()
        .find(|ws| ws.archived_at.is_none())
        .cloned()
        .or_else(|| workspaces.into_iter().next()))
}

/// USE CASE — soft-archive a workspace (the ROW is permanent — PDR-0008 — archiving marks
/// it inactive without deleting it). Emits [`DomainEventKind::WorkspaceArchived`] only when
/// a row flipped. Returns rows updated.
///
/// SYSTÈME: refused — the scope's one workspace has no archive lifecycle.
pub fn archive_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> Result<usize, SystemScopeError> {
    require_user_workspace(conn, id, "archive_workspace")?;
    let updated = conn.transaction(|conn| {
        let updated = db::archive_workspace(conn, id)?;
        if updated > 0 {
            // The row still exists (archive is a soft flip), so correlate from it.
            if let Some(correlation) = correlate_workspace(conn, id)? {
                persist_event(
                    conn,
                    DomainEventKind::WorkspaceArchived,
                    DomainEntity::Workspace,
                    id,
                    &correlation,
                    source,
                )?;
            }
        }
        Ok::<_, diesel::result::Error>(updated)
    })?;
    if updated > 0 {
        events.emit(DomainEvent::new(
            DomainEventKind::WorkspaceArchived,
            DomainEntity::Workspace,
            id,
        ));
    }
    Ok(updated)
}

/// USE CASE — reverse [`archive_workspace`]. Emits
/// [`DomainEventKind::WorkspaceUnarchived`] only when a row flipped. Returns rows updated.
///
/// SYSTÈME: refused, like its twin.
pub fn unarchive_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> Result<usize, SystemScopeError> {
    require_user_workspace(conn, id, "unarchive_workspace")?;
    let updated = conn.transaction(|conn| {
        let updated = db::unarchive_workspace(conn, id)?;
        if updated > 0 {
            if let Some(correlation) = correlate_workspace(conn, id)? {
                persist_event(
                    conn,
                    DomainEventKind::WorkspaceUnarchived,
                    DomainEntity::Workspace,
                    id,
                    &correlation,
                    source,
                )?;
            }
        }
        Ok::<_, diesel::result::Error>(updated)
    })?;
    if updated > 0 {
        events.emit(DomainEvent::new(
            DomainEventKind::WorkspaceUnarchived,
            DomainEntity::Workspace,
            id,
        ));
    }
    Ok(updated)
}

/// Why a [`remove_workspace`] call was refused, as a CLOSED domain error both adapters map
/// to their own transport error. Modelling the two DB-expressible refusals as distinct
/// variants (not two ad-hoc string comparisons in each bridge) is exactly what stops the UI
/// and the MCP route from drifting on the invariant wording / status code.
#[derive(Debug)]
pub enum RemoveWorkspaceError {
    /// No workspace resolves to the given id.
    NotFound,
    /// The id is the project's ROOT workspace — removing it would leave a rootless project,
    /// so it is refused (delete the whole project instead).
    IsRoot,
    /// The workspace belongs to the SYSTÈME singleton. Named SEPARATELY from [`Self::IsRoot`]
    /// although the Système workspace happens to be root: the refusal must survive the day the
    /// root rule changes, and the caller must be able to tell a lifecycle invariant from a
    /// container that has no lifecycle at all.
    SystemScope(SystemScopeError),
    /// A DB error surfaced while resolving or deleting.
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for RemoveWorkspaceError {
    fn from(e: diesel::result::Error) -> Self {
        RemoveWorkspaceError::Db(e)
    }
}

impl From<SystemScopeError> for RemoveWorkspaceError {
    fn from(e: SystemScopeError) -> Self {
        RemoveWorkspaceError::SystemScope(e)
    }
}

/// USE CASE — HARD-remove a non-root workspace and its FULL runtime footprint (the row, its
/// cascaded command instances, AND its terminals + agent sessions). This is the SINGLE
/// nyx-core owner of the DB-expressible remove decision both the UI (`workspace_delete`) and
/// the MCP (`remove_workspace`) route funnel into: it enforces the root-workspace invariant,
/// DELETES the workspace's `agent_sessions` + `terminals` rows (so nothing is detached into
/// the orphaned "loose" bucket — review 01KWEX11), performs [`db::delete_workspace`], guards
/// the zero-row race (the row vanished between resolve and delete), and emits
/// [`DomainEventKind::WorkspaceRemoved`] on success. Returns the number of WORKSPACE rows
/// deleted (always `1` on `Ok`).
///
/// NOT here (a pure RUNTIME concern, like the `mkdir` [`register_workspace`] leaves to its
/// adapters): KILLING the workspace's LIVE processes — the running command instances' PTY
/// trees and the interactive terminals' shells. That kill reaches the sidecar's LIVE process
/// runner (`CommandRunner`) and PTY manager, neither reachable from a `&mut SqliteConnection`.
/// Per the review's decision ("Tout supprimer + tuer les vivants") the former refusal-if-running
/// guard is REPLACED by a kill: each adapter STOPS the workspace's running instances + CLOSES
/// its terminal PTYs BEFORE delegating here; the DB-expressible business decision (invariant +
/// runtime-row delete + workspace delete + zero-row + event) is this one use case.
pub fn remove_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> Result<usize, RemoveWorkspaceError> {
    // SYSTÈME first, before the root rule: the singleton's workspace is not removable at all, and
    // saying so by name beats leaning on the fact that it also happens to be root.
    require_user_workspace(conn, id, "remove_workspace")?;
    // The invariant check, the delete AND the `workspace.removed` outbox row are ONE
    // transaction. The correlation is captured from the workspace row BEFORE the delete
    // (after the delete the row — and its `project_id` — are gone), so the removal event is
    // still correlated to its project. A refusal (root / not-found) or a zero-row race
    // returns an error, which rolls the transaction back (nothing written).
    let deleted = conn.transaction(|conn| {
        let workspace = match db::get_workspace(conn, id)? {
            None => return Err(RemoveWorkspaceError::NotFound),
            Some(ws) if ws.is_root => return Err(RemoveWorkspaceError::IsRoot),
            Some(ws) => ws,
        };
        // Capture correlation before the row disappears.
        let correlation = DomainEventCorrelation {
            project_id: workspace.project_id.clone(),
            workspace_id: Some(workspace.id.clone()),
        };
        // DESTRUCTIVE runtime cascade (review 01KWEX11): a workspace's terminals + agent
        // sessions carry an ON DELETE SET NULL FK, so `delete_workspace` alone would DETACH
        // them (workspace_id → NULL) and the renderer would reclass them as orphaned "loose"
        // terminals. We instead DELETE them explicitly, IN this transaction, before removing
        // the workspace — so removal is a full runtime cascade (parity with `command_instances`,
        // which already ON DELETE CASCADE). Order: sessions first (covers those anchored to the
        // workspace even on a loose host terminal), then terminals (whose delete cascades any
        // remaining session on a workspace-owned terminal). We do NOT emit per-row events: the
        // single `workspace.removed` event below is the coarse signal both adapters push on.
        // That event alone does NOT refresh the terminal list: the renderer re-pulls terminals on
        // `terminals://changed`, a DIFFERENT topic, so an adapter that deletes a workspace must ALSO
        // announce the terminal mutation or the deleted rows stay on screen until something else
        // triggers a re-list (bridge: `mutates_terminals`). The KILL of any LIVE
        // PTY / running command is an adapter concern (it needs the sidecar runner, unreachable
        // from `&mut SqliteConnection`) and runs BEFORE this use case — see the bridge/MCP route.
        db::delete_agent_sessions_for_workspace(conn, id)?;
        db::delete_terminals_for_workspace(conn, id)?;
        let deleted = db::delete_workspace(conn, id)?;
        if deleted == 0 {
            // The row vanished between resolve and delete (a concurrent removal): treat it as
            // not-found rather than emitting a phantom removal event. Erroring rolls back.
            return Err(RemoveWorkspaceError::NotFound);
        }
        persist_event(
            conn,
            DomainEventKind::WorkspaceRemoved,
            DomainEntity::Workspace,
            id,
            &correlation,
            source,
        )?;
        Ok(deleted)
    })?;
    events.emit(DomainEvent::new(
        DomainEventKind::WorkspaceRemoved,
        DomainEntity::Workspace,
        id,
    ));
    Ok(deleted)
}

// --- Repository use cases ---------------------------------------------------

/// USE CASE — create a logical repository identity in `project_id`. The manual-declaration
/// companion to auto-detection (both funnel through the same `db` write; auto-detect uses
/// the idempotent upsert). Emits [`DomainEventKind::RepositoryCreated`] on success. Returns
/// the new repository.
///
/// SYSTÈME: refused. A repository is a Git identity, and the Système scope is closed to Git end to
/// end — its `root_path` is the home directory, so a repo declared there would point every Git read
/// at the user's whole home. Refusing HERE is what makes
/// [`crate::repository::workspace_members`]'s empty answer a consequence rather than a special
/// case: the singleton can hold no repository to derive in the first place.
pub fn create_repository(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    project_id: &str,
    name: &str,
    relative_path: &str,
    optional: bool,
    source: DomainEventSource,
) -> Result<Repository, SystemScopeError> {
    require_user_project(conn, project_id, "create_repository")?;
    let repo = conn.transaction(|conn| {
        let repo = db::create_repository(conn, project_id, name, relative_path, optional)?;
        let correlation = DomainEventCorrelation {
            project_id: repo.project_id.clone(),
            workspace_id: None,
        };
        persist_event(
            conn,
            DomainEventKind::RepositoryCreated,
            DomainEntity::Repository,
            &repo.id,
            &correlation,
            source,
        )?;
        Ok::<_, diesel::result::Error>(repo)
    })?;
    events.emit(DomainEvent::new(
        DomainEventKind::RepositoryCreated,
        DomainEntity::Repository,
        &repo.id,
    ));
    Ok(repo)
}

/// USE CASE — list the LIVE (non-archived) repositories of `project_id`. Read companion;
/// no event.
pub fn list_repositories(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<Vec<Repository>> {
    db::list_repositories(conn, project_id)
}

/// USE CASE — resolve ONE repository by its stable id, or `None`. Returns archived rows
/// too (the id still resolves). No event (a read).
pub fn resolve_repository(
    conn: &mut SqliteConnection,
    id: &str,
) -> QueryResult<Option<Repository>> {
    db::get_repository(conn, id)
}

/// USE CASE — soft-archive a repository identity (retire it from active membership without
/// deleting it). Emits [`DomainEventKind::RepositoryArchived`] only when a row flipped.
/// Returns rows updated.
pub fn archive_repository(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> QueryResult<usize> {
    let updated = repository_flip(
        conn,
        id,
        DomainEventKind::RepositoryArchived,
        source,
        db::archive_repository,
    )?;
    if updated > 0 {
        events.emit(DomainEvent::new(
            DomainEventKind::RepositoryArchived,
            DomainEntity::Repository,
            id,
        ));
    }
    Ok(updated)
}

/// USE CASE — reverse [`archive_repository`]. Emits
/// [`DomainEventKind::RepositoryUnarchived`] only when a row flipped. Returns rows updated.
pub fn unarchive_repository(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> QueryResult<usize> {
    let updated = repository_flip(
        conn,
        id,
        DomainEventKind::RepositoryUnarchived,
        source,
        db::unarchive_repository,
    )?;
    if updated > 0 {
        events.emit(DomainEvent::new(
            DomainEventKind::RepositoryUnarchived,
            DomainEntity::Repository,
            id,
        ));
    }
    Ok(updated)
}

/// USE CASE — RENAME a repository's logical name (a manual override; the identity
/// `relative_path` is untouched). Emits [`DomainEventKind::RepositoryRenamed`] only when a
/// row was updated. Returns rows updated.
pub fn rename_repository(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    name: &str,
    source: DomainEventSource,
) -> QueryResult<usize> {
    let updated = conn.transaction(|conn| {
        let updated = db::rename_repository(conn, id, name)?;
        if updated > 0 {
            if let Some(correlation) = correlate_repository(conn, id)? {
                persist_event(
                    conn,
                    DomainEventKind::RepositoryRenamed,
                    DomainEntity::Repository,
                    id,
                    &correlation,
                    source,
                )?;
            }
        }
        Ok::<_, diesel::result::Error>(updated)
    })?;
    if updated > 0 {
        events.emit(DomainEvent::new(
            DomainEventKind::RepositoryRenamed,
            DomainEntity::Repository,
            id,
        ));
    }
    Ok(updated)
}

/// USE CASE — set a repository's `optional` flag (the in-DB "nyxignore" override). Emits
/// [`DomainEventKind::RepositoryOptionalSet`] only when a row was updated. Returns rows
/// updated.
pub fn set_repository_optional(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    optional: bool,
    source: DomainEventSource,
) -> QueryResult<usize> {
    let updated = conn.transaction(|conn| {
        let updated = db::set_repository_optional(conn, id, optional)?;
        if updated > 0 {
            if let Some(correlation) = correlate_repository(conn, id)? {
                persist_event(
                    conn,
                    DomainEventKind::RepositoryOptionalSet,
                    DomainEntity::Repository,
                    id,
                    &correlation,
                    source,
                )?;
            }
        }
        Ok::<_, diesel::result::Error>(updated)
    })?;
    if updated > 0 {
        events.emit(DomainEvent::new(
            DomainEventKind::RepositoryOptionalSet,
            DomainEntity::Repository,
            id,
        ));
    }
    Ok(updated)
}

/// Shared body of the two repository soft-flip use cases (archive / unarchive): run the
/// `flip` mutation and, when a row flipped, persist the correlated outbox row — all in ONE
/// transaction. The `flip` is a `db::*` function taking `(conn, id)` and returning the rows
/// updated. The post-commit push is done by the caller (which knows the [`EventSink`]).
fn repository_flip(
    conn: &mut SqliteConnection,
    id: &str,
    kind: DomainEventKind,
    source: DomainEventSource,
    flip: impl Fn(&mut SqliteConnection, &str) -> QueryResult<usize>,
) -> QueryResult<usize> {
    conn.transaction(|conn| {
        let updated = flip(conn, id)?;
        if updated > 0 {
            if let Some(correlation) = correlate_repository(conn, id)? {
                persist_event(
                    conn,
                    kind,
                    DomainEntity::Repository,
                    id,
                    &correlation,
                    source,
                )?;
            }
        }
        Ok(updated)
    })
}

// --- Runtime lifecycle events (timeline follow-up 01KWMCT58832JZW7M5GQDYZZ4B) -----
//
// The workspace timeline (`workspace_timeline`) reads `domain_events` FAITHFULLY, but the
// runtime families the timeline's scope names as its content — command start/stop and agent
// sessions — never transited through the outbox before, so the fil was vide-en-pratique for
// runtime activity. These two functions close that: each writes the STATE mutation
// (`command_instances` / `agent_sessions`) AND its `domain_events` row IN ONE transaction (the
// SAME outbox pattern as the identity use cases), correlated to the workspace so
// `recent_domain_events_for_workspace` surfaces it. The payload is a SMALL business bag
// (id/name/run-state/provider) — NEVER a terminal byte (Q-008; the outbox forbids it).

/// A managed command instance's run-state transition PLUS its outbox event, atomic in one
/// transaction — the runtime writer the production runner sink calls in place of a bare
/// `db::set_run_state`.
///
/// In one transaction it: (1) writes the instance's run state (`db::set_run_state`, the SAME
/// factual-outcome writer as before), then (2) — when the instance resolves — persists a
/// correlated `command.started` (on `running`) or `command.stopped` (on `idle`/`success`/
/// `error`) `domain_events` row so the workspace timeline surfaces the start/stop. The event's
/// `workspace_id`/`project_id` come from the instance's workspace, resolved in the SAME
/// transaction. A rollback erases both; a commit persists both. Unknown instance → the state
/// write is a 0-row no-op and no event is emitted (nothing to correlate).
///
/// `state` is the [`crate::command::RunState`] the runner transitioned to; `exit_code` is the
/// natural exit code for a success/error finish, else `None`. Payload = `{ instance_id,
/// command_id, command_name, run_state, exit_code? }` — a small business bag, NEVER scrollback.
pub fn record_command_run_state(
    conn: &mut SqliteConnection,
    instance_id: &str,
    state: crate::command::RunState,
    exit_code: Option<i32>,
) -> QueryResult<()> {
    use crate::command::RunState;
    conn.transaction(|conn| {
        // (1) The factual run-state write — the SAME single writer the sink used before.
        db::set_run_state(conn, instance_id, state.as_db_str(), exit_code)?;

        // (2) The outbox event, IN this transaction, correlated to the instance's workspace.
        // A `running` transition is a START; leaving `running` (idle/success/error) is a STOP.
        let kind = match state {
            RunState::Running => DomainEventKind::CommandStarted,
            RunState::Idle | RunState::Success | RunState::Error => DomainEventKind::CommandStopped,
        };
        // Resolve the instance → its workspace → correlation. An unknown instance yields no
        // correlation, so no event is emitted (the state write above was a 0-row no-op).
        if let Some(instance) = db::get_instance(conn, instance_id)? {
            if let Some(correlation) = correlate_workspace(conn, &instance.workspace_id)? {
                // The template name is a business label for the fil (best-effort — a missing
                // template leaves it absent, never blocks the event).
                let command_name = db::get_template(conn, &instance.command_id)?.map(|t| t.name);
                let payload = serde_json::json!({
                    "instance_id": instance_id,
                    "command_id": instance.command_id,
                    "command_name": command_name,
                    "run_state": state.as_db_str(),
                    "exit_code": exit_code,
                })
                .to_string();
                persist_event_with_payload(
                    conn,
                    kind,
                    DomainEntity::Command,
                    &payload,
                    &correlation,
                    DomainEventSource::System,
                )?;
            }
        }
        Ok(())
    })
}

/// An agent-session `SessionStart` / `SessionEnd` outbox event, persisted IN the same
/// transaction as the `agent_sessions` mutation the caller just applied (or is applying) — so
/// the workspace timeline surfaces the session start/end.
///
/// The caller (the shared `agent_session_event` MCP tool) has ALREADY resolved the session row
/// (the upsert for a start, the `mark_session_ended` for an end) on THIS connection; this adds
/// the correlated `agent_session.started` / `agent_session.ended` row so both commit together.
/// The event is correlated to the session's `workspace_id` (an unattached session — no
/// workspace — is skipped: it cannot appear on any workspace timeline). `provider` is the
/// `agent_kind` (claude_code/codex/…). Payload = `{ session_id, agent_kind, external_session_id,
/// run_state }` — a small business bag, NEVER a transcript / terminal byte.
///
/// Returns `Ok(())` and is a no-op (no event) when the session has no workspace anchor, so an
/// unattached session never blocks the hook.
pub fn record_agent_session_event(
    conn: &mut SqliteConnection,
    session: &crate::db::AgentSession,
    started: bool,
) -> QueryResult<()> {
    // No workspace anchor → the session cannot be correlated to any workspace timeline; skip.
    let Some(workspace_id) = session.workspace_id.as_deref() else {
        return Ok(());
    };
    let Some(correlation) = correlate_workspace(conn, workspace_id)? else {
        return Ok(());
    };
    let (kind, run_state) = if started {
        (DomainEventKind::AgentSessionStarted, "started")
    } else {
        (DomainEventKind::AgentSessionEnded, "ended")
    };
    let payload = serde_json::json!({
        "session_id": session.id,
        "agent_kind": session.agent_kind,
        "external_session_id": session.external_session_id,
        "run_state": run_state,
    })
    .to_string();
    persist_event_with_payload(
        conn,
        kind,
        DomainEntity::AgentSession,
        &payload,
        &correlation,
        DomainEventSource::System,
    )
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::cell::RefCell;

    /// A test [`EventSink`] that COLLECTS every emitted [`DomainEvent`] so a test can assert
    /// a mutation emitted the expected one (task #8: "prouver que les mutations MCP émettent
    /// bien les events attendus"). `RefCell` (not a lock) is enough — a use case runs on ONE
    /// thread with `&mut SqliteConnection`, so emission is never concurrent.
    #[derive(Default)]
    pub struct RecordingEventSink {
        events: RefCell<Vec<DomainEvent>>,
    }

    impl RecordingEventSink {
        pub fn new() -> Self {
            Self::default()
        }
        /// A snapshot of the events emitted so far, in emission order.
        pub fn events(&self) -> Vec<DomainEvent> {
            self.events.borrow().clone()
        }
        /// The `kind`s emitted so far, in order — the compact form most assertions use.
        pub fn kinds(&self) -> Vec<DomainEventKind> {
            self.events.borrow().iter().map(|e| e.kind).collect()
        }
    }

    impl EventSink for RecordingEventSink {
        fn emit(&self, event: DomainEvent) {
            self.events.borrow_mut().push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::RecordingEventSink;
    use super::*;
    use crate::db::open_in_memory;

    fn root_path(tag: &str) -> String {
        #[cfg(windows)]
        {
            format!("C:\\uc-{tag}")
        }
        #[cfg(not(windows))]
        {
            format!("/uc-{tag}")
        }
    }

    /// A create_project use case creates the project + root and emits ProjectCreated.
    #[test]
    fn create_project_emits_project_created() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("cp"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        assert_eq!(root.project_id, project.id);
        assert_eq!(sink.kinds(), vec![DomainEventKind::ProjectCreated]);
        assert_eq!(sink.events()[0].identity_id, project.id);
        assert_eq!(sink.events()[0].entity, DomainEntity::Project);
    }

    /// register_workspace use case inserts the workspace and emits WorkspaceCreated.
    #[test]
    fn register_workspace_emits_workspace_created() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, _root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("rw"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();

        let ws = register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("rw-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        assert_eq!(ws.project_id, project.id);
        assert!(!ws.is_root);
        assert_eq!(
            sink.kinds(),
            vec![
                DomainEventKind::ProjectCreated,
                DomainEventKind::WorkspaceCreated
            ]
        );
        assert_eq!(sink.events()[1].identity_id, ws.id);
    }

    /// resolve_* key on the stable id and never emit an event.
    #[test]
    fn resolve_by_id_no_event() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("res"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        // Baseline: only the ProjectCreated from setup.
        assert_eq!(sink.kinds(), vec![DomainEventKind::ProjectCreated]);

        assert_eq!(
            resolve_project(&mut conn, &project.id).unwrap().unwrap().id,
            project.id
        );
        assert_eq!(
            resolve_workspace(&mut conn, &root.id).unwrap().unwrap().id,
            root.id
        );
        assert!(resolve_project(&mut conn, "nope").unwrap().is_none());
        // Reads emitted nothing further.
        assert_eq!(sink.kinds(), vec![DomainEventKind::ProjectCreated]);
    }

    /// archive_* emit ONLY when a row actually flips: the first archive emits, an
    /// idempotent re-archive (0 rows) emits nothing.
    #[test]
    fn archive_emits_only_on_a_real_flip() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, _root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("arc"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();

        let n = archive_project(&mut conn, &sink, &project.id, DomainEventSource::Ui).unwrap();
        assert_eq!(n, 1, "first archive flips one row");
        // Re-archive is idempotent: 0 rows, no new event.
        let n2 = archive_project(&mut conn, &sink, &project.id, DomainEventSource::Ui).unwrap();
        assert_eq!(n2, 0, "re-archive is a no-op");
        // An unknown id likewise emits nothing.
        let n3 = archive_project(&mut conn, &sink, "nope", DomainEventSource::Ui).unwrap();
        assert_eq!(n3, 0);

        assert_eq!(
            sink.kinds(),
            vec![
                DomainEventKind::ProjectCreated,
                DomainEventKind::ProjectArchived
            ],
            "exactly one ProjectArchived — the idempotent re-archive did not re-emit"
        );
    }

    /// The full repository use-case surface: create/rename/optional/archive each emit their
    /// kind concerning the repository id.
    #[test]
    fn repository_use_cases_emit_expected_kinds() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, _root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("repo"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();

        let repo = create_repository(
            &mut conn,
            &sink,
            &project.id,
            "api",
            "api",
            false,
            DomainEventSource::Ui,
        )
        .unwrap();
        rename_repository(&mut conn, &sink, &repo.id, "backend", DomainEventSource::Ui).unwrap();
        set_repository_optional(&mut conn, &sink, &repo.id, true, DomainEventSource::Ui).unwrap();
        archive_repository(&mut conn, &sink, &repo.id, DomainEventSource::Ui).unwrap();

        // resolve returns the (now archived) identity — id resolution is path-independent.
        let resolved = resolve_repository(&mut conn, &repo.id).unwrap().unwrap();
        assert_eq!(resolved.name, "backend");
        assert!(resolved.archived_at.is_some());

        assert_eq!(
            sink.kinds(),
            vec![
                DomainEventKind::ProjectCreated,
                DomainEventKind::RepositoryCreated,
                DomainEventKind::RepositoryRenamed,
                DomainEventKind::RepositoryOptionalSet,
                DomainEventKind::RepositoryArchived,
            ]
        );
        // Every repository event concerned THIS repo id.
        for e in sink
            .events()
            .iter()
            .filter(|e| e.entity == DomainEntity::Repository)
        {
            assert_eq!(e.identity_id, repo.id);
        }
    }

    /// set_primary_workspace emits PrimaryWorkspaceSet on a coherent set, and NOTHING when
    /// the invariant is violated (workspace of another project).
    #[test]
    fn set_primary_workspace_emits_only_when_set() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("pw"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let (other, other_root) = create_project(
            &mut conn,
            &sink,
            "Other",
            &root_path("pw2"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();

        // Coherent: root belongs to project → set + emit.
        assert!(set_primary_workspace(
            &mut conn,
            &sink,
            &project.id,
            &root.id,
            DomainEventSource::Ui
        )
        .unwrap());
        // Incoherent: other_root belongs to `other`, not `project` → no write, no emit.
        assert!(!set_primary_workspace(
            &mut conn,
            &sink,
            &project.id,
            &other_root.id,
            DomainEventSource::Ui
        )
        .unwrap());
        let _ = other;

        assert_eq!(
            sink.kinds(),
            vec![
                DomainEventKind::ProjectCreated, // project
                DomainEventKind::ProjectCreated, // other
                DomainEventKind::PrimaryWorkspaceSet,
            ],
            "exactly one PrimaryWorkspaceSet — the cross-project violation emitted nothing"
        );
    }

    /// FREE-WORK model (task `01KWHPV33H0FX8NA4C94T5W46J`): `resolve_primary_workspace`
    /// returns the project's canonical `primary_workspace_id` (the default free-work
    /// workspace), FOLLOWS the pointer when it is moved to a sibling, and NEVER depends on a
    /// Spec (there is no `specs` table in v2.2). An unknown project resolves to `None`.
    #[test]
    fn resolve_primary_workspace_returns_the_canonical_free_work_workspace() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("prim"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();

        // At creation the canonical pointer is anchored at the root: it IS the free-work default.
        let primary = resolve_primary_workspace(&mut conn, &project.id)
            .unwrap()
            .expect("a project always has a primary workspace");
        assert_eq!(
            primary.id, root.id,
            "the primary (free-work) workspace is the root at creation"
        );

        // Moving the canonical pointer to a sibling makes THAT the resolved free-work workspace.
        let sibling = register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("prim-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        assert!(set_primary_workspace(
            &mut conn,
            &sink,
            &project.id,
            &sibling.id,
            DomainEventSource::Ui
        )
        .unwrap());
        let moved = resolve_primary_workspace(&mut conn, &project.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            moved.id, sibling.id,
            "resolve follows the canonical primary_workspace_id pointer"
        );

        // An unknown project resolves to None (no workspace at all).
        assert!(resolve_primary_workspace(&mut conn, "nope")
            .unwrap()
            .is_none());
    }

    /// FALLBACK: a project whose canonical `primary_workspace_id` is NULL (predates the v11
    /// backfill) still resolves its primary via the root workspace (`is_root` first). Proves
    /// the free-work default is available even before the canonical pointer is set.
    #[test]
    fn resolve_primary_workspace_falls_back_to_root_when_pointer_is_null() {
        use diesel::prelude::*;
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("fb"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        // Null out the canonical pointer to simulate a pre-backfill project.
        {
            use crate::schema::projects::dsl as p;
            diesel::update(p::projects.filter(p::id.eq(&project.id)))
                .set(p::primary_workspace_id.eq::<Option<String>>(None))
                .execute(&mut conn)
                .unwrap();
        }
        let primary = resolve_primary_workspace(&mut conn, &project.id)
            .unwrap()
            .expect("root fallback resolves the primary");
        assert_eq!(
            primary.id, root.id,
            "with a NULL pointer, the root workspace is the free-work default"
        );
        assert!(primary.is_root, "the fallback is the root workspace");
    }

    /// ARCHIVED-TARGET GUARD (review #24): when the canonical `primary_workspace_id` points at a
    /// SOFT-ARCHIVED workspace, that target is NOT a valid free-work default, so resolution treats
    /// the pointer as dangling and falls back to the first non-archived candidate (the live root).
    /// This keeps the doc-comment promise ("a soft-archived target FALLS BACK to the root") honest.
    #[test]
    fn resolve_primary_workspace_skips_an_archived_pointer_target() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("arch"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        // A live sibling that will become the primary, then get archived.
        let sibling = register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("arch-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        assert!(set_primary_workspace(
            &mut conn,
            &sink,
            &project.id,
            &sibling.id,
            DomainEventSource::Ui
        )
        .unwrap());
        // Soft-archive the pointed-at primary: it is no longer a valid free-work default.
        assert!(
            archive_workspace(&mut conn, &sink, &sibling.id, DomainEventSource::Ui).unwrap() > 0
        );

        let primary = resolve_primary_workspace(&mut conn, &project.id)
            .unwrap()
            .expect("an archived primary still resolves via the live fallback");
        assert_eq!(
            primary.id, root.id,
            "an archived pointer target is skipped for the live root"
        );
        assert!(
            primary.archived_at.is_none(),
            "the resolved free-work default is never an archived workspace"
        );
    }

    /// remove_workspace: refuses the ROOT workspace and an unknown id (each WITHOUT emitting),
    /// but hard-removes a real non-root workspace and emits exactly one WorkspaceRemoved.
    #[test]
    fn remove_workspace_invariant_and_event() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("rmw"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("rmw-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        // Baseline events from setup: ProjectCreated + WorkspaceCreated.
        assert_eq!(
            sink.kinds(),
            vec![
                DomainEventKind::ProjectCreated,
                DomainEventKind::WorkspaceCreated
            ]
        );

        // Root workspace is refused (invariant) — nothing deleted, nothing emitted.
        let err = remove_workspace(&mut conn, &sink, &root.id, DomainEventSource::Ui).unwrap_err();
        assert!(matches!(err, RemoveWorkspaceError::IsRoot));
        assert!(
            resolve_workspace(&mut conn, &root.id).unwrap().is_some(),
            "root still present after a refused removal"
        );

        // An unknown id is a NotFound — nothing emitted.
        let err = remove_workspace(&mut conn, &sink, "nope", DomainEventSource::Ui).unwrap_err();
        assert!(matches!(err, RemoveWorkspaceError::NotFound));

        // The two refusals emitted nothing beyond the setup baseline.
        assert_eq!(
            sink.kinds(),
            vec![
                DomainEventKind::ProjectCreated,
                DomainEventKind::WorkspaceCreated
            ]
        );

        // A real non-root workspace is removed: 1 row, one WorkspaceRemoved concerning its id.
        let n = remove_workspace(&mut conn, &sink, &ws.id, DomainEventSource::Ui).unwrap();
        assert_eq!(n, 1);
        assert!(
            resolve_workspace(&mut conn, &ws.id).unwrap().is_none(),
            "the non-root workspace row is gone"
        );
        assert_eq!(
            sink.kinds(),
            vec![
                DomainEventKind::ProjectCreated,
                DomainEventKind::WorkspaceCreated,
                DomainEventKind::WorkspaceRemoved,
            ]
        );
        let last = sink.events().pop().unwrap();
        assert_eq!(last.entity, DomainEntity::Workspace);
        assert_eq!(last.identity_id, ws.id);

        // Re-removing the now-gone workspace is NotFound (zero-row path), still no new event.
        let err = remove_workspace(&mut conn, &sink, &ws.id, DomainEventSource::Ui).unwrap_err();
        assert!(matches!(err, RemoveWorkspaceError::NotFound));
        assert_eq!(
            sink.kinds(),
            vec![
                DomainEventKind::ProjectCreated,
                DomainEventKind::WorkspaceCreated,
                DomainEventKind::WorkspaceRemoved,
            ]
        );
    }

    /// Review 01KWEX11 — removing a non-root workspace is a FULL destructive runtime cascade:
    /// its terminals AND agent sessions are DELETED (not detached into the orphaned "loose"
    /// bucket), its command instances stay cascaded, and a SIBLING workspace's runtime is
    /// untouched. Proves done_criteria #1 (terminals + sessions gone, no loose orphan), #3
    /// (command_instances still cascade), and #7 (no collateral on other rows).
    #[test]
    fn remove_workspace_hard_deletes_terminals_and_agent_sessions() {
        use crate::schema::{agent_sessions, terminals};
        use diesel::prelude::*;

        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, _root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("rmw-rt"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        // The workspace under test + a SIBLING (its runtime must survive the removal).
        let ws = register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("rmw-rt-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        let sibling = register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "sib",
            &root_path("rmw-rt-sib"),
            DomainEventSource::Ui,
        )
        .unwrap();

        // A managed-command template materializes one instance per workspace (ws + sibling +
        // root) — the `command_instances` FK is already ON DELETE CASCADE.
        db::create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            crate::db::CommandSource {
                source_kind: None,
                source_package_json_path: None,
                source_script_name: None,
                source_script_command_snapshot: None,
                package_manager: None,
            },
        )
        .unwrap();
        assert!(
            !db::instance_ids_for_workspace(&mut conn, &ws.id)
                .unwrap()
                .is_empty(),
            "the workspace has at least one command instance to cascade"
        );

        // Two terminals attached to `ws`; one hosts a live agent session anchored to `ws`.
        let t1 = db::create_terminal(&mut conn, &root_path("rmw-rt-wt"), None).unwrap();
        let t2 = db::create_terminal(&mut conn, &root_path("rmw-rt-wt"), None).unwrap();
        db::attach_terminal(&mut conn, &t1.id, &ws.id, "auto").unwrap();
        db::attach_terminal(&mut conn, &t2.id, &ws.id, "auto").unwrap();
        db::record_session_start(
            &mut conn,
            &t1.id,
            "claude_code",
            db::SessionCapture {
                workspace_id: Some(ws.id.clone()),
                external_session_id: "sess-on-ws-terminal".to_string(),
                cwd: root_path("rmw-rt-wt"),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();

        // A LOOSE terminal (not attached to ws) hosting a session that is nonetheless ANCHORED
        // to ws by workspace_id — this is exactly the row that must NOT survive as an orphan.
        let loose = db::create_terminal(&mut conn, &root_path("loose"), None).unwrap();
        db::record_session_start(
            &mut conn,
            &loose.id,
            "claude_code",
            db::SessionCapture {
                workspace_id: Some(ws.id.clone()),
                external_session_id: "sess-anchored-to-ws-on-loose-term".to_string(),
                cwd: root_path("loose"),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();

        // A terminal + session on the SIBLING — must survive untouched.
        let st = db::create_terminal(&mut conn, &root_path("rmw-rt-sib"), None).unwrap();
        db::attach_terminal(&mut conn, &st.id, &sibling.id, "auto").unwrap();
        db::record_session_start(
            &mut conn,
            &st.id,
            "claude_code",
            db::SessionCapture {
                workspace_id: Some(sibling.id.clone()),
                external_session_id: "sess-on-sibling".to_string(),
                cwd: root_path("rmw-rt-sib"),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();

        // Count helpers.
        let terminals_for = |conn: &mut SqliteConnection, wid: &str| -> i64 {
            terminals::table
                .filter(terminals::workspace_id.eq(wid))
                .count()
                .get_result(conn)
                .unwrap()
        };
        let sessions_for = |conn: &mut SqliteConnection, wid: &str| -> i64 {
            agent_sessions::table
                .filter(agent_sessions::workspace_id.eq(wid))
                .count()
                .get_result(conn)
                .unwrap()
        };
        let loose_terminals = |conn: &mut SqliteConnection| -> i64 {
            terminals::table
                .filter(terminals::workspace_id.is_null())
                .count()
                .get_result(conn)
                .unwrap()
        };

        // Baseline before removal: ws owns 2 terminals + 2 anchored sessions; the loose
        // terminal is the only workspace_id-null terminal.
        assert_eq!(terminals_for(&mut conn, &ws.id), 2);
        assert_eq!(sessions_for(&mut conn, &ws.id), 2);
        assert_eq!(loose_terminals(&mut conn), 1, "only the loose terminal");

        // ACT: remove the non-root workspace.
        let n = remove_workspace(&mut conn, &sink, &ws.id, DomainEventSource::Ui).unwrap();
        assert_eq!(n, 1, "one workspace row removed");

        // done_criterion #1 — its terminals + sessions are DELETED (0 rows), NOT detached: the
        // loose-terminal count did NOT grow, so nothing fell into the orphaned bucket.
        assert_eq!(terminals_for(&mut conn, &ws.id), 0, "ws terminals deleted");
        assert_eq!(sessions_for(&mut conn, &ws.id), 0, "ws sessions deleted");
        assert_eq!(
            loose_terminals(&mut conn),
            1,
            "no ws terminal was detached into the loose bucket"
        );
        // The loose terminal itself survives (it belonged to no workspace); only its
        // ws-anchored session was reaped (both ws sessions are gone, asserted above).
        assert!(
            db::get_terminal(&mut conn, &loose.id).unwrap().is_some(),
            "the loose terminal record survives; only its ws-anchored session was reaped"
        );

        // done_criterion #3 — command_instances of ws cascaded away.
        assert!(
            db::instance_ids_for_workspace(&mut conn, &ws.id)
                .unwrap()
                .is_empty(),
            "ws command instances cascaded"
        );

        // done_criterion #7 — the SIBLING's runtime is untouched.
        assert_eq!(
            terminals_for(&mut conn, &sibling.id),
            1,
            "sibling terminal kept"
        );
        assert_eq!(
            sessions_for(&mut conn, &sibling.id),
            1,
            "sibling session kept"
        );
        assert!(
            !db::instance_ids_for_workspace(&mut conn, &sibling.id)
                .unwrap()
                .is_empty(),
            "sibling command instances kept"
        );

        // done_criterion #4 — exactly one WorkspaceRemoved event for the removed workspace.
        let removed_events: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|e| e.kind == DomainEventKind::WorkspaceRemoved)
            .collect();
        assert_eq!(removed_events.len(), 1);
        assert_eq!(removed_events[0].identity_id, ws.id);
    }

    /// Review 01KWEX11 done_criterion #7 — the `folder_removed_at` soft marker (worktree
    /// folder cleaned up) is a DISTINCT concept from a destructive workspace removal: it must
    /// leave the workspace row AND its runtime (terminals) fully intact.
    #[test]
    fn folder_removed_marker_does_not_reap_workspace_runtime() {
        use crate::schema::terminals;
        use diesel::prelude::*;

        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, _root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("frm"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("frm-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        let t = db::create_terminal(&mut conn, &root_path("frm-wt"), None).unwrap();
        db::attach_terminal(&mut conn, &t.id, &ws.id, "auto").unwrap();

        // Mark the folder removed (soft): the workspace row stays, its terminal stays attached.
        let flipped = db::mark_workspace_folder_removed(&mut conn, &ws.id).unwrap();
        assert_eq!(flipped, 1, "the folder_removed_at marker flipped one row");
        assert!(
            resolve_workspace(&mut conn, &ws.id).unwrap().is_some(),
            "the workspace row survives a folder-removed marker (soft, not a delete)"
        );
        let attached: i64 = terminals::table
            .filter(terminals::workspace_id.eq(&ws.id))
            .count()
            .get_result(&mut conn)
            .unwrap();
        assert_eq!(
            attached, 1,
            "folder_removed_at does NOT reap the workspace's terminals"
        );
    }

    /// rename_workspace emits WorkspaceRenamed and persists a correlated `workspace.renamed`
    /// outbox row in the SAME transaction, only on a real update (unknown id is a no-op).
    #[test]
    fn rename_workspace_emits_and_persists_correlated_event() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, _root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("rnw"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("rnw-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        let baseline = db::list_domain_events(&mut conn).unwrap().len();

        // A real rename: 1 row, one WorkspaceRenamed concerning the workspace id.
        let n =
            rename_workspace(&mut conn, &sink, &ws.id, "renamed", DomainEventSource::Ui).unwrap();
        assert_eq!(n, 1, "the rename flipped one row");
        assert_eq!(
            resolve_workspace(&mut conn, &ws.id).unwrap().unwrap().name,
            "renamed",
            "the new name is persisted"
        );

        // An unknown id is a no-op — nothing renamed, nothing emitted/persisted.
        let n0 = rename_workspace(&mut conn, &sink, "nope", "x", DomainEventSource::Ui).unwrap();
        assert_eq!(n0, 0);

        // The sink saw exactly one WorkspaceRenamed (after the setup baseline).
        assert_eq!(
            sink.kinds(),
            vec![
                DomainEventKind::ProjectCreated,
                DomainEventKind::WorkspaceCreated,
                DomainEventKind::WorkspaceRenamed,
            ],
            "exactly one WorkspaceRenamed — the unknown id emitted nothing"
        );

        // The outbox persisted exactly one correlated `workspace.renamed` row, in the same tx.
        let events = db::list_domain_events(&mut conn).unwrap();
        assert_eq!(
            events.len(),
            baseline + 1,
            "only the real rename persisted a domain_event"
        );
        let renamed = events.last().unwrap();
        assert_eq!(renamed.event_type, "workspace.renamed");
        assert_eq!(renamed.aggregate, "workspace");
        assert_eq!(renamed.project_id, project.id, "correlated to its project");
        assert_eq!(
            renamed.workspace_id.as_deref(),
            Some(ws.id.as_str()),
            "correlated to the renamed workspace"
        );
        assert_eq!(renamed.source, "ui");
    }

    // --- Phase 4 task #9: transactional outbox persistence ------------------

    /// done_criterion #9 — "Toute mutation d'identité émet un domain_event corrélé, dans la
    /// même transaction." Each identity FAMILY (create / archive / rename / set_optional /
    /// set_primary / register / remove) persists ONE `domain_events` row, correlated
    /// (project_id always; workspace_id when workspace-scoped) and typed, in the SAME
    /// transaction as its mutation. The persisted rows are asserted directly against the
    /// `domain_events` table (not just the push sink) — that IS the outbox.
    #[test]
    fn every_identity_family_persists_a_correlated_domain_event() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();

        // project.created (+ its root workspace) — project-scoped.
        let (project, root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("dev"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        // workspace.created — workspace-scoped, MCP source.
        let ws = register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("dev-wt"),
            DomainEventSource::Mcp,
        )
        .unwrap();
        // workspace.primary_set — concerns a workspace.
        assert!(set_primary_workspace(
            &mut conn,
            &sink,
            &project.id,
            &ws.id,
            DomainEventSource::Ui
        )
        .unwrap());
        // repository family: create / rename / optional / archive / unarchive.
        let repo = create_repository(
            &mut conn,
            &sink,
            &project.id,
            "api",
            "api",
            false,
            DomainEventSource::Mcp,
        )
        .unwrap();
        rename_repository(&mut conn, &sink, &repo.id, "backend", DomainEventSource::Ui).unwrap();
        set_repository_optional(&mut conn, &sink, &repo.id, true, DomainEventSource::Ui).unwrap();
        archive_repository(&mut conn, &sink, &repo.id, DomainEventSource::Ui).unwrap();
        unarchive_repository(&mut conn, &sink, &repo.id, DomainEventSource::Ui).unwrap();
        // project archive/unarchive family.
        archive_project(&mut conn, &sink, &project.id, DomainEventSource::Ui).unwrap();
        unarchive_project(&mut conn, &sink, &project.id, DomainEventSource::Ui).unwrap();
        // workspace archive/unarchive family (on the added workspace, never a root).
        archive_workspace(&mut conn, &sink, &ws.id, DomainEventSource::Ui).unwrap();
        unarchive_workspace(&mut conn, &sink, &ws.id, DomainEventSource::Ui).unwrap();
        // workspace.removed — the row is deleted; the event is still correlated.
        remove_workspace(&mut conn, &sink, &ws.id, DomainEventSource::Ui).unwrap();

        let events = db::list_domain_events(&mut conn).expect("list domain_events");
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "project.created",
                "workspace.created",
                "workspace.primary_set",
                "repository.created",
                "repository.renamed",
                "repository.optional_set",
                "repository.archived",
                "repository.unarchived",
                "project.archived",
                "project.unarchived",
                "workspace.archived",
                "workspace.unarchived",
                "workspace.removed",
            ],
            "every identity family persisted its typed domain_event, in emission order"
        );

        // Correlation: project events carry the project_id and no workspace; workspace events
        // carry BOTH; repository events carry the project_id and no workspace.
        for e in &events {
            assert_eq!(
                e.project_id, project.id,
                "every event correlates to the project"
            );
            match e.aggregate.as_str() {
                "project" | "repository" => assert!(
                    e.workspace_id.is_none(),
                    "{} event has no workspace correlation",
                    e.aggregate
                ),
                "workspace" => {
                    // The root's `workspace.primary_set`? No — primary was set to `ws`.
                    // All workspace events here concern `ws` (created/primary/archive/remove).
                    assert_eq!(
                        e.workspace_id.as_deref(),
                        Some(ws.id.as_str()),
                        "workspace event correlates to the concerned workspace"
                    );
                }
                other => panic!("unexpected aggregate {other}"),
            }
        }
        // The root workspace exists (never removed) — sanity that project scoping is right.
        assert_eq!(root.project_id, project.id);

        // Sequence is strictly monotonic across the whole outbox.
        let seqs: Vec<i64> = events.iter().map(|e| e.sequence).collect();
        assert!(
            seqs.windows(2).all(|w| w[1] > w[0]),
            "domain_events.sequence is strictly increasing (monotonic global order)"
        );

        // Source is persisted verbatim per call (ui vs mcp).
        let created = &events[0];
        assert_eq!(created.source, "ui", "create_project came from the UI");
        let ws_created = &events[1];
        assert_eq!(ws_created.source, "mcp", "register_workspace came from MCP");
    }

    /// done_criterion #9 (transactionality) — a mutation that ROLLS BACK persists NO
    /// domain_event. We wrap `create_project` (which internally records its outbox row in
    /// the same transaction) in an OUTER transaction that then fails: SQLite rolls back the
    /// whole thing, so neither the project NOR its `domain_events` row survive. This is the
    /// outbox's atomicity guarantee: the event exists IFF the mutation committed.
    #[test]
    fn a_rolled_back_mutation_persists_no_domain_event() {
        use diesel::result::Error as DieselError;
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();

        // Baseline: empty outbox.
        assert!(db::list_domain_events(&mut conn).unwrap().is_empty());

        // Run the mutation inside an OUTER transaction we deliberately abort. The use case
        // opens its own (nested, SAVEPOINT) transaction and records the outbox row; the outer
        // rollback erases everything — the mutation AND the event.
        let outcome: Result<(), DieselError> = conn.transaction(|conn| {
            let (project, _root) = create_project(
                conn,
                &sink,
                "Doomed",
                &root_path("rb"),
                None,
                DomainEventSource::Ui,
            )?;
            // Within the transaction the row IS visible (the outbox write happened)…
            let mid = db::list_domain_events(conn).unwrap();
            assert_eq!(mid.len(), 1, "the outbox row exists inside the transaction");
            assert_eq!(mid[0].project_id, project.id);
            // …now force a rollback by returning an error.
            Err(DieselError::RollbackTransaction)
        });
        assert!(outcome.is_err(), "the outer transaction was rolled back");

        // After the rollback: NO project, and — the point — NO domain_event.
        assert!(
            list_projects(&mut conn).unwrap().is_empty(),
            "the mutation rolled back (no project)"
        );
        assert!(
            db::list_domain_events(&mut conn).unwrap().is_empty(),
            "transactionality: a rolled-back mutation left NO domain_event"
        );
    }

    /// done_criterion #9 (no phantom event) — a mutation that DID NOT change a row (an
    /// idempotent re-archive, an unknown id, a cross-project set_primary violation) persists
    /// NO domain_event. The outbox row is written ONLY when a row actually flipped, so the
    /// log never carries a phantom event for a no-op.
    #[test]
    fn a_no_op_mutation_persists_no_domain_event() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let (project, root) = create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("noop"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let (other, other_root) = create_project(
            &mut conn,
            &sink,
            "Other",
            &root_path("noop2"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let baseline = db::list_domain_events(&mut conn).unwrap().len();
        assert_eq!(baseline, 2, "two project.created rows so far");

        // Idempotent re-archive (first flips, second is a no-op).
        archive_project(&mut conn, &sink, &project.id, DomainEventSource::Ui).unwrap();
        archive_project(&mut conn, &sink, &project.id, DomainEventSource::Ui).unwrap();
        // Unknown id: no row.
        archive_project(&mut conn, &sink, "nope", DomainEventSource::Ui).unwrap();
        // Cross-project set_primary violation: no write.
        assert!(!set_primary_workspace(
            &mut conn,
            &sink,
            &project.id,
            &other_root.id,
            DomainEventSource::Ui
        )
        .unwrap());
        let _ = (other, root);

        let events = db::list_domain_events(&mut conn).unwrap();
        // Exactly ONE new event beyond baseline: the single real archive flip.
        assert_eq!(
            events.len(),
            baseline + 1,
            "only the real flip persisted an event — no phantom for the no-ops"
        );
        assert_eq!(events.last().unwrap().event_type, "project.archived");
    }

    // --- Runtime lifecycle events (timeline follow-up 01KWMCT58832JZW7M5GQDYZZ4B) --------
    //
    // Prove the two runtime families now transit through the outbox, correlated to the
    // workspace, with a small business payload and NEVER a terminal byte — so the timeline
    // read model (which reads `domain_events` only) surfaces them.

    /// Materialize a project + root workspace + one command instance in the root workspace.
    fn seed_command_instance(conn: &mut SqliteConnection, tag: &str) -> (String, String, String) {
        let (project, root) = create_project(
            conn,
            &NullEventSink,
            "Nyx",
            &root_path(tag),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        db::create_template(
            conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            db::CommandSource::default(),
        )
        .unwrap();
        let instance_id = db::instance_ids_for_workspace(conn, &root.id)
            .unwrap()
            .pop()
            .expect("the template materialized one instance in the root workspace");
        (project.id, root.id, instance_id)
    }

    /// Starting a command emits a `command.started` outbox row correlated to the workspace,
    /// with a business payload and no terminal byte; stopping emits `command.stopped`. The
    /// timeline read model (reading `domain_events`) then surfaces both.
    #[test]
    fn command_start_and_stop_emit_correlated_domain_events() {
        use crate::command::RunState;
        let mut conn = open_in_memory();
        let (project_id, ws_id, instance_id) = seed_command_instance(&mut conn, "cmd-ev");
        let baseline = db::list_domain_events(&mut conn).unwrap().len();

        // START → running.
        record_command_run_state(&mut conn, &instance_id, RunState::Running, None).unwrap();
        // STOP → idle (a plain stop, no exit code).
        record_command_run_state(&mut conn, &instance_id, RunState::Idle, None).unwrap();

        let events = db::list_domain_events(&mut conn).unwrap();
        assert_eq!(
            events.len(),
            baseline + 2,
            "exactly one event per transition (start + stop)"
        );
        let started = &events[baseline];
        let stopped = &events[baseline + 1];
        assert_eq!(started.event_type, "command.started");
        assert_eq!(stopped.event_type, "command.stopped");
        // Correlated to the workspace + project so the timeline read model surfaces them.
        for e in [started, stopped] {
            assert_eq!(e.workspace_id.as_deref(), Some(ws_id.as_str()));
            assert_eq!(e.project_id, project_id);
            assert_eq!(e.aggregate, "command");
            assert_eq!(e.source, "system");
            // Business payload — the instance/command identity + run state, NEVER scrollback.
            assert!(e.payload.contains(&instance_id));
            assert!(e.payload.contains("\"command_name\":\"dev\""));
            assert!(!e.payload.to_lowercase().contains("scrollback"));
            assert!(!e.payload.contains("\\x1b")); // no terminal escape bytes
        }
        assert!(started.payload.contains("\"run_state\":\"running\""));
        assert!(stopped.payload.contains("\"run_state\":\"idle\""));

        // The FACTUAL run-state write still happened atomically with the event.
        let inst = db::get_instance(&mut conn, &instance_id).unwrap().unwrap();
        assert_eq!(inst.last_state, "idle");

        // And the timeline read model surfaces both, oldest-first (done_criterion: the fil is
        // no longer vide-en-pratique for runtime activity).
        let tl = crate::workspace_timeline::timeline(&mut conn, &ws_id, 50)
            .unwrap()
            .unwrap();
        let kinds: Vec<&str> = tl.entries.iter().map(|e| e.event_type.as_str()).collect();
        assert!(kinds.contains(&"command.started"));
        assert!(kinds.contains(&"command.stopped"));
    }

    /// A natural success/error exit is a STOP too (leaving `running`), carrying the exit code.
    #[test]
    fn command_natural_exit_emits_command_stopped_with_exit_code() {
        use crate::command::RunState;
        let mut conn = open_in_memory();
        let (_project_id, ws_id, instance_id) = seed_command_instance(&mut conn, "cmd-exit");
        record_command_run_state(&mut conn, &instance_id, RunState::Running, None).unwrap();
        record_command_run_state(&mut conn, &instance_id, RunState::Error, Some(2)).unwrap();

        let events = db::list_domain_events(&mut conn).unwrap();
        let last = events.last().unwrap();
        assert_eq!(last.event_type, "command.stopped");
        assert!(last.payload.contains("\"exit_code\":2"));
        assert!(last.payload.contains("\"run_state\":\"error\""));
        let _ = ws_id;
    }

    /// An unknown instance → the state write is a no-op and NO event is emitted (nothing to
    /// correlate). No phantom timeline row.
    #[test]
    fn command_run_state_unknown_instance_emits_nothing() {
        use crate::command::RunState;
        let mut conn = open_in_memory();
        let baseline = db::list_domain_events(&mut conn).unwrap().len();
        record_command_run_state(&mut conn, "nope", RunState::Running, None).unwrap();
        assert_eq!(db::list_domain_events(&mut conn).unwrap().len(), baseline);
    }

    /// An agent SessionStart / SessionEnd emits a correlated `agent_session.started` /
    /// `agent_session.ended` outbox row (workspace-correlated, business payload, no transcript
    /// byte) so the timeline surfaces the session lifecycle.
    #[test]
    fn agent_session_start_and_end_emit_correlated_domain_events() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(
            &mut conn,
            &NullEventSink,
            "Nyx",
            &root_path("agent-ev"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let term = db::create_terminal(&mut conn, &root_path("agent-ev"), None).unwrap();
        db::attach_terminal(&mut conn, &term.id, &root.id, "auto").unwrap();
        let baseline = db::list_domain_events(&mut conn).unwrap().len();

        // Start: upsert the session row, then emit the correlated started event (as the tool
        // does inside ONE transaction).
        let session = db::record_session_start(
            &mut conn,
            &term.id,
            "claude_code",
            db::SessionCapture {
                workspace_id: Some(root.id.clone()),
                external_session_id: "ext-sess-1".to_string(),
                cwd: root_path("agent-ev"),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        record_agent_session_event(&mut conn, &session, true).unwrap();

        // End: mark ended, re-read, emit the ended event.
        db::mark_session_ended(&mut conn, &session.id).unwrap();
        let ended = db::get_session(&mut conn, &session.id).unwrap().unwrap();
        record_agent_session_event(&mut conn, &ended, false).unwrap();

        let events = db::list_domain_events(&mut conn).unwrap();
        assert_eq!(events.len(), baseline + 2);
        let started = &events[baseline];
        let ended_ev = &events[baseline + 1];
        assert_eq!(started.event_type, "agent_session.started");
        assert_eq!(ended_ev.event_type, "agent_session.ended");
        for e in [started, ended_ev] {
            assert_eq!(e.workspace_id.as_deref(), Some(root.id.as_str()));
            assert_eq!(e.project_id, project.id);
            assert_eq!(e.aggregate, "agent_session");
            assert_eq!(e.source, "system");
            assert!(e.payload.contains(&session.id));
            assert!(e.payload.contains("\"agent_kind\":\"claude_code\""));
            assert!(e.payload.contains("ext-sess-1"));
            // No transcript / terminal byte in the business bag.
            assert!(!e.payload.to_lowercase().contains("transcript"));
            assert!(!e.payload.contains("\\x1b"));
        }

        // Timeline surfaces both.
        let tl = crate::workspace_timeline::timeline(&mut conn, &root.id, 50)
            .unwrap()
            .unwrap();
        let kinds: Vec<&str> = tl.entries.iter().map(|e| e.event_type.as_str()).collect();
        assert!(kinds.contains(&"agent_session.started"));
        assert!(kinds.contains(&"agent_session.ended"));
    }

    /// An UNATTACHED session (no workspace anchor) emits nothing — it cannot appear on any
    /// workspace timeline, and must never block the hook.
    #[test]
    fn agent_session_without_workspace_emits_nothing() {
        let mut conn = open_in_memory();
        let term = db::create_terminal(&mut conn, &root_path("agent-loose"), None).unwrap();
        let session = db::record_session_start(
            &mut conn,
            &term.id,
            "claude_code",
            db::SessionCapture {
                workspace_id: None,
                external_session_id: "ext-loose".to_string(),
                cwd: root_path("agent-loose"),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        let baseline = db::list_domain_events(&mut conn).unwrap().len();
        record_agent_session_event(&mut conn, &session, true).unwrap();
        assert_eq!(db::list_domain_events(&mut conn).unwrap().len(), baseline);
    }

    /// DONE-CRITERION (« Frontière de domaine ») — the boundary holds INSIDE the use-case layer,
    /// not only at the two adapters. A caller that reaches these functions directly — a future
    /// bridge, a test, a graph node — gets the same refusal, and nothing is written.
    ///
    /// Both directions again: every verb below refuses the SINGLETON and accepts the equivalent
    /// USER container, so the guard is a boundary and not a broken function.
    #[test]
    fn the_identity_use_cases_refuse_the_system_scope_and_write_nothing() {
        let mut conn = open_in_memory();
        let sink = RecordingEventSink::new();
        let scope = crate::system_scope::ensure_system_scope(
            &mut conn,
            &crate::system_scope::FixedHomeDirProvider::at(root_path("home")),
        )
        .expect("bootstrap the Système scope");
        let (user, user_root) =
            create_project(&mut conn, &sink, "app", &root_path("app"), None, DomainEventSource::Ui).unwrap();
        let baseline = db::list_domain_events(&mut conn).unwrap().len();

        let forbidden = |e: SystemScopeError, what: &str| {
            assert!(e.is_forbidden(), "{what}: {e}");
            assert_eq!(e.code(), crate::system_scope::SYSTEM_SCOPE_FORBIDDEN);
        };

        // --- the PROJECT's own lifecycle ------------------------------------
        forbidden(
            archive_project(&mut conn, &sink, &scope.project.id, DomainEventSource::Ui).unwrap_err(),
            "archive_project",
        );
        forbidden(
            unarchive_project(&mut conn, &sink, &scope.project.id, DomainEventSource::Ui).unwrap_err(),
            "unarchive_project",
        );
        forbidden(
            set_primary_workspace(
                &mut conn,
                &sink,
                &scope.project.id,
                &user_root.id,
                DomainEventSource::Ui,
            )
            .unwrap_err(),
            "set_primary_workspace",
        );
        // …and from the WORKSPACE side too: a user project may not adopt the Système workspace
        // as its primary either.
        forbidden(
            set_primary_workspace(&mut conn, &sink, &user.id, &scope.workspace.id, DomainEventSource::Ui)
                .unwrap_err(),
            "set_primary_workspace (workspace side)",
        );

        // --- the WORKSPACE lifecycle ----------------------------------------
        match register_workspace_anchored(
            &mut conn,
            &sink,
            &scope.project.id,
            "extra",
            &root_path("extra"),
            None,
            DomainEventSource::Ui,
        ) {
            Err(RegisterWorkspaceError::SystemScope(e)) => forbidden(e, "register_workspace"),
            other => panic!("a SECOND Système workspace must be refused, got {other:?}"),
        }
        forbidden(
            rename_workspace(&mut conn, &sink, &scope.workspace.id, "hack", DomainEventSource::Ui).unwrap_err(),
            "rename_workspace",
        );
        forbidden(
            archive_workspace(&mut conn, &sink, &scope.workspace.id, DomainEventSource::Ui).unwrap_err(),
            "archive_workspace",
        );
        forbidden(
            unarchive_workspace(&mut conn, &sink, &scope.workspace.id, DomainEventSource::Ui).unwrap_err(),
            "unarchive_workspace",
        );
        match remove_workspace(&mut conn, &sink, &scope.workspace.id, DomainEventSource::Ui) {
            Err(RemoveWorkspaceError::SystemScope(e)) => forbidden(e, "remove_workspace"),
            other => panic!("the Système workspace is not removable, got {other:?}"),
        }
        forbidden(
            list_workspaces(&mut conn, &scope.project.id).unwrap_err(),
            "list_workspaces",
        );

        // --- REPOSITORIES: the scope is closed to Git end to end -------------
        forbidden(
            create_repository(&mut conn, &sink, &scope.project.id, "api", "api", false, DomainEventSource::Ui)
                .unwrap_err(),
            "create_repository",
        );

        // NOTHING WAS WRITTEN: no outbox row, and the scope is byte-for-byte what it was.
        assert_eq!(
            db::list_domain_events(&mut conn).unwrap().len(),
            baseline,
            "a refusal must not persist a domain event"
        );
        assert_eq!(
            crate::system_scope::load(&mut conn).unwrap(),
            Some(scope.clone()),
            "a refusal must not touch the scope"
        );

        // --- THE CONTROL: the same verbs work on the USER container ---------
        assert_eq!(list_workspaces(&mut conn, &user.id).unwrap().len(), 1);
        assert_eq!(
            rename_workspace(&mut conn, &sink, &user_root.id, "renamed", DomainEventSource::Ui).unwrap(),
            1
        );
        assert_eq!(
            archive_project(&mut conn, &sink, &user.id, DomainEventSource::Ui).unwrap(),
            1
        );
        assert_eq!(
            unarchive_project(&mut conn, &sink, &user.id, DomainEventSource::Ui).unwrap(),
            1
        );
        create_repository(&mut conn, &sink, &user.id, "api", "api", false, DomainEventSource::Ui)
            .expect("a user project may declare a repository");
    }
}
