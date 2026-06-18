import { render, screen, waitFor } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import { act } from "react";
import { describe, expect, it, vi } from "vitest";

import { TerminalItem } from "./terminal-item";
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
});
