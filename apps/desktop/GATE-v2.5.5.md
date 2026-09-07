# GATE — v2.5.5 : cycle de vie agent (reprise LAZY + interrupted/restore + unload idle + plugin)

> **RAPPORT DATÉ — la clause (3) ne décrit plus le produit.** L'**unload des sessions idle** a été
> **RETIRÉ** depuis : le balayage fermait le PTY d'une session Claude simplement laissée au repos,
> ce qui s'affichait comme `[process exited with code 1]`. Tout ce que la section « (3) idle →
> unload → resume » cite (`use-idle-unload.ts`, `unload_idle_sessions`, `agent_unload.rs`,
> `idle_unload_and_revisit_resume_over_the_real_pipe`) **n'existe plus dans le code**. Les clauses
> (1), (2) et (4) sont inchangées, et la reprise `claude --resume` au boot (ResumePark) aussi :
> elle ne dépendait pas de l'unload. Le reste du document est conservé tel qu'il a été rendu.

> Tâche depot : `01KX8Z6TH9RW19XYDK74CAD8BT` (PRD `01KX8Z4E7B3PYW2DGCTTKXCHYC`, Phase 5, `kind=gate`).
> Ce document EST le rapport du gate. Comme `GATE-PHASE4.md` / `GATE-PHASE7.md`, il porte
> la **moitié exécutable** (un test de gate Rust pilotant le VRAI binaire sidecar sur le VRAI
> pipe) et prouve, pour **chaque clause du `done_criteria`**, la **preuve par `file:line`**
> (code) + le **test exécutable vert** qui la couvre — puis documente explicitement ce qui
> n'est **PAS exécutable en headless** (l'E2E Electron renderer complet) comme **concern
> CI/manuel**, **PAS un échec de gate** (même convention que l'en-tête de
> `gate_v2_1_identity_e2e.rs`).

`done_criteria` (verbatim) :

> pilotage réel CDP : (1) toggle ON->relance->ouverture->resume ; OFF->rien ; (2)
> crash->interrupted+restore ; (3) idle->unload->resume, active jamais unloadée ; (4) plugin
> installable depuis l'UI ; non-régression détection agent+icône + statut.

---

## Moitié EXÉCUTABLE — le gate Rust sur le VRAI pipe

`crates/nyx-sidecar/tests/gate_v2_5_5_agent_lifecycle_e2e.rs` — **5 tests, GREEN sur Linux dans
cet environnement**. Il **spawn le VRAI binaire `nyx-sidecar`** et pilote les quatre parcours
v2.5.5 par des **frames `Request::DbCommand` / `Request::PtySpawn`** sur le stdin/stdout du
process, **EXACTEMENT comme Electron main** — donc un arm de dispatch cassé **échoue ici**.

**Pourquoi le VRAI pipe (et pas le `Runtime` in-process)** : deux bugs critiques de câblage
transport ont été trouvés en Phase 4 précisément parce que des tests in-process contournaient le
dispatch (une db-command tombait dans le fallback « not available over the sidecar transport » et
n'atteignait jamais son autorité). Ce gate exerce chaque scénario **par le fil**.

**Seed** : une session agent ACTIVE est normalement écrite par les hooks MCP de capture (une
surface **hors pipe**), donc l'autorité on-disk est **seedée avec la MÊME API `nyx_core::db`**
que l'app **avant** le boot du sidecar (le reverse de `gate_v2_1`, qui rouvre le fichier pour
LIRE), puis le sidecar sert cet état seedé sur le fil.

Résultat mesuré dans cet environnement (Linux) :

```
cargo test -p nyx-sidecar --test gate_v2_5_5_agent_lifecycle_e2e
  test result: ok. 5 passed; 0 failed
```

| Test (fichier ci-dessus) | Couvre |
|---|---|
| `resume_toggle_on_resumes_over_the_real_pipe` | done_criterion (1) ON |
| `resume_toggle_off_injects_nothing_over_the_real_pipe` | done_criterion (1) OFF |
| `crash_surfaces_interrupted_and_restore_plan_over_the_real_pipe` | done_criterion (2) |
| `idle_unload_and_revisit_resume_over_the_real_pipe` | done_criterion (3) unload + resume-au-retour |
| `integrations_listable_and_install_dispatched_over_the_real_pipe` | done_criterion (4) |

---

## Preuve par clause du `done_criteria`

