//! CRASH RECOVERY of the sidecar runtime (Phase 4 — `01KWCJPFP9JSJMRDZGW2E0N905`).
//!
//! ## What this is
//!
//! The sidecar is a supervised CHILD of Electron main, not a daemon. When it CRASHES
//! (a panic, a SIGKILL, an OOM — anything that is NOT the ordered `shutdown` request),
//! the supervisor relaunches it EXACTLY ONCE (see `apps/desktop/src/main/sidecar.ts`).
//! On that relaunch the sidecar reopens SQLite and rebuilds the persisted state from the
//! runtime tables — but a row persisted as `running` is now SUSPECT: the process that
//! backed it died with the old sidecar, and its liveness CANNOT be re-verified (the fresh
//! runner has no handle to it). The PRD is explicit: such doubtful `running` states must be
//! marked **unknown / interrupted**, NOT blindly relaunched — the USER chooses what to
//! resume.
//!
//! ## Why a sidecar-owned state file (and not a new DB column)
//!
//! `command_instances.last_state` is CHECK-constrained to `idle|running|success|error`
//! (migration 00000000000003) and `nyx-core` is the FROZEN v1 port — byte-unchanged. So we
//! cannot persist a literal `interrupted` in that column nor add a migration. Instead the
//! sidecar owns a tiny JSON state file next to `nyx.db`
//! (`<data_dir>/sidecar-recovery.json`) that records:
//!   * `clean_shutdown` — `false` while serving, flipped to `true` only by the ordered
//!     [`crate::runtime::Runtime::shutdown`]. If the next boot reads it still `false`, the
//!     previous run CRASHED.
//!   * `interrupted` — the instance ids whose `running` could not be re-verified after that
//!     crash. The renderer reads this (via the `recovery_snapshot` db-command) to badge them
//!     `interrupted` and offer the user a resume choice. Reset once consumed.
//!   * `idempotency` — a journal of the idempotency keys of mutating operations already
//!     applied, so a mutation retried after a crash is de-duplicated rather than re-run.
//!
//! Agent SESSIONS, by contrast, DO have an `unknown` state in the (frozen) schema
//! (`agent_sessions.state IN ('active','ended','unknown','resume_failed')`), so a crash
//! reconcile flips every still-`active` session to `unknown` through the existing
//! [`nyx_core::db::sweep_stale_active_sessions`] — the exact "stale active ⇒ unknown,
//! still a resume candidate but liveness unconfirmed" semantics the resume policy already
//! understands.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nyx_core::db::{self, Db};
use serde::{Deserialize, Serialize};

/// The file name of the sidecar-owned recovery state, next to `nyx.db` under the data dir.
pub const RECOVERY_FILE_NAME: &str = "sidecar-recovery.json";

/// How many idempotency keys to retain in the journal (a bounded ring so the file cannot
/// grow without limit across a long session). Mutating ops are user-driven and low-rate, so
/// a few hundred is ample to dedupe a retry burst right after a crash.
const MAX_IDEMPOTENCY_KEYS: usize = 512;

/// The persisted sidecar-recovery state. Small, human-readable JSON owned ENTIRELY by the
/// sidecar (nyx-core never reads it), so it adds no schema surface to the frozen v1 DB.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecoveryState {
    /// `true` only after an ORDERED shutdown wrote it. A boot that reads `false` (or no
    /// file at all) infers the previous run did NOT shut down cleanly — i.e. it crashed.
    #[serde(default)]
    pub clean_shutdown: bool,
    /// The command-instance ids whose persisted `running` could not be re-verified after the
    /// last crash. The renderer badges these `interrupted` and offers a resume choice; the
    /// sidecar NEVER auto-relaunches them (no blind resume — the PRD non-goal).
    #[serde(default)]
    pub interrupted: Vec<String>,
    /// The idempotency keys of mutating operations already applied (a bounded FIFO journal),
    /// so a mutation retried after a crash is recognised and not re-applied.
    #[serde(default)]
    pub idempotency: Vec<String>,
}

