#!/usr/bin/env node
/**
 * Produce a THROWAWAY unpacked package for the current OS (electron-builder `--dir`),
 * to de-risk embedding the nyx-sidecar binary. Not an installer — just the unpacked app
 * dir with `app.asar` + `app.asar.unpacked/` so we can prove the packaged sidecar binary
 * spawns + drives a PTY over the pipe.
 *
 * Steps: full build (main+preload bundle, renderer build+copy, sidecar build+stage),
 * then `electron-builder --dir` for the host OS. The sidecar MUST be built natively on
 * the SAME OS first (`cargo build -p nyx-sidecar`, run by `build:sidecar`) — electron-
 * builder does not cross-compile native binaries.
 */
"use strict";
const { spawnSync } = require("node:child_process");
const path = require("node:path");

const appDir = path.resolve(__dirname, "..");
const osFlag = process.platform === "win32" ? "--win" : process.platform === "darwin" ? "--mac" : "--linux";

function run(cmd, args, opts) {
  console.log(`[package] $ ${cmd} ${args.join(" ")}`);
  const r = spawnSync(cmd, args, { stdio: "inherit", cwd: appDir, shell: process.platform === "win32", ...opts });
  if (r.status !== 0) {
    console.error(`[package] step failed: ${cmd} ${args.join(" ")} (status ${r.status})`);
    process.exit(r.status || 1);
  }
}

// 1. Full app build (bundles + renderer + sidecar build & staging).
run("bun", ["run", "build"]);

// 2. electron-builder unpacked dir for this OS. Invoke its `cli.js` directly via
// node (cross-platform; avoids per-OS `.bin` shim naming differences). `--config`
// points at our yml.
const builderPkgDir = path.dirname(require.resolve("electron-builder/package.json"));
const builderCli = path.join(builderPkgDir, require("electron-builder/package.json").bin["electron-builder"]);
run(process.execPath, [builderCli, "--dir", osFlag, "--config", "electron-builder.yml"]);

console.log("[package] done — unpacked package under release/");