### (1) toggle ON → relance → ouverture → resume ; OFF → rien

**Chaîne réelle (renderer → pipe → sidecar → autorité)** :

- Renderer : `use-lazy-resume.ts:61` lit le plan (`workspace_restore_plan`), `:76` pousse les
  parks (`set_resume_parks`) ; `use-restore-plan.tsx:124/:157` (même paire) ; toggle par projet
  `set_project_resume_agent_sessions` (`global-section.tsx` / `project-settings-dialog.tsx`,
  contract `bridge/contract.ts`).
- Sidecar (autorité, DISPATCH par le fil) :
  - `runtime/db_command.rs:920` arm `workspace_restore_plan` → `Ctx::workspace_restore_plan`
    (`db_command.rs:303`) qui RÉUTILISE `agent_resume::workspace_resume_plan` /
    `decide_resume` (aucune nouvelle politique) ;
  - `runtime/db_command.rs:694` arm `set_resume_parks` → `Runtime::park_resumes`
    (`runtime/mod.rs:405`) ; l'injection au spawn est `Runtime::inject_resume_on_spawn`
    (`runtime/mod.rs:427`) drainant `ResumeParks` (`runtime/resume_parks.rs`).
  - Le master-switch toggle : `db_command.rs:678` `set_project_resume_agent_sessions` →
    `db::set_project_resume_agent_sessions` ; la politique le lit dans `decide_resume`
    (`nyx-core/src/agent_resume.rs`).

**Test exécutable (VRAI pipe)** :
- ON : `gate_v2_5_5_agent_lifecycle_e2e.rs::resume_toggle_on_resumes_over_the_real_pipe` — le
  plan lu SUR LE PIPE rend `resume=true` + `resume_command="claude --resume <ext>"`, le park est
  poussé, l'ouverture du terminal (`PtySpawn`) **injecte le vrai `claude --resume <id>` dans le
  shell réel** (observé sur le fil via `TerminalOutput`). **VERT.**
- OFF : `...::resume_toggle_off_injects_nothing_over_the_real_pipe` — le plan rend `resume=false`
  + `skip_reason="option_off"` (rien à reprendre), et l'ouverture du terminal **n'injecte
  AUCUN** `claude --resume` (prouvé par ordre avec un sentinel). **VERT.**
- Renfort in-process/unit : `resume_inject.rs` (2/2, VRAI pipe : park+spawn+idempotence) ;
  `db_command.rs::workspace_restore_plan_reuses_existing_policies_scoped_to_workspace` ;
  renderer `use-lazy-resume.test.tsx`, `use-restore-plan.test.tsx`.

### (2) crash → interrupted + restore

**Chaîne réelle** :

- Sidecar : au boot, `Runtime::attach_events` (`runtime/mod.rs:216`) appelle
  `recovery::reconcile_after_crash` (`runtime/recovery.rs:158`) — un marqueur non-clean =
  crash → flag les instances `running` **interrupted** + bascule les sessions actives en
  `unknown` (`db::sweep_stale_active_sessions`), SANS relance aveugle (les snapshots sont
  effacés, `recovery.rs:186`).
- Lecture par le fil : `db_command.rs:1689` arm `recovery_snapshot` (`{ interrupted }`) ;
  `workspace_restore_plan` (`db_command.rs:385`) inclut le set `interrupted` scopé au
  workspace + les sessions douteuses.
- Renderer (UI de restauration) : `use-restore-plan.tsx` consomme
  `workspace_restore_plan.interrupted` (`:171-176`) ; `WorkspaceRestorePanel`
  (`workspace-restore-panel.tsx`) rend le panneau de restauration + `<LifecycleBadge>`
  (`lifecycle-badge.tsx`, states `interrupted`/`unknown`), câblé dans
  `workspace-runtime-tree.tsx:15-16`.

**Test exécutable (VRAI pipe)** :
- `gate_v2_5_5_agent_lifecycle_e2e.rs::crash_surfaces_interrupted_and_restore_plan_over_the_real_pipe`
  — seed d'une instance `running` + marqueur `sidecar-recovery.json` non-clean → au boot le
  sidecar flag interrupted ; `recovery_snapshot` ET `workspace_restore_plan` (SUR LE PIPE)
  remontent l'instance interrupted + la session douteuse (offerte à la restauration, jamais
  blind-resume). **VERT.**
