-- Le MOTEUR D'ORCHESTRATION quitte le schéma : `workflows`, `nodes`, `workflow_runs`,
-- `command_slots`, `human_turns` et `gate_attempts` disparaissent, avec leurs index.
--
-- (PDR-0031 — « Retrait du moteur d'orchestration : des manuels en prose, pas un behavior tree ».)
--
-- ## Pourquoi ces six tables partent ensemble
--
-- Elles ne sont pas six domaines : elles sont UNE seule idée écrite en six morceaux — le parcours de
-- dev tenu en DONNÉE, sous forme d'arbre de comportement interprété par `Engine::tick`. `workflows`
-- porte l'arbre, `nodes` ses nœuds typés, `workflow_runs` le curseur et le blackboard d'une
-- exécution, `command_slots` la reliure d'un gate à des commandes managées, `human_turns` le tour
-- humain qu'un `ask_human` attendait, `gate_attempts` le journal de preuve d'un `gate_check`.
-- Aucune ne survit à la disparition de l'interpréteur : sans `Engine`, il n'y a plus personne pour
-- écrire une ligne dans l'une d'elles, ni pour en lire une.
--
-- Le PDR dit pourquoi l'interpréteur part, et ce n'est pas de la dette : **le graphe décidait à la
-- place de l'agent, et il décidait moins bien**. Ce que nyx garde, c'est l'état et les verbes ; le
-- raisonnement retourne au modèle, guidé par un manuel en prose injecté par hook. Il n'y a donc plus
-- de chemin à persister, et une table qui ne décrit plus rien n'est pas une archive : c'est un piège
-- pour la prochaine lecture de schéma.
--
-- ## L'ordre des DROP n'est pas décoratif
--
-- `PRAGMA foreign_keys` est ON sur toute connexion du pool, et un `DROP TABLE` sous ce régime exécute
-- un `DELETE FROM` implicite qui DÉCLENCHE les actions référentielles. Les enfants tombent donc
-- avant les parents : `workflow_runs` (qui pointe `workflows`, `nodes` et `specs`) avant `nodes`,
-- puis `nodes` — dont la chute pose `workflows.root_node_id` à NULL par son `ON DELETE SET NULL` —
-- puis `workflows` en dernier. Inversé, l'ordre laisserait SQLite refuser la suppression d'un parent
-- encore référencé, et la migration échouerait au boot du premier utilisateur.
--
-- Les index sont retirés explicitement avant leur table. SQLite les emporterait de toute façon avec
-- elle ; les nommer ici rend la migration lisible sans aller relire les six `up.sql` d'origine, et
-- symétrique du `down.sql` qui doit tous les reconstruire.
--
-- ## Ce que cette migration NE touche PAS
--
--   * `specs.mode` (`stepped` / `autopilot`) RESTE en base, comme le PDR le prescrit. Plus rien ne
--     la lit ni ne l'expose — l'arrêt de fin de phase venait du moteur, il vient maintenant du
--     manuel et du gate humain final. Elle partira à un prochain nettoyage de schéma, pas ici : une
--     recréation de `specs` pour une colonne morte coûterait un rebuild de table pour zéro effet.
--   * `projects.workflow_id` n'existe pas et n'a jamais existé dans cette lignée de migrations. Le
--     rattachement projet → workflow évoqué par PDR-0020 est resté à l'état d'intention ; il n'y a
--     donc aucune colonne à retirer de `projects`, et aucune recréation de cette table.

-- --- gate_attempts : le journal de preuve d'un `gate_check` ------------------
-- Aucune clé étrangère (ses `run_id` / `spec_id` / `node_id` sont des ids de corrélation), donc
-- rien à ordonner : elle tombe la première.
DROP INDEX IF EXISTS idx_gate_attempts_run;
DROP INDEX IF EXISTS idx_gate_attempts_spec;
DROP TABLE IF EXISTS gate_attempts;

-- --- human_turns : le dernier tour humain par terminal -----------------------
-- Table isolée elle aussi : sa clé primaire est le terminal, sans référence déclarée.
DROP TABLE IF EXISTS human_turns;

-- --- command_slots : la reliure « slot déclaré » → commandes managées --------
DROP INDEX IF EXISTS idx_command_slots_project_slot;
DROP TABLE IF EXISTS command_slots;

-- --- workflow_runs : le curseur et le blackboard d'une exécution -------------
-- Enfant de `workflows`, `nodes` et `specs`, et de lui-même par `parent_run_id` : il part avant
-- les tables qu'il référence.
DROP INDEX IF EXISTS idx_workflow_runs_spec;
DROP INDEX IF EXISTS idx_workflow_runs_workflow;
DROP TABLE IF EXISTS workflow_runs;

-- --- nodes : l'arbre lui-même ------------------------------------------------
-- Sa disparition pose `workflows.root_node_id` à NULL (ON DELETE SET NULL) — c'est précisément
-- pourquoi `workflows` doit encore exister à cet instant.
DROP INDEX IF EXISTS idx_nodes_parent;
DROP INDEX IF EXISTS idx_nodes_workflow;
DROP TABLE IF EXISTS nodes;

-- --- workflows : la racine du domaine ---------------------------------------
DROP INDEX IF EXISTS idx_workflows_scope;
DROP INDEX IF EXISTS idx_workflows_project;
DROP TABLE IF EXISTS workflows;
