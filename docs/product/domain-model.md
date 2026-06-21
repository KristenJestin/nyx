# Modèle de domaine

## État

**Status: proposed, except explicit validated directions**

Ce document décrit le modèle cible à valider. Les cardinalités et responsabilités marquées ouvertes ne doivent pas être implémentées comme des décisions définitives.

Le schéma concret correspondant est [`data-model.dbml`](data-model.dbml) (importable dans dbdiagram.io).

## Vue d'ensemble proposée

```text
Project
 |- Repositories                (identites logiques : api, front, libs ; relative_path + optional)
 |- Workspaces                  (primary durable + worktrees per-PRD ephemeres)
 |   |- Terminals
 |   |- Agent Sessions
 |   |- Managed Commands
 |   `- (runtime ici ; repos derives du projet, non persistes par workspace)
 |- PRDs / Work Items           (tickets durables -> pointent vers leur workspace de dev)
 |   `- Tasks
 |- Reviews                     (un par repo dans un workspace ; comments -> tasks)
 |- Ideas
 |- ADRs
 |- Directives
 `- Documents
```

**Le workspace est le pivot.** Il n'y a pas d'entité `Execution` : le lien entre intention et runtime est porté par le workspace, et la relation PRD -> workspace est une simple colonne. Voir PDR-0008.

## Project

Conteneur durable représentant un produit ou un système logiciel.

**Direction proposée :** nyx devient l'autorité unique. Le `project` Depot ne survit pas comme entité parallèle.

Possède :

- workspaces ;
- PRD, tâches et idées ;
- directives et ADR ;
- configuration agentique ;
- commandes managées partagées.

## Workspace

**Status: validated comme pivot (PDR-0008)**

Dossier physique concret rattaché à un projet, et **point de rencontre unique entre intention et runtime**. Il peut être un checkout principal, un git worktree, un faux worktree multi-repo ou un dossier quelconque.

**À conserver de nyx :** identité folder-anchored et traitement git-agnostique.

Deux rôles d'une même entité :

- **primary workspace** : le dossier de base durable, désigné par `projects.primary_workspace_id`. C'est là que se font la conception et le travail libre sans PRD.
- **worktrees per-PRD** : éphémères. Créés au lancement du développement d'un PRD, nettoyés au merge.

**Cycle de vie.** La ligne workspace est un enregistrement permanent ; elle n'est jamais supprimée automatiquement. Le champ `folder_removed_at` marque le nettoyage du dossier physique au merge : le runtime se détache, la trace (path, branche, dates) est conservée. Une suppression de ligne n'est qu'une purge manuelle explicite.

**Cardinalité.** À un instant donné, un workspace porte au plus un PRD. Le runtime (terminaux, sessions agents, services) est rattaché au workspace.

## Repository

**Status: validated (PDR-0013)**

Identité **logique** d'un repo dans le projet (ex : `api`, `front`). Entité persistée, indispensable pour que « le repo `api` » soit le même objet dans le primary workspace et dans chaque worktree (continuité review/historique).

Porte son chemin conventionnel (`relative_path`, constant dans le projet ; `"."` en mono-repo) et un flag `optional` (le « nyxignore » en DB : non worktreé par défaut au lancement). Il n'y a **pas** de table de checkout par workspace : les repos d'un workspace = les repos non-`optional` du projet, au chemin `workspace.root_path + relative_path` ; branche/HEAD/dirty sont relus depuis Git. Mono et multi-repo partagent ce modèle ; en UI, un seul repo masque la couche repo (« juste un workspace »).

## PRD / Work Item

Intention structurée et **ticket durable** appartenant au projet, indépendante d'un dossier particulier. Sa vie s'étend au-delà du développement : conception (avec prototypage), développement, documentation.

Au lancement du développement, le PRD pointe vers son workspace de dev par la colonne `prds.workspace_id` : un pointeur unique, posé à ce moment, écrasable si le développement migre, jamais perdu (la ligne workspace survit au worktree). L'historique du PRD ne dépend donc d'aucune entité mutable : il vit sur le ticket (review, branche et commit de merge, ligne workspace conservée).

**Validé :** le domaine PRD/tâches utile de Depot doit devenir natif dans nyx.

**Ouvert :** conserver le terme PRD partout dans l'UI ou exposer une notion plus simple de `Work Item` dont PRD serait un type avancé.

## Task

Unité de travail issue d'un PRD ou créée indépendamment selon une règle à définir.

Doit distinguer au minimum :

- tâche exécutable par agent ;
- action humaine/décision ;
- finding issu d'une review.

## Prototype (annexe de conception)

**Status: proposed, absorbé tard (PDR-0009)**

Annexe visuelle d'un PRD, au scope projet. Aucun workspace, aucun runtime : du HTML auto-contenu rendu dans une iframe. L'agent génère des variantes de pages ; l'utilisateur itère par rounds et commentaires pinés, puis élit une variante qui se distille en tâches.

Décisions de forme (PDR-0009, par usage réel) : un seul prototype par PRD ; rounds conservés comme **mécanisme de rollback** quand l'agent dégrade le design ; pages, versions, variants, states, commentaires/résolution et élection conservés. Le schéma détaillé est figé à l'absorption (v2.6), pas ici. Le pont conception -> dev est l'élection d'une variante, porté par le PRD.

Le rendu et le vocabulaire visuel sont tranchés par PDR-0010 : design-system imposé (tables `design` au format DESIGN.md + `components`), composants Handlebars compilés dans le sidecar Rust, Tailwind exécuté localement dans l'iframe, sync incrémentale par md5 visuel. L'agent compose depuis ce vocabulaire imposé, pour le proto comme pour le dev.

## Execution (rejetée)

**Status: rejected (PDR-0008)**

`Execution` avait été proposée pour combler le trou entre Depot (qui connaît l'intention) et nyx (qui connaît le runtime). Après validation par workflow et analyse du flux réel, elle est rejetée :

- elle ne possédait rien en propre (terminaux et commandes appartenaient déjà au workspace ; son `execution_id` était nullable partout) ;
- son cycle de vie `active/paused/completed/abandoned` dupliquait celui, réel, du worktree (création = début, nettoyage au merge = fin) ;
- le flux est worktree-par-PRD : un workspace porte au plus un PRD, donc `Execution` n'était qu'une clé étrangère `PRD -> workspace` déguisée en entité.

Le trou intention/runtime est comblé par le **workspace** lui-même, plus la colonne `prds.workspace_id`. Aucune entité supplémentaire. Voir PDR-0008.

## Agent Session

Session persistante d'un fournisseur agentique, rattachée à un workspace et liée à un terminal courant qu'elle peut survivre pour rester reprenable. Le modèle doit rester provider-agnostic ; Claude Code est un adaptateur.

## Review

**Validated direction:** la review doit devenir générique et ne pas exiger un PRD.

Une review peut cibler un workspace, un repository, une branche, un commit/range ou un PRD. Elle possède des commentaires globaux ou ancrés, des threads, des réponses agent, un état outdated et une résolution humaine.

## Managed Command

Définition de service au niveau projet, instanciée par workspace. Ce modèle nyx reste la base. Les runs d'une instance appartiennent au workspace ; aucune Execution ne s'intercale.

## Attention Item

**Status: proposed projection**

Signal nécessitant une action humaine : agent en attente, service en erreur, review, commentaire non lu, décision ou tâche bloquée. Ce devrait probablement être une projection calculée d'événements existants, pas une nouvelle entité mutable universelle.

## Autorités validées ou recherchées

Sujet                 | Autorité cible                  | Statut
--------------------- | ------------------------------- | -------------------
Produit et navigation | nyx                             | validated
Backend métier        | Rust                            | validated
Shell desktop         | Electron                        | validated
Renderer              | React embarqué dans Electron    | validated
Project/workspace     | modèle nyx unifié               | proposed
Lien intention/runtime| workspace (Execution rejetée)   | validated (PDR-0008)
PRD -> dev            | colonne prds.workspace_id       | validated (PDR-0008)
PRD/tasks             | domaine natif nyx issu de Depot | validated direction
Review                | moteur générique nyx            | validated direction
UI et MCP             | mêmes use cases/état            | validated direction
Fichier SQLite unique | non décidé                      | open
