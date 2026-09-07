# Workflows produit

## État

**Status: proposed - used to validate the domain model**

Un workflow marqué **(livré)** fait exception : son contrat est implémenté et couvert par des tests,
il décrit donc le produit tel qu'il est. Les autres restent des propositions.

**« Workflow » ici, c'est le parcours humain** — la suite de gestes que l'utilisateur et l'agent
enchaînent dans le produit. Ce n'est pas, et ce n'est plus, une **composition exécutable** : le moteur
d'orchestration qui stockait des workflows en graphe a été retiré (PDR-0031). Un parcours ci-dessous
se déroule parce que le manuel de la commande le décrit à l'agent et que nyx tient l'état, pas parce
qu'un interpréteur avance un run.

**Le texte de ces manuels n'est pas ici.** Il vit, mot pour mot, dans
[`agent-manuals.md`](agent-manuals.md) : `/nspec` (grill → Spec → décomposition → `ready`), `/ndev`
(frontière de tâches → sous-agents → vérification → audit → `in_review` → boucle de review), `/ndoc`
(réconciliation de la doc sur la plage Git de la Spec), plus la **charte** injectée dans chaque
sous-agent. Ce document-ci décrit le **parcours** tel que l'humain le vit ; `agent-manuals.md` porte
la **source** de ce que nyx injecte, et c'est elle qui fait foi sur ce que l'agent lit. Les gestes
qui ponctuent un parcours — `spec_transition`, `task_transition`, les commandes managées — sont des
verbes nyx : l'agent les demande, nyx tranche, et les transitions sensibles (`ready`, `completed`,
`cancelled`) n'avancent qu'avec `user_confirmed=true`, donc après une phrase claire de l'humain.

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
4. L'agent reçoit le **manuel de sa commande** (rôle + rules + Spec + tâches + workspace + commandes managées) rendu live par nyx-core et injecté via les **hooks** du plugin (SessionStart / UserPromptSubmit) ; ses sous-agents reçoivent la **charte** au `SubagentStart` ; le **MCP ne porte que les actions** (PDR-0020, PDR-0031).
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
5. L'ancre posée, le travail se fait **sans recopier l'id** : `agent_context` résout lui-même la Spec
   depuis le workspace ou le simple `cwd` de l'agent.

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

Résolution de la Spec par `agent_context`, **dans cet ordre**, en s'arrêtant à la première ancre qui
répond (le moteur d'orchestration retiré par PDR-0031 emportait avec lui `run_start` et l'ancre
`run_id` : il n'y a plus qu'un seul résolveur) :

- **d'abord** un `spec_id` explicite ;
- **puis** le `workspace_id` — la Spec active ancrée dessus ;
- **enfin** le `cwd` — le workspace dont la racine est le cwd ou son ancêtre (le `root_path` le plus
  profond gagne ; à racine identique, c'est le workspace **le plus anciennement enregistré** qui
  l'emporte, jamais le plus récent), puis sa Spec active. C'est le sélecteur cwd → workspace unique
  du produit, celui-là même qui attache les terminaux ; la **vivacité n'entre pas dans le
  classement** : un workspace archivé, ou dont le dossier a été nettoyé au merge, garde son
  ancienneté et l'emporte donc sur un enregistrement vivant posé après lui sur le même dossier.
  Un `cwd` qui ne correspond à **aucun** workspace est signalé sous son propre code,
  `spec_location_unresolved` — la portée de Spec est perdue parce qu'on appelle depuis le mauvais
  dossier, et le code nomme ce correctif-là.

Sur les deux dernières ancres, l'ensemble des candidates est le **même** : les Specs ancrées, **non
archivées et non terminales** (`completed` / `cancelled` ne concourent pas — un workspace accumule
légitimement les Specs qu'il a livrées). Le résultat est un contrat **0 / 1 / N** :

| candidates | résolution | flux qui **exige** une Spec (`/ndev`, `/ndoc`) | flux d'**authoring** (`/nspec`) |
| --- | --- | --- | --- |
| **0** | aucune Spec — **et le dire** : `warnings` porte `spec_none_on_location` (la localisation **est** un vrai workspace, il n'ancre simplement aucune Spec active) ou `spec_location_unresolved` (aucun workspace ne correspond — erreur d'invocation : on appelle depuis le mauvais dossier) | `role=doc` : **refus** `invalid_id` — passe un `spec_id`, ou un `cwd`/`workspace_id` ancré. `role=dev` fait **exception** et rend `ok` + warning : le manuel `/ndev` sait résoudre une référence lui-même, et lui fermer la porte fermerait la seule façon de lui dire quoi faire | `role=spec` rend **`ok`** : c'est le point de départ prévu de l'authoring — le premier `/nspec` d'un projet n'a par définition aucune Spec — signalé de la même façon |
| **1** | son id | travaille dessus | travaille dessus |
| **N** | **aucune** | **refus** `invalid_argument`, `reason = spec_ambiguous_for_workspace`, avec la **liste des candidates** | démarre **non lié**, et la phrase du refus est relayée en **warning** (`spec_ambiguous_started_unbound`) |

**Le refus de référence ambiguë appartient désormais à `agent_context`**, qui est le seul point
d'entrée : c'est lui que le hook `UserPromptSubmit` appelle pour rendre le manuel de la commande, avec
le `cwd` et, si l'humain en a passé un, le `spec_id`.

**Le cas 0 n'est pas muet, et il parle sur deux canaux.** Le zéro est un état parfaitement
légitime — le premier `/nspec` d'un projet démarre exactement là — donc il est **signalé, jamais
refusé** pour les deux rôles qui doivent pouvoir travailler sans Spec (`dev` et `spec`). Ces deux
rôles reçoivent :

1. le canal **machine** — une entrée `warnings` non vide `{ code, message }`, à code stable ; et
2. le canal **texte** — la **même** phrase rendue *dans* le manuel, sur une ligne
   `> NO SPEC ANCHORED — …`, parce qu'un agent qui lit le manuel ne doit pas avoir à remarquer un
   `spec_id` à `null` pour comprendre que rien de ce qu'il lit n'est scopé.

Les **deux** codes du cas 0 disent laquelle des deux situations très différentes on est :
`spec_none_on_location` (workspace réel, aucune Spec active) et `spec_location_unresolved` (aucune
localisation ne résout — mauvais dossier). Le code compte plus que le `spec_id` nul : il nomme le
correctif, et les deux n'ont pas le même.

Jamais, dans aucune de ces cases, nyx ne choisit « la plus récente » : N candidates donnent une
ambiguïté nommée avec ses candidates, pas un pari. Un `project_id` qui contredit le projet de la Spec
résolue est refusé de la même façon : une même ancre vise toujours la même Spec.

## Invariants de workflow proposés

- Aucun workflow ne doit exiger l'ouverture de Depot.
- Une mutation MCP doit être visible dans l'UI sans redémarrage.
- Une mutation UI doit être immédiatement lisible par MCP.
- Les agents ne résolvent pas les commentaires humains.
- Fermer nyx ne doit jamais laisser silencieusement des processus non maîtrisés.
- Le terminal libre reste disponible sans configuration produit préalable.
