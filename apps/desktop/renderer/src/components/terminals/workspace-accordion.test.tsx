import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it } from "vitest";

import type { WorkspaceRecord } from "@/components/sidebar/use-projects";
import { RuntimeSelectionProvider } from "./runtime-selection";
import { ConsumedNotificationsProvider } from "./use-consumed-notifications";
import { WorkspaceAccordion } from "./workspace-accordion";
import type { WorkspaceRuntimeSnapshot, WorkspaceRuntimeTerminal } from "./use-workspace-runtime";

function workspace(overrides: Partial<WorkspaceRecord> = {}): WorkspaceRecord {
  return {
    id: "ws1",
    project_id: "p1",
    name: "root",
    path: "/repo",
    branch: null,
    is_root: true,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
    ...overrides,
  };
}

function terminal(overrides: Partial<WorkspaceRuntimeTerminal> = {}): WorkspaceRuntimeTerminal {
  return {
    id: "t1",
    cwd: "/repo",
    label: "t1",
    workspace_id: "ws1",
    status: "alive",
    exec_state: "idle",
    exec_state_unread: false,
    sessions: [],
    ...overrides,
  };
}

function snapshot(terminals: WorkspaceRuntimeTerminal[]): WorkspaceRuntimeSnapshot {
  return {
    workspace: { id: "ws1", name: "root", path: "/repo" },
    terminals,
    agent_sessions: [],
    services: [],
  };
}

/** Mount the accordion with the runtime-selection + consumed-notifications providers it needs. */
function renderAccordion(
  snap: WorkspaceRuntimeSnapshot | null,
  defaultOpen = false,
  props: Partial<React.ComponentProps<typeof WorkspaceAccordion>> = {},
) {
  return render(
    <RuntimeSelectionProvider initialSelection={null}>
      <ConsumedNotificationsProvider>
        <WorkspaceAccordion
          workspace={workspace()}
          snapshot={snap}
          defaultOpen={defaultOpen}
          {...props}
        />
      </ConsumedNotificationsProvider>
    </RuntimeSelectionProvider>,
  );
}

/** The accordion header carries the rollup dot; find it by its `[data-state]` host (or null). */
function headerDot(container: HTMLElement): HTMLElement | null {
  const header = container.querySelector("button[aria-expanded]") as HTMLElement | null;
  return header?.querySelector("[data-state]") ?? null;
}

describe("WorkspaceAccordion — rollup dot masking (finding: hide when deployed)", () => {
  beforeEach(() => {
    // No agent activity — `useAgentSessions` resolves to empty maps.
    mockIPC((cmd) => {
      if (cmd === "agent_active_sessions") return [];
      if (cmd === "agent_activity_snapshot") return [];
      return undefined;
    });
  });

  it("shows the rollup dot when the accordion is COLLAPSED (non-idle summary)", async () => {
    // A single unread success terminal → a non-idle rollup. Collapsed (default) → the dot shows.
    const snap = snapshot([terminal({ exec_state: "success", exec_state_unread: true })]);
    const { container } = renderAccordion(snap, false);
    await waitFor(() => {
      expect(headerDot(container)?.getAttribute("data-state")).toBe("success");
    });
  });

  it("MASKS the rollup dot when the accordion is DEPLOYED/open (rows already visible)", async () => {
    // Same non-idle rollup, but opened → the dot is masked (the tree rows carry their own dots).
    const snap = snapshot([terminal({ exec_state: "success", exec_state_unread: true })]);
    const { container } = renderAccordion(snap, true);
    // The header is expanded; its rollup dot is absent.
    const header = container.querySelector('button[aria-expanded="true"]');
    expect(header).not.toBeNull();
    // Give effects a chance to settle, then assert the header dot stayed hidden.
    await waitFor(() => expect(screen.getByText("root")).toBeTruthy());
    expect(headerDot(container)).toBeNull();
  });

  it("toggles the dot: visible collapsed → hidden once expanded by the user", async () => {
    const snap = snapshot([terminal({ exec_state: "error", exec_state_unread: true })]);
    const { container } = renderAccordion(snap, false);
    await waitFor(() => expect(headerDot(container)?.getAttribute("data-state")).toBe("error"));
    // Expand: the summary dot disappears (detail now on screen).
    fireEvent.click(container.querySelector("button[aria-expanded]") as HTMLElement);
    await waitFor(() => expect(headerDot(container)).toBeNull());
  });

  it("stays quiet (no dot) for an idle workspace whether collapsed or open", async () => {
    const snap = snapshot([terminal({ exec_state: "idle" })]);
    const { container: collapsed } = renderAccordion(snap, false);
    await waitFor(() => expect(screen.getAllByText("root").length).toBeGreaterThan(0));
    expect(headerDot(collapsed)).toBeNull();
  });
});

