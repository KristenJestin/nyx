# nyx Tauri shell — dormant, but reversible (task #21)

The Electron shell is nyx's shipping host. The **Tauri shell is kept DORMANT**: it
still compiles against `nyx-core` and still opens the shared front through
`nyxBridge.tauri`, so the migration stays **reversible**. This is the documented
control path — a re-test/rollback to Tauri without touching a single UI component.

> What "dormant" means here: maintained green (`cargo build --workspace` exits 0) and
> launchable on demand, **not** shipped in the Electron releases and **not**
> double-maintained for feature parity going forward.

## The seam: one runtime switch, zero component changes

Every UI component imports the active bridge from **one** module —
[`apps/frontend/src/bridge/index.ts`](../frontend/src/bridge/index.ts) — and depends
only on the `NyxBridge` contract (`bridge/contract.ts`). That module picks the
adapter **at runtime**:

```ts
// apps/frontend/src/bridge/index.ts
function isElectronShell(): boolean {
  return typeof window !== "undefined" && typeof window.nyxCore !== "undefined";
}
export const nyxBridge = isElectronShell() ? electronBridge : tauriBridge;
```

- The **Electron** preload installs a deep-frozen `window.nyxCore` allowlist → the
  Electron adapter (`bridge/electron.ts`) is selected.
- Under **Tauri** there is no `window.nyxCore`, so the **fallback** `tauriBridge`
  (`bridge/tauri.ts`) is selected. The Tauri shell injects `window.__TAURI_INTERNALS__`,
  which is what the Tauri adapter's `invoke`/`listen` ride on.

Because the choice is made here and nowhere else, **switching shells changes no
component and no call-site** — only which adapter `index.ts` resolves. `@tauri-apps/*`
imports are confined to `bridge/tauri.ts`; `bridge/electron.ts` imports none, so both
adapters compile in either build, and only the selected one is wired to `nyxBridge`.

The Rust side mirrors this: `apps/tauri/src-tauri` is a thin shell over `nyx-core`
(its `bridge.rs` adapts `nyx-core` to Tauri commands, exactly as the Electron
core-host adapts it to napi). No Tauri or Electron type crosses the `nyx-core` API.

## Switchover / re-test procedure (Electron ⇆ Tauri)

No code change is required to switch — both shells consume the same
`apps/frontend` build. You just launch the other shell.

### Run the dormant Tauri shell

```sh
# Dev (hot-reloading front via the shared Vite dev server):
bun run dev:tauri                 # = tauri dev; serves apps/frontend at :1420

# Or a production Tauri build (embeds apps/frontend/dist):
bun run build:tauri               # = tauri build  → target/release/nyx(.exe) + bundles
```

`tauri.conf.json` wires the SAME frontend the Electron shell ships:
`beforeBuildCommand: bun run --filter @nyx/frontend build`, `frontendDist:
../../frontend/dist`. So the bytes the user sees are identical; only the host
(WebKitGTK/WebView2 vs Chromium) and the bridge adapter differ.

> A bare `cargo build -p nyx` proves the shell COMPILES but yields a binary that
> points at the dev server (`devUrl`); to actually OPEN the embedded front you must
> use `tauri build` (or `tauri dev`), which runs the front build and embeds it.

### Switch back to Electron

```sh
bun run dev:electron              # Electron dev (its preload installs window.nyxCore)
# or package: see apps/electron/PACKAGING.md
```

The front auto-selects `electronBridge` because the preload installs `window.nyxCore`.
Nothing else changes.

### Compile-only control check (the dormant guarantee)

```sh
cargo build --workspace           # MUST exit 0 — proves the Tauri shell + nyx-core
                                  # + nyx-napi all still compile (the dormant gate).
```

## Each shell owns its OWN storage — NO data migration

The two shells **do not share data and do not migrate it**. They resolve their data
directory through different app-framework path APIs that land in **distinct**
per-shell directories:

| Shell    | Data-dir resolver                                   | Typical location                                                       |
| -------- | --------------------------------------------------- | ---------------------------------------------------------------------- |
| Tauri    | `app.path().app_data_dir()` (`src-tauri/lib.rs`)    | `%APPDATA%\com.netsirk.nyx\` (Win) · `$XDG_DATA_HOME/com.netsirk.nyx/` (Linux) |
| Electron | `app.getPath("userData")` (`main/core-host.ts`)     | `%APPDATA%\nyx\` (Win) · `~/.config/nyx/` (Linux)                       |

Each holds its own `nyx.db` (SQLite). **Switching shells does NOT carry terminals,
projects, command history or agent sessions across** — a Tauri re-test starts from
the Tauri DB, an Electron run from the Electron DB. This is intentional: the dormant
path is a control/rollback, not a live mirror.

The ONLY shared override is the env var **`NYX_DATA_DIR`**, honored by BOTH shells
(`resolve_data_dir` in Tauri, `resolveDataDir` in Electron). It is the seam the e2e
harness pins to a temp dir for a deterministic, isolated DB. If — and only if — you
deliberately point both shells at the same `NYX_DATA_DIR`, they read the same DB; the
default behavior keeps them fully separate.

Likewise the bundled **Claude plugin** is staged independently per shell (Tauri via
`tauri.conf.json bundle.resources`, Electron via `dist/resources/**` +
`asarUnpack` — see `apps/electron/PACKAGING.md`); both resolve it through
`nyx-core resolve_bundled_plugin_dir`, so the integration behaves identically without
shared state.
