import { render } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { afterEach, beforeEach, expect, it } from "vitest";

import { TerminalDeck } from "./terminal-deck";
import type { TerminalRecord } from "./use-terminals";

/**
 * BROWSER-MODE multi-terminal render test (real Chromium via Playwright,
 * headless). This is the place WebGL actually paints, so it is where we can
 * prove the load-bearing anti-context-exhaustion design of `<TerminalDeck>` +
 * `useWebglAddon`:
 *
 *   - several terminals are mounted at once (all alive, buffers + PTY listeners
 *     live), only ONE of them is the active/visible pane;
 *   - the `@xterm/addon-webgl` renderer is attached to the ACTIVE pane ONLY, so
 *     no matter how many terminals are open there is at most ONE live WebGL
 *     context â€” browsers cap the pool (~16) and attaching WebGL to every pane
 *     would exhaust it and start dropping (context-loss) the older ones;
 *   - switching the active pane MOVES the single context (the old one is
 *     disposed, a fresh one attaches on the newly-active pane) â€” it never
 *     accumulates and never context-losses the live one.
 *
 * The jsdom unit suite (`use-webgl-addon.test.tsx`) proves the attach/dispose
 * CYCLE with a fake factory; only here, with a real GL context, can we count the
 * ACTUAL live contexts across all panes.
 */

const SPAWNED_ID = 11;

beforeEach(() => {
  // Resolve every backend command so the deck's terminals spawn + persist
  // without a real backend. pty_spawn returns an id; the rest are harmless.
  mockIPC(
    (cmd) => {
      if (cmd === "pty_spawn") return SPAWNED_ID;
      if (cmd === "list_terminals") return [];
      if (cmd === "terminal_info") return { cwd: null, foreground: null };
      return null;
    },
    { shouldMockEvents: true },
  );
});

afterEach(() => {
  document.body.replaceChildren();
});

function record(id: number, cwd: string): TerminalRecord {
  return {
    id: String(id),
    cwd,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: id,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  };
}

/**
 * Count the live WebGL contexts across every canvas under `root`. We read (never
 * create) a context: asking `getContext('webgl2'|'webgl')` on a canvas returns
 * the EXISTING context if the WebGL addon already made one, or null otherwise
 * (e.g. the default DOM/2d renderer of a hidden pane). So the count is exactly
 * the number of panes whose WebGL renderer is currently attached.
 */
function countWebglContexts(root: HTMLElement): number {
  let n = 0;
  for (const canvas of root.querySelectorAll("canvas")) {
    const gl =
      (canvas.getContext("webgl2") as WebGL2RenderingContext | null) ??
      (canvas.getContext("webgl") as WebGLRenderingContext | null);
    if (gl && typeof gl.getParameter === "function") n += 1;
  }
  return n;
}

/** Whether ANY live WebGL context under `root` reports a lost context. */
function anyContextLost(root: HTMLElement): boolean {
  for (const canvas of root.querySelectorAll("canvas")) {
    const gl =
      (canvas.getContext("webgl2") as WebGL2RenderingContext | null) ??
      (canvas.getContext("webgl") as WebGLRenderingContext | null);
    if (gl && typeof gl.isContextLost === "function" && gl.isContextLost()) {
      return true;
    }
  }
  return false;
}

function mountHost(): HTMLElement {
  const host = document.createElement("div");
  host.setAttribute("data-testid", "deck-host");
  host.style.width = "640px";
  host.style.height = "360px";
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  document.body.appendChild(host);
  return host;
}

/** Poll until `pred()` holds or we time out; returns whether it held. */
async function waitFor(pred: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 50));
  }
  return pred();
}

it("mounts MANY terminals but keeps exactly ONE live WebGL context (no exhaustion)", async () => {
  const host = mountHost();

  // FIVE terminals open at once â€” well into the territory where attaching WebGL
  // to every pane would start stressing the context pool. Only the first is
  // active.
  const terminals = [
    record(1, "/a"),
    record(2, "/b"),
    record(3, "/c"),
    record(4, "/d"),
    record(5, "/e"),
  ];

  const { rerender } = render(<TerminalDeck terminals={terminals} activeId={"1"} />, {
    container: host,
  });

  // The active pane's WebGL addon attaches in an effect after open(); poll until
  // exactly one context exists.
  const oneContext = await waitFor(() => countWebglContexts(host) === 1);
  expect(
    oneContext,
    `expected exactly ONE live WebGL context across ${terminals.length} mounted ` +
      `terminals, found ${countWebglContexts(host)} â€” the active-only WebGL ` +
      `attach is what prevents context-pool exhaustion at 15+ terminals.`,
  ).toBe(true);

  // And the single live context is healthy (not lost).
  expect(anyContextLost(host), "the active pane's WebGL context must not be lost").toBe(false);

  // Switching the active terminal MOVES the context (old disposed, new attached)
  // â€” it must STILL be exactly one, never two, never zero, never lost. Do a few
  // switches to prove it does not accumulate or leak.
  for (const next of ["3", "5", "2", "4"]) {
    rerender(<TerminalDeck terminals={terminals} activeId={next} />);
    const stillOne = await waitFor(() => countWebglContexts(host) === 1);
    expect(
      stillOne,
      `after switching activeâ†’${next}, exactly one WebGL context must remain ` +
        `(found ${countWebglContexts(host)}); the renderer must move, not stack.`,
    ).toBe(true);
    expect(
      anyContextLost(host),
      `after switching activeâ†’${next}, the live context must not be lost`,
    ).toBe(false);
  }
});

it("keeps INACTIVE terminals mounted and visually hidden (buffer stays alive)", async () => {
  const host = mountHost();
  const terminals = [record(1, "/a"), record(2, "/b"), record(3, "/c")];

  render(<TerminalDeck terminals={terminals} activeId={"2"} />, {
    container: host,
  });

  // All three panes are mounted in the DOM (inactive ones are merely hidden, not
  // unmounted â€” that is what keeps their xterm buffer + PTY listeners alive).
  await waitFor(() => host.querySelectorAll("[data-terminal-id]").length === 3);
  const panes = host.querySelectorAll<HTMLElement>("[data-terminal-id]");
  expect(panes.length, "all three terminals stay mounted").toBe(3);

  // Exactly one pane is active; the other two are display:none (hidden but live).
  const active = [...panes].filter((p) => p.dataset.active === "true");
  const hidden = [...panes].filter((p) => p.dataset.active !== "true");
  expect(active.length, "exactly one active pane").toBe(1);
  expect(active[0].dataset.terminalId, "the active pane is #2").toBe("2");
  for (const pane of hidden) {
    expect(
      getComputedStyle(pane).display,
      "an inactive pane is hidden via display:none (kept mounted/alive)",
    ).toBe("none");
  }
});
