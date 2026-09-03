# Modèle de domaine

## État

**Status: proposed, except explicit validated directions**

Ce document décrit le modèle cible à valider. Les cardinalités et responsabilités marquées ouvertes ne doivent pas être implémentées comme des décisions définitives.

Le schéma concret correspondant est [`data-model.dbml`](data-model.dbml) (importable dans dbdiagram.io).

## Vue d'ensemble proposée

```text
Project
 |- Repositories                (identites logiques : api, front, libs ; relative_path + optional)
 |- Workspaces                  (primary durable + worktrees per-Spec ephemeres)
 |   |- Terminals
 |   |- Agent Sessions
 |   |- Managed Commands
 |   `- (runtime ici ; repos derives du projet, non persistes par workspace)
 |- Specs                       (unité humaine, clé Linear ; tickets durables -> workspace de dev)
 |   `- Tasks                   (décomposition agent ; executor agent|human)
 |- Reviews                     (vue dérivée : commentaires ancrés sur (workspace, repo) + spec_id ; finding = task)
 |- Agents / Workflows          (bibliothèque réutilisable ; le projet pointe un workflow)
 |- Ideas
 |- ADRs
 |- Directives (rules) + Checks (managed commands à un gate)
 `- Documents
```

**Le workspace est le pivot.** Il n'y a pas d'entité `Execution` : le lien entre intention et runtime est porté par le workspace, et la relation Spec -> workspace est une simple colonne. Voir PDR-0008.

## Project

Conteneur durable représentant un produit ou un système logiciel.

**Direction proposée :** nyx devient l'autorité unique. Le `project` Depot ne survit pas comme entité parallèle.

Possède :

- workspaces ;
- Spec, tâches et idées ;
- directives et ADR ;
- configuration agentique ;
- commandes managées partagées.

## Workspace

**Status: validated comme pivot (PDR-0008)**

Dossier physique concret rattaché à un projet, et **point de rencontre unique entre intention et runtime**. Il peut être un checkout principal, un git worktree, un faux worktree multi-repo ou un dossier quelconque.

**À conserver de nyx :** identité folder-anchored et traitement git-agnostique.

Deux rôles d'une même entité :

- **primary workspace** : le dossier de base durable, désigné par `projects.primary_workspace_id`. C'est là que se font la conception et le travail libre sans Spec.
- **worktrees per-Spec** : éphémères. Créés au lancement du développement d'un Spec, nettoyés au merge.

**Cycle de vie.** La ligne workspace est un enregistrement permanent ; elle n'est jamais supprimée automatiquement. Le champ `folder_removed_at` marque le nettoyage du dossier physique au merge : le runtime se détache, la trace (path, branche, dates) est conservée. Une suppression de ligne n'est qu'une purge manuelle explicite.

**Cardinalité.** À un instant donné, un workspace porte au plus un Spec. Le runtime (terminaux, sessions agents, services) est rattaché au workspace.

## Repository

**Status: validated (PDR-0013)**

Identité **logique** d'un repo dans le projet (ex : `api`, `front`). Entité persistée, indispensable pour que « le repo `api` » soit le même objet dans le primary workspace et dans chaque worktree (continuité review/historique).

Porte son chemin conventionnel (`relative_path`, constant dans le projet ; `"."` en mono-repo) et un flag `optional` (le « nyxignore » en DB : non worktreé par défaut au lancement). Il n'y a **pas** de table de checkout par workspace : les repos d'un workspace = les repos non-`optional` du projet, au chemin `workspace.root_path + relative_path` ; branche/HEAD/dirty sont relus depuis Git. Mono et multi-repo partagent ce modèle ; en UI, un seul repo masque la couche repo (« juste un workspace »).

## Spec

**Status: validated comme unité humaine (PDR-0014)**

Intention structurée et **ticket durable** appartenant au projet, indépendante d'un dossier particulier. Sa vie s'étend au-delà du développement : conception (avec prototypage), développement, documentation.

**Le Spec est l'unité de travail de l'humain** (PDR-0014) : c'est l'interface entre l'humain et la feature. L'humain conçoit et valide le Spec ; l'agent décompose en tâches et, si besoin, en phases. L'UI montre les tâches mais n'offre pas de gestion de tâches à la main.

Le Spec porte une **clé Linear** = préfixe projet (`projects.key`) + numéro incrémental par projet (`specs.number`), ex. `NYX-42` : c'est la référence humaine. Le **slug** descriptif est conservé pour l'URL (`NYX-42/workspace-pivot`). La **priorité** (`urgent/high/med/low`) est stockée sur le Spec. Le Spec porte une **catégorie unique** (`specs.category_id`, axe de classification du projet ; l'agent assigne depuis l'existant et peut en **minter** une — valide telle quelle, non bloquant ; l'humain cure le vocabulaire depuis les settings, créer / renommer / supprimer — PDR-0017 amendé le 2026-09-02).

Le Spec pointe vers son workspace de dev par la colonne `specs.workspace_id` : un pointeur unique, posable **dès que le Spec est `ready`** (le workspace existe souvent avant que le dev démarre — l'ancrer ne transitionne jamais le Spec) et au plus tard au lancement du développement, écrasable si le développement migre, jamais perdu (la ligne workspace survit au worktree). L'historique du Spec ne dépend donc d'aucune entité mutable : il vit sur le ticket (review, branche et commit de merge, ligne workspace conservée).

**Validé :** le domaine Spec/tâches utile de Depot doit devenir natif dans nyx. L'unité s'appelle **Spec** (renommée depuis « PRD » par PDR-0018 ; ferme Q-005). Alternatives `Work Item` / `Change` écartées.

## Task

**Status: validated comme décomposition agent (PDR-0014)**

Unité de décomposition issue d'un Spec (ou créée indépendamment pour du travail léger). **Gérée par l'agent, pas administrée à la main par l'humain** : pas de clé humaine (identité ULID interne). La **priorité** est un enum string (`urgent/high/med/low`) ; le champ **`phase`** est un regroupement nullable et optionnel posé par l'agent (pas de table de phases ; pas de phase = tâches à plat). Les phases sont **nommées** via une liste ordonnée sur la spec (`specs.phases` = `[{number,name}]`, PDR-0022) que `tasks.phase` indexe ; le **statut d'une phase reste dérivé** de ses tasks.

**Trois saveurs d'un seul concept** (pas trois tables) :

- **agent** — `executor = agent` (défaut) : la décomposition normale ;
- **humaine** — `executor = human` : une action que **seul l'humain** peut faire (ex. renseigner un secret, approuver un irréversible). Remplace la table `human_actions` (supprimée). Affichée dans l'**onglet Tasks** (badge « toi »), **actionnable** par l'humain (`done`) ; l'agent l'annonce aussi en chat. Peut bloquer une tâche agent via `task_dependencies`. L'Attention Inbox transverse est différée (PDR-0021) ;
- **finding** — une task promue depuis un commentaire de review (`review_comment_tasks`).

## Prototype (annexe de conception)

**Status: proposed, absorbé tard (PDR-0009)**

Annexe visuelle d'un Spec, au scope projet. Aucun workspace, aucun runtime : du HTML auto-contenu rendu dans une iframe. L'agent génère des variantes de pages ; l'utilisateur itère par rounds et commentaires pinés, puis élit une variante qui se distille en tâches.

Décisions de forme (PDR-0009, par usage réel) : un seul prototype par Spec ; rounds conservés comme **mécanisme de rollback** quand l'agent dégrade le design ; pages, versions, variants, states, commentaires/résolution et élection conservés. Le schéma détaillé est figé à l'absorption (v2.7), pas ici. Le pont conception -> dev est l'élection d'une variante, porté par le Spec.

Le rendu et le vocabulaire visuel sont tranchés par PDR-0010 : design-system imposé (tables `design` au format DESIGN.md + `components`), composants Handlebars compilés dans le sidecar Rust, Tailwind exécuté localement dans l'iframe, sync incrémentale par md5 visuel. L'agent compose depuis ce vocabulaire imposé, pour le proto comme pour le dev.

## Execution (rejetée)

**Status: rejected (PDR-0008)**

`Execution` avait été proposée pour combler le trou entre Depot (qui connaît l'intention) et nyx (qui connaît le runtime). Après validation par workflow et analyse du flux réel, elle est rejetée :

- elle ne possédait rien en propre (terminaux et commandes appartenaient déjà au workspace ; son `execution_id` était nullable partout) ;
- son cycle de vie `active/paused/completed/abandoned` dupliquait celui, réel, du worktree (création = début, nettoyage au merge = fin) ;
- le flux est worktree-par-Spec : un workspace porte au plus un Spec, donc `Execution` n'était qu'une clé étrangère `Spec -> workspace` déguisée en entité.

Le trou intention/runtime est comblé par le **workspace** lui-même, plus la colonne `specs.workspace_id`. Aucune entité supplémentaire. Voir PDR-0008.

## Agent Session

Session persistante d'un fournisseur agentique, rattachée à un workspace et liée à un terminal courant qu'elle peut survivre pour rester reprenable. Le modèle doit rester provider-agnostic ; Claude Code est un adaptateur.

## Agents, Workflows, Rules & Checks

**Status: validated (PDR-0020)**

Le fonctionnement agentique se décompose en concepts de première classe :

- **Agent** — définition de rôle, **bibliothèque réutilisable** (table `agents`). Quatre types : **spec**, **dev** (orchestrateur), **doc** (les principaux, déclenchables `/nspec` `/ndev` `/ndoc`) et **subagent** (coder, auditeur, ui-tester…, lancés par les principaux via le **Task du provider**, observés par nyx). nyx livre des défauts forkables ; les customs sont réutilisables entre projets. Contenu = template de prompt (statique + marqueurs rules/checks/données) + outils/skills + modèle.
- **Workflow** — composition réutilisable (`workflows` + `workflow_agents`) : 1 spec + 1 dev + 1 doc + N subagents + wiring (quel subagent/check à quel scope). nyx livre un workflow par défaut ; `projects.workflow_id` l'assigne.
- **Rule** — consigne advisory projet injectée dans un agent (`directives`). Ex. « Angular v20 idioms ».
- **Check** — commande déterministe à un gate, blocking (`managed_commands.gate`). Ex. lint / test / secrets.

Le contexte d'un agent est **rendu live par nyx-core** et injecté via les **hooks du plugin** (SessionStart / UserPromptSubmit), sans CLI ni fichiers générés (PDR-0020). Une **Agent Session** (ci-dessus) est l'instance **runtime** d'un de ces agents.

## Review

**Status: validated comme vue dérivée (PDR-0015)**

Il n'y a **pas d'entité review stockée** : une review est la **vue calculée** du diff d'un repo (relu depuis Git : base = point de fork, target = worktree/HEAD) plus les **commentaires** ancrés dessus. Même raisonnement que PDR-0013 (dériver de Git, ne pas persister ce que Git sait).

Les **commentaires** (`review_comments`) s'ancrent directement sur `(workspace, repository)` avec un `spec_id` **optionnel** : null = review générique d'un workspace sans Spec ; renseigné = la review « vit avec le Spec » et suit son statut. Ils portent threads, réponses agent, ancrage (fichier/ligne/plage) ou global, `base_blob_oid` + contexte pour remap/`outdated`, et une **résolution réservée à l'humain**. Pas de statut de review propre, pas de snapshot `pinned` (réintroductibles si un usage le prouve).

Un commentaire **promu en task** (`review_comment_tasks`) est un **finding** : c'est le seul lien durable vers le Spec et le seul travail que voit le coder.

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
Spec -> dev            | colonne specs.workspace_id       | validated (PDR-0008)
Spec/tasks             | domaine natif nyx issu de Depot | validated direction
Review                | vue dérivée (Git + comments)    | validated (PDR-0015)
UI et MCP             | mêmes use cases/état            | validated direction
Fichier SQLite unique | un seul fichier                 | validated (PDR-0024)
