#!/usr/bin/env node
/**
 * Runtime smoke for the PROD LOAD PATH (task #1 done-criterion: "the shell opens the
 * front from its production build"). Reproduces the production main's loader exactly
 * (`loadFile(dist/renderer/index.html)` through the REAL preload + webPreferences),
 * and asserts:
 *   - the renderer document FINISHES loading (`did-finish-load`) from the on-disk
 *     production build, at a `file://` origin;
 *   - the main frame does NOT `did-fail-load`;
 *   - the allowlisted `window.nyxWindow` bridge is present in the real front.
 *
 * Requires the renderer to be built + copied first (`bun run build:renderer &&
 * bun run copy:renderer`). Run under Electron: `electron scripts/smoke-prod-load.cjs`.
 */
"use strict";
const path = require("node:path");
const fs = require("node:fs");
const { app, BrowserWindow } = require("electron");

const indexHtml = path.join(__dirname, "..", "dist", "renderer", "index.html");
const preload = path.join(__dirname, "..", "dist", "preload", "index.js");

function fail(msg) {
  console.error("[smoke-prod-load] FAIL:", msg);
  app.exit(1);
}

if (!fs.existsSync(indexHtml)) {
  console.error(`[smoke-prod-load] ${indexHtml} not found — build + copy the renderer first`);
  process.exit(1);
}

app.whenReady().then(() => {
  const win = new BrowserWindow({
    show: false,
    frame: false,
    webPreferences: { preload, contextIsolation: true, nodeIntegration: false, sandbox: true },
  });

  let mainFrameFailed = null;
  win.webContents.on("did-fail-load", (_e, errorCode, errorDesc, _url, isMainFrame) => {
    if (isMainFrame) mainFrameFailed = `${errorCode} ${errorDesc}`;
  });

  win.webContents.on("did-finish-load", async () => {
    if (mainFrameFailed) return fail(`main frame failed to load: ${mainFrameFailed}`);
    const url = win.webContents.getURL();
    if (!url.startsWith("file://")) return fail(`renderer loaded at non-file origin: ${url}`);
    console.log(`[smoke-prod-load] production build loaded from ${url} ✓`);

    const hasBridge = await win.webContents.executeJavaScript(
      `typeof window.nyxWindow === 'object' && typeof window.nyxWindow.close === 'function'`,
    );
    if (!hasBridge) return fail("window.nyxWindow not exposed in the production renderer");
    console.log("[smoke-prod-load] allowlisted window.nyxWindow present in the real front ✓");

    console.log("[smoke-prod-load] OK — prod load path verified.");
    app.exit(0);
  });

  win.loadFile(indexHtml);
});

setTimeout(() => fail("timed out waiting for did-finish-load"), 25000);
