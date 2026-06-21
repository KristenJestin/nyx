#!/usr/bin/env node
/**
 * Stage the built native addon (`nyx-napi`) into the Electron app so the packaged
 * build can load it OUTSIDE the ASAR archive (a `.node` cannot be `require`d from
 * inside an asar; see task #23 + electron-builder `asarUnpack`).
 *
 * Copies the napi-rs OFFICIAL loader (`index.js`), its types (`index.d.ts`) and the
 * platform-suffixed `.node` produced by `napi build` (e.g. `nyx-napi.win32-x64-msvc.node`,
 * `nyx-napi.linux-x64-gnu.node`) into `dist/native/`. The core-host requires
 * `dist/native/index.js`, which resolves the correct suffix for the host platform.
 *
 * The `.node` must be built FIRST (`bun run --filter @nyx/napi build`) — this script
 * only stages whatever artifacts exist, and errors loudly if none is present.
 */
"use strict";
const fs = require("node:fs");
const path = require("node:path");

const napiDir = path.resolve(__dirname, "..", "..", "..", "crates", "nyx-napi");
const dest = path.resolve(__dirname, "..", "dist", "native");

fs.mkdirSync(dest, { recursive: true });

// The napi-rs loader + types are always required.
for (const f of ["index.js", "index.d.ts"]) {
  const from = path.join(napiDir, f);
  if (!fs.existsSync(from)) {
    console.error(`[copy-napi] missing ${from} — build @nyx/napi first`);
    process.exit(1);
  }
  fs.copyFileSync(from, path.join(dest, f));
}

// Copy every platform-suffixed .node that exists (normally just the host's).
const nodeFiles = fs.readdirSync(napiDir).filter((f) => f.endsWith(".node"));
if (nodeFiles.length === 0) {
  console.error(
    `[copy-napi] no .node addon found in ${napiDir} — run \`bun run --filter @nyx/napi build\` first`,
  );
  process.exit(1);
}
for (const f of nodeFiles) {
  fs.copyFileSync(path.join(napiDir, f), path.join(dest, f));
}
console.log(`[copy-napi] staged loader + ${nodeFiles.join(", ")} -> ${dest}`);
