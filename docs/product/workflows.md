# Workflows produit

## État

**Status: proposed - used to validate the domain model**

Un workflow marqué **(livré)** fait exception : son contrat est implémenté et couvert par des tests,
il décrit donc le produit tel qu'il est. Les autres restent des propositions.

## W-001 - Travail libre sans Spec

1. L'utilisateur ouvre un projet/workspace.
2. Il ouvre un terminal ou reprend une session agent.
3. Le travail se fait dans le workspace courant, sans Spec ni entité intermédiaire.
4. L'agent modifie le code et lance des services.
5. nyx expose les changements Git et permet une review locale.

Ce workflow protège le caractère terminal-first : un Spec ne doit jamais être obligatoire pour utiliser nyx.

## W-002 - Réaliser un Spec

1. L'utilisateur sélectionne un Spec prêt.
2. nyx crée le worktree de réalisation et pose `specs.workspace_id` vers lui.
3. nyx associe branche et contexte agent au workspace.
4. L'agent reçoit son contexte (rôle + rules + Spec) rendu live par nyx-core et injecté via les **hooks** du plugin (SessionStart / UserPromptSubmit) ; le **MCP ne porte que les actions** (PDR-0020).
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

Le Spec est facultatif.

## W-004 - Reprise après redémarrage

1. nyx restaure projets, workspaces, terminaux et états utiles.
2. Les sessions agents exactes sont proposées ou reprises selon la politique projet.
3. Les workspaces actifs retrouvent leurs liens de contexte (Spec, branche, sessions).
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

## W-007 - Lier un workspace déjà là à une Spec, puis travailler sans id (livré)

1. Le dossier de dev **existe déjà** — l'utilisateur l'a créé, ou nyx le suit depuis longtemps — et
   la Spec à réaliser est écrite ; tant que les deux objets ne sont pas liés, chaque appel doit
   recopier l'id de la Spec à la main.
2. L'utilisateur enregistre le dossier existant (`workspace_add`, dont le `path` doit exister et être
   un dossier) ou le fait créer (`create_workspace`, qui fait le `mkdir -p`), en passant un `spec_id`
   **optionnel** : le workspace est enregistré et l'ancre `specs.workspace_id` posée dans la même
   opération. Sans `spec_id`, le contrat des deux outils est strictement inchangé, et la réponse ne
   porte la Spec que si un `spec_id` a été passé.
3. `create_workspace` valide le `spec_id` **avant** le `mkdir` : une Spec refusée ne laisse aucun
   dossier derrière elle. `spec_set_workspace` corrige une ancre après coup, aux mêmes règles.
4. **Lier, c'est poser l'ancre `specs.workspace_id`** — un acte de *contexte*, pas de *cycle de vie* :
   ce n'est **ni** W-002 (là, c'est le lancement du dev qui crée le worktree) **ni** une transition de
   statut.
5. L'ancre posée, le travail se fait **sans recopier l'id** : `agent_context` et `run_start` résolvent
   eux-mêmes la Spec depuis le workspace ou le simple `cwd` de l'agent.

Règles de l'ancrage, identiques sur `workspace_add`, `create_workspace` et `spec_set_workspace` :

- **Statuts ancrables : `ready` et `in_progress` uniquement.** Une Spec `draft` — encore en écriture —
  est refusée, comme une Spec settled (`completed`, `cancelled`) ou archivée.
- **Aucune transition.** Ancrer une Spec `ready` la laisse `ready` ; c'est le lancement du dev
  (`ready → in_progress`) qui change le statut, jamais la liaison.
- **Même projet.** Le workspace et la Spec doivent appartenir au même projet ; un désaccord est
  refusé, jamais arbitré silencieusement.
- **Aucun provisioning.** Lier ne crée aucun worktree, aucune branche, aucun dossier au-delà du
  `mkdir` que `create_workspace` fait pour son propre `path`. Le provisioning d'un workspace de dev
  est un autre verbe.
- **Idempotent.** Ré-ancrer la Spec sur le workspace qu'elle porte déjà est un no-op ; la déplacer
  vers un autre workspace est atomique.
