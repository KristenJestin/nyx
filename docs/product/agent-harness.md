# Harness agentique et contextes

## État

**Status: mécanisme validé (PDR-0011, PDR-0019, PDR-0020, PDR-0031). Le texte livré des manuels vit dans [`agent-manuals.md`](agent-manuals.md). Reste à écrire : le contenu par projet (rules, agents custom, DESIGN.md).**

Ce document rassemble tout ce qui touche au comportement des agents dans nyx : le principe fondateur, le harness, et où chaque contexte se branche. Il sépare ce qui est **posé** (le mécanisme) de ce qui reste **à écrire** (le contenu réel des contextes). Le texte des manuels lui-même — `/nspec`, `/ndev`, `/ndoc` et la charte des sous-agents — n'est pas recopié ici : il vit dans [`agent-manuals.md`](agent-manuals.md).

## Principe fondateur : l'humain lâche brut, l'agent grille, l'humain valide

L'humain n'est pas bon pour produire des artefacts structurés directement. Il **lâche brut**, comme il le voit ; l'**agent grille** ce brut (l'interroge, le clarifie, le structure) pour en sortir une version cohérente ; l'**humain valide**.

Ce motif traverse tout nyx :

- **conception Spec** : idée vague -> grill -> Spec cohérent ;
- **review** : commentaire brut -> grill -> finding cohérent (PDR-0012) ;
- c'est littéralement le motif d'une session de grilling.

Bénéfice : friction minimale pour l'humain (il ne classifie jamais à la main) et contrôle conservé (il valide la sortie). Le grill est une **behavior de playbook**, pas un moteur.

## Le harness : playbook + verbes (rappel PDR-0011)

nyx ne construit **pas** de moteur de workflow. Le playbook est du **texte injecté comme instructions à l'agent** ; l'agent le lit, raisonne, et **exécute** en appelant les **verbes observables** de nyx. nyx tient l'état et les verbes ; l'agent décide de la marche à suivre. Le moteur d'orchestration behavior-tree qui contredisait ce principe a été construit en v2.8 puis **retiré** (PDR-0031) : il n'y a plus de graphe, plus de run, plus de tick.

- **Verbes (primitives v1)** : lancer/observer une session avec un rôle, injecter du contexte, hooks de cycle de vie (start/close), transitions de Spec et de tâche.
- **Playbook (donnée)** : le manuel lui-même, rendu live par nyx-core et injecté par les hooks.
- Frontière : *nyx possède les verbes, le manuel dit quoi en faire.*

## Les manuels livrés

Un **manuel par commande** (`/nspec`, `/ndev`, `/ndoc`), injecté au `UserPromptSubmit`, plus une **charte unique** injectée dans **chaque** sous-agent au `SubagentStart` — quelle que soit la mission que l'orchestrateur lui a écrite. Le texte livré, mot pour mot, est dans [`agent-manuals.md`](agent-manuals.md) ; il n'est pas dupliqué ici.

Rôles rendus : **spec**, **dev** (orchestrateur), **doc** — trois, un par commande. Coder, auditeur et prototypeur ne sont plus des rôles nyx : ce sont des **sous-agents** que l'orchestrateur brieffe lui-même via le Task du provider, encadrés par la charte et **observés** par nyx (hooks `SubagentStart` / `SubagentStop`) — pas d'angle mort, même quand ils tournent dans le Task du provider.

La seule machine à états qui subsiste est celle du **Spec** (`draft → ready → in_progress → in_review → completed`, plus `blocked`, `cancelled`, `archived`), avancée par `spec_transition` ; les transitions sensibles (`ready`, `completed`, `cancelled`) exigent `user_confirmed=true`, donc une phrase claire de l'humain dans le chat.

## Contexte injecté, par agent

- **Design-system** (table `design`, format DESIGN.md) : vocabulaire visuel imposé, pour le proto **et** le dev (PDR-0010). L'agent compose depuis ce vocabulaire, il ne réinvente pas.
- **Directives** (table `directives`) : fragments de contexte projet -> constituent le playbook (« directives v2 »).
- **`agent_context_snapshots`** (table) : où les contextes générés sont tracés.
- **Manuel du rôle et charte de sous-agent** : le texte fixe vit dans [`agent-manuals.md`](agent-manuals.md) ; les blocs `{{…}}` (Spec, tâches, workspace, commandes, catégorie) sont rendus live par nyx.

## Le modèle d'exécution : agents, déclenchement, livraison (PDR-0019 + PDR-0020 + PDR-0031)

**Partage en 3 couches (PDR-0019).** (1) **Verbe nyx** = exécution + autorité de l'état (worktree, status, diff, lancement terminaux/commandes/sessions). (2) Les **inputs** d'un verbe = humain (UI) ou agent (MCP) — même use case (PDR-0005). (3) Le **travail** (juger, coder, auditer, documenter) = **session agent observable**. nyx n'est pas un moteur.

