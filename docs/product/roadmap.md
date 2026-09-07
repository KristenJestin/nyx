# Roadmap produit nyx V2

## État

**Status: proposed sequencing, no dates committed**

## Convention de version

**V2 désigne la totalité de la super-app cible décrite par la vision produit.** Les versions `v2.0`, `v2.1`, `v2.2`, etc. sont des tranches livrables de cette même V2 ; elles ne représentent pas des produits ou orientations séparés.

La V2 est complète uniquement lorsque les capacités utiles de Depot sont natives dans nyx, les données ont été migrées et Depot peut être retiré.

## État d'avancement

| Tranche | Intitulé | État |
| --- | --- | --- |
| v2.0 | Consolidation Electron/Rust | ✅ livré |
| v2.1 | Noyau produit unifié | ✅ livré |
| v2.2 | Workbench de travail actif | ✅ livré |
| v2.2.5 | Persistance terminal | ✅ livré |
| v2.3 | Terminal en blocs (honor-the-shell) | ⏸️ parké (WIP, branche non mergée) |
| v2.4 | Specs : tâches et contextes agents | 🚧 en cours |
| v2.5 | Diff et review locale | ✅ livré (clôturé ; greffe onglet Review = suivi) |
| v2.5.5 | Reprise & cycle de vie des sessions agent | ✅ livré (mergé) |
| v2.5.6 | Cockpit de pilotage (close-warning, CRUD, stats non-inline, reorder dnd-kit) | 🚧 en cours |
| v2.5.7 | Spec révisions : cliché complet (tâches versionnées par révision) | ✅ livré |
| v2.5.8 | Contextes agents : livraison du harnais (stubs /n… + hook push + rôles spec/doc, multi-provider) | 📋 PRD prêt |
| v2.5.9 | Reprise sessions : retrait de la carte Interrupted/Resume (crash = arrêt propre) | 🚫 annulé |
| v2.5.10 | Surcouche terminal↔Spec : overlay live (avancement dev + création) | 📋 PRD prêt |
| v2.5.11 | Boucle review : findings (tasks kind=review) + rework en rounds + résolution + retour-chat | 📋 PRD prêt |
| v2.6 | Knowledge et mémoire projet | ⬜ à venir |
| v2.7 | Machinerie prototype | ⬜ à venir |
| v2.8 | Harnais customisable (rules/checks/agents/workflows + visualiser) | ⬜ à venir |
| v2.9 | Migration finale et extinction Depot | ⬜ à venir |
| tardif | Délégation & reprise cross-machine (handoff continuable) | 🔮 capturé — post-v2.9, position provisoire |

> Le détail des chantiers menés hors de la ligne principale (review v2.4-review, terminal-blocks parké) vit dans
> [`session-notes.md`](session-notes.md) ; les sections ci-dessous n'en gardent qu'un pointeur.

## v2.0 - Consolidation de la plateforme Electron/Rust

**Status: livré**

- terminer la migration Electron ;
- retirer Tauri du build, des dépendances et de la maintenance active ;
- regrouper le renderer React dans l'application desktop Electron ;
- conserver toute la logique métier et les autorités de données dans `nyx-core` en Rust ;
- réduire Electron main/preload à la fenêtre, au lifecycle desktop et au transport IPC sécurisé ;
- remplacer le `core-host` Node et N-API par un exécutable sidecar Rust supervisé ;
- stabiliser le protocole V1 MessagePack, versionné et encadré sur pipes entre Electron main et le sidecar ;
- utiliser des frames binaires bornées avec backpressure pour les flux PTY ;
- supprimer le candidat Node du POC, ses switches et toute double implémentation ;
- déplacer dans Rust la logique métier/runtime encore présente dans le `core-host` TypeScript ;
- simplifier le monorepo autour de `apps/desktop`, `crates/nyx-core`, `crates/nyx-sidecar` et `e2e` ;
- produire et embarquer le sidecar natif Windows, Linux et macOS hors `app.asar` ;
- valider packaging, handshake, lifecycle, crash recovery, PTY, SQLite et performances sur les OS cibles.