- **La chaîne vide n'est pas une façon de dire « aucun ».** `""` est **refusé** en
  `invalid_argument` sur les trois arguments d'ancrage (`workspace_add.spec_id`,
  `create_workspace.spec_id`, `spec_set_workspace.workspace_id`), au lieu d'être absorbé
  silencieusement comme un non-ancrage ou un désancrage — un `""` vient presque toujours d'une
  variable non substituée, et l'avaler ferait exactement l'inverse de ce que l'appelant demandait.
  Les deux façons documentées de dire « aucun » sont **inchangées** : omettre l'argument, ou passer
  `null`. Sur `create_workspace`, le refus est prononcé **avant** le `mkdir` : un argument que nyx
  n'acceptera jamais ne doit pas coûter un dossier sur le disque.

Résolution de la Spec par `agent_context` et `run_start`, **dans cet ordre**, en s'arrêtant à la
première ancre qui répond :

- **d'abord** un `spec_id` explicite ;
- **puis** le `run_id` — un run porte la Spec qu'il pilote (et s'il n'en porte pas, la réponse est
  « aucune Spec » : on ne remonte pas au-delà du run). C'est le **chemin nominal de `/nspec`**, dont
  le run ne pilote rien tant qu'il n'a pas créé sa Spec ; `agent_context` le signale sous son propre
  code, **`spec_run_unbound`**, distinct des deux autres cas 0 parce qu'ici la localisation a
  parfaitement résolu — c'est le *run* qui ne porte pas de Spec, et répondre « passe un `cwd` »
  désignerait le mauvais correctif ;
- **puis** le `workspace_id` — la Spec active ancrée dessus ;
- **enfin** le `cwd` — le workspace dont la racine est le cwd ou son ancêtre (le `root_path` le plus
  profond gagne ; à racine identique, c'est le workspace **le plus anciennement enregistré** qui
  l'emporte, jamais le plus récent), puis sa Spec active. C'est le sélecteur cwd → workspace unique
  du produit, celui-là même qui attache les terminaux ; la **vivacité n'entre pas dans le
  classement** : un workspace archivé, ou dont le dossier a été nettoyé au merge, garde son
  ancienneté et l'emporte donc sur un enregistrement vivant posé après lui sur le même dossier.
  Un `cwd` qui ne correspond à **aucun** workspace est signalé des deux côtés, sous deux codes
  différents parce que les deux outils n'y perdent pas la même chose : `agent_context` perd une
  *portée de Spec* (`spec_location_unresolved`), tandis que `run_start` perd l'*ancre de workspace
  du run* — ses effecteurs ne résolvent alors plus aucun repo — et le dit sous
  **`run_location_unresolved`**. `run_start` ne signale que cette moitié-là, et **pas** parce que
  l'autre lui conviendrait : sur une localisation qui résout un vrai workspace sans Spec active, il
  n'avertit pas, il **refuse** en `invalid_argument` dès que le flux travaille *sur* une Spec
  (`ndev`, `ndoc`, `coder`, `prototyper`) — un warning y ferait double emploi avec le refus. Les
  seuls scopes qui y démarrent **non liés** sont `nspec`, dont c'est le point de départ prévu (et
  l'avertir *systématiquement* tirerait un warning sur 100 % des démarrages d'authoring), et
  `custom`, qui ne déclare aucun contrat de Spec. Mais `custom` n'échappe qu'au warning
  *systématique* : il en reçoit un **conditionnel** — un **`query_without_spec` par nœud `query`**
  de son graphe — dès que ce graphe interroge une Spec que le run ne porte pas, parce que ces
  `query` répondent alors la valeur vide (`null` / `0` / `false`) qu'une branche lit comme « plus
  rien à faire ». Enfin, un `run_start(spec_id=…)` ne nomme aucun dossier et n'est donc jamais
  averti.