impl RecoveryState {
    /// Read the recovery state from `<data_dir>/sidecar-recovery.json`. A missing file (the
    /// very first run, or a wiped data dir) is the default (`clean_shutdown=false` — treated
    /// as "unknown, assume not clean"); a corrupt file is logged-by-return and treated as the
    /// default rather than aborting boot.
    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path_in(data_dir);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => RecoveryState::default(),
        }
    }

    /// Persist the recovery state atomically (write to a temp sibling, then rename) so a
    /// crash mid-write never leaves a torn file. Best-effort: an I/O error is swallowed (the
    /// recovery state is an OPTIMIZATION on top of the DB, never the source of truth).
    pub fn save(&self, data_dir: &Path) {
        let path = Self::path_in(data_dir);
        let tmp = path.with_extension("json.tmp");
        let Ok(bytes) = serde_json::to_vec_pretty(self) else {
            return;
        };
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// The recovery-file path under a data dir.
    pub fn path_in(data_dir: &Path) -> PathBuf {
        data_dir.join(RECOVERY_FILE_NAME)
    }

    /// Whether a recovery file already EXISTS under the data dir. A missing file means the
    /// sidecar has never written one here yet (a first run / a wiped data dir) — which the
    /// reconcile treats as "nothing to recover", NOT a crash.
    pub fn exists(data_dir: &Path) -> bool {
        Self::path_in(data_dir).exists()
    }

    /// Has the idempotency key already been applied (a duplicate / replayed mutation)?
    pub fn is_duplicate(&self, key: &str) -> bool {
        self.idempotency.iter().any(|k| k == key)
    }

    /// Record an applied idempotency key, evicting the oldest if the bounded journal is full.
    pub fn record_key(&mut self, key: &str) {
        if self.is_duplicate(key) {
            return;
        }
        self.idempotency.push(key.to_string());
        if self.idempotency.len() > MAX_IDEMPOTENCY_KEYS {
            let overflow = self.idempotency.len() - MAX_IDEMPOTENCY_KEYS;
            self.idempotency.drain(0..overflow);
        }
    }
}

/// The outcome of a boot-time crash reconcile — what the previous run left doubtful.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileOutcome {
    /// `true` when the previous run did NOT shut down cleanly (a crash): the reconcile ran.
    pub was_crash: bool,
    /// Command-instance ids whose persisted `running` was unverifiable and is now flagged
    /// `interrupted` (NOT relaunched).
    pub interrupted_instances: Vec<String>,
    /// How many `active` agent sessions were flipped to `unknown` (liveness unconfirmed).
    pub sessions_marked_unknown: usize,
}

/// Reconcile the persisted runtime state after a (possible) crash, BEFORE the boot restore
/// runs. Reads the sidecar-owned recovery file:
///
///   * If the previous run shut down cleanly (`clean_shutdown == true`), this is a no-op
///     reconcile — the ordered shutdown already snapshotted/normalised everything — and we
///     simply re-arm the file to `clean_shutdown = false` for the run we are starting.
///   * If it did NOT (a crash), every command instance persisted as `last_state = 'running'`
///     is an ORPHAN whose liveness we cannot confirm: we record its id in `interrupted`
///     (the renderer surfaces it; the user chooses) and we DO NOT relaunch it. We also flip
///     every still-`active` agent session to `unknown` via
///     [`nyx_core::db::sweep_stale_active_sessions`] with a zero threshold (every active row
///     is stale relative to "right now"), the exact stale⇒unknown semantics the resume
///     policy already understands. We also clear the `was_running_on_shutdown` snapshots so
///     the normal boot restore — which keys off that snapshot — cannot blindly relaunch a
///     service the crash left running (the PRD non-goal).
///
/// Returns the [`ReconcileOutcome`] (logged by the caller). Always re-arms the recovery file
/// with `clean_shutdown = false` + the interrupted set for the run now starting.
pub fn reconcile_after_crash(db: &Db, data_dir: &Path) -> ReconcileOutcome {
    let had_file = RecoveryState::exists(data_dir);
    let prior = RecoveryState::load(data_dir);
    let mut outcome = ReconcileOutcome::default();

    if !had_file || prior.clean_shutdown {
        // FIRST/FRESH boot (no recovery file yet) OR a clean prior shutdown: nothing doubtful.
        // A missing file is "the sidecar has never run against this data dir" — not a crash.
        // A clean prior shutdown already snapshotted/normalised everything. Re-arm the file to
        // not-clean for the run we are starting, preserving the idempotency journal (so a key
        // applied just before a clean shutdown still dedupes after a restart).
        let next = RecoveryState {
            clean_shutdown: false,
            interrupted: Vec::new(),
            idempotency: prior.idempotency,
        };
        next.save(data_dir);
        return outcome;
    }

    // CRASH PATH. Gather every instance persisted as `running` — these are the doubtful
    // ones (the process that backed them died with the old sidecar; the fresh runner cannot
    // re-verify them). We mark them interrupted and refuse to blindly relaunch them.
    outcome.was_crash = true;
    let running = db
        .with_conn(running_instance_ids)
        .unwrap_or_else(|_| Vec::new());

    // Clear the shutdown snapshots BEFORE the boot restore reads them, so the snapshot-keyed
    // restore (restart_on_startup AND was_running_on_shutdown) cannot relaunch a crashed
    // service blindly. The user resumes interrupted instances explicitly instead.
    db.with_conn(|c| {
        for id in &running {
            let _ = db::set_was_running_on_shutdown(c, id, false);
        }
    });

    // Flip every still-`active` agent session to `unknown` (liveness unconfirmed). The sweep
    // cutoff is `now - threshold`; a threshold of -1 puts the cutoff one ms in the FUTURE so
    // the strict `last_seen_at < cutoff` filter matches EVERY active row — including one
    // recorded in the current millisecond (a zero threshold would miss those). The row stays
    // a resume CANDIDATE (active OR unknown) but now carries the doubt the resume policy reads.
    outcome.sessions_marked_unknown = db
        .with_conn(|c| db::sweep_stale_active_sessions(c, -1))
        .unwrap_or(0);

    outcome.interrupted_instances = running;

    // Persist the interrupted set + re-arm the clean-shutdown marker to false for THIS run.
    let next = RecoveryState {
        clean_shutdown: false,
        interrupted: outcome.interrupted_instances.clone(),
        idempotency: prior.idempotency,
    };
    next.save(data_dir);

    outcome
}