**Gate :** Electron est l'unique shell livré, Rust l'unique backend métier, le sidecar et tous ses descendants meurent avec l'application, Tauri/N-API/core-host ne sont plus compilés et aucune logique métier concurrente ne subsiste dans Node/TypeScript.

## v2.1 - Noyau produit unifié

**Status: livré**

- figer le modèle Project/Workspace/Repository ;
- faire de nyx l'autorité unique de ces identités ;
- acter le workspace comme pivot intention/runtime (`Execution` rejetée, PDR-0008) ;
- acter le workspace comme point de rattachement du travail, **travail libre possible sans Spec** ; le lien Spec→runtime (`specs.workspace_id`) et la table `specs` sont **réservés ici mais réalisés en v2.4** (v2.1 ne construit pas le domaine Spec) ;
- mettre en place les use cases communs UI/MCP ;
- établir le flux d'événements temps réel des **mutations métier** (`domain_events` poussés, pas de polling) ; les **vues dérivées de Git** (diff/dirty/branche) sont **différées en v2.5** (Q-015) ;
- définir la stratégie de stockage (**un seul fichier SQLite**, PDR-0024) et de migrations de la V2 (chaîne unique, reprise des données v1).

**Gate :** le modèle relie clairement intention, dossiers, runtime et Git sans autorité Depot parallèle.

## v2.2 - Workbench de travail actif

**Status: livré** (périmètre **travail libre** ; les liens Spec/tâches sont réservés en seams, réalisés en v2.4)

Livré (validé par la gate e2e cockpit, pilotage CDP) :

- workspace de travail comme unité, écran de travail actif ;
- terminaux, agents et services **contextualisés** au workspace, live sans polling (`domain_events`) ;
- timeline utile et bornée ;
- reprise complète du contexte runtime après redémarrage (terminal restauré + bindé, service + session agent
  persistés) ;
- **travail libre possible sans Spec** (W-001).

Réservé / différé (pas construit ici, conforme aux décisions) :

- **association Spec/tâches ↔ workspace** = **seams réservés**, réalisés en **v2.4** (domaine Spec) ;
- **Attention Inbox** = **différée** (Q-007 / PDR-0021) — la gate a explicitement validé « no Attention Inbox » ;
  les human tasks et le signal review passent par l'onglet Tasks / Overview ;
- agents **sans 3ᵉ catégorie d'arbre** (Q-013), pas d'entité `Execution` (PDR-0008).

**Gate (atteinte) :** exécuter et suivre une feature libre réelle sans autre outil d'exécution que nyx.

## v2.2.5 - Persistance terminal (livré)

**Status: livré**

- bascule **xterm → wterm** (`@wterm/dom` + core **ghostty**, full VT), une seule lib terminal (PDR-0026) ;
- **store d'instances no-host** (Map module-level hors React et hors cycle routeur) : detach/reattach du nœud DOM au fil de la navigation, **navigation lossless** (buffer, alt-screen, focus intacts) ;
- **modèle d'écran VT headless** dans le sidecar + **re-hydratation au restart** : **restart lossless** (le TUI, hors scrollback, est restauré) ;
- fondation **routeur** du shell de navigation multi-pages (PDR-0025) sur laquelle se branche le store terminal.

**Gate :** naviguer et redémarrer l'app sans perdre le contexte terminal (buffer + TUI en alt-screen), sans host de terminaux persistant.

## v2.3 - Terminal en blocs (honor-the-shell)

**Status: parké** (WIP validé en direct, branche `feature/v2.3-terminal-blocks` non mergée) — PRD `01KWY7NFQJN068Q5ZSAJSSK2V2`

