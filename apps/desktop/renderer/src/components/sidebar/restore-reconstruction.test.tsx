import { render, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it } from "vitest";

import { TerminalManager } from "./terminal-manager";
import type { TerminalRecord } from "./use-terminals";

/**
 * Reconstruction at launch (jsdom): given a persisted set of records â€” some
 * ALIVE (with scrollback + cwd + order) and one CLOSED â€” the manager must
 * re-mount only the alive terminals IN ORDER, spawn a fresh shell at each
 * persisted cwd, and inject each one's prior scrollback as dead history. The
 * closed record is never mounted, never re-spawned.
 *
 * The cross-process "actually close the app and reopen" cycle is the e2e
 * tauri-driver scenario (phase 4); here we prove the FRONT reconstruction logic
 * off a mocked `list_terminals`.
 */
interface IpcSpy {
  /** cwd of every pty_spawn, in call order (= mount order). */
  spawnCwds: (string | undefined)[];
  /** Anything written to a PTY â€” must never contain restored history. */
  ptyWrites: string[];
}

function row(
  id: number,
  cwd: string,
  order: number,
  status: "alive" | "closed",
  scrollback: string,
): TerminalRecord {
  return {
    id: String(id),
    cwd,
    label: null,
    scrollback,
    status,
    order_index: order,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  };
}

function installIpc(rows: TerminalRecord[]): IpcSpy {
  const spy: IpcSpy = { spawnCwds: [], ptyWrites: [] };
  let nextPtyId = 300;
  const decoder = new TextDecoder();
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_terminals":
          return [...rows].sort(
            (x, y) => x.order_index - y.order_index || x.id.localeCompare(y.id),
          );
        case "pty_spawn":
          spy.spawnCwds.push(a.cwd as string | undefined);
          return nextPtyId++;
        case "pty_write":
          spy.ptyWrites.push(decoder.decode(Uint8Array.from(a.data as number[])));
          return null;
        // create/close/reorder/rename/persist are exercised elsewhere; no-op here.
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  return spy;
}

const SCROLLBACK_A = "A_HISTORY_marker";
const SCROLLBACK_C = "C_HISTORY_marker";

describe("restore reconstruction at launch", () => {
  beforeEach(() => {
    // installed per-test in each it() with the desired record set.
  });

  it("re-mounts only ALIVE terminals, in order, never the closed one", async () => {
    installIpc([
      row(1, "/projA", 0, "alive", SCROLLBACK_A),
      row(2, "/projB", 1, "closed", "B_HISTORY_should_not_appear"),
      row(3, "/projC", 2, "alive", SCROLLBACK_C),
    ]);

    const { container } = render(<TerminalManager />);

    // Two alive panes mount; the closed one does not.
    await waitFor(() => {
      const panes = container.querySelectorAll("[data-terminal-id]");
      expect(panes).toHaveLength(2);
    });
    const ids = Array.from(container.querySelectorAll("[data-terminal-id]")).map((p) =>
      p.getAttribute("data-terminal-id"),
    );
    // Order preserved (1 then 3); the closed id 2 is absent.
    expect(ids).toEqual(["1", "3"]);
  });

  it("spawns a FRESH shell at each persisted cwd, in order", async () => {
    const spy = installIpc([
      row(1, "/projA", 0, "alive", SCROLLBACK_A),
      row(3, "/projC", 1, "alive", SCROLLBACK_C),
    ]);

    render(<TerminalManager />);

    await waitFor(() => expect(spy.spawnCwds).toHaveLength(2));
    // Each alive terminal spawned a new shell at its persisted cwd, in order.
    expect(spy.spawnCwds).toEqual(["/projA", "/projC"]);
  });

  it("injects each terminal's prior scrollback as dead history (read-only)", async () => {
    const spy = installIpc([
      row(1, "/projA", 0, "alive", SCROLLBACK_A),
      row(3, "/projC", 1, "alive", SCROLLBACK_C),
    ]);

    render(<TerminalManager />);

    await waitFor(() => expect(spy.spawnCwds).toHaveLength(2));

    // Each terminal's buffer carries its OWN restored history (read via the
    // deck seam, which works for hidden panes too).
    await waitFor(() => {
      const win = window as unknown as {
        __nyxDeck?: Record<number, () => string>;
      };
      expect(win.__nyxDeck?.[1]?.()).toContain(SCROLLBACK_A);
      expect(win.__nyxDeck?.[3]?.()).toContain(SCROLLBACK_C);
    });

    // The restored history was never sent to any PTY (it is read-only).
    for (const w of spy.ptyWrites) {
      expect(w).not.toContain(SCROLLBACK_A);
      expect(w).not.toContain(SCROLLBACK_C);
    }
  });
});
