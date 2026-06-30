# PDR-0020 - Fonctionnement agentique : agents, workflows, rules/checks, déclenchement & install

**Status: validated**

Résout Q-014. Étend PDR-0011 (playbook, pas de moteur) et PDR-0019 (verbes / inputs / travail ; sous-agents = provider).

## Context

Revue complète du « fonctionnement » agentique, ancrée sur le concret : une commande maison de préparation de feature, ses directives (dont la *visual pass* qui spawne un sous-agent `agent-browser`), et le **plugin nyx v1** (`nyx-claude-integration` : MCP + hooks `SessionStart` / `UserPromptSubmit` / `SubagentStart` / `SubagentStop`, install via `Settings → Integrations`).

## Decision

### Agents — bibliothèque réutilisable, 4 types
- Quatre types : **spec**, **dev** (orchestrateur), **doc** (les trois **principaux**) et **subagent**.
- Un **workflow** définit **un** spec, **un** dev, **un** doc, et **autant de subagents** que voulu.
- **Subagents** = coder, auditeur, ui-tester… **lancés par les agents principaux** (surtout `dev`) via le **mécanisme natif du provider** (Task de Claude). nyx ne construit **pas** son propre système de sous-agents (PDR-0019) ; il les **observe** via les hooks `SubagentStart`/`SubagentStop` (déjà dans le plugin v1).
- Les agents sont une **bibliothèque réutilisable** : nyx livre des **défauts** (spec/dev/doc + subagents coder/auditeur), **forkables** ; on crée des **customs** (ui-tester, coder2), **assignés à un type**, **réutilisables entre projets**.
- **Contenu d'un agent** = un **template de prompt** (statique + marqueurs `rules`/`checks`/données live) + des **outils/skills** (ex. `agent-browser`) + un modèle.
- Seuls les **3 principaux** sont **déclenchables** (`/nspec` `/ndev` `/ndoc`) ; les subagents n'ont pas de commande → **pas d'explosion de fichiers**.

### Workflows — bibliothèque, défaut nyx
- `workflows` = composition réutilisable : (1 spec + 1 dev + 1 doc) + N subagents + **wiring** (quel subagent/check à quel scope). nyx livre un **workflow par défaut** (la base générique).
- `projects.workflow_id` assigne un workflow à un projet (le défaut sinon).

### Décomposition des « directives » Depot → rules / checks / agents
- **rules** = contexte advisory **par projet**, injecté dans un agent à un scope (« Angular v20 idioms »…) → table `directives`.
- **checks** = commandes **déterministes** à un gate, blocking (lint/test/secrets) → **managed commands + gate**.
- **agents/subagents** = les rôles (l'ex-« visual pass » devient un subagent `ui-tester` **observable**).

### Livraison du contexte — zéro CLI, zéro fichier généré
- Le contenu (rôle + rules + workflow + spec) est **rendu live par nyx-core** et **injecté via les hooks du plugin**, en **`additionalContext`** (instruction, pas `tool_result`) :
  - **SessionStart** → au lancement (bouton UI) ;
  - **UserPromptSubmit** → sur `/ndev` / `/nspec` / `/ndoc` tapé (TUI).
- **Déclencheurs** = 3 **stubs de commande fixes** (`/nspec` `/ndev` `/ndoc`), installés une fois avec le plugin, portant un **marqueur** ; `$ARGUMENTS` → l'**id sélectionne la spec**, le **texte libre reste l'instruction** (injection **additive**). Sans id → spec résolue via `cwd`.
- **MCP** = uniquement les **actions** (worktree, status, start command…). **Aucun CLI tapé par l'agent, aucun fichier de contenu généré/synchronisé.**

### Install
- Via **Settings → Integrations** (modal, comme nyx v1 : `integrations.rs` / `onboarding.rs`) qui installe le **plugin bundlé** `nyx-claude-integration` (MCP `.mcp.json` + hooks + les stubs `/n…`), **par provider** (`claude_code` ; `codex`/`opencode` à venir). **Pas de CLI d'install.**

### Modes
- `specs.mode` = **stepped** (l'orchestrateur s'arrête à chaque fin de phase pour validation) | **autopilot** (il déroule jusqu'à la review finale). Le **gate humain final** est conservé dans les deux.

### Grill
- Le grill reste **humain seul** (brut humain → structuré). Les findings d'un subagent auditeur sont déjà structurés → **pas de grill** dessus.

## Consequences

- Q-014 **résolue** (décomposition rules/checks/agents + mécanique de livraison).
- `data-model.dbml` : nouvelles tables `agents`, `workflows`, `workflow_agents` ; `projects.workflow_id` ; `specs.mode` ; `directives` recentrée sur les **rules** ; `managed_commands` gagne `gate` + `blocking` pour les **checks**.
- `agent-harness.md` réécrit (3 principaux + subagents, livraison par hooks, install, déclencheurs).
- Ajuste PDR-0016 : `depot install` n'est **pas** supprimé sèchement — **remplacé par le plugin nyx** (Settings→Integrations : hooks + stubs fixes), pas de CLI.
- Étend PDR-0011 (le workflow = playbook, ici structuré en agents/workflow) et PDR-0019 (verbes/inputs/travail ; sous-agents = provider).