/// Read the ids of every command instance whose persisted `last_state` is `running` (a
/// connection-scoped helper so the reconcile checks out the SAME pool). Uses the existing
/// frozen `nyx_core::db::all_instances_for_restore` view (which carries `last_state`), so it
/// adds no nyx-core surface.
fn running_instance_ids(conn: &mut diesel::SqliteConnection) -> diesel::QueryResult<Vec<String>> {
    Ok(db::all_instances_for_restore(conn)?
        .into_iter()
        .filter(|row| row.last_state == db::STATE_RUNNING)
        .map(|row| row.instance_id)
        // Deterministic order for stable snapshots/tests.
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nyx-recovery-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn open_db(dir: &Path) -> Db {
        Db::open(&dir.join("nyx.db")).expect("open db")
    }

    /// Seed one project + workspace + template (which materialises one instance per
    /// workspace) and return the project's root-workspace instance id, set to `running`.
    fn seed_running_instance(db: &Db) -> String {
        db.with_conn(|c| {
            let (project, _root) = db::create_project(c, "proj", "/tmp/work", None).unwrap();
            let tmpl = db::create_template(
                c,
                &project.id,
                "dev",
                "sleep 999",
                None,
                db::CommandSource::default(),
            )
            .unwrap();
            let ids = db::instance_ids_for_template(c, &tmpl.id).unwrap();
            let id = ids[0].clone();
            db::set_run_state(c, &id, db::STATE_RUNNING, None).unwrap();
            id
        })
    }

    /// CRASH PATH: no recovery file (or `clean_shutdown=false`) ⇒ the reconcile flags every
    /// `running` instance interrupted, marks active sessions unknown, clears the snapshot,
    /// and persists the interrupted set. The done-criterion's "doubtful running ⇒
    /// unknown/interrupted".
    #[test]
    fn reconcile_flags_running_as_interrupted_after_crash() {
        let dir = temp_dir("crash");
        let db = open_db(&dir);
        let id = seed_running_instance(&db);

        // Simulate a crash: a PRIOR run armed the recovery file `clean_shutdown=false` at boot
        // and then crashed (never flipping it to true). An active agent session lingers, and the
        // running instance is still `running` + snapshotted was_running_on_shutdown (nothing
        // reset them). The existing not-clean file is the crash signal.
        RecoveryState {
            clean_shutdown: false,
            interrupted: Vec::new(),
            idempotency: vec![],
        }
        .save(&dir);
        let term = db
            .with_conn(|c| db::create_terminal(c, "/tmp/work", Some("t".into())))
            .unwrap();
        db.with_conn(|c| {
            db::set_was_running_on_shutdown(c, &id, true).unwrap();
            db::record_session_start(
                c,
                &term.id,
                db::AGENT_KIND_CLAUDE_CODE,
                db::SessionCapture {
                    workspace_id: None,
                    external_session_id: "ext-1".into(),
                    cwd: "/tmp/work".into(),
                    transcript_path: None,
                    metadata_json: None,
                },
            )
            .unwrap();
        });
        assert_eq!(
            db.with_conn(db::active_agent_sessions).unwrap().len(),
            1,
            "an active session exists pre-reconcile"
        );

        let outcome = reconcile_after_crash(&db, &dir);

        assert!(outcome.was_crash, "no clean marker -> treated as a crash");
        assert_eq!(
            outcome.interrupted_instances,
            vec![id.clone()],
            "the running instance is flagged interrupted"
        );
        assert_eq!(
            outcome.sessions_marked_unknown, 1,
            "the active session was flipped to unknown"
        );
        // The active session is now `unknown` (liveness unconfirmed), no longer `active`.
        assert_eq!(
            db.with_conn(db::active_agent_sessions).unwrap().len(),
            0,
            "no session remains `active` after the reconcile"
        );
        // The snapshot was cleared so the boot restore cannot blindly relaunch it.
        let row = db.with_conn(|c| db::get_instance(c, &id)).unwrap().unwrap();
        assert!(
            !row.was_running_on_shutdown,
            "the shutdown snapshot was cleared (no blind relaunch)"
        );
        // The interrupted set is persisted for the renderer to read.
        let persisted = RecoveryState::load(&dir);
        assert!(
            !persisted.clean_shutdown,
            "re-armed to not-clean for this run"
        );
        assert_eq!(persisted.interrupted, vec![id], "interrupted set persisted");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// FRESH-BOOT PATH: NO recovery file yet (first run against this data dir) ⇒ no crash, no
    /// instance flagged, sessions untouched; the file is armed to not-clean for the new run.
    #[test]
    fn reconcile_is_noop_on_first_boot_without_a_recovery_file() {
        let dir = temp_dir("fresh");
        let db = open_db(&dir);
        let _id = seed_running_instance(&db);
        assert!(
            !RecoveryState::exists(&dir),
            "no recovery file exists before the very first reconcile"
        );

        let outcome = reconcile_after_crash(&db, &dir);
        assert!(
            !outcome.was_crash,
            "a first boot with no recovery file is NOT a crash"
        );
        assert!(outcome.interrupted_instances.is_empty());
        assert_eq!(outcome.sessions_marked_unknown, 0);

        // The file is now armed for this run.
        assert!(
            RecoveryState::exists(&dir),
            "the reconcile armed a recovery file for this run"
        );
        let persisted = RecoveryState::load(&dir);
        assert!(!persisted.clean_shutdown, "armed not-clean for this run");
        assert!(persisted.interrupted.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// CLEAN PATH: a prior `clean_shutdown=true` ⇒ no instance is flagged, sessions are
    /// untouched, and the file is re-armed to not-clean for the new run.
    #[test]
    fn reconcile_is_noop_after_clean_shutdown() {
        let dir = temp_dir("clean");
        let db = open_db(&dir);
        let _id = seed_running_instance(&db);
        // The ordered shutdown wrote clean_shutdown=true.
        RecoveryState {
            clean_shutdown: true,
            interrupted: Vec::new(),
            idempotency: vec!["k-1".into()],
        }
        .save(&dir);

        let outcome = reconcile_after_crash(&db, &dir);
        assert!(!outcome.was_crash, "a clean prior shutdown is not a crash");
        assert!(outcome.interrupted_instances.is_empty());
        assert_eq!(outcome.sessions_marked_unknown, 0);

        let persisted = RecoveryState::load(&dir);
        assert!(!persisted.clean_shutdown, "re-armed to not-clean");
        assert!(persisted.interrupted.is_empty());
        assert_eq!(
            persisted.idempotency,
            vec!["k-1".to_string()],
            "the idempotency journal survives a clean shutdown"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The idempotency journal dedupes a replayed key and stays bounded.
    #[test]
    fn idempotency_journal_dedupes_and_bounds() {
        let mut s = RecoveryState::default();
        assert!(!s.is_duplicate("a"));
        s.record_key("a");
        assert!(s.is_duplicate("a"), "key recorded -> duplicate");
        s.record_key("a");
        assert_eq!(
            s.idempotency.len(),
            1,
            "re-recording the same key is a no-op"
        );

        for i in 0..(MAX_IDEMPOTENCY_KEYS + 50) {
            s.record_key(&format!("key-{i}"));
        }
        assert!(
            s.idempotency.len() <= MAX_IDEMPOTENCY_KEYS,
            "journal stays bounded at {MAX_IDEMPOTENCY_KEYS}"
        );
        // The OLDEST keys were evicted; the most recent survive.
        assert!(
            s.is_duplicate(&format!("key-{}", MAX_IDEMPOTENCY_KEYS + 49)),
            "the newest key is retained"
        );
        assert!(!s.is_duplicate("a"), "the oldest key 'a' was evicted");
    }

    /// A corrupt recovery file is treated as the default (not-clean) rather than aborting.
    #[test]
    fn corrupt_recovery_file_loads_as_default() {
        let dir = temp_dir("corrupt");
        std::fs::write(RecoveryState::path_in(&dir), b"{not json").unwrap();
        let s = RecoveryState::load(&dir);
        assert!(!s.clean_shutdown);
        assert!(s.interrupted.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
