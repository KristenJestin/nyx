# PDR-0024 - Stockage : un seul fichier SQLite

**Status: validated**

Complète PDR-0023 (ferme Q-004).

## Context

PDR-0023 a fixé le moteur (SQLite-compatible, statiquement lié au sidecar) mais a laissé **Q-004** ouverte : **un seul fichier SQLite** pour toute l'autorité, ou **plusieurs bases internes** par bounded context (structure / work / runtime / review / knowledge / events) ?

L'utilisateur doit voir une application et une autorité uniques ; cela n'impose pas mécaniquement un fichier physique unique, d'où la question.

## Decision

**Un seul fichier SQLite** porte toute l'autorité métier de nyx (un par installation, local-first).

Raisons :

- **Transactions atomiques cross-contexte** : les invariants traversent les contextes (promouvoir une idée en Spec, un commentaire de review en task, émettre un `domain_event` dans la même transaction que la mutation). Plusieurs fichiers interdiraient ces transactions atomiques sans complexité d'`ATTACH` / commit à deux phases.
- **Une seule autorité, un seul writer** : nyx est mono-writer local ; SQLite en WAL gère parfaitement ce profil. Plusieurs fichiers résoudraient un problème de concurrence/sync qu'on n'a pas.
- **Backup / migration / intégrité triviaux** : un fichier = un backup restaurable, une chaîne de migrations, un point de cohérence.
- Le seul bénéfice réel de fichiers séparés (lifecycle / **sync** indépendants par contexte) correspond au **handoff multi-machine, différé** (PDR-0016) — pas un besoin V2.

Les bounded contexts restent une **séparation logique** (modules / schéma / préfixes de tables), pas physique.

## Consequences

- `data-model.dbml` est porté par une base unique ; aucune table ne change.
- **Réversible** : si la sync (handoff) devient un besoin, on pourra scinder un contexte vers libsql/Turso (PDR-0023) — décision rouvrable, pas un cul-de-sac.
- Ferme **Q-004**. La stratégie de migrations V2 (chaîne unique de migrations sur ce fichier, reprise des données v1) est cadrée dans la tranche v2.1.
