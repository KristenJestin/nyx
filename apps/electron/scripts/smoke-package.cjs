#!/usr/bin/env node
/**
 * SMOKE-PACKAGING verification (task #23). Given the throwaway unpacked package
 * built by `scripts/package.cjs` (electron-builder `--dir`), this:
 *
 *   1. locates the PACKAGED Electron binary and the PACKAGED, UNPACKED core-host
 *      entry (`resources/app.asar.unpacked/dist/core-host/index.js`) + native addon
 *      (`resources/app.asar.unpacked/dist/native/`), proving the `.node` was
 *      unpacked OUTSIDE the asar;
 *   2. spawns the packaged host via the packaged Electron binary in
 *      `ELECTRON_RUN_AS_NODE=1` (exactly as the packaged main does) and asserts a
 *      correlated `ready` + the minimal PTY streams output — i.e. the `.node`
 *      genuinely loads and runs from the packaged unpack layout on THIS OS.
 *
 * Any failure here is a GATE: it blocks the phase (no silent sidecar fallback).
 *
 * Run with system Node: `node scripts/smoke-package.cjs`.
 */
"use strict";
const { spawn } = require("node:child_process");
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");

const appDir = path.resolve(__dirname, "..");
const releaseDir = path.join(appDir, "release");

function fail(msg) {
  console.error("[smoke-package] FAIL (GATE):", msg);
  process.exit(1);
}

if (!fs.existsSync(releaseDir)) fail(`no release/ dir — run \`bun run package\` first`);

// --- 1. locate the packaged dir for this OS -------------------------------
// electron-builder `--dir` emits e.g. `win-unpacked/`, `linux-unpacked/`.
const unpackedDir = fs
  .readdirSync(releaseDir)
  .map((d) => path.join(releaseDir, d))
  .find((d) => fs.statSync(d).isDirectory() && /unpacked$/.test(path.basename(d)));
if (!unpackedDir) fail(`no *-unpacked dir under ${releaseDir}`);
console.log(`[smoke-package] packaged app dir: ${unpackedDir}`);

// Packaged Electron binary (named after productName).
const exe =
  process.platform === "win32"
    ? path.join(unpackedDir, "nyx.exe")
    : process.platform === "darwin"
      ? path.join(unpackedDir, "nyx.app", "Contents", "MacOS", "nyx")
      : path.join(unpackedDir, "nyx");
if (!fs.existsSync(exe)) fail(`packaged Electron binary not found at ${exe}`);
console.log(`[smoke-package] packaged binary: ${exe}`);

// The UNPACKED host entry + native addon (proof the .node is OUTSIDE the asar).
const resources = path.join(unpackedDir, "resources");
const unpackedApp = path.join(resources, "app.asar.unpacked");
// The unpacked RESOURCE ROOT the real main passes as `resourceDir`: electron-builder
// preserves the `dist/` prefix when unpacking, so the resource base is
// `app.asar.unpacked/dist` (see main/core-host.ts resolveResourceDir).
const unpackedResourceRoot = path.join(unpackedApp, "dist");
const hostEntry = path.join(unpackedApp, "dist", "core-host", "index.js");
const nativeDir = path.join(unpackedApp, "dist", "native");
if (!fs.existsSync(path.join(resources, "app.asar"))) fail("no app.asar in the package");
if (!fs.existsSync(hostEntry)) fail(`core-host entry NOT unpacked: ${hostEntry}`);
if (!fs.existsSync(nativeDir)) fail(`native dir NOT unpacked: ${nativeDir}`);
const nodeFiles = fs.readdirSync(nativeDir).filter((f) => f.endsWith(".node"));
if (nodeFiles.length === 0) fail(`no .node in the unpacked native dir ${nativeDir}`);
console.log(`[smoke-package] .node unpacked OUTSIDE asar: ${nodeFiles.join(", ")} ✓`);

