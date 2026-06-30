# PDR-0016 - Dispositions d'absorption de Depot (lot 1)

**Status: validated**

Avance Q-009 (premier lot de dispositions). La matrice complète vit dans [`absorption.md`](../absorption.md).

## Context

Q-009 a fixé la **méthode** : la forme d'absorption se décide par **revue d'usage avec l'utilisateur**, sous-système par sous-système, jamais par un agent qui lit le schéma et tranche seul. Ce PDR est la première passe de cette revue sur les sous-systèmes Depot encore non tranchés. Quatre verbes : **porter / simplifier / différer / supprimer**.

## Decision

- **`pending_actions` → supprimer.** Le besoin (action en attente, gate de validation) est déjà couvert : `human_actions` (to-do humain), **Attention Inbox** (signal), verbes/managed commands (commande à lancer), et surtout la **résolution de commentaire de review** (le gate humain, humain-seul, PDR-0012/0015). Pas de file séparée.
- **Handoff / sync cross-machine → différer.** Tranche tardive, hors périmètre v2.0–v2.7. nyx reste local-first mono-machine ; seul l'export/import de **migration** (`import_runs`) est conservé. À rouvrir si un usage réel l'exige.
- **`activity_log` → simplifier.** Capture **généreuse** d'événements **métier** via `domain_events` ; jamais le runtime octet par octet. Familles canoniques de `event_type` : Spec / Task / Review / Runtime / Knowledge / Human / Prototype / Migration (détail dans `absorption.md`), raffinable.
- **`user_stories` → supprimer.** Vivent dans le corps du Spec (section de spec).
- **`out_of_scope_items` → supprimer.** Vivent dans le corps du Spec (section « hors périmètre »).
- **Annexe vs attachment.** **Annexe** = le concept produit (artefact nommé d'un Spec : prototype, échantillon, doc, image). « attachment » **disparaît du vocabulaire produit** et redevient du **stockage interne** (blob dédupliqué sha256), réutilisé sous le capot par les annexes et les images de commentaires de review. DBML : `spec_attachments` → `spec_annexes` (`name`, `kind` ; source = `body` inline **xor** `attachment_id` blob interne **xor** `path` référence fichier vivant, résolu via `repository_id`).
- **`depot install` / slash-commands → supprimer.** Remplacé par le **MCP natif** + le **plugin Claude Code** (hook SessionStart) de nyx.
- **`project_config` (key-value libre) → supprimer comme concept.** Réglages typés sur le projet au besoin, pas une feature.

## Consequences

- Q-009 avance : lot 1 tranché ; le détail des sous-systèmes « porter » reste à scoper par tranche v2.x.
- DBML : `spec_attachments` → `spec_annexes` ; `attachments` annoté « stockage interne » ; `domain_events` annoté avec les familles `event_type`. Aucune table à ajouter pour les éléments supprimés (absents du modèle v2).
- Inventaire complet et dispositions des deux sources : `absorption.md`.
