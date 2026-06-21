#!/usr/bin/env node
/**
 * NyxCore smoke (PRD-5 tasks #2 + #3): proves, under Electron-as-Node, that
 *
 *   1. `NyxCore.open(dataDir)` builds the r2d2 pool + migrates (the DB opens);
 *   2. a DELIBERATELY SLOW DB query (`dbSlowQuery`) runs on a libuv worker and does
 *      NOT block the Node event loop — a `setInterval` keeps firing while it is in
 *      flight, and OTHER pooled reads progress concurrently (the done-criterion
 *      "une requete Diesel lente ne bloque ni les messages du core-host");
 *   3. the DB AsyncTask round-trips (`listTerminals`, `setExecState`) off the loop;
 *   4. the MCP server starts on the SHARED pool and answers the HTTP handshake +
 *      `tools/list` + a pool-backed `tools/call` (`list_projects`) — "démarre et
 *      répond comme sous Tauri", sharing the SAME pool the DB tasks use.
 *
 * Re-execs itself under the Electron binary with ELECTRON_RUN_AS_NODE=1 (same shape
 * as verify-abi.cjs), so the checks run against the host's embedded Node ABI.
 */
"use strict";
const path = require("node:path");
const os = require("node:os");
const fs = require("node:fs");
const http = require("node:http");
const { spawnSync } = require("node:child_process");

const ADDON = path.join(__dirname, "..", "index.js");

// --- Phase A: re-exec under the Electron binary as Node ---------------------
if (!process.versions.electron) {
  let electronBin;
  try {
    electronBin = require("electron");
  } catch {
    console.error("[verify-core] the `electron` devDependency is not installed.");
    process.exit(2);
  }
  // A fixed, non-default MCP port so we never collide with a running nyx instance.
  const res = spawnSync(electronBin, [__filename], {
    stdio: "inherit",
    env: { ...process.env, ELECTRON_RUN_AS_NODE: "1", NYX_MCP_PORT: "8791" },
  });
  process.exit(res.status === null ? 1 : res.status);
}

// --- Phase B: under Electron-as-Node ----------------------------------------
console.log(
  `[verify-core] under Electron ${process.versions.electron} · Node ${process.versions.node} · ABI ${process.versions.modules}`,
);

function fail(msg) {
  console.error(`[verify-core] FAILED — ${msg}`);
  process.exit(1);
}

const addon = require(ADDON);
if (typeof addon.NyxCore !== "function") fail("addon has no NyxCore class");

// A fresh, isolated data dir (no Tauri data migration; userData-neuf).
const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-core-smoke-"));

async function main() {
  // 1. open the pool + migrate.
  const core = new addon.NyxCore(dataDir);
  console.log("[verify-core] NyxCore.open OK — pool built + migrated");

  // 2. NON-BLOCKING proof: start a slow query, and assert the Node loop keeps
  //    ticking AND a concurrent read finishes WHILE the slow query is still running.
  let ticks = 0;
  const ticker = setInterval(() => { ticks += 1; }, 10);
  const slowMs = 600;
  const t0 = Date.now();
  const slow = core.dbSlowQuery(slowMs); // runs on a libuv worker, holds a pooled conn
  // A concurrent read must resolve well BEFORE the slow query (different pooled conn).
  const concurrentRead = core.listTerminals();
  const concurrentDoneAt = await concurrentRead.then(() => Date.now() - t0);
  if (concurrentDoneAt > slowMs - 100) {
    fail(`concurrent read waited ${concurrentDoneAt}ms (>= slow ${slowMs}ms) — pool serialized reads`);
  }
  await slow;
  clearInterval(ticker);
  const elapsed = Date.now() - t0;
  // The event loop must have ticked many times during the ~600ms stall (it would tick
  // ~0 times if the slow query blocked the loop).
  if (ticks < 20) fail(`event loop ticked only ${ticks}x during a ${slowMs}ms DB stall — loop was blocked`);
  console.log(`[verify-core] non-blocking OK — loop ticked ${ticks}x, concurrent read done @${concurrentDoneAt}ms, slow done @${elapsed}ms`);

  // 3. DB AsyncTask round-trip: setExecState on a non-existent id returns updated=false
  //    (proves the write path + read-back run off the loop without throwing).
  const persist = await core.setExecState("does-not-exist", "success", 0, true);
  if (persist.updated !== false) fail("setExecState on unknown id should report updated=false");
  console.log("[verify-core] setExecState AsyncTask OK (unknown id → updated=false)");

  // 3b. Boot resume scan (PRD-5 #5): on a fresh DB there are no sessions, so it returns
  //     no parks — proves the sweep → candidates → decide chain runs off the loop
  //     without throwing (the decision logic itself is covered by nyx-core tests).
  const parks = await core.resumeScanOnBoot();
  if (!Array.isArray(parks) || parks.length !== 0) fail(`resumeScanOnBoot should be empty on a fresh DB, got ${JSON.stringify(parks)}`);
  console.log("[verify-core] resumeScanOnBoot AsyncTask OK (fresh DB → no parks)");

  // 4. MCP shares the pool: start it, hit the HTTP handshake + a pool-backed tool.
  const port = core.mcpStart();
  if (!core.mcpIsStarted() || core.mcpPort() !== port) fail("mcpStart/port/isStarted inconsistent");
  console.log(`[verify-core] MCP started on 127.0.0.1:${port}`);

  await mcpCall(port, "initialize", {});
  const tools = await mcpCall(port, "tools/list", {});
  if (!tools.result || !Array.isArray(tools.result.tools)) fail("tools/list did not return a tools array");
  const listProjects = await mcpCall(port, "tools/call", { name: "list_projects", arguments: {} });
  // A pool-backed read resolves (empty projects on a fresh DB) — proves MCP reads the
  // SAME shared pool the DB tasks use.
  if (listProjects.error) fail(`list_projects (pool-backed) errored: ${JSON.stringify(listProjects.error)}`);
  console.log("[verify-core] MCP handshake + pool-backed tools/call OK — server responds, shares the pool");

  console.log("[verify-core] OK — pool + non-blocking AsyncTask + MCP-shares-pool verified.");
  process.exit(0);
}

/** Minimal JSON-RPC over the loopback MCP HTTP endpoint. */
function mcpCall(port, method, params) {
  return new Promise((resolve, reject) => {
    const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
    const req = http.request(
      { host: "127.0.0.1", port, path: "/mcp", method: "POST", headers: { "content-type": "application/json", "content-length": Buffer.byteLength(body) } },
      (res) => {
        let data = "";
        res.on("data", (c) => (data += c));
        res.on("end", () => {
          try { resolve(JSON.parse(data)); } catch (e) { reject(e); }
        });
      },
    );
    req.on("error", reject);
    req.write(body);
    req.end();
  });
}

main().catch((e) => fail(e.stack || String(e)));
