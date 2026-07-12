import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { RuntimeSelectionProvider } from "./runtime-selection";
import { ConsumedNotificationsProvider } from "./use-consumed-notifications";
import { WorkspaceRuntimeTree } from "./workspace-runtime-tree";
import type {
  WorkspaceRuntimeSession,
  WorkspaceRuntimeSnapshot,
  WorkspaceRuntimeTerminal,
} from "./use-workspace-runtime";

/**
 * PROVIDER-AGNOSTIC passive adoption (v2.5.5 review T3). We mock `agent-providers` with a GENERIC
 * resolver that returns an identifiable icon for ANY kind it is given (claude_code AND a non-claude
 * `codex`), and `undefined` for an unknown kind — proving the tree renders the icon of WHATEVER
 * kind flows in (hook `sessions[]` OR the passive `detected_agent_kind`) through the SAME resolver,
 * with no `claude` hardcode. This is deliberately a SEPARATE file so the module mock does not touch
 * the other tree tests (which assert against the real Claude provider).
 */
vi.mock("@/components/sidebar/agent-providers", () => ({
  agentProviderFor: (kind?: string | null) => {
    if (!kind) return undefined;
    // A generic map — any kind resolves to its own labelled icon (the "structure ouverte").
    const known: Record<string, string> = { claude_code: "Claude Code", codex: "Codex" };
    const label = known[kind];
    if (!label) return undefined;
    return {
      label,
      icon: (props: { className?: string }) => (
        <svg data-testid={`provider-icon-${kind}`} className={props.className} />
      ),
    };
  },
}));

function terminal(overrides: Partial<WorkspaceRuntimeTerminal> = {}): WorkspaceRuntimeTerminal {
  return {
    id: "t1",
    cwd: "/repo",
    label: "term",
    workspace_id: "ws1",
    status: "alive",
    exec_state: "idle",
    exec_state_unread: false,
    sessions: [],
    ...overrides,
  };
}

function session(kind: string, state = "active"): WorkspaceRuntimeSession {
  return {
    id: "sess1",
    terminal_id: "t1",
    workspace_id: "ws1",
    agent_kind: kind,
    external_session_id: "ext1",
    state,
  };
}

function snapshot(terminals: WorkspaceRuntimeTerminal[]): WorkspaceRuntimeSnapshot {
  return {
    workspace: { id: "ws1", name: "root", path: "/repo" },
    terminals,
    agent_sessions: terminals.flatMap((t) => t.sessions),
    services: [],
  };
}

function renderTree(snap: WorkspaceRuntimeSnapshot) {
  return render(
    <RuntimeSelectionProvider initialSelection={null}>
      <ConsumedNotificationsProvider>
        <WorkspaceRuntimeTree snapshot={snap} />
      </ConsumedNotificationsProvider>
    </RuntimeSelectionProvider>,
  );
}

describe("WorkspaceRuntimeTree — passive agent adoption (review T3)", () => {
  it("adopts a hand-launched agent: shows its provider icon from detected_agent_kind, NO hook session", () => {
    renderTree(
      snapshot([terminal({ detected_agent_kind: "claude_code", sessions: [], busy: true })]),
    );
    // The passive kind supplies the icon even though there is no agent_sessions row.
    expect(screen.getByTestId("provider-icon-claude_code")).toBeTruthy();
  });

  it("is PROVIDER-AGNOSTIC: a non-claude detected kind is displayed the SAME way", () => {
    renderTree(snapshot([terminal({ detected_agent_kind: "codex", sessions: [], busy: true })]));
    // A non-claude kind flows through the same resolver → its own icon (no `if claude`).
    expect(screen.getByTestId("provider-icon-codex")).toBeTruthy();
    expect(screen.queryByTestId("provider-icon-claude_code")).toBeNull();
  });

  it("RECONCILIATION: a hook session takes precedence over the passive kind (single icon)", () => {
    // The terminal both hosts a real hook session (claude_code) AND passively detects a different
    // foreground agent (codex). The resumable hook session wins → exactly ONE icon, the session's.
    renderTree(
      snapshot([
        terminal({ detected_agent_kind: "codex", sessions: [session("claude_code")], busy: true }),
      ]),
    );
    expect(screen.getByTestId("provider-icon-claude_code")).toBeTruthy();
    // No double icon: the passive codex icon is NOT rendered alongside it.
    expect(screen.queryByTestId("provider-icon-codex")).toBeNull();
  });

  it("the passive icon DISAPPEARS when the agent stops (detected_agent_kind null → terminal glyph)", () => {
    renderTree(snapshot([terminal({ detected_agent_kind: null, sessions: [] })]));
    // No provider icon at all — the row falls back to the generic terminal glyph.
    expect(screen.queryByTestId("provider-icon-claude_code")).toBeNull();
    expect(screen.queryByTestId("provider-icon-codex")).toBeNull();
    // The row still renders (by its label), just without a provider icon.
    expect(screen.getByText("term")).toBeTruthy();
  });

  it("an ENDED hook session does not win — a passive foreground agent is still adopted", () => {
    // `activeAgentSession` ignores `ended` sessions, so a terminal whose only session ended but
    // whose foreground is a live agent still adopts the passive icon (the reconciliation is on the
    // ACTIVE session, not any historical row).
    renderTree(
      snapshot([
        terminal({
          detected_agent_kind: "codex",
          sessions: [session("claude_code", "ended")],
          busy: true,
        }),
      ]),
    );
    expect(screen.getByTestId("provider-icon-codex")).toBeTruthy();
    expect(screen.queryByTestId("provider-icon-claude_code")).toBeNull();
  });
});
