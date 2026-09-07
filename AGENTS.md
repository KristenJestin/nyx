# AGENTS.md

Guide d'intervention pour agents sur **nyx v2**. Le `README.md` s'adresse aux humains ; ce fichier
rassemble ce qu'il faut pour construire, tester et valider sans casser les invariants du projet.

## Vue d'ensemble

nyx est un gestionnaire de terminaux orienté travail agentique : l'environnement local où un travail
logiciel est défini, exécuté, observé, reviewé et validé. Application desktop unique — un shell
**Electron** (`apps/desktop`) qui supervise un **sidecar Rust** unique (`crates/nyx-sidecar`, qui
embarque `crates/nyx-core`).

```text
apps/desktop            shell Electron (main/preload) + renderer React
apps/desktop/renderer   UI React (TanStack Router, @xterm/xterm en rendu DOM)
crates/nyx-core         backend métier Rust (PTY, SQLite, MCP, domaine)
crates/nyx-sidecar      exécutable sidecar supervisé par Electron
contract                tests de parité du protocole Rust ↔ TS (MessagePack)
e2e                     tests end-to-end pilotant l'app Electron réelle (WDIO)
docs/product            mémoire produit durable — source de vérité
```

**Avant de concevoir quoi que ce soit**, lire `docs/product/README.md` : index canonique et registre
des concepts. Les PDR dans `docs/product/decisions/` portent le *pourquoi* — s'y référer plutôt que
de re-litiger une décision actée.

## Environnement

**bun 1.3.14** (épinglé via `devEngines`, téléchargé au besoin) et **cargo** sont requis. Workspaces
bun : `apps/desktop` et `apps/desktop/renderer`.

```sh
bun run dev            # renderer en dev
bun run dev:desktop    # app desktop en dev
bun run build          # build renderer ; build:desktop pour l'app complète
bun run install:linux  # Linux : build release + paquet unpacked + lanceur .desktop (voir apps/desktop/PACKAGING.md)
```

## Tests

```sh
bun run test           # suite complète : renderer + core + e2e de gate + contrat
bun run test:frontend  # renderer seul
bun run test:core      # cargo test --workspace --lib + contrat sidecar
bun run test:core:e2e  # les e2e de gate de crates/nyx-core/tests/ (voir ci-dessous)
bun run test:contract  # parité protocole Rust ↔ TS
bun run check          # oxfmt + oxlint + tsc côté renderer, cargo check côté Rust

bun run sim <nom>      # rejoue UN scénario du simulateur de run (--list pour les voir)
bun run test:sim       # rejoue les six, en test d'intégration (voir ci-dessous)
```

Dans `apps/desktop/renderer`, la suite se scinde en deux projets vitest : `test:unit` (jsdom) et
`test:browser` (Vitest Browser Mode — vrai Chromium via Playwright, vrai xterm en rendu DOM).

`test:core:e2e` est **le** point d'entrée des e2e de `crates/nyx-core/tests/`, et il est appelé par
`bun run test`. Il nomme ses cibles une à une plutôt que de balayer le dossier, parce que trois
fichiers de `tests/` n'y ont pas leur place et sont **exclus délibérément** : `nyx_next_loop_spike`
et `agent_activity_signal_spike` sont des *spikes* — des relevés de faisabilité datés, adossés à
`docs/spikes/`, pas des gates de non-régression — et `nyx_next_loop_spike` est rouge sous Windows
(3/3) pour des raisons d'environnement PTY ; `sim_scenarios_e2e` est le simulateur, dont le lanceur
est `test:sim` (voir juste après). Ajouter un fichier à `tests/` n'a donc aucun effet tant qu'il
n'est pas ajouté à l'un de ces deux scripts.

Détails : `contract/README.md` pour le protocole, `e2e/README.md` pour les prérequis end-to-end.

### Le simulateur de run

`bun run sim <nom>` rejoue un workflow nyx (`ndev`, `nspec`…) **sans agent et sans app** : base
SQLite jetable ouverte par le vrai chemin de boot, réponses scriptées dans un JSON, gates exécutés
sur de vrais PTY, verdict en quelques secondes. Il prouve le **chemin** — quelle branche, quels
gates, quel journal, quels statuts — pas la pertinence d'un LLM. Détails et format :
`crates/nyx-core/examples/sim/README.md`.

`bun run test:sim` rejoue les six scénarios livrés en `cargo test`. Il n'est chaîné **ni** dans
`test:core` (`--lib` ne compile aucune cible `tests/`) **ni** dans `test:core:e2e`, donc pas non plus
dans `bun run test` : chaque scénario seede une base et lance de vrais shells, ce qui n'a pas sa
place dans une suite unitaire. Sa **compilation**, elle, est couverte par `bun run check:core`
(`cargo check --workspace --all-targets`), qui compile les cibles `example` et `test` — c'est là
qu'un `#[path]`-include cassé se voit.

**Mesurer depuis PowerShell** vaut ici comme ailleurs (voir « Pièges connus ») : les sentinelles de
gate du simulateur sont rendues dans le dialecte que `pty::resolve_shell()` désignera vraiment, donc
`bun run sim` doit sortir **vert des deux côtés** — et si les deux divergent, c'est le signal.

