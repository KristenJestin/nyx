/* eslint-disable */
// Shared helpers for the multi-terminal RESTORE e2e (restore-01 seed →
// restore-02 verify). The two specs run as separate Electron app sessions sharing one
// NYX_DATA_DIR (see wdio.conf.cjs specDataDir), so they communicate the
// expected ids/markers/order through a small JSON file in that dir.

const fs = require("fs");
const path = require("path");

// Where the seed spec drops the expectations for the verify spec. Lives in the
// shared data dir (NYX_E2E_DATA_DIR is injected per-session by wdio.conf.cjs).
function handoffPath() {
  const dir = process.env.NYX_E2E_DATA_DIR || require("os").tmpdir();
  return path.join(dir, "restore-handoff.json");
}

function writeHandoff(data) {
  fs.writeFileSync(handoffPath(), JSON.stringify(data, null, 2));
}

function readHandoff() {
  return JSON.parse(fs.readFileSync(handoffPath(), "utf8"));
}

// Wait for the WebView to mount React + the control seam with an active term.
async function waitForApp() {
  await browser.waitUntil(
    async function () {
      return browser.execute(function () {
        return !!(window.__nyx && window.__nyx.activeId() != null);
      });
    },
    { timeout: 30000, timeoutMsg: "window.__nyx active terminal never appeared" },
  );
  // Let the first shell print its prompt.
  await browser.pause(1200);
}

// Snapshot the current records (id, cwd, label, status, order_index).
async function listRecords() {
  return browser.execute(function () {
    return window.__nyx ? window.__nyx.list() : [];
  });
}

async function activeId() {
  return browser.execute(function () {
    return window.__nyx ? window.__nyx.activeId() : null;
  });
}

// Create a terminal at `cwd`, returning the id of the new (now-active) record.
async function createAt(cwd) {
  const before = await listRecords();
  const beforeIds = before.map(function (r) {
    return r.id;
  });
  await browser.execute(function (c) {
    return window.__nyx.create(c);
  }, cwd);
  // Wait for the new record to appear + its shell to spawn.
  await browser.waitUntil(
    async function () {
      const now = await listRecords();
      return now.length > before.length;
    },
    { timeout: 15000, timeoutMsg: "new terminal record did not appear" },
  );
  await browser.pause(800); // let the shell spawn + print a prompt
  const after = await listRecords();
  const fresh = after.filter(function (r) {
    return beforeIds.indexOf(r.id) === -1;
  });
  return fresh.length ? fresh[fresh.length - 1].id : null;
}

// Type a line into the terminal `id` (keystrokes → PTY runs it).
async function typeInto(id, text) {
  await browser.execute(
    function (id, t) {
      if (window.__nyx) window.__nyx.typeInto(id, t);
    },
    id,
    text,
  );
}

// Poll terminal `id`'s buffer until it contains `needle`, returning the buffer.
async function waitForBuffer(id, needle, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 15000);
  let last = "";
  while (Date.now() < deadline) {
    last = await browser.execute(function (id) {
      return window.__nyx ? window.__nyx.readBuffer(id) : "";
    }, id);
    if (last.indexOf(needle) !== -1) return last;
    await browser.pause(200);
  }
  throw new Error(
    "timed out waiting for '" + needle + "' in terminal " + id + ". Buffer:\n" + last,
  );
}

async function readBuffer(id) {
  return browser.execute(function (id) {
    return window.__nyx ? window.__nyx.readBuffer(id) : "";
  }, id);
}

async function reorder(ids) {
  await browser.execute(function (ids) {
    return window.__nyx.reorder(ids);
  }, ids);
  await browser.pause(400);
}

async function closeTerminal(id) {
  await browser.execute(function (id) {
    return window.__nyx.close(id);
  }, id);
  await browser.pause(600);
}

module.exports = {
  handoffPath,
  writeHandoff,
  readHandoff,
  waitForApp,
  listRecords,
  activeId,
  createAt,
  typeInto,
  waitForBuffer,
  readBuffer,
  reorder,
  closeTerminal,
};
