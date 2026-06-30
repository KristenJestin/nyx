import { render, screen, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import { act } from "react";
import { describe, expect, it, vi } from "vitest";

import { TerminalItem } from "./terminal-item";
import { AgentSessionsProvider } from "./use-agent-sessions";
import type { TerminalRecord } from "./use-terminals";

/**
 * The DOT of an agent-hosting terminal reflects the AGENT'S runtime ACTIVITY (the live
 * dot), NOT the PTY `busy` bit (the feature): `working` → running, a finished turn →
 * focus-aware "response ready" green dot, idle → nothing. Driven by the
 * `agent_activity_snapshot` read + the `agent-sessions://changed` event, shared via
 * `<AgentSessionsProvider>`. We assert through the REAL row (`<TerminalItem>` →
 * `<TerminalItemBody>`) by the `<TerminalStateBadge>`'s `role="status"` a11y name.
 */

/**
 * The badge COLOUR token lives on the shared `<CrossfadeFill>` layer (an `aria-hidden`
 * absolute child of the `role="status"` badge), not the badge host. Aggregate the
 * layers (two coexist briefly mid-cross-fade) so a token assertion is order-agnostic.
 */
function fillClasses(host: HTMLElement): string {
  return Array.from(host.querySelectorAll("[aria-hidden]"))
    .map((el) => el.className)
    .join(" ");
}

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
    // CRUCIAL: the PTY is busy for the whole Claude session. The dot must NOT follow
    // this for an agent terminal — only the agent activity decides.
    busy: true,
  };
}

interface ActiveSession {
  terminal_id: string;
  agent_kind: string;
}
interface ActivityRow {
  terminal_id: string;
  activity: "working" | "waiting" | "idle";
  ready_unread: boolean;
  /** #35 — the RED analogue: the last turn ended on an API error (optional, like the real snapshot). */
  error_unread?: boolean;
  /** #18b — the per-session stale-plugin badge flag (optional, like the real snapshot). */
  plugin_outdated?: boolean;
}

/**
 * Mock the backend: `agent_active_sessions` + `agent_activity_snapshot` return whatever
 * `state` holds (so a test flips them then emits `agent-sessions://changed` to drive a
 * live update). `terminal_info` is inert; `agent_mark_ready_read` records the focus clear.
 */
