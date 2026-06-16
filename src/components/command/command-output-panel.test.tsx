import { act, render, waitFor } from "@testing-library/react";
import type { Terminal as XTerm } from "@xterm/xterm";
import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import { beforeEach, describe, expect, it } from "vitest";

import { CommandOutputPanel } from "./command-output-panel";

/**
 * Spy on every IPC call so we can prove the read-only contract: the panel
 * rehydrates via `command_output` and NEVER calls any write/stdin/resize command.
 * `command_output` returns a scripted history (cold rehydration) per instance.
 */
interface IpcSpy {
  calls: { cmd: string; args: Record<string, unknown> }[];
  callsTo: (cmd: string) => { cmd: string; args: Record<string, unknown> }[];
}

function installIpc(history: Record<string, string> = {}): IpcSpy {
  const spy: IpcSpy = {
    calls: [],
    callsTo: (cmd) => spy.calls.filter((c) => c.cmd === cmd),
  };
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      spy.calls.push({ cmd, args: a });
      if (cmd === "command_output") {
        const id = a.instanceId as string;
        return history[id] ?? "";
      }
      return null;
    },
    { shouldMockEvents: true },
  );
  return spy;
}

/** Read the xterm buffer as plain text (trimmed-right per line). */
function readBuffer(term: XTerm | null): string {
  if (!term) return "";
  const buf = term.buffer.active;
  let out = "";
  for (let i = 0; i < buf.length; i++) {
    const line = buf.getLine(i);
    if (line) out += line.translateToString(true) + "\n";
  }
  return out;
}

/** Read xterm's first row CELL colors so we can assert ANSI styling is applied. */
function firstStyledCell(term: XTerm | null): { fg: number; isFgRGB: boolean } | null {
  if (!term) return null;
  const buf = term.buffer.active;
  for (let row = 0; row < buf.length; row++) {
    const line = buf.getLine(row);
    if (!line) continue;
    for (let col = 0; col < term.cols; col++) {
      const cell = line.getCell(col);
      if (!cell) continue;
      const ch = cell.getChars();
      if (ch && ch.trim() !== "" && (cell.isFgPalette() || cell.isFgRGB())) {
        return { fg: cell.getFgColor(), isFgRGB: cell.isFgRGB() };
      }
    }
  }
  return null;
}

