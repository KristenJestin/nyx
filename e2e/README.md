# nyx e2e (wdio-electron-service + WebdriverIO)

End-to-end tests that drive the **real** nyx **Electron** app through
[`wdio-electron-service`](https://webdriver.io/docs/desktop-testing/electron) and
WebdriverIO, on **Linux** and **Windows**. They cover what neither the jsdom unit
suite nor the mock-runtime Rust tests can: the actual app process (Electron main +
the dedicated core-host), a real Chromium WebGL render, and a real PTY/shell.

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
  whose `before-quit` stops the core-host — simulating nyx quitting.
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

### The other journeys

`commands`, `workspace`, `typing`, `sidebar-redesign`, `rail-and-list`, `idle-replay`
exercise the managed-command band, projects/workspaces + auto-attach, per-terminal
typing, and the sidebar/animation surfaces — all through the same seams.

### The test seam

The app exposes an **inert** control seam on `window.__nyx`
(`apps/frontend/src/components/sidebar/terminal-manager.tsx`) plus per-terminal
read/input seams on `window.__nyxDeck` / `window.__nyxDeckInput`
(`terminal-deck.tsx`), all keyed by record id. The e2e drives + reads terminals
through these because xterm paints to a WebGL canvas — the text is not in the DOM and
a WebDriver cannot type into or query it directly. The seams are **gated behind the
build-time flag `VITE_NYX_E2E=1`**, so they never ship to real production; the
Electron `bun run build:e2e` script builds the renderer with that flag.

## Isolation & cleanup (per the done-criteria)

- Each spec runs against a **temporary `userData`** pinned via `NYX_DATA_DIR` (cleaned
  at `onPrepare`, swept at `onComplete`). The restore pair deliberately shares one dir.
- When the electron service closes a session, the Electron app's `before-quit` runs the
  ordered core-host shutdown (`coreHost.stop()`), which stops every PTY/managed command
  — so no `core-host`/PTY leaks between sessions.

## Prerequisites

Shared (both OSes):

- **bun** (the project's package manager; it drives the WDIO v9 toolchain in this
  folder via its own `e2e/bun.lock`, isolated from the root deps).
- A built Electron app with the e2e seam. `onPrepare` builds it for you
  (`bun run build:e2e` in `apps/electron`); set `NYX_E2E_SKIP_BUILD=1` to reuse a build.
- The **native `.node`** built for the host OS first
  (`bun run --filter @nyx/napi build`) — the core-host loads it.

### Windows

- A **POSIX shell** for the specs (`export` / `echo "$FOO"` / `printf`). The core-host
  PTY honors `$SHELL` first; `wdio.conf.cjs` auto-detects **Git Bash** when `$SHELL` is
  unset, so no WSL is required.
- No display needed (Electron runs windowed; Chromedriver drives it).

### Linux

- A **display**: a real X/Wayland session (`$DISPLAY` / `$WAYLAND_DISPLAY`) or **`xvfb`**
  in CI. Unlike WebKitWebDriver, Electron's Chromium *can* run headless, but the PRD's
  Wayland-native surface is validated under a real Wayland session at the phase-7 gate.

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
cargo build -p nyx-napi                 # (or `bun run --filter @nyx/napi build`)
cd e2e && bun install --frozen-lockfile
xvfb-run -a bun run test
```

A GitHub Actions step would be roughly:

```yaml
- run: bun run --filter @nyx/napi build
- run: cd e2e && bun install --frozen-lockfile
- run: xvfb-run -a bun run test
  working-directory: e2e
```

(No workflow file is committed; this is the documented recipe only.)