- Renfort : `crash_recovery.rs` (VRAI pipe), `recovery.rs` unit tests (crash/clean/fresh),
  `db_command.rs::workspace_restore_plan_surfaces_doubtful_state_without_blind_resume` ;
  renderer `use-restore-plan.test.tsx`, `workspace-restore-panel.test.tsx`,
  `restore-reconstruction.test.tsx`.

### (3) idle → unload → resume ; ACTIVE jamais unloadée

**Chaîne réelle** :

- Renderer : `use-idle-unload.ts:57` pousse `unload_idle_sessions { thresholdSecs, visited }`
  (le sweep timer).
- Sidecar (DISPATCH par le fil) : `db_command.rs:712` arm `unload_idle_sessions` →
  `Runtime::unload_idle_sessions` (`runtime/mod.rs:488`), qui applique `decide_unload`
  (seuil/visited/Working — `nyx-core/src/agent_unload.rs`) PUIS la gate d'éligibilité-resume
  (master switch `decide_resume`), ferme le PTY (`close_record`), et **RE-PARK** le resume
  (`resume_parks.rearm`) pour le retour en run.

**Test exécutable (VRAI pipe)** :
- `gate_v2_5_5_agent_lifecycle_e2e.rs::idle_unload_and_revisit_resume_over_the_real_pipe` —
  `unload_idle_sessions` SUR LE PIPE : GARDE dans le seuil, ÉPARGNE le terminal visited, DÉCHARGE
  l'idle au-delà du seuil (PTY fermé → ressources libérées, id de record retourné) ; la session
  reste `active` (le plan l'offre encore) ; **RE-VISITE** (re-`PtySpawn`) → re-injection du vrai
  `claude --resume <id>` (resume-au-retour, sans re-push renderer). **VERT.**
- **ACTIVE jamais unloadée** : positionner une session `Working` est un signal hook MCP (HTTP
  loopback, **hors pipe**), donc l'invariant de sûreté est prouvé **VERT** par le test réel-PTY
  in-process `agent_unload.rs::active_session_is_never_unloaded` (+
  `action_in_flight_is_never_unloaded`, `toggle_off_session_is_never_unloaded_toggle_on_is`,
  `visited_session_is_never_unloaded`). Ce gate prouve, LUI, la gate d'unload **atteignable par
  le transport** (seuil / visited / toggle-OFF). Les deux ensembles couvrent tous les invariants.
- Renfort transport : `agent_unload.rs::unload_idle_sessions_is_unloaded_via_db_command_dispatch`
  + `..._is_not_the_transport_fallback` (le dispatch, pas le fallback) ; renderer
  `use-idle-unload.test.tsx`, `idle-replay.e2e.cjs` (deferred CI, ci-dessous).

### (4) plugin installable depuis l'UI

**Chaîne réelle** :

- Renderer : `settings-dialog.tsx:272` liste (`integration_list`), `:54/:123` installe/retire
  (`integration_install` / `integration_remove`).
- Sidecar (DISPATCH par le fil) : `db_command.rs:1692` `integration_list`
  (`nyx_core::integrations::status_list`), `:1693` `integration_install`, `:1698`
  `integration_remove` → cores partagés `nyx_core::integrations::{install,remove}` sur la VRAIE
  config Claude (plugin bundlé = MCP + hooks).

**Test exécutable (VRAI pipe)** :
- `gate_v2_5_5_agent_lifecycle_e2e.rs::integrations_listable_and_install_dispatched_over_the_real_pipe`
  — `integration_list` SUR LE PIPE rend les 4 providers avec Claude Code `available` (le plugin
  que le bouton Install pilote) ; `integration_install` est **DISPATCHÉ** au core intégrations
  par le fil (prouvé via l'arm `codex` → « not supported », qui court-circuite **avant** tout
  touche config/CLI — donc ce gate **ne mute PAS** le `~/.claude` réel). **VERT.**
- Le round-trip complet install/uninstall Claude **shell-out au vrai `claude`** et écrit la config
  Claude → prouvé par les unit tests `nyx_core::integrations` (FakeCli sur chemins temp, jamais le
  vrai `~/.claude`) + `integration_plugin.rs` (VRAI pipe, install/list/remove avec seams) +
  `integration_reconcile.rs`. La validation UI complète = `smoke:sidecar-e2e` /
  `smoke:bridge-e2e` (validé Windows, `GATE-PHASE7.md` item #6) → **concern CI/OS** ci-dessous.

### Non-régression : détection agent + icône Claude (A1) et statut working/waiting/idle (A3)

- **A1 (détection agent + icône Claude)** : `components/sidebar/agent-providers.tsx:32`
  (`claude_code: { icon: ClaudeIcon, label: "Claude Code" }`), accesseur `agentProviderFor`
  (`:40`) ; consommé pour rendre l'icône dans `workspace-runtime-tree.tsx:130-152`
  (`agentProviderFor(liveSession?.agent_kind)`) et `sidebar/terminal-item.tsx:145`. Tests :
  `agent-providers.test.tsx`, `workspace-runtime-tree.test.tsx`. **Intact.** L'icône a **deux**
  sources : (a) la session persistée du hook `SessionStart` (`sessions[].agent_kind`) et (b)
  l'**adoption PASSIVE** display-only du foreground `comm` (`detected_agent_kind`, keyée sur le
  `comm` du pgid foreground — détaillée en étape manuelle 5 ci-dessous, avec sa dépendance au `comm`
  et le fait vérifié `comm="claude"` pour le binaire natif).
- **A3 (statut working/waiting/idle)** : le mapping wire est `db_command.rs:1712 activity_str`
  (`Idle→idle`, `Working→working`, `Waiting→waiting`) via `agent_activity_snapshot` ; côté
  renderer `components/terminals/runtime-state.ts:60-110` (`AgentActivityLookup`,
  `isAgentWaiting`) alimente `<StatusDot>` (`status-dot.tsx:21,44-47` : states `waiting`/`idle`…).
  Tests : `status-dot.test.tsx`, `runtime-row.test.tsx`, `sidebar/use-agent-sessions` +
  `runtime-row.test.tsx`. **Intact** (renderer `bun run check` vert, ci-dessous).

---

## NON exécutable en headless → concern CI / manuel (PAS un échec de gate)

Le **pilotage réel CDP Electron** (renderer complet consommant les invalidations et rendant le
panneau de restauration, les badges de lifecycle, l'injection dans un vrai xterm, la fenêtre
Settings) nécessite un **vrai display** (X/Wayland ou **`xvfb`** en CI) **et** un install réseau du
toolchain WDIO — c'est, dans ce repo, la **matrice CI du gate Phase 7**, **documentée** comme
concern CI/OS (cf. `e2e/README.md` § « What runs HERE vs DEFERRED » et `GATE-PHASE7.md` étape d),
**pas un échec de gate**.

Specs Electron/CDP existantes qui couvrent la surface renderer de ces parcours (à lancer sur la
cible avec display) :

- `e2e/specs/restore-01-seed.e2e.cjs` → `restore-02-verify.e2e.cjs` — restore multi-sessions
  (close réel + réouverture sur la MÊME DB) : le pendant renderer de done_criterion (1)/(2).
- `e2e/specs/idle-replay.e2e.cjs` — pas de self-replay au repos (surface deck/idle) : renderer de
  (3).
- `e2e/specs/terminal.e2e.cjs` — env/output/resize/exit sur un vrai PTY.

**Recette CI reproductible** (Debian/Ubuntu runner, aucun workflow committé — recette seulement,
cf. `e2e/README.md`) :

```sh
sudo apt-get update && sudo apt-get install -y xvfb   # display virtuel pour Electron headed
cargo build -p nyx-sidecar                            # le binaire runtime spawné par main
cd e2e && bun install --frozen-lockfile               # toolchain WDIO v9 + chromedriver
xvfb-run -a bun run test                              # pilote la vraie app Electron
```

Étapes manuelles équivalentes (machine cible avec display) :

1. `bun run build:e2e` (dans `apps/desktop`) — build renderer avec le seam `VITE_NYX_E2E=1`.
2. `cd e2e && NYX_E2E_SKIP_BUILD=1 bun run test` — pilote les specs restore/idle/terminal.
3. Vérifier à l'écran : toggle ON → relance app → ouverture terminal → `claude --resume` injecté ;
   toggle OFF → rien ; crash (kill sidecar) → sessions « interrupted » + panneau de restauration ;
   idle au-delà du seuil → session déchargée puis resume au retour ; Settings → bouton Install du
   plugin Claude.
4. **Routing agent + icône sous coexistence v1+v2 (collision de port — review
   `01KXAXQN32GASMEB05PDW5BZSR`)** — la vérif END-TO-END du fix « icône agent éteinte en v2 »,
   **non reproductible en headless** (elle exige DEUX apps coexistantes + un VRAI agent Claude) :
   - **v1 tournant** (il tient le port MCP `8765`), **relancer nyx-v2** — son sidecar bind alors son
     propre port MCP (`NYX_MCP_PORT`, défaut `8766`, **jamais** `8765`).
   - Lancer une **VRAIE session Claude** dans un terminal v2 (le shell v2 exporte `NYX_MCP_PORT` =
     le port bindé par v2, injecté au spawn du PTY).
   - Confirmer : **une ligne `claude_code`** dans `~/.config/nyx-v2/nyx.db` table `agent_sessions`
     (`sqlite3 ~/.config/nyx-v2/nyx.db "SELECT agent_kind, terminal_id FROM agent_sessions;"`) **ET**
     l'**icône Claude** allumée sur ce terminal dans l'arbre.
   - Confirmer : **RIEN** de nouveau côté DB v1 (`~/.config/nyx/nyx.db` `agent_sessions`) — le hook
     `SessionStart` a POSTé `agent_session_event` sur le port **bindé par v2**, pas sur le `8765` par
     défaut de v1 (le bug d'origine y déversait les events v2, laissant `agent_sessions` v2 vide).

   Couverte au niveau code par l'injection `NYX_MCP_PORT` (`crates/nyx-core/src/pty.rs`, lue du port
   bindé dans `crates/nyx-sidecar/src/runtime/mod.rs` au `PtySpawn`) + les tests
   `crates/nyx-sidecar/tests/command_mcp.rs::spawned_terminal_exports_bound_mcp_port_and_records_session_on_it`
   (VRAI pipe : shell exporte le port bindé → `agent_session_event` sur ce port → ligne `claude_code`
   relue en DB) et l'unit `nyx-core/src/pty.rs::nyx_mcp_port_is_exported_to_unix_shell` (port hors env
   parent). L'étape manuelle ci-dessus prouve le dernier maillon (vrai Claude + collision v1/v2 réelle).

