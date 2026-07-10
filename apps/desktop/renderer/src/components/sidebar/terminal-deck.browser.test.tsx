import { render } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { afterEach, beforeEach, expect, it } from "vitest";

import { TerminalDeck } from "./terminal-deck";
import { __resetTerminalStore } from "@/components/terminal/terminal-store";
import type { TerminalRecord } from "./use-terminals";

/**
 * BROWSER-MODE multi-terminal render test (real Chromium via Playwright,
 * headless). The interactive terminal uses xterm's DEFAULT DOM renderer (NO
 * WebGL), so there is no GL context to count/exhaust; instead this suite proves
 * the KEEP-ALIVE design of `<TerminalDeck>` + the module-level store:
 *
 *   - several terminals are mounted at once (all alive, buffers + PTY listeners
 *     live), only ONE of them is the active/visible pane;
 *   - switching the active pane never RESPAWNS a shell and never unmounts an
 *     inactive terminal — the live instances stay in the store, so their buffers
 *     survive across switches (and across a full navigation).
 */

const SPAWNED_ID = 11;
let spawns = 0;

beforeEach(() => {
  spawns = 0;
  // Resolve every backend command so the deck's terminals spawn without a real
  // backend. pty_spawn returns an id and is counted; the rest are harmless.
  mockIPC(
    (cmd) => {
      if (cmd === "pty_spawn") {
        spawns += 1;
        return SPAWNED_ID;
      }
      if (cmd === "list_terminals") return [];
      if (cmd === "terminal_info") return { cwd: null, foreground: null };
      return null;
    },
    { shouldMockEvents: true },
  );
});

afterEach(() => {
  __resetTerminalStore();
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

it("mounts MANY terminals; switching the active pane never respawns or unmounts them", async () => {
  const host = mountHost();

  // FIVE terminals open at once. Only the first is active.
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

  // Each record spawns exactly once (its instance is created once in the store).
  const allSpawned = await waitFor(() => spawns === terminals.length);
  expect(allSpawned, `expected ${terminals.length} spawns, saw ${spawns}`).toBe(true);
  expect(host.querySelectorAll("[data-terminal-id]").length, "all panes mounted").toBe(5);

  // Switching the active terminal must NOT respawn any shell (the instances live in
  // the store, decoupled from which pane is visible) and must keep all panes mounted.
  for (const next of ["3", "5", "2", "4"]) {
    rerender(<TerminalDeck terminals={terminals} activeId={next} />);
    // Give any (erroneous) respawn a chance to fire.
    await new Promise((r) => setTimeout(r, 100));
    expect(spawns, `switching active→${next} must not respawn a shell`).toBe(terminals.length);
    expect(
      host.querySelectorAll("[data-terminal-id]").length,
      `switching active→${next} must keep every terminal mounted`,
    ).toBe(5);
  }
});

it("keeps INACTIVE terminals mounted and visually hidden (buffer stays alive)", async () => {
  const host = mountHost();
  const terminals = [record(1, "/a"), record(2, "/b"), record(3, "/c")];

  render(<TerminalDeck terminals={terminals} activeId={"2"} />, {
    container: host,
  });

  // All three panes are mounted in the DOM (inactive ones are merely hidden, not
  // unmounted — that is what keeps their xterm buffer + PTY listeners alive).
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
