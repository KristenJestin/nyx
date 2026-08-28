//! SQLite boot for the sidecar (HHE — `01KWCJPFP72FP5CDVWSB8DTHHE`).
//!
//! ## What this is
//!
//! The persistence foundation of the sidecar runtime. SQLite is statically linked
//! into the sidecar (PDR-0023): `nyx-core` depends on `libsqlite3-sys` with the
//! `bundled` feature, so no system SQLite is needed and the sidecar binary carries
//! its own engine. The query layer is **Diesel** (chosen in `nyx-core`'s
//! `Cargo.toml`): synchronous, schema-checked, with the embedded `migrations/` and
//! the r2d2 connection pool (WAL + `foreign_keys` + `busy_timeout` per connection).
//!
//! ### Why Diesel over sqlx (the impl choice the task asks us to justify)
//!
//! `nyx-core` already ships its ENTIRE persistence layer on Diesel: the hand-written
//! `schema.rs`, the embedded `diesel_migrations`, the typed CRUD over the runtime
//! tables (terminals, scrollback, managed commands, command instances/runs, agent
//! sessions) and the r2d2 pool with the WAL/foreign-keys customizer. The v1 runtime
//! we are porting (the managed-command runner, the MCP tools, the boot restore) is
//! written against that Diesel surface. Re-implementing it on sqlx would mean
//! rewriting the whole ported runtime AND introducing a Tokio runtime (sqlx is
//! async-first) — both explicitly out of scope ("port it, don't rewrite it"; "NO
//! Tokio" per the frozen pool decision). So the sidecar reuses the existing Diesel +
//! r2d2 stack: synchronous, no async runtime, one shared pool for every reader.
//!
//! ## What boots here
//!
//! [`open_database`] opens (creating if absent) `<data_dir>/nyx.db`, applies the
//! pending migrations ONCE on a pooled connection (the customizer has already set the
//! WAL, foreign-keys, and busy-timeout pragmas on it), and returns the pool-backed
//! [`Db`]. After this the runtime tables exist and are ready for the ported runtime to
//! read/write.

use std::path::{Path, PathBuf};

use nyx_core::db::Db;

/// The database file name under the data dir — the SAME `nyx.db` name + location
/// convention the v1 shells use (`NyxCore::open(data_dir)` joined `nyx.db`).
pub const DB_FILE_NAME: &str = "nyx.db";

