import { cleanup, render, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { TerminalManager } from "./terminal-manager";
import { __resetTerminalStore, terminalStore } from "@/components/terminal/terminal-store";
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
    // The IPC double is installed per-test in each it() with the desired record set. What is
    // checked here is the PRECONDITION every one of them depends on: no live instance survives
    // from the previous test. `ensureTerminal` is idempotent by id, so a leftover instance for a
    // re-used id silently skips the spawn the tests assert on — a cross-test leak that shows up
    // as an empty spawn list three tests later and reads as a timeout. Named here, it fails where
    // it happens.
    expect(terminalStore.state.ids).toEqual([]);
  });

  afterEach(() => {
    // UNMOUNT FIRST, then drop the store. The manager's terminals live in a module-level store,
    // and a later test's `ensureTerminal` for a re-used id would find the previous test's instance
    // and skip the spawn we assert on. (Previously this reset was implicit — the deck's per-render
    // reconcile disposed leftovers on the initial empty list — but reconciliation now lives
    // app-wide in `base-layout`, so the test owns its cleanup.)
    //
    // The ORDER is the whole point (finding `01KYW0SA15GX1CZ4QFVY9VD75G`). Vitest runs `afterEach`
    // hooks LAST-REGISTERED-FIRST, so this one runs BEFORE `vitest.setup.ts`'s `cleanup()` — i.e.
    // the store used to be emptied while the previous test's tree was STILL MOUNTED, and that live
    // tree re-created the instances it had just lost before the unmount arrived. The next test then
    // opened on a store already holding ids `1` and `3`, spawned nothing, and failed with an empty
    // spawn list that no amount of waiting could fill. Unmounting here first (RTL's `cleanup` is
    // idempotent, so the setup file's own call stays a harmless no-op) leaves nothing alive to
    // repopulate the store.
    cleanup();
    __resetTerminalStore();
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
