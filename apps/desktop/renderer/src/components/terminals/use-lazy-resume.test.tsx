import { renderHook, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { bridgeFake } from "@/bridge/test-harness";
import { describe, expect, it, vi } from "vitest";

import { useLazyResume } from "./use-lazy-resume";

/** Build a `workspace_restore_plan` shape with the given resume_sessions. */
function plan(workspaceId: string, resumeSessions: unknown[]) {
  return {
    workspace_id: workspaceId,
    resume_sessions: resumeSessions,
    restart_on_startup: [],
    interrupted: [],
  };
}

describe("use-lazy-resume (renderer half of the lazy resume)", () => {
  it("consumes workspace_restore_plan and pushes ONLY the resumable sessions as parks", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () =>
      plan("ws-1", [
        // resumable → pushed as a park (terminalId/sessionId/command).
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: "claude --resume ext-1",
          resume: true,
          uncertain: false,
          skip_reason: null,
        },
        // toggle OFF / non-candidate → NOT pushed (no injection).
        {
          terminal_id: "t-2",
          session_id: "s-2",
          agent_kind: "claude_code",
          resume_command: null,
          resume: false,
          uncertain: false,
          skip_reason: "option_off",
        },
      ]),
    );
    const parkArgs = vi.fn();
    fake.on("set_resume_parks", (args) => {
      parkArgs(args);
      return null;
    });

    renderHook(() => useLazyResume(["ws-1"]));

    await waitFor(() => expect(parkArgs).toHaveBeenCalledTimes(1));
    expect(parkArgs).toHaveBeenCalledWith({
      parks: [{ terminalId: "t-1", sessionId: "s-1", command: "claude --resume ext-1" }],
    });
  });

  it("does NOT auto-park a DOUBTFUL (uncertain) resumable session — that belongs to the panel", async () => {
    // The partition with `use-restore-plan`: lazy auto-drains ONLY the confident sessions
    // (`!uncertain`); a post-crash `unknown`/stale candidate (`uncertain: true`) is left for the
    // restore panel so the user chooses (and an Ignore there truly blocks — no park is ever set).
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () =>
      plan("ws-1", [
        // Confident resumable → auto-parked by lazy.
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: "claude --resume ext-1",
          resume: true,
          uncertain: false,
          skip_reason: null,
        },
        // DOUBTFUL resumable (post-crash unknown) → NOT auto-parked; the panel owns it.
        {
          terminal_id: "t-2",
          session_id: "s-2",
          agent_kind: "claude_code",
          resume_command: "claude --resume ext-2",
          resume: true,
          uncertain: true,
          skip_reason: null,
        },
      ]),
    );
    const parkArgs = vi.fn();
    fake.on("set_resume_parks", (args) => {
      parkArgs(args);
      return null;
    });

    renderHook(() => useLazyResume(["ws-1"]));

    await waitFor(() => expect(parkArgs).toHaveBeenCalledTimes(1));
    // ONLY the confident session is parked; the doubtful one is never pushed by lazy.
    expect(parkArgs).toHaveBeenCalledWith({
      parks: [{ terminalId: "t-1", sessionId: "s-1", command: "claude --resume ext-1" }],
    });
  });

  it("still injects under React StrictMode (dev double-invoke) — pushes the parks EXACTLY ONCE", async () => {
    // Regression guard: <React.StrictMode> mounts→unmounts→remounts the hook in dev. A prior
    // version marked pushedRef before the await AND cancelled the resolved push on the StrictMode
    // cleanup, so run1's invoke bailed (`if (cancelled) return`) while run2 skipped the already-
    // marked workspace → set_resume_parks was NEVER called (empty resume in `tauri dev`; prod
    // StrictMode is inert, hiding the bug). The wrapper reproduces the double-invoke; the hook must
    // still push the resumable parks exactly once — no missed injection, and no double injection.
    const fake = bridgeFake();
    const planReads = vi.fn();
    fake.on("workspace_restore_plan", (args) => {
      planReads(args);
      return plan("ws-1", [
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: "claude --resume ext-1",
          resume: true,
          uncertain: false,
          skip_reason: null,
        },
      ]);
    });
    const parkArgs = vi.fn();
    fake.on("set_resume_parks", (args) => {
      parkArgs(args);
      return null;
    });

    renderHook(() => useLazyResume(["ws-1"]), { wrapper: StrictMode });

    await waitFor(() => expect(parkArgs).toHaveBeenCalledTimes(1));
    expect(parkArgs).toHaveBeenCalledWith({
      parks: [{ terminalId: "t-1", sessionId: "s-1", command: "claude --resume ext-1" }],
    });
    // The ref guard survives the StrictMode remount, so the plan is read once and the parks are
    // pushed once — never twice.
    expect(planReads).toHaveBeenCalledTimes(1);
    expect(parkArgs).toHaveBeenCalledTimes(1);
  });

  it("pushes nothing when no session is resumable (toggle OFF everywhere)", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () =>
      plan("ws-1", [
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: null,
          resume: false,
          uncertain: false,
          skip_reason: "option_off",
        },
      ]),
    );
    const setParks = vi.fn(() => null);
    fake.on("set_resume_parks", setParks);

    renderHook(() => useLazyResume(["ws-1"]));

    // Let the plan read resolve, then assert NO park push happened.
    await waitFor(() =>
      expect(fake.calls.some((c) => c.command === "workspace_restore_plan")).toBe(true),
    );
    await Promise.resolve();
    expect(setParks).not.toHaveBeenCalled();
  });

  it("reads + pushes each workspace AT MOST ONCE per app run (idempotent driver)", async () => {
    const fake = bridgeFake();
    const planReads = vi.fn();
    fake.on("workspace_restore_plan", (args) => {
      planReads(args);
      return plan("ws-1", [
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: "claude --resume ext-1",
          resume: true,
          uncertain: false,
          skip_reason: null,
        },
      ]);
    });
    fake.on("set_resume_parks", () => null);

    const { rerender } = renderHook(({ ids }: { ids: string[] }) => useLazyResume(ids), {
      initialProps: { ids: ["ws-1"] },
    });
    await waitFor(() => expect(planReads).toHaveBeenCalledTimes(1));

    // A re-render with the SAME workspace must NOT re-read/re-push (the ref guard holds).
    rerender({ ids: ["ws-1"] });
    await Promise.resolve();
    expect(planReads).toHaveBeenCalledTimes(1);
  });

  it("never throws when the restore-plan read fails (best-effort)", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => {
      throw new Error("sidecar not ready");
    });
    const setParks = vi.fn(() => null);
    fake.on("set_resume_parks", setParks);

    // The hook swallows the rejection — the page never sees an error.
    expect(() => renderHook(() => useLazyResume(["ws-1"]))).not.toThrow();
    await waitFor(() =>
      expect(fake.calls.some((c) => c.command === "workspace_restore_plan")).toBe(true),
    );
    expect(setParks).not.toHaveBeenCalled();
  });
});