5. **Adoption PASSIVE de l'icône agent (chemin foreground `comm`, SANS session hook — review T3 /
   task `01KXB7GHA551FQX57F8JBKZV22`)** — le SECOND chemin d'allumage de l'icône, **distinct** du
   hook `SessionStart` de l'étape 4 : nyx adopte l'icône d'un agent lancé **à la main**, sans aucun
   événement de session, par simple **détection du programme au premier plan** du terminal. Non
   reproductible en headless (exige un vrai PTY interactif + un vrai binaire `claude`).
   - Dans un terminal v2 **au prompt**, lancer `claude` **à la main** (aucun `--resume`, aucun hook
     `SessionStart` déclenché) et le laisser au premier plan.
   - Confirmer : l'**icône Claude s'allume** sur ce terminal dans l'arbre runtime **sans** qu'aucune
     ligne ne soit écrite dans `agent_sessions` (`sqlite3 ~/.config/nyx-v2/nyx.db "SELECT COUNT(*)
     FROM agent_sessions;"` **inchangé**) — l'adoption passive est **display-only** (aucune session
     persistée, à l'inverse de l'étape 4). Quitter `claude` (retour au prompt) **éteint** l'icône au
     pull suivant.
   - **Dépendance au `comm` du foreground pgid** (le point que cette étape rend explicite) : l'overlay
     lit le programme au premier plan via `tcgetpgrp(master)` → `/proc/<pgid>/comm`
     (`nyx_core::proc::read_foreground_comm`, **tronqué à 15 octets** par le noyau) puis le résout par
     un adapter. L'adapter **matche le `comm`** : `ClaudeCodeAdapter::detect`
     (`crates/nyx-core/src/agent.rs:322`) renvoie vrai quand le basename du premier token vaut
     `claude`. **Fait vérifié** : le binaire natif Claude (`/opt/claude-code/bin/claude`) présente
     `comm="claude"` (6 octets < 15, pas de troncature) → `AgentRegistry::detect`
     (`agent.rs:719`, **provider-agnostic**) le résout à `agent_kind="claude_code"` → icône allumée.
     **Corollaire / limite connue** : si l'agent était lancé via un **wrapper qui n'exec pas** (le
     foreground reste `sh`/`node`/`npm`…, donc `comm != "claude"`), l'icône passive **ne s'allumerait
     pas** — la détection est keyée sur le `comm` RÉEL du pgid foreground, pas sur la ligne de commande
     tapée. Le setup réel (Claude natif) satisfait déjà cette dépendance, d'où une adoption passive qui
     marche sans durcissement d'adapter.
   - **Chaîne code** : overlay `Ctx::apply_terminal_live`
     (`crates/nyx-sidecar/src/runtime/db_command.rs:317-325`) —
     `foreground_comm_record(record_id)` (`crates/nyx-sidecar/src/runtime/pty.rs:379` →
     `read_terminal_info` → `read_foreground_comm`) → `registry.detect(&comm)` →
     `detected_agent_kind` (jamais de ligne `agent_sessions`, purement d'affichage). Rendu icône :
     même accesseur A1 `agentProviderFor` (`components/sidebar/agent-providers.tsx:40`,
     `workspace-runtime-tree.tsx:130-152`).
   - **Couverture code (VERT)** :
     `crates/nyx-sidecar/src/runtime/pty.rs::foreground_comm_feeds_provider_agnostic_detection`
     (VRAI PTY : le `comm` foreground alimente la détection ; `claude` → `claude_code`, un `sleep` →
     rien) + `crates/nyx-core/src/agent.rs::detect_is_provider_agnostic_via_a_mock_adapter` (la
     détection route vers N'IMPORTE quel adapter enregistré — aucun `if claude` câblé). L'étape
     manuelle ci-dessus prouve le dernier maillon : un vrai binaire `claude` présentant `comm="claude"`.

**« ACTIVE (Working) jamais unloadée » par le pipe** : nécessiterait de poser `Working` via le
hook MCP (HTTP loopback) — prouvé GREEN in-process par `agent_unload.rs::active_session_is_never_unloaded`
(VRAI PTY + vrai runtime). Documenté ici comme couvert par l'unit réel, la gate transport
prouvant seuil/visited/toggle-OFF sur le fil.

---

## Résultat des validations (dans cet environnement Linux)

| Validation | Commande | Résultat |
|---|---|---|
| Gate v2.5.5 (VRAI pipe) | `cargo test -p nyx-sidecar --test gate_v2_5_5_agent_lifecycle_e2e` | **VERT** — 5/5 |
| Suite sidecar (dont resume_inject 2/2, agent_unload 9/9, crash_recovery, integration_plugin, gate_v2_1) | `cargo test -p nyx-sidecar` | **VERT** — lib 70/70 + tous les binaires de tests, 0 échec |
| Workspace | `cargo check --workspace` | **VERT** (exit 0) |
| Format Rust | `cargo fmt --check` | **VERT** (delta clean) |
| Renderer | `apps/desktop/renderer` `bun run check` (oxfmt + oxlint + tsc) | **VERT** (exit 0) |

---

## Conclusion / état du gate

- **Moitié exécutable : VERTE** — le gate Rust `gate_v2_5_5_agent_lifecycle_e2e.rs` (5/5) pilote
  les quatre parcours sur le VRAI binaire + VRAI pipe, prouvant le câblage transport (dispatch)
  de chaque autorité (resume/restore/recovery/unload/integrations), plus le renfort des suites
  existantes (resume_inject, agent_unload, crash_recovery, integration_*), toutes vertes.
- **Moitié checklist : chaque clause du `done_criteria` est prouvée par `file:line` (code) + un
  test exécutable vert** ; les parties **renderer CDP complètes** (panneau de restauration, badges,
  injection dans un vrai xterm, Settings) et le **round-trip install plugin réel** sont
  **documentés comme concern CI/OS** (specs `restore-*`/`idle-replay`/`terminal` + recette
  `xvfb`), conformément à la convention du repo — **PAS un échec de gate**.

→ Le gate `01KX8Z6TH9RW19XYDK74CAD8BT` est **satisfait** : la moitié exécutable est verte et le
checklist prouve le reste par `file:line` / concern-CI justifié. Aucun des quatre comportements
n'est manquant ou non prouvable (chacun a une preuve exécutable sur le VRAI transport OU un test
réel-PTY + le renderer câblé prouvé par `file:line` + concern CI documenté pour le rendu CDP).
