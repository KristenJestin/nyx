-- Schema v51: bring the EXISTING `kind='url'` annexes to the CANONICAL form the write authority
-- now stores, and re-deduplicate on it (human review 01M07WC6BXJ85YTDPY00P0H56R, finding F1).
--
-- ## Why a NEW migration and not a fix to v50
--
-- v50 deduplicated the inherited links on the RAW hash and created the partial unique index. It
-- may already have been applied on a real base, and an applied migration is history: editing its
-- `up.sql` would leave that base silently unmigrated (diesel keys applied migrations by VERSION,
-- never by content). Everything v51 does is therefore additive, on top of what v50 left. Its prose
-- is left untouched for the same reason — where v50 says "NO normalisation is implied […] two
-- distinct links — assumed", read it as the state of the decision AT v50: this file is what
-- supersedes it, and `db.rs` (the `url` annexe header) carries the current rule.
--
-- ## What changed above it
--
-- `add_annexe` now stores the CANONICAL form of a url (`db.rs::canonical_annexe_url`): surrounding
-- blanks trimmed, scheme and host lowered, the path's trailing slash dropped — and nothing else
-- (no `www.` stripping: a different host is a different link; no percent-decoding, no query
-- sorting, no default-port or fragment removal). Rows written BEFORE that change hold the string
-- as it was typed, so the same target can sit in the base three times (`…/pull/1`, `…/pull/1/`,
-- `HTTPS://…`) while a new write of any of them now hashes to a single value. This migration makes
-- the stored rows say what the writer would say today — otherwise the dedup index protects new
-- writes only, and the duplicates a user can SEE stay on screen.
--
-- ## Why SQL calls back into Rust
--
-- `nyx_canonical_url` and `nyx_sha1` are registered on the migrating connection by
-- `db.rs::register_migration_functions`. The canonicalisation rule then has ONE implementation
-- instead of a Rust one and a re-written-in-SQLite one free to drift (the split this codebase
-- refuses elsewhere — one authority for UI + MCP, PDR-0005), and SHA-1 becomes computable at all:
-- SQLite has no hash function, and `annex_blobs` is CONTENT-ADDRESSED, so changing a blob's
-- content without recomputing its `content_hash` would corrupt the store's core invariant.
--
-- ## The order of the statements, and why each is where it is
--
-- 1. DROP the v50 index. Re-pointing the rows at their canonical blob is exactly what CREATES the
--    duplicates this migration then removes; with the unique index still in place the very first
--    `UPDATE` would trip it and fail the migration — and a failed migration makes nyx refuse to
--    start. The index is recreated identically at the end, so the schema after v51 is the schema
--    after v50 plus canonical data.
-- 2. REMEMBER the blobs about to be left behind (the temp table). After the re-point their hashes
--    are unreachable, and the GC at step 6 must delete THOSE and nothing else: a blanket "delete
--    every unreferenced blob" would also sweep the orphans a Spec deletion leaves behind (the v27
--    cascade drops attachments and keeps blobs — declared behaviour, out of scope here).
-- 3. MATERIALISE the canonical blob of every stale url. `INSERT OR IGNORE` because the canonical
--    form is very often ALREADY stored — by the row that was written correctly, or by another
--    Spec — and `annex_blobs` is global and content-addressed: one physical copy per content. The
--    source blob's `created_at` is carried over rather than "now": the content is the same bytes,
--    and its age is real.
-- 4. RE-POINT the stale url attachments at that blob. `kind = 'url'` only — a doc / artefact /
--    prototype is a DOCUMENT and its bytes are never rewritten (v27 surface, untouched).
-- 5. DEDUPLICATE, by the SAME rule v50 used, on the now-canonical hashes: keep the OLDEST row of
--    each `(spec_id, content_hash)` group (`created_at` ascending, the UUIDv7 `id` breaking a tie),
--    which preserves the link's place in the newest-first list and the FIRST label a human gave it.
--    No blob is orphaned HERE: every deleted row shares its hash with the row kept beside it.
-- 6. GC the blobs the re-point stranded — but only those, and only if nothing else references them
--    (a `doc` may legitimately hold the very same string, in which case the blob stays).
-- 7. RECREATE the v50 index, over data that can now hold it.

DROP INDEX IF EXISTS idx_annexes_url_unique_per_spec;

CREATE TEMP TABLE nyx_v51_stale_blobs (content_hash TEXT PRIMARY KEY);

INSERT OR IGNORE INTO nyx_v51_stale_blobs (content_hash)
SELECT a.content_hash
  FROM annexes AS a
  JOIN annex_blobs AS b ON b.content_hash = a.content_hash
 WHERE a.kind = 'url'
   AND nyx_canonical_url(b.content) <> b.content;

INSERT OR IGNORE INTO annex_blobs (content_hash, content, byte_len, created_at)
SELECT nyx_sha1(nyx_canonical_url(b.content)),
       nyx_canonical_url(b.content),
       length(CAST(nyx_canonical_url(b.content) AS BLOB)),
       b.created_at
  FROM annexes AS a
  JOIN annex_blobs AS b ON b.content_hash = a.content_hash
 WHERE a.kind = 'url'
   AND nyx_canonical_url(b.content) <> b.content;

UPDATE annexes
   SET content_hash = (
       SELECT nyx_sha1(nyx_canonical_url(b.content))
         FROM annex_blobs AS b
        WHERE b.content_hash = annexes.content_hash
   )
 WHERE kind = 'url'
   AND content_hash IN (SELECT content_hash FROM nyx_v51_stale_blobs);

DELETE FROM annexes
WHERE kind = 'url'
  AND id <> (
      SELECT keep.id
        FROM annexes AS keep
       WHERE keep.kind = 'url'
         AND keep.spec_id = annexes.spec_id
         AND keep.content_hash = annexes.content_hash
       ORDER BY keep.created_at ASC, keep.id ASC
       LIMIT 1
  );

DELETE FROM annex_blobs
 WHERE content_hash IN (SELECT content_hash FROM nyx_v51_stale_blobs)
   AND content_hash NOT IN (SELECT content_hash FROM annexes);

DROP TABLE nyx_v51_stale_blobs;

CREATE UNIQUE INDEX idx_annexes_url_unique_per_spec
    ON annexes (spec_id, content_hash)
    WHERE kind = 'url';
