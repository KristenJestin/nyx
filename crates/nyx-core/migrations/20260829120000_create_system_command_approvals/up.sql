-- `system_command_approvals` — le LEDGER one-shot des approbations humaines du scope **Système**
-- (PRD « Scope Système : terminaux & commandes hors-projet (singleton) », section « Protocole
-- d'approbation »).
--
-- ## Ce que cette table est
--
-- Une demande d'autorisation, faite par un appelant MCP, pour UNE opération EXACTE sur le scope
-- Système, tranchée par un humain depuis l'app, et utilisable UNE SEULE FOIS. Ce n'est ni une
-- règle permanente, ni une délégation, ni un historique : une ligne = une opération = une décision
-- = un usage.
--
-- ## Pourquoi une table, et pas un état en mémoire
--
-- Parce que les deux bouts du protocole ne partagent aucun processus dans le temps. L'appelant est
-- un agent MCP sans terminal ; le décideur est un humain qui peut mettre trente secondes à cliquer,
-- ou fermer l'app entre-temps. Un jeton en mémoire disparaîtrait au redémarrage — soit en accordant
-- silencieusement (pire), soit en perdant la demande sans le dire. Persistée, la demande survit au
-- reboot ET expire quand même : `expires_at` est absolu, donc une app rallumée après le TTL trouve
-- une demande morte, pas une demande fraîche.
--
-- ## L'empreinte, et pourquoi la charge canonique est stockée À CÔTÉ
--
-- `fingerprint` est le hash de (projet + genre d'opération + charge canonique + révision de cible +
-- snapshot ordonné des membres). Il sert à DEUX choses et à deux seulement : dédupliquer les
-- demandes en attente (l'index partiel ci-dessous) et donner à l'humain/à l'agent un identifiant
-- court et stable de « la même demande ».
--
-- Il n'est PAS ce sur quoi le replay s'autorise. `canonical_payload`, `target_revision` et
-- `member_snapshot` sont conservés en clair et comparés VERBATIM au moment de la revendication.
-- Conséquence : une collision de hash ne peut pas transformer une approbation pour l'opération A en
-- autorisation pour l'opération B — l'égalité qui autorise est celle des chaînes, pas celle du
-- digest. C'est la raison pour laquelle ces trois colonnes existent au lieu de la seule empreinte.
--
-- ## L'index partiel EST la règle « une seule demande en attente par empreinte »
--
-- `idx_system_approvals_pending_fingerprint` est UNIQUE PARTIEL sur `fingerprint` restreint à
-- `state = 'pending'`. Deux appels concurrents portant la MÊME opération ne peuvent donc pas créer
-- deux demandes : le perdant prend une violation d'unicité, sa transaction est annulée entière, et
-- il reçoit `approval_pending_existing` — l'id et l'expiration de la demande gagnante, SANS jeton.
-- Ne pas fabriquer un second jeton est le cœur de la propriété « seul le créateur peut rejouer » :
-- un appelant qui n'a pas reçu le jeton ne peut ni sonder ni revendiquer, même en connaissant l'id.
--
-- Le partiel laisse évidemment autant de lignes TERMINALES (`granted`/`denied`/`expired`) que l'on
-- veut porter la même empreinte : deux `docker compose up` approuvés à dix minutes d'intervalle sont
-- deux lignes légitimes.
--
-- ## Le jeton n'est pas ici
--
-- `token_hash` porte le HASH du jeton, jamais le jeton. Le jeton brut est retourné UNE FOIS, dans la
-- réponse d'erreur `approval_required`, et n'est ni persisté ni journalisé nulle part. Une base
-- volée ne rejoue donc rien : elle ne contient pas de quoi se faire passer pour le créateur.
--
-- ## Les horodatages sont un cycle de vie, pas de la télémétrie
--
-- `requested_at` / `expires_at` bornent la fenêtre (TTL de cinq minutes, horloge injectable côté
-- Rust). `decided_at` marque l'accord ou le refus humain. `claimed_at` marque la REVENDICATION —
-- c'est lui qui rend l'approbation one-shot : un `UPDATE … WHERE claimed_at IS NULL` ne peut réussir
-- qu'une fois, quelle que soit la concurrence. `completed_at` + `outcome` journalisent ce que
-- l'opération a produit APRÈS coup, sans jamais rendre la ligne rejouable — un échec de runner a
-- consommé sa tentative.
--
-- ## Pourquoi le préfixe est DATÉ
--
-- La lignée `000000000000NN` est FIGÉE à `00000000000051` : deux worktrees frères qui frappent
-- chacun « le prochain » numéro contre le même `%APPDATA%/nyx-v2/nyx.db` collisionnent en silence.
-- La règle est dans `docs/product/technical-architecture.md` et tenue par
-- `db.rs::every_migration_added_after_the_frozen_lineage_is_dated_not_numbered`.

CREATE TABLE system_command_approvals (
    -- UUIDv7 : ordonné dans le temps, donc la liste « en attente » se trie sans index secondaire.
    id TEXT PRIMARY KEY NOT NULL,
    -- Le projet Système visé. ON DELETE CASCADE par cohérence de schéma ; en pratique le singleton
    -- ne se supprime pas (la frontière de domaine refuse `delete_project` sur lui).
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- Le VERBE protégé, tel que la matrice de garde le nomme (`add_command`, `stop_command`,
    -- `manage_command_group.add`…). Vocabulaire ouvert en SQL et CLOS en Rust : la matrice est la
    -- source de vérité, et un CHECK ici obligerait une migration à chaque verbe ajouté.
    operation_kind TEXT NOT NULL,
    -- La charge de l'opération, sérialisée sous forme CANONIQUE (clés triées, JSON compact), sans
    -- les identifiants d'approbation eux-mêmes. C'est elle qui est recomparée au replay.
    canonical_payload TEXT NOT NULL,
    -- Le digest de l'ensemble (projet + genre + charge + révision + membres). Dédup et affichage.
    fingerprint TEXT NOT NULL,
    -- L'état de la CIBLE au moment de la demande (le digest des colonnes signifiantes de la commande
    -- ou du groupe visé), ou NULL quand l'opération n'a pas de cible préexistante (`add_command`).
    target_revision TEXT,
    -- Pour une opération de GROUPE ou de LOT : l'ensemble ORDONNÉ exact des membres et leur
    -- révision. Un membre ajouté, retiré ou déplacé entre l'accord et le replay change cette
    -- chaîne, donc invalide la revendication. NULL pour une opération à cible unique.
    member_snapshot TEXT,
    -- Le HASH du jeton opaque. Jamais le jeton.
    token_hash TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'granted', 'denied', 'expired')),
    requested_at BIGINT NOT NULL,
    -- Absolu (epoch ms), pas une durée : un reboot ne rafraîchit pas la fenêtre.
    expires_at BIGINT NOT NULL,
    decided_at BIGINT,
    -- NON NULL ⇔ l'approbation a été CONSOMMÉE. La condition one-shot.
    claimed_at BIGINT,
    completed_at BIGINT,
    -- `success` | `failure` (+ raison courte) — journalisé après l'effet, jamais avant.
    outcome TEXT,
    -- Métadonnées d'acteur : qui a demandé (l'agent, par son terminal appelant quand il en a un),
    -- qui a tranché (l'humain, `ui` par défaut). Jamais un jeton, jamais un secret.
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
