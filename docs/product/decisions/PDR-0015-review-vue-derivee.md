# PDR-0015 - Review = vue dérivée ; suppression de la table reviews

**Status: validated**

Supersède la partie « `review` comme entité » de PDR-0012 (les concepts `comment` et `task` sont conservés). Prolonge PDR-0013 (dériver de Git plutôt que persister).

## Context

PDR-0012 avait réduit le domaine review à trois concepts dont `review` = une **entité stockée par repo** (living/pinned, statut, archivée au merge). En revisitant le modèle pour la page Review, cette entité s'est révélée ne porter presque rien d'irréductible :

- son périmètre de diff (`base_ref` / `target_ref` / `pinned_sha`) est **relisible depuis Git** (base = point de fork, target = worktree/HEAD) — même argument que PDR-0013 ;
- son **statut** est redondant : la review « vit avec le Spec » et suit le statut du Spec ;
- les **snapshots `pinned`** n'ont pas démontré de gain ;
- « exister sans commentaire » n'a aucune valeur.

Pire : grouper les commentaires sous des lignes `reviews` (living + pinned + closed) **fragmente** la lecture des commentaires passés, alors que le besoin réel est simplement « voir tous les commentaires du repo ».

## Decision

- **Suppression de la table `reviews`** (et des enums `review_status`, `review_variety`). Une « review » est une **vue dérivée** : le diff d'un repo relu depuis Git + les commentaires ancrés dessus.
- **`review_comments` s'ancre directement sur `(workspace_id, repository_id)`** (+ `project_id`), avec un **`spec_id` nullable** : renseigné = review qui suit le Spec ; **null = review générique** (workspace sans Spec, conserve « Spec facultatif » de PDR-0004/PDR-0012).
- **Pas de statut de review** (il suit le Spec), **pas de snapshot `pinned`** (réintroductible plus tard si un usage le prouve — règle « absorber tard »).
- `tasks.review_id` est **supprimé** : une task est un **finding** si elle est promue depuis un commentaire (`review_comment_tasks`). Le coder ne travaille que sur des tasks.
- Conservé de PDR-0012 : `comment` (ancré/global, threadé, auteur humain/agent, **résolution humain-seul**, remap/`outdated` via `base_blob_oid` + contexte) et `task` / `finding = task`.

## Consequences

- Une table de moins ; cohérent avec PDR-0013 (ne pas persister ce que Git sait).
- « Voir les commentaires d'avant » = une requête sur `(workspace, repository)` filtrée par `status` / `outdated`, sans fragmentation.
- Le lien durable vers le Spec passe par le **finding** (commentaire promu en task), pas par une ligne review.
- DBML : `reviews` supprimée ; `review_comments` gagne `project_id` / `workspace_id` / `repository_id` / `spec_id` et perd `review_id` ; `tasks` perd `review_id` ; enums `review_status` / `review_variety` supprimés.
- PDR-0012 : sa partie « entité review » est superseded ; ses concepts `comment` et `task` restent canoniques.
