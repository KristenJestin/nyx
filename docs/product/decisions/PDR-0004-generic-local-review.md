# PDR-0004 - Review locale générique

**Status: validated direction**

## Context

Le PRD nyx v2 et le système de review Depot couvrent tous deux gros diffs multi-repo, commentaires ancrés, threads humain/agent et résolution humaine. Le modèle Depot reste trop lié aux révisions de PRD pour servir tout le travail terminal.

## Decision

nyx possède un moteur de review locale générique. Une review peut être liée à un PRD, mais ce lien est facultatif.

Le moteur doit couvrir working tree, commits/ranges/branches, multi-repo, commentaires globaux ou ancrés, réponses agent, ancres remappées/outdated et résolution humaine.

## Consequences

- ne pas réimplémenter deux systèmes de commentaires ;
- extraire les invariants utiles de Depot sans conserver son couplage obligatoire ;
- la review appartient au produit nyx et partage le temps réel UI/MCP ;
- l'utilisateur reste seul propriétaire de la résolution.
