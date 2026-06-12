# nyx e2e (tauri-driver + WebdriverIO)

End-to-end tests that drive the **real** nyx Tauri app on Linux through
[`tauri-driver`](https://v2.tauri.app/develop/tests/webdriver/) and
WebdriverIO. They cover what neither the jsdom unit suite nor the mock-runtime
Rust tests can: the actual app process, a real WebView, and a real PTY/shell.

## What is tested (`specs/terminal.e2e.cjs`)

1. **Env persists** — `export FOO=bar...` then `echo "$FOO"`, asserting the value
   survives between commands.
2. **Program output** — run `printf` and assert a known marker appears.
3. **Resize** — resize the window twice; assert the app does not crash and the
   terminal still accepts input afterwards (reflow is best-effort; no-crash is
   the contract).
4. **`exit`** — typing `exit` closes the shell; the `[process exited]` notice
   (emitted by the backend, written by `src/components/terminal/use-pty.ts`) appears.

Terminal output is read through `window.__nyx.readBuffer()`, a small inert test
seam exposed by `src/app.tsx` (xterm paints to a WebGL canvas, so the text is
not in the DOM and a WebDriver cannot query it directly).

## Why WebdriverIO v7

tauri-driver targets the classic W3C/JSON-Wire session protocol that WDIO v7
speaks directly. Newer WDIO majors bundle a different webdriver/session stack
that has repeatedly mismatched tauri-driver's session/capability negotiation.
The Tauri docs and this harness therefore pin **WDIO v7** (`e2e/package.json`).
If you bump WDIO, expect to revisit `beforeSession`/`afterSession` and the
capabilities in `wdio.conf.cjs`.

## Prerequisites

- **Rust + cargo**, and `tauri-driver`:
  ```sh
  cargo install tauri-driver --locked   # → ~/.cargo/bin/tauri-driver
  ```
- **WebKitWebDriver** (the native driver tauri-driver shells out to):
  - Arch: `webkit2gtk-4.1` / `webkitgtk-6.0` provide `/usr/bin/WebKitWebDriver`.
  - Debian/Ubuntu (CI): package **`webkit2gtk-driver`**.
- A **display**. WebKitWebDriver has no headless mode, so either a real X
  server (`$DISPLAY` set) or **`xvfb`** in CI (see below).
- **bun** (the project's single package manager; it drives the WDIO v7 toolchain
  in this folder via its own `e2e/bun.lock` — isolated from the root deps).

## Run locally

```sh
# 1) Install the WDIO toolchain (once)
cd e2e && bun install

# 2) Run — builds the release binary on first run, then drives it
bun run test
```

`onPrepare` builds the release binary (`bun run tauri build --no-bundle`) so the
suite is self-contained. To reuse an existing build and skip the (long) compile:

```sh
NYX_E2E_SKIP_BUILD=1 bun run test
```

The binary it launches is `src-tauri/target/release/nyx` (the Cargo package is
named `nyx`); `wdio.conf.cjs` points `tauri:options.application` at it.

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
