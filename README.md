# nyx v2

**nyx** est un gestionnaire de terminaux orienté travail agentique : l'environnement local où un travail logiciel
est défini, exécuté, observé, reviewé et validé. Application desktop unique — un shell **Electron** (`apps/desktop`)
qui supervise un **sidecar Rust** unique (`crates/nyx-sidecar`, qui embarque `nyx-core`). Tauri et le `core-host`
Node sont retirés (PDR-0006 / PDR-0007).

## Par où commencer

La **mémoire produit durable** vit dans [`docs/product/`](docs/product/) — c'est la source de vérité :

- [`docs/product/README.md`](docs/product/README.md) — index canonique, registre des concepts, ordre de lecture ;
- [`docs/product/vision.md`](docs/product/vision.md) — ce qu'est nyx et sa boucle cible ;
- [`docs/product/roadmap.md`](docs/product/roadmap.md) — tranches v2.0 → v2.8 et **état d'avancement** ;
- [`docs/product/data-model.dbml`](docs/product/data-model.dbml) — schéma cible (importable dans dbdiagram.io) ;
- [`docs/product/decisions/`](docs/product/decisions/) — les PDR (Product Decision Records) : le *pourquoi* ;
- [`docs/product/session-notes.md`](docs/product/session-notes.md) — chantiers hors ligne principale.

## Structure du dépôt

```text
apps/desktop            shell Electron (main/preload) + renderer React
apps/desktop/renderer   UI React (TanStack Router, wterm/ghostty)
crates/nyx-core         backend métier Rust (PTY, SQLite, MCP, domaine)
crates/nyx-sidecar      exécutable sidecar supervisé par Electron
contract                tests de parité du protocole Rust ↔ TS (MessagePack)
e2e                     tests end-to-end pilotant l'app Electron réelle (WDIO)
```

## Build & tests

Depuis la racine (bun + cargo requis) :

```sh
bun run dev            # renderer en dev
bun run dev:desktop    # app desktop en dev
bun run build          # build renderer ; build:desktop pour l'app complète

bun run test           # renderer + core + contrat
bun run test:core      # cargo test --workspace + contrat sidecar
bun run test:contract  # parité protocole Rust ↔ TS (voir contract/README.md)
bun run check          # typecheck renderer + cargo check
```

- Détail du contrat de protocole : [`contract/README.md`](contract/README.md).
- Détail des tests e2e (prérequis, isolation, gates) : [`e2e/README.md`](e2e/README.md).

## Branches

- **`v2`** — branche canonique (ligne principale).
- **`v2.4-review`** — capacité review (diff/comments, PDR-0027), validée en gate CDP 9/9, **non mergée**, à greffer
  en v2.5 (voir `session-notes.md`).
- **`feature/v2.3-terminal-blocks`** — terminal en blocs (OSC 133), WIP **parké** (voir `session-notes.md`).

## Règle d'or

Une capacité n'est « livrée » que lorsqu'elle a été **observée fonctionner dans l'app réelle** (pilotage CDP sur
shells réels), pas seulement lorsqu'une suite verte le suggère — voir la *Politique de validation* dans
[`docs/product/technical-architecture.md`](docs/product/technical-architecture.md).