Sur les deux dernières ancres, l'ensemble des candidates est le **même** : les Specs ancrées, **non
archivées et non terminales** (`completed` / `cancelled` ne concourent pas — un workspace accumule
légitimement les Specs qu'il a livrées). Le résultat est un contrat **0 / 1 / N** :

| candidates | résolution | flux qui **exige** une Spec (`/ndev`, coder, `/ndoc`) | flux d'**authoring** (`/nspec`) |
| --- | --- | --- | --- |
| **0** | aucune Spec — **et le dire** : `warnings` porte `spec_none_on_location` (la localisation **est** un vrai workspace, il n'ancre simplement aucune Spec active) ou `spec_location_unresolved` (aucun workspace ne correspond — erreur d'invocation : on appelle depuis le mauvais dossier) | **`run_start(scope=ndev\|ndoc\|coder\|prototyper)` : refus** `invalid_argument` — passe un `spec_id`, ou un `cwd`/`workspace_id` ancré. **`agent_context(role=coder\|auditor\|doc\|prototype)` : refus** `invalid_id`, même correctif. `agent_context(role=dev)` fait **exception** et rend `ok` + warning, alors que `run_start(scope=ndev)` refuse : voir la note sous le tableau | **`run_start(scope=nspec)`** démarre **non lié** — il va justement créer la Spec — et **`agent_context(role=spec)`** rend **`ok`**, signalé de la même façon |
| **1** | son id | travaille dessus | travaille dessus |
| **N** | **aucune** | **refus** `invalid_argument`, `reason = spec_ambiguous_for_workspace`, avec la **liste des candidates** | démarre **non lié**, et la phrase du refus est relayée en **warning** (`spec_ambiguous_started_unbound`) |

**Deux outils, deux critères — et au cas 0 ils divergent.** Les colonnes ci-dessus nomment des
*flux*, mais chaque outil décide sur son propre axe, et ces deux axes ne se recouvrent pas :

- **`agent_context` tranche sur le `role`.** Au cas 0, `dev` et `spec` reçoivent **`ok` + warning** ;
  les quatre rôles scopés Spec — `coder`, `auditor`, `doc`, `prototype` — sont **refusés** en
  `invalid_id`.
- **`run_start`, au cas 0, tranche sur le `scope` du workflow résolu** — pas sur l'argument `scope`,
  qu'un `workflow_id` explicite laisse à son défaut `ndev`. Les scopes qui travaillent *sur* une
  Spec — `ndev`, `ndoc`, `coder`, `prototyper` — sont **refusés** en `invalid_argument` ; seuls
  `nspec` et `custom` démarrent **non liés**, et sans warning *systématique* de cas 0 : `nspec`
  n'en reçoit aucun, `custom` en reçoit un **par nœud `query`** de son graphe
  (`query_without_spec`).
- **Conséquence : `/ndev` reçoit deux réponses opposées selon l'outil appelé.**
  `agent_context(role=dev)` rend `ok` avec warning, `run_start(scope=ndev)` **refuse**. Ce n'est pas
  une incohérence mais la répartition du travail : le dispatcher n'ancre aucune Spec lui-même — c'est
  `run_start` qui résout, ou pour un `/nspec` neuf *crée*, le run — donc lui refuser son manuel
  fermerait la seule porte d'entrée de l'authoring, tandis qu'un run `ndev` parti non lié serait
  inerte, ses requêtes et ses transitions de Spec ne portant sur rien.

**Le cas 0 n'est pas muet, et il parle sur deux canaux.** Le zéro est un état parfaitement
légitime — le premier `/nspec` d'un projet démarre exactement là — donc il est **signalé, jamais
refusé** pour les deux rôles qui doivent pouvoir travailler sans Spec (`dev` et `spec`). Ces deux
rôles reçoivent :

1. le canal **machine** — une entrée `warnings` non vide `{ code, message }`, à code stable, la même
   forme que celle de `run_start` ; et
2. le canal **texte** — la **même** phrase rendue *dans* le manuel, sur une ligne
   `> NO SPEC ANCHORED — …`, parce qu'un agent qui lit le manuel ne doit pas avoir à remarquer un
   `spec_id` à `null` pour comprendre que rien de ce qu'il lit n'est scopé.

Les **trois** codes du cas 0 disent laquelle des trois situations très différentes on est :
`spec_none_on_location` (workspace réel, aucune Spec active), `spec_location_unresolved` (aucune
localisation ne résout — mauvais dossier) et `spec_run_unbound` (le `run_id` a résolu, mais le run ne
pilote aucune Spec : le cas nominal de `/nspec`). Le code compte plus que le `spec_id` nul : il
nomme le correctif, et les trois n'ont pas le même.

Jamais, dans aucune de ces cases, nyx ne choisit « la plus récente » : N candidates donnent une
ambiguïté nommée avec ses candidates, pas un pari. Un `project_id` qui contredit le projet de la Spec
résolue est refusé de la même façon, sur `agent_context` comme sur `run_start` — les deux visent
toujours la même Spec pour une même ancre.

## Invariants de workflow proposés

- Aucun workflow ne doit exiger l'ouverture de Depot.
- Une mutation MCP doit être visible dans l'UI sans redémarrage.
- Une mutation UI doit être immédiatement lisible par MCP.
- Les agents ne résolvent pas les commentaires humains.
- Fermer nyx ne doit jamais laisser silencieusement des processus non maîtrisés.
- Le terminal libre reste disponible sans configuration produit préalable.
