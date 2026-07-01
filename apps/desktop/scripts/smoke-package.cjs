#!/usr/bin/env node
/**
 * SMOKE-PACKAGING verification. Given the throwaway unpacked package built by
 * `scripts/package.cjs` (electron-builder `--dir`), this:
 *
 *   1. locates the PACKAGED Electron binary and the PACKAGED, UNPACKED nyx-sidecar
 *      BINARY (`resources/app.asar.unpacked/dist/sidecar/nyx-sidecar[.exe]`), proving
 *      the native executable was unpacked OUTSIDE the asar (it cannot be spawned from
 *      inside the archive), plus the bundled Claude plugin at the resolver path;
 *   2. spawns the packaged sidecar binary and drives it over the framed MessagePack
 *      pipe (the production transport): asserts the handshake succeeds and a minimal
 *      PTY streams output — i.e. the binary genuinely boots and runs from the packaged
 *      unpack layout on THIS OS.
 *
 * Any failure here is a GATE: it blocks the phase.
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

// Packaged Electron binary (named after productName `nyx-v2`).
const exe =
  process.platform === "win32"
    ? path.join(unpackedDir, "nyx-v2.exe")
    : process.platform === "darwin"
      ? path.join(unpackedDir, "nyx-v2.app", "Contents", "MacOS", "nyx-v2")
      : path.join(unpackedDir, "nyx-v2");
if (!fs.existsSync(exe)) fail(`packaged Electron binary not found at ${exe}`);
console.log(`[smoke-package] packaged binary: ${exe}`);

// The UNPACKED sidecar BINARY (proof the native executable is OUTSIDE the asar — it
// cannot be spawned from inside the archive).
const resources = path.join(unpackedDir, "resources");
const unpackedApp = path.join(resources, "app.asar.unpacked");
// The unpacked RESOURCE ROOT the real main passes as `resourceDir`: electron-builder
// preserves the `dist/` prefix when unpacking, so the resource base is
// `app.asar.unpacked/dist` (see main/sidecar.ts resolveResourceDir).
const unpackedResourceRoot = path.join(unpackedApp, "dist");
const sidecarExe = process.platform === "win32" ? "nyx-sidecar.exe" : "nyx-sidecar";
const sidecarBin = path.join(unpackedApp, "dist", "sidecar", sidecarExe);
if (!fs.existsSync(path.join(resources, "app.asar"))) fail("no app.asar in the package");
if (!fs.existsSync(sidecarBin)) fail(`nyx-sidecar binary NOT unpacked: ${sidecarBin}`);
console.log(`[smoke-package] nyx-sidecar binary unpacked OUTSIDE asar: ${sidecarBin} ✓`);

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

// --- 1b. verify the REAL Electron-main RESOURCE-PATH RESOLUTION --------------
// The above proves the binary EXISTS at the unpacked path; this proves the main-process
// RESOLVER lands on it. We exercise the EXACT production resolver code (the bundled
// `dist/main/sidecar.js`'s pure `deriveSidecarBin` / `deriveResourceDir`) with the EXACT
// packaged inputs:
//   * `__dirname` = the bundled main dir INSIDE the asar (`…/resources/app.asar/dist/main`) —
//     where `sidecar.js` actually runs in the installed app;
//   * `isPackaged = true`;
//   * `process.resourcesPath` = the packaged `…/resources` dir.
// and assert the resolver redirects `app.asar/dist/sidecar/…` to the `app.asar.unpacked`
// sibling (the production "spawn from resources" path, NOT a dev/source path). This is the
// "Electron résout le chemin du sidecar depuis ses resources" done-criterion, verified on the
// host OS WITHOUT needing a real packaged Electron process (which won't run an injected main).
// The sidecar path RESOLVED THROUGH the production derivation — section 2 below SPAWNS this
// (not the direct `sidecarBin`), so the smoke exercises the real runtime resolution end-to-end.
let resolvedBin;
{
  const { deriveSidecarBin, deriveResourceDir, sidecarExeName } = require("../dist/main/sidecar.js");
  // The dir the bundled main runs from in the INSTALLED app (inside the asar).
  const packagedMainDir = path.join(resources, "app.asar", "dist", "main");
  // CRITICAL (review `01KWD96WAD7MAPWM9Y5S56G138`): resolve THROUGH the real production resolver
  // and use its RETURN VERBATIM — NO `path.normalize` wrapper. The earlier smoke wrapped the
  // result in `path.normalize`, which collapsed the `dist/main/../sidecar` itself and so MASKED
  // the bug: `deriveSidecarBin` returned an un-normalized `…/dist/main/../sidecar/…` that the OS
  // could NOT traverse under the unpacked layout (only `dist/sidecar`/`dist/resources` are
  // unpacked, `dist/main` is not → ENOENT). The assertion that WOULD have caught it — and now
  // does — is `fs.existsSync(resolvedBin)` on the resolver's OWN output, with `resolvedBin`
  // unmodified, since at runtime main passes exactly this string to `spawn`.
  resolvedBin = deriveSidecarBin(packagedMainDir, true, sidecarExeName(), undefined, path.sep);
  const resolvedResourceDir = deriveResourceDir(true, resources, path.sep);

  // The resolver must already return the NORMALIZED, real on-disk path (no `..`, no `dist/main`):
  // this is the runtime spawn path, so it must exist verbatim — without any normalization here.
  if (resolvedBin.includes("..")) {
    fail(`deriveSidecarBin() returned an UN-NORMALIZED path (contains '..'): ${resolvedBin} — the packaged sidecar would ENOENT`);
  }
  if (resolvedBin.includes(`dist${path.sep}main`)) {
    fail(`deriveSidecarBin() still points through the un-unpacked dist/main: ${resolvedBin}`);
  }
  if (resolvedBin !== sidecarBin) {
    fail(`deriveSidecarBin() returned ${resolvedBin}, expected the unpacked ${sidecarBin}`);
  }
  if (!resolvedBin.includes("app.asar.unpacked")) {
    fail(`deriveSidecarBin() did not redirect into app.asar.unpacked: ${resolvedBin}`);
  }
  if (resolvedBin.includes(`app.asar${path.sep}dist`)) {
    fail(`deriveSidecarBin() still points INSIDE the asar (a native binary can't spawn there): ${resolvedBin}`);
  }
  if (resolvedResourceDir !== unpackedResourceRoot) {
    fail(`deriveResourceDir() returned ${resolvedResourceDir}, expected ${unpackedResourceRoot}`);
  }
  // THE assertion that catches this class of bug: the resolver's OWN output (unmodified, the
  // exact string main hands to `spawn`) really exists on disk.
  if (!fs.existsSync(resolvedBin)) {
    fail(`the resolved sidecar path does not exist on disk: ${resolvedBin} (deriveSidecarBin must return a normalized, real path — this is the runtime spawn path)`);
  }

  // Also prove the per-OS Windows resolution (the `.exe` + `\\` separator rewrite) lands in
  // app.asar.unpacked too AND is normalized — the cross-OS resolution branch, deterministically,
  // on this host. The buggy code left a `\\main\\..\\sidecar` here too.
  const winBin = deriveSidecarBin("C:\\app\\resources\\app.asar\\dist\\main", true, "nyx-sidecar.exe", undefined, "\\");
  if (winBin.includes("..")) {
    fail(`the Windows resolution branch returned an UN-NORMALIZED path (contains '..'): ${winBin}`);
  }
  if (winBin.includes("dist\\main")) {
    fail(`the Windows resolution branch still points through dist\\main: ${winBin}`);
  }
  if (!winBin.includes("app.asar.unpacked\\") || !winBin.endsWith("sidecar\\nyx-sidecar.exe")) {
    fail(`the Windows resolution branch did not redirect into app.asar.unpacked or name the .exe: ${winBin}`);
  }
  if (winBin.includes("app.asar\\dist") && !winBin.includes("app.asar.unpacked\\dist")) {
    fail(`the Windows resolution branch still points INSIDE the asar: ${winBin}`);
  }
  console.log(
    `[smoke-package] main-process RESOLVES the sidecar from resources (through deriveSidecarBin, NO normalize wrapper) ✓ — ` +
      `deriveSidecarBin(packaged)=${resolvedBin} EXISTS on disk (app.asar.unpacked, no '..'/dist/main; Windows branch -> ${winBin})`,
  );
}

// --- 2. drive the packaged SIDECAR BINARY over its stdin/stdout pipe --------
// Spawn the unpacked nyx-sidecar (the runtime authority, outside the asar) and drive
// it over the framed MessagePack pipe with the SAME codec the main process uses, so
// this proves the packaged binary boots, handshakes, and streams a live PTY — the
// production runtime path, with no Electron needed.
const {
  encodeMessageFrame,
  decodeMessage,
  FrameDecoder,
  PROTOCOL_VERSION,
} = require("../dist/shared/pipe-protocol.js");

const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-pkg-"));
// Spawn the binary as RESOLVED BY THE PRODUCTION DERIVATION (resolvedBin), not the direct
// `sidecarBin` — so a regression in deriveSidecarBin (e.g. an un-normalized `..`) is caught here
// by the spawn failing, exactly as the real packaged main would fail.
const child = spawn(resolvedBin, [], {
  stdio: ["pipe", "pipe", "inherit"],
  env: {
    ...process.env,
    NYX_DATA_DIR: dataDir,
    NYX_RESOURCE_DIR: unpackedResourceRoot,
    SHELL: process.env.SHELL || "/bin/sh",
  },
});

let handshaked = false;
let ptyId = null;
let ptyOut = false;
let nextId = 1;
const decoder = new FrameDecoder();

function send(msg) {
  child.stdin.write(encodeMessageFrame(msg));
}

function cleanup(code) {
  try {
    if (child.exitCode === null) child.kill();
  } catch {}
  try {
    fs.rmSync(dataDir, { recursive: true, force: true });
  } catch {}
  process.exit(code);
}

child.on("error", (e) => fail(`packaged sidecar spawn error: ${e.message}`));
child.stdout.on("data", (chunk) => {
  decoder.push(new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength));
  for (;;) {
    const payload = decoder.nextFrame();
    if (!payload) break;
    const m = decodeMessage(payload);
    if (m.category === "response" && m.kind === "handshake_ok" && !handshaked) {
      handshaked = true;
      console.log(`[smoke-package] packaged sidecar handshake_ok (protocol v${m.protocolVersion}) ✓`);
      // Spawn an interactive PTY over the pipe.
      const id = nextId++;
      send({ category: "request", kind: "pty_spawn", id, cols: 80, rows: 24, cwd: "", recordId: "" });
    } else if (m.category === "response" && m.kind === "pty_spawned") {
      ptyId = m.terminalId;
      console.log(`[smoke-package] packaged PTY spawned (terminalId=${ptyId}) ✓`);
      send({
        category: "request",
        kind: "pty_write",
        id: nextId++,
        terminalId: ptyId,
        data: new TextEncoder().encode("echo NYX_OK\n"),
      });
    } else if (m.category === "event" && m.kind === "terminal_output" && m.data && m.data.length > 0) {
      ptyOut = true;
    } else if (m.category === "response" && m.kind === "error") {
      fail(`packaged sidecar error: ${m.message}`);
    }
  }
});

// Open the pipe with the versioned handshake.
send({ category: "request", kind: "handshake", id: nextId++, protocolVersion: PROTOCOL_VERSION });

setTimeout(() => {
  if (!handshaked) fail("packaged sidecar never handshaked (the binary likely failed to boot from the package)");
  if (!ptyOut) fail("packaged sidecar never streamed PTY output");
  console.log("[smoke-package] packaged minimal PTY streamed output (portable-pty / ConPTY) ✓");
  console.log("[smoke-package] OK — packaged nyx-sidecar binary + pipe + PTY verified on " + process.platform);
  cleanup(0);
}, 6000);
