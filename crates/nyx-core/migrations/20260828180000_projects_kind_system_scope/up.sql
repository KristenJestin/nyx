-- `projects.kind` — la NATURE du conteneur, et l'unique source de vérité du scope **Système**
-- (PRD « Scope Système : terminaux & commandes hors-projet (singleton) »).
--
-- ## Le problème que cette colonne résout
--
-- nyx ne sait tenir des terminaux et des commandes QUE sous un projet (`managed_commands.project_id`
-- est NOT NULL, un terminal se rattache à un `workspace_id`). Les lanceurs transverses de
-- l'utilisateur — services, docker, scripts machine — n'appartiennent à aucun projet de code et
-- vivaient donc hors de nyx. Il leur faut un conteneur POSITIF.
--
-- La seule « absence de projet » qui existe aujourd'hui est SUBIE, pas voulue : `db::delete_project`
-- détache délibérément les terminaux (`ON DELETE SET NULL` — « interactive terminals are the
-- deliberate exception: they are detached, never closed »). Ce fourre-tout `workspace_id IS NULL`
-- reste, et le scope Système ne doit NI s'y confondre, NI le capturer. D'où une appartenance par
-- rattachement RÉEL à un projet singleton, jamais par l'absence de projet.
--
-- ## Pourquoi `projects.kind` et RIEN sur `workspaces`
--
-- Une seule colonne porte la nature. La nature d'un workspace est DÉRIVÉE par jointure sur son
-- projet (`workspaces.project_id -> projects.kind`), jamais dupliquée : deux colonnes seraient deux
-- vérités à resynchroniser, et un workspace « system » sous un projet `user` (ou l'inverse) serait
-- représentable. Ici il ne l'est pas.
--
-- Le vocabulaire est CLOS en SQL (`user` | `system`), comme `layout_shape` ou `nodes.type` : un
-- troisième mot ne peut pas atteindre le backend. DEFAULT 'user' → toute ligne existante garde
-- exactement la nature qu'elle avait implicitement, et une insertion qui ignore la colonne (le
-- `NewProject` du chemin utilisateur) reste un projet utilisateur.
--
-- ## L'index partiel EST le singleton
--
-- `idx_projects_single_system` est un index UNIQUE PARTIEL sur une expression constante restreinte
-- à `kind = 'system'` : il n'admet qu'UNE SEULE ligne système dans toute la base, et il l'admet au
-- niveau SQL, pas au niveau applicatif. C'est ce qui rend le bootstrap concurrent sûr — deux
-- ouvertures simultanées de la base ne peuvent pas créer deux projets Système ; la perdante prend
-- une violation d'unicité et relit la ligne gagnante. Le partiel laisse évidemment autant de projets
-- `user` que l'on veut (ils ne sont pas dans l'index).
--
-- ## Pourquoi le préfixe est DATÉ
--
-- La lignée `000000000000NN` est FIGÉE à `00000000000049` : deux worktrees frères qui frappent
-- chacun « le prochain » numéro contre le même `%APPDATA%/nyx-v2/nyx.db` collisionnent en silence.
-- La règle est dans `docs/product/technical-architecture.md` et tenue par
-- `db.rs::every_migration_added_after_the_frozen_lineage_is_dated_not_numbered`.

ALTER TABLE projects
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'user'
    CHECK (kind IN ('user', 'system'));

CREATE UNIQUE INDEX idx_projects_single_system ON projects(kind) WHERE kind = 'system';
