import { act, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { mockIPC, emit } from "@/bridge/test-harness";
import { Terminal } from "./terminal";
import { FakeTerminalCore } from "./fake-core";
import { setCoreFactory, type CoreFactory } from "./ghostty-core";
import { __resetTerminalStore, getTerminal } from "./terminal-store";

/**
 * `<Terminal>` unit contract (jsdom). v2.2.5 decouples the live terminal from the
 * component: the instance lives in the module-level store; the component only
 * attaches its persistent DOM node into a container and detaches on unmount. The
 * ghostty WASM core cannot load under jsdom, so we inject a {@link FakeTerminalCore}
 * (which records writes) via the core-factory seam. Full VT RENDER (shell + TUI)
 * is a browser-mode / e2e concern — see the validation gate (Phase 3).
 */

interface IpcRecorder {
  calls: { cmd: string; args: Record<string, unknown> }[];
  callsTo: (cmd: string) => { cmd: string; args: Record<string, unknown> }[];
}

const SPAWNED_ID = 42;

function installIpc(): IpcRecorder {
  const calls: IpcRecorder["calls"] = [];
  mockIPC(
    (cmd, args) => {
      calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      if (cmd === "pty_spawn") return SPAWNED_ID;
      return null;
    },
    { shouldMockEvents: true },
  );
  return { calls, callsTo: (cmd) => calls.filter((c) => c.cmd === cmd) };
}

/** The fake core the CURRENT test's instance is built with (captured per test). */
let lastCore: FakeTerminalCore | null = null;
let restoreFactory: CoreFactory | null = null;

describe("<Terminal> (no-host store)", () => {
  let ipc: IpcRecorder;

  beforeEach(() => {
    ipc = installIpc();
    lastCore = null;
    restoreFactory = setCoreFactory(async () => {
      lastCore = new FakeTerminalCore();
      return lastCore;
    });
  });

  afterEach(() => {
    __resetTerminalStore();
    if (restoreFactory) setCoreFactory(restoreFactory);
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("creates a store instance and spawns a PTY at mount", async () => {
    render(<Terminal recordId="t1" />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));
    expect(getTerminal("t1")).toBeDefined();
    // The spawn carries the record id so the backend maps pty_id → record.
    expect(ipc.callsTo("pty_spawn")[0].args.terminalId).toBe("t1");
  });

  it("writes bytes received on pty://output into the terminal core", async () => {
    render(<Terminal recordId="t1" />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    const text = "hello-from-pty-7f3a";
    const bytes = Array.from(new TextEncoder().encode(text));
    await act(async () => {
      await emit("pty://output", { id: SPAWNED_ID, bytes });
    });

    await waitFor(() => expect(lastCore?.written).toContain(text));
  });

  it("ignores pty://output for a different terminal id", async () => {
    render(<Terminal recordId="t1" />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    const bytes = Array.from(new TextEncoder().encode("not-mine"));
    await act(async () => {
      await emit("pty://output", { id: SPAWNED_ID + 999, bytes });
    });
    await new Promise((r) => setTimeout(r, 20));
    expect(lastCore?.written ?? "").not.toContain("not-mine");
  });

  it("forwards keystrokes (onData) to pty_write encoded as bytes", async () => {
    render(<Terminal recordId="t1" />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    act(() => {
      // Drive the instance's input path directly (what the wterm input handler calls).
      getTerminal("t1")?.sendInput("ls\n");
    });

    await waitFor(() => expect(ipc.callsTo("pty_write").length).toBeGreaterThanOrEqual(1));
    const writes = ipc.callsTo("pty_write");
    const joined = writes
      .flatMap((w) => w.args.data as number[])
      .map((n) => String.fromCharCode(n))
      .join("");
    expect(joined).toContain("ls");
    expect(writes[0].args.id).toBe(SPAWNED_ID);
  });

  it("DETACHES on unmount without destroying — the instance survives (no pty_close)", async () => {
    const { unmount } = render(<Terminal recordId="t1" />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));
    const instance = getTerminal("t1");
    expect(instance).toBeDefined();

    await act(async () => {
      unmount();
      await new Promise((r) => setTimeout(r, 0));
    });

    // The instance is STILL in the store (navigation preserves it) and the PTY was
    // NOT closed — detach, not destroy.
    expect(getTerminal("t1")).toBe(instance);
    expect(ipc.callsTo("pty_close")).toHaveLength(0);
  });

  it("REATTACHES the SAME instance on remount without respawning the shell", async () => {
    const first = render(<Terminal recordId="t1" />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));
    const instance = getTerminal("t1");

    await act(async () => {
      first.unmount();
      await new Promise((r) => setTimeout(r, 0));
    });

    // Navigate back: remount the same record. The node is reattached into the new
    // container and NO second spawn occurs.
    const second = render(<Terminal recordId="t1" />);
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(getTerminal("t1")).toBe(instance);
    expect(ipc.callsTo("pty_spawn")).toHaveLength(1);
    // The persistent node (with its input textarea child) is inside the new container.
    const container = second.container.querySelector("[data-terminal-container='t1']");
    expect(container?.querySelector("textarea")).not.toBeNull();
  });

  it("gives the container inner padding (p-2.5) so the terminal is not glued to the edge", async () => {
    // Regression guard (v2.2.5): the OUTER container must carry `p-2.5` like the
    // pre-wterm xterm pane. The instance node (`el`, 100%×100%, in normal flow) is
    // appended INTO this padded box, so its bounding rect is inset and the grid
    // recomputes on the reduced content-box — the content is not flush to the edge.
    const { container } = render(<Terminal recordId="t1" />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));
    const host = container.querySelector("[data-terminal-container='t1']");
    expect(host).not.toBeNull();
    expect(host?.className).toContain("p-2.5");
  });

  it("handles pty://exit without crashing", async () => {
    render(<Terminal recordId="t1" />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    await act(async () => {
      await emit("pty://exit", { id: SPAWNED_ID, code: 0 });
    });
    await waitFor(() => expect(lastCore?.written).toContain("process exited"));
  });
});
