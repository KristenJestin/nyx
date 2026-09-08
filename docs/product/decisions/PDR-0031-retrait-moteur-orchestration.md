# PDR-0031 - Retrait du moteur d'orchestration : des manuels en prose, pas un behavior tree

**Status: validated**

Retire de nyx le **moteur d'orchestration behavior-tree** livré en v2.8 (commit `4a2b07a`) et tout ce
qui en dépend : les tables `workflows` / `nodes` / `workflow_runs` / `gate_attempts` / `human_turns` /
`command_slots`, le verbe `nyx_next` et sa famille d'outils MCP, la page « Engine » du cockpit et le
simulateur de run. À la place, nyx injecte **un manuel en prose par commande** (`/nspec`, `/ndev`,
`/ndoc`) et **une charte unique dans chaque sous-agent** ; le texte livré est celui de
[`agent-manuals.md`](../agent-manuals.md). Supersède la partie « moteur » de PDR-0020 et rend à
PDR-0011 sa lettre : nyx ne construit pas de moteur de workflow.

## Context

Le moteur promettait une orchestration **déterministe et stockée en donnée** : un graphe de nœuds
(`sequence`, `loop`, `selector`, `spawn_subagent`, `gate_check`, `judge`, `transition`, `ask_human`,
`query`, `produce`, `fork_spec`, `sub_workflow`, `set_category`…) interprété par `Engine::tick`, un
run repris d'une session à l'autre par un traversal persisté, et une session agent réduite à un
**dispatcher mince** bouclant sur `nyx_next`. L'enforcement vivait dans les handlers de verbes : un
gate bloquant rouge refusait d'avancer, les transitions de Spec passaient par une autorité gardée, le
consentement humain appartenait à la session à qui la question était posée. C'était éditable,
visualisable, et rejouable sans agent grâce au simulateur.

