# PDR-0018 - Renommage : PRD → Spec

**Status: validated**

Supersède le choix terminologique « PRD » de PDR-0014 (Q-005 reste fermée, terme = Spec).

## Context

PDR-0014 avait gardé « PRD » comme nom de l'unité humaine. À l'usage, le terme est du jargon « document-lourd » et rend les petits changements artificiellement pesants. « Spec » est plus léger, scale du petit au gros, et reste juste (l'unité contient bien une spec : problème / scope / solution / critères).

## Decision

- **L'unité humaine s'appelle « Spec »**, partout : UI, docs de vérité courante, et **schéma** (tables `specs`, `spec_revisions`, `spec_annexes` ; colonnes `spec_id`, `spec_status`, `specs.workspace_id`, `specs.category_id`, `promoted_spec_id`…).
- Clé Linear, slug, priorité, catégorie, `executor`, phases : **inchangés** (seul le nom change).
- **PDR antérieurs** : **réécrits en « Spec »** pour cohérence stricte. Seul ce PDR garde « PRD » pour documenter le renommage.
- Le proto est renommé séparément (prompt fourni).

## Consequences

- Docs, DBML, glossaire, registre **et PDR** passés à « Spec » ; seul ce PDR garde « PRD » (record du renommage).
- Q-005 reste fermée (terme = Spec).
- Aucun changement de modèle au-delà du nom.
