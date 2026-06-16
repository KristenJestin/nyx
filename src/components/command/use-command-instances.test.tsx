import { act, renderHook, waitFor } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import { beforeEach, describe, expect, it } from "vitest";

import { useCommandInstances } from "./use-command-instances";
import type { InstanceWithTemplate } from "./use-commands";
import type { ProjectTree } from "@/components/sidebar/use-projects";

/**
 * Build a one-project / one-workspace tree whose single workspace `command_
 * instance_list` returns the given instances. This is the input shape the sidebar
 * feeds `useCommandInstances`.
 */
function installIpc(instances: InstanceWithTemplate[]) {
  mockIPC(
    (cmd, args) => {
      if (cmd === "command_instance_list") {
        const a = (args ?? {}) as { workspaceId?: string };
        return a.workspaceId === "ws1" ? instances : [];
      }
      return null;
    },
    { shouldMockEvents: true },
  );
}

function instance(overrides: Partial<InstanceWithTemplate>): InstanceWithTemplate {
  return {
    id: "i1",
    command_id: "c1",
    workspace_id: "ws1",
    last_state: "idle",
    scrollback: "",
    was_running_on_shutdown: false,
    created_at: 0,
    updated_at: 0,
    name: "dev",
    command: "bun run dev",
    subfolder: null,
    order_index: 0,
    source_kind: null,
    source_package_json_path: null,
    source_script_name: null,
    package_manager: null,
    workspace_path: "/p",
    cwd: "/p",
    ...overrides,
  };
}

const tree: ProjectTree[] = [
  {
    project: { id: "p1", name: "p", collapsed: false, created_at: 0, updated_at: 0 },
    workspaces: [
      {
        id: "ws1",
        project_id: "p1",
        name: "root",
        path: "/p",
        branch: null,
        is_root: true,
        collapsed: false,
        created_at: 0,
        updated_at: 0,
      },
    ],
  },
];

describe("useCommandInstances (sidebar live run-state for gating, finding 7)", () => {
  beforeEach(() => {
    installIpc([instance({ id: "i1", last_state: "idle" })]);
  });

  it("seeds each instance state from last_state, exposed per workspace for the sidebar dot", async () => {
    const { result } = renderHook(() => useCommandInstances(tree));
    await waitFor(() => expect(result.current.instances).toHaveLength(1));
    expect(result.current.instances[0].state).toBe("idle");
    // The sidebar consumes `commandsByWorkspace`; the row's run-state drives gating.
    const records = result.current.commandsByWorkspace.get("ws1");
    expect(records?.[0]).toMatchObject({ id: "i1", state: "idle" });
  });

  it("threads the info-bar fields (command, resolved cwd, source) onto each instance", async () => {
    installIpc([
      instance({
        id: "i1",
        command: "bun run start",
        cwd: "/p/frontend",
        workspace_path: "/p",
        source_script_name: "start",
        source_package_json_path: "frontend/package.json",
        source_kind: "package_json",
      }),
    ]);
    const { result } = renderHook(() => useCommandInstances(tree));
    await waitFor(() => expect(result.current.instances).toHaveLength(1));
    const inst = result.current.instances[0];
    expect(inst.command).toBe("bun run start");
    // The bridge-resolved cwd is used directly (not the bare workspace path).
    expect(inst.cwd).toBe("/p/frontend");
    expect(inst.sourceScriptName).toBe("start");
    expect(inst.sourcePackageJsonPath).toBe("frontend/package.json");
    expect(inst.sourceKind).toBe("package_json");
  });

  it("falls back to the workspace path when the bridge leaves cwd null", async () => {
    installIpc([instance({ id: "i1", cwd: null, workspace_path: "/p" })]);
    const { result } = renderHook(() => useCommandInstances(tree));
    await waitFor(() => expect(result.current.instances).toHaveLength(1));
    expect(result.current.instances[0].cwd).toBe("/p");
  });

  it("updates an instance's state LIVE from command://state (the sidebar dot + gating follow it)", async () => {
    installIpc([instance({ id: "i1", last_state: "idle" })]);
    const { result } = renderHook(() => useCommandInstances(tree));
    await waitFor(() => expect(result.current.instances).toHaveLength(1));
    expect(result.current.instances[0].state).toBe("idle");

    // A running transition for i1 flips its live state → the sidebar row gates to
    // running (Stop+Relaunch active, Play disabled).
    await act(async () => {
      await emit("command://state", { instanceId: "i1", state: "running", code: null });
    });
    await waitFor(() => expect(result.current.instances[0].state).toBe("running"));
    expect(result.current.commandsByWorkspace.get("ws1")?.[0].state).toBe("running");

    // Back to error (a finished run): the row re-gates to stopped (Play+Relaunch
    // active, Stop disabled).
    await act(async () => {
      await emit("command://state", { instanceId: "i1", state: "error", code: 1 });
    });
    await waitFor(() => expect(result.current.instances[0].state).toBe("error"));
  });

  it("ignores a command://state for an unknown instance (no spurious row change)", async () => {
    installIpc([instance({ id: "i1", last_state: "running" })]);
    const { result } = renderHook(() => useCommandInstances(tree));
    await waitFor(() => expect(result.current.instances[0].state).toBe("running"));

    await act(async () => {
      await emit("command://state", { instanceId: "other", state: "idle", code: null });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });
    // i1 is untouched by an event for a different instance.
    expect(result.current.instances[0].state).toBe("running");
  });
});
