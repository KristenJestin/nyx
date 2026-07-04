import { render, screen } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it } from "vitest";

import { RuntimePaneHost } from "./runtime-pane-host";
import { RuntimeSelectionProvider, type RuntimeSelection } from "./runtime-selection";
import type { WorkspaceRuntimeService } from "./use-workspace-runtime";

function service(overrides: Partial<WorkspaceRuntimeService> = {}): WorkspaceRuntimeService {
  return {
    id: "s1",
    workspace_id: "ws1",
    name: "preview",
    command: "npm run preview",
    run_state: "idle",
    exit_code: 0,
    unread: false,
    ...overrides,
  };
}

function renderHost(services: WorkspaceRuntimeService[], selection: RuntimeSelection) {
  return render(
    <RuntimeSelectionProvider initialSelection={selection}>
      <RuntimePaneHost terminals={[]} services={services} />
    </RuntimeSelectionProvider>,
  );
}

describe("RuntimePaneHost — managed-command empty state", () => {
  beforeEach(() => {
    // The output panel rehydrates via `command_output`; keep it inert for the running case.
    mockIPC((cmd) => (cmd === "command_output" ? "" : undefined));
  });

  it("shows the MINIMAL empty state (a discrete state label, not a card/black pane) when idle", () => {
    renderHost([service({ run_state: "idle", exit_code: 0 })], { kind: "service", id: "s1" });
    // The minimal empty state = just the run-state word; the rejected card's copy is gone.
    expect(screen.getByText("idle")).toBeTruthy();
    expect(screen.queryByText(/npm run preview/)).toBeNull();
    expect(screen.queryByText(/Last run/)).toBeNull();
    expect(screen.queryByText("Output appears here when the command runs.")).toBeNull();
  });

  it("shows the live output pane (no idle empty state) once the service is running", () => {
    renderHost([service({ run_state: "running", exit_code: null })], { kind: "service", id: "s1" });
    // A running service streams output — the idle state label is NOT rendered.
    expect(screen.queryByText("idle")).toBeNull();
  });
});
