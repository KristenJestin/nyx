-- Reconstruction des six tables du moteur d'orchestration, dans la forme EXACTE qu'elles avaient
-- juste avant le `up.sql` d'à côté — c'est-à-dire la forme CUMULÉE que la chaîne leur avait donnée,
-- pas celle de leur migration de création.
--
-- ## Pourquoi la « forme cumulée » et pas le v34 d'origine
--
-- Les tests de migration rejouent la chaîne À L'ENVERS (`revert_last_migration`), puis la
-- réappliquent. Ce `down.sql` n'est donc pas un ornement : c'est la marche sur laquelle les
-- `down.sql` situés EN DESSOUS vont poser le pied. `00000000000049_human_turns_consumed_by` va
-- vouloir retirer `human_turns.consumed_by` ; `20260828205640_nodes_set_category_type` va vouloir
-- rétrécir le CHECK de `nodes.type` en supprimant d'abord les lignes `set_category` ;
-- `20260828230451_create_gate_attempts` va vouloir droper `gate_attempts`. Rendre ici la table de
-- création plutôt que la table finale les ferait toutes échouer sur une colonne absente.
--
-- La forme rendue est donc celle d'après :
--
--   * `workflows` : v34, plus `builtin_revision` (v40), plus le vocabulaire `scope` élargi à
--     `coder` / `prototyper` (v43), plus `forked_from_revision` (v47) ;
--   * `nodes` : v34, avec le CHECK `type` à ses DIX-SEPT valeurs — v39 (`query`), v41
--     (`fork_spec`), v43 (`sub_workflow`, `transition_task`), `20260818130000`
--     (`create_workspace`), `20260828205640` (`set_category`) ;
--   * `workflow_runs` : v34, plus `traversal` (v39), `workspace_id` (v42), `parent_run_id` (v43) et
--     `pending_consent` (v45) ;
--   * `command_slots` (v46), `human_turns` (v48 + `consumed_by` de v49) et `gate_attempts`
--     (`20260828230451`) tels que leur unique migration les a écrits.
--
-- Les colonnes ajoutées par ALTER sont ici déclarées EN LIGNE, à leur place logique plutôt qu'en
-- queue de table : `sqlite_master` n'en garde que l'ordre, et le relevé de schéma que nyx confronte
-- au registre (`schema_fingerprint`) lit un ENSEMBLE de noms, jamais une position. Rejouer les cinq
-- danses `ADD COLUMN` / `UPDATE` / `DROP COLUMN` d'origine ne reconstituerait donc rien de plus, sur
-- des tables qui viennent de naître vides.
--
-- Les données, elles, ne reviennent pas : le `up.sql` a droppé les tables, et un `down.sql` ne
-- ressuscite pas ce qu'un `DROP TABLE` a emporté. C'est assumé — le moteur est retiré (PDR-0031),
-- ce chemin ne sert qu'à laisser la chaîne se rejouer.

-- --- workflows : un arbre de comportement stocké -----------------------------
-- La référence AVANT sur `nodes(id)` est délibérée et valide : SQLite résout la table parente d'une
-- clé étrangère paresseusement, à l'écriture, pas au `CREATE TABLE`.
CREATE TABLE workflows (
    id                   TEXT    NOT NULL PRIMARY KEY,
    -- NULL = un défaut nyx intégré (les flux ndev / nspec / ndoc seedés).
    project_id           TEXT    REFERENCES projects(id) ON DELETE CASCADE,
    name                 TEXT    NOT NULL,
    -- Vocabulaire élargi par v43 : `coder` et `prototyper` s'ajoutent aux quatre d'origine.
    scope                TEXT    NOT NULL DEFAULT 'custom'
        CHECK (scope IN ('ndev', 'nspec', 'ndoc', 'custom', 'coder', 'prototyper')),
    -- La RACINE de l'arbre, NULL tant qu'aucun nœud n'existe.
    root_node_id         TEXT    REFERENCES nodes(id) ON DELETE SET NULL,
    -- Provenance : le workflow dont celui-ci est un fork, et la révision de built-in au moment du
    -- fork (v47) — un fork survit à son origine, d'où le SET NULL.
    forked_from          TEXT    REFERENCES workflows(id) ON DELETE SET NULL,
    forked_from_revision INTEGER,
    -- La révision du jeu de graphes intégré qui a produit cette ligne (v40).
    builtin_revision     INTEGER,
    created_at           INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
    updated_at           INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);

CREATE INDEX idx_workflows_project ON workflows (project_id);
CREATE INDEX idx_workflows_scope ON workflows (scope);

-- --- nodes : l'arbre PUR (parent_id + order_index) ---------------------------

CREATE TABLE nodes (
    id          TEXT    NOT NULL PRIMARY KEY,
    workflow_id TEXT    NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    -- NULL = la racine du workflow. Auto-référence en CASCADE : droper un composite emporte
    -- tout son sous-arbre.
    parent_id   TEXT    REFERENCES nodes(id) ON DELETE CASCADE,
    order_index INTEGER NOT NULL DEFAULT 0,
    -- Le vocabulaire COMPLET, à ses dix-sept valeurs. `type` est un mot-clé SQL et Rust : entre
    -- guillemets ici, exposé `type_` côté Diesel.
    "type"      TEXT    NOT NULL DEFAULT 'note'
        CHECK ("type" IN (
            'sequence', 'loop', 'selector',
            'spawn_subagent', 'judge', 'ask_human', 'produce',
            'gate_check', 'transition', 'wait', 'note', 'query', 'fork_spec',
            'sub_workflow', 'transition_task', 'create_workspace', 'set_category'
        )),
    label       TEXT    NOT NULL DEFAULT '',
    -- Le config JSON, typé PAR nature de nœud.
    config      TEXT    NOT NULL DEFAULT '{}',
    created_at  INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
    updated_at  INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);

