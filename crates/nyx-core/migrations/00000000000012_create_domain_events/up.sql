-- Schema v12: the `domain_events` OUTBOX table (v2.1 Phase 4, task
-- 01KWDCNDJZ6KTVX36D4401KK89 — "domain_events : table + émission métier
-- transactionnelle"). The persistent, monotonically-ordered log of business
-- mutations of the structuring identities (project / workspace / repository) that
-- Phase 3 already emits through the `usecase::EventSink` seam — now given a real,
-- transactional home.
--
-- ## Why this table (outbox pattern)
--
-- Every mutating identity use case (create / register / archive / unarchive /
-- set_primary / remove / rename / set_optional) writes ONE `domain_events` row IN
-- THE SAME TRANSACTION as the mutation it describes. So the event and the state
-- change are atomic: a rollback erases both, a commit persists both — an event can
-- never disagree with the DB (no lost event, no phantom event). This row is the
-- durable fact the Phase-4 real-time PUSH (task #10) rides out to the renderer,
-- and the ordered history a later replay/audit could read.
--
-- ## Columns (the PRD's required shape)
--
--   * `sequence`    — INTEGER PRIMARY KEY (rowid alias) — the monotonic, global
--     ordering key. Because it is the rowid, SQLite assigns a strictly increasing
--     value on every insert, so `ORDER BY sequence` is the total emission order
--     across all aggregates/projects. (No AUTOINCREMENT keyword: the plain
--     `INTEGER PRIMARY KEY` rowid is already monotonic for an append-only log and
--     avoids the extra `sqlite_sequence` bookkeeping.)
--   * `project_id`  — the project the event is correlated to (NOT NULL: every
--     identity — project, workspace, repository — belongs to a project). NO FK
--     CASCADE: the event log is an audit trail that must OUTLIVE a `delete_project`
--     (a `workspace.removed` / future `project.deleted` event describes a row that
--     is, by definition, gone), so the id is stored as a plain correlation value.
--   * `workspace_id`— the workspace the event is correlated to, or NULL for a
--     project- or repository-scoped event that has no single workspace. Plain
--     correlation value (no FK), same audit-survives-delete reasoning.
--   * `aggregate`   — the entity CLASS the event concerns: `project` | `workspace`
--     | `repository` (mirrors `usecase::DomainEntity`).
--   * `event_type`  — the dotted `entity.action` wire string (`project.created`,
--     `workspace.archived`, `repository.renamed`, …; mirrors
--     `usecase::DomainEventKind::as_str`).
--   * `source`      — WHERE the mutation originated: `ui` | `mcp` (the two callers
--     of the shared use-case layer, PDR-0005). Free TEXT (no CHECK) so a future
--     source needs no migration.
--   * `actor`       — WHO drove it (nullable — unattributed today; reserved for a
--     future authenticated actor id / agent-session id).
--   * `payload`     — a JSON bag of event-specific detail (default '{}'; at least
--     the concerned `identity_id`). Deliberately SMALL — NEVER the terminal byte
--     stream (explicit non-goal) and NEVER a Git-derived view (Q-015, v2.3).
--   * `created_at`  — epoch MILLISECONDS the event was recorded (mirrors the rest
--     of the schema; set explicitly by the backend, julianday DEFAULT as a raw-insert
--     safety net).

CREATE TABLE domain_events (
    -- Monotonic global ordering key (rowid alias): strictly increasing per insert.
    sequence     INTEGER NOT NULL PRIMARY KEY,
    -- Correlation ids (NOT foreign keys — the log must survive a delete of the row
    -- it describes). `project_id` is always present; `workspace_id` is NULL for a
    -- project- or repository-scoped event.
    project_id   TEXT    NOT NULL,
    workspace_id TEXT,
    -- The entity class + dotted event type (mirror of usecase::DomainEntity /
    -- DomainEventKind::as_str).
    aggregate    TEXT    NOT NULL,
    event_type   TEXT    NOT NULL,
    -- Origin (`ui` | `mcp`) and optional actor (reserved).
    source       TEXT    NOT NULL,
    actor        TEXT,
    -- Event-specific JSON detail (at least the concerned identity id). Small by
    -- design — never terminal bytes, never a Git-derived view.
    payload      TEXT    NOT NULL DEFAULT '{}',
    created_at   INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);

-- Index the project correlation for "the events of this project" scans (the common
-- query when a UI/replay filters the log by project).
CREATE INDEX idx_domain_events_project ON domain_events (project_id);

-- Index the workspace correlation for the workspace-scoped subset (partial: only
-- rows that actually carry a workspace participate).
CREATE INDEX idx_domain_events_workspace ON domain_events (workspace_id)
    WHERE workspace_id IS NOT NULL;
