# PDR-0017 - Catégorie unique par Spec (remplace les tags)

**Status: validated**

Supersède la partie « vocabulaire tags » de PDR-0014.

## Context

PDR-0014 avait acté les **tags** (m:n) comme vocabulaire de classification du Spec. À l'usage, un nuage de tags prolifère et brouille la lecture. On a déjà trois axes (`status`, `priority`, `milestone`) ; il ne manquait qu'un seul axe « genre de travail ». Un tag soup va à l'encontre de « Spec = unité humaine, pas de bloat ».

## Decision

- **Catégorie unique par Spec.** `specs.category_id` (nullable = non catégorisé), FK vers une table `categories` par projet (`name`, `color_token`, `position`). Une seule par Spec.
- **Suppression des tags** : tables `tags`, `spec_tags`, `task_tags` supprimées. **Pas de classification sur les tâches** (territoire agent).
- **Libellés transverses droppés** (`breaking`, `security`…) : `priority` + la spec du Spec portent déjà l'info. La catégorie = le **genre** (Feature / Bug / Refactor / Infra / Design…).
- **Gouvernance — grill, non bloquant.** Le set de catégories est **curé par l'humain** ; un **seed par défaut** est posé à la création du projet. L'agent **assigne depuis l'existant** ; s'il ne trouve rien, il **crée la catégorie à la volée (jamais bloquant)**, flaggée proposée (`created_by_type = agent`, `confirmed_at` null) ; l'humain **cure après** (confirme / renomme / fusionne). L'anti-prolifération vient de la curation + de la directive « préfère réutiliser », pas d'un verrou.
- La règle de gouvernance vit dans le **playbook/directives** de l'agent (PDR-0011), pas dans le schéma.

## Consequences

- DBML : `categories` (+ `created_by_type`, `confirmed_at`) ; `specs.category_id` ; `tags` / `spec_tags` / `task_tags` supprimées ; TableGroup à jour.
- Board : carte = **une** puce catégorie ; facette « Catégorie » (single-select) ; groupement possible par catégorie.
- Supersède le « vocabulaire tags » de PDR-0014 ; le reste de PDR-0014 (Spec unité humaine, clé, priorité, phases, `executor`) reste valide.
