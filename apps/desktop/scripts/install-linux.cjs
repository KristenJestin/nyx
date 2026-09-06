#!/usr/bin/env node
/**
 * "Install" nyx from the source tree on a Linux host, WITHOUT an AppImage / FUSE:
 *
 *   1. `cargo build -p nyx-sidecar --release`          — the native sidecar (host arch)
 *   2. `bun run package`                                — full app build + electron-builder
 *                                                         `--dir` → release/linux-unpacked/
 *   3. `bun run smoke:package` (unless `--skip-smoke`)  — proves the sidecar + plugin are
 *                                                         embedded and a PTY streams from the
 *                                                         packaged layout
 *   4. a freedesktop launcher                           — ~/.local/share/applications/nyx-v2.desktop
 *      + a CLI symlink                                  — ~/.local/bin/nyx-v2
 *
 * Both entries point at the ABSOLUTE path of `release/linux-unpacked/nyx-v2` inside this
 * checkout: re-running the script after a `git pull` rebuilds in place and the launcher
 * keeps working. Nothing is copied under /usr or /opt; uninstall = remove the two files.
 *
 * Flags: `--skip-smoke` (skip step 3), `--no-desktop` (skip step 4).
 */
"use strict";
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

if (process.platform !== "linux") {
  console.error(`[install-linux] Linux only (got ${process.platform}); see PACKAGING.md for the other OS.`);
  process.exit(1);
}

const args = new Set(process.argv.slice(2));
const appDir = path.resolve(__dirname, "..");
const repoRoot = path.resolve(appDir, "..", "..");
const unpackedDir = path.join(appDir, "release", "linux-unpacked");
const binary = path.join(unpackedDir, "nyx-v2");

function run(cmd, cmdArgs, opts = {}) {
  console.log(`[install-linux] $ ${cmd} ${cmdArgs.join(" ")}`);
  const r = spawnSync(cmd, cmdArgs, { stdio: "inherit", cwd: appDir, ...opts });
  if (r.status !== 0) {
    console.error(`[install-linux] step failed: ${cmd} ${cmdArgs.join(" ")} (status ${r.status})`);
    process.exit(r.status || 1);
  }
}

// 1. Native sidecar, release profile. copy-sidecar.cjs picks it up because we pin the profile
//    in the environment for the whole run (the default `bun run build` builds a debug sidecar
//    too, but the staged one is the release binary).
run("cargo", ["build", "-p", "nyx-sidecar", "--release"], { cwd: repoRoot });
const env = { ...process.env, NYX_SIDECAR_PROFILE: "release" };

// 2. Full build + unpacked package for this OS.
run("bun", ["run", "package"], { env });

if (!fs.existsSync(binary)) {
  console.error(`[install-linux] expected binary missing: ${binary}`);
  process.exit(1);
}

// 3. Packaging gate (embedding + resolution + PTY from the packaged layout).
if (!args.has("--skip-smoke")) run("bun", ["run", "smoke:package"], { env });

if (args.has("--no-desktop")) {
  console.log(`[install-linux] done — run ${binary}`);
  process.exit(0);
}

// 4. Launcher + CLI symlink.
const home = os.homedir();
const appsDir = path.join(home, ".local", "share", "applications");
const binDir = path.join(home, ".local", "bin");
fs.mkdirSync(appsDir, { recursive: true });
fs.mkdirSync(binDir, { recursive: true });

// `build/icon.png` is the nyx face (electron-builder convention for the installers too).
const icon = path.join(appDir, "build", "icon.png");
const desktop = [
  "[Desktop Entry]",
  "Type=Application",
  "Name=nyx",
  "Comment=nyx v2 — agentic terminal manager (source build)",
  `Exec=${binary} %U`,
  `Path=${unpackedDir}`,
  ...(fs.existsSync(icon) ? [`Icon=${icon}`] : []),
  "Categories=Development;",
  "Terminal=false",
  "StartupWMClass=nyx-v2",
  "",
].join("\n");
const desktopPath = path.join(appsDir, "nyx-v2.desktop");
fs.writeFileSync(desktopPath, desktop);

const link = path.join(binDir, "nyx-v2");
try {
  fs.rmSync(link, { force: true });
} catch {}
fs.symlinkSync(binary, link);

// Best-effort cache refresh; absent tool is fine.
spawnSync("update-desktop-database", [appsDir], { stdio: "ignore" });

console.log(`[install-linux] launcher : ${desktopPath}`);
console.log(`[install-linux] cli      : ${link} -> ${binary}`);
if (!process.env.PATH.split(":").includes(binDir)) {
  console.log(`[install-linux] note: ${binDir} is not on your PATH; the launcher works regardless.`);
}
console.log("[install-linux] done");
