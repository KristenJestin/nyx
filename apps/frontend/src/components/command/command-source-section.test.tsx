import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CommandSourceSection } from "./command-source-section";
import type { ManagedCommand } from "./use-commands";

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

describe("<CommandSourceSection> (read-only package.json provenance)", () => {
  it("renders the package.json path + script reference", () => {
    render(<CommandSourceSection command={cmd({})} />);
    expect(screen.getByText("/repo/package.json")).toBeInTheDocument();
    expect(screen.getByText("package.json · scripts.dev")).toBeInTheDocument();
  });

  it("renders NO source actions (mutations live in the edit form, not the card)", () => {
    render(<CommandSourceSection command={cmd({})} />);
    // No buttons at all on the read-only provenance strip.
    expect(screen.queryByRole("button")).toBeNull();
    // The removed actions are gone for good.
    expect(screen.queryByText(/reset to script runner/i)).toBeNull();
    expect(screen.queryByText(/swap to raw script/i)).toBeNull();
    expect(screen.queryByText(/refresh source/i)).toBeNull();
  });

  it("shows a passive 'changed in package.json' hint when drifted", () => {
    render(<CommandSourceSection command={cmd({})} driftValue="node server.js" />);
    expect(screen.getByText(/changed in package\.json/i)).toBeInTheDocument();
    expect(screen.getByText("node server.js")).toBeInTheDocument();
  });

  it("shows the neutral 'open Edit to resync or unlink' hint when in sync", () => {
    render(<CommandSourceSection command={cmd({})} driftValue={null} />);
    expect(screen.getByText(/open edit to resync or unlink/i)).toBeInTheDocument();
  });

  it("renders nothing for a hand-authored (un-sourced) command", () => {
    const { container } = render(
      <CommandSourceSection
        command={cmd({ source_script_name: null, source_package_json_path: null })}
      />,
    );
    expect(container.firstChild).toBeNull();
  });
});