// The bundled Claude plugin must be unpacked at the EXACT path nyx-core resolves:
// `<resource_dir>/resources/claude-plugin/.claude-plugin/marketplace.json`, where the
// host receives `resource_dir = <app.asar.unpacked>/dist` (the unpacked resource
// root). Proves the Claude resources work from the installation, outside the asar,
// with no source path (done-criterion #3).
const pluginManifest = path.join(
  unpackedResourceRoot,
  "resources",
  "claude-plugin",
  ".claude-plugin",
  "marketplace.json",
);
if (!fs.existsSync(pluginManifest)) {
  fail(`bundled Claude plugin NOT unpacked at the resolver path: ${pluginManifest}`);
}
console.log(`[smoke-package] Claude plugin unpacked OUTSIDE asar at the resolver path ✓`);

// --- 2. run the packaged host via the packaged Electron-as-Node ------------
const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-pkg-"));
// Mirror the real main: pass the unpacked resource ROOT (app.asar.unpacked/dist) so
// the host's resourceDir matches what production resolves.
const config = JSON.stringify({ dataDir, resourceDir: unpackedResourceRoot });
const child = spawn(exe, [hostEntry], {
  stdio: ["inherit", "inherit", "inherit", "ipc"],
  env: { ...process.env, ELECTRON_RUN_AS_NODE: "1", NYX_HOST_CONFIG: config },
});

let ready = null;
let ptyOut = false;

function cleanup(code) {
  try {
    if (child.exitCode === null) child.kill();
  } catch {}
  try {
    fs.rmSync(dataDir, { recursive: true, force: true });
  } catch {}
  process.exit(code);
}

// The live PTY id the host allocates in its `pty-spawn` REPLY; we then key the
// `pty-write` (and the expected `pty-output` events) by it. The current host
// protocol (phases 3-5) routes every PTY op by this id — `pty-spawn` → `{ptyId}`,
// `pty-write {ptyId,dataB64}`, `pty-output {ptyId,dataB64,bytes}`.
let ptyId = null;

child.on("error", (e) => fail(`packaged host spawn error: ${e.message}`));
child.on("message", (msg) => {
  if (!msg || typeof msg !== "object") return;

  // Correlated REPLY (e.g. the pty-spawn result carrying the live ptyId).
  if (msg.type === "res") {
    if (msg.id === 1) {
      if (!msg.ok) fail(`packaged host pty-spawn failed: ${msg.error}`);
      ptyId = msg.result && msg.result.ptyId;
      if (typeof ptyId !== "number") fail(`packaged host pty-spawn returned no ptyId`);
      console.log(`[smoke-package] packaged PTY spawned (ptyId=${ptyId}) ✓`);
      // Write a command and expect its echo/output back, keyed by the live id.
      child.send({
        type: "req",
        id: 2,
        payload: {
          kind: "pty-write",
          ptyId,
          dataB64: Buffer.from("echo NYX_OK\r\n").toString("base64"),
        },
      });
    }
    return;
  }

  if (msg.type !== "evt") return;
  const p = msg.payload;
  if (p.kind === "fatal") fail(`packaged host fatal: ${p.error}`);
  if (p.kind === "ready") {
    ready = p.info;
    if (!ready.nodePure) fail("packaged host is not Node-pure");
    console.log(
      `[smoke-package] packaged host ready — nyx-core ${ready.coreVersion}, electron ${ready.electron}, abi ${ready.abi}, nodePure=${ready.nodePure} ✓`,
    );
    // Spawn an interactive PTY (current protocol: `pty-spawn`, reply carries ptyId).
    child.send({ type: "req", id: 1, payload: { kind: "pty-spawn", cols: 80, rows: 24 } });
  }
  if (p.kind === "pty-output" && p.dataB64 && p.dataB64.length > 0) ptyOut = true;
});

setTimeout(() => {
  if (!ready) fail("packaged host never became ready (the .node likely failed to load from the package)");
  if (!ptyOut) fail("packaged host never streamed PTY output");
  console.log("[smoke-package] packaged minimal PTY streamed output (ConPTY on Windows / portable-pty on Linux) ✓");
  console.log("[smoke-package] OK — packaged .node + ELECTRON_RUN_AS_NODE host + PTY verified on " + process.platform);
  cleanup(0);
}, 6000);
