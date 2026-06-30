import { act, render, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import type { Terminal as XTerm } from "@xterm/xterm";
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
    // Drop any per-test navigator/clipboard stub so it never leaks across tests.
    vi.unstubAllGlobals();
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

  it("copies the selection on Ctrl+Shift+C without sending ^C to the PTY", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });

    let term: XTerm | null = null;
    render(<Terminal onInstance={(t) => (term = t ?? term)} />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    // Seed a known selection. We stub hasSelection/getSelection on the live
    // instance (xterm's selection model needs a painted viewport jsdom lacks),
    // so the copy path reads a deterministic value.
    const t = term as unknown as XTerm;
    vi.spyOn(t, "hasSelection").mockReturnValue(true);
    vi.spyOn(t, "getSelection").mockReturnValue("selected-text-1a2b");

    const textarea = document.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
    expect(textarea, "xterm helper textarea must exist").not.toBeNull();

    await act(async () => {
      textarea.dispatchEvent(
        new KeyboardEvent("keydown", {
          code: "KeyC",
          key: "C",
          ctrlKey: true,
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
      await Promise.resolve();
    });

    await waitFor(() => expect(writeText).toHaveBeenCalledWith("selected-text-1a2b"));
    // The chord must NOT reach the PTY (no ^C byte written for it).
    const writes = ipc.callsTo("pty_write").flatMap((w) => w.args.data as number[]);
    expect(writes).not.toContain(0x03); // ETX / Ctrl+C
  });

  it("lets plain Ctrl+C through to the PTY as SIGINT (^C byte)", async () => {
    let term: XTerm | null = null;
    render(<Terminal onInstance={(t) => (term = t ?? term)} />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    // The cleanest way to assert the SIGINT path is untouched: xterm's own input
    // path still produces the ^C byte. `input("\x03")` exercises onData → pty_write.
    act(() => {
      (term as unknown as XTerm).input("\x03", true);
    });

    await waitFor(() => {
      const writes = ipc.callsTo("pty_write").flatMap((w) => w.args.data as number[]);
      expect(writes).toContain(0x03);
    });
  });

  it("writes the Shift+Enter newline sequence to the PTY (ESC+CR, not a bare \\r)", async () => {
    let term: XTerm | null = null;
    render(<Terminal onInstance={(t) => (term = t ?? term)} />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    const textarea = document.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
    expect(textarea, "xterm helper textarea must exist").not.toBeNull();

    const event = new KeyboardEvent("keydown", {
      code: "Enter",
      key: "Enter",
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    });

    await act(async () => {
      textarea.dispatchEvent(event);
      await Promise.resolve();
    });

    // The chord is "handled": xterm's default `\r` is suppressed and the ESC+CR
    // newline sequence is written to the PTY instead.
    await waitFor(() => {
      const writes = ipc.callsTo("pty_write").flatMap((w) => w.args.data as number[]);
      expect(writes).toEqual([0x1b, 0x0d]); // ESC + CR
    });
    // The DOM event must be cancelled so xterm's native `\r` path can't also fire.
    expect(event.defaultPrevented).toBe(true);
    // Exactly the 2-byte sequence reached the PTY — no extra bare `\r` (0x0d alone)
    // from a double-send, and no submit.
    const writes = ipc.callsTo("pty_write").flatMap((w) => w.args.data as number[]);
    expect(writes).toEqual([0x1b, 0x0d]);
  });

  it("lets plain Enter (no Shift) through to the PTY as a submit (\\r)", async () => {
    let term: XTerm | null = null;
    render(<Terminal onInstance={(t) => (term = t ?? term)} />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    // Plain Enter must be untouched: xterm's own input path produces the bare `\r`.
    // `input("\r")` exercises onData → pty_write the same way a real Enter would,
    // confirming the Shift+Enter intercept does NOT hijack a modifier-less Enter.
    act(() => {
      (term as unknown as XTerm).input("\r", true);
    });

    await waitFor(() => {
      const writes = ipc.callsTo("pty_write").flatMap((w) => w.args.data as number[]);
      expect(writes).toContain(0x0d); // CR — the normal submit
    });
    // And no stray ESC (0x1b) sneaks in — plain Enter is NOT the newline chord.
    const writes = ipc.callsTo("pty_write").flatMap((w) => w.args.data as number[]);
    expect(writes).not.toContain(0x1b);
  });

  it("pastes the clipboard into the terminal on Ctrl+Shift+V", async () => {
    const readText = vi.fn().mockResolvedValue("pasted-2c3d");
    vi.stubGlobal("navigator", { clipboard: { readText } });

    let term: XTerm | null = null;
    render(<Terminal onInstance={(t) => (term = t ?? term)} />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    const t = term as unknown as XTerm;
    const pasteSpy = vi.spyOn(t, "paste");

    const textarea = document.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
    await act(async () => {
      textarea.dispatchEvent(
        new KeyboardEvent("keydown", {
          code: "KeyV",
          key: "V",
          ctrlKey: true,
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
      await Promise.resolve();
    });

    await waitFor(() => expect(pasteSpy).toHaveBeenCalledWith("pasted-2c3d"));
  });

  it("gates the reveal on the first post-activation reconcile (#33 anti-flash)", async () => {
    // The inner xterm container (the element xterm opens into) must be kept
    // visually HIDDEN from the moment the pane is active until its first
    // post-activation geometry reconcile has run — so the stale-metrics first
    // frame ("t e s t" spacing) is never shown. It carries `transition-opacity`
    // and toggles `opacity-0` → `opacity-100`.
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(480);
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(300);

    const { container } = render(<Terminal active />);

    // SYNCHRONOUSLY after the first render — before any rAF has fired — the
    // reveal-gated inner container is HIDDEN: it is active but not yet reconciled,
    // so the stale-metrics first frame would be painted here, invisibly.
    const innerBefore = container.querySelector(".transition-opacity");
    expect(innerBefore, "reveal-gated inner xterm container must exist").not.toBeNull();
    expect(innerBefore?.className).toContain("opacity-0");
    expect(innerBefore?.className).not.toContain("opacity-100");

    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    // Flush the activation reconcile rAF (which sets reconciledSinceActivation).
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => r(null)));
      await new Promise((r) => requestAnimationFrame(() => r(null)));
    });

    // Once reconciled, the container is revealed — the user sees the correct frame.
    await waitFor(() => {
      const inner = container.querySelector(".transition-opacity");
      expect(inner?.className).toContain("opacity-100");
      expect(inner?.className).not.toContain("opacity-0");
    });
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
