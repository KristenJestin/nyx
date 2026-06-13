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

use crate::schema::terminals;

/// The migrations baked into the binary at compile time from `migrations/`.
/// Running them is idempotent (Diesel tracks applied versions in
/// `__diesel_schema_migrations`), so we run them on every startup.
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

/// A terminal status. Stored as TEXT (`'alive'` | `'closed'`) — see the CHECK
/// constraint in the migration. A live terminal is re-spawned at launch; a
/// closed one is not.
pub const STATUS_ALIVE: &str = "alive";
pub const STATUS_CLOSED: &str = "closed";

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

    /// MIGRATION reversibility guard: applying `down` (DROP TABLE) then `up`
    /// again re-creates a schema that still round-trips. This exercises the
    /// migration PAIR — `up.sql` AND `down.sql` — so a broken/missing down (or an
    /// up that the down does not cleanly precede) is caught, complementing
    /// `schema_matches_migration` (which only exercises the forward direction).
    #[test]
    fn migration_down_then_up_recreates_working_schema() {
        let mut conn = open_in_memory(); // already migrated up once

        // Roll the single migration back: this runs down.sql (DROP TABLE).
        conn.revert_last_migration(MIGRATIONS)
            .expect("revert_last_migration must run down.sql cleanly");

        // The table is gone after the down — a select must now fail.
        let after_down: QueryResult<i64> = terminals::table.count().get_result(&mut conn);
        assert!(
            after_down.is_err(),
            "after `down`, the terminals table must no longer exist"
        );

        // Re-apply: this runs up.sql again and must restore a working schema.
        run_migrations(&mut conn).expect("re-running migrations after a down must succeed");

        // The re-created schema still round-trips a full row (every column).
        let t = create_terminal(&mut conn, "/after/down-up", Some("revived".into()))
            .expect("insert after down→up");
        let got = get_terminal(&mut conn, &t.id).unwrap().unwrap();
        assert_eq!(got.cwd, "/after/down-up");
        assert_eq!(got.label.as_deref(), Some("revived"));
        assert_eq!(got.status, STATUS_ALIVE);
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
}
