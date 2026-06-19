//! Persistence layer: SQLite owned by the Rust backend via Diesel (ADR-0001).
//!
//! This module owns the connection, applies the embedded migrations at startup,
//! and defines the `terminals` models + CRUD used by the bridge commands. The
//! `schema.rs` table definition is committed by hand (no `diesel` CLI required
//! at build); `tests::schema_matches_migration` enforces it stays in sync with
//! the SQL migration.
//!
//! Concurrency: SQLite has a single-writer model and `SqliteConnection` is not
//! `Sync`, so the connection lives behind a `Mutex`. nyx is mono-process
//! (single-instance), so one serialized connection is the simple, correct choice.

use std::path::Path;
use std::sync::Mutex;

use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use uuid::Uuid;

use crate::pathnorm;
use crate::schema::{
    agent_sessions, command_instances, managed_commands, projects, terminals, workspaces,
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
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = projects)]
struct NewProject {
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
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
    /// Canonical, normalized absolute path (see [`crate::pathnorm`]).
    pub path: String,
    pub branch: Option<String>,
    pub is_root: bool,
    /// Sidebar open/closed state, persisted across restarts (`false` = open).
    pub collapsed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = workspaces)]
struct NewWorkspace {
    id: String,
    project_id: String,
    name: String,
    path: String,
    branch: Option<String>,
    is_root: bool,
    created_at: i64,
    updated_at: i64,
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

/// Managed Tauri state: the single serialized SQLite connection.
pub struct Db {
    conn: Mutex<SqliteConnection>,
}

impl Db {
    /// Open (creating if absent) the SQLite database at `db_path`, apply pending
    /// migrations, and wrap it for use as Tauri state.
    ///
    /// `db_path`'s parent directory must already exist (callers derive it from
    /// `app_data_dir` and create it). Enables foreign-key enforcement, which is
    /// off by default in SQLite and load-bearing for the relational tables of
    /// later PRDs.
    pub fn open(db_path: &Path) -> anyhow::Result<Self> {
        let url = db_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 db path: {}", db_path.display()))?;
        let mut conn = SqliteConnection::establish(url)
            .map_err(|e| anyhow::anyhow!("failed to open SQLite at {url}: {e}"))?;
        // Enforce foreign keys (off by default in SQLite). Harmless for v1's
        // single table; correct for the relational schema of later PRDs.
        diesel::sql_query("PRAGMA foreign_keys = ON;")
            .execute(&mut conn)
            .map_err(|e| anyhow::anyhow!("failed to enable foreign_keys: {e}"))?;
        run_migrations(&mut conn)?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    /// Run a closure with exclusive access to the connection. The single
    /// entry-point so callers never hold the lock across an await or a blocking
    /// call they don't control.
    pub fn with_conn<T>(&self, f: impl FnOnce(&mut SqliteConnection) -> T) -> T {
        let mut guard = self.conn.lock().expect("db mutex poisoned");
        f(&mut guard)
    }

    /// In-memory, migrated `Db` for tests — never touches the real
    /// `app_data_dir` file. Managed on the mock Tauri app by bridge tests.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Db {
            conn: Mutex::new(open_in_memory()),
        }
    }
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
    /// The number of migrations that are still pending (0 when `up_to_date`).
    pub pending_count: usize,
}

