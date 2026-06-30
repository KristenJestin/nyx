# PDR-0013 - Multi-repo : Repository logique, pas de table de checkout

**Status: validated**

Ferme Q-003.

## Context

Un projet peut contenir plusieurs repos (ex : `api`, `front`, `libs`) sous un shell root, ou un seul repo à la racine (mono-repo). Il fallait trancher le modèle Repository (Q-003) : entité persistée, détection dynamique ou config ; et la place du chemin et de l'état Git.

L'analyse a montré que le chemin d'un repo est **constant** dans tout le projet (un worktree copie la structure du primary), et que l'appartenance d'un repo à un workspace est **dérivable**.

## Decision

- **`repositories` est une entité persistée** : l'identité logique d'un repo dans le projet (ex : `api`). Nécessaire pour que « le repo `api` » soit le **même objet** dans le primary workspace et dans chaque worktree de dev — sinon review et historique ne suivent pas d'un workspace à l'autre.
- **Le chemin vit sur `repositories`** (`relative_path`, chemin conventionnel ; `"."` en mono-repo), pas par workspace : il est constant dans tout le projet.
- **`optional` sur `repositories`** : un repo `optional` n'est pas worktreé par défaut au `start`. C'est le « nyxignore », **en DB**, structuré et par repo — pas de fichier physique. Règle aussi les « repos optionnels » de Q-003.
- **La table `workspace_repositories` est supprimée.** Une fois le chemin sur `repositories` et l'état Git relu depuis Git, il ne restait que l'appartenance, qui est dérivable : **les repos d'un workspace = les `repositories` non-`optional` du projet**, au chemin `workspace.root_path + relative_path`. Un workspace a toujours la même structure que le primary (pas de set custom par worktree).
- **Mono-repo = même modèle** : 1 `repository` à `relative_path = "."`. Côté UI, quand il n'y a qu'un repo, la couche repo est **masquée** : c'est « juste un workspace ». Schéma uniforme, UI qui s'adapte.
- **`reviews` se rattache à `(workspace_id, repository_id)`** au lieu d'un `workspace_repository_id`.
- **`managed_commands` gagne un `repository_id` optionnel** : une commande peut cibler un repo (chemin résolu par workspace, robuste aux worktrees) au lieu d'un `subfolder` littéral.

## Consequences

- Q-003 fermée : Repository persisté, chemin et `optional` sur lui, pas de table de checkout ;
- une table de moins (`workspace_repositories`) ; l'appartenance et l'état Git ne sont jamais autoritaires en DB ;
- multi-repo et mono-repo partagent un modèle unique, la distinction est purement UI ;
- les commandes managées peuvent suivre un repo où qu'il soit dans le workspace.
