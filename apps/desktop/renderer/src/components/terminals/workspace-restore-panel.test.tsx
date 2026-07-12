import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { bridgeFake } from "@/bridge/test-harness";
import { describe, expect, it, vi } from "vitest";

import { RestorePlanProvider } from "./use-restore-plan";
import { WorkspaceRestorePanel } from "./workspace-restore-panel";

function plan(resumeSessions: unknown[]) {
  return {
    workspace_id: "ws-1",
    resume_sessions: resumeSessions,
    restart_on_startup: [],
    interrupted: [],
  };
}

function entry(over: Record<string, unknown> = {}) {
  return {
    terminal_id: "t-1",
    session_id: "s-1",
    agent_kind: "claude_code",
    resume_command: "claude --resume ext-1",
    resume: true,
    uncertain: true,
    skip_reason: null,
    ...over,
  };
}

function renderPanel() {
  return render(
    <RestorePlanProvider workspaceIds={["ws-1"]}>
      <WorkspaceRestorePanel workspaceId="ws-1" />
    </RestorePlanProvider>,
  );
}

describe("WorkspaceRestorePanel (post-crash restoration UI)", () => {
  it("renders NOTHING when a workspace has no resumable interrupted session", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => plan([]));
    renderPanel();
    await waitFor(() =>
      expect(fake.calls.some((c) => c.command === "workspace_restore_plan")).toBe(true),
    );
    expect(screen.queryByRole("region", { name: /restore interrupted sessions/i })).toBeNull();
  });

  it("lists an interrupted session and offers BOTH resume and ignore", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => plan([entry()]));
    renderPanel();

    await waitFor(() => expect(screen.getByText("Claude Code")).toBeTruthy());
    expect(screen.getByRole("button", { name: /resume session/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /ignore session/i })).toBeTruthy();
    // Every listed session is a post-crash DOUBTFUL candidate, so its row carries the `unknown`
    // doubt marker unconditionally (the confident ones are auto-drained by use-lazy-resume).
    expect(screen.getByTitle("Unknown")).toBeTruthy();
  });

  it("RESUME re-injects the session via set_resume_parks then drops it from the panel", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => plan([entry()]));
    const parkArgs = vi.fn();
    fake.on("set_resume_parks", (args) => {
      parkArgs(args);
      return null;
    });
    renderPanel();

    await waitFor(() => expect(screen.getByText("Claude Code")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /resume session/i }));

    await waitFor(() => expect(parkArgs).toHaveBeenCalledTimes(1));
    expect(parkArgs).toHaveBeenCalledWith({
      parks: [{ terminalId: "t-1", sessionId: "s-1", command: "claude --resume ext-1" }],
    });
    // The session is gone from the panel (which then unmounts as it is empty).
    await waitFor(() =>
      expect(screen.queryByRole("region", { name: /restore interrupted sessions/i })).toBeNull(),
    );
  });

  it("IGNORE dismisses the session WITHOUT resuming it", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => plan([entry()]));
    const setParks = vi.fn(() => null);
    fake.on("set_resume_parks", setParks);
    renderPanel();

    await waitFor(() => expect(screen.getByText("Claude Code")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /ignore session/i }));

    await waitFor(() =>
      expect(screen.queryByRole("region", { name: /restore interrupted sessions/i })).toBeNull(),
    );
    expect(setParks).not.toHaveBeenCalled();
  });
});