> Amélioration UX du terminal, suite de v2.2.5. Occupe le slot **v2.3** mais **parké** : la priorité est v2.4 (Specs),
> une re-cadrage en v2.2.x reste possible. Le détail **fait / reste à faire / leçon** (dont le défaut visuel bloquant
> de la grille wterm) est dans [`session-notes.md`](session-notes.md).

- **sortie shell en blocs** commande+résultat via **OSC 133** (bornes A/B/C/D, code de sortie) ;
- **honor-the-shell** : le shell garde l'édition (Tab, Ctrl-R, historique, sudo, vim natifs) — nyx n'intercepte pas la saisie ;
- **historique en blocs HTML statiques** (ANSI → style) ;
- **région active + TUI** rendus via la grille **wterm** ;
- **vrai prompt rejoué** (jamais synthétisé) ;
- **bascule blocs ↔ grille plein écran** sur alt-screen ;
- réutilise le **socle terminal v2.2.5** (wterm/ghostty, store no-host, routeur).

**Gate :** travailler au shell en blocs (badges de statut, helpers natifs, TUI plein écran) sur **bash + starship**.

## v2.4 - Specs : tâches et contextes agents

**Status: en cours** — PRD `01KX1C2CZ4D7PHT709N3Z92FR6`

Domaine Spec :

- absorber Spec et révisions ;
- absorber tâches, dépendances, actions humaines (`executor=human`) et findings de review ;
- porter lifecycle, validations et garde-fous au backend nyx (Rust/SQLite) ;
- relier Spec/tâches au workspace de dev (**réalise `specs.workspace_id`**, le seam réservé en v2.1) ; le
  **détail Spec avec onglets** construit ici est le **foyer** où la review se grafte (onglet « Review » du détail
  Spec, v2.5) ;
- fournir l'interface `Specs` native nyx ;
- exposer les outils MCP nyx d'authoring et de transition ;
- actualiser immédiatement l'UI après toute mutation MCP.

Fonctionnement agentique (PDR-0020) — câblé ici :

- tables **`agents`** (4 types spec/dev/doc/subagent) + **`workflows`** / **`workflow_agents`** ; `projects.workflow_id`
  assigne un workflow ; nyx livre les défauts forkables ;
- **rules** (`directives`) + **checks** (`managed_commands.gate` / `blocking`) qui décomposent les « directives »
  Depot ;
- **contextes agents rendus live par nyx-core** et **injectés via les hooks du plugin** (SessionStart /
  UserPromptSubmit), **zéro CLI, zéro fichier généré** ; déclencheurs = stubs fixes `/nspec` `/ndev` `/ndoc` ; MCP =
  actions seules ;
- **install via Settings → Integrations** (plugin bundlé `nyx-claude-integration`, par provider) ;
- `specs.mode` = **stepped** | **autopilot**.

Ne pas porter : shell web Depot, daemon, serveur Hono, CLI agent, projets/workspaces dupliqués ou présentation historique devenue inutile.

**Gate :** le workflow complet Spec -> tâches -> workspace de dev -> review -> done fonctionne dans nyx sans appeler Depot.

## v2.5 - Diff et review locale

- moteur Git multi-repo ;
- **rafraîchissement temps réel des vues dérivées de Git** (diff/dirty/branche) : watch fs/git ou re-read sur déclencheur — tranche **Q-015** (différée depuis v2.1) ;
- working tree, staged, unstaged, untracked, commit, range et branches ;
- gros diffs virtualisés ;
- review générique, Spec facultatif ; review = vue dérivée (pas de table), modèle comment/task, finding = task (cf. PDR-0012, PDR-0015) ;
- commentaires globaux, fichiers, lignes et plages ;
- threads humain/agent ;
- réponses agent via MCP ;
- remapping des ancres et état outdated ;
- pièces jointes utiles ;
- résolution réservée à l'humain ;
- compteurs non lus et temps réel UI/MCP ;
- **présentation** : la review est un **onglet « Review » du détail Spec** construit en v2.4 (domaine Spec), ancré sur le **workspace du Spec**.

