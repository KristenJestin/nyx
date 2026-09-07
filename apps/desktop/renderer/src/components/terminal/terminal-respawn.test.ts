import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";
import {
  __resetTerminalStore,
  ensureTerminal,
  getTerminal,
  respawnTerminal,
  terminalStore,
} from "./terminal-store";

/**
 * A PANE WHOSE PROCESS EXITED IS NOT A DEAD END (jsdom).
 *
 * Before this, `pty://exit` wrote a grey `[process exited]` line, dropped the pty id, and the
 * instance's `spawned` latch made a second shell impossible for the life of the record: the only
 * way to get a working terminal back was to CLOSE the pane, losing its place in the tiling and its
 * whole history. What is held here is the way out — the exit is a published STATE, and `respawn`
 * puts a live shell back in the SAME instance and against the SAME record.
 *
 * The record identity is the load-bearing part: the relaunch's `pty_spawn` carries the same
 * `terminalId`, which is what re-arms everything the sidecar keys on the record (a parked
 * `claude --resume` drains at that spawn exactly as it would at the first one). Nothing in the
 * renderer knows about resumes; it only has to spawn against the right row.
 */

const FIRST_PTY_ID = 300;

let spawns: { terminalId: unknown }[] = [];
let closes: number[] = [];
let rehydrations = 0;
/** When set, `pty_spawn` throws — the "the relaunch did not land" case. */
let spawnFails = false;

/** Poll until `pred()` holds; the spawn and xterm's writes are both asynchronous. */
async function waitFor(pred: () => boolean, timeoutMs = 2000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return;
    await new Promise((r) => setTimeout(r, 5));
  }
  expect(pred(), "waitFor predicate timed out").toBe(true);
}

/** Kill the terminal's live shell the way the backend does — an exit event on its pty id. */
async function killShell(recordId: string, code: number | null): Promise<void> {
  const id = getTerminal(recordId)?.pty;
  expect(id, "the terminal must have a live pty to kill").not.toBeNull();
  await emit("pty://exit", { id, code });
  await waitFor(() => terminalStore.state.exits[recordId] !== undefined);
}

describe("a pane whose process exited relaunches in place", () => {
  beforeEach(() => {
    spawns = [];
    closes = [];
    rehydrations = 0;
    spawnFails = false;
    mockIPC((cmd, args) => {
      if (cmd === "pty_spawn") {
        if (spawnFails) throw new Error("sidecar unavailable");
        spawns.push({ terminalId: args?.terminalId });
        return FIRST_PTY_ID + spawns.length - 1;
      }
      if (cmd === "pty_close") closes.push((args?.id as number) ?? -1);
      if (cmd === "terminal_screen_rehydration") {
        rehydrations += 1;
        return { bytes: [] };
      }
      return null;
    });
  });

  afterEach(() => {
    __resetTerminalStore();
  });

  it("publishes the exit CODE on the store when the process goes", async () => {
    ensureTerminal({ recordId: "a" });
    await waitFor(() => getTerminal("a")?.pty != null);

    await killShell("a", 1);

    expect(terminalStore.state.exits["a"]).toEqual({ code: 1 });
    expect(getTerminal("a")?.pty, "the pty id is dropped with the process").toBeNull();
  });

  it("reports the exit with NO code when the backend could not read one", async () => {
    ensureTerminal({ recordId: "a" });
    await waitFor(() => getTerminal("a")?.pty != null);

    await killShell("a", null);

    expect(terminalStore.state.exits["a"]).toEqual({ code: null });
  });

  it("relaunches a NEW shell in the SAME instance and against the SAME record", async () => {
    const instance = ensureTerminal({ recordId: "a" });
    await waitFor(() => getTerminal("a")?.pty != null);
    const node = instance.el;
    await killShell("a", 0);

    const relaunched = await respawnTerminal("a");

    expect(relaunched).toBe(true);
    // SAME instance, SAME host node: the pane keeps its place in the tiling and its buffer.
    expect(getTerminal("a")).toBe(instance);
    expect(instance.el).toBe(node);
    // A second, DISTINCT pty — and it is bound to the same terminals row, which is what re-arms
    // everything the sidecar keys on the record (a parked resume among them).
    expect(spawns.length).toBe(2);
    expect(spawns[1].terminalId).toBe("a");
    expect(instance.pty).toBe(FIRST_PTY_ID + 1);
    // …and the bar is gone, because a shell is live again.
    expect(terminalStore.state.exits["a"]).toBeUndefined();
  });

  it("does NOT re-hydrate the screen on a relaunch — the pane keeps its history", async () => {
    ensureTerminal({ recordId: "a" });
    await waitFor(() => getTerminal("a")?.pty != null);
    await waitFor(() => rehydrations === 1);
    await killShell("a", 0);

    await respawnTerminal("a");

    // The re-hydration repaint STARTS BY CLEARING the screen: replaying it here would erase what
    // the dead shell printed and the grey `[process exited]` line under it.
    expect(rehydrations).toBe(1);
  });

  it("streams the NEW shell's output into the same pane", async () => {
    const instance = ensureTerminal({ recordId: "a" });
    await waitFor(() => getTerminal("a")?.pty != null);
    await killShell("a", 0);
    await respawnTerminal("a");

    await emit("pty://output", {
      id: FIRST_PTY_ID + 1,
      bytes: Array.from(new TextEncoder().encode("relaunched\r\n")),
    });

    await waitFor(() => instance.readScreenText().includes("relaunched"));
    // The old session's epitaph is still above it: a relaunch adds to the pane, it does not reset
    // it.
    expect(instance.readScreenText()).toContain("process exited");
  });

  it("is a NO-OP on a terminal whose shell is still live", async () => {
    ensureTerminal({ recordId: "a" });
    await waitFor(() => getTerminal("a")?.pty != null);

    expect(await respawnTerminal("a")).toBe(false);
    expect(spawns.length, "a live pane must never be given a second shell").toBe(1);
  });

  it("is a NO-OP on an unknown record", async () => {
    expect(await respawnTerminal("nope")).toBe(false);
    expect(spawns.length).toBe(0);
  });

  it("PUTS THE BAR BACK, flagged, when the relaunch does not land", async () => {
    ensureTerminal({ recordId: "a" });
    await waitFor(() => getTerminal("a")?.pty != null);
    await killShell("a", 3);

    spawnFails = true;
    const relaunched = await respawnTerminal("a");

    expect(relaunched).toBe(false);
    // Never a silent nothing: the pane keeps its exit AND says the relaunch failed, so a second
    // press is an informed one.
    expect(terminalStore.state.exits["a"]).toEqual({ code: 3, relaunchFailed: true });
  });

  it("drops the exit when the terminal is really CLOSED", async () => {
    ensureTerminal({ recordId: "a" });
    await waitFor(() => getTerminal("a")?.pty != null);
    await killShell("a", 1);

    __resetTerminalStore();

    // A closed record is not a finished one waiting to be relaunched — nothing may keep offering
    // it a button.
    expect(terminalStore.state.exits["a"]).toBeUndefined();
    expect(terminalStore.state.ids).toEqual([]);
  });
});