/// Resolve the writable per-user data directory the sidecar persists into.
///
/// Precedence (mirrors the v1 `AppPaths` frontier / the e2e harness pin):
///   1. `NYX_DATA_DIR` — the explicit override (the e2e suite + a direct test spawn
///      pin it). Wins unconditionally so tests are deterministic.
///   2. the OS-conventional per-user data dir under `$XDG_DATA_HOME`/`$HOME` (Linux/
///      macOS) or `%APPDATA%` (Windows), in a `nyx-v2` subdir.
///
/// SAFETY (review #37): the v2-DISTINCT `nyx-v2` subdir (NOT v1's `nyx`) means that even
/// when `NYX_DATA_DIR` is unset and the Electron main did NOT pass an explicit dir, the
/// sidecar's own default never lands on v1's live data. In the packaged app the main
/// process ALWAYS passes the pinned `<appData>/nyx-v2` userData via `NYX_DATA_DIR`, so this
/// default is a defense-in-depth backstop for a bare sidecar spawn.
///
/// The directory is created if missing (matching the v1 adapter's `create_dir_all`
/// before `Db::open`), so the DB file's parent always exists by the time we open it.
pub fn resolve_data_dir() -> anyhow::Result<PathBuf> {
    let dir = if let Some(override_dir) = std::env::var_os("NYX_DATA_DIR") {
        if !override_dir.is_empty() {
            PathBuf::from(override_dir)
        } else {
            default_data_dir()?
        }
    } else {
        default_data_dir()?
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("failed to create data dir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The v2-DISTINCT per-user data dir subdir name (NOT v1's `nyx`). The whole safe-by-default
/// design hinges on this literal being different from v1's, so an unset `NYX_DATA_DIR` can
/// never resolve onto v1's live data (review #37).
pub const DEFAULT_DATA_DIR_NAME: &str = "nyx-v2";

/// The OS-conventional per-user data dir (a `nyx-v2` subdir), used when `NYX_DATA_DIR`
/// is not set. Reads the OS env (`APPDATA` / `XDG_DATA_HOME` / `HOME`) and delegates the
/// PURE path construction to [`default_data_dir_from`] (which the tests drive without
/// mutating process env).
fn default_data_dir() -> anyhow::Result<PathBuf> {
    #[cfg(windows)]
    {
        return default_data_dir_from(std::env::var_os("APPDATA"), None, None);
    }
    #[cfg(not(windows))]
    {
        default_data_dir_from(
            None,
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("HOME"),
        )
    }
}

/// PURE derivation of the default data dir from the OS env VALUES (no process-env reads —
/// testable, race-free). Precedence: `%APPDATA%/nyx-v2` on Windows; else `$XDG_DATA_HOME/
/// nyx-v2` then `$HOME/.local/share/nyx-v2`. The subdir is always [`DEFAULT_DATA_DIR_NAME`]
/// (`nyx-v2`), v2-DISTINCT from v1 (review #37). An empty `XDG_DATA_HOME` is ignored (falls
/// through to `HOME`), matching the conventional XDG handling.
fn default_data_dir_from(
    appdata: Option<std::ffi::OsString>,
    xdg_data_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> anyhow::Result<PathBuf> {
    if let Some(appdata) = appdata {
        return Ok(PathBuf::from(appdata).join(DEFAULT_DATA_DIR_NAME));
    }
    if let Some(xdg) = xdg_data_home {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join(DEFAULT_DATA_DIR_NAME));
        }
    }
    if let Some(home) = home {
        return Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join(DEFAULT_DATA_DIR_NAME));
    }
    Err(anyhow::anyhow!(
        "could not resolve a data dir (set NYX_DATA_DIR)"
    ))
}

/// Open + migrate the SQLite database under `data_dir`, returning the pool-backed
/// [`Db`]. The DB file is `<data_dir>/nyx.db`; its parent must already exist (the
/// caller, via [`resolve_data_dir`], created it).
///
/// [`Db::open`] runs the embedded migrations ONCE here (idempotent — Diesel tracks
/// applied versions in `__diesel_schema_migrations`) before the pool serves any
/// query, so the runtime tables are guaranteed present at boot. A migration failure
/// propagates so the sidecar refuses to start rather than serving a broken schema.
pub fn open_database(data_dir: &Path) -> anyhow::Result<Db> {
    let db_path = data_dir.join(DB_FILE_NAME);
    Db::open(&db_path)
        .map_err(|e| anyhow::anyhow!("failed to open database at {}: {e}", db_path.display()))
}

/// THE SCHEMA VERDICT THE BOOT PRINTS — the seam `main.rs` consults instead of asserting.
///
/// The boot used to announce "SQLite migrated, runtime tables ready" the moment [`Runtime::boot`]
/// returned `Ok`, without asking the schema anything. That sentence was measurably false in the
/// one case it mattered (human review 01M0CY3F075ACP4TFBK1KJ49X8): a sibling worktree's ledger row
/// made Diesel skip one of OUR migrations, nothing was pending, boot claimed success, and the
/// first command read failed with `no such table: managed_command_groups`.
///
/// So the sentence is now derived from the ledger — see [`nyx_core::db::SchemaHealth::boot_report`]
/// for what each verdict means. This function is the runtime-side entry point, so the decision is
/// testable without spawning the binary.
///
/// [`Runtime::boot`]: crate::runtime::Runtime::boot
pub fn schema_boot_report(db: &Db) -> nyx_core::db::SchemaBootReport {
    db.with_conn(|conn| {
        let health = nyx_core::db::schema_health(conn);
        let foreign = nyx_core::db::foreign_ledger_versions(conn);
        health.boot_report(&foreign)
    })
}

/// The v2 OWNERSHIP MARKER file written in the data dir. Its presence means "this dir is a
/// nyx-v2 data dir, safe to open/migrate". Its ABSENCE next to a `nyx.db` means the dir
/// belongs to another nyx (a v1 install) — we MUST NOT touch it (review #38).
pub const OWNER_MARKER_FILE: &str = ".nyx-v2-owner";

/// The bytes written into the ownership marker (human-readable; the existence is what matters,
/// the content is informative for anyone who finds the file).
const OWNER_MARKER_CONTENT: &str = "nyx-v2 data dir — do not point a different nyx version here\n";

/// HARD GUARD (review #38 — `01KWD3S503GFX649YZ4YH93BVH`): refuse to open/migrate a data dir
/// that belongs to ANOTHER version of nyx (a live v1 install). The user's explicit fear:
/// "on l'oublie et un jour on écrase sans faire exprès". This is the loud, fail-fast backstop.
///
/// Decision table (run BEFORE [`open_database`], i.e. before any SQLite open/migrate):
///   - marker present: OURS. Proceed (idempotent — re-asserts the marker).
///   - no marker but `nyx.db` present: FOREIGN (a v1 DB, or any non-v2 nyx). REFUSE with a clear error; NO migration runs, the DB is untouched.
///   - no marker and no `nyx.db` (fresh): a brand-new/empty dir. Write the marker and proceed.
///
/// On the fresh path the marker is written so a SUBSEQUENT boot recognizes the dir as ours.
/// A marker-write failure is fatal (we refuse rather than silently run unmarked).
pub fn guard_data_dir_ownership(data_dir: &Path) -> anyhow::Result<()> {
    let marker = data_dir.join(OWNER_MARKER_FILE);
    let db = data_dir.join(DB_FILE_NAME);

    if marker.exists() {
        // Ours already — nothing to do (the marker is idempotent across boots).
        return Ok(());
    }

    if db.exists() {
        // A `nyx.db` with NO v2 marker = a foreign/v1 data dir. Refuse, loudly, in French (the
        // user's language) — NO migration, the foreign DB is never opened.
        anyhow::bail!(
            "refus: ce data dir appartient a une autre version de nyx (pas de marqueur {marker}) \
             — pointe NYX_DATA_DIR ailleurs. data_dir={dir}, db trouvee={db}",
            marker = OWNER_MARKER_FILE,
            dir = data_dir.display(),
            db = db.display(),
        );
    }

    // Fresh/empty dir: claim it for v2 by writing the marker, then proceed.
    std::fs::write(&marker, OWNER_MARKER_CONTENT).map_err(|e| {
        anyhow::anyhow!(
            "failed to write the nyx-v2 ownership marker {}: {e}",
            marker.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp dir per test (no shared-state flakiness across the parallel suite).
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nyx-sidecar-db-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Review #37 (safe-by-default data dir): the OS-conventional default data dir is
    /// `nyx-v2`-namespaced — DISTINCT from v1's `nyx`/`@nyx/electron` — on EVERY platform.
    /// Booting WITHOUT `NYX_DATA_DIR` (when even the Electron main didn't pin one) must never
    /// resolve onto v1's live data. Driven through the PURE `default_data_dir_from` so it is
    /// race-free (no process-env mutation) and platform-independent.
    #[test]
    fn default_data_dir_is_nyx_v2_namespaced_distinct_from_v1() {
        use std::ffi::OsString;
        use std::path::Path;

        // The literal is the v2-distinct name, never v1's `nyx`.
        assert_eq!(DEFAULT_DATA_DIR_NAME, "nyx-v2");
        assert_ne!(DEFAULT_DATA_DIR_NAME, "nyx");

        // Windows: %APPDATA%/nyx-v2 (NOT %APPDATA%/nyx).
        let win = default_data_dir_from(Some(OsString::from("/X/AppData")), None, None).unwrap();
        assert_eq!(win, Path::new("/X/AppData").join("nyx-v2"));
        assert!(win.ends_with("nyx-v2"));
        assert_ne!(win, Path::new("/X/AppData").join("nyx"));

        // Linux/macOS with XDG_DATA_HOME: $XDG_DATA_HOME/nyx-v2.
        let xdg = default_data_dir_from(None, Some(OsString::from("/home/u/.local/share")), None)
            .unwrap();
        assert_eq!(xdg, Path::new("/home/u/.local/share").join("nyx-v2"));
        assert!(xdg.ends_with("nyx-v2"));

        // Empty XDG_DATA_HOME falls through to $HOME/.local/share/nyx-v2.
        let home = default_data_dir_from(
            None,
            Some(OsString::from("")),
            Some(OsString::from("/home/u")),
        )
        .unwrap();
        assert_eq!(
            home,
            Path::new("/home/u")
                .join(".local")
                .join("share")
                .join("nyx-v2")
        );

        // None of the resolutions ever land on the v1 `@nyx/electron` userData dir.
        for p in [&win, &xdg, &home] {
            assert!(
                !p.to_string_lossy().contains("@nyx/electron"),
                "default data dir {} must never be the v1 @nyx/electron dir",
                p.display()
            );
        }
    }

    /// Review #38 (hard guard): a data dir with a v1-LIKE `nyx.db` and NO `nyx-v2` ownership
    /// marker is REFUSED — the guard errors with a clear message, the marker is NOT written,
    /// and (critically) NO migration runs (we never even reach `open_database`). This is the
    /// fail-fast backstop against accidentally clobbering a live v1 DB. The `nyx.db` is a fake
    /// "v1-like" file SEEDED in a /tmp dir — never the real `~/.config/@nyx/electron`.
    #[test]
    fn guard_refuses_a_v1_like_dir_without_marker_and_runs_no_migration() {
        let dir = temp_dir("guard-foreign");
        // Seed a fake "v1-like" DB: a `nyx.db` file with arbitrary bytes (NOT a v2 dir — no
        // marker). The guard must refuse BEFORE any SQLite open, so the bytes don't matter.
        std::fs::write(dir.join(DB_FILE_NAME), b"fake-v1-sqlite-bytes").unwrap();

        let err = guard_data_dir_ownership(&dir).expect_err("a v1-like dir must be refused");
        let msg = err.to_string();
        assert!(msg.contains("refus"), "clear refusal message, got: {msg}");
        assert!(
            msg.contains(OWNER_MARKER_FILE),
            "message names the missing marker, got: {msg}"
        );
        // NO marker was written (we did not claim the foreign dir).
        assert!(
            !dir.join(OWNER_MARKER_FILE).exists(),
            "the ownership marker must NOT be written for a refused foreign dir"
        );
        // The seeded `nyx.db` is BYTE-UNCHANGED (no migration / open ran against it).
        assert_eq!(
            std::fs::read(dir.join(DB_FILE_NAME)).unwrap(),
            b"fake-v1-sqlite-bytes",
            "the foreign nyx.db was never opened/migrated (bytes intact)"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Review #38: a FRESH/empty dir (no `nyx.db`, no marker) is CLAIMED for v2 — the guard
    /// writes the ownership marker and proceeds. A SUBSEQUENT boot (marker present) is a no-op
    /// pass (idempotent). After claiming, opening the DB succeeds + migrates normally.
    #[test]
    fn guard_claims_a_fresh_dir_then_is_idempotent_and_db_opens() {
        let dir = temp_dir("guard-fresh");

        // Fresh dir: the guard writes the marker and proceeds.
        guard_data_dir_ownership(&dir).expect("a fresh dir is claimed");
        assert!(
            dir.join(OWNER_MARKER_FILE).exists(),
            "the ownership marker was written for a fresh dir"
        );

        // Idempotent: a second call (marker now present) is a clean pass, no error.
        guard_data_dir_ownership(&dir).expect("re-guarding our own dir is a no-op pass");

        // After claiming, the DB opens + migrates normally (the runtime tables exist).
        let db = open_database(&dir).expect("open + migrate the claimed dir");
        let health = db.with_conn(nyx_core::db::schema_health);
        assert!(health.up_to_date, "schema migrated on the claimed dir");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// THE BOOT READS THE LEDGER INSTEAD OF ASSERTING (human review 01M0CY4Q1A9CS1BRK10F8S3BC5,
    /// criterion 4). A clean boot is entitled to say "migrated"; a boot whose ledger carries a
    /// version this build does not embed is NOT — even though nothing is pending, which is exactly
    /// what made the original failure silent.
    #[test]
    fn schema_boot_report_is_read_off_the_ledger_not_assumed() {
        use diesel::RunQueryDsl;
        let dir = temp_dir("boot-report");
        guard_data_dir_ownership(&dir).expect("claim the fresh dir");
        let db = open_database(&dir).expect("open + migrate");

        let clean = schema_boot_report(&db);
        assert!(clean.healthy, "a freshly migrated DB is clean: {}", clean.line);
        assert!(!clean.fatal);

        // A sibling worktree's row, exactly as Diesel writes it. Nothing becomes pending — and yet
        // the boot must stop claiming an unqualified success and NAME the version. `00000000000052`
        // is the NEXT sequence number and can never be embedded here: `nyx-core`'s
        // `every_migration_added_after_the_frozen_lineage_is_dated_not_numbered` refuses it. (It was
        // `00000000000050` until that number became a REAL migration of this build —
        // `annexes_url_unique` — at which point the insert collided with its own ledger row.)
        db.with_conn(|conn| {
            diesel::sql_query(
                "INSERT INTO __diesel_schema_migrations (version, run_on) \
                 VALUES ('00000000000052', '2026-08-16 10:49:30')",
            )
            .execute(conn)
            .expect("write a foreign ledger row");
        });
        let flagged = schema_boot_report(&db);
        assert!(!flagged.healthy, "a foreign ledger row is not a clean boot");
        assert!(!flagged.fatal, "…but it does not stop the sidecar");
        assert!(flagged.line.contains("00000000000052"), "got: {}", flagged.line);

        drop(db);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// …AND `main.rs` ACTS ON THAT VERDICT. The boot line cannot be exercised from a unit test
    /// (it is `main`), so what is pinned here is the WIRING: that the entry point consults
    /// [`schema_boot_report`] and refuses to serve on a `fatal` one, instead of printing the old
    /// unconditional "SQLite migrated" sentence.
    #[test]
    fn main_consults_the_schema_verdict_and_refuses_to_serve_a_fatal_one() {
        let main_rs = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("main.rs"),
        )
        .expect("read the sidecar entry point");
        assert!(
            main_rs.contains("runtime::db::schema_boot_report(runtime.db())"),
            "the boot must ASK the schema"
        );
        assert!(
            main_rs.contains("if schema.fatal"),
            "a schema behind the binary must be refused, not served"
        );
        assert!(
            !main_rs.contains("runtime booted (SQLite migrated, runtime tables ready)"),
            "the unconditional success sentence must be gone — it was false in the one case it \
             mattered"
        );
    }

    /// Review #38: a dir that has BOTH our marker AND a `nyx.db` (the normal steady state after
    /// the first boot) is recognized as OURS and proceeds — the marker presence wins over the
    /// db presence (we own the db).
    #[test]
    fn guard_allows_our_own_dir_with_marker_and_db() {
        let dir = temp_dir("guard-ours");
        std::fs::write(dir.join(OWNER_MARKER_FILE), b"nyx-v2").unwrap();
        std::fs::write(dir.join(DB_FILE_NAME), b"our-db").unwrap();
        guard_data_dir_ownership(&dir).expect("our own marked dir is allowed");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// HHE done-criterion 1: migrations apply at boot. Opening a brand-new DB creates
    /// the file and runs the embedded migrations — proven by the runtime tables being
    /// immediately queryable below.
    #[test]
    fn open_database_applies_migrations_at_boot() {
        let dir = temp_dir("migrate");
        let db = open_database(&dir).expect("open + migrate a fresh db");
        // The DB file was created under the data dir.
        assert!(
            dir.join(DB_FILE_NAME).exists(),
            "nyx.db created under data dir"
        );
        // Migrations are up to date right after open (no pending migration).
        let health = db.with_conn(nyx_core::db::schema_health);
        assert!(health.up_to_date, "schema is migrated at boot");
        assert_eq!(
            health.pending_count,
            Some(0),
            "no pending migration after boot"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// HHE done-criterion 2: the v1 RUNTIME tables are created and USABLE by the
    /// runtime. We exercise the full runtime-table chain on the freshly migrated DB —
    /// terminals + scrollback, projects/workspaces, managed commands + instances, and
    /// agent sessions — proving each ported table is created AND read/written through
    /// the same `nyx-core` CRUD the runtime port uses.
    #[test]
    fn runtime_tables_are_created_and_used() {
        use nyx_core::db;
        let dir = temp_dir("tables");
        let database = open_database(&dir).expect("open db");

        database.with_conn(|c| {
            // terminals + scrollback (runtime table #1).
            let term = db::create_terminal(c, "/tmp/work", Some("t1".into())).unwrap();
            db::persist_scrollback(c, &term.id, "hello-scrollback").unwrap();
            let rows = db::list_terminals(c).unwrap();
            assert_eq!(rows.len(), 1, "terminals table read back the created row");
            assert_eq!(
                rows[0].scrollback, "hello-scrollback",
                "scrollback persisted"
            );

            // projects + workspaces (the FK parents the command tables reference).
            // `create_project` also creates the project's ROOT workspace, so the
            // project has two workspaces (root + ws) once we add ours below.
            let (project, _root) = db::create_project(c, "proj", "/tmp/work", None).unwrap();
            let ws = db::create_workspace(c, &project.id, "ws", "/tmp/work/ws").unwrap();

            // managed commands + command instances/runs (runtime tables #2/#3).
            let tmpl = db::create_template(
                c,
                &project.id,
                "dev",
                "echo hi",
                None,
                db::CommandSource::default(),
            )
            .unwrap();
            // `create_template` already materializes one instance per existing
            // workspace (the run row), so the instance/run rows exist now — one per
            // workspace (root + ws = 2). Re-running the workspace materializer is
            // idempotent (the unique pair is skipped).
            let instances = db::instance_ids_for_template(c, &tmpl.id).unwrap();
            assert_eq!(
                instances.len(),
                2,
                "a command instance/run row was created per workspace and linked to its template"
            );
            let reinsert = db::materialize_instances_for_workspace(c, &ws.id).unwrap();
            assert_eq!(
                reinsert, 0,
                "materialize is idempotent (pair already present)"
            );
            // Drive a run-state transition through the table (the runtime's persistence).
            db::set_run_state(c, &instances[0], db::STATE_RUNNING, None).unwrap();
            let inst = db::get_instance(c, &instances[0]).unwrap().unwrap();
            assert_eq!(inst.last_state, db::STATE_RUNNING, "run-state row updated");

            // agent sessions (runtime table #4).
            let session = db::record_session_start(
                c,
                &term.id,
                db::AGENT_KIND_CLAUDE_CODE,
                db::SessionCapture {
                    workspace_id: Some(ws.id.clone()),
                    external_session_id: "ext-sess-1".into(),
                    cwd: "/tmp/work".into(),
                    transcript_path: None,
                    metadata_json: None,
                },
            )
            .unwrap();
            let active = db::active_agent_sessions(c).unwrap();
            assert_eq!(
                active.len(),
                1,
                "agent_sessions table read back the session"
            );
            assert_eq!(
                active[0].terminal_id, term.id,
                "the active session is bound to the started terminal"
            );
            // The session row is fetchable by its id (the row we just inserted).
            let fetched = db::get_session(c, &session.id).unwrap();
            assert!(fetched.is_some(), "agent session row is retrievable by id");
        });

        std::fs::remove_dir_all(&dir).ok();
    }
}
