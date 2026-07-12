import { act, render, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { TerminalDeck } from "./terminal-deck";
import { __resetTerminalStore, getTerminal } from "@/components/terminal/terminal-store";
import type { TerminalRecord } from "./use-terminals";

/**
 * Each mounted <Terminal> spawns its own PTY. We hand out a DISTINCT pty id per
 * spawn (in call order) and record the cwd, so we can map record â†’ pty id and
 * then push `pty://output` to a SPECIFIC (possibly inactive) terminal and prove
 * it landed.
 */
interface IpcRecorder {
  spawnCwds: (string | undefined)[];
  /** PTY ids passed to `pty_close` — a NON-empty array means a terminal was disposed. */
  closes: number[];
}

function installIpc(): IpcRecorder {
  const rec: IpcRecorder = { spawnCwds: [], closes: [] };
  let nextPtyId = 100;
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      if (cmd === "pty_spawn") {
        rec.spawnCwds.push(a.cwd as string | undefined);
        return nextPtyId++;
      }
      if (cmd === "pty_close") {
        rec.closes.push((a.id as number) ?? -1);
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
  });

  afterEach(() => {
    // The deck's terminals live in a module-level store; drop them so instances
    // (and their PTY subscriptions) never leak across tests.
    __resetTerminalStore();
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
      // Active (id 2) is shown; the other two are hidden (display:none) yet
      // still in the DOM (mounted) so their xterm buffer stays alive.
      expect(hidden).toBe(id !== 2);
      if (hidden) {
        expect(pane.style.display).toBe("none");
      }
    }
  });

  it("an INACTIVE terminal still receives its pty output", async () => {
    const ipc = installIpc();
    // Active is id 1; we drive output to id 3 (inactive) and prove it landed.
    render(<TerminalDeck terminals={records} activeId={"1"} />);

    // Wait until all three PTYs have spawned (so we know each pty id).
    await waitFor(() => expect(ipc.spawnCwds).toHaveLength(3));

    // Spawn order follows record order â†’ record /aâ†’100, /bâ†’101, /câ†’102.
    const inactivePtyId = 102; // record id 3, which is NOT active.
    const text = "inactive-got-output-zz9";
    const bytes = Array.from(new TextEncoder().encode(text));

    await act(async () => {
      await emit("pty://output", { id: inactivePtyId, bytes });
    });

    // The hidden pane's xterm buffer must contain the bytes. We read via the
    // test seam each <Terminal> exposes on the pane element.
    await waitFor(() => {
      const win = window as unknown as {
        __nyxDeck?: Record<number, () => string>;
      };
      const read = win.__nyxDeck?.[3];
      expect(read).toBeTypeOf("function");
      expect(read?.()).toContain(text);
    });
  });

  it("does NOT dispose store instances when its terminal set shrinks (a project switch)", async () => {
    const ipc = installIpc();
    // The deck starts showing the ENTERED project's 3 terminals.
    const { rerender } = render(<TerminalDeck terminals={records} activeId={"1"} />);
    await waitFor(() => expect(ipc.spawnCwds).toHaveLength(3));
    expect(getTerminal("1")).toBeDefined();
    expect(getTerminal("2")).toBeDefined();
    expect(getTerminal("3")).toBeDefined();

    // The user switches to ANOTHER project: the deck now receives only that project's terminals
    // (here, just record 1). The panes for 2 + 3 unmount — but the deck must NOT dispose their
    // store instances (their records are still alive in the other project). This is the regression
    // guard for "changer de projet TUE les terminaux de l'ancien projet": no `pty_close`.
    rerender(<TerminalDeck terminals={[records[0]!]} activeId={"1"} />);
    await waitFor(() => expect(document.querySelectorAll("[data-terminal-id]")).toHaveLength(1));

    // Give any (erroneous) reconcile a chance to fire, then assert none closed a PTY and the
    // instances for the now-hidden project survive in the store (their agents keep running).
    await new Promise((r) => setTimeout(r, 20));
    expect(ipc.closes).toHaveLength(0);
    expect(getTerminal("1")).toBeDefined();
    expect(getTerminal("2")).toBeDefined();
    expect(getTerminal("3")).toBeDefined();
  });
});
