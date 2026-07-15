//! CRASH RECOVERY of the sidecar runtime (Phase 4 — `01KWCJPFP9JSJMRDZGW2E0N905`).
//!
//! ## What this is
//!
//! The sidecar is a supervised CHILD of Electron main, not a daemon. When it CRASHES
//! (a panic, a SIGKILL, an OOM — anything that is NOT the ordered `shutdown` request),
//! the supervisor relaunches it EXACTLY ONCE (see `apps/desktop/src/main/sidecar.ts`).
//! On that relaunch the sidecar reopens SQLite and rebuilds the persisted state from the
//! runtime tables.
//!
//! ## A crash is a CLEAN shutdown (v2.5.8 — revert of the v2.5.5 crash-UI)
//!
//! Earlier (v2.5.5) a crash got SPECIAL handling: every instance persisted as `running`
//! was flagged `interrupted`, every still-`active` agent session was forced to `unknown`,
//! and the renderer showed an orange "Interrupted after a crash — choose what to resume"
//! card (Resume / Ignore). That special path is GONE. A crash is now treated as an ORDINARY
//! (clean) shutdown: the boot reconcile marks NOTHING interrupted and flips NO session to
//! `unknown`. The NORMAL resume path then applies — the SILENT lazy agent-session resume,
//! gated by the project's `resume_agent_sessions` toggle AND the #28 activity gate (a resume
//! is never injected into a live/working session). Toggle ON → the session resumes silently;
//! toggle OFF → nothing resumes. No card, no `interrupted` set, no doubtful-session UI.
//!
//! ## Why a sidecar-owned state file (and not a new DB column)
//!
//! `command_instances.last_state` is CHECK-constrained to `idle|running|success|error`
//! (migration 00000000000003) and `nyx-core` is the FROZEN v1 port — byte-unchanged. So we
//! cannot persist recovery bookkeeping in that column nor add a migration. Instead the
//! sidecar owns a tiny JSON state file next to `nyx.db`
//! (`<data_dir>/sidecar-recovery.json`) that records:
//!   * `clean_shutdown` — `false` while serving, flipped to `true` only by the ordered
//!     [`crate::runtime::Runtime::shutdown`]. If the next boot reads it still `false`, the
//!     previous run CRASHED (used ONLY for a diagnostic log now — the reconcile does the same
//!     thing either way).
//!   * `idempotency` — a journal of the idempotency keys of mutating operations already
//!     applied, so a mutation retried after a crash is de-duplicated rather than re-run.

use std::path::{Path, PathBuf};

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

/// The outcome of a boot-time recovery reconcile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileOutcome {
    /// `true` when the previous run did NOT shut down cleanly (a crash). Informational only:
    /// a crash is now handled exactly like a clean shutdown (no interrupted set, no
    /// `active -> unknown` sweep) — this flag just lets the boot log note it happened.
    pub was_crash: bool,
}

