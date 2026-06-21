#!/usr/bin/env node
/**
 * Dev orchestrator for `@nyx/electron` (cross-platform, no shell-specific syntax).
 *
 * 1. Starts the frontend Vite dev server (`@nyx/frontend dev`, port 1420).
 * 2. Waits for it to answer.
 * 3. BUNDLES the Electron main/preload/core-host once with esbuild (scripts/bundle.cjs)
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
 * @nyx/electron dev` behaves identically on Windows and Linux.
 */
"use strict";
const { spawn } = require("node:child_process");
const path = require("node:path");
const http = require("node:http");

const DEV_URL = process.env.NYX_DEV_SERVER_URL || "http://localhost:1420";
const repoRoot = path.resolve(__dirname, "..", "..", "..");
const electronDir = path.resolve(__dirname, "..");

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

// 1. Vite dev server (filtered through the monorepo runner).
run("bun", ["run", "--filter", "@nyx/frontend", "dev"], { cwd: repoRoot });

// 2. Poll the dev server until it responds, then build main + launch Electron.
function waitForServer(url, attempts) {
  http
    .get(url, () => startElectron())
    .on("error", () => {
      if (attempts <= 0) {
        console.error(`[dev] dev server ${url} never came up`);
        return shutdown(1);
      }
      setTimeout(() => waitForServer(url, attempts - 1), 300);
    });
}

function startElectron() {
  // BUNDLE main/preload/core-host once before launch — the SAME esbuild path prod
  // uses (scripts/bundle.cjs), so the preload is self-contained (shared/ipc inlined)
  // and sandbox-safe. A bare `tsc` would leave a `require("../shared/ipc.js")` the
  // sandboxed preload can't resolve, killing the bridge and forcing the Tauri fallback.
  const bundle = run(process.execPath, [path.join(__dirname, "bundle.cjs")], {
    cwd: electronDir,
  });
  bundle.on("exit", (code) => {
    if (code !== 0) {
      console.error("[dev] bundle failed — aborting launch");
      return shutdown(code || 1);
    }
    // Type-check in the background (non-blocking): types are validated but a type
    // error must NOT block bringing the dev shell up (the bundle is the build of
    // record). Its exit is logged, never fatal to the running Electron.
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
  });
}

waitForServer(DEV_URL, 100);
