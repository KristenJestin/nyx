import { useState } from "react";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CommandImportSection } from "./command-import-section";
import { importedSourceKeys, toRows, type ImportRow } from "./import-utils";
import type { DiscoveredScript, ManagedCommand } from "./use-commands";

function script(over: Partial<DiscoveredScript>): DiscoveredScript {
  return {
    proposed_name: over.script_name ?? "dev",
    script_name: "dev",
    default_command: "pnpm dev",
    script_command_snapshot: "vite",
    subfolder: "",
    package_json_path: "/repo/package.json",
    package_manager: "pnpm",
    ...over,
  };
}

const TWO_GROUPS: DiscoveredScript[] = [
  script({ script_name: "dev", proposed_name: "dev", package_json_path: "/repo/package.json" }),
  script({
    script_name: "build",
    proposed_name: "build",
    default_command: "pnpm build",
    package_json_path: "/repo/package.json",
  }),
  script({
    script_name: "start",
    proposed_name: "start",
    default_command: "npm run start",
    package_json_path: "/repo/api/package.json",
    subfolder: "api",
    package_manager: "npm",
  }),
];

/**
 * Controlled test harness: the section is presentational now, so the test owns
 * the editable rows + selection (mirroring the dialog / `useImportRows`).
 */
function Harness({
  scripts,
  existingNames = [],
  importedCommands = [],
}: {
  scripts: DiscoveredScript[];
  existingNames?: string[];
  importedCommands?: ManagedCommand[];
}) {
  const [rows, setRows] = useState<ImportRow[]>(() => toRows(scripts));
  const importedKeys = importedSourceKeys(importedCommands);
  return (
    <CommandImportSection
      rows={rows}
      existingNames={existingNames}
      importedKeys={importedKeys}
      onPatchRow={(key, next) =>
        setRows((prev) => prev.map((r) => (r.key === key ? { ...r, ...next } : r)))
      }
      onSelectGroup={(path, selected) =>
        setRows((prev) =>
          prev.map((r) => (r.script.package_json_path === path ? { ...r, selected } : r)),
        )
      }
    />
  );
}

function command(over: Partial<ManagedCommand>): ManagedCommand {
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

describe("<CommandImportSection> (import from package.json)", () => {
  it("renders one group per package.json with its detected package manager", () => {
    render(<Harness scripts={TWO_GROUPS} />);
    const managers = screen.getAllByTestId("package-manager").map((e) => e.textContent);
    expect(managers).toContain("pnpm");
    expect(managers).toContain("npm");
  });

  it("has editable name + command inputs per script", () => {
    render(<Harness scripts={TWO_GROUPS} />);
    const nameInput = screen.getByLabelText("Name for dev") as HTMLInputElement;
    expect(nameInput.value).toBe("dev");
    fireEvent.change(nameInput, { target: { value: "dev-server" } });
    expect((screen.getByLabelText("Name for dev") as HTMLInputElement).value).toBe("dev-server");
    const cmdInput = screen.getByLabelText("Command for dev") as HTMLInputElement;
    expect(cmdInput.value).toBe("pnpm dev");
  });

  it("select-all toggles every script in its group", () => {
    render(<Harness scripts={TWO_GROUPS} />);
    const selectAll = screen.getByLabelText("Select all scripts in root");
    fireEvent.click(selectAll);
    expect(screen.getByLabelText("Select script dev")).toHaveAttribute("aria-checked", "true");
    expect(screen.getByLabelText("Select script build")).toHaveAttribute("aria-checked", "true");
  });

  it("flags an inline collision when a selected name already exists in the project", () => {
    render(<Harness scripts={TWO_GROUPS} existingNames={["dev"]} />);
    fireEvent.click(screen.getByLabelText("Select script dev"));
    expect(screen.getByTestId("collision-error")).toHaveTextContent(/already exists/i);
  });

  it("clears the collision once the colliding name is renamed to a unique one", () => {
    render(<Harness scripts={TWO_GROUPS} existingNames={["dev"]} />);
    fireEvent.click(screen.getByLabelText("Select script dev"));
    expect(screen.getByTestId("collision-error")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Name for dev"), { target: { value: "dev-2" } });
    expect(screen.queryByTestId("collision-error")).toBeNull();
  });

  it("flags both rows when two selected rows share one name", () => {
    const both: DiscoveredScript[] = [
      script({ script_name: "dev", proposed_name: "dev", package_json_path: "/a/package.json" }),
      script({
        script_name: "dev",
        proposed_name: "dev",
        package_json_path: "/b/package.json",
        subfolder: "b",
      }),
    ];
    render(<Harness scripts={both} />);
    const checks = screen.getAllByLabelText("Select script dev");
    fireEvent.click(checks[0]);
    fireEvent.click(checks[1]);
    expect(screen.getAllByTestId("collision-error").length).toBe(2);
  });

  it("shows an empty hint when there are no scripts", () => {
    render(<Harness scripts={[]} />);
    expect(screen.getByText(/no package.json scripts/i)).toBeInTheDocument();
  });

  it("scopes the package-manager badge to the matching group", () => {
    render(<Harness scripts={TWO_GROUPS} />);
    // The package-manager badge lives in the group's collapsible HEADER button,
    // next to its package.json path label.
    const apiHeader = screen.getByText("api/package.json").closest("button")!;
    expect(within(apiHeader).getByTestId("package-manager")).toHaveTextContent("npm");
  });

  // === Review T3: already-imported scripts are greyed + not selectable ========
  it("greys an already-imported script (matched by source path+name) with 'already imported'", () => {
    // A project command sourced from /repo/package.json scripts.dev → the dev row
    // is already imported.
    render(
      <Harness
        scripts={TWO_GROUPS}
        importedCommands={[
          command({ source_package_json_path: "/repo/package.json", source_script_name: "dev" }),
        ]}
      />,
    );
    expect(screen.getByTestId("already-imported")).toBeInTheDocument();
    // Its checkbox is disabled (not selectable). Base UI renders a
    // `role="checkbox"` span with `aria-disabled` / `data-disabled`.
    expect(screen.getByLabelText("Select script dev")).toHaveAttribute("aria-disabled", "true");
    // Its inputs are disabled too.
    expect(screen.getByLabelText("Name for dev")).toBeDisabled();
    // A non-imported sibling (build) stays selectable.
    expect(screen.getByLabelText("Select script build")).not.toHaveAttribute(
      "aria-disabled",
      "true",
    );
  });

  it("an already-imported script cannot be selected (no collision-error path)", () => {
    render(
      <Harness
        scripts={TWO_GROUPS}
        importedCommands={[
          command({ source_package_json_path: "/repo/package.json", source_script_name: "dev" }),
        ]}
      />,
    );
    // Clicking the disabled checkbox does nothing → never selected, never collides.
    fireEvent.click(screen.getByLabelText("Select script dev"));
    expect(screen.getByLabelText("Select script dev")).toHaveAttribute("aria-checked", "false");
    expect(screen.queryByTestId("collision-error")).toBeNull();
  });

  it("the group header counts importable vs imported", () => {
    render(
      <Harness
        scripts={TWO_GROUPS}
        importedCommands={[
          command({ source_package_json_path: "/repo/package.json", source_script_name: "dev" }),
        ]}
      />,
    );
    // The root group: dev imported, build importable → "1 importable · 1 imported".
    expect(screen.getByText(/1 importable · 1 imported/)).toBeInTheDocument();
  });
});
