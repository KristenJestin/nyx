import { render, screen, waitFor } from "@testing-library/react";
import { bridgeFake } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import { RuntimeSelectionProvider } from "./runtime-selection";
import { ConsumedNotificationsProvider } from "./use-consumed-notifications";
import { RestorePlanProvider } from "./use-restore-plan";
import { WorkspaceRuntimeTree } from "./workspace-runtime-tree";
import type {
  WorkspaceRuntimeService,
  WorkspaceRuntimeSnapshot,
  WorkspaceRuntimeTerminal,
} from "./use-workspace-runtime";

function snapshot(over: Partial<WorkspaceRuntimeSnapshot> = {}): WorkspaceRuntimeSnapshot {
  return {
    workspace: { id: "ws1", name: "root", path: "/repo" },
    terminals: [],
    agent_sessions: [],
    services: [],
    ...over,
  };
}

function terminalWithSession(state: string): WorkspaceRuntimeTerminal {
  return {
    id: "t1",
    cwd: "/repo",
    label: "agent-term",
    workspace_id: "ws1",
    status: "alive",
    exec_state: "running",
    exec_state_unread: false,
    sessions: [
      {
        id: "sess1",
        terminal_id: "t1",
        workspace_id: "ws1",
        agent_kind: "claude_code",
        external_session_id: "ext1",
        state,
      },
    ],
  };
}

function service(id: string, name: string): WorkspaceRuntimeService {
  return {
    id,
    workspace_id: "ws1",
    name,
    command: "npm run dev",
    run_state: "running",
    exit_code: null,
    unread: false,
  };
}

/** Restore-plan wire shape flagging the given command instances interrupted (no resumables). */
function restorePlan(interrupted: string[]) {
  return {
    workspace_id: "ws1",
    resume_sessions: [],
    restart_on_startup: [],
    interrupted,
  };
}

function renderTree(snap: WorkspaceRuntimeSnapshot) {
  return render(
    <RuntimeSelectionProvider>
      <ConsumedNotificationsProvider>
        <RestorePlanProvider workspaceIds={["ws1"]}>
          <WorkspaceRuntimeTree snapshot={snap} />
        </RestorePlanProvider>
      </ConsumedNotificationsProvider>
    </RuntimeSelectionProvider>,
  );
}

describe("WorkspaceRuntimeTree — post-crash lifecycle badges", () => {
  it("badges a service `interrupted` when its instance is in the crash-reconcile set (running -> interrupted)", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => restorePlan(["svc-1"]));

    renderTree(snapshot({ services: [service("svc-1", "dev-server")] }));

    // After the crash the running service is surfaced as `interrupted` beside its dot.
    await waitFor(() => {
      const badge = screen.getByText("Interrupted");
      expect(badge.getAttribute("data-lifecycle")).toBe("interrupted");
    });
  });

  it("does NOT badge a service that is not in the interrupted set", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => restorePlan(["other"]));

    renderTree(snapshot({ services: [service("svc-1", "dev-server")] }));

    await waitFor(() =>
      expect(fake.calls.some((c) => c.command === "workspace_restore_plan")).toBe(true),
    );
    expect(screen.queryByText("Interrupted")).toBeNull();
  });

  it("badges a terminal's agent session `unknown` (active -> unknown) from the live snapshot", () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => restorePlan([]));

    renderTree(snapshot({ terminals: [terminalWithSession("unknown")] }));

    const badge = screen.getByText("Unknown");
    expect(badge.getAttribute("data-lifecycle")).toBe("unknown");
  });

  it("does NOT badge a healthy `active` agent session", () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => restorePlan([]));

    renderTree(snapshot({ terminals: [terminalWithSession("active")] }));

    expect(screen.queryByText("Unknown")).toBeNull();
  });
});