**Gate :** reviewer un gros changement local multi-repo sans VS Code ni forge distante.

### Note de session — capacité review (branche v2.4-review)

> La capacité review a été **développée et validée en direct (pilotage CDP, gate e2e 9/9)** sur la branche
> **`v2.4-review`** (commit `61525ee`, non mergé), mais son **foyer de navigation** correct — un **onglet « Review »
> dans le détail Spec** — appartient au **domaine Spec** (v2.4). Cette tranche **est** v2.5 : la review s'y **grafte**
> une fois le domaine Spec disponible. Moteur de diff = **PDR-0027** (gix primaire + fallback git CLI + `notify`).
>
> **Détail complet** (ce qui est développé, le placement, la leçon de séquençage) → [`session-notes.md`](session-notes.md).

## v2.5.5 - Reprise & cycle de vie des sessions agent

**Status: livré** — PRD `01KX8Z4E7B3PYW2DGCTTKXCHYC`

> Point-release issue de l'audit v1→v2 : beaucoup de code de pilotage v1 a été recopié en v2 mais laissé
> non-câblé ; la reprise des sessions avait le setting **et** la politique backend mais **aucune exécution**.
> Comme v2.2.5, cette tranche **n'incrémente pas** la suite (v2.6 à v2.9 gardent leurs numéros).

