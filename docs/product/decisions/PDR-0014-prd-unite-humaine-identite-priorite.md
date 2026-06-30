# PDR-0014 - Spec unité humaine ; identité, priorité et phases des work items

**Status: validated**

Ferme Q-005 (terminologie) et tranche l'identité, la priorité et les phases des work items.

## Context

L'alignement du prototype a fait remonter **deux modèles de « tâche » divergents** (un board générique façon Linear, et un modèle Spec fidèle au DBML) et plusieurs questions de données non tranchées : quelle est l'unité de travail de l'humain, qui porte une clé lisible, la priorité est-elle stockée et où, comment se modélisent les phases.

Le principe directeur, formulé par l'utilisateur : **l'humain travaille au niveau du Spec ; la tâche est une affaire d'agent.** L'humain conçoit et valide le Spec ; l'agent crée et découpe les tâches (en phases si besoin). L'humain ne fait pas de gestion de tâches à la main.

## Decision

- **Spec = unité de travail de l'humain.** L'interface entre l'humain et la feature est le Spec. L'humain le valide ; l'agent décompose en tâches et, si besoin, en phases. L'UI montre les tâches mais n'offre **pas** de CRUD de tâches façon gestionnaire de projet.
- **Le terme est « Spec ».** Alternatives `Work Item` / `Change` écartées. Le travail léger sans Spec reste possible (workspace sans Spec, W-001 ; une tâche peut exister sans Spec).
- **Clé Linear sur le Spec.** Le Spec porte une clé lisible = **préfixe projet + numéro incrémental** (ex. `NYX-42`). C'est la référence humaine. Le **slug** descriptif est conservé pour l'URL/lisibilité (`NYX-42/workspace-pivot`). Le projet gagne un `key` (préfixe) ; le Spec gagne un `number` monotone par projet.
- **Les tâches n'ont pas de clé humaine.** Identité ULID interne, gérée par l'agent.
- **Priorité = enum string `urgent / high / med / low`**, **stockée** sur le **Spec** et sur la **tâche**. Remplace l'`integer` des tâches et réintroduit une priorité au niveau Spec (présente dans Depot, retirée à tort du DBML v2 comme « agrégat dérivé »).
- **Phases = champ `phase` nullable sur la tâche**, optionnel, posé par l'agent. Pas de table de phases, pas de convention par plage de positions. Pas de phase = tâches à plat.
- **`executor` sur la tâche (`agent` | `human`).** Une action humaine = une task `executor = human` (l'agent la flague, elle remonte dans l'**Attention Inbox**, l'humain ne la gère pas dans le board ; peut bloquer une task agent via `task_dependencies`). La table `human_actions` et l'enum `human_action_status` sont **supprimés** ; les trois saveurs (agent / human / finding) convergent dans `tasks`.
- **Board Work = board de Spec** (pas de tâches). Colonnes = `spec_status`. Classification = **catégorie unique** par Spec (remplace les tags ; voir **PDR-0017**) ; plus de `labels` ; « sprint » et « issue » bannis (vocabulaire PM générique, cf. Q-012).

## Consequences

- Q-005 fermée côté terminologie : l'unité humaine est le Spec, keyé Linear ; les alternatives de renommage sont écartées.
- `data-model.dbml` : nouvel `Enum priority` ; `projects.key` ; `specs.number` (+ unique `(project_id, number)`) et `specs.priority` ; `tasks.priority` passe `integer → priority` et gagne `phase` nullable ; la note « priorité = agrégat » disparaît.
- Le prototype fusionne ses deux modèles de tâche en un seul ; le board devient Spec-centré ; `labels → catégorie unique` (PDR-0017) ; en-tête sans « sprint »/« issue ».
- Les tâches restent le territoire de l'agent : observées dans le Spec, jamais administrées à la main par l'humain.
- Cohérent avec `agent-harness.md` (l'orchestrateur découpe en phases, le coder ne travaille que sur des tasks) et avec PDR-0011/PDR-0012.
