-- Schema v50: ONE external link per Spec — a PARTIAL unique index on `annexes (spec_id,
-- content_hash) WHERE kind = 'url'` (PRD "Liens externes sur un Spec", Impl Decisions
-- "DÉDUPLICATION").
--
-- ## Why no new table
--
-- A "lien externe attaché à une Spec" is already an annexe: `kind='url'` is a value of the v27
-- CHECK vocabulary, `annex_blobs.content` holds the URL string ("for a `url` the \"content\" IS
-- the URL string", v27), `annexes.title` holds the label, and the attachment hangs off
-- `specs(id)` — the LOGICAL identity — so a fork keeps it without copying anything. The only
-- thing the model lacked was UNIQUENESS: nothing stopped the same URL being attached to the
-- same Spec twice, which is how a link list rots into duplicates.
--
-- ## Why PARTIAL
--
-- The uniqueness is a property of LINKS, not of annexes. A doc, an artefact or a prototype may
-- legitimately be attached twice to one Spec (two names over the same bytes — v27 relies on it:
-- its own dedup test attaches identical content twice to one Spec). Narrowing the index with
-- `WHERE kind = 'url'` is what keeps that right intact while closing the duplicate-link door.
--
-- ## Why (spec_id, content_hash)
--
-- The dedup axis is the ATTACHMENT, not the content. `annex_blobs` is a GLOBAL content-addressed
-- store, so the same URL attached to two DIFFERENT Specs already shares one blob — and that is
-- the wanted behaviour, not a collision. Keying the index on the pair scopes the constraint to
-- one Spec: `https://x` on Spec A and on Spec B are two attachment rows over one blob, while a
-- second `https://x` on Spec A is refused.
--
-- `content_hash` (and not the URL text) is the column because it IS the URL's identity here: it
-- is the SHA-1 of the stored content, and the content of a `url` annexe is the URL string.
--
-- NO normalisation is implied. `https://x` and `https://x/` hash differently, so they are two
-- distinct links — assumed (PRD Impl Decisions "VALIDATION": "deux URL qui ne diffèrent que par
-- un slash final sont deux liens distincts").
--
-- ## Why the DELETE comes first (PRD Scope IN: "déduplication […] sur les annexes EXISTANTES")
--
-- SQLite REFUSES to build a UNIQUE index over rows that already violate it, and nothing before
-- v50 stopped the same URL being attached twice to one Spec (`annexe_add` never checked). A bare
-- `CREATE UNIQUE INDEX` would therefore fail on any inherited base carrying such a pair — and a
-- failed migration makes `run_migrations` propagate, which makes nyx REFUSE TO START (`db.rs`
-- "Returns Err if any migration fails"). The migration must therefore CLEAN what it then forbids:
-- the up deduplicates the existing `kind='url'` rows first, so the index is created over data that
-- can hold it.
--
-- WHICH ROW IS KEPT: the OLDEST of each `(spec_id, content_hash)` group — `created_at` ascending,
-- `annexes.id` (a UUIDv7, itself time-ordered) breaking a tie. Keeping the oldest preserves the
-- link's original position in the newest-first list (PRD Impl Decisions "ORDRE") and keeps the
-- FIRST label a human gave it; the later re-attachments are the accidental ones. The choice is
-- lossless in content: every deleted row is by construction a duplicate of the kept row's
-- `(spec_id, content_hash)` pair, i.e. THE SAME URL on THE SAME Spec.
--
-- NO blob is orphaned by this DELETE, so it needs no `annex_blobs` GC (the Rust-side GC of
-- `remove_annexe` cannot run from SQL): every deleted row shares its `content_hash` with the row
-- kept in its own group, so each touched blob still has at least one referencing attachment.
--
-- Scoped to `kind = 'url'` exactly like the index: a `doc` / `artefact` / `prototype` attached
-- twice to one Spec is legitimate (v27 relies on it) and is NOT touched.

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

CREATE UNIQUE INDEX idx_annexes_url_unique_per_spec
    ON annexes (spec_id, content_hash)
    WHERE kind = 'url';
