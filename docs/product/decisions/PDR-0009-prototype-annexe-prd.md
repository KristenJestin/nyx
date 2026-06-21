# PDR-0009 - Prototype : annexe du PRD, conservé, absorbé tard

**Status: validated**

## Context

Depot possède un sous-système de prototypage (phase conception) d'environ 8 tables : prototypes, pages, versions de page, variants (HTML auto-contenu stocké en base), feedback piné au sélecteur CSS, rounds, et élection de variante. Il sert à valider un rendu visuel avant le développement : l'agent génère plusieurs variantes d'une page, l'utilisateur itère par rounds et commentaires pinés, élit une variante, puis la fait distiller en tâches.

Il fallait décider, pour la super-app nyx (Q-009), son placement, son timing et sa forme. Un premier passage a tenté de trancher la forme en lisant le schéma : c'était une erreur. Un schéma dit ce qui a été construit, jamais ce qui est utilisé.

## Decision

**Placement.** Le prototype est une **annexe du PRD**, au scope projet. Aucun workspace, aucun runtime : c'est de la donnée (HTML) rendue dans une iframe. Il ne touche pas le pivot workspace.

**Timing.** Pas en v1. C'est de la phase conception ; il s'absorbe tard (roadmap v2.6). La v1 reste le runtime.

**Forme, décidée par l'usage réel (pas par le schéma) :**

- **conservé** : pages, versions, variants, states, commentaires pinés et leur résolution, élection. Tout cela est utilisé.
- **rounds conservés** comme **mécanisme de rollback** : quand l'agent dégrade le design ou sort des variantes mauvaises, l'utilisateur revient à un round antérieur meilleur et rebase dessus. Ce n'est pas de l'historique write-only.
- **un seul prototype par PRD** : le multi-prototype n'est jamais utilisé. Le prototype devient une annexe 1:1 du PRD, pas une entité multi-lignes.

**Pont conception -> dev.** L'élection d'une variante est le seul lien vers le développement : la variante élue se distille en tâches (cf. `task_prototype_pages` de Depot). Ce pont passe par le PRD, jamais par un workspace.

**Le « fouilli » est un problème de contrat d'outil, pas de schéma.** L'agent fumble parce que l'outil agent est sous-contraint, pas parce que le modèle est mauvais. Le système de states que l'utilisateur a construit (états explicites pour qu'aucune action ou navigation de l'agent ne passe inaperçue) est déjà la mécanique de lisibilité côté design. Le contrat d'outil se redessine au moment de l'intégration, pas maintenant.

## Consequences

- le prototype entre comme annexe du PRD, jamais comme concept lié au workspace ou au runtime ;
- le modèle reste runtime-first ; la donnée de conception se pose sur le PRD ;
- le schéma détaillé des tables de prototypage n'est pas figé ici : il est spécifié à l'absorption (v2.6), par revue d'usage, pas par lecture de schéma ;
- méthode générale pour Q-009 : la forme d'absorption de chaque sous-système Depot se décide par revue d'usage avec l'utilisateur, jamais par un agent qui lit le schéma et tranche seul.