describe("<CommandOutputPanel> (read-only command output, T8)", () => {
  beforeEach(() => {
    installIpc();
  });

  it("rehydrates the persisted history via command_output on open", async () => {
    const spy = installIpc({ inst1: "RESTORED_HISTORY_42\r\n" });
    let term: XTerm | null = null;
    render(
      <CommandOutputPanel
        instanceId="inst1"
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());

    // command_output was invoked for this instance (cold rehydration path).
    await waitFor(() => {
      const calls = spy.callsTo("command_output");
      expect(calls.length).toBeGreaterThan(0);
      expect(calls[0].args.instanceId).toBe("inst1");
    });
    // The persisted scrollback is written into the buffer.
    await waitFor(() => expect(readBuffer(term)).toContain("RESTORED_HISTORY_42"));
  });

  it("renders streamed command://output with its ANSI colors", async () => {
    installIpc({ inst2: "" });
    let term: XTerm | null = null;
    render(
      <CommandOutputPanel
        instanceId="inst2"
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());
    // Let the rehydrate round-trip resolve so live output writes (not buffers).
    await waitFor(() => expect(readBuffer(term)).toBeDefined());

    // A red-foreground "ERR" via SGR 31, then reset. Filtered by instanceId.
    const ansi = "\x1b[31mERR_TOKEN\x1b[0m\r\n";
    const bytes = Array.from(new TextEncoder().encode(ansi));
    await act(async () => {
      await emit("command://output", { instanceId: "inst2", bytes });
    });

    await waitFor(() => expect(readBuffer(term)).toContain("ERR_TOKEN"));
    // The cell carries a non-default foreground color → ANSI was applied, not
    // stripped (palette index 1 = red, or an RGB value).
    await waitFor(() => {
      const cell = firstStyledCell(term);
      expect(cell).not.toBeNull();
    });
  });

  it("only writes output whose instanceId matches (filtered stream)", async () => {
    installIpc({ inst3: "" });
    let term: XTerm | null = null;
    render(
      <CommandOutputPanel
        instanceId="inst3"
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(readBuffer(term)).toBeDefined());

    const mine = Array.from(new TextEncoder().encode("MINE_3\r\n"));
    const other = Array.from(new TextEncoder().encode("OTHER_9\r\n"));
    await act(async () => {
      await emit("command://output", { instanceId: "other9", bytes: other });
      await emit("command://output", { instanceId: "inst3", bytes: mine });
    });

    await waitFor(() => expect(readBuffer(term)).toContain("MINE_3"));
    expect(readBuffer(term)).not.toContain("OTHER_9");
  });

  it("clears the panel on a new run (running transition) so output does not pile", async () => {
    // Rehydrate the PREVIOUS run's output, then a `running` state event (a fresh
    // start/relaunch for THIS instance) must wipe the panel so the new run's output
    // does not stack under the old (finding: relaunch piled output).
    installIpc({ inst5: "OLD_RUN_OUTPUT\r\n" });
    let term: XTerm | null = null;
    render(
      <CommandOutputPanel
        instanceId="inst5"
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());
    // The old run's history is shown first.
    await waitFor(() => expect(readBuffer(term)).toContain("OLD_RUN_OUTPUT"));

    // A running transition for this instance = a new run → clear the panel.
    await act(async () => {
      await emit("command://state", { instanceId: "inst5", state: "running", code: null });
    });
    await waitFor(() => expect(readBuffer(term)).not.toContain("OLD_RUN_OUTPUT"));

    // New output streams onto the cleared panel, alone.
    const fresh = Array.from(new TextEncoder().encode("NEW_RUN_OUTPUT\r\n"));
    await act(async () => {
      await emit("command://output", { instanceId: "inst5", bytes: fresh });
    });
    await waitFor(() => expect(readBuffer(term)).toContain("NEW_RUN_OUTPUT"));
    expect(readBuffer(term)).not.toContain("OLD_RUN_OUTPUT");
  });

  it("ignores a running transition for a DIFFERENT instance (no spurious clear)", async () => {
    installIpc({ inst6: "KEEP_THIS_OUTPUT\r\n" });
    let term: XTerm | null = null;
    render(
      <CommandOutputPanel
        instanceId="inst6"
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(readBuffer(term)).toContain("KEEP_THIS_OUTPUT"));

    // A running transition for ANOTHER instance must NOT clear this panel.
    await act(async () => {
      await emit("command://state", { instanceId: "other99", state: "running", code: null });
    });
    // Give the listener a tick; the panel must still hold its output.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(readBuffer(term)).toContain("KEEP_THIS_OUTPUT");
  });

  it("NEVER sends a keystroke/stdin/resize to the backend (read-only strict)", async () => {
    const spy = installIpc({ inst4: "HELLO\r\n" });
    let term: XTerm | null = null;
    render(
      <CommandOutputPanel
        instanceId="inst4"
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(spy.callsTo("command_output").length).toBeGreaterThan(0));

    // Simulate the user "typing" into the panel: with the read-only wiring there
    // is NO onData handler, so even an explicit write triggers no IPC. We also
    // drive xterm's input directly to be sure no data path exists.
    await act(async () => {
      // xterm exposes `input()` which would fire onData IF one were wired.
      (term as unknown as { input: (d: string) => void }).input("echo pwned\r");
      await new Promise((r) => setTimeout(r, 0));
    });

    // No write/stdin/resize command was ever invoked — the only command-* IPC the
    // panel makes is the rehydration read.
    expect(spy.callsTo("pty_write")).toHaveLength(0);
    expect(spy.callsTo("command_write")).toHaveLength(0);
    expect(spy.callsTo("pty_resize")).toHaveLength(0);
    expect(spy.callsTo("command_resize")).toHaveLength(0);
    // Every IPC call the panel makes is the read-only rehydration.
    for (const call of spy.calls) {
      expect(call.cmd).toBe("command_output");
    }
  });
});
