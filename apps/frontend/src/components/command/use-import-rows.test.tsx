import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useImportRows } from "./use-import-rows";
import { rowKey } from "./import-utils";
import type { DiscoveredScript } from "./use-commands";

function script(over: Partial<DiscoveredScript>): DiscoveredScript {
  return {
    proposed_name: over.proposed_name ?? over.script_name ?? "dev",
    script_name: over.script_name ?? "dev",
    default_command: "pnpm dev",
    script_command_snapshot: "vite",
    subfolder: "",
    package_json_path: "/repo/package.json",
    package_manager: "pnpm",
    ...over,
  };
}

// Two package.json files. The SAME script name `dev` appears in both — the backend
// disambiguates the proposed name (`dev` vs `api:dev`, see pkgjson.rs), and the row
// KEY is `(package_json_path, script_name)`, so the two rows are distinct and never
// collide. This is the exact "select across multiple package.json" case of #1.
const ROOT_DEV = script({
  script_name: "dev",
  proposed_name: "dev",
  package_json_path: "/repo/package.json",
});
const API_DEV = script({
  script_name: "dev",
  proposed_name: "api:dev",
  subfolder: "api",
  package_json_path: "/repo/api/package.json",
});

describe("useImportRows — multi-package.json selection (#1)", () => {
  it("accumulates selection across two package.json files (no last-file-wins)", () => {
    const { result } = renderHook(() => useImportRows([ROOT_DEV, API_DEV], [], []));

    act(() => result.current.patch(rowKey(ROOT_DEV), { selected: true }));
    act(() => result.current.patch(rowKey(API_DEV), { selected: true }));

    expect(result.current.selectedCount).toBe(2);
    expect(result.current.ready.map((r) => r.key).sort()).toEqual(
      [rowKey(ROOT_DEV), rowKey(API_DEV)].sort(),
    );
    expect(result.current.blocked).toBe(false);
  });

  it("keeps selection across a re-render with a fresh array of the SAME scripts", () => {
    const { result, rerender } = renderHook(({ s }) => useImportRows(s, [], []), {
      initialProps: { s: [ROOT_DEV, API_DEV] },
    });
    act(() => result.current.patch(rowKey(ROOT_DEV), { selected: true }));

    // New array reference but identical keys → identity unchanged → no re-seed.
    rerender({ s: [{ ...ROOT_DEV }, { ...API_DEV }] });

    expect(result.current.selectedCount).toBe(1);
    expect(result.current.rows.find((r) => r.key === rowKey(ROOT_DEV))?.selected).toBe(true);
  });

  it("preserves prior selections + edits when discovery GROWS with another package.json", () => {
    const { result, rerender } = renderHook(({ s }) => useImportRows(s, [], []), {
      initialProps: { s: [ROOT_DEV] },
    });
    act(() => result.current.patch(rowKey(ROOT_DEV), { selected: true, name: "web-dev" }));

    // A second package.json is discovered and appended → identity changes → re-seed.
    // The merge must keep ROOT_DEV's selection + edited name; the new row seeds fresh.
    rerender({ s: [ROOT_DEV, API_DEV] });

    const root = result.current.rows.find((r) => r.key === rowKey(ROOT_DEV));
    const api = result.current.rows.find((r) => r.key === rowKey(API_DEV));
    expect(root?.selected).toBe(true);
    expect(root?.name).toBe("web-dev");
    expect(api?.selected).toBe(false);
    expect(result.current.selectedCount).toBe(1);
  });

  it("drops a row whose script vanished from a later discovery", () => {
    const { result, rerender } = renderHook(({ s }) => useImportRows(s, [], []), {
      initialProps: { s: [ROOT_DEV, API_DEV] },
    });
    act(() => result.current.patch(rowKey(API_DEV), { selected: true }));

    rerender({ s: [ROOT_DEV] });

    expect(result.current.rows.map((r) => r.key)).toEqual([rowKey(ROOT_DEV)]);
    expect(result.current.selectedCount).toBe(0);
  });
});
