#!/usr/bin/env node
/**
 * Stage the built `nyx-sidecar` Rust binary into the Electron app so a dev run (and a
 * packaged build) launches it from a real on-disk path OUTSIDE the ASAR archive
 * (PDR-0007 — the sidecar is placed outside `app.asar`, signed with the app, and launched
 * from the Electron resources).
 *
 * The binary must be built FIRST (`cargo build -p nyx-sidecar [--release]`). This script
 * copies `target/<profile>/nyx-sidecar[.exe]` into `dist/sidecar/`, where the main-side
 * `Sidecar` manager resolves it (`resolveSidecarBin` dev fallback). `NYX_SIDECAR_BIN`
 * overrides the path entirely (the e2e harness / a dev launch point it at the cargo target
 * binary directly, so this staging is not strictly required to run, only to package).
 *
 * Profile: `NYX_SIDECAR_PROFILE` (release|debug), else release if present, else debug.
 */
"use strict";
const fs = require("node:fs");
const path = require("node:path");

const exe = process.platform === "win32" ? "nyx-sidecar.exe" : "nyx-sidecar";
const targetRoot = path.resolve(__dirname, "..", "..", "..", "target");
const dest = path.resolve(__dirname, "..", "dist", "sidecar");

/** Pick the built binary: the requested profile, else release, else debug. */
function resolveBuilt() {
  const requested = process.env.NYX_SIDECAR_PROFILE;
  const profiles = requested ? [requested] : ["release", "debug"];
  for (const profile of profiles) {
    const candidate = path.join(targetRoot, profile, exe);
    if (fs.existsSync(candidate)) return candidate;
  }
  return null;
}

const from = resolveBuilt();
if (!from) {
  console.error(
    `[copy-sidecar] no built ${exe} under ${targetRoot}/{release,debug} — run \`cargo build -p nyx-sidecar\` first`,
  );
  process.exit(1);
}

fs.mkdirSync(dest, { recursive: true });
const to = path.join(dest, exe);
fs.copyFileSync(from, to);
fs.chmodSync(to, 0o755);
console.log(`[copy-sidecar] staged ${from} -> ${to}`);
