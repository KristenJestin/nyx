# nyx

nyx is a terminal-centric desktop app. Its shell-agnostic core is Rust; the UI is
React/Vite. The repo is a **polyglot monorepo** mid-migration from a Tauri/WebKitGTK
shell to an Electron (Chromium, Wayland-native) shell, keeping the Rust core via
napi-rs and keeping the Tauri shell dormant-but-reversible.

## Monorepo layout

```
apps/
  frontend/        React + Vite UI (the single shared frontend; @nyx/frontend)
  electron/        Electron shell — reserved placeholder, built in phase 2 (@nyx/electron)
  tauri/
    src-tauri/     the Tauri shell crate `nyx` (kept green through the migration)
crates/
  nyx-core/        shell-agnostic Rust core (PTY, OSC, DB, MCP, agents, commands,
                   sessions) + the four frontiers (EventSink, AppPaths, service
                   container, boot/shutdown). No Tauri/Electron type crosses its API.
  nyx-napi/        napi-rs module exposing nyx-core to Node (the Electron core-host);
                   builds a .node addon + .d.ts. Its npm wrapper is `@nyx/napi`.
e2e/               end-to-end harness (standalone; its own bun.lock)
```

One **Bun workspace** at the repo root covers `apps/*` and the `crates/nyx-napi`
npm wrapper, with a single root `bun.lock` and `node_modules`. One **Cargo workspace**
at the repo root covers `apps/tauri/src-tauri`, `crates/nyx-core` and `crates/nyx-napi`,
with a single root `Cargo.lock` and shared `target/`.

## Root commands (run from the repo root; cwd-independent)

All scripts target a workspace member by name, so they work from any directory once
you are at the repo root:

| Command              | What it does                                                   |
| -------------------- | ------------------------------------------------------------- |
| `bun run dev`        | Frontend dev server (Vite) — `@nyx/frontend`                  |
| `bun run dev:tauri`  | Tauri shell in dev (drives the shared frontend)               |
| `bun run dev:electron` | Electron shell in dev (placeholder until phase 2)           |
| `bun run build`      | Build the frontend (`tsc && vite build`)                      |
| `bun run build:tauri`| Build the Tauri app                                           |
| `bun run build:electron` | Build the Electron app (placeholder until phase 2)        |
| `bun run build:napi` | Build the napi `.node` + `.d.ts` (placeholder until skeleton) |
| `bun run test`       | Frontend tests (vitest) + core tests (`cargo test`)           |
| `bun run test:frontend` | Frontend tests only                                        |
| `bun run test:core`  | Rust core tests (`cargo test --workspace --lib`)              |
| `bun run check`      | Frontend check (oxfmt/oxlint/tsc) + `cargo check --workspace` |
| `bun run check:core` | `cargo check --workspace`                                     |
| `bun run package`    | Package the app (currently the Tauri build)                   |

Rust-only workflows can also use Cargo directly from the root:

```sh
cargo check --workspace      # type-check every crate (nyx, nyx-core, nyx-napi)
cargo build -p nyx           # build the Tauri shell crate
cargo test  -p nyx-core      # run the shell-agnostic core tests (no Tauri runtime)
```

On Windows, `cargo test -p nyx-core --lib` reports a fixed set of **15
pre-existing, environmental failures** (missing `sh` shell + Windows path
assertions) that are **not** migration regressions. They are frozen by name in
[`crates/nyx-core/BASELINE-REDS.md`](crates/nyx-core/BASELINE-REDS.md); any
failure outside that named list is a real regression.

## Recommended IDE setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