function mockBackend(state: {
  sessions: ActiveSession[];
  activity: ActivityRow[];
  marked?: string[];
}) {
  state.marked = state.marked ?? [];
  mockIPC(
    (cmd, args) => {
      switch (cmd) {
        case "agent_active_sessions":
          return [...state.sessions];
        case "agent_activity_snapshot":
          return [...state.activity];
        case "agent_mark_ready_read":
          state.marked!.push((args as { terminalId: string }).terminalId);
          return null;
        case "terminal_info":
          return { cwd: null, foreground: null };
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}

function renderRow(active = false) {
  return render(
    <AgentSessionsProvider>
      <ul>
        <TerminalItem
          record={row(1, "/work")}
          index={0}
          active={active}
          ptyId={null}
          onSelect={vi.fn()}
          onClose={vi.fn()}
        />
      </ul>
    </AgentSessionsProvider>,
  );
}

describe("sidebar terminal row — agent ACTIVITY dot (the feature)", () => {
  it("an agent terminal with NO activity shows NO running dot, even though the PTY is busy", async () => {
    // Session live (icon swaps) but activity idle → the PTY `busy=true` must be ignored.
    mockBackend({
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [],
    });
    renderRow();
    await waitFor(() => expect(screen.getByTitle("Claude Code")).toBeInTheDocument());
    // No running dot despite busy=true — the agent override suppresses the PTY signal.
    await new Promise((r) => setTimeout(r, 150));
    expect(screen.queryByRole("status", { name: /terminal status: running/i })).toBeNull();
  });

  it("shows the RUNNING dot while the agent is WORKING", async () => {
    mockBackend({
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [{ terminal_id: "1", activity: "working", ready_unread: false }],
    });
    renderRow();
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /terminal status: running/i })).toBeInTheDocument(),
    );
  });

  it("shows the YELLOW 'waiting' dot while the agent is WAITING on the user", async () => {
    // `waiting` (an AskUserQuestion / permission prompt) → the yellow (--warning) dot, a
    // LIVE state shown distinct from the blue running dot.
    mockBackend({
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [{ terminal_id: "1", activity: "waiting", ready_unread: false }],
    });
    renderRow();
    const badge = await screen.findByRole("status", { name: /terminal status: waiting/i });
    expect(fillClasses(badge)).toContain("bg-warning");
    // It is NOT the blue running dot. (The row briefly shows the transient PTY-busy running
    // badge before the agent context resolves to `waiting`; that badge then EXITS via
    // animation, so wait for it to finish leaving rather than asserting an instant cut.)
    await waitFor(() =>
      expect(
        screen.queryByRole("status", { name: /terminal status: running/i }),
      ).not.toBeInTheDocument(),
    );
  });

  it("a finished turn shows the GREEN 'response ready' dot on an INACTIVE terminal", async () => {
    mockBackend({
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [{ terminal_id: "1", activity: "idle", ready_unread: true }],
    });
    renderRow(false);
    // success badge = the green "ready" notification (shown only while unread + not active).
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /terminal status: success/i })).toBeInTheDocument(),
    );
  });

  // #35 — a turn that ended on an API ERROR (`StopFailure`) shows a RED dot, behaving like
  // the green "ready" but red: it surfaces on an INACTIVE terminal (the `error` run-state =
  // --destructive), and red takes PRIORITY over green if both were somehow set.
  it("a turn that ended in an API ERROR shows the RED dot on an INACTIVE terminal", async () => {
    mockBackend({
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [{ terminal_id: "1", activity: "idle", ready_unread: false, error_unread: true }],
    });
    renderRow(false);
    // error badge = the red errored-turn notification (shown only while unread + not active).
    const badge = await screen.findByRole("status", { name: /terminal status: error/i });
    expect(fillClasses(badge)).toContain("bg-destructive");
  });

  it("does NOT show the red dot on the ACTIVE terminal, and marks it read (focus-aware)", async () => {
    const state = {
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [
        { terminal_id: "1", activity: "idle" as const, ready_unread: false, error_unread: true },
      ],
      marked: [] as string[],
    };
    mockBackend(state);
    renderRow(true); // the terminal is ACTIVE (being viewed).
    // The red dot is suppressed while active (a notification only matters when not viewed).
    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.queryByRole("status", { name: /terminal status: error/i })).toBeNull();
    // And the active-settle effect acknowledged the errored turn (focus-aware clear).
    await waitFor(() => expect(state.marked).toContain("1"));
  });

  it("does NOT show the green dot on the ACTIVE terminal, and marks it read (focus-aware)", async () => {
    const state = {
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [{ terminal_id: "1", activity: "idle" as const, ready_unread: true }],
      marked: [] as string[],
    };
    mockBackend(state);
    renderRow(true); // the terminal is ACTIVE (being viewed).
    // The green dot is suppressed while active (a notification only matters when not viewed).
    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.queryByRole("status", { name: /terminal status: success/i })).toBeNull();
    // And the active-settle effect acknowledged the ready (focus-aware clear).
    await waitFor(() => expect(state.marked).toContain("1"));
  });

  it("swaps WORKING → ready LIVE on the activity change event", async () => {
    const state: { sessions: ActiveSession[]; activity: ActivityRow[]; marked: string[] } = {
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [{ terminal_id: "1", activity: "working", ready_unread: false }],
      marked: [],
    };
    mockBackend(state);
    renderRow(false);
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /terminal status: running/i })).toBeInTheDocument(),
    );
    // Turn finishes: activity → idle + ready; the change event makes the row re-pull.
    state.activity = [{ terminal_id: "1", activity: "idle", ready_unread: true }];
    await act(async () => {
      await emit("agent-sessions://changed");
    });
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /terminal status: success/i })).toBeInTheDocument(),
    );
    // The running badge leaves via an EXIT animation (no longer an instant cut) — wait it out.
    await waitFor(() =>
      expect(
        screen.queryByRole("status", { name: /terminal status: running/i }),
      ).not.toBeInTheDocument(),
    );
  });

  // #18b — the per-session STALE-PLUGIN affordance.
  it("shows the STALE-PLUGIN ⚠ affordance when the session's plugin is outdated", async () => {
    mockBackend({
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [
        { terminal_id: "1", activity: "idle", ready_unread: false, plugin_outdated: true },
      ],
    });
    renderRow();
    // The muted warning carries the restart invitation as its accessible name.
    await waitFor(() =>
      expect(
        screen.getByRole("img", { name: /plugin nyx périmé — redémarre la session/i }),
      ).toBeInTheDocument(),
    );
  });

  it("shows NO stale-plugin affordance when the plugin is current", async () => {
    mockBackend({
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [
        { terminal_id: "1", activity: "working", ready_unread: false, plugin_outdated: false },
      ],
    });
    renderRow();
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /terminal status: running/i })).toBeInTheDocument(),
    );
    expect(screen.queryByRole("img", { name: /plugin nyx périmé/i })).not.toBeInTheDocument();
  });

  it("surfaces the stale badge LIVE when the change event reports it", async () => {
    const state: { sessions: ActiveSession[]; activity: ActivityRow[]; marked: string[] } = {
      sessions: [{ terminal_id: "1", agent_kind: "claude_code" }],
      activity: [{ terminal_id: "1", activity: "working", ready_unread: false }],
      marked: [],
    };
    mockBackend(state);
    renderRow();
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /terminal status: running/i })).toBeInTheDocument(),
    );
    // No badge yet.
    expect(screen.queryByRole("img", { name: /plugin nyx périmé/i })).not.toBeInTheDocument();
    // The snapshot now reports the session is stale → the change event makes the row re-pull.
    state.activity = [
      { terminal_id: "1", activity: "working", ready_unread: false, plugin_outdated: true },
    ];
    await act(async () => {
      await emit("agent-sessions://changed");
    });
    await waitFor(() =>
      expect(
        screen.getByRole("img", { name: /plugin nyx périmé — redémarre la session/i }),
      ).toBeInTheDocument(),
    );
  });
});
