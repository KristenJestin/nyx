//! Durable per-terminal VT SCREEN SNAPSHOT store (v2.2.5 — terminal persistence, PRD
//! `01KWMDBKW7GZ0X310TK64QEN32`, Phase 2 task #7: re-hydration at restart).
//!
//! ## Why a sidecar-owned file (and not a DB column)
//!
//! `nyx-core`'s SQLite schema is the FROZEN v1 port — its migrations are byte-unchanged, so
//! the sidecar must NOT add a migration or a `terminals` column (the exact constraint the
//! crash-recovery state faced — see [`crate::runtime::recovery`]). It solves it the same way:
//! the sidecar owns a small JSON file next to `nyx.db` and `nyx-core` never reads it.
//!
//! This store keeps, per persistent terminal RECORD id, the last serializable
//! [`ScreenSnapshot`] the headless VT model produced (main + alternate screen). The runtime
//! writes it on the ordered SHUTDOWN (snapshotting every live terminal); the next boot reads
//! it, and the `terminal_screen_rehydration` db-command hands the renderer the ANSI
//! re-hydration bytes so a restarted terminal is repainted FAITHFULLY — a full-screen TUI
//! (vim/claude) included, not just the scrollback history.
//!
//! ## Restart flow
//!
//! 1. **Shutdown** — [`crate::runtime::Runtime::shutdown`] calls [`SnapshotStore::save_live`]
//!    with every live terminal's `(record_id, snapshot)`; the store atomically writes
//!    `<data_dir>/terminal-snapshots.json`.
//! 2. **Boot** — [`SnapshotStore::load`] reads that file (a missing/corrupt file is an empty
//!    store, never a boot failure).
//! 3. **Re-hydrate** — as the renderer re-creates each terminal it calls
//!    `terminal_screen_rehydration { recordId }`; the sidecar returns the snapshot's
//!    [`ScreenSnapshot::to_rehydration_bytes`], which the renderer writes verbatim into the
//!    fresh wterm/ghostty terminal. Idempotent: the byte stream starts with a reset+clear, so
//!    replaying it is safe, and reading it does not consume it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nyx_core::vt_screen::ScreenSnapshot;
use serde::{Deserialize, Serialize};

/// The file name of the sidecar-owned terminal-snapshot store, next to `nyx.db`.
pub const SNAPSHOT_FILE_NAME: &str = "terminal-snapshots.json";

/// The persisted store: a map from persistent terminal RECORD id to its last VT screen
/// snapshot. `BTreeMap` for a deterministic on-disk order (stable diffs / tests).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotStore {
    #[serde(default)]
    snapshots: BTreeMap<String, ScreenSnapshot>,
}

impl SnapshotStore {
    /// The store-file path under a data dir.
    pub fn path_in(data_dir: &Path) -> PathBuf {
        data_dir.join(SNAPSHOT_FILE_NAME)
    }

    /// Load the store from `<data_dir>/terminal-snapshots.json`. A missing file (first run /
    /// wiped dir) or a corrupt one is an EMPTY store, never a boot failure — the snapshot is an
    /// optimization on top of the DB scrollback, not the source of truth.
    pub fn load(data_dir: &Path) -> Self {
        match std::fs::read(Self::path_in(data_dir)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => SnapshotStore::default(),
        }
    }

    /// Persist the store atomically (temp sibling + rename) so a crash mid-write never leaves a
    /// torn file. Best-effort: an I/O error is swallowed (the store is an optimization).
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

    /// Replace the whole store with the given live snapshots and persist it. Called on the
    /// ordered shutdown with every live terminal's `(record_id, snapshot)`. Terminals with no
    /// record id are skipped (they cannot be correlated back at restart). Records absent from
    /// `live` are DROPPED — the store reflects exactly what was on screen at shutdown.
    pub fn save_live<I>(&mut self, data_dir: &Path, live: I)
    where
        I: IntoIterator<Item = (String, ScreenSnapshot)>,
    {
        self.snapshots = live
            .into_iter()
            .filter(|(record_id, _)| !record_id.is_empty())
            .collect();
        self.save(data_dir);
    }

