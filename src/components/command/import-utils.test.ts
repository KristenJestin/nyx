import { describe, expect, it } from "vitest";

import {
  collidingKeys,
  groupRows,
  importableRows,
  importedSourceKeys,
  isAlreadyImported,
  rowKey,
  toRows,
} from "./import-utils";
import type { DiscoveredScript, ManagedCommand } from "./use-commands";

function script(over: Partial<DiscoveredScript>): DiscoveredScript {
  return {
    proposed_name: "dev",
    script_name: "dev",
    default_command: "pnpm dev",
    script_command_snapshot: "vite",
    subfolder: "",
    package_json_path: "/repo/package.json",
    package_manager: "pnpm",
    ...over,
  };
}

function command(over: Partial<ManagedCommand> = {}): ManagedCommand {
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
    source_script_command_snapshot: "vite",
    package_manager: "pnpm",
    ...over,
  };
}

describe("import-utils: grouping", () => {
  it("groups rows by their package.json, preserving manager + subfolder", () => {
    const rows = toRows([
      script({ script_name: "dev", package_json_path: "/repo/package.json" }),
      script({ script_name: "build", package_json_path: "/repo/package.json" }),
      script({
        script_name: "start",
        package_json_path: "/repo/api/package.json",
        subfolder: "api",
        package_manager: "npm",
      }),
    ]);
    const groups = groupRows(rows);
    expect(groups).toHaveLength(2);
    expect(groups[0].packageJsonPath).toBe("/repo/package.json");
    expect(groups[0].rows).toHaveLength(2);
    expect(groups[0].packageManager).toBe("pnpm");
    expect(groups[1].subfolder).toBe("api");
    expect(groups[1].packageManager).toBe("npm");
  });

  it("seeds editable rows from the discovered name + command, unselected", () => {
    const rows = toRows([script({ proposed_name: "api:dev", default_command: "npm run dev" })]);
    expect(rows[0].name).toBe("api:dev");
    expect(rows[0].command).toBe("npm run dev");
    expect(rows[0].selected).toBe(false);
    expect(rows[0].key).toBe(rowKey(script({})));
  });
});

describe("import-utils: collisions (blocking)", () => {
  it("flags a selected row whose name already exists in the project", () => {
    const rows = toRows([script({ script_name: "dev", proposed_name: "dev" })]).map((r) => ({
      ...r,
      selected: true,
    }));
    const colliding = collidingKeys(rows, ["dev"]);
    expect(colliding.has(rows[0].key)).toBe(true);
  });

  it("flags TWO selected rows that share one name (intra-selection duplicate)", () => {
    const rows = toRows([
      script({ script_name: "dev", package_json_path: "/a/package.json" }),
      script({ script_name: "dev", package_json_path: "/b/package.json" }),
    ]).map((r) => ({ ...r, selected: true, name: "dev" }));
    const colliding = collidingKeys(rows, []);
    expect(colliding.size).toBe(2);
  });

  it("flags an empty name", () => {
    const rows = toRows([script({})]).map((r) => ({ ...r, selected: true, name: "  " }));
    expect(collidingKeys(rows, []).has(rows[0].key)).toBe(true);
  });

  it("does NOT flag an unselected row even if its name collides", () => {
    const rows = toRows([script({ script_name: "dev" })]).map((r) => ({
      ...r,
      selected: false,
      name: "dev",
    }));
    expect(collidingKeys(rows, ["dev"]).size).toBe(0);
  });

  it("importableRows returns only selected, non-colliding rows", () => {
    const rows = toRows([
      script({ script_name: "dev", package_json_path: "/a/package.json" }),
      script({ script_name: "build", package_json_path: "/a/package.json" }),
    ]).map((r, i) => ({ ...r, selected: true, name: i === 0 ? "taken" : "build" }));
    const colliding = collidingKeys(rows, ["taken"]);
    const ready = importableRows(rows, colliding);
    expect(ready).toHaveLength(1);
    expect(ready[0].name).toBe("build");
  });
});

describe("import-utils: already-imported detection (review T3)", () => {
  it("collects the (path, script) identities of every sourced project command", () => {
    const keys = importedSourceKeys([
      command({ source_package_json_path: "/repo/package.json", source_script_name: "dev" }),
      command({ source_package_json_path: "/repo/api/package.json", source_script_name: "start" }),
      // A hand-authored command contributes nothing.
      command({ source_package_json_path: null, source_script_name: null }),
    ]);
    expect(keys.has("/repo/package.json::dev")).toBe(true);
    expect(keys.has("/repo/api/package.json::start")).toBe(true);
    expect(keys.size).toBe(2);
  });

  it("matches a discovered script on BOTH package.json path and script name", () => {
    const keys = importedSourceKeys([
      command({ source_package_json_path: "/repo/package.json", source_script_name: "dev" }),
    ]);
    // Same path + name → already imported.
    expect(isAlreadyImported(script({ package_json_path: "/repo/package.json" }), keys)).toBe(true);
    // Same name, DIFFERENT package.json → not imported.
    expect(isAlreadyImported(script({ package_json_path: "/repo/api/package.json" }), keys)).toBe(
      false,
    );
    // Same package.json, DIFFERENT script → not imported.
    expect(
      isAlreadyImported(
        script({ script_name: "build", package_json_path: "/repo/package.json" }),
        keys,
      ),
    ).toBe(false);
  });
});
