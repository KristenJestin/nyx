import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ProjectSettingsDialog } from "./project-settings-dialog";

/**
 * Unit coverage for the project-settings modal (the mini-sidebar refactor of the
 * former "Manage commands" modal). The modal is a left **section rail** (Global /
 * Commands) + a right **detail pane**:
 *  - Global: rename (non-empty + change-required validation) + resume-agent-sessions
 *    toggle, wired to the injected persistence callbacks;
 *  - Commands: the migrated commands UI (shared `CommandsSection`), unchanged.
 *
 * The Commands pane reads `command_list` / `command_import_scripts`, so we install a
 * minimal IPC mock for those; the Global pane talks to the injected `onRename` /
 * `onResumeChange` props (which `terminal-manager` maps to `update_project` /
 * `set_project_resume_agent_sessions`).
 */

function installIpc() {
  mockIPC(
    (cmd) => {
      switch (cmd) {
        case "command_list":
          return [];
        case "command_import_scripts":
          return [];
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}

function baseProps() {
  return {
    open: true,
    projectId: "p1",
    projectName: "Demo",
    resumeAgentSessions: false,
    importWorkspaceId: "w1" as string | null,
    workspacePath: "/demo" as string | null,
    onRename: vi.fn(async () => {}),
    onResumeChange: vi.fn(),
    onClose: vi.fn(),
  };
}

describe("<ProjectSettingsDialog> — section rail", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    installIpc();
  });

  it("does not render while closed", () => {
    render(<ProjectSettingsDialog {...baseProps()} open={false} />);
    expect(screen.queryByText(/Project settings/)).toBeNull();
  });

  it("renders the rail with Global + Commands and opens on the requested section", () => {
    render(<ProjectSettingsDialog {...baseProps()} initialSection="global" />);
    const rail = screen.getByRole("navigation", { name: "Project settings sections" });
    const global = within(rail).getByRole("button", { name: "Global" });
    const commands = within(rail).getByRole("button", { name: "Commands" });
    expect(global).toHaveAttribute("aria-current", "page");
    expect(commands).not.toHaveAttribute("aria-current", "page");
    // The Global pane is shown (its rename field is present).
    expect(screen.getByLabelText("Project name")).toBeInTheDocument();
  });

  it("opens directly on the Commands section when asked", async () => {
    render(<ProjectSettingsDialog {...baseProps()} initialSection="commands" />);
    const rail = screen.getByRole("navigation", { name: "Project settings sections" });
    expect(within(rail).getByRole("button", { name: "Commands" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    // The Commands pane is shown (its Commands tab is present).
    await waitFor(() => expect(screen.getByRole("tab", { name: /commands/i })).toBeInTheDocument());
    // The Global rename field is NOT mounted on the Commands section.
    expect(screen.queryByLabelText("Project name")).toBeNull();
  });

  it("navigates between Global and Commands via the rail", async () => {
    render(<ProjectSettingsDialog {...baseProps()} initialSection="global" />);
    const rail = screen.getByRole("navigation", { name: "Project settings sections" });
    // Global → Commands.
    fireEvent.click(within(rail).getByRole("button", { name: "Commands" }));
    await waitFor(() => expect(screen.getByRole("tab", { name: /commands/i })).toBeInTheDocument());
    expect(screen.queryByLabelText("Project name")).toBeNull();
    // Commands → Global.
    fireEvent.click(within(rail).getByRole("button", { name: "Global" }));
    expect(screen.getByLabelText("Project name")).toBeInTheDocument();
  });
});

describe("<ProjectSettingsDialog> — Global pane", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    installIpc();
  });

  it("disables Save until the name changes (and trims)", () => {
    render(<ProjectSettingsDialog {...baseProps()} />);
    const save = screen.getByRole("button", { name: /save/i });
    // Unchanged name → Save disabled.
    expect(save).toBeDisabled();
    // A whitespace-only edit (trims to empty) → still disabled.
    const input = screen.getByLabelText("Project name");
    fireEvent.change(input, { target: { value: "   " } });
    expect(save).toBeDisabled();
    // A real change → enabled.
    fireEvent.change(input, { target: { value: "Renamed" } });
    expect(save).not.toBeDisabled();
  });

  it("renames via onRename with the trimmed value", async () => {
    const props = baseProps();
    render(<ProjectSettingsDialog {...props} />);
    fireEvent.change(screen.getByLabelText("Project name"), { target: { value: "  Renamed  " } });
    fireEvent.click(screen.getByRole("button", { name: /save/i }));
    await waitFor(() => expect(props.onRename).toHaveBeenCalledTimes(1));
    expect(props.onRename).toHaveBeenCalledWith("Renamed");
  });

  it("surfaces a rename failure inline and keeps the edited value", async () => {
    const props = baseProps();
    props.onRename = vi.fn(async () => {
      throw "name already in use";
    });
    render(<ProjectSettingsDialog {...props} />);
    fireEvent.change(screen.getByLabelText("Project name"), { target: { value: "Renamed" } });
    fireEvent.click(screen.getByRole("button", { name: /save/i }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("name already in use"));
    // The edited value is retained for a retry.
    expect((screen.getByLabelText("Project name") as HTMLInputElement).value).toBe("Renamed");
  });

  it("toggles resume-agent-sessions immediately via onResumeChange", () => {
    const props = baseProps();
    render(<ProjectSettingsDialog {...props} />);
    const toggle = screen.getByRole("switch", { name: /resume agent sessions/i });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    fireEvent.click(toggle);
    expect(props.onResumeChange).toHaveBeenCalledWith(true);
  });

  it("reflects the current resume opt-in from props", () => {
    render(<ProjectSettingsDialog {...baseProps()} resumeAgentSessions />);
    expect(screen.getByRole("switch", { name: /resume agent sessions/i })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });
});
