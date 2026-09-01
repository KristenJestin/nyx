-- Retour à la table jetonnée du protocole claim/replay. Comme l'`up`, une recréation : le ledger
-- porte un état de protocole de cinq minutes, pas un historique (celui-ci est dans `domain_events`).

DROP INDEX IF EXISTS idx_system_approvals_pending_fingerprint;
DROP INDEX IF EXISTS idx_system_approvals_state_expiry;
DROP TABLE IF EXISTS system_command_approvals;

CREATE TABLE system_command_approvals (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    operation_kind TEXT NOT NULL,
    canonical_payload TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    target_revision TEXT,
    member_snapshot TEXT,
    token_hash TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'granted', 'denied', 'expired')),
    requested_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    decided_at BIGINT,
    claimed_at BIGINT,
    completed_at BIGINT,
    outcome TEXT,
    requested_by TEXT,
    decided_by TEXT
);

CREATE UNIQUE INDEX idx_system_approvals_pending_fingerprint
    ON system_command_approvals(fingerprint)
    WHERE state = 'pending';

CREATE INDEX idx_system_approvals_state_expiry
    ON system_command_approvals(state, expires_at);
