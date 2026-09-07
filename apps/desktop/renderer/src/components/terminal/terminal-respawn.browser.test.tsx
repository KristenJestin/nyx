// xterm's own stylesheet, exactly as the app entry loads it: the render layers fall out of flow
// without it, and this suite asserts against a really-laid-out pane.
import "@xterm/xterm/css/xterm.css";

import { render } from "@testing-library/react";
import { page, userEvent } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";
import { Terminal } from "./terminal";
import { __resetTerminalStore, getTerminal } from "./terminal-store";

/**
 * THE RELAUNCH, IN A REAL BROWSER (Chromium via Playwright).
 *
 * The jsdom suite (`terminal-respawn.test.ts`) holds the STATE machine: the exit is published, the
 * relaunch spawns against the same record, a failure puts the bar back. What it cannot hold is the
 * part the user actually meets — that a pane whose process died SHOWS something, that the thing it
 * shows is clickable, and that a bare Enter inside the emulator does the same. Those need a real
 * layout, a real hit test and a real keyboard, so they live here.
 *
 * WHY THE BRIDGE IS DRIVEN DIRECTLY and not through `@/bridge/test-harness`. That harness works by
 * `vi.mock("@/bridge", …)` INSIDE ITS OWN MODULE, and a `vi.mock` written there is hoisted within
 * that module — not into the spec that imports it — so in BROWSER mode the swap never happens and
 * `nyxBridge` stays whatever `@/bridge` chose at load. That is not a problem here: with no Electron
 * preload in the page there is no `window.nyxCore`, so what `@/bridge` chose IS a `FakeNyxBridge`
 * (its documented test/SSR safety net). We drive that instance through the same two verbs the
 * harness exposes — `onAnyCommand` (its `mockIPC`) and `emit` — so the spec reads the same and no
 * mocking machinery is involved. `reset()` between tests replaces the harness's auto-clean.
 */

const FIRST_PTY_ID = 500;

/** The ambient bridge, which is a fake in this page — see the note above. */
const bridge = nyxBridge as unknown as FakeNyxBridge;

let spawns: { terminalId: unknown }[] = [];
let host: HTMLDivElement;

/**
 * Poll until `pred()` holds — the spawn, xterm's writes and React's commit are all async.
 *
 * The budget is generous ON PURPOSE. This spec waits on a real Chromium's layout and paint while
 * the rest of the suite runs beside it, and a starved worker spends most of a wait not scheduled at
 * all — the same reason `vitest.setup.ts` raises the unit `testTimeout`. MEASURED: green three
 * times out of three in isolation, one timeout under the full suite at 5 s.
 */
async function waitFor(pred: () => boolean, timeoutMs = 20000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return;
    await new Promise((r) => setTimeout(r, 20));
  }
  expect(pred(), "waitFor predicate timed out").toBe(true);
}

/** The pane's exit bar, or `null` while a shell is live. */
function exitBar(): Element | null {
  return host.querySelector("[data-terminal-exit-bar]");
}

/** Mount a `<Terminal>` bound to `recordId` in a real, sized box and wait for its first shell. */
async function mountLiveTerminal(recordId: string): Promise<void> {
  render(<Terminal recordId={recordId} />, { container: host });
  await waitFor(() => getTerminal(recordId)?.pty != null);
  await waitFor(() => host.querySelector(".xterm-rows") != null);
}

/** Kill the pane's shell the way the backend does, and wait for the bar to appear. */
async function killShell(recordId: string, code: number | null): Promise<void> {
  bridge.emit("pty://exit", { id: getTerminal(recordId)?.pty, code });
  await waitFor(() => exitBar() != null);
}

beforeEach(() => {
  spawns = [];
  bridge.reset();
  bridge.onAnyCommand((cmd, args) => {
    if (cmd === "pty_spawn") {
      spawns.push({ terminalId: args?.terminalId });
      return FIRST_PTY_ID + spawns.length - 1;
    }
    if (cmd === "terminal_screen_rehydration") return { bytes: [] };
    return null;
  });
  host = document.createElement("div");
  host.style.width = "640px";
  host.style.height = "360px";
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  document.body.appendChild(host);
});

afterEach(() => {
  __resetTerminalStore();
  bridge.reset();
  document.body.replaceChildren();
});

it("shows a bar naming the exit CODE, and relaunches the pane on click", async () => {
  await mountLiveTerminal("rec-click");

  // Nothing to say while the shell is alive — the bar must not be a permanent fixture.
  expect(exitBar()).toBeNull();

  await killShell("rec-click", 1);

  const bar = exitBar();
  expect(bar, "a pane whose process is gone must SAY so").not.toBeNull();
  expect(bar?.textContent).toContain("Exited");
  expect(bar?.textContent, "the code is what tells a clean exit from a crash").toContain("code 1");

  await page.getByRole("button", { name: "Relaunch" }).click();

  // A SECOND shell, in the same pane, bound to the same record — and the bar stands down.
  await waitFor(() => spawns.length === 2);
  expect(spawns[1].terminalId).toBe("rec-click");
  await waitFor(() => exitBar() == null);
  expect(getTerminal("rec-click")?.pty).toBe(FIRST_PTY_ID + 1);
});

it("relaunches on a bare Enter when the pane has the focus", async () => {
  await mountLiveTerminal("rec-enter");
  // The pane is the active one, so `<Terminal>` has already put the focus in xterm's textarea —
  // which is exactly where a user's Enter would land. Focused explicitly so the chord is being
  // proven, not the focus restore (which has its own spec).
  const textarea = host.querySelector<HTMLTextAreaElement>("textarea.xterm-helper-textarea");
  expect(textarea, "xterm's input must exist to type into").not.toBeNull();
  textarea?.focus();

  await killShell("rec-enter", 0);
  textarea?.focus();

  await userEvent.keyboard("{Enter}");

  await waitFor(() => spawns.length === 2);
  expect(spawns[1].terminalId).toBe("rec-enter");
  await waitFor(() => exitBar() == null);
});
