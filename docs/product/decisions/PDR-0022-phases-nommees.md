# PDR-0022 - Phases nommées (liste ordonnée sur la spec, sans table ni statut stocké)

**Status: validated**

Précise PDR-0014 (le champ `phase` de la task).

## Context

PDR-0014 a posé `tasks.phase` (entier nullable) **sans nom** — donc des phases « numérotées ». À l'usage, **nommer** les phases (« Schema & migration », « Unified runtime »…) est plus lisible dans l'onglet Tasks.

## Decision

- Les phases gagnent un **nom**, stocké comme une **liste ordonnée portée par la spec** : `specs.phases` (json) = `[{ number, name }]`. La task référence sa phase par `tasks.phase` (le `number`, qui indexe la liste).
- **Pas de table de phases** (on respecte PDR-0014) ni d'entité phase : juste une liste nommée sur la spec, **gérée par l'agent**.
- **Pas de statut de phase stocké** : il reste **dérivé** des statuts des tasks de la phase (`done` si toutes done ; `in_progress` si une `in_progress`/`in_review` ; `blocked` si une `blocked` ; sinon `todo`).
- Pas de phase = tasks à plat (inchangé).

## Consequences

- `data-model.dbml` : `specs` gagne `phases` (json) ; `tasks.phase` inchangé (indexe la liste).
- Le proto nommait déjà les phases côté UI ; c'est désormais **backé par le modèle**.
- Refine PDR-0014 (phase = entier + nom via liste spec) ; le **statut de phase reste dérivé**.
