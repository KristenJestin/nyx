# Produit nyx - Index canonique

## Rôle de ce dossier

Ce dossier est la mémoire produit durable de la prochaine génération de nyx.

- Les documents canoniques décrivent uniquement la vérité actuellement retenue.
- Les PDR (`decisions/`) expliquent pourquoi une décision a été prise ou remplacée.
- `open-questions.md` contient les hypothèses et sujets non tranchés.
- Une proposition ne devient pas canonique uniquement parce qu'elle apparaît dans une roadmap ou une conversation.

## Ordre de lecture

1. [Vision](vision.md)
2. [Modèle de domaine](domain-model.md)
3. [Architecture de l'information](information-architecture.md)
4. [Workflows](workflows.md)
5. [Harness agentique et contextes](agent-harness.md)
6. [Architecture technique](technical-architecture.md)
7. [Roadmap](roadmap.md)
8. [Questions ouvertes](open-questions.md)
9. [Décisions produit](decisions/)

Le schéma concret est dans [`data-model.dbml`](data-model.dbml). Le lexique des termes est dans [`glossary.md`](glossary.md). Le plan d'absorption de Depot est dans [`absorption.md`](absorption.md).

## Statuts

- **validated** : décision explicitement retenue.
- **proposed** : direction sérieuse à valider par conception, prototype ou dogfood.
- **investigating** : nécessite une recherche ou un POC.
- **rejected** : piste écartée, avec raison conservée.
- **superseded** : ancienne décision remplacée.

## Registre des concepts

ID    | Concept                                | Statut              | Source
----- | -------------------------------------- | ------------------- | ------------------------
C-001 | Nom du produit : nyx                   | validated           | PDR-0001
C-002 | Application unique nyx                 | validated           | PDR-0001
C-003 | Absorption puis disparition de Depot   | validated           | PDR-0001
C-004 | Backend métier dans un seul langage    | validated           | PDR-0002
C-005 | Backend métier Rust                    | validated           | PDR-0002
C-006 | Project comme conteneur durable        | proposed            | domain-model
C-007 | Workspace comme pivot intention/runtime | validated           | PDR-0008
C-008 | Repository explicite dans un workspace | superseded          | PDR-0013 (voir C-028)
C-009 | Execution comme entité première classe | rejected            | PDR-0008
C-010 | Spec et tâches natifs dans nyx          | validated           | PDR-0001
C-011 | Review Git générique                   | validated direction | PDR-0004
C-012 | UI et MCP sur le même état temps réel  | validated direction | PDR-0005
C-013 | Attention Inbox                        | proposed            | information-architecture
C-014 | Timeline unifiée                       | proposed            | information-architecture
C-015 | Workflow simple par défaut             | proposed            | vision
C-016 | Electron comme shell desktop unique    | validated           | PDR-0006
C-017 | Monorepo produit simplifié             | validated           | PDR-0006
C-018 | Tauri retiré de la plateforme active   | validated           | PDR-0006
C-019 | Backend Rust en sidecar supervisé      | validated           | PDR-0007
C-020 | IPC interne sans N-API ni core-host    | validated           | PDR-0007
C-021 | Spec ticket durable, pointeur workspace | validated           | PDR-0008
C-022 | Workspace = ligne permanente, jamais auto-supprimée | validated | PDR-0008
C-023 | Prototype = annexe du Spec, absorbé tard | validated           | PDR-0009
C-024 | Design-system imposé comme contexte agent | validated         | PDR-0010
C-025 | Rendu proto : Handlebars sidecar + Tailwind iframe | validated   | PDR-0010
C-026 | Harness = playbook éditable + verbes observables | validated direction | PDR-0011
C-027 | Review = review / comment / task ; finding = task | validated     | PDR-0012
C-028 | Repository logique ; pas de table de checkout    | validated     | PDR-0013
C-029 | Grill : l'humain lâche brut, l'agent structure, l'humain valide | validated | agent-harness
C-030 | Playbook par défaut (orchestrateur/coder/auditeurs/doc) | proposed | agent-harness
C-031 | Spec = unité humaine ; tâches = décomposition agent | validated | PDR-0014
C-032 | Clé Linear sur le Spec (préfixe projet + numéro) ; tâches sans clé | validated | PDR-0014
C-033 | Priorité = enum string, stockée sur Spec et tâche | validated | PDR-0014
C-034 | Phases = champ nullable sur la tâche, optionnel, posé par l'agent | validated | PDR-0014
C-035 | Board = Spec ; catégorie unique, pas de sprint/issue | validated | PDR-0014, PDR-0017
C-036 | Review = vue dérivée ; pas de table reviews | validated | PDR-0015
C-037 | Dispositions d'absorption Depot (lot 1) | validated | PDR-0016
C-038 | Annexe = concept Spec ; attachment = stockage interne | validated | PDR-0016
C-039 | Human action = task executor=human ; pas de table human_actions | validated | PDR-0014
C-040 | Catégorie unique par Spec (remplace les tags) ; l'agent assigne/minte (valide tel quel), l'humain cure le vocabulaire | validated (amendé 2026-09-02) | PDR-0017
C-041 | Unité humaine renommée PRD → Spec | validated | PDR-0018
C-042 | Partage app/agent/humain : verbe nyx / inputs humain-ou-agent / travail = session agent | validated | PDR-0019
C-043 | Start dev = squelette générique + points d'extension projet (repo-set, setup hook, playbook, commands) | validated | PDR-0019
C-044 | Mécanisme de hooks projet (directives enrichies) | superseded | PDR-0020
C-045 | Agents = bibliothèque réutilisable, 4 types (spec/dev/doc/subagent) | validated | PDR-0020
C-046 | Workflow = composition réutilisable (défaut nyx) ; projet → workflow | validated | PDR-0020
C-047 | rules / checks / agents (décompose les « directives » Depot) | validated | PDR-0020
C-048 | Contexte livré par hooks (SessionStart/UserPromptSubmit), zéro CLI ; install via Settings | validated | PDR-0020
C-049 | Mode par spec (stepped / autopilot) | validated | PDR-0020
C-050 | Human task surfacée dans l'onglet Tasks (badge, actionnable) ; Attention Inbox différée | validated | PDR-0021
C-051 | Phases nommées (liste ordonnée sur la spec) ; pas de table, statut dérivé | validated | PDR-0022
C-052 | Stockage = SQLite (statiquement liée au sidecar) ; couche requêtes = impl ; Turso différé | validated | PDR-0023
C-053 | Stockage = un seul fichier SQLite ; séparation des bounded contexts logique, pas physique | validated | PDR-0024
C-054 | Shell de navigation multi-pages (routeur TanStack, memory history) + terminal no-host (wterm/ghostty, store module-level) | validated | PDR-0025, PDR-0026
C-055 | Moteur de diff Git : gix primaire + fallback git CLI cohérent + `notify` (temps réel sans polling) | validated | PDR-0027

## Décisions validées à ce jour

1. Le produit garde le nom **nyx**.
2. La cible n'est pas « nyx avec Depot embarqué », mais un produit redessiné et unique.
3. Depot doit disparaître comme marque, binaire, daemon, serveur et interface autonome après migration de ses capacités utiles.
4. Spec, tâches, review, sessions agents, terminaux et services doivent partager le même modèle produit et être accessibles à l'UI comme au MCP.
5. Le backend métier final ne doit pas être partagé entre Rust et TypeScript.
6. Rust est l'unique backend métier. Le TypeScript du renderer et le JavaScript/TypeScript minimal d'Electron main/preload restent des adaptateurs, pas un second backend.
7. Electron est l'unique shell desktop de la V2. Tauri est retiré du build et de la maintenance active ; son historique reste récupérable dans Git.
8. Le backend Rust est un exécutable sidecar enfant, démarré et supervisé par Electron ; il ne survit pas à l'application et n'est pas un daemon.
9. Le renderer communique avec main/preload par l'IPC Electron allowlisté. Main communique avec le sidecar par des pipes locaux avec protocole versionné et framing explicite.
10. N-API et le `core-host` Node ne font pas partie de l'architecture cible.
11. Le repository reste un monorepo léger regroupant l'application desktop, le binaire Rust et les tests transverses.
12. La migration et la consolidation Electron doivent être terminées avant l'absorption fonctionnelle massive de Depot.
13. Le **workspace** est le pivot unique entre intention et runtime. `Execution` est rejetée comme entité : la relation Spec -> dev est une simple colonne `specs.workspace_id`, le runtime se rattache au workspace.
14. La ligne workspace est un enregistrement permanent ; `folder_removed_at` marque le nettoyage du worktree au merge, aucune suppression automatique. L'historique d'un Spec vit sur le ticket durable, pas sur l'éphémère.
15. Le **prototype** est une annexe du Spec (scope projet, sans workspace), absorbé tard (v2.7) : un seul par Spec, rounds conservés comme rollback, forme décidée par revue d'usage et non par lecture de schéma.
16. Le **design-system** est du knowledge projet stocké dans nyx (tables `design` au format DESIGN.md + `components`), imposé en contexte à l'agent pour le proto comme pour le dev. Rendu proto : Handlebars compilé dans le sidecar Rust, Tailwind exécuté localement dans l'iframe, sync incrémentale par md5 visuel. Electron et le renderer ne compilent rien (PDR-0010).
17. Le **harness agent** est un **playbook éditable par projet** (« directives v2 ») exécuté par l'agent via des **verbes observables** (lancer/observer une session, injecter du contexte, hooks). nyx ne construit **pas** de moteur de workflow. Les verbes sont des primitives v1 ; le harness est le payoff v2.7 (PDR-0011).
18. La **review** tient en 3 concepts : `review` (le diff d'un repo + ses commentaires, un par repo), `comment` (annotation ancrée/globale, résolution humain-seul), `task` (un commentaire promu pour le coder). **finding = task** ; le coder ne travaille que sur des tasks. Diff-engine et UI dans le Spec nyx « Viewer de gros diffs », v2.5 (PDR-0012).
19. **Multi-repo** : `repositories` est une identité logique persistée (chemin conventionnel + flag `optional` = nyxignore en DB) ; la table de checkout `workspace_repositories` est **supprimée**, l'appartenance dérivée et l'état Git relu depuis Git. Mono et multi partagent le modèle (distinction UI). Les commandes managées peuvent cibler un repo (PDR-0013).
20. Le **Spec est l'unité de travail de l'humain** : il valide le Spec, l'agent décompose en tâches (phases optionnelles via un champ `phase` nullable). Le Spec porte une **clé Linear** (préfixe projet + numéro, ex. `NYX-42`) plus son slug ; les tâches restent **sans clé** (ULID, agent). La **priorité** est un enum string (`urgent/high/med/low`) stocké sur Spec et tâche. Le **board liste des Spec** ; classification par **catégorie unique** (PDR-0017, remplace tags/labels), pas de « sprint »/« issue ». L'unité s'appelle **Spec** (renommée depuis « PRD » par PDR-0018, ferme Q-005). Une action humaine = une task `executor=human` (table `human_actions` supprimée), affichée dans l'**onglet Tasks** (badge « toi », actionnable) ; l'Attention Inbox est différée (PDR-0021).
21. La **review n'est pas une entité stockée** : c'est la **vue dérivée** du diff Git d'un repo + ses **commentaires**, ancrés sur `(workspace, repository)` avec `spec_id` optionnel (null = review générique). Pas de table `reviews`, pas de statut propre (il suit le Spec), pas de snapshot `pinned`. Un commentaire promu en task = finding (seul lien durable vers le Spec). Supersède la partie « entité review » de PDR-0012 (PDR-0015).
22. **Absorption Depot — lot 1** : `pending_actions`, `user_stories`, `out_of_scope_items`, `project_config`, `depot install`/slash-commands → **supprimés** (couverts ailleurs ou dans le corps du Spec) ; **handoff/multi-machine différé** (tardif) ; **activity log simplifié** en `domain_events` (capture généreuse, familles canoniques) ; **annexe** = concept produit du Spec, **attachment** = stockage interne (DBML : annexe = `spec_annexes`). Inventaire complet dans `absorption.md` (PDR-0016).
23. **Catégorie unique par Spec** : `specs.category_id` (FK unique) remplace les **tags** (`tags`/`spec_tags`/`task_tags` supprimées) ; libellés transverses (`breaking`/`security`) droppés. Seed par défaut de quatre catégories système par projet ; l'agent **assigne** depuis l'existant et, si rien ne colle, **minte** à la volée (non bloquant). *Amendement du 2026-09-02 (PDR-0017, section « Amendement (2026-09-02) — l'axe proposé/confirmé est retiré ») : ce que l'agent minte est **valide tel quel** — plus d'état « proposée », plus de geste `confirm`, plus de badge. Ce que l'humain cure, c'est le **vocabulaire**, depuis la section « Categories » des settings du projet : **créer / renommer / supprimer**, la suppression étant refusée sur les quatre slugs système et sur une catégorie encore portée par une Spec. L'anti-prolifération repose sur la directive « préfère réutiliser » et sur cette curation, jamais sur un verrou.* Pas de classification sur les tâches. Supersède le « vocabulaire tags » de PDR-0014 (PDR-0017).
24. **Renommage PRD → Spec** : l'unité humaine s'appelle désormais **Spec** partout (UI, docs, schéma : tables `specs`/`spec_revisions`/`spec_annexes`, colonnes `spec_id`/`spec_status`…). Clé, slug, priorité, catégorie, `executor` inchangés. Les PDR ont été **réécrits en « Spec »** (seul PDR-0018 garde « PRD » pour documenter le renommage). Supersède le choix terminologique de PDR-0014 (PDR-0018).
25. **Partage app / agent / humain** (PDR-0019) : **verbe nyx** (exécution + autorité de l'état : worktree, workspace, status, diff, lancement) / **inputs** d'un verbe = humain (UI) ou agent (MCP, même use case PDR-0005) / **travail** = session agent observable. « Démarrer le dev » = squelette générique (verbe worktree → workspace/status → managed commands → session agent + playbook → setup hooks) + **points d'extension projet** (repo-set rule, setup hook, dev playbook, managed commands) qui rejouent `feature-start`. Le câblage manuel (tabs Warp, enregistrement workspace, `.feature.json`) devient natif = la plus-value. Le **mécanisme de hooks** est tranché par PDR-0020 (décision 26).
26. **Fonctionnement agentique** (PDR-0020) : **agents** = bibliothèque réutilisable à **4 types** (spec/dev/doc principaux + subagent) ; un **workflow** (composition réutilisable, défaut nyx) câble 1 spec + 1 dev + 1 doc + N subagents, assigné via `projects.workflow_id`. Les « directives » Depot se décomposent en **rules** (`directives`), **checks** (`managed_commands.gate`), **agents**. Les **sous-agents = Task du provider**, observés via hooks. Le contexte est **rendu live par nyx-core et injecté via les hooks du plugin** (SessionStart / UserPromptSubmit) — **zéro CLI, zéro fichier généré** ; déclencheurs = 3 stubs fixes `/nspec` `/ndev` `/ndoc`. Install via **Settings → Integrations** (plugin bundlé, comme v1). `specs.mode` = stepped|autopilot. Résout Q-014.
27. **Surfaçage des human tasks** (PDR-0021) : une human task (`executor=human`) s'affiche dans l'**onglet Tasks** avec un badge « toi », **actionnable** par l'humain (`done`) — seule exception à la lecture seule des tasks ; l'agent l'annonce aussi en chat ; une task agent bloquée montre « bloquée par [human task] ». L'**Attention Inbox** (roll-up transverse de tous les signaux) n'est **pas un prérequis**, elle est différée (Q-007).
28. **Phases nommées** (PDR-0022) : les phases gagnent un **nom** via une **liste ordonnée sur la spec** (`specs.phases` = `[{number,name}]`) que `tasks.phase` indexe. **Pas de table de phases** (respecte PDR-0014), **statut de phase toujours dérivé** des tasks. Le proto nommait déjà les phases ; c'est désormais backé.
29. **Stockage** (PDR-0023) : moteur **SQLite** (SQLite-compatible), **compilé statiquement dans le sidecar** (pas de module natif per-OS — c'était un artefact Node/N-API, mort avec le sidecar Rust). La **couche requêtes/ORM** (Diesel/sqlx/…) est un **détail d'implémentation** (v2.0), pas une décision produit. **Turso (libsql/Turso DB) + Toasty = upgrade path différé** (sync = handoff différé), réversible. Ne résout pas Q-004 (fichier unique vs plusieurs).
30. **Stockage : un seul fichier SQLite** (PDR-0024, ferme Q-004) : toute l'autorité métier dans une base unique (transactions atomiques cross-contexte, mono-writer local WAL, backup/migration triviaux). Les bounded contexts = séparation **logique** (modules/schéma), pas physique. Le seul gain de fichiers séparés (sync indépendante) = handoff multi-machine, différé (PDR-0016). Réversible. **Décisions de cadrage v2.1** liées : le lien Spec→runtime (`specs.workspace_id` + table `specs`) est **réservé en v2.1, réalisé en v2.4** ; le temps réel des **vues dérivées de Git** (diff/dirty/branche) est **différé en v2.5** (Q-015), v2.1 ne pousse que les `domain_events` métier.
31. **Shell de navigation + stack terminal** (PDR-0025, PDR-0026) : le renderer adopte **`@tanstack/react-router`** (routing par code, `createMemoryHistory`, `BaseLayout` racine) pour un shell **multi-pages** (rail projets + section-nav + `<Outlet/>`) ; le seam hash `#/cockpit/…` est **supprimé** (on entre dans un **projet**, jamais dans un workspace). Le terminal bascule de **xterm** vers **wterm `@wterm/dom` + core ghostty** (une seule lib VT) avec un **store d'instances no-host** (Map module-level hors React/routeur) : detach/reattach du nœud DOM → **navigation lossless**, plus un **modèle d'écran VT headless** sidecar pour un **restart lossless**. `@wterm/*` épinglés en exact (0.x jeune). Livrés en v2.2.5.
32. **Moteur de diff Git v2.4** (PDR-0027) : **`gix` primaire** (pur-Rust, `default-features = false`, features serrées, pas de toolchain C) + **fallback `git` CLI cohérent** (porcelain `-z` partagé pour index/worktree, cohérence gix↔CLI **testable**), multi-repo sans mélange d'identités, review = vue dérivée non stockée (living/pinned). Rafraîchissement temps réel **sans polling** via **`notify`** (inotify/FSEvents/RDCW + debounce + allowlist `.git`) — tranche Q-015. Développé sur `v2.4-review`, à greffer en v2.5.

## Discipline de mise à jour

Lorsqu'une question est tranchée :

1. créer ou mettre à jour un PDR ;
2. mettre à jour le document canonique concerné ;
3. mettre à jour le registre ci-dessus ;
4. fermer ou déplacer la question dans `open-questions.md` ;
5. seulement ensuite créer les Spec d'implémentation.
