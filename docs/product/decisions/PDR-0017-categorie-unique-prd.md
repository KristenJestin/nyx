# PDR-0017 - Catégorie unique par Spec (remplace les tags)

**Status: validated**

Supersède la partie « vocabulaire tags » de PDR-0014.

## Context

PDR-0014 avait acté les **tags** (m:n) comme vocabulaire de classification du Spec. À l'usage, un nuage de tags prolifère et brouille la lecture. On a déjà trois axes (`status`, `priority`, `milestone`) ; il ne manquait qu'un seul axe « genre de travail ». Un tag soup va à l'encontre de « Spec = unité humaine, pas de bloat ».

## Decision

- **Catégorie unique par Spec.** `specs.category_id` (nullable = non catégorisé), FK vers une table `categories` par projet (`name`, `color_token`, `position`). Une seule par Spec.
- **Suppression des tags** : tables `tags`, `spec_tags`, `task_tags` supprimées. **Pas de classification sur les tâches** (territoire agent).
- **Libellés transverses droppés** (`breaking`, `security`…) : `priority` + la spec du Spec portent déjà l'info. La catégorie = le **genre** (Feature / Bug / Refactor / Infra / Design…).
- **Gouvernance — grill, non bloquant.** Le set de catégories est **curé par l'humain** ; un **seed par défaut** est posé à la création du projet. L'agent **assigne depuis l'existant** ; s'il ne trouve rien, il **crée la catégorie à la volée (jamais bloquant)**, flaggée proposée (`created_by_type = agent`, `confirmed_at` null) ; l'humain **cure après** (confirme / renomme / fusionne). L'anti-prolifération vient de la curation + de la directive « préfère réutiliser », pas d'un verrou. — *Clause AMENDÉE le 2026-09-02 : voir « Amendement (2026-09-02) — l'axe proposé/confirmé est retiré » plus bas ; le flag « proposée » et le geste « confirme » n'existent plus.*
- La règle de gouvernance vit dans le **playbook/directives** de l'agent (PDR-0011), pas dans le schéma.

## Consequences

- DBML : `categories` (+ `created_by_type`, `confirmed_at`) ; `specs.category_id` ; `tags` / `spec_tags` / `task_tags` supprimées ; TableGroup à jour.
- **Note d'implémentation (2026-08-28)** — ce que la mise en œuvre a ajouté sans rien retrancher à la décision ci-dessus. Une catégorie porte un **`slug`** en plus de son `name` : le `slug` est l'identité **stable et immuable**, unique par projet (l'index unique porte sur `(project_id, slug)`, pas sur `(project_id, name)`), et c'est lui que le harnais agent lit ; le `name` reste le libellé humain, éditable et libre d'être homonyme — sans quoi « renommer » (une des gestes de curation actés ici) changerait la règle à laquelle une Spec est soumise. Corollaire : un slug forgé pour une catégorie mintée par l'agent (« proposée » dans le vocabulaire d'avant l'amendement du 2026-09-02) ne retombe **jamais** sur un des quatre slugs système (`feature`, `bug`, `refactor`, `chore`), qui sont seedés par projet de façon idempotente. Le refus « catégorie d'un autre projet » est prononcé par le service de domaine (`crates/nyx-core/src/category.rs`), la FK SQL ne pouvant pas l'exprimer.
- Board : carte = **une** puce catégorie ; facette « Catégorie » (single-select) ; groupement possible par catégorie.
- Supersède le « vocabulaire tags » de PDR-0014 ; le reste de PDR-0014 (Spec unité humaine, clé, priorité, phases, `executor`) reste valide.

## Amendement (2026-09-02) — l'axe proposé/confirmé est retiré

Décision utilisateur prise en revue humaine du PRD `01KYA6K26JBYETDMDXYGZVVA03`. Elle **amende** la clause « Gouvernance — grill, non bloquant » ci-dessus sans rien retrancher au reste de la décision d'origine, qui reste la référence pour tout ce qu'elle ne touche pas.

- **L'agent a les pleins droits sur la classification.** `spec_set_category` (et la création à la volée qu'il entraîne) **vaut validation** : une catégorie mintée par un agent est valide telle quelle. Il n'y a plus d'état « proposée » à confirmer, plus de geste `confirm`, plus de badge, plus d'événement `category.confirmed`, et le champ dérivé `state` (`proposed` | `confirmed`) disparaît du payload et du type TypeScript.
- **Ce que l'humain cure, c'est le VOCABULAIRE**, pas chaque mint un par un : il **crée**, **renomme** et **supprime** des entrées depuis la section « Categories » du modal de settings du projet. La suppression est **refusée** sur les quatre catégories système (`feature`, `bug`, `refactor`, `chore` — le slug porte la discipline) et sur une catégorie encore portée par au moins une Spec (le refus nomme le compte).
- **`created_by_type` est conservé** (`system` | `agent` | `human`) : la provenance reste un fait utile, distingué en base et dans `domain_events` (`category.created` en source `mcp` pour l'agent, `ui` pour l'humain). C'est l'**état de curation** qui disparaît, pas la traçabilité.
- **Pas de migration.** La colonne `categories.confirmed_at` est conservée et désormais **toujours renseignée** ; plus rien ne la lit. Son retrait (colonne + champ Rust) est noté comme nettoyage futur, à faire lors de la prochaine migration de la table `categories` pour une autre raison.
- Anti-prolifération : elle repose maintenant sur la directive « préfère réutiliser » **et** sur la curation du vocabulaire depuis les settings — jamais sur un verrou à la création.