describe("WorkspaceAccordion — notification-aware rollup (finding: consumed leaf drops out)", () => {
  beforeEach(() => {
    mockIPC((cmd) => {
      if (cmd === "agent_active_sessions") return [];
      if (cmd === "agent_activity_snapshot") return [];
      return undefined;
    });
  });

  it("does NOT show a rollup dot for a workspace whose only success is already READ", async () => {
    // A success the snapshot reports as already read (`exec_state_unread: false`) never
    // contributes → the collapsed rollup is idle → no dot.
    const snap = snapshot([terminal({ exec_state: "success", exec_state_unread: false })]);
    const { container } = renderAccordion(snap, false);
    await waitFor(() => expect(screen.getByText("root")).toBeTruthy());
    expect(headerDot(container)).toBeNull();
  });
});

/**
 * The lifecycle KEBAB, one of the two entry points to a workspace's rename/remove (the other is
 * the project rail's per-workspace group in `base-layout.tsx`, guarded in its own suite).
 *
 * On a ROOT workspace the trigger itself must not exist (review `01M0A7QGBK43MZB10RF51QR6TS`).
 * It used to open onto an active Rename and a DISABLED Remove explaining that the root leaves
 * with its project — a dead end costing a click to learn there was nothing to do. The user asked
 * for nothing at all there, rename included.
 */
describe("WorkspaceAccordion — lifecycle kebab (root shows none)", () => {
  beforeEach(() => {
    mockIPC((cmd) => {
      if (cmd === "agent_active_sessions") return [];
      if (cmd === "agent_activity_snapshot") return [];
      return undefined;
    });
  });

  const actions = { onRename: () => {}, onRemove: () => {} };

  it("ROOT workspace: no kebab trigger, and therefore no Rename / Remove entry", async () => {
    renderAccordion(snapshot([]), false, { workspace: workspace({ is_root: true }), ...actions });
    await waitFor(() => expect(screen.getByText("root")).toBeTruthy());
    // Not the three dots…
    expect(screen.queryByRole("button", { name: "Workspace actions for root" })).toBeNull();
    // …and nothing behind them either.
    expect(screen.queryByRole("menuitem", { name: /Rename workspace/ })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: /Remove workspace/ })).toBeNull();
  });

  it("NON-root workspace: the kebab opens on a live Rename + Remove that fire their handlers", async () => {
    const renamed: string[] = [];
    const removed: string[] = [];
    renderAccordion(snapshot([]), false, {
      workspace: workspace({ id: "ws2", name: "api", is_root: false }),
      onRename: (w) => renamed.push(w.id),
      onRemove: (w) => removed.push(w.id),
    });

    const trigger = await screen.findByRole("button", { name: "Workspace actions for api" });
    fireEvent.click(trigger);

    const remove = await screen.findByRole("menuitem", { name: "Remove workspace api" });
    // Live, not a greyed placeholder — the root's disabled row has no equivalent here.
    expect(remove).not.toHaveAttribute("aria-disabled", "true");
    fireEvent.click(remove);
    expect(removed).toEqual(["ws2"]);

    fireEvent.click(await screen.findByRole("button", { name: "Workspace actions for api" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Rename workspace api" }));
    expect(renamed).toEqual(["ws2"]);
  });
});
