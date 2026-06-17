import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
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

  it("a command row uses the SHARED item gabarit: same px-2 row shape, NO pl-5.5 inset", () => {
    const commands: CommandRecord[] = [{ id: "c1", label: "build" }];
    render(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        commands={commands}
        activeCommandId={null}
        onSelect={noop}
        onClose={noop}
        onSelectCommand={noop}
        onNewTerminal={noop}
      />,
    );
    // The command row is the shared sidebar item shape: the old command-specific
    // left inset (`pl-5.5`, which pushed it right of the terminals) is gone, and it
    // carries the same `px-2`/`py-1.5`/`rounded-md`/`gap-2` row gabarit as a terminal.
    // The row is a `div[role=button]` carrying `[data-rail-row]` (the controls inside
    // are `<button>`s, so the row itself must NOT be one).
    const row = screen.getByText("build").closest("[data-rail-row]") as HTMLElement;
    expect(row.tagName).toBe("DIV");
    expect(row.className).not.toMatch(/pl-5\.5/);
    expect(row.className).toContain("px-2");
    expect(row.className).toContain("py-1.5");
    expect(row.className).toContain("rounded-md");
    expect(row.className).toContain("gap-2");
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

  it("the settled badge follows `unread` while the row reflects the factual state (v4)", () => {
    // An UNREAD error: the settled badge is visible (the dot fill is destructive),
    // and the factual state is reported on the dot.
    const { rerender } = render(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        commands={[{ id: "c1", label: "build", state: "error", unread: true }]}
        activeCommandId={null}
        onSelect={noop}
        onClose={noop}
        onSelectCommand={noop}
        onNewTerminal={noop}
      />,
    );
    let dot = screen.getByRole("status", { name: /status: error/i });
    // Factual state is observable, and the visible fill is the error badge.
    expect(dot).toHaveAttribute("data-state", "error");
    expect(dot.className).toContain("bg-destructive");

    // After acknowledge (unread=false): the settled badge HIDES (fill reverts to the
    // neutral idle muted fill) while the row STILL reflects the factual error state.
    rerender(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        commands={[{ id: "c1", label: "build", state: "error", unread: false }]}
        activeCommandId={null}
        onSelect={noop}
        onClose={noop}
        onSelectCommand={noop}
        onNewTerminal={noop}
      />,
    );
    dot = screen.getByRole("status", { name: /status: error/i });
    expect(dot).toHaveAttribute("data-state", "error"); // factual state preserved
    expect(dot.className).toContain("bg-muted-foreground/50"); // badge hidden
    expect(dot.className).not.toContain("bg-destructive");
  });

  it("a command row shows start/stop/relaunch icons with the SAME gating as the view (finding 01KV63TEGB…)", () => {
    mockIPC(() => null, { shouldMockEvents: true });
    // idle command: start + relaunch enabled, stop disabled.
    const { rerender } = render(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        commands={[{ id: "c1", label: "build", state: "idle" }]}
        onSelect={noop}
        onClose={noop}
        onSelectCommand={noop}
        onNewTerminal={noop}
      />,
    );
    expect(screen.getByRole("button", { name: /start command/i })).not.toBeDisabled();
    expect(screen.getByRole("button", { name: /stop command/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /relaunch command/i })).not.toBeDisabled();

    // running command: stop enabled, start disabled, relaunch always enabled.
    rerender(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        commands={[{ id: "c1", label: "build", state: "running" }]}
        onSelect={noop}
        onClose={noop}
        onSelectCommand={noop}
        onNewTerminal={noop}
      />,
    );
    expect(screen.getByRole("button", { name: /start command/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /stop command/i })).not.toBeDisabled();
    expect(screen.getByRole("button", { name: /relaunch command/i })).not.toBeDisabled();
  });

  it("clicking a command row's Start invokes command_start WITHOUT selecting the row", async () => {
    const calls: string[] = [];
    mockIPC(
      (cmd) => {
        calls.push(cmd);
        if (cmd === "command_start") return "running";
        return null;
      },
      { shouldMockEvents: true },
    );
    const onSelectCommand = vi.fn();
    render(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        commands={[{ id: "c1", label: "build", state: "idle" }]}
        onSelect={noop}
        onClose={noop}
        onSelectCommand={onSelectCommand}
        onNewTerminal={noop}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /start command/i }));
    await waitFor(() => expect(calls).toContain("command_start"));
    // Acting on the command must not also select the row (stopPropagation).
    expect(onSelectCommand).not.toHaveBeenCalled();
  });

  it("the COMMANDS subsection is NON-collapsible too: no chevron toggle on its label", () => {
    const commands: CommandRecord[] = [{ id: "c1", label: "build" }];
    render(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        commands={commands}
        onSelect={noop}
        onClose={noop}
        onSelectCommand={noop}
        onNewTerminal={noop}
      />,
    );
    // The Commands label is plain text, not a collapse toggle button (the chevron +
    // CollapsibleSection were removed — finding 01KV63TD5E…).
    expect(screen.queryByRole("button", { name: /^commands$/i })).toBeNull();
    expect(screen.getByText(/^commands$/i)).toBeInTheDocument();
    // The rows are always present (cannot be collapsed away).
    expect(screen.getByText("build")).toBeInTheDocument();
  });

  it("the COMMANDS band shows a GEAR that opens the manage-commands modal", () => {
    const onManageCommands = vi.fn();
    const commands: CommandRecord[] = [{ id: "c1", label: "build" }];
    render(
      <WorkspaceSubsections
        terminals={[]}
        activeId={null}
        commands={commands}
        onSelect={noop}
        onClose={noop}
        onSelectCommand={noop}
        onManageCommands={onManageCommands}
        onNewTerminal={noop}
      />,
    );
    const gear = screen.getByRole("button", { name: /manage commands/i });
    expect(gear).toBeInTheDocument();
    fireEvent.click(gear);
    expect(onManageCommands).toHaveBeenCalledTimes(1);
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