/// Check whether the DB schema matches the embedded migrations. Returns a
/// [`SchemaHealth`] snapshot. Best-effort: any error reading the migration
/// table degrades to `up_to_date = false` with a note that the check itself
/// failed, rather than panicking.
pub fn schema_health(conn: &mut SqliteConnection) -> SchemaHealth {
    match conn.pending_migrations(MIGRATIONS) {
        Ok(pending) => SchemaHealth {
            pending_count: pending.len(),
            up_to_date: pending.is_empty(),
        },
        Err(_) => SchemaHealth {
            up_to_date: false,
            pending_count: usize::MAX, // sentinel: check itself failed
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
    use std::process::Command;

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
        let output = Command::new("git")
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

/// Create a project plus its single, explicitly-named ROOT workspace anchored at
/// `root_path`, in one transaction. `root_name` names the root workspace
/// (defaults to [`DEFAULT_ROOT_WORKSPACE_NAME`] when `None`/empty). The path is
/// normalized before storage. Returns the new project and its root workspace.
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
        let project = diesel::insert_into(projects::table)
            .values(NewProject {
                id: project_id.clone(),
                name: name.to_string(),
                created_at: now,
                updated_at: now,
            })
            .returning(Project::as_returning())
            .get_result(conn)?;

        let branch = gitbranch::detect(&normalized);
        let workspace = diesel::insert_into(workspaces::table)
            .values(NewWorkspace {
                id: Uuid::now_v7().to_string(),
                project_id: project_id.clone(),
                name: workspace_name,
                path: normalized,
                branch,
                is_root: true,
                created_at: now,
                updated_at: now,
            })
            .returning(Workspace::as_returning())
            .get_result(conn)?;

        // Materialize the project's templates as instances for the new root
        // workspace. A brand-new project has no templates yet, so this is a no-op
        // here; it keeps the "create workspace → materialize instances" invariant
        // uniform across both workspace-creation paths (root + added).
        materialize_instances_for_workspace(conn, &workspace.id)?;

        Ok((project, workspace))
    })
}

