# Journal — retours terrain du 2026-09-07

Orchestration : ce fichier tient l'état des chantiers issus du test en conditions réelles du
2026-09-07. Un chantier = une branche `feat/terrain-*` dans un worktree, mergée en squash sur
`v2` une fois validée (suites vertes **et** observée dans l'app réelle).

Légende : ⬜ à faire · 🟦 en cours · 🟨 livré par l'agent, à valider · ✅ validé et mergé · ⛔ bloqué

## Chantiers

| # | Chantier | Branche | Statut | Notes |
|---|---|---|---|---|
| T | Terminal : retrait idle-unload, « Relancer » sur pane sorti, log sidecar, liens cliquables | `feat/terrain-terminal` | ✅ | squash `22b006b` ; libellés UI en anglais (Exited / Relaunch / Open link) |
| S | Scope Système : 3e scope, rail/nav/dots, All projects sans dot, clic terminal Système → `/system?terminal=`, palette | `feat/terrain-scope` | ✅ | renverse le finding U1 (validé par l'humain) ; squash `398b4b9` |
| C | Commandes : note pour l'agent, clé projet éditable, « Détails » depuis la sidebar, sections repliables | `feat/terrain-commands` | ✅ | squash `5a21843` ; deux conflits de fusion avec S résolus |
| W | Moteur de workflow → manuels en prose (`docs/product/agent-manuals.md`), trous MCP, hook, charte SubagentStart | `feat/terrain-workflow` | ✅ | validé par l'humain le 2026-09-07 ; squash `2fd5f54` ; 7 conflits résolus, note des commandes câblée, CRLF normalisé |

## Points de validation

Cochés par l'orchestrateur, jamais par un sous-agent.

### T — Terminal
- [x] idle-unload retiré (renderer + Rust), plus aucun kill sur inactivité
- [x] pane dont le process est sorti : barre « Terminé, code N · Relancer », respawn dans le même pane
- [x] fichier de log sidecar (chemin, rotation simple), traces des fermetures de PTY avec raison
- [x] liens http(s) cliquables dans xterm, OSC 8 et regex, ouverture externe, entrée menu contextuel
- [x] tests unitaires + navigateur + Rust ; observé dans l'app réelle (captures `.shots/`)

### S — Scope Système
- [x] scope `{kind:"system"}` ; aucune tuile projet sélectionnée en Système ; nav projet masquée
- [x] dot projet visible dès que sa page n'est pas à l'écran ; All projects sans dot
- [x] clic terminal/service Système depuis All projects → `/system?terminal=` ; palette alignée
- [x] tests ; observé dans l'app réelle

### C — Commandes
- [x] colonne `description`, formulaire projet + Système, rendue dans SessionStart et `list_commands`
- [x] clé projet éditable (settings), unique, majuscules, avertissement si Specs existantes
- [x] menu « Détails » sur une ligne de commande de la sidebar → modal ouvert sur la commande ; survol
- [x] sections Terminals et Commands repliables, animation `CollapsibleSection`, état persisté
- [x] tests ; observé dans l'app réelle

## Journal
- 2026-09-07 : worktrees créés, trois agents lancés en parallèle (opus).
- 2026-09-07 : workflow validé (tâches gardées dans nspec, phases = étiquettes, plus de mode stepped, audit unique en fin, charte SubagentStart). Quatrième agent lancé.

### W — Workflow
- [x] moteur, authoring, seed, simulateur, page Engine, tests e2e moteur supprimés ; migration de drop des six tables ; `check`/`test` verts
- [x] `spec_transition` routé et décrit ; boutons de transition dans l'UI Spec ; `user_confirmed` sur ready/completed/cancelled
- [x] `agent_context` : rôles spec/dev/doc, ancrage spec_id/workspace_id/cwd, blocs `{{tasks}}`/`{{commands}}` ; manuels conformes à `agent-manuals.md`
- [x] hook : plus de `run_start`, rôle par commande, charte `SubagentStart` ; stubs réécrits ; e2e d'injection verts
- [x] docs : PDR-0031, PDR-0020 superseded, agent-harness/workflows/roadmap/data-model/open-questions/CLAUDE.md alignés ; `agent-manuals.md` en validated
- [x] observé : injection réelle dans une session Claude lancée dans un terminal nyx (nspec puis ndev sur une Spec de test), transition depuis l'UI
- 2026-09-07 : S validé (tsc, 2491 unit, home 13/13, cargo check, captures CDP) et mergé.
- 2026-09-07 : T validé (cargo check, sidecar 136/136, pty 18/18, tsc, tests exit-bar/links/respawn, captures CDP ; libellés passés en anglais) et mergé.
- 2026-09-07 : C validé (cargo check, Rust ciblés 28/28, tsc, 2501 unit sur branche, 2526/2527 unit sur v2 fusionné, le rouge vert en isolation ; captures CDP) et mergé.
- 2026-09-07 : W validé (core 1424/1424, sidecar 134/134, e2e core 43/43, contrat 77/77, tsc, 2420 unit ; injection réelle nspec/ndev et charte observées) et mergé. Quatre chantiers sur v2.
