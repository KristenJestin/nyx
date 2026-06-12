import { act, render, waitFor } from "@testing-library/react";
import type { Terminal as XTerm } from "@xterm/xterm";
import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Terminal } from "./terminal";

/**
 * Records every `invoke` the component makes and serves canned results so no
 * real Tauri backend is touched. Event mocking is enabled so `listen`/`emit`
 * round-trip in-process, letting us inject `pty://output` bytes.
 */
interface IpcRecorder {
  calls: { cmd: string; args: Record<string, unknown> }[];
  /** invoke calls for a given command name. */
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
  return {
    calls,
    callsTo: (cmd) => calls.filter((c) => c.cmd === cmd),
  };
}

/** Read the whole xterm buffer (active screen + scrollback) as one string. */
function bufferText(term: XTerm): string {
  const buf = term.buffer.active;
  let out = "";
  for (let i = 0; i < buf.length; i++) {
    const line = buf.getLine(i);
    if (line) out += line.translateToString(true);
  }
  return out;
}

describe("<Terminal>", () => {
  let ipc: IpcRecorder;

  beforeEach(() => {
    ipc = installIpc();
  });

  afterEach(() => {
    // clearMocks() runs in vitest.setup.ts afterEach.
    vi.useRealTimers();
  });

  it("writes bytes received on pty://output into the xterm buffer", async () => {
    let term: XTerm | null = null;
    render(<Terminal onInstance={(t) => (term = t ?? term)} />);

    // Wait until the component has spawned (so listen() is registered and the
    // session id is set) before injecting output.
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));
    expect(term).not.toBeNull();

    const text = "hello-from-pty-7f3a";
    const bytes = Array.from(new TextEncoder().encode(text));

    await act(async () => {
      await emit("pty://output", { id: SPAWNED_ID, bytes });
    });

    // xterm parses writes asynchronously; wait for the buffer to reflect it.
    await waitFor(() => {
      expect(bufferText(term as unknown as XTerm)).toContain(text);
    });
  });

  it("ignores pty://output for a different terminal id", async () => {
    let term: XTerm | null = null;
    render(<Terminal onInstance={(t) => (term = t ?? term)} />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    const bytes = Array.from(new TextEncoder().encode("not-mine"));
    await act(async () => {
      await emit("pty://output", { id: SPAWNED_ID + 999, bytes });
    });

    // Give any (incorrect) write a chance to land, then assert it did NOT.
    await new Promise((r) => setTimeout(r, 20));
    expect(bufferText(term as unknown as XTerm)).not.toContain("not-mine");
  });

  it("spawns a PTY at mount and closes it at unmount", async () => {
    const { unmount } = render(<Terminal />);

    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));
    expect(ipc.callsTo("pty_close")).toHaveLength(0);

    await act(async () => {
      unmount();
      // usePty defers teardown to a macrotask so a StrictMode re-setup can
      // cancel it; flush that here.
      await new Promise((r) => setTimeout(r, 0));
    });

    await waitFor(() => {
      const close = ipc.callsTo("pty_close");
      expect(close).toHaveLength(1);
      expect(close[0].args.id).toBe(SPAWNED_ID);
    });
  });

  it("spawns exactly one PTY under React.StrictMode (no double-spawn)", async () => {
    render(
      <StrictMode>
        <Terminal />
      </StrictMode>,
    );

    // StrictMode mounts → unmounts → remounts. Wait long enough that a buggy
    // implementation's second spawn would have fired, then assert a single one.
    await waitFor(() => expect(ipc.callsTo("pty_spawn").length).toBeGreaterThan(0));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(ipc.callsTo("pty_spawn")).toHaveLength(1);
    // The StrictMode throwaway teardown must NOT close the surviving PTY.
    expect(ipc.callsTo("pty_close")).toHaveLength(0);
  });

  it("forwards keystrokes (onData) to pty_write encoded as bytes", async () => {
    let term: XTerm | null = null;
    render(<Terminal onInstance={(t) => (term = t ?? term)} />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    act(() => {
      (term as unknown as XTerm).input("ls\n", true);
    });

    await waitFor(() => {
      const writes = ipc.callsTo("pty_write");
      expect(writes.length).toBeGreaterThanOrEqual(1);
    });
    const writes = ipc.callsTo("pty_write");
    const joined = writes
      .flatMap((w) => w.args.data as number[])
      .map((n) => String.fromCharCode(n))
      .join("");
    expect(joined).toContain("ls");
    expect(writes[0].args.id).toBe(SPAWNED_ID);
  });

  it("handles pty://exit without crashing", async () => {
    let term: XTerm | null = null;
    render(<Terminal onInstance={(t) => (term = t ?? term)} />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    await act(async () => {
      await emit("pty://exit", { id: SPAWNED_ID, code: 0 });
    });

    await waitFor(() => {
      expect(bufferText(term as unknown as XTerm)).toContain("process exited");
    });
  });
});
