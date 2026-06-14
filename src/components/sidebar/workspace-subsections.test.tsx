import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { WorkspaceSubsections } from "./workspace-subsections";
import type { CommandRecord } from "./use-projects";
import type { TerminalRecord } from "./use-terminals";

function term(id: string, cwd: string): TerminalRecord {
  return {
    id,
    cwd,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    workspace_id: "ws",
    workspace_binding_mode: "manual",
  };
}

function noop() {}

describe("<WorkspaceSubsections>", () => {
  it("renders drag-reorderable terminal rows (whole-item drag = SortableTerminalItem/dnd-kit wired)", () => {
    render(
      <WorkspaceSubsections
        terminals={[term("t1", "/a"), term("t2", "/b")]}
        activeId={null}
        onSelect={noop}
        onClose={noop}
        onNewTerminal={noop}
        onReorderTerminals={vi.fn()}
      />,
    );
    // Each terminal is a `SortableTerminalItem` (whole-item drag via dnd-kit — no
    // separate grip handle). The rows are `<li>`s inside the list `<ul>`; each
    // carries a selectable name + a Close button.
    const rows = screen.getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    // Whole-item-drag rows expose NO separate "Reorder terminal" grip handle.
    expect(screen.queryByLabelText(/reorder terminal/i)).toBeNull();
    // …but each still has its per-row close affordance (it's a real row).
    expect(screen.getAllByLabelText(/close terminal/i)).toHaveLength(2);
  });

  it("shows a muted hint for an empty Terminaux instead of a bare label", () => {
    render(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        onSelect={noop}
        onClose={noop}
        onNewTerminal={noop}
      />,
    );
    expect(screen.getByText(/no terminals/i)).toBeInTheDocument();
  });

  it("hides the COMMANDS subsection entirely when there are no commands", () => {
    render(
      <WorkspaceSubsections
        terminals={[term("t1", "/a")]}
        activeId={null}
        onSelect={noop}
        onClose={noop}
        onNewTerminal={noop}
      />,
    );
    expect(screen.queryByText("Commands")).toBeNull();
  });

  it("renders the COMMANDS subsection only when commands exist", () => {
    const commands: CommandRecord[] = [{ id: "c1", label: "build" }];
    render(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        commands={commands}
        onSelect={noop}
        onClose={noop}
        onNewTerminal={noop}
      />,
    );
    expect(screen.getByText("Commands")).toBeInTheDocument();
    expect(screen.getByText("build")).toBeInTheDocument();
  });

  it("a command row renders a run-state StatusDot and selects on click", () => {
    const onSelectCommand = vi.fn();
    const commands: CommandRecord[] = [{ id: "c1", label: "build" }];
    render(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        commands={commands}
        activeCommandId={null}
        onSelect={noop}
        onClose={noop}
        onSelectCommand={onSelectCommand}
        onNewTerminal={noop}
      />,
    );
    // The command's lead StatusDot reports its (idle) run-state.
    expect(screen.getByLabelText(/status: idle/i)).toBeInTheDocument();
    fireEvent.click(screen.getByText("build"));
    expect(onSelectCommand).toHaveBeenCalledWith("c1");
  });

  it("the TERMINALS subsection is NON-collapsible: no chevron toggle, always open (finding 01KV3CNH1…)", () => {
    render(
      <WorkspaceSubsections
        terminals={[term("t1", "/a"), term("t2", "/b")]}
        activeId={null}
        onSelect={noop}
        onClose={noop}
        onNewTerminal={noop}
        onReorderTerminals={vi.fn()}
      />,
    );
    // The label is plain text, not a toggle button — there is NO Terminals
    // chevron/collapse control (a prior round wrongly added one).
    expect(screen.queryByRole("button", { name: /^terminals$/i })).toBeNull();
    expect(screen.getByText(/^terminals$/i)).toBeInTheDocument();
    // The rows are always present (cannot be collapsed away).
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });

  it("does NOT render a terminal COUNT beside the TERMINALS label", () => {
    render(
      <WorkspaceSubsections
        terminals={[term("t1", "/a"), term("t2", "/b"), term("t3", "/c")]}
        activeId={null}
        onSelect={noop}
        onClose={noop}
        onNewTerminal={noop}
        onReorderTerminals={vi.fn()}
      />,
    );
    // The small count (e.g. "3") that used to sit next to the label is gone:
    // the header span holds ONLY the "Terminals" label, no numeric badge.
    const label = screen.getByText(/^terminals$/i);
    const labelHeader = label.parentElement as HTMLElement;
    expect(within(labelHeader).queryByText("3")).toBeNull();
  });

  it("the new-terminal + is tooltip-wrapped and fires onNewTerminal", () => {
    const onNewTerminal = vi.fn();
    render(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        onSelect={noop}
        onClose={noop}
        onNewTerminal={onNewTerminal}
      />,
    );
    const plus = screen.getByRole("button", {
      name: /new terminal in workspace/i,
    });
    // The tooltip wrapper keeps the button's accessible label intact.
    expect(plus).toBeInTheDocument();
    // The '+' sits in the TERMINALS subsection header alongside the label.
    const label = screen.getByText(/^terminals$/i);
    const header = label.parentElement?.parentElement as HTMLElement;
    expect(within(header).getByRole("button", { name: /new terminal in workspace/i })).toBe(plus);
    fireEvent.click(plus);
    expect(onNewTerminal).toHaveBeenCalled();
  });
});