**Agents — bibliothèque réutilisable (PDR-0020).** **spec**, **dev** (orchestrateur), **doc** = les **principaux**, un par commande, **déclenchables**. Les **sous-agents** sont **lancés par les principaux via le Task natif du provider** (pas de système nyx, PDR-0019) et **observés** par nyx via les hooks `SubagentStart`/`SubagentStop` ; ils reçoivent la **charte** de nyx et leur **mission de l'orchestrateur**, pas un rôle nyx. nyx livre les défauts, **forkables** ; les customs sont **réutilisables entre projets**. Contenu d'un agent = **template** (statique + marqueurs `rules`/`checks`/données live) + **outils/skills** + modèle.

**Pas de composition de workflow (PDR-0031).** La composition réutilisable (1 spec + 1 dev + 1 doc + N subagents), son wiring et `projects.workflow_id` sont **retirés** : il n'y a rien à câbler entre trois manuels et une charte.

**rules / checks / agents** (ce que Depot entassait dans « directives »). **rules** = consignes advisory projet (table `directives`), injectées dans un agent. **checks** = commandes managées déterministes ; leur **note** dit quand l'humain veut qu'elles tournent, et c'est cette note — rendue dans `list_commands` et dans le contexte injecté — que l'agent lit, à la place des slots de gate retirés avec le moteur. **agents** = les rôles. La *visual pass* du projet devient un **sous-agent** briefé par l'orchestrateur — observable, fini le sous-agent caché.

**« Démarrer le dev » = squelette générique.** Verbe worktree (repos non-`optional` par défaut) → ligne workspace + `specs.workspace_id` + status → managed commands → session `dev` (+ playbook) → checks/hooks. **Points d'extension** projet (rejouent la préparation de feature) : **repo-set** (auto-inclusion des libs partagées), **setup hook** post-worktree (`.envrc` + `setup-feature.sh`), **dev playbook**, **managed commands/services**. Ce qui était câblé à la main (tabs de terminal, enregistrement du workspace, fichier de config maison) **disparaît** : natif + observé.

**Livraison du contexte — zéro CLI, zéro fichier généré (PDR-0020).** Le contenu (manuel du rôle + rules + Spec + tâches + workspace + commandes managées et leurs notes) est **rendu live par nyx-core** et **injecté via les hooks du plugin** en `additionalContext` (instruction, pas `tool_result`) : **SessionStart** (lancement UI), **UserPromptSubmit** (`/ndev` `/nspec` `/ndoc` tapé) et **SubagentStart** (la charte, dans chaque sous-agent). Les **déclencheurs** = 3 **stubs de commande fixes** portant un marqueur ; `$ARGUMENTS` → l'id sélectionne la spec, le **texte libre reste l'instruction** (injection additive) ; sans id → résolu via `cwd`. **MCP** = uniquement les **actions**.

**Les trois branches du hook, en détail** (`claude-plugin/hooks/nyx-session-hook.cjs`, un seul script node cross-plateforme) :

| hook | ce que nyx rend | forme de la réponse |
| --- | --- | --- |
| `SessionStart` | le contexte résolu du terminal (projet / workspace / Spec) | `{ hookSpecificOutput: { hookEventName, additionalContext } }` |
| `UserPromptSubmit` | le **manuel de la commande**, via `agent_context { role, cwd, spec_id? }` | `{ hookSpecificOutput: { hookEventName, additionalContext } }` |
| `SubagentStart` | la **charte**, portée par le `context` de l'`agent_session_event` que le hook poste déjà | `{ hookSpecificOutput: { hookEventName, additionalContext } }` |

Trois choses s'y jouent, et chacune est une décision, pas un détail d'implémentation :

- **Le déclencheur est le NOM de la commande**, parce que c'est tout ce qu'un `UserPromptSubmit` voit : le client ne passe que le prompt brut et ne développe le corps du stub — donc le marqueur `<!-- NYX-AGENT-CONTEXT role=… -->` — qu'après. La table `nspec → spec`, `ndev → dev`, `ndoc → doc` vit donc dans le hook, et le rôle est à lui seul ce qui sélectionne un manuel : le moteur parti, le `scope` ne nomme plus rien.
- **L'ancre est le `cwd`** que le client donne au hook, plus le `spec_id` quand l'humain a nommé une Spec — la référence est passée **verbatim**, jamais résolue côté hook. Et c'est `agent_context`, seul point d'entrée, qui **refuse** une référence ambiguë : ce refus appartenait au `run_start` du moteur, il a déménagé avec lui (PDR-0031). Un refus n'est pas avalé — la phrase de nyx est injectée à la place du manuel, parce que stderr n'est pas lu par le modèle.
- **`SubagentStart` prend la même enveloppe `hookSpecificOutput` que les deux autres** — et ce point a longtemps été écrit à l'envers ici, dans le hook et dans son test : on tenait la forme **nue** pour obligatoire, et l'enveloppe pour ce qui faisait *silencieusement* jeter la charte. **Mesuré le 2026-09-07**, Claude Code v2.1.246 sous Windows, en pilotant l'app réelle — un vrai sous-agent lancé depuis un vrai terminal nyx, plugin installé depuis ce build. L'observable est la pièce jointe **`hook_additional_context`** du transcript du sous-agent (`~/.claude/projects/<proj>/<session>/subagents/agent-*.jsonl`) : elle n'est écrite **que** si le texte a atteint le modèle. La forme nue n'a laissé qu'un `hook_success` portant le stdout du hook — il a donc bien tiré, et réussi — et **aucun** `hook_additional_context` ; le sous-agent interrogé a confirmé n'avoir reçu aucun contexte nyx. Dans le **même** transcript parent, les `SessionStart` et `UserPromptSubmit` **enveloppés** laissent chacun les deux pièces jointes, et les manuels qu'ils portaient ont été cités mot pour mot par le modèle. Conclusion : c'est l'enveloppe qui passe, la clé nue qui est jetée sans un mot. La forme écrite ici est celle du hook et de son gate ; ne pas la « restaurer » sur la foi d'un commentaire.

