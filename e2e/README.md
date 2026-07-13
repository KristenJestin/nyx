# nyx e2e (wdio-electron-service + WebdriverIO)

> **⚠️ Staleness — ce README précède plusieurs pivots plateforme, à re-porter.** Certaines descriptions ci-dessous
> datent du portage Tauri et n'ont pas suivi la V2 : (1) le **`core-host` Node est supprimé** (PDR-0007) — c'est
> désormais le **sidecar Rust** qui possède PTY/commandes ; (2) **xterm est remplacé par wterm (rendu DOM)**
> (PDR-0026) — le texte du terminal est maintenant **dans le DOM** ; (3) l'**écran cockpit et la route `#/cockpit/…`
> sont supprimés** (PDR-0025, shell multi-pages Terminals/Work/Knowledge) → les specs `cockpit-0*` visent un écran
> disparu et sont **à re-câbler**. La section **Prerequisites** (sidecar) est, elle, à jour. Corrections partielles
> appliquées ; le re-port complet des specs reste à faire.

End-to-end tests that drive the **real** nyx **Electron** app through
[`wdio-electron-service`](https://webdriver.io/docs/desktop-testing/electron) and
WebdriverIO, on **Linux** and **Windows**. They cover what neither the jsdom unit
suite nor the mock-runtime Rust tests can: the actual app process (Electron main +
the supervised **Rust sidecar**), a real Chromium render, and a real PTY/shell.

> **Ported from the Tauri harness (task #27).** The previous harness drove the
> WebKitGTK / WebView2 app via `tauri-driver`. This one launches the Electron app
> directly. The **spec files are essentially unchanged** — they drive + read terminals
> through the inert `window.__nyx` / `window.__nyxDeck` seams, which are renderer-side
> React and therefore **shell-agnostic**. Only the driver layer (`wdio.conf.cjs`) and
> the toolchain (`package.json`) changed.

## What is tested

### Smoke (`specs/terminal.e2e.cjs`)

1. **Env persists** — `export FOO=bar...` then `echo "$FOO"`, asserting the value
   survives between commands.
2. **Program output** — run `printf` and assert a known marker appears.
3. **Resize** — resize the window twice; assert the app does not crash and the
   terminal still accepts input afterwards.
4. **`exit`** — typing `exit` closes the shell; the `[process exited]` notice appears.

### Restore scenario (`specs/restore-01-seed.e2e.cjs` → `restore-02-verify.e2e.cjs`)

The big PRD-1 scenario, split across **two app sessions** (a real close + reopen):

- **Seed (session 1)** opens **3 terminals at distinct cwds** each running a command
  with observable output, **reorders** them, and **closes the auto-created default**
  voluntarily. The session ends — `wdio-electron-service` closes the Electron app,
  whose `before-quit` stops the Rust sidecar — simulating nyx quitting.
- **Verify (session 2)** boots a fresh app on the **same SQLite DB** (same
  `NYX_DATA_DIR`) and asserts the restore contract: the **3 terminals are restored
  with their scrollback**, the voluntarily-closed default is **NOT re-spawned**, the
  **reordered order persists**, and the **auto-naming reflects each cwd**.

The two specs share one data dir (so the relaunch reads the persisted DB) and hand off
the expected ids/markers/order via a small JSON in that dir; see `wdio.conf.cjs`
(`specDataDir`, `onPrepare` cleans the data root). The dir is pinned via
**`NYX_DATA_DIR`** — the portable DB-location override honored by the Electron shell on
every OS — so the scenario is deterministic on Windows too. Each non-restore spec gets
its own isolated, temporary data dir.

### v2.2 cockpit gate (`specs/cockpit-01-seed.e2e.cjs` → `cockpit-02-verify.e2e.cjs`)

The v2.2 "travail actif" cockpit validation gate (PRD `01KWHPV32MD0JCGM4F71APNKQ0`, task
`01KWHPV33TY0VM284TJEEBGP8K`), a seed→verify pair sharing one `NYX_DATA_DIR` (the `cockpit`
key in `specDataDir`) like the restore pair:

- **Seed (session 1)** drives a real free-work feature end-to-end **through the cockpit**:
  opens a project whose root is a **real git repo** (so the Git-changes volet is non-empty),
  runs a managed command (a **service**) + a **terminal** with observable output, and starts a
  **real agent session** by POSTing the MCP `agent_session_event` tool over the sidecar's
  loopback (`http://127.0.0.1:$NYX_MCP_PORT/mcp`) — the exact transport a Claude Code hook uses,
  so it writes a genuine `agent_sessions` row + domain event with **zero test-only backend
  plumbing**. It then navigates into the cockpit (`#/cockpit/project/<id>`, free-work) and
  asserts the live runtime navigator, the live terminal pane (typed through the reused
  `__nyxDeck`/`__nyxDeckInput` seams), and the Services / Changements / Timeline / Agents volets,
  plus the **full v2.2 gate checklist** (workspace-centric, live-no-polling, no Execution entity,
  agents without a 3rd tree category (Q-013), no Attention Inbox (Q-007), reserved review/Spec
  seams).
- **Verify (session 2)** relaunches on the same DB and asserts the workspace **context resumes**
  (terminal restored + bound, service + agent session persisted) and re-renders in the cockpit.

The seed uses two seams (no new production code): the shell `window.__nyx` seam to seed the
runtime, and the **real MCP loopback** (fixed `NYX_MCP_PORT`) to seed the agent session.

### v2.5.6 cockpit-rebranch gate (`specs/cockpit-v256-*.e2e.cjs`)

The v2.5.6 "Cockpit de pilotage" gate (PRD `01KXBNX508EYHSQ1CW7RWYKV95`, task
`01KXBNX51EPQ69MVKYG4W960HE`). The v2.2 cockpit screen + its `#/cockpit/…` route are **gone**
(PDR-0025) and `<TerminalManager>`'s `window.__nyx` seam with them, so these specs drive the
**current multi-page shell** (Terminals / Work / Knowledge) through the seams it actually
publishes — `window.__nyxWork` (`{ createProject, listProjects }`, base-layout) and the
per-terminal `window.__nyxDeck` / `window.__nyxDeckInput` (terminal-deck) — plus the real DOM
(rail project tiles, the Terminals sidebar tree, `[data-terminal-id]` deck panes,
`[data-terminal-row]` sortables). Shared drivers live in `specs/cockpit-v256-helpers.cjs`; the
MCP-loopback + git-repo primitives are reused verbatim from `cockpit-helpers.cjs`, and the
handoff primitive (`makeHandoff`) from `restore-helpers.cjs`.

- **`cockpit-v256-close-warning`** — the CHROME leg: min/max/close+drag are the **WM's** (out of
  scope), so it validates the one piece of chrome nyx owns — the **close-warning** guard. It
  MCP-seeds a **live agent session** on a non-resuming project, fires the OS `close-requested`
  push from the main process (`browser.electron.execute`), and asserts the guard withholds the
  close and lists the session; **Keep open** leaves the window intact.
- **`cockpit-v256-crud`** — the rail project **kebab CRUD**: rename project (settings → Save),
  add workspace (folder picker patched in main), remove workspace, delete project; plus the
  kebab⇄right-click parity and the section-nav (v2.2 non-regression).
- **`cockpit-v256-terminals`** — per-terminal affordances: a **live** workspace terminal (typed
  via `__nyxDeck`), **inline rename**, the **CPU/RAM hover tooltip**, **per-row close**, the
  **loose ("libres")** band, and Ctrl+T / Ctrl+W (v2.2 non-regression).
- **`cockpit-v256-reorder-01-seed` → `-02-verify`** — a seed→verify pair sharing the
  `cockpit-v256-reorder` `NYX_DATA_DIR` key: **drag-reorder** terminal rows AND rail projects,
  turn the **resume-agent-sessions** toggle on, then **relaunch** and assert both orders + the
  restored terminals (live PTYs) + the persisted resume setting survive the quit (reorder-persist
  + v2.2/v2.5.5 non-regression).

Run just this gate (avoiding the stale `cockpit-0*` / pre-shell specs):

```sh
cd e2e && bun install
bun run test -- --spec ./specs/cockpit-v256-close-warning.e2e.cjs \
               --spec ./specs/cockpit-v256-crud.e2e.cjs \
               --spec ./specs/cockpit-v256-terminals.e2e.cjs \
               --spec ./specs/cockpit-v256-reorder-01-seed.e2e.cjs \
               --spec ./specs/cockpit-v256-reorder-02-verify.e2e.cjs
```

### The other journeys

`commands`, `workspace`, `typing`, `sidebar-redesign`, `rail-and-list`, `idle-replay`
exercise the managed-command band, projects/workspaces + auto-attach, per-terminal
typing, and the sidebar/animation surfaces — all through the same seams.

### The test seam

The app exposes an **inert** control seam on `window.__nyx`
(under `apps/desktop/renderer/src/…` — the exact module may have moved with the
navigation-shell rewrite) plus per-terminal read/input seams on `window.__nyxDeck` /
`window.__nyxDeckInput`, all keyed by record id. **Historical rationale:** these seams
date from **xterm**, which painted to a **WebGL canvas** — the text was not in the DOM,
so a WebDriver could not type into or query it directly. Since the **wterm (DOM render)**
switch (PDR-0026) the terminal text *is* in the DOM, but the seams are **retained** for
**deterministic, id-keyed** driving (typing into a specific terminal without racing the
DOM). They are **gated behind the build-time flag `VITE_NYX_E2E=1`**, so they never ship
to real production; the Electron `bun run build:e2e` script builds the renderer with that
flag.

## Isolation & cleanup (per the done-criteria)

- Each spec runs against a **temporary `userData`** pinned via `NYX_DATA_DIR` (cleaned
  at `onPrepare`, swept at `onComplete`). The restore pair deliberately shares one dir.
- When the electron service closes a session, the Electron app's `before-quit` runs the
  ordered **sidecar** shutdown, which stops every PTY/managed command — so no
  sidecar/PTY leaks between sessions.

## Prerequisites

Shared (both OSes):

- **bun** (the project's package manager; it drives the WDIO v9 toolchain in this
  folder via its own `e2e/bun.lock`, isolated from the root deps).
- A built Electron app with the e2e seam. `onPrepare` builds it for you
  (`bun run build:e2e` in `apps/desktop`); set `NYX_E2E_SKIP_BUILD=1` to reuse a build.
- The **`nyx-sidecar` binary** built for the host OS (`cargo build -p nyx-sidecar`) — the
  Electron main process spawns it over the stdin/stdout pipe. `bun run build:e2e` already
  builds + stages it (`build:sidecar` + `copy:sidecar`); the harness can also point
  `NYX_SIDECAR_BIN` at a cargo-target binary directly.

### Windows

- A **POSIX shell** for the specs (`export` / `echo "$FOO"` / `printf`). The sidecar
  PTY honors `$SHELL` first; `wdio.conf.cjs` auto-detects **Git Bash** when `$SHELL` is
  unset, so no WSL is required.
- No display needed (Electron runs windowed; Chromedriver drives it).

### Linux

- A **display**: a real X/Wayland session (`$DISPLAY` / `$WAYLAND_DISPLAY`) or **`xvfb`**
  in CI. Unlike WebKitWebDriver, Electron's Chromium *can* run headless, but the PRD's
  Wayland-native surface is validated under a real Wayland session at the phase-7 gate.
- `wdio.conf.cjs` reconciles three Linux launch realities so Chromedriver can attach:
  - it **strips `ELECTRON_RUN_AS_NODE`** (a common dev-shell/Electron-CLI export) — otherwise
    every `electron <entry>` runs as plain Node and the app crashes on boot;
  - it passes `--no-sandbox --disable-dev-shm-usage --disable-gpu --ozone-platform=x11` so the
    renderer child comes up and the DevTools port appears (X11 over XWayland; the Wayland-native
    surface is the phase-7 gate);
  - it aligns Chromium's `--user-data-dir` with the app's **pinned** Electron userData (the app
    pins `<appData>/nyx-v2` in code as a v1-safety guard, ignoring `--user-data-dir`), by
    steering `XDG_CONFIG_HOME`/`APPDATA` to a fixed e2e root so both agree and
    `DevToolsActivePort` lands where Chromedriver reads it.

## Run locally

```sh
# 1) Install the WDIO toolchain (once)
cd e2e && bun install

# 2) Run — builds the Electron app (with the e2e seam) on first run, then drives it
bun run test
```

```sh
# Reuse an existing build and skip the (long) compile:
NYX_E2E_SKIP_BUILD=1 bun run test
```

## What runs HERE vs DEFERRED to the phase-7 gate

This task **ports the harness code** (the Electron driver config, the e2e build wiring,
the per-spec `NYX_DATA_DIR` isolation + teardown, and the shell-agnostic specs). Two
things are intentionally NOT exercised green in this session, and are the **phase-7
native gate**:

- **The actual suite run.** `bun install` pulls the WDIO v9 + `wdio-electron-service`
  + Chromedriver toolchain (network) and the run needs a display. We do **not** fabricate
  a green run here; the harness is wired and ready.
- **Wayland-specific assertions on Linux.** The PRD's Wayland/HiDPI surface is the
  phase-7 native Linux gate. Any Wayland-only assertion is the gate's responsibility;
  document the skip there rather than asserting it off-target.

## CI dependencies (documented only — no workflow committed)

On a Debian/Ubuntu runner:

```sh
sudo apt-get update
sudo apt-get install -y xvfb            # virtual display for headed Electron
cargo build -p nyx-sidecar             # the runtime binary the main process spawns
cd e2e && bun install --frozen-lockfile
xvfb-run -a bun run test
```

A GitHub Actions step would be roughly:

```yaml
- run: cargo build -p nyx-sidecar
- run: cd e2e && bun install --frozen-lockfile
- run: xvfb-run -a bun run test
  working-directory: e2e
```

(No workflow file is committed; this is the documented recipe only.)
