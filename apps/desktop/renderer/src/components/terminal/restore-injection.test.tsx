import { act, render, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import type { Terminal as XTerm } from "@xterm/xterm";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { Terminal } from "./terminal";
import { __resetTerminalStore } from "./terminal-store";
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

  afterEach(() => {
    // Drop module-level store instances so they never leak across tests.
    __resetTerminalStore();
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

describe("<Terminal> screen re-hydration (a pane taking over a running shell)", () => {
  afterEach(() => {
    __resetTerminalStore();
  });

  /**
   * A shell can be running long before any pane exists: an agent opens a terminal over MCP and
   * drives it while its project is not the one on screen. When the human finally opens that
   * project, the mount ATTACHES to that shell — and used to show a BLANK pane, because
   * everything it printed before the pane existed lives in the backend and the renderer never
   * asked for it.
   */
  it("repaints the screen of the shell it takes over", async () => {
    const calls: string[] = [];
    const ptyWrites: string[] = [];
    const decoder = new TextDecoder();
    mockIPC(
      (cmd, args) => {
        calls.push(cmd);
        const a = (args ?? {}) as Record<string, unknown>;
        switch (cmd) {
          case "terminal_screen_rehydration":
            return { bytes: Array.from(new TextEncoder().encode("SHELL_WAS_HERE_77")) };
          case "pty_spawn":
            return 300;
          case "pty_write":
            ptyWrites.push(decoder.decode(Uint8Array.from((a.data as number[]) ?? [])));
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );

    let term: XTerm | null = null;
    render(
      <Terminal
        recordId="rehydrate-1"
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(readBuffer(term)).toContain("SHELL_WAS_HERE_77"));

    // The re-hydration is asked for BEFORE the attach: the backend streams a terminal's output
    // only to an attached view, so this ordering is what keeps the repaint and the live stream
    // from doubling each other.
    expect(calls).toContain("terminal_screen_rehydration");
    expect(calls.indexOf("terminal_screen_rehydration")).toBeLessThan(calls.indexOf("pty_spawn"));
    // A repaint is for the SCREEN — it must never be typed into the shell.
    expect(ptyWrites.join("")).not.toContain("SHELL_WAS_HERE_77");
  });

  /** Nothing to restore (a brand-new terminal): nothing written, and the spawn still happens. */
  it("writes nothing when the backend has no screen to restore", async () => {
    const calls: string[] = [];
    mockIPC(
      (cmd) => {
        calls.push(cmd);
        if (cmd === "terminal_screen_rehydration") return { bytes: [] };
        if (cmd === "pty_spawn") return 301;
        return null;
      },
      { shouldMockEvents: true },
    );

    let term: XTerm | null = null;
    render(
      <Terminal
        recordId="rehydrate-2"
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(calls).toContain("pty_spawn"));
    expect(readBuffer(term).trim()).toBe("");
  });
});

/**
 * GATE CDP « Scope Système », the second defect it caught: a REPLAY must not talk back.
 *
 * Dead history and the screen re-hydration are RECORDINGS — bytes some past shell printed. Among
 * them can be a terminal QUERY: a shell probes its terminal at startup with `ESC[6n` (DSR cursor
 * position), and on Windows that probe is literally the first thing in a view-less terminal's
 * scrollback. xterm ANSWERS any query it parses, through the very `onData` that carries the
 * user's keystrokes — so replaying that history typed `ESC[1;1R` into the LIVE shell, which was
 * long past its own probe and sitting at a prompt. readline swallowed the CSI and left a bare
 * `R` on the input line, so the human's next command came out as `Rcd "…"` → `command not found`.
 * Observed in the real app on a Système terminal.
 *
 * It was INTERMITTENT for a mechanical reason worth pinning: `term.write` parses a small payload
 * synchronously — before the spawn publishes a pty id, which mutes the write on its own — and a
 * large one across several turns of the event loop, straddling the spawn. So both tests below
 * use a payload big enough to force the chunked parse; that is the case that used to leak.
 */
describe("<Terminal> a replayed recording never answers back to the shell", () => {
  afterEach(() => {
    __resetTerminalStore();
  });

  /**
   * A DSR cursor-position reply, `ESC[<row>;<col>R` — what xterm sends when it parses `ESC[6n`.
   * Built from a string so the ESC stays out of a regex literal (`no-control-regex`).
   */
  const ESC = "\x1b";
  const DSR_REPLY = new RegExp(`${ESC}\\[\\d+;\\d+R`);
  /** Big enough that xterm cannot parse it in one turn; the query rides at the very end. */
  const bulky = (query: string) => "filler line, replayed\r\n".repeat(12_000) + query;

  it("does not answer a query buried in DEAD HISTORY", async () => {
    const spy = installIpc();
    let term: XTerm | null = null;
    render(
      <Terminal
        recordId="replay-dead-history"
        cwd="/projectA"
        deadHistory={bulky("\x1b[6n")}
        onInstance={(t) => {
          term = t;
        }}
      />,
    );

    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(spy.spawnCwds).toHaveLength(1));
    // Let the chunked parse run to its end, well past the spawn that published the pty id.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 50));
    });

    expect(spy.ptyWrites.join("")).not.toMatch(DSR_REPLY);
  });

  it("does not answer a query buried in the SCREEN RE-HYDRATION", async () => {
    const ptyWrites: string[] = [];
    const decoder = new TextDecoder();
    const encoder = new TextEncoder();
    let spawned = false;
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        switch (cmd) {
          case "terminal_screen_rehydration":
            return { bytes: Array.from(encoder.encode(bulky("\x1b[6n"))) };
          case "pty_spawn":
            spawned = true;
            return 302;
          case "pty_write":
            ptyWrites.push(decoder.decode(Uint8Array.from((a.data as number[]) ?? [])));
            return null;
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );

    let term: XTerm | null = null;
    render(
      <Terminal
        recordId="replay-rehydration"
        onInstance={(t) => {
          term = t;
        }}
      />,
    );
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(spawned).toBe(true));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 50));
    });

    expect(ptyWrites.join("")).not.toMatch(DSR_REPLY);
  });

  /** …and the mute is RELEASED: once the replay is parsed, real keystrokes reach the shell again. */
  it("still forwards the user's own keystrokes once the replay is done", async () => {
    const spy = installIpc();
    let term: XTerm | null = null;
    render(
      <Terminal
        recordId="replay-then-typing"
        cwd="/projectA"
        deadHistory={bulky("\x1b[6n")}
        onInstance={(t) => {
          term = t;
        }}
      />,
    );

    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(spy.spawnCwds).toHaveLength(1));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 50));
    });

    await act(async () => {
      term?.input("echo hello\r");
    });
    await waitFor(() => expect(spy.ptyWrites.join("")).toContain("echo hello"));
    expect(spy.ptyWrites.join("")).not.toMatch(DSR_REPLY);
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