Le tout est **best-effort et borné** : le hook ne jette jamais et sort toujours en 0. Sidecar éteint, réponse mal formée, `agent_context` en refus — la session s'ouvre sans manuel plutôt que de casser, et les stubs disent à l'agent de **tirer** le manuel lui-même (`agent_context`) quand il ne l'a pas reçu. Les stubs ne portent plus que ça : le marqueur de rôle, la grammaire de référence 0/1/N, et le renvoi au manuel injecté — plus une ligne de `run_start` ni de `nyx_next`.

**Install.** Via **Settings → Integrations** (comme nyx v1) : installe le **plugin bundlé** `nyx-claude-integration` (MCP + hooks + stubs `/n…`), par provider. **Pas de CLI.**

**Pas de modes.** `specs.mode` (`stepped` / `autopilot`) n'est plus ni lu ni exposé : rien ne s'arrête à une fin de phase, puisque les phases ne sont que des étiquettes de regroupement. Le rythme vient du manuel, et le **gate humain final** est conservé — c'est l'humain qui prononce `completed`. La colonne reste en base jusqu'à un prochain nettoyage de schéma (PDR-0031).

## Aucun angle mort (le fil rouge)

Toute action de l'agent passe par une surface que l'utilisateur voit, sur le même état. L'agent a **deux surfaces** :

- le **terminal** (shell ouvert : il peut tout lancer) ;
- les **verbes MCP** (opérations structurées).

La lisibilité ne vient **pas** de la restriction de l'agent, mais de l'**observation** : nyx possède le pty et l'arbre de processus, donc même un `npm run dev` brut est vu, son port aussi. On ne contraint pas ce que l'agent peut faire ; on rend visible tout ce qu'il fait. C'est la raison technique pour laquelle nyx est un gestionnaire de terminaux **avant** tout.

## Doc-sync (phase doc post-dev)

Déclenchée par l'humain (`/ndoc`) une fois le Spec `in_review` ou `completed` — l'orchestrateur la **suggère** en fin de dev, il ne l'enclenche pas. C'est un **agent-job**, pas un nouveau sous-système : une session agent dans le workspace du Spec, sur sa plage Git, qui lit les règles de doc du projet (CLAUDE.md / AGENTS.md, index docs, conventions ADR) — nyx n'en impose aucune — et écrit dans le working tree. La sortie est **reviewable** (l'agent propose la doc, l'humain valide — pas d'angle mort sur la doc), sans commit sauf demande explicite.

## Où vit le texte livré

Les manuels `/nspec`, `/ndev`, `/ndoc` et la charte des sous-agents sont écrits **en toutes lettres** dans [`agent-manuals.md`](agent-manuals.md) : c'est la source de ce que le rendu de contexte injecte, et elle n'est pas recopiée ici pour ne pas se dédoubler. Ce document dit **où** le contexte se branche et **quel** partage il respecte ; `agent-manuals.md` dit **ce que l'agent lit**.

## Posé vs à écrire

**Posé (mécanisme + texte livré) :** le harness (manuel + verbes), les trois rôles et la charte des sous-agents, les principes (grill, aucun angle mort), où chaque contexte se branche (`design`, `directives`, `agent_context_snapshots`), et le **texte des manuels** dans [`agent-manuals.md`](agent-manuals.md).

**À faire (authoring / raffinage) :** affiner les manuels à l'usage, et écrire le contenu **par projet** : les `directives` spécifiques, les notes des commandes managées et le `DESIGN.md` de chaque projet. Ça, c'est de la donnée éditable, pas une décision figée.

**Reporté à la v3 (PDR-0031) :** sessions par étape, outils visibles par étape, validateurs configurables, empreinte des tours humains.