/// Reconcile the persisted runtime state at boot, BEFORE the boot restore runs. Reads the
/// sidecar-owned recovery file only to (a) note whether the previous run crashed (for the
/// boot log) and (b) re-arm the file to `clean_shutdown = false` for the run now starting,
/// preserving the idempotency journal.
///
/// A crash is treated as a CLEAN shutdown: this does NOT flag any instance `interrupted`, does
/// NOT flip any `active` agent session to `unknown`, and does NOT touch the shutdown snapshots.
/// The NORMAL resume path takes over — the silent lazy agent-session resume gated by the
/// project `resume_agent_sessions` toggle and the #28 activity gate — so nothing surfaces a
/// crash-recovery card and nothing is blindly relaunched.
pub fn reconcile_after_crash(data_dir: &Path) -> ReconcileOutcome {
    let had_file = RecoveryState::exists(data_dir);
    let prior = RecoveryState::load(data_dir);
    // A recovery file present but not-clean means the previous run crashed. Diagnostic only.
    let was_crash = had_file && !prior.clean_shutdown;

    // Re-arm the file to not-clean for the run we are starting, preserving the idempotency
    // journal (so a key applied just before a clean shutdown still dedupes after a restart).
    // Whether the prior run crashed or shut down cleanly, the reconcile does the SAME thing:
    // no interrupted set, no session sweep, no snapshot clearing — the normal (silent) resume
    // path handles everything downstream.
    let next = RecoveryState {
        clean_shutdown: false,
        idempotency: prior.idempotency,
    };
    next.save(data_dir);

    ReconcileOutcome { was_crash }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_core::db::{self, Db};

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

    /// CRASH PATH (v2.5.8 revert): a not-clean recovery file ⇒ the reconcile treats the crash
    /// as a CLEAN shutdown. It flags NOTHING interrupted, leaves the running instance's
    /// shutdown snapshot untouched, and — crucially — does NOT flip the still-`active` agent
    /// session to `unknown`. The session stays `active`, so the NORMAL (silent, toggle-gated)
    /// resume path applies. `was_crash` is still reported for the boot log.
    #[test]
    fn reconcile_after_crash_treats_a_crash_as_clean_no_interrupted_no_sweep() {
        let dir = temp_dir("crash");
        let db = open_db(&dir);
        let id = seed_running_instance(&db);

        // Simulate a crash: a PRIOR run armed the recovery file `clean_shutdown=false` at boot
        // and then crashed (never flipping it to true). An active agent session lingers, and the
        // running instance is still `running` + snapshotted was_running_on_shutdown.
        RecoveryState {
            clean_shutdown: false,
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

        let outcome = reconcile_after_crash(&dir);

        assert!(
            outcome.was_crash,
            "a not-clean recovery file is still recognised as a crash (for the boot log)"
        );
        // The active session is STILL `active` — the crash reconcile no longer flips it to
        // `unknown`, so the silent normal-resume path (not a card) will pick it up.
        assert_eq!(
            db.with_conn(db::active_agent_sessions).unwrap().len(),
            1,
            "the active session stays `active` after a crash (no forced active -> unknown)"
        );
        // The shutdown snapshot is untouched (a crash is treated as clean — no special clear).
        let row = db.with_conn(|c| db::get_instance(c, &id)).unwrap().unwrap();
        assert!(
            row.was_running_on_shutdown,
            "the shutdown snapshot is left as-is (crash handled like a clean shutdown)"
        );
        // The recovery file is re-armed to not-clean for THIS run — and there is no interrupted
        // bookkeeping anymore (the field is gone).
        let persisted = RecoveryState::load(&dir);
        assert!(
            !persisted.clean_shutdown,
            "re-armed to not-clean for this run"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// FRESH-BOOT PATH: NO recovery file yet (first run against this data dir) ⇒ not a crash;
    /// sessions untouched; the file is armed to not-clean for the new run.
    #[test]
    fn reconcile_is_noop_on_first_boot_without_a_recovery_file() {
        let dir = temp_dir("fresh");
        let db = open_db(&dir);
        let _id = seed_running_instance(&db);
        assert!(
            !RecoveryState::exists(&dir),
            "no recovery file exists before the very first reconcile"
        );

        let outcome = reconcile_after_crash(&dir);
        assert!(
            !outcome.was_crash,
            "a first boot with no recovery file is NOT a crash"
        );

        // The file is now armed for this run.
        assert!(
            RecoveryState::exists(&dir),
            "the reconcile armed a recovery file for this run"
        );
        let persisted = RecoveryState::load(&dir);
        assert!(!persisted.clean_shutdown, "armed not-clean for this run");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// CLEAN PATH: a prior `clean_shutdown=true` ⇒ not a crash; the file is re-armed to
    /// not-clean for the new run and the idempotency journal survives.
    #[test]
    fn reconcile_is_noop_after_clean_shutdown() {
        let dir = temp_dir("clean");
        let db = open_db(&dir);
        let _id = seed_running_instance(&db);
        // The ordered shutdown wrote clean_shutdown=true.
        RecoveryState {
            clean_shutdown: true,
            idempotency: vec!["k-1".into()],
        }
        .save(&dir);

        let outcome = reconcile_after_crash(&dir);
        assert!(!outcome.was_crash, "a clean prior shutdown is not a crash");

        let persisted = RecoveryState::load(&dir);
        assert!(!persisted.clean_shutdown, "re-armed to not-clean");
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
        std::fs::remove_dir_all(&dir).ok();
    }
}
