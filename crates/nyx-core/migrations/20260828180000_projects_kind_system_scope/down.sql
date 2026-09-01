-- Revert `projects_kind_system_scope` (20260828180000) : retirer la nature du conteneur.
--
-- L'index partiel se drope AVANT la colonne qu'il indexe (SQLite refuse de laisser un index orphelin
-- derrière un `DROP COLUMN` — même recette que le down de `20260819140000_create_command_groups`).
-- Le CHECK est une contrainte de COLONNE : il disparaît avec elle, rien d'autre à défaire.
--
-- Redescendre efface la NATURE, pas les lignes : un projet Système bootstrapé redevient un projet
-- ordinaire portant ses terminaux et ses commandes. C'est volontairement non destructif — le down
-- d'une migration ne doit jamais perdre le travail de l'utilisateur — mais cela signifie qu'un
-- re-`up` retrouve une base où le singleton n'est plus marqué : `ensure_system_scope` recrée alors
-- un projet Système neuf et l'ancien reste visible comme projet utilisateur, à la main de l'humain.

DROP INDEX idx_projects_single_system;
ALTER TABLE projects DROP COLUMN kind;
