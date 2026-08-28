import { render } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { TerminalDeck } from "@/components/sidebar/terminal-deck";
import { PaneTreeHost } from "@/components/terminals/pane-tree-host";
import { createPaneLayout } from "@/components/panes/pane-tree";
import { __deckSeamHolders, claimDeckSeam } from "./deck-seam";
import { __resetTerminalStore } from "./terminal-store";

/**
 * The `__nyxDeck` / `__nyxDeckInput` seam is a CONTRACT read by the e2e specs, and it has TWO
 * hosts (`<TerminalDeck>` and `<PaneTreeHost>`). These tests pin the ownership rule the shared
 * module exists for: the globals live as long as ANY host is mounted, and the unmount of one
 * never takes away the seam the other still publishes.
 */

interface SeamWindow {
  __nyxDeck?: Record<string, () => string>;
  __nyxDeckInput?: Record<string, (data: string) => void>;
}

const seam = () => window as unknown as SeamWindow;

function record(id: string): TerminalRecord {
  return {
    id,
    cwd: `/repo/${id}`,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  };
}

beforeEach(() => {
  mockIPC(
    (cmd) => {
      if (cmd === "pty_spawn") return 7;
      return null;
    },
    { shouldMockEvents: true },
  );
});

afterEach(() => {
  __resetTerminalStore();
  expect(__deckSeamHolders(), "every test releases its claims").toBe(0);
});

describe("claimDeckSeam", () => {
  it("publishes on the FIRST claim and removes only on the LAST release", () => {
    expect(seam().__nyxDeck).toBeUndefined();
    const releaseA = claimDeckSeam();
    const published = seam().__nyxDeck;
    expect(published).toBeDefined();
    expect(seam().__nyxDeckInput).toBeDefined();

    // A second holder does NOT republish (the same live objects stay in place)…
    const releaseB = claimDeckSeam();
    expect(seam().__nyxDeck).toBe(published);

    // …and the first release leaves the seam standing for the remaining holder.
    releaseA();
    expect(seam().__nyxDeck).toBe(published);
    // Releasing twice is a no-op — it must not drop someone else's claim.
    releaseA();
    expect(seam().__nyxDeck).toBe(published);

    releaseB();
    expect(seam().__nyxDeck).toBeUndefined();
    expect(seam().__nyxDeckInput).toBeUndefined();
  });
});

describe("the seam's two hosts", () => {
  it("survives the unmount of ONE host while the other is still mounted", async () => {
    const deck = render(<TerminalDeck terminals={[record("t1")]} activeId="t1" />);
    const tiling = render(
      <PaneTreeHost
        terminals={[record("t1")]}
        layout={createPaneLayout("t1", "p1")}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    expect(typeof seam().__nyxDeck?.["t1"]).toBe("function");
    expect(typeof seam().__nyxDeckInput?.["t1"]).toBe("function");

    // The deck goes away (the page switched to the tiling): the seam the tiling publishes must
    // still be there — this is the defect the shared owner closes.
    deck.unmount();
    expect(typeof seam().__nyxDeck?.["t1"]).toBe("function");
    expect(typeof seam().__nyxDeck?.["t1"]?.()).toBe("string");
    expect(typeof seam().__nyxDeckInput?.["t1"]).toBe("function");

    // The last host leaves → the globals go with it (nothing is left dangling).
    tiling.unmount();
    expect(seam().__nyxDeck).toBeUndefined();
    expect(seam().__nyxDeckInput).toBeUndefined();
  });

  it("is published by EACH host on its own", () => {
    const tiling = render(
      <PaneTreeHost
        terminals={[record("t1")]}
        layout={createPaneLayout("t1", "p1")}
        focusedPaneId="p1"
        onFocusPane={() => {}}
        onSplitPane={() => {}}
        onClosePane={() => {}}
      />,
    );
    expect(typeof seam().__nyxDeck?.["t1"]).toBe("function");
    tiling.unmount();
    expect(seam().__nyxDeck).toBeUndefined();

    const deck = render(<TerminalDeck terminals={[record("t1")]} activeId="t1" />);
    expect(typeof seam().__nyxDeck?.["t1"]).toBe("function");
    deck.unmount();
    expect(seam().__nyxDeck).toBeUndefined();
  });
});