Ce qu'il a coûté est mesurable : dix-sept types de nœuds et leur validation d'authoring, un pool de
workers MCP pour servir les requêtes du graphe, six tables d'exécution et leurs migrations, un
visualiseur de graphe et de blackboard, un jeu de graphes seedés versionnés (`BUILTIN_REVISION`, dont
le bump 25 a dû être **différé** parce qu'il aurait réinitialisé les runs vivants), un simulateur à
six scénarios avec son format JSON et son lanceur, et deux gates e2e. Chaque évolution du parcours —
ajouter une reproduction avant le fix d'un bug, changer l'ordre d'un audit — devenait une édition de
graphe, une révision de seed, une migration de compatibilité et une passe de tests.

Il ne tient pas, pour une raison qui n'est pas de la dette : **le graphe décide à la place de
l'agent, et il décide moins bien**. Un modèle sait découper un travail, lancer trois coders en
parallèle, s'arrêter quand un rapport sent mauvais, réordonner ce que la Spec avait mal coupé. Le
graphe, lui, ne connaît que la branche prévue à l'écriture. Tout ce qu'il gagnait en déterminisme, il
le payait en rigidité — et le déterminisme portait sur le **chemin**, pas sur le travail, qui restait
intégralement à la charge du LLM. Restait un dispatcher qui consomme un tour de contexte par tick
pour se faire dire quoi faire, là où un manuel de deux pages le lui dit une fois pour toutes.

## Decision

### Le moteur est retiré

Plus de `workflows`, plus de `nodes`, plus de `workflow_runs`, plus de `gate_attempts`, plus de
`human_turns`, plus de `command_slots`. Plus d'outils MCP `nyx_next`, `run_start`, `run_bind_spec`,
`run_list`, `run_delete`, `workflow_*`, `node_*`, `slot_bind`. Plus de page « Engine » dans le
cockpit. Plus de simulateur de run : il ne prouvait qu'un chemin de graphe, et il n'y a plus de
graphe.

### Des manuels en prose, un par commande

Le hook `UserPromptSubmit` injecte, sur `/nspec`, `/ndev` ou `/ndoc`, le manuel de la commande — le
rôle, la méthode, ce que l'agent peut et ne peut pas, rendu live avec la Spec, ses tâches, le
workspace et les commandes managées. Le hook `SubagentStart` injecte dans **chaque** sous-agent, quelle
que soit la mission que l'orchestrateur lui a écrite, une **charte unique** : le workspace, la Spec
ancrée, les commandes managées, ce qu'il ne décide pas, comment il rapporte. La mission vient de
l'orchestrateur ; nyx ne la lui dicte pas. Le texte des trois manuels et de la charte est celui de
[`agent-manuals.md`](../agent-manuals.md), source livrée, mot pour mot.

Les rôles rendus tombent de six (`spec`, `dev`, `coder`, `auditor`, `doc`, `prototype`) à **trois** :
`spec`, `dev`, `doc`. Coder, auditeur et prototypeur ne sont plus des rôles nyx — ce sont des
sous-agents que l'orchestrateur brieffe lui-même et que la charte encadre.

### `spec_transition` est l'outil exposé

`spec_transition` existait sans être routé ni décrit : aucune surface, MCP ou UI, ne pouvait faire
avancer un Spec autrement que par le moteur. Il est **routé, décrit et exposé**, et l'UI Spec gagne
ses boutons de transition. Les transitions sensibles — `ready`, `completed`, `cancelled` — exigent
`user_confirmed=true`, que l'agent ne pose qu'après une phrase claire de l'humain dans le chat.

### Le Spec est la seule machine à états qui reste

`draft → ready → in_progress → in_review → completed`, plus `blocked`, `cancelled`, `archived`. Il
n'y a plus d'état d'exécution parallèle : pas de run, pas de nœud courant, pas de blackboard, pas de
tour humain en attente. nyx tient **l'état et les verbes** ; l'agent raisonne. C'est exactement le
partage de PDR-0011 et PDR-0019, que le moteur avait débordé.

## Consequences

- **Schéma.** Une migration droppe `workflows`, `nodes`, `workflow_runs`, `gate_attempts`,
  `human_turns` et `command_slots`, ainsi que `projects.workflow_id`. `specs.mode`
  (`stepped`/`autopilot`) **reste en base** mais n'est ni lue ni exposée : plus rien ne s'arrête à une
  fin de phase, l'arrêt vient du manuel et du gate humain final. La colonne partira à un prochain
  nettoyage de schéma.
- **MCP.** La famille `run_*` / `workflow_*` / `node_*` / `slot_bind` / `nyx_next` disparaît de la
  surface. `spec_transition` y entre. `agent_context` cesse de s'ancrer sur `run_id` et s'ancre sur
  `spec_id` / `workspace_id` / `cwd` ; le refus de référence ambiguë vient désormais de lui. La note
  optionnelle d'une commande managée remplace les slots de gate : c'est elle qui dit à l'agent quand
  la lancer, rendue dans `list_commands` et dans le contexte injecté.
- **UI.** La page « Engine » et son visualiseur de graphe/blackboard sont retirés ; la section-nav
  d'un projet passe de six entrées à cinq.
- **Tests.** Le simulateur (`crates/nyx-core/examples/sim/`), ses six scénarios, ses scripts `sim` /
  `test:sim` et le gate `gate_v2_8_engine_e2e` disparaissent, avec le spike `nyx_next_loop_spike`.
- **PDR-0020 est partiellement superseded** : la composition workflow réutilisable, le wiring et
  `projects.workflow_id` tombent ; le reste (agents comme bibliothèque, décomposition
  rules/checks/agents, livraison par hooks, install par Settings → Integrations) tient.
- **Concept C-046** (« Workflow = composition réutilisable ; projet → workflow ») passe **superseded**.
  Un nouveau concept **C-056** (« manuels par commande ») le remplace au registre.
- **La règle d'or ne bouge pas**, et le défaut de PDR-0030 s'applique ici plus qu'ailleurs : ces
  manuels s'adressent à un agent, donc ils ne sont validés que par un agent réel sur un vrai client,
  jamais par une suite verte ni par un chemin de graphe rejoué.

## Amendement du 2026-09-08 — le gel de la Spec couvre le fork et la réouverture

**Constat de terrain.** Une Spec `in_review` a reçu les remarques de review de l'humain. L'agent
`/ndev`, plutôt que d'en faire des findings et de corriger le CODE, a **forké la Spec** puis l'a
rouverte en `draft` pour en réécrire le contenu jusqu'à ce qu'il colle à ce qu'il avait développé.
La garde B4 gelait bien les **sections** hors `draft`, mais `db::fork_spec` n'avait **aucune** garde
de statut et la transition `revise` (→ `draft`) n'était **pas sensible** : deux portes ouvertes sur
un contrat validé.

**Le verrou, dans l'autorité unique (`crates/nyx-core/src/db.rs`) :**

- `fork_spec` est **gardée par le même `require_spec_status`** que la garde B4 : autorisée depuis
  `draft` et `ready` **seulement**. Depuis `in_progress`, `in_review`, `blocked` ou un statut
  terminal, elle est refusée par `ContentWriteError::ForkForbidden` — un `invalid_state` dont le
  message **nomme la route juste** : un retour de review devient un **finding** (`task_add`
  `kind="review"` avec une `severity`, ou `review_task_structure` sur un commentaire promu), et ce
  qui change est le **code**.
- `SpecTransition::Revise` (→ `draft`) devient **sensible**. Les transitions sensibles sont donc
  **quatre** — `ready`, `completed`, `cancelled`, `draft` — sans exception à retenir : rouvrir une
  Spec en cours de build ou de review dégèle le contrat que l'humain a validé, donc c'est lui qui le
  décide, et l'agent transmet `user_confirmed=true`.

**Conséquence de doctrine.** La Spec est **gelée hors `draft`, fork et réouverture inclus**. Le
rework par fork reste la route de `/nspec` depuis `ready` ; au-delà, ce qui se corrige est le code.
Si l'agent pense que c'est la Spec qui a tort et non le code, il le **dit** à l'humain et s'arrête
là. Les manuels (`agent-manuals.md` §/nspec 5 et §/ndev 3, plus les interdits de `/ndev`) portent la
règle mot pour mot ; `workflows.md` (W-002) et `agent-harness.md` la répètent au niveau parcours.

### Ce qui attend la v3

Repris de la fin d'[`agent-manuals.md`](../agent-manuals.md) : **sessions par étape**, **outils
visibles par étape**, **validateurs configurables**, **empreinte des tours humains**. Ce sont les
quatre choses que le moteur essayait de rendre au prix d'un interpréteur ; elles reviendront comme
modèle v3, pas comme graphe.
