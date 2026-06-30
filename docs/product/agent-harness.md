# Harness agentique et contextes

## État

**Status: mécanisme validé (PDR-0011, PDR-0019, PDR-0020) ; contenu par défaut des playbooks proposé, raffinable. Reste à écrire : le contenu par projet (rules, agents custom, DESIGN.md).**

Ce document rassemble tout ce qui touche au comportement des agents dans nyx : le principe fondateur, le harness, le playbook par défaut livré avec nyx, et où chaque contexte se branche. Il sépare ce qui est **posé** (le mécanisme) de ce qui reste **à écrire** (le contenu réel des contextes).

## Principe fondateur : l'humain lâche brut, l'agent grille, l'humain valide

L'humain n'est pas bon pour produire des artefacts structurés directement. Il **lâche brut**, comme il le voit ; l'**agent grille** ce brut (l'interroge, le clarifie, le structure) pour en sortir une version cohérente ; l'**humain valide**.

Ce motif traverse tout nyx :

- **conception Spec** : idée vague -> grill -> Spec cohérent ;
- **review** : commentaire brut -> grill -> finding cohérent (PDR-0012) ;
- c'est littéralement le motif d'une session de grilling.

Bénéfice : friction minimale pour l'humain (il ne classifie jamais à la main) et contrôle conservé (il valide la sortie). Le grill est une **behavior de playbook**, pas un moteur.

## Le harness : playbook + verbes (rappel PDR-0011)

