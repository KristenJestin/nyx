# nyx e2e (tauri-driver + WebdriverIO)

End-to-end tests that drive the **real** nyx Tauri app through
[`tauri-driver`](https://v2.tauri.app/develop/tests/webdriver/) and WebdriverIO,
on **Linux** (WebKitWebDriver) and **Windows** (Microsoft Edge WebDriver /
WebView2). They cover what neither the jsdom unit suite nor the mock-runtime
Rust tests can: the actual app process, a real WebView, and a real PTY/shell.

## What is tested

### Smoke (`specs/terminal.e2e.cjs`)

1. **Env persists** — `export FOO=bar...` then `echo "$FOO"`, asserting the value
   survives between commands.
2. **Program output** — run `printf` and assert a known marker appears.
3. **Resize** — resize the window twice; assert the app does not crash and the
   terminal still accepts input afterwards (reflow is best-effort; no-crash is
   the contract).
4. **`exit`** — typing `exit` closes the shell; the `[process exited]` notice
   (emitted by the backend, written by `src/components/terminal/use-pty.ts`) appears.

### Restore scenario (`specs/restore-01-seed.e2e.cjs` → `restore-02-verify.e2e.cjs`)

The big PRD 1 scenario, split across **two app sessions** (a real close + reopen):

- **Seed (session 1)** opens **3 terminals at distinct cwds** (`/tmp`, `/usr`,
  `/etc`) each running a command with observable output (a unique marker echoed
  into its scrollback), **reorders** them to a known order, and **closes the
  auto-created default terminal** voluntarily. The session then ends —
  tauri-driver kills the app — simulating nyx quitting.
- **Verify (session 2)** boots a fresh app on the **same SQLite DB** (same
  `XDG_DATA_HOME`) and asserts the restore contract: the **3 terminals are
  restored with their scrollback** (each marker is back in its buffer), the
  voluntarily-closed default is **NOT re-spawned** (no live pane), the
  **reordered order persists**, and the **auto-naming reflects each cwd**.

The two specs share a fixed data dir (so the relaunch reads the persisted DB) and
hand off the expected ids/markers/order via a small JSON in that dir; see
`wdio.conf.cjs` (`specDataDir`, `onPrepare` cleans the data root for a
deterministic empty start). The dir is pinned via **`NYX_DATA_DIR`** — a portable
DB-location override honored on every OS by the backend (`resolve_data_dir`), so
the scenario is deterministic on Windows too, where `XDG_DATA_HOME` has no effect
(it only steers the Linux data path). Each non-restore spec gets its own isolated
data dir.

### The test seam

The app exposes an **inert** control seam on `window.__nyx`
(`src/components/sidebar/terminal-manager.tsx`) plus per-terminal read/input seams
on `window.__nyxDeck` / `window.__nyxDeckInput` (`terminal-deck.tsx`), all keyed
by record id. The e2e drives + reads terminals through these because xterm paints
to a WebGL canvas — the text is not in the DOM and a WebDriver cannot type into
or query it directly. The seams are inert in production (nothing reads them).

## Why WebdriverIO v7

tauri-driver targets the classic W3C/JSON-Wire session protocol that WDIO v7
speaks directly. Newer WDIO majors bundle a different webdriver/session stack
that has repeatedly mismatched tauri-driver's session/capability negotiation.
The Tauri docs and this harness therefore pin **WDIO v7** (`e2e/package.json`).
If you bump WDIO, expect to revisit `beforeSession`/`afterSession` and the
capabilities in `wdio.conf.cjs`.

## Prerequisites

Shared (both OSes):

- **Rust + cargo**, and `tauri-driver`:
  ```sh
  cargo install tauri-driver --locked   # → ~/.cargo/bin/tauri-driver(.exe)
  ```
- **bun** (the project's single package manager; it drives the WDIO v7 toolchain
  in this folder via its own `e2e/bun.lock` — isolated from the root deps).

### Linux

- **WebKitWebDriver** (the native driver tauri-driver shells out to):
  - Arch: `webkit2gtk-4.1` / `webkitgtk-6.0` provide `/usr/bin/WebKitWebDriver`.
  - Debian/Ubuntu (CI): package **`webkit2gtk-driver`**.
- A **display**. WebKitWebDriver has no headless mode, so either a real X
  server (`$DISPLAY` set) or **`xvfb`** in CI (see below).

### Windows

- **Microsoft Edge WebDriver** (`msedgedriver.exe`) whose version **matches the
  installed WebView2 runtime** — mismatched versions make the WebDriver session
  hang. Find the runtime version, then grab the matching driver:
  ```powershell
  # WebView2 runtime version:
  (Get-ItemProperty "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}").pv
  # Download for that EXACT version:
  #   https://msedgedriver.microsoft.com/<version>/edgedriver_win64.zip
  ```
  Put `msedgedriver.exe` on `$PATH`, or point `$env:MSEDGEDRIVER` at it (the
  config forwards it to tauri-driver via `--native-driver`).
- **WebView2 runtime** — preinstalled on Windows 11.
- A **POSIX shell** for the specs (they run `export` / `echo "$FOO"` / `printf`).
  The app honors `$SHELL` first; `wdio.conf.cjs` auto-detects **Git Bash** when
  `$SHELL` is unset, so no WSL is required. Set `$SHELL` to override.
- No display / `xvfb` needed (Edge WebDriver runs windowed).

## Run locally

### Linux / macOS

```sh
# 1) Install the WDIO toolchain (once)
cd e2e && bun install

# 2) Run — builds the release binary on first run, then drives it
bun run test
```

### Windows (PowerShell)

```powershell
cd e2e; bun install                                 # once
$env:MSEDGEDRIVER = "C:\path\to\msedgedriver.exe"   # if not on PATH
# $env:SHELL is auto-detected (Git Bash) when unset; set it to override.
bun run test                                        # builds nyx.exe, then drives it
```

`onPrepare` builds the release binary (`bun run tauri build --no-bundle`) so the
suite is self-contained. To reuse an existing build and skip the (long) compile:

```sh
NYX_E2E_SKIP_BUILD=1 bun run test
```

The binary it launches is `src-tauri/target/release/nyx` (`nyx.exe` on Windows;
the Cargo package is named `nyx`); `wdio.conf.cjs` points
`tauri:options.application` at it and appends `.exe` on Windows automatically.

## CI dependencies (not wired up here — documented only)

Per the task scope we list the CI deps without standing up a full pipeline. On a
Debian/Ubuntu runner you need:

```sh
sudo apt-get update
sudo apt-get install -y \
  webkit2gtk-driver \      # provides WebKitWebDriver
  xvfb \                   # virtual display (WebKitWebDriver needs $DISPLAY)
  libwebkit2gtk-4.1-dev \  # build/runtime deps for the Tauri app
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev
cargo install tauri-driver --locked
```

Then run under a virtual display:

```sh
xvfb-run -a bun run test        # from e2e/, after `bun install`
```

A GitHub Actions step would be roughly:

```yaml
- run: cargo install tauri-driver --locked
- run: cd e2e && bun install --frozen-lockfile
- run: xvfb-run -a bun run test
  working-directory: e2e
```

(No workflow file is committed; this is the documented recipe only.)
