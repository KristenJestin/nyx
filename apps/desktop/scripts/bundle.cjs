#!/usr/bin/env node
/**
 * Build the Electron main + preload entrypoints into SELF-CONTAINED CommonJS files with
 * esbuild.
 *
 * Why bundle (not bare `tsc`): a `sandbox: true` preload runs in a restricted context whose
 * `require` only resolves a few built-ins (`electron`, `events`, …) — it CANNOT `require` a
 * sibling `../shared/ipc.js`. Bundling inlines the shared allowlist into one file, so the
 * preload is sandbox-safe while keeping a single source of truth.
 *
 * `electron` is marked EXTERNAL (provided by the runtime, never bundled). The runtime
 * authority is the nyx-sidecar BINARY, spawned over a pipe (staged by copy-sidecar.cjs),
 * not a bundled module.
 *
 * Typecheck is a separate step (`bun run typecheck`, full `tsc --noEmit`); esbuild only
 * transpiles/bundles.
 */
"use strict";
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
  // `electron` is provided by the runtime, never bundled. The runtime authority is the
  // nyx-sidecar BINARY (spawned over a pipe), not a bundled module — nothing else is external.
  external: ["electron"],
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
    // Emit the IPC allowlist as a standalone module too, so a smoke can `require` it
    // without re-bundling the whole main.
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/shared/ipc.ts")],
      outfile: path.join(root, "dist/shared/ipc.js"),
    }),
    // The pipe-protocol codec (the byte-exact mirror of the sidecar's Rust wire) as a
    // standalone, so the packaging smoke can drive the packaged sidecar binary's pipe
    // directly (handshake / pty) without spinning up Electron.
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/shared/pipe-protocol.ts")],
      outfile: path.join(root, "dist/shared/pipe-protocol.js"),
    }),
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/main/window-ipc.ts")],
      outfile: path.join(root, "dist/main/window-ipc.js"),
    }),
    // The main-side SIDECAR MANAGER as a standalone module (the sidecar smokes import it
    // directly). It owns the stdin/stdout MessagePack pipe to the nyx-sidecar binary.
    esbuild.build({
      ...common,
      entryPoints: [path.join(root, "src/main/sidecar.ts")],
      outfile: path.join(root, "dist/main/sidecar.js"),
    }),
    // The main-side core IPC RELAY (renderer↔sidecar) as a standalone module, so a smoke
    // drives the REAL relay (`registerCoreIpc`) against the real front + sidecar.
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
  ];
  await Promise.all(builds);
  console.log(`[bundle] bundled ${builds.length} entrypoint(s) to dist/`);
}

main().catch((e) => {
  console.error("[bundle] FAILED:", e.message);
  process.exit(1);
});