CREATE INDEX idx_nodes_workflow ON nodes (workflow_id);
CREATE INDEX idx_nodes_parent ON nodes (parent_id, order_index);

-- --- workflow_runs : une exécution vivante (curseur + blackboard) -------------

CREATE TABLE workflow_runs (
    id              TEXT    NOT NULL PRIMARY KEY,
    workflow_id     TEXT    NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    -- Le Spec que le run pilote ; le run survit à la suppression du Spec, sans le pointeur.
    spec_id         TEXT    REFERENCES specs(id) ON DELETE SET NULL,
    -- Le workspace où le run travaille (v42) : texte nu, comme l'ALTER l'avait posé.
    workspace_id    TEXT,
    -- Le CURSEUR : le nœud sur lequel le run est garé.
    current_node_id TEXT    REFERENCES nodes(id) ON DELETE SET NULL,
    -- Le sous-run parent (v43) : un `sub_workflow` ouvre un run fils.
    parent_run_id   TEXT    REFERENCES workflow_runs(id) ON DELETE SET NULL,
    blackboard      TEXT    NOT NULL DEFAULT '{}',
    -- Le traversal persisté (v39) : ce qui permet de reprendre un run d'une session à l'autre.
    traversal       TEXT,
    -- La question de consentement en attente (v45), NULL quand le run n'attend personne.
    pending_consent TEXT,
    status          TEXT    NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'blocked', 'done', 'failed')),
    started_at      INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
    updated_at      INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);

CREATE INDEX idx_workflow_runs_workflow ON workflow_runs (workflow_id);
CREATE INDEX idx_workflow_runs_spec ON workflow_runs (spec_id);

-- --- command_slots : « ce slot, chez ce projet, ce sont ces commandes » -------

CREATE TABLE command_slots (
    id          TEXT    NOT NULL PRIMARY KEY,
    project_id  TEXT    NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- Le nom de slot qu'un `gate_check` déclare — CE QUI doit être vérifié, dans les mots de
    -- l'auteur du graphe.
    slot        TEXT    NOT NULL,
    -- Les ids d'INSTANCES de commandes managées, en tableau JSON ORDONNÉ.
    command_ids TEXT    NOT NULL DEFAULT '[]',
    created_at  INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
    updated_at  INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);

CREATE UNIQUE INDEX idx_command_slots_project_slot ON command_slots (project_id, slot);

-- --- human_turns : le dernier tour humain, un par terminal --------------------

CREATE TABLE human_turns (
    terminal_id  TEXT    NOT NULL PRIMARY KEY,
    -- L'IDENTITÉ du tour : un nouveau prompt reçoit une nouvelle valeur, un hook redélivré garde
    -- la même — c'est toute la comparaison.
    turn_id      TEXT    NOT NULL,
    prompt       TEXT    NOT NULL,
    submitted_at INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
    -- La marque de consommation (v49) : le consentement que ce tour a payé.
    consumed_by  TEXT
);

-- --- gate_attempts : le journal de preuve d'un `gate_check` -------------------
-- Pas de clés étrangères : `run_id` / `spec_id` / `node_id` sont des ids de CORRÉLATION, et le
-- journal doit survivre à la disparition de ce qu'il décrit.

CREATE TABLE gate_attempts (
    -- Alias de rowid : l'ordre d'ajout global, strictement croissant. LA clé d'ordre du journal —
    -- jamais l'horloge, qui peut égaliser ou reculer.
    sequence       INTEGER NOT NULL PRIMARY KEY,
    run_id         TEXT    NOT NULL,
    spec_id        TEXT,
    node_id        TEXT    NOT NULL,
    slot           TEXT,
    expect         TEXT    NOT NULL CHECK (expect IN ('green', 'red')),
    declared       INTEGER NOT NULL DEFAULT 0,
    blocking       INTEGER NOT NULL DEFAULT 1,
    outcome        TEXT    NOT NULL CHECK (outcome IN ('passed', 'failed', 'indeterminate')),
    satisfied      INTEGER NOT NULL DEFAULT 0,
    command_line   TEXT    NOT NULL DEFAULT '',
    cwd            TEXT    NOT NULL DEFAULT '',
    exit_code      INTEGER,
    duration_ms    INTEGER NOT NULL DEFAULT 0,
    output_excerpt TEXT    NOT NULL DEFAULT '',
    commands_json  TEXT    NOT NULL DEFAULT '[]',
    created_at     INTEGER NOT NULL DEFAULT (CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
);

CREATE INDEX idx_gate_attempts_spec ON gate_attempts (spec_id, sequence);
CREATE INDEX idx_gate_attempts_run ON gate_attempts (run_id, sequence);
