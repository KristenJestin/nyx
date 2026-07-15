//! Persistence layer: SQLite owned by the Rust backend via Diesel (ADR-0001).
//!
//! This module owns the connection, applies the embedded migrations at startup,
//! and defines the `terminals` models + CRUD used by the bridge commands. The
//! `schema.rs` table definition is committed by hand (no `diesel` CLI required
//! at build); `tests::schema_matches_migration` enforces it stays in sync with
//! the SQL migration.
//!
//! Concurrency: SQLite has a single-writer model and `SqliteConnection` is not
//! `Sync`. Under the Electron core-host the DB is driven from TWO sources that must
//! progress concurrently — the napi `AsyncTask` worker pool (libuv threads serving
//! the Node host) and the MCP server's own accept/handler threads — so a single
//! `Mutex<SqliteConnection>` would serialize every reader behind one connection and
//! a slow query would stall the whole host. The connection therefore lives behind an
//! **r2d2 pool** (`diesel::r2d2`, the PRD-frozen pool — NOT deadpool-diesel, NO
//! Tokio): multiple checked-out connections let independent readers run in parallel,
//! while SQLite's own single-writer rule + WAL + a `busy_timeout` serialize writes
//! safely. WAL and `foreign_keys` are set on EVERY connection at checkout time via a
//! [`PragmaCustomizer`], so they hold for pool connections opened lazily on demand,
//! not just the first one. `with_conn` stays the single accessor every caller uses,
//! so the pool is a drop-in for the old `Mutex` — the only change is that a checkout
//! can come from any of several live connections.

use std::path::Path;
use std::time::Duration;

use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, CustomizeConnection, Pool, PooledConnection};
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use uuid::Uuid;

/// Busy timeout applied to every pooled connection (and the migration connection):
/// when a write cannot take the lock immediately (another connection is mid-write),
/// SQLite retries internally for up to this long before returning `SQLITE_BUSY`
/// instead of failing instantly. Load-bearing now that several pooled connections can
/// contend: a brief writer overlap resolves transparently rather than erroring. 5s is
/// generous for nyx's tiny, short transactions while still bounding a true deadlock.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

/// Max pooled connections. nyx's DB load is light (UI CRUD + the MCP tools + the boot
/// restore), so a small pool covers the napi worker pool and the MCP threads with
/// headroom while keeping the number of open SQLite handles modest.
const POOL_MAX_SIZE: u32 = 8;

use crate::pathnorm;
use crate::schema::{
    agent_sessions, command_instances, domain_events, managed_commands, projects, repositories,
    review_comments, review_reads, review_tasks, spec_revisions, specs, task_dependencies, tasks,
    terminals, workspaces,
};

/// The migrations baked into the binary at compile time from `migrations/`.
/// Running them is idempotent (Diesel tracks applied versions in
/// `__diesel_schema_migrations`), so we run them on every startup.
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

/// A terminal status. Stored as TEXT (`'alive'` | `'closed'`) — see the CHECK
/// constraint in the migration. A live terminal is re-spawned at launch; a
/// closed one is not.
pub const STATUS_ALIVE: &str = "alive";
pub const STATUS_CLOSED: &str = "closed";

/// A terminal's workspace binding mode. Stored as TEXT (`'auto'` | `'manual'`) —
/// see the CHECK constraint in migration v2. `auto` follows the resolved live
/// cwd (auto-attach); `manual` stays pinned to `workspace_id` until unpinned.
pub const BINDING_AUTO: &str = "auto";
pub const BINDING_MANUAL: &str = "manual";

/// Default name of the root workspace `create_project` creates when the caller
/// does not provide one.
pub const DEFAULT_ROOT_WORKSPACE_NAME: &str = "root";

/// A command instance's last run state. Stored as TEXT — see the CHECK constraint
/// in migration v3. `idle` (never run / stopped), `running`, `success` (exit 0),
/// `error` (non-zero exit / spawn failure). The runner (later phase) drives the
/// live transitions; this column is the last value persisted across restarts.
/// `allow(dead_code)` until the runner/bridge (later phases) consume them; the
/// v3 tests already reference them.
#[cfg_attr(not(test), allow(dead_code))]
pub const STATE_IDLE: &str = "idle";
#[cfg_attr(not(test), allow(dead_code))]
pub const STATE_RUNNING: &str = "running";
#[cfg_attr(not(test), allow(dead_code))]
pub const STATE_SUCCESS: &str = "success";
#[cfg_attr(not(test), allow(dead_code))]
pub const STATE_ERROR: &str = "error";

/// A managed command's `source_kind`. Only `package_json` exists today (a template
/// imported from a package.json script); a hand-authored template has `None`. See
/// the CHECK constraint in migration v3.
#[cfg_attr(not(test), allow(dead_code))]
pub const SOURCE_KIND_PACKAGE_JSON: &str = "package_json";

// --- Agent session vocabularies (PRD-5 v7, ADR-0010) ---------------------
//
// An agent session's `agent_kind` and `state` are stored as TEXT with a CHECK
// constraint enforcing these vocabularies (see migration v7). Exposed as consts so
// callers never type the strings inline. Only `claude_code` is put into production
// by this PRD; the other kinds are representable for future adapters (Codex /
// OpenCode are spikes of PRD-6). `allow(dead_code)` until the adapters/bridge of
// later phases consume them; the v7 tests already reference them.

/// `agent_sessions.agent_kind` — Claude Code (the only v1 production adapter).
#[cfg_attr(not(test), allow(dead_code))]
pub const AGENT_KIND_CLAUDE_CODE: &str = "claude_code";
/// `agent_sessions.agent_kind` — Codex (representable; spike of PRD-6, no adapter here).
#[cfg_attr(not(test), allow(dead_code))]
pub const AGENT_KIND_CODEX: &str = "codex";
/// `agent_sessions.agent_kind` — OpenCode (representable; spike of PRD-6, no adapter here).
#[cfg_attr(not(test), allow(dead_code))]
pub const AGENT_KIND_OPENCODE: &str = "opencode";
/// `agent_sessions.agent_kind` — a custom/other agent (representable; no adapter here).
#[cfg_attr(not(test), allow(dead_code))]
pub const AGENT_KIND_CUSTOM: &str = "custom";

/// `agent_sessions.state` — in progress, or left as-is after an app kill (still a
/// resume candidate; SQLite is the authority, not a clean `SessionEnd`).
#[cfg_attr(not(test), allow(dead_code))]
pub const SESSION_STATE_ACTIVE: &str = "active";
/// `agent_sessions.state` — a clean `SessionEnd` was observed.
#[cfg_attr(not(test), allow(dead_code))]
pub const SESSION_STATE_ENDED: &str = "ended";
/// `agent_sessions.state` — was `active` but `last_seen_at` exceeded the staleness
/// threshold without a clean end (probable kill, state unconfirmed). Still a resume
/// candidate, but signals the doubt.
#[cfg_attr(not(test), allow(dead_code))]
pub const SESSION_STATE_UNKNOWN: &str = "unknown";
/// `agent_sessions.state` — a resume was attempted but failed.
#[cfg_attr(not(test), allow(dead_code))]
pub const SESSION_STATE_RESUME_FAILED: &str = "resume_failed";

// --- Spec domain vocabularies (v2.4 Phase 1, v13, PDR-0014) ---------------
//
// A Spec's `status` / `priority` / `mode` and a revision's `revision_status` are
// stored as TEXT with a CHECK constraint enforcing the canonical vocabularies
// (`spec_status` / `priority` / `spec_mode` from `docs/product/data-model.dbml`; see
// migration v13). Exposed as consts so callers never type the strings inline. The
// GUARDED/VALIDATED lifecycle transitions between the statuses are the NEXT task; this
// module only stores them (with `draft` / `med` / `stepped` / `active` defaults).

/// `specs.status` — the design is being authored; the pre-ready gate has not passed.
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_STATUS_DRAFT: &str = "draft";
/// `specs.status` — the design has converged (pre-ready validations passed); actionable.
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_STATUS_READY: &str = "ready";
/// `specs.status` — work is underway (a dev workspace is active).
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_STATUS_IN_PROGRESS: &str = "in_progress";
/// `specs.status` — halted on a blocker (representable; not a lifecycle target here).
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_STATUS_BLOCKED: &str = "blocked";
/// `specs.status` — under review (the "review" lifecycle point).
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_STATUS_IN_REVIEW: &str = "in_review";
/// `specs.status` — done: the terminal success state (`completed_at` stamped).
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_STATUS_COMPLETED: &str = "completed";
/// `specs.status` — cancelled: the terminal abandon state.
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_STATUS_CANCELLED: &str = "cancelled";
/// `specs.status` — soft-archived (representable; not a lifecycle target here).
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_STATUS_ARCHIVED: &str = "archived";

/// `specs.priority` — the canonical `priority` vocabulary (`med` is the default).
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_PRIORITY_URGENT: &str = "urgent";
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_PRIORITY_HIGH: &str = "high";
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_PRIORITY_MED: &str = "med";
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_PRIORITY_LOW: &str = "low";

/// `specs.mode` — human validates each step (the default).
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_MODE_STEPPED: &str = "stepped";
/// `specs.mode` — the agent drives autonomously.
#[cfg_attr(not(test), allow(dead_code))]
pub const SPEC_MODE_AUTOPILOT: &str = "autopilot";

/// `spec_revisions.revision_status` — the current, editable revision (exactly one per
/// Spec, the one `specs.current_revision_number` points at).
#[cfg_attr(not(test), allow(dead_code))]
pub const REVISION_STATUS_ACTIVE: &str = "active";
/// `spec_revisions.revision_status` — retired by a FORK that created a newer revision.
#[cfg_attr(not(test), allow(dead_code))]
pub const REVISION_STATUS_SUPERSEDED: &str = "superseded";

// --- task domain vocabularies (v2.4 Phase 2, v14, PDR-0014) ---------------
//
// A task's `kind` / `executor` / `type` / `status` / `priority` / `severity` and a
// dependency's `type` are stored as TEXT with a CHECK constraint enforcing the canonical
// vocabularies (`task_executor` / `task_type` / `task_status` / `priority` / `severity` /
// `dependency_type` from `docs/product/data-model.dbml`, plus the PRD `kind` vocabulary;
// see migration v14). Exposed as consts so callers never type the strings inline.

/// `tasks.kind` — a `slice` of agent implementation work (the default; `executor=agent`).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_KIND_SLICE: &str = "slice";
/// `tasks.kind` — a `human` action only the user can do (`executor=human`), VERIFIABLE.
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_KIND_HUMAN: &str = "human";
/// `tasks.kind` — a review `finding` (FINDING = TASK, PDR-0016; carries a `severity`).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_KIND_REVIEW: &str = "review";

/// `tasks.executor` — the task is run by the agent (the default; `slice`/`review` kinds).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_EXECUTOR_AGENT: &str = "agent";
/// `tasks.executor` — the task is run by the human (`human` kind, Attention Inbox).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_EXECUTOR_HUMAN: &str = "human";

/// `tasks.type` — the canonical `task_type` vocabulary (`implementation` is the default).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_TYPE_IMPLEMENTATION: &str = "implementation";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_TYPE_INVESTIGATION: &str = "investigation";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_TYPE_VALIDATION: &str = "validation";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_TYPE_DOCUMENTATION: &str = "documentation";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_TYPE_MIGRATION: &str = "migration";

/// `tasks.status` — fresh, not yet actionable (the default).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_STATUS_TODO: &str = "todo";
/// `tasks.status` — actionable (dependencies satisfied); a candidate to start.
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_STATUS_READY: &str = "ready";
/// `tasks.status` — the coder is working on it (`start` target).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_STATUS_IN_PROGRESS: &str = "in_progress";
/// `tasks.status` — halted on a blocker (`block` target; carries `blocked_reason`).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_STATUS_BLOCKED: &str = "blocked";
/// `tasks.status` — under review (representable; not a transition target here).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_STATUS_IN_REVIEW: &str = "in_review";
/// `tasks.status` — done: the terminal success state (`done` target, `completed_at` set).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_STATUS_DONE: &str = "done";
/// `tasks.status` — skipped/abandoned: the terminal skip state (`skip` target).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_STATUS_CANCELLED: &str = "cancelled";

/// `tasks.priority` — the canonical `priority` vocabulary (`med` is the default).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_PRIORITY_URGENT: &str = "urgent";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_PRIORITY_HIGH: &str = "high";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_PRIORITY_MED: &str = "med";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_PRIORITY_LOW: &str = "low";

/// `tasks.severity` — the canonical `severity` vocabulary (set = review FINDING).
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_SEVERITY_INFO: &str = "info";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_SEVERITY_LOW: &str = "low";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_SEVERITY_MEDIUM: &str = "medium";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_SEVERITY_HIGH: &str = "high";
#[cfg_attr(not(test), allow(dead_code))]
pub const TASK_SEVERITY_CRITICAL: &str = "critical";

/// `task_dependencies.type` — a hard ordering edge (the default): the prerequisite must
/// be done before the dependent can proceed.
#[cfg_attr(not(test), allow(dead_code))]
pub const DEPENDENCY_TYPE_BLOCKS: &str = "blocks";
/// `task_dependencies.type` — a soft relation that does NOT gate ordering.
#[cfg_attr(not(test), allow(dead_code))]
pub const DEPENDENCY_TYPE_RELATES_TO: &str = "relates_to";

/// Current wall-clock time as epoch MILLISECONDS. All `terminals` timestamps are
/// stored this way (a plain JS-friendly number on the front). Saturates to 0 if
/// the clock is before the Unix epoch (never expected).
pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A `terminals` row as read from the database. `Serialize` so the bridge
/// commands can return it across the Tauri IPC boundary. Timestamps are epoch
/// milliseconds; `closed_at` is `None` until the terminal is closed.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = terminals)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Terminal {
    pub id: String,
    pub cwd: String,
    pub label: Option<String>,
    pub scrollback: String,
    pub status: String,
    /// Sidebar order; SQL column is `"order"` (a keyword) exposed as `order_index`.
    pub order_index: i32,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
    /// Epoch ms of the last time this terminal was the active one (`None` =
    /// never). The launcher reopens on the alive terminal with the greatest value.
    pub last_active_at: Option<i64>,
    /// The workspace this terminal is attached to (`None` = not attached). Set by
    /// `attach`/`pin` and auto-attach; cleared by `detach`.
    pub workspace_id: Option<String>,
    /// `auto` (follows resolved cwd) or `manual` (pinned until unpin). See
    /// [`BINDING_AUTO`] / [`BINDING_MANUAL`].
    pub workspace_binding_mode: String,
    /// Last exec-state of this terminal (PRD-2.1): `idle` | `running` | `success`
    /// | `error` (CHECK-enforced — see [`STATE_IDLE`] etc., the SAME vocabulary as
    /// `command_instances.last_state`). Defaults to `idle` so an OLD terminal (a
    /// row predating migration v4) loads as idle — no false badge on upgrade.
    pub exec_state: String,
    /// Exit code of the last finished command (`None` = none yet, or a `133;D`
    /// end event that carried no parseable code). The state machine maps `Some(0)`
    /// → success, non-zero → error.
    pub exec_exit_code: Option<i32>,
    /// Notification flag: a settled `success`/`error` the user has NOT yet seen on
    /// an inactive terminal. Kept SEPARATE from `exec_state` so mark-read clears
    /// the unread flag while preserving the settled result (the deliberate
    /// difference from the managed-command acknowledge model). SQLite 0/1 boolean.
    pub exec_state_unread: bool,
    /// Epoch ms of the last exec-state transition. Stamped on every transition.
    pub exec_state_updated_at: i64,
}

/// A new `terminals` row to insert. `id` is a backend-generated UUIDv7;
/// `created_at`/`updated_at` are set explicitly (to the same instant) so
/// creation is deterministic and testable. `closed_at` is left NULL (column
/// default). `scrollback`/`status`/`order` also have column defaults but are set
/// explicitly here.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = terminals)]
pub struct NewTerminal {
    pub id: String,
    pub cwd: String,
    pub label: Option<String>,
    pub status: String,
    pub order_index: i32,
    pub created_at: i64,
    pub updated_at: i64,
    /// Stamped explicitly at creation so a fresh terminal carries a real epoch-ms
    /// (`exec_state` itself stays `idle` via the column DEFAULT). This must be set
    /// here because the v4 migration's column DEFAULT is the CONSTANT `0` (a
    /// non-constant DEFAULT is illegal on `ADD COLUMN` for a non-empty table — the
    /// migration backfills EXISTING rows separately); without stamping it, a new
    /// row would land on 0 instead of "now".
    pub exec_state_updated_at: i64,
}

impl NewTerminal {
    /// A fresh `alive` terminal at `cwd`, with an optional label, placed at the
    /// given sidebar order. Generates a fresh, time-ordered UUIDv7 id and stamps
    /// `created_at`/`updated_at`/`exec_state_updated_at` with the current epoch-ms.
    pub fn alive(cwd: impl Into<String>, label: Option<String>, order_index: i32) -> Self {
        let now = now_millis();
        NewTerminal {
            id: Uuid::now_v7().to_string(),
            cwd: cwd.into(),
            label,
            status: STATUS_ALIVE.to_string(),
            order_index,
            created_at: now,
            updated_at: now,
            exec_state_updated_at: now,
        }
    }
}

// --- Project / Workspace models (PRD-2 v2) -------------------------------

/// A `projects` row. `Serialize` for the IPC boundary. Timestamps are epoch ms.
/// `collapsed` is the persisted sidebar disclosure state (`false` = open); the
/// front initializes a project band's open state from `!collapsed` on reload.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = projects)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Sidebar open/closed state, persisted across restarts (`false` = open).
    pub collapsed: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// Per-project, default-OFF opt-in to RESUME an active agent session at relaunch
    /// (PRD-5 #5): when `true`, nyx injects the adapter's exact-resume command (e.g.
    /// `claude --resume <id>`) into a respawned shell instead of leaving a bare shell.
    /// `false` (the default, and the only value for a project predating v8) means no
    /// auto-resume — which is also what the close-warning (#6) keys on.
    pub resume_agent_sessions: bool,
    /// Sidebar order (PRD-#11). SQL column is `"order"` (a keyword) exposed as
    /// `order_index`, same shape as `terminals`/`managed_commands`. Lower sorts first;
    /// ties broken by `created_at`/`id`. A v9 migration backfills it from the prior
    /// created-at order so existing projects keep their position.
    pub order_index: i32,
    /// Canonical Linear prefix of the project (e.g. `NYX`) — NOT NULL, UNIQUE (v10).
    /// A Spec's human key is `key || '-' || specs.number`. The v10 migration seeds a
    /// deterministic unique stub for existing projects; the v11 data migration and the
    /// use-case layer (later phases) own its curated value.
    pub key: String,
    /// The project's durable base workspace (v10): the canonical source of truth for
    /// the primary role (`primary/worktree`), replacing reliance on `workspaces.is_root`.
    /// `None` until backfilled (v11 points it at the current root workspace). FK to
    /// `workspaces(id)` with ON DELETE SET NULL.
    pub primary_workspace_id: Option<String>,
    /// Soft-archive timestamp, epoch ms (v10). `None` while the project is active.
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = projects)]
struct NewProject {
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
    /// Sidebar order; set to MAX(order)+1 at creation so a new project appends LAST.
    order_index: i32,
    /// Canonical Linear prefix (v10): NOT NULL + UNIQUE. `create_project` derives a
    /// deterministic, unique-by-construction stub so the UNIQUE constraint holds even
    /// before the curated value is set by the use-case layer (later phases).
    key: String,
}

/// A `workspaces` row. `path` is the canonical (normalized) absolute folder.
/// `is_root` and `collapsed` are SQLite 0/1 booleans exposed as Rust `bool` via
/// the `Queryable`/`Selectable` Integer↔bool mapping Diesel provides for SQLite.
/// `collapsed` is the persisted sidebar disclosure state (`false` = open); the
/// front initializes a workspace band's open state from `!collapsed` on reload.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = workspaces)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Workspace {
    pub id: String,
    pub project_id: String,
    pub name: String,
    /// Canonical, normalized absolute folder path (see [`crate::pathnorm`]). SQL column
    /// is `root_path` (renamed from `path` in v10 to the data-model canonical name); the
    /// value is unchanged. This is the workspace's identity within its project.
    ///
    /// The IPC/MCP wire field STAYS `path` (`#[serde(rename)]`): the UI/bridge contract
    /// (owned by the use-case layer, later phases) is unchanged by this DB rename, so the
    /// v2.1 schema task lands the canonical column without churning the renderer.
    #[serde(rename = "path")]
    pub root_path: String,
    pub branch: Option<String>,
    pub is_root: bool,
    /// Sidebar open/closed state, persisted across restarts (`false` = open).
    pub collapsed: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// Epoch ms the worktree folder was cleaned up at merge (v10). The workspace ROW is
    /// PERMANENT — this marks the folder cleanup, never an auto-delete of the row.
    /// `None` while the folder is live.
    pub folder_removed_at: Option<i64>,
    /// Soft-archive timestamp, epoch ms (v10). `None` while the workspace is active.
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = workspaces)]
struct NewWorkspace {
    id: String,
    project_id: String,
    name: String,
    root_path: String,
    branch: Option<String>,
    is_root: bool,
    created_at: i64,
    updated_at: i64,
}

// --- Repository model (v2.1 canonical structure_locale, v10, PDR-0013) ----

/// A `repositories` row: the LOGICAL identity of a repo within a project (e.g. `api`).
/// `Serialize` for the IPC/MCP boundary. `relative_path` is the conventional path
/// inside the project (`"."` in a mono-repo), constant across the project's workspaces.
/// `optional` (SQLite 0/1 ↔ Rust `bool`) marks a repo NOT worktree'd by default at
/// start (the structured, in-DB "nyxignore", per repo). There is deliberately NO
/// checkout table: a workspace's repos are DERIVED (the non-optional ones) at
/// `workspace.root_path || relative_path`, and Git state is re-read from Git (PDR-0013).
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = repositories)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Repository {
    pub id: String,
    pub project_id: String,
    pub name: String,
    /// Conventional path within the project (`"."` in a mono-repo).
    pub relative_path: String,
    /// `true` = not worktree'd by default at start (in-DB "nyxignore").
    pub optional: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// Soft-archive timestamp, epoch ms. `None` while the repository is active.
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = repositories)]
struct NewRepository {
    id: String,
    project_id: String,
    name: String,
    relative_path: String,
    optional: bool,
    created_at: i64,
    updated_at: i64,
}

// --- domain_events OUTBOX model (v2.1 Phase 4, v12) ----------------------

/// A `domain_events` row: ONE persisted business mutation of a structuring identity
/// (project / workspace / repository), written IN THE SAME TRANSACTION as the mutation
/// it describes (the outbox pattern — see [`record_domain_event`]). `Serialize` for the
/// IPC/observer boundary. `sequence` is the monotonic global ordering key (the rowid
/// alias); `payload` is a JSON string (at least the concerned `identity_id`). The
/// correlation ids are plain values (no FK) so the log survives a delete of the row it
/// describes.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = domain_events)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct DomainEvent {
    /// Monotonic, strictly-increasing global ordering key (the rowid alias).
    pub sequence: i64,
    /// The project the event is correlated to (always present).
    pub project_id: String,
    /// The workspace the event is correlated to, or `None` for a project- or
    /// repository-scoped event that has no single workspace.
    pub workspace_id: Option<String>,
    /// The entity CLASS: `project` | `workspace` | `repository`.
    pub aggregate: String,
    /// The dotted `entity.action` wire string (`project.created`, …).
    pub event_type: String,
    /// WHERE the mutation originated: `ui` | `mcp`.
    pub source: String,
    /// WHO drove it (reserved — `None` today).
    pub actor: Option<String>,
    /// Event-specific JSON detail (at least the concerned identity id). Small by
    /// design — never terminal bytes, never a Git-derived view.
    pub payload: String,
    /// Epoch ms the event was recorded.
    pub created_at: i64,
}

/// A new `domain_events` row to insert. `sequence` is OMITTED — SQLite assigns the
/// rowid (the monotonic ordering key) on insert. `created_at` is stamped explicitly by
/// the backend (the julianday DEFAULT is only a raw-insert safety net).
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = domain_events)]
pub struct NewDomainEvent {
    pub project_id: String,
    pub workspace_id: Option<String>,
    pub aggregate: String,
    pub event_type: String,
    pub source: String,
    pub actor: Option<String>,
    pub payload: String,
    pub created_at: i64,
}

/// Append ONE `domain_events` row to the outbox and return it (with its assigned
/// `sequence`). Called by the mutating identity use cases INSIDE the same transaction as
/// the mutation they describe (outbox pattern, task #9): if the surrounding transaction
/// rolls back, this row is erased with it; if it commits, the event is durably present
/// and ordered. `created_at` is stamped to now when the caller leaves it at 0.
pub fn record_domain_event(
    conn: &mut SqliteConnection,
    mut event: NewDomainEvent,
) -> QueryResult<DomainEvent> {
    if event.created_at == 0 {
        event.created_at = now_millis();
    }
    diesel::insert_into(domain_events::table)
        .values(event)
        .returning(DomainEvent::as_returning())
        .get_result(conn)
}

/// List `domain_events` in emission order (`sequence` asc — the monotonic global
/// order). The read companion to [`record_domain_event`], used by tests (and any future
/// replay/audit surface) to assert the persisted, ordered outbox.
pub fn list_domain_events(conn: &mut SqliteConnection) -> QueryResult<Vec<DomainEvent>> {
    domain_events::table
        .order(domain_events::sequence.asc())
        .select(DomainEvent::as_select())
        .load(conn)
}

// --- review_comments model (v2.4 Phase 4, v16, PDR-0012/0015) --------------

/// `review_comments.author_type` — a HUMAN reviewer.
pub const AUTHOR_HUMAN: &str = "human";
/// `review_comments.author_type` — an AGENT (writes via MCP; may reply, never resolve).
pub const AUTHOR_AGENT: &str = "agent";
/// `review_comments.side` — the OLD (removed / base) diff side.
pub const SIDE_OLD: &str = "old";
/// `review_comments.side` — the NEW (added / target) diff side.
pub const SIDE_NEW: &str = "new";
/// `review_comments.status` — open (not yet resolved).
pub const COMMENT_STATUS_OPEN: &str = "open";
/// `review_comments.status` — resolved (a HUMAN closed it).
pub const COMMENT_STATUS_RESOLVED: &str = "resolved";

/// A `review_comments` row: the ONLY persisted review artifact (PDR-0012/0015 — review is a
/// vue dérivée). Auto-threaded by `parent_id` (`None` = a thread ROOT, `Some` = a reply).
/// ANCHORED (`file_path` present, + `side`/`line_*`/`blob`/`snippet`/`context` for
/// re-mapping) or GLOBAL (`file_path` `None`). `Serialize` for the IPC/MCP boundary. The
/// `(workspace_id, repository_id, spec_id)` correlation is stored as plain values so a
/// comment survives Git churn / an archive. Resolution is HUMAN-ONLY (the agent/MCP façade
/// cannot set `status = resolved`).
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = review_comments)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct ReviewComment {
    pub id: String,
    pub workspace_id: String,
    pub repository_id: String,
    /// The OPTIONAL spec the review is bound to (`None` = generic review, PDR-0004).
    pub spec_id: Option<String>,
    /// `None` = thread ROOT; `Some(root_id)` = a reply under that root.
    pub parent_id: Option<String>,
    /// `human` | `agent`.
    pub author_type: String,
    pub author_name: String,
    /// The anchored changed-file path (`None` = a GLOBAL comment).
    pub file_path: Option<String>,
    /// `old` | `new` — which diff side the anchor is on (`None` for a global comment).
    pub side: Option<String>,
    /// 1-based anchored line (or range start) on `side`.
    pub line_start: Option<i32>,
    /// 1-based range end on `side` (equals `line_start` for a single-line anchor).
    pub line_end: Option<i32>,
    /// The blob SHA the anchor was captured on (the re-mapping's starting point).
    pub blob: Option<String>,
    /// The anchored line text — the primary snippet the re-mapping relocates.
    pub snippet: Option<String>,
    /// The surrounding lines — the re-mapping's disambiguation context.
    pub context: Option<String>,
    /// `true` when the anchor could no longer be relocated after the repo advanced.
    pub outdated: bool,
    pub body: String,
    /// `open` | `resolved`.
    pub status: String,
    /// WHO resolved it (`None` while open; only ever `human` by policy).
    pub resolved_by_type: Option<String>,
    pub created_at: i64,
}

/// The anchor of a NEW review comment — the where-on-the-diff a comment hangs, plus the
/// material the re-mapping needs to survive a commit. Absent (`None` on [`NewReviewComment`])
/// = a GLOBAL comment. `line_end` defaults to `line_start` for a single-line anchor.
#[derive(Debug, Clone, Default)]
pub struct CommentAnchor {
    pub file_path: String,
    pub side: String,
    pub line_start: i32,
    pub line_end: i32,
    pub blob: Option<String>,
    pub snippet: Option<String>,
    pub context: Option<String>,
}

/// The fields to POST a new review comment (root, reply, or global) — grouped so the
/// call sites stay readable. `parent_id` `Some` = a reply; `anchor` `Some` = an anchored
/// comment (a reply inherits its root's anchor at the read layer, so a reply usually
/// carries no anchor of its own).
#[derive(Debug, Clone, Default)]
pub struct NewComment {
    pub workspace_id: String,
    pub repository_id: String,
    pub spec_id: Option<String>,
    pub parent_id: Option<String>,
    pub author_type: String,
    pub author_name: String,
    pub anchor: Option<CommentAnchor>,
    pub body: String,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = review_comments)]
struct NewReviewCommentRow {
    id: String,
    workspace_id: String,
    repository_id: String,
    spec_id: Option<String>,
    parent_id: Option<String>,
    author_type: String,
    author_name: String,
    file_path: Option<String>,
    side: Option<String>,
    line_start: Option<i32>,
    line_end: Option<i32>,
    blob: Option<String>,
    snippet: Option<String>,
    context: Option<String>,
    outdated: bool,
    body: String,
    status: String,
    resolved_by_type: Option<String>,
    created_at: i64,
}

/// INSERT a review comment (a thread root, a reply, or a global comment) and return it.
/// The author may be `human` OR `agent`; a reply carries a `parent_id`, an anchored comment
/// carries an `anchor`. A fresh comment is always `open` and never `outdated` at creation
/// (re-mapping only flips `outdated` later, when the repo advances). Called by BOTH façades
/// (the UI db-command and the MCP tool), which is why author/anchor are parameters.
pub fn insert_review_comment(
    conn: &mut SqliteConnection,
    new: NewComment,
) -> QueryResult<ReviewComment> {
    let anchor = new.anchor;
    // A REPLY inherits its root's Spec binding: a reply posted without an explicit `spec_id`
    // (typically an agent reply via MCP, which only knows the `parent_id`) resolves the parent's
    // `spec_id` so the reply lands in the SAME spec-scoped feed as its root — never orphaned to
    // the unscoped (`NULL`) bucket where it would drop out of the Spec's review. A ROOT (no
    // parent), or a reply that was handed an explicit `spec_id`, keeps what it was given.
    let spec_id = match (new.spec_id, &new.parent_id) {
        (None, Some(parent_id)) => review_comments::table
            .find(parent_id)
            .select(review_comments::spec_id)
            .first::<Option<String>>(conn)
            .optional()?
            .flatten(),
        (spec_id, _) => spec_id,
    };
    let row = NewReviewCommentRow {
        id: Uuid::now_v7().to_string(),
        workspace_id: new.workspace_id,
        repository_id: new.repository_id,
        spec_id,
        parent_id: new.parent_id,
        author_type: new.author_type,
        author_name: new.author_name,
        file_path: anchor.as_ref().map(|a| a.file_path.clone()),
        side: anchor.as_ref().map(|a| a.side.clone()),
        line_start: anchor.as_ref().map(|a| a.line_start),
        line_end: anchor.as_ref().map(|a| a.line_end),
        blob: anchor.as_ref().and_then(|a| a.blob.clone()),
        snippet: anchor.as_ref().and_then(|a| a.snippet.clone()),
        context: anchor.as_ref().and_then(|a| a.context.clone()),
        outdated: false,
        body: new.body,
        status: COMMENT_STATUS_OPEN.to_string(),
        resolved_by_type: None,
        created_at: now_millis(),
    };
    diesel::insert_into(review_comments::table)
        .values(row)
        .returning(ReviewComment::as_returning())
        .get_result(conn)
}

/// List the review comments of a `(workspace, repository)`, oldest-first (`created_at`
/// asc, ties broken by `id`) — the whole feed the review panel threads by `parent_id`.
/// Includes both roots and replies, anchored and global, open and resolved.
///
/// `spec_id` SCOPES the feed to ONE Spec (PDR-0004/0028): a review is bound to the Spec it was
/// opened from, so `Some(spec)` returns ONLY that Spec's comments — the fix for the leak where
/// comments of one Spec surfaced on every Spec sharing the `(workspace, repository)` pair. A
/// reply inherits its root's spec at INSERT time (see [`insert_review_comment`]), so scoping a
/// root in scopes its whole thread. `None` is the UNSCOPED read (the whole repo feed, spec or
/// not) the MCP agent façade and the repo-wide re-mapping still use.
pub fn list_review_comments(
    conn: &mut SqliteConnection,
    workspace_id: &str,
    repository_id: &str,
    spec_id: Option<&str>,
) -> QueryResult<Vec<ReviewComment>> {
    let mut query = review_comments::table
        .filter(review_comments::workspace_id.eq(workspace_id))
        .filter(review_comments::repository_id.eq(repository_id))
        .into_boxed();
    if let Some(spec_id) = spec_id {
        query = query.filter(review_comments::spec_id.eq(spec_id.to_string()));
    }
    query
        .order((review_comments::created_at.asc(), review_comments::id.asc()))
        .select(ReviewComment::as_select())
        .load(conn)
}

/// Resolve ONE comment by id, or `None` when unknown — used by the re-mapping and the
/// resolve/reopen paths to load-then-mutate.
pub fn get_review_comment(
    conn: &mut SqliteConnection,
    id: &str,
) -> QueryResult<Option<ReviewComment>> {
    review_comments::table
        .find(id)
        .select(ReviewComment::as_select())
        .first(conn)
        .optional()
}

/// RESOLVE a comment — the HUMAN-ONLY state transition. `resolved_by_type` is always
/// stamped `human`: this function is only reachable from the human-resolve write path (the
/// UI db-command), NOT from the MCP agent façade, which advertises no resolve tool. Returns
/// the updated row, or `None` when the id is unknown. Idempotent (resolving an already
/// resolved comment is a no-op that returns it).
pub fn resolve_review_comment(
    conn: &mut SqliteConnection,
    id: &str,
) -> QueryResult<Option<ReviewComment>> {
    let updated = diesel::update(review_comments::table.find(id))
        .set((
            review_comments::status.eq(COMMENT_STATUS_RESOLVED),
            review_comments::resolved_by_type.eq(Some(AUTHOR_HUMAN)),
        ))
        .returning(ReviewComment::as_returning())
        .get_result(conn)
        .optional()?;
    Ok(updated)
}

/// RE-OPEN a resolved comment (the human's undo of a resolve) — clears `status` back to
/// `open` and drops `resolved_by_type`. Human-only, same as [`resolve_review_comment`].
pub fn reopen_review_comment(
    conn: &mut SqliteConnection,
    id: &str,
) -> QueryResult<Option<ReviewComment>> {
    let updated = diesel::update(review_comments::table.find(id))
        .set((
            review_comments::status.eq(COMMENT_STATUS_OPEN),
            review_comments::resolved_by_type.eq(None::<String>),
        ))
        .returning(ReviewComment::as_returning())
        .get_result(conn)
        .optional()?;
    Ok(updated)
}

/// The re-mapped anchor to persist for ONE comment — the relocated line numbers + blob +
/// snippet/context + the `outdated` verdict, grouped so [`update_review_comment_anchor`] takes
/// one value. Produced by [`crate::review_comment::remap_repo_comments`].
#[derive(Debug, Clone, Default)]
pub struct AnchorUpdate {
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub blob: Option<String>,
    pub snippet: Option<String>,
    pub context: Option<String>,
    pub outdated: bool,
}

/// Persist the outcome of a re-mapping for ONE anchored comment: its (possibly relocated)
/// `line_start`/`line_end`/`blob`/`snippet`/`context` and its `outdated` flag. Called by
/// [`crate::review_comment::remap_repo_comments`] after the repo advances; a global comment
/// (no anchor) is never passed here. Returns the updated row, or `None` when unknown.
pub fn update_review_comment_anchor(
    conn: &mut SqliteConnection,
    id: &str,
    update: AnchorUpdate,
) -> QueryResult<Option<ReviewComment>> {
    let updated = diesel::update(review_comments::table.find(id))
        .set((
            review_comments::line_start.eq(update.line_start),
            review_comments::line_end.eq(update.line_end),
            review_comments::blob.eq(update.blob),
            review_comments::snippet.eq(update.snippet),
            review_comments::context.eq(update.context),
            review_comments::outdated.eq(update.outdated),
        ))
        .returning(ReviewComment::as_returning())
        .get_result(conn)
        .optional()?;
    Ok(updated)
}

// --- review_tasks (finding = task) + unread watermark (v2.4 Phase 5, v17, PDR-0012) ---------

/// `review_tasks.grill_state` — PROPOSED: the human lâche brut (initiated the promotion from
/// a comment, not yet structured). Never "ready for the coder".
pub const GRILL_PROPOSED: &str = "proposed";
/// `review_tasks.grill_state` — STRUCTURED: the agent gave it a clean title/details (via MCP)
/// but the human has not validated it yet. Never "ready for the coder".
pub const GRILL_STRUCTURED: &str = "structured";
/// `review_tasks.grill_state` — VALIDATED: the human validated the structured task. ONLY a
/// validated task is "ready for the coder" (the coder works on tasks, never raw comments).
pub const GRILL_VALIDATED: &str = "validated";

/// A `review_tasks` row: a comment PROMOTED for the coder — `finding = task` (PDR-0012). The
/// promoted comment IS the coder task; `from_comment_id` is the trail back to its source comment.
/// `severity` is OPTIONAL; `grill_state` walks proposed → structured → validated (the "grill":
/// human lâche brut, agent structures via MCP, human validates). `Serialize` for the IPC/MCP
/// boundary. Correlation ids are plain values (no FK) so a task survives Git churn / an
/// archive, like `review_comments`.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = review_tasks)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct ReviewTask {
    pub id: String,
    pub workspace_id: String,
    pub repository_id: String,
    /// The comment this task was promoted from (finding = a promoted comment).
    pub from_comment_id: String,
    pub title: String,
    /// The structured task detail (`None` until the agent structures it).
    pub details: Option<String>,
    /// OPTIONAL severity (`info` | `low` | `medium` | `high` | `critical`; `None` = unset).
    pub severity: Option<String>,
    /// `proposed` | `structured` | `validated` — the grill lifecycle.
    pub grill_state: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// The OPTIONAL Spec this task is scoped to (`None` = un-scoped), INHERITED from its source
    /// comment's `spec_id` at promotion (finding #53). Kept LAST to mirror the v18
    /// `ALTER TABLE ADD COLUMN` physical column order (Diesel positional decoding). Scopes
    /// [`list_review_tasks`] so a task promoted from one Spec's comment never leaks onto another
    /// Spec sharing the same `(workspace, repository)` — the tasks counterpart of the comment fix
    /// (#41).
    pub spec_id: Option<String>,
}

impl ReviewTask {
    /// Whether this task is READY FOR THE CODER: only a `validated` task is. The coder
    /// works on tasks (validated promotions), never on raw comments or un-validated promotions.
    pub fn is_ready_for_coder(&self) -> bool {
        self.grill_state == GRILL_VALIDATED
    }
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = review_tasks)]
struct NewReviewTaskRow {
    id: String,
    workspace_id: String,
    repository_id: String,
    from_comment_id: String,
    title: String,
    details: Option<String>,
    severity: Option<String>,
    grill_state: String,
    created_at: i64,
    updated_at: i64,
    spec_id: Option<String>,
}

/// PROMOTE a comment into a coder task (the grill's FIRST step — the human lâche brut). Creates a
/// `review_tasks` row in state `proposed` linked to `from_comment_id`, seeding `title` from the
/// comment body and anchoring on the SAME `(workspace, repository)` as the comment. `severity` is
/// OPTIONAL. Human-initiated (the UI façade). Returns the created task.
///
/// The task INHERITS its source comment's `spec_id` (finding #53): a promotion from a Spec-scoped
/// comment is itself Spec-scoped, so [`list_review_tasks`] can keep a Spec's task list its own — a
/// task promoted on Spec A never surfaces on Spec B sharing the same `(workspace, repository)`, the
/// tasks counterpart of the comment scoping (#41). A promotion from an un-scoped comment stays
/// un-scoped (`None`).
pub fn promote_comment_to_task(
    conn: &mut SqliteConnection,
    comment: &ReviewComment,
    title: &str,
    severity: Option<&str>,
) -> QueryResult<ReviewTask> {
    let now = now_millis();
    let row = NewReviewTaskRow {
        id: Uuid::now_v7().to_string(),
        workspace_id: comment.workspace_id.clone(),
        repository_id: comment.repository_id.clone(),
        from_comment_id: comment.id.clone(),
        title: title.to_string(),
        details: None,
        severity: severity.map(|s| s.to_string()),
        grill_state: GRILL_PROPOSED.to_string(),
        created_at: now,
        updated_at: now,
        spec_id: comment.spec_id.clone(),
    };
    diesel::insert_into(review_tasks::table)
        .values(row)
        .returning(ReviewTask::as_returning())
        .get_result(conn)
}

/// STRUCTURE a proposed task (the grill's SECOND step — the AGENT structures it, via MCP).
/// Rewrites `title`/`details` (+ optionally `severity`) and moves the state to `structured`. This
/// is the AGENT capability; the agent NEVER validates (there is no validate path reachable from
/// the MCP façade — the human-only guard-rail, symmetric to phase-4 human-only resolution). A
/// task already `validated` is not re-openable to `structured` here (only a `proposed` OR
/// `structured` task restructures); `None` on an unknown/validated id. Returns the updated row.
pub fn structure_task(
    conn: &mut SqliteConnection,
    id: &str,
    title: &str,
    details: Option<&str>,
    severity: Option<Option<&str>>,
) -> QueryResult<Option<ReviewTask>> {
    let Some(current) = get_review_task(conn, id)? else {
        return Ok(None);
    };
    // The agent can only (re)structure a not-yet-validated task. A validated task is the
    // human's committed decision; the agent cannot silently reopen it.
    if current.grill_state == GRILL_VALIDATED {
        return Ok(None);
    }
    let now = now_millis();
    // `severity`: Some(v) sets it (v may be None to clear), None leaves it unchanged.
    let sev = match severity {
        Some(v) => v.map(|s| s.to_string()),
        None => current.severity.clone(),
    };
    let updated = diesel::update(review_tasks::table.find(id))
        .set((
            review_tasks::title.eq(title.to_string()),
            review_tasks::details.eq(details.map(|d| d.to_string())),
            review_tasks::severity.eq(sev),
            review_tasks::grill_state.eq(GRILL_STRUCTURED),
            review_tasks::updated_at.eq(now),
        ))
        .returning(ReviewTask::as_returning())
        .get_result(conn)
        .optional()?;
    Ok(updated)
}

/// VALIDATE a structured task (the grill's THIRD step — the HUMAN validates it). Moves the
/// state to `validated`, after which the task is "ready for the coder". HUMAN-ONLY: this is
/// only reachable from the UI façade (the MCP agent façade advertises no validate tool, so the
/// agent can never reach it). Idempotent on an already-validated task. `None` on an unknown id.
/// Returns the updated row.
pub fn validate_task(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<ReviewTask>> {
    let updated = diesel::update(review_tasks::table.find(id))
        .set((
            review_tasks::grill_state.eq(GRILL_VALIDATED),
            review_tasks::updated_at.eq(now_millis()),
        ))
        .returning(ReviewTask::as_returning())
        .get_result(conn)
        .optional()?;
    Ok(updated)
}

/// Read ONE task by id, or `None` when unknown.
pub fn get_review_task(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<ReviewTask>> {
    review_tasks::table
        .find(id)
        .select(ReviewTask::as_select())
        .first(conn)
        .optional()
}

/// List the tasks (promoted tasks) of a `(workspace, repository)`, oldest-first — the review's
/// promoted-task list. Includes every grill state (proposed / structured / validated).
///
/// `spec_id` SCOPES the list to ONE Spec (finding #53), mirroring [`list_review_comments`]: a review
/// is bound to the Spec it was opened from, so `Some(spec)` returns ONLY that Spec's promoted tasks —
/// the fix for the leak where a task promoted from one Spec's comment surfaced on every OTHER Spec
/// sharing the `(workspace, repository)` pair. A task inherits its source comment's `spec_id` at
/// promotion (see [`promote_comment_to_task`]). `None` is the UNSCOPED read (the whole repo's task
/// list, scoped or not) the MCP agent façade still uses.
pub fn list_review_tasks(
    conn: &mut SqliteConnection,
    workspace_id: &str,
    repository_id: &str,
    spec_id: Option<&str>,
) -> QueryResult<Vec<ReviewTask>> {
    let mut query = review_tasks::table
        .filter(review_tasks::workspace_id.eq(workspace_id))
        .filter(review_tasks::repository_id.eq(repository_id))
        .into_boxed();
    if let Some(spec_id) = spec_id {
        query = query.filter(review_tasks::spec_id.eq(spec_id.to_string()));
    }
    query
        .order((review_tasks::created_at.asc(), review_tasks::id.asc()))
        .select(ReviewTask::as_select())
        .load(conn)
}

/// The unread state of a `(workspace, repository)` review feed — the COUNT of comments/replies
/// not yet seen (created strictly after the read watermark) + the watermark itself. Reuses the
/// runtime `unread` pattern as a per-repo READ WATERMARK. `Serialize` for the IPC/MCP boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewUnread {
    pub workspace_id: String,
    pub repository_id: String,
    /// The number of comments (roots + replies) created strictly after `last_read_at`.
    pub unread: i64,
    /// The epoch-ms watermark up to which the human has seen the feed (0 = never read).
    pub last_read_at: i64,
}

/// The read watermark of a `(workspace, repository)`, or 0 when never read.
fn read_watermark(
    conn: &mut SqliteConnection,
    workspace_id: &str,
    repository_id: &str,
) -> QueryResult<i64> {
    review_reads::table
        .find((workspace_id, repository_id))
        .select(review_reads::last_read_at)
        .first::<i64>(conn)
        .optional()
        .map(|o| o.unwrap_or(0))
}

/// Compute the UNREAD count of a `(workspace, repository)` review feed: the number of comments
/// (roots + replies, any author) whose `created_at` is STRICTLY AFTER the read watermark. A new
/// comment/reply raises it; [`mark_review_read`] resets it to 0. Never-read = every comment is
/// unread. This is the "compteur de non-lus" backing the live UI + MCP badge.
pub fn review_unread(
    conn: &mut SqliteConnection,
    workspace_id: &str,
    repository_id: &str,
) -> QueryResult<ReviewUnread> {
    let last_read_at = read_watermark(conn, workspace_id, repository_id)?;
    let unread: i64 = review_comments::table
        .filter(review_comments::workspace_id.eq(workspace_id))
        .filter(review_comments::repository_id.eq(repository_id))
        .filter(review_comments::created_at.gt(last_read_at))
        .count()
        .get_result(conn)?;
    Ok(ReviewUnread {
        workspace_id: workspace_id.to_string(),
        repository_id: repository_id.to_string(),
        unread,
        last_read_at,
    })
}

/// MARK-READ the review feed of a `(workspace, repository)`: stamp the read watermark to `now`, so
/// the unread count drops to 0 (mirrors the runtime `acknowledge`). Upserts the ONE
/// `review_reads` row. Returns the fresh (zeroed) [`ReviewUnread`]. Idempotent.
pub fn mark_review_read(
    conn: &mut SqliteConnection,
    workspace_id: &str,
    repository_id: &str,
) -> QueryResult<ReviewUnread> {
    let now = now_millis();
    diesel::insert_into(review_reads::table)
        .values((
            review_reads::workspace_id.eq(workspace_id),
            review_reads::repository_id.eq(repository_id),
            review_reads::last_read_at.eq(now),
        ))
        .on_conflict((review_reads::workspace_id, review_reads::repository_id))
        .do_update()
        .set(review_reads::last_read_at.eq(now))
        .execute(conn)?;
    review_unread(conn, workspace_id, repository_id)
}

/// List the MOST RECENT `domain_events` correlated to a workspace, newest first, capped at
/// `limit` rows — the BOUNDED slice the v2.2 cockpit timeline reads (task
/// `01KWHPV33SAV9NV1STSY403CF0`). "Correlated to the workspace" is the union of:
///   * the workspace-scoped events (`workspace_id = workspace_id`), and
///   * the events of the workspace's PROJECT that carry no single workspace
///     (`workspace_id IS NULL AND project_id = project_id`) — the project / repository
///     "changements notables" that still belong to this workspace's context.
///
/// The result is ORDERED newest-first (`sequence` desc, the monotonic global order) and
/// LIMITED to `limit`, so the timeline is bounded by construction — it never loads the whole
/// unbounded log. The caller re-orders to oldest-first for display if it wants a chronology.
/// This reads ONLY the small, business `domain_events` rows (each payload is a tiny JSON bag,
/// never a terminal byte stream — see the v12 migration), so no terminal output can leak here.
pub fn recent_domain_events_for_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
    project_id: &str,
    limit: i64,
) -> QueryResult<Vec<DomainEvent>> {
    domain_events::table
        .filter(
            domain_events::workspace_id
                .eq(Some(workspace_id))
                .or(domain_events::workspace_id
                    .is_null()
                    .and(domain_events::project_id.eq(project_id))),
        )
        .order(domain_events::sequence.desc())
        .limit(limit)
        .select(DomainEvent::as_select())
        .load(conn)
}

// --- Spec/task domain events (v2.4 — the Spec activity feed) --------------
//
// The Spec authoring/lifecycle mutations (`create_spec` / `update_active_revision_section`
// / `transition_spec`) and the task mutations (`create_task` / `transition_task` /
// `add_task_dependency`) are the SINGLE nyx-core authority every caller (the MCP tools and
// any UI path) funnels into, so recording a `domain_events` row INSIDE their transaction —
// exactly like the identity use cases (`usecase::persist_event`) do for project/workspace —
// makes the Spec's activity durable and atomic with the mutation it describes. Every event
// carries the concerned `spec_id` in its `payload` so the Spec-scoped feed
// ([`recent_domain_events_for_spec`]) can correlate it; a `task.*` event also carries the
// `task_id`. Correlation columns follow the outbox convention: `project_id` always, and
// `workspace_id` when the Spec is anchored (so a workspace timeline folds Spec activity in
// too). The `aggregate` is `spec` | `task`; the `event_type` follows the dotted
// `entity.action` convention of the lifecycle events (`spec.created`, `task.added`, …).

/// The `aggregate` string for a Spec-domain event (the entity CLASS the id refers to).
const AGGREGATE_SPEC: &str = "spec";
/// The `aggregate` string for a task-domain event.
const AGGREGATE_TASK: &str = "task";

/// Resolve a Spec's correlation (`project_id`, `workspace_id`) inside the current
/// transaction — `None` when the id does not resolve. The `workspace_id` is the Spec's anchor
/// (`None` until posed). Read from the mutated Spec so an event is correlated for the mutation
/// it describes (even a transition that leaves the Spec resolvable).
fn spec_correlation(
    conn: &mut SqliteConnection,
    spec_id: &str,
) -> QueryResult<Option<(String, Option<String>)>> {
    Ok(specs::table
        .find(spec_id)
        .select((specs::project_id, specs::workspace_id))
        .first::<(String, Option<String>)>(conn)
        .optional()?)
}

/// Resolve a task's correlation (`project_id`, anchor `workspace_id` of its Spec, and its
/// `spec_id`) inside the current transaction — `None` when the task id does not resolve. A
/// task carries `project_id` directly; the `workspace_id` is DERIVED from the task's Spec
/// anchor (so the event correlates to the Spec's workspace when there is one). `spec_id` is
/// `None` for light work with no Spec.
fn task_correlation(
    conn: &mut SqliteConnection,
    task_id: &str,
) -> QueryResult<Option<(String, Option<String>, Option<String>)>> {
    let Some((project_id, spec_id)) = tasks::table
        .find(task_id)
        .select((tasks::project_id, tasks::spec_id))
        .first::<(String, Option<String>)>(conn)
        .optional()?
    else {
        return Ok(None);
    };
    let workspace_id = match &spec_id {
        Some(sid) => spec_correlation(conn, sid)?.and_then(|(_, ws)| ws),
        None => None,
    };
    Ok(Some((project_id, workspace_id, spec_id)))
}

/// Record ONE `domain_events` outbox row for a SPEC-domain mutation, INSIDE the caller's
/// transaction. `event_type` is the dotted wire string (`spec.created`, …); `spec_id` is the
/// concerned Spec (carried in the payload so [`recent_domain_events_for_spec`] correlates the
/// event). `project_id` is always present; `workspace_id` is the Spec's anchor when set (so a
/// workspace timeline surfaces the Spec's activity too). Errors bubble up so a failed emit
/// rolls the whole mutation back (the event is atomic with the change it describes). The
/// `source` is left `"mcp"` — the Spec authoring surface is MCP-driven (PDR-0020: zéro CLI),
/// and the UI mutates through the same db authority.
fn record_spec_event(
    conn: &mut SqliteConnection,
    event_type: &str,
    project_id: &str,
    workspace_id: Option<&str>,
    spec_id: &str,
) -> QueryResult<()> {
    let payload = serde_json::json!({ "spec_id": spec_id }).to_string();
    record_domain_event(
        conn,
        NewDomainEvent {
            project_id: project_id.to_string(),
            workspace_id: workspace_id.map(str::to_string),
            aggregate: AGGREGATE_SPEC.to_string(),
            event_type: event_type.to_string(),
            source: "mcp".to_string(),
            actor: None,
            payload,
            created_at: 0,
        },
    )?;
    Ok(())
}

/// Record ONE `domain_events` outbox row for a TASK-domain mutation, INSIDE the caller's
/// transaction. Like [`record_spec_event`] but the payload carries BOTH the concerned
/// `task_id` and the `spec_id` it belongs to (so the Spec-scoped feed folds its tasks'
/// activity in). When the task has no Spec (light work), `spec_id` is `None` and the event is
/// still recorded (correlated to the project) but will not surface in any Spec feed.
fn record_task_event(
    conn: &mut SqliteConnection,
    event_type: &str,
    project_id: &str,
    workspace_id: Option<&str>,
    spec_id: Option<&str>,
    task_id: &str,
) -> QueryResult<()> {
    let payload = serde_json::json!({ "task_id": task_id, "spec_id": spec_id }).to_string();
    record_domain_event(
        conn,
        NewDomainEvent {
            project_id: project_id.to_string(),
            workspace_id: workspace_id.map(str::to_string),
            aggregate: AGGREGATE_TASK.to_string(),
            event_type: event_type.to_string(),
            source: "mcp".to_string(),
            actor: None,
            payload,
            created_at: 0,
        },
    )?;
    Ok(())
}

/// List the MOST RECENT `domain_events` correlated to a SPEC, newest first, capped at `limit`
/// rows — the BOUNDED slice the Spec's Activity tab reads. "Correlated to the Spec" is any
/// `spec` / `task` aggregate event whose payload carries this `spec_id` (its own lifecycle
/// plus its tasks' add/transition events). The read is ORDERED newest-first (`sequence` desc)
/// and LIMITED, so the feed is bounded by construction — it never loads the whole log. The
/// caller re-orders oldest-first for a natural chronology. Reads ONLY the small business
/// outbox rows (payload is a tiny JSON bag), never a terminal byte. The `spec_id` is matched
/// with `json_extract` over the payload (json1 is compiled into the bundled SQLite).
pub fn recent_domain_events_for_spec(
    conn: &mut SqliteConnection,
    spec_id: &str,
    limit: i64,
) -> QueryResult<Vec<DomainEvent>> {
    use diesel::dsl::sql;
    use diesel::sql_types::{Bool, Text};
    domain_events::table
        .filter(domain_events::aggregate.eq_any([AGGREGATE_SPEC, AGGREGATE_TASK]))
        .filter(sql::<Bool>("json_extract(payload, '$.spec_id') = ").bind::<Text, _>(spec_id))
        .order(domain_events::sequence.desc())
        .limit(limit)
        .select(DomainEvent::as_select())
        .load(conn)
}

// --- Spec domain models (v2.4 Phase 1, v13, PDR-0014) --------------------

/// A `specs` row: the durable Spec identity within a project (PDR-0014). `Serialize`
/// for the IPC/MCP boundary. The human key is `projects.key || '-' || number`. `status`
/// / `priority` / `mode` are TEXT with CHECK vocabularies (see the `SPEC_*` consts);
/// `phases` is a JSON list `[{number,name}]` (PDR-0022). `workspace_id` is the v2.1 seam
/// (a Spec is anchored on a dev workspace; repos + project are DERIVED from it) —
/// nullable, populated in v2.4 Phase 3. `current_revision_number` points at the single
/// ACTIVE [`SpecRevision`]; a FORK bumps it. Terminal timestamps (`completed_at` /
/// `archived_at`) are `None` while the Spec is active.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = specs)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Spec {
    pub id: String,
    pub project_id: String,
    /// The dev workspace the Spec is anchored on (the v2.1 seam). `None` until posed
    /// at dev launch (v2.4 Phase 3). Repos + project of a review are DERIVED from it.
    pub workspace_id: Option<String>,
    pub title: String,
    /// Optional descriptive slug (kept for the URL); `None` when unset.
    pub slug: Option<String>,
    /// Per-project sequence number (the human key axis; UNIQUE within the project).
    pub number: i32,
    /// Canonical `priority` (`urgent`|`high`|`med`|`low`); `med` by default.
    pub priority: String,
    /// Canonical `spec_status` (`draft`|`ready`|…); `draft` by default. The lifecycle
    /// task owns the GUARDED transitions between values; this column just stores it.
    pub status: String,
    /// Canonical `spec_mode` (`stepped`|`autopilot`); `stepped` by default.
    pub mode: String,
    /// Ordered phase plan as a JSON list `[{number,name}]` (PDR-0022); `[]` = no phases.
    pub phases: String,
    /// Points at the single ACTIVE revision (a FORK bumps this).
    pub current_revision_number: i32,
    /// Board/list position within the project.
    pub position: i32,
    pub created_at: i64,
    pub updated_at: i64,
    /// Epoch ms the Spec reached `completed`. `None` while not completed.
    pub completed_at: Option<i64>,
    /// Soft-archive timestamp, epoch ms. `None` while the Spec is active.
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = specs)]
struct NewSpec {
    id: String,
    project_id: String,
    workspace_id: Option<String>,
    title: String,
    slug: Option<String>,
    number: i32,
    current_revision_number: i32,
    position: i32,
    created_at: i64,
    updated_at: i64,
}

/// A `spec_revisions` row: one revision in a Spec's chain. `Serialize` for the IPC/MCP
/// boundary. A FORK creates a NEW revision (`revision_status = 'active'`) and marks the
/// previous one `'superseded'`; exactly ONE `active` revision exists per Spec (a partial
/// unique index enforces it). The body is STRUCTURED into the six sections the PRD names
/// (problem / solution / implementation_decisions / testing_decisions / user_stories /
/// out_of_scope — each markdown TEXT, `""` when empty). `change_reason` records WHY a
/// FORK was made.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = spec_revisions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct SpecRevision {
    pub id: String,
    pub spec_id: String,
    /// Monotonic revision number within the Spec (1, 2, 3…; UNIQUE per Spec).
    pub revision_number: i32,
    /// `active` (current, editable) or `superseded` (retired by a FORK).
    pub revision_status: String,
    pub title: String,
    /// The six STRUCTURED sections (markdown; `""` when empty).
    pub problem: String,
    pub solution: String,
    pub implementation_decisions: String,
    pub testing_decisions: String,
    pub user_stories: String,
    pub out_of_scope: String,
    /// Optional reason a FORK created this revision (why it supersedes the prior one).
    pub change_reason: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = spec_revisions)]
struct NewSpecRevision {
    id: String,
    spec_id: String,
    revision_number: i32,
    revision_status: String,
    title: String,
    problem: String,
    solution: String,
    implementation_decisions: String,
    testing_decisions: String,
    user_stories: String,
    out_of_scope: String,
    change_reason: Option<String>,
    created_at: i64,
}

/// The six STRUCTURED section fields of a Spec revision (PRD Impl Decisions), passed as
/// a group to [`create_spec`] / [`fork_spec`]. Each is markdown; [`SpecSections::empty`]
/// gives all-empty sections (a fresh draft). This mirrors the `CommandSource` grouping
/// pattern so a caller sets the sections without a long positional argument list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecSections {
    pub problem: String,
    pub solution: String,
    pub implementation_decisions: String,
    pub testing_decisions: String,
    pub user_stories: String,
    pub out_of_scope: String,
}

impl SpecSections {
    /// All-empty sections — the body of a fresh `draft` Spec's first revision.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn empty() -> Self {
        Self::default()
    }
}

/// One of the six STRUCTURED sections of a Spec revision — the addressable unit
/// [`update_active_revision_section`] edits. Mapping to the column is centralized here
/// so callers (and the authoring MCP tools of later phases) never type a column name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum SpecSection {
    Problem,
    Solution,
    ImplementationDecisions,
    TestingDecisions,
    UserStories,
    OutOfScope,
}

impl SpecSection {
    /// The stable snake_case wire name of this section (`problem`, `implementation_decisions`,
    /// …) — the same vocabulary the MCP `spec_update_section` tool accepts. Carried in the
    /// `spec.section_updated` event payload so the activity feed can name WHICH section changed.
    pub fn as_str(self) -> &'static str {
        match self {
            SpecSection::Problem => "problem",
            SpecSection::Solution => "solution",
            SpecSection::ImplementationDecisions => "implementation_decisions",
            SpecSection::TestingDecisions => "testing_decisions",
            SpecSection::UserStories => "user_stories",
            SpecSection::OutOfScope => "out_of_scope",
        }
    }
}

/// The per-project next Spec `number` (the human key axis). Returns `MAX(number) + 1`
/// for the project, or 1 when it has no Specs yet. Read inside the caller's transaction
/// so the read-then-insert is race-free under the single writer (mirrors the
/// `next_order` pattern in [`create_project`]).
fn next_spec_number(conn: &mut SqliteConnection, project_id: &str) -> QueryResult<i32> {
    use diesel::dsl::max;
    let current: Option<i32> = specs::table
        .filter(specs::project_id.eq(project_id))
        .select(max(specs::number))
        .first::<Option<i32>>(conn)?;
    Ok(current.map(|m| m + 1).unwrap_or(1))
}

/// Create a Spec in `project_id` with its FIRST revision (number 1, `active`), carrying
/// the given structured `sections`. The Spec starts in `draft` / `med` / `stepped` with
/// `current_revision_number = 1` and no `workspace_id` (the v2.1 seam is posed later).
/// The per-project `number` is assigned as `MAX+1`. Runs in ONE transaction so the Spec
/// and its first revision are atomic (a Spec always has an active revision — the
/// invariant the fork/section CRUD relies on). Returns the created [`Spec`] and its first
/// [`SpecRevision`].
#[cfg_attr(not(test), allow(dead_code))]
pub fn create_spec(
    conn: &mut SqliteConnection,
    project_id: &str,
    title: &str,
    sections: SpecSections,
) -> QueryResult<(Spec, SpecRevision)> {
    let now = now_millis();
    let spec_id = Uuid::now_v7().to_string();
    conn.transaction(|conn| {
        let number = next_spec_number(conn, project_id)?;

        let spec = diesel::insert_into(specs::table)
            .values(NewSpec {
                id: spec_id.clone(),
                project_id: project_id.to_string(),
                workspace_id: None,
                title: title.to_string(),
                slug: None,
                number,
                current_revision_number: 1,
                position: 0,
                created_at: now,
                updated_at: now,
            })
            .returning(Spec::as_returning())
            .get_result(conn)?;

        let revision = diesel::insert_into(spec_revisions::table)
            .values(NewSpecRevision {
                id: Uuid::now_v7().to_string(),
                spec_id: spec_id.clone(),
                revision_number: 1,
                revision_status: REVISION_STATUS_ACTIVE.to_string(),
                title: title.to_string(),
                problem: sections.problem,
                solution: sections.solution,
                implementation_decisions: sections.implementation_decisions,
                testing_decisions: sections.testing_decisions,
                user_stories: sections.user_stories,
                out_of_scope: sections.out_of_scope,
                change_reason: None,
                created_at: now,
            })
            .returning(SpecRevision::as_returning())
            .get_result(conn)?;

        // Outbox: the Spec creation AND its `spec.created` event commit atomically.
        record_spec_event(conn, "spec.created", project_id, None, &spec_id)?;

        Ok((spec, revision))
    })
}

/// Fetch ONE Spec by id, or `None` when the id is unknown. The read companion to the
/// Spec mutations; returns archived Specs too (callers filter on `archived_at`).
#[cfg_attr(not(test), allow(dead_code))]
pub fn get_spec(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<Spec>> {
    specs::table
        .find(id)
        .select(Spec::as_select())
        .first(conn)
        .optional()
}

/// List the Specs of `project_id`, ordered by `position` (then `number`) for a
/// deterministic board order. Excludes soft-archived rows (`archived_at IS NOT NULL`) so
/// callers see only the LIVE Specs.
#[cfg_attr(not(test), allow(dead_code))]
pub fn list_specs(conn: &mut SqliteConnection, project_id: &str) -> QueryResult<Vec<Spec>> {
    specs::table
        .filter(specs::project_id.eq(project_id))
        .filter(specs::archived_at.is_null())
        .order((specs::position.asc(), specs::number.asc()))
        .select(Spec::as_select())
        .load(conn)
}

/// The most recent LIVE Spec ANCHORED on `workspace_id` (its `workspace_id` points at it),
/// or `None` when no live Spec is anchored there. Newest first (`created_at` desc) so a
/// cwd/workspace-resolved agent context lands on the Spec currently being worked — the
/// resolution path the v2.4 Phase-5 agent-context render uses when no explicit `spec_id`
/// is given (PDR-0020: "sans id → spec résolue via cwd"). Archived Specs are excluded.
#[cfg_attr(not(test), allow(dead_code))]
pub fn spec_for_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<Option<Spec>> {
    specs::table
        .filter(specs::workspace_id.eq(workspace_id))
        .filter(specs::archived_at.is_null())
        .order(specs::created_at.desc())
        .select(Spec::as_select())
        .first(conn)
        .optional()
}

/// The single ACTIVE revision of a Spec (the one `current_revision_number` points at),
/// or `None` when the Spec is unknown. The read companion the authoring/fork CRUD keys
/// on: there is exactly one `active` revision per Spec (a partial unique index enforces
/// it), so this resolves the editable revision.
#[cfg_attr(not(test), allow(dead_code))]
pub fn get_active_revision(
    conn: &mut SqliteConnection,
    spec_id: &str,
) -> QueryResult<Option<SpecRevision>> {
    spec_revisions::table
        .filter(spec_revisions::spec_id.eq(spec_id))
        .filter(spec_revisions::revision_status.eq(REVISION_STATUS_ACTIVE))
        .select(SpecRevision::as_select())
        .first(conn)
        .optional()
}

/// List EVERY revision of a Spec, oldest first (`revision_number` asc) — the full chain
/// (active + superseded). Used to inspect the Spec's history.
#[cfg_attr(not(test), allow(dead_code))]
pub fn list_revisions(
    conn: &mut SqliteConnection,
    spec_id: &str,
) -> QueryResult<Vec<SpecRevision>> {
    spec_revisions::table
        .filter(spec_revisions::spec_id.eq(spec_id))
        .order(spec_revisions::revision_number.asc())
        .select(SpecRevision::as_select())
        .load(conn)
}

/// Edit ONE structured `section` of a Spec's ACTIVE revision (in-place authoring — NOT a
/// fork). Sets the section's column to `value` and bumps the Spec's `updated_at`, in one
/// transaction. Returns `Ok(true)` when the active revision was updated, `Ok(false)` when
/// the Spec is unknown (no active revision to edit). A FORK ([`fork_spec`]) is the
/// separate operation that snapshots the current sections into a NEW revision.
#[cfg_attr(not(test), allow(dead_code))]
pub fn update_active_revision_section(
    conn: &mut SqliteConnection,
    spec_id: &str,
    section: SpecSection,
    value: &str,
) -> QueryResult<bool> {
    conn.transaction(|conn| {
        let active_id: Option<String> = spec_revisions::table
            .filter(spec_revisions::spec_id.eq(spec_id))
            .filter(spec_revisions::revision_status.eq(REVISION_STATUS_ACTIVE))
            .select(spec_revisions::id)
            .first::<String>(conn)
            .optional()?;
        let Some(active_id) = active_id else {
            return Ok(false);
        };
        // Each arm targets its own column; the value type is uniform (TEXT), so the
        // match selects the column and issues the matching update.
        let target = spec_revisions::table.find(&active_id);
        match section {
            SpecSection::Problem => {
                diesel::update(target)
                    .set(spec_revisions::problem.eq(value))
                    .execute(conn)?;
            }
            SpecSection::Solution => {
                diesel::update(target)
                    .set(spec_revisions::solution.eq(value))
                    .execute(conn)?;
            }
            SpecSection::ImplementationDecisions => {
                diesel::update(target)
                    .set(spec_revisions::implementation_decisions.eq(value))
                    .execute(conn)?;
            }
            SpecSection::TestingDecisions => {
                diesel::update(target)
                    .set(spec_revisions::testing_decisions.eq(value))
                    .execute(conn)?;
            }
            SpecSection::UserStories => {
                diesel::update(target)
                    .set(spec_revisions::user_stories.eq(value))
                    .execute(conn)?;
            }
            SpecSection::OutOfScope => {
                diesel::update(target)
                    .set(spec_revisions::out_of_scope.eq(value))
                    .execute(conn)?;
            }
        }
        diesel::update(specs::table.find(spec_id))
            .set(specs::updated_at.eq(now_millis()))
            .execute(conn)?;

        // Outbox: the section edit AND its `spec.section_updated` event commit atomically.
        // The payload names WHICH section changed (`section`) on top of the `spec_id`.
        if let Some((project_id, workspace_id)) = spec_correlation(conn, spec_id)? {
            let payload = serde_json::json!({
                "spec_id": spec_id,
                "section": section.as_str(),
            })
            .to_string();
            record_domain_event(
                conn,
                NewDomainEvent {
                    project_id,
                    workspace_id,
                    aggregate: AGGREGATE_SPEC.to_string(),
                    event_type: "spec.section_updated".to_string(),
                    source: "mcp".to_string(),
                    actor: None,
                    payload,
                    created_at: 0,
                },
            )?;
        }
        Ok(true)
    })
}

/// FORK a Spec: create a NEW revision (the previous one becomes `superseded`) and point
/// the Spec's `current_revision_number` at it. This is THE fork semantics of the Spec
/// domain (PDR-0014): the new revision SNAPSHOTS the current active revision's structured
/// sections + title (so a fork starts from where the last one left off), records the
/// optional `change_reason`, and becomes the single `active` revision. The old revision
/// is retained (superseded) as history. Runs in ONE transaction so the supersede + insert
/// + pointer bump are atomic — there is never zero or two active revisions.
///
/// Returns the new active [`SpecRevision`]. Errors if the Spec is unknown (no active
/// revision to fork from) — a Spec always has an active revision by the [`create_spec`]
/// invariant, so a missing one means the id is wrong.
#[cfg_attr(not(test), allow(dead_code))]
pub fn fork_spec(
    conn: &mut SqliteConnection,
    spec_id: &str,
    change_reason: Option<&str>,
) -> QueryResult<SpecRevision> {
    let now = now_millis();
    conn.transaction(|conn| {
        // The current active revision is the base of the fork.
        let current: SpecRevision = spec_revisions::table
            .filter(spec_revisions::spec_id.eq(spec_id))
            .filter(spec_revisions::revision_status.eq(REVISION_STATUS_ACTIVE))
            .select(SpecRevision::as_select())
            .first(conn)?;

        // Supersede it FIRST so the partial unique index (one active per Spec) never
        // sees two active rows during the insert.
        diesel::update(spec_revisions::table.find(&current.id))
            .set(spec_revisions::revision_status.eq(REVISION_STATUS_SUPERSEDED))
            .execute(conn)?;

        let next_number = current.revision_number + 1;
        let revision = diesel::insert_into(spec_revisions::table)
            .values(NewSpecRevision {
                id: Uuid::now_v7().to_string(),
                spec_id: spec_id.to_string(),
                revision_number: next_number,
                revision_status: REVISION_STATUS_ACTIVE.to_string(),
                // Snapshot the base revision's title + sections so the fork continues
                // from the last state rather than resetting to empty.
                title: current.title.clone(),
                problem: current.problem.clone(),
                solution: current.solution.clone(),
                implementation_decisions: current.implementation_decisions.clone(),
                testing_decisions: current.testing_decisions.clone(),
                user_stories: current.user_stories.clone(),
                out_of_scope: current.out_of_scope.clone(),
                change_reason: change_reason.map(str::to_string),
                created_at: now,
            })
            .returning(SpecRevision::as_returning())
            .get_result(conn)?;

        // Point the Spec at the new active revision.
        diesel::update(specs::table.find(spec_id))
            .set((
                specs::current_revision_number.eq(next_number),
                specs::updated_at.eq(now),
            ))
            .execute(conn)?;

        // Outbox: the fork AND its `spec.forked` event commit atomically. The payload names
        // the base (`from`) and new (`to`) revision numbers so the activity feed makes the
        // forked revision visible (it was invisible before — the F4 coverage gap).
        if let Some((project_id, workspace_id)) = spec_correlation(conn, spec_id)? {
            let payload = serde_json::json!({
                "spec_id": spec_id,
                "from": current.revision_number,
                "to": next_number,
            })
            .to_string();
            record_domain_event(
                conn,
                NewDomainEvent {
                    project_id,
                    workspace_id,
                    aggregate: AGGREGATE_SPEC.to_string(),
                    event_type: "spec.forked".to_string(),
                    source: "mcp".to_string(),
                    actor: None,
                    payload,
                    created_at: 0,
                },
            )?;
        }

        Ok(revision)
    })
}

// --- Spec LIFECYCLE: guarded + validated transitions (v2.4 Phase 1, v13) ----
//
// The Spec cycle of life, GUARDED (valid source state) and VALIDATED, carried in the
// nyx-core backend (task 01KX1DH59XXJ6XVCXE6Z33H1E0). The transitions are:
//
//   draft ──ready──▶ ready ──start──▶ in_progress ──review──▶ in_review ──done──▶ completed
//     │                │                   │                      │
//     └────────────────┴───────cancel──────┴──────────────────────┴──────────▶ cancelled
//
// (The canonical `spec_status` stores `in_review`/`completed`/`cancelled`; the
// conceptual lifecycle points `review`/`done`/`cancel` map onto them.)
//
// Two orthogonal gates apply to every transition:
//   * GUARD — the source state must be a legal predecessor (a draft can go ready, but
//     an in_progress cannot go ready again). Encoded in [`SpecTransition::source_allowed`].
//   * VALIDATION — a transition may carry business preconditions. The `ready` gate runs
//     the PRE-READY validations ([`validate_ready`]): the design must be substantive
//     (criteria present) and the phase plan coherent (no cycle / well-formed ordering) —
//     an INCOMPLETE draft must NOT pass ready.
//
// GARDE-FOUS (guard-rails), in the spirit of depot's `--user-confirmed`: the SENSITIVE
// transitions (`ready`, `start`, `done`, `cancel`) require an explicit user confirmation.
// `start` is sensitive because it POSES the workspace anchor (ready→in_progress binds the
// Spec to a dev workspace and kicks off implementation — a consequential, human-owned move).
// The backend refuses them with [`SpecTransitionError::ConfirmationRequired`] unless the
// caller passes `user_confirmed = true`, so a UI/MCP mutation cannot silently mark a
// Spec ready/started/done/cancelled without the human's go-ahead.

/// A Spec lifecycle transition (the conceptual move, independent of the stored status
/// string). Each maps to a canonical target `spec_status`, declares its legal source
/// states (the GUARD), and whether it is SENSITIVE (needs user confirmation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecTransition {
    /// draft → ready. Sensitive; runs the pre-ready VALIDATIONS.
    Ready,
    /// ready → in_progress. Sensitive: it POSES the Spec's workspace anchor
    /// ([`transition_spec_with_workspace`]) in the SAME operation.
    Start,
    /// in_progress → in_review ("review"). Not sensitive.
    Review,
    /// in_review → completed ("done"). Sensitive.
    Done,
    /// (draft|ready|in_progress|in_review) → cancelled. Sensitive.
    Cancel,
}

impl SpecTransition {
    /// The canonical `spec_status` this transition moves the Spec TO.
    pub fn target_status(self) -> &'static str {
        match self {
            SpecTransition::Ready => SPEC_STATUS_READY,
            SpecTransition::Start => SPEC_STATUS_IN_PROGRESS,
            SpecTransition::Review => SPEC_STATUS_IN_REVIEW,
            SpecTransition::Done => SPEC_STATUS_COMPLETED,
            SpecTransition::Cancel => SPEC_STATUS_CANCELLED,
        }
    }

    /// The VERB of the transition (the conceptual move requested), independent of the target
    /// status it lands on. Carried in the `spec.transitioned` event payload so the activity
    /// feed can distinguish moves that share a target status (e.g. a `cancel` from any other
    /// path onto `cancelled`) — the `to` status alone is ambiguous.
    pub fn verb(self) -> &'static str {
        match self {
            SpecTransition::Ready => "ready",
            SpecTransition::Start => "start",
            SpecTransition::Review => "review",
            SpecTransition::Done => "done",
            SpecTransition::Cancel => "cancel",
        }
    }

    /// The GUARD: is `from` (a canonical `spec_status`) a legal SOURCE for this
    /// transition? Encodes the lifecycle edges — a transition from an illegal source is
    /// refused with [`SpecTransitionError::InvalidSource`].
    pub fn source_allowed(self, from: &str) -> bool {
        match self {
            SpecTransition::Ready => from == SPEC_STATUS_DRAFT,
            SpecTransition::Start => from == SPEC_STATUS_READY,
            SpecTransition::Review => from == SPEC_STATUS_IN_PROGRESS,
            SpecTransition::Done => from == SPEC_STATUS_IN_REVIEW,
            // Cancel is reachable from any NON-terminal state (not from completed /
            // cancelled / archived — those are terminal).
            SpecTransition::Cancel => matches!(
                from,
                SPEC_STATUS_DRAFT
                    | SPEC_STATUS_READY
                    | SPEC_STATUS_IN_PROGRESS
                    | SPEC_STATUS_IN_REVIEW
            ),
        }
    }

    /// Whether this transition is SENSITIVE and therefore requires an explicit user
    /// confirmation (the guard-rail, à la depot `--user-confirmed`): `ready`, `start`,
    /// `done`, `cancel`. `start` is sensitive because it POSES the workspace anchor
    /// (ready→in_progress). Only `review` is routine and needs no confirmation.
    pub fn is_sensitive(self) -> bool {
        matches!(
            self,
            SpecTransition::Ready
                | SpecTransition::Start
                | SpecTransition::Done
                | SpecTransition::Cancel
        )
    }
}

/// One reason a Spec's design is NOT ready — a PRE-READY validation failure. A `draft`
/// that produces any of these must NOT pass `ready`. Serializable so a UI/MCP surface can
/// show the human EXACTLY what is missing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadyValidation {
    /// The active revision has no `problem` section — the design has no stated problem
    /// (criteria absent). An empty draft trips this.
    MissingProblem,
    /// The active revision has no `solution` section — the design has no proposed
    /// solution (criteria absent).
    MissingSolution,
    /// The `phases` JSON is malformed / not a list of `{number,name}` — an incoherent
    /// phase plan. Carries a short human reason.
    IncoherentPhasePlan { reason: String },
    /// The phase plan has a DUPLICATE or non-sequential phase `number` — a cycle/ordering
    /// incoherence (phase numbers must be a clean 1..N ordering, no repeats/gaps).
    PhasePlanOrdering { reason: String },
}

/// One phase entry in `specs.phases` (`[{number,name}]`, PDR-0022). Parsed to VALIDATE the
/// plan's coherence for the pre-ready gate, and re-serialized (same shape) when
/// [`resolve_or_create_phase`] GROWS the plan from a task's phase name; the column stays the
/// JSON source of truth.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct PhaseEntry {
    number: i64,
    #[serde(default)]
    name: String,
}

/// Run the PRE-READY VALIDATIONS on a Spec and return the list of failures (empty = the
/// design is ready to pass the gate). This is the pure, side-effect-free check the
/// `ready` transition runs; it is also exposed on its own so a UI/MCP surface can preview
/// "why is this draft not ready" WITHOUT attempting the transition.
///
/// The checks, against the data the Spec model carries:
///   * CRITERIA PRESENT — the active revision must state a `problem` AND a `solution`
///     (a blank draft, with empty sections, fails). This is the "an incomplete draft
///     must NOT pass ready" gate.
///   * COHERENT PHASE PLAN (deps / cycles / phases) — `specs.phases` must parse as a list
///     of `{number,name}` with a clean, acyclic 1..N ordering (each `number` present once,
///     no duplicate/out-of-range number — a duplicate/gap is an ordering cycle in the
///     phase graph). An empty plan (`[]`, tasks flat) is coherent by construction.
///
/// Returns `Err(reasons)` when the Spec is unknown (a single synthetic reason) is NOT the
/// shape here — an unknown id surfaces as a DB `NotFound` at the [`transition_spec`]
/// layer; this function assumes the Spec exists and validates its content. It resolves
/// the active revision itself.
#[cfg_attr(not(test), allow(dead_code))]
pub fn validate_ready(
    conn: &mut SqliteConnection,
    spec_id: &str,
) -> QueryResult<Vec<ReadyValidation>> {
    let mut failures = Vec::new();

    // CRITERIA PRESENT — the active revision must have a problem + a solution.
    if let Some(revision) = get_active_revision(conn, spec_id)? {
        if revision.problem.trim().is_empty() {
            failures.push(ReadyValidation::MissingProblem);
        }
        if revision.solution.trim().is_empty() {
            failures.push(ReadyValidation::MissingSolution);
        }
    } else {
        // No active revision at all → nothing is designed. Both criteria are missing.
        failures.push(ReadyValidation::MissingProblem);
        failures.push(ReadyValidation::MissingSolution);
    }

    // COHERENT PHASE PLAN — resolve the Spec's `phases` and validate its coherence.
    let phases_json: Option<String> = specs::table
        .find(spec_id)
        .select(specs::phases)
        .first::<String>(conn)
        .optional()?;
    if let Some(phases_json) = phases_json {
        failures.extend(validate_phase_plan(&phases_json));
    }

    Ok(failures)
}

/// Validate a `specs.phases` JSON string for coherence (the phase-plan / cycle gate).
/// Pure (no DB): parses and checks the ordering. An empty list is coherent. Returns the
/// list of [`ReadyValidation`] the plan trips (empty = coherent).
///
/// The SINGLE coherence authority for a phase plan (numbers a clean 1..N, names non-empty).
/// The plan is never authored directly: it self-builds as tasks name their phase
/// ([`create_task`] → [`resolve_or_create_phase`]), and this same check runs at the `ready`
/// gate ([`validate_ready`], above) — so the authoring write and the lifecycle gate share
/// ONE rule, with no drift between them. `pub(crate)`: every caller is in-crate (the gate
/// plus same-crate tests asserting a self-built plan is coherent); no out-of-crate consumer.
pub(crate) fn validate_phase_plan(phases_json: &str) -> Vec<ReadyValidation> {
    let trimmed = phases_json.trim();
    // An absent plan ('' or '[]') is coherent by construction (tasks flat).
    if trimmed.is_empty() || trimmed == "[]" {
        return Vec::new();
    }
    let parsed: Result<Vec<PhaseEntry>, _> = serde_json::from_str(trimmed);
    let entries = match parsed {
        Ok(entries) => entries,
        Err(e) => {
            return vec![ReadyValidation::IncoherentPhasePlan {
                reason: format!("phases is not a list of {{number,name}}: {e}"),
            }];
        }
    };
    let mut failures = Vec::new();
    // Every phase must carry a non-empty name.
    if entries.iter().any(|p| p.name.trim().is_empty()) {
        failures.push(ReadyValidation::IncoherentPhasePlan {
            reason: "a phase has an empty name".to_string(),
        });
    }
    // The numbers must be a clean 1..N ordering: each of 1..=N present exactly once (no
    // duplicate — a repeated number is an ordering cycle — and no gap/out-of-range).
    let n = entries.len() as i64;
    let mut numbers: Vec<i64> = entries.iter().map(|p| p.number).collect();
    numbers.sort_unstable();
    let expected: Vec<i64> = (1..=n).collect();
    if numbers != expected {
        failures.push(ReadyValidation::PhasePlanOrdering {
            reason: format!(
                "phase numbers must be a clean 1..{n} ordering (no duplicate/gap); got {:?}",
                entries.iter().map(|p| p.number).collect::<Vec<_>>()
            ),
        });
    }
    failures
}

/// Why a Spec lifecycle transition was refused (or the DB failed). The nyx-core layer
/// returns this so a UI/MCP caller can react precisely (show the invalid-source message,
/// list the validation failures, or prompt for confirmation).
#[derive(Debug)]
pub enum SpecTransitionError {
    /// No Spec resolves to the given id.
    NotFound,
    /// The GUARD failed: the Spec's current `status` (`from`) is not a legal source for
    /// the requested transition (which targets `to`).
    InvalidSource { from: String, to: &'static str },
    /// The VALIDATION failed: the pre-ready checks produced these failures (only ready).
    ValidationFailed(Vec<ReadyValidation>),
    /// The GUARD-RAIL: a SENSITIVE transition (ready/start/done/cancel) was attempted
    /// without the user's explicit confirmation (`user_confirmed = false`).
    ConfirmationRequired,
    /// A `start` transition carried a `workspace_id` to anchor on, but no workspace resolves
    /// to it (the anchor GUARD — a Spec never anchors on a dangling id, exactly as
    /// [`SetSpecWorkspaceError::WorkspaceNotFound`] guards the standalone anchor).
    WorkspaceNotFound,
    /// A DB error surfaced while resolving or updating.
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for SpecTransitionError {
    fn from(e: diesel::result::Error) -> Self {
        SpecTransitionError::Db(e)
    }
}

/// Perform a Spec lifecycle `transition`, GUARDED + VALIDATED + guard-railed, and return
/// the updated [`Spec`]. The no-anchor convenience over [`transition_spec_with_workspace`]
/// (like [`create_spec`] over [`create_spec_with_workspace`]): every transition BUT `start`
/// never touches the workspace anchor, so those callers need not thread a workspace id.
#[cfg_attr(not(test), allow(dead_code))]
pub fn transition_spec(
    conn: &mut SqliteConnection,
    spec_id: &str,
    transition: SpecTransition,
    user_confirmed: bool,
) -> Result<Spec, SpecTransitionError> {
    transition_spec_with_workspace(conn, spec_id, transition, user_confirmed, None)
}

/// Perform a Spec lifecycle `transition`, GUARDED + VALIDATED + guard-railed, and — on the
/// `start` transition (ready→in_progress) — ANCHOR the Spec on `workspace_id` in the SAME
/// transaction. This is the SINGLE nyx-core owner of a Spec status change (both the UI and
/// the MCP transition tools route through it), so the lifecycle rules live in one place:
///
///   1. RESOLVE the Spec (unknown id → [`SpecTransitionError::NotFound`]).
///   2. GUARD-RAIL: a SENSITIVE transition (ready/start/done/cancel) without `user_confirmed`
///      is refused with [`SpecTransitionError::ConfirmationRequired`] — the human must
///      confirm (à la depot `--user-confirmed`).
///   3. GUARD: the current `status` must be a legal SOURCE ([`SpecTransition::source_allowed`]),
///      else [`SpecTransitionError::InvalidSource`].
///   4. VALIDATE: the `ready` transition runs [`validate_ready`]; any failure →
///      [`SpecTransitionError::ValidationFailed`] (an incomplete draft does NOT pass ready).
///   5. ANCHOR (start only): a non-null `workspace_id` MUST resolve to an existing workspace
///      ([`SpecTransitionError::WorkspaceNotFound`]); it is then written onto `specs.workspace_id`
///      TOGETHER with the status flip. So the anchor is posed EXACTLY at ready→in_progress, in
///      one atomic move — never at authoring (the review finding fix). For every other
///      transition `workspace_id` is ignored (the anchor is left untouched).
///   6. WRITE the new `status` (and stamp `completed_at` on `done`, or the anchor on `start`),
///      bumping `updated_at`.
///
/// Everything runs in ONE transaction so a refusal writes nothing.
#[cfg_attr(not(test), allow(dead_code))]
pub fn transition_spec_with_workspace(
    conn: &mut SqliteConnection,
    spec_id: &str,
    transition: SpecTransition,
    user_confirmed: bool,
    workspace_id: Option<&str>,
) -> Result<Spec, SpecTransitionError> {
    conn.transaction(|conn| {
        let spec = match get_spec(conn, spec_id)? {
            None => return Err(SpecTransitionError::NotFound),
            Some(s) => s,
        };

        // GUARD-RAIL: sensitive transitions demand explicit user confirmation.
        if transition.is_sensitive() && !user_confirmed {
            return Err(SpecTransitionError::ConfirmationRequired);
        }

        // GUARD: the current status must be a legal source for this transition.
        if !transition.source_allowed(&spec.status) {
            return Err(SpecTransitionError::InvalidSource {
                from: spec.status.clone(),
                to: transition.target_status(),
            });
        }

        // VALIDATE: the ready gate runs the pre-ready validations.
        if transition == SpecTransition::Ready {
            let failures = validate_ready(conn, spec_id)?;
            if !failures.is_empty() {
                return Err(SpecTransitionError::ValidationFailed(failures));
            }
        }

        // ANCHOR (start only): the workspace to anchor on MUST exist. Resolved here so the
        // anchor write and the status flip share the transaction (atomic transition + anchor).
        let start_anchor: Option<&str> = if transition == SpecTransition::Start {
            match workspace_id {
                Some(ws) => {
                    let exists = workspaces::table
                        .find(ws)
                        .select(workspaces::id)
                        .first::<String>(conn)
                        .optional()?;
                    if exists.is_none() {
                        return Err(SpecTransitionError::WorkspaceNotFound);
                    }
                    Some(ws)
                }
                None => None,
            }
        } else {
            None
        };

        // WRITE the new status (+ completed_at on done, or the workspace anchor on start),
        // bumping updated_at.
        let now = now_millis();
        let target = transition.target_status();
        if transition == SpecTransition::Done {
            diesel::update(specs::table.find(spec_id))
                .set((
                    specs::status.eq(target),
                    specs::completed_at.eq(now),
                    specs::updated_at.eq(now),
                ))
                .execute(conn)?;
        } else if let Some(ws) = start_anchor {
            diesel::update(specs::table.find(spec_id))
                .set((
                    specs::status.eq(target),
                    specs::workspace_id.eq(ws),
                    specs::updated_at.eq(now),
                ))
                .execute(conn)?;
        } else {
            diesel::update(specs::table.find(spec_id))
                .set((specs::status.eq(target), specs::updated_at.eq(now)))
                .execute(conn)?;
        }

        // Outbox: the status flip AND its `spec.transitioned` event commit atomically. The
        // payload records the `from`/`to` status AND the requested `verb` (the conceptual
        // move) so the feed can distinguish moves onto the same target status (e.g. `cancel`).
        // Correlate the event to the EFFECTIVE workspace — the freshly-posed anchor on a
        // `start`, else the Spec's existing one — so the workspace timeline folds it in.
        let effective_ws = start_anchor
            .map(str::to_string)
            .or_else(|| spec.workspace_id.clone());
        let payload = serde_json::json!({
            "spec_id": spec_id,
            "from": spec.status,
            "to": target,
            "verb": transition.verb(),
        })
        .to_string();
        record_domain_event(
            conn,
            NewDomainEvent {
                project_id: spec.project_id.clone(),
                workspace_id: effective_ws,
                aggregate: AGGREGATE_SPEC.to_string(),
                event_type: "spec.transitioned".to_string(),
                source: "mcp".to_string(),
                actor: None,
                payload,
                created_at: 0,
            },
        )?;

        let updated = get_spec(conn, spec_id)?.ok_or(SpecTransitionError::NotFound)?;
        Ok(updated)
    })
}

/// Set a Spec's ordered `phases` plan (`[{number,name}]`, PDR-0022) as a JSON string,
/// bumping `updated_at`. Returns rows updated (0 = unknown id). The coherence of the plan
/// is checked at the `ready` gate ([`validate_ready`]); this write is the authoring
/// primitive the UI/MCP phase-plan editor drives. Kept minimal (a raw JSON set) so the
/// authoring layer owns the shape; the lifecycle only VALIDATES it.
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_spec_phases(
    conn: &mut SqliteConnection,
    spec_id: &str,
    phases_json: &str,
) -> QueryResult<usize> {
    diesel::update(specs::table.find(spec_id))
        .set((
            specs::phases.eq(phases_json),
            specs::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// GET-OR-CREATE a phase by NAME in a Spec's `specs.phases` plan, returning its 1-based
/// number (PDR-0022). If a phase whose (trimmed) name already matches `name` exists, its
/// number is returned unchanged (reuse — so a name passed twice maps to the SAME phase);
/// otherwise the name is APPENDED as a new phase with the next number (max existing + 1 —
/// 1-based, in FIRST-SEEN order) and the grown plan is persisted through [`set_spec_phases`].
///
/// This is the authoring primitive behind `task_add(phase="Name")`: the plan MATERIALIZES
/// incrementally from the names the tasks carry, so a task's phase can never point outside
/// the plan (no orphan by construction). It runs on the CALLER's connection so it composes
/// INTO the task-insert transaction ([`create_task`]) — the plan grow and the task write
/// commit atomically. A plan the new flow builds is a clean 1..N (it starts empty and only
/// ever appends); a legacy plan is read as-is and never renumbered.
fn resolve_or_create_phase(
    conn: &mut SqliteConnection,
    spec_id: &str,
    name: &str,
) -> QueryResult<i32> {
    let target = name.trim();
    let phases_json: String = specs::table
        .find(spec_id)
        .select(specs::phases)
        .first::<String>(conn)?;
    let trimmed = phases_json.trim();
    let mut entries: Vec<PhaseEntry> = if trimmed.is_empty() || trimmed == "[]" {
        Vec::new()
    } else {
        serde_json::from_str(trimmed)
            .map_err(|e| diesel::result::Error::DeserializationError(Box::new(e)))?
    };
    // GET: an existing phase with the same (trimmed) name keeps its number.
    if let Some(existing) = entries.iter().find(|e| e.name.trim() == target) {
        return Ok(existing.number as i32);
    }
    // CREATE: append with the next number (max + 1 — 1-based, first-seen order).
    let next = entries.iter().map(|e| e.number).max().unwrap_or(0) + 1;
    entries.push(PhaseEntry {
        number: next,
        name: target.to_string(),
    });
    let grown = serde_json::to_string(&entries)
        .map_err(|e| diesel::result::Error::SerializationError(Box::new(e)))?;
    set_spec_phases(conn, spec_id, &grown)?;
    Ok(next as i32)
}

// --- Spec ↔ workspace LINK + DERIVED repos/project (v2.4 Phase 3, v15) ------
//
// REALIZE `specs.workspace_id` — the v2.1 reserved seam (declared nullable in v13, left
// NULL there). A Spec is ANCHORED on a dev WORKSPACE; that anchor is what gives the review
// and the coder their context: the associated REPOS (and the PROJECT). Per the canonical
// model (data-model.dbml, PDR-0013) the repos + project are DERIVED THROUGH the workspace,
// NEVER duplicated onto the Spec:
//
//   Spec ──workspace_id──▶ Workspace ──project_id──▶ Project ──┐
//                                                              └──▶ Repositories (project's)
//
// There is deliberately NO checkout table: a workspace's repos are its PROJECT's logical
// `repositories` (the non-optional ones are worktree'd at `workspace.root_path ||
// relative_path`; Git state is re-read from Git). So the derivation is a plain read chain,
// not a stored copy. [`spec_workspace_context`] resolves and returns the whole shape as a
// serializable struct — the BACKEND DATA PATH the UI (Phase 4) and MCP (Phase 5) will
// consume; this task ships the capability + its serializable shape, not those surfaces.

/// Why setting a Spec's `workspace_id` was refused (or the DB failed). Mirrors the
/// [`SpecTransitionError`] shape so a UI/MCP caller can react precisely. The GUARD is
/// "the workspace must exist": a Spec must not anchor on a dangling workspace id.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum SetSpecWorkspaceError {
    /// No Spec resolves to the given id.
    SpecNotFound,
    /// A non-null `workspace_id` was given but no workspace resolves to it (the GUARD).
    WorkspaceNotFound,
    /// The STATE GUARD: the standalone anchor is only legal while the Spec is `in_progress`.
    /// A Spec in `draft`/`ready` (or a terminal state) is refused — the anchor is posed at
    /// the ready→in_progress transition (the review finding), never earlier. Carries the
    /// current status so the caller can explain the refusal.
    InvalidState { status: String },
    /// A DB error surfaced while resolving or updating.
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for SetSpecWorkspaceError {
    fn from(e: diesel::result::Error) -> Self {
        SetSpecWorkspaceError::Db(e)
    }
}

/// SET (or CLEAR) a Spec's `workspace_id` — the anchor on a dev workspace (the v2.1 seam,
/// realized here). STATE-GUARDED: only a Spec that is `in_progress` may (re)anchor or detach
/// through this standalone path — a `draft`/`ready` (or terminal) Spec is refused with
/// [`SetSpecWorkspaceError::InvalidState`], because the anchor is posed at the ready→in_progress
/// transition ([`transition_spec_with_workspace`]), never at authoring (the review finding).
/// Also GUARDED: a non-null `workspace_id` MUST resolve to an existing workspace (else
/// [`SetSpecWorkspaceError::WorkspaceNotFound`]), so a Spec never anchors on a dangling id;
/// passing `None` DETACHES the Spec (a sane, allowed state while in_progress — the pointer is
/// "posé au lancement, écrasable, jamais perdu", data-model.dbml). Bumps `updated_at`.
/// Returns the updated [`Spec`]. Runs in ONE transaction so a refusal writes nothing. This
/// is the SINGLE nyx-core owner of the standalone anchor write (UI + MCP route here).
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_spec_workspace(
    conn: &mut SqliteConnection,
    spec_id: &str,
    workspace_id: Option<&str>,
) -> Result<Spec, SetSpecWorkspaceError> {
    conn.transaction(|conn| {
        // RESOLVE the Spec first (unknown id → SpecNotFound, not a silent no-op). Read the
        // whole row so we know the PRIOR anchor (`from`) and the `project_id` for the event.
        let existing = match get_spec(conn, spec_id)? {
            None => return Err(SetSpecWorkspaceError::SpecNotFound),
            Some(s) => s,
        };
        // STATE GUARD: the standalone (re)anchor/detach is only legal while in_progress. In
        // draft/ready the anchor must NOT be posed — it belongs to the start transition.
        if existing.status != SPEC_STATUS_IN_PROGRESS {
            return Err(SetSpecWorkspaceError::InvalidState {
                status: existing.status.clone(),
            });
        }
        // GUARD: a non-null anchor must point at an existing workspace.
        if let Some(ws_id) = workspace_id {
            let ws_exists = workspaces::table
                .find(ws_id)
                .select(workspaces::id)
                .first::<String>(conn)
                .optional()?;
            if ws_exists.is_none() {
                return Err(SetSpecWorkspaceError::WorkspaceNotFound);
            }
        }
        let spec = diesel::update(specs::table.find(spec_id))
            .set((
                specs::workspace_id.eq(workspace_id),
                specs::updated_at.eq(now_millis()),
            ))
            .returning(Spec::as_returning())
            .get_result(conn)?;

        // Outbox: the re-anchoring AND its `spec.workspace_changed` event commit atomically.
        // The payload records the workspace `from`/`to` (both nullable — `None` when the Spec
        // was/becomes detached) so a re-anchoring is visible in Activity. Correlate the event
        // to the NEW anchor so the destination workspace's timeline folds it in too.
        let payload = serde_json::json!({
            "spec_id": spec_id,
            "from": existing.workspace_id,
            "to": workspace_id,
        })
        .to_string();
        record_domain_event(
            conn,
            NewDomainEvent {
                project_id: existing.project_id.clone(),
                workspace_id: workspace_id.map(str::to_string),
                aggregate: AGGREGATE_SPEC.to_string(),
                event_type: "spec.workspace_changed".to_string(),
                source: "mcp".to_string(),
                actor: None,
                payload,
                created_at: 0,
            },
        )?;

        Ok(spec)
    })
}

/// Create a Spec ALREADY ANCHORED on a dev workspace, in ONE transaction. Same as
/// [`create_spec`] (first `active` revision, `draft`/`med`/`stepped`, per-project `MAX+1`
/// number) but with the v2.1 seam posed at creation. GUARDED: `workspace_id` MUST resolve
/// to an existing workspace ([`SetSpecWorkspaceError::WorkspaceNotFound`]) — the Spec never
/// starts anchored on a dangling id. Returns the created [`Spec`] and its first
/// [`SpecRevision`]. (The no-workspace path stays [`create_spec`].)
///
/// NOTE (review finding — anchor at the transition, not at authoring): the PRODUCTION path
/// no longer anchors at birth. `spec_create` (the MCP tool) creates a PROJECT-scoped draft
/// via [`create_spec`]; the anchor is posed later, at the ready→in_progress `start`
/// transition ([`transition_spec_with_workspace`]). This lower-level primitive remains as a
/// test/seed helper to stand up an anchored Spec directly, bypassing the lifecycle walk.
#[cfg_attr(not(test), allow(dead_code))]
pub fn create_spec_with_workspace(
    conn: &mut SqliteConnection,
    project_id: &str,
    workspace_id: &str,
    title: &str,
    sections: SpecSections,
) -> Result<(Spec, SpecRevision), SetSpecWorkspaceError> {
    conn.transaction(|conn| {
        // GUARD: the workspace must exist before we anchor a Spec on it.
        let ws_exists = workspaces::table
            .find(workspace_id)
            .select(workspaces::id)
            .first::<String>(conn)
            .optional()?;
        if ws_exists.is_none() {
            return Err(SetSpecWorkspaceError::WorkspaceNotFound);
        }
        let now = now_millis();
        let spec_id = Uuid::now_v7().to_string();
        let number = next_spec_number(conn, project_id)?;

        let spec = diesel::insert_into(specs::table)
            .values(NewSpec {
                id: spec_id.clone(),
                project_id: project_id.to_string(),
                workspace_id: Some(workspace_id.to_string()),
                title: title.to_string(),
                slug: None,
                number,
                current_revision_number: 1,
                position: 0,
                created_at: now,
                updated_at: now,
            })
            .returning(Spec::as_returning())
            .get_result(conn)?;

        let revision = diesel::insert_into(spec_revisions::table)
            .values(NewSpecRevision {
                id: Uuid::now_v7().to_string(),
                spec_id: spec_id.clone(),
                revision_number: 1,
                revision_status: REVISION_STATUS_ACTIVE.to_string(),
                title: title.to_string(),
                problem: sections.problem,
                solution: sections.solution,
                implementation_decisions: sections.implementation_decisions,
                testing_decisions: sections.testing_decisions,
                user_stories: sections.user_stories,
                out_of_scope: sections.out_of_scope,
                change_reason: None,
                created_at: now,
            })
            .returning(SpecRevision::as_returning())
            .get_result(conn)?;

        // Outbox: the anchored Spec creation AND its `spec.created` event commit atomically,
        // correlated to the anchor workspace so a workspace timeline folds it in too.
        record_spec_event(
            conn,
            "spec.created",
            project_id,
            Some(workspace_id),
            &spec_id,
        )?;

        Ok((spec, revision))
    })
}

/// The DERIVED context of a Spec's dev workspace: the [`Workspace`] it is anchored on, the
/// [`Project`] that workspace belongs to, and that project's live [`Repository`] set — the
/// repos + project a review/coder needs, DERIVED THROUGH the workspace, never stored on the
/// Spec (PDR-0013). `Serialize` for the IPC/MCP boundary: this is the serializable shape the
/// Phase-4 UI and Phase-5 MCP tools consume. Every id is captured so a consumer can follow
/// the chain without re-deriving it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SpecWorkspaceContext {
    /// The Spec this context was derived for.
    pub spec_id: String,
    /// The dev workspace the Spec is anchored on.
    pub workspace: Workspace,
    /// The PROJECT the workspace belongs to — DERIVED via `workspace.project_id`, not a
    /// column on the Spec.
    pub project: Project,
    /// The project's LIVE logical repositories (`archived_at IS NULL`), ordered by
    /// `relative_path` — DERIVED via the project, not duplicated onto the Spec. The
    /// non-`optional` ones are the repos worktree'd at `workspace.root_path || relative_path`;
    /// `optional` marks the in-DB "nyxignore" so a consumer can filter (PDR-0013).
    pub repos: Vec<Repository>,
}

/// DERIVE and return a Spec's workspace context — its [`Workspace`] + the [`Project`] and
/// [`Repository`] set DERIVED THROUGH that workspace (never stored on the Spec, PDR-0013).
/// This is THE Phase-3 accessor: the backend data path the review, the UI (Phase 4) and the
/// MCP tools (Phase 5) consume to know a Spec's repos + project.
///
/// The derivation chain (a plain read, no copy):
///   1. RESOLVE the Spec (unknown id → `Ok(None)`).
///   2. If the Spec has NO `workspace_id` (not yet anchored), return `Ok(None)` — a Spec
///      with no workspace has no derived context, which is a SANE state (the anchor is
///      posed later), NOT an error.
///   3. RESOLVE the workspace; if it was removed after being linked (`ON DELETE SET NULL`
///      keeps the pointer NULL, but a stale read races), return `Ok(None)`.
///   4. DERIVE the project via `workspace.project_id` and the repos via
///      [`list_repositories`] on that project (live rows only).
///
/// Returns `Ok(Some(context))` when the Spec is anchored on a live workspace, else
/// `Ok(None)`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn spec_workspace_context(
    conn: &mut SqliteConnection,
    spec_id: &str,
) -> QueryResult<Option<SpecWorkspaceContext>> {
    // 1–2. The Spec and its anchor. No Spec, or no anchor → no derived context.
    let Some(spec) = get_spec(conn, spec_id)? else {
        return Ok(None);
    };
    let Some(workspace_id) = spec.workspace_id else {
        return Ok(None);
    };
    // 3. The workspace it is anchored on.
    let Some(workspace) = get_workspace(conn, &workspace_id)? else {
        return Ok(None);
    };
    // 4. DERIVE the project (via workspace.project_id) and its live repos — through the
    //    workspace, never duplicated onto the Spec.
    let Some(project) = get_project(conn, &workspace.project_id)? else {
        return Ok(None);
    };
    let repos = list_repositories(conn, &workspace.project_id)?;
    Ok(Some(SpecWorkspaceContext {
        spec_id: spec.id,
        workspace,
        project,
        repos,
    }))
}

// --- task domain models (v2.4 Phase 2, v14, PDR-0014) --------------------

/// A `tasks` row: the delegable unit of work of a Spec — what the coder picks up
/// (PDR-0014). `Serialize` for the IPC/MCP boundary. `kind` (`slice`|`human`|`review`) is
/// the PRD vocabulary; `executor` is kept coherent (a `human` kind is `executor=human`,
/// else `agent`). A `severity` makes the task a review FINDING (finding=task, PDR-0016).
/// `spec_id` is nullable (a task may exist without a Spec); `parent_task_id` is the subtask
/// tree. `phase` indexes `specs.phases` (PDR-0022; `None` = flat). The GUARDED transitions
/// (start/done/block/skip) are carried in [`transition_task`]; this struct just reads the
/// stored columns. `verification_command` records how a `kind=human` action is confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = tasks)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Task {
    pub id: String,
    pub project_id: String,
    /// The Spec this task belongs to. `None` for light work with no Spec.
    pub spec_id: Option<String>,
    /// Optional parent task (subtask decomposition). `None` for a top-level task.
    pub parent_task_id: Option<String>,
    /// PRD kind (`slice`|`human`|`review`); `slice` by default.
    pub kind: String,
    /// Canonical `task_executor` (`agent`|`human`); coherent with `kind`.
    pub executor: String,
    /// Canonical `task_type` (`implementation`|…); the Rust field is `type_` (`type` is a
    /// keyword), mapped to the SQL `type` column via `schema.rs` `#[sql_name]`.
    #[diesel(column_name = type_)]
    pub type_: String,
    /// Canonical `task_status` (`todo`|`ready`|…); `todo` by default. The transition path
    /// owns the GUARDED moves between values; this column just stores it.
    pub status: String,
    /// Canonical `priority` (`urgent`|…); `med` by default.
    pub priority: String,
    /// FINDING severity (canonical `severity`): set = review finding. `None` otherwise.
    pub severity: Option<String>,
    pub title: String,
    /// Optional long-form body.
    pub description: Option<String>,
    /// Optional acceptance criteria (the done conditions).
    pub acceptance_criteria: Option<String>,
    /// HUMAN-ACTION VERIFIABILITY: how a `kind=human` action is confirmed. `None` on a
    /// non-human task; required before a human task can be marked done (see
    /// [`verify_human_task`]).
    pub verification_command: Option<String>,
    /// Phase grouping (indexes `specs.phases`, PDR-0022). `None` = flat (no phase).
    pub phase: Option<i32>,
    /// List/board position within the Spec.
    pub position: i32,
    /// Reason recorded when the task is blocked (`block` stamps it). `None` otherwise.
    pub blocked_reason: Option<String>,
    /// Reason recorded when the task is skipped (`skip` stamps it). `None` otherwise.
    pub skip_reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Epoch ms the task reached `done`. `None` while not done.
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = tasks)]
struct NewTask {
    id: String,
    project_id: String,
    spec_id: Option<String>,
    parent_task_id: Option<String>,
    kind: String,
    executor: String,
    #[diesel(column_name = type_)]
    type_: String,
    priority: String,
    severity: Option<String>,
    title: String,
    description: Option<String>,
    acceptance_criteria: Option<String>,
    verification_command: Option<String>,
    phase: Option<i32>,
    position: i32,
    created_at: i64,
    updated_at: i64,
}

/// A `task_dependencies` row: one directed edge `task_id --depends_on--> depends_on_task_id`
/// of the ACYCLIC dependency graph. `Serialize` for the IPC/MCP boundary. `type` (`blocks`
/// gates ordering, `relates_to` is a soft link) is the Rust field `type_` (keyword),
/// mapped to the SQL `type` column.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = task_dependencies)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TaskDependency {
    pub task_id: String,
    pub depends_on_task_id: String,
    #[diesel(column_name = type_)]
    pub type_: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = task_dependencies)]
struct NewTaskDependency {
    task_id: String,
    depends_on_task_id: String,
    #[diesel(column_name = type_)]
    type_: String,
    created_at: i64,
}

/// The optional attributes of a task, passed as a group to [`create_task`] so a caller
/// sets what it needs without a long positional argument list (mirrors the `SpecSections`
/// / `CommandSource` grouping pattern). `kind` selects slice/human/review; the rest default
/// to the canonical defaults. `executor` is DERIVED from `kind` (a `human` kind is
/// `executor=human`, else `agent`) so the two never contradict.
#[derive(Debug, Clone)]
pub struct NewTaskInput {
    /// PRD kind (`slice`|`human`|`review`); drives `executor`.
    pub kind: String,
    /// Canonical `task_type`; `implementation` when unset.
    pub type_: String,
    /// Canonical `priority`; `med` when unset.
    pub priority: String,
    /// FINDING severity (for `kind=review`); `None` on a non-finding.
    pub severity: Option<String>,
    /// Optional long-form body.
    pub description: Option<String>,
    /// Optional acceptance criteria.
    pub acceptance_criteria: Option<String>,
    /// How a `kind=human` action is verified (required at done time for a human task).
    pub verification_command: Option<String>,
    /// Optional Spec link (`None` = light work with no Spec).
    pub spec_id: Option<String>,
    /// Optional parent task (subtask).
    pub parent_task_id: Option<String>,
    /// Optional phase grouping BY NAME (PDR-0022). `create_task` resolves it to a 1-based
    /// number through [`resolve_or_create_phase`], MATERIALIZING the Spec's plan from the
    /// names in first-seen order (get-or-create). `None` (or a blank name / no `spec_id`) =
    /// flat.
    pub phase_name: Option<String>,
}

impl Default for NewTaskInput {
    fn default() -> Self {
        Self {
            kind: TASK_KIND_SLICE.to_string(),
            type_: TASK_TYPE_IMPLEMENTATION.to_string(),
            priority: TASK_PRIORITY_MED.to_string(),
            severity: None,
            description: None,
            acceptance_criteria: None,
            verification_command: None,
            spec_id: None,
            parent_task_id: None,
            phase_name: None,
        }
    }
}

impl NewTaskInput {
    /// A `slice` task (agent implementation work) with the canonical defaults.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn slice() -> Self {
        Self::default()
    }

    /// A `human` task (an action only the user can do), carrying its `verification_command`
    /// (how the action is confirmed) — a human task is VERIFIABLE by construction.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn human(verification_command: impl Into<String>) -> Self {
        Self {
            kind: TASK_KIND_HUMAN.to_string(),
            verification_command: Some(verification_command.into()),
            ..Self::default()
        }
    }

    /// A `review` finding (finding=task) carrying its `severity`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn review(severity: impl Into<String>) -> Self {
        Self {
            kind: TASK_KIND_REVIEW.to_string(),
            severity: Some(severity.into()),
            ..Self::default()
        }
    }
}

/// The `executor` COHERENT with a `kind` (the DB CHECK enforces this pairing too): a
/// `human` kind runs by the human, every other kind by the agent. Centralized here so a
/// caller never has to remember the pairing.
fn executor_for_kind(kind: &str) -> &'static str {
    if kind == TASK_KIND_HUMAN {
        TASK_EXECUTOR_HUMAN
    } else {
        TASK_EXECUTOR_AGENT
    }
}

/// Create a task, deriving `executor` from `kind` and stamping the timestamps. The task
/// starts in `todo`. Returns the created [`Task`]. Links to a Spec (+ user stories live in
/// the Spec revision) via `spec_id`; a subtask sets `parent_task_id`. A `kind=human` task
/// carries its `verification_command` so it is verifiable; a `kind=review` task carries a
/// `severity` (finding=task).
#[cfg_attr(not(test), allow(dead_code))]
pub fn create_task(
    conn: &mut SqliteConnection,
    project_id: &str,
    title: &str,
    input: NewTaskInput,
) -> QueryResult<Task> {
    let now = now_millis();
    let executor = executor_for_kind(&input.kind).to_string();
    let task_id = Uuid::now_v7().to_string();
    let spec_id = input.spec_id.clone();
    // Outbox: the task insert AND its `task.added` event commit atomically.
    conn.transaction(|conn| {
        // Resolve the phase NAME to its 1-based number (GET-OR-CREATE in the Spec's plan),
        // INSIDE this transaction so the plan grow + the task insert commit atomically — a
        // task can never carry a phase absent from the plan. A phase name needs a Spec to
        // live in; without a `spec_id` (or with a blank name) the task is flat.
        let phase = match (spec_id.as_deref(), input.phase_name.as_deref()) {
            (Some(sid), Some(name)) if !name.trim().is_empty() => {
                Some(resolve_or_create_phase(conn, sid, name)?)
            }
            _ => None,
        };
        let task = diesel::insert_into(tasks::table)
            .values(NewTask {
                id: task_id.clone(),
                project_id: project_id.to_string(),
                spec_id: input.spec_id,
                parent_task_id: input.parent_task_id,
                kind: input.kind,
                executor,
                type_: input.type_,
                priority: input.priority,
                severity: input.severity,
                title: title.to_string(),
                description: input.description,
                acceptance_criteria: input.acceptance_criteria,
                verification_command: input.verification_command,
                phase,
                position: 0,
                created_at: now,
                updated_at: now,
            })
            .returning(Task::as_returning())
            .get_result(conn)?;
        // Correlate to the task's Spec anchor (workspace) when it belongs to a Spec.
        let workspace_id = match &spec_id {
            Some(sid) => spec_correlation(conn, sid)?.and_then(|(_, ws)| ws),
            None => None,
        };
        record_task_event(
            conn,
            "task.added",
            project_id,
            workspace_id.as_deref(),
            spec_id.as_deref(),
            &task_id,
        )?;
        Ok(task)
    })
}

/// Fetch ONE task by id, or `None` when the id is unknown. The read companion to the task
/// mutations.
#[cfg_attr(not(test), allow(dead_code))]
pub fn get_task(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<Task>> {
    tasks::table
        .find(id)
        .select(Task::as_select())
        .first(conn)
        .optional()
}

/// List the tasks of a Spec, ordered by `position` (then `created_at`) for a deterministic
/// board order. The read companion the tasks tab (later phase) keys on.
#[cfg_attr(not(test), allow(dead_code))]
pub fn list_tasks_for_spec(conn: &mut SqliteConnection, spec_id: &str) -> QueryResult<Vec<Task>> {
    tasks::table
        .filter(tasks::spec_id.eq(spec_id))
        .order((tasks::position.asc(), tasks::created_at.asc()))
        .select(Task::as_select())
        .load(conn)
}

// --- task DEPENDENCIES: the ACYCLIC graph (cycle detection) ---------------

/// Why adding a task dependency edge was refused (or the DB failed). Returned by
/// [`add_task_dependency`] so a UI/MCP caller can react precisely.
#[derive(Debug)]
pub enum DependencyError {
    /// The `task_id` or `depends_on_task_id` does not resolve to a task.
    TaskNotFound { id: String },
    /// A task cannot depend on itself (the smallest cycle).
    SelfDependency,
    /// The edge would create a CYCLE in the dependency graph (`depends_on_task_id` already
    /// reaches `task_id` transitively), so it is refused.
    WouldCreateCycle {
        task_id: String,
        depends_on_task_id: String,
    },
    /// A DB error surfaced while resolving or inserting.
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for DependencyError {
    fn from(e: diesel::result::Error) -> Self {
        DependencyError::Db(e)
    }
}

/// The tasks `to` DEPENDS ON directly (its out-edges in the dependency graph): the
/// prerequisites of `to`. Reads the forward direction of `task_dependencies`.
fn direct_dependencies(conn: &mut SqliteConnection, to: &str) -> QueryResult<Vec<String>> {
    task_dependencies::table
        .filter(task_dependencies::task_id.eq(to))
        .select(task_dependencies::depends_on_task_id)
        .load::<String>(conn)
}

/// Is `target` REACHABLE from `start` by following the "depends on" edges (start's
/// transitive prerequisites)? A DFS over `task_dependencies`. Used to detect a cycle BEFORE
/// inserting an edge: adding `task_id --depends_on--> dep` would close a cycle exactly when
/// `task_id` is already reachable from `dep` (i.e. `dep` already, transitively, depends on
/// `task_id`). The graph is a DAG by this very invariant, so the walk terminates; a `seen`
/// set guards against revisiting even mid-construction.
fn depends_on_reaches(conn: &mut SqliteConnection, start: &str, target: &str) -> QueryResult<bool> {
    let mut stack = vec![start.to_string()];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some(node) = stack.pop() {
        if node == target {
            return Ok(true);
        }
        if !seen.insert(node.clone()) {
            continue;
        }
        for next in direct_dependencies(conn, &node)? {
            if !seen.contains(&next) {
                stack.push(next);
            }
        }
    }
    Ok(false)
}

/// Add a dependency edge `task_id --depends_on--> depends_on_task_id` (task_id needs
/// depends_on_task_id done first), REFUSING any edge that would create a CYCLE. This is THE
/// guarded entry point for the acyclic dependency graph: SQLite cannot check transitive
/// acyclicity, so the invariant lives here, the single writer. Steps (in ONE transaction):
///
///   1. Reject a SELF edge ([`DependencyError::SelfDependency`]) — the DB CHECK also
///      catches it, but returning the typed error is clearer.
///   2. RESOLVE both endpoints ([`DependencyError::TaskNotFound`]).
///   3. CYCLE CHECK: if `depends_on_task_id` already reaches `task_id` through existing
///      edges, adding this one would close a cycle → [`DependencyError::WouldCreateCycle`].
///   4. INSERT the edge (`ON CONFLICT` on the composite PK makes a duplicate a no-op — an
///      already-present edge returns `Ok` idempotently).
///
/// Returns `Ok(())` on success (including the idempotent duplicate). `dep_type` is
/// [`DEPENDENCY_TYPE_BLOCKS`] (ordering) or [`DEPENDENCY_TYPE_RELATES_TO`] (soft link).
#[cfg_attr(not(test), allow(dead_code))]
pub fn add_task_dependency(
    conn: &mut SqliteConnection,
    task_id: &str,
    depends_on_task_id: &str,
    dep_type: &str,
) -> Result<(), DependencyError> {
    conn.transaction(|conn| {
        if task_id == depends_on_task_id {
            return Err(DependencyError::SelfDependency);
        }
        for id in [task_id, depends_on_task_id] {
            if get_task(conn, id)?.is_none() {
                return Err(DependencyError::TaskNotFound { id: id.to_string() });
            }
        }
        // A cycle would form iff the prerequisite already (transitively) depends on the
        // dependent — i.e. `task_id` is reachable from `depends_on_task_id`.
        if depends_on_reaches(conn, depends_on_task_id, task_id)? {
            return Err(DependencyError::WouldCreateCycle {
                task_id: task_id.to_string(),
                depends_on_task_id: depends_on_task_id.to_string(),
            });
        }
        diesel::insert_into(task_dependencies::table)
            .values(NewTaskDependency {
                task_id: task_id.to_string(),
                depends_on_task_id: depends_on_task_id.to_string(),
                type_: dep_type.to_string(),
                created_at: now_millis(),
            })
            // A duplicate edge (same composite PK) is a no-op, not an error.
            .on_conflict((
                task_dependencies::task_id,
                task_dependencies::depends_on_task_id,
            ))
            .do_nothing()
            .execute(conn)?;

        // Outbox: a `task.dependency_added` event on the DEPENDENT task (both endpoints
        // resolved above), correlated to its Spec so the Spec feed surfaces the edge.
        if let Some((project_id, workspace_id, spec_id)) = task_correlation(conn, task_id)? {
            let payload = serde_json::json!({
                "task_id": task_id,
                "spec_id": spec_id,
                "depends_on_task_id": depends_on_task_id,
                "type": dep_type,
            })
            .to_string();
            record_domain_event(
                conn,
                NewDomainEvent {
                    project_id,
                    workspace_id,
                    aggregate: AGGREGATE_TASK.to_string(),
                    event_type: "task.dependency_added".to_string(),
                    source: "mcp".to_string(),
                    actor: None,
                    payload,
                    created_at: 0,
                },
            )?;
        }
        Ok(())
    })
}

/// Remove a dependency edge. Returns rows deleted (0 = the edge was not present). The
/// inverse of [`add_task_dependency`] (no cycle risk on removal).
#[cfg_attr(not(test), allow(dead_code))]
pub fn remove_task_dependency(
    conn: &mut SqliteConnection,
    task_id: &str,
    depends_on_task_id: &str,
) -> QueryResult<usize> {
    diesel::delete(
        task_dependencies::table
            .filter(task_dependencies::task_id.eq(task_id))
            .filter(task_dependencies::depends_on_task_id.eq(depends_on_task_id)),
    )
    .execute(conn)
}

/// The direct dependencies (prerequisites) of a task, as full [`Task`] rows. The read
/// companion the "can I start this?" gate keys on (a task is ready when all its
/// prerequisites are `done`).
#[cfg_attr(not(test), allow(dead_code))]
pub fn list_task_dependencies(
    conn: &mut SqliteConnection,
    task_id: &str,
) -> QueryResult<Vec<Task>> {
    let prereq_ids = direct_dependencies(conn, task_id)?;
    tasks::table
        .filter(tasks::id.eq_any(prereq_ids))
        .select(Task::as_select())
        .load(conn)
}

/// The direct dependency EDGES of a task (its forward out-edges in the graph), as
/// [`TaskDependency`] rows carrying the edge `type` (`blocks`/`relates_to`). Unlike
/// [`list_task_dependencies`] (which resolves the prerequisite TASKS), this returns the
/// raw edges so the tasks tab (Phase 4) can render "blocked by" WITHOUT losing the edge
/// type — a Spec's whole dependency graph is the union of these over its tasks.
#[cfg_attr(not(test), allow(dead_code))]
pub fn list_task_dependency_edges(
    conn: &mut SqliteConnection,
    task_id: &str,
) -> QueryResult<Vec<TaskDependency>> {
    task_dependencies::table
        .filter(task_dependencies::task_id.eq(task_id))
        .order(task_dependencies::created_at.asc())
        .select(TaskDependency::as_select())
        .load(conn)
}

// --- task TRANSITIONS: guarded start / done / block / skip (v2.4 Phase 2) --
//
// The task lifecycle GUARDED (valid source state) and carried in the nyx-core backend, so
// the rules live in one place (the UI and the MCP tools of later phases route through it).
// The transitions the PRD names:
//
//   (todo|ready|blocked) ──start──▶ in_progress ──done──▶ done
//         │                              │
//         │                              ├──block──▶ blocked ──start──▶ in_progress
//         └──────────────skip────────────┴──────────────────────────▶ cancelled
//
// GUARD — the source state must be a legal predecessor (a `done`/`cancelled` task is
// terminal and cannot transition). Encoded in [`TaskTransition::source_allowed`].
//
// HUMAN-ACTION VERIFIABILITY (the guard-rail): a `kind=human` task cannot be `done`d through
// the plain `done` transition — it must go through the VERIFIED path ([`verify_human_task`])
// which requires the action to be verifiable (a `verification_command` present). This keeps
// a human action from being silently closed without its verification recorded (mirrors
// depot's `task verify`), while an agent `slice`/`review` task uses `done` directly.

/// A task lifecycle transition (the conceptual move). Each maps to a canonical target
/// `task_status` and declares its legal source states (the GUARD).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTransition {
    /// (todo|ready|blocked) → in_progress.
    Start,
    /// in_progress → done (agent tasks; a human task uses [`verify_human_task`]).
    Done,
    /// (todo|ready|in_progress) → blocked. Carries a reason.
    Block,
    /// (todo|ready|in_progress|blocked) → cancelled ("skip"). Carries a reason.
    Skip,
}

impl TaskTransition {
    /// The canonical `task_status` this transition moves the task TO.
    pub fn target_status(self) -> &'static str {
        match self {
            TaskTransition::Start => TASK_STATUS_IN_PROGRESS,
            TaskTransition::Done => TASK_STATUS_DONE,
            TaskTransition::Block => TASK_STATUS_BLOCKED,
            TaskTransition::Skip => TASK_STATUS_CANCELLED,
        }
    }

    /// The VERB of the transition (the conceptual move requested), independent of the target
    /// status. Carried in the `task.transitioned` event payload so the activity feed can
    /// distinguish moves that share a target status — notably `skip` (→ `cancelled`) from any
    /// other path, and (alongside the `verify` verb of [`verify_human_task`]) a `done` from a
    /// human `verify`, both of which land on `done`.
    pub fn verb(self) -> &'static str {
        match self {
            TaskTransition::Start => "start",
            TaskTransition::Done => "done",
            TaskTransition::Block => "block",
            TaskTransition::Skip => "skip",
        }
    }

    /// The GUARD: is `from` (a canonical `task_status`) a legal SOURCE for this transition?
    /// A transition from an illegal source is refused with
    /// [`TaskTransitionError::InvalidSource`]. `done`/`cancelled` are terminal (no source).
    pub fn source_allowed(self, from: &str) -> bool {
        match self {
            // Start is reachable from the non-started, non-terminal states (including a
            // restart out of `blocked`).
            TaskTransition::Start => matches!(
                from,
                TASK_STATUS_TODO | TASK_STATUS_READY | TASK_STATUS_BLOCKED
            ),
            // Done closes an in-progress task.
            TaskTransition::Done => from == TASK_STATUS_IN_PROGRESS,
            // Block halts a not-yet-terminal, not-already-blocked task.
            TaskTransition::Block => matches!(
                from,
                TASK_STATUS_TODO | TASK_STATUS_READY | TASK_STATUS_IN_PROGRESS
            ),
            // Skip abandons any non-terminal task.
            TaskTransition::Skip => matches!(
                from,
                TASK_STATUS_TODO
                    | TASK_STATUS_READY
                    | TASK_STATUS_IN_PROGRESS
                    | TASK_STATUS_BLOCKED
            ),
        }
    }
}

/// Why a task lifecycle transition was refused (or the DB failed). The nyx-core layer
/// returns this so a UI/MCP caller can react precisely.
#[derive(Debug)]
pub enum TaskTransitionError {
    /// No task resolves to the given id.
    NotFound,
    /// The GUARD failed: the task's current `status` (`from`) is not a legal source for the
    /// requested transition (which targets `to`).
    InvalidSource { from: String, to: &'static str },
    /// A `kind=human` task was `done`d through the plain `done` path instead of the
    /// VERIFIED path ([`verify_human_task`]) — a human action must be verified to close.
    HumanRequiresVerification,
    /// The VERIFY path ([`verify_human_task`]) was attempted on a task whose current `status`
    /// (`from`) is not `in_progress` — a human action must be STARTED before it can be
    /// verified. Distinct from [`Self::InvalidSource`] so the message speaks of the `verify`
    /// verb (not the internal `done` target it lands on).
    CannotVerify { from: String },
    /// A DB error surfaced while resolving or updating.
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for TaskTransitionError {
    fn from(e: diesel::result::Error) -> Self {
        TaskTransitionError::Db(e)
    }
}

/// Perform a task lifecycle `transition`, GUARDED, and return the updated [`Task`]. THE
/// single nyx-core owner of a task status change (start/done/block/skip), so the lifecycle
/// rules live in one place. `reason` carries the block/skip reason (`None` for start/done;
/// stamped into `blocked_reason`/`skip_reason`). Steps (in ONE transaction, so a refusal
/// writes nothing):
///
///   1. RESOLVE the task (unknown id → [`TaskTransitionError::NotFound`]).
///   2. HUMAN GUARD-RAIL: a `kind=human` task cannot be `done`d here — it must use
///      [`verify_human_task`] → [`TaskTransitionError::HumanRequiresVerification`].
///   3. GUARD: the current `status` must be a legal SOURCE
///      ([`TaskTransition::source_allowed`]) → [`TaskTransitionError::InvalidSource`].
///   4. WRITE the new `status` (stamp `completed_at` on done; `blocked_reason`/`skip_reason`
///      on block/skip; CLEAR the stale reasons on start), bumping `updated_at`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn transition_task(
    conn: &mut SqliteConnection,
    task_id: &str,
    transition: TaskTransition,
    reason: Option<&str>,
) -> Result<Task, TaskTransitionError> {
    conn.transaction(|conn| {
        let task = match get_task(conn, task_id)? {
            None => return Err(TaskTransitionError::NotFound),
            Some(t) => t,
        };

        // HUMAN GUARD-RAIL: a human action closes only through the verified path.
        if transition == TaskTransition::Done && task.kind == TASK_KIND_HUMAN {
            return Err(TaskTransitionError::HumanRequiresVerification);
        }

        // GUARD: the current status must be a legal source.
        if !transition.source_allowed(&task.status) {
            return Err(TaskTransitionError::InvalidSource {
                from: task.status.clone(),
                to: transition.target_status(),
            });
        }

        let now = now_millis();
        let target = transition.target_status();
        match transition {
            TaskTransition::Done => {
                diesel::update(tasks::table.find(task_id))
                    .set((
                        tasks::status.eq(target),
                        tasks::completed_at.eq(now),
                        tasks::updated_at.eq(now),
                    ))
                    .execute(conn)?;
            }
            TaskTransition::Block => {
                diesel::update(tasks::table.find(task_id))
                    .set((
                        tasks::status.eq(target),
                        tasks::blocked_reason.eq(reason),
                        tasks::updated_at.eq(now),
                    ))
                    .execute(conn)?;
            }
            TaskTransition::Skip => {
                diesel::update(tasks::table.find(task_id))
                    .set((
                        tasks::status.eq(target),
                        tasks::skip_reason.eq(reason),
                        tasks::updated_at.eq(now),
                    ))
                    .execute(conn)?;
            }
            TaskTransition::Start => {
                // Starting clears a stale blocked_reason (the blocker is resolved).
                diesel::update(tasks::table.find(task_id))
                    .set((
                        tasks::status.eq(target),
                        tasks::blocked_reason.eq::<Option<String>>(None),
                        tasks::updated_at.eq(now),
                    ))
                    .execute(conn)?;
            }
        }

        // Outbox: the status flip AND its `task.transitioned` event commit atomically. The
        // payload records the `from`/`to` status and (when set) the Spec the task belongs to.
        let workspace_id = match &task.spec_id {
            Some(sid) => spec_correlation(conn, sid)?.and_then(|(_, ws)| ws),
            None => None,
        };
        // The payload also carries the requested `verb` (so `skip` → `cancelled` is
        // distinguishable) and the `reason` when the move carries one (block/skip), so the
        // feed can say "Task blocked: <reason>" rather than a bare status flip.
        let payload = serde_json::json!({
            "task_id": task_id,
            "spec_id": task.spec_id,
            "from": task.status,
            "to": target,
            "verb": transition.verb(),
            "reason": reason,
        })
        .to_string();
        record_domain_event(
            conn,
            NewDomainEvent {
                project_id: task.project_id.clone(),
                workspace_id,
                aggregate: AGGREGATE_TASK.to_string(),
                event_type: "task.transitioned".to_string(),
                source: "mcp".to_string(),
                actor: None,
                payload,
                created_at: 0,
            },
        )?;

        let updated = get_task(conn, task_id)?.ok_or(TaskTransitionError::NotFound)?;
        Ok(updated)
    })
}

/// Mark a `kind=human` task DONE through the VERIFIED path (the human-action verifiability
/// guard-rail). A human action is closed here — NOT through the plain [`transition_task`]
/// `Done` — and ONLY when it is VERIFIABLE: the task must carry a `verification_command`
/// (how the action was confirmed). This is the guarded path that prevents a human action
/// from being silently marked done without its verification recorded (mirrors depot's
/// `task verify`). Steps (ONE transaction):
///
///   1. RESOLVE the task ([`TaskTransitionError::NotFound`]).
///   2. It must be `kind=human` (an agent task uses [`transition_task`]) and VERIFIABLE
///      (a `verification_command` present) → [`TaskTransitionError::HumanRequiresVerification`]
///      otherwise.
///   3. GUARD the source (`in_progress` → done, like any done) →
///      [`TaskTransitionError::CannotVerify`] (phrased in terms of the `verify` verb: a task
///      that is not started cannot be verified — start it first).
///   4. WRITE `done` + stamp `completed_at`.
///
/// Returns the updated [`Task`].
#[cfg_attr(not(test), allow(dead_code))]
pub fn verify_human_task(
    conn: &mut SqliteConnection,
    task_id: &str,
) -> Result<Task, TaskTransitionError> {
    conn.transaction(|conn| {
        let task = match get_task(conn, task_id)? {
            None => return Err(TaskTransitionError::NotFound),
            Some(t) => t,
        };

        // Only a VERIFIABLE human task can be closed here: it must be kind=human and carry
        // a verification_command (the "how it was confirmed" the guard-rail requires).
        let verifiable = task.kind == TASK_KIND_HUMAN
            && task
                .verification_command
                .as_deref()
                .is_some_and(|c| !c.trim().is_empty());
        if !verifiable {
            return Err(TaskTransitionError::HumanRequiresVerification);
        }

        // GUARD the source exactly as a normal done (in_progress → done), but surface the
        // refusal in terms of the `verify` VERB the caller used (not the internal `done`
        // target) so the message reads "cannot verify a task in 'todo': start it first".
        if !TaskTransition::Done.source_allowed(&task.status) {
            return Err(TaskTransitionError::CannotVerify {
                from: task.status.clone(),
            });
        }

        let now = now_millis();
        diesel::update(tasks::table.find(task_id))
            .set((
                tasks::status.eq(TASK_STATUS_DONE),
                tasks::completed_at.eq(now),
                tasks::updated_at.eq(now),
            ))
            .execute(conn)?;

        // Outbox: a human task closed through the verified path emits the same
        // `task.transitioned` event as the plain path (its activity is uniform in the feed).
        let workspace_id = match &task.spec_id {
            Some(sid) => spec_correlation(conn, sid)?.and_then(|(_, ws)| ws),
            None => None,
        };
        // The `verify` verb distinguishes a human-verified close from a plain agent `done`
        // (both land on `done`) in the activity feed.
        let payload = serde_json::json!({
            "task_id": task_id,
            "spec_id": task.spec_id,
            "from": task.status,
            "to": TASK_STATUS_DONE,
            "verb": "verify",
        })
        .to_string();
        record_domain_event(
            conn,
            NewDomainEvent {
                project_id: task.project_id.clone(),
                workspace_id,
                aggregate: AGGREGATE_TASK.to_string(),
                event_type: "task.transitioned".to_string(),
                source: "mcp".to_string(),
                actor: None,
                payload,
                created_at: 0,
            },
        )?;

        let updated = get_task(conn, task_id)?.ok_or(TaskTransitionError::NotFound)?;
        Ok(updated)
    })
}

// --- task DELETE: the draft-authoring hard removal (v2.5.8, finding #31) ----
//
// A DELETE (vs. `skip`, which cancels but keeps the ghost) is only legitimate while the
// parent Spec is still being AUTHORED. In `draft` you are still shaping the plan, so a
// mis-sliced task must disappear cleanly; once the Spec is ready/in_progress the only
// legitimate move is `skip` (which preserves the audit trail — the "18 ghosts cancelled"
// the redesign called out are the price of NOT having a delete during authoring). Hence the
// STATE GUARD below: draft-only.

/// Why deleting a task was refused (or the DB failed). Returned by [`delete_task`] so a
/// UI/MCP caller can react precisely. The GUARD is "the parent Spec must be in `draft`".
#[derive(Debug)]
pub enum DeleteTaskError {
    /// No task resolves to the given id (mapped to `invalid_id`).
    NotFound,
    /// The STATE GUARD: the task's parent Spec is not in `draft` — carries the current spec
    /// status, or `"(none)"` when the task has NO parent Spec (the draft gate cannot be
    /// satisfied). A real delete is legal ONLY while the parent Spec is a draft; otherwise
    /// `skip` the task (which keeps the trace). Mapped to `invalid_state`.
    NotDraft { status: String },
    /// A DB error surfaced while resolving or deleting.
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for DeleteTaskError {
    fn from(e: diesel::result::Error) -> Self {
        DeleteTaskError::Db(e)
    }
}

/// DELETE a task outright — the draft-authoring affordance that REMOVES an erroneous task
/// (vs. [`transition_task`] `skip`, which only cancels it and leaves the ghost). GUARDED to
/// a parent Spec still in `draft`. THE single nyx-core owner of a task deletion, so the guard
/// lives in one place (the UI + the MCP tool route here). Steps (ONE transaction, so a
/// refusal writes nothing):
///
///   1. RESOLVE the task (unknown id → [`DeleteTaskError::NotFound`] → `invalid_id`).
///   2. STATE GUARD: the task's parent Spec must be in `draft`. Any other state — or NO
///      parent Spec (the draft gate cannot be satisfied) — is refused with
///      [`DeleteTaskError::NotDraft`] (→ `invalid_state`); the caller should `skip` instead.
///   3. CLEAN the dependency graph EXPLICITLY: delete every `task_dependencies` row where the
///      task is the SOURCE (`task_id`) or the TARGET (`depends_on_task_id`), so no dangling
///      edge survives. (`foreign_keys = ON` would also cascade these on the row delete, but
///      the explicit cleanup is the contract and holds regardless of the pragma.)
///   4. DELETE the task row. Any SUBTASKS (`parent_task_id ON DELETE CASCADE`) are removed by
///      the FK cascade, so no child is left pointing at a deleted parent.
///
/// PHASE PLAN: `specs.phases` is auto-built by NAME ([`resolve_or_create_phase`]) and is left
/// UNTOUCHED here — even when the deleted task was the last one of its phase. Keeping the
/// (now empty) phase entry preserves the clean `1..N` invariant that [`validate_phase_plan`]
/// / the `ready` gate require: PRUNING a middle phase would open a gap (breaking that gate)
/// and RENUMBERING would orphan the OTHER tasks' `phase` numbers. So the coherent choice is
/// to leave the entry (the UI simply renders it empty). No orphan, no crash.
#[cfg_attr(not(test), allow(dead_code))]
pub fn delete_task(conn: &mut SqliteConnection, task_id: &str) -> Result<(), DeleteTaskError> {
    conn.transaction(|conn| {
        let task = match get_task(conn, task_id)? {
            None => return Err(DeleteTaskError::NotFound),
            Some(t) => t,
        };

        // STATE GUARD: the parent Spec must be in `draft`. Resolve its status (None = the task
        // has no Spec, or the Spec vanished) — anything but `draft` is refused, draft-only.
        let spec_status: Option<String> = match &task.spec_id {
            Some(sid) => specs::table
                .find(sid)
                .select(specs::status)
                .first::<String>(conn)
                .optional()?,
            None => None,
        };
        if spec_status.as_deref() != Some(SPEC_STATUS_DRAFT) {
            return Err(DeleteTaskError::NotDraft {
                status: spec_status.unwrap_or_else(|| "(none)".to_string()),
            });
        }

        // CLEAN the dependency graph: drop every edge touching this task in EITHER direction
        // (as source, then as target), so the delete never leaves a dangling edge.
        diesel::delete(task_dependencies::table.filter(task_dependencies::task_id.eq(task_id)))
            .execute(conn)?;
        diesel::delete(
            task_dependencies::table.filter(task_dependencies::depends_on_task_id.eq(task_id)),
        )
        .execute(conn)?;

        // DELETE the task row (subtasks cascade via `parent_task_id ON DELETE CASCADE`).
        diesel::delete(tasks::table.find(task_id)).execute(conn)?;

        // Outbox: the deletion AND its `task.deleted` event commit atomically. Correlate to
        // the task's project + its Spec's workspace anchor (captured before the row went).
        let workspace_id = match &task.spec_id {
            Some(sid) => spec_correlation(conn, sid)?.and_then(|(_, ws)| ws),
            None => None,
        };
        record_task_event(
            conn,
            "task.deleted",
            &task.project_id,
            workspace_id.as_deref(),
            task.spec_id.as_deref(),
            task_id,
        )?;

        Ok(())
    })
}

// --- Managed command / instance models (PRD-3 v3) ------------------------

/// A `managed_commands` row: a per-project command TEMPLATE. `Serialize` for the
/// IPC boundary. `subfolder` is an optional run path relative to the workspace
/// (`None` = root). `restart_on_startup` is the 0/1 boolean exposed as Rust
/// `bool`. `order_index` maps the keyword `"order"` SQL column. The `source_*`
/// columns + `package_manager` are optional package.json provenance metadata.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = managed_commands)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct ManagedCommand {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub command: String,
    /// Optional run subfolder, relative to the workspace path (`None` = root).
    pub subfolder: Option<String>,
    /// When set, the runner relaunches this template's instances at app start.
    pub restart_on_startup: bool,
    /// Sidebar order within the project; SQL column is `"order"` (a keyword).
    pub order_index: i32,
    pub created_at: i64,
    pub updated_at: i64,
    /// `None` for a hand-authored template, or [`SOURCE_KIND_PACKAGE_JSON`].
    pub source_kind: Option<String>,
    pub source_package_json_path: Option<String>,
    pub source_script_name: Option<String>,
    /// Snapshot of the script's command line at import time (drift detection).
    pub source_script_command_snapshot: Option<String>,
    /// Detected package manager (`npm`/`pnpm`/`yarn`/`bun`) when imported.
    pub package_manager: Option<String>,
}

/// Optional package.json provenance for a template — passed as a group to
/// [`create_template`] / [`set_template_source`]. All-`None` means hand-authored.
#[derive(Debug, Clone, Default)]
pub struct CommandSource {
    pub source_kind: Option<String>,
    pub source_package_json_path: Option<String>,
    pub source_script_name: Option<String>,
    pub source_script_command_snapshot: Option<String>,
    pub package_manager: Option<String>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = managed_commands)]
struct NewManagedCommand {
    id: String,
    project_id: String,
    name: String,
    command: String,
    subfolder: Option<String>,
    restart_on_startup: bool,
    order_index: i32,
    created_at: i64,
    updated_at: i64,
    source_kind: Option<String>,
    source_package_json_path: Option<String>,
    source_script_name: Option<String>,
    source_script_command_snapshot: Option<String>,
    package_manager: Option<String>,
}

/// A `command_instances` row: the materialization of one template for one
/// workspace. `Serialize` for the IPC boundary. `last_state` is the last persisted
/// run state (idle|running|success|error). `was_running_on_shutdown` is the 0/1
/// boolean snapshot the shutdown flow sets and boot resets.
///
/// `last_exit_code` / `ended_at` / `unread` (v4) split the FACTUAL run outcome from
/// the notification: `last_exit_code` + `ended_at` persist the last completed run's
/// natural exit code + finish time (NULL while never-finished / running) — the
/// outcome an acknowledge must NEVER erase — and `unread` is the separate
/// "unseen result" flag a UI acknowledge clears WITHOUT collapsing the outcome.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = command_instances)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct CommandInstance {
    pub id: String,
    pub command_id: String,
    pub workspace_id: String,
    /// Last persisted run state: idle|running|success|error. The FACTUAL outcome —
    /// an acknowledge never collapses it.
    pub last_state: String,
    pub scrollback: String,
    /// Shutdown snapshot: was this instance running when the app last quit?
    pub was_running_on_shutdown: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// Natural exit code of the LAST completed run (v4). `None` while the instance
    /// has never finished a run (idle never-run / running). Persisted so an observer
    /// can tell a crash from a clean run even after a cold restart.
    pub last_exit_code: Option<i32>,
    /// Epoch-millis the last run finished (v4). `None` while never-finished/running.
    pub ended_at: Option<i64>,
    /// "Unseen result" flag (v4): `true` once a run finishes, cleared by an
    /// acknowledge WITHOUT touching `last_state`/`last_exit_code`/`ended_at`.
    pub unread: bool,
    /// Bounded retained scrollback of the LAST completed run (v5), kept across a
    /// (re)launch so an observer can still read the PREVIOUS run after the current
    /// run reset the live columns. `""` while no prior run is retained (idle never-run
    /// / first run). Bounded to N=1 prior run.
    pub prev_scrollback: String,
    /// The retained prior run's natural exit code (v5). `None` while none is retained
    /// (or the prior run had no code).
    pub prev_exit_code: Option<i32>,
    /// Epoch-millis the retained prior run finished (v5). `None` while none retained.
    pub prev_ended_at: Option<i64>,
    /// The retained prior run's factual outcome string (v5): `success`|`error`, or
    /// `None` while no prior run is retained.
    pub prev_last_state: Option<String>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = command_instances)]
struct NewCommandInstance {
    id: String,
    command_id: String,
    workspace_id: String,
    created_at: i64,
    updated_at: i64,
}

/// An r2d2 [`CustomizeConnection`] that, on EVERY connection the pool opens (not just
/// the first), enables WAL journaling + foreign-key enforcement and sets a busy
/// timeout. Without this, a connection the pool creates lazily under load would come
/// up with SQLite's defaults (rollback journal, `foreign_keys = OFF`, no busy
/// timeout), so the pragmas must be (re)applied per checkout-source, not once at open.
///
/// `journal_mode = WAL` is a database-level persistent setting (it survives once set),
/// but re-issuing it per connection is cheap and makes the guarantee explicit and
/// connection-local-test-verifiable; `foreign_keys` and `busy_timeout` ARE
/// per-connection settings and MUST be set on each one.
#[derive(Debug)]
struct PragmaCustomizer;

impl CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for PragmaCustomizer {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), diesel::r2d2::Error> {
        conn.batch_execute(&format!(
            "PRAGMA journal_mode = WAL; \
             PRAGMA foreign_keys = ON; \
             PRAGMA busy_timeout = {};",
            BUSY_TIMEOUT.as_millis()
        ))
        .map_err(diesel::r2d2::Error::QueryError)
    }
}

/// The shell-managed handle to nyx's SQLite database: an **r2d2 connection pool**
/// shared by the napi worker pool (libuv) and the MCP server threads.
///
/// Each `with_conn` checks a connection out of the pool, runs the closure, and
/// returns it — so independent readers progress concurrently instead of serializing
/// behind one `Mutex`, while WAL + a busy timeout let SQLite's single writer
/// interleave safely. Every connection is initialized by [`PragmaCustomizer`]
/// (WAL + foreign_keys + busy_timeout). `Clone` is cheap (the pool is `Arc`-backed),
/// so the boot flow can hand the SAME pool to the MCP dispatcher and the napi DB
/// tasks without a second connection authority.
#[derive(Clone)]
pub struct Db {
    pool: Pool<ConnectionManager<SqliteConnection>>,
}

impl Db {
    /// Open (creating if absent) the SQLite database at `db_path`, apply pending
    /// migrations, and wrap it in an r2d2 pool for use as shell-managed state.
    ///
    /// `db_path`'s parent directory must already exist (callers derive it from
    /// `app_data_dir` and create it). Migrations run ONCE here on a dedicated
    /// connection (with the same pragmas applied) before the pool is returned, so the
    /// schema is ready before any pooled checkout serves a query. The
    /// [`PragmaCustomizer`] then enables WAL + foreign-key enforcement + the busy
    /// timeout on every connection the pool subsequently opens.
    pub fn open(db_path: &Path) -> anyhow::Result<Self> {
        let url = db_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 db path: {}", db_path.display()))?;
        let pool = build_pool(url)?;
        // Run migrations once on a checked-out connection (the customizer has already
        // set WAL + foreign_keys + busy_timeout on it). Propagate any failure so the
        // boot setup refuses to start rather than serving a broken schema (D1).
        let mut conn = pool
            .get()
            .map_err(|e| anyhow::anyhow!("failed to check out a connection for migrations: {e}"))?;
        run_migrations(&mut conn)?;
        drop(conn);
        Ok(Db { pool })
    }

    /// Run a closure with exclusive access to a pooled connection. The single
    /// entry-point every caller uses — UNCHANGED signature, so the pool is a drop-in
    /// for the previous `Mutex<SqliteConnection>`. Checking a connection out of the
    /// pool is the analogue of taking the old mutex; a checkout failure (pool
    /// exhausted past the timeout / build error) is as unrecoverable as a poisoned
    /// mutex was, so we `expect` it, matching the old `lock().expect(...)` contract
    /// (callers return `T`, not `Result<T>`).
    pub fn with_conn<T>(&self, f: impl FnOnce(&mut SqliteConnection) -> T) -> T {
        let mut conn = self.checkout();
        f(&mut conn)
    }

    /// Check a connection out of the pool. Separated from [`with_conn`] so the napi
    /// `AsyncTask` DB layer can also obtain one off the libuv worker thread.
    fn checkout(&self) -> PooledConnection<ConnectionManager<SqliteConnection>> {
        self.pool.get().expect("db pool checkout failed")
    }

    /// In-memory, migrated `Db` for tests — never touches a real `app_data_dir`
    /// file. Uses a SHARED-CACHE in-memory database with `max_size = 1` so the single
    /// pooled connection survives (a `:memory:` DB is dropped when its last connection
    /// closes; a one-connection pool keeps it alive and every checkout sees the same
    /// migrated schema). The customizer still applies foreign_keys + busy_timeout;
    /// WAL is a no-op on `:memory:` (it has no journal file), which is fine.
    ///
    /// The shared-cache DB is given a UNIQUE per-instance NAME
    /// (`file:nyx-mem-<uuid>?mode=memory&cache=shared`). Without a unique name, every
    /// `in_memory()` in the process would share ONE `:memory:` database (shared cache
    /// keys on the name), so tests running in PARALLEL would migrate the same DB and
    /// race the schema-migration write lock ("database schema is locked: main"). The
    /// per-instance name isolates each test's DB while keeping the shared-cache +
    /// one-connection-pool trick that keeps the memory DB alive.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        let url = format!("file:nyx-mem-{}?mode=memory&cache=shared", Uuid::now_v7());
        let manager = ConnectionManager::<SqliteConnection>::new(url);
        let pool = Pool::builder()
            .max_size(1)
            .connection_customizer(Box::new(PragmaCustomizer))
            .build(manager)
            .expect("failed to build in-memory test pool");
        let mut conn = pool
            .get()
            .expect("failed to check out in-memory connection");
        run_migrations(&mut conn).expect("failed to migrate in-memory test db");
        drop(conn);
        Db { pool }
    }
}

/// Build the r2d2 pool for `url` with the [`PragmaCustomizer`] (WAL + foreign_keys +
/// busy_timeout per connection) and a bounded size. Eagerly opens one connection
/// (`test_on_check_out` + a probe checkout in [`Db::open`]) so a bad path fails fast
/// here rather than on the first query.
fn build_pool(url: &str) -> anyhow::Result<Pool<ConnectionManager<SqliteConnection>>> {
    let manager = ConnectionManager::<SqliteConnection>::new(url);
    Pool::builder()
        .max_size(POOL_MAX_SIZE)
        .connection_timeout(BUSY_TIMEOUT)
        .connection_customizer(Box::new(PragmaCustomizer))
        .build(manager)
        .map_err(|e| anyhow::anyhow!("failed to build SQLite pool at {url}: {e}"))
}

/// Apply all pending embedded migrations. Idempotent. Returns `Err` if any
/// migration fails — the caller (boot setup) must propagate the error, which
/// causes nyx to refuse to start rather than serving a broken schema (D1).
pub fn run_migrations(conn: &mut SqliteConnection) -> anyhow::Result<()> {
    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow::anyhow!("failed to run migrations: {e}"))?;
    Ok(())
}

/// Schema health snapshot (D1 probe check).
#[derive(Debug, Clone)]
pub struct SchemaHealth {
    /// `true` when every embedded migration has been applied; `false` if any
    /// pending migration was not applied (schema is behind the binary).
    pub up_to_date: bool,
    /// The number of migrations that are still pending (`Some(0)` when `up_to_date`),
    /// or `None` when the check itself failed (the migration table could not be read).
    /// Modelled as an explicit `Option` rather than a magic sentinel so a failed check
    /// can never leak an absurd count (e.g. `usize::MAX`) onto the probe wire.
    pub pending_count: Option<usize>,
}

/// Check whether the DB schema matches the embedded migrations. Returns a
/// [`SchemaHealth`] snapshot. Best-effort: any error reading the migration
/// table degrades to `up_to_date = false` with a note that the check itself
/// failed, rather than panicking.
pub fn schema_health(conn: &mut SqliteConnection) -> SchemaHealth {
    match conn.pending_migrations(MIGRATIONS) {
        Ok(pending) => SchemaHealth {
            pending_count: Some(pending.len()),
            up_to_date: pending.is_empty(),
        },
        Err(_) => SchemaHealth {
            up_to_date: false,
            pending_count: None, // the check itself failed (can't read the migration table)
        },
    }
}

/// Upper bound on stored scrollback, in bytes. Scrollback is unbounded history;
/// we keep only the most-recent slice so the DB (and a single row) can't grow
/// without limit. The TAIL is kept — recent output is what the user wants to see
/// on restore — and we cut on a UTF-8 char boundary so the stored string stays
/// valid. ~256 KiB holds a generous on-screen-plus-scrollback history per term.
pub const MAX_SCROLLBACK_BYTES: usize = 256 * 1024;

/// Bound a scrollback string to [`MAX_SCROLLBACK_BYTES`], keeping the tail and
/// never splitting a multi-byte char. Pure (testable without a DB).
pub fn bound_scrollback(s: &str) -> &str {
    if s.len() <= MAX_SCROLLBACK_BYTES {
        return s;
    }
    // Target start index = len - MAX; walk forward to the next char boundary so
    // we don't slice through a multi-byte sequence.
    let mut start = s.len() - MAX_SCROLLBACK_BYTES;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

// --- CRUD over the `terminals` table ------------------------------------
//
// Pure DB functions taking `&mut SqliteConnection` so they are unit-tested
// against an in-memory database; the bridge wraps each in a `#[tauri::command]`.

/// Insert a new `alive` terminal at `cwd` (optional `label`), appended to the
/// end of the sidebar order (max existing order + 1, or 0 if first). Returns the
/// inserted row (id + defaults filled by SQLite).
pub fn create_terminal(
    conn: &mut SqliteConnection,
    cwd: &str,
    label: Option<String>,
) -> QueryResult<Terminal> {
    use diesel::dsl::max;
    // Append after the current max order so creation order is stable.
    let next_order = terminals::table
        .select(max(terminals::order_index))
        .first::<Option<i32>>(conn)?
        .map(|m| m + 1)
        .unwrap_or(0);

    diesel::insert_into(terminals::table)
        .values(NewTerminal::alive(cwd, label, next_order))
        .returning(Terminal::as_returning())
        .get_result(conn)
}

/// List all terminals in sidebar order (`order` asc, then `id` asc as a stable
/// tiebreaker). Includes closed terminals — the caller decides what to show /
/// re-spawn.
pub fn list_terminals(conn: &mut SqliteConnection) -> QueryResult<Vec<Terminal>> {
    terminals::table
        .order((terminals::order_index.asc(), terminals::id.asc()))
        .select(Terminal::as_select())
        .load(conn)
}

/// Mark a terminal `closed` (it is no longer re-spawned at launch), stamping
/// `closed_at` (and bumping `updated_at`) with the current epoch-ms. Returns the
/// number of `terminals` rows updated (0 if the id is unknown).
///
/// Also marks any of the terminal's live agent sessions (`active`/`unknown`) as
/// `ended` (stamping `ended_at = now`): a voluntarily-closed terminal is logically
/// dead, so its session must not linger `active`/`unknown` forever (review #58) —
/// otherwise a sessions-history UI would show it active by mistake. Resume is
/// unaffected: `resume_candidates_on_boot` filters `terminals.status = ALIVE`, so a
/// closed terminal is never a boot candidate regardless of its session state. Both
/// writes run in one transaction so the terminal/session states are never observed
/// half-applied.
pub fn close_terminal(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    let now = now_millis();
    conn.transaction(|conn| {
        // Mark the terminal's still-live sessions ended (logically dead with it).
        diesel::update(
            agent_sessions::table
                .filter(agent_sessions::terminal_id.eq(id))
                .filter(
                    agent_sessions::state
                        .eq(SESSION_STATE_ACTIVE)
                        .or(agent_sessions::state.eq(SESSION_STATE_UNKNOWN)),
                ),
        )
        .set((
            agent_sessions::state.eq(SESSION_STATE_ENDED),
            agent_sessions::ended_at.eq(Some(now)),
        ))
        .execute(conn)?;
        // Flip the terminal itself to closed.
        diesel::update(terminals::table.find(id))
            .set((
                terminals::status.eq(STATUS_CLOSED),
                terminals::closed_at.eq(now),
                terminals::updated_at.eq(now),
            ))
            .execute(conn)
    })
}

/// Set a terminal's `order` to its position in `ids` (0-based), bumping
/// `updated_at`. Ids absent from the table are silently skipped. Runs in a
/// single transaction so the order is never observed half-applied.
pub fn reorder(conn: &mut SqliteConnection, ids: &[String]) -> QueryResult<()> {
    let now = now_millis();
    conn.transaction(|conn| {
        for (pos, id) in ids.iter().enumerate() {
            diesel::update(terminals::table.find(id.as_str()))
                .set((
                    terminals::order_index.eq(pos as i32),
                    terminals::updated_at.eq(now),
                ))
                .execute(conn)?;
        }
        Ok(())
    })
}

/// Rename a terminal (set its `label`; `None` clears it), bumping `updated_at`.
/// Returns rows updated.
pub fn rename(conn: &mut SqliteConnection, id: &str, label: Option<String>) -> QueryResult<usize> {
    diesel::update(terminals::table.find(id))
        .set((
            terminals::label.eq(label),
            terminals::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Record a terminal as the active one by stamping `last_active_at` with the
/// current epoch-ms. The launcher restores the alive terminal with the greatest
/// `last_active_at`, so a relaunch reopens on the last-used terminal. Does NOT
/// bump `updated_at` (becoming active is not a content change). Returns rows
/// updated (0 if the id is unknown).
pub fn set_active(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(terminals::table.find(id))
        .set(terminals::last_active_at.eq(now_millis()))
        .execute(conn)
}

/// Persist (overwrite) a terminal's serialized scrollback, bounded to
/// [`MAX_SCROLLBACK_BYTES`], bumping `updated_at`. The caller debounces; this
/// just stores the latest snapshot. Returns rows updated.
pub fn persist_scrollback(
    conn: &mut SqliteConnection,
    id: &str,
    serialized: &str,
) -> QueryResult<usize> {
    let bounded = bound_scrollback(serialized);
    diesel::update(terminals::table.find(id))
        .set((
            terminals::scrollback.eq(bounded),
            terminals::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Persist a terminal's working directory into its durable record, bumping
/// `updated_at` (FEEDBACK #32). The auto-naming poll reads the LIVE cwd from
/// `/proc` (for the sidebar label) but, before this, never wrote it back into the
/// record — so a relaunch re-spawned the shell at the STALE original `cwd` (the
/// workspace root) instead of the last directory the user `cd`'d into. The
/// front-side polls this on cwd CHANGE (debounced), so a respawn resumes where the
/// user left off. Mirrors `rename`. Returns rows updated (0 if the id is unknown).
pub fn set_terminal_cwd(conn: &mut SqliteConnection, id: &str, cwd: &str) -> QueryResult<usize> {
    diesel::update(terminals::table.find(id))
        .set((
            terminals::cwd.eq(cwd),
            terminals::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Read back a single terminal by id. Used by the CRUD tests now and by the
/// re-spawn flow in a later PRD; allow dead_code until that consumer lands.
#[cfg_attr(not(test), allow(dead_code))]
pub fn get_terminal(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<Terminal>> {
    terminals::table
        .find(id)
        .select(Terminal::as_select())
        .first(conn)
        .optional()
}

// --- Terminal exec-state (PRD-2.1 v4) ------------------------------------
//
// The DB record is the AUTHORITY for the sidebar exec-state badge after a
// restart. These helpers persist a transition's full tuple (state, exit code,
// unread flag, updated_at) and clear the unread flag on mark-read. The state
// machine that decides the transition from OSC 133 events lives in a later phase
// (`crate::command`-style runner); these are the pure persistence primitives it
// drives, mirroring `set_last_state` for command instances. The constraint set
// (CHECK on `exec_state` + `exec_state_unread`) is what makes an invalid value
// un-persistable through this path — see `set_exec_state` below.

/// Persist a terminal's exec-state transition: the new `state` (idle|running|
/// success|error — the [`STATE_IDLE`] … vocabulary), the last `exit_code`
/// (`None` clears it), and the `unread` notification flag, stamping
/// `exec_state_updated_at` with the current epoch-ms. An invalid `state` yields
/// an `Err` (the CHECK constraint rejects it — no invalid exec_state can be
/// persisted through normal code paths). Returns rows updated (0 if id unknown).
pub fn set_exec_state(
    conn: &mut SqliteConnection,
    id: &str,
    state: &str,
    exit_code: Option<i32>,
    unread: bool,
) -> QueryResult<usize> {
    let now = now_millis();
    diesel::update(terminals::table.find(id))
        .set((
            terminals::exec_state.eq(state),
            terminals::exec_exit_code.eq(exit_code),
            terminals::exec_state_unread.eq(unread),
            terminals::exec_state_updated_at.eq(now),
            terminals::updated_at.eq(now),
        ))
        .execute(conn)
}

/// BOOT NORMALIZATION (PRD task #2): settle every terminal stuck at a persisted
/// `exec_state = 'running'` down to `idle`, clearing the exit code and unread flag.
///
/// Busy/idle is now derived LIVE from the OS foreground process group (task #1), so
/// a persisted `running` is never the authority for the dot any more — it is at most
/// a stale artefact of a force-quit (the exact dogfood symptom: terminals left
/// `running` in the DB after `tauri dev` restarted the app mid-command). A live PTY
/// has no foreground command after a restart (the process did not survive), so it is
/// idle by construction; this call makes the PERSISTED field agree, so nothing — not
/// even a transient read before the first busy-state poll — can resurface a phantom
/// running. SETTLED results (`success`/`error`) and `idle` are LEFT UNTOUCHED (their
/// badge/unread survive the restart, like the managed-command `normalize_unrelaunched`).
/// Returns the number of rows normalized.
pub fn normalize_phantom_running_terminals(conn: &mut SqliteConnection) -> QueryResult<usize> {
    let now = now_millis();
    diesel::update(terminals::table.filter(terminals::exec_state.eq(STATE_RUNNING)))
        .set((
            terminals::exec_state.eq(STATE_IDLE),
            terminals::exec_exit_code.eq(None::<i32>),
            terminals::exec_state_unread.eq(false),
            terminals::exec_state_updated_at.eq(now),
            terminals::updated_at.eq(now),
        ))
        .execute(conn)
}

/// Mark a terminal's settled exec-state as READ: clear `exec_state_unread` to
/// false while LEAVING `exec_state`/`exec_exit_code` intact (the badge keeps its
/// success/error color but stops being a notification). This is the mark-read
/// path (the user viewed the terminal); it deliberately does NOT collapse the
/// state to idle, unlike the managed-command acknowledge model. Stamps
/// `exec_state_updated_at`. Returns rows updated (0 if id unknown).
pub fn mark_exec_state_read(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    let now = now_millis();
    diesel::update(terminals::table.find(id))
        .set((
            terminals::exec_state_unread.eq(false),
            terminals::exec_state_updated_at.eq(now),
            terminals::updated_at.eq(now),
        ))
        .execute(conn)
}

// --- Git branch detection (PRD-4 dogfood) --------------------------------
//
// A workspace is just a folder (nyx stays git-agnostic), but when the folder IS a
// git work tree we record its CURRENT HEAD branch as optional metadata, surfaced
// over MCP via `list_workspaces` (the `Workspace.branch` column). We shell out to
// the user's `git` rather than take a libgit2/gix dependency: it is a single,
// short, read-only call at workspace-creation time, and matches whatever git the
// user already has (worktrees, symlinked dirs, custom configs all "just work").
mod gitbranch {
    /// Turn the stdout of `git rev-parse --abbrev-ref HEAD` into a branch name, or
    /// `None`. `git` prints the branch name (`main`) on a normal checkout, or the
    /// literal `HEAD` when the work tree is in DETACHED HEAD state — there is no
    /// branch to record then, so that maps to `None`. Trailing newline / blank
    /// output is treated as "no branch".
    pub(super) fn parse_branch(stdout: &str) -> Option<String> {
        let name = stdout.trim();
        if name.is_empty() || name == "HEAD" {
            None
        } else {
            Some(name.to_string())
        }
    }

    /// Detect the current HEAD branch of the git work tree at `path`, or `None`
    /// when `path` is not a git work tree, git is unavailable, the command fails,
    /// or HEAD is detached. Never errors — branch is best-effort optional metadata,
    /// so a non-git path (the common case) simply yields `None`.
    pub(super) fn detect(path: &str) -> Option<String> {
        // Hardened spawn: the workspace-creation git probe must not flash a Windows
        // console (the dogfood finding on workspace-add).
        let output = crate::proc_util::command("git")
            .args(["-C", path, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()?;
        if !output.status.success() {
            // Not a repo, or any git error → no branch (stay git-agnostic).
            return None;
        }
        parse_branch(&String::from_utf8_lossy(&output.stdout))
    }
}

/// Resolve the CURRENT HEAD branch of the git work tree at `path` LIVE (or `None`
/// when `path` is not a git work tree, git is unavailable, or HEAD is detached).
/// The public entry point so a READER (the MCP `list_workspaces`) can refresh the
/// branch at read time instead of trusting the value captured at workspace-add time
/// — which goes stale the moment the user switches branches (the dogfood finding:
/// two worktrees both on `main` reported `branch:null`). The SAME `gitbranch::detect`
/// the creation path uses, so the read-time value matches the add-time semantics.
pub fn detect_branch(path: &str) -> Option<String> {
    gitbranch::detect(path)
}

// --- Project / Workspace CRUD (PRD-2 v2) ---------------------------------
//
// Pure DB functions taking `&mut SqliteConnection`, unit-tested against an
// in-memory database; the bridge wraps each in a `#[tauri::command]`. Paths are
// normalized via `crate::pathnorm` BEFORE storage so UNIQUE(project_id, path)
// and the auto-attach ancestor matching operate on canonical strings.

/// Derive a canonical uppercase-alpha `key` STUB from a project name (v10). Keeps
/// ASCII letters, uppercases them, and caps the length; empty input (or a name with no
/// letters) falls back to `PRJ`. This is only the SHAPE — uniqueness is enforced by the
/// caller (which disambiguates a collision) and by the `UNIQUE(projects.key)` index.
fn derive_project_key_stub(name: &str) -> String {
    let letters: String = name
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase();
    if letters.is_empty() {
        "PRJ".to_string()
    } else {
        letters
    }
}

/// Pick a UNIQUE `projects.key` for a new project, given the derived `stub`. Returns the
/// stub if free, else `stub || N` for the smallest N making it unique. Runs inside the
/// creating transaction so the read-then-insert is race-free under the single writer.
fn unique_project_key(conn: &mut SqliteConnection, stub: &str) -> QueryResult<String> {
    let exists = |conn: &mut SqliteConnection, candidate: &str| -> QueryResult<bool> {
        projects::table
            .filter(projects::key.eq(candidate))
            .select(projects::id)
            .first::<String>(conn)
            .optional()
            .map(|o| o.is_some())
    };
    if !exists(conn, stub)? {
        return Ok(stub.to_string());
    }
    let mut n = 2;
    loop {
        let candidate = format!("{stub}{n}");
        if !exists(conn, &candidate)? {
            return Ok(candidate);
        }
        n += 1;
    }
}

/// Create a project plus its single, explicitly-named ROOT workspace anchored at
/// `root_path`, in one transaction. `root_name` names the root workspace
/// (defaults to [`DEFAULT_ROOT_WORKSPACE_NAME`] when `None`/empty). The path is
/// normalized before storage. Returns the new project and its root workspace.
///
/// The project's canonical `key` (v10, NOT NULL + UNIQUE) is derived from `name`
/// ([`derive_project_key_stub`]) and disambiguated ([`unique_project_key`]) so the
/// UNIQUE constraint always holds; the durable `primary_workspace_id` is then set to the
/// freshly-created root workspace so the canonical primary role is anchored at creation.
pub fn create_project(
    conn: &mut SqliteConnection,
    name: &str,
    root_path: &str,
    root_name: Option<&str>,
) -> QueryResult<(Project, Workspace)> {
    let now = now_millis();
    let project_id = Uuid::now_v7().to_string();
    let normalized = pathnorm::normalize(root_path);
    let workspace_name = match root_name.map(str::trim) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => DEFAULT_ROOT_WORKSPACE_NAME.to_string(),
    };

    conn.transaction(|conn| {
        use diesel::dsl::max;
        // Append after the current max order so a new project lands LAST in the sidebar
        // (mirrors create_template / create_terminal). No rows yet → 0.
        let next_order = projects::table
            .select(max(projects::order_index))
            .first::<Option<i32>>(conn)?
            .map(|m| m + 1)
            .unwrap_or(0);

        let key = unique_project_key(conn, &derive_project_key_stub(name))?;

        diesel::insert_into(projects::table)
            .values(NewProject {
                id: project_id.clone(),
                name: name.to_string(),
                created_at: now,
                updated_at: now,
                order_index: next_order,
                key,
            })
            .execute(conn)?;

        let branch = gitbranch::detect(&normalized);
        let workspace = diesel::insert_into(workspaces::table)
            .values(NewWorkspace {
                id: Uuid::now_v7().to_string(),
                project_id: project_id.clone(),
                name: workspace_name,
                root_path: normalized,
                branch,
                is_root: true,
                created_at: now,
                updated_at: now,
            })
            .returning(Workspace::as_returning())
            .get_result(conn)?;

        // Anchor the canonical primary role (v10): the durable base workspace is the
        // root we just created. `primary_workspace_id` is the source of truth for
        // "the project's primary workspace" (the v11 data migration backfills it for
        // pre-v10 projects; here it is set at creation).
        let project = diesel::update(projects::table.find(&project_id))
            .set(projects::primary_workspace_id.eq(&workspace.id))
            .returning(Project::as_returning())
            .get_result(conn)?;

        // Materialize the project's templates as instances for the new root
        // workspace. A brand-new project has no templates yet, so this is a no-op
        // here; it keeps the "create workspace → materialize instances" invariant
        // uniform across both workspace-creation paths (root + added).
        materialize_instances_for_workspace(conn, &workspace.id)?;

        Ok((project, workspace))
    })
}

/// List all projects in SIDEBAR ORDER (`order` asc, then `created_at`/`id` asc as a
/// stable tiebreak). The order column is user-controlled via [`reorder_projects`]; the
/// v9 backfill seeds it from the prior created-at order so this is unchanged on upgrade.
pub fn list_projects(conn: &mut SqliteConnection) -> QueryResult<Vec<Project>> {
    projects::table
        .order((
            projects::order_index.asc(),
            projects::created_at.asc(),
            projects::id.asc(),
        ))
        .select(Project::as_select())
        .load(conn)
}

/// Set each project's `order` to its position in `ids` (0-based), bumping `updated_at`.
/// Ids absent from the table are silently skipped. Runs in a single transaction so the
/// order is never observed half-applied. The caller passes ALL project ids in their new
/// order. Mirrors [`reorder`] (terminals) / [`reorder_templates`] (commands).
pub fn reorder_projects(conn: &mut SqliteConnection, ids: &[String]) -> QueryResult<()> {
    let now = now_millis();
    conn.transaction(|conn| {
        for (pos, id) in ids.iter().enumerate() {
            diesel::update(projects::table.find(id.as_str()))
                .set((
                    projects::order_index.eq(pos as i32),
                    projects::updated_at.eq(now),
                ))
                .execute(conn)?;
        }
        Ok(())
    })
}

/// Fetch ONE project by id, or `None` when the id is unknown. The read companion
/// to the project mutations (`update_project` / `set_project_resume_agent_sessions`),
/// used by the MCP `set_project_settings` tool to return the project's post-write
/// state and to validate the id before mutating.
pub fn get_project(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<Project>> {
    projects::table
        .find(id)
        .select(Project::as_select())
        .first(conn)
        .optional()
}

/// Rename a project's display `name`, bumping `updated_at`. Returns rows updated
/// (0 if the id is unknown). The name is the human label shown in the sidebar
/// header; it carries no path semantics (the root workspace owns the path).
pub fn update_project(conn: &mut SqliteConnection, id: &str, name: &str) -> QueryResult<usize> {
    diesel::update(projects::table.find(id))
        .set((
            projects::name.eq(name),
            projects::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Delete a project and ALL its workspaces (the workspaces cascade via the FK
/// `workspaces.project_id REFERENCES projects(id) ON DELETE CASCADE`). Terminals
/// bound to those workspaces are NOT deleted — they are DETACHED (their
/// `workspace_id` is set to NULL) via `terminals.workspace_id REFERENCES
/// workspaces(id) ON DELETE SET NULL`, so they survive the project removal and
/// become loose/unattached. The two cascades require `PRAGMA foreign_keys = ON`
/// (enabled in [`Db::open`] / [`open_in_memory`]). Returns rows deleted from
/// `projects` (0 if the id is unknown).
pub fn delete_project(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::delete(projects::table.find(id)).execute(conn)
}

/// Persist a project's sidebar `collapsed` (open/closed) disclosure state,
/// bumping `updated_at`. Returns rows updated (0 if the id is unknown). The
/// sidebar restores each project band's open state from `!collapsed` on reload.
pub fn set_project_collapsed(
    conn: &mut SqliteConnection,
    id: &str,
    collapsed: bool,
) -> QueryResult<usize> {
    diesel::update(projects::table.find(id))
        .set((
            projects::collapsed.eq(collapsed),
            projects::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Persist a project's `resume_agent_sessions` opt-in (PRD-5 #5), bumping
/// `updated_at`. `true` makes nyx RESUME an active agent session for that project's
/// terminals at relaunch (inject the adapter's exact-resume command); `false` (the
/// default) leaves a bare shell. Returns rows updated (0 if the id is unknown).
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_project_resume_agent_sessions(
    conn: &mut SqliteConnection,
    id: &str,
    resume: bool,
) -> QueryResult<usize> {
    diesel::update(projects::table.find(id))
        .set((
            projects::resume_agent_sessions.eq(resume),
            projects::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Whether the PROJECT owning `terminal_id` opts in to agent-session resume
/// (PRD-5 #5). Resolves the terminal's workspace → project → `resume_agent_sessions`
/// in one join. Returns `false` when the terminal is unknown, has NO workspace (a
/// loose terminal with no project — OFF by construction per the PRD), or its
/// workspace/project was deleted. This is the per-project, default-OFF gate the
/// resume flow and the close-warning both consult.
#[cfg_attr(not(test), allow(dead_code))]
pub fn project_resumes_for_terminal(
    conn: &mut SqliteConnection,
    terminal_id: &str,
) -> QueryResult<bool> {
    let resume: Option<bool> = terminals::table
        .inner_join(workspaces::table.on(workspaces::id.nullable().eq(terminals::workspace_id)))
        .inner_join(projects::table.on(projects::id.eq(workspaces::project_id)))
        .filter(terminals::id.eq(terminal_id))
        .select(projects::resume_agent_sessions)
        .first::<bool>(conn)
        .optional()?;
    Ok(resume.unwrap_or(false))
}

/// Rename a workspace's display `name`, bumping `updated_at`. Returns rows
/// updated (0 if the id is unknown). The name is a human label; the path is
/// immutable (it identifies the folder), so a rename never touches `path`.
pub fn rename_workspace(conn: &mut SqliteConnection, id: &str, name: &str) -> QueryResult<usize> {
    diesel::update(workspaces::table.find(id))
        .set((
            workspaces::name.eq(name),
            workspaces::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Persist a workspace's sidebar `collapsed` (open/closed) disclosure state,
/// bumping `updated_at`. Returns rows updated (0 if the id is unknown). The
/// sidebar restores each workspace band's open state from `!collapsed` on reload.
pub fn set_workspace_collapsed(
    conn: &mut SqliteConnection,
    id: &str,
    collapsed: bool,
) -> QueryResult<usize> {
    diesel::update(workspaces::table.find(id))
        .set((
            workspaces::collapsed.eq(collapsed),
            workspaces::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Create a NON-root workspace at `path` in `project_id`. The path is normalized
/// before storage; the DB's UNIQUE(project_id, path) rejects a path already
/// present in the SAME project (a different project may hold the same path). The
/// caller surfaces that as a duplicate error. Returns the new workspace.
///
/// On creation the project's command templates are MATERIALIZED as instances for
/// the new workspace (one per template), in the same transaction as the insert —
/// so a project with N templates yields N instances for every workspace added.
pub fn create_workspace(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    path: &str,
) -> QueryResult<Workspace> {
    let now = now_millis();
    let normalized = pathnorm::normalize(path);
    let branch = gitbranch::detect(&normalized);
    conn.transaction(|conn| {
        let workspace = diesel::insert_into(workspaces::table)
            .values(NewWorkspace {
                id: Uuid::now_v7().to_string(),
                project_id: project_id.to_string(),
                name: name.to_string(),
                root_path: normalized,
                branch,
                is_root: false,
                created_at: now,
                updated_at: now,
            })
            .returning(Workspace::as_returning())
            .get_result(conn)?;

        // Materialize one instance per project template for the new workspace.
        materialize_instances_for_workspace(conn, &workspace.id)?;

        Ok(workspace)
    })
}

/// List the workspaces of `project_id`: the root first (`is_root` desc), then by
/// creation order. The single-root invariant makes "root first" deterministic.
pub fn list_workspaces(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<Vec<Workspace>> {
    workspaces::table
        .filter(workspaces::project_id.eq(project_id))
        .order((
            workspaces::is_root.desc(),
            workspaces::created_at.asc(),
            workspaces::id.asc(),
        ))
        .select(Workspace::as_select())
        .load(conn)
}

/// Every workspace across all projects — the candidate set the auto-attach
/// resolver matches a live cwd against (matching is global; binding then records
/// the single best match). Ordered by path for deterministic iteration.
pub fn all_workspaces(conn: &mut SqliteConnection) -> QueryResult<Vec<Workspace>> {
    workspaces::table
        .order((workspaces::root_path.asc(), workspaces::id.asc()))
        .select(Workspace::as_select())
        .load(conn)
}

// --- Repository CRUD (v2.1 canonical structure_locale, v10, PDR-0013) -----
//
// A repository is the LOGICAL identity of a repo within a project. The path lives
// HERE (`relative_path`), not per workspace — it is constant across the project's
// workspaces. There is NO checkout table; a workspace's repos are derived and Git
// state re-read from Git. These functions are the persistence socle; the detection
// (auto-detect under root_path) + derived-membership logic land in Phase 2.

/// Create a `repositories` row: a logical repo identity in `project_id`. `name` is the
/// logical name (e.g. `api`), `relative_path` the conventional path within the project
/// (`"."` in a mono-repo). `optional` marks a repo not worktree'd by default. The two
/// UNIQUE constraints — (project_id, name) and (project_id, relative_path) — reject a
/// duplicate name or path within the SAME project (a different project may reuse either).
#[cfg_attr(not(test), allow(dead_code))]
pub fn create_repository(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    relative_path: &str,
    optional: bool,
) -> QueryResult<Repository> {
    let now = now_millis();
    diesel::insert_into(repositories::table)
        .values(NewRepository {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.to_string(),
            name: name.to_string(),
            relative_path: relative_path.to_string(),
            optional,
            created_at: now,
            updated_at: now,
        })
        .returning(Repository::as_returning())
        .get_result(conn)
}

/// List the repositories of `project_id`, ordered by `relative_path` (then id) for
/// deterministic iteration. The read companion to [`create_repository`]. Excludes
/// soft-archived rows (`archived_at IS NOT NULL`) so callers see only the LIVE
/// logical repos; [`list_repositories_including_archived`] returns the full set.
#[cfg_attr(not(test), allow(dead_code))]
pub fn list_repositories(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<Vec<Repository>> {
    repositories::table
        .filter(repositories::project_id.eq(project_id))
        .filter(repositories::archived_at.is_null())
        .order((repositories::relative_path.asc(), repositories::id.asc()))
        .select(Repository::as_select())
        .load(conn)
}

/// List EVERY repository of `project_id`, archived or not (ordered like
/// [`list_repositories`]). Used where the caller needs the full logical set — e.g.
/// re-detection reconciling against previously-archived identities.
#[cfg_attr(not(test), allow(dead_code))]
pub fn list_repositories_including_archived(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<Vec<Repository>> {
    repositories::table
        .filter(repositories::project_id.eq(project_id))
        .order((repositories::relative_path.asc(), repositories::id.asc()))
        .select(Repository::as_select())
        .load(conn)
}

// --- Identity authority: stable-id resolution + archive lifecycle (task #4) ---
//
// nyx is the SINGLE authority for the three structuring identities
// (project / workspace / repository): there is no parallel Depot authority. The
// FULL lifecycle — create (above), RESOLVE (by the stable internal id, which is
// independent of any disk path), and ARCHIVE (soft, `archived_at`, never a hard
// delete of the row) — passes through these nyx functions, with the canonical
// invariants (unique project key, coherent `primary_workspace_id`, derived
// membership) enforced here and by the schema.

/// Resolve a repository by its STABLE internal id (UUIDv7), or `None` when unknown.
/// The read companion to [`get_project`] / [`get_workspace`]: identity resolution
/// keys on the id, NOT on a disk path — a repo keeps its identity when its
/// `relative_path` or the workspace `root_path` moves. Returns archived rows too
/// (the id still resolves; callers filter on `archived_at` when they want live-only).
#[cfg_attr(not(test), allow(dead_code))]
pub fn get_repository(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<Repository>> {
    repositories::table
        .find(id)
        .select(Repository::as_select())
        .first::<Repository>(conn)
        .optional()
}

/// Soft-ARCHIVE a project: stamp `archived_at` (epoch ms), leaving the row and all
/// its children in place. nyx NEVER hard-deletes an identity as part of archiving —
/// the row is permanent so history/references stay resolvable. Idempotent: an
/// already-archived project keeps its original `archived_at` (the guard skips it).
/// Returns rows updated (0 = unknown id, or already archived). [`unarchive_project`]
/// reverses it.
#[cfg_attr(not(test), allow(dead_code))]
pub fn archive_project(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(projects::table.find(id))
        .filter(projects::archived_at.is_null())
        .set((
            projects::archived_at.eq(now_millis()),
            projects::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Reverse [`archive_project`]: clear `archived_at` so the project is active again.
/// Returns rows updated (0 = unknown id or not currently archived).
#[cfg_attr(not(test), allow(dead_code))]
pub fn unarchive_project(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(projects::table.find(id))
        .filter(projects::archived_at.is_not_null())
        .set((
            projects::archived_at.eq(None::<i64>),
            projects::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Soft-ARCHIVE a workspace: stamp `archived_at` (epoch ms). The workspace ROW is
/// permanent (PDR-0008) — archiving marks it inactive without deleting it or its
/// runtime, so `folder_removed_at` (worktree cleanup) and the identity both survive.
/// Idempotent by the `archived_at IS NULL` guard. Returns rows updated.
#[cfg_attr(not(test), allow(dead_code))]
pub fn archive_workspace(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(workspaces::table.find(id))
        .filter(workspaces::archived_at.is_null())
        .set((
            workspaces::archived_at.eq(now_millis()),
            workspaces::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Reverse [`archive_workspace`]: clear `archived_at`. Returns rows updated.
#[cfg_attr(not(test), allow(dead_code))]
pub fn unarchive_workspace(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(workspaces::table.find(id))
        .filter(workspaces::archived_at.is_not_null())
        .set((
            workspaces::archived_at.eq(None::<i64>),
            workspaces::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Mark a workspace's worktree folder as CLEANED UP: stamp `folder_removed_at` (epoch
/// ms). This is the workspace-as-pivot invariant (PDR-0008): the ROW is PERMANENT, so
/// cleaning up a per-Spec worktree's folder at merge marks it here WITHOUT deleting the
/// row — its runtime history (terminals / agent_sessions / command_instances still
/// keyed on `workspace_id`) and its identity survive. There is deliberately NO
/// auto-delete path: a folder disappearing NEVER removes the workspace row. Idempotent
/// by the `folder_removed_at IS NULL` guard. Returns rows updated.
#[cfg_attr(not(test), allow(dead_code))]
pub fn mark_workspace_folder_removed(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(workspaces::table.find(id))
        .filter(workspaces::folder_removed_at.is_null())
        .set((
            workspaces::folder_removed_at.eq(now_millis()),
            workspaces::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Reverse [`mark_workspace_folder_removed`]: clear `folder_removed_at` (the folder was
/// re-materialized). Returns rows updated (0 = unknown id or not currently marked).
#[cfg_attr(not(test), allow(dead_code))]
pub fn clear_workspace_folder_removed(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(workspaces::table.find(id))
        .filter(workspaces::folder_removed_at.is_not_null())
        .set((
            workspaces::folder_removed_at.eq(None::<i64>),
            workspaces::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Soft-ARCHIVE a repository identity: stamp `archived_at` (epoch ms). The logical
/// identity is retired from active membership (auto-detection stops re-listing it)
/// WITHOUT deleting the row, so any history keyed on the repo id stays resolvable.
/// Idempotent by the `archived_at IS NULL` guard. Returns rows updated.
#[cfg_attr(not(test), allow(dead_code))]
pub fn archive_repository(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(repositories::table.find(id))
        .filter(repositories::archived_at.is_null())
        .set((
            repositories::archived_at.eq(now_millis()),
            repositories::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Reverse [`archive_repository`]: clear `archived_at`. Returns rows updated.
#[cfg_attr(not(test), allow(dead_code))]
pub fn unarchive_repository(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(repositories::table.find(id))
        .filter(repositories::archived_at.is_not_null())
        .set((
            repositories::archived_at.eq(None::<i64>),
            repositories::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Set (or move) a project's canonical `primary_workspace_id` to `workspace_id`,
/// enforcing the invariant that the target workspace BELONGS to the same project.
/// Returns `Ok(true)` when the pointer was set, `Ok(false)` when the invariant is
/// violated (the workspace is unknown or belongs to another project) — in which case
/// nothing is written, so `primary_workspace_id` never dangles at a foreign workspace.
/// The schema's `ON DELETE SET NULL` FK guarantees the pointer can never reference a
/// deleted workspace; this guard adds the cross-project coherence the FK alone cannot.
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_primary_workspace(
    conn: &mut SqliteConnection,
    project_id: &str,
    workspace_id: &str,
) -> QueryResult<bool> {
    conn.transaction(|conn| {
        let belongs: Option<String> = workspaces::table
            .find(workspace_id)
            .filter(workspaces::project_id.eq(project_id))
            .select(workspaces::id)
            .first::<String>(conn)
            .optional()?;
        if belongs.is_none() {
            return Ok(false);
        }
        diesel::update(projects::table.find(project_id))
            .set((
                projects::primary_workspace_id.eq(workspace_id),
                projects::updated_at.eq(now_millis()),
            ))
            .execute(conn)?;
        Ok(true)
    })
}

// --- Repository manual override + explicit config (task #5 / finding #27, PDR-0013) ---
//
// Repositories are configured EXPLICITLY, once per PROJECT (finding #27): there is NO
// disk auto-detection. Each repo is a `relative_path` (+ a logical `name`) stored on the
// project; a workspace's repos are then DERIVED at `workspace.root_path || relative_path`
// (PDR-0013). The user may additionally RENAME an identity, or mark it `optional` (the
// in-DB "nyxignore" that excludes it from a workspace's DERIVED membership). The Git state
// (branch/dirty/remotes) is NEVER written here — it is re-read from Git on demand.

/// Rename a repository's logical `name` (a manual override), bumping `updated_at`.
/// `relative_path` (the identity within the project) is untouched — a rename is a
/// label change only. Returns rows updated (0 = unknown id). The UNIQUE(project_id,
/// name) constraint rejects a collision with a sibling repo (surfaced as `Err`).
#[cfg_attr(not(test), allow(dead_code))]
pub fn rename_repository(conn: &mut SqliteConnection, id: &str, name: &str) -> QueryResult<usize> {
    diesel::update(repositories::table.find(id))
        .set((
            repositories::name.eq(name),
            repositories::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Set a repository's `optional` flag (a manual override), bumping `updated_at`.
/// `optional = true` is the in-DB "nyxignore": the repo is NOT worktree'd by default
/// and DROPS OUT of a workspace's derived membership. Returns rows updated (0 =
/// unknown id).
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_repository_optional(
    conn: &mut SqliteConnection,
    id: &str,
    optional: bool,
) -> QueryResult<usize> {
    diesel::update(repositories::table.find(id))
        .set((
            repositories::optional.eq(optional),
            repositories::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// One repository the user configures on a project (finding #27): a `relative_path`
/// within the project (`"."` for a mono-repo at the workspace root) plus an optional
/// logical `name` (defaults to the path's basename when absent/blank). This is the
/// EXPLICIT association that replaces the removed disk auto-detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoConfig {
    /// Conventional path within the project (`"."` = the workspace root itself).
    pub relative_path: String,
    /// Optional logical name; `None`/blank derives it from `relative_path`.
    pub name: Option<String>,
}

/// Normalize a user-supplied `relative_path` to the canonical conventional form:
/// trimmed, without a leading `./` or a trailing `/`; an empty path or `.` collapses to
/// [`crate::repository::MONO_REPO_RELATIVE_PATH`] (`"."`). Keeps the identity key
/// `(project_id, relative_path)` stable regardless of how the caller spelled it.
fn normalize_relative_path(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_start_matches("./")
        .trim_end_matches('/')
        .trim();
    if trimmed.is_empty() || trimmed == "." {
        crate::repository::MONO_REPO_RELATIVE_PATH.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Derive a logical repo `name` from its `relative_path` (the basename), or `"repo"`
/// when the path has no basename (e.g. `"."`). Used only when the caller supplies no
/// explicit name.
fn derive_repo_name(relative_path: &str) -> String {
    std::path::Path::new(relative_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "repo".to_string())
}

/// SET the explicit repository configuration of `project_id` (finding #27): the given
/// `repos` become the project's active logical identities, keyed on `(project_id,
/// relative_path)`. This is a full RECONCILE — the single write path the explicit config
/// tool uses:
///   * each configured repo is UPSERTED (created if absent, reactivated + renamed if it
///     had been archived or renamed) so re-running the config converges without
///     duplicating an identity or losing its history;
///   * any currently-active repo whose `relative_path` is NOT in the new set is
///     soft-ARCHIVED (removed from the config) — its row/history survive.
///
/// NEVER scans the disk. A project left with NO configured repos falls back at
/// derivation time to the workspace's current folder ([`crate::repository::workspace_members`]).
/// Returns the project's post-reconcile live repositories (canonical order).
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_project_repositories(
    conn: &mut SqliteConnection,
    project_id: &str,
    repos: &[RepoConfig],
) -> QueryResult<Vec<Repository>> {
    conn.transaction(|conn| {
        let mut kept: std::collections::HashSet<String> = std::collections::HashSet::new();
        for repo in repos {
            let relative_path = normalize_relative_path(&repo.relative_path);
            upsert_configured_repository(conn, project_id, &relative_path, repo.name.as_deref())?;
            kept.insert(relative_path);
        }
        // Reconcile: archive any ACTIVE identity no longer in the configured set.
        let active = list_repositories(conn, project_id)?;
        for repo in active {
            if !kept.contains(&repo.relative_path) {
                archive_repository(conn, &repo.id)?;
            }
        }
        list_repositories(conn, project_id)
    })
}

/// Upsert ONE explicitly-configured repository at `relative_path` in `project_id`, keyed
/// on the stable identity `(project_id, relative_path)`:
///   * ABSENT → insert a new logical identity (active, non-optional). Its `name` is the
///     caller's (disambiguated against siblings via [`unique_repository_name`]) or the
///     path's basename.
///   * PRESENT → reactivate it if it was archived, and apply the caller's explicit
///     `name` when provided (config is authoritative). A `None`/blank name keeps the
///     current name (so re-running the same config, or a rename-only edit elsewhere, is
///     preserved).
///
/// Returns the resolved [`Repository`] row.
fn upsert_configured_repository(
    conn: &mut SqliteConnection,
    project_id: &str,
    relative_path: &str,
    name: Option<&str>,
) -> QueryResult<Repository> {
    let desired_name = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    let existing: Option<Repository> = repositories::table
        .filter(repositories::project_id.eq(project_id))
        .filter(repositories::relative_path.eq(relative_path))
        .select(Repository::as_select())
        .first::<Repository>(conn)
        .optional()?;

    match existing {
        Some(repo) => {
            let reactivate = repo.archived_at.is_some();
            let rename = desired_name
                .as_deref()
                .map(|n| n != repo.name)
                .unwrap_or(false);
            if reactivate || rename {
                let final_name = desired_name.unwrap_or_else(|| repo.name.clone());
                diesel::update(repositories::table.find(&repo.id))
                    .set((
                        repositories::archived_at.eq(None::<i64>),
                        repositories::name.eq(final_name),
                        repositories::updated_at.eq(now_millis()),
                    ))
                    .execute(conn)?;
            }
            repositories::table
                .find(&repo.id)
                .select(Repository::as_select())
                .first::<Repository>(conn)
        }
        None => {
            // A brand-new identity. The `name` may collide with a sibling's; disambiguate
            // so UNIQUE(project_id, name) holds — the path is the identity, name a label.
            let base = desired_name.unwrap_or_else(|| derive_repo_name(relative_path));
            let name = unique_repository_name(conn, project_id, &base)?;
            create_repository(conn, project_id, &name, relative_path, false)
        }
    }
}

/// Pick a UNIQUE `repositories.name` within `project_id`, given a detected `stub`.
/// Returns the stub if free, else `stub-N` for the smallest N making it unique. Runs
/// inside the caller's transaction so the read-then-insert is race-free under the
/// single writer. Mirrors [`unique_project_key`] for the per-project name axis.
#[cfg_attr(not(test), allow(dead_code))]
fn unique_repository_name(
    conn: &mut SqliteConnection,
    project_id: &str,
    stub: &str,
) -> QueryResult<String> {
    let exists = |conn: &mut SqliteConnection, candidate: &str| -> QueryResult<bool> {
        repositories::table
            .filter(repositories::project_id.eq(project_id))
            .filter(repositories::name.eq(candidate))
            .select(repositories::id)
            .first::<String>(conn)
            .optional()
            .map(|o| o.is_some())
    };
    if !exists(conn, stub)? {
        return Ok(stub.to_string());
    }
    let mut n = 2;
    loop {
        let candidate = format!("{stub}-{n}");
        if !exists(conn, &candidate)? {
            return Ok(candidate);
        }
        n += 1;
    }
}

/// Attach `terminal_id` to `workspace_id` with the given binding `mode`
/// (`auto`|`manual`), bumping `updated_at`. Returns rows updated (0 = unknown
/// terminal). The foreign key ensures `workspace_id` exists; the CHECK ensures
/// `mode` is valid (an invalid mode yields an `Err`).
pub fn attach_terminal(
    conn: &mut SqliteConnection,
    terminal_id: &str,
    workspace_id: &str,
    mode: &str,
) -> QueryResult<usize> {
    diesel::update(terminals::table.find(terminal_id))
        .set((
            terminals::workspace_id.eq(Some(workspace_id)),
            terminals::workspace_binding_mode.eq(mode),
            terminals::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Detach `terminal_id` from any workspace (clears `workspace_id`) and reset its
/// binding mode to `auto` so it resumes auto-attach. Returns rows updated.
pub fn detach_terminal(conn: &mut SqliteConnection, terminal_id: &str) -> QueryResult<usize> {
    diesel::update(terminals::table.find(terminal_id))
        .set((
            terminals::workspace_id.eq(None::<String>),
            terminals::workspace_binding_mode.eq(BINDING_AUTO),
            terminals::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// PIN `terminal_id` to `workspace_id`: attach it and set mode `manual` so a
/// later `cd` no longer moves it. Returns rows updated.
pub fn pin_terminal_workspace(
    conn: &mut SqliteConnection,
    terminal_id: &str,
    workspace_id: &str,
) -> QueryResult<usize> {
    attach_terminal(conn, terminal_id, workspace_id, BINDING_MANUAL)
}

/// UNPIN `terminal_id`: flip mode back to `auto` (auto-attach resumes) while
/// KEEPING the current `workspace_id` until the resolver next moves it. Returns
/// rows updated.
pub fn unpin_terminal_workspace(
    conn: &mut SqliteConnection,
    terminal_id: &str,
) -> QueryResult<usize> {
    diesel::update(terminals::table.find(terminal_id))
        .set((
            terminals::workspace_binding_mode.eq(BINDING_AUTO),
            terminals::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

// --- Managed command (template) CRUD (PRD-3 v3) --------------------------
//
// Pure DB functions taking `&mut SqliteConnection`, unit-tested against an
// in-memory database; the bridge wraps each in a `#[tauri::command]` in a later
// phase. A template is a per-project record; instances are materialized from it
// separately (see the materialization helpers below).

/// Create a per-project command template, appended to the end of the project's
/// template order (max existing order + 1, or 0 if first). `subfolder` is an
/// optional run path relative to the workspace (`None` = root). `source` carries
/// optional package.json provenance (all-`None` = hand-authored). The DB's
/// UNIQUE(project_id, name) rejects a name already present in the SAME project
/// (surfaced as an `Err`). Returns the inserted row.
///
/// On creation the template is MATERIALIZED as instances across every existing
/// workspace of its project (one per workspace), in the same transaction as the
/// insert — so adding a template to a project with M workspaces yields M
/// instances. The symmetric half of `create_workspace`'s materialization.
#[cfg_attr(not(test), allow(dead_code))]
pub fn create_template(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    command: &str,
    subfolder: Option<&str>,
    source: CommandSource,
) -> QueryResult<ManagedCommand> {
    use diesel::dsl::max;
    let now = now_millis();
    conn.transaction(|conn| {
        // Append after the current max order WITHIN this project so creation order
        // is stable and scoped per project.
        let next_order = managed_commands::table
            .filter(managed_commands::project_id.eq(project_id))
            .select(max(managed_commands::order_index))
            .first::<Option<i32>>(conn)?
            .map(|m| m + 1)
            .unwrap_or(0);

        let template = diesel::insert_into(managed_commands::table)
            .values(NewManagedCommand {
                id: Uuid::now_v7().to_string(),
                project_id: project_id.to_string(),
                name: name.to_string(),
                command: command.to_string(),
                subfolder: subfolder.map(str::to_string),
                restart_on_startup: false,
                order_index: next_order,
                created_at: now,
                updated_at: now,
                source_kind: source.source_kind,
                source_package_json_path: source.source_package_json_path,
                source_script_name: source.source_script_name,
                source_script_command_snapshot: source.source_script_command_snapshot,
                package_manager: source.package_manager,
            })
            .returning(ManagedCommand::as_returning())
            .get_result(conn)?;

        // Materialize one instance per existing project workspace for the new
        // template.
        materialize_instances_for_template(conn, &template.id)?;

        Ok(template)
    })
}

/// List a project's command templates in sidebar order (`order` asc, then `id`
/// asc as a stable tiebreaker).
#[cfg_attr(not(test), allow(dead_code))]
pub fn list_templates(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<Vec<ManagedCommand>> {
    managed_commands::table
        .filter(managed_commands::project_id.eq(project_id))
        .order((
            managed_commands::order_index.asc(),
            managed_commands::id.asc(),
        ))
        .select(ManagedCommand::as_select())
        .load(conn)
}

/// Update a template's editable fields (`name`, `command`, `subfolder`), bumping
/// `updated_at`. `subfolder` `None` clears it (run at workspace root). Returns
/// rows updated (0 if the id is unknown). A name colliding with another template
/// in the same project yields an `Err` (UNIQUE(project_id, name)).
#[cfg_attr(not(test), allow(dead_code))]
pub fn update_template(
    conn: &mut SqliteConnection,
    id: &str,
    name: &str,
    command: &str,
    subfolder: Option<&str>,
) -> QueryResult<usize> {
    diesel::update(managed_commands::table.find(id))
        .set((
            managed_commands::name.eq(name),
            managed_commands::command.eq(command),
            managed_commands::subfolder.eq(subfolder),
            managed_commands::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Delete a template. Its `command_instances` cascade away (ON DELETE CASCADE on
/// `command_instances.command_id`). Returns rows deleted (0 if the id is unknown).
#[cfg_attr(not(test), allow(dead_code))]
pub fn delete_template(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::delete(managed_commands::table.find(id)).execute(conn)
}

/// Set each template's `order` to its position in `ids` (0-based), bumping
/// `updated_at`. Ids absent from the table are silently skipped. Runs in a single
/// transaction so the order is never observed half-applied. The caller passes the
/// ids of ONE project's templates in their new order.
#[cfg_attr(not(test), allow(dead_code))]
pub fn reorder_templates(conn: &mut SqliteConnection, ids: &[String]) -> QueryResult<()> {
    let now = now_millis();
    conn.transaction(|conn| {
        for (pos, id) in ids.iter().enumerate() {
            diesel::update(managed_commands::table.find(id.as_str()))
                .set((
                    managed_commands::order_index.eq(pos as i32),
                    managed_commands::updated_at.eq(now),
                ))
                .execute(conn)?;
        }
        Ok(())
    })
}

/// Toggle (set) a template's `restart_on_startup` flag, bumping `updated_at`.
/// Returns rows updated (0 if the id is unknown).
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_restart_on_startup(
    conn: &mut SqliteConnection,
    id: &str,
    restart: bool,
) -> QueryResult<usize> {
    diesel::update(managed_commands::table.find(id))
        .set((
            managed_commands::restart_on_startup.eq(restart),
            managed_commands::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Set (or clear) a template's package.json provenance fields, bumping
/// `updated_at`. An all-`None` `source` clears provenance (marks it
/// hand-authored). Returns rows updated (0 if the id is unknown).
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_template_source(
    conn: &mut SqliteConnection,
    id: &str,
    source: CommandSource,
) -> QueryResult<usize> {
    diesel::update(managed_commands::table.find(id))
        .set((
            managed_commands::source_kind.eq(source.source_kind),
            managed_commands::source_package_json_path.eq(source.source_package_json_path),
            managed_commands::source_script_name.eq(source.source_script_name),
            managed_commands::source_script_command_snapshot
                .eq(source.source_script_command_snapshot),
            managed_commands::package_manager.eq(source.package_manager),
            managed_commands::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Read back a single template by id. Used by tests now and the bridge/runner in
/// a later phase; allow dead_code until that consumer lands.
#[cfg_attr(not(test), allow(dead_code))]
pub fn get_template(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<ManagedCommand>> {
    managed_commands::table
        .find(id)
        .select(ManagedCommand::as_select())
        .first(conn)
        .optional()
}

// --- Command instance helpers (PRD-3 v3) ---------------------------------
//
// An instance is the materialization of one template for one workspace. The
// materialization functions themselves live below (PRD-3 Phase 1 task 2); these
// helpers mutate an existing instance's run-state columns. The runner (later
// phase) drives them on live transitions.

/// Set an instance's `last_state` (idle|running|success|error), bumping
/// `updated_at`. An invalid state yields an `Err` (the CHECK constraint). Returns
/// rows updated (0 if the id is unknown).
///
/// This writes ONLY the `last_state` column — it does NOT touch the v4 outcome
/// columns (`last_exit_code` / `ended_at` / `unread`). It stays the right call for
/// transitions that carry no run OUTCOME: a `running` start (use
/// [`set_run_state`] to also clear a stale code), a `stop` to idle, or boot
/// normalization of an orphaned `running`. A natural success/error finish must go
/// through [`set_run_state`] so the factual exit code + finish time + unread flag
/// are recorded.
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_last_state(conn: &mut SqliteConnection, id: &str, state: &str) -> QueryResult<usize> {
    diesel::update(command_instances::table.find(id))
        .set((
            command_instances::last_state.eq(state),
            command_instances::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Persist a full run-state TRANSITION with the v4 outcome columns kept consistent
/// — the single DB writer the production runner sink uses for every transition so
/// the FACTUAL outcome is decoupled from the notification/ack state:
///
///  - `running`           → set `last_state='running'`; CLEAR the prior outcome
///    (`last_exit_code`/`ended_at` → NULL) so a fresh run never reads a stale code;
///    leave `unread` as-is (a start does not produce an unseen result yet).
///  - `success` / `error` → set `last_state` + record `last_exit_code` (the natural
///    code; may be NULL for an unknown exit) + `ended_at = now` + `unread = 1` (a
///    finished run is an "unseen result" until acknowledged).
///  - `idle`              → set `last_state='idle'` only (a stop/normalize is not a
///    run completion: no code, no ended_at, no unread).
///
/// This is what makes a UI acknowledge unable to erase the outcome: an ack clears
/// `unread` via [`acknowledge_instance`] and NEVER calls this with `idle`, so the
/// persisted `last_state`/`last_exit_code`/`ended_at` survive. Returns rows updated
/// (0 if the id is unknown). An invalid `state` yields an `Err` (CHECK constraint).
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_run_state(
    conn: &mut SqliteConnection,
    id: &str,
    state: &str,
    exit_code: Option<i32>,
) -> QueryResult<usize> {
    let now = now_millis();
    match state {
        STATE_SUCCESS | STATE_ERROR => diesel::update(command_instances::table.find(id))
            .set((
                command_instances::last_state.eq(state),
                command_instances::last_exit_code.eq(exit_code),
                command_instances::ended_at.eq(Some(now)),
                command_instances::unread.eq(true),
                command_instances::updated_at.eq(now),
            ))
            .execute(conn),
        STATE_RUNNING => diesel::update(command_instances::table.find(id))
            .set((
                command_instances::last_state.eq(state),
                command_instances::last_exit_code.eq::<Option<i32>>(None),
                command_instances::ended_at.eq::<Option<i64>>(None),
                // A (re)start clears any stale unseen-result flag carried over from a
                // PRIOR finished run: the new run has produced no outcome yet, so it can
                // never be "unread". Without this, relaunching a finished-but-unacked
                // command leaves unread=1 on a now-`running` row → a phantom "unseen
                // result" surfaces after a cold restart (DB read, no live runner).
                command_instances::unread.eq(false),
                command_instances::updated_at.eq(now),
            ))
            .execute(conn),
        // idle (or any other state): touch only last_state — a stop/normalize is not
        // a completed run, so it carries no outcome and clears no unread flag.
        _ => set_last_state(conn, id, state),
    }
}

/// Clear an instance's `unread` notification flag — the persisted half of an
/// acknowledge. It touches ONLY `unread` (+ `updated_at`): the factual outcome
/// (`last_state` / `last_exit_code` / `ended_at`) is left intact, which is the whole
/// point of the v4 split (a UI ack must never erase the error the MCP sees). Returns
/// rows updated (0 if the id is unknown OR it was already read).
pub fn acknowledge_instance(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(command_instances::table.find(id))
        .filter(command_instances::unread.eq(true))
        .set((
            command_instances::unread.eq(false),
            command_instances::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Archive the LAST completed run into the bounded `prev_*` columns, then reset the
/// CURRENT run for a fresh (re)launch — the v5 "retain the previous run" writer the
/// production runner sink calls on `start` (BEFORE the new run's first persist).
///
/// Bounded to ONE prior run (N=1): the prior `prev_*` are OVERWRITTEN, never appended,
/// so history can never grow without limit. The archive only happens when the CURRENT
/// `last_state` is a finished run (`success`|`error`) — a start over an idle / running
/// / never-run instance has no completed run to keep, so `prev_*` are left untouched
/// (the very first launch keeps them at their `''`/NULL defaults).
///
/// In one transaction it:
///  - copies `scrollback`→`prev_scrollback`, `last_exit_code`→`prev_exit_code`,
///    `ended_at`→`prev_ended_at`, `last_state`→`prev_last_state` (only when the
///    current run finished);
///  - resets the CURRENT run: `scrollback=''`, `last_state='running'`,
///    `last_exit_code=NULL`, `ended_at=NULL` (a fresh run produces no outcome yet),
///    `unread=false` (a start is not an unseen result, and a stale unread from the
///    now-archived prior run must not survive onto the fresh `running` row).
///
/// This keeps the per-run separation intact: the CURRENT run starts clean (never
/// polluted by the prior run's bytes), while the prior run stays retrievable through
/// the `prev_*` columns. Returns rows updated (0 if the id is unknown).
#[cfg_attr(not(test), allow(dead_code))]
pub fn archive_and_reset_for_relaunch(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    let now = now_millis();
    conn.transaction(|conn| {
        // Snapshot the row's CURRENT run so we can decide what (if anything) to
        // archive. Unknown id → 0 rows updated (nothing to do).
        let Some(inst) = get_instance(conn, id)? else {
            return Ok(0);
        };

        // Archive the current run into prev_* ONLY when it actually FINISHED
        // (success|error). A running / idle / never-run instance has no completed run
        // to retain, so prev_* are left untouched (the very first launch keeps them at
        // their ''/NULL defaults). Bounded to ONE prior run: prev_* are OVERWRITTEN,
        // never appended, so retained history can never grow without limit.
        if inst.last_state == STATE_SUCCESS || inst.last_state == STATE_ERROR {
            diesel::update(command_instances::table.find(id))
                .set((
                    command_instances::prev_scrollback.eq(&inst.scrollback),
                    command_instances::prev_exit_code.eq(inst.last_exit_code),
                    command_instances::prev_ended_at.eq(inst.ended_at),
                    command_instances::prev_last_state.eq(Some(&inst.last_state)),
                ))
                .execute(conn)?;
        }

        // Reset the CURRENT run for the fresh launch so the new run begins clean
        // (never polluted by the prior run's bytes). Always runs (even on a first
        // start over an idle/never-run instance). Clears `unread`: a (re)start is not
        // an unseen result, and any stale unread=1 from the prior finished run (now
        // archived into prev_*) must not survive onto the fresh `running` row — else a
        // cold restart would surface a phantom "unseen result" on a run with no outcome.
        diesel::update(command_instances::table.find(id))
            .set((
                command_instances::scrollback.eq(""),
                command_instances::last_state.eq(STATE_RUNNING),
                command_instances::last_exit_code.eq::<Option<i32>>(None),
                command_instances::ended_at.eq::<Option<i64>>(None),
                command_instances::unread.eq(false),
                command_instances::updated_at.eq(now),
            ))
            .execute(conn)
    })
}

/// Persist (overwrite) an instance's serialized scrollback, bounded to
/// [`MAX_SCROLLBACK_BYTES`] (keeping the tail), bumping `updated_at`. Mirrors the
/// terminals' `persist_scrollback`. Returns rows updated (0 if the id is unknown).
#[cfg_attr(not(test), allow(dead_code))]
pub fn persist_instance_scrollback(
    conn: &mut SqliteConnection,
    id: &str,
    serialized: &str,
) -> QueryResult<usize> {
    let bounded = bound_scrollback(serialized);
    diesel::update(command_instances::table.find(id))
        .set((
            command_instances::scrollback.eq(bounded),
            command_instances::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Clear an instance's captured output buffer (PRD-4 review R-OUTPUT): empties BOTH
/// the current-run `scrollback` AND the retained-prior-run `prev_scrollback`, so a
/// subsequent `get_command_output` (current OR `run="previous"`) returns empty/new-only.
/// The FACTUAL outcome columns (`last_state`/`last_exit_code`/`ended_at`/`unread` and
/// the `prev_*` outcome) are LEFT INTACT — a clear wipes the bytes, it does not erase
/// the run result (an agent must still be able to tell a crash from a clean run after
/// clearing the noisy log). Bumps `updated_at`. Returns rows updated (0 if id unknown).
#[cfg_attr(not(test), allow(dead_code))]
pub fn clear_instance_scrollback(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(command_instances::table.find(id))
        .set((
            command_instances::scrollback.eq(""),
            command_instances::prev_scrollback.eq(""),
            command_instances::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Set an instance's `was_running_on_shutdown` snapshot flag, bumping
/// `updated_at`. The shutdown flow sets it to `true` for running instances; the
/// boot flow resets it to `false` after restoring. Returns rows updated.
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_was_running_on_shutdown(
    conn: &mut SqliteConnection,
    id: &str,
    was_running: bool,
) -> QueryResult<usize> {
    diesel::update(command_instances::table.find(id))
        .set((
            command_instances::was_running_on_shutdown.eq(was_running),
            command_instances::updated_at.eq(now_millis()),
        ))
        .execute(conn)
}

/// Read back a single instance by id. Used by tests now and the runner in a later
/// phase; allow dead_code until that consumer lands.
#[cfg_attr(not(test), allow(dead_code))]
pub fn get_instance(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<CommandInstance>> {
    command_instances::table
        .find(id)
        .select(CommandInstance::as_select())
        .first(conn)
        .optional()
}

/// Everything the runner needs to SPAWN an instance: its template's `command` +
/// `subfolder` and its workspace's `path`. A single join so `command_start` /
/// `command_relaunch` resolve the cwd (via [`crate::subfolder`]) and the command
/// line in one query. `None` if the instance id is unknown.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRunContext {
    pub command: String,
    pub subfolder: Option<String>,
    pub workspace_path: String,
    /// Whether the instance's template relaunches at app start (for restore).
    pub restart_on_startup: bool,
}

/// Resolve the [`InstanceRunContext`] for `instance_id` by joining the instance to
/// its template (`command`, `subfolder`, `restart_on_startup`) and its workspace
/// (`path`). `None` if the instance does not exist.
#[cfg_attr(not(test), allow(dead_code))]
pub fn instance_run_context(
    conn: &mut SqliteConnection,
    instance_id: &str,
) -> QueryResult<Option<InstanceRunContext>> {
    command_instances::table
        .inner_join(managed_commands::table)
        .inner_join(workspaces::table)
        .filter(command_instances::id.eq(instance_id))
        .select((
            managed_commands::command,
            managed_commands::subfolder,
            workspaces::root_path,
            managed_commands::restart_on_startup,
        ))
        .first::<(String, Option<String>, String, bool)>(conn)
        .optional()
        .map(|opt| {
            opt.map(
                |(command, subfolder, workspace_path, restart_on_startup)| InstanceRunContext {
                    command,
                    subfolder,
                    workspace_path,
                    restart_on_startup,
                },
            )
        })
}

/// The stored (normalized) `path` of a workspace by id, or `None` if unknown. Used
/// by the package.json import command to know what folder to scan.
#[cfg_attr(not(test), allow(dead_code))]
pub fn workspace_path(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<String>> {
    workspaces::table
        .find(id)
        .select(workspaces::root_path)
        .first::<String>(conn)
        .optional()
}

/// A single workspace row by id, or `None` if unknown. Mirrors
/// [`get_template`]/[`get_instance`] for the cases that need the FULL row (both the
/// `project_id` and the `path`) — e.g. the MCP `import_commands` tool's
/// `workspace_id` form, which scans the one workspace and imports into its project.
#[cfg_attr(not(test), allow(dead_code))]
pub fn get_workspace(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<Workspace>> {
    workspaces::table
        .find(id)
        .select(Workspace::as_select())
        .first::<Workspace>(conn)
        .optional()
}

/// The ids of every instance of `command_id` (one per workspace of the project).
/// Used by the running-mutation guard: a template cannot be updated/deleted while
/// ANY of its instances is running.
#[cfg_attr(not(test), allow(dead_code))]
pub fn instance_ids_for_template(
    conn: &mut SqliteConnection,
    command_id: &str,
) -> QueryResult<Vec<String>> {
    command_instances::table
        .filter(command_instances::command_id.eq(command_id))
        .select(command_instances::id)
        .load(conn)
}

/// The ids of every command instance belonging to `project_id` (across all its
/// templates and workspaces). Used by the `delete_project` guard: a project cannot
/// be deleted while ANY of its command instances is running.
#[cfg_attr(not(test), allow(dead_code))]
pub fn instance_ids_for_project(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<Vec<String>> {
    command_instances::table
        .inner_join(managed_commands::table)
        .filter(managed_commands::project_id.eq(project_id))
        .select(command_instances::id)
        .load(conn)
}

/// The ids of every command instance belonging to `workspace_id`. Used by the
/// `remove_workspace` guard (A2): a workspace cannot be deleted while ANY of its
/// command instances is running, same as the project-level guard.
pub fn instance_ids_for_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<Vec<String>> {
    command_instances::table
        .filter(command_instances::workspace_id.eq(workspace_id))
        .select(command_instances::id)
        .load(conn)
}

/// The RECORD ids of every terminal attached to `workspace_id`. Used by the
/// destructive `remove_workspace` cascade: the sidecar adapter closes each of
/// these terminals' LIVE PTYs (via its PTY manager / `TerminalHost`) before the
/// use case deletes the rows, so no interactive shell is orphaned.
pub fn terminal_ids_for_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<Vec<String>> {
    terminals::table
        .filter(terminals::workspace_id.eq(workspace_id))
        .select(terminals::id)
        .load(conn)
}

/// HARD-delete every `agent_sessions` row anchored to `workspace_id` (the workspace-scoped
/// half of the destructive remove cascade). This covers sessions whose `workspace_id`
/// points at the workspace even when their host terminal is loose (not attached to it);
/// sessions on the workspace's OWN terminals are additionally cascaded by
/// [`delete_terminals_for_workspace`] (agent_sessions → terminals is ON DELETE CASCADE).
/// Returns rows deleted. MUST run inside the same transaction as the workspace delete.
pub fn delete_agent_sessions_for_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<usize> {
    diesel::delete(agent_sessions::table.filter(agent_sessions::workspace_id.eq(workspace_id)))
        .execute(conn)
}

/// HARD-delete every `terminals` row attached to `workspace_id` (the workspace-scoped half
/// of the destructive remove cascade). Deleting a terminal CASCADES its remaining
/// `agent_sessions` (FK ON DELETE CASCADE), so this also reaps any session still hosted on
/// one of the workspace's terminals. Returns rows deleted. MUST run inside the same
/// transaction as the workspace delete.
pub fn delete_terminals_for_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<usize> {
    diesel::delete(terminals::table.filter(terminals::workspace_id.eq(workspace_id))).execute(conn)
}

/// Delete a workspace. ON DELETE CASCADE removes its command instances. Its terminals and
/// agent sessions are NOT reaped by this call — the workspace FK on both is ON DELETE SET
/// NULL, so the [`crate::usecase::remove_workspace`] use case deletes those rows EXPLICITLY
/// (via [`delete_agent_sessions_for_workspace`] + [`delete_terminals_for_workspace`]) in the
/// same transaction, BEFORE this call, to make workspace removal a full destructive cascade
/// of its runtime (no orphaned "loose" terminals). Returns rows deleted (0 = workspace not
/// found). The caller is responsible for guarding against deleting a project's ROOT
/// workspace — removing the root would leave a rootless project (see the use case, which
/// refuses `is_root` rows) — and for killing any LIVE process (see `instance_ids_for_workspace`
/// + `terminal_ids_for_workspace`) from the adapter that owns the runner.
pub fn delete_workspace(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::delete(workspaces::table.find(id)).execute(conn)
}

/// Every command instance across all projects, with the run context needed to
/// restore it at boot (template `restart_on_startup`, the instance's
/// `last_state`/`was_running_on_shutdown`, plus the command line + cwd inputs).
/// Used by the shutdown snapshot + boot restoration flow (task 16).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRow {
    pub instance_id: String,
    pub last_state: String,
    pub was_running_on_shutdown: bool,
    pub restart_on_startup: bool,
    pub command: String,
    pub subfolder: Option<String>,
    pub workspace_path: String,
}

/// Load every instance joined to its template + workspace, returning the rows the
/// shutdown/boot flow reasons over (task 16). One row per instance.
#[cfg_attr(not(test), allow(dead_code))]
pub fn all_instances_for_restore(conn: &mut SqliteConnection) -> QueryResult<Vec<RestoreRow>> {
    command_instances::table
        .inner_join(managed_commands::table)
        .inner_join(workspaces::table)
        .select((
            command_instances::id,
            command_instances::last_state,
            command_instances::was_running_on_shutdown,
            managed_commands::restart_on_startup,
            managed_commands::command,
            managed_commands::subfolder,
            workspaces::root_path,
        ))
        .load::<(String, String, bool, bool, String, Option<String>, String)>(conn)
        .map(|rows| {
            rows.into_iter()
                .map(
                    |(
                        instance_id,
                        last_state,
                        was_running_on_shutdown,
                        restart_on_startup,
                        command,
                        subfolder,
                        workspace_path,
                    )| RestoreRow {
                        instance_id,
                        last_state,
                        was_running_on_shutdown,
                        restart_on_startup,
                        command,
                        subfolder,
                        workspace_path,
                    },
                )
                .collect()
        })
}

// --- Materialization + per-workspace listing (PRD-3 v3, task 2) -----------
//
// An instance is the materialization of one template for one workspace. Creating
// a workspace materializes one instance per template of its project; creating a
// template materializes one instance per existing workspace of its project. Both
// are IDEMPOTENT: `INSERT ... ON CONFLICT(command_id, workspace_id) DO NOTHING`
// relies on the v3 `UNIQUE(command_id, workspace_id)` so re-running never
// duplicates or errors. They are NOT public spawn/runner logic — only the
// persistent rows (the runner lands in a later phase).

/// Materialize the project's templates as instances for ONE workspace: for every
/// `managed_command` of `workspace_id`'s project, ensure a `command_instance`
/// exists for that (template, workspace) pair. Idempotent (ON CONFLICT DO
/// NOTHING). Returns the number of instances actually inserted (0 if all already
/// existed or the project has no templates). Runs in one transaction.
pub fn materialize_instances_for_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<usize> {
    conn.transaction(|conn| {
        // The project owning this workspace.
        let project_id: String = workspaces::table
            .find(workspace_id)
            .select(workspaces::project_id)
            .first(conn)?;

        // Every template of that project.
        let command_ids: Vec<String> = managed_commands::table
            .filter(managed_commands::project_id.eq(&project_id))
            .select(managed_commands::id)
            .load(conn)?;

        let now = now_millis();
        let mut inserted = 0usize;
        for command_id in command_ids {
            inserted += diesel::insert_into(command_instances::table)
                .values(NewCommandInstance {
                    id: Uuid::now_v7().to_string(),
                    command_id,
                    workspace_id: workspace_id.to_string(),
                    created_at: now,
                    updated_at: now,
                })
                // Idempotent: a pre-existing (command, workspace) pair is skipped.
                .on_conflict((
                    command_instances::command_id,
                    command_instances::workspace_id,
                ))
                .do_nothing()
                .execute(conn)?;
        }
        Ok(inserted)
    })
}

/// Materialize ONE template as instances across every existing workspace of its
/// project: for each `workspace` of the template's project, ensure a
/// `command_instance` exists for that (template, workspace) pair. Idempotent (ON
/// CONFLICT DO NOTHING). Returns the number of instances actually inserted. Runs
/// in one transaction.
#[cfg_attr(not(test), allow(dead_code))]
pub fn materialize_instances_for_template(
    conn: &mut SqliteConnection,
    command_id: &str,
) -> QueryResult<usize> {
    conn.transaction(|conn| {
        // The project owning this template.
        let project_id: String = managed_commands::table
            .find(command_id)
            .select(managed_commands::project_id)
            .first(conn)?;

        // Every workspace of that project.
        let workspace_ids: Vec<String> = workspaces::table
            .filter(workspaces::project_id.eq(&project_id))
            .select(workspaces::id)
            .load(conn)?;

        let now = now_millis();
        let mut inserted = 0usize;
        for workspace_id in workspace_ids {
            inserted += diesel::insert_into(command_instances::table)
                .values(NewCommandInstance {
                    id: Uuid::now_v7().to_string(),
                    command_id: command_id.to_string(),
                    workspace_id,
                    created_at: now,
                    updated_at: now,
                })
                .on_conflict((
                    command_instances::command_id,
                    command_instances::workspace_id,
                ))
                .do_nothing()
                .execute(conn)?;
        }
        Ok(inserted)
    })
}

/// An instance joined to its template's display fields — the row a per-workspace
/// listing returns. `Serialize` for the IPC boundary. The instance carries run
/// state + scrollback; the template fields (`name`, `command`, `subfolder`, the
/// `source_*` provenance) come from the joined `managed_commands` row, and the
/// `workspace_path` from the joined `workspaces` row, so the UI can render a
/// service's info bar (command + run directory + source) without a second query.
///
/// `cwd` is NOT a DB column: `list_instances_for_workspace` leaves it `None`; the
/// bridge fills it with the resolved run directory (`workspace_path` + `subfolder`)
/// before serializing to the front (see `command_instance_list`). The raw
/// `workspace_path` + `subfolder` stay on the row so a caller that does not resolve
/// (e.g. a pure-DB test) still has them.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InstanceWithTemplate {
    // Instance columns.
    pub id: String,
    pub command_id: String,
    pub workspace_id: String,
    pub last_state: String,
    pub scrollback: String,
    pub was_running_on_shutdown: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// v4 factual-outcome columns: the last completed run's natural exit code +
    /// finish time (None while never-finished/running), and the separate `unread`
    /// "unseen result" flag a UI acknowledge clears without erasing the outcome.
    pub last_exit_code: Option<i32>,
    pub ended_at: Option<i64>,
    pub unread: bool,
    // Joined template columns.
    pub name: String,
    pub command: String,
    pub subfolder: Option<String>,
    /// The template's sidebar order (so the listing can sort like the project).
    pub order_index: i32,
    /// package.json provenance (None for a hand-authored template): the source
    /// kind, the package.json path, the script name, and the detected manager.
    pub source_kind: Option<String>,
    pub source_package_json_path: Option<String>,
    pub source_script_name: Option<String>,
    pub package_manager: Option<String>,
    /// The instance's workspace path (joined from `workspaces`). The base the run
    /// directory resolves against.
    pub workspace_path: String,
    /// The RESOLVED run directory (`workspace_path` + `subfolder`), filled by the
    /// bridge for the front's info bar. `None` straight out of the DB query.
    pub cwd: Option<String>,
}

/// List a workspace's command instances, each JOINED to its template's display
/// fields (`name`, `command`, `subfolder`, the `source_*` provenance, order) and
/// its workspace `path`. Ordered by the template's `order` (then `id` as a stable
/// tiebreaker) so the listing matches the project's template order. Returns one row
/// per instance of `workspace_id`. `cwd` is left `None` (the bridge resolves it).
#[cfg_attr(not(test), allow(dead_code))]
pub fn list_instances_for_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<Vec<InstanceWithTemplate>> {
    command_instances::table
        .inner_join(managed_commands::table)
        .inner_join(workspaces::table)
        .filter(command_instances::workspace_id.eq(workspace_id))
        .order((
            managed_commands::order_index.asc(),
            managed_commands::id.asc(),
        ))
        .select((
            command_instances::id,
            command_instances::command_id,
            command_instances::workspace_id,
            command_instances::last_state,
            command_instances::scrollback,
            command_instances::was_running_on_shutdown,
            command_instances::created_at,
            command_instances::updated_at,
            command_instances::last_exit_code,
            command_instances::ended_at,
            command_instances::unread,
            managed_commands::name,
            managed_commands::command,
            managed_commands::subfolder,
            managed_commands::order_index,
            managed_commands::source_kind,
            managed_commands::source_package_json_path,
            managed_commands::source_script_name,
            managed_commands::package_manager,
            workspaces::root_path,
        ))
        .load::<(
            String,
            String,
            String,
            String,
            String,
            bool,
            i64,
            i64,
            Option<i32>,
            Option<i64>,
            bool,
            String,
            String,
            Option<String>,
            i32,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        )>(conn)
        .map(|rows| {
            rows.into_iter()
                .map(|r| InstanceWithTemplate {
                    id: r.0,
                    command_id: r.1,
                    workspace_id: r.2,
                    last_state: r.3,
                    scrollback: r.4,
                    was_running_on_shutdown: r.5,
                    created_at: r.6,
                    updated_at: r.7,
                    last_exit_code: r.8,
                    ended_at: r.9,
                    unread: r.10,
                    name: r.11,
                    command: r.12,
                    subfolder: r.13,
                    order_index: r.14,
                    source_kind: r.15,
                    source_package_json_path: r.16,
                    source_script_name: r.17,
                    package_manager: r.18,
                    workspace_path: r.19,
                    cwd: None,
                })
                .collect()
        })
}

// --- Agent session models + CRUD (PRD-5 v7, ADR-0010) --------------------
//
// The GENERIC agent-session record: one row per captured agent session, anchored
// to a terminal (CASCADE) and an OPTIONAL workspace (SET NULL — the project is
// derived via the workspace, never denormalized). Pure DB functions taking
// `&mut SqliteConnection`, unit-tested against an in-memory database; the adapter
// registry + bridge of later phases wrap them. Only `claude_code` is produced in
// v1, but every helper is agent-agnostic (keyed by `agent_kind`).
//
// `allow(dead_code)` on the not-yet-wired functions until the registry/bridge
// (later phases) consume them; the v7 tests already exercise them.

/// An `agent_sessions` row as read from the database. `Serialize` so the bridge can
/// return it across the Tauri IPC boundary. Timestamps are epoch ms; `ended_at` is
/// `None` until a clean end. `state`/`agent_kind` are the CHECK-enforced vocabularies
/// ([`SESSION_STATE_ACTIVE`] … / [`AGENT_KIND_CLAUDE_CODE`] …). `metadata_json` is a
/// raw adapter JSON bag (default `"{}"`).
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, serde::Serialize)]
#[diesel(table_name = agent_sessions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AgentSession {
    pub id: String,
    /// The terminal hosting this session (FK → `terminals`, CASCADE on delete).
    pub terminal_id: String,
    /// Optional workspace anchor (`None` = unattached). FK → `workspaces`, SET NULL
    /// on delete. The project is DERIVED via this workspace — there is no project_id.
    pub workspace_id: Option<String>,
    /// Which agent (`claude_code` | `codex` | `opencode` | `custom`). CHECK-enforced.
    pub agent_kind: String,
    /// The agent's OWN session id — what the resume command is built from.
    pub external_session_id: String,
    /// The working directory the session was captured in.
    pub cwd: String,
    /// Lifecycle state (`active` | `ended` | `unknown` | `resume_failed`). CHECK-enforced.
    pub state: String,
    /// Optional path to the agent's transcript (e.g. Claude's `<id>.jsonl`).
    pub transcript_path: Option<String>,
    /// Adapter-specific JSON bag (default `"{}"`). No key required at the common level.
    pub metadata_json: String,
    pub started_at: i64,
    /// `None` until a clean end stamps it.
    pub ended_at: Option<i64>,
    /// Refreshed on every `SessionStart` event; the staleness probe compares it
    /// against a threshold to flip a long-silent `active` row to `unknown`.
    pub last_seen_at: i64,
}

/// A new `agent_sessions` row to insert. `id` is a backend-generated UUIDv7;
/// `started_at`/`last_seen_at` are set explicitly to the same instant so creation is
/// deterministic and testable. `state` defaults to `active` (column default) but is
/// set explicitly here. `ended_at` is left NULL (column default).
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = agent_sessions)]
struct NewAgentSession {
    id: String,
    terminal_id: String,
    workspace_id: Option<String>,
    agent_kind: String,
    external_session_id: String,
    cwd: String,
    state: String,
    transcript_path: Option<String>,
    metadata_json: String,
    started_at: i64,
    last_seen_at: i64,
}

/// The fields an adapter captures for a session — passed as a group to
/// [`record_session_start`]. `metadata_json` is an adapter JSON bag; `None` stores
/// the empty object `"{}"`.
#[derive(Debug, Clone, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct SessionCapture {
    pub workspace_id: Option<String>,
    pub external_session_id: String,
    pub cwd: String,
    pub transcript_path: Option<String>,
    pub metadata_json: Option<String>,
}

/// Record an agent `SessionStart` for `terminal_id` + `agent_kind`: UPSERT the
/// single ACTIVE session for that pair.
///
/// SessionStart is the authoritative "this terminal now hosts a live session"
/// signal. Because at most ONE `active` row may exist per `(terminal_id, agent_kind)`
/// (partial unique index), this:
///   * UPDATEs the existing active row in place when one exists — refreshing
///     `external_session_id` / `cwd` / `transcript_path` / `metadata_json` and
///     stamping `last_seen_at = now` (a `resume` SessionStart on the SAME terminal
///     keeps one row, not two);
///   * otherwise INSERTs a fresh `active` row.
///
/// Runs in a transaction so the find-then-write is atomic. Returns the active row.
#[cfg_attr(not(test), allow(dead_code))]
pub fn record_session_start(
    conn: &mut SqliteConnection,
    terminal_id: &str,
    agent_kind: &str,
    capture: SessionCapture,
) -> QueryResult<AgentSession> {
    let now = now_millis();
    let metadata = capture.metadata_json.unwrap_or_else(|| "{}".to_string());
    conn.transaction(|conn| {
        // Is there already an active session for this terminal+agent? (the partial
        // unique index guarantees at most one).
        let existing: Option<String> = agent_sessions::table
            .filter(agent_sessions::terminal_id.eq(terminal_id))
            .filter(agent_sessions::agent_kind.eq(agent_kind))
            .filter(agent_sessions::state.eq(SESSION_STATE_ACTIVE))
            .select(agent_sessions::id)
            .first::<String>(conn)
            .optional()?;

        let row: AgentSession = if let Some(id) = existing {
            // Refresh the live row in place (a resume on the same terminal keeps ONE
            // row). last_seen_at + started fields move forward; state stays active.
            diesel::update(agent_sessions::table.find(&id))
                .set((
                    agent_sessions::workspace_id.eq(capture.workspace_id),
                    agent_sessions::external_session_id.eq(capture.external_session_id),
                    agent_sessions::cwd.eq(capture.cwd),
                    agent_sessions::transcript_path.eq(capture.transcript_path),
                    agent_sessions::metadata_json.eq(metadata),
                    agent_sessions::last_seen_at.eq(now),
                ))
                .returning(AgentSession::as_returning())
                .get_result(conn)?
        } else {
            // No live row. A `resume` SessionStart re-attaches to a session that may have
            // been swept to `unknown` (stale active, probable kill) on boot — or left
            // `resume_failed` / cleanly `ended` — carrying the SAME external id. REVIVE
            // that exact row to `active` instead of inserting a duplicate: a blind insert
            // would ORPHAN the prior row, which keeps re-qualifying as a resume +
            // close-warning candidate on every boot and accumulates one orphan per
            // kill→resume cycle (it contradicts this fn's "keeps ONE row, not two" intent).
            // Match on the agent's OWN session id (the resume correlation) so a genuinely
            // NEW session (new id) still inserts its own distinct row.
            let prior: Option<String> = agent_sessions::table
                .filter(agent_sessions::terminal_id.eq(terminal_id))
                .filter(agent_sessions::agent_kind.eq(agent_kind))
                .filter(agent_sessions::external_session_id.eq(&capture.external_session_id))
                // Several historical rows can share one external id on the same terminal
                // (e.g. an `ended` row plus a later `resume_failed` row for the same
                // conversation), so pin the revive to the MOST RECENT one — otherwise
                // `.first()` would revive an arbitrary (storage-order) row.
                .order(agent_sessions::started_at.desc())
                .select(agent_sessions::id)
                .first::<String>(conn)
                .optional()?;
            if let Some(id) = prior {
                diesel::update(agent_sessions::table.find(&id))
                    .set((
                        agent_sessions::workspace_id.eq(capture.workspace_id),
                        agent_sessions::cwd.eq(capture.cwd),
                        agent_sessions::transcript_path.eq(capture.transcript_path),
                        agent_sessions::metadata_json.eq(metadata),
                        agent_sessions::state.eq(SESSION_STATE_ACTIVE),
                        // It is live again — clear any stale end stamp.
                        agent_sessions::ended_at.eq(None::<i64>),
                        agent_sessions::last_seen_at.eq(now),
                    ))
                    .returning(AgentSession::as_returning())
                    .get_result(conn)?
            } else {
                diesel::insert_into(agent_sessions::table)
                    .values(NewAgentSession {
                        id: Uuid::now_v7().to_string(),
                        terminal_id: terminal_id.to_string(),
                        workspace_id: capture.workspace_id,
                        agent_kind: agent_kind.to_string(),
                        external_session_id: capture.external_session_id,
                        cwd: capture.cwd,
                        state: SESSION_STATE_ACTIVE.to_string(),
                        transcript_path: capture.transcript_path,
                        metadata_json: metadata,
                        started_at: now,
                        last_seen_at: now,
                    })
                    .returning(AgentSession::as_returning())
                    .get_result(conn)?
            }
        };

        // SUPERSEDE-ON-START (finding #82): a fresh live session on a terminal makes any
        // OTHER `active`/`unknown` session of the SAME terminal+agent provably stale — a
        // kill→relaunch leaves the prior row `active`/`unknown` (no clean SessionEnd), and
        // it would otherwise keep warning at every close as a phantom duplicate. Mark those
        // siblings `ended` here (in the same transaction), EXCLUDING the row we just wrote
        // (`id != row.id`). The partial-unique `active` index already forbids two `active`
        // rows, but an `unknown` (stale-swept) sibling can coexist — this is what clears it.
        diesel::update(
            agent_sessions::table
                .filter(agent_sessions::terminal_id.eq(terminal_id))
                .filter(agent_sessions::agent_kind.eq(agent_kind))
                .filter(agent_sessions::id.ne(&row.id))
                .filter(
                    agent_sessions::state
                        .eq(SESSION_STATE_ACTIVE)
                        .or(agent_sessions::state.eq(SESSION_STATE_UNKNOWN)),
                ),
        )
        .set((
            agent_sessions::state.eq(SESSION_STATE_ENDED),
            agent_sessions::ended_at.eq(Some(now)),
        ))
        .execute(conn)?;

        Ok(row)
    })
}

/// The live (`active`) session for `terminal_id` + `agent_kind`, if any. At most one
/// exists (partial unique index). The resume flow reads this to decide whether a
/// terminal has a session worth resuming.
#[cfg_attr(not(test), allow(dead_code))]
pub fn active_session_for(
    conn: &mut SqliteConnection,
    terminal_id: &str,
    agent_kind: &str,
) -> QueryResult<Option<AgentSession>> {
    agent_sessions::table
        .filter(agent_sessions::terminal_id.eq(terminal_id))
        .filter(agent_sessions::agent_kind.eq(agent_kind))
        .filter(agent_sessions::state.eq(SESSION_STATE_ACTIVE))
        .select(AgentSession::as_select())
        .first(conn)
        .optional()
}

/// One ACTIVE agent session, reduced to what the sidebar provider-aware icon needs
/// (finding #55): which terminal hosts a live session and of which agent kind. The
/// front maps `agent_kind` through its provider registry to pick the logo to show in
/// place of the generic terminal glyph.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, serde::Serialize)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct ActiveAgentSession {
    pub terminal_id: String,
    pub agent_kind: String,
}

/// Every `active` agent session across ALL terminals, as `(terminal_id, agent_kind)`
/// pairs (finding #55). At most one active session exists per terminal+agent (partial
/// unique index), so the sidebar reads this once on mount and on the
/// `agent-sessions://changed` event to know which terminal rows should swap to the
/// agent's icon. Ordered by terminal for deterministic iteration/tests.
#[cfg_attr(not(test), allow(dead_code))]
pub fn active_agent_sessions(conn: &mut SqliteConnection) -> QueryResult<Vec<ActiveAgentSession>> {
    agent_sessions::table
        .filter(agent_sessions::state.eq(SESSION_STATE_ACTIVE))
        .order((
            agent_sessions::terminal_id.asc(),
            agent_sessions::agent_kind.asc(),
        ))
        .select((agent_sessions::terminal_id, agent_sessions::agent_kind))
        .load::<ActiveAgentSession>(conn)
}

/// All sessions of `terminal_id` (every state), newest started last
/// (`started_at` asc, `id` asc tiebreak). A terminal keeps many historical rows.
#[cfg_attr(not(test), allow(dead_code))]
pub fn sessions_for_terminal(
    conn: &mut SqliteConnection,
    terminal_id: &str,
) -> QueryResult<Vec<AgentSession>> {
    agent_sessions::table
        .filter(agent_sessions::terminal_id.eq(terminal_id))
        .order((agent_sessions::started_at.asc(), agent_sessions::id.asc()))
        .select(AgentSession::as_select())
        .load(conn)
}

/// All sessions ANCHORED to `workspace_id` (every state), across ALL their host
/// terminals — the read counterpart of [`delete_agent_sessions_for_workspace`]. Used
/// by the v2.2 workspace-runtime read model to surface the cross-terminal "Agents"
/// view of a workspace, INCLUDING a session anchored to the workspace on a loose
/// (unattached) host terminal. Oldest started first (`id` tiebreak).
#[cfg_attr(not(test), allow(dead_code))]
pub fn sessions_for_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<Vec<AgentSession>> {
    agent_sessions::table
        .filter(agent_sessions::workspace_id.eq(workspace_id))
        .order((agent_sessions::started_at.asc(), agent_sessions::id.asc()))
        .select(AgentSession::as_select())
        .load(conn)
}

/// Read back a single session by id.
#[cfg_attr(not(test), allow(dead_code))]
pub fn get_session(conn: &mut SqliteConnection, id: &str) -> QueryResult<Option<AgentSession>> {
    agent_sessions::table
        .find(id)
        .select(AgentSession::as_select())
        .first(conn)
        .optional()
}

/// Mark a session `ended` (a clean `SessionEnd`): set `state='ended'` and stamp
/// `ended_at = now`. The transition that vacates the partial-unique `active` slot so
/// a later SessionStart can take it. Returns rows updated (0 if the id is unknown).
#[cfg_attr(not(test), allow(dead_code))]
pub fn mark_session_ended(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    let now = now_millis();
    diesel::update(agent_sessions::table.find(id))
        .set((
            agent_sessions::state.eq(SESSION_STATE_ENDED),
            agent_sessions::ended_at.eq(Some(now)),
        ))
        .execute(conn)
}

/// Mark a BATCH of sessions `ended` by id (finding #82 boot-cleanup): set
/// `state='ended'` + `ended_at = now` for every id in `ids`. Used by the boot resume
/// scan to retire the sessions it decided NOT to resume (provably dead — claude was not
/// relaunched after the restart) so they stop warning on every subsequent close. A
/// single UPDATE … WHERE id IN (…); an empty slice is a no-op (returns 0). Returns the
/// number of rows updated.
#[cfg_attr(not(test), allow(dead_code))]
pub fn mark_sessions_ended(conn: &mut SqliteConnection, ids: &[String]) -> QueryResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let now = now_millis();
    diesel::update(agent_sessions::table.filter(agent_sessions::id.eq_any(ids)))
        .set((
            agent_sessions::state.eq(SESSION_STATE_ENDED),
            agent_sessions::ended_at.eq(Some(now)),
        ))
        .execute(conn)
}

/// Mark a session `resume_failed` (a resume was attempted but failed). Does NOT
/// stamp `ended_at` — the session did not end cleanly, it failed to resume. Returns
/// rows updated (0 if the id is unknown).
#[cfg_attr(not(test), allow(dead_code))]
pub fn mark_session_resume_failed(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    diesel::update(agent_sessions::table.find(id))
        .set(agent_sessions::state.eq(SESSION_STATE_RESUME_FAILED))
        .execute(conn)
}

/// Default staleness threshold (ms) for the active→unknown sweep: an `active`
/// session whose `last_seen_at` is older than this without a clean end is treated as
/// a probable kill (state unconfirmed). 30 minutes is generous — a live Claude
/// session refreshes `last_seen_at` on every SessionStart (incl. resume), so a row
/// only goes stale when the terminal/app was killed without a clean `SessionEnd`.
#[cfg_attr(not(test), allow(dead_code))]
pub const SESSION_STALE_AFTER_MS: i64 = 30 * 60 * 1000;

/// Sweep `active` sessions whose `last_seen_at` is older than `now - threshold_ms`
/// to `unknown` (probable kill, state unconfirmed). Idempotent: a row already
/// `unknown`/`ended`/`resume_failed` is untouched (the filter is `state='active'`),
/// and a still-fresh `active` row is left alone. An `unknown` row STAYS a resume
/// candidate (the resume flow reads active OR unknown); this only records the doubt.
/// Returns the number of rows flipped.
///
/// WIRING: currently has NO production caller. The v2.5.8 crash-UI revert removed the
/// boot crash-reconcile that used to run this on each boot/scan, so nothing in prod
/// flips a killed-then-relaunched `active` row to `unknown` today (a "crash" here is a
/// clean shutdown). Retained as the canonical active→unknown staleness authority (hence
/// the `#[cfg_attr(not(test), allow(dead_code))]`) and reached only by tests; re-wire it
/// into a periodic/boot scan if that reconcile is reinstated.
#[cfg_attr(not(test), allow(dead_code))]
pub fn sweep_stale_active_sessions(
    conn: &mut SqliteConnection,
    threshold_ms: i64,
) -> QueryResult<usize> {
    let cutoff = now_millis() - threshold_ms;
    diesel::update(
        agent_sessions::table
            .filter(agent_sessions::state.eq(SESSION_STATE_ACTIVE))
            .filter(agent_sessions::last_seen_at.lt(cutoff)),
    )
    .set(agent_sessions::state.eq(SESSION_STATE_UNKNOWN))
    .execute(conn)
}

/// Derive the PROJECT id of a session via its workspace anchor (PRD-2 derivation —
/// no denormalized `project_id`). `None` when the session is unattached
/// (`workspace_id IS NULL`) or its workspace was deleted. A single join, exactly as
/// "sessions by project" filtering is meant to work (ADR-0010).
#[cfg_attr(not(test), allow(dead_code))]
pub fn project_id_for_session(
    conn: &mut SqliteConnection,
    session_id: &str,
) -> QueryResult<Option<String>> {
    agent_sessions::table
        .inner_join(
            workspaces::table.on(workspaces::id.nullable().eq(agent_sessions::workspace_id)),
        )
        .filter(agent_sessions::id.eq(session_id))
        .select(workspaces::project_id)
        .first::<String>(conn)
        .optional()
}

/// One resume CANDIDATE row for the boot resume scan (PRD-5 #5): a terminal that is
/// still `alive`, has a resume-candidate session (`active` or `unknown`), and carries
/// its project's `resume_agent_sessions` flag (derived via workspace → project). The
/// bridge feeds these into the pure resume decision. `project_resume_on` is `false`
/// when the terminal has no workspace/project (loose terminal = OFF), thanks to the
/// LEFT join below.
#[derive(Debug, Clone, PartialEq, Eq, Queryable)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct ResumeCandidate {
    pub terminal_id: String,
    pub session_id: String,
    pub agent_kind: String,
    pub external_session_id: String,
    pub session_state: String,
    /// The agent's captured transcript path (`agent_sessions.transcript_path`), or
    /// `None` when the session has no transcript recorded. The bridge `stat`s this path
    /// (finding #53) to decide whether a real conversation exists before resuming — a
    /// candidate with a missing/absent transcript is skipped (`claude --resume` would
    /// otherwise fail "No conversation found").
    pub transcript_path: Option<String>,
    pub project_resume_on: bool,
}

/// Gather the resume CANDIDATES across all `alive` terminals (PRD-5 #5): every
/// `active`/`unknown` agent session whose terminal is still alive, joined to the
/// project's `resume_agent_sessions` flag (via the optional workspace → project — a
/// loose terminal yields `project_resume_on = false`, OFF by construction). The bridge
/// runs the pure resume decision per row; the OPTION gate and the `closed_voluntarily`
/// gate are NOT applied here (the latter is structural: a voluntarily-closed terminal
/// is `closed`, so it is excluded by the `alive` filter and never restored at all).
/// Newest started last for deterministic iteration.
#[cfg_attr(not(test), allow(dead_code))]
pub fn resume_candidates_on_boot(conn: &mut SqliteConnection) -> QueryResult<Vec<ResumeCandidate>> {
    use diesel::dsl::sql;
    use diesel::sql_types::Bool;
    agent_sessions::table
        .inner_join(terminals::table.on(terminals::id.eq(agent_sessions::terminal_id)))
        .left_join(workspaces::table.on(workspaces::id.nullable().eq(agent_sessions::workspace_id)))
        .left_join(
            projects::table.on(projects::id
                .nullable()
                .eq(workspaces::project_id.nullable())),
        )
        .filter(terminals::status.eq(STATUS_ALIVE))
        .filter(
            agent_sessions::state
                .eq(SESSION_STATE_ACTIVE)
                .or(agent_sessions::state.eq(SESSION_STATE_UNKNOWN)),
        )
        .order((agent_sessions::started_at.asc(), agent_sessions::id.asc()))
        .select((
            agent_sessions::terminal_id,
            agent_sessions::id,
            agent_sessions::agent_kind,
            agent_sessions::external_session_id,
            agent_sessions::state,
            agent_sessions::transcript_path,
            // COALESCE the optional project flag to 0 (false): a loose terminal (no
            // workspace/project) is OFF by construction.
            sql::<Bool>("COALESCE(projects.resume_agent_sessions, 0)"),
        ))
        .load::<ResumeCandidate>(conn)
}

/// Gather the resume CANDIDATES anchored to ONE workspace (v2.2 task
/// `01KWHPV33M4H5YSMGYNW9TG6J0`, workspace-scoped context restore): the SAME candidate shape
/// and gates as [`resume_candidates_on_boot`] — every `active`/`unknown` session on an `alive`
/// terminal, joined to the project's `resume_agent_sessions` flag — but FILTERED to sessions
/// whose `workspace_id` is `workspace_id`. This is the per-workspace slice the cockpit reads
/// to offer "resume these sessions" after a restart; it REUSES the existing resume policy
/// (`crate::agent_resume::decide_resume`) unchanged — no new policy. Newest started last.
#[cfg_attr(not(test), allow(dead_code))]
pub fn resume_candidates_for_workspace(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<Vec<ResumeCandidate>> {
    use diesel::dsl::sql;
    use diesel::sql_types::Bool;
    agent_sessions::table
        .inner_join(terminals::table.on(terminals::id.eq(agent_sessions::terminal_id)))
        .left_join(workspaces::table.on(workspaces::id.nullable().eq(agent_sessions::workspace_id)))
        .left_join(
            projects::table.on(projects::id
                .nullable()
                .eq(workspaces::project_id.nullable())),
        )
        .filter(terminals::status.eq(STATUS_ALIVE))
        .filter(agent_sessions::workspace_id.eq(workspace_id))
        .filter(
            agent_sessions::state
                .eq(SESSION_STATE_ACTIVE)
                .or(agent_sessions::state.eq(SESSION_STATE_UNKNOWN)),
        )
        .order((agent_sessions::started_at.asc(), agent_sessions::id.asc()))
        .select((
            agent_sessions::terminal_id,
            agent_sessions::id,
            agent_sessions::agent_kind,
            agent_sessions::external_session_id,
            agent_sessions::state,
            agent_sessions::transcript_path,
            sql::<Bool>("COALESCE(projects.resume_agent_sessions, 0)"),
        ))
        .load::<ResumeCandidate>(conn)
}

/// One CLOSE-WARNING candidate (PRD-5 #6): an alive terminal hosting a LIVE
/// (`active`/`unknown`) agent session, joined with its project's resume flag (loose
/// terminal → `false`) and the fields the warning message needs (agent kind, terminal
/// label/id, optional workspace name). The bridge applies the PURE gate
/// [`crate::agent_resume::should_warn_on_close`] to each row so the warn/resume policy
/// lives in ONE place (not duplicated in SQL).
#[derive(Debug, Clone, PartialEq, Eq, Queryable)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct CloseWarning {
    pub terminal_id: String,
    pub terminal_label: Option<String>,
    pub agent_kind: String,
    pub external_session_id: String,
    pub session_state: String,
    pub workspace_name: Option<String>,
    pub project_resume_on: bool,
}

/// Gather every alive terminal's LIVE (`active`/`unknown`) agent session with the data
/// the close-warning needs (PRD-5 #6), DEDUPLICATED to at most ONE entry per terminal
/// (finding #82). The actual warn/no-warn decision is the bridge's (it applies
/// [`crate::agent_resume::should_warn_on_close`] per row), so a resume-ON project's
/// sessions are RETURNED here but the bridge filters them out — keeping the policy
/// single-sourced in `agent_resume`.
///
/// DEDUP (finding #82): the close-warning dialog showed the SAME terminal twice when a
/// terminal carried more than one live (`active`/`unknown`) session (e.g. a stale
/// `unknown` row left by a kill, alongside a fresh `active`). We collapse to the newest
/// session per terminal (the `started_at asc, id asc` order means the LAST row seen per
/// terminal is the most recent). This is safe across the bridge's policy filter because
/// every row of a given terminal shares the same `project_resume_on` (it is derived from
/// the terminal's workspace→project), so all of a terminal's rows warn-or-not together —
/// keeping the most recent one never changes the warn verdict. The supersede-on-start +
/// boot-cleanup normally leave a single live row per terminal already; this dedup is the
/// belt-and-suspenders that guarantees no duplicate ever reaches the dialog.
#[cfg_attr(not(test), allow(dead_code))]
pub fn close_warning_candidates(conn: &mut SqliteConnection) -> QueryResult<Vec<CloseWarning>> {
    use diesel::dsl::sql;
    use diesel::sql_types::Bool;
    let rows = agent_sessions::table
        .inner_join(terminals::table.on(terminals::id.eq(agent_sessions::terminal_id)))
        .left_join(workspaces::table.on(workspaces::id.nullable().eq(agent_sessions::workspace_id)))
        .left_join(
            projects::table.on(projects::id
                .nullable()
                .eq(workspaces::project_id.nullable())),
        )
        .filter(terminals::status.eq(STATUS_ALIVE))
        .filter(
            agent_sessions::state
                .eq(SESSION_STATE_ACTIVE)
                .or(agent_sessions::state.eq(SESSION_STATE_UNKNOWN)),
        )
        .order((agent_sessions::started_at.asc(), agent_sessions::id.asc()))
        .select((
            agent_sessions::terminal_id,
            terminals::label.nullable(),
            agent_sessions::agent_kind,
            agent_sessions::external_session_id,
            agent_sessions::state,
            workspaces::name.nullable(),
            // COALESCE the optional project flag to false (loose terminal = OFF).
            sql::<Bool>("COALESCE(projects.resume_agent_sessions, 0)"),
        ))
        .load::<CloseWarning>(conn)?;

    // Collapse to ONE entry per terminal, keeping the most recent (the rows arrive in
    // `started_at asc, id asc` order, so a later insert into the map overwrites an older
    // one). Preserve first-seen terminal order for deterministic output.
    let mut order: Vec<String> = Vec::new();
    let mut latest: std::collections::HashMap<String, CloseWarning> =
        std::collections::HashMap::new();
    for row in rows {
        if !latest.contains_key(&row.terminal_id) {
            order.push(row.terminal_id.clone());
        }
        latest.insert(row.terminal_id.clone(), row);
    }
    Ok(order
        .into_iter()
        .map(|tid| latest.remove(&tid).expect("terminal id present"))
        .collect())
}

/// Open an in-memory SQLite database with migrations applied — used by tests so
/// they never touch the real `app_data_dir` DB.
#[cfg(test)]
pub fn open_in_memory() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("open in-memory sqlite");
    diesel::sql_query("PRAGMA foreign_keys = ON;")
        .execute(&mut conn)
        .expect("enable foreign_keys");
    run_migrations(&mut conn).expect("run migrations on in-memory db");
    conn
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `review_comments` model (v13) round-trips through the migrated DB: an anchored
    /// comment (every anchor column populated), a reply (auto-threaded by `parent_id`), and a
    /// global comment (all-NULL anchor) all persist and re-load, and `list_review_comments`
    /// filters by `(workspace, repository)` oldest-first. Proves the v13 migration + `schema.rs`
    /// `review_comments` table + the model columns are consistent (positional decoding of every
    /// nullable/keyword column).
    #[test]
    fn review_comments_round_trip_and_scope() {
        let mut conn = open_in_memory();

        let root = insert_review_comment(
            &mut conn,
            NewComment {
                workspace_id: "ws".into(),
                repository_id: "repo".into(),
                spec_id: Some("spec1".into()),
                author_type: AUTHOR_HUMAN.into(),
                author_name: "Kris".into(),
                anchor: Some(CommentAnchor {
                    file_path: "api/src/a.rs".into(),
                    side: SIDE_NEW.into(),
                    line_start: 10,
                    line_end: 12,
                    blob: Some("deadbeef".into()),
                    snippet: Some("let x = 1;".into()),
                    context: Some("ctx above\nlet x = 1;\nctx below".into()),
                }),
                body: "anchored root".into(),
                parent_id: None,
            },
        )
        .expect("insert anchored root");
        // Every anchor column round-trips (positional decoding sanity).
        assert_eq!(root.spec_id.as_deref(), Some("spec1"));
        assert_eq!(root.file_path.as_deref(), Some("api/src/a.rs"));
        assert_eq!(root.side.as_deref(), Some(SIDE_NEW));
        assert_eq!(root.line_start, Some(10));
        assert_eq!(root.line_end, Some(12));
        assert_eq!(root.blob.as_deref(), Some("deadbeef"));
        assert_eq!(root.snippet.as_deref(), Some("let x = 1;"));
        assert!(root.context.is_some());
        assert!(!root.outdated);
        assert_eq!(root.status, COMMENT_STATUS_OPEN);

        // A reply auto-threads (parent_id), no separate table.
        let reply = insert_review_comment(
            &mut conn,
            NewComment {
                workspace_id: "ws".into(),
                repository_id: "repo".into(),
                parent_id: Some(root.id.clone()),
                author_type: AUTHOR_AGENT.into(),
                author_name: "nyx-agent".into(),
                body: "agent reply".into(),
                ..Default::default()
            },
        )
        .expect("insert reply");
        assert_eq!(reply.parent_id.as_deref(), Some(root.id.as_str()));
        assert!(
            reply.file_path.is_none(),
            "a reply carries no anchor of its own"
        );

        // A GLOBAL comment (all-NULL anchor).
        insert_review_comment(
            &mut conn,
            NewComment {
                workspace_id: "ws".into(),
                repository_id: "repo".into(),
                author_type: AUTHOR_HUMAN.into(),
                author_name: "Kris".into(),
                body: "overall".into(),
                ..Default::default()
            },
        )
        .expect("insert global");

        // A comment of ANOTHER repo is excluded by the scope filter.
        insert_review_comment(
            &mut conn,
            NewComment {
                workspace_id: "ws".into(),
                repository_id: "other-repo".into(),
                author_type: AUTHOR_HUMAN.into(),
                author_name: "Kris".into(),
                body: "elsewhere".into(),
                ..Default::default()
            },
        )
        .expect("insert other repo");

        let listed = list_review_comments(&mut conn, "ws", "repo", None).expect("list");
        assert_eq!(
            listed.len(),
            3,
            "only this (workspace, repository)'s comments"
        );
        assert_eq!(listed[0].id, root.id, "oldest-first order");
        assert_eq!(listed[1].id, reply.id);

        // SPEC SCOPING (Finding 41): the anchored root was bound to `spec1`, and its reply INHERITS
        // that binding at insert time, so a `spec1`-scoped read returns the pair — the `spec1`
        // thread — while EXCLUDING the (unscoped) global comment. Proves comments no longer leak
        // across Specs sharing a `(workspace, repository)`.
        let spec1 = list_review_comments(&mut conn, "ws", "repo", Some("spec1")).expect("scoped");
        assert_eq!(spec1.len(), 2, "only spec1's root + its (inherited) reply");
        assert_eq!(spec1[0].id, root.id);
        assert_eq!(
            spec1[1].id, reply.id,
            "the reply inherited spec1 from its root"
        );
        assert_eq!(
            reply.spec_id.as_deref(),
            Some("spec1"),
            "reply inherits root spec"
        );
        // A DIFFERENT Spec sharing the same (workspace, repository) sees NONE of spec1's comments.
        let spec2 = list_review_comments(&mut conn, "ws", "repo", Some("spec2")).expect("scoped2");
        assert!(
            spec2.is_empty(),
            "spec2 sees no spec1 comment (no cross-spec leak)"
        );
    }

    /// A small helper: post a GLOBAL human comment in `(ws, repo)` and return it.
    #[cfg(test)]
    fn seed_comment(
        conn: &mut SqliteConnection,
        ws: &str,
        repo: &str,
        body: &str,
    ) -> ReviewComment {
        insert_review_comment(
            conn,
            NewComment {
                workspace_id: ws.into(),
                repository_id: repo.into(),
                author_type: AUTHOR_HUMAN.into(),
                author_name: "Kris".into(),
                body: body.into(),
                ..Default::default()
            },
        )
        .expect("seed comment")
    }

    /// The `review_tasks` GRILL flow (v14, `finding = task`): a comment PROMOTED becomes a
    /// `proposed` task linked to its source comment, the AGENT structures it (→ `structured`),
    /// and the HUMAN validates it (→ `validated`, ready for the coder). Severity is OPTIONAL, and
    /// `list_review_tasks` scopes to the `(workspace, repository)`.
    #[test]
    fn review_tasks_grill_flow_and_severity_optional() {
        let mut conn = open_in_memory();
        let comment = seed_comment(&mut conn, "ws", "repo", "this login path looks fragile");

        // PROMOTE — the human lâche brut: a `proposed` task, seeded title, NO severity, linked
        // to the source comment. `finding = task`: this row IS the coder task.
        let task = promote_comment_to_task(&mut conn, &comment, "fragile login path", None)
            .expect("promote");
        assert_eq!(task.grill_state, GRILL_PROPOSED);
        assert_eq!(task.from_comment_id, comment.id, "linked to its comment");
        assert_eq!(task.workspace_id, "ws");
        assert_eq!(task.repository_id, "repo");
        assert_eq!(
            task.severity, None,
            "severity is OPTIONAL — unset at promotion"
        );
        assert!(task.details.is_none());
        assert!(
            !task.is_ready_for_coder(),
            "a proposed task is NOT ready for the coder"
        );

        // STRUCTURE — the agent gives it a clean title/details + an optional severity.
        let structured = structure_task(
            &mut conn,
            &task.id,
            "Harden the login path against empty credentials",
            Some("Add validation + a test for the empty-password branch."),
            Some(Some("high")),
        )
        .expect("structure ok")
        .expect("task present");
        assert_eq!(structured.grill_state, GRILL_STRUCTURED);
        assert_eq!(structured.severity.as_deref(), Some("high"));
        assert!(structured.details.is_some());
        assert!(
            !structured.is_ready_for_coder(),
            "a structured-but-unvalidated task is NOT ready for the coder"
        );

        // VALIDATE — the human validates; ONLY now is it ready for the coder.
        let validated = validate_task(&mut conn, &task.id)
            .expect("validate ok")
            .expect("task present");
        assert_eq!(validated.grill_state, GRILL_VALIDATED);
        assert!(
            validated.is_ready_for_coder(),
            "a validated task IS ready for the coder"
        );

        // A task of ANOTHER repo is excluded by the scope filter.
        let other_comment = seed_comment(&mut conn, "ws", "other", "elsewhere");
        promote_comment_to_task(&mut conn, &other_comment, "elsewhere task", None)
            .expect("promote other");
        let listed = list_review_tasks(&mut conn, "ws", "repo", None).expect("list");
        assert_eq!(listed.len(), 1, "only this (workspace, repository)'s tasks");
        assert_eq!(listed[0].id, task.id);
    }

    /// Finding #53 — a promoted task is SPEC-SCOPED (the tasks counterpart of the comment fix #41).
    /// A task inherits its source comment's `spec_id` at promotion, and `list_review_tasks(spec)`
    /// returns ONLY that Spec's tasks: a task promoted on Spec A never leaks onto Spec B sharing the
    /// SAME `(workspace, repository)`. An un-scoped promotion stays in the `None` bucket.
    #[test]
    fn review_tasks_are_spec_scoped_no_cross_spec_leak() {
        let mut conn = open_in_memory();
        // TWO comments on the SAME (workspace, repo), each bound to a DIFFERENT Spec, plus one
        // un-scoped comment (no Spec).
        let seed_scoped = |conn: &mut SqliteConnection, spec: Option<&str>, body: &str| {
            insert_review_comment(
                conn,
                NewComment {
                    workspace_id: "ws".into(),
                    repository_id: "repo".into(),
                    spec_id: spec.map(|s| s.to_string()),
                    author_type: AUTHOR_HUMAN.into(),
                    author_name: "Kris".into(),
                    body: body.into(),
                    ..Default::default()
                },
            )
            .expect("seed scoped comment")
        };
        let comment_a = seed_scoped(&mut conn, Some("spec-A"), "A: fragile");
        let comment_b = seed_scoped(&mut conn, Some("spec-B"), "B: also fragile");
        let comment_none = seed_scoped(&mut conn, None, "generic");

        let task_a =
            promote_comment_to_task(&mut conn, &comment_a, "fix A", None).expect("promote A");
        let task_b =
            promote_comment_to_task(&mut conn, &comment_b, "fix B", None).expect("promote B");
        let task_none = promote_comment_to_task(&mut conn, &comment_none, "fix generic", None)
            .expect("promote none");

        // The task INHERITS its source comment's Spec.
        assert_eq!(task_a.spec_id.as_deref(), Some("spec-A"));
        assert_eq!(task_b.spec_id.as_deref(), Some("spec-B"));
        assert_eq!(
            task_none.spec_id, None,
            "an un-scoped promotion stays un-scoped"
        );

        // Spec A's list shows ONLY task A — task B never leaks onto it (the #53 fix).
        let list_a = list_review_tasks(&mut conn, "ws", "repo", Some("spec-A")).expect("list A");
        assert_eq!(list_a.len(), 1, "Spec A sees only its own task");
        assert_eq!(list_a[0].id, task_a.id);
        assert!(
            !list_a.iter().any(|t| t.id == task_b.id),
            "Spec B's task must NOT leak onto Spec A"
        );

        // Spec B's list shows ONLY task B.
        let list_b = list_review_tasks(&mut conn, "ws", "repo", Some("spec-B")).expect("list B");
        assert_eq!(list_b.len(), 1, "Spec B sees only its own task");
        assert_eq!(list_b[0].id, task_b.id);

        // The UNSCOPED read (the MCP agent façade) still sees the whole repo's tasks.
        let all = list_review_tasks(&mut conn, "ws", "repo", None).expect("list all");
        assert_eq!(
            all.len(),
            3,
            "the unscoped read sees every task of the repo"
        );
    }

    /// The GRILL GUARD-RAIL: the agent's `structure` cannot touch a VALIDATED task (it cannot
    /// silently reopen the human's committed decision), and there is no `structure`-to-`validated`
    /// path — validation is human-only (`validate_task`, reachable only from the UI façade).
    #[test]
    fn structure_cannot_reopen_a_validated_task() {
        let mut conn = open_in_memory();
        let comment = seed_comment(&mut conn, "ws", "repo", "x");
        let task = promote_comment_to_task(&mut conn, &comment, "t", None).expect("promote");
        structure_task(&mut conn, &task.id, "structured", None, None)
            .expect("structure")
            .expect("present");
        validate_task(&mut conn, &task.id)
            .expect("validate")
            .expect("present");

        // The agent tries to re-structure the VALIDATED task → refused (no-op, None).
        let reopened = structure_task(&mut conn, &task.id, "sneaky rewrite", None, None)
            .expect("structure call ok");
        assert!(
            reopened.is_none(),
            "structure must NOT reopen a validated task"
        );
        // The stored state is untouched — still validated with the human's title.
        let stored = get_review_task(&mut conn, &task.id).unwrap().unwrap();
        assert_eq!(stored.grill_state, GRILL_VALIDATED);
        assert_eq!(stored.title, "structured");
    }

    /// The UNREAD watermark (v14): a fresh repo counts every comment as unread; a new
    /// comment/reply raises the count; `mark_review_read` zeroes it; a comment posted AFTER a
    /// mark-read is unread again. Reuses the runtime `unread` pattern as a per-repo read watermark.
    #[test]
    fn review_unread_counts_and_mark_read_resets() {
        let mut conn = open_in_memory();

        // Never-read: no comments → 0 unread, watermark 0.
        let fresh = review_unread(&mut conn, "ws", "repo").expect("unread");
        assert_eq!(fresh.unread, 0);
        assert_eq!(fresh.last_read_at, 0);

        // Two comments land — both unread (never read).
        seed_comment(&mut conn, "ws", "repo", "one");
        seed_comment(&mut conn, "ws", "repo", "two");
        assert_eq!(review_unread(&mut conn, "ws", "repo").unwrap().unread, 2);

        // Another repo's comment does NOT count here (scoped).
        seed_comment(&mut conn, "ws", "other", "elsewhere");
        assert_eq!(review_unread(&mut conn, "ws", "repo").unwrap().unread, 2);

        // Mark-read → 0, watermark stamped.
        let read = mark_review_read(&mut conn, "ws", "repo").expect("mark read");
        assert_eq!(read.unread, 0);
        assert!(read.last_read_at > 0, "watermark stamped to now");
        assert_eq!(review_unread(&mut conn, "ws", "repo").unwrap().unread, 0);

        // A comment created AFTER the mark-read is unread again (its created_at > watermark). The
        // in-memory clock can share a millisecond with the watermark; force a strictly-later
        // created_at so the STRICTLY-AFTER semantics are exercised deterministically.
        let later = read.last_read_at + 1000;
        diesel::insert_into(review_comments::table)
            .values(NewReviewCommentRow {
                id: Uuid::now_v7().to_string(),
                workspace_id: "ws".into(),
                repository_id: "repo".into(),
                spec_id: None,
                parent_id: None,
                author_type: AUTHOR_AGENT.into(),
                author_name: "nyx-agent".into(),
                file_path: None,
                side: None,
                line_start: None,
                line_end: None,
                blob: None,
                snippet: None,
                context: None,
                outdated: false,
                body: "agent replies later".into(),
                status: COMMENT_STATUS_OPEN.into(),
                resolved_by_type: None,
                created_at: later,
            })
            .execute(&mut conn)
            .expect("insert later comment");
        assert_eq!(
            review_unread(&mut conn, "ws", "repo").unwrap().unread,
            1,
            "a comment created after mark-read is unread again"
        );
    }

    /// A unique, file-backed `Db` in the OS temp dir (so WAL — which has no effect on
    /// `:memory:` — is observable on a real journal). Returns the `Db` and the path;
    /// the caller may inspect the on-disk pragmas. No `tempfile` dev-dep: uniqueness
    /// comes from a UUIDv7 suffix (same convention as the git-branch test helper).
    fn file_backed_db() -> (Db, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("nyx-db-pool-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("create temp db dir");
        let path = dir.join("nyx.db");
        let db = Db::open(&path).expect("open file-backed db");
        (db, path)
    }

    /// DONE-CRITERION (task #2): `foreign_keys = ON` and `journal_mode = WAL` are
    /// verified on EVERY new connection the pool opens — not just the first. We force
    /// the pool to hand out several DISTINCT connections concurrently (holding each
    /// open so the pool must open a fresh one for the next), then assert each reports
    /// `foreign_keys = 1` and `journal_mode = wal`. This proves the `PragmaCustomizer`
    /// runs per `on_acquire`, the exact guarantee a single-open `PRAGMA` would miss.
    #[test]
    fn wal_and_foreign_keys_on_every_pooled_connection() {
        let (db, _path) = file_backed_db();

        // Check out 4 connections at once so the pool must create 4 separate ones;
        // assert the pragmas on each independently.
        let mut conns: Vec<PooledConnection<ConnectionManager<SqliteConnection>>> = Vec::new();
        for _ in 0..4 {
            let mut c = db.pool.get().expect("checkout pooled connection");
            let fk: i32 = diesel::sql_query("PRAGMA foreign_keys;")
                .get_result::<PragmaInt>(&mut c)
                .expect("read foreign_keys")
                .value;
            assert_eq!(fk, 1, "foreign_keys must be ON for every pooled connection");
            let journal: String = diesel::sql_query("PRAGMA journal_mode;")
                .get_result::<PragmaText>(&mut c)
                .expect("read journal_mode")
                .value;
            assert_eq!(
                journal.to_lowercase(),
                "wal",
                "journal_mode must be WAL for every pooled connection"
            );
            conns.push(c);
        }
        // Hold all 4 simultaneously to prove they are genuinely independent
        // connections (not the same one re-handed), then drop them back.
        assert_eq!(conns.len(), 4);
    }

    /// DONE-CRITERION (task #2): "Plusieurs lectures peuvent progresser concurremment".
    /// Two reader threads each check out a connection from the SAME pool and hold it
    /// while the other also holds one — only possible if the pool yields >1 connection
    /// concurrently (a single `Mutex<SqliteConnection>` could not). A barrier proves
    /// BOTH are checked out at the same instant.
    #[test]
    fn concurrent_reads_progress_in_parallel() {
        let (db, _path) = file_backed_db();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let db = db.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut conn = db.pool.get().expect("checkout for concurrent read");
                // Each reader runs a real query, then waits at the barrier WHILE still
                // holding its connection — both connections are live simultaneously.
                let _ = list_terminals(&mut conn).expect("read terminals");
                barrier.wait();
            }));
        }
        for h in handles {
            h.join().expect("reader thread joined");
        }
    }

    /// DONE-CRITERION (task #2): "les ecritures respectent WAL/busy timeout". With WAL
    /// + a busy timeout, an INSERT from a second connection succeeds even while a first
    /// connection holds an open read snapshot — the write does not error with
    /// `SQLITE_BUSY`; it lands under WAL's concurrent-reader model. This is the
    /// observable proof the busy_timeout + WAL pragmas are in force on pooled writers.
    #[test]
    fn writes_respect_wal_and_busy_timeout_under_a_concurrent_reader() {
        let (db, _path) = file_backed_db();
        // Connection A: open a read that holds a snapshot.
        let mut reader = db.pool.get().expect("checkout reader");
        let _rows = list_terminals(&mut reader).expect("reader snapshot");
        // Connection B: write while the reader is still checked out. Under WAL this
        // commits without blocking the reader; the busy_timeout absorbs any transient
        // lock contention rather than returning SQLITE_BUSY immediately.
        let id = {
            let mut writer = db.pool.get().expect("checkout writer");
            create_terminal(&mut writer, "/tmp/wal-write", Some("w".into()))
                .expect("write under concurrent reader")
                .id
        };
        // The reader's later checkout sees the committed row (fresh snapshot).
        drop(reader);
        let mut verify = db.pool.get().expect("checkout verify");
        let found = get_terminal(&mut verify, &id).expect("get").is_some();
        assert!(
            found,
            "the write committed and is visible to a new snapshot"
        );
    }

    /// Helper row types for reading scalar `PRAGMA` results through Diesel's
    /// `sql_query` (which needs a `QueryableByName` target).
    #[derive(diesel::QueryableByName)]
    struct PragmaInt {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        #[diesel(column_name = "foreign_keys")]
        value: i32,
    }
    #[derive(diesel::QueryableByName)]
    struct PragmaText {
        #[diesel(sql_type = diesel::sql_types::Text)]
        #[diesel(column_name = "journal_mode")]
        value: String,
    }

    /// The migration applies and creates the `terminals` table: we can insert a
    /// row and read it back (the foundational round-trip of done-criterion 1+2).
    #[test]
    fn migration_creates_terminals_and_roundtrips() {
        let mut conn = open_in_memory();

        let inserted: Terminal = diesel::insert_into(terminals::table)
            .values(NewTerminal::alive(
                "/tmp/work",
                Some("shell".to_string()),
                0,
            ))
            .returning(Terminal::as_returning())
            .get_result(&mut conn)
            .expect("insert terminal");

        let fetched: Terminal = terminals::table
            .find(inserted.id.as_str())
            .select(Terminal::as_select())
            .first(&mut conn)
            .expect("select terminal back");

        assert_eq!(fetched, inserted, "select must return the inserted row");
        assert_eq!(fetched.cwd, "/tmp/work");
        assert_eq!(fetched.label.as_deref(), Some("shell"));
        assert_eq!(fetched.status, STATUS_ALIVE);
        assert_eq!(fetched.order_index, 0);
        assert!(
            fetched.created_at > 0,
            "created_at must be a positive epoch-ms timestamp"
        );
        assert_eq!(
            fetched.updated_at, fetched.created_at,
            "a fresh row's updated_at equals its created_at"
        );
        assert_eq!(fetched.closed_at, None, "a fresh row is not closed");
    }

    /// schema.rs ↔ migration consistency: exercise EVERY column declared in
    /// `schema.rs` through a real insert+select against the migrated DB. If a
    /// column name/type drifted from the SQL (e.g. `"order"` not mapped to
    /// `order_index`, or a type mismatch), Diesel's typed query would fail to
    /// compile or to run here. `check_for_backend` on the model adds a
    /// compile-time column-type check against the SQLite backend.
    #[test]
    fn schema_matches_migration() {
        let mut conn = open_in_memory();

        // Two rows with distinct values in every nullable/keyword column so a
        // mismatch surfaces as wrong data, not just a missing column.
        let a: Terminal = diesel::insert_into(terminals::table)
            .values(NewTerminal::alive("/a", None, 5))
            .returning(Terminal::as_returning())
            .get_result(&mut conn)
            .expect("insert a");
        let b: Terminal = diesel::insert_into(terminals::table)
            .values(NewTerminal::alive("/b", Some("named".to_string()), 2))
            .returning(Terminal::as_returning())
            .get_result(&mut conn)
            .expect("insert b");

        // Order by the keyword column to prove `order_index` maps to `"order"`.
        let rows: Vec<Terminal> = terminals::table
            .order(terminals::order_index.asc())
            .select(Terminal::as_select())
            .load(&mut conn)
            .expect("load ordered");

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, b.id, "order_index asc puts order=2 first");
        assert_eq!(rows[1].id, a.id, "order_index asc puts order=5 last");
        assert_eq!(rows[0].label.as_deref(), Some("named"));
        assert_eq!(rows[1].label, None);
    }

    /// The `domain_events` model (v12) round-trips through the migrated DB, `sequence`
    /// is strictly increasing (the monotonic global ordering key), and `list_domain_events`
    /// returns them in emission order. Proves the v12 migration + `schema.rs` `domain_events`
    /// table + the model columns are consistent (the outbox's persistence half).
    #[test]
    fn domain_events_round_trip_and_monotonic_sequence() {
        let mut conn = open_in_memory();

        let a = record_domain_event(
            &mut conn,
            NewDomainEvent {
                project_id: "p1".into(),
                workspace_id: Some("w1".into()),
                aggregate: "workspace".into(),
                event_type: "workspace.created".into(),
                source: "ui".into(),
                actor: None,
                payload: "{\"identity_id\":\"w1\"}".into(),
                created_at: 0,
            },
        )
        .expect("record a");
        let b = record_domain_event(
            &mut conn,
            NewDomainEvent {
                project_id: "p1".into(),
                workspace_id: None,
                aggregate: "project".into(),
                event_type: "project.archived".into(),
                source: "mcp".into(),
                actor: None,
                payload: "{\"identity_id\":\"p1\"}".into(),
                created_at: 0,
            },
        )
        .expect("record b");

        // Strictly increasing sequence (the monotonic global order).
        assert!(b.sequence > a.sequence, "sequence is monotonic per insert");
        // created_at was stamped (0 → now).
        assert!(a.created_at > 0 && b.created_at > 0, "created_at stamped");
        // Nullable workspace_id round-trips both ways.
        assert_eq!(a.workspace_id.as_deref(), Some("w1"));
        assert_eq!(b.workspace_id, None);

        let all = list_domain_events(&mut conn).expect("list");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].sequence, a.sequence, "listed in sequence order");
        assert_eq!(all[1].sequence, b.sequence);
        assert_eq!(all[0].event_type, "workspace.created");
        assert_eq!(all[1].source, "mcp");
    }

    /// `recent_domain_events_for_workspace` (the v2.2 timeline query, task
    /// `01KWHPV33SAV9NV1STSY403CF0`) returns the workspace-scoped events PLUS its project's
    /// workspace-less events, newest-first, capped at `limit`. A row of ANOTHER project (even
    /// workspace-less) is excluded, and a row of ANOTHER workspace in the SAME project is
    /// excluded. The cap keeps the RECENT tail.
    #[test]
    fn recent_domain_events_for_workspace_scopes_and_bounds() {
        let mut conn = open_in_memory();
        let ev = |project: &str, workspace: Option<&str>, ty: &str| NewDomainEvent {
            project_id: project.into(),
            workspace_id: workspace.map(str::to_string),
            aggregate: "workspace".into(),
            event_type: ty.into(),
            source: "ui".into(),
            actor: None,
            payload: "{\"identity_id\":\"x\"}".into(),
            created_at: 0,
        };
        // p1: a project-scoped event, a w1-scoped event, a w2-scoped event.
        record_domain_event(&mut conn, ev("p1", None, "project.created")).unwrap();
        record_domain_event(&mut conn, ev("p1", Some("w1"), "workspace.created")).unwrap();
        record_domain_event(&mut conn, ev("p1", Some("w2"), "workspace.created")).unwrap();
        // p2: a project-scoped event of a DIFFERENT project (must not leak into w1's timeline).
        record_domain_event(&mut conn, ev("p2", None, "project.archived")).unwrap();
        // p1: a second w1-scoped event (the newest of w1's own events).
        record_domain_event(&mut conn, ev("p1", Some("w1"), "workspace.renamed")).unwrap();

        // The full (uncapped) correlated set for w1 of p1: its two workspace events + the ONE
        // p1 project-scoped event = 3 rows; p2's project event and w2's event are excluded.
        let got = recent_domain_events_for_workspace(&mut conn, "w1", "p1", 100).unwrap();
        assert_eq!(
            got.len(),
            3,
            "w1 events + p1 project-scoped event; no p2, no w2"
        );
        assert!(got.iter().all(|e| e.project_id == "p1"));
        assert!(got.iter().all(|e| e.workspace_id.as_deref() != Some("w2")));
        // Newest-first ordering.
        assert!(
            got.windows(2).all(|w| w[0].sequence > w[1].sequence),
            "newest-first"
        );

        // Bounded: cap at 2 keeps the two NEWEST correlated rows.
        let capped = recent_domain_events_for_workspace(&mut conn, "w1", "p1", 2).unwrap();
        assert_eq!(capped.len(), 2, "bounded to the cap");
        assert_eq!(
            capped[0].event_type, "workspace.renamed",
            "the newest correlated event is retained"
        );
    }

    // --- Spec domain: model + revisions + fork (v2.4 Phase 1, v13) --------

    /// DONE-CRITERION (task 01KX1DH4QC04C1FKHZT5ZF4BMS): "l'entité Spec + révisions
    /// existe dans nyx-core avec les sections structurées ... persiste dans le SQLite
    /// unique." schema.rs ↔ migration v13 consistency: a Spec created with structured
    /// sections round-trips through the migrated DB — every `specs` and every
    /// `spec_revisions` column (including the six SECTIONS) is read back, proving the
    /// migration + schema.rs + models are consistent. A drift in name/type/order would
    /// fail to compile (`check_for_backend`) or to run here.
    #[test]
    fn spec_and_revision_schema_matches_migration_with_structured_sections() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();

        let sections = SpecSections {
            problem: "the problem".into(),
            solution: "the solution".into(),
            implementation_decisions: "impl decisions".into(),
            testing_decisions: "testing decisions".into(),
            user_stories: "as a user…".into(),
            out_of_scope: "not this".into(),
        };
        let (spec, rev) = create_spec(&mut conn, &project.id, "My Spec", sections.clone()).unwrap();

        // Spec identity defaults (stored, not yet lifecycle-driven).
        assert_eq!(spec.project_id, project.id);
        assert_eq!(
            spec.workspace_id, None,
            "workspace_id is the v2.1 seam, NULL now"
        );
        assert_eq!(spec.title, "My Spec");
        assert_eq!(spec.number, 1, "first Spec of the project is number 1");
        assert_eq!(spec.status, SPEC_STATUS_DRAFT, "a fresh Spec is a draft");
        assert_eq!(spec.priority, SPEC_PRIORITY_MED);
        assert_eq!(spec.mode, SPEC_MODE_STEPPED);
        assert_eq!(spec.phases, "[]", "no phases by default");
        assert_eq!(spec.current_revision_number, 1);
        assert_eq!(spec.completed_at, None);
        assert_eq!(spec.archived_at, None);
        assert!(spec.created_at > 0 && spec.updated_at == spec.created_at);

        // First revision: active, number 1, with the STRUCTURED sections persisted.
        assert_eq!(rev.spec_id, spec.id);
        assert_eq!(rev.revision_number, 1);
        assert_eq!(rev.revision_status, REVISION_STATUS_ACTIVE);
        assert_eq!(rev.title, "My Spec");
        assert_eq!(rev.problem, "the problem");
        assert_eq!(rev.solution, "the solution");
        assert_eq!(rev.implementation_decisions, "impl decisions");
        assert_eq!(rev.testing_decisions, "testing decisions");
        assert_eq!(rev.user_stories, "as a user…");
        assert_eq!(rev.out_of_scope, "not this");
        assert_eq!(rev.change_reason, None);

        // PERSISTED and RE-READ from the single SQLite file: get_spec + get_active_revision
        // return exactly what was written (the round-trip that proves persistence).
        let got_spec = get_spec(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(
            got_spec, spec,
            "the Spec is persisted and re-read identically"
        );
        let got_rev = get_active_revision(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(
            got_rev, rev,
            "the active revision is persisted and re-read identically"
        );
    }

    /// The per-project Spec `number` increments (`MAX+1`) and is UNIQUE within the
    /// project — the human-key axis (`projects.key || '-' || number`, PDR-0014). Two
    /// Specs in the same project get 1 then 2.
    #[test]
    fn spec_number_increments_per_project() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (a, _) = create_spec(&mut conn, &project.id, "A", SpecSections::empty()).unwrap();
        let (b, _) = create_spec(&mut conn, &project.id, "B", SpecSections::empty()).unwrap();
        assert_eq!(a.number, 1);
        assert_eq!(b.number, 2, "the second Spec of the project is number 2");
    }

    /// Editing a structured section of the ACTIVE revision persists in place (authoring,
    /// NOT a fork) and bumps the Spec's `updated_at`; the other sections are untouched and
    /// the revision number does not change.
    #[test]
    fn update_active_revision_section_persists_in_place() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "Spec", SpecSections::empty()).unwrap();

        let ok = update_active_revision_section(
            &mut conn,
            &spec.id,
            SpecSection::Solution,
            "a designed solution",
        )
        .unwrap();
        assert!(ok, "the active revision was updated");

        let rev = get_active_revision(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(
            rev.solution, "a designed solution",
            "the edited section persisted"
        );
        assert_eq!(rev.problem, "", "other sections are untouched");
        assert_eq!(rev.revision_number, 1, "an in-place edit is NOT a fork");

        // Editing an unknown Spec is a no-op (false), not an error.
        assert!(!update_active_revision_section(
            &mut conn,
            "no-such-spec",
            SpecSection::Problem,
            "x"
        )
        .unwrap());
    }

    /// DONE-CRITERION (task 01KX1DH4QC04C1FKHZT5ZF4BMS): "fork crée une nouvelle
    /// révision et supersède l'ancienne." A FORK creates revision N+1 (`active`),
    /// snapshots the base sections, records the change_reason, points the Spec at it, and
    /// marks the previous revision `superseded` — exactly ONE active revision remains.
    #[test]
    fn fork_creates_new_revision_and_supersedes_the_previous() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, rev1) = create_spec(
            &mut conn,
            &project.id,
            "Spec",
            SpecSections {
                problem: "P1".into(),
                solution: "S1".into(),
                ..SpecSections::empty()
            },
        )
        .unwrap();

        let rev2 = fork_spec(&mut conn, &spec.id, Some("scope grew")).unwrap();

        // The new revision is number 2, active, snapshotting the base sections + title.
        assert_eq!(rev2.revision_number, 2, "the fork is revision 2");
        assert_eq!(rev2.revision_status, REVISION_STATUS_ACTIVE);
        assert_eq!(rev2.problem, "P1", "the fork snapshots the base sections");
        assert_eq!(rev2.solution, "S1");
        assert_eq!(rev2.title, "Spec");
        assert_eq!(rev2.change_reason.as_deref(), Some("scope grew"));

        // The PREVIOUS revision is now superseded (re-read from the DB).
        let all = list_revisions(&mut conn, &spec.id).unwrap();
        assert_eq!(all.len(), 2, "both revisions are retained (history)");
        let old = all.iter().find(|r| r.id == rev1.id).unwrap();
        assert_eq!(
            old.revision_status, REVISION_STATUS_SUPERSEDED,
            "the previous revision is superseded by the fork"
        );

        // Exactly ONE active revision, and the Spec points at it.
        let active = get_active_revision(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(active.id, rev2.id, "the active revision is the fork");
        let got_spec = get_spec(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(
            got_spec.current_revision_number, 2,
            "the Spec points at the new active revision"
        );

        // A SECOND fork chains: revision 3 active, revision 2 superseded.
        let rev3 = fork_spec(&mut conn, &spec.id, None).unwrap();
        assert_eq!(rev3.revision_number, 3);
        let active_count = list_revisions(&mut conn, &spec.id)
            .unwrap()
            .into_iter()
            .filter(|r| r.revision_status == REVISION_STATUS_ACTIVE)
            .count();
        assert_eq!(
            active_count, 1,
            "still exactly one active revision after a second fork"
        );
    }

    /// A FORK edits are independent of the superseded revision: editing the active
    /// (forked) revision's section does NOT mutate the superseded one — the history is
    /// immutable.
    #[test]
    fn editing_forked_revision_does_not_touch_superseded_history() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, rev1) = create_spec(
            &mut conn,
            &project.id,
            "Spec",
            SpecSections {
                solution: "original".into(),
                ..SpecSections::empty()
            },
        )
        .unwrap();
        fork_spec(&mut conn, &spec.id, None).unwrap();
        update_active_revision_section(&mut conn, &spec.id, SpecSection::Solution, "revised")
            .unwrap();

        let all = list_revisions(&mut conn, &spec.id).unwrap();
        let old = all.iter().find(|r| r.id == rev1.id).unwrap();
        assert_eq!(
            old.solution, "original",
            "the superseded revision is immutable history"
        );
        let active = get_active_revision(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(
            active.solution, "revised",
            "the active revision carries the edit"
        );
    }

    /// The `specs.status` CHECK rejects anything outside the canonical vocabulary, and
    /// `spec_revisions.revision_status` rejects anything but active|superseded — proving
    /// the migration constraints reached the DB.
    #[test]
    fn spec_check_constraints_enforced() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "S", SpecSections::empty()).unwrap();

        let bad_status = diesel::update(specs::table.find(&spec.id))
            .set(specs::status.eq("bogus"))
            .execute(&mut conn);
        assert!(
            bad_status.is_err(),
            "specs.status CHECK rejects unknown states"
        );

        let bad_priority = diesel::update(specs::table.find(&spec.id))
            .set(specs::priority.eq("wat"))
            .execute(&mut conn);
        assert!(
            bad_priority.is_err(),
            "specs.priority CHECK rejects unknown priorities"
        );

        let bad_rev = diesel::insert_into(spec_revisions::table)
            .values((
                spec_revisions::id.eq(Uuid::now_v7().to_string()),
                spec_revisions::spec_id.eq(&spec.id),
                spec_revisions::revision_number.eq(99),
                spec_revisions::revision_status.eq("weird"),
                spec_revisions::title.eq("t"),
            ))
            .execute(&mut conn);
        assert!(
            bad_rev.is_err(),
            "revision_status CHECK rejects unknown values"
        );
    }

    /// A partial unique index guarantees at most ONE `active` revision per Spec: a raw
    /// insert of a SECOND active revision for the same Spec is rejected.
    #[test]
    fn at_most_one_active_revision_per_spec_enforced() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "S", SpecSections::empty()).unwrap();

        let dup = diesel::insert_into(spec_revisions::table)
            .values(NewSpecRevision {
                id: Uuid::now_v7().to_string(),
                spec_id: spec.id.clone(),
                revision_number: 2,
                revision_status: REVISION_STATUS_ACTIVE.to_string(),
                title: "dup".to_string(),
                problem: String::new(),
                solution: String::new(),
                implementation_decisions: String::new(),
                testing_decisions: String::new(),
                user_stories: String::new(),
                out_of_scope: String::new(),
                change_reason: None,
                created_at: now_millis(),
            })
            .execute(&mut conn);
        assert!(
            dup.is_err(),
            "a second active revision for the same Spec must be rejected"
        );
    }

    /// Deleting a project CASCADES to its Specs and their revisions (the FK
    /// `specs.project_id REFERENCES projects(id) ON DELETE CASCADE` +
    /// `spec_revisions.spec_id ... ON DELETE CASCADE`), requiring `PRAGMA foreign_keys
    /// = ON` (enabled in `open_in_memory`).
    #[test]
    fn deleting_project_cascades_to_specs_and_revisions() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "S", SpecSections::empty()).unwrap();
        fork_spec(&mut conn, &spec.id, None).unwrap();

        delete_project(&mut conn, &project.id).unwrap();
        assert!(
            get_spec(&mut conn, &spec.id).unwrap().is_none(),
            "the Spec cascaded away"
        );
        assert!(
            list_revisions(&mut conn, &spec.id).unwrap().is_empty(),
            "the Spec's revisions cascaded away"
        );
    }

    /// v13 migration reverts cleanly (`down.sql` drops `spec_revisions` then `specs`).
    /// Since later phases landed v14 (tasks) then v15 (the `specs.workspace_id` index) ON
    /// TOP of v13, we peel them back to front — v15, then v14, then v13 — so this still
    /// genuinely exercises the Spec down-migration.
    #[test]
    fn spec_domain_migration_reverts() {
        let mut conn = open_in_memory();
        // Peel v15 (specs.workspace_id index), v14 (tasks/task_dependencies), then v13
        // (specs/spec_revisions); all must succeed. Each revert drops the current last link.
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v18 (review_tasks.spec_id) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v17 (review_tasks/review_reads) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v16 (review_comments) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("v15 (specs.workspace_id index) reverts cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("v14 (tasks/task_dependencies) reverts cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("v13 (specs/spec_revisions) reverts cleanly");
    }

    // --- Spec LIFECYCLE: guards + validations + guard-rails (v2.4 Phase 1) --

    /// Sections for a COMPLETE draft (problem + solution present) so it can pass the
    /// pre-ready validations. Keeps the lifecycle tests terse.
    fn complete_sections() -> SpecSections {
        SpecSections {
            problem: "a real problem".into(),
            solution: "a real solution".into(),
            ..SpecSections::empty()
        }
    }

    /// Drive a Spec through the full happy path to `in_review`, confirming the sensitive
    /// `ready` step. Returns the Spec's id. Shared by the tests that need a Spec in a
    /// given lifecycle state.
    fn spec_ready_to_review(conn: &mut SqliteConnection, project_id: &str) -> String {
        let (spec, _) = create_spec(conn, project_id, "Spec", complete_sections()).unwrap();
        transition_spec(conn, &spec.id, SpecTransition::Ready, true).unwrap();
        // `start` is now sensitive (it poses the workspace anchor) → confirm it.
        transition_spec(conn, &spec.id, SpecTransition::Start, true).unwrap();
        transition_spec(conn, &spec.id, SpecTransition::Review, false).unwrap();
        spec.id
    }

    /// DONE-CRITERION (task 01KX1DH59XXJ6XVCXE6Z33H1E0): "transitions
    /// draft->ready->in_progress->review->done + cancel gardées et validées." The full
    /// happy path walks every edge, each GUARDED by its source state, ending in
    /// `completed`; `completed_at` is stamped on done.
    #[test]
    fn full_lifecycle_happy_path_draft_to_done() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "Spec", complete_sections()).unwrap();
        assert_eq!(spec.status, SPEC_STATUS_DRAFT, "a fresh Spec is a draft");

        // draft → ready (sensitive: confirmed).
        let s = transition_spec(&mut conn, &spec.id, SpecTransition::Ready, true).unwrap();
        assert_eq!(s.status, SPEC_STATUS_READY);
        // ready → in_progress (sensitive: it poses the anchor — confirmed).
        let s = transition_spec(&mut conn, &spec.id, SpecTransition::Start, true).unwrap();
        assert_eq!(s.status, SPEC_STATUS_IN_PROGRESS);
        // in_progress → in_review ("review", routine).
        let s = transition_spec(&mut conn, &spec.id, SpecTransition::Review, false).unwrap();
        assert_eq!(s.status, SPEC_STATUS_IN_REVIEW);
        // in_review → completed ("done", sensitive: confirmed).
        let s = transition_spec(&mut conn, &spec.id, SpecTransition::Done, true).unwrap();
        assert_eq!(s.status, SPEC_STATUS_COMPLETED);
        assert!(s.completed_at.is_some(), "done stamps completed_at");
    }

    /// DONE-CRITERION: cancel is a guarded transition reachable from any non-terminal
    /// state and refused from a terminal one. Confirmed cancel from `in_progress` works;
    /// cancel from `completed` (terminal) is an invalid source.
    #[test]
    fn cancel_from_non_terminal_states_only() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();

        // Cancel from in_progress (a non-terminal state) — confirmed → cancelled.
        let (spec, _) = create_spec(&mut conn, &project.id, "A", complete_sections()).unwrap();
        transition_spec(&mut conn, &spec.id, SpecTransition::Ready, true).unwrap();
        transition_spec(&mut conn, &spec.id, SpecTransition::Start, true).unwrap();
        let cancelled = transition_spec(&mut conn, &spec.id, SpecTransition::Cancel, true).unwrap();
        assert_eq!(cancelled.status, SPEC_STATUS_CANCELLED);

        // Cancel from a TERMINAL state (completed) is an invalid source.
        let done_id = spec_ready_to_review(&mut conn, &project.id);
        transition_spec(&mut conn, &done_id, SpecTransition::Done, true).unwrap();
        let err = transition_spec(&mut conn, &done_id, SpecTransition::Cancel, true).unwrap_err();
        assert!(
            matches!(err, SpecTransitionError::InvalidSource { .. }),
            "cancel from a terminal (completed) state is refused as an invalid source"
        );
    }

    /// DONE-CRITERION: the GUARD refuses a transition from an illegal source state — e.g.
    /// `start` (ready→in_progress) attempted on a `draft` is rejected, writing nothing.
    #[test]
    fn guard_refuses_transition_from_illegal_source() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "Spec", complete_sections()).unwrap();

        // start requires source=ready; on a draft it is an invalid source. `start` is now
        // sensitive, so confirm it to reach the source guard (the guard-rail is checked first).
        let err = transition_spec(&mut conn, &spec.id, SpecTransition::Start, true).unwrap_err();
        match err {
            SpecTransitionError::InvalidSource { from, to } => {
                assert_eq!(from, SPEC_STATUS_DRAFT);
                assert_eq!(to, SPEC_STATUS_IN_PROGRESS);
            }
            other => panic!("expected InvalidSource, got {other:?}"),
        }
        // The refusal wrote nothing: the Spec is still a draft.
        assert_eq!(
            get_spec(&mut conn, &spec.id).unwrap().unwrap().status,
            SPEC_STATUS_DRAFT
        );

        // done requires source=in_review; on a draft it is likewise refused (and needs
        // confirmation — but the guard-rail is checked first, so pass confirmed to reach
        // the source guard).
        let err = transition_spec(&mut conn, &spec.id, SpecTransition::Done, true).unwrap_err();
        assert!(matches!(err, SpecTransitionError::InvalidSource { .. }));
    }

    /// DONE-CRITERION: "garde-fous (confirmation) sur ready/start/done/cancel." A SENSITIVE
    /// transition without user confirmation is refused with `ConfirmationRequired`, writing
    /// nothing; WITH confirmation it proceeds. `start` is now sensitive (it poses the
    /// workspace anchor); only `review` stays routine (no confirmation needed).
    #[test]
    fn sensitive_transitions_require_user_confirmation() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "Spec", complete_sections()).unwrap();

        // ready is sensitive: unconfirmed → refused, nothing written.
        let err = transition_spec(&mut conn, &spec.id, SpecTransition::Ready, false).unwrap_err();
        assert!(matches!(err, SpecTransitionError::ConfirmationRequired));
        assert_eq!(
            get_spec(&mut conn, &spec.id).unwrap().unwrap().status,
            SPEC_STATUS_DRAFT,
            "an unconfirmed sensitive transition writes nothing"
        );

        // Confirmed ready proceeds. `start` is ALSO sensitive now: unconfirmed → refused,
        // still `ready` (nothing written).
        transition_spec(&mut conn, &spec.id, SpecTransition::Ready, true).unwrap();
        let err = transition_spec(&mut conn, &spec.id, SpecTransition::Start, false).unwrap_err();
        assert!(
            matches!(err, SpecTransitionError::ConfirmationRequired),
            "start is sensitive (it poses the anchor) — unconfirmed is refused"
        );
        assert_eq!(
            get_spec(&mut conn, &spec.id).unwrap().unwrap().status,
            SPEC_STATUS_READY,
            "an unconfirmed start writes nothing"
        );

        // Confirmed start proceeds; review is routine (no confirmation needed).
        transition_spec(&mut conn, &spec.id, SpecTransition::Start, true)
            .expect("confirmed start proceeds");
        transition_spec(&mut conn, &spec.id, SpecTransition::Review, false)
            .expect("review needs no confirmation");

        // done is sensitive: unconfirmed → refused.
        let err = transition_spec(&mut conn, &spec.id, SpecTransition::Done, false).unwrap_err();
        assert!(matches!(err, SpecTransitionError::ConfirmationRequired));

        // cancel is sensitive: unconfirmed → refused (from in_review here).
        let err = transition_spec(&mut conn, &spec.id, SpecTransition::Cancel, false).unwrap_err();
        assert!(matches!(err, SpecTransitionError::ConfirmationRequired));
    }

    /// DONE-CRITERION: "validations pré-ready (critères/deps/cycles/phases) bloquent un
    /// draft incomplet." An INCOMPLETE draft (empty sections) does NOT pass ready — the
    /// validation lists the missing criteria, and the Spec stays a draft.
    #[test]
    fn incomplete_draft_does_not_pass_ready() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        // A blank draft: no problem, no solution.
        let (spec, _) =
            create_spec(&mut conn, &project.id, "Empty", SpecSections::empty()).unwrap();

        let err = transition_spec(&mut conn, &spec.id, SpecTransition::Ready, true).unwrap_err();
        match err {
            SpecTransitionError::ValidationFailed(failures) => {
                assert!(
                    failures.contains(&ReadyValidation::MissingProblem),
                    "an empty draft is missing its problem"
                );
                assert!(
                    failures.contains(&ReadyValidation::MissingSolution),
                    "an empty draft is missing its solution"
                );
            }
            other => panic!("expected ValidationFailed, got {other:?}"),
        }
        // The failed validation wrote nothing: still a draft.
        assert_eq!(
            get_spec(&mut conn, &spec.id).unwrap().unwrap().status,
            SPEC_STATUS_DRAFT,
            "a draft that fails validation stays a draft"
        );

        // Filling in the criteria then lets it pass ready.
        update_active_revision_section(&mut conn, &spec.id, SpecSection::Problem, "P").unwrap();
        update_active_revision_section(&mut conn, &spec.id, SpecSection::Solution, "S").unwrap();
        let ok = transition_spec(&mut conn, &spec.id, SpecTransition::Ready, true).unwrap();
        assert_eq!(
            ok.status, SPEC_STATUS_READY,
            "a complete draft passes ready"
        );
    }

    /// DONE-CRITERION (validations pré-ready: cycles/phases): an INCOHERENT phase plan
    /// blocks ready. A malformed JSON, a duplicate phase number (an ordering cycle), and
    /// an empty phase name each trip a validation; a clean 1..N plan (or an empty plan)
    /// passes.
    #[test]
    fn incoherent_phase_plan_blocks_ready() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "Spec", complete_sections()).unwrap();

        // Malformed JSON → IncoherentPhasePlan.
        set_spec_phases(&mut conn, &spec.id, "not json").unwrap();
        let v = validate_ready(&mut conn, &spec.id).unwrap();
        assert!(
            v.iter()
                .any(|f| matches!(f, ReadyValidation::IncoherentPhasePlan { .. })),
            "malformed phases JSON trips IncoherentPhasePlan"
        );

        // Duplicate phase number (an ordering cycle) → PhasePlanOrdering.
        set_spec_phases(
            &mut conn,
            &spec.id,
            r#"[{"number":1,"name":"a"},{"number":1,"name":"b"}]"#,
        )
        .unwrap();
        let v = validate_ready(&mut conn, &spec.id).unwrap();
        assert!(
            v.iter()
                .any(|f| matches!(f, ReadyValidation::PhasePlanOrdering { .. })),
            "a duplicate phase number trips PhasePlanOrdering (a cycle in the ordering)"
        );
        // The incoherent plan blocks ready.
        let err = transition_spec(&mut conn, &spec.id, SpecTransition::Ready, true).unwrap_err();
        assert!(matches!(err, SpecTransitionError::ValidationFailed(_)));

        // An empty phase name → IncoherentPlan.
        set_spec_phases(&mut conn, &spec.id, r#"[{"number":1,"name":"  "}]"#).unwrap();
        let v = validate_ready(&mut conn, &spec.id).unwrap();
        assert!(v
            .iter()
            .any(|f| matches!(f, ReadyValidation::IncoherentPhasePlan { .. })));

        // A CLEAN 1..N plan passes (no phase-plan failures).
        set_spec_phases(
            &mut conn,
            &spec.id,
            r#"[{"number":1,"name":"Foundations"},{"number":2,"name":"UI"}]"#,
        )
        .unwrap();
        let v = validate_ready(&mut conn, &spec.id).unwrap();
        assert!(
            v.is_empty(),
            "a complete draft with a clean 1..N phase plan has no validation failures, got {v:?}"
        );
        let ok = transition_spec(&mut conn, &spec.id, SpecTransition::Ready, true).unwrap();
        assert_eq!(ok.status, SPEC_STATUS_READY);
    }

    /// A transition on an UNKNOWN Spec id is refused with `NotFound` (never a silent
    /// no-op or a panic).
    #[test]
    fn transition_on_unknown_spec_is_not_found() {
        let mut conn = open_in_memory();
        let err =
            transition_spec(&mut conn, "no-such-spec", SpecTransition::Start, false).unwrap_err();
        assert!(matches!(err, SpecTransitionError::NotFound));
    }

    /// `validate_ready` is a PURE preview: it returns the failures WITHOUT changing the
    /// Spec's status (a UI can show "why not ready" without attempting the transition).
    #[test]
    fn validate_ready_is_a_pure_preview() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "Spec", SpecSections::empty()).unwrap();

        let before = get_spec(&mut conn, &spec.id).unwrap().unwrap();
        let failures = validate_ready(&mut conn, &spec.id).unwrap();
        assert!(!failures.is_empty(), "an empty draft has failures");
        let after = get_spec(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(before, after, "validate_ready does not mutate the Spec");
    }

    /// The `status` CHECK constraint rejects anything but alive|closed — proves
    /// the migration's constraint reached the DB.
    #[test]
    fn status_check_constraint_enforced() {
        let mut conn = open_in_memory();
        let bad = diesel::insert_into(terminals::table)
            .values((
                terminals::id.eq(Uuid::now_v7().to_string()),
                terminals::cwd.eq("/x"),
                terminals::status.eq("bogus"),
                terminals::order_index.eq(0),
            ))
            .execute(&mut conn);
        assert!(
            bad.is_err(),
            "status CHECK must reject values outside alive|closed"
        );
    }

    // --- Terminal exec-state (PRD-2.1 v4) --------------------------------

    /// A freshly-created terminal loads with the exec-state defaults: `idle`, no
    /// exit code, NOT unread, and a stamped `exec_state_updated_at`. This is the
    /// "new terminals must get idle defaults" criterion (and the shape an OLD
    /// terminal also takes after the ALTER TABLE ADD COLUMN migration runs).
    #[test]
    fn new_terminal_defaults_to_idle_exec_state() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).expect("create_terminal");
        assert_eq!(t.exec_state, STATE_IDLE, "default exec_state is idle");
        assert_eq!(t.exec_exit_code, None, "no exit code yet");
        assert!(!t.exec_state_unread, "default unread is false");
        assert!(
            t.exec_state_updated_at > 0,
            "exec_state_updated_at is stamped (DEFAULT julianday-ms)"
        );
    }

    /// OLD terminals (a row that predates v4, inserted WITHOUT the exec-state
    /// columns) migrate to `idle` with `unread = false`. We simulate a pre-v4 row
    /// by inserting only the v1/v2 columns and letting the migration DEFAULTs fill
    /// the rest, then read it back through the full `Terminal` model.
    #[test]
    fn old_terminal_loads_as_idle_with_unread_false() {
        let mut conn = open_in_memory();
        let id = Uuid::now_v7().to_string();
        // Insert touching ONLY the pre-exec-state columns: the four v4 columns are
        // filled by their ALTER TABLE DEFAULTs, exactly as a real old row would be.
        diesel::insert_into(terminals::table)
            .values((
                terminals::id.eq(&id),
                terminals::cwd.eq("/legacy"),
                terminals::status.eq(STATUS_ALIVE),
                terminals::order_index.eq(0),
            ))
            .execute(&mut conn)
            .expect("insert legacy-shaped row");

        let got = get_terminal(&mut conn, &id)
            .expect("get_terminal")
            .expect("row present");
        assert_eq!(got.exec_state, STATE_IDLE, "old terminal migrates to idle");
        assert_eq!(got.exec_exit_code, None);
        assert!(
            !got.exec_state_unread,
            "old terminal is read (unread=false)"
        );
    }

    /// The GENUINE upgrade path (done-criterion "migration/default tests pass on a
    /// fresh AND **upgraded** DB"): revert v4 so the schema is the PRE-exec-state
    /// shape (no `exec_*` columns), insert a terminal into THAT old schema — proving
    /// it really predates v4 — then RUN the v4 migration (the actual ALTER TABLE ADD
    /// COLUMN upgrade) and read the row back through the full `Terminal` model. The
    /// pre-existing row must surface with the idle defaults the ADD COLUMN DEFAULTs
    /// supply: `idle`, no exit code, unread=false, a stamped `updated_at`. This is
    /// stronger than `old_terminal_loads_as_idle_with_unread_false` (which inserts a
    /// legacy-SHAPED row into an already-migrated DB): here the columns genuinely do
    /// not exist when the row is written, so the migration itself is exercised.
    #[test]
    fn migration_v4_upgrade_backfills_old_terminals_with_idle_defaults() {
        use diesel::sql_query;
        let mut conn = open_in_memory();

        // Step DOWN to the pre-v4 schema → the four exec-state columns are gone. The
        // exec-state migration is dir #4; later PRDs stacked dirs #5..#17 on top (v2.4
        // Phase 1 added dir #13 the Spec domain; Phase 2 added dir #14 the task domain;
        // Phase 3 added dir #15 the specs.workspace_id index; Phase 4 added dir #16 the
        // review_comments domain; Phase 5 added dir #17 the review_tasks domain), so
        // reaching the pre-v4 schema peels all of them then dir #4 itself (14 reverts,
        // reverse order).
        for _ in 0..15 {
            conn.revert_last_migration(MIGRATIONS)
                .expect("revert migration cleanly down to (and including) exec_state");
        }
        // Sanity: the column really is absent now — referencing it must error.
        let exec_col_present: QueryResult<usize> =
            sql_query("SELECT exec_state FROM terminals").execute(&mut conn);
        assert!(
            exec_col_present.is_err(),
            "after reverting v4 the exec_state column must NOT exist (pre-upgrade schema)"
        );

        // Insert a terminal into the PRE-v4 schema (only v1/v2 columns exist). A raw
        // SQL insert is used because the diesel `terminals::table` model now knows
        // the v4 columns; this writes a row exactly as a pre-v4 build would have.
        let id = Uuid::now_v7().to_string();
        sql_query(format!(
            "INSERT INTO terminals (id, cwd, status, \"order\") \
             VALUES ('{id}', '/legacy', '{STATUS_ALIVE}', 0)"
        ))
        .execute(&mut conn)
        .expect("insert a pre-v4 terminal row");

        // Now UPGRADE: run the v4 migration (ALTER TABLE ADD COLUMN ...). The old
        // row must be backfilled by the column DEFAULTs.
        run_migrations(&mut conn).expect("re-apply v4 (the upgrade)");

        let got = get_terminal(&mut conn, &id)
            .expect("get_terminal after upgrade")
            .expect("the pre-v4 row survived the upgrade");
        assert_eq!(
            got.exec_state, STATE_IDLE,
            "an upgraded old terminal backfills to idle — no false badge on upgrade"
        );
        assert_eq!(got.exec_exit_code, None, "no exit code on an upgraded row");
        assert!(
            !got.exec_state_unread,
            "an upgraded old terminal is read (unread=false)"
        );
        assert!(
            got.exec_state_updated_at > 0,
            "the backfill UPDATE (up.sql:58-60) stamps the julianday epoch-ms on the \
             upgraded row — the column itself is added with a constant DEFAULT 0"
        );
    }

    /// `set_exec_state` round-trips the full tuple, and `list`/`get` return it —
    /// covering the "list/get terminal APIs return the new fields" criterion.
    #[test]
    fn set_exec_state_round_trips_through_list_and_get() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();

        // running: no exit code, and (for an active terminal) read.
        set_exec_state(&mut conn, &t.id, STATE_RUNNING, None, false).expect("set running");
        let got = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(got.exec_state, STATE_RUNNING);
        assert_eq!(got.exec_exit_code, None);
        assert!(!got.exec_state_unread);

        // error with a non-zero exit code, unread (settled on an inactive terminal).
        set_exec_state(&mut conn, &t.id, STATE_ERROR, Some(3), true).expect("set error");
        let listed = list_terminals(&mut conn).unwrap();
        let row = listed.iter().find(|r| r.id == t.id).unwrap();
        assert_eq!(row.exec_state, STATE_ERROR);
        assert_eq!(row.exec_exit_code, Some(3));
        assert!(
            row.exec_state_unread,
            "settled error on inactive term is unread"
        );

        // success with exit 0.
        set_exec_state(&mut conn, &t.id, STATE_SUCCESS, Some(0), true).expect("set success");
        let got = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(got.exec_state, STATE_SUCCESS);
        assert_eq!(got.exec_exit_code, Some(0));
    }

    /// BOOT NORMALIZATION (PRD task #2): `normalize_phantom_running_terminals`
    /// settles every terminal stuck at a persisted `running` to `idle` (clearing the
    /// exit code + unread), while LEAVING `success`/`error`/`idle` untouched. This is
    /// the terminal analogue of the managed-command boot normalize — the exact
    /// dogfood symptom (terminals left `running` in the DB after a force-quit) is
    /// erased at launch.
    #[test]
    fn boot_normalizes_phantom_running_terminals() {
        let mut conn = open_in_memory();
        // A phantom running (force-quit mid-command), plus the three states that must
        // SURVIVE the normalization untouched.
        let running = create_terminal(&mut conn, "/running", None).unwrap();
        let success = create_terminal(&mut conn, "/success", None).unwrap();
        let error = create_terminal(&mut conn, "/error", None).unwrap();
        let idle = create_terminal(&mut conn, "/idle", None).unwrap();
        set_exec_state(&mut conn, &running.id, STATE_RUNNING, None, false).unwrap();
        set_exec_state(&mut conn, &success.id, STATE_SUCCESS, Some(0), true).unwrap();
        set_exec_state(&mut conn, &error.id, STATE_ERROR, Some(2), true).unwrap();
        // `idle` keeps the default idle state.

        let normalized = normalize_phantom_running_terminals(&mut conn).expect("normalize");
        assert_eq!(
            normalized, 1,
            "exactly the one phantom-running row is normalized"
        );

        // The phantom running is now idle, with no exit code and not unread.
        let got = get_terminal(&mut conn, &running.id).unwrap().unwrap();
        assert_eq!(
            got.exec_state, STATE_IDLE,
            "phantom running settled to idle"
        );
        assert_eq!(got.exec_exit_code, None, "phantom exit code cleared");
        assert!(
            !got.exec_state_unread,
            "phantom is not an unread notification"
        );

        // Settled results + idle survive untouched (their badge/unread persist).
        let s = get_terminal(&mut conn, &success.id).unwrap().unwrap();
        assert_eq!(s.exec_state, STATE_SUCCESS);
        assert_eq!(s.exec_exit_code, Some(0));
        assert!(s.exec_state_unread, "settled success keeps its unread flag");
        let e = get_terminal(&mut conn, &error.id).unwrap().unwrap();
        assert_eq!(e.exec_state, STATE_ERROR);
        assert_eq!(e.exec_exit_code, Some(2));
        let i = get_terminal(&mut conn, &idle.id).unwrap().unwrap();
        assert_eq!(i.exec_state, STATE_IDLE);

        // Idempotent: a second pass normalizes nothing (no running left).
        assert_eq!(
            normalize_phantom_running_terminals(&mut conn).unwrap(),
            0,
            "second boot normalize is a no-op (no phantom running remains)"
        );
    }

    /// `mark_exec_state_read` clears the unread flag but PRESERVES the settled
    /// state + exit code (the deliberate difference from the acknowledge model).
    #[test]
    fn mark_exec_state_read_clears_unread_but_keeps_result() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        set_exec_state(&mut conn, &t.id, STATE_ERROR, Some(2), true).expect("set error unread");

        mark_exec_state_read(&mut conn, &t.id).expect("mark read");
        let got = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert!(!got.exec_state_unread, "unread cleared on mark-read");
        assert_eq!(got.exec_state, STATE_ERROR, "settled state preserved");
        assert_eq!(got.exec_exit_code, Some(2), "exit code preserved");
    }

    /// The `exec_state` CHECK constraint rejects anything but idle|running|
    /// success|error — invalid exec_state values cannot be persisted through the
    /// normal `set_exec_state` path. Proves the migration constraint reached the DB.
    #[test]
    fn exec_state_check_constraint_enforced() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let bad = set_exec_state(&mut conn, &t.id, "bogus", None, false);
        assert!(
            bad.is_err(),
            "exec_state CHECK must reject values outside idle|running|success|error"
        );
        // A raw insert with a bad exec_state is rejected too (defense in depth).
        let bad_insert = diesel::insert_into(terminals::table)
            .values((
                terminals::id.eq(Uuid::now_v7().to_string()),
                terminals::cwd.eq("/x"),
                terminals::status.eq(STATUS_ALIVE),
                terminals::order_index.eq(0),
                terminals::exec_state.eq("nope"),
            ))
            .execute(&mut conn);
        assert!(
            bad_insert.is_err(),
            "raw insert with bad exec_state rejected"
        );
    }

    // --- CRUD (YR done criteria) -----------------------------------------

    /// `create` then `list` returns the terminal (criterion 1, first half). A
    /// freshly created terminal is `alive` and appears in the list.
    #[test]
    fn create_then_list_returns_the_terminal() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/home/kris/work", Some("dev".into()))
            .expect("create_terminal");
        assert_eq!(t.status, STATUS_ALIVE, "new terminal is alive");
        assert_eq!(t.cwd, "/home/kris/work");

        let listed = list_terminals(&mut conn).expect("list_terminals");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], t, "list returns the created terminal verbatim");
    }

    /// `create` appends in creation order via `order`, and `list` returns them
    /// in that order. Guards the auto-order logic.
    #[test]
    fn create_appends_in_order_and_list_is_ordered() {
        let mut conn = open_in_memory();
        let a = create_terminal(&mut conn, "/a", None).unwrap();
        let b = create_terminal(&mut conn, "/b", None).unwrap();
        let c = create_terminal(&mut conn, "/c", None).unwrap();
        assert!(
            a.order_index < b.order_index && b.order_index < c.order_index,
            "each create appends after the previous max order"
        );
        let ids: Vec<String> = list_terminals(&mut conn)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec![a.id, b.id, c.id], "list follows order asc");
    }

    /// `close` flips status to `closed`; the row still exists and lists (closed
    /// terminals are kept, just not re-spawned). Criterion 1, second half.
    #[test]
    fn close_marks_closed_and_row_survives() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/x", None).unwrap();
        let n = close_terminal(&mut conn, &t.id).expect("close_terminal");
        assert_eq!(n, 1, "exactly one row closed");

        let got = get_terminal(&mut conn, &t.id)
            .unwrap()
            .expect("row still there");
        assert_eq!(got.status, STATUS_CLOSED, "status flipped to closed");
        assert!(
            got.closed_at.is_some(),
            "closing must stamp closed_at (was None before)"
        );
        assert_eq!(
            list_terminals(&mut conn).unwrap().len(),
            1,
            "closed terminals are retained in the list"
        );
    }

    /// Closing a terminal that hosts a live agent session ENDS that session
    /// (review #58): a voluntarily-closed terminal is logically dead, so its
    /// `active` session must not linger `active`/`unknown` forever — `close_terminal`
    /// flips it to `ended` (stamping `ended_at`) in the same write. Resume is
    /// unaffected (a closed terminal is never a boot candidate), so this only makes
    /// the DB reflect reality.
    #[test]
    fn close_terminal_ends_its_active_agent_session() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let s = record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("ext-1"))
            .expect("record_session_start");
        assert_eq!(s.state, SESSION_STATE_ACTIVE, "fresh session is active");
        assert_eq!(s.ended_at, None, "fresh session has not ended");

        let n = close_terminal(&mut conn, &t.id).expect("close_terminal");
        assert_eq!(n, 1, "exactly one terminal row closed");

        // The terminal is closed (unchanged behavior).
        let got_t = get_terminal(&mut conn, &t.id)
            .unwrap()
            .expect("terminal row");
        assert_eq!(got_t.status, STATUS_CLOSED, "terminal flipped to closed");

        // The previously-active session is now ended with ended_at stamped.
        let got_s = get_session(&mut conn, &s.id).unwrap().expect("session row");
        assert_eq!(
            got_s.state, SESSION_STATE_ENDED,
            "the terminal's active session is ended on close"
        );
        assert!(
            got_s.ended_at.is_some(),
            "ending the session stamps ended_at (was None before)"
        );

        // Resume behavior is unchanged: a closed terminal is never a boot resume
        // candidate, regardless of its (now ended) session state.
        let candidates = resume_candidates_on_boot(&mut conn).unwrap();
        assert!(
            candidates.iter().all(|c| c.terminal_id != t.id),
            "a closed terminal is never a boot resume candidate"
        );
    }

    /// Timestamps are epoch-ms and track mutations: `created_at`/`updated_at` are
    /// set on create (equal), mutations bump `updated_at` (not `created_at`), and
    /// `closed_at` goes from NULL → set on close.
    #[test]
    fn timestamps_are_epoch_millis_and_track_mutations() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/x", None).unwrap();
        assert!(t.created_at > 0 && t.updated_at == t.created_at);
        assert_eq!(t.closed_at, None);

        // A mutation bumps updated_at past created_at but never moves created_at.
        // Force a distinct instant so the assertion is robust on fast clocks.
        std::thread::sleep(std::time::Duration::from_millis(2));
        rename(&mut conn, &t.id, Some("named".into())).unwrap();
        let after_rename = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(
            after_rename.created_at, t.created_at,
            "created_at is immutable"
        );
        assert!(
            after_rename.updated_at >= t.updated_at,
            "a mutation bumps updated_at"
        );
        assert_eq!(after_rename.closed_at, None, "still open after a rename");

        // Closing stamps closed_at and bumps updated_at.
        close_terminal(&mut conn, &t.id).unwrap();
        let after_close = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        let closed_at = after_close.closed_at.expect("closed_at set on close");
        assert!(closed_at > 0, "closed_at is a positive epoch-ms timestamp");
        assert!(
            after_close.updated_at >= after_rename.updated_at,
            "closing bumps updated_at"
        );
    }

    /// `persist_scrollback` stores then reads back the SAME string (criterion 2).
    #[test]
    fn persist_scrollback_roundtrips_same_string() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/x", None).unwrap();
        let payload = "line1\r\nline2\r\n\x1b[31mred\x1b[0m\r\n";
        let n = persist_scrollback(&mut conn, &t.id, payload).expect("persist");
        assert_eq!(n, 1);
        let got = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(
            got.scrollback, payload,
            "scrollback must round-trip byte-for-byte"
        );
    }

    /// `persist_scrollback` is BOUNDED: a payload larger than the cap is stored
    /// truncated to the cap (keeping the tail) and stays valid UTF-8.
    #[test]
    fn persist_scrollback_is_bounded_keeps_tail() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/x", None).unwrap();
        // Build a payload well over the cap with a recognizable tail.
        let filler = "a".repeat(MAX_SCROLLBACK_BYTES);
        let payload = format!("{filler}TAIL_MARKER_END");
        assert!(payload.len() > MAX_SCROLLBACK_BYTES);

        persist_scrollback(&mut conn, &t.id, &payload).unwrap();
        let got = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert!(
            got.scrollback.len() <= MAX_SCROLLBACK_BYTES,
            "stored scrollback must be bounded to the cap, got {}",
            got.scrollback.len()
        );
        assert!(
            got.scrollback.ends_with("TAIL_MARKER_END"),
            "bounding must keep the TAIL (most recent output)"
        );
    }

    /// `bound_scrollback` never splits a multi-byte UTF-8 char at the cut point.
    #[test]
    fn bound_scrollback_respects_char_boundaries() {
        // A string of multi-byte chars longer than the cap.
        let big = "é".repeat(MAX_SCROLLBACK_BYTES); // each 'é' is 2 bytes
        let bounded = bound_scrollback(&big);
        assert!(bounded.len() <= MAX_SCROLLBACK_BYTES);
        // The result is valid UTF-8 by construction (slicing on a boundary);
        // assert it round-trips as chars (no replacement char from a bad cut).
        assert!(
            bounded.chars().all(|c| c == 'é'),
            "bounded slice must contain only whole 'é' chars (no split byte)"
        );
    }

    /// `reorder` persists a new order that `list` then reflects (criterion 3a).
    #[test]
    fn reorder_persists_new_order() {
        let mut conn = open_in_memory();
        let a = create_terminal(&mut conn, "/a", None).unwrap();
        let b = create_terminal(&mut conn, "/b", None).unwrap();
        let c = create_terminal(&mut conn, "/c", None).unwrap();

        // New order: c, a, b.
        reorder(&mut conn, &[c.id.clone(), a.id.clone(), b.id.clone()]).expect("reorder");

        let ids: Vec<String> = list_terminals(&mut conn)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(
            ids,
            vec![c.id.clone(), a.id.clone(), b.id.clone()],
            "list must reflect the persisted reorder"
        );

        // And the stored order_index values are 0,1,2 in that sequence.
        assert_eq!(
            get_terminal(&mut conn, &c.id).unwrap().unwrap().order_index,
            0
        );
        assert_eq!(
            get_terminal(&mut conn, &a.id).unwrap().unwrap().order_index,
            1
        );
        assert_eq!(
            get_terminal(&mut conn, &b.id).unwrap().unwrap().order_index,
            2
        );
    }

    /// `reorder_projects` persists a new project order and `list_projects` reflects it
    /// (FEEDBACK #11); also covers the append-last invariant on create.
    #[test]
    fn reorder_projects_persists_new_order() {
        let mut conn = open_in_memory();
        let (a, _) = create_project(&mut conn, "A", "/a", None).unwrap();
        let (b, _) = create_project(&mut conn, "B", "/b", None).unwrap();
        let (c, _) = create_project(&mut conn, "C", "/c", None).unwrap();

        // Created in order → order_index 0,1,2 (append-last), so the list is A,B,C.
        let initial: Vec<String> = list_projects(&mut conn)
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(initial, vec![a.id.clone(), b.id.clone(), c.id.clone()]);

        // New order: C, A, B.
        reorder_projects(&mut conn, &[c.id.clone(), a.id.clone(), b.id.clone()]).expect("reorder");

        let ids: Vec<String> = list_projects(&mut conn)
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(
            ids,
            vec![c.id.clone(), a.id.clone(), b.id.clone()],
            "list_projects must reflect the persisted reorder"
        );

        // The stored order_index values are 0,1,2 in the new sequence.
        assert_eq!(
            get_project(&mut conn, &c.id).unwrap().unwrap().order_index,
            0
        );
        assert_eq!(
            get_project(&mut conn, &a.id).unwrap().unwrap().order_index,
            1
        );
        assert_eq!(
            get_project(&mut conn, &b.id).unwrap().unwrap().order_index,
            2
        );
    }

    /// `rename` persists the label and clears it on `None` (criterion 3b).
    #[test]
    fn rename_persists_label_and_clears() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/x", None).unwrap();
        assert_eq!(t.label, None);

        rename(&mut conn, &t.id, Some("my-shell".into())).expect("rename set");
        assert_eq!(
            get_terminal(&mut conn, &t.id)
                .unwrap()
                .unwrap()
                .label
                .as_deref(),
            Some("my-shell"),
            "rename must persist the new label"
        );

        rename(&mut conn, &t.id, None).expect("rename clear");
        assert_eq!(
            get_terminal(&mut conn, &t.id).unwrap().unwrap().label,
            None,
            "rename(None) must clear the label"
        );
    }

    /// FEEDBACK #32: `set_terminal_cwd` overwrites the record's `cwd` and bumps
    /// `updated_at`, so a relaunch re-spawns the shell at the LAST directory the
    /// user was in (the live `/proc` cwd the front-side persists on change), not
    /// the stale original workspace-root cwd. A no-op on an unknown id.
    #[test]
    fn set_terminal_cwd_persists_cwd_and_bumps_updated_at() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work/sandbox", None).unwrap();
        assert_eq!(t.cwd, "/work/sandbox", "fresh terminal keeps its spawn cwd");

        // A real `cd` clock would always be later; sleep so the bump is observable
        // even on a fast clock (mirrors set_active's timing assertion).
        std::thread::sleep(std::time::Duration::from_millis(2));
        let rows =
            set_terminal_cwd(&mut conn, &t.id, "/work/sandbox/sandbox-tests").expect("set cwd");
        assert_eq!(rows, 1, "the existing terminal row is updated");

        let after = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(
            after.cwd, "/work/sandbox/sandbox-tests",
            "set_terminal_cwd must persist the new (subdir) cwd"
        );
        assert!(
            after.updated_at > t.updated_at,
            "set_terminal_cwd is a content mutation: updated_at must move forward"
        );

        // Unknown id is a silent no-op (no panic, no spurious insert).
        assert_eq!(
            set_terminal_cwd(&mut conn, "no-such-id", "/elsewhere").unwrap(),
            0,
            "set_terminal_cwd on an unknown id affects zero rows"
        );
    }

    /// RESTORE round-trip at the DATA layer (the exact read the re-spawn flow
    /// makes): persist scrollback on several terminals, close one, then `list`
    /// and assert the restore-relevant invariants in ONE place —
    ///   - every terminal's stored scrollback comes back BYTE-FOR-BYTE (the
    ///     done-criterion "le restore ne rend pas le scrollback stocké");
    ///   - the `alive`/`closed` split is preserved so the launcher re-spawns only
    ///     the live ones (the closed one is retained but flagged closed);
    ///   - the persisted ORDER is the order `list` returns.
    ///
    /// A regression in persistence (truncation, wrong column, lost status, broken
    /// order) breaks THIS test, distinct from the single-row round-trip above.
    #[test]
    fn restore_roundtrip_lists_alive_with_scrollback_and_excludes_closed() {
        let mut conn = open_in_memory();

        let a = create_terminal(&mut conn, "/work/a", Some("alpha".into())).unwrap();
        let b = create_terminal(&mut conn, "/work/b", None).unwrap();
        let c = create_terminal(&mut conn, "/work/c", None).unwrap();

        // Distinct scrollback per terminal, incl. ANSI + CRLF so a naive store
        // that mangled control bytes would surface here.
        let sb_a = "alpha-history\r\n\x1b[32mok\x1b[0m\r\n";
        let sb_b = "beta-history\r\nsecond line\r\n";
        let sb_c = "gamma-history\r\n";
        persist_scrollback(&mut conn, &a.id, sb_a).unwrap();
        persist_scrollback(&mut conn, &b.id, sb_b).unwrap();
        persist_scrollback(&mut conn, &c.id, sb_c).unwrap();

        // Close the middle one: the launcher must NOT re-spawn it, but it stays a
        // row carrying its scrollback.
        close_terminal(&mut conn, &b.id).unwrap();

        // The restore read: list everything in order.
        let rows = list_terminals(&mut conn).unwrap();
        assert_eq!(rows.len(), 3, "all rows are retained (closed kept)");
        assert_eq!(
            rows.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            vec![a.id.clone(), b.id.clone(), c.id.clone()],
            "list returns rows in persisted order"
        );

        // Scrollback survives the round-trip byte-for-byte for EACH row.
        let by_id = |id: &str| rows.iter().find(|t| t.id == id).unwrap();
        assert_eq!(by_id(&a.id).scrollback, sb_a, "alive A scrollback restored");
        assert_eq!(by_id(&c.id).scrollback, sb_c, "alive C scrollback restored");
        assert_eq!(
            by_id(&b.id).scrollback,
            sb_b,
            "closed B retains its scrollback (kept, just not re-spawned)"
        );

        // The alive/closed split the launcher re-spawns on.
        let alive: Vec<String> = rows
            .iter()
            .filter(|t| t.status == STATUS_ALIVE)
            .map(|t| t.id.clone())
            .collect();
        assert_eq!(
            alive,
            vec![a.id.clone(), c.id.clone()],
            "only A and C are re-spawn candidates"
        );
        assert_eq!(
            by_id(&b.id).status,
            STATUS_CLOSED,
            "B is the closed (not re-spawned) row"
        );
    }

    /// MIGRATION reversibility guard, FULL down→up round-trip across the WHOLE
    /// migration chain (v1 `terminals` + v2 `projects`/`workspaces`/binding cols).
    ///
    /// History: this test predates PRD-2. Back then there was a SINGLE migration,
    /// so `revert_last_migration` rolled `terminals` away and the test asserted
    /// the table was gone. PRD-2 added migration **v2**, so `revert_last_migration`
    /// now reverts ONLY v2 — `terminals` (created in v1) correctly survives a
    /// single revert. Asserting "terminals gone after one revert" is therefore a
    /// STALE expectation, not a migration bug (the per-migration v2 down→up is
    /// covered separately by `migration_v2_down_then_up_recreates_working_schema`).
    ///
    /// To preserve this test's ORIGINAL intent — "down removes the base schema,
    /// up rebuilds a working one" — under multi-migration semantics, we revert the
    /// ENTIRE chain: after reverting all migrations BOTH `terminals` (v1) and
    /// `projects` (v2) are gone; re-applying the whole chain restores a schema that
    /// still round-trips a full `terminals` row AND the v2 project/workspace model.
    /// This exercises every `down.sql`/`up.sql` pair end-to-end, complementing
    /// `schema_matches_migration` (forward direction only).
    #[test]
    fn migration_down_then_up_recreates_working_schema() {
        use diesel_migrations::MigrationHarness;
        let mut conn = open_in_memory(); // already migrated up once (v1 + v2)

        // Roll the ENTIRE chain back: runs every down.sql (v2 then v1).
        conn.revert_all_migrations(MIGRATIONS)
            .expect("revert_all_migrations must run every down.sql cleanly");

        // After the full down, the base tables of BOTH migrations are gone — a
        // select against either must now fail (no such table).
        let terminals_after_down: QueryResult<i64> = terminals::table.count().get_result(&mut conn);
        assert!(
            terminals_after_down.is_err(),
            "after reverting the whole chain, the v1 `terminals` table must be gone"
        );
        let projects_after_down: QueryResult<i64> = projects::table.count().get_result(&mut conn);
        assert!(
            projects_after_down.is_err(),
            "after reverting the whole chain, the v2 `projects` table must be gone"
        );

        // Re-apply the whole chain: runs every up.sql and must restore a working
        // schema for both migrations.
        run_migrations(&mut conn).expect("re-running migrations after a full down must succeed");

        // The re-created v1 schema still round-trips a full `terminals` row,
        // including the v2 binding columns (workspace_id NULL, mode 'auto').
        let t = create_terminal(&mut conn, "/after/down-up", Some("revived".into()))
            .expect("insert after down→up");
        let got = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(got.cwd, "/after/down-up");
        assert_eq!(got.label.as_deref(), Some("revived"));
        assert_eq!(got.status, STATUS_ALIVE);
        assert_eq!(
            got.workspace_id, None,
            "rebuilt binding column defaults NULL"
        );
        assert_eq!(
            got.workspace_binding_mode, BINDING_AUTO,
            "rebuilt binding-mode column defaults to auto"
        );

        // And the re-created v2 schema round-trips a project + its root workspace.
        let (_project, root) = create_project(
            &mut conn,
            "revived",
            p("/after/proj", "C:\\after\\proj"),
            None,
        )
        .expect("create_project after full down→up");
        assert!(
            root.is_root,
            "the rebuilt v2 schema still creates a root workspace"
        );
    }

    /// v2.1 STORAGE SEAL (task `01KWDCNDJRM6V0M5D01KRP4ENT`, done-criteria:
    /// "Un seul fichier SQLite, WAL actif, migrations appliquées au boot" +
    /// "La chaîne de migrations est versionnée et rejouable à blanc sans erreur").
    ///
    /// Opens a REAL, single file-backed `nyx.db` (the same path shape the sidecar
    /// boots — one file for the whole authority, no second base), which runs the
    /// FULL versioned migration chain once at open (`Db::open` → `run_migrations`).
    /// Then it asserts the three sealed guarantees:
    ///   1. WAL is active on that single on-disk file (`journal_mode = wal`), proving
    ///      the boot pragmas hold on a persistent journal (not just `:memory:`).
    ///   2. The chain is fully applied — ZERO pending migrations after boot
    ///      ([`schema_health`] `up_to_date`), i.e. the whole versioned chain ran.
    ///   3. Re-running the chain on the SAME already-migrated file is a clean no-op
    ///      ("rejouable à blanc sans erreur"): `run_migrations` succeeds again and
    ///      STILL reports zero pending — Diesel's `__diesel_schema_migrations` makes a
    ///      replay idempotent, so a second boot never re-applies or errors.
    #[test]
    fn migration_chain_is_single_file_wal_and_replayable_from_scratch() {
        let (db, path) = file_backed_db();

        // Exactly ONE SQLite authority file at the sidecar's `<data_dir>/nyx.db` path
        // (WAL adds transient -wal/-shm siblings, but the DB itself is a single file).
        assert!(path.is_file(), "the DB is a single on-disk SQLite file");

        db.with_conn(|conn| {
            // (1) WAL is active on the real file-backed connection.
            let journal: String = diesel::sql_query("PRAGMA journal_mode;")
                .get_result::<PragmaText>(conn)
                .expect("read journal_mode")
                .value;
            assert_eq!(
                journal.to_lowercase(),
                "wal",
                "the single DB file must run in WAL mode after boot"
            );

            // (2) The versioned chain is fully applied at boot: nothing pending.
            let health = schema_health(conn);
            assert!(
                health.up_to_date,
                "boot must leave the schema fully migrated (chain applied)"
            );
            assert_eq!(
                health.pending_count,
                Some(0),
                "zero migrations may remain pending after boot"
            );

            // (3) Replay the whole chain on the already-migrated file — idempotent,
            // no error, still zero pending. This is "rejouable à blanc sans erreur":
            // a second boot re-runs `run_migrations` and does nothing.
            run_migrations(conn).expect("re-running the migration chain must be a clean no-op");
            let health_after = schema_health(conn);
            assert!(
                health_after.up_to_date,
                "a replay of the chain must not leave anything pending"
            );
            assert_eq!(
                health_after.pending_count,
                Some(0),
                "replaying an already-applied chain adds no pending migrations"
            );
        });
    }

    // --- v2.1 canonical structure_locale (task 01KWDCNDJWP9SVFXFQXJCC85WD) ----

    /// Helper: does a table exist in the migrated DB? Reads `sqlite_master`.
    #[cfg(test)]
    fn table_exists(conn: &mut SqliteConnection, table: &str) -> bool {
        #[derive(diesel::QueryableByName)]
        struct Cnt {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            #[diesel(column_name = "n")]
            n: i64,
        }
        diesel::sql_query(format!(
            "SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table' AND name='{table}'"
        ))
        .get_result::<Cnt>(conn)
        .expect("query sqlite_master")
        .n > 0
    }

    /// Helper: does an index exist in the migrated DB? Reads `sqlite_master`.
    #[cfg(test)]
    fn index_exists(conn: &mut SqliteConnection, index: &str) -> bool {
        #[derive(diesel::QueryableByName)]
        struct Cnt {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            #[diesel(column_name = "n")]
            n: i64,
        }
        diesel::sql_query(format!(
            "SELECT COUNT(*) AS n FROM sqlite_master WHERE type='index' AND name='{index}'"
        ))
        .get_result::<Cnt>(conn)
        .expect("query sqlite_master")
        .n > 0
    }

    /// DONE-CRITERION (task #2): "Les 3 tables structure_locale existent à la forme du
    /// data-model.dbml (colonnes, index, FKs)." Exercises EVERY canonical column of
    /// `projects` / `workspaces` / `repositories` through a real typed insert+select
    /// against the migrated DB. A drifted column name/type would fail to compile or run
    /// here (`check_for_backend` also adds a compile-time column-type check). It also
    /// asserts the canonical FKs: `projects.primary_workspace_id → workspaces.id` and
    /// `repositories.project_id → projects.id` (a repo in a non-existent project is
    /// rejected by the FK).
    #[test]
    fn canonical_structure_locale_tables_roundtrip_all_columns() {
        let mut conn = open_in_memory();

        // projects: key + primary_workspace_id + archived_at reach the model.
        let (project, root) =
            create_project(&mut conn, "sandbox", p("/p/pal", "C:\\p\\pal"), None).unwrap();
        assert!(!project.key.is_empty(), "projects.key is NOT NULL, seeded");
        assert_eq!(
            project.primary_workspace_id.as_deref(),
            Some(root.id.as_str()),
            "primary_workspace_id (canonical primary role) points at the root workspace"
        );
        assert_eq!(project.archived_at, None, "a fresh project is not archived");

        // workspaces: root_path (renamed) + folder_removed_at + archived_at reach the model.
        assert_eq!(
            root.root_path,
            crate::pathnorm::normalize(p("/p/pal", "C:\\p\\pal")),
            "root_path carries the canonical normalized folder"
        );
        assert_eq!(root.folder_removed_at, None, "a live folder is not removed");
        assert_eq!(root.archived_at, None, "a fresh workspace is not archived");

        // repositories: name + relative_path + optional + archived_at roundtrip; the
        // (project_id) FK holds (this project exists).
        let repo = create_repository(&mut conn, &project.id, "api", "api", false)
            .expect("create a non-optional repo");
        assert_eq!(repo.name, "api");
        assert_eq!(repo.relative_path, "api");
        assert!(
            !repo.optional,
            "optional defaults to false for a normal repo"
        );
        assert_eq!(repo.archived_at, None);
        let mono = create_repository(&mut conn, &project.id, "root", ".", true)
            .expect("create an optional mono-repo-style '.' repo");
        assert!(mono.optional, "optional=true persists");
        let listed = list_repositories(&mut conn, &project.id).unwrap();
        assert_eq!(listed.len(), 2, "both repos are listed for the project");

        // FK: primary_workspace_id references a REAL workspace (the root just created).
        // Deleting the workspace SET NULLs the pointer rather than orphaning it — proves
        // the FK is wired ON DELETE SET NULL as the canonical model specifies.
        diesel::delete(workspaces::table.find(&root.id))
            .execute(&mut conn)
            .expect("delete the root workspace");
        let after = get_project(&mut conn, &project.id).unwrap().unwrap();
        assert_eq!(
            after.primary_workspace_id, None,
            "primary_workspace_id FK is ON DELETE SET NULL"
        );

        // FK: a repository in a NON-existent project is rejected (project_id FK).
        let bad = create_repository(&mut conn, "no-such-project", "x", "x", false);
        assert!(
            bad.is_err(),
            "repositories.project_id FK rejects a dangling project"
        );
    }

    /// DONE-CRITERION (task #2): the canonical INDEXES exist at the dbml form —
    /// UNIQUE(projects.key), UNIQUE(workspaces.project_id, root_path) (preserved across
    /// the rename), the secondary (project_id, name) workspace index, and the two
    /// repository UNIQUE indexes. Verified structurally via `sqlite_master` AND
    /// behaviourally via constraint violations.
    #[test]
    fn canonical_structure_locale_indexes_and_uniques_are_enforced() {
        let mut conn = open_in_memory();

        // The named indexes the v10 migration creates exist.
        assert!(
            index_exists(&mut conn, "idx_projects_key"),
            "UNIQUE projects.key index"
        );
        assert!(
            index_exists(&mut conn, "idx_workspaces_project_name"),
            "secondary (project_id, name) workspace index"
        );
        assert!(
            index_exists(&mut conn, "idx_repositories_project"),
            "repositories (project_id) index"
        );

        let (project, _root) = create_project(&mut conn, "Nyx", p("/n", "C:\\n"), None).unwrap();

        // UNIQUE(projects.key): a second project sharing a key is rejected. We force a
        // collision by updating a second project's key to the first's.
        let (project2, _r2) = create_project(&mut conn, "Other", p("/o", "C:\\o"), None).unwrap();
        let dup_key = diesel::update(projects::table.find(&project2.id))
            .set(projects::key.eq(&project.key))
            .execute(&mut conn);
        assert!(
            dup_key.is_err(),
            "UNIQUE(projects.key) rejects a duplicate key"
        );

        // UNIQUE(repositories.project_id, name): same name in the same project is rejected.
        create_repository(&mut conn, &project.id, "api", "api", false).unwrap();
        let dup_name = create_repository(&mut conn, &project.id, "api", "different", false);
        assert!(
            dup_name.is_err(),
            "UNIQUE(project_id, name) rejects a duplicate repo name"
        );

        // UNIQUE(repositories.project_id, relative_path): same path in the same project rejected.
        let dup_path = create_repository(&mut conn, &project.id, "api2", "api", false);
        assert!(
            dup_path.is_err(),
            "UNIQUE(project_id, relative_path) rejects a duplicate repo path"
        );

        // The SAME repo name/path in a DIFFERENT project is accepted (uniqueness is per project).
        create_repository(&mut conn, &project2.id, "api", "api", false)
            .expect("same repo identity in another project is allowed");
    }

    /// DONE-CRITERION (v2.1 task #2): "workspace_repositories n'existe pas" — the PDR-0013
    /// checkout table must NEVER exist (a workspace's membership is DERIVED, never a stored
    /// checkout list). This invariant is PERMANENT and still holds.
    ///
    /// The companion v2.1 assertion — "aucune table specs créée (réservée à v2.4)" — is now
    /// SUPERSEDED: v2.4 Phase 1 (migration v13, task 01KX1DH4QC04C1FKHZT5ZF4BMS) realizes the
    /// Spec domain, so `specs` + `spec_revisions` now EXIST by design. This test therefore
    /// asserts the checkout table's continued absence AND the Spec tables' presence, keeping
    /// the derived-membership invariant guarded while reflecting the Spec-domain arrival.
    #[test]
    fn no_workspace_repositories_and_spec_domain_present() {
        let mut conn = open_in_memory();

        assert!(table_exists(&mut conn, "projects"), "projects exists");
        assert!(table_exists(&mut conn, "workspaces"), "workspaces exists");
        assert!(
            table_exists(&mut conn, "repositories"),
            "repositories exists"
        );

        assert!(
            !table_exists(&mut conn, "workspace_repositories"),
            "the checkout table workspace_repositories must NEVER exist (PDR-0013)"
        );
        // v2.4 Phase 1: the Spec domain is now realized (was reserved in v2.1).
        assert!(
            table_exists(&mut conn, "specs"),
            "the specs table exists (Spec domain realized in v2.4 Phase 1)"
        );
        assert!(
            table_exists(&mut conn, "spec_revisions"),
            "the spec_revisions table exists (Spec domain realized in v2.4 Phase 1)"
        );
        // v2.4 Phase 2: the task domain is now realized (tasks + dependency graph).
        assert!(
            table_exists(&mut conn, "tasks"),
            "the tasks table exists (task domain realized in v2.4 Phase 2)"
        );
        assert!(
            table_exists(&mut conn, "task_dependencies"),
            "the task_dependencies table exists (task domain realized in v2.4 Phase 2)"
        );
    }

    // --- Spec ↔ workspace LINK + DERIVED repos/project (v2.4 Phase 3, v15) ----

    /// Drive a Spec to `in_progress` (unanchored) so the standalone anchor tests can exercise
    /// `set_spec_workspace` — which is now STATE-GUARDED to `in_progress`. Confirms the two
    /// sensitive steps (ready, start) and leaves the anchor unset (start with no workspace).
    fn spec_in_progress_unanchored(conn: &mut SqliteConnection, project_id: &str) -> Spec {
        let (spec, _) = create_spec(conn, project_id, "S", complete_sections()).unwrap();
        transition_spec(conn, &spec.id, SpecTransition::Ready, true).unwrap();
        transition_spec(conn, &spec.id, SpecTransition::Start, true).unwrap();
        get_spec(conn, &spec.id).unwrap().unwrap()
    }

    /// DONE-CRITERION (task 01KX1DH6BB1QGMC0XBAQBRVZ5W), clause 1: "une Spec est liée à un
    /// workspace via specs.workspace_id". [`set_spec_workspace`] (re)anchors an IN_PROGRESS
    /// Spec on an EXISTING workspace and the pointer persists (round-trips via [`get_spec`]);
    /// clearing it (`None`) detaches sanely.
    #[test]
    fn set_spec_workspace_links_and_persists() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let spec = spec_in_progress_unanchored(&mut conn, &project.id);
        assert_eq!(spec.workspace_id, None, "in_progress but not yet anchored");

        let linked = set_spec_workspace(&mut conn, &spec.id, Some(&root.id)).unwrap();
        assert_eq!(
            linked.workspace_id.as_deref(),
            Some(root.id.as_str()),
            "the Spec is now anchored on the workspace"
        );
        assert!(
            linked.updated_at >= spec.updated_at,
            "the anchor write bumps updated_at"
        );
        // Persisted and re-read from the single SQLite file.
        let got = get_spec(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(got.workspace_id.as_deref(), Some(root.id.as_str()));

        // Clearing the anchor (None) is a sane, allowed state (posé/écrasable/jamais perdu).
        let cleared = set_spec_workspace(&mut conn, &spec.id, None).unwrap();
        assert_eq!(cleared.workspace_id, None, "the Spec detaches cleanly");
    }

    /// GUARD: anchoring on a workspace that does NOT exist is refused
    /// ([`SetSpecWorkspaceError::WorkspaceNotFound`]) and writes NOTHING — a Spec never
    /// anchors on a dangling id. An unknown Spec id is `SpecNotFound`.
    #[test]
    fn set_spec_workspace_guards_missing_workspace_and_spec() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let spec = spec_in_progress_unanchored(&mut conn, &project.id);

        let err = set_spec_workspace(&mut conn, &spec.id, Some("no-such-workspace")).unwrap_err();
        assert!(
            matches!(err, SetSpecWorkspaceError::WorkspaceNotFound),
            "a dangling workspace id is refused, got {err:?}"
        );
        // The refusal wrote nothing (the Spec is still unanchored).
        let got = get_spec(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(got.workspace_id, None, "a refused link writes nothing");

        let err2 = set_spec_workspace(&mut conn, "no-such-spec", Some(&project.id)).unwrap_err();
        assert!(
            matches!(err2, SetSpecWorkspaceError::SpecNotFound),
            "an unknown Spec is SpecNotFound, got {err2:?}"
        );
    }

    /// STATE GUARD (review finding): the standalone [`set_spec_workspace`] REFUSES to anchor a
    /// Spec that is not `in_progress` — a `draft` and a `ready` Spec are both rejected with
    /// [`SetSpecWorkspaceError::InvalidState`], writing NOTHING. The anchor is posed at the
    /// ready→in_progress transition, never at authoring. Once in_progress it is allowed.
    #[test]
    fn set_spec_workspace_refused_outside_in_progress() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();

        // draft → refused.
        let (draft, _) = create_spec(&mut conn, &project.id, "S", complete_sections()).unwrap();
        let err = set_spec_workspace(&mut conn, &draft.id, Some(&root.id)).unwrap_err();
        assert!(
            matches!(&err, SetSpecWorkspaceError::InvalidState { status } if status == SPEC_STATUS_DRAFT),
            "anchoring a draft is refused as InvalidState, got {err:?}"
        );
        assert_eq!(
            get_spec(&mut conn, &draft.id)
                .unwrap()
                .unwrap()
                .workspace_id,
            None,
            "the refused anchor wrote nothing"
        );

        // ready → still refused.
        transition_spec(&mut conn, &draft.id, SpecTransition::Ready, true).unwrap();
        let err = set_spec_workspace(&mut conn, &draft.id, Some(&root.id)).unwrap_err();
        assert!(
            matches!(&err, SetSpecWorkspaceError::InvalidState { status } if status == SPEC_STATUS_READY),
            "anchoring a ready Spec is refused as InvalidState, got {err:?}"
        );

        // in_progress → allowed.
        transition_spec(&mut conn, &draft.id, SpecTransition::Start, true).unwrap();
        let linked = set_spec_workspace(&mut conn, &draft.id, Some(&root.id)).unwrap();
        assert_eq!(linked.workspace_id.as_deref(), Some(root.id.as_str()));
    }

    /// The `start` transition POSES the workspace anchor in the SAME operation
    /// ([`transition_spec_with_workspace`]): ready→in_progress writes both the status and
    /// `specs.workspace_id` atomically. A dangling workspace id is refused
    /// ([`SpecTransitionError::WorkspaceNotFound`]) and writes nothing.
    #[test]
    fn start_transition_anchors_the_workspace_atomically() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "S", complete_sections()).unwrap();
        transition_spec(&mut conn, &spec.id, SpecTransition::Ready, true).unwrap();

        // A dangling anchor is refused; the Spec stays `ready`, unanchored.
        let err = transition_spec_with_workspace(
            &mut conn,
            &spec.id,
            SpecTransition::Start,
            true,
            Some("no-such-workspace"),
        )
        .unwrap_err();
        assert!(matches!(err, SpecTransitionError::WorkspaceNotFound));
        let got = get_spec(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(got.status, SPEC_STATUS_READY, "the refusal wrote nothing");
        assert_eq!(got.workspace_id, None);

        // A real workspace: status + anchor flip together.
        let started = transition_spec_with_workspace(
            &mut conn,
            &spec.id,
            SpecTransition::Start,
            true,
            Some(&root.id),
        )
        .unwrap();
        assert_eq!(started.status, SPEC_STATUS_IN_PROGRESS);
        assert_eq!(
            started.workspace_id.as_deref(),
            Some(root.id.as_str()),
            "start posed the anchor in the same operation"
        );
        // Persisted and resolvable through the workspace.
        assert_eq!(
            spec_for_workspace(&mut conn, &root.id)
                .unwrap()
                .map(|s| s.id),
            Some(spec.id.clone()),
            "the Spec resolves from its anchor after the start transition"
        );
    }

    /// [`create_spec_with_workspace`] anchors at creation (GUARDED: the workspace must
    /// exist) and produces the first active revision like [`create_spec`].
    #[test]
    fn create_spec_with_workspace_anchors_at_creation() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();

        let (spec, rev) = create_spec_with_workspace(
            &mut conn,
            &project.id,
            &root.id,
            "Anchored",
            SpecSections::empty(),
        )
        .unwrap();
        assert_eq!(
            spec.workspace_id.as_deref(),
            Some(root.id.as_str()),
            "the Spec is anchored at creation"
        );
        assert_eq!(spec.number, 1);
        assert_eq!(rev.revision_number, 1);
        assert_eq!(rev.revision_status, REVISION_STATUS_ACTIVE);

        // GUARD: a dangling workspace id is refused and writes nothing.
        let err = create_spec_with_workspace(
            &mut conn,
            &project.id,
            "no-such-workspace",
            "Bad",
            SpecSections::empty(),
        )
        .unwrap_err();
        assert!(matches!(err, SetSpecWorkspaceError::WorkspaceNotFound));
        assert_eq!(
            list_specs(&mut conn, &project.id).unwrap().len(),
            1,
            "the refused creation wrote no Spec"
        );
    }

    /// DONE-CRITERION clause 2: "ses repos + projet sont DÉRIVÉS du workspace (pas
    /// dupliqués)". [`spec_workspace_context`] returns the workspace + the PROJECT and the
    /// REPOSITORIES DERIVED THROUGH the workspace (Spec → workspace → project → repos), and
    /// they match the project's rows — nothing is stored on the Spec.
    #[test]
    fn spec_workspace_context_derives_repos_and_project_from_workspace() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        // The repos belong to the PROJECT (logical identities), derived via the workspace.
        let api = create_repository(&mut conn, &project.id, "api", "api", false).unwrap();
        let web = create_repository(&mut conn, &project.id, "web", "web", true).unwrap();

        let (spec, _) = create_spec_with_workspace(
            &mut conn,
            &project.id,
            &root.id,
            "S",
            SpecSections::empty(),
        )
        .unwrap();

        let ctx = spec_workspace_context(&mut conn, &spec.id)
            .unwrap()
            .expect("an anchored Spec has a derived context");
        assert_eq!(ctx.spec_id, spec.id);
        // Workspace = the anchor.
        assert_eq!(ctx.workspace.id, root.id);
        // PROJECT derived via workspace.project_id (NOT a column on the Spec).
        assert_eq!(ctx.project.id, project.id);
        assert_eq!(ctx.workspace.project_id, project.id);
        // REPOS derived via the project — exactly the project's live repos, not a copy on
        // the Spec. Ordered by relative_path (api before web).
        let names: Vec<&str> = ctx.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["api", "web"], "both derived repos are present");
        assert_eq!(
            ctx.repos,
            list_repositories(&mut conn, &project.id).unwrap()
        );
        // The `optional` flag is preserved so a consumer can filter the nyxignore repos.
        assert!(!ctx.repos[0].optional, "api is worktree'd by default");
        assert!(ctx.repos[1].optional, "web is optional (nyxignore)");
        // Sanity: the two seed ids are the ones we created.
        assert!(ctx.repos.iter().any(|r| r.id == api.id));
        assert!(ctx.repos.iter().any(|r| r.id == web.id));
    }

    /// A Spec with NO workspace has NO derived context — `Ok(None)`, a SANE state (the
    /// anchor is posed later), not an error. Likewise an unknown Spec id.
    #[test]
    fn spec_workspace_context_is_none_without_anchor() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "S", SpecSections::empty()).unwrap();

        assert!(
            spec_workspace_context(&mut conn, &spec.id)
                .unwrap()
                .is_none(),
            "an unanchored Spec has no derived context"
        );
        assert!(
            spec_workspace_context(&mut conn, "no-such-spec")
                .unwrap()
                .is_none(),
            "an unknown Spec resolves to None, not an error"
        );
    }

    /// The context follows the ON DELETE SET NULL seam: after the anchored workspace is
    /// removed, `specs.workspace_id` is nulled and the derived context is `None` again — no
    /// dangling repos/project. The v15 reverse-scan index exists in the migrated DB.
    #[test]
    fn spec_workspace_context_after_workspace_removed_and_index_present() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        create_repository(&mut conn, &project.id, "api", "api", false).unwrap();
        // A non-root workspace we can delete (the root is the project's primary).
        let feat = create_workspace(
            &mut conn,
            &project.id,
            "feat",
            p("/work/feat", "C:\\work\\feat"),
        )
        .unwrap();

        let (spec, _) = create_spec_with_workspace(
            &mut conn,
            &project.id,
            &feat.id,
            "S",
            SpecSections::empty(),
        )
        .unwrap();
        assert!(spec_workspace_context(&mut conn, &spec.id)
            .unwrap()
            .is_some());

        // Delete the workspace → ON DELETE SET NULL nulls the Spec's anchor.
        delete_workspace(&mut conn, &feat.id).unwrap();
        let got = get_spec(&mut conn, &spec.id).unwrap().unwrap();
        assert_eq!(
            got.workspace_id, None,
            "removing the workspace detaches the Spec (ON DELETE SET NULL)"
        );
        assert!(
            spec_workspace_context(&mut conn, &spec.id)
                .unwrap()
                .is_none(),
            "no derived context once the anchor is gone"
        );

        // The v15 reverse-scan index is present in the migrated DB.
        assert!(
            index_exists(&mut conn, "idx_specs_workspace"),
            "the specs.workspace_id reverse-scan index exists (v15)"
        );
    }

    // --- task domain: model + deps + finding + transitions (v2.4 Phase 2) ----

    /// A COMPLETE `NewTaskInput` (every optional attribute set) so a schema round-trip
    /// surfaces any drifted column as wrong data, not just a missing column.
    fn full_slice_input(spec_id: &str) -> NewTaskInput {
        NewTaskInput {
            kind: TASK_KIND_SLICE.to_string(),
            type_: TASK_TYPE_INVESTIGATION.to_string(),
            priority: TASK_PRIORITY_HIGH.to_string(),
            severity: None,
            description: Some("a body".to_string()),
            acceptance_criteria: Some("done when X".to_string()),
            verification_command: None,
            spec_id: Some(spec_id.to_string()),
            parent_task_id: None,
            phase_name: Some("Delivery".to_string()),
        }
    }

    /// DONE-CRITERION: "modèle task (slice/human/review) … lié à une Spec". A task
    /// round-trips through the migrated DB (every column persisted and re-read), the three
    /// KINDS create with the coherent `executor`, and the task is LINKED to its Spec.
    #[test]
    fn task_schema_matches_migration_and_kinds_link_to_spec() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "Spec", SpecSections::empty()).unwrap();

        // SLICE (agent implementation work): every column round-trips.
        let slice = create_task(
            &mut conn,
            &project.id,
            "Do the thing",
            full_slice_input(&spec.id),
        )
        .expect("create slice task");
        assert_eq!(slice.project_id, project.id);
        assert_eq!(
            slice.spec_id.as_deref(),
            Some(spec.id.as_str()),
            "the task is LINKED to its Spec"
        );
        assert_eq!(slice.kind, TASK_KIND_SLICE);
        assert_eq!(
            slice.executor, TASK_EXECUTOR_AGENT,
            "a slice runs by the agent"
        );
        assert_eq!(slice.type_, TASK_TYPE_INVESTIGATION);
        assert_eq!(slice.status, TASK_STATUS_TODO, "a fresh task is todo");
        assert_eq!(slice.priority, TASK_PRIORITY_HIGH);
        assert_eq!(slice.severity, None, "a slice is not a finding");
        assert_eq!(slice.description.as_deref(), Some("a body"));
        assert_eq!(slice.acceptance_criteria.as_deref(), Some("done when X"));
        assert_eq!(slice.verification_command, None);
        // The phase is resolved from the NAME "Delivery": the first phase seen on this Spec,
        // so it materializes as number 1 (get-or-create, 1-based, first-seen order).
        assert_eq!(slice.phase, Some(1));
        assert_eq!(slice.position, 0);
        assert_eq!(slice.blocked_reason, None);
        assert_eq!(slice.skip_reason, None);
        assert_eq!(slice.completed_at, None);
        assert!(slice.created_at > 0 && slice.updated_at == slice.created_at);

        // Persisted and re-read identically (the round-trip proving persistence).
        let got = get_task(&mut conn, &slice.id).unwrap().unwrap();
        assert_eq!(got, slice, "the task is persisted and re-read identically");

        // HUMAN (an action only the user can do) — executor derived to human.
        let human = create_task(
            &mut conn,
            &project.id,
            "Rotate the secret",
            NewTaskInput {
                spec_id: Some(spec.id.clone()),
                ..NewTaskInput::human("confirm the new secret works")
            },
        )
        .expect("create human task");
        assert_eq!(human.kind, TASK_KIND_HUMAN);
        assert_eq!(
            human.executor, TASK_EXECUTOR_HUMAN,
            "a human kind runs by the human"
        );
        assert_eq!(
            human.verification_command.as_deref(),
            Some("confirm the new secret works"),
            "a human task carries its verification (verifiable)"
        );

        // REVIEW (a finding = task) — carries a severity, NO separate findings table.
        let finding = create_task(
            &mut conn,
            &project.id,
            "Fix the off-by-one",
            NewTaskInput {
                spec_id: Some(spec.id.clone()),
                ..NewTaskInput::review(TASK_SEVERITY_HIGH)
            },
        )
        .expect("create review finding");
        assert_eq!(finding.kind, TASK_KIND_REVIEW);
        assert_eq!(
            finding.executor, TASK_EXECUTOR_AGENT,
            "a review finding is worked by the agent (coder)"
        );
        assert_eq!(
            finding.severity.as_deref(),
            Some(TASK_SEVERITY_HIGH),
            "a review task IS the finding (finding=task, carries severity)"
        );

        // All three kinds are listed under the Spec (the task↔Spec link, ordered).
        let listed = list_tasks_for_spec(&mut conn, &spec.id).unwrap();
        assert_eq!(listed.len(), 3, "all three kinds link to the Spec");
    }

    /// A task can exist WITHOUT a Spec (light work): `spec_id` is nullable.
    #[test]
    fn task_may_exist_without_a_spec() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let t = create_task(&mut conn, &project.id, "Light work", NewTaskInput::slice()).unwrap();
        assert_eq!(t.spec_id, None, "a task may have no Spec");
    }

    /// The DB CHECKs enforce the canonical vocabularies AND the kind↔executor coherence
    /// (a `human` kind that is not `executor=human`, or vice-versa, is rejected — the two
    /// vocabularies can never contradict).
    #[test]
    fn task_check_constraints_enforced() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let t = create_task(&mut conn, &project.id, "T", NewTaskInput::slice()).unwrap();

        let bad_kind = diesel::update(tasks::table.find(&t.id))
            .set(tasks::kind.eq("bogus"))
            .execute(&mut conn);
        assert!(bad_kind.is_err(), "tasks.kind CHECK rejects unknown kinds");

        let bad_status = diesel::update(tasks::table.find(&t.id))
            .set(tasks::status.eq("weird"))
            .execute(&mut conn);
        assert!(
            bad_status.is_err(),
            "tasks.status CHECK rejects unknown statuses"
        );

        let bad_sev = diesel::update(tasks::table.find(&t.id))
            .set(tasks::severity.eq("catastrophic"))
            .execute(&mut conn);
        assert!(
            bad_sev.is_err(),
            "tasks.severity CHECK rejects unknown severities"
        );

        // kind↔executor coherence: a slice (agent) flipped to executor=human is rejected.
        let incoherent = diesel::update(tasks::table.find(&t.id))
            .set(tasks::executor.eq(TASK_EXECUTOR_HUMAN))
            .execute(&mut conn);
        assert!(
            incoherent.is_err(),
            "a slice kind cannot be executor=human (kind↔executor CHECK)"
        );
    }

    /// DONE-CRITERION: "phases" — task_add BY NAME materializes `specs.phases` (PDR-0022).
    /// A task carries a phase NAME; `create_task` GET-OR-CREATEs it in the Spec's plan and
    /// stores the resolved 1-based number on `task.phase`. The plan builds incrementally in
    /// FIRST-SEEN order, a repeated name REUSES its number, and no task is ever orphaned
    /// (its phase always exists in the plan). `None` (or no Spec) = flat.
    #[test]
    fn task_add_by_name_materializes_the_plan_get_or_create() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "S", SpecSections::empty()).unwrap();
        // A fresh Spec has no plan.
        assert_eq!(get_spec(&mut conn, &spec.id).unwrap().unwrap().phases, "[]");

        let phase_task = |conn: &mut SqliteConnection, title: &str, name: &str| {
            create_task(
                conn,
                &project.id,
                title,
                NewTaskInput {
                    spec_id: Some(spec.id.clone()),
                    phase_name: Some(name.to_string()),
                    ..NewTaskInput::slice()
                },
            )
            .unwrap()
        };

        // First-seen "Foundation" → phase 1 (1-based), and the plan is auto-created.
        let a = phase_task(&mut conn, "wire it", "Foundation");
        assert_eq!(a.phase, Some(1), "first phase name seen is number 1");
        // A NEW name "Delivery" → phase 2 (next number, first-seen order).
        let b = phase_task(&mut conn, "ship it", "Delivery");
        assert_eq!(b.phase, Some(2), "second distinct name is number 2");
        // The SAME name "Foundation" again → REUSES number 1 (get-or-create, no new phase).
        let c = phase_task(&mut conn, "more foundation", "Foundation");
        assert_eq!(c.phase, Some(1), "a repeated name reuses its number");

        // The plan MATERIALIZED from the names, 1-based, in first-seen order, no duplicates.
        let phases = get_spec(&mut conn, &spec.id).unwrap().unwrap().phases;
        assert_eq!(
            phases, r#"[{"number":1,"name":"Foundation"},{"number":2,"name":"Delivery"}]"#,
            "specs.phases self-builds from the task phase names (1-based, first-seen)",
        );
        // No orphan is possible: every task's phase exists in the plan by construction.
        assert!(
            validate_phase_plan(&phases).is_empty(),
            "the built plan is coherent 1..N"
        );

        // A whitespace-only name, or no Spec, is flat (no phase, no plan growth).
        let blank = phase_task(&mut conn, "blank phase", "   ");
        assert_eq!(blank.phase, None, "a blank name = flat");
        let flat = create_task(&mut conn, &project.id, "Flat", NewTaskInput::slice()).unwrap();
        assert_eq!(flat.phase, None, "no phase name = flat");
        assert_eq!(
            get_spec(&mut conn, &spec.id).unwrap().unwrap().phases,
            r#"[{"number":1,"name":"Foundation"},{"number":2,"name":"Delivery"}]"#,
            "a flat/blank task never grows the plan",
        );
    }

    /// DONE-CRITERION: "dépendances sans cycle". An edge is added and read back; a
    /// self-edge is refused; a duplicate edge is idempotent; and — the load-bearing part —
    /// an edge that would create a CYCLE is REFUSED.
    #[test]
    fn task_dependencies_edges_and_cycle_refused() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let a = create_task(&mut conn, &project.id, "A", NewTaskInput::slice()).unwrap();
        let b = create_task(&mut conn, &project.id, "B", NewTaskInput::slice()).unwrap();
        let c = create_task(&mut conn, &project.id, "C", NewTaskInput::slice()).unwrap();

        // A depends on B, B depends on C (a clean chain A → B → C).
        add_task_dependency(&mut conn, &a.id, &b.id, DEPENDENCY_TYPE_BLOCKS).expect("A→B");
        add_task_dependency(&mut conn, &b.id, &c.id, DEPENDENCY_TYPE_BLOCKS).expect("B→C");

        // The edge is read back (A's direct prerequisite is B).
        let deps = list_task_dependencies(&mut conn, &a.id).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].id, b.id, "A directly depends on B");

        // A SELF edge is refused.
        assert!(
            matches!(
                add_task_dependency(&mut conn, &a.id, &a.id, DEPENDENCY_TYPE_BLOCKS),
                Err(DependencyError::SelfDependency)
            ),
            "a task cannot depend on itself"
        );

        // A DUPLICATE edge is idempotent (no error, no second row).
        add_task_dependency(&mut conn, &a.id, &b.id, DEPENDENCY_TYPE_BLOCKS)
            .expect("duplicate edge is a no-op");
        assert_eq!(
            list_task_dependencies(&mut conn, &a.id).unwrap().len(),
            1,
            "a duplicate edge does not add a second prerequisite"
        );

        // A CYCLE is REFUSED: C → A would close A → B → C → A.
        assert!(
            matches!(
                add_task_dependency(&mut conn, &c.id, &a.id, DEPENDENCY_TYPE_BLOCKS),
                Err(DependencyError::WouldCreateCycle { .. })
            ),
            "an edge that would create a cycle is refused"
        );
        // The refusal wrote nothing (C still has no prerequisites).
        assert!(
            list_task_dependencies(&mut conn, &c.id).unwrap().is_empty(),
            "the refused cycle edge left the graph unchanged"
        );

        // A DIRECT 2-cycle (B → A while A → B exists) is also refused.
        assert!(
            matches!(
                add_task_dependency(&mut conn, &b.id, &a.id, DEPENDENCY_TYPE_BLOCKS),
                Err(DependencyError::WouldCreateCycle { .. })
            ),
            "a direct back-edge (2-cycle) is refused"
        );

        // An unknown endpoint is refused.
        assert!(matches!(
            add_task_dependency(&mut conn, &a.id, "no-such-task", DEPENDENCY_TYPE_BLOCKS),
            Err(DependencyError::TaskNotFound { .. })
        ));

        // Removing an edge is the inverse (and re-adding the reverse is then legal).
        assert_eq!(remove_task_dependency(&mut conn, &a.id, &b.id).unwrap(), 1);
        add_task_dependency(&mut conn, &b.id, &a.id, DEPENDENCY_TYPE_BLOCKS)
            .expect("with A→B removed, B→A is acyclic");
    }

    /// v2.5.8 finding #31: `delete_task` REALLY removes a task (+ every `task_dependencies`
    /// edge touching it, in EITHER direction) but ONLY while its parent Spec is in `draft`.
    /// In draft the row and its edges are gone, the OTHER tasks survive, and the auto-built
    /// phase plan stays a coherent 1..N (the emptied phase entry is KEPT — no gap, no orphan).
    /// Out of draft (in_progress) the delete is REFUSED (`NotDraft`) and removes nothing.
    /// Unknown id → `NotFound`.
    #[test]
    fn delete_task_draft_only_removes_task_and_its_dependency_edges() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "S", SpecSections::empty()).unwrap();

        // A phased task A (Foundation) plus B and C (both Delivery, so the plan is 1..2).
        let mk = |conn: &mut SqliteConnection, title: &str, phase: &str| {
            create_task(
                conn,
                &project.id,
                title,
                NewTaskInput {
                    spec_id: Some(spec.id.clone()),
                    phase_name: Some(phase.to_string()),
                    ..NewTaskInput::slice()
                },
            )
            .unwrap()
        };
        let a = mk(&mut conn, "A", "Foundation");
        let b = mk(&mut conn, "B", "Delivery");
        let c = mk(&mut conn, "C", "Delivery");
        // A is the SOURCE of A→B and the TARGET of C→A — deleting A must drop BOTH edges.
        add_task_dependency(&mut conn, &a.id, &b.id, DEPENDENCY_TYPE_BLOCKS).unwrap();
        add_task_dependency(&mut conn, &c.id, &a.id, DEPENDENCY_TYPE_BLOCKS).unwrap();

        // Unknown id → NotFound.
        assert!(matches!(
            delete_task(&mut conn, "no-such-task"),
            Err(DeleteTaskError::NotFound)
        ));

        // In DRAFT: deleting A really removes the row AND both edges touching it.
        delete_task(&mut conn, &a.id).expect("delete in draft");
        assert!(
            get_task(&mut conn, &a.id).unwrap().is_none(),
            "the task row is gone"
        );
        let remaining_edges: i64 = task_dependencies::table
            .count()
            .get_result(&mut conn)
            .unwrap();
        assert_eq!(
            remaining_edges, 0,
            "every dependency edge touching the deleted task (source OR target) is gone"
        );
        // The OTHER tasks survive, and the phase plan stays a coherent 1..N — the now-empty
        // "Foundation" entry is KEPT (pruning it would open a gap the ready gate rejects).
        assert!(get_task(&mut conn, &b.id).unwrap().is_some(), "B survives");
        assert!(get_task(&mut conn, &c.id).unwrap().is_some(), "C survives");
        let phases = get_spec(&mut conn, &spec.id).unwrap().unwrap().phases;
        assert_eq!(
            phases, r#"[{"number":1,"name":"Foundation"},{"number":2,"name":"Delivery"}]"#,
            "the emptied phase entry is kept — the plan stays a clean 1..N after delete",
        );
        assert!(
            validate_phase_plan(&phases).is_empty(),
            "the plan is still coherent after the delete (no gap, no orphan)"
        );

        // Move the Spec OUT of draft → the delete is now REFUSED (skip is the legit move).
        diesel::update(specs::table.find(&spec.id))
            .set(specs::status.eq(SPEC_STATUS_IN_PROGRESS))
            .execute(&mut conn)
            .unwrap();
        match delete_task(&mut conn, &b.id) {
            Err(DeleteTaskError::NotDraft { status }) => {
                assert_eq!(
                    status, SPEC_STATUS_IN_PROGRESS,
                    "the refusal carries the status"
                )
            }
            other => panic!("expected NotDraft out of draft, got {other:?}"),
        }
        assert!(
            get_task(&mut conn, &b.id).unwrap().is_some(),
            "a refused delete removed nothing"
        );
    }

    /// DONE-CRITERION: "transitions de task portées" — each of start / done / block / skip
    /// is GUARDED at the backend, moves the status, and stamps the right column.
    #[test]
    fn task_transitions_start_done_block_skip() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();

        // START: todo → in_progress.
        let t = create_task(&mut conn, &project.id, "T", NewTaskInput::slice()).unwrap();
        let started = transition_task(&mut conn, &t.id, TaskTransition::Start, None).unwrap();
        assert_eq!(started.status, TASK_STATUS_IN_PROGRESS);

        // DONE: in_progress → done, completed_at stamped.
        let done = transition_task(&mut conn, &t.id, TaskTransition::Done, None).unwrap();
        assert_eq!(done.status, TASK_STATUS_DONE);
        assert!(done.completed_at.is_some(), "done stamps completed_at");

        // A terminal task refuses a further transition (GUARD).
        assert!(matches!(
            transition_task(&mut conn, &t.id, TaskTransition::Start, None),
            Err(TaskTransitionError::InvalidSource { .. })
        ));

        // BLOCK: a fresh task → blocked, blocked_reason stamped; then START clears it.
        let t2 = create_task(&mut conn, &project.id, "T2", NewTaskInput::slice()).unwrap();
        let blocked = transition_task(
            &mut conn,
            &t2.id,
            TaskTransition::Block,
            Some("waiting on API"),
        )
        .unwrap();
        assert_eq!(blocked.status, TASK_STATUS_BLOCKED);
        assert_eq!(blocked.blocked_reason.as_deref(), Some("waiting on API"));
        let restarted = transition_task(&mut conn, &t2.id, TaskTransition::Start, None).unwrap();
        assert_eq!(
            restarted.status, TASK_STATUS_IN_PROGRESS,
            "a blocked task restarts"
        );
        assert_eq!(
            restarted.blocked_reason, None,
            "starting clears the stale blocked_reason"
        );

        // SKIP: a task → cancelled, skip_reason stamped.
        let t3 = create_task(&mut conn, &project.id, "T3", NewTaskInput::slice()).unwrap();
        let skipped =
            transition_task(&mut conn, &t3.id, TaskTransition::Skip, Some("obsolete")).unwrap();
        assert_eq!(skipped.status, TASK_STATUS_CANCELLED);
        assert_eq!(skipped.skip_reason.as_deref(), Some("obsolete"));

        // An unknown task is NotFound.
        assert!(matches!(
            transition_task(&mut conn, "no-such-task", TaskTransition::Start, None),
            Err(TaskTransitionError::NotFound)
        ));
    }

    /// DONE-CRITERION: "action humaine (kind human) vérifiable". A `kind=human` task cannot
    /// be closed through the plain `done` path — it goes through the VERIFIED path, and ONLY
    /// when it is verifiable (a `verification_command` present). This is the guarded human
    /// action verifiability the criterion demands.
    #[test]
    fn human_task_is_verifiable_and_guarded() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();

        // A human task with its verification command; drive it to in_progress.
        let human = create_task(
            &mut conn,
            &project.id,
            "Rotate secret",
            NewTaskInput::human("confirm the app boots with the new secret"),
        )
        .unwrap();
        transition_task(&mut conn, &human.id, TaskTransition::Start, None).unwrap();

        // The plain `done` path is REFUSED for a human task (must be verified).
        assert!(
            matches!(
                transition_task(&mut conn, &human.id, TaskTransition::Done, None),
                Err(TaskTransitionError::HumanRequiresVerification)
            ),
            "a human action cannot be silently marked done via the plain path"
        );

        // The VERIFIED path closes it (it is verifiable — carries a verification_command).
        let verified = verify_human_task(&mut conn, &human.id).unwrap();
        assert_eq!(verified.status, TASK_STATUS_DONE);
        assert!(
            verified.completed_at.is_some(),
            "the verified human task is done"
        );

        // A human task WITHOUT a verification_command is NOT verifiable: the verified path
        // refuses it (there is nothing recording how the action was confirmed).
        let unverifiable = create_task(
            &mut conn,
            &project.id,
            "Do something manual",
            NewTaskInput {
                kind: TASK_KIND_HUMAN.to_string(),
                verification_command: None,
                ..NewTaskInput::default()
            },
        )
        .unwrap();
        transition_task(&mut conn, &unverifiable.id, TaskTransition::Start, None).unwrap();
        assert!(
            matches!(
                verify_human_task(&mut conn, &unverifiable.id),
                Err(TaskTransitionError::HumanRequiresVerification)
            ),
            "a human task with no verification_command is not verifiable"
        );

        // The verified path is human-only: an agent slice cannot use it.
        let slice = create_task(&mut conn, &project.id, "Slice", NewTaskInput::slice()).unwrap();
        transition_task(&mut conn, &slice.id, TaskTransition::Start, None).unwrap();
        assert!(
            matches!(
                verify_human_task(&mut conn, &slice.id),
                Err(TaskTransitionError::HumanRequiresVerification)
            ),
            "an agent slice does not go through the human-verify path"
        );
        // …but an agent slice DOES close through the plain done path.
        let slice_done = transition_task(&mut conn, &slice.id, TaskTransition::Done, None).unwrap();
        assert_eq!(slice_done.status, TASK_STATUS_DONE);
    }

    /// H6: a FORK emits a `spec.forked` domain event carrying the base (`from`) and new
    /// (`to`) revision numbers, so a forked revision becomes VISIBLE in the Activity feed
    /// (`fork_spec` emitted no event before — the F4 coverage gap).
    #[test]
    fn fork_emits_spec_forked_event_with_revision_from_to() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _rev1) = create_spec(
            &mut conn,
            &project.id,
            "Spec",
            SpecSections {
                problem: "P".into(),
                solution: "S".into(),
                ..SpecSections::empty()
            },
        )
        .unwrap();

        fork_spec(&mut conn, &spec.id, Some("scope grew")).unwrap();

        // The Spec-scoped feed surfaces exactly one `spec.forked` event.
        let events = recent_domain_events_for_spec(&mut conn, &spec.id, 50).unwrap();
        let forked = events
            .iter()
            .find(|e| e.event_type == "spec.forked")
            .expect("a spec.forked event was recorded");
        assert_eq!(forked.aggregate, AGGREGATE_SPEC);
        let payload: serde_json::Value = serde_json::from_str(&forked.payload).unwrap();
        assert_eq!(payload["spec_id"], serde_json::json!(spec.id));
        assert_eq!(
            payload["from"],
            serde_json::json!(1),
            "base revision number"
        );
        assert_eq!(payload["to"], serde_json::json!(2), "new revision number");
    }

    /// H6: SET/CLEAR of a Spec's workspace anchor emits a `spec.workspace_changed` event
    /// carrying the workspace `from`/`to` (both nullable — `None` when detached), so a
    /// re-anchoring is VISIBLE in Activity (`set_spec_workspace` emitted no event before).
    #[test]
    fn set_spec_workspace_emits_workspace_changed_event() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        // The standalone anchor is state-guarded to in_progress — drive the Spec there first.
        let spec = spec_in_progress_unanchored(&mut conn, &project.id);

        // ANCHOR: from null → to the workspace.
        set_spec_workspace(&mut conn, &spec.id, Some(&root.id)).unwrap();
        let events = recent_domain_events_for_spec(&mut conn, &spec.id, 50).unwrap();
        let anchored = events
            .iter()
            .find(|e| e.event_type == "spec.workspace_changed")
            .expect("a spec.workspace_changed event was recorded on anchor");
        let p: serde_json::Value = serde_json::from_str(&anchored.payload).unwrap();
        assert_eq!(p["spec_id"], serde_json::json!(spec.id));
        assert!(
            p["from"].is_null(),
            "from is null when the Spec was unanchored"
        );
        assert_eq!(p["to"], serde_json::json!(root.id), "to is the new anchor");

        // DETACH: from the workspace → to null. The newest workspace_changed is the detach.
        set_spec_workspace(&mut conn, &spec.id, None).unwrap();
        let events = recent_domain_events_for_spec(&mut conn, &spec.id, 50).unwrap();
        let detach = events
            .iter()
            .find(|e| e.event_type == "spec.workspace_changed")
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(&detach.payload).unwrap();
        assert_eq!(
            p["from"],
            serde_json::json!(root.id),
            "from is the prior anchor"
        );
        assert!(p["to"].is_null(), "to is null when the Spec detaches");
    }

    /// H7: transition events carry the requested VERB and the block/skip REASON, so moves
    /// that share a target status are distinguishable — a task `skip` (→ cancelled) vs a spec
    /// `cancel` (→ cancelled), and a human `verify` (→ done) vs an agent `done`.
    #[test]
    fn transition_events_carry_verb_and_reason() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(
            &mut conn,
            &project.id,
            "Spec",
            SpecSections {
                problem: "P".into(),
                solution: "S".into(),
                ..SpecSections::empty()
            },
        )
        .unwrap();

        let mk = |conn: &mut SqliteConnection, title: &str, input: NewTaskInput| {
            create_task(
                conn,
                &project.id,
                title,
                NewTaskInput {
                    spec_id: Some(spec.id.clone()),
                    ..input
                },
            )
            .unwrap()
        };

        // A task SKIP (→ cancelled) carries verb="skip" + its reason.
        let t_skip = mk(&mut conn, "Skipme", NewTaskInput::slice());
        transition_task(
            &mut conn,
            &t_skip.id,
            TaskTransition::Skip,
            Some("obsolete"),
        )
        .unwrap();

        // A task BLOCK carries verb="block" + its reason.
        let t_block = mk(&mut conn, "Blockme", NewTaskInput::slice());
        transition_task(
            &mut conn,
            &t_block.id,
            TaskTransition::Block,
            Some("waiting on API"),
        )
        .unwrap();

        // A human task VERIFY (→ done) carries verb="verify".
        let t_human = mk(&mut conn, "Verifyme", NewTaskInput::human("confirm"));
        transition_task(&mut conn, &t_human.id, TaskTransition::Start, None).unwrap();
        verify_human_task(&mut conn, &t_human.id).unwrap();

        // An agent slice DONE (→ done) carries verb="done".
        let t_done = mk(&mut conn, "Doneme", NewTaskInput::slice());
        transition_task(&mut conn, &t_done.id, TaskTransition::Start, None).unwrap();
        transition_task(&mut conn, &t_done.id, TaskTransition::Done, None).unwrap();

        // A spec CANCEL (→ cancelled) carries verb="cancel".
        transition_spec(&mut conn, &spec.id, SpecTransition::Cancel, true).unwrap();

        let events = recent_domain_events_for_spec(&mut conn, &spec.id, 50).unwrap();
        let task_payload = |task_id: &str| -> serde_json::Value {
            let e = events
                .iter()
                .find(|e| {
                    e.event_type == "task.transitioned"
                        && serde_json::from_str::<serde_json::Value>(&e.payload).unwrap()["task_id"]
                            == serde_json::json!(task_id)
                })
                .unwrap();
            serde_json::from_str(&e.payload).unwrap()
        };

        let skip = task_payload(&t_skip.id);
        assert_eq!(skip["verb"], serde_json::json!("skip"));
        assert_eq!(skip["to"], serde_json::json!("cancelled"));
        assert_eq!(
            skip["reason"],
            serde_json::json!("obsolete"),
            "skip reason is carried in the payload"
        );

        let block = task_payload(&t_block.id);
        assert_eq!(block["verb"], serde_json::json!("block"));
        assert_eq!(
            block["reason"],
            serde_json::json!("waiting on API"),
            "block reason is carried in the payload"
        );

        let verify = task_payload(&t_human.id);
        assert_eq!(verify["verb"], serde_json::json!("verify"));
        assert_eq!(verify["to"], serde_json::json!("done"));

        let done = task_payload(&t_done.id);
        assert_eq!(done["verb"], serde_json::json!("done"));
        assert_eq!(done["to"], serde_json::json!("done"));

        // verify vs done: SAME target status, DIFFERENT verb → distinguishable.
        assert_ne!(
            verify["verb"], done["verb"],
            "verify and done are distinguishable by verb"
        );

        // The spec CANCEL: verb="cancel", to="cancelled" — distinguishable from a task skip
        // that also lands on cancelled.
        let spec_ev = events
            .iter()
            .find(|e| e.event_type == "spec.transitioned")
            .unwrap();
        let sp: serde_json::Value = serde_json::from_str(&spec_ev.payload).unwrap();
        assert_eq!(sp["verb"], serde_json::json!("cancel"));
        assert_eq!(sp["to"], serde_json::json!("cancelled"));
        assert_ne!(
            sp["verb"], skip["verb"],
            "spec cancel vs task skip are distinguishable by verb despite the same target"
        );
    }

    /// H8: verifying a human task that was never STARTED is refused with `CannotVerify`
    /// (phrased in terms of the `verify` verb + its source state), NOT the misleading
    /// `InvalidSource`/internal `done` target.
    #[test]
    fn verify_on_non_started_task_is_cannot_verify() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let human = create_task(
            &mut conn,
            &project.id,
            "Rotate secret",
            NewTaskInput::human("confirm"),
        )
        .unwrap();
        // The task is still `todo` (never started) — verifying it is refused with CannotVerify.
        let err = verify_human_task(&mut conn, &human.id).unwrap_err();
        match err {
            TaskTransitionError::CannotVerify { from } => assert_eq!(from, TASK_STATUS_TODO),
            other => panic!("expected CannotVerify, got {other:?}"),
        }
    }

    /// Deleting a project CASCADES to its tasks and their dependency edges (the FK
    /// `tasks.project_id … ON DELETE CASCADE` + `task_dependencies.* … ON DELETE CASCADE`).
    /// Also proves a task's Spec link cascades when the Spec is removed.
    #[test]
    fn deleting_project_cascades_to_tasks_and_dependencies() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "Nyx", "/work", None).unwrap();
        let (spec, _) = create_spec(&mut conn, &project.id, "S", SpecSections::empty()).unwrap();
        let a = create_task(
            &mut conn,
            &project.id,
            "A",
            NewTaskInput {
                spec_id: Some(spec.id.clone()),
                ..NewTaskInput::slice()
            },
        )
        .unwrap();
        let b = create_task(&mut conn, &project.id, "B", NewTaskInput::slice()).unwrap();
        add_task_dependency(&mut conn, &a.id, &b.id, DEPENDENCY_TYPE_BLOCKS).unwrap();

        delete_project(&mut conn, &project.id).unwrap();
        assert!(
            get_task(&mut conn, &a.id).unwrap().is_none(),
            "the task cascaded away with its project"
        );
        assert!(
            list_task_dependencies(&mut conn, &a.id).unwrap().is_empty(),
            "the dependency edges cascaded away too"
        );
    }

    /// v14 migration reverts cleanly (`down.sql` drops `task_dependencies` then `tasks`).
    /// v2.4 Phase 3 landed v15 (the specs.workspace_id index) ON TOP, so we peel v15 first
    /// (the new last link), then v14 — so this still genuinely exercises v14's down.sql.
    #[test]
    fn task_domain_migration_reverts() {
        let mut conn = open_in_memory();
        // Peel v15 (specs.workspace_id index) then v14 (tasks/task_dependencies); both must
        // succeed. Each revert drops the current last link.
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v18 (review_tasks.spec_id) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v17 (review_tasks/review_reads) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v16 (review_comments) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("v15 (specs.workspace_id index) reverts cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("v14 (tasks/task_dependencies) reverts cleanly");
    }

    // --- v2.1 identity authority: create/resolve/archive (task #4) -----------

    /// DONE-CRITERION (task #4): "Créer / résoudre / archiver un projet, un workspace
    /// et un repository passe par nyx, invariants vérifiés." The FULL lifecycle of the
    /// three identities goes through nyx functions. Create → resolve BY STABLE ID (not by
    /// path) → archive (soft, `archived_at`, no row deleted) → the row still resolves by
    /// id while dropping out of the LIVE listing → unarchive restores it.
    #[test]
    fn create_resolve_archive_lifecycle_for_the_three_identities() {
        let mut conn = open_in_memory();

        // CREATE — project (+ its root workspace) through nyx.
        let (project, root) = create_project(&mut conn, "Nyx", p("/n", "C:\\n"), None).unwrap();
        let repo = create_repository(&mut conn, &project.id, "api", "api", false).unwrap();

        // RESOLVE by the STABLE internal id (independent of any disk path).
        assert_eq!(
            get_project(&mut conn, &project.id).unwrap().unwrap().id,
            project.id,
            "a project resolves by its stable id"
        );
        assert_eq!(
            get_workspace(&mut conn, &root.id).unwrap().unwrap().id,
            root.id,
            "a workspace resolves by its stable id"
        );
        assert_eq!(
            get_repository(&mut conn, &repo.id).unwrap().unwrap().id,
            repo.id,
            "a repository resolves by its stable id"
        );

        // ARCHIVE the repository: soft (archived_at set), NOT a delete.
        assert_eq!(archive_repository(&mut conn, &repo.id).unwrap(), 1);
        let archived = get_repository(&mut conn, &repo.id).unwrap().unwrap();
        assert!(
            archived.archived_at.is_some(),
            "archive stamps archived_at, keeping the row resolvable by id"
        );
        assert!(
            list_repositories(&mut conn, &project.id)
                .unwrap()
                .is_empty(),
            "an archived repo drops out of the LIVE listing"
        );
        assert_eq!(
            list_repositories_including_archived(&mut conn, &project.id)
                .unwrap()
                .len(),
            1,
            "…but is still present in the archive-inclusive listing (no delete)"
        );

        // Idempotent archive; then unarchive restores it to the live listing.
        assert_eq!(
            archive_repository(&mut conn, &repo.id).unwrap(),
            0,
            "re-archiving is a no-op (idempotent)"
        );
        assert_eq!(unarchive_repository(&mut conn, &repo.id).unwrap(), 1);
        assert_eq!(
            list_repositories(&mut conn, &project.id).unwrap().len(),
            1,
            "unarchive returns the repo to the live listing"
        );

        // Archive a project / workspace: soft, resolvable by id, no row deleted.
        assert_eq!(archive_workspace(&mut conn, &root.id).unwrap(), 1);
        assert!(get_workspace(&mut conn, &root.id)
            .unwrap()
            .unwrap()
            .archived_at
            .is_some());
        assert_eq!(archive_project(&mut conn, &project.id).unwrap(), 1);
        assert!(get_project(&mut conn, &project.id)
            .unwrap()
            .unwrap()
            .archived_at
            .is_some());
    }

    /// DONE-CRITERION (task #4): invariant — "clé projet unique". nyx derives a
    /// deterministic, disambiguated key on create so the `UNIQUE(projects.key)` invariant
    /// holds even for same-named projects; there is no second authority minting keys.
    #[test]
    fn project_key_is_unique_and_disambiguated_by_nyx() {
        let mut conn = open_in_memory();
        let (a, _) = create_project(&mut conn, "sandbox", p("/a", "C:\\a"), None).unwrap();
        let (b, _) = create_project(&mut conn, "sandbox", p("/b", "C:\\b"), None).unwrap();
        assert_ne!(a.key, b.key, "same-named projects get DISTINCT unique keys");
        assert!(!a.key.is_empty() && !b.key.is_empty(), "keys are NOT NULL");
    }

    /// DONE-CRITERION (task #4): invariant — "primary_workspace_id cohérent". Creation
    /// anchors it at the root workspace; [`set_primary_workspace`] moves it ONLY to a
    /// workspace of the SAME project (a cross-project target is refused, leaving the
    /// pointer coherent). This is the coherence the FK (ON DELETE SET NULL) alone cannot
    /// give.
    #[test]
    fn primary_workspace_id_stays_coherent_through_nyx() {
        let mut conn = open_in_memory();
        let (proj, root) = create_project(&mut conn, "Nyx", p("/n", "C:\\n"), None).unwrap();
        assert_eq!(
            proj.primary_workspace_id.as_deref(),
            Some(root.id.as_str()),
            "creation anchors primary_workspace_id at the root workspace"
        );

        // Moving to another workspace of the SAME project succeeds and repoints.
        let other =
            create_workspace(&mut conn, &proj.id, "feat", p("/n/feat", "C:\\n\\feat")).unwrap();
        assert!(set_primary_workspace(&mut conn, &proj.id, &other.id).unwrap());
        assert_eq!(
            get_project(&mut conn, &proj.id)
                .unwrap()
                .unwrap()
                .primary_workspace_id
                .as_deref(),
            Some(other.id.as_str()),
            "primary_workspace_id moved to the sibling workspace"
        );

        // A workspace of a DIFFERENT project is refused; the pointer is left untouched.
        let (proj2, root2) = create_project(&mut conn, "Other", p("/o", "C:\\o"), None).unwrap();
        assert!(
            !set_primary_workspace(&mut conn, &proj.id, &root2.id).unwrap(),
            "a foreign workspace is refused as primary (invariant guard)"
        );
        assert_eq!(
            get_project(&mut conn, &proj.id)
                .unwrap()
                .unwrap()
                .primary_workspace_id
                .as_deref(),
            Some(other.id.as_str()),
            "the refused move left primary_workspace_id coherent (unchanged)"
        );
        // proj2's own pointer is unaffected.
        assert_eq!(
            proj2.primary_workspace_id.as_deref(),
            Some(root2.id.as_str())
        );
    }

    /// DONE-CRITERION (task #4): "Résolution d'identité stable (id interne) indépendante
    /// des chemins disque." A workspace's `root_path` can change (folder moved/renamed on
    /// disk) yet the SAME stable id keeps resolving the SAME identity — resolution never
    /// keys on the path.
    #[test]
    fn identity_resolution_is_stable_across_a_disk_path_change() {
        let mut conn = open_in_memory();
        let (_proj, root) =
            create_project(&mut conn, "Nyx", p("/old/loc", "C:\\old\\loc"), None).unwrap();
        let id = root.id.clone();

        // Simulate the folder moving on disk: the root_path changes, the id does not.
        diesel::update(workspaces::table.find(&id))
            .set(workspaces::root_path.eq(p("/new/loc", "C:\\new\\loc")))
            .execute(&mut conn)
            .unwrap();

        let resolved = get_workspace(&mut conn, &id).unwrap().unwrap();
        assert_eq!(
            resolved.id, id,
            "the same stable id resolves after a path move"
        );
        assert_eq!(
            resolved.root_path,
            pathnorm::normalize(p("/new/loc", "C:\\new\\loc")),
            "resolution followed the identity, not the old path"
        );
    }

    // --- v2.1 workspace = pivot intention/runtime, no Execution (task #6) -----

    /// DONE-CRITERION (task #6): "Tout le runtime se rattache au workspace ; aucune
    /// entité Execution." The three runtime kinds — a TERMINAL, an AGENT SESSION, and a
    /// COMMAND INSTANCE — all key on `workspace_id` (the pivot), and NO `execution*`
    /// table/column exists anywhere in the schema.
    #[test]
    fn all_runtime_attaches_to_workspace_and_no_execution_entity() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "Nyx", p("/n", "C:\\n"), None).unwrap();

        // A terminal, attached to the workspace (the pivot).
        let term = create_terminal(&mut conn, p("/n", "C:\\n"), None).unwrap();
        attach_terminal(&mut conn, &term.id, &root.id, BINDING_MANUAL).unwrap();
        assert_eq!(
            get_terminal(&mut conn, &term.id)
                .unwrap()
                .unwrap()
                .workspace_id
                .as_deref(),
            Some(root.id.as_str()),
            "a terminal attaches to the workspace_id pivot"
        );

        // An agent session, keyed on the workspace.
        let session = record_session_start(
            &mut conn,
            &term.id,
            AGENT_KIND_CLAUDE_CODE,
            SessionCapture {
                workspace_id: Some(root.id.clone()),
                external_session_id: "ext-1".to_string(),
                cwd: p("/n", "C:\\n").to_string(),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        assert_eq!(
            session.workspace_id.as_deref(),
            Some(root.id.as_str()),
            "an agent session attaches to the workspace_id pivot"
        );

        // A command instance, materialized for the workspace.
        let tmpl = create_template(
            &mut conn,
            &project.id,
            "dev",
            "echo hi",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let insts = list_instances_for_workspace(&mut conn, &root.id).unwrap();
        let inst = insts
            .iter()
            .find(|i| i.command_id == tmpl.id)
            .expect("template materialized an instance for the workspace");
        assert_eq!(
            inst.workspace_id, root.id,
            "a command instance attaches to the workspace_id pivot"
        );

        // NO Execution entity: no table nor column named after it exists in the schema.
        assert!(
            !table_exists(&mut conn, "executions"),
            "there is NO executions table (PDR-0008)"
        );
        assert!(
            !table_exists(&mut conn, "execution"),
            "there is NO execution table (PDR-0008)"
        );
        #[derive(diesel::QueryableByName)]
        struct Cnt {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            #[diesel(column_name = "n")]
            n: i64,
        }
        let has_execution_col: i64 = diesel::sql_query(
            "SELECT COUNT(*) AS n FROM pragma_table_info('command_instances') \
             WHERE name LIKE '%execution%'",
        )
        .get_result::<Cnt>(&mut conn)
        .unwrap()
        .n;
        assert_eq!(
            has_execution_col, 0,
            "no execution* column on the runtime tables (runtime keys on workspace_id)"
        );
    }

    /// DONE-CRITERION (task #6): "folder_removed_at modélisé ; aucune suppression
    /// automatique de ligne workspace." Marking the worktree folder cleaned up stamps
    /// `folder_removed_at` and leaves the ROW (and its runtime) in place — it is NEVER an
    /// auto-delete. The row still resolves by id; clearing the mark reverses it.
    #[test]
    fn folder_removed_at_marks_cleanup_without_deleting_the_row() {
        let mut conn = open_in_memory();
        let (_project, root) = create_project(&mut conn, "Nyx", p("/n", "C:\\n"), None).unwrap();

        // A runtime terminal attached to the workspace, to prove it survives the mark.
        let term = create_terminal(&mut conn, p("/n", "C:\\n"), None).unwrap();
        attach_terminal(&mut conn, &term.id, &root.id, BINDING_MANUAL).unwrap();

        assert!(
            get_workspace(&mut conn, &root.id)
                .unwrap()
                .unwrap()
                .folder_removed_at
                .is_none(),
            "a live workspace has no folder_removed_at"
        );

        // Mark the folder cleaned up.
        assert_eq!(
            mark_workspace_folder_removed(&mut conn, &root.id).unwrap(),
            1
        );
        let after = get_workspace(&mut conn, &root.id).unwrap();
        assert!(
            after.is_some(),
            "the workspace ROW is PERMANENT (not deleted)"
        );
        assert!(
            after.unwrap().folder_removed_at.is_some(),
            "folder_removed_at is stamped to mark the worktree cleanup"
        );
        // The runtime terminal is STILL attached — no auto-delete cascaded.
        assert_eq!(
            get_terminal(&mut conn, &term.id)
                .unwrap()
                .unwrap()
                .workspace_id
                .as_deref(),
            Some(root.id.as_str()),
            "marking the folder removed does NOT auto-delete the workspace or detach runtime"
        );

        // Idempotent; clearing the mark (folder re-materialized) reverses it.
        assert_eq!(
            mark_workspace_folder_removed(&mut conn, &root.id).unwrap(),
            0,
            "re-marking is a no-op (idempotent)"
        );
        assert_eq!(
            clear_workspace_folder_removed(&mut conn, &root.id).unwrap(),
            1
        );
        assert!(get_workspace(&mut conn, &root.id)
            .unwrap()
            .unwrap()
            .folder_removed_at
            .is_none());
    }

    // --- v2.1 v1→canonical DATA migration (task 01KWDCNDJX87BWJ6JMTRSETJYT) --

    /// DONE-CRITERION (task #3): "Les projets/workspaces/runtime v1 existants sont
    /// lisibles sous la forme canonique après migration." This is the GENUINE upgrade
    /// path. We step DOWN to the PRE-canonical (pre-v10) schema — where `workspaces.path`
    /// still exists, `projects` has no `key`/`primary_workspace_id`, and there is no
    /// `repositories` table — insert v1-shaped rows (a project + its root workspace + a
    /// runtime terminal ATTACHED to that workspace) with RAW SQL exactly as a v1 build
    /// would, then run the v10+v11 UPGRADE and assert the rows are now readable through
    /// the canonical model:
    ///   * `workspaces.root_path` carries the OLD `path` value (no loss, canonical name);
    ///   * `projects.key` is backfilled NOT-NULL + UNIQUE;
    ///   * `projects.primary_workspace_id` points at the pre-existing root workspace
    ///     (v11 backfill of the durable primary role);
    ///   * the RUNTIME terminal is STILL attached to the SAME workspace (runtime
    ///     preserved — the rename never moved a runtime row).
    #[test]
    fn v1_data_migrates_to_canonical_form_without_loss() {
        use diesel::sql_query;
        let mut conn = open_in_memory();

        // Step DOWN to the pre-canonical schema: peel v15 (v2.4 specs.workspace_id index)
        // then v14 (v2.4 task domain) then v13 (v2.4 Spec domain) then v12 (Phase 4
        // domain_events) then v11 (data backfill) then v10 (canonical structure). After
        // this, `workspaces.root_path` does NOT exist yet (`path` does), and `projects` has
        // no `key`/`primary_workspace_id`.
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v18 (review_tasks.spec_id) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v17 (review_tasks/review_reads) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v16 (review_comments) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v15 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v14 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v13 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v12 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v11 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v10 cleanly");
        assert!(
            !table_exists(&mut conn, "repositories"),
            "pre-v10 schema has no repositories table"
        );
        let root_path_present: QueryResult<usize> =
            sql_query("SELECT root_path FROM workspaces").execute(&mut conn);
        assert!(
            root_path_present.is_err(),
            "pre-v10 schema still has `path`, not `root_path`"
        );

        // Insert v1-shaped rows via RAW SQL (the diesel models now know the canonical
        // columns; raw SQL writes rows exactly as a pre-v10 build would). A project, its
        // single ROOT workspace at a real path, and a terminal ATTACHED to that workspace.
        let project_id = Uuid::now_v7().to_string();
        let workspace_id = Uuid::now_v7().to_string();
        let terminal_id = Uuid::now_v7().to_string();
        let old_path = "/home/kris/legacy-project";
        sql_query(format!(
            "INSERT INTO projects (id, name) VALUES ('{project_id}', 'Legacy Project')"
        ))
        .execute(&mut conn)
        .expect("insert a pre-v10 project");
        sql_query(format!(
            "INSERT INTO workspaces (id, project_id, name, path, is_root) \
             VALUES ('{workspace_id}', '{project_id}', 'root', '{old_path}', 1)"
        ))
        .execute(&mut conn)
        .expect("insert a pre-v10 root workspace");
        sql_query(format!(
            "INSERT INTO terminals (id, cwd, status, \"order\", workspace_id) \
             VALUES ('{terminal_id}', '{old_path}', 'alive', 0, '{workspace_id}')"
        ))
        .execute(&mut conn)
        .expect("insert a pre-v10 terminal attached to the workspace");

        // UPGRADE: run v10 (structure + key seed + rename) then v11 (primary backfill).
        run_migrations(&mut conn).expect("re-apply v10+v11 (the upgrade)");

        // The project is now readable in canonical form.
        let project = get_project(&mut conn, &project_id)
            .expect("get_project after upgrade")
            .expect("the pre-v10 project survived the upgrade");
        assert!(
            !project.key.is_empty(),
            "key backfilled NOT NULL on upgrade"
        );
        assert_eq!(
            project.primary_workspace_id.as_deref(),
            Some(workspace_id.as_str()),
            "primary_workspace_id backfilled to the pre-existing root workspace (v11)"
        );

        // The workspace's canonical `root_path` carries the OLD `path` value — no loss.
        let ws = list_workspaces(&mut conn, &project_id).unwrap();
        assert_eq!(
            ws.len(),
            1,
            "the single workspace is preserved (no dup/loss)"
        );
        assert_eq!(
            ws[0].root_path, old_path,
            "root_path carries the pre-v10 path value verbatim"
        );
        assert!(ws[0].is_root, "the root role is preserved");

        // The RUNTIME terminal is STILL attached to the SAME workspace (runtime preserved).
        let term = get_terminal(&mut conn, &terminal_id)
            .unwrap()
            .expect("the runtime terminal survived the upgrade");
        assert_eq!(
            term.workspace_id.as_deref(),
            Some(workspace_id.as_str()),
            "the runtime stays attached to its workspace across the canonical migration"
        );
    }

    /// DONE-CRITERION (task #3): "La migration est rejouable (idempotente) sans dupliquer
    /// ni perdre de lignes." After the FULL canonical migration has run once, RE-RUNNING
    /// the v11 backfill statements a second time must be a NO-OP: no row count changes,
    /// no `primary_workspace_id` is repointed, no duplicate is created. We snapshot the
    /// canonical state, replay the exact v11 backfill SQL, and assert the state is
    /// IDENTICAL — the guards (`primary_workspace_id IS NULL`) make the replay converge.
    #[test]
    fn canonical_data_backfill_is_idempotent_on_replay() {
        let mut conn = open_in_memory();

        // A project (its create sets primary_workspace_id) + a second project whose
        // pointer we clear to simulate a pre-v10 project the backfill must fix.
        let (p1, root1) = create_project(&mut conn, "Alpha", p("/a", "C:\\a"), None).unwrap();
        let (p2, root2) = create_project(&mut conn, "Beta", p("/b", "C:\\b"), None).unwrap();
        // Simulate p2 as pre-v10 (pointer NULL) so the backfill has real work to do.
        diesel::update(projects::table.find(&p2.id))
            .set(projects::primary_workspace_id.eq::<Option<String>>(None))
            .execute(&mut conn)
            .unwrap();

        // The v11 backfill SQL (verbatim shape from migrations/..._backfill_canonical_v1_data).
        let backfill = "UPDATE projects \
             SET primary_workspace_id = ( \
                 SELECT w.id FROM workspaces AS w \
                 WHERE w.project_id = projects.id AND w.is_root = 1 LIMIT 1) \
             WHERE primary_workspace_id IS NULL \
               AND EXISTS (SELECT 1 FROM workspaces AS w \
                           WHERE w.project_id = projects.id AND w.is_root = 1)";

        // First application: fixes p2, leaves p1 (already anchored at create) untouched.
        diesel::sql_query(backfill)
            .execute(&mut conn)
            .expect("first backfill");
        let after_first_p1 = get_project(&mut conn, &p1.id).unwrap().unwrap();
        let after_first_p2 = get_project(&mut conn, &p2.id).unwrap().unwrap();
        assert_eq!(
            after_first_p1.primary_workspace_id.as_deref(),
            Some(root1.id.as_str()),
            "p1 stays anchored at its root (backfill skips already-set pointers)"
        );
        assert_eq!(
            after_first_p2.primary_workspace_id.as_deref(),
            Some(root2.id.as_str()),
            "p2's NULL pointer is backfilled to its root workspace"
        );

        // Snapshot counts + pointers, REPLAY the backfill, assert nothing changed.
        let projects_before: i64 = projects::table.count().get_result(&mut conn).unwrap();
        let workspaces_before: i64 = workspaces::table.count().get_result(&mut conn).unwrap();

        diesel::sql_query(backfill)
            .execute(&mut conn)
            .expect("replay backfill (idempotent)");

        let projects_after: i64 = projects::table.count().get_result(&mut conn).unwrap();
        let workspaces_after: i64 = workspaces::table.count().get_result(&mut conn).unwrap();
        assert_eq!(
            projects_before, projects_after,
            "replay dups no project row"
        );
        assert_eq!(
            workspaces_before, workspaces_after,
            "replay dups no workspace row"
        );

        let replay_p1 = get_project(&mut conn, &p1.id).unwrap().unwrap();
        let replay_p2 = get_project(&mut conn, &p2.id).unwrap().unwrap();
        assert_eq!(
            replay_p1.primary_workspace_id, after_first_p1.primary_workspace_id,
            "replay does not repoint p1"
        );
        assert_eq!(
            replay_p2.primary_workspace_id, after_first_p2.primary_workspace_id,
            "replay does not repoint p2 (converges, no change)"
        );
    }

    /// DONE-CRITERION (task #3, replay-safety at the CHAIN level): running the whole
    /// embedded migration chain a SECOND time (a second boot) is a clean no-op that
    /// neither errors nor loses the canonical data. Complements the task-#1 replay seal
    /// by asserting the canonical rows survive a re-run unchanged.
    #[test]
    fn full_chain_replay_preserves_canonical_rows() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "Gamma", p("/g", "C:\\g"), None).unwrap();

        // Re-run the entire chain (idempotent): a second boot must not error.
        run_migrations(&mut conn).expect("replaying the whole chain is a clean no-op");

        // The canonical rows are intact and still readable in canonical form.
        let got = get_project(&mut conn, &project.id).unwrap().unwrap();
        assert_eq!(got.key, project.key, "key unchanged by a chain replay");
        assert_eq!(
            got.primary_workspace_id.as_deref(),
            Some(root.id.as_str()),
            "primary_workspace_id unchanged by a chain replay"
        );
        let ws = list_workspaces(&mut conn, &project.id).unwrap();
        assert_eq!(ws.len(), 1, "no workspace duplicated by a chain replay");
        assert_eq!(
            ws[0].root_path, root.root_path,
            "root_path unchanged by a chain replay"
        );
    }

    /// `set_active` stamps `last_active_at` (greatest = most-recently-activated,
    /// the launcher's restore rule) WITHOUT bumping `updated_at`.
    #[test]
    fn set_active_stamps_last_active_at_without_bumping_updated_at() {
        let mut conn = open_in_memory();
        let a = create_terminal(&mut conn, "/a", None).unwrap();
        let b = create_terminal(&mut conn, "/b", None).unwrap();
        assert_eq!(a.last_active_at, None, "a fresh terminal was never active");
        assert_eq!(b.last_active_at, None);

        set_active(&mut conn, &a.id).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        set_active(&mut conn, &b.id).unwrap();

        let a2 = get_terminal(&mut conn, &a.id).unwrap().unwrap();
        let b2 = get_terminal(&mut conn, &b.id).unwrap().unwrap();
        assert!(
            b2.last_active_at.unwrap() >= a2.last_active_at.unwrap(),
            "the most-recently-activated terminal has the greatest last_active_at"
        );
        assert_eq!(
            a2.updated_at, a.updated_at,
            "set_active is not a content mutation: updated_at must not move"
        );
    }

    /// Mutations on an unknown id affect zero rows (no panic, no spurious insert).
    #[test]
    fn mutations_on_unknown_id_are_noops() {
        let mut conn = open_in_memory();
        assert_eq!(close_terminal(&mut conn, "no-such-id").unwrap(), 0);
        assert_eq!(
            rename(&mut conn, "no-such-id", Some("x".into())).unwrap(),
            0
        );
        assert_eq!(set_active(&mut conn, "no-such-id").unwrap(), 0);
        assert_eq!(
            persist_scrollback(&mut conn, "no-such-id", "data").unwrap(),
            0
        );
        // reorder over absent ids is a silent no-op (does not error).
        reorder(&mut conn, &["no-such-id".to_string(), "nope".to_string()])
            .expect("reorder absent ids");
        assert!(list_terminals(&mut conn).unwrap().is_empty());
    }

    // --- Project / Workspace v2 (PRD-2 Phase 1 done-criteria) -------------

    /// A test path that normalizes deterministically on either platform, so the
    /// stored canonical form is predictable in assertions.
    fn p(unix: &str, win: &str) -> &'static str {
        // Returning &'static via leak keeps the test call-sites terse; the leak
        // is bounded (a handful of literals per test run).
        let _ = (unix, win); // both consumed below depending on platform
        #[cfg(windows)]
        {
            Box::leak(win.to_string().into_boxed_str())
        }
        #[cfg(not(windows))]
        {
            Box::leak(unix.to_string().into_boxed_str())
        }
    }

    /// Migration v2 applied + schema.rs coherent: every column of `projects`,
    /// `workspaces`, and the new `terminals` binding columns round-trips through
    /// a real insert+select against the migrated in-memory DB. A drift between
    /// schema.rs and the SQL migration would fail to compile or to run here.
    #[test]
    fn migration_v2_schema_roundtrips_projects_workspaces_and_terminal_binding() {
        let mut conn = open_in_memory();

        let (project, root) =
            create_project(&mut conn, "demo", p("/home/kris/demo", "C:\\demo"), None).unwrap();
        assert_eq!(project.name, "demo");
        assert!(project.created_at > 0 && project.updated_at == project.created_at);
        assert!(
            root.is_root,
            "the auto-created root workspace is flagged root"
        );
        assert_eq!(root.project_id, project.id);
        assert_eq!(root.branch, None);
        // A fresh project + its root default to OPEN (collapsed = false): a new
        // band starts expanded, and the column round-trips through schema.rs.
        assert!(!project.collapsed, "a fresh project defaults to open");
        assert!(!root.collapsed, "a fresh workspace defaults to open");

        // The new terminal binding columns default correctly (workspace_id NULL,
        // mode 'auto') and then persist an attach.
        let t = create_terminal(&mut conn, "/x", None).unwrap();
        assert_eq!(t.workspace_id, None, "a fresh terminal is unattached");
        assert_eq!(
            t.workspace_binding_mode, BINDING_AUTO,
            "binding mode defaults to auto"
        );
        let n = attach_terminal(&mut conn, &t.id, &root.id, BINDING_MANUAL).unwrap();
        assert_eq!(n, 1);
        let got = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(got.workspace_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(got.workspace_binding_mode, BINDING_MANUAL);
    }

    /// `create_project` creates a project AND exactly ONE root workspace. The
    /// root name defaults to "root" when not provided, and is honored otherwise.
    #[test]
    fn create_project_creates_a_single_root_workspace() {
        let mut conn = open_in_memory();

        let (project, root) =
            create_project(&mut conn, "proj", p("/srv/proj", "D:\\proj"), None).unwrap();
        assert_eq!(
            root.name, DEFAULT_ROOT_WORKSPACE_NAME,
            "default root name is 'root'"
        );

        let ws = list_workspaces(&mut conn, &project.id).unwrap();
        assert_eq!(
            ws.len(),
            1,
            "exactly one workspace exists after create_project"
        );
        assert!(ws[0].is_root, "and it is the root");
        assert_eq!(ws[0].id, root.id);

        // An explicit root name is honored.
        let (proj2, root2) = create_project(
            &mut conn,
            "proj2",
            p("/srv/proj2", "D:\\proj2"),
            Some("main"),
        )
        .unwrap();
        assert_eq!(root2.name, "main");
        let ws2 = list_workspaces(&mut conn, &proj2.id).unwrap();
        assert_eq!(ws2.len(), 1);
        assert_eq!(ws2[0].name, "main");
    }

    /// `create_project` normalizes the root path before storing it: a messy
    /// spelling collapses to the canonical form the resolver later compares.
    #[test]
    fn create_project_normalizes_the_root_path() {
        let mut conn = open_in_memory();
        let raw = p("/home//kris/./demo/", "C:/Home/Kris/Demo/");
        let (_, root) = create_project(&mut conn, "demo", raw, None).unwrap();
        let expected = crate::pathnorm::normalize(raw);
        assert_eq!(
            root.root_path, expected,
            "stored path is the normalized form"
        );
        // And the normalized form has no trailing separator / redundant pieces.
        assert!(!root.root_path.ends_with('/') || root.root_path == "/");
    }

    /// PURE branch parsing: a normal checkout reports a branch name; DETACHED HEAD
    /// reports the literal `HEAD`; blank/whitespace output is "no branch". Trailing
    /// newlines (git always appends one) are trimmed.
    #[test]
    fn parse_branch_maps_detached_and_blank_to_none() {
        assert_eq!(gitbranch::parse_branch("main\n").as_deref(), Some("main"));
        assert_eq!(
            gitbranch::parse_branch("feature/foo\n").as_deref(),
            Some("feature/foo")
        );
        // Detached HEAD: git prints the literal `HEAD` → no branch to record.
        assert_eq!(gitbranch::parse_branch("HEAD\n"), None);
        assert_eq!(gitbranch::parse_branch(""), None);
        assert_eq!(gitbranch::parse_branch("   \n"), None);
    }

    /// Run a git subcommand in `dir`, asserting success. Deterministic identity is
    /// passed via `-c` so the test never depends on the host's git user config.
    /// Commit/tag signing is forced OFF too: a developer host may set
    /// `commit.gpgsign=true` globally, which would make this non-interactive commit
    /// prompt for a signing-key passphrase and fail. The test only needs a commit to
    /// exist, not a signed one.
    #[cfg(test)]
    fn git_in(dir: &std::path::Path, args: &[&str]) {
        let status = crate::proc_util::command("git")
            .args([
                "-c",
                "user.email=test@nyx",
                "-c",
                "user.name=nyx-test",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    /// Make a unique scratch directory under the OS temp dir. Returned path is the
    /// caller's to remove. (We avoid a `tempfile` dev-dep; uniqueness comes from a
    /// v7 UUID so parallel test threads never collide.)
    #[cfg(test)]
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nyx-gitbranch-{tag}-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// `gitbranch::detect` reports the CURRENT HEAD branch of a real git work tree,
    /// and returns `None` for a non-git path. This exercises the actual `git`
    /// subprocess the workspace-creation path runs (the dogfood finding: branch was
    /// always None). The repo is created on a KNOWN branch (`work-prd3`) and a
    /// commit is made so the branch is born (an unborn branch makes `rev-parse`
    /// fail → None, which is not the case under test here).
    ///
    /// Skipped gracefully if `git` is not on PATH (CI image without git) — the
    /// detector is itself git-agnostic, so its contract still holds.
    #[test]
    fn detect_returns_head_branch_for_a_git_repo_and_none_otherwise() {
        if crate::proc_util::command("git")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping detect test: git not available");
            return;
        }

        let repo = scratch_dir("repo");
        let repo_str = repo.to_str().expect("utf8 repo path");

        // Init, pin HEAD to a known branch (portable across git versions that
        // default to `master` vs `main`), then commit so the branch is born.
        git_in(&repo, &["init", "-q"]);
        git_in(&repo, &["symbolic-ref", "HEAD", "refs/heads/work-prd3"]);
        std::fs::write(repo.join("README.md"), b"nyx").expect("seed file");
        git_in(&repo, &["add", "README.md"]);
        git_in(&repo, &["commit", "-q", "-m", "init"]);

        assert_eq!(
            gitbranch::detect(repo_str).as_deref(),
            Some("work-prd3"),
            "a git work tree reports its current HEAD branch"
        );

        // A plain directory (no git) → None.
        let plain = scratch_dir("plain");
        assert_eq!(
            gitbranch::detect(plain.to_str().expect("utf8 plain path")),
            None,
            "a non-git path yields no branch"
        );

        // Best-effort cleanup (ignore failures: temp dir is reclaimed by the OS).
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&plain);
    }

    /// `create_workspace` REFUSES a path already present in the SAME project, but
    /// ACCEPTS the same path in a DIFFERENT project (UNIQUE(project_id, path), not
    /// global). Done-criterion verbatim.
    #[test]
    fn create_workspace_rejects_dup_path_in_same_project_allows_across_projects() {
        let mut conn = open_in_memory();
        let path_a = p("/work/feature", "C:\\work\\feature");

        let (proj1, _) = create_project(&mut conn, "p1", p("/work", "C:\\work"), None).unwrap();
        let (proj2, _) = create_project(&mut conn, "p2", p("/other", "D:\\other"), None).unwrap();

        // First add in p1 succeeds.
        let w1 = create_workspace(&mut conn, &proj1.id, "feat", path_a).unwrap();
        assert!(!w1.is_root, "an added workspace is non-root");

        // Same path AGAIN in p1 (even with a different spelling/casing that
        // normalizes equal) must be rejected by UNIQUE(project_id, path).
        let messy_same = p("/work//feature/", "C:/Work/Feature");
        let dup = create_workspace(&mut conn, &proj1.id, "feat-dup", messy_same);
        assert!(
            dup.is_err(),
            "a duplicate normalized path in the same project must be rejected"
        );

        // The SAME path in a DIFFERENT project is accepted.
        let w_other = create_workspace(&mut conn, &proj2.id, "feat", path_a)
            .expect("same path in another project must be accepted");
        assert_eq!(
            w_other.root_path, w1.root_path,
            "both store the same canonical path"
        );
        assert_ne!(w_other.project_id, w1.project_id);

        // p1 still has exactly root + the one added workspace (no dup leaked in).
        assert_eq!(list_workspaces(&mut conn, &proj1.id).unwrap().len(), 2);
    }

    /// The single-root-per-project constraint is enforced: a second `is_root=1`
    /// workspace in the same project is rejected by the partial unique index. A
    /// root in a DIFFERENT project is fine (the index is per project_id).
    #[test]
    fn single_root_per_project_constraint_is_enforced() {
        let mut conn = open_in_memory();
        let (proj1, _root1) = create_project(&mut conn, "p1", p("/r1", "C:\\r1"), None).unwrap();
        let (proj2, _root2) = create_project(&mut conn, "p2", p("/r2", "C:\\r2"), None).unwrap();

        // Attempt to insert a SECOND root into proj1 directly — must violate the
        // partial unique index `idx_one_root_per_project`.
        let now = now_millis();
        let second_root = diesel::insert_into(workspaces::table)
            .values(NewWorkspace {
                id: Uuid::now_v7().to_string(),
                project_id: proj1.id.clone(),
                name: "root2".into(),
                root_path: pathnorm::normalize(p("/r1/extra", "C:\\r1\\extra")),
                branch: None,
                is_root: true,
                created_at: now,
                updated_at: now,
            })
            .execute(&mut conn);
        assert!(
            second_root.is_err(),
            "a second root in the same project must violate the single-root index"
        );

        // proj2 already has its own root — proving the constraint is PER project
        // (two projects each with a root coexist).
        assert!(list_workspaces(&mut conn, &proj1.id).unwrap()[0].is_root);
        assert!(list_workspaces(&mut conn, &proj2.id).unwrap()[0].is_root);
    }

    /// attach / pin / unpin / detach persist `workspace_id` and
    /// `workspace_binding_mode` correctly. Done-criterion verbatim, one test for
    /// the full pin lifecycle at the data layer.
    #[test]
    fn attach_pin_unpin_detach_persist_workspace_and_mode() {
        let mut conn = open_in_memory();
        let (_proj, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let other = create_workspace(
            &mut conn,
            &root.project_id,
            "feat",
            p("/p/feat", "C:\\p\\feat"),
        )
        .unwrap();
        let t = create_terminal(&mut conn, "/p", None).unwrap();

        // attach with explicit auto mode.
        attach_terminal(&mut conn, &t.id, &root.id, BINDING_AUTO).unwrap();
        let a = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(a.workspace_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(a.workspace_binding_mode, BINDING_AUTO);

        // pin → workspace set + mode manual.
        pin_terminal_workspace(&mut conn, &t.id, &other.id).unwrap();
        let pinned = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(pinned.workspace_id.as_deref(), Some(other.id.as_str()));
        assert_eq!(
            pinned.workspace_binding_mode, BINDING_MANUAL,
            "pin sets manual"
        );

        // unpin → mode back to auto, workspace KEPT (until the resolver moves it).
        unpin_terminal_workspace(&mut conn, &t.id).unwrap();
        let unpinned = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(
            unpinned.workspace_binding_mode, BINDING_AUTO,
            "unpin restores auto"
        );
        assert_eq!(
            unpinned.workspace_id.as_deref(),
            Some(other.id.as_str()),
            "unpin keeps the current workspace; only the mode changes"
        );

        // detach → workspace cleared + mode auto.
        detach_terminal(&mut conn, &t.id).unwrap();
        let detached = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(detached.workspace_id, None, "detach clears the workspace");
        assert_eq!(detached.workspace_binding_mode, BINDING_AUTO);
    }

    /// An invalid binding mode is rejected by the CHECK constraint (defends the
    /// auto|manual invariant the resolver relies on).
    #[test]
    fn invalid_binding_mode_is_rejected() {
        let mut conn = open_in_memory();
        let (_proj, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let t = create_terminal(&mut conn, "/p", None).unwrap();
        let bad = attach_terminal(&mut conn, &t.id, &root.id, "bogus");
        assert!(
            bad.is_err(),
            "binding mode CHECK must reject values outside auto|manual"
        );
    }

    /// Deleting a workspace DETACHES its bound terminals (ON DELETE SET NULL),
    /// rather than deleting the terminal record — terminals outlive a workspace
    /// removal.
    #[test]
    fn deleting_a_workspace_detaches_its_terminals() {
        let mut conn = open_in_memory();
        let (_proj, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let feat = create_workspace(
            &mut conn,
            &root.project_id,
            "feat",
            p("/p/feat", "C:\\p\\feat"),
        )
        .unwrap();
        let t = create_terminal(&mut conn, "/p/feat", None).unwrap();
        attach_terminal(&mut conn, &t.id, &feat.id, BINDING_MANUAL).unwrap();

        diesel::delete(workspaces::table.find(&feat.id))
            .execute(&mut conn)
            .expect("delete workspace");

        let after = get_terminal(&mut conn, &t.id)
            .unwrap()
            .expect("terminal survives");
        assert_eq!(
            after.workspace_id, None,
            "deleting the workspace sets the terminal's workspace_id to NULL"
        );
    }

    /// `update_project` renames the project's display name (persists + bumps
    /// `updated_at`) without touching its workspaces or paths. Done-criterion:
    /// "rename a project display name (persists + reflected)".
    #[test]
    fn update_project_renames_and_bumps_updated_at() {
        let mut conn = open_in_memory();
        let (project, root) =
            create_project(&mut conn, "old-name", p("/proj", "C:\\proj"), None).unwrap();
        assert_eq!(project.name, "old-name");

        std::thread::sleep(std::time::Duration::from_millis(2));
        let n = update_project(&mut conn, &project.id, "new-name").expect("update_project");
        assert_eq!(n, 1, "exactly one project row renamed");

        let listed = list_projects(&mut conn).unwrap();
        let got = listed.iter().find(|pr| pr.id == project.id).unwrap();
        assert_eq!(got.name, "new-name", "the rename is persisted");
        assert!(
            got.updated_at > project.updated_at,
            "a rename bumps updated_at"
        );
        assert_eq!(
            got.created_at, project.created_at,
            "created_at is immutable"
        );
        // The root workspace and its path are untouched by a project rename.
        let ws = list_workspaces(&mut conn, &project.id).unwrap();
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].id, root.id);
        assert_eq!(
            ws[0].root_path, root.root_path,
            "rename does not touch the path"
        );

        // Renaming an unknown id is a no-op (0 rows), not an error.
        assert_eq!(update_project(&mut conn, "no-such-id", "x").unwrap(), 0);
    }

    /// `delete_project` removes the project AND its workspaces (ON DELETE
    /// CASCADE), but terminals bound to those workspaces SURVIVE — they are
    /// DETACHED (workspace_id → NULL via ON DELETE SET NULL), not killed.
    /// Done-criterion verbatim: "its workspaces removed, its terminals become
    /// loose/unattached and survive".
    #[test]
    fn delete_project_removes_workspaces_and_detaches_but_keeps_terminals() {
        let mut conn = open_in_memory();
        let (project, root) =
            create_project(&mut conn, "doomed", p("/doomed", "C:\\doomed"), None).unwrap();
        let feat = create_workspace(
            &mut conn,
            &project.id,
            "feat",
            p("/doomed/feat", "C:\\doomed\\feat"),
        )
        .unwrap();

        // Two terminals bound to two of the project's workspaces (root + feat),
        // plus an UNRELATED terminal that must be left entirely alone.
        let t_root = create_terminal(&mut conn, "/doomed", None).unwrap();
        let t_feat = create_terminal(&mut conn, "/doomed/feat", None).unwrap();
        let t_other = create_terminal(&mut conn, "/elsewhere", None).unwrap();
        attach_terminal(&mut conn, &t_root.id, &root.id, BINDING_MANUAL).unwrap();
        attach_terminal(&mut conn, &t_feat.id, &feat.id, BINDING_AUTO).unwrap();

        // A SECOND project (with its own root + a terminal) to prove delete is
        // scoped to ONE project — the other project and its bindings survive.
        let (other_proj, other_root) =
            create_project(&mut conn, "survivor", p("/survivor", "C:\\survivor"), None).unwrap();
        let t_survivor = create_terminal(&mut conn, "/survivor", None).unwrap();
        attach_terminal(&mut conn, &t_survivor.id, &other_root.id, BINDING_MANUAL).unwrap();

        // Delete the doomed project.
        let n = delete_project(&mut conn, &project.id).expect("delete_project");
        assert_eq!(n, 1, "exactly one project row deleted");

        // The project is gone from the list.
        assert!(
            list_projects(&mut conn)
                .unwrap()
                .iter()
                .all(|pr| pr.id != project.id),
            "the deleted project no longer lists"
        );
        // Its workspaces cascaded away (none remain for that project id).
        assert!(
            list_workspaces(&mut conn, &project.id).unwrap().is_empty(),
            "the project's workspaces are removed by ON DELETE CASCADE"
        );

        // Its terminals SURVIVE as records but are now DETACHED (workspace_id NULL).
        let r1 = get_terminal(&mut conn, &t_root.id)
            .unwrap()
            .expect("root-bound terminal survives the project delete");
        let r2 = get_terminal(&mut conn, &t_feat.id)
            .unwrap()
            .expect("feat-bound terminal survives the project delete");
        assert_eq!(
            r1.workspace_id, None,
            "deleting the project detached the root-bound terminal (SET NULL), not deleted it"
        );
        assert_eq!(
            r2.workspace_id, None,
            "deleting the project detached the feat-bound terminal (SET NULL), not deleted it"
        );
        // They are still ALIVE — a project delete does NOT close terminals.
        assert_eq!(r1.status, STATUS_ALIVE, "detached terminal stays alive");
        assert_eq!(r2.status, STATUS_ALIVE, "detached terminal stays alive");

        // The unrelated terminal is entirely untouched (never bound, still there).
        let other = get_terminal(&mut conn, &t_other.id).unwrap().unwrap();
        assert_eq!(other.workspace_id, None);
        assert_eq!(other.status, STATUS_ALIVE);

        // The OTHER project, its root workspace, and its bound terminal all survive.
        assert!(
            list_projects(&mut conn)
                .unwrap()
                .iter()
                .any(|pr| pr.id == other_proj.id),
            "an unrelated project must survive deleting a different one"
        );
        let survivor = get_terminal(&mut conn, &t_survivor.id).unwrap().unwrap();
        assert_eq!(
            survivor.workspace_id.as_deref(),
            Some(other_root.id.as_str()),
            "the other project's terminal keeps its binding (delete is scoped)"
        );

        // Deleting an unknown id is a no-op (0 rows), not an error.
        assert_eq!(delete_project(&mut conn, "no-such-id").unwrap(), 0);
    }

    /// `rename_workspace` persists a new display name without touching the path.
    #[test]
    fn rename_workspace_persists_name_keeps_path() {
        let mut conn = open_in_memory();
        let (project, root) =
            create_project(&mut conn, "p", p("/p", "C:\\p"), Some("main")).unwrap();
        let feat =
            create_workspace(&mut conn, &project.id, "feat", p("/p/feat", "C:\\p\\feat")).unwrap();

        // Rename the non-root workspace; the path stays identical.
        let n = rename_workspace(&mut conn, &feat.id, "frontend").expect("rename_workspace");
        assert_eq!(n, 1);
        let listed = list_workspaces(&mut conn, &project.id).unwrap();
        let got = listed.iter().find(|w| w.id == feat.id).unwrap();
        assert_eq!(got.name, "frontend", "rename persists the new name");
        assert_eq!(
            got.root_path, feat.root_path,
            "rename does not touch the path"
        );

        // Rename the root too (the editable "main" relabel).
        rename_workspace(&mut conn, &root.id, "primary").unwrap();
        let listed = list_workspaces(&mut conn, &project.id).unwrap();
        assert_eq!(
            listed.iter().find(|w| w.id == root.id).unwrap().name,
            "primary"
        );

        // Unknown id → no-op.
        assert_eq!(rename_workspace(&mut conn, "no-such-id", "x").unwrap(), 0);
    }

    /// `set_project_collapsed` / `set_workspace_collapsed` PERSIST the sidebar
    /// open/closed state and `list_projects`/`list_workspaces` read it back — the
    /// round-trip the sidebar relies on to restore disclosure across a restart.
    #[test]
    fn set_collapsed_persists_for_project_and_workspace() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let feat =
            create_workspace(&mut conn, &project.id, "feat", p("/p/feat", "C:\\p\\feat")).unwrap();

        // Everything starts OPEN (collapsed = false) by column default.
        assert!(!project.collapsed && !root.collapsed && !feat.collapsed);

        // Collapse the project; a re-list reflects it without touching workspaces.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let n = set_project_collapsed(&mut conn, &project.id, true).expect("collapse project");
        assert_eq!(n, 1, "exactly one project row updated");
        let got = list_projects(&mut conn)
            .unwrap()
            .into_iter()
            .find(|pr| pr.id == project.id)
            .unwrap();
        assert!(got.collapsed, "the project's collapsed state is persisted");
        assert!(
            got.updated_at > project.updated_at,
            "persisting collapse bumps updated_at"
        );

        // Re-open the project (idempotent toggle back to false).
        set_project_collapsed(&mut conn, &project.id, false).unwrap();
        assert!(
            !list_projects(&mut conn)
                .unwrap()
                .into_iter()
                .find(|pr| pr.id == project.id)
                .unwrap()
                .collapsed,
            "the project can be re-opened (collapsed back to false)"
        );

        // Collapse one workspace; the OTHER workspace's state is untouched.
        set_workspace_collapsed(&mut conn, &feat.id, true).expect("collapse workspace");
        let workspaces = list_workspaces(&mut conn, &project.id).unwrap();
        let got_feat = workspaces.iter().find(|w| w.id == feat.id).unwrap();
        let got_root = workspaces.iter().find(|w| w.id == root.id).unwrap();
        assert!(
            got_feat.collapsed,
            "the workspace's collapsed state persists"
        );
        assert!(!got_root.collapsed, "a sibling workspace is left open");
        assert_eq!(
            got_feat.root_path, feat.root_path,
            "persisting collapse never touches the path"
        );

        // Unknown ids are no-ops (0 rows), not errors.
        assert_eq!(
            set_project_collapsed(&mut conn, "no-such-id", true).unwrap(),
            0
        );
        assert_eq!(
            set_workspace_collapsed(&mut conn, "no-such-id", true).unwrap(),
            0
        );
    }

    /// The `collapsed` CHECK constraint rejects values outside 0/1 (defends the
    /// boolean invariant the Diesel `Bool` mapping assumes), on BOTH tables.
    #[test]
    fn collapsed_check_constraint_enforced_on_both_tables() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();

        let bad_project = diesel::sql_query(format!(
            "UPDATE projects SET collapsed = 2 WHERE id = '{}'",
            project.id
        ))
        .execute(&mut conn);
        assert!(
            bad_project.is_err(),
            "projects.collapsed CHECK must reject values outside 0/1"
        );

        let feat =
            create_workspace(&mut conn, &project.id, "feat", p("/p/feat", "C:\\p\\feat")).unwrap();
        let bad_ws = diesel::sql_query(format!(
            "UPDATE workspaces SET collapsed = 2 WHERE id = '{}'",
            feat.id
        ))
        .execute(&mut conn);
        assert!(
            bad_ws.is_err(),
            "workspaces.collapsed CHECK must reject values outside 0/1"
        );
    }

    /// Migration v2 down→up reversibility: revert v2, the new tables/columns are
    /// gone, re-apply, and the schema round-trips again.
    ///
    /// History: every later PRD stacked another migration on top of v2 (PRD-3 → v3,
    /// PRD-4 → v4/v5, PRD-5 → v7/v8), so to still target v2's down→up specifically we
    /// peel ALL the migrations above v2 first (in reverse order), THEN revert v2 —
    /// only after that is `projects` gone. Re-applying the whole chain restores a
    /// working schema.
    #[test]
    fn migration_v2_down_then_up_recreates_working_schema() {
        let mut conn = open_in_memory();

        // Peel every migration stacked above the projects migration (dirs #15 → #3),
        // then the projects migration (dir #2) itself — 14 reverts in reverse order
        // (v2.4 Phase 1 added dir #13 the Spec domain; Phase 2 added dir #14 the task
        // domain; Phase 3 added dir #15 the specs.workspace_id index). projects must then
        // be absent.
        for _ in 0..17 {
            conn.revert_last_migration(MIGRATIONS)
                .expect("revert migration cleanly down to (and including) projects");
        }
        let after_down: QueryResult<i64> = projects::table.count().get_result(&mut conn);
        assert!(
            after_down.is_err(),
            "after reverting v2, the projects table must be gone"
        );

        // Re-apply and round-trip a project + root again.
        run_migrations(&mut conn).expect("re-apply v2");
        let (_p, root) = create_project(&mut conn, "revived", p("/rev", "C:\\rev"), None)
            .expect("create_project after down→up");
        assert!(root.is_root);
    }

    // --- Managed command / instance v3 (PRD-3 Phase 1 done-criteria) ------

    /// Migration v3 applied + schema.rs coherent: EVERY column of BOTH new tables
    /// round-trips through a real insert+select against the migrated in-memory DB.
    /// A drift between schema.rs and the SQL migration would fail to compile or to
    /// run here. Covers done-criterion "Les deux tables sont creees et chaque
    /// colonne round-trip" + "Source fields package.json + package_manager
    /// round-trippent".
    #[test]
    fn migration_v3_schema_roundtrips_commands_and_instances() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();

        // A template with a full set of distinct non-default values in every column
        // (incl. subfolder, source provenance, package_manager) so a mismatch
        // surfaces as wrong data, not just a missing column.
        let tpl = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            Some("frontend"),
            CommandSource {
                source_kind: Some(SOURCE_KIND_PACKAGE_JSON.to_string()),
                source_package_json_path: Some("frontend/package.json".to_string()),
                source_script_name: Some("dev".to_string()),
                source_script_command_snapshot: Some("vite".to_string()),
                package_manager: Some("pnpm".to_string()),
            },
        )
        .expect("create_template");

        let got = get_template(&mut conn, &tpl.id).unwrap().unwrap();
        assert_eq!(got, tpl, "select returns the inserted template verbatim");
        assert_eq!(got.project_id, project.id);
        assert_eq!(got.name, "dev");
        assert_eq!(got.command, "npm run dev");
        assert_eq!(got.subfolder.as_deref(), Some("frontend"));
        assert!(!got.restart_on_startup, "restart defaults to false");
        assert_eq!(got.order_index, 0, "first template gets order 0");
        assert!(got.created_at > 0 && got.updated_at == got.created_at);
        // Source provenance + package_manager round-trip.
        assert_eq!(got.source_kind.as_deref(), Some(SOURCE_KIND_PACKAGE_JSON));
        assert_eq!(
            got.source_package_json_path.as_deref(),
            Some("frontend/package.json")
        );
        assert_eq!(got.source_script_name.as_deref(), Some("dev"));
        assert_eq!(got.source_script_command_snapshot.as_deref(), Some("vite"));
        assert_eq!(got.package_manager.as_deref(), Some("pnpm"));

        // A hand-authored template leaves all source columns NULL (and subfolder).
        let plain = create_template(
            &mut conn,
            &project.id,
            "build",
            "make",
            None,
            CommandSource::default(),
        )
        .expect("create_template plain");
        assert_eq!(plain.subfolder, None);
        assert_eq!(plain.source_kind, None);
        assert_eq!(plain.source_package_json_path, None);
        assert_eq!(plain.source_script_name, None);
        assert_eq!(plain.source_script_command_snapshot, None);
        assert_eq!(plain.package_manager, None);
        assert_eq!(
            plain.order_index, 1,
            "second template appends after the first"
        );

        // The instance materialized for `tpl` on the root workspace (auto-created
        // by `create_template`) round-trips every column with its defaults.
        let inst = insert_instance(&mut conn, &tpl.id, &root.id);
        let got_inst = get_instance(&mut conn, &inst.id).unwrap().unwrap();
        assert_eq!(
            got_inst, inst,
            "select returns the inserted instance verbatim"
        );
        assert_eq!(got_inst.command_id, tpl.id);
        assert_eq!(got_inst.workspace_id, root.id);
        assert_eq!(
            got_inst.last_state, STATE_IDLE,
            "last_state defaults to idle"
        );
        assert_eq!(got_inst.scrollback, "", "scrollback defaults to empty");
        assert!(
            !got_inst.was_running_on_shutdown,
            "was_running_on_shutdown defaults to false"
        );
        // v4 outcome columns default to "never finished, already seen".
        assert_eq!(
            got_inst.last_exit_code, None,
            "last_exit_code defaults to NULL (never finished)"
        );
        assert_eq!(
            got_inst.ended_at, None,
            "ended_at defaults to NULL (never finished)"
        );
        assert!(
            !got_inst.unread,
            "unread defaults to false (no unseen result)"
        );
        assert!(got_inst.created_at > 0 && got_inst.updated_at == got_inst.created_at);
    }

    /// A small helper: ENSURE one instance of `command_id` for `workspace_id`
    /// exists and return it. Because `create_template`/`create_workspace` now
    /// auto-materialize instances, the pair may already exist; this helper is
    /// idempotent (insert ON CONFLICT DO NOTHING, then fetch) so the
    /// constraint/helper tests can grab a known instance regardless of whether
    /// materialization already created it.
    #[cfg(test)]
    fn insert_instance(
        conn: &mut SqliteConnection,
        command_id: &str,
        workspace_id: &str,
    ) -> CommandInstance {
        let now = now_millis();
        diesel::insert_into(command_instances::table)
            .values(NewCommandInstance {
                id: Uuid::now_v7().to_string(),
                command_id: command_id.to_string(),
                workspace_id: workspace_id.to_string(),
                created_at: now,
                updated_at: now,
            })
            .on_conflict((
                command_instances::command_id,
                command_instances::workspace_id,
            ))
            .do_nothing()
            .execute(conn)
            .expect("insert instance");
        command_instances::table
            .filter(command_instances::command_id.eq(command_id))
            .filter(command_instances::workspace_id.eq(workspace_id))
            .select(CommandInstance::as_select())
            .first(conn)
            .expect("fetch instance")
    }

    /// `UNIQUE(project_id, name)` rejects two templates of the same name in ONE
    /// project but accepts the same name in a DIFFERENT project. Done-criterion
    /// verbatim: "UNIQUE(project_id,name) bloque deux commandes du meme nom dans
    /// un projet".
    #[test]
    fn template_name_unique_per_project() {
        let mut conn = open_in_memory();
        let (p1, _) = create_project(&mut conn, "p1", p("/p1", "C:\\p1"), None).unwrap();
        let (p2, _) = create_project(&mut conn, "p2", p("/p2", "C:\\p2"), None).unwrap();

        create_template(
            &mut conn,
            &p1.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .expect("first dev template");
        let dup = create_template(
            &mut conn,
            &p1.id,
            "dev",
            "different command",
            None,
            CommandSource::default(),
        );
        assert!(
            dup.is_err(),
            "a duplicate template name in the same project must be rejected"
        );

        // The SAME name in another project is fine.
        create_template(
            &mut conn,
            &p2.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .expect("same name in another project is allowed");

        assert_eq!(list_templates(&mut conn, &p1.id).unwrap().len(), 1);
        assert_eq!(list_templates(&mut conn, &p2.id).unwrap().len(), 1);
    }

    /// `UNIQUE(command_id, workspace_id)` rejects a second instance of the SAME
    /// template in the SAME workspace (the idempotency guard) but accepts the same
    /// template in a DIFFERENT workspace and a different template in the same
    /// workspace. Done-criterion verbatim.
    #[test]
    fn instance_unique_per_command_and_workspace() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let feat =
            create_workspace(&mut conn, &project.id, "feat", p("/p/feat", "C:\\p\\feat")).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let build = create_template(
            &mut conn,
            &project.id,
            "build",
            "make",
            None,
            CommandSource::default(),
        )
        .unwrap();

        insert_instance(&mut conn, &dev.id, &root.id);

        // Same (template, workspace) again → rejected.
        let now = now_millis();
        let dup = diesel::insert_into(command_instances::table)
            .values(NewCommandInstance {
                id: Uuid::now_v7().to_string(),
                command_id: dev.id.clone(),
                workspace_id: root.id.clone(),
                created_at: now,
                updated_at: now,
            })
            .execute(&mut conn);
        assert!(
            dup.is_err(),
            "a second instance of the same template in the same workspace must be rejected"
        );

        // Same template in a DIFFERENT workspace is fine.
        insert_instance(&mut conn, &dev.id, &feat.id);
        // A DIFFERENT template in the SAME workspace is fine.
        insert_instance(&mut conn, &build.id, &root.id);
    }

    /// Invalid foreign keys are rejected: an instance referencing an unknown
    /// template or an unknown workspace fails (PRAGMA foreign_keys = ON). A
    /// template referencing an unknown project also fails. Done-criterion: "FK
    /// invalides echouent".
    #[test]
    fn invalid_foreign_keys_are_rejected() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let now = now_millis();

        // Template with an unknown project_id → FK violation.
        let bad_tpl = diesel::insert_into(managed_commands::table)
            .values(NewManagedCommand {
                id: Uuid::now_v7().to_string(),
                project_id: "no-such-project".to_string(),
                name: "x".to_string(),
                command: "echo".to_string(),
                subfolder: None,
                restart_on_startup: false,
                order_index: 0,
                created_at: now,
                updated_at: now,
                source_kind: None,
                source_package_json_path: None,
                source_script_name: None,
                source_script_command_snapshot: None,
                package_manager: None,
            })
            .execute(&mut conn);
        assert!(
            bad_tpl.is_err(),
            "managed_commands.project_id FK must reject an unknown project"
        );

        // Instance with an unknown command_id → FK violation.
        let bad_cmd = diesel::insert_into(command_instances::table)
            .values(NewCommandInstance {
                id: Uuid::now_v7().to_string(),
                command_id: "no-such-command".to_string(),
                workspace_id: root.id.clone(),
                created_at: now,
                updated_at: now,
            })
            .execute(&mut conn);
        assert!(
            bad_cmd.is_err(),
            "command_instances.command_id FK must reject an unknown template"
        );

        // Instance with an unknown workspace_id → FK violation.
        let bad_ws = diesel::insert_into(command_instances::table)
            .values(NewCommandInstance {
                id: Uuid::now_v7().to_string(),
                command_id: dev.id.clone(),
                workspace_id: "no-such-workspace".to_string(),
                created_at: now,
                updated_at: now,
            })
            .execute(&mut conn);
        assert!(
            bad_ws.is_err(),
            "command_instances.workspace_id FK must reject an unknown workspace"
        );
    }

    /// Deleting a template, a workspace, OR a project CASCADES its instances away.
    /// Done-criterion verbatim: "delete template/workspace/project cascade les
    /// instances".
    #[test]
    fn delete_cascades_instances() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let feat =
            create_workspace(&mut conn, &project.id, "feat", p("/p/feat", "C:\\p\\feat")).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let build = create_template(
            &mut conn,
            &project.id,
            "build",
            "make",
            None,
            CommandSource::default(),
        )
        .unwrap();

        // Materialize a full grid: 2 templates × 2 workspaces = 4 instances.
        let i_dev_root = insert_instance(&mut conn, &dev.id, &root.id);
        let i_dev_feat = insert_instance(&mut conn, &dev.id, &feat.id);
        let i_build_root = insert_instance(&mut conn, &build.id, &root.id);
        let i_build_feat = insert_instance(&mut conn, &build.id, &feat.id);
        let count = |c: &mut SqliteConnection| -> i64 {
            command_instances::table.count().get_result(c).unwrap()
        };
        assert_eq!(count(&mut conn), 4);

        // Delete one TEMPLATE: only its two instances cascade away.
        assert_eq!(delete_template(&mut conn, &dev.id).unwrap(), 1);
        assert_eq!(
            count(&mut conn),
            2,
            "deleting a template removes its instances"
        );
        assert!(get_instance(&mut conn, &i_dev_root.id).unwrap().is_none());
        assert!(get_instance(&mut conn, &i_dev_feat.id).unwrap().is_none());
        assert!(get_instance(&mut conn, &i_build_root.id).unwrap().is_some());

        // Delete one WORKSPACE: the build instance in that workspace cascades.
        diesel::delete(workspaces::table.find(&feat.id))
            .execute(&mut conn)
            .unwrap();
        assert_eq!(
            count(&mut conn),
            1,
            "deleting a workspace removes its instances"
        );
        assert!(get_instance(&mut conn, &i_build_feat.id).unwrap().is_none());
        assert!(get_instance(&mut conn, &i_build_root.id).unwrap().is_some());

        // Delete the PROJECT: its templates (and remaining instances) all cascade.
        delete_project(&mut conn, &project.id).unwrap();
        assert_eq!(
            count(&mut conn),
            0,
            "deleting the project removes all instances"
        );
        assert!(list_templates(&mut conn, &project.id).unwrap().is_empty());
    }

    /// The `last_state` CHECK rejects anything outside idle|running|success|error.
    /// Done-criterion verbatim: "CHECK last_state rejette hors
    /// idle/running/success/error". Also proves `set_last_state` round-trips the
    /// four valid states.
    #[test]
    fn last_state_check_and_set_last_state_roundtrip() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let inst = insert_instance(&mut conn, &dev.id, &root.id);

        // Each valid state round-trips.
        for state in [STATE_IDLE, STATE_RUNNING, STATE_SUCCESS, STATE_ERROR] {
            assert_eq!(set_last_state(&mut conn, &inst.id, state).unwrap(), 1);
            assert_eq!(
                get_instance(&mut conn, &inst.id)
                    .unwrap()
                    .unwrap()
                    .last_state,
                state,
                "set_last_state must persist {state}"
            );
        }

        // An invalid state is rejected by the CHECK constraint.
        let bad = set_last_state(&mut conn, &inst.id, "bogus");
        assert!(
            bad.is_err(),
            "last_state CHECK must reject values outside the enum"
        );
        // A raw insert with a bad default-overriding state is also rejected.
        let now = now_millis();
        let bad_insert = diesel::insert_into(command_instances::table)
            .values((
                command_instances::id.eq(Uuid::now_v7().to_string()),
                command_instances::command_id.eq(&dev.id),
                command_instances::workspace_id.eq(&root.id),
                command_instances::last_state.eq("weird"),
                command_instances::created_at.eq(now),
                command_instances::updated_at.eq(now),
            ))
            .execute(&mut conn);
        assert!(
            bad_insert.is_err(),
            "an out-of-enum last_state insert must be rejected"
        );
    }

    /// v4: `set_run_state` records the FACTUAL outcome (last_state + exit code +
    /// ended_at + unread) on a finish, and `acknowledge_instance` clears ONLY the
    /// `unread` flag — the outcome (last_state / last_exit_code / ended_at) is
    /// preserved. This is the persisted half of the fix: a UI ack can no longer
    /// erase the error an observer (the MCP) reads. Also proves the `running`
    /// transition clears a stale code and that `idle` is outcome-free.
    #[test]
    fn set_run_state_and_acknowledge_decouple_outcome_from_unread() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let inst = insert_instance(&mut conn, &dev.id, &root.id);
        let reload = |c: &mut SqliteConnection| get_instance(c, &inst.id).unwrap().unwrap();

        // A non-zero finish records the full outcome + flags the unseen result.
        assert_eq!(
            set_run_state(&mut conn, &inst.id, STATE_ERROR, Some(7)).unwrap(),
            1
        );
        let after_err = reload(&mut conn);
        assert_eq!(after_err.last_state, STATE_ERROR);
        assert_eq!(after_err.last_exit_code, Some(7), "exit code persisted");
        assert!(after_err.ended_at.is_some(), "ended_at stamped on finish");
        assert!(after_err.unread, "a finished run is an unseen result");

        // ACKNOWLEDGE: clears only `unread` — the outcome is untouched (the crux of
        // the finding: the MCP must still see state=error + exit_code=7 after an ack).
        assert_eq!(
            acknowledge_instance(&mut conn, &inst.id).unwrap(),
            1,
            "ack clears the unread flag of an unseen result"
        );
        let after_ack = reload(&mut conn);
        assert!(!after_ack.unread, "ack clears the unread flag");
        assert_eq!(
            after_ack.last_state, STATE_ERROR,
            "ack must NOT erase the factual state"
        );
        assert_eq!(
            after_ack.last_exit_code,
            Some(7),
            "ack must NOT erase the factual exit code"
        );
        assert_eq!(
            after_ack.ended_at, after_err.ended_at,
            "ack must NOT erase ended_at"
        );

        // A second ack is a no-op (already read): 0 rows touched.
        assert_eq!(
            acknowledge_instance(&mut conn, &inst.id).unwrap(),
            0,
            "ack on an already-read row touches nothing"
        );

        // A fresh `running` start clears the stale code/ended_at but leaves unread
        // as-is (a start is not an unseen result yet).
        assert_eq!(
            set_run_state(&mut conn, &inst.id, STATE_RUNNING, None).unwrap(),
            1
        );
        let after_run = reload(&mut conn);
        assert_eq!(after_run.last_state, STATE_RUNNING);
        assert_eq!(
            after_run.last_exit_code, None,
            "a fresh run clears the prior exit code"
        );
        assert_eq!(after_run.ended_at, None, "a fresh run clears ended_at");
        assert!(!after_run.unread, "running is not yet an unseen result");

        // A clean (exit 0) finish records code 0 + unread again.
        assert_eq!(
            set_run_state(&mut conn, &inst.id, STATE_SUCCESS, Some(0)).unwrap(),
            1
        );
        let after_ok = reload(&mut conn);
        assert_eq!(after_ok.last_state, STATE_SUCCESS);
        assert_eq!(after_ok.last_exit_code, Some(0), "clean exit records 0");
        assert!(after_ok.unread, "a fresh finish is unseen again");

        // `idle` (a stop / boot-normalize) is outcome-free: only last_state moves.
        assert_eq!(
            set_run_state(&mut conn, &inst.id, STATE_IDLE, None).unwrap(),
            1
        );
        let after_idle = reload(&mut conn);
        assert_eq!(after_idle.last_state, STATE_IDLE);

        // The `unread` CHECK rejects out-of-domain values via a raw update.
        let bad = diesel::sql_query(format!(
            "UPDATE command_instances SET unread = 2 WHERE id = '{}'",
            inst.id
        ))
        .execute(&mut conn);
        assert!(bad.is_err(), "unread CHECK must reject values outside 0|1");
    }

    /// `set_was_running_on_shutdown` round-trips and can be reset after boot.
    /// Done-criterion verbatim: "was_running_on_shutdown round-trip et peut etre
    /// reset apres boot".
    #[test]
    fn was_running_on_shutdown_roundtrips_and_resets() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let inst = insert_instance(&mut conn, &dev.id, &root.id);
        assert!(
            !inst.was_running_on_shutdown,
            "a fresh instance was not running on shutdown"
        );

        // Shutdown flow sets it true.
        assert_eq!(
            set_was_running_on_shutdown(&mut conn, &inst.id, true).unwrap(),
            1
        );
        assert!(
            get_instance(&mut conn, &inst.id)
                .unwrap()
                .unwrap()
                .was_running_on_shutdown,
            "the snapshot flag persists true"
        );

        // Boot flow resets it false.
        assert_eq!(
            set_was_running_on_shutdown(&mut conn, &inst.id, false).unwrap(),
            1
        );
        assert!(
            !get_instance(&mut conn, &inst.id)
                .unwrap()
                .unwrap()
                .was_running_on_shutdown,
            "the snapshot flag can be reset after boot"
        );

        // The flag CHECK rejects values outside 0/1.
        let bad = diesel::sql_query(format!(
            "UPDATE command_instances SET was_running_on_shutdown = 2 WHERE id = '{}'",
            inst.id
        ))
        .execute(&mut conn);
        assert!(
            bad.is_err(),
            "was_running_on_shutdown CHECK must reject values outside 0/1"
        );
    }

    /// CRUD round-trip: create/list/update/delete, reorder, restart_on_startup
    /// and source-field update all persist. Done-criterion verbatim: "CRUD
    /// template, reorder, restart_on_startup, set_last_state et persist_scrollback
    /// borne round-trippent" (set_last_state/scrollback covered by their own
    /// tests too).
    #[test]
    fn template_crud_reorder_and_restart_roundtrip() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();

        // create + list in order.
        let a = create_template(
            &mut conn,
            &project.id,
            "a",
            "cmd-a",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let b = create_template(
            &mut conn,
            &project.id,
            "b",
            "cmd-b",
            Some("sub"),
            CommandSource::default(),
        )
        .unwrap();
        let c = create_template(
            &mut conn,
            &project.id,
            "c",
            "cmd-c",
            None,
            CommandSource::default(),
        )
        .unwrap();
        assert!(a.order_index < b.order_index && b.order_index < c.order_index);
        let listed: Vec<String> = list_templates(&mut conn, &project.id)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(
            listed,
            vec![a.id.clone(), b.id.clone(), c.id.clone()],
            "list follows order asc"
        );

        // update: rename + change command + set subfolder.
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(
            update_template(&mut conn, &a.id, "a2", "cmd-a2", Some("client")).unwrap(),
            1
        );
        let got_a = get_template(&mut conn, &a.id).unwrap().unwrap();
        assert_eq!(got_a.name, "a2");
        assert_eq!(got_a.command, "cmd-a2");
        assert_eq!(got_a.subfolder.as_deref(), Some("client"));
        assert!(got_a.updated_at > a.updated_at, "update bumps updated_at");
        assert_eq!(got_a.created_at, a.created_at, "created_at is immutable");
        // update can clear the subfolder back to root.
        update_template(&mut conn, &b.id, "b", "cmd-b", None).unwrap();
        assert_eq!(
            get_template(&mut conn, &b.id).unwrap().unwrap().subfolder,
            None
        );

        // reorder: c, a, b.
        reorder_templates(&mut conn, &[c.id.clone(), a.id.clone(), b.id.clone()]).unwrap();
        let after: Vec<String> = list_templates(&mut conn, &project.id)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(
            after,
            vec![c.id.clone(), a.id.clone(), b.id.clone()],
            "list reflects the persisted reorder"
        );
        assert_eq!(
            get_template(&mut conn, &c.id).unwrap().unwrap().order_index,
            0
        );

        // restart_on_startup toggles and persists.
        assert!(!got_a.restart_on_startup);
        set_restart_on_startup(&mut conn, &a.id, true).unwrap();
        assert!(
            get_template(&mut conn, &a.id)
                .unwrap()
                .unwrap()
                .restart_on_startup,
            "restart flag persists true"
        );
        set_restart_on_startup(&mut conn, &a.id, false).unwrap();
        assert!(
            !get_template(&mut conn, &a.id)
                .unwrap()
                .unwrap()
                .restart_on_startup,
            "restart flag resets to false"
        );

        // set_template_source sets then clears provenance.
        set_template_source(
            &mut conn,
            &a.id,
            CommandSource {
                source_kind: Some(SOURCE_KIND_PACKAGE_JSON.to_string()),
                source_package_json_path: Some("package.json".to_string()),
                source_script_name: Some("a".to_string()),
                source_script_command_snapshot: Some("cmd-a2".to_string()),
                package_manager: Some("yarn".to_string()),
            },
        )
        .unwrap();
        let sourced = get_template(&mut conn, &a.id).unwrap().unwrap();
        assert_eq!(
            sourced.source_kind.as_deref(),
            Some(SOURCE_KIND_PACKAGE_JSON)
        );
        assert_eq!(sourced.package_manager.as_deref(), Some("yarn"));
        // Clear it back to hand-authored.
        set_template_source(&mut conn, &a.id, CommandSource::default()).unwrap();
        assert_eq!(
            get_template(&mut conn, &a.id).unwrap().unwrap().source_kind,
            None
        );

        // delete: removes the row; list shrinks.
        assert_eq!(delete_template(&mut conn, &a.id).unwrap(), 1);
        assert!(get_template(&mut conn, &a.id).unwrap().is_none());
        assert_eq!(list_templates(&mut conn, &project.id).unwrap().len(), 2);

        // unknown-id mutations are no-ops (0 rows), not errors.
        assert_eq!(
            update_template(&mut conn, "no-such-id", "x", "y", None).unwrap(),
            0
        );
        assert_eq!(
            set_restart_on_startup(&mut conn, "no-such-id", true).unwrap(),
            0
        );
        assert_eq!(delete_template(&mut conn, "no-such-id").unwrap(), 0);
        assert_eq!(
            set_template_source(&mut conn, "no-such-id", CommandSource::default()).unwrap(),
            0
        );
    }

    /// `persist_instance_scrollback` round-trips a string and is BOUNDED to the cap
    /// (keeping the tail). Done-criterion verbatim: "persist_scrollback borne
    /// round-trippent" (for instances).
    #[test]
    fn persist_instance_scrollback_roundtrips_and_is_bounded() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let inst = insert_instance(&mut conn, &dev.id, &root.id);

        // Exact round-trip of a modest payload with ANSI + CRLF.
        let payload = "out1\r\n\x1b[32mok\x1b[0m\r\n";
        assert_eq!(
            persist_instance_scrollback(&mut conn, &inst.id, payload).unwrap(),
            1
        );
        assert_eq!(
            get_instance(&mut conn, &inst.id)
                .unwrap()
                .unwrap()
                .scrollback,
            payload,
            "instance scrollback round-trips byte-for-byte"
        );

        // Over-cap payload is stored truncated, keeping the tail, staying valid.
        let filler = "x".repeat(MAX_SCROLLBACK_BYTES);
        let big = format!("{filler}INSTANCE_TAIL_END");
        assert!(big.len() > MAX_SCROLLBACK_BYTES);
        persist_instance_scrollback(&mut conn, &inst.id, &big).unwrap();
        let got = get_instance(&mut conn, &inst.id).unwrap().unwrap();
        assert!(
            got.scrollback.len() <= MAX_SCROLLBACK_BYTES,
            "instance scrollback is bounded to the cap"
        );
        assert!(
            got.scrollback.ends_with("INSTANCE_TAIL_END"),
            "bounding keeps the tail"
        );

        // Unknown id → no-op.
        assert_eq!(
            persist_instance_scrollback(&mut conn, "no-such-id", "x").unwrap(),
            0
        );
    }

    /// Migration v3 down→up reversibility: revert down to before v3, the v3 tables
    /// are gone, re-apply, and the schema round-trips a template + instance again.
    ///
    /// History: v3 used to be the LAST migration, so a single `revert_last_migration`
    /// reached the v3-absent state. Later PRDs stacked migrations on top (PRD-4 → v4/v5,
    /// PRD-5 → v7/v8), so reaching the v3-absent state now peels all of them first. The
    /// v4-only down→up is covered separately by `migration_v4_down_then_up_*`.
    #[test]
    fn migration_v3_down_then_up_recreates_working_schema() {
        let mut conn = open_in_memory();

        // Peel every migration stacked above the managed_commands migration (dirs
        // #15 → #4), then the managed_commands migration (dir #3) itself — 13 reverts in
        // reverse order (v2.4 Phase 1 added dir #13 the Spec domain; Phase 2 added dir #14
        // the task domain; Phase 3 added dir #15 the specs.workspace_id index).
        // managed_commands must then be absent.
        for _ in 0..16 {
            conn.revert_last_migration(MIGRATIONS)
                .expect("revert migration cleanly down to (and including) managed_commands");
        }
        let after_down: QueryResult<i64> = managed_commands::table.count().get_result(&mut conn);
        assert!(
            after_down.is_err(),
            "after reverting v3, the managed_commands table must be gone"
        );
        let inst_after_down: QueryResult<i64> =
            command_instances::table.count().get_result(&mut conn);
        assert!(
            inst_after_down.is_err(),
            "after reverting v3, the command_instances table must be gone"
        );

        // Re-apply and round-trip a template + its instance again.
        run_migrations(&mut conn).expect("re-apply v3");
        let (project, root) =
            create_project(&mut conn, "revived", p("/rev", "C:\\rev"), None).unwrap();
        let tpl = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .expect("create_template after down→up");
        let inst = insert_instance(&mut conn, &tpl.id, &root.id);
        assert_eq!(
            inst.last_state, STATE_IDLE,
            "rebuilt v3 schema round-trips an instance"
        );
    }

    /// Migration v4 down→up reversibility + safe back-fill: the v4 outcome columns
    /// (`last_exit_code` / `ended_at` / `unread`) are added with safe defaults for an
    /// EXISTING row, the down drops them (the table + a pre-v4 row survive), and the
    /// re-applied up restores them with the same safe defaults — so an upgrade never
    /// strands a row.
    #[test]
    fn migration_v4_down_then_up_recreates_columns_with_safe_defaults() {
        let mut conn = open_in_memory();

        // Seed a row through the FULL (v4) schema so it exists across the revert.
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let inst = insert_instance(&mut conn, &dev.id, &root.id);

        // Revert down to the v4-absent state. Migrations stack v15, v14, v13, v12, v11, v10,
        // v9, v8, v7, v5 ON TOP of v4 (v2.4 Phase 3 added v15 the specs.workspace_id index;
        // Phase 2 added v14 the task domain; Phase 1 added v13 the Spec domain; v2.1 Phase 4
        // added v12 domain_events; v2.1 added v10/v11; #11 added v9; PRD-5 added v7/v8; PRD-4
        // added v5), so reaching the v4-absent state peels v15, v14, v13, v12, v11, v10, v9,
        // v8, v7, v5, then v4 (migrations revert in reverse order). The instance row + its v3
        // columns survive; the v4 columns are gone (a typed select of `unread` would now fail
        // to run).
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v18 (review_tasks.spec_id) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v17 (review_tasks/review_reads) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v16 (review_comments) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v15 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v14 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v13 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v12 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v11 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v10 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v9 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v8 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v7 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v5 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v4 cleanly");
        let unread_after_down: QueryResult<bool> = command_instances::table
            .select(command_instances::unread)
            .filter(command_instances::id.eq(&inst.id))
            .first(&mut conn);
        assert!(
            unread_after_down.is_err(),
            "after reverting v4, the `unread` column must be gone"
        );
        // The row itself (and managed_commands) is still there — v4 is additive.
        let still_there: i64 = command_instances::table
            .count()
            .get_result(&mut conn)
            .unwrap();
        assert_eq!(
            still_there, 1,
            "the pre-v4 instance row survives the v4 down"
        );

        // Re-apply v4: the columns return with safe defaults for the EXISTING row
        // (unread=0 / NULL code / NULL ended_at) — an upgrade does not strand a row.
        run_migrations(&mut conn).expect("re-apply v4");
        let back = get_instance(&mut conn, &inst.id).unwrap().unwrap();
        assert!(
            !back.unread,
            "back-filled unread defaults to false (already seen)"
        );
        assert_eq!(
            back.last_exit_code, None,
            "back-filled last_exit_code defaults to NULL"
        );
        assert_eq!(back.ended_at, None, "back-filled ended_at defaults to NULL");
    }

    /// `archive_and_reset_for_relaunch` retains the LAST completed run (v5, the dogfood
    /// review fix): a finished run's scrollback + outcome roll into the bounded `prev_*`
    /// columns (N=1) while the current run resets clean. A non-finished start has no run
    /// to retain. Bounded: a SECOND relaunch overwrites the retained prior run, never
    /// stacks.
    #[test]
    fn archive_and_reset_retains_one_prior_run_bounded() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let inst = insert_instance(&mut conn, &dev.id, &root.id);
        let reload = |c: &mut SqliteConnection| get_instance(c, &inst.id).unwrap().unwrap();

        // A FRESH start on an idle never-run instance: nothing to retain. prev_* stay
        // at their defaults; the current run resets to a clean `running` row.
        assert_eq!(
            archive_and_reset_for_relaunch(&mut conn, &inst.id).unwrap(),
            1
        );
        let first = reload(&mut conn);
        assert_eq!(
            first.prev_scrollback, "",
            "first start retains no prior run"
        );
        assert_eq!(first.prev_last_state, None);
        assert_eq!(first.prev_exit_code, None);
        assert_eq!(
            first.last_state, STATE_RUNNING,
            "the current run is reset to running"
        );
        assert_eq!(
            first.scrollback, "",
            "the current run starts with empty scrollback"
        );

        // Run 1 produces output and FINISHES error(7).
        persist_instance_scrollback(&mut conn, &inst.id, "RUN1 output\n").unwrap();
        set_run_state(&mut conn, &inst.id, STATE_ERROR, Some(7)).unwrap();

        // RELAUNCH: the finished run 1 is archived into prev_*; the current run resets.
        assert_eq!(
            archive_and_reset_for_relaunch(&mut conn, &inst.id).unwrap(),
            1
        );
        let after_relaunch = reload(&mut conn);
        assert_eq!(
            after_relaunch.prev_scrollback, "RUN1 output\n",
            "the retained prior run keeps run 1's output"
        );
        assert_eq!(after_relaunch.prev_last_state.as_deref(), Some(STATE_ERROR));
        assert_eq!(
            after_relaunch.prev_exit_code,
            Some(7),
            "retained prior exit code"
        );
        assert!(
            after_relaunch.prev_ended_at.is_some(),
            "retained prior ended_at"
        );
        // The CURRENT run is clean — not polluted by run 1's bytes.
        assert_eq!(after_relaunch.scrollback, "", "current run resets to empty");
        assert_eq!(after_relaunch.last_state, STATE_RUNNING);
        assert_eq!(
            after_relaunch.last_exit_code, None,
            "current run has no code yet"
        );

        // Run 2 produces output and finishes success(0).
        persist_instance_scrollback(&mut conn, &inst.id, "RUN2 output\n").unwrap();
        set_run_state(&mut conn, &inst.id, STATE_SUCCESS, Some(0)).unwrap();

        // A SECOND relaunch OVERWRITES the retained prior run with run 2 — bounded N=1,
        // never stacking run 1 + run 2.
        archive_and_reset_for_relaunch(&mut conn, &inst.id).unwrap();
        let after_second = reload(&mut conn);
        assert_eq!(
            after_second.prev_scrollback, "RUN2 output\n",
            "the retained prior run is now run 2 (bounded N=1, run 1 evicted)"
        );
        assert_eq!(after_second.prev_last_state.as_deref(), Some(STATE_SUCCESS));
        assert_eq!(after_second.prev_exit_code, Some(0));
    }

    /// Migration v5 down→up reversibility + safe back-fill: the v5 retained-prior-run
    /// columns (`prev_scrollback` / `prev_exit_code` / `prev_ended_at` /
    /// `prev_last_state`) are added with safe defaults for an EXISTING row, the down
    /// drops them (the table + a pre-v5 row survive), and the re-applied up restores
    /// them with the same safe defaults. Mirrors the v4 down→up test.
    #[test]
    fn migration_v5_down_then_up_recreates_columns_with_safe_defaults() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let inst = insert_instance(&mut conn, &dev.id, &root.id);

        // Revert down to the v5-absent state. Migrations v15 (v2.4 Phase 3
        // specs.workspace_id index), v14 (v2.4 Phase 2 task domain), v13 (v2.4 Phase 1 Spec
        // domain), v12 (v2.1 Phase 4 domain_events), v11 (v2.1 data backfill), v10 (v2.1
        // canonical structure), v9 (#11 project order), v8 (project resume option) and v7
        // (agent_sessions) stack ON TOP of v5/prev_run, so reaching the v5-absent state now
        // peels v15, v14, v13, v12, v11, v10, v9, v8, v7, then v5 (migrations revert in
        // reverse order). The instance row + its v3/v4 columns survive; the v5 columns are
        // gone (a typed select of `prev_scrollback` would now fail to run).
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v18 (review_tasks.spec_id) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v17 (review_tasks/review_reads) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v16 (review_comments) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v15 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v14 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v13 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v12 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v11 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v10 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v9 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v8 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v7 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v5 cleanly");
        let prev_after_down: QueryResult<String> = command_instances::table
            .select(command_instances::prev_scrollback)
            .filter(command_instances::id.eq(&inst.id))
            .first(&mut conn);
        assert!(
            prev_after_down.is_err(),
            "after reverting v5, the `prev_scrollback` column must be gone"
        );
        let still_there: i64 = command_instances::table
            .count()
            .get_result(&mut conn)
            .unwrap();
        assert_eq!(
            still_there, 1,
            "the pre-v5 instance row survives the v5 down"
        );

        // Re-apply v5: the columns return with safe defaults for the EXISTING row
        // ('' scrollback / NULL code / NULL ended_at / NULL state).
        run_migrations(&mut conn).expect("re-apply v5");
        let back = get_instance(&mut conn, &inst.id).unwrap().unwrap();
        assert_eq!(
            back.prev_scrollback, "",
            "back-filled prev_scrollback defaults to ''"
        );
        assert_eq!(
            back.prev_exit_code, None,
            "back-filled prev_exit_code defaults to NULL"
        );
        assert_eq!(
            back.prev_ended_at, None,
            "back-filled prev_ended_at defaults to NULL"
        );
        assert_eq!(
            back.prev_last_state, None,
            "back-filled prev_last_state defaults to NULL"
        );

        // The `prev_last_state` CHECK rejects an out-of-domain value via a raw update.
        let bad = diesel::sql_query(format!(
            "UPDATE command_instances SET prev_last_state = 'running' WHERE id = '{}'",
            inst.id
        ))
        .execute(&mut conn);
        assert!(
            bad.is_err(),
            "prev_last_state CHECK must reject values outside success|error"
        );
    }

    // --- Materialization + per-workspace listing (PRD-3 task 2) -----------

    /// Count all command_instance rows (test convenience).
    #[cfg(test)]
    fn instance_count(conn: &mut SqliteConnection) -> i64 {
        command_instances::table.count().get_result(conn).unwrap()
    }

    /// Creating a workspace in a project that already has N templates materializes
    /// N instances linked to that workspace. Done-criterion verbatim: "Creer un
    /// workspace dans un projet a N templates cree N instances liees".
    #[test]
    fn creating_a_workspace_materializes_one_instance_per_template() {
        let mut conn = open_in_memory();
        // Project (root workspace) + 3 templates → 3 instances for the root.
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        for name in ["dev", "build", "test"] {
            create_template(
                &mut conn,
                &project.id,
                name,
                "cmd",
                None,
                CommandSource::default(),
            )
            .unwrap();
        }
        assert_eq!(
            instance_count(&mut conn),
            3,
            "3 templates materialize 3 instances on the existing root workspace"
        );

        // Now ADD a workspace: it must get one instance per existing template (3).
        let feat =
            create_workspace(&mut conn, &project.id, "feat", p("/p/feat", "C:\\p\\feat")).unwrap();
        let feat_instances = list_instances_for_workspace(&mut conn, &feat.id).unwrap();
        assert_eq!(
            feat_instances.len(),
            3,
            "creating a workspace in a project with N templates creates N instances"
        );
        // Every instance is linked to THIS workspace and to a real template.
        for inst in &feat_instances {
            assert_eq!(
                inst.workspace_id, feat.id,
                "instance is linked to the new workspace"
            );
        }
        // 3 root + 3 feat = 6 instances total.
        assert_eq!(instance_count(&mut conn), 6);

        // The root workspace's own instances are untouched (still 3).
        assert_eq!(
            list_instances_for_workspace(&mut conn, &root.id)
                .unwrap()
                .len(),
            3
        );
    }

    /// Adding a template to a project that already has M workspaces materializes M
    /// instances. Done-criterion verbatim: "Ajouter un template a un projet a M
    /// workspaces cree M instances".
    #[test]
    fn adding_a_template_materializes_one_instance_per_workspace() {
        let mut conn = open_in_memory();
        // Project (root) + 2 added workspaces = 3 workspaces, no templates yet.
        let (project, _root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        create_workspace(&mut conn, &project.id, "feat", p("/p/feat", "C:\\p\\feat")).unwrap();
        create_workspace(&mut conn, &project.id, "api", p("/p/api", "C:\\p\\api")).unwrap();
        assert_eq!(
            instance_count(&mut conn),
            0,
            "no templates yet → no instances"
        );

        // Add a template: it materializes one instance per workspace (3).
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        assert_eq!(
            instance_count(&mut conn),
            3,
            "adding a template to a project with M=3 workspaces creates 3 instances"
        );
        // Each is linked to `dev` and to a distinct workspace.
        let mut ws_ids: Vec<String> = command_instances::table
            .filter(command_instances::command_id.eq(&dev.id))
            .select(command_instances::workspace_id)
            .load(&mut conn)
            .unwrap();
        ws_ids.sort();
        ws_ids.dedup();
        assert_eq!(
            ws_ids.len(),
            3,
            "the template materialized once per distinct workspace"
        );

        // A second template adds another instance per workspace (3 → 6 total).
        create_template(
            &mut conn,
            &project.id,
            "build",
            "make",
            None,
            CommandSource::default(),
        )
        .unwrap();
        assert_eq!(instance_count(&mut conn), 6);
    }

    /// Materialization is IDEMPOTENT: re-running it for a workspace or a template
    /// never duplicates or errors (UNIQUE(command_id, workspace_id) + ON CONFLICT
    /// DO NOTHING). The "idempotente grace a UNIQUE" guarantee from the spec.
    #[test]
    fn materialization_is_idempotent() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let feat =
            create_workspace(&mut conn, &project.id, "feat", p("/p/feat", "C:\\p\\feat")).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        // create_template already materialized for root + feat (2 instances).
        assert_eq!(instance_count(&mut conn), 2);

        // Re-materializing for the workspace creates 0 new rows (all exist).
        assert_eq!(
            materialize_instances_for_workspace(&mut conn, &feat.id).unwrap(),
            0,
            "re-materializing an already-materialized workspace inserts nothing"
        );
        // Re-materializing for the template creates 0 new rows too.
        assert_eq!(
            materialize_instances_for_template(&mut conn, &dev.id).unwrap(),
            0,
            "re-materializing an already-materialized template inserts nothing"
        );
        assert_eq!(
            instance_count(&mut conn),
            2,
            "count is unchanged after re-runs"
        );
        let _ = root; // root participates via create_template's materialization.
    }

    /// `list_instances_for_workspace` returns the workspace's instances JOINED to
    /// their template's display fields (name, command, subfolder), ordered by the
    /// template order. Done-criterion verbatim: "list-par-workspace retourne les
    /// instances jointes a leur template".
    #[test]
    fn list_instances_joins_template_fields_in_order() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        // Three templates with distinct command/subfolder, created in a known order.
        let a = create_template(
            &mut conn,
            &project.id,
            "alpha",
            "cmd-a",
            Some("client"),
            CommandSource::default(),
        )
        .unwrap();
        let b = create_template(
            &mut conn,
            &project.id,
            "bravo",
            "cmd-b",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let c = create_template(
            &mut conn,
            &project.id,
            "charlie",
            "cmd-c",
            Some("server"),
            CommandSource::default(),
        )
        .unwrap();

        let rows = list_instances_for_workspace(&mut conn, &root.id).unwrap();
        assert_eq!(rows.len(), 3, "one joined row per materialized instance");

        // Order follows the template order (a, b, c).
        assert_eq!(
            rows.iter()
                .map(|r| r.command_id.clone())
                .collect::<Vec<_>>(),
            vec![a.id.clone(), b.id.clone(), c.id.clone()],
            "listing is ordered by template order"
        );

        // Each row carries the JOINED template fields verbatim.
        assert_eq!(rows[0].name, "alpha");
        assert_eq!(rows[0].command, "cmd-a");
        assert_eq!(rows[0].subfolder.as_deref(), Some("client"));
        assert_eq!(rows[1].name, "bravo");
        assert_eq!(rows[1].command, "cmd-b");
        assert_eq!(
            rows[1].subfolder, None,
            "a null subfolder joins through as None"
        );
        assert_eq!(rows[2].name, "charlie");
        assert_eq!(rows[2].subfolder.as_deref(), Some("server"));

        // The instance columns are present and carry their defaults.
        assert!(rows.iter().all(|r| r.last_state == STATE_IDLE));
        assert!(rows.iter().all(|r| r.workspace_id == root.id));

        // The joined WORKSPACE path is carried on every row (the info bar's run-dir
        // base); a hand-authored template has no source provenance. `cwd` is left
        // None by the pure-DB query — the bridge resolves it before serializing.
        assert!(rows.iter().all(|r| r.workspace_path == p("/p", "C:\\p")));
        assert!(rows.iter().all(|r| r.source_kind.is_none()));
        assert!(rows.iter().all(|r| r.source_package_json_path.is_none()));
        assert!(rows.iter().all(|r| r.source_script_name.is_none()));
        assert!(rows.iter().all(|r| r.package_manager.is_none()));
        assert!(
            rows.iter().all(|r| r.cwd.is_none()),
            "the DB query leaves cwd None; the bridge fills the resolved run dir"
        );

        // A reorder of the templates is reflected by a subsequent listing.
        reorder_templates(&mut conn, &[c.id.clone(), a.id.clone(), b.id.clone()]).unwrap();
        let reordered = list_instances_for_workspace(&mut conn, &root.id).unwrap();
        assert_eq!(
            reordered
                .iter()
                .map(|r| r.command_id.clone())
                .collect::<Vec<_>>(),
            vec![c.id.clone(), a.id.clone(), b.id.clone()],
            "listing tracks the persisted template order"
        );

        // The listing is SCOPED to one workspace: an unknown workspace lists empty.
        assert!(
            list_instances_for_workspace(&mut conn, "no-such-workspace")
                .unwrap()
                .is_empty(),
            "listing an unknown workspace returns an empty vec (not an error)"
        );
    }

    /// A SOURCED (package.json) template's provenance joins through the listing so
    /// the command info bar can show the source. The instance row carries the
    /// `source_*` group + `package_manager` from the joined template, and the
    /// workspace `path` from the joined workspace.
    #[test]
    fn list_instances_carries_source_provenance_and_workspace_path() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        create_template(
            &mut conn,
            &project.id,
            "dev",
            "pnpm dev",
            Some("frontend"),
            CommandSource {
                source_kind: Some(SOURCE_KIND_PACKAGE_JSON.to_string()),
                source_package_json_path: Some("frontend/package.json".to_string()),
                source_script_name: Some("dev".to_string()),
                source_script_command_snapshot: Some("vite".to_string()),
                package_manager: Some("pnpm".to_string()),
            },
        )
        .unwrap();

        let rows = list_instances_for_workspace(&mut conn, &root.id).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.command, "pnpm dev");
        assert_eq!(r.subfolder.as_deref(), Some("frontend"));
        assert_eq!(r.workspace_path, p("/p", "C:\\p"));
        assert_eq!(r.source_kind.as_deref(), Some(SOURCE_KIND_PACKAGE_JSON));
        assert_eq!(
            r.source_package_json_path.as_deref(),
            Some("frontend/package.json")
        );
        assert_eq!(r.source_script_name.as_deref(), Some("dev"));
        assert_eq!(r.package_manager.as_deref(), Some("pnpm"));
        // The snapshot column is NOT projected into the listing (the bar shows the
        // source LOCATION, not the snapshot body), and `cwd` is the bridge's job.
        assert!(r.cwd.is_none());
    }

    /// Deleting a template OR a workspace removes its instances from the
    /// per-workspace listing (cascade FK). Done-criterion verbatim: "delete d'un
    /// template ou d'un workspace retire ses instances (cascade FK)".
    #[test]
    fn delete_template_or_workspace_removes_instances_from_listing() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let feat =
            create_workspace(&mut conn, &project.id, "feat", p("/p/feat", "C:\\p\\feat")).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let build = create_template(
            &mut conn,
            &project.id,
            "build",
            "make",
            None,
            CommandSource::default(),
        )
        .unwrap();

        // Full grid: each workspace lists 2 instances (dev, build).
        assert_eq!(
            list_instances_for_workspace(&mut conn, &root.id)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            list_instances_for_workspace(&mut conn, &feat.id)
                .unwrap()
                .len(),
            2
        );

        // Delete the `dev` TEMPLATE: it disappears from BOTH workspaces' listings.
        delete_template(&mut conn, &dev.id).unwrap();
        let root_after = list_instances_for_workspace(&mut conn, &root.id).unwrap();
        let feat_after = list_instances_for_workspace(&mut conn, &feat.id).unwrap();
        assert_eq!(
            root_after.len(),
            1,
            "the deleted template's instance left the root listing"
        );
        assert_eq!(
            feat_after.len(),
            1,
            "the deleted template's instance left the feat listing"
        );
        assert!(
            root_after.iter().all(|r| r.command_id == build.id),
            "only the surviving template's instance remains"
        );

        // Delete the `feat` WORKSPACE: its listing is now empty; root is untouched.
        diesel::delete(workspaces::table.find(&feat.id))
            .execute(&mut conn)
            .unwrap();
        assert!(
            list_instances_for_workspace(&mut conn, &feat.id)
                .unwrap()
                .is_empty(),
            "deleting a workspace removes its instances from the listing"
        );
        assert_eq!(
            list_instances_for_workspace(&mut conn, &root.id)
                .unwrap()
                .len(),
            1,
            "the other workspace's listing is unaffected"
        );
    }

    // --- Running-mutation guard inputs (the id sets the bridge guard reads) ---

    /// `instance_ids_for_template` returns every instance of a template (one per
    /// workspace of its project) and nothing from a sibling template. This is the
    /// id set the bridge running-guard feeds to `runner.any_running(..)` to refuse
    /// editing/deleting a template while one of its instances runs.
    #[test]
    fn instance_ids_for_template_lists_one_per_workspace() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        // 2 extra workspaces → 3 workspaces total.
        create_workspace(&mut conn, &project.id, "feat", p("/p/feat", "C:\\p\\feat")).unwrap();
        create_workspace(&mut conn, &project.id, "api", p("/p/api", "C:\\p\\api")).unwrap();

        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let build = create_template(
            &mut conn,
            &project.id,
            "build",
            "make",
            None,
            CommandSource::default(),
        )
        .unwrap();

        let dev_ids = instance_ids_for_template(&mut conn, &dev.id).unwrap();
        assert_eq!(
            dev_ids.len(),
            3,
            "one instance per workspace for the dev template"
        );
        // None of `build`'s instances leak into dev's id set.
        let build_ids = instance_ids_for_template(&mut conn, &build.id).unwrap();
        assert!(
            dev_ids.iter().all(|id| !build_ids.contains(id)),
            "a template's instance ids are disjoint from a sibling template's"
        );

        // An unknown template yields an empty set (guard never blocks on nothing).
        assert!(
            instance_ids_for_template(&mut conn, "no-such-template")
                .unwrap()
                .is_empty(),
            "unknown template id → no instance ids"
        );
    }

    /// `instance_ids_for_project` returns every command instance of a project
    /// (across all its templates and workspaces) and excludes other projects. This
    /// is the id set the `delete_project` guard checks before allowing the delete.
    #[test]
    fn instance_ids_for_project_spans_all_templates_and_excludes_others() {
        let mut conn = open_in_memory();
        let (p1, _root1) = create_project(&mut conn, "p1", p("/p1", "C:\\p1"), None).unwrap();
        create_workspace(&mut conn, &p1.id, "feat", p("/p1/feat", "C:\\p1\\feat")).unwrap();
        // p1: 2 templates × 2 workspaces = 4 instances.
        for name in ["dev", "build"] {
            create_template(
                &mut conn,
                &p1.id,
                name,
                "cmd",
                None,
                CommandSource::default(),
            )
            .unwrap();
        }

        // A second project with its own instance must NOT appear in p1's set.
        let (p2, root2) = create_project(&mut conn, "p2", p("/p2", "C:\\p2"), None).unwrap();
        let dev2 = create_template(
            &mut conn,
            &p2.id,
            "dev",
            "cmd",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let p2_inst = list_instances_for_workspace(&mut conn, &root2.id)
            .unwrap()
            .into_iter()
            .find(|i| i.command_id == dev2.id)
            .unwrap();

        let p1_ids = instance_ids_for_project(&mut conn, &p1.id).unwrap();
        assert_eq!(p1_ids.len(), 4, "every instance of p1 across templates×ws");
        assert!(
            !p1_ids.contains(&p2_inst.id),
            "a sibling project's instance is excluded from the delete guard set"
        );

        // Unknown project → empty (the guard never blocks the delete spuriously).
        assert!(instance_ids_for_project(&mut conn, "no-such-project")
            .unwrap()
            .is_empty());
    }

    // --- Restore inputs: snapshot + restart_on_startup eligibility -------------

    /// `instance_run_context` joins an instance to its template (command,
    /// subfolder, restart_on_startup) and its workspace (path) — the inputs the
    /// runner needs to spawn. Unknown id → None.
    #[test]
    fn instance_run_context_joins_template_and_workspace() {
        let mut conn = open_in_memory();
        let (project, root) =
            create_project(&mut conn, "p", p("/srv/p", "C:\\srv\\p"), None).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            Some("frontend"),
            CommandSource::default(),
        )
        .unwrap();
        set_restart_on_startup(&mut conn, &dev.id, true).unwrap();
        let inst = list_instances_for_workspace(&mut conn, &root.id)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();

        let ctx = instance_run_context(&mut conn, &inst.id).unwrap().unwrap();
        assert_eq!(ctx.command, "npm run dev");
        assert_eq!(ctx.subfolder.as_deref(), Some("frontend"));
        assert_eq!(ctx.workspace_path, p("/srv/p", "C:\\srv\\p"));
        assert!(
            ctx.restart_on_startup,
            "the joined template restart flag is surfaced for the restore path"
        );

        assert!(
            instance_run_context(&mut conn, "no-such-instance")
                .unwrap()
                .is_none(),
            "unknown instance → no run context"
        );
    }

    /// `all_instances_for_restore` returns one row per instance with the two boot
    /// signals (`restart_on_startup`, `was_running_on_shutdown`) joined in, and the
    /// boot-eligibility predicate (`restart_on_startup && was_running_on_shutdown`)
    /// selects ONLY the instance whose template toggle is ON and whose snapshot is
    /// true. Done-criterion verbatim: "shutdown snapshot + eligibilite
    /// restart_on_startup".
    #[test]
    fn restore_rows_carry_both_signals_and_eligibility_selects_only_on_and_running() {
        let mut conn = open_in_memory();
        let (project, root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();

        // toggle ON template, snapshot=running  → eligible.
        let on = create_template(
            &mut conn,
            &project.id,
            "on",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        set_restart_on_startup(&mut conn, &on.id, true).unwrap();
        // toggle OFF template, snapshot=running  → NOT eligible (off).
        let off = create_template(
            &mut conn,
            &project.id,
            "off",
            "npm run build",
            None,
            CommandSource::default(),
        )
        .unwrap();
        // toggle ON template but snapshot=not-running → NOT eligible (idle at quit).
        let on_idle = create_template(
            &mut conn,
            &project.id,
            "on_idle",
            "npm test",
            None,
            CommandSource::default(),
        )
        .unwrap();
        set_restart_on_startup(&mut conn, &on_idle.id, true).unwrap();

        let by_cmd = |c: &mut SqliteConnection, command_id: &str| {
            list_instances_for_workspace(c, &root.id)
                .unwrap()
                .into_iter()
                .find(|i| i.command_id == command_id)
                .unwrap()
                .id
        };
        let on_inst = by_cmd(&mut conn, &on.id);
        let off_inst = by_cmd(&mut conn, &off.id);
        let _on_idle_inst = by_cmd(&mut conn, &on_idle.id);

        // Snapshot: the two "was running at shutdown" instances.
        set_was_running_on_shutdown(&mut conn, &on_inst, true).unwrap();
        set_was_running_on_shutdown(&mut conn, &off_inst, true).unwrap();
        // on_idle stays false (it was idle when the app quit).

        let rows = all_instances_for_restore(&mut conn).unwrap();
        assert_eq!(rows.len(), 3, "one restore row per instance");

        // The eligibility predicate used by the boot restore (bridge).
        let eligible: Vec<&str> = rows
            .iter()
            .filter(|r| r.restart_on_startup && r.was_running_on_shutdown)
            .map(|r| r.instance_id.as_str())
            .collect();
        assert_eq!(
            eligible,
            vec![on_inst.as_str()],
            "only the (toggle ON × was-running) instance is eligible for boot relaunch"
        );

        // Sanity: the rows carry the command line + cwd inputs the runner needs.
        let on_row = rows.iter().find(|r| r.instance_id == on_inst).unwrap();
        assert_eq!(on_row.command, "npm run dev");
        assert_eq!(on_row.workspace_path, p("/p", "C:\\p"));
    }

    // --- Source provenance mutation (set/clear) --------------------------------

    /// `set_template_source` sets the full provenance tuple and an all-`None`
    /// source clears it (hand-authored), never touching `command`. This is the
    /// DB-level write behind unlink/import source actions.
    #[test]
    fn set_template_source_sets_then_clears_without_touching_command() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
        let dev = create_template(
            &mut conn,
            &project.id,
            "dev",
            "pnpm dev",
            None,
            CommandSource::default(),
        )
        .unwrap();

        // Set the package.json provenance.
        assert_eq!(
            set_template_source(
                &mut conn,
                &dev.id,
                CommandSource {
                    source_kind: Some(SOURCE_KIND_PACKAGE_JSON.to_string()),
                    source_package_json_path: Some("package.json".to_string()),
                    source_script_name: Some("dev".to_string()),
                    source_script_command_snapshot: Some("vite".to_string()),
                    package_manager: Some("pnpm".to_string()),
                },
            )
            .unwrap(),
            1
        );
        let linked = get_template(&mut conn, &dev.id).unwrap().unwrap();
        assert_eq!(
            linked.source_kind.as_deref(),
            Some(SOURCE_KIND_PACKAGE_JSON)
        );
        assert_eq!(linked.source_script_name.as_deref(), Some("dev"));
        assert_eq!(linked.package_manager.as_deref(), Some("pnpm"));
        assert_eq!(linked.command, "pnpm dev", "source set never edits command");

        // Clear it (unlink): all provenance columns go NULL, command untouched.
        assert_eq!(
            set_template_source(&mut conn, &dev.id, CommandSource::default()).unwrap(),
            1
        );
        let unlinked = get_template(&mut conn, &dev.id).unwrap().unwrap();
        assert_eq!(unlinked.source_kind, None);
        assert_eq!(unlinked.source_package_json_path, None);
        assert_eq!(unlinked.source_script_name, None);
        assert_eq!(unlinked.source_script_command_snapshot, None);
        assert_eq!(unlinked.package_manager, None);
        assert_eq!(unlinked.command, "pnpm dev", "unlink never edits command");

        // Unknown id → no-op (0 rows).
        assert_eq!(
            set_template_source(&mut conn, "no-such-id", CommandSource::default()).unwrap(),
            0
        );
    }

    // --- Agent sessions (PRD-5 v7, ADR-0010) -----------------------------

    /// A capture for `terminal_id` with the given external id, unattached and at a
    /// fixed cwd. Keeps the session tests terse.
    fn capture(external: &str) -> SessionCapture {
        SessionCapture {
            workspace_id: None,
            external_session_id: external.to_string(),
            cwd: "/work".to_string(),
            transcript_path: None,
            metadata_json: None,
        }
    }

    /// schema.rs ↔ migration v7 consistency: exercise EVERY `agent_sessions` column
    /// through a real insert (via `record_session_start`) + select, covering both
    /// branches of every nullable column and the keyword-free columns. A drift in
    /// name/type/order would fail to compile (`check_for_backend`) or to run here.
    #[test]
    fn agent_sessions_schema_matches_migration() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).expect("create_terminal");
        let (_p, ws) = create_project(&mut conn, "proj", "/work", None).expect("create_project");

        let s = record_session_start(
            &mut conn,
            &t.id,
            AGENT_KIND_CLAUDE_CODE,
            SessionCapture {
                workspace_id: Some(ws.id.clone()),
                external_session_id: "ext-abc".to_string(),
                cwd: "/work/sub".to_string(),
                transcript_path: Some("/home/u/.claude/x.jsonl".to_string()),
                metadata_json: Some(r#"{"source":"startup"}"#.to_string()),
            },
        )
        .expect("record_session_start");

        let got = get_session(&mut conn, &s.id).unwrap().unwrap();
        assert_eq!(got, s, "select returns the inserted row");
        assert_eq!(got.terminal_id, t.id);
        assert_eq!(got.workspace_id.as_deref(), Some(ws.id.as_str()));
        assert_eq!(got.agent_kind, AGENT_KIND_CLAUDE_CODE);
        assert_eq!(got.external_session_id, "ext-abc");
        assert_eq!(got.cwd, "/work/sub");
        assert_eq!(got.state, SESSION_STATE_ACTIVE, "fresh session is active");
        assert_eq!(
            got.transcript_path.as_deref(),
            Some("/home/u/.claude/x.jsonl")
        );
        assert_eq!(got.metadata_json, r#"{"source":"startup"}"#);
        assert!(got.started_at > 0);
        assert_eq!(got.ended_at, None, "fresh session has not ended");
        assert_eq!(
            got.last_seen_at, got.started_at,
            "fresh: last_seen == started"
        );
    }

    /// `metadata_json` defaults to the empty object when the capture omits it.
    #[test]
    fn agent_session_metadata_defaults_to_empty_object() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let s =
            record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("e1")).unwrap();
        assert_eq!(s.metadata_json, "{}", "omitted metadata stores '{{}}'");
    }

    /// The `agent_kind` CHECK rejects anything outside the v1 vocabulary — proves
    /// the migration constraint reached the DB.
    #[test]
    fn agent_kind_check_constraint_enforced() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let bad = diesel::insert_into(agent_sessions::table)
            .values((
                agent_sessions::id.eq(Uuid::now_v7().to_string()),
                agent_sessions::terminal_id.eq(&t.id),
                agent_sessions::agent_kind.eq("not_an_agent"),
                agent_sessions::external_session_id.eq("e"),
                agent_sessions::cwd.eq("/work"),
            ))
            .execute(&mut conn);
        assert!(bad.is_err(), "agent_kind CHECK must reject unknown kinds");
    }

    /// The `state` CHECK rejects anything outside active|ended|unknown|resume_failed.
    #[test]
    fn session_state_check_constraint_enforced() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let bad = diesel::insert_into(agent_sessions::table)
            .values((
                agent_sessions::id.eq(Uuid::now_v7().to_string()),
                agent_sessions::terminal_id.eq(&t.id),
                agent_sessions::agent_kind.eq(AGENT_KIND_CLAUDE_CODE),
                agent_sessions::external_session_id.eq("e"),
                agent_sessions::cwd.eq("/work"),
                agent_sessions::state.eq("bogus"),
            ))
            .execute(&mut conn);
        assert!(bad.is_err(), "state CHECK must reject unknown states");
    }

    /// At most ONE active session per (terminal_id, agent_kind): a SECOND raw active
    /// insert for the same pair violates the partial unique index.
    #[test]
    fn one_active_session_per_terminal_agent_enforced() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("e1")).unwrap();

        // A raw insert of a SECOND active row for the same terminal+agent must be
        // rejected by `idx_one_active_session_per_terminal_agent`.
        let dup = diesel::insert_into(agent_sessions::table)
            .values(NewAgentSession {
                id: Uuid::now_v7().to_string(),
                terminal_id: t.id.clone(),
                workspace_id: None,
                agent_kind: AGENT_KIND_CLAUDE_CODE.to_string(),
                external_session_id: "e2".to_string(),
                cwd: "/work".to_string(),
                state: SESSION_STATE_ACTIVE.to_string(),
                transcript_path: None,
                metadata_json: "{}".to_string(),
                started_at: now_millis(),
                last_seen_at: now_millis(),
            })
            .execute(&mut conn);
        assert!(
            dup.is_err(),
            "a second active session for the same terminal+agent must be rejected"
        );
    }

    /// A different `agent_kind` on the SAME terminal CAN be active simultaneously
    /// (the uniqueness is per terminal+agent, not per terminal). And two DIFFERENT
    /// terminals can each host an active claude_code session.
    #[test]
    fn distinct_agents_and_terminals_each_keep_an_active_session() {
        let mut conn = open_in_memory();
        let t1 = create_terminal(&mut conn, "/w1", None).unwrap();
        let t2 = create_terminal(&mut conn, "/w2", None).unwrap();

        record_session_start(&mut conn, &t1.id, AGENT_KIND_CLAUDE_CODE, capture("c1")).unwrap();
        // Same terminal, DIFFERENT agent → allowed.
        record_session_start(&mut conn, &t1.id, AGENT_KIND_CODEX, capture("x1")).unwrap();
        // Different terminal, same agent → allowed.
        record_session_start(&mut conn, &t2.id, AGENT_KIND_CLAUDE_CODE, capture("c2")).unwrap();

        assert!(
            active_session_for(&mut conn, &t1.id, AGENT_KIND_CLAUDE_CODE)
                .unwrap()
                .is_some()
        );
        assert!(active_session_for(&mut conn, &t1.id, AGENT_KIND_CODEX)
            .unwrap()
            .is_some());
        assert!(
            active_session_for(&mut conn, &t2.id, AGENT_KIND_CLAUDE_CODE)
                .unwrap()
                .is_some()
        );
    }

    /// A `resume` SessionStart on the SAME terminal+agent UPDATES the live row in
    /// place (one row, not two): the external id / last_seen advance, no new row.
    #[test]
    fn record_session_start_upserts_the_active_row() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let first =
            record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("e1")).unwrap();

        let second = record_session_start(
            &mut conn,
            &t.id,
            AGENT_KIND_CLAUDE_CODE,
            SessionCapture {
                external_session_id: "e2".to_string(),
                metadata_json: Some(r#"{"source":"resume"}"#.to_string()),
                ..capture("ignored")
            },
        )
        .unwrap();

        assert_eq!(
            second.id, first.id,
            "the SAME row is refreshed, not a new one"
        );
        assert_eq!(second.external_session_id, "e2");
        assert_eq!(second.metadata_json, r#"{"source":"resume"}"#);
        assert_eq!(
            sessions_for_terminal(&mut conn, &t.id).unwrap().len(),
            1,
            "a resume keeps exactly ONE row for the terminal"
        );
    }

    /// A `resume` SessionStart for a session that was swept to `unknown` (stale active,
    /// probable kill) REVIVES that exact row back to `active` rather than inserting a
    /// second row — so a kill→resume cycle never orphans the original (which would keep
    /// re-qualifying as a resume + close-warning candidate every boot).
    #[test]
    fn record_session_start_revives_a_swept_unknown_row() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let first =
            record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("e1")).unwrap();
        // Simulate the boot sweep flipping the stale active row to `unknown`.
        diesel::update(agent_sessions::table.find(&first.id))
            .set(agent_sessions::state.eq(SESSION_STATE_UNKNOWN))
            .execute(&mut conn)
            .unwrap();

        // The resume re-attaches with the SAME external id → revive the same row.
        let revived =
            record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("e1")).unwrap();
        assert_eq!(
            revived.id, first.id,
            "the unknown row is revived, not duplicated"
        );
        assert_eq!(
            revived.state, SESSION_STATE_ACTIVE,
            "revived back to active"
        );
        assert_eq!(
            sessions_for_terminal(&mut conn, &t.id).unwrap().len(),
            1,
            "a resume of a swept session keeps exactly ONE row (no orphan)"
        );
    }

    /// Clean end: `mark_session_ended` flips state→ended, stamps `ended_at`, and
    /// VACATES the active slot so a fresh SessionStart can take it (yielding a second,
    /// distinct row).
    #[test]
    fn end_then_restart_yields_a_new_active_row() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let s1 =
            record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("e1")).unwrap();

        assert_eq!(mark_session_ended(&mut conn, &s1.id).unwrap(), 1);
        let ended = get_session(&mut conn, &s1.id).unwrap().unwrap();
        assert_eq!(ended.state, SESSION_STATE_ENDED);
        assert!(
            ended.ended_at.is_some(),
            "ended_at is stamped on a clean end"
        );
        assert!(
            active_session_for(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE)
                .unwrap()
                .is_none(),
            "no active session after a clean end"
        );

        // The active slot is free again → a new SessionStart inserts a distinct row.
        let s2 =
            record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("e2")).unwrap();
        assert_ne!(s2.id, s1.id, "a post-end restart is a NEW session row");
        assert_eq!(s2.state, SESSION_STATE_ACTIVE);
        assert_eq!(
            sessions_for_terminal(&mut conn, &t.id).unwrap().len(),
            2,
            "the ended row is kept as history alongside the new active one"
        );
    }

    /// resume_failed transition: state flips, `ended_at` is NOT stamped (it did not
    /// end cleanly), and the active slot is vacated.
    #[test]
    fn mark_resume_failed_transitions_without_ended_at() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let s =
            record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("e1")).unwrap();

        assert_eq!(mark_session_resume_failed(&mut conn, &s.id).unwrap(), 1);
        let got = get_session(&mut conn, &s.id).unwrap().unwrap();
        assert_eq!(got.state, SESSION_STATE_RESUME_FAILED);
        assert_eq!(got.ended_at, None, "resume_failed does not stamp ended_at");
        assert!(
            active_session_for(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE)
                .unwrap()
                .is_none(),
            "resume_failed vacates the active slot"
        );
    }

    /// active→unknown by `last_seen_at` péremption: a stale active row (last_seen far
    /// in the past) is swept to `unknown`; a FRESH active row and a non-active row are
    /// left untouched. The unknown row is still readable (a resume candidate).
    #[test]
    fn sweep_stale_active_sessions_flips_only_stale_active_rows() {
        let mut conn = open_in_memory();
        let t_stale = create_terminal(&mut conn, "/stale", None).unwrap();
        let t_fresh = create_terminal(&mut conn, "/fresh", None).unwrap();
        let t_ended = create_terminal(&mut conn, "/ended", None).unwrap();

        // A stale active row: insert directly with last_seen_at well in the past.
        let stale_id = Uuid::now_v7().to_string();
        let past = now_millis() - SESSION_STALE_AFTER_MS - 60_000; // an hour-ish ago
        diesel::insert_into(agent_sessions::table)
            .values(NewAgentSession {
                id: stale_id.clone(),
                terminal_id: t_stale.id.clone(),
                workspace_id: None,
                agent_kind: AGENT_KIND_CLAUDE_CODE.to_string(),
                external_session_id: "stale".to_string(),
                cwd: "/stale".to_string(),
                state: SESSION_STATE_ACTIVE.to_string(),
                transcript_path: None,
                metadata_json: "{}".to_string(),
                started_at: past,
                last_seen_at: past,
            })
            .execute(&mut conn)
            .unwrap();

        // A fresh active row (last_seen = now).
        let fresh = record_session_start(
            &mut conn,
            &t_fresh.id,
            AGENT_KIND_CLAUDE_CODE,
            capture("fresh"),
        )
        .unwrap();
        // An ended row (must never be swept).
        let ended = record_session_start(
            &mut conn,
            &t_ended.id,
            AGENT_KIND_CLAUDE_CODE,
            capture("ended"),
        )
        .unwrap();
        mark_session_ended(&mut conn, &ended.id).unwrap();

        let flipped = sweep_stale_active_sessions(&mut conn, SESSION_STALE_AFTER_MS).unwrap();
        assert_eq!(flipped, 1, "exactly the one stale active row is swept");

        assert_eq!(
            get_session(&mut conn, &stale_id).unwrap().unwrap().state,
            SESSION_STATE_UNKNOWN,
            "the stale active row became unknown"
        );
        assert_eq!(
            get_session(&mut conn, &fresh.id).unwrap().unwrap().state,
            SESSION_STATE_ACTIVE,
            "a fresh active row is left active"
        );
        assert_eq!(
            get_session(&mut conn, &ended.id).unwrap().unwrap().state,
            SESSION_STATE_ENDED,
            "an ended row is never swept"
        );

        // The sweep is idempotent: a second pass flips nothing more.
        assert_eq!(
            sweep_stale_active_sessions(&mut conn, SESSION_STALE_AFTER_MS).unwrap(),
            0,
            "second sweep is a no-op (unknown rows are not active)"
        );
    }

    /// An `unknown` session no longer occupies the active slot, so the partial unique
    /// index does NOT block a fresh SessionStart on the same terminal (the relaunch
    /// path). This proves `unknown` is genuinely outside the active uniqueness scope.
    #[test]
    fn unknown_session_frees_the_active_slot() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let past = now_millis() - SESSION_STALE_AFTER_MS - 60_000;
        let stale_id = Uuid::now_v7().to_string();
        diesel::insert_into(agent_sessions::table)
            .values(NewAgentSession {
                id: stale_id.clone(),
                terminal_id: t.id.clone(),
                workspace_id: None,
                agent_kind: AGENT_KIND_CLAUDE_CODE.to_string(),
                external_session_id: "old".to_string(),
                cwd: "/work".to_string(),
                state: SESSION_STATE_ACTIVE.to_string(),
                transcript_path: None,
                metadata_json: "{}".to_string(),
                started_at: past,
                last_seen_at: past,
            })
            .execute(&mut conn)
            .unwrap();
        sweep_stale_active_sessions(&mut conn, SESSION_STALE_AFTER_MS).unwrap();

        // A fresh SessionStart now inserts a NEW active row (the unknown row no longer
        // holds the unique active slot).
        let s =
            record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("new")).unwrap();
        assert_ne!(s.id, stale_id);
        assert_eq!(s.state, SESSION_STATE_ACTIVE);
        assert_eq!(sessions_for_terminal(&mut conn, &t.id).unwrap().len(), 2);
    }

    /// The project is DERIVED via the workspace (no project_id column). An attached
    /// session resolves to its workspace's project; an unattached session resolves to
    /// None.
    #[test]
    fn project_id_for_session_derives_via_workspace() {
        let mut conn = open_in_memory();
        let (project, ws) =
            create_project(&mut conn, "proj", "/work", None).expect("create_project");
        let t = create_terminal(&mut conn, "/work", None).unwrap();

        let attached = record_session_start(
            &mut conn,
            &t.id,
            AGENT_KIND_CLAUDE_CODE,
            SessionCapture {
                workspace_id: Some(ws.id.clone()),
                ..capture("attached")
            },
        )
        .unwrap();
        assert_eq!(
            project_id_for_session(&mut conn, &attached.id).unwrap(),
            Some(project.id.clone()),
            "an attached session derives its project via the workspace"
        );

        let t2 = create_terminal(&mut conn, "/loose", None).unwrap();
        let loose =
            record_session_start(&mut conn, &t2.id, AGENT_KIND_CLAUDE_CODE, capture("loose"))
                .unwrap();
        assert_eq!(
            project_id_for_session(&mut conn, &loose.id).unwrap(),
            None,
            "an unattached session has no derivable project"
        );
    }

    /// FK behaviour: deleting a terminal CASCADES its sessions away; deleting a
    /// workspace SET-NULLs the anchor (the session survives, unattached).
    #[test]
    fn session_fk_cascade_on_terminal_and_set_null_on_workspace() {
        let mut conn = open_in_memory();
        let (_p, ws) = create_project(&mut conn, "proj", "/work", None).unwrap();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let s = record_session_start(
            &mut conn,
            &t.id,
            AGENT_KIND_CLAUDE_CODE,
            SessionCapture {
                workspace_id: Some(ws.id.clone()),
                ..capture("e")
            },
        )
        .unwrap();

        // Deleting the WORKSPACE detaches the session (SET NULL), it survives.
        diesel::delete(workspaces::table.find(&ws.id))
            .execute(&mut conn)
            .unwrap();
        let after_ws = get_session(&mut conn, &s.id).unwrap().unwrap();
        assert_eq!(
            after_ws.workspace_id, None,
            "workspace delete SET-NULLs anchor"
        );

        // Deleting the TERMINAL cascades the session away.
        close_terminal(&mut conn, &t.id).unwrap(); // status change is fine; now hard-delete
        diesel::delete(terminals::table.find(&t.id))
            .execute(&mut conn)
            .unwrap();
        assert!(
            get_session(&mut conn, &s.id).unwrap().is_none(),
            "terminal delete CASCADEs its sessions away"
        );
    }

    // --- Project resume option (PRD-5 #5, schema v8) ---------------------

    /// A fresh project defaults to resume OFF, and `set_project_resume_agent_sessions`
    /// round-trips the flag (the per-project opt-in, default OFF).
    #[test]
    fn project_resume_option_defaults_off_and_round_trips() {
        let mut conn = open_in_memory();
        let (project, _root) = create_project(&mut conn, "proj", "/work", None).unwrap();
        assert!(
            !project.resume_agent_sessions,
            "a fresh project defaults to resume OFF"
        );

        assert_eq!(
            set_project_resume_agent_sessions(&mut conn, &project.id, true).unwrap(),
            1
        );
        let got = list_projects(&mut conn)
            .unwrap()
            .into_iter()
            .find(|p| p.id == project.id)
            .unwrap();
        assert!(got.resume_agent_sessions, "the ON flag is persisted");

        set_project_resume_agent_sessions(&mut conn, &project.id, false).unwrap();
        let off = list_projects(&mut conn)
            .unwrap()
            .into_iter()
            .find(|p| p.id == project.id)
            .unwrap();
        assert!(!off.resume_agent_sessions, "toggling back to OFF persists");
    }

    /// The v8 UPGRADE path: revert v8 so `resume_agent_sessions` is gone, insert a
    /// pre-v8 project row, then re-apply the migration. The old project must backfill
    /// to resume OFF (DEFAULT 0) — no surprise resume on upgrade.
    #[test]
    fn migration_v8_upgrade_backfills_projects_with_resume_off() {
        use diesel::sql_query;
        let mut conn = open_in_memory();

        // Step DOWN past v15 (v2.4 Phase 3 specs.workspace_id index) then v14 (v2.4 Phase 2
        // task domain) then v13 (v2.4 Phase 1 Spec domain) then v12 (v2.1 Phase 4
        // domain_events) then v11 (v2.1 data backfill) then v10 (v2.1 canonical structure)
        // then v9 (#11 project order) then v8 → the resume_agent_sessions column is gone.
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v18 (review_tasks.spec_id) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v17 (review_tasks/review_reads) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v16 (review_comments) cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v15 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v14 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v13 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v12 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v11 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v10 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v9 cleanly");
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v8 cleanly");
        let col_present: QueryResult<usize> =
            sql_query("SELECT resume_agent_sessions FROM projects").execute(&mut conn);
        assert!(
            col_present.is_err(),
            "after reverting v8 the resume_agent_sessions column must NOT exist"
        );

        // Insert a project into the PRE-v8 schema (raw SQL: the diesel model now knows
        // the v8 column).
        let id = Uuid::now_v7().to_string();
        sql_query(format!(
            "INSERT INTO projects (id, name) VALUES ('{id}', 'legacy')"
        ))
        .execute(&mut conn)
        .expect("insert a pre-v8 project row");

        // UPGRADE: re-apply v8. The old row backfills to the DEFAULT 0 (OFF).
        run_migrations(&mut conn).expect("re-apply v8 (the upgrade)");
        let got = list_projects(&mut conn)
            .unwrap()
            .into_iter()
            .find(|p| p.id == id)
            .expect("the pre-v8 project survived the upgrade");
        assert!(
            !got.resume_agent_sessions,
            "an upgraded old project backfills to resume OFF — no surprise resume"
        );
    }

    /// `project_resumes_for_terminal`: a terminal attached to a resume-ON project's
    /// workspace → true; attached to a resume-OFF project → false; a loose terminal
    /// (no workspace/project) → false (OFF by construction).
    #[test]
    fn project_resumes_for_terminal_resolves_via_workspace() {
        let mut conn = open_in_memory();
        let (project, ws) = create_project(&mut conn, "proj", "/work", None).unwrap();

        // A loose terminal (no workspace) → false.
        let loose = create_terminal(&mut conn, "/loose", None).unwrap();
        assert!(
            !project_resumes_for_terminal(&mut conn, &loose.id).unwrap(),
            "a loose terminal (no project) is OFF by construction"
        );

        // Attach a terminal to the project's workspace; OFF by default → false.
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        attach_terminal(&mut conn, &t.id, &ws.id, BINDING_AUTO).unwrap();
        assert!(
            !project_resumes_for_terminal(&mut conn, &t.id).unwrap(),
            "attached to a resume-OFF project → false"
        );

        // Turn the project's resume ON → true.
        set_project_resume_agent_sessions(&mut conn, &project.id, true).unwrap();
        assert!(
            project_resumes_for_terminal(&mut conn, &t.id).unwrap(),
            "attached to a resume-ON project → true"
        );

        // An unknown terminal → false (no panic).
        assert!(!project_resumes_for_terminal(&mut conn, "nope").unwrap());
    }

    /// `resume_candidates_on_boot`: only ALIVE terminals with `active`/`unknown`
    /// sessions are candidates; the project flag is COALESCED (loose → false); a
    /// `closed` terminal and an `ended` session are excluded.
    #[test]
    fn resume_candidates_on_boot_gathers_alive_candidates_with_flag() {
        let mut conn = open_in_memory();
        let (project, ws) = create_project(&mut conn, "proj", "/work", None).unwrap();
        set_project_resume_agent_sessions(&mut conn, &project.id, true).unwrap();

        // 1) Alive terminal attached to the resume-ON project, active session → candidate.
        //    Carries a transcript_path so the query surfaces it for the #53 bridge stat.
        let t_on = create_terminal(&mut conn, "/work", None).unwrap();
        attach_terminal(&mut conn, &t_on.id, &ws.id, BINDING_AUTO).unwrap();
        let s_on = record_session_start(
            &mut conn,
            &t_on.id,
            AGENT_KIND_CLAUDE_CODE,
            SessionCapture {
                workspace_id: Some(ws.id.clone()),
                transcript_path: Some("/home/u/.claude/on-id.jsonl".to_string()),
                ..capture("on-id")
            },
        )
        .unwrap();

        // 2) Alive LOOSE terminal (no project), active session → candidate, flag false.
        let t_loose = create_terminal(&mut conn, "/loose", None).unwrap();
        record_session_start(
            &mut conn,
            &t_loose.id,
            AGENT_KIND_CLAUDE_CODE,
            capture("loose-id"),
        )
        .unwrap();

        // 3) CLOSED terminal with an active session → NOT a candidate (status filter).
        let t_closed = create_terminal(&mut conn, "/closed", None).unwrap();
        record_session_start(
            &mut conn,
            &t_closed.id,
            AGENT_KIND_CLAUDE_CODE,
            capture("closed-id"),
        )
        .unwrap();
        close_terminal(&mut conn, &t_closed.id).unwrap();

        // 4) Alive terminal whose session is ENDED → NOT a candidate (state filter).
        let t_ended = create_terminal(&mut conn, "/ended", None).unwrap();
        let ended = record_session_start(
            &mut conn,
            &t_ended.id,
            AGENT_KIND_CLAUDE_CODE,
            capture("ended-id"),
        )
        .unwrap();
        mark_session_ended(&mut conn, &ended.id).unwrap();

        let candidates = resume_candidates_on_boot(&mut conn).unwrap();
        let by_session: std::collections::HashMap<&str, &ResumeCandidate> = candidates
            .iter()
            .map(|c| (c.session_id.as_str(), c))
            .collect();

        // Exactly the two alive+active sessions are candidates.
        assert_eq!(
            candidates.len(),
            2,
            "only the two alive+active sessions are candidates"
        );

        let on = by_session
            .get(s_on.id.as_str())
            .expect("the resume-ON candidate is present");
        assert!(
            on.project_resume_on,
            "the attached resume-ON project flag is true"
        );
        assert_eq!(on.external_session_id, "on-id");
        assert_eq!(on.session_state, SESSION_STATE_ACTIVE);
        assert_eq!(
            on.transcript_path.as_deref(),
            Some("/home/u/.claude/on-id.jsonl"),
            "the captured transcript_path is surfaced for the #53 stat"
        );

        let loose = candidates
            .iter()
            .find(|c| c.external_session_id == "loose-id")
            .expect("the loose candidate is present");
        assert!(
            !loose.project_resume_on,
            "a loose terminal coalesces to resume OFF"
        );
        assert_eq!(loose.transcript_path, None, "no transcript captured → None");
    }

    /// `active_agent_sessions` (finding #55) returns ONE `(terminal_id, agent_kind)` pair
    /// per ACTIVE session and excludes ended ones — the set the sidebar maps to a
    /// provider icon. Ending a session drops it from the list (the icon reverts).
    #[test]
    fn active_agent_sessions_lists_active_pairs_and_drops_ended() {
        let mut conn = open_in_memory();

        // Two terminals with an active Claude session.
        let t1 = create_terminal(&mut conn, "/a", None).unwrap();
        record_session_start(&mut conn, &t1.id, AGENT_KIND_CLAUDE_CODE, capture("a-id")).unwrap();
        let t2 = create_terminal(&mut conn, "/b", None).unwrap();
        record_session_start(&mut conn, &t2.id, AGENT_KIND_CLAUDE_CODE, capture("b-id")).unwrap();

        // A third terminal whose session is ENDED → excluded.
        let t3 = create_terminal(&mut conn, "/c", None).unwrap();
        let ended =
            record_session_start(&mut conn, &t3.id, AGENT_KIND_CLAUDE_CODE, capture("c-id"))
                .unwrap();
        mark_session_ended(&mut conn, &ended.id).unwrap();

        let active = active_agent_sessions(&mut conn).unwrap();
        assert_eq!(active.len(), 2, "only the two active sessions are listed");
        assert!(
            active
                .iter()
                .any(|s| s.terminal_id == t1.id && s.agent_kind == AGENT_KIND_CLAUDE_CODE),
            "terminal 1's active claude session is present"
        );
        assert!(
            active.iter().any(|s| s.terminal_id == t2.id),
            "terminal 2 is present"
        );
        assert!(
            !active.iter().any(|s| s.terminal_id == t3.id),
            "the ended session's terminal is NOT present (icon reverts)"
        );

        // Ending t1's session drops it from the list.
        let t1_session = active_session_for(&mut conn, &t1.id, AGENT_KIND_CLAUDE_CODE)
            .unwrap()
            .expect("t1 has an active session");
        mark_session_ended(&mut conn, &t1_session.id).unwrap();
        let after = active_agent_sessions(&mut conn).unwrap();
        assert_eq!(after.len(), 1, "after ending t1, only t2 remains active");
        assert_eq!(after[0].terminal_id, t2.id);
    }

    /// `close_warning_candidates` (PRD-5 #6) returns every alive terminal's LIVE
    /// session with its project flag + the message fields. The bridge applies the pure
    /// warn gate; here we assert the QUERY surfaces the right rows and data: a closed
    /// terminal and an ended session are excluded; the project flag is COALESCED; the
    /// terminal label + workspace name come through.
    #[test]
    fn close_warning_candidates_surface_live_sessions_with_message_fields() {
        let mut conn = open_in_memory();
        let (project, ws) = create_project(&mut conn, "proj", "/work", None).unwrap();

        // Resume-ON project + active session, with a terminal label.
        set_project_resume_agent_sessions(&mut conn, &project.id, true).unwrap();
        let t_on = create_terminal(&mut conn, "/work", Some("build".to_string())).unwrap();
        attach_terminal(&mut conn, &t_on.id, &ws.id, BINDING_AUTO).unwrap();
        record_session_start(
            &mut conn,
            &t_on.id,
            AGENT_KIND_CLAUDE_CODE,
            SessionCapture {
                workspace_id: Some(ws.id.clone()),
                ..capture("on-id")
            },
        )
        .unwrap();

        // A loose alive terminal with an active session (no project → flag false).
        let t_loose = create_terminal(&mut conn, "/loose", None).unwrap();
        record_session_start(
            &mut conn,
            &t_loose.id,
            AGENT_KIND_CODEX,
            capture("loose-id"),
        )
        .unwrap();

        // A closed terminal + an ended session must NOT surface.
        let t_closed = create_terminal(&mut conn, "/c", None).unwrap();
        record_session_start(
            &mut conn,
            &t_closed.id,
            AGENT_KIND_CLAUDE_CODE,
            capture("c-id"),
        )
        .unwrap();
        close_terminal(&mut conn, &t_closed.id).unwrap();
        let t_ended = create_terminal(&mut conn, "/e", None).unwrap();
        let ended = record_session_start(
            &mut conn,
            &t_ended.id,
            AGENT_KIND_CLAUDE_CODE,
            capture("e-id"),
        )
        .unwrap();
        mark_session_ended(&mut conn, &ended.id).unwrap();

        let rows = close_warning_candidates(&mut conn).unwrap();
        assert_eq!(rows.len(), 2, "only the two alive+live sessions surface");

        let on = rows
            .iter()
            .find(|w| w.external_session_id == "on-id")
            .unwrap();
        assert!(
            on.project_resume_on,
            "resume-ON flag surfaces (bridge will filter it out)"
        );
        assert_eq!(
            on.terminal_label.as_deref(),
            Some("build"),
            "terminal label comes through"
        );
        assert_eq!(
            on.workspace_name.as_deref(),
            Some("root"),
            "workspace name comes through"
        );
        assert_eq!(on.agent_kind, AGENT_KIND_CLAUDE_CODE);

        let loose = rows
            .iter()
            .find(|w| w.external_session_id == "loose-id")
            .unwrap();
        assert!(
            !loose.project_resume_on,
            "loose terminal coalesces to OFF → will warn"
        );
        assert_eq!(loose.agent_kind, AGENT_KIND_CODEX);
        assert_eq!(loose.workspace_name, None);
    }

    /// SUPERSEDE-ON-START (finding #82): when a NEW session B starts on a terminal that
    /// still carries a stale-but-`unknown` session A of the same agent (a kill left it
    /// behind, the boot sweep flipped it `active`→`unknown`), recording B marks A `ended`.
    /// Only one live (`active`/`unknown`) row remains — the close-warning never lists the
    /// terminal twice. (The partial-unique index already forbids two `active` rows; this
    /// covers the `unknown` sibling it cannot.)
    #[test]
    fn record_session_start_supersedes_stale_sibling() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();

        // Session A, then simulate the boot sweep flipping it active→unknown (a kill).
        let a = record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("a-id"))
            .unwrap();
        diesel::update(agent_sessions::table.find(&a.id))
            .set(agent_sessions::state.eq(SESSION_STATE_UNKNOWN))
            .execute(&mut conn)
            .unwrap();

        // A genuinely NEW session B (different external id) starts on the SAME terminal.
        let b = record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("b-id"))
            .unwrap();
        assert_ne!(b.id, a.id, "B is a new row (distinct external id)");

        // A is now `ended` (superseded); B is the single live row.
        let a_after = get_session(&mut conn, &a.id).unwrap().unwrap();
        assert_eq!(
            a_after.state, SESSION_STATE_ENDED,
            "the stale sibling A is superseded → ended"
        );
        assert!(a_after.ended_at.is_some(), "supersede stamps ended_at");
        let b_after = get_session(&mut conn, &b.id).unwrap().unwrap();
        assert_eq!(b_after.state, SESSION_STATE_ACTIVE, "B stays active");

        // Exactly ONE live (active/unknown) row for the terminal → no close-warning dupe.
        let live = sessions_for_terminal(&mut conn, &t.id)
            .unwrap()
            .into_iter()
            .filter(|s| s.state == SESSION_STATE_ACTIVE || s.state == SESSION_STATE_UNKNOWN)
            .count();
        assert_eq!(live, 1, "only the new session is live after supersede");
    }

    /// Supersede does NOT touch sessions of a DIFFERENT terminal or a different agent
    /// kind — the filter is scoped to the same `terminal_id` + `agent_kind`.
    #[test]
    fn record_session_start_supersede_is_scoped_to_terminal_and_agent() {
        let mut conn = open_in_memory();
        let t1 = create_terminal(&mut conn, "/w1", None).unwrap();
        let t2 = create_terminal(&mut conn, "/w2", None).unwrap();

        // A live session on t2 (other terminal) and a live Codex session on t1 (other
        // agent) — both must survive a new Claude session on t1.
        let other_term =
            record_session_start(&mut conn, &t2.id, AGENT_KIND_CLAUDE_CODE, capture("t2")).unwrap();
        let other_agent =
            record_session_start(&mut conn, &t1.id, AGENT_KIND_CODEX, capture("codex")).unwrap();

        record_session_start(&mut conn, &t1.id, AGENT_KIND_CLAUDE_CODE, capture("claude")).unwrap();

        assert_eq!(
            get_session(&mut conn, &other_term.id)
                .unwrap()
                .unwrap()
                .state,
            SESSION_STATE_ACTIVE,
            "a session on another terminal is untouched"
        );
        assert_eq!(
            get_session(&mut conn, &other_agent.id)
                .unwrap()
                .unwrap()
                .state,
            SESSION_STATE_ACTIVE,
            "a session of another agent on the same terminal is untouched"
        );
    }

    /// `mark_sessions_ended` (finding #82 batch helper): ends every id in the slice
    /// (stamping `ended_at`), leaves others alone, and is a no-op for an empty slice.
    #[test]
    fn mark_sessions_ended_batches_the_transition() {
        let mut conn = open_in_memory();
        let t1 = create_terminal(&mut conn, "/w1", None).unwrap();
        let t2 = create_terminal(&mut conn, "/w2", None).unwrap();
        let t3 = create_terminal(&mut conn, "/w3", None).unwrap();
        let s1 =
            record_session_start(&mut conn, &t1.id, AGENT_KIND_CLAUDE_CODE, capture("s1")).unwrap();
        let s2 =
            record_session_start(&mut conn, &t2.id, AGENT_KIND_CLAUDE_CODE, capture("s2")).unwrap();
        let s3 =
            record_session_start(&mut conn, &t3.id, AGENT_KIND_CLAUDE_CODE, capture("s3")).unwrap();

        // Empty slice → no-op.
        assert_eq!(mark_sessions_ended(&mut conn, &[]).unwrap(), 0);

        // End s1 + s2 in one batch; s3 stays active.
        let n = mark_sessions_ended(&mut conn, &[s1.id.clone(), s2.id.clone()]).unwrap();
        assert_eq!(n, 2, "both ids transition");
        for id in [&s1.id, &s2.id] {
            let got = get_session(&mut conn, id).unwrap().unwrap();
            assert_eq!(got.state, SESSION_STATE_ENDED);
            assert!(got.ended_at.is_some(), "ended_at is stamped");
        }
        assert_eq!(
            get_session(&mut conn, &s3.id).unwrap().unwrap().state,
            SESSION_STATE_ACTIVE,
            "an id not in the batch is untouched"
        );
    }

    /// DEDUP BY TERMINAL (finding #82): if a terminal somehow carries TWO live rows (a
    /// stale `unknown` left by a kill alongside a fresh `active`), `close_warning_candidates`
    /// collapses them to ONE entry — the most recent (`started_at`) — so the dialog never
    /// lists the same terminal twice. We force the two-row state directly (the supersede +
    /// boot-cleanup normally prevent it) to prove the dedup is independent belt-and-braces.
    #[test]
    fn close_warning_candidates_dedup_one_entry_per_terminal() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();

        // An OLDER `unknown` row (started in the past), inserted directly.
        let past = now_millis() - 60_000;
        let old_id = Uuid::now_v7().to_string();
        diesel::insert_into(agent_sessions::table)
            .values(NewAgentSession {
                id: old_id.clone(),
                terminal_id: t.id.clone(),
                workspace_id: None,
                agent_kind: AGENT_KIND_CLAUDE_CODE.to_string(),
                external_session_id: "old".to_string(),
                cwd: "/work".to_string(),
                state: SESSION_STATE_UNKNOWN.to_string(),
                transcript_path: None,
                metadata_json: "{}".to_string(),
                started_at: past,
                last_seen_at: past,
            })
            .execute(&mut conn)
            .unwrap();
        // A NEWER `active` row (started now).
        let new_id = Uuid::now_v7().to_string();
        let now = now_millis();
        diesel::insert_into(agent_sessions::table)
            .values(NewAgentSession {
                id: new_id.clone(),
                terminal_id: t.id.clone(),
                workspace_id: None,
                agent_kind: AGENT_KIND_CLAUDE_CODE.to_string(),
                external_session_id: "new".to_string(),
                cwd: "/work".to_string(),
                state: SESSION_STATE_ACTIVE.to_string(),
                transcript_path: None,
                metadata_json: "{}".to_string(),
                started_at: now,
                last_seen_at: now,
            })
            .execute(&mut conn)
            .unwrap();

        let rows = close_warning_candidates(&mut conn).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "two live rows on one terminal collapse to a single entry"
        );
        assert_eq!(
            rows[0].external_session_id, "new",
            "the most recent (started_at) session is kept"
        );
    }
}