    /// The stored snapshot for a record id, if any.
    pub fn get(&self, record_id: &str) -> Option<&ScreenSnapshot> {
        self.snapshots.get(record_id)
    }

    /// The re-hydration ANSI byte stream for a record id, or `None` when no snapshot is stored.
    /// This is the payload the renderer writes verbatim into a fresh terminal to restore the
    /// screen (see [`ScreenSnapshot::to_rehydration_bytes`]).
    pub fn rehydration_bytes(&self, record_id: &str) -> Option<Vec<u8>> {
        self.snapshots
            .get(record_id)
            .map(|s| s.to_rehydration_bytes())
    }

    /// Number of stored snapshots (for logging / tests).
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_core::vt_screen::VtScreen;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nyx-snapstore-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A snapshot of a terminal that entered the alt-screen and painted a TUI line.
    fn tui_snapshot() -> ScreenSnapshot {
        let mut vt = VtScreen::new(5, 20);
        vt.feed(b"shell history\r\n");
        vt.feed(b"\x1b[?1049h\x1b[2J\x1b[HTUI ROW");
        vt.snapshot()
    }

    #[test]
    fn save_and_load_round_trips_a_snapshot() {
        let dir = temp_dir("roundtrip");
        let snap = tui_snapshot();
        let mut store = SnapshotStore::default();
        store.save_live(&dir, [("rec-1".to_string(), snap.clone())]);

        let loaded = SnapshotStore::load(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.get("rec-1"),
            Some(&snap),
            "snapshot survives the disk round-trip"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rehydration_bytes_carry_the_alt_screen_tui() {
        let dir = temp_dir("rehydrate");
        let mut store = SnapshotStore::default();
        store.save_live(&dir, [("rec-1".to_string(), tui_snapshot())]);
        let loaded = SnapshotStore::load(&dir);

        let bytes = loaded
            .rehydration_bytes("rec-1")
            .expect("bytes for a stored record");
        // Feeding the bytes into a fresh VT reconstructs the TUI on the alt-screen.
        let mut fresh = VtScreen::new(5, 20);
        fresh.feed(&bytes);
        let restored = fresh.snapshot();
        assert!(
            restored.alt_active,
            "restored terminal is back on the alt-screen"
        );
        let alt = restored.alt.expect("alt buffer");
        assert!(
            (0..alt.rows).any(|r| alt.row_text(r).contains("TUI ROW")),
            "the alt-screen TUI content is restored from the persisted snapshot"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_record_and_missing_file_yield_none() {
        let dir = temp_dir("missing");
        // No file written yet: an empty store, no rehydration bytes.
        let store = SnapshotStore::load(&dir);
        assert!(store.is_empty());
        assert!(store.rehydration_bytes("nope").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_file_loads_as_empty() {
        let dir = temp_dir("corrupt");
        std::fs::write(SnapshotStore::path_in(&dir), b"{not json").unwrap();
        let store = SnapshotStore::load(&dir);
        assert!(
            store.is_empty(),
            "a corrupt store file degrades to empty, never a boot failure"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_less_terminals_are_not_persisted() {
        let dir = temp_dir("recordless");
        let mut store = SnapshotStore::default();
        store.save_live(
            &dir,
            [
                ("".to_string(), tui_snapshot()),      // no record id -> skipped
                ("rec-2".to_string(), tui_snapshot()), // kept
            ],
        );
        let loaded = SnapshotStore::load(&dir);
        assert_eq!(loaded.len(), 1);
        assert!(loaded.get("rec-2").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_live_reflects_only_what_was_on_screen_at_shutdown() {
        let dir = temp_dir("replace");
        let mut store = SnapshotStore::default();
        store.save_live(&dir, [("a".to_string(), tui_snapshot())]);
        // A later shutdown with a different live set REPLACES the store (no stale 'a').
        store.save_live(&dir, [("b".to_string(), tui_snapshot())]);
        let loaded = SnapshotStore::load(&dir);
        assert!(
            loaded.get("a").is_none(),
            "a terminal closed before shutdown is not carried"
        );
        assert!(loaded.get("b").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
