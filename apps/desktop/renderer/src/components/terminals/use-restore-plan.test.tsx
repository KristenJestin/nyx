import { act, renderHook, waitFor } from "@testing-library/react";
import { StrictMode, type ReactNode } from "react";
import { bridgeFake } from "@/bridge/test-harness";
import { describe, expect, it, vi } from "vitest";

import { RestorePlanProvider, useRestorePlan, type RestorableSession } from "./use-restore-plan";

/** A restore-plan wire shape with the given interrupted instances + resume_sessions. */
function plan(workspaceId: string, interrupted: string[], resumeSessions: unknown[]) {
  return {
    workspace_id: workspaceId,
    resume_sessions: resumeSessions,
    restart_on_startup: [],
    interrupted,
  };
}

function resumableEntry(over: Record<string, unknown> = {}) {
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

function wrapper(workspaceIds: string[]) {
  return ({ children }: { children: ReactNode }) => (
    <RestorePlanProvider workspaceIds={workspaceIds}>{children}</RestorePlanProvider>
  );
}

/** Same provider, wrapped in <StrictMode> to reproduce React's dev mount→unmount→remount. */
function strictWrapper(workspaceIds: string[]) {
  return ({ children }: { children: ReactNode }) => (
    <StrictMode>
      <RestorePlanProvider workspaceIds={workspaceIds}>{children}</RestorePlanProvider>
    </StrictMode>
  );
}

describe("use-restore-plan (post-crash restore registry)", () => {
  it("flags the crash-interrupted command instances from workspace_restore_plan", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => plan("ws-1", ["inst-A", "inst-B"], []));

    const { result } = renderHook(() => useRestorePlan(), { wrapper: wrapper(["ws-1"]) });

    await waitFor(() => expect(result.current.isInterrupted("inst-A")).toBe(true));
    expect(result.current.isInterrupted("inst-B")).toBe(true);
    expect(result.current.isInterrupted("inst-other")).toBe(false);
  });

  it("exposes ONLY the resumable sessions (a null resume_command is not restorable)", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () =>
      plan(
        "ws-1",
        [],
        [
          resumableEntry({ session_id: "s-1", terminal_id: "t-1" }),
          // toggle OFF / non-resumable → no resume_command → not offered.
          resumableEntry({
            session_id: "s-2",
            resume: false,
            resume_command: null,
            skip_reason: "option_off",
          }),
        ],
      ),
    );

    const { result } = renderHook(() => useRestorePlan(), { wrapper: wrapper(["ws-1"]) });

    await waitFor(() => expect(result.current.restorablesForWorkspace("ws-1")).toHaveLength(1));
    const [only] = result.current.restorablesForWorkspace("ws-1");
    expect(only?.sessionId).toBe("s-1");
    expect(only?.command).toBe("claude --resume ext-1");
  });

  it("surfaces ONLY the DOUBTFUL sessions — a confident resumable is lazy's, not the panel's", async () => {
    // The partition with `use-lazy-resume`: the panel owns exactly the post-crash doubtful
    // sessions (`uncertain: true`); a CONFIDENT resumable (`uncertain: false`) is auto-drained by
    // lazy and must NOT appear here, so the two consumers never overlap.
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () =>
      plan(
        "ws-1",
        [],
        [
          // DOUBTFUL (unknown/stale post-crash) → surfaced by the panel.
          resumableEntry({ session_id: "s-doubtful", terminal_id: "t-1", uncertain: true }),
          // CONFIDENT (active, clean shutdown) → owned by use-lazy-resume, hidden from the panel.
          resumableEntry({ session_id: "s-confident", terminal_id: "t-2", uncertain: false }),
        ],
      ),
    );

    const { result } = renderHook(() => useRestorePlan(), { wrapper: wrapper(["ws-1"]) });

    await waitFor(() => expect(result.current.restorablesForWorkspace("ws-1")).toHaveLength(1));
    const [only] = result.current.restorablesForWorkspace("ws-1");
    expect(only?.sessionId).toBe("s-doubtful");
  });

  it("RESUME pushes the session's park via set_resume_parks and drops it from the list", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => plan("ws-1", [], [resumableEntry()]));
    const parkArgs = vi.fn();
    fake.on("set_resume_parks", (args) => {
      parkArgs(args);
      return null;
    });

    const { result } = renderHook(() => useRestorePlan(), { wrapper: wrapper(["ws-1"]) });
    await waitFor(() => expect(result.current.restorablesForWorkspace("ws-1")).toHaveLength(1));

    const session = result.current.restorablesForWorkspace("ws-1")[0] as RestorableSession;
    act(() => result.current.resume(session));

    await waitFor(() => expect(parkArgs).toHaveBeenCalledTimes(1));
    expect(parkArgs).toHaveBeenCalledWith({
      parks: [{ terminalId: "t-1", sessionId: "s-1", command: "claude --resume ext-1" }],
    });
    // Resumed → dismissed from the restore list.
    expect(result.current.restorablesForWorkspace("ws-1")).toHaveLength(0);
  });

  it("IGNORE drops the session WITHOUT resuming (no set_resume_parks)", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => plan("ws-1", [], [resumableEntry()]));
    const setParks = vi.fn(() => null);
    fake.on("set_resume_parks", setParks);

    const { result } = renderHook(() => useRestorePlan(), { wrapper: wrapper(["ws-1"]) });
    await waitFor(() => expect(result.current.restorablesForWorkspace("ws-1")).toHaveLength(1));

    act(() => result.current.ignore("ws-1", "s-1"));

    expect(result.current.restorablesForWorkspace("ws-1")).toHaveLength(0);
    expect(setParks).not.toHaveBeenCalled();
  });

  it("reads workspace_restore_plan ONCE under React StrictMode (dev double-invoke)", async () => {
    // Convention parity with use-lazy-resume.test.tsx: this provider runs an async effect
    // (useEffect → nyxBridge.invoke) with a ref guard marked BEFORE the await. <StrictMode> mounts
    // →unmounts→remounts it in dev; the ref survives, so the plan must be read EXACTLY ONCE and the
    // doubtful session still surface correctly — locking the read-at-most-once guard against the
    // double-invoke (no cleanup cancels the resolved read).
    const fake = bridgeFake();
    const planReads = vi.fn();
    fake.on("workspace_restore_plan", (args) => {
      planReads(args);
      return plan("ws-1", [], [resumableEntry()]);
    });

    const { result } = renderHook(() => useRestorePlan(), { wrapper: strictWrapper(["ws-1"]) });

    // Behaviour stays correct under StrictMode: the doubtful session surfaces...
    await waitFor(() => expect(result.current.restorablesForWorkspace("ws-1")).toHaveLength(1));
    // ...and the ref guard survives the mount→unmount→remount: the plan is read exactly once.
    expect(planReads).toHaveBeenCalledTimes(1);
  });

  it("never throws when the restore-plan read fails (best-effort, inert surface)", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => {
      throw new Error("sidecar not ready");
    });

    const { result } = renderHook(() => useRestorePlan(), { wrapper: wrapper(["ws-1"]) });
    await waitFor(() =>
      expect(fake.calls.some((c) => c.command === "workspace_restore_plan")).toBe(true),
    );
    expect(result.current.isInterrupted("inst-A")).toBe(false);
    expect(result.current.restorablesForWorkspace("ws-1")).toHaveLength(0);
  });
});
