#!/usr/bin/env node
/**
 * Dev orchestrator for `nyx-v2` (cross-platform, no shell-specific syntax).
 *
 * 1. Starts the frontend Vite dev server (`nyx-v2-renderer dev`, port 1420). Its non-zero exit is
 *    HANDLED rather than ignored (it used to be the one failure this script swallowed) — and handled
 *    by ASKING the identity probe of step 2 who holds the port, never by assuming. `strictPort: true`
 *    makes Vite die when 1420 is taken, and the server holding it is very often our OWN
 *    (`bun run dev` in one shell, `bun run dev:desktop` in another): that case ATTACHES. Anything
 *    else aborts, printing the exit code and what the probe measured.
 * 2. Waits for it to answer, and checks WHOSE server answered (scripts/dev-server-identity.cjs).
 *    A presence on the port is not an appartenance: three launches ran this worktree's backend
 *    against another worktree's UI before this check existed.
 * 3. BUNDLES the Electron main/preload once with esbuild (scripts/bundle.cjs)
 *    — NOT a bare `tsc`. A `sandbox: true` preload runs in a restricted context whose
 *    `require` cannot resolve a sibling `../shared/ipc.js`; bare `tsc` would leave that
 *    bare `require` in `dist/preload/index.js`, the preload would die on load
 *    ('Unable to load preload script'), `window.nyxCore` would never be installed, and
 *    the front would fall back to the Tauri adapter and crash. Bundling inlines the
 *    shared allowlist so dev is sandbox-safe and IDENTICAL to prod (scripts/bundle.cjs).
 * 4. Runs a SEPARATE, non-blocking typecheck (`tsc --noEmit`) — types are checked but a
 *    type error never blocks the dev launch (the bundle is the build of record).
 * 5. Launches Electron with `NYX_DEV_SERVER_URL` pointing at the live Vite server
 *    so the main process loads the dev server instead of an on-disk build.
 *
 * Kept as a plain CJS node script (not a shell one-liner) so `bun run --filter
 * nyx-v2 dev` behaves identically on Windows and Linux.
 */
"use strict";
const { spawn } = require("node:child_process");
const path = require("node:path");
const {
  DEV_SERVER_URL,
  IDENTITY,
  VITE_EXIT,
  decideOnViteExit,
  probeDevServerIdentity,
} = require("./dev-server-identity.cjs");

const DEV_URL = process.env.NYX_DEV_SERVER_URL || DEV_SERVER_URL;
const repoRoot = path.resolve(__dirname, "..", "..", "..");
const electronDir = path.resolve(__dirname, "..");
// The renderer root THIS worktree serves — the identity the dev server must claim before we point
// Electron at it. An absolute path: no two worktrees can produce the same one.
const rendererRoot = path.resolve(electronDir, "renderer");

const children = [];
function shutdown(code) {
  for (const c of children) {
    try {
      c.kill();
    } catch {}
  }
  process.exit(code);
}
process.on("SIGINT", () => shutdown(0));
process.on("SIGTERM", () => shutdown(0));

function run(cmd, args, opts) {
  const c = spawn(cmd, args, { stdio: "inherit", shell: process.platform === "win32", ...opts });
  children.push(c);
  return c;
}

// 1. Vite dev server (filtered through the monorepo runner). A non-zero exit is not, on its own, a
// reason to abort: `strictPort: true` makes Vite die when 1420 is taken, and the server holding it
// may be OURS — the `bun run dev` this developer left running in another shell. So the exit is a
// QUESTION for the probe of step 2, not a verdict ([`decideOnViteExit`]).
const vite = run("bun", ["run", "--filter", "nyx-v2-renderer", "dev"], { cwd: repoRoot });
vite.on("exit", (code) => {
  if (code === 0) return;
  decideOnViteExit(DEV_URL, rendererRoot, code, ({ action, message }) => {
    if (action === VITE_EXIT.ATTACH) {
      // The poll below is already asking the same server the same question, and its OURS branch is
      // what launches Electron. Nothing to do here but say what happened — and above all NOT to
      // shut down: an abort at this instant kills `bundle.cjs` mid-write and leaves a truncated
      // `dist/main` / `dist/preload` behind.
      return console.error(`[dev] ${message}`);
    }
    if (launching) {
      // …AND THE SAME PROTECTION ON THE ABORT VERDICTS, because the race is not only the ATTACH
      // one. The poll and this handler probe the SAME server a few hundred milliseconds apart:
      // the poll can answer OURS and start bundling while this probe times out
      // (`PROBE_TIMEOUT_MS`) on a loaded machine and comes back UNREACHABLE. Shutting down then
      // kills `bundle.cjs` mid-write — exactly the truncated `dist/main` / `dist/preload` the
      // ATTACH branch above exists to avoid. The launch has already been decided by a probe that
      // DID get an answer, so this one has nothing to add but a line.
      return console.error(`[dev] ${message} — the launch is already under way, not aborting it`);
    }
    console.error(`[dev] ${message} — aborting launch`);
    shutdown(code || 1);
  });
});

