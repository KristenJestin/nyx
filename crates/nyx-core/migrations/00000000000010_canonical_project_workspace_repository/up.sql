-- Schema v10: the CANONICAL Project / Workspace / Repository model (v2.1, PRD
-- 01KWDCNDJPWBY3M6YCQGWMP4RM — "Noyau produit unifié"). Ports the three
-- `structure_locale` identities to the form of `docs/product/data-model.dbml`.
--
-- This migration is the SCHEMA half (task 01KWDCNDJWP9SVFXFQXJCC85WD). It creates
-- the canonical columns/indexes/FKs and the new `repositories` table, and structurally
-- seeds the NOT-NULL canonical fields so the constraints hold on an UPGRADE of a
-- non-empty v1 DB. The DATA half (task 01KWDCNDJX87BWJ6JMTRSETJYT, migration v11)
-- refines the backfill from real v1 semantics, idempotently.
--
-- Impl Decisions honoured (PRD):
--   * ONE SQLite file, one migration chain (PDR-0024) — this is just the next link.
--   * projects.key (Linear prefix, UNIQUE) + primary_workspace_id.
--   * workspaces.root_path (canonical name for the folder) + folder_removed_at
--     (permanent row; NEVER an auto-delete) + archived_at.
--   * repositories = LOGICAL identity (name, relative_path, optional). NO checkout
--     table `workspace_repositories` (PDR-0013): membership is DERIVED, Git state is
--     re-read from Git, never authoritative in DB.
--   * NO `specs` table here — the Spec domain (and specs.workspace_id) is RESERVED
--     for v2.4 and deliberately NOT built in v2.1.
--
-- SQLite ALTER-TABLE notes (same shapes migrations v2/v4/v8/v9 already rely on):
--   * ADD COLUMN on a NON-EMPTY table forbids a non-constant DEFAULT, so NOT-NULL
--     canonical columns are added with a CONSTANT default then backfilled by UPDATE.
--   * A column RENAME (`path` -> `root_path`) is `ALTER TABLE ... RENAME COLUMN`,
--     available on the bundled SQLite (>= 3.25).

-- --- projects: key + primary_workspace_id + archived_at ----------------------

-- `key` = the project's Linear prefix (e.g. NYX); a Spec's human key is
-- `projects.key || '-' || specs.number`. NOT NULL + UNIQUE per the canonical model.
-- Added with a CONSTANT '' default (legal on a non-empty table), then backfilled to a
-- deterministic, structurally-valid, UNIQUE per-project key so the UNIQUE index below
-- can be created without collisions. The data migration (v11) may refine these.
ALTER TABLE projects
    ADD COLUMN key TEXT NOT NULL DEFAULT '';

-- Deterministic seed: an uppercase alpha stub of the project name (fallback 'PRJ')
-- plus the project's dense rank by (created_at, id), so keys are UNIQUE and stable on
-- replay of a given row set. Only touches rows still on the '' placeholder, so a
-- re-run (or a later refinement) never clobbers an already-set key.
UPDATE projects
SET key = 'P' || (
    SELECT COUNT(*)
    FROM projects AS p2
    WHERE p2.created_at < projects.created_at
       OR (p2.created_at = projects.created_at AND p2.id < projects.id)
)
WHERE key = '';

-- One Linear prefix per project, globally unique.
CREATE UNIQUE INDEX idx_projects_key ON projects (key);

-- `primary_workspace_id` = the durable base workspace (canonical source of truth for
-- "the project's primary workspace", replacing reliance on the is_root flag). Nullable
-- FK to workspaces(id); ON DELETE SET NULL so removing the workspace row detaches the
-- pointer rather than cascading the project away. Backfilled (to the current root
-- workspace) by the data migration v11.
ALTER TABLE projects
    ADD COLUMN primary_workspace_id TEXT REFERENCES workspaces(id) ON DELETE SET NULL;

-- `archived_at` = soft-archive timestamp (epoch ms), NULL while the project is active.
ALTER TABLE projects
    ADD COLUMN archived_at INTEGER;

-- --- workspaces: root_path + folder_removed_at + archived_at + canonical indexes ---

-- Canonical rename: the folder column is `root_path` in the data model (a workspace is
-- a physical folder and the intent/runtime pivot). The value (a normalized absolute
-- path) is unchanged; only the name moves to the canonical form.
ALTER TABLE workspaces
    RENAME COLUMN path TO root_path;

-- `folder_removed_at` = epoch ms the worktree folder was cleaned up (at merge). The
-- workspace ROW is PERMANENT — this marks the cleanup, and there is NEVER an auto-delete
-- of the row. NULL while the folder is live.
ALTER TABLE workspaces
    ADD COLUMN folder_removed_at INTEGER;

-- `archived_at` = soft-archive timestamp (epoch ms), NULL while the workspace is active.
ALTER TABLE workspaces
    ADD COLUMN archived_at INTEGER;

-- The pre-v10 UNIQUE(project_id, path) constraint was attached to the old column name;
-- after the rename SQLite keeps it as UNIQUE(project_id, root_path) automatically, so
-- the canonical unique index already holds. We add the SECONDARY canonical index the
-- data model asks for: (project_id, name) — NON-unique (a name is a human label, not a
-- key; the path is the identity).
CREATE INDEX idx_workspaces_project_name ON workspaces (project_id, name);

-- --- repositories: the logical-identity table (PDR-0013) ---------------------

-- A repository is a LOGICAL identity within a project (e.g. `api`): the SAME object in
-- the primary workspace and every dev worktree, so review/history follow across
-- workspaces. `relative_path` is the conventional path inside the project ('.' for a
-- mono-repo). `optional` = not worktree'd by default at start (the structured, in-DB
-- "nyxignore", per repo). There is deliberately NO `workspace_repositories` checkout
-- table: a workspace's repos are DERIVED (the project's non-optional repositories) at
-- `workspace.root_path || relative_path`, and branch/HEAD/dirty are re-read from Git.
--
-- Timestamps mirror the rest of the schema: epoch MILLISECONDS as INTEGER, set
-- explicitly by the backend on every write, with the portable julianday DEFAULT as a
-- safety net for any raw insert.
CREATE TABLE repositories (
    -- UUIDv7 generated by the backend (SQLite does not auto-generate it).
    id            TEXT    NOT NULL PRIMARY KEY,
    project_id    TEXT    NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    -- Conventional path within the project ('.' in a mono-repo). Constant across the
    -- project's workspaces (a worktree copies the primary's structure).
    relative_path TEXT    NOT NULL,
    -- 0/1 boolean (mapped to Rust bool): a repo NOT worktree'd by default at start.
    optional      INTEGER NOT NULL DEFAULT 0 CHECK (optional IN (0, 1)),
    created_at    INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
    updated_at    INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
    -- Soft-archive timestamp (epoch ms), NULL while active.
    archived_at   INTEGER,
    -- A repo's logical name is unique within its project.
    UNIQUE (project_id, name),
    -- Its conventional path is likewise unique within the project.
    UNIQUE (project_id, relative_path)
);

-- Index the FK column for fast `list_repositories(project_id)` scans (the two UNIQUE
-- constraints above already index (project_id, name) / (project_id, relative_path)).
CREATE INDEX idx_repositories_project ON repositories (project_id);
