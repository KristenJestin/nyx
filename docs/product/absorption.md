# Plan d'absorption (nyx v1 + Depot → nyx V2)

## État

**Status: living — dispositions du lot 1 validées (PDR-0016)**

Ce document transforme « Q-009 à auditer » en plan traçable. Il mappe, sous-système par sous-système, ce que fournissent les deux sources (le runtime **nyx v1** et l'intention/knowledge **Depot**) vers leur disposition dans la V2.

**Méthode (Q-009) :** la forme d'absorption se décide par **revue d'usage avec l'utilisateur**, jamais par lecture de schéma. Un schéma dit ce qui a été construit, pas ce qui est utilisé.

**Quatre verbes :**

- **porter** — la capacité devient native nyx, telle quelle ou réécrite ;
- **simplifier** — portée mais réduite à l'essentiel ;
- **différer** — utile mais repoussée à une tranche tardive ;
- **supprimer** — non reprise (couverte ailleurs, ou hors produit).

## Source A — nyx v1 (le runtime)

Core Rust shell-agnostique + shells + renderer. Globalement **la fondation** : presque tout est porté.

| Sous-système | Disposition | Tranche | Décision |
| --- | --- | --- | --- |
| PTY / terminaux (portable-pty, flow-control lossless) | porter | v2.0 | validated |
| Managed commands (start/stop/relaunch, import package.json) | porter | v2.0 / v2.2 | validated |
| Projects / workspaces (git-agnostic) | porter (refondu) | v2.1 | PDR-0008/0013 |
| Agent sessions Claude record/resume (plugin SessionStart, `NYX_TERMINAL_ID`) | porter | v2.2 | validated |
| MCP server HTTP | porter (dans le sidecar) | v2.0 | PDR-0007 |
| OSC 7 / OSC 133 / détection portless | porter | v2.0 / v2.2 | validated |
| `nyx-core` Rust + SQLite | porter (base) | v2.0 | validated |
| Renderer React partagé | porter (embarqué Electron) | v2.0 | PDR-0006 |
| Shells **Tauri** + **napi-rs** + **core-host** Node | **supprimer** | v2.0 | PDR-0006 / PDR-0007 |

## Source B — Depot (l'intention / knowledge)

CLI + web UI Node, ~39 tables. Le **domaine** est porté ; les **enveloppes** (web/CLI/serve) sont supprimées.

| Sous-système / table | Disposition | Tranche | Décision |
| --- | --- | --- | --- |
| Spec + révisions | porter (natif) | v2.4 | PDR-0014 |
| Tasks (slice/human/review) + dépendances | porter | v2.4 | PDR-0014 |
| Reviews / comments / anchors | **simplifier** (vue dérivée) | v2.5 | PDR-0012 / PDR-0015 |
| `review_comment_tasks` (findings) | porter | v2.5 | PDR-0012 |
| Prototypes (pages/versions/variants/rounds/feedback/design) | **différer** | v2.7 | PDR-0009 / PDR-0010 |
| `spec_design_lock` / `round_page_design` | différer (sous prototype) | v2.7 | PDR-0010 |
| Annexes (`spec_annexes`) | porter (concept **annexe**) | v2.4 / v2.6 | PDR-0016 |
| Ideas | porter | v2.6 | proposed |
| ADR | porter | v2.6 | proposed |
| Directives | **décomposer** → rules (`directives`) + checks (commandes managées ; leur note dit quand les lancer) + agents | v2.4 / v2.7 | PDR-0011 / PDR-0020 / PDR-0031 |
| Documents / `doc_profiles` / `doc_sync_runs` | porter (doc-sync = agent-job) | v2.6 / v2.7 | agent-harness |
| Milestones | porter | v2.7 | proposed |
| Tags | **remplacés** par catégorie unique (`categories`) | v2.4 | PDR-0017 |
| Context commands (dev/coder/auditor) | porter : rendus **live par nyx-core**, injectés via **hooks** (zéro CLI) | v2.4 | PDR-0020 |
| Stockage de fichiers (`attachments`) | porter (**interne**, dédup) | v2.4 | PDR-0016 |
| Activity log → timeline | **simplifier** (`domain_events`) | v2.6 | PDR-0016 / Q-008 |
| `pending_actions` | **supprimer** | — | PDR-0016 |
| `user_stories` | **supprimer** (dans le Spec) | — | PDR-0016 |
| `out_of_scope_items` | **supprimer** (dans le Spec) | — | PDR-0016 |
| `project_config` | **supprimer** (réglages typés) | — | PDR-0016 |
| Handoff / lineage (sync cross-machine) | **différer** (tardif) | post-V2 | PDR-0016 |
| Web UI (React/TanStack) | **supprimer** | v2.8 | validated |
| CLI (citty) | **supprimer** | v2.8 | validated |
| `serve` (Hono) / API HTTP | **supprimer** | v2.8 | validated |
| `depot install` (slash-commands) | **remplacer** : plugin nyx (Settings→Integrations) — hooks + stubs `/n…` fixes, zéro CLI | v2.0 / v2.4 | PDR-0016 / PDR-0020 |

## Familles d'événements (`domain_events.event_type`)

Capture **généreuse** d'événements métier (jamais le runtime octet par octet) :

- **Spec** : `created`, `revision_published`, `status_changed`, `priority_changed`, `category_set`, `milestone_set`, `dependency_changed`, `workspace_assigned`, `completed`, `archived`
- **Category** (PDR-0017 — la catégorie est une ENTITÉ de projet, pas un champ du Spec ; son cycle de vie a donc ses propres événements, distincts du `spec.category_set` qui n'enregistre qu'une affectation) : `created` (mintée à la volée par un agent — source `mcp` — ou créée par un humain depuis les settings — source `ui`), `renamed` (un humain renomme ; le slug, lui, ne bouge jamais), `deleted` (un humain retire une entrée du vocabulaire ; refusé sur les quatre catégories système et sur une catégorie encore portée par une Spec). L'événement `confirmed` a été **retiré** le 2026-09-02 avec l'axe proposé/confirmé (amendement de PDR-0017) : ce que l'agent minte vaut validation.
- **Task** : `created`, `status_changed`, `phase_changed`, `priority_changed`, `agent_assigned`, `done`, `blocked`
- **Review** : `comment_created`, `comment_replied`, `comment_resolved`, `comment_reopened`, `comment_outdated`, `comment_promoted`
- **Runtime** : `workspace_created`, `workspace_folder_removed`, `terminal_opened`, `terminal_closed`, `command_started`, `command_succeeded`, `command_failed`, `agent_started`, `agent_waiting`, `agent_ended`, `agent_resumed`, `agent_resume_failed`
- **Knowledge** : `idea_created`, `idea_promoted`, `adr_created`, `adr_accepted`, `document_created`, `document_revised`, `directive_changed`
- **Human** (task `executor=human`, ex-`human_actions` foldé dans `tasks` — PDR-0021) : réutilise les événements **Task** (`created` / `done` / `blocked`) ; pas d'événement `human_action_*` séparé (plus d'entité dédiée)
- **Prototype** : `round_presented`, `variant_elected`, `feedback_created`, `feedback_resolved`
- **Migration** : `import_started`, `import_completed`

## Reste à scoper

Les lignes **porter** ne sont pas encore détaillées au niveau implémentation ; chacune sera scopée dans sa tranche v2.x (mapping de champs, garde-fous, validations). Les dispositions **différer** (prototype, handoff) sont rouvertes à leur tranche. Q-009 reste ouverte pour ce détail ; seules les **dispositions** du lot 1 sont figées (PDR-0016).
