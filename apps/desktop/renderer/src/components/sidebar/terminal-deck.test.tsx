import { act, render, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC, emit } from "@/bridge/test-harness";
import { FakeTerminalCore } from "@/components/terminal/fake-core";
import { setCoreFactory } from "@/components/terminal/ghostty-core";
import { getTerminal } from "@/components/terminal/terminal-store";
import { TerminalDeck } from "./terminal-deck";
import type { TerminalRecord } from "./use-terminals";

/**
 * Post-v2.2.5 the deck mounts one `<Terminal>` pane per record; each pane attaches
 * the record's store instance. We hand out a DISTINCT pty id per spawn (in call
 * order) and capture the fake core each instance is built on, so we can push
 * `pty://output` to a SPECIFIC (possibly inactive) terminal and prove it landed
 * in that instance's core (the assertion that an inactive terminal still receives
 * its output). The ghostty WASM cannot load under jsdom, hence the fake core.
 */
interface IpcRecorder {
  spawnCwds: (string | undefined)[];
}

/** Map record id → the fake core its instance was built on. */
const coresByRecord = new Map<string, FakeTerminalCore>();

function installIpc(): IpcRecorder {
  const rec: IpcRecorder = { spawnCwds: [] };
  let nextPtyId = 100;
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      if (cmd === "pty_spawn") {
        rec.spawnCwds.push(a.cwd as string | undefined);
        return nextPtyId++;
      }
      return null;
    },
    { shouldMockEvents: true },
  );
  return rec;
}

function row(id: number, cwd: string, order: number): TerminalRecord {
  return {
    id: String(id),
    cwd,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: order,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  };
}

const records = [row(1, "/a", 0), row(2, "/b", 1), row(3, "/c", 2)];

describe("<TerminalDeck>", () => {
  beforeEach(() => {
    installIpc();
    coresByRecord.clear();
    // Capture the fake core each instance is built on. The store builds one core
    // per record in record order, so we tag them by the render order below.
    setCoreFactory(async () => new FakeTerminalCore());
  });

  it("mounts one terminal container per record, all present in the DOM", async () => {
    const { container } = render(<TerminalDeck terminals={records} activeId={"1"} />);
    await waitFor(() => expect(container.querySelectorAll("[data-terminal-id]")).toHaveLength(3));
  });

  it("shows only the active terminal; inactive ones are hidden but mounted", async () => {
    const { container } = render(<TerminalDeck terminals={records} activeId={"2"} />);
    await waitFor(() => expect(container.querySelectorAll("[data-terminal-id]")).toHaveLength(3));

    const panes = Array.from(container.querySelectorAll<HTMLElement>("[data-terminal-id]"));
    for (const pane of panes) {
      const id = Number(pane.getAttribute("data-terminal-id"));
      const hidden = pane.getAttribute("data-active") !== "true";
      // Active (id 2) is shown; the other two are hidden (display:none) yet still
      // in the DOM (mounted) — and their store instance stays alive regardless.
      expect(hidden).toBe(id !== 2);
      if (hidden) expect(pane.style.display).toBe("none");
    }
  });

  it("an INACTIVE terminal still receives its pty output", async () => {
    const ipc = installIpc();
    // Active is id 1; we drive output to id 3 (inactive) and prove it landed.
    render(<TerminalDeck terminals={records} activeId={"1"} />);

    // Wait until all three PTYs have spawned (so each instance is wired).
    await waitFor(() => expect(ipc.spawnCwds).toHaveLength(3));

    // Spawn order follows record order → record 1→100, 2→101, 3→102.
    const inactivePtyId = 102; // record id 3, which is NOT active.
    const text = "inactive-got-output-zz9";
    const bytes = Array.from(new TextEncoder().encode(text));

    await act(async () => {
      await emit("pty://output", { id: inactivePtyId, bytes });
    });

    // The hidden terminal's instance exists in the store and its core received the
    // bytes even while its pane is hidden — read via the instance's screen-text
    // seam (the same `__nyxDeck` contract the e2e uses). The fake core models a
    // linear screen from printable writes, so the seam reflects those bytes.
    await waitFor(() => expect(getTerminal("3")).toBeDefined());
    await waitFor(() => {
      const win = window as unknown as { __nyxDeck?: Record<string, () => string> };
      const read = win.__nyxDeck?.["3"];
      expect(read).toBeTypeOf("function");
      expect(read?.()).toContain(text);
    });
  });
});
