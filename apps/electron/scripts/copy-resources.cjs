#!/usr/bin/env node
/**
 * Stage the read-only BUNDLED RESOURCES (currently the Claude integration plugin)
 * into the Electron app so a packaged build resolves them OUTSIDE the ASAR archive
 * (task #20). These are read at runtime by the core-host via the resolved
 * `resourceDir` (see `core-host/app-paths.ts` + nyx-core `resolve_bundled_plugin_dir`).
 *
 * ### The contract this script honors
 *
 * The Rust resolver (`crates/nyx-core/src/plugin.rs::resolve_bundled_plugin_dir`)
 * looks for the bundled plugin at:
 *
 *     <resource_dir>/resources/claude-plugin/.claude-plugin/marketplace.json
 *
 * In a PACKAGED Electron build the main process passes
 * `resource_dir = <app>/resources/app.asar.unpacked` (see `main/core-host.ts
 * resolveResourceDir`). So the plugin must end up at:
 *
 *     <app>/resources/app.asar.unpacked/resources/claude-plugin/...
 *
 * electron-builder takes our `dist/**` and packs it into `app.asar`, unpacking only
 * the `asarUnpack` globs into `app.asar.unpacked`. Therefore we stage the plugin at
 * `dist/resources/claude-plugin/**` AND `electron-builder.yml` unpacks
 * `dist/resources/**` — which lands it at the exact path the resolver expects.
 *
 * In DEV the host's `resource_dir` is null, and the Rust resolver falls back to the
 * source tree (`<nyx-core>/resources/claude-plugin`), so this staging is a
 * PACKAGING concern only — but we run it on every `build` so the packaged layout is
 * always correct (no separate packaging-only build path to drift).
 *
 * Plain CJS + `fs.cpSync` so it is cross-platform (no `cp -r` / `xcopy`).
 */
"use strict";
const fs = require("node:fs");
const path = require("node:path");

// The canonical plugin source lives in nyx-core (the single source of truth shared
// with the Tauri shell's bundled resource).
const src = path.resolve(
  __dirname,
  "..",
  "..",
  "..",
  "crates",
  "nyx-core",
  "resources",
  "claude-plugin",
);
// Stage under dist/resources/claude-plugin so the unpacked packaged layout becomes
// `<app>/resources/app.asar.unpacked/resources/claude-plugin` (see header).
const dest = path.resolve(__dirname, "..", "dist", "resources", "claude-plugin");

const manifest = path.join(src, ".claude-plugin", "marketplace.json");
if (!fs.existsSync(manifest)) {
  console.error(
    `[copy-resources] bundled plugin manifest not found at ${manifest} — is crates/nyx-core/resources/claude-plugin present?`,
  );
  process.exit(1);
}

fs.rmSync(dest, { recursive: true, force: true });
fs.mkdirSync(path.dirname(dest), { recursive: true });
fs.cpSync(src, dest, { recursive: true });
console.log(`[copy-resources] staged ${src} -> ${dest}`);
