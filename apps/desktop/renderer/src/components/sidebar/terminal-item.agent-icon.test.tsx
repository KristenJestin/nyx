import { render, screen, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import { act } from "react";
import { describe, expect, it, vi } from "vitest";

import { TerminalItem, dropAgentProgramToken } from "./terminal-item";
import { AgentSessionsProvider } from "./use-agent-sessions";
import type { TerminalRecord } from "./use-terminals";

/**
 * Provider-aware sidebar icon (finding #55): a terminal row hosting a LIVE agent session
 * shows the agent's brand logo (Claude) instead of the generic terminal glyph, reverting
 * when the session ends. The active-session map comes from the `agent_active_sessions`
 * command + the `agent-sessions://changed` event, shared via `<AgentSessionsProvider>`.
 *
 * We assert through the REAL row (`<TerminalItem>` → `<TerminalItemBody>`): the lead glyph
 * span is `title`-labelled with the provider label ("Claude Code") only when the agent
 * icon is shown — so `getByTitle` is a faithful proxy for "the logo swapped in".
 */

function row(id: number, cwd: string): TerminalRecord {
  return {
    id: String(id),
    cwd,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    exec_state: "idle",
    exec_state_unread: false,
  };
}

interface ActiveSession {
  terminal_id: string;
  agent_kind: string;
}

/**
 * Mock the backend: `agent_active_sessions` returns whatever `state.sessions` holds (so a
 * test can flip it and emit `agent-sessions://changed` to drive a live swap); other reads
 * the row makes (`terminal_info`) are stubbed inert. `shouldMockEvents` enables emit/listen.
 */
function mockBackend(state: { sessions: ActiveSession[] }) {
  mockIPC(
    (cmd) => {
      switch (cmd) {
        case "agent_active_sessions":
          return [...state.sessions];
        case "terminal_info":
          return { cwd: null, foreground: null };
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}

function renderRow() {
  return render(
    <AgentSessionsProvider>
      <ul>
        <TerminalItem
          record={row(1, "/work")}
          index={0}
          active={false}
          ptyId={null}
          onSelect={vi.fn()}
          onClose={vi.fn()}
        />
      </ul>
    </AgentSessionsProvider>,
  );
}

describe("sidebar terminal row — provider-aware icon (#55)", () => {
  it("shows the generic terminal icon when no agent session is active", async () => {
    mockBackend({ sessions: [] });
    renderRow();

    // Give the initial `agent_active_sessions` pull a tick to resolve.
    await act(async () => {
      await Promise.resolve();
    });
    // No provider label → the agent logo is NOT shown (generic terminal icon stands).
    expect(screen.queryByTitle("Claude Code")).not.toBeInTheDocument();
  });

  it("shows the Claude logo when the terminal hosts an active claude_code session", async () => {
    mockBackend({ sessions: [{ terminal_id: "1", agent_kind: "claude_code" }] });
    renderRow();

    await waitFor(() => {
      expect(screen.getByTitle("Claude Code")).toBeInTheDocument();
    });
  });

  it("swaps the icon LIVE on session start, then reverts on session end", async () => {
    const state = { sessions: [] as ActiveSession[] };
    mockBackend(state);
    renderRow();

    // Initially generic (no session).
    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.queryByTitle("Claude Code")).not.toBeInTheDocument();

    // SessionStart: the backend now has an active session; the change event makes the
    // row re-pull and swap to the Claude logo.
    state.sessions = [{ terminal_id: "1", agent_kind: "claude_code" }];
    await act(async () => {
      await emit("agent-sessions://changed");
    });
    await waitFor(() => expect(screen.getByTitle("Claude Code")).toBeInTheDocument());

    // SessionEnd: the session drops out; the row reverts to the terminal icon.
    state.sessions = [];
    await act(async () => {
      await emit("agent-sessions://changed");
    });
    await waitFor(() => expect(screen.queryByTitle("Claude Code")).not.toBeInTheDocument());
  });

  it("DECLUTTERS the redundant agent program text (FEEDBACK #29)", async () => {
    // The lead logo already says "Claude", so the agent foreground program (`claude`) is
    // redundant text. It leaks into BOTH the auto-name (`work · claude`) and the muted
    // shell suffix (`· claude`) — the screenshot's `work · claude · claude`. With an active
    // agent session the row must read just `work`, with NO trailing `· claude` anywhere.
    mockIPC(
      (cmd) => {
        switch (cmd) {
          case "agent_active_sessions":
            return [{ terminal_id: "1", agent_kind: "claude_code" }];
          case "agent_activity_snapshot":
            return [];
          case "terminal_info":
            return { cwd: "/home/x/work", foreground: "claude" };
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    render(
      <AgentSessionsProvider>
        <ul>
          <TerminalItem
            record={row(1, "/home/x/work")}
            index={0}
            active={false}
            ptyId={50}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </AgentSessionsProvider>,
    );

    // The logo swapped in (agent session live) …
    await waitFor(() => expect(screen.getByTitle("Claude Code")).toBeInTheDocument());
    // … the name is the bare workspace, NOT `work · claude` …
    await waitFor(() => expect(screen.getByText("work")).toBeInTheDocument());
    expect(screen.queryByText(/· claude/)).not.toBeInTheDocument();
    expect(screen.queryByText("work · claude")).not.toBeInTheDocument();
  });

  it("keeps the Claude logo when a re-pull's IPC read FAILS (no transient blink)", async () => {
    // The "icône qui saute" resilience: a transient IPC failure must NOT collapse into
    // "no session" and drop the logo. The hook keeps the last good icon and recovers on
    // the next good event.
    const state = { sessions: [{ terminal_id: "1", agent_kind: "claude_code" }], fail: false };
    mockIPC(
      (cmd) => {
        switch (cmd) {
          case "agent_active_sessions":
            if (state.fail) throw new Error("transient IPC failure");
            return [...state.sessions];
          case "agent_activity_snapshot":
            if (state.fail) throw new Error("transient IPC failure");
            return [];
          case "terminal_info":
            return { cwd: null, foreground: null };
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );
    renderRow();

    // The logo is shown after the initial good pull.
    await waitFor(() => expect(screen.getByTitle("Claude Code")).toBeInTheDocument());

    // A change tick whose reads FAIL must keep the logo (not blink to the terminal icon).
    state.fail = true;
    await act(async () => {
      await emit("agent-sessions://changed");
    });
    // Give the failed pull time to settle; the logo must still be present.
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.getByTitle("Claude Code")).toBeInTheDocument();

    // A subsequent GOOD event still resolves normally.
    state.fail = false;
    await act(async () => {
      await emit("agent-sessions://changed");
    });
    await waitFor(() => expect(screen.getByTitle("Claude Code")).toBeInTheDocument());
  });
});

describe("dropAgentProgramToken (FEEDBACK #29)", () => {
  it("strips a trailing ` · <program>` that matches the agent foreground program", () => {
    expect(dropAgentProgramToken("nyx-v2 · claude", "claude")).toBe("nyx-v2");
    expect(dropAgentProgramToken("work · claude", "claude")).toBe("work");
  });

  it("leaves the label untouched when the trailing token is NOT the agent program", () => {
    // A real cwd basename / a different program must survive — the user still tells rows apart.
    expect(dropAgentProgramToken("projetA · htop", "claude")).toBe("projetA · htop");
    expect(dropAgentProgramToken("claude", "claude")).toBe("claude");
    expect(dropAgentProgramToken("my-claude", "claude")).toBe("my-claude");
  });

  it("is a no-op when there is no auto label or no program (non-agent rows)", () => {
    expect(dropAgentProgramToken(null, "claude")).toBeNull();
    expect(dropAgentProgramToken("nyx-v2 · claude", null)).toBe("nyx-v2 · claude");
    expect(dropAgentProgramToken(null, null)).toBeNull();
  });
});
