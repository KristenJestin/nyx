-- Schema v32: the `agent_session_prototypes` link table (v2.7 — Prototype : machinerie, Phase 2,
-- task 01KY50ZPVSMZPSY351VJNR7Z0F — "Seam proto <-> agent-session + actions agent-driven (MCP)").
--
-- ## Why this table (the proto ↔ session seam, PDR-0009)
--
-- "Le proto est rattaché à la SESSION AGENT (terminal) qui le crée/travaille, COMME les Specs."
-- The Specs use `agent_session_specs` (v20); the prototype gets the SAME shape — a MANY-TO-MANY
-- link between an agent SESSION and the prototypes it works. The auto-map ADDS a prototype to a
-- session's set when the session WORKS it via a `proto_*` MCP mutation (present a round, elect a
-- variant, raise/resolve a comment, rebase, distill), and a `/clear` vacates the set.
--
-- Modeled DELIBERATELY as its OWN link table — NOT derived from `agent_session_specs` through the
-- Spec — because a session can work a prototype (the `proto_*` tools) WITHOUT touching the Spec's
-- authoring tools, so the Spec would not be in the session's set and a derived proto set would be
-- empty. The direct link captures proto work on its own terms.
--
-- Anchored on the SESSION (agent_sessions.id), the generic session substrate (ADR-0010), so the
-- mapping is provider-agnostic (Claude today, another provider tomorrow — the core never names a
-- provider). The "prototypes of a terminal" resolution is a JOIN: terminal → its ACTIVE session →
-- this set (`db.rs::prototypes_for_terminal`) — the anchor a FUTURE proto overlay reads (this
-- migration POSES the seam; the overlay itself is a follow-up slice).
--
-- Timestamps mirror the rest of the schema: epoch MILLISECONDS as INTEGER, stamped by the Rust
-- backend on every write, with the portable julianday DEFAULT as a raw-insert safety net.

CREATE TABLE agent_session_prototypes (
    -- The agent session that works the prototype (FK → agent_sessions). ON DELETE CASCADE: the
    -- mapping has no meaning without its session — a session removed (terminal closed) drops its
    -- whole set. The set also lives and dies with the session row: `/clear` vacates it explicitly
    -- (the row stays active, so the set would otherwise persist), and a brand-new session row
    -- simply starts with no rows here.
    session_id    TEXT    NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,

    -- The prototype the session works (FK → prototypes). ON DELETE CASCADE: a deleted prototype
    -- drops out of every session's set automatically (no dangling mapping).
    prototype_id  TEXT    NOT NULL REFERENCES prototypes(id) ON DELETE CASCADE,

    -- When the prototype entered this session's set (first-worked instant). Epoch ms.
    created_at    INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),

    -- The (session, prototype) pair is the PRIMARY KEY: a prototype is in a session's set AT MOST
    -- ONCE. Re-working an already-mapped prototype is a no-op (INSERT … ON CONFLICT DO NOTHING —
    -- accumulation is idempotent, never a duplicate row).
    PRIMARY KEY (session_id, prototype_id)
);

-- Reverse-lookup index: "which sessions work THIS prototype". The forward direction — "the set
-- of a session" and the un-map delete — is served by the composite PRIMARY KEY prefix.
CREATE INDEX idx_agent_session_prototypes_prototype ON agent_session_prototypes (prototype_id);
