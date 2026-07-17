-- Schema v20: the `agent_session_specs` link table (v2.5.10 — Surcouche terminal↔Spec,
-- Phase 1, task 01KXN0HSQFP25TQVF61JHXKZAY — "DB : table agent_session_specs
-- (many-to-many) + migration").
--
-- ## Why this table (the ephemeral session↔Specs mapping, PRD Impl Decisions)
--
-- An agent SESSION works one OR MORE Specs (a feature split into several Specs, all created
-- in the same session). The overlay on a terminal must surface the whole SET of Specs its
-- live session is working. The mapping is MANY-TO-MANY and EPHEMERAL: nyx-core ADDS a Spec
-- to a session's set when the session WORKS it via an MCP mutation (spec_create /
-- spec_update_section / task_add / task_transition / a dev transition), and a `/clear`
-- vacates the set. It is deliberately a LINK TABLE — NOT a scalar `agent_sessions.spec_id`
-- (a session has N Specs, not one) and NOT a pin on a terminal/workspace (the session, not
-- the terminal, is the unit of work — the pin was rejected).
--
-- Anchored on the SESSION (agent_sessions.id), the generic session substrate (ADR-0010), so
-- the mapping is provider-agnostic (Claude today, OpenCode/Codex tomorrow — the core never
-- names a provider). The "Specs of a terminal" resolution is a JOIN: terminal → its ACTIVE
-- session → this set (see `db.rs::specs_for_terminal`).
--
-- Timestamps mirror the rest of the schema: epoch MILLISECONDS as INTEGER, stamped by the
-- Rust backend on every write, with the portable julianday DEFAULT as a raw-insert safety net.

CREATE TABLE agent_session_specs (
    -- The agent session that works the Spec (FK → agent_sessions). ON DELETE CASCADE: the
    -- mapping has no meaning without its session — a session removed (terminal closed) drops
    -- its whole set. The set also lives and dies with the session row: `/clear` vacates it
    -- explicitly (the row stays active, so the set would otherwise persist), and a brand-new
    -- session row simply starts with no rows here.
    session_id  TEXT    NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,

    -- The Spec the session works (FK → specs). ON DELETE CASCADE: a deleted Spec drops out of
    -- every session's set automatically (no dangling mapping to a Spec that no longer exists).
    spec_id     TEXT    NOT NULL REFERENCES specs(id) ON DELETE CASCADE,

    -- When the Spec entered this session's set (first-worked instant). Epoch ms.
    created_at  INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),

    -- The (session, spec) pair is the PRIMARY KEY: a Spec is in a session's set AT MOST ONCE.
    -- Re-working an already-mapped Spec is a no-op (the backend `link_session_spec` uses
    -- INSERT OR IGNORE — accumulation is idempotent, never a duplicate row).
    PRIMARY KEY (session_id, spec_id)
);

-- Reverse-lookup index: "which sessions work THIS Spec" (a Spec transitioning done/archived
-- can be dropped from every set, PRD Impl Decision option). The forward direction — "the set
-- of a session" and the un-map delete — is served by the composite PRIMARY KEY prefix.
CREATE INDEX idx_agent_session_specs_spec ON agent_session_specs (spec_id);
