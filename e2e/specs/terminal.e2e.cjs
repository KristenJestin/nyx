/* eslint-disable */
// End-to-end scenarios driving the REAL nyx app (release build) through
// tauri-driver + WebKitWebDriver. xterm renders to a WebGL canvas, so terminal
// text is not in the DOM; we read it through the `window.__nyx.readBuffer()`
// seam exposed by src/App.tsx (see that file for the rationale).

const assert = require("assert");

// Type bytes into the live xterm via its public `input()` API and let the PTY
// echo + execute them. We go through xterm (not raw key events) so the bytes
// reach the backend exactly as keystrokes would.
async function typeLine(text) {
  await browser.execute(function (t) {
    var term = window.__nyx && window.__nyx.term;
    if (term) term.input(t, true);
  }, text);
}

async function readBuffer() {
  return browser.execute(function () {
    return window.__nyx ? window.__nyx.readBuffer() : "";
  });
}

// Poll readBuffer() until it contains `needle` or we time out.
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

describe("nyx terminal e2e", function () {
  before(async function () {
    // Wait for the WebView to mount React + the xterm seam.
    await browser.waitUntil(
      async function () {
        return browser.execute(function () {
          return !!(window.__nyx && window.__nyx.term);
        });
      },
      { timeout: 30000, timeoutMsg: "window.__nyx.term never appeared" },
    );
    // Let the shell print its first prompt so subsequent input is at a prompt.
    await browser.pause(1000);
  });

  it("preserves the shell environment across commands (export FOO=bar; echo)", async function () {
    // A unique marker so we don't match the echoed command itself.
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
    // Resize a few times; the app/WebView must stay alive and the seam must
    // still respond afterward (reflow is best-effort, no-crash is the contract).
    await browser.setWindowSize(700, 500);
    await browser.pause(300);
    await browser.setWindowSize(1100, 800);
    await browser.pause(300);
    const stillAlive = await browser.execute(function () {
      return !!(window.__nyx && window.__nyx.term);
    });
    assert(stillAlive, "app must remain alive after resize");
    // And the terminal still accepts input after the resize.
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
