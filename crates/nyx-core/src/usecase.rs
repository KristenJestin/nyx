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
}

impl DomainEntity {
    pub fn as_str(self) -> &'static str {
        match self {
            DomainEntity::Project => "project",
            DomainEntity::Workspace => "workspace",
            DomainEntity::Repository => "repository",
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
}

impl DomainEventSource {
    /// The stable wire/DB string (`ui` | `mcp`).
    pub fn as_str(self) -> &'static str {
        match self {
            DomainEventSource::Ui => "ui",
            DomainEventSource::Mcp => "mcp",
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
/// is what matters. Kept crate-private: only the use cases below persist events.
fn persist_event(
    conn: &mut SqliteConnection,
    kind: DomainEventKind,
    entity: DomainEntity,
    identity_id: &str,
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
            payload: build_payload(identity_id),
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
pub fn archive_project(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> QueryResult<usize> {
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
pub fn unarchive_project(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> QueryResult<usize> {
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
pub fn set_primary_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    project_id: &str,
    workspace_id: &str,
    source: DomainEventSource,
) -> QueryResult<bool> {
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
/// repositories. The single orchestration both the UI (`create_workspace`) and the MCP
/// (`workspace_add` / `create_workspace`) funnel into once the on-disk path is ready
/// (each adapter owns its own path precondition — validate-existing vs `mkdir -p`). It
/// composes [`crate::repository::register_workspace`] (insert + auto-detect) and emits a
/// [`DomainEventKind::WorkspaceCreated`] on success. Returns the new workspace.
pub fn register_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    project_id: &str,
    name: &str,
    path: &str,
    source: DomainEventSource,
) -> QueryResult<Workspace> {
    let workspace = conn.transaction(|conn| {
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
        Ok::<_, diesel::result::Error>(workspace)
    })?;
    events.emit(DomainEvent::new(
        DomainEventKind::WorkspaceCreated,
        DomainEntity::Workspace,
        &workspace.id,
    ));
    Ok(workspace)
}

/// USE CASE — RENAME a workspace's display `name` (a human label; the identity `path` is
/// untouched — it identifies the folder). The single orchestration both the UI
/// (`rename_workspace`) and any MCP caller funnel into, mirroring [`rename_repository`]:
/// inside ONE transaction it renames (`db::rename_workspace`) and, when a row was updated,
/// persists a correlated [`DomainEventKind::WorkspaceRenamed`] outbox row (task #9); after
/// commit it emits the domain event through the injected sink (task #10). An unknown id is a
/// no-op (0 rows) that writes and emits nothing. Returns rows updated.
pub fn rename_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    name: &str,
    source: DomainEventSource,
) -> QueryResult<usize> {
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
pub fn list_workspaces(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<Vec<Workspace>> {
    db::list_workspaces(conn, project_id)
}

/// USE CASE — resolve ONE workspace by its stable id, or `None`. No event (a read).
pub fn resolve_workspace(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<Workspace>> {
    db::get_workspace(conn, id)
}

/// USE CASE — soft-archive a workspace (the ROW is permanent — PDR-0008 — archiving marks
/// it inactive without deleting it). Emits [`DomainEventKind::WorkspaceArchived`] only when
/// a row flipped. Returns rows updated.
pub fn archive_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> QueryResult<usize> {
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
pub fn unarchive_workspace(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    id: &str,
    source: DomainEventSource,
) -> QueryResult<usize> {
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
    /// A DB error surfaced while resolving or deleting.
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for RemoveWorkspaceError {
    fn from(e: diesel::result::Error) -> Self {
        RemoveWorkspaceError::Db(e)
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
        // single `workspace.removed` event below is the coarse signal both adapters push on
        // (the UI re-pulls the terminals/sessions on that invalidation). The KILL of any LIVE
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
pub fn create_repository(
    conn: &mut SqliteConnection,
    events: &dyn EventSink,
    project_id: &str,
    name: &str,
    relative_path: &str,
    optional: bool,
    source: DomainEventSource,
) -> QueryResult<Repository> {
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
}
