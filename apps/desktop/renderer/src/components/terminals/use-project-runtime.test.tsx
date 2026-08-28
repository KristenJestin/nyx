import { renderHook, waitFor } from "@testing-library/react";
import { emit, mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import { useProjectRuntime } from "./use-project-runtime";
import type { WorkspaceRuntimeSnapshot } from "@/components/runtime/use-workspace-runtime";

function snapshot(wsId: string, terminalCount: number): WorkspaceRuntimeSnapshot {
  return {
    workspace: { id: wsId, name: wsId, path: `/${wsId}` },
    terminals: Array.from({ length: terminalCount }, (_, i) => ({
      id: `${wsId}-t${i}`,
      cwd: `/${wsId}`,
      label: null,
      workspace_id: wsId,
      status: "alive" as const,
      exec_state: "idle" as const,
      exec_state_unread: false,
      sessions: [],
    })),
    agent_sessions: [],
    services: [],
  };
}

describe("useProjectRuntime", () => {
  it("is disabled (empty, not loading) for an empty workspace list", async () => {
    mockIPC(() => undefined);
    const { result } = renderHook(() => useProjectRuntime([]));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.runtimeByWorkspace.size).toBe(0);
  });

  it("loads a snapshot per workspace, keyed by workspace id", async () => {
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      if (cmd === "workspace_runtime_snapshot") {
        const wsId = a.workspaceId as string;
        return snapshot(wsId, wsId === "ws1" ? 2 : 1);
      }
      return undefined;
    });
    const { result } = renderHook(() => useProjectRuntime(["ws1", "ws2"]));
    await waitFor(() => expect(result.current.runtimeByWorkspace.size).toBe(2));
    expect(result.current.runtimeByWorkspace.get("ws1")?.terminals).toHaveLength(2);
    expect(result.current.runtimeByWorkspace.get("ws2")?.terminals).toHaveLength(1);
  });

  it("re-pulls LIVE on a `*://changed` push (no polling — event-driven)", async () => {
    let terminalsForWs1 = 1;
    let calls = 0;
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      if (cmd === "workspace_runtime_snapshot") {
        calls += 1;
        return snapshot(a.workspaceId as string, terminalsForWs1);
      }
      return undefined;
    });
    const { result } = renderHook(() => useProjectRuntime(["ws1"]));
    await waitFor(() =>
      expect(result.current.runtimeByWorkspace.get("ws1")?.terminals).toHaveLength(1),
    );
    const callsAfterLoad = calls;

    // A runtime mutation is announced by the backend; the hook re-pulls WITHOUT any timer.
    terminalsForWs1 = 3;
    await emit("terminals://changed", {});
    await waitFor(() =>
      expect(result.current.runtimeByWorkspace.get("ws1")?.terminals).toHaveLength(3),
    );
    // The re-pull was triggered by the event (call count grew), not by polling on a timer.
    expect(calls).toBeGreaterThan(callsAfterLoad);
  });

  it("omits a workspace whose snapshot fails without blanking the others", async () => {
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      if (cmd === "workspace_runtime_snapshot") {
        if (a.workspaceId === "bad") throw new Error("boom");
        return snapshot(a.workspaceId as string, 1);
      }
      return undefined;
    });
    const { result } = renderHook(() => useProjectRuntime(["ws1", "bad"]));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.runtimeByWorkspace.get("ws1")?.terminals).toHaveLength(1);
    expect(result.current.runtimeByWorkspace.has("bad")).toBe(false);
  });
});