- rebrancher l'**UI Integrations** (install du plugin Claude, dont dépendent détection d'agent + icône + statut) + le toggle `resume_agent_sessions` dans le shell actif ;
- **EXÉCUTER la reprise en mode LAZY** (à l'ouverture d'un terminal, pas eager au boot) : consommer `workspace_restore_plan`, re-porter park+inject `claude --resume` (réutilise `decide_resume` / `workspace_resume_plan`) ;
- afficher les états post-crash `interrupted` / `unknown` + une UI de restauration ;
- ~~**UNLOAD des sessions idle non visitées** + resume au retour (spike sur la détection d'activité réelle) — c'est ici que vit l'idée parquée `01KX0X3ZZQMEPKBQX30SX47HJF` (déplacée depuis v2.7).~~ **RETIRÉ** : le balayage tuait le shell d'une session simplement laissée au repos (symptôme `[process exited with code 1]` sur une session Claude inactive). Décision humaine : **aucun kill sur inactivité**. Le toggle `resume_agent_sessions` et la reprise `claude --resume` au boot (ResumePark) restent, ils ne dépendaient pas de l'unload.

**Gate :** toggle ON → relance → reprise à l'ouverture ; crash → interrupted + restore ; plugin installable depuis l'UI.

## v2.5.6 - Cockpit de pilotage : close-warning, CRUD, stats (non-inline), reorder (dnd-kit)

**Status: ready** — PRD `01KXBNX508EYHSQ1CW7RWYKV95` (rev 2 courante ; la rev 1 `01KX905W…3M` est superseded)

> Point-release qui rebranche le reste du cockpit v1 déjà recopié en v2 mais non-câblé.
> Comme v2.2.5, elle **n'incrémente pas** la suite (v2.6 à v2.9 gardent leurs numéros).

- **close-warning** avant de tuer des sessions live (contrôles de fenêtre min/max/close = **hors scope**, fournis par le WM) ;
- **kebab projet** + CRUD (rename/delete projet + workspace) ;
- **stats CPU/RAM par terminal** en affichage **non-inline** (survol/tooltip ou zone dédiée — ne pas cacher le nom comme en v1) ; ajoute le producteur sidecar `terminal://stats` manquant ;
- rename inline / bouton fermer par ligne / bande terminaux libres ;
- **reorder** terminaux + projets (drag, **dnd-kit** comme les sortable-*-list de v1).

**Gate :** parcourir le cockpit rebranché (close-warning, CRUD, stats non-inline, rename/close, terminaux libres, reorder dnd-kit), sans régression du workbench v2.2 ni de v2.5.5.

## v2.5.7 - Spec révisions : cliché complet (tâches versionnées par révision)

**Status: livré** — PRD `01KXBQ8QVYR887Y1C79V6EEQK4`

> Suivi de v2.4 (Specs). Point-release qui **n'incrémente pas** la suite (v2.6 à v2.9 gardent leurs numéros).
> Déclencheur : en absorbant Depot, on a constaté que son fork ne clonait que le narratif + une partie des tâches et **perdait des liens** (vécu au fork rev 1→2 du PRD cockpit). Le natif ne doit **pas** reproduire ça.

- une révision de Spec devient un **cliché immuable complet** : narratif **+ tâches + dépendances** (aujourd'hui seul le narratif est figé ; les tâches pendent du Spec via `tasks.spec_id` et traversent les forks) ;
- `tasks` + `task_dependencies` **rattachées à une révision** + migration idempotente (backfill sur la révision active) ;
- `fork_spec` **deep-clone atomique** des tâches + dépendances + sous-tâches (nouveaux ids, références de deps et de parent **remappées**, zéro lien perdu) ;
- tâches d'une révision `superseded` en **lecture seule** (historique immuable) ;
- lectures (task list, contexte agent, review) **scopées à une révision** (active par défaut ; historique consultable).

**Gate :** forker un Spec à N tâches + M dépendances produit N tâches + M dépendances remappées, la révision précédente restant figée et en lecture seule ; zéro lien perdu ; sans régression du lifecycle Spec v2.4.

## v2.5.8 - Contextes agents : livraison du harnais (injecté, générique multi-provider)

**Status: ready** — PRD `01KXC80E2CDSNN9PPMJFJFE3X7` (6 contextes rédigés : spec/dev/coder/auditor/doc/prototype)

> Finit ce que v2.4 avait **promis mais pas livré**. Le RENDER des contextes existe (`agent_context`, en *pull*, rôles dev/coder/auditor) ; la **livraison** (hook push + stubs `/n…`) et les rôles **spec/doc** manquent → le Gate v2.4 « workflow complet **sans Depot** » n'est pas tenu. Point-release qui n'incrémente pas la suite.

- ajouter les rôles **`spec`** (auteur-PRD qui grille) et **`doc`** au render live (aujourd'hui dev/coder/auditor seuls) ;
- **livraison par hook** : `UserPromptSubmit` détecte le stub, appelle `agent_context` au sidecar (`127.0.0.1/mcp`) et injecte le manuel en `additionalContext` — **le patron SessionStart existe déjà** dans `nyx-session-hook.cjs` ;
- **3 stubs fixes** `/nspec` `/ndev` `/ndoc` livrés avec le plugin bundlé (aujourd'hui : dossier `hooks/` seul, pas de `commands/`) ;
- **contexte dynamique**, pas un template mort : rôle + rules (`directives`) + spec/phase/tâches + workspace/repos + design, rendus **live par nyx-core** ;
- **sous-agents** (coder/auditor) : contexte transmis à leur lancement — mécanisme à arrêter dans le PRD (SubagentStart vs passé par l'orchestrateur) ;
- **générique multi-provider** : Claude Code aujourd'hui, mais l'abstraction doit tenir pour **OpenCode / Codex / autres** — le harnais est de la **donnée** (rendue par nyx-core), le **transport** (hooks/stubs) est **par-provider**.

**Gate :** taper `/nspec` (ou `/ndev`, `/ndoc`) injecte le harnais rendu **live** depuis nyx, MCP = actions seules ; la boucle create→dev→doc se pilote **sans Depot** — le vrai Gate v2.4 enfin tenu.

## v2.5.9 - Reprise sessions : crash = arrêt propre (retrait de la carte Interrupted/Resume)

**Status: annulé** — Spec `01KXJSTWB4E42FFQ4GVJMBMQMD` (dev jamais réellement lancé)

> Revert ciblé de la partie « interrupted + carte » de v2.5.5 : **pas de distinction crash**. Un arrêt de nyx (propre OU crash) est traité pareil ; le lazy-resume piloté par le toggle projet **reste**.

- backend : `recovery.rs` (l.200-202) ne surface plus d'état `interrupted` — le redémarrage suit la seule resume policy (toggle projet) ;
- UI : retirer la **carte orange Interrupted/Resume** + l'état `interrupted` (ambre) du `LifecycleBadge` ;
- comportement : au redémarrage, toggle ON → reprise **silencieuse** ; OFF → rien.

**Gate :** après un crash, plus aucune carte orange ; la reprise est pilotée par le seul toggle resume, sans régression du lazy-resume v2.5.5.

## v2.5.10 - Surcouche terminal↔Spec : overlay live (avancement dev + création)

**Status: ready** — Spec `01KXKM4X65P9CVQMX3ZBZY7H6H` (6 rounds de proto distillés)

> Relier un terminal au(x) Spec(s) qu'y travaille une session agent, et afficher **en surcouche** : l'avancement des tâches (dev/review) ou la construction du Spec **en direct** (création). Mapping **automatique + éphémère** (démappe au `/clear`), **générique multi-provider**.

- **DB** : `agent_session_specs` (many-to-many) ; **auto-map** par nyx-core (session ↔ Specs travaillés via MCP) ; **un-map** au reset de session (Claude = `SessionEnd reason=clear`, jamais compact/resume) ;
- **résolution** : Specs de la **session active** du terminal, temps réel via `domain_events` + état de session ;
- **UI** : rail « avancement dev » (fidèle `tasks-tab`) + tiroir « création live » (sections + tâches qui streament) ; onglet fermé cohérent ; **empilement multi-Spec sobre** (tiroir uniquement ; dev = un Spec).

**Gate :** créer/dev un ou plusieurs Specs dans une session → overlay live + empilement sur le bon terminal ; `/clear` démappe (pas compact/resume) ; temps réel sans polling ; cœur générique multi-provider.

## v2.5.11 - Boucle review complète : findings (tasks kind=review) + rework en rounds + résolution + retour-chat

**Status: ready** — Spec `01KXR1AGWK08H9P800YHXPBCHS` · **couplé à v2.5.10** (l'overlay affiche ce modèle) · proto sous-phases `nested` distillé

> Corrige la boucle review de `/ndev` (v2.5.8), écarts révélés en test + par l'overlay mergé : **deux modèles finding coexistent** (`tasks kind=review` que l'overlay affiche vs `review_tasks` du viewer) → on **unifie sur `tasks kind=review`** ; retour‑chat sans chemin vers une review humaine ; rework invisible.

- **Findings = `tasks kind=review`** (aligné overlay ; enrichi de `from_comment_id` + grill + **origine** auditeur/humain ; `review_tasks` déprécié) ;
- **rework en ROUNDS** (sous‑phases 2.1, 4.1…) rattachés à la phase courante — chaque passage coder = un round → rework **visible** (= temps) ;
- **grill** (proposed→structured→validated, validate human‑only) ; **comment↔task** (`from_comment_id`, 1→N) + **résolution** human‑only quand les tâches sont done ;
- **retour‑chat = canal de review** (verbe MCP) — donner un retour dans la session, pas que le viewer ;
- **statut in‑place** : le Spec reste `in_review` pendant le rework (pas de bounce) ; corrige la state rule `/ndev`.

**Gate :** retour (chat ET viewer) → grill → `review_tasks` → rounds par‑phase → coder → done → resolve (human‑only) ; findings auditeur en rounds aussi ; le Spec reste `in_review`.

## v2.6 - Knowledge et mémoire projet

- idées et promotion vers Spec ;
- ADR ;
- directives projet ;
- documents et annexes ;
- décisions et contexte durable ;
- activité Depot utile convertie en timeline contextuelle ;
- recherche et navigation transversales ;
- progressive disclosure pour ne pas encombrer le terminal et le travail actif.

**Gate :** les informations durables nécessaires aux agents et à l'utilisateur sont consultables et modifiables dans nyx, sans second système de mémoire.

## v2.7 - Machinerie prototype

PRD `01KXC8WVEYZM32Y1BAPWY3QJPS` (draft). Porter la machinerie prototype en natif — **la vraie douleur v1** — pour pouvoir retirer Depot :

- prototypes comme annexe du Spec (un seul par Spec ; rounds conservés comme rollback ; forme par revue d'usage, cf. PDR-0009) ;
- design-system imposé + **rendu proto** (Handlebars sidecar, Tailwind iframe, sync md5 ; cf. PDR-0010) ;
- catalogues de composants **mappés** ;
- **distill → dev fidèle** (le contrat de placement exécuté sans réinvention).

Le **rôle prototype léger** + son entrée (`/nspec` propose un proto quand c'est graphique) sont déjà livrés en **v2.5.8** ; ici on livre la machinerie de rendu / mapping / distill.

**Gate :** un proto élu se distille en un dev fidèle au design retenu, sans Depot.

## v2.8 - Harnais customisable (rules / checks / agents / workflows + visualiser)

PRD `01KY4DQS06B939Z1CM8CPE7TW0` (draft). Livre PDR-0011/0019/0020 en entier : le triptyque **rules / checks / agents** (ce que Depot entassait dans « directives ») devient de la **donnée éditable**.

- **Défauts nyx clé-en-main** : les 6 rôles hardcodés en Rust deviennent des templates DB seedés + un workflow par défaut qui tourne sans config ;
- **agents forkables** par projet + **subagents custom** réutilisables cross-projet (ui-tester…), observés (fini le sous-agent caché) ;
- **rules** (table `directives`) advisory, catégorisées, injectées par scope via marqueur — lean (faits transverses ; le comportement de rôle vit dans le template) ;
- **checks** = `managed_commands.gate` déterministes, blocking, scopés (robustes aux renames : ce sont des commandes, pas des ancres fichier) ;
- **workflows** composables + wiring (subagent / check → scope), assignés via `projects.workflow_id` ; lancement / observation des subagents **abstrait par provider** (fallback = session nyx observée, aucun angle mort) ;
- **authoring 100% MCP** + **VISUALISER UI** requis. Édition UI directe = itératif, plus tard.

Le **design-system rendu** reste en v2.7 ; ici c'est l'orchestration / rules / checks / agents.

**Gate :** un projet neuf tourne sur le workflow défaut ; un scénario customisé (rule + subagent custom wiré + check gate) se reflète dans les contextes rendus, l'exécution observée et le visualiser.

## Autres capacités avancées (à cadrer, post-v2.9)

Non encore tranchées : milestones / catégories / graphes de dépendances avancés ; automatisations ; diagnostics et outils de réparation ; import/export et sauvegardes de longue durée. Chaque sous-système Depot reçoit une décision explicite — **porter, simplifier, remplacer ou supprimer** — jamais copié par défaut.

## v2.9 - Migration finale et extinction de Depot

- importer de façon idempotente projets liés, Spec, révisions, tâches, reviews, idées, ADR, directives, documents et pièces jointes retenues ;
- gérer les chemins historiques WSL/Windows et les identités déjà présentes dans nyx ;
- produire un rapport de migration et un backup restaurable ;
- vérifier la parité des contextes agents ;
- dogfood prolongé sans retour nécessaire vers Depot ;
- retirer le binaire, daemon, Hono, UI, CLI, MCP et la configuration Depot ;
- archiver le repository Depot en lecture seule.

**Gate final V2 :** nyx couvre les workflows retenus, les données sont migrées, Depot n'est plus requis et il n'existe qu'une seule application et une seule autorité métier.

## Tardif — Délégation & reprise cross-machine (handoff continuable)

**Status: capturé (draft)** — Spec `01KXJS1PMAXT989QGPN34TA8S8` · **position PROVISOIRE**

> Direction validée avec l'utilisateur, figée pour ne pas la perdre. **Un des derniers chantiers** : dépend de tout le socle natif (Spec + knowledge + `domain_events` + harnais v2.5.8) **et** de l'extinction de Depot (v2.9). D'autres tranches non encore définies viendront probablement **avant** — d'où la position provisoire.

- **Besoin** : déléguer un job de dev agent continuable à une machine toujours allumée (serveur maison h24), éteindre son PC, laisser avancer, rapatrier le résultat pour le tester. **Relais séquentiel**, pas de sync concurrente.
- **nyx = primitives locales pures** (0 réseau/secret) : artefact **auto-contenu** (état Spec/tâches/events/ideas/docs **+** code delta = bundle git + dirty + untracked) · `export`/`import` · **bail** (X en lecture seule sur l'origine pendant la délégation) · **réconciliation pilotée par agent** (union additive + escalade humaine, pas de moteur de merge).
- **Reprise** = un agent frais `ndev` reprend les tâches ouvertes depuis l'état importé (déjà v2.5.8 + `domain_events`).
- **Transport = skill SÉPARÉ** (hors nyx) : bouge l'artefact (SSH au choix), déclenche import + `ndev`, rapatrie ; possède connexion + secrets. nyx ne touche jamais au réseau.
- **Env** = parité **manuelle** sur serveur pré-configuré (nyx détecte/prévient le mismatch, ne provisionne pas).

**Gate :** éteindre le PC en cours de Spec → le serveur reprend et avance seul → rapatriement absorbé par union sans conflit — sans que nyx ne fasse de réseau.

## Ordre strict

```text
v2.0 consolidation Electron/Rust
-> v2.1 modèle produit
-> v2.2 travail actif (workspace, travail libre ; Attention Inbox différée)
-> v2.2.5 persistance terminal (livré)
-> v2.3 terminal en blocs (honor-the-shell)
-> v2.4 Spec/tâches/contextes MCP
-> v2.5 diff/review
-> v2.5.5 sessions agent (reprise/cycle de vie)
-> v2.5.6 cockpit (close-warning, CRUD, stats non-inline, reorder dnd-kit)
-> v2.5.7 spec révisions (cliché complet, tâches versionnées)
-> v2.5.8 contextes agents (livraison harnais, multi-provider)
-> v2.5.9 reprise sessions (retrait carte Interrupted/Resume, crash = arrêt propre)
-> v2.5.10 surcouche terminal↔spec (overlay live dev + création)
-> v2.5.11 boucle review (findings review_tasks + rework en rounds + retour-chat)
-> v2.6 knowledge
-> v2.7 machinerie prototype
-> v2.8 harnais (rules/checks/agents/workflows forkables + visualiser)
-> v2.9 migration et suppression Depot
```

## Règle de contrôle du scope

Une tranche `v2.x` ne doit pas aspirer les suivantes. En particulier :

- v2.1 ne construit pas toute l'UI Spec ;
- v2.2 ne réimplémente pas la review ;
- v2.5 ne copie pas le lifecycle Depot ;
- v2.4 ne copie pas prototypes et documents ;
- v2.7 ne devient pas un prétexte pour porter tout le legacy.

La roadmap peut évoluer, mais **tout ce qui a été retenu pour la super-app appartient à la V2**, jamais à une hypothétique V3 utilisée comme parking.

## Terminal « blocks » — voir la tranche v2.3

Le chantier « terminal en blocs » **est** la tranche **v2.3 ci-dessus** (parkée, branche
`feature/v2.3-terminal-blocks` non mergée). Le détail **fait / reste à faire / leçon** est consolidé dans
[`session-notes.md`](session-notes.md) — il n'existe **plus** deux « v2.3 » concurrents ni de note de version
contradictoire.
