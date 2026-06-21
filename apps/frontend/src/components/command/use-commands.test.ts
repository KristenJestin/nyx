import { act, renderHook, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import {
  driftedScriptValue,
  isCustomized,
  runnerCommand,
  useCommands,
  type DiscoveredScript,
  type ManagedCommand,
} from "./use-commands";

function discovered(over: Partial<DiscoveredScript> = {}): DiscoveredScript {
  return {
    proposed_name: "dev",
    script_name: "dev",
    default_command: "pnpm dev",
    script_command_snapshot: "vite --host",
    subfolder: "",
    package_json_path: "/repo/package.json",
    package_manager: "pnpm",
    ...over,
  };
}

function cmd(over: Partial<ManagedCommand>): ManagedCommand {
  return {
    id: "c1",
    project_id: "p1",
    name: "dev",
    command: "pnpm dev",
    subfolder: null,
    restart_on_startup: false,
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    source_kind: "package_json",
    source_package_json_path: "/repo/package.json",
    source_script_name: "dev",
    source_script_command_snapshot: "vite --host",
    package_manager: "pnpm",
    ...over,
  };
}

describe("runnerCommand (front mirror of PackageManager::run_script)", () => {
  it("uses the manager's runner invocation form", () => {
    expect(runnerCommand("pnpm", "dev")).toBe("pnpm dev");
    expect(runnerCommand("yarn", "dev")).toBe("yarn dev");
    expect(runnerCommand("npm", "dev")).toBe("npm run dev");
    expect(runnerCommand("bun", "dev")).toBe("bun run dev");
    expect(runnerCommand(null, "dev")).toBe("npm run dev"); // fallback
  });
});

describe("isCustomized (the `customized` badge predicate)", () => {
  it("is FALSE when command equals the detected runner call", () => {
    expect(isCustomized(cmd({ command: "pnpm dev" }))).toBe(false);
  });

  it("is FALSE when command equals the current raw script snapshot", () => {
    expect(
      isCustomized(cmd({ command: "vite --host", source_script_command_snapshot: "vite --host" })),
    ).toBe(false);
  });

  it("is TRUE when command matches NEITHER the runner NOR the raw script", () => {
    expect(isCustomized(cmd({ command: "node server.js" }))).toBe(true);
  });

  it("is FALSE for a hand-authored (un-sourced) template", () => {
    expect(
      isCustomized(
        cmd({
          command: "anything goes",
          source_script_name: null,
          source_package_json_path: null,
          source_kind: null,
        }),
      ),
    ).toBe(false);
  });
});

describe("driftedScriptValue (passive 'changed in package.json' detection)", () => {
  it("returns the live on-disk value when the script body moved since sync", () => {
    const cmdRow = cmd({ source_script_command_snapshot: "vite --host" });
    const found = [discovered({ script_command_snapshot: "vite --port 4000" })];
    expect(driftedScriptValue(cmdRow, found)).toBe("vite --port 4000");
  });

  it("returns null when the on-disk body still matches the synced snapshot", () => {
    const cmdRow = cmd({ source_script_command_snapshot: "vite --host" });
    const found = [discovered({ script_command_snapshot: "vite --host" })];
    expect(driftedScriptValue(cmdRow, found)).toBeNull();
  });

  it("returns null for a hand-authored (un-sourced) command", () => {
    const cmdRow = cmd({ source_script_name: null, source_package_json_path: null });
    expect(driftedScriptValue(cmdRow, [discovered()])).toBeNull();
  });

  it("returns null when the source script is no longer discoverable", () => {
    const cmdRow = cmd({ source_script_name: "dev" });
    const found = [discovered({ script_name: "build", package_json_path: "/repo/package.json" })];
    expect(driftedScriptValue(cmdRow, found)).toBeNull();
  });

  it("matches on BOTH package.json path and script name", () => {
    const cmdRow = cmd({
      source_package_json_path: "/repo/package.json",
      source_script_name: "dev",
      source_script_command_snapshot: "vite --host",
    });
    // Same script name but a DIFFERENT package.json → not the same source.
    const found = [
      discovered({
        package_json_path: "/repo/api/package.json",
        script_command_snapshot: "vite --port 9",
      }),
    ];
    expect(driftedScriptValue(cmdRow, found)).toBeNull();
  });
});

describe("useCommands (commands://changed re-pull)", () => {
  it("re-lists templates when commands://changed fires (an MCP/UI template mutation)", async () => {
    // The modal only re-lists on a projectId change; a template mutated by another
    // surface (an MCP tool the modal never invoked) must still appear, via the event.
    let current: ManagedCommand[] = [cmd({ id: "c1", name: "dev" })];
    mockIPC(
      (command, args) => {
        if (command === "command_list") {
          const a = (args ?? {}) as { projectId?: string };
          return a.projectId === "p1" ? current : [];
        }
        return null;
      },
      { shouldMockEvents: true },
    );
    const { result } = renderHook(() => useCommands("p1"));
    await waitFor(() => expect(result.current.templates).toHaveLength(1));
    expect(result.current.templates[0].name).toBe("dev");

    // A template was added on the project → the next list returns two rows.
    current = [cmd({ id: "c1", name: "dev" }), cmd({ id: "c2", name: "build" })];
    await act(async () => {
      await emit("commands://changed");
    });
    await waitFor(() => expect(result.current.templates).toHaveLength(2));
  });
});
