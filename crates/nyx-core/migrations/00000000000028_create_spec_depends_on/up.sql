-- Schema v28: DÉPENDANCES Spec↔Spec — the `spec_depends_on` directed dependency graph
-- (v2.6 — Idées entretenues, annexes & dépendances Spec, Phase 3, task
-- 01KXZ9K4MBCXQP137ZD6CKJAJE — "Dépendances Spec↔Spec (spec_depends_on + détection cycle)
-- + MCP + badges").
--
-- ## Why this table (PRD Impl Decisions "DÉPENDANCES SPEC↔SPEC")
--
-- Specs carry STRUCTURAL constraints between them ("ne pas merger A avant B", "quand X land,
-- fais Y"). This table stores them as a DIRECTED graph:
--
--   * An edge `(from_spec_id, to_spec_id)` means **`from` depends on / must come after `to`**
--     — `to` is a prerequisite of `from`; `from` is "blocked-by" `to`, and `to` "unblocks"
--     `from`. This is the SAME orientation as `task_dependencies` (from --depends_on--> to).
--   * `kind` is the flavour of the link:
--       - `blocks` / `after` — DIRECTIONAL hard dependencies (from needs to first). Both feed
--         the cycle detection and the "bloquée par N / débloque M" badges.
--       - `related` — NON-directional CONTEXT only: it never gates ordering, so the backend
--         EXCLUDES it from cycle detection (a `related` link can never be refused) and from the
--         badge counts. It is surfaced separately as context.
--   * `note` is an optional free-text rationale ("don't merge before the auth rework lands").
--
-- Acyclicity is enforced in the BACKEND ([`add_spec_dependency`]: a reachability DFS over the
-- directional edges before each insert), like `task_dependencies` — SQLite cannot check
-- transitive acyclicity. Timestamps mirror the rest of the schema: epoch MILLISECONDS as
-- INTEGER, stamped by the Rust backend on write, with the portable julianday DEFAULT as a
-- raw-insert safety net.

CREATE TABLE spec_depends_on (
    -- The DEPENDENT side (the Spec that depends on / comes after the other). ON DELETE CASCADE:
    -- deleting a Spec removes every edge it participates in (no dangling edge to a gone Spec).
    from_spec_id TEXT    NOT NULL REFERENCES specs(id) ON DELETE CASCADE,

    -- The PREREQUISITE side (the Spec that must come first / that unblocks the dependent). ON
    -- DELETE CASCADE like the other endpoint.
    to_spec_id   TEXT    NOT NULL REFERENCES specs(id) ON DELETE CASCADE,

    -- The link flavour. `blocks`/`after` are directional (feed cycle detection + badges);
    -- `related` is context-only (excluded from both). CHECK-enforced vocabulary.
    kind         TEXT    NOT NULL DEFAULT 'blocks'
        CHECK (kind IN ('blocks', 'after', 'related')),

    -- Optional free-text rationale for the constraint.
    note         TEXT,

    created_at   INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),

    -- A Spec pair is linked AT MOST ONCE (re-linking upserts the kind/note in the backend).
    PRIMARY KEY (from_spec_id, to_spec_id),

    -- A Spec can never depend on itself (the smallest cycle) — the backend also returns a typed
    -- error, but the CHECK is the last line of defence.
    CHECK (from_spec_id <> to_spec_id)
);

-- Reverse-lookup index: "which Specs depend on THIS Spec" — the query the "débloque M" badge
-- and the cycle DFS's incoming direction run. The forward direction ("this Spec's
-- prerequisites", the "bloquée par N" badge) is served by the composite PRIMARY KEY prefix.
CREATE INDEX idx_spec_depends_on_to ON spec_depends_on (to_spec_id);
