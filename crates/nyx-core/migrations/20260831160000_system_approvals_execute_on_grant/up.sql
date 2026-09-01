-- `system_command_approvals`, RESHAPÉE : l'effet s'exécute **à l'accord**, plus au rejeu.
--
-- (Revue humaine 01M1C7C8K0Q1WBHDQKZ29GH8YJ, findings 01M1C7CWMF4ABHRWT1DGBJTXW3 et
-- 01M1C7DVQ53JJ90HGD5K1HDY11 — voir PDR-0030.)
--
-- ## Pourquoi la table change
--
-- Le protocole d'origine remettait un JETON à l'appelant dans `error.data`, et l'agent rejouait son
-- appel en le portant. Un agent réel a montré que ce canal n'existe pas : aucun client MCP n'est
-- tenu d'exposer `error.data` au modèle, et Claude Code ne remonte que `error.message`. Le jeton
-- n'atteignait donc jamais le décideur, et 100 % des opérations mutantes du scope Système étaient
-- inatteignables.
--
-- Le protocole est renversé : **l'accord humain déclenche l'effet côté serveur**, sur la charge
-- canonique que le ledger détient déjà. Il n'y a plus rien de secret à transporter, donc plus de
-- jeton — d'où la disparition de `token_hash`.
--
-- ## Les deux colonnes qui apparaissent
--
--   * `tool_name` — le NOM D'OUTIL à réexécuter. `operation_kind` est le verbe tel que la matrice
--     le nomme (`manage_command_group.add`, `start_command.env`) : lisible par un humain, mais pas
--     dispatchable. L'exécuteur a besoin du nom réel, et le déduire par découpage du genre serait
--     une convention tacite que le premier verbe composé casserait.
--   * `result_json` — le RÉSULTAT de l'effet, sérialisé. C'est ce que l'agent lit lorsqu'il revient
--     après coup (`system_approval_status`) : la décision et son effet se sont produits sans lui,
--     donc le ledger doit pouvoir lui rendre ce que l'appel aurait rendu.
--
-- `claimed_at` reste, et prend un sens plus fort : il n'est plus posé par une revendication de
-- l'agent mais par la TRANSITION D'ACCORD elle-même, dans le même `UPDATE … WHERE state='pending'`.
-- Un seul de N clics concurrents peut donc voir la ligne, ce qui fait de l'exécution unique une
-- propriété du SQL et non d'un verrou applicatif — et rend l'état « accordée jamais réclamée »
-- littéralement inconstructible.
--
-- ## Pourquoi une RECRÉATION et pas un ALTER
--
-- `token_hash` est `NOT NULL` et SQLite ne sait pas retirer une colonne d'une table existante dans
-- le dialecte que ce bundle embarque. Surtout, le ledger n'est PAS un historique : c'est un état de
-- protocole dont la fenêtre est de cinq minutes et la rétention de vingt-quatre heures. La trace
-- durable est dans `domain_events`, que chaque transition écrit. Recréer la table coûte donc au pire
-- des demandes déjà mortes, et jamais une information qui n'existe qu'ici.

DROP INDEX IF EXISTS idx_system_approvals_pending_fingerprint;
DROP INDEX IF EXISTS idx_system_approvals_state_expiry;
DROP TABLE IF EXISTS system_command_approvals;

CREATE TABLE system_command_approvals (
    -- UUIDv7 : ordonné dans le temps, donc la liste « en attente » se trie sans index secondaire.
    id TEXT PRIMARY KEY NOT NULL,
    -- Le projet Système visé. ON DELETE CASCADE par cohérence de schéma ; en pratique le singleton
    -- ne se supprime pas (la frontière de domaine refuse `delete_project` sur lui).
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- L'OUTIL MCP à réexécuter à l'accord (`add_command`, `start_command`…). Distinct du genre
    -- ci-dessous, qui peut le qualifier (`manage_command_group.add`).
    tool_name TEXT NOT NULL,
    -- Le VERBE protégé, tel que la matrice de garde le nomme. Vocabulaire ouvert en SQL et CLOS en
    -- Rust : la matrice est la source de vérité, et un CHECK ici obligerait une migration à chaque
    -- verbe ajouté.
    operation_kind TEXT NOT NULL,
    -- La charge de l'opération, sérialisée sous forme CANONIQUE (clés triées, JSON compact). C'est
    -- elle, et elle seule, que l'exécuteur rejoue : l'humain approuve ce qu'il a vu, et le serveur
    -- exécute ce qu'il a montré.
    canonical_payload TEXT NOT NULL,
    -- Le digest de l'ensemble (projet + genre + charge + révision + membres). Dédup et affichage.
    fingerprint TEXT NOT NULL,
    -- L'état de la CIBLE au moment de la demande, ou NULL quand l'opération n'a pas de cible
    -- préexistante (`add_command`).
    target_revision TEXT,
    -- Pour une opération de GROUPE ou de LOT : l'ensemble ORDONNÉ exact des membres et leur
    -- révision. NULL pour une opération à cible unique.
    member_snapshot TEXT,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'granted', 'denied', 'expired')),
    requested_at BIGINT NOT NULL,
    -- Absolu (epoch ms), pas une durée : un reboot ne rafraîchit pas la fenêtre.
    expires_at BIGINT NOT NULL,
    decided_at BIGINT,
    -- Posé PAR l'accord, dans la même transition conditionnelle. NON NULL ⇔ l'effet a été autorisé
    -- à partir, exactement une fois.
    claimed_at BIGINT,
    completed_at BIGINT,
    -- `success` | `failure` (+ raison courte) — journalisé après l'effet, jamais avant.
    outcome TEXT,
    -- Ce que l'effet a rendu, sérialisé : la réponse que l'agent lira s'il revient après coup.
    result_json TEXT,
    -- Métadonnées d'acteur : qui a demandé (l'agent, par son terminal appelant quand il en a un),
    -- qui a tranché (l'humain, `ui` par défaut). Jamais un secret : il n'y en a plus.
    requested_by TEXT,
    decided_by TEXT
);

-- LA règle du protocole en SQL : au plus UNE demande en attente par empreinte.
CREATE UNIQUE INDEX idx_system_approvals_pending_fingerprint
    ON system_command_approvals(fingerprint)
    WHERE state = 'pending';

-- La lecture de la surface d'attention UI (« les demandes en attente, les plus anciennes d'abord »)
-- et le balayage d'expiration paresseuse passent tous deux par (state, expires_at).
CREATE INDEX idx_system_approvals_state_expiry
    ON system_command_approvals(state, expires_at);
