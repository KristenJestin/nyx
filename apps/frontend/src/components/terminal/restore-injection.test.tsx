import { act, render, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import type { Terminal as XTerm } from "@xterm/xterm";
import { beforeEach, describe, expect, it } from "vitest";

import { Terminal } from "./terminal";
import { RESTORE_SEPARATOR_LABEL } from "./dead-history";

/**
 * Record every IPC call so we can prove the restored history is written to xterm
 * but NEVER to the PTY (`pty_write`). Hands out a deterministic pty id so we can
 * push live output to the freshly-spawned shell.
 */
interface IpcSpy {
  ptyWrites: string[];
  spawnCwds: (string | undefined)[];
}

function installIpc(): IpcSpy {
  const spy: IpcSpy = { ptyWrites: [], spawnCwds: [] };
  let nextPtyId = 200;
  const decoder = new TextDecoder();
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "pty_spawn":
          spy.spawnCwds.push(a.cwd as string | undefined);
          return nextPtyId++;
        case "pty_write": {
          const data = a.data as number[];
          spy.ptyWrites.push(decoder.decode(Uint8Array.from(data)));
          return null;
        }
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  return spy;
}

describe("<Terminal> restore injection (dead history)", () => {
  beforeEach(() => {
    installIpc();
  });

  it("writes the prior scrollback + separator into the buffer as dead history", async () => {
    let term: XTerm | null = null;
    render(
      <Terminal
        recordId={"1"}
        cwd="/projectA"
        deadHistory={"PREVIOUS_OUTPUT_42\r\nmore old output"}
        onInstance={(t) => {
          term = t;
        }}
      />,
    );

    await waitFor(() => expect(term).not.toBeNull());

    await waitFor(() => {
      const buf = readBuffer(term);
      // The old session's output is restoredâ€¦
      expect(buf).toContain("PREVIOUS_OUTPUT_42");
      // â€¦and the separator distinguishes it from the new session.
      expect(buf).toContain(RESTORE_SEPARATOR_LABEL);
    });
  });

  it("never writes the restored history to the PTY (history is read-only)", async () => {
    const spy = installIpc();
    let term: XTerm | null = null;
    render(
      <Terminal
        recordId={"2"}
        cwd="/projectA"
        deadHistory={"OLD_COMMAND_OUTPUT_zz"}
        onInstance={(t) => {
          term = t;
        }}
      />,
    );

    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(spy.spawnCwds).toHaveLength(1));
    // Give any (erroneous) replay-to-PTY a chance to happen.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });

    // The shell must never receive the old history on its stdin â€” that would
    // re-run the previous commands. Nothing written to the PTY contains it.
    for (const w of spy.ptyWrites) {
      expect(w).not.toContain("OLD_COMMAND_OUTPUT_zz");
    }
    expect(spy.ptyWrites.join("")).not.toContain(RESTORE_SEPARATOR_LABEL);
  });

  it("live PTY output is written BELOW the restored history", async () => {
    const spy = installIpc();
    let term: XTerm | null = null;
    render(
      <Terminal
        recordId={"3"}
        cwd="/projectA"
        deadHistory={"HISTORY_HEAD_abc"}
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(spy.spawnCwds).toHaveLength(1));

    const liveText = "LIVE_PROMPT_xyz";
    const bytes = Array.from(new TextEncoder().encode(liveText));
    // Spawn order â†’ pty id 200.
    await act(async () => {
      await emit("pty://output", { id: 200, bytes });
    });

    await waitFor(() => {
      const buf = readBuffer(term);
      const histAt = buf.indexOf("HISTORY_HEAD_abc");
      const sepAt = buf.indexOf(RESTORE_SEPARATOR_LABEL);
      const liveAt = buf.indexOf(liveText);
      expect(histAt).toBeGreaterThanOrEqual(0);
      expect(liveAt).toBeGreaterThanOrEqual(0);
      // history < separator < live output, top to bottom.
      expect(histAt).toBeLessThan(sepAt);
      expect(sepAt).toBeLessThan(liveAt);
    });
  });

  it("a terminal with no prior scrollback shows no separator", async () => {
    let term: XTerm | null = null;
    render(
      <Terminal
        recordId={"4"}
        cwd="/fresh"
        deadHistory=""
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(readBuffer(term)).not.toContain(RESTORE_SEPARATOR_LABEL);
  });
});

/** Read the whole xterm buffer (screen + scrollback) as a newline-joined string. */
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
