/* eslint-disable */
// Smoke e2e against the REAL nyx app (release build) through tauri-driver +
// WebKitWebDriver. xterm renders to a WebGL canvas, so terminal text is not in
// the DOM; we drive + read the ACTIVE terminal through the inert `window.__nyx`
// control seam exposed by src/components/sidebar/terminal-manager.tsx (it mirrors
// the per-terminal deck read/input seams, keyed by record id).
//
// This is the basic-behaviour smoke (env survives, program output, resize, exit);
// the big multi-terminal RESTORE scenario lives in restore-01/02-*.e2e.cjs.

const assert = require("assert");

// The active terminal's record id (the visible pane). All seam calls target it.
async function activeId() {
  return browser.execute(function () {
    return window.__nyx ? window.__nyx.activeId() : null;
  });
}

// Type bytes into the active terminal via the inert input seam (goes through
// xterm `input`, so the PTY echoes + executes them — exactly like keystrokes).
async function typeLine(text) {
  const id = await activeId();
  await browser.execute(
    function (id, t) {
      if (window.__nyx && id != null) window.__nyx.typeInto(id, t);
    },
    id,
    text,
  );
}

async function readBuffer() {
  const id = await activeId();
  return browser.execute(function (id) {
    return window.__nyx && id != null ? window.__nyx.readBuffer(id) : "";
  }, id);
}

async function waitForOutput(needle, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 15000);
  let last = "";
  while (Date.now() < deadline) {
    last = await readBuffer();
    if (last.includes(needle)) return last;
    await browser.pause(200);
  }
  throw new Error(
    "timed out waiting for '" + needle + "'. Buffer was:\n" + last,
  );
}

describe("nyx terminal e2e (smoke)", function () {
  before(async function () {
    // Wait for the WebView to mount React + the control seam + an active term.
    await browser.waitUntil(
      async function () {
        return browser.execute(function () {
          return !!(
            window.__nyx && window.__nyx.activeId() != null
          );
        });
      },
      { timeout: 30000, timeoutMsg: "window.__nyx active terminal never appeared" },
    );
    // Let the shell print its first prompt so subsequent input is at a prompt.
    await browser.pause(1000);
  });

  it("preserves the shell environment across commands (export FOO=bar; echo)", async function () {
    await typeLine("export FOO=bar_e2e_9k1\n");
    await browser.pause(300);
    await typeLine('echo "FOOIS:$FOO"\n');
    const buf = await waitForOutput("FOOIS:bar_e2e_9k1", 15000);
    assert(
      buf.includes("FOOIS:bar_e2e_9k1"),
      "env var FOO must survive between commands",
    );
  });

  it("runs a program and shows its known output", async function () {
    const marker = "MARKER_" + "abc123";
    await typeLine('printf "%s\\n" ' + marker + "\n");
    const buf = await waitForOutput(marker, 15000);
    assert(buf.includes(marker), "program output marker must appear");
  });

  it("survives a window resize without crashing", async function () {
    await browser.setWindowSize(700, 500);
    await browser.pause(300);
    await browser.setWindowSize(1100, 800);
    await browser.pause(300);
    const stillAlive = await browser.execute(function () {
      return !!(window.__nyx && window.__nyx.activeId() != null);
    });
    assert(stillAlive, "app must remain alive after resize");
    await typeLine('echo "AFTER_RESIZE_OK"\n');
    const buf = await waitForOutput("AFTER_RESIZE_OK", 15000);
    assert(buf.includes("AFTER_RESIZE_OK"), "terminal usable after resize");
  });

  it("closes the shell cleanly on `exit`", async function () {
    await typeLine("exit\n");
    // The backend emits a "[process exited...]" notice into xterm when the PTY
    // closes; the front writes it to the buffer (see usePty.ts).
    const buf = await waitForOutput("process exited", 15000);
    assert(
      buf.includes("process exited"),
      "exit must close the shell and surface the exit notice",
    );
  });
});
