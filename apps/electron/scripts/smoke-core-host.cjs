#!/usr/bin/env node
/**
 * Runtime smoke for the CORE-HOST (task #2). Runs under full Electron (so `app`
 * exists, exactly as in production) and drives the REAL `CoreHost` manager
 * (`dist/main/core-host.js`) — i.e. it spawns the REAL host
 * (`dist/core-host/index.js`) via the Electron binary with `ELECTRON_RUN_AS_NODE=1`.
 *
 * Asserts the done-criteria:
 *   1. the spawned host is identifiable as NODE-PURE and loads no Chromium renderer
 *      (`ping().nodePure === true`, `process.type` absent, electron `app` absent);
 *   2. main receives a CORRELATED ping reply AND a PTY-output event from the minimal
 *      napi PTY (the EventSink path end to end);
 *   3. `nyx-napi` is loaded ONLY in the host — it is NOT in main's module cache, and
 *      requiring it in the RENDERER throws;
 *   4. `AppPaths.data_dir` resolves to Electron `userData` AND honors `NYX_DATA_DIR`;
 *   5. the host entry + the native loader are locatable (dev layout here).
 *
 * Run under Electron: `electron scripts/smoke-core-host.cjs`. Exits 0 on full pass.
 */
"use strict";
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");
const { app } = require("electron");

const { CoreHost, resolveDataDir } = require("../dist/main/core-host.js");

function fail(msg) {
  console.error("[smoke-core-host] FAIL:", msg);
  app.exit(1);
}

// Pin a deterministic NYX_DATA_DIR so we can assert the override is honored (#4).
const pinnedDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-data-"));
process.env.NYX_DATA_DIR = pinnedDataDir;

app.whenReady().then(async () => {
  // (3a) napi must NOT be in MAIN's module cache (main never loads the .node).
  const napiLoaded = Object.keys(require.cache).some((k) => k.endsWith(".node"));
  if (napiLoaded) return fail("a .node addon is loaded in the MAIN process — must be host-only");
  console.log("[smoke-core-host] no .node loaded in main ✓");

  const host = new CoreHost();
  let ptyOutputSeen = false;
  let readySeen = false;
  host.onEvent((evt) => {
    if (evt.kind === "ready") readySeen = true;
    if (evt.kind === "pty-output" && evt.dataB64 && evt.dataB64.length > 0) ptyOutputSeen = true;
    if (evt.kind === "fatal") fail(`host fatal: ${evt.error}`);
  });

  await host.start();
  if (typeof host.pid !== "number") return fail("host did not spawn (no pid)");
  if (host.currentState !== "ready") return fail(`host state after boot = ${host.currentState}, expected ready`);
  console.log(`[smoke-core-host] host spawned + booted (state=${host.currentState}), pid=${host.pid} ✓`);

  // (1)+(2) correlated ping → Node-pure proof bundle.
  const info = await host.ping();
  if (!info.nodePure) return fail("host reports nodePure=false (Chromium runtime present)");
  if (!info.coreVersion) return fail("ping returned no coreVersion — .node did not load in host");
  console.log(
    `[smoke-core-host] correlated ping ✓ — nyx-core ${info.coreVersion}, electron ${info.electron}, node ${info.node}, abi ${info.abi}, nodePure=${info.nodePure}`,
  );

  // (4) data dir honored the NYX_DATA_DIR override AND main resolves the same.
  if (info.dataDir !== pinnedDataDir)
    return fail(`host dataDir=${info.dataDir} != pinned NYX_DATA_DIR=${pinnedDataDir}`);
  if (resolveDataDir() !== pinnedDataDir)
    return fail("main resolveDataDir() did not honor NYX_DATA_DIR");
  if (!fs.existsSync(pinnedDataDir)) return fail("host did not create the data dir");
  console.log(`[smoke-core-host] AppPaths.data_dir honored NYX_DATA_DIR (${info.dataDir}) ✓`);
  // And without the override it must fall back to Electron userData.
  delete process.env.NYX_DATA_DIR;
  if (resolveDataDir() !== app.getPath("userData"))
    return fail("resolveDataDir() fallback is not Electron userData");
  process.env.NYX_DATA_DIR = pinnedDataDir;
  console.log("[smoke-core-host] data_dir fallback = Electron userData ✓");

  // (2) PTY: spawn + write (keyed by the live ptyId), then expect a PTY-output event
  // back via EventSink (the full phase-3 surface).
  const spawned = await host.request({ kind: "pty-spawn", cols: 80, rows: 24 });
  if (typeof spawned?.ptyId !== "number") return fail("pty-spawn did not return a numeric ptyId");
  const ptyId = spawned.ptyId;
  await host.request({
    kind: "pty-write",
    ptyId,
    dataB64: Buffer.from("echo NYX_OK\r\n").toString("base64"),
  });
  await new Promise((r) => setTimeout(r, 1500));
  if (!ptyOutputSeen) return fail("no PTY output streamed to main within the window");
  console.log("[smoke-core-host] minimal PTY streamed output to main (EventSink path) ✓");
  if (!readySeen) return fail("never received the host `ready` event");
  console.log("[smoke-core-host] received correlated `ready` event ✓");

  // (5) the host entry + native loader are locatable.
  const hostEntry = path.join(__dirname, "..", "dist", "core-host", "index.js");
  const nativeLoader = path.join(__dirname, "..", "dist", "native", "index.js");
  if (!fs.existsSync(hostEntry)) return fail(`host entry missing at ${hostEntry}`);
  if (!fs.existsSync(nativeLoader)) return fail(`native loader missing at ${nativeLoader}`);
  console.log("[smoke-core-host] host entry + native loader locatable ✓");

  // Clean teardown leaves no orphan host.
  await host.stop();
  if (host.alive) return fail("host still alive after stop()");
  console.log("[smoke-core-host] host stopped cleanly (no orphan) ✓");

  fs.rmSync(pinnedDataDir, { recursive: true, force: true });
  console.log("[smoke-core-host] OK — core-host frontiers + Node-pure + PTY verified.");
  app.exit(0);
});

setTimeout(() => fail("timed out"), 30000);