### Pièges connus

- Le mode navigateur exige le binaire Chromium de Playwright : `npx playwright install chromium`.
  Sans lui, une quinzaine de fichiers ne démarrent pas et `bun run test` sort en échec alors que
  toute la suite jsdom est verte.
- **`node` doit être sur le `PATH`** pour `bun run test:core:e2e`. Un gate lance sa commande à
  travers le shell du jour (`bash -c` sous Unix ou Git Bash, `powershell.exe -Command` sous Windows
  nu), donc aucune de ces suites n'arme un built-in POSIX : elles écrivent un petit script `node`
  dans le workspace du gate et lancent `node <script>`, une phrase qui veut dire la même chose sous
  les deux shells. Sans `node`, les checks sortent rouges et les e2e échouent pour une raison qui
  n'est pas une régression.
- Sous Windows, les suites Rust sont **vertes** sur machine au repos. La liste **nommée** fait foi —
  `crates/nyx-core/BASELINE-REDS.md` — et se compare nom par nom, jamais en comptant. Elle ne porte
  plus aucun rouge gelé ; une seule famille y subsiste : **sensible à la charge, verte en
  isolation** — budgets d'horloge (`design`) et watchers de fichiers réels (`git_watch`,
  `repo_watch` côté sidecar). Rejouer le nom seul, ou `-- --test-threads=1`, avant de conclure.
  Tout autre rouge est une régression et doit bloquer.
- **Mesurer depuis PowerShell, pas depuis Git Bash.** `cargo test` lancé depuis Git Bash hérite d'un
  `PATH` qui porte le `usr\bin` de Git, où vivent `sh.exe`, `true.exe` et `test.exe` : ces noms
  deviennent résolvables à l'intérieur du shell que nyx lance pour une commande managée, et un test
  dont la ligne de commande est du POSIX passe au vert. Deux agents obtiennent alors deux baselines
  sur la même machine, au même commit, et chacun conclut que l'autre a régressé — c'est exactement
  ce qui s'est produit le 2026-08-26. **Corriger les 18 rouges de cette famille n'a pas désarmé le
  piège**, seulement retiré les tests qui y étaient tombés. Il a deux bouches, et la seconde est la
  dangereuse : un test qui attend du VERT diverge visiblement d'un shell à l'autre ; un test qui
  attend du ROUGE est vert **des deux côtés** en n'assertant rien, `The term 'sh' is not recognized`
  étant un code de sortie non nul comme un autre — et PowerShell recopiant la ligne entière dans son
  message d'erreur, même une assertion sur une sentinelle imprimée passe alors que rien n'a tourné.
  D'où la règle : **jamais de ligne de commande littérale** dans un test qui arme une commande
  managée. `crate::test_cmdline` rend « la ligne qui fait X sur cette plateforme », dans le dialecte
  du shell que `pty::resolve_shell` désignera vraiment.
- La CI porte un gate cross-OS (`.github/workflows/gate-v2.2.5-terminal-persistence.yml`) qui rejoue
  les suites navigateur, unitaire et Rust sur Windows, Linux et macOS.

## Style de code

- **Renderer** : oxfmt pour le format, oxlint pour le lint, `tsc --noEmit` pour les types — les trois
  via `bun run check`. Aucun fichier de configuration : les outils tournent sur leurs défauts.
- **Rust** : `cargo check --workspace --all-targets`, la commande que lance `bun run check:core` —
  seul gate qui compile aussi les cibles de test, d'exemple et de bench. Les tests unitaires vivent
  dans le module qu'ils couvrent (`mod tests`), pas dans un dossier séparé.
- **Nommage** : kebab-case pour les fichiers TS/TSX, snake_case pour le Rust.

## Commits

Convention Angular, sujet en français : `type(scope): sujet`. Types en usage dans l'historique :
`feat`, `fix`, `docs`, `test`, `refactor`, `build`, `chore`.

## Invariants à ne pas casser

- **nyx reste générique.** Aucune logique spécifique à un projet, à un agent ou à un outil externe
  câblée dans le domaine central quand un concept générique suffit.
- **Un seul backend.** Le sidecar Rust est unique et supervisé par Electron : pas de second backend,
  pas de retour au `core-host` Node ni à Tauri (PDR-0006 / PDR-0007).
- **Règle d'or de validation.** Une capacité n'est livrée que lorsqu'elle a été *observée fonctionner
  dans l'app réelle* — pilotage CDP sur shells réels — et pas seulement lorsqu'une suite verte le
  suggère. Voir la Politique de validation dans `docs/product/technical-architecture.md`. Les preuves
  visuelles et traces CDP sont déposées dans `.shots/`.

## Branches

`v2` est la ligne principale de ce code ; la branche `main` du dépôt porte encore la v1. Les
chantiers hors ligne principale sont décrits dans `docs/product/session-notes.md` — s'y référer
plutôt qu'à la liste de branches du `README.md`, qui ne reflète plus l'état du dépôt.

## Skills

Les skills d'agent vivent dans `.agents/skills/`, dupliqués à l'identique dans `.claude/skills/` pour
les agents qui ne lisent que ce chemin. Toute modification doit être reportée des deux côtés.
