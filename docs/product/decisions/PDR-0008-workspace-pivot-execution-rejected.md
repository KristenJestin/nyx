# PDR-0008 - Le workspace est le pivot, Execution rejetée

**Status: validated**

Remplace PDR-0003.

## Context

PDR-0003 proposait `Execution` comme entité première classe pour relier une intention facultative au runtime réel (workspace, branches, sessions agents, terminaux, services, commits, reviews). Il exigeait explicitement de prototyper les workflows avant de figer le schema.

Cette validation a été faite, et elle est négative. Trois constats convergents :

1. **Execution ne possède rien en propre.** Le schema cible lui-même posait « Terminal et CommandInstance appartiennent au workspace, jamais à l'Execution ». La colonne `execution_id` était alors *nullable* sur `human_actions`, `reviews`, `agent_sessions`, `command_runs`, `agent_context_snapshots` et `domain_events` : optionnelle partout. Une entité qui n'est propriétaire de rien et facultative partout est une étiquette, pas un agrégat.

2. **Son cycle de vie était artificiel.** `Execution` portait un `status active/paused/completed/abandoned` avec `paused_at` et `abandoned_at`. Or le cycle de vie réel est déjà porté par le **worktree** : sa création démarre le développement, son nettoyage au merge le termine. Execution dupliquait ce cycle de vie en moins fiable.

3. **Le flux de travail réel est worktree-par-PRD.** Un PRD validé est lancé en développement dans un **nouveau workspace** dédié. À un instant donné, un workspace porte au plus un PRD. Le cas « plusieurs executions concurrentes sur un workspace » n'existe pas dans l'usage. `Execution` n'était donc qu'une clé étrangère `PRD -> workspace` promue au rang d'entité.

## Decision

`Execution` est **rejetée** comme entité persistée. Le **workspace** est le point de rencontre unique entre intention et runtime.

- **Workspace = un dossier physique.** Une seule entité, deux rôles : le **primary workspace** durable (dossier de base, conception, travail libre) et les **worktrees per-PRD** éphémères, créés au lancement du développement et nettoyés au merge.
- **PRD = ticket durable** au niveau projet. Il pointe vers son workspace de développement par **une seule colonne** `prds.workspace_id`, posée au lancement, écrasable si le développement migre ailleurs, jamais perdue.
- **Le runtime** (terminaux, sessions agents, services) est **rattaché au workspace**, jamais à un PRD ni à une Execution.
- **La review** est rattachée au PRD (et peut viser un workspace pour une review générique). Elle est durable et survit au worktree.
- **La ligne workspace est un enregistrement permanent.** Elle n'est jamais supprimée automatiquement. Un champ `folder_removed_at` marque le nettoyage du dossier physique au merge ; le runtime se détache, la trace (path, branche, dates) reste. Toute suppression réelle de ligne est une purge manuelle.
- **Le merge est un événement atomique** : PRD `completed`, worktree nettoyé, `workspaces.folder_removed_at` posé, runtime du worktree éteint naturellement.

Les tables `executions`, `execution_tasks`, `execution_repositories`, `execution_commits`, `execution_terminals` et l'enum `execution_status` sont supprimées. Les colonnes `execution_id` du runtime sont re-rattachées au `workspace_id` lorsque l'attribution runtime est utile, sinon supprimées.

## Consequences

- le modèle perd cinq tables, un enum et un agrégat fictif sans perdre une seule capacité ;
- l'historique d'un PRD ne dépend plus d'une entité mutable : il vit sur le ticket durable (review `review.prd_id`, branche et commit de merge dans Git, ligne workspace conservée) ;
- le socle obtenu — `project / workspace (primary + worktrees) / PRD-ticket -> workspace / runtime@workspace / review@PRD` — **est exactement la v1 autonome de nyx** : workspace + runtime, sans Depot ;
- la super-app ne se construit pas par réécriture mais par accumulation : ajouter `prds.workspace_id` et les tables intention/review/doc issues de Depot par-dessus le même noyau runtime ;
- la discipline est posée : on modélise le flux réel d'un seul utilisateur. Une colonne devient une table le jour où une vraie seconde ligne existe, pas le jour où on l'imagine.

## Validation

Cette décision ferme Q-002. Toute réintroduction d'une entité de type `Execution` exigera une preuve d'usage concrète : un cas réel où deux unités de travail distinctes coexistent dans un même workspace et doivent être distinguées.
