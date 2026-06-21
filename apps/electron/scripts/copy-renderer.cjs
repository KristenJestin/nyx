#!/usr/bin/env node
/**
 * Copy the frontend's production build into the Electron app so a packaged build
 * loads the renderer from `dist/renderer/index.html` (the `file://` path the main
 * process's nav-guard pins). Run after `@nyx/frontend build`.
 *
 * Plain CJS + `fs.cpSync` so it is cross-platform (no `cp -r` / `xcopy`).
 */
"use strict";
const fs = require("node:fs");
const path = require("node:path");

const src = path.resolve(__dirname, "..", "..", "frontend", "dist");
const dest = path.resolve(__dirname, "..", "dist", "renderer");

if (!fs.existsSync(src)) {
  console.error(`[copy-renderer] frontend build not found at ${src} — run the frontend build first`);
  process.exit(1);
}
fs.rmSync(dest, { recursive: true, force: true });
fs.cpSync(src, dest, { recursive: true });
console.log(`[copy-renderer] copied ${src} -> ${dest}`);
