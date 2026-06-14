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
use crate::schema::{projects, terminals, workspaces};

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
}

impl NewTerminal {
    /// A fresh `alive` terminal at `cwd`, with an optional label, placed at the
    /// given sidebar order. Generates a fresh, time-ordered UUIDv7 id and stamps
    /// `created_at`/`updated_at` with the current epoch-ms.
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

/// Apply all pending embedded migrations. Idempotent.
pub fn run_migrations(conn: &mut SqliteConnection) -> anyhow::Result<()> {
    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow::anyhow!("failed to run migrations: {e}"))?;
    Ok(())
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
/// number of rows updated (0 if the id is unknown).
pub fn close_terminal(conn: &mut SqliteConnection, id: &str) -> QueryResult<usize> {
    let now = now_millis();
    diesel::update(terminals::table.find(id))
        .set((
            terminals::status.eq(STATUS_CLOSED),
            terminals::closed_at.eq(now),
            terminals::updated_at.eq(now),
        ))
        .execute(conn)
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
        .set((terminals::label.eq(label), terminals::updated_at.eq(now_millis())))
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

        let workspace = diesel::insert_into(workspaces::table)
            .values(NewWorkspace {
                id: Uuid::now_v7().to_string(),
                project_id: project_id.clone(),
                name: workspace_name,
                path: normalized,
                branch: None,
                is_root: true,
                created_at: now,
                updated_at: now,
            })
            .returning(Workspace::as_returning())
            .get_result(conn)?;

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
pub fn create_workspace(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    path: &str,
) -> QueryResult<Workspace> {
    let now = now_millis();
    let normalized = pathnorm::normalize(path);
    diesel::insert_into(workspaces::table)
        .values(NewWorkspace {
            id: Uuid::now_v7().to_string(),
            project_id: project_id.to_string(),
            name: name.to_string(),
            path: normalized,
            branch: None,
            is_root: false,
            created_at: now,
            updated_at: now,
        })
        .returning(Workspace::as_returning())
        .get_result(conn)
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
        assert_eq!(rename(&mut conn, "no-such-id", Some("x".into())).unwrap(), 0);
        assert_eq!(set_active(&mut conn, "no-such-id").unwrap(), 0);
        assert_eq!(persist_scrollback(&mut conn, "no-such-id", "data").unwrap(), 0);
        // reorder over absent ids is a silent no-op (does not error).
        reorder(&mut conn, &["no-such-id".to_string(), "nope".to_string()]).expect("reorder absent ids");
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
        let (project, root) =
            create_project(&mut conn, "p", p("/p", "C:\\p"), None).unwrap();
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
        assert!(got_feat.collapsed, "the workspace's collapsed state persists");
        assert!(!got_root.collapsed, "a sibling workspace is left open");
        assert_eq!(
            got_feat.path, feat.path,
            "persisting collapse never touches the path"
        );

        // Unknown ids are no-ops (0 rows), not errors.
        assert_eq!(set_project_collapsed(&mut conn, "no-such-id", true).unwrap(), 0);
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
    #[test]
    fn migration_v2_down_then_up_recreates_working_schema() {
        let mut conn = open_in_memory();

        // Revert v2 only (the last migration). projects must then be absent.
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert v2 cleanly");
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
}