nyx ne construit **pas** de moteur de workflow. Le workflow est un **playbook éditable** (du texte/directive injecté comme instructions à l'agent). L'agent l'**exécute** en appelant les **verbes observables** de nyx. nyx fournit un playbook **par défaut**, forkable par projet.

- **Verbes (primitives v1)** : lancer/observer une session avec un rôle, injecter du contexte, hooks de cycle de vie (start/close).
- **Playbook (donnée, v2.6)** : le workflow lui-même, éditable.
- Frontière : *nyx possède les verbes, le projet possède le playbook.*

## Le playbook par défaut (dev) — proposé

Le harness de base livré avec nyx. L'agent **orchestrateur** ne code pas, il orchestre :

1. lance un **coder** avec le contexte (Spec / phase ou round) ;
2. quand le coder a fini, lance des **auditeurs** ;
3. si les auditeurs remontent des findings -> relance le coder avec ces findings (un finding = une task d'un round) ;
4. reboucle coder -> auditeurs jusqu'à ce que les auditeurs passent ;
5. découpe par **phases** ;
6. au **close**, déclenche le **doc-sync**.

Rôles par défaut (PDR-0020) : **dev** (orchestrateur) + **spec** + **doc** = principaux ; **coder** et **auditeur(s)** = **subagents** lancés par `dev` via le Task du provider. Chaque rôle est **observé** par nyx (hooks `SubagentStart`/`SubagentStop` pour les subagents) — pas d'angle mort, même quand ils tournent dans le Task du provider.

Ce playbook par défaut est une conception **séparée et révisable** : sa forme est posée ici, son contenu exact (prompts) s'écrit et se valide à l'implémentation.

## Contexte injecté, par agent

- **Design-system** (table `design`, format DESIGN.md) : vocabulaire visuel imposé, pour le proto **et** le dev (PDR-0010). L'agent compose depuis ce vocabulaire, il ne réinvente pas.
- **Directives** (table `directives`) : fragments de contexte projet -> constituent le playbook (« directives v2 »).
- **`agent_context_snapshots`** (table) : où les contextes générés sont tracés.
- Le contenu réel est à **définir et valider par agent et sous-agent** (authoring v2.6), pas figé ici.

## Le modèle d'exécution : agents, workflows, déclenchement (PDR-0019 + PDR-0020)

**Partage en 3 couches (PDR-0019).** (1) **Verbe nyx** = exécution + autorité de l'état (worktree, status, diff, lancement terminaux/commandes/sessions). (2) Les **inputs** d'un verbe = humain (UI) ou agent (MCP) — même use case (PDR-0005). (3) Le **travail** (juger, coder, auditer, documenter) = **session agent observable**. nyx n'est pas un moteur.

**Agents — bibliothèque réutilisable, 4 types (PDR-0020).** **spec**, **dev** (orchestrateur), **doc** = les **principaux** (un de chaque par workflow, **déclenchables**) ; **subagent** = coder, auditeur, ui-tester… (autant que voulu). Les subagents sont **lancés par les principaux via le Task natif du provider** (pas de système nyx, PDR-0019) et **observés** par nyx via les hooks `SubagentStart`/`SubagentStop`. nyx livre des **défauts** (spec/dev/doc + coder/auditeur), **forkables** ; les **customs** (ui-tester, coder2) sont **réutilisables entre projets**. Contenu d'un agent = **template** (statique + marqueurs `rules`/`checks`/données live) + **outils/skills** + modèle.

**Workflow (PDR-0020).** Composition réutilisable (défaut nyx forkable) : 1 spec + 1 dev + 1 doc + N subagents + **wiring** (quel subagent/check à quel scope). `projects.workflow_id` assigne un workflow à un projet.

**rules / checks / agents** (ce que Depot entassait dans « directives »). **rules** = consignes advisory projet (table `directives`), injectées dans un agent. **checks** = commandes déterministes à un gate, blocking (`managed_commands.gate` : lint/test/secrets). **agents** = les rôles. La *visual pass* du projet devient un **subagent `ui-tester`** branché à `pre-handoff` — observable, fini le sous-agent caché.

**« Démarrer le dev » = squelette générique.** Verbe worktree (repos non-`optional` par défaut) → ligne workspace + `specs.workspace_id` + status → managed commands → session `dev` (+ playbook) → checks/hooks. **Points d'extension** projet (rejouent la préparation de feature) : **repo-set** (auto-inclusion des libs partagées), **setup hook** post-worktree (`.envrc` + `setup-feature.sh`), **dev playbook**, **managed commands/services**. Ce qui était câblé à la main (tabs de terminal, enregistrement du workspace, fichier de config maison) **disparaît** : natif + observé.

**Livraison du contexte — zéro CLI, zéro fichier généré (PDR-0020).** Le contenu (rôle + rules + workflow + spec) est **rendu live par nyx-core** et **injecté via les hooks du plugin** en `additionalContext` (instruction, pas `tool_result`) : **SessionStart** (lancement UI) et **UserPromptSubmit** (`/ndev` `/nspec` `/ndoc` tapé). Les **déclencheurs** = 3 **stubs de commande fixes** portant un marqueur ; `$ARGUMENTS` → l'id sélectionne la spec, le **texte libre reste l'instruction** (injection additive) ; sans id → résolu via `cwd`. **MCP** = uniquement les **actions**.

**Install.** Via **Settings → Integrations** (comme nyx v1) : installe le **plugin bundlé** `nyx-claude-integration` (MCP + hooks + stubs `/n…`), par provider. **Pas de CLI.**

**Modes.** `specs.mode` = **stepped** (validation par phase) | **autopilot** (déroule jusqu'à la review finale). Gate humain final conservé.

## Aucun angle mort (le fil rouge)

Toute action de l'agent passe par une surface que l'utilisateur voit, sur le même état. L'agent a **deux surfaces** :

- le **terminal** (shell ouvert : il peut tout lancer) ;
- les **verbes MCP** (opérations structurées).

La lisibilité ne vient **pas** de la restriction de l'agent, mais de l'**observation** : nyx possède le pty et l'arbre de processus, donc même un `npm run dev` brut est vu, son port aussi. On ne contraint pas ce que l'agent peut faire ; on rend visible tout ce qu'il fait. C'est la raison technique pour laquelle nyx est un gestionnaire de terminaux **avant** tout.

## Doc-sync (phase doc post-dev)

Déclenchée par le **close** (un hook du playbook). C'est un **agent-job configuré**, pas un nouveau sous-système : un `profile` (recette : quoi lire, où écrire, style, guardrails) joue le rôle d'un managed-command ; le run est un agent-session dans le primary workspace, sur la plage Git du Spec ; la sortie est **reviewable** (l'agent propose la doc, l'humain valide — pas d'angle mort sur la doc).

## Playbooks par défaut — contenu proposé

Le contenu réel livré avec nyx. Proposé et raffinable, mais écrit pour ne rien perdre de ce qu'on a établi. Ce sont des **instructions**, pas du code ; éditables par projet.

### Orchestrateur (dev)

```text
Tu es l'orchestrateur. Tu n'écris pas de code toi-même ; tu pilotes les autres rôles via les outils nyx.

Pour réaliser un Spec :
1. Découpe le travail en phases si nécessaire.
2. Pour chaque phase / round :
   a. lance une session `coder` avec le contexte : le Spec, le round courant et ses tasks ouvertes ;
   b. quand le coder a fini, lance une ou plusieurs sessions `auditeur` sur le diff produit ;
   c. récupère les findings des auditeurs (déjà structurés) et les commentaires humains ; grille **uniquement les commentaires humains**, puis promeus le tout en tasks du round ;
   d. s'il reste des tasks ouvertes, relance le `coder` avec elles ;
   e. reboucle (c-d) jusqu'à ce que les auditeurs passent et qu'il ne reste plus de task ouverte.
3. À la clôture, déclenche le `doc-sync`.

Règles :
- lance chaque rôle via les outils nyx ; jamais de sous-agent caché ;
- tu ne résous jamais un commentaire humain : seul l'utilisateur le fait ;
- un commentaire ne devient un finding qu'après avoir été grillé en task cohérente.
```

### Coder

```text
Tu es le coder. Tu travailles UNIQUEMENT sur les tasks ouvertes de ton round / phase courant. Tu ne décides pas du périmètre.

Règles :
- pour toute UI, compose depuis le design-system du projet (composants + tokens) ; ne réinvente jamais un rendu ;
- tu n'adresses que des tasks ; tu ne résous pas les commentaires humains ;
- quand une task est faite, marque-la `done` ;
- toute commande ou service passe par les outils nyx, jamais un lancement sauvage.
```

### Auditeur

```text
Tu es un auditeur. Tu relis le diff produit par le coder. Tu signales, tu ne corriges pas.

Pour chaque problème :
- pose un finding (commentaire ancré sur le diff) avec une severité (critical / major / minor / info) ;
- sois précis et actionnable : le coder doit pouvoir corriger sans deviner.

Tu ne modifies pas le code. Tu ne résous pas les commentaires humains.
```

### Grill (idée / commentaire -> structuré)

```text
On te donne un input humain brut (une idée vague, un commentaire de review). Ta tâche : le transformer en artefact structuré et cohérent (une task de Spec, un finding) ; pas le recopier.

- clarifie les ambiguïtés ; au besoin, pose la question à l'humain ;
- déduplique, ordonne, ajoute des critères de done ;
- conserve l'ancre d'origine (file:line) si elle existe ;
- ne perds aucune intention de l'input ;
- pour un Spec, assigne une **catégorie** du set du projet ; si rien ne colle, crée-la (elle naît « proposée », l'humain curera) — ne bloque jamais (PDR-0017).

L'humain valide ensuite. Tu ne valides jamais à sa place.
```

### Doc-sync

```text
Déclenché à la clôture d'un Spec. Lis la plage Git du Spec (since..until) et les sources définies par le profile. Mets à jour la doc dans le repo cible selon le profile (style, audience, guardrails, topics).

- écris dans le working tree ; pas de commit auto sauf si le profile le demande ;
- ta sortie est une review : l'utilisateur valide avant que ça parte.
```

## Posé vs à écrire

**Posé (mécanisme + contenu par défaut) :** le harness (playbook + verbes), les rôles, les principes (grill, aucun angle mort), où chaque contexte se branche (`design`, `directives`, `agent_context_snapshots`), et le **contenu par défaut des playbooks ci-dessus**.

**À faire (authoring / raffinage) :** affiner ces playbooks à l'usage, et écrire le contenu **par projet** : les `directives` spécifiques et le `DESIGN.md` de chaque projet. Ça, c'est de la donnée éditable, pas une décision figée.