// 2. Poll the dev server until it responds AS OURS, then build main + launch Electron.
function waitForServer(url, attempts) {
  probeDevServerIdentity(url, rendererRoot, (result) => {
    if (result.verdict === IDENTITY.OURS) return startElectron();
    if (result.verdict === IDENTITY.FOREIGN) {
      // Never launch Electron on a server we did not recognise: this backend would drive that
      // UI, and nothing in the running app would say so.
      console.error(`[dev] a FOREIGN server is answering on ${url} — aborting launch`);
      console.error(`[dev] ${result.detail}`);
      return shutdown(1);
    }
    if (attempts <= 0) {
      console.error(`[dev] dev server ${url} never came up`);
      return shutdown(1);
    }
    setTimeout(() => waitForServer(url, attempts - 1), 300);
  });
}

/**
 * IDEMPOTENT. Two callers can now reach the same OURS verdict — the poll below and the Vite-exit
 * handler above, which race by design — and bundling twice into the same `dist/` is how a half-written
 * preload gets loaded.
 */
let launching = false;
function startElectron() {
  if (launching) return;
  launching = true;
  // BUNDLE main/preload once before launch — the SAME esbuild path prod
  // uses (scripts/bundle.cjs), so the preload is self-contained (shared/ipc inlined)
  // and sandbox-safe. A bare `tsc` would leave a `require("../shared/ipc.js")` the
  // sandboxed preload can't resolve, killing the bridge.
  const bundle = run(process.execPath, [path.join(__dirname, "bundle.cjs")], {
    cwd: electronDir,
  });
  bundle.on("exit", (code) => {
    if (code !== 0) {
      console.error("[dev] bundle failed — aborting launch");
      return shutdown(code || 1);
    }
    // BUILD + STAGE the nyx-sidecar binary (the runtime authority) before launch, so the
    // main process resolves it at `dist/sidecar/`. A build/stage failure aborts — the dev
    // shell has no runtime without it.
    const cargo = run("cargo", ["build", "-p", "nyx-sidecar"], { cwd: electronDir });
    cargo.on("exit", (cc) => {
      if (cc !== 0) {
        console.error("[dev] sidecar build failed — aborting launch");
        return shutdown(cc || 1);
      }
      const stage = run(process.execPath, [path.join(__dirname, "copy-sidecar.cjs")], {
        cwd: electronDir,
      });
      stage.on("exit", (sc) => {
        if (sc !== 0) {
          console.error("[dev] sidecar staging failed — aborting launch");
          return shutdown(sc || 1);
        }
        launchElectron();
      });
    });
  });
}

function launchElectron() {
  // Type-check in the background (non-blocking): types are validated but a type error
  // must NOT block bringing the dev shell up (the bundle is the build of record).
  const tsc = run(process.execPath, [require.resolve("typescript/bin/tsc"), "-p", "tsconfig.json", "--noEmit"], {
    cwd: electronDir,
  });
  tsc.on("exit", (c) => {
    if (c !== 0) console.error(`[dev] typecheck reported errors (exit ${c}) — dev shell left running`);
    else console.log("[dev] typecheck clean ✓");
  });

  const electronBin = require("electron");
  const el = run(electronBin, ["."], {
    cwd: electronDir,
    env: { ...process.env, NYX_DEV_SERVER_URL: DEV_URL },
  });
  el.on("exit", (c) => shutdown(c || 0));
}

waitForServer(DEV_URL, 100);
