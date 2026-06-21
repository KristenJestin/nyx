#!/usr/bin/env node
/**
 * Build the Electron main, preload AND core-host entrypoints into SELF-CONTAINED
 * CommonJS files with esbuild.
 *
 * Why bundle (not bare `tsc`):
 *   - a `sandbox: true` preload runs in a restricted context whose `require` only
 *     resolves a few built-ins (`electron`, `events`, …) — it CANNOT `require` a
 *     sibling `../shared/ipc.js`. Bundling inlines the shared allowlist into one
 *     file, so the preload is sandbox-safe while keeping a single source of truth.
 *   - the core-host is spawned via `ELECTRON_RUN_AS_NODE`; a single bundled file is
 *     trivial to locate + ship (and to keep OUTSIDE asar) for packaging (task #23).
 *
 * `electron` is marked EXTERNAL (provided by the runtime, never bundled). `nyx-napi`
 * is also external — its `.node` is staged separately (copy-napi.cjs) and loaded at
 * runtime from `dist/native`, never inlined.
 *
 * Typecheck is a separate step (`bun run typecheck`, full `tsc --noEmit`); esbuild
 * only transpiles/bundles.
 */
"use strict";
const fs = require("node:fs");
const path = require("node:path");
const esbuild = require("esbuild");

const root = path.resolve(__dirname, "..");

/** @type {import('esbuild').BuildOptions} */
const common = {
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node20", // Electron 42 embeds Node 24; node20 features are a safe floor.
  sourcemap: true,
  // Never bundle the runtime or the native addon into these files.
  external: ["electron", "@nyx/napi"],
  logLevel: "info",
};

async function main() {
  const builds = [
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/main/index.ts")],
      outfile: path.join(root, "dist/main/index.js"),
    }),
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/preload/index.ts")],
      outfile: path.join(root, "dist/preload/index.js"),
    }),
    // Emit the IPC allowlist + window handlers as standalone modules too, so the
    // smoke harness and the phase-3 renderer adapter can `require`/import them
    // without re-bundling the whole main.
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/shared/ipc.ts")],
      outfile: path.join(root, "dist/shared/ipc.js"),
    }),
    // The host wire-protocol module (carries the flow-control constants
    // HIGH_WATER / LOW_WATER / CHUNK_BYTES) as a standalone, so the flow-control
    // smoke can import the exact thresholds the host enforces.
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/shared/host-protocol.ts")],
      outfile: path.join(root, "dist/shared/host-protocol.js"),
    }),
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/main/window-ipc.ts")],
      outfile: path.join(root, "dist/main/window-ipc.js"),
    }),
    // The main-side core-host MANAGER as a standalone module (the smoke + the
    // phase-3 IPC bridge import it directly).
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/main/core-host.ts")],
      outfile: path.join(root, "dist/main/core-host.js"),
    }),
    // The main-side core IPC RELAY (renderer↔core-host) as a standalone module, so
    // the phase-4 gate harness drives the REAL relay (`registerCoreIpc`) against the
    // real front + host — measuring the production path, not a duplicate of it.
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/main/core-ipc.ts")],
      outfile: path.join(root, "dist/main/core-ipc.js"),
    }),
    // The Wayland/HiDPI flag module as a standalone (its smoke imports it).
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/main/wayland.ts")],
      outfile: path.join(root, "dist/main/wayland.js"),
    }),
    // The core-host PTY MANAGER as a standalone module, so the deterministic
    // flow-control smoke can drive it against a FAKE addon (no live shell needed)
    // and assert the chunking + credit/backpressure logic on any OS.
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/core-host/pty-manager.ts")],
      outfile: path.join(root, "dist/core-host/pty-manager.js"),
    }),
  ];
  // The core-host entry lands in task #2; bundle it once it exists.
  const coreHostEntry = path.join(root, "src/core-host/index.ts");
  if (fs.existsSync(coreHostEntry)) {
    builds.push(
      esbuild.build({
        ...common,
        entryPoints: [coreHostEntry],
        outfile: path.join(root, "dist/core-host/index.js"),
      }),
    );
  }
  await Promise.all(builds);
  console.log(`[bundle] bundled ${builds.length} entrypoint(s) to dist/`);
}

main().catch((e) => {
  console.error("[bundle] FAILED:", e.message);
  process.exit(1);
});
