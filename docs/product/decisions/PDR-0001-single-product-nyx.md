# PDR-0001 - Un seul produit nommé nyx

**Status: validated**

## Context

nyx gère le runtime terminal, les workspaces, services, sessions et MCP. Depot gère les PRD, tâches, reviews, idées, ADR et contextes agents. Les deux produits ont commencé à dupliquer projects/workspaces, serveur local, MCP, SQLite et review.

## Decision

Le futur produit reste nommé **nyx**. Il absorbe les capacités utiles de Depot dans un modèle produit redessiné. Depot disparaît à terme comme marque, application, binaire, daemon, serveur, UI et intégration MCP distincte.

Il ne s'agit pas d'afficher Depot dans nyx ni de conserver deux autorités synchronisées.

## Consequences

- nyx porte la boucle intention -> exécution -> review ;
- projects/workspaces doivent avoir une seule autorité ;
- le domaine Depot doit être trié, adapté et migré, pas copié aveuglément ;
- une migration de données vérifiable sera nécessaire ;
- le retrait de Depot arrive en dernier, après dogfood.

## Rejected alternatives

- Deux applications intégrées par API : conserve les doublons et la synchronisation.
- Un onglet Depot embarqué : fusion visuelle sans fusion produit.
- Conserver les deux MCP : expose la frontière technique à l'agent et à l'utilisateur.
