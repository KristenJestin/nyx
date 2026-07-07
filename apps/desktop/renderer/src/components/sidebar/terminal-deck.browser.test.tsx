import "@wterm/dom/css";

import { render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { getTerminal, __resetTerminalStore } from "@/components/terminal/terminal-store";
import { TerminalDeck } from "./terminal-deck";
import type { TerminalRecord } from "./use-terminals";

/**
 * BROWSER-MODE multi-terminal render test (real Chromium via Playwright,
 * headless) with the REAL ghostty WASM core. Post-v2.2.5 wterm renders to the
 * DOM, NOT a WebGL canvas — so the old "keep at most one WebGL context to avoid
 * exhaustion" concern is GONE by construction (there is no GL context to
 * exhaust). This suite instead proves what matters now:
 *
 *   - MANY terminals mount at once, each rendering its OWN wterm DOM (no shared
 *     GL context, no pool ceiling) — the keep-alive win the DOM renderer buys;
 *   - only ONE pane is visible; the rest are `display:none` but still mounted and
 *     their store instances alive (buffer/PTY survive a selection switch);
 *   - switching the active pane never destroys an instance.
 */

const SPAWNED_ID = 11;

beforeEach(() => {
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

async function waitFor(pred: () => boolean, timeoutMs = 8000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 50));
  }
  return pred();
}

it("mounts MANY terminals, each with its own live wterm DOM (no shared-context ceiling)", async () => {
  const host = mountHost();
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

  // Every one of the five terminals builds its own wterm grid (the DOM renderer
  // means N terminals cost N DOM subtrees — never a capped GL context pool).
  const allRendered = await waitFor(
    () => host.querySelectorAll(".wterm .term-grid").length === terminals.length,
  );
  expect(
    allRendered,
    `all ${terminals.length} terminals must render their own wterm DOM (found ` +
      `${host.querySelectorAll(".wterm .term-grid").length})`,
  ).toBe(true);

  // Switching the active pane never destroys an instance — they all stay in the
  // store (the no-host keep-alive), regardless of which is visible.
  for (const next of ["3", "5", "2", "4"]) {
    rerender(<TerminalDeck terminals={terminals} activeId={next} />);
    await new Promise((r) => setTimeout(r, 50));
    for (const t of terminals) {
      expect(
        getTerminal(t.id),
        `terminal ${t.id} must stay alive after switch→${next}`,
      ).toBeDefined();
    }
  }
});

it("keeps INACTIVE terminals mounted and visually hidden (state stays alive)", async () => {
  const host = mountHost();
  const terminals = [record(1, "/a"), record(2, "/b"), record(3, "/c")];

  render(<TerminalDeck terminals={terminals} activeId={"2"} />, { container: host });

  // All three panes are mounted in the DOM (inactive ones are merely hidden, not
  // unmounted — and their store instances stay alive regardless).
  await waitFor(() => host.querySelectorAll("[data-terminal-id]").length === 3);
  const panes = host.querySelectorAll<HTMLElement>("[data-terminal-id]");
  expect(panes.length, "all three terminals stay mounted").toBe(3);

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
  // Every terminal's instance is alive in the store, hidden or not.
  for (const t of terminals) expect(getTerminal(t.id)).toBeDefined();
});