/// List all projects, newest-created last (`created_at` asc, `id` asc tiebreak).
pub fn list_projects(conn: &mut SqliteConnection) -> QueryResult<Vec<Project>> {
    projects::table
        .order((projects::created_at.asc(), projects::id.asc()))
        .select(Project::as_select())
        .load(conn)
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
        .inner_join(
            workspaces::table.on(workspaces::id.nullable().eq(terminals::workspace_id)),
        )
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
                path: normalized,
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
        .order((workspaces::path.asc(), workspaces::id.asc()))
        .select(Workspace::as_select())
        .load(conn)
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
///    leaving `unread` as-is (a start is not an unseen result).
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
        // start over an idle/never-run instance). Leaves `unread` as-is — a start is
        // not an unseen result yet.
        diesel::update(command_instances::table.find(id))
            .set((
                command_instances::scrollback.eq(""),
                command_instances::last_state.eq(STATE_RUNNING),
                command_instances::last_exit_code.eq::<Option<i32>>(None),
                command_instances::ended_at.eq::<Option<i64>>(None),
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
            workspaces::path,
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
        .select(workspaces::path)
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

/// Delete a workspace (ON DELETE CASCADE removes its command instances, SET NULL
/// detaches its terminals). Returns rows deleted (0 = workspace not found).
/// The caller is responsible for guarding against live running instances before
/// calling this (see `instance_ids_for_workspace` + runner's `any_running`).
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
            workspaces::path,
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
            workspaces::path,
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
    let metadata = capture
        .metadata_json
        .unwrap_or_else(|| "{}".to_string());
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

        if let Some(id) = existing {
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
                .get_result(conn)
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
                    .get_result(conn)
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
                    .get_result(conn)
            }
        }
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
pub fn active_agent_sessions(
    conn: &mut SqliteConnection,
) -> QueryResult<Vec<ActiveAgentSession>> {
    agent_sessions::table
        .filter(agent_sessions::state.eq(SESSION_STATE_ACTIVE))
        .order((agent_sessions::terminal_id.asc(), agent_sessions::agent_kind.asc()))
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
        .order((
            agent_sessions::started_at.asc(),
            agent_sessions::id.asc(),
        ))
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
/// Returns the number of rows flipped. Run on each boot/scan (the kill-then-relaunch
/// path leaves a row `active`, and this rebascules it on the next scan).
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
        .inner_join(workspaces::table.on(
            workspaces::id.nullable().eq(agent_sessions::workspace_id),
        ))
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
pub fn resume_candidates_on_boot(
    conn: &mut SqliteConnection,
) -> QueryResult<Vec<ResumeCandidate>> {
    use diesel::dsl::sql;
    use diesel::sql_types::Bool;
    agent_sessions::table
        .inner_join(terminals::table.on(terminals::id.eq(agent_sessions::terminal_id)))
        .left_join(workspaces::table.on(
            workspaces::id.nullable().eq(agent_sessions::workspace_id),
        ))
        .left_join(projects::table.on(
            projects::id.nullable().eq(workspaces::project_id.nullable()),
        ))
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
/// the close-warning needs (PRD-5 #6). The actual warn/no-warn decision is the bridge's
/// (it applies [`crate::agent_resume::should_warn_on_close`] per row), so a resume-ON
/// project's sessions are RETURNED here but the bridge filters them out — keeping the
/// policy single-sourced in `agent_resume`. Newest started last for deterministic order.
#[cfg_attr(not(test), allow(dead_code))]
pub fn close_warning_candidates(conn: &mut SqliteConnection) -> QueryResult<Vec<CloseWarning>> {
    use diesel::dsl::sql;
    use diesel::sql_types::Bool;
    agent_sessions::table
        .inner_join(terminals::table.on(terminals::id.eq(agent_sessions::terminal_id)))
        .left_join(workspaces::table.on(
            workspaces::id.nullable().eq(agent_sessions::workspace_id),
        ))
        .left_join(projects::table.on(
            projects::id.nullable().eq(workspaces::project_id.nullable()),
        ))
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
        .load::<CloseWarning>(conn)
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
        assert!(!got.exec_state_unread, "old terminal is read (unread=false)");
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
        // exec-state migration is dir #4; later PRDs stacked dirs #5..#8 on top, so
        // reaching the pre-v4 schema peels all of them then dir #4 itself (5 reverts,
        // reverse order).
        for _ in 0..5 {
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
        assert!(row.exec_state_unread, "settled error on inactive term is unread");

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
        assert_eq!(normalized, 1, "exactly the one phantom-running row is normalized");

        // The phantom running is now idle, with no exit code and not unread.
        let got = get_terminal(&mut conn, &running.id).unwrap().unwrap();
        assert_eq!(got.exec_state, STATE_IDLE, "phantom running settled to idle");
        assert_eq!(got.exec_exit_code, None, "phantom exit code cleared");
        assert!(!got.exec_state_unread, "phantom is not an unread notification");

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
        assert!(bad_insert.is_err(), "raw insert with bad exec_state rejected");
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
        let got_t = get_terminal(&mut conn, &t.id).unwrap().expect("terminal row");
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
        assert_eq!(root.path, expected, "stored path is the normalized form");
        // And the normalized form has no trailing separator / redundant pieces.
        assert!(!root.path.ends_with('/') || root.path == "/");
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
        let status = std::process::Command::new("git")
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
        if std::process::Command::new("git")
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
        assert_eq!(w_other.path, w1.path, "both store the same canonical path");
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
                path: pathnorm::normalize(p("/r1/extra", "C:\\r1\\extra")),
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
        assert_eq!(ws[0].path, root.path, "rename does not touch the path");

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
        assert_eq!(got.path, feat.path, "rename does not touch the path");

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
            got_feat.path, feat.path,
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

        // Peel every migration stacked above the projects migration (dirs #8 → #3),
        // then the projects migration (dir #2) itself — 7 reverts in reverse order.
        // projects must then be absent.
        for _ in 0..7 {
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
        assert!(!got_inst.unread, "unread defaults to false (no unseen result)");
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
        // #8 → #4), then the managed_commands migration (dir #3) itself — 6 reverts in
        // reverse order. managed_commands must then be absent.
        for _ in 0..6 {
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

        // Revert down to the v4-absent state. Migrations stack v8, v7, v5 ON TOP of v4
        // (PRD-5 added v7/v8; PRD-4 added v5), so reaching the v4-absent state peels
        // v8, v7, v5, then v4 (migrations revert in reverse order). The instance row +
        // its v3 columns survive; the v4 columns are gone (a typed select of `unread`
        // would now fail to run).
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
        let still_there: i64 = command_instances::table.count().get_result(&mut conn).unwrap();
        assert_eq!(still_there, 1, "the pre-v4 instance row survives the v4 down");

        // Re-apply v4: the columns return with safe defaults for the EXISTING row
        // (unread=0 / NULL code / NULL ended_at) — an upgrade does not strand a row.
        run_migrations(&mut conn).expect("re-apply v4");
        let back = get_instance(&mut conn, &inst.id).unwrap().unwrap();
        assert!(!back.unread, "back-filled unread defaults to false (already seen)");
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
        assert_eq!(first.prev_scrollback, "", "first start retains no prior run");
        assert_eq!(first.prev_last_state, None);
        assert_eq!(first.prev_exit_code, None);
        assert_eq!(first.last_state, STATE_RUNNING, "the current run is reset to running");
        assert_eq!(first.scrollback, "", "the current run starts with empty scrollback");

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
        assert_eq!(after_relaunch.prev_exit_code, Some(7), "retained prior exit code");
        assert!(after_relaunch.prev_ended_at.is_some(), "retained prior ended_at");
        // The CURRENT run is clean — not polluted by run 1's bytes.
        assert_eq!(after_relaunch.scrollback, "", "current run resets to empty");
        assert_eq!(after_relaunch.last_state, STATE_RUNNING);
        assert_eq!(after_relaunch.last_exit_code, None, "current run has no code yet");

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

        // Revert down to the v5-absent state. PRD-5 stacked migrations v7
        // (agent_sessions) and v8 (project resume option) ON TOP of v5/prev_run, so
        // reaching the v5-absent state now peels v8, v7, then v5 (migrations revert in
        // reverse order). The instance row + its v3/v4 columns survive; the v5 columns
        // are gone (a typed select of `prev_scrollback` would now fail to run).
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
        let still_there: i64 = command_instances::table.count().get_result(&mut conn).unwrap();
        assert_eq!(still_there, 1, "the pre-v5 instance row survives the v5 down");

        // Re-apply v5: the columns return with safe defaults for the EXISTING row
        // ('' scrollback / NULL code / NULL ended_at / NULL state).
        run_migrations(&mut conn).expect("re-apply v5");
        let back = get_instance(&mut conn, &inst.id).unwrap().unwrap();
        assert_eq!(back.prev_scrollback, "", "back-filled prev_scrollback defaults to ''");
        assert_eq!(back.prev_exit_code, None, "back-filled prev_exit_code defaults to NULL");
        assert_eq!(back.prev_ended_at, None, "back-filled prev_ended_at defaults to NULL");
        assert_eq!(back.prev_last_state, None, "back-filled prev_last_state defaults to NULL");

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
            create_template(&mut conn, &p1.id, name, "cmd", None, CommandSource::default()).unwrap();
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
        assert!(
            instance_ids_for_project(&mut conn, "no-such-project")
                .unwrap()
                .is_empty()
        );
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
        assert_eq!(linked.source_kind.as_deref(), Some(SOURCE_KIND_PACKAGE_JSON));
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
        assert_eq!(got.transcript_path.as_deref(), Some("/home/u/.claude/x.jsonl"));
        assert_eq!(got.metadata_json, r#"{"source":"startup"}"#);
        assert!(got.started_at > 0);
        assert_eq!(got.ended_at, None, "fresh session has not ended");
        assert_eq!(got.last_seen_at, got.started_at, "fresh: last_seen == started");
    }

    /// `metadata_json` defaults to the empty object when the capture omits it.
    #[test]
    fn agent_session_metadata_defaults_to_empty_object() {
        let mut conn = open_in_memory();
        let t = create_terminal(&mut conn, "/work", None).unwrap();
        let s = record_session_start(&mut conn, &t.id, AGENT_KIND_CLAUDE_CODE, capture("e1"))
            .unwrap();
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

        assert!(active_session_for(&mut conn, &t1.id, AGENT_KIND_CLAUDE_CODE)
            .unwrap()
            .is_some());
        assert!(active_session_for(&mut conn, &t1.id, AGENT_KIND_CODEX)
            .unwrap()
            .is_some());
        assert!(active_session_for(&mut conn, &t2.id, AGENT_KIND_CLAUDE_CODE)
            .unwrap()
            .is_some());
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

        assert_eq!(second.id, first.id, "the SAME row is refreshed, not a new one");
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
        assert_eq!(revived.id, first.id, "the unknown row is revived, not duplicated");
        assert_eq!(revived.state, SESSION_STATE_ACTIVE, "revived back to active");
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
        assert!(ended.ended_at.is_some(), "ended_at is stamped on a clean end");
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
        let fresh =
            record_session_start(&mut conn, &t_fresh.id, AGENT_KIND_CLAUDE_CODE, capture("fresh"))
                .unwrap();
        // An ended row (must never be swept).
        let ended =
            record_session_start(&mut conn, &t_ended.id, AGENT_KIND_CLAUDE_CODE, capture("ended"))
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
        assert_eq!(after_ws.workspace_id, None, "workspace delete SET-NULLs anchor");

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

        // Step DOWN v8 → the resume_agent_sessions column is gone.
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
        record_session_start(&mut conn, &t_loose.id, AGENT_KIND_CLAUDE_CODE, capture("loose-id"))
            .unwrap();

        // 3) CLOSED terminal with an active session → NOT a candidate (status filter).
        let t_closed = create_terminal(&mut conn, "/closed", None).unwrap();
        record_session_start(&mut conn, &t_closed.id, AGENT_KIND_CLAUDE_CODE, capture("closed-id"))
            .unwrap();
        close_terminal(&mut conn, &t_closed.id).unwrap();

        // 4) Alive terminal whose session is ENDED → NOT a candidate (state filter).
        let t_ended = create_terminal(&mut conn, "/ended", None).unwrap();
        let ended =
            record_session_start(&mut conn, &t_ended.id, AGENT_KIND_CLAUDE_CODE, capture("ended-id"))
                .unwrap();
        mark_session_ended(&mut conn, &ended.id).unwrap();

        let candidates = resume_candidates_on_boot(&mut conn).unwrap();
        let by_session: std::collections::HashMap<&str, &ResumeCandidate> =
            candidates.iter().map(|c| (c.session_id.as_str(), c)).collect();

        // Exactly the two alive+active sessions are candidates.
        assert_eq!(candidates.len(), 2, "only the two alive+active sessions are candidates");

        let on = by_session.get(s_on.id.as_str()).expect("the resume-ON candidate is present");
        assert!(on.project_resume_on, "the attached resume-ON project flag is true");
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
        assert!(!loose.project_resume_on, "a loose terminal coalesces to resume OFF");
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
            record_session_start(&mut conn, &t3.id, AGENT_KIND_CLAUDE_CODE, capture("c-id")).unwrap();
        mark_session_ended(&mut conn, &ended.id).unwrap();

        let active = active_agent_sessions(&mut conn).unwrap();
        assert_eq!(active.len(), 2, "only the two active sessions are listed");
        assert!(
            active.iter().any(|s| s.terminal_id == t1.id && s.agent_kind == AGENT_KIND_CLAUDE_CODE),
            "terminal 1's active claude session is present"
        );
        assert!(active.iter().any(|s| s.terminal_id == t2.id), "terminal 2 is present");
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
            SessionCapture { workspace_id: Some(ws.id.clone()), ..capture("on-id") },
        )
        .unwrap();

        // A loose alive terminal with an active session (no project → flag false).
        let t_loose = create_terminal(&mut conn, "/loose", None).unwrap();
        record_session_start(&mut conn, &t_loose.id, AGENT_KIND_CODEX, capture("loose-id"))
            .unwrap();

        // A closed terminal + an ended session must NOT surface.
        let t_closed = create_terminal(&mut conn, "/c", None).unwrap();
        record_session_start(&mut conn, &t_closed.id, AGENT_KIND_CLAUDE_CODE, capture("c-id"))
            .unwrap();
        close_terminal(&mut conn, &t_closed.id).unwrap();
        let t_ended = create_terminal(&mut conn, "/e", None).unwrap();
        let ended =
            record_session_start(&mut conn, &t_ended.id, AGENT_KIND_CLAUDE_CODE, capture("e-id"))
                .unwrap();
        mark_session_ended(&mut conn, &ended.id).unwrap();

        let rows = close_warning_candidates(&mut conn).unwrap();
        assert_eq!(rows.len(), 2, "only the two alive+live sessions surface");

        let on = rows.iter().find(|w| w.external_session_id == "on-id").unwrap();
        assert!(on.project_resume_on, "resume-ON flag surfaces (bridge will filter it out)");
        assert_eq!(on.terminal_label.as_deref(), Some("build"), "terminal label comes through");
        assert_eq!(on.workspace_name.as_deref(), Some("root"), "workspace name comes through");
        assert_eq!(on.agent_kind, AGENT_KIND_CLAUDE_CODE);

        let loose = rows.iter().find(|w| w.external_session_id == "loose-id").unwrap();
        assert!(!loose.project_resume_on, "loose terminal coalesces to OFF → will warn");
        assert_eq!(loose.agent_kind, AGENT_KIND_CODEX);
        assert_eq!(loose.workspace_name, None);
    }
}
