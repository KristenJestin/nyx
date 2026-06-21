# Workflows produit

## État

**Status: proposed - used to validate the domain model**

## W-001 - Travail libre sans PRD

1. L'utilisateur ouvre un projet/workspace.
2. Il ouvre un terminal ou reprend une session agent.
3. Le travail se fait dans le workspace courant, sans PRD ni entité intermédiaire.
4. L'agent modifie le code et lance des services.
5. nyx expose les changements Git et permet une review locale.

Ce workflow protège le caractère terminal-first : un PRD ne doit jamais être obligatoire pour utiliser nyx.

## W-002 - Réaliser un PRD

1. L'utilisateur sélectionne un PRD prêt.
2. nyx crée le worktree de réalisation et pose `prds.workspace_id` vers lui.
3. nyx associe branche et contexte agent au workspace.
4. L'agent reçoit le contexte exact via MCP.
5. Terminaux, services et sessions apparaissent sur le workspace.
6. Les tâches progressent et l'UI se met à jour en temps réel.
7. L'agent demande une review.
8. L'utilisateur review puis renvoie les commentaires à l'agent.
9. L'utilisateur seul résout et valide.

## W-003 - Review locale générique

1. L'utilisateur ouvre les changements d'un workspace ou d'une branche.
2. nyx calcule un diff multi-repo incluant commits et working tree.
3. Il ajoute des commentaires globaux, fichiers, lignes ou plages.
4. L'agent lit les threads et répond via MCP.
5. Le code évolue ; les ancres sont remappées ou marquées outdated.
6. L'utilisateur résout ou rouvre chaque thread.

Le PRD est facultatif.

## W-004 - Reprise après redémarrage

1. nyx restaure projets, workspaces, terminaux et états utiles.
2. Les sessions agents exactes sont proposées ou reprises selon la politique projet.
3. Les workspaces actifs retrouvent leurs liens de contexte (PRD, branche, sessions).
4. Les services sont relancés uniquement selon les options explicites et snapshots.

## W-005 - Attention humaine

1. Un agent pose une question, un service échoue ou une review devient disponible.
2. Le backend émet un événement métier.
3. L'UI actualise immédiatement l'inbox et le projet concerné.
4. L'utilisateur ouvre directement le contexte et prend la décision.
5. L'événement d'attention disparaît lorsque sa condition factuelle est satisfaite.

## W-006 - Projet multi-repo

1. Un workspace contient plusieurs repositories ou références de repositories.
2. Chaque repo conserve branche, base de diff et historique propres.
3. Le workspace les agrège sans mélanger leurs identités Git.
4. Les commandes s'exécutent dans le bon sous-dossier.
5. La review permet de naviguer par repository.

## Invariants de workflow proposés

- Aucun workflow ne doit exiger l'ouverture de Depot.
- Une mutation MCP doit être visible dans l'UI sans redémarrage.
- Une mutation UI doit être immédiatement lisible par MCP.
- Les agents ne résolvent pas les commentaires humains.
- Fermer nyx ne doit jamais laisser silencieusement des processus non maîtrisés.
- Le terminal libre reste disponible sans configuration produit préalable.
