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
 *  - Commands: the shared `CommandsSection`, itself a master/detail since finding
 *    01M132XFZ5KY8QMJ79HZZSBV3J (a catalogue on the left, a detail panel on the right).
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
    // The Commands pane is shown — its ONE list (the tabs went with the master/detail refonte,
    // finding 01M132XFZ5KY8QMJ79HZZSBV3J).
    await waitFor(() => expect(document.querySelector("[data-command-list]")).not.toBeNull());
    expect(screen.queryByRole("tab"), "no tabs any more").toBeNull();
    // The Global rename field is NOT mounted on the Commands section.
    expect(screen.queryByLabelText("Project name")).toBeNull();
  });

  it("navigates between Global and Commands via the rail", async () => {
    render(<ProjectSettingsDialog {...baseProps()} initialSection="global" />);
    const rail = screen.getByRole("navigation", { name: "Project settings sections" });
    // Global → Commands.
    fireEvent.click(within(rail).getByRole("button", { name: "Commands" }));
    await waitFor(() => expect(document.querySelector("[data-command-list]")).not.toBeNull());
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

  it("hides the rename field when showRename=false but keeps the resume toggle", () => {
    // The phase-1 shell entry point passes `showRename={false}` so only the resume toggle is
    // exposed (rename/delete = PRD-B kebab CRUD). The Name field + Save must not render.
    const props = baseProps();
    render(<ProjectSettingsDialog {...props} showRename={false} />);
    expect(screen.queryByLabelText("Project name")).toBeNull();
    expect(screen.queryByRole("button", { name: /save/i })).toBeNull();
    // The resume toggle stays present and functional.
    const toggle = screen.getByRole("switch", { name: /resume agent sessions/i });
    fireEvent.click(toggle);
    expect(props.onResumeChange).toHaveBeenCalledWith(true);
  });
});

/**
 * THE DISMISSAL IS A CROSS IN THE HEADER, AND THE FOOTER IS GONE (human review
 * 01M140VQ4S434P2DMCJSTDR4VM).
 *
 * « le close en bas est quand même très chelou, est-ce qu'on peut virer le footer, et juste avoir
 * une croix dans le header à droite ? » — the footer held a whole band at the bottom of the screen
 * for one button, on a master/detail whose useful height is already counted out.
 *
 * The cross ADDS a path; it replaces none. Escape and the outside press are Base UI's and still
 * close the dialog, so all three are asserted rather than the new one alone.
 */
describe("<ProjectSettingsDialog> — its dismissal", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    installIpc();
  });

  it("closes from a LABELLED cross in the header, and has no footer button left", () => {
    const props = baseProps();
    const { container } = render(<ProjectSettingsDialog {...props} />);

    // NO footer "Close": the band is gone, not merely restyled.
    expect(screen.queryByRole("button", { name: "Close" })).toBeNull();

    // The cross is inside the HEADER — the band that carries the title — and it is named.
    const cross = screen.getByRole("button", { name: "Close project settings" });
    expect(cross, "a real button, hence a tab stop inside the focus trap").toBeTruthy();
    const header = container.ownerDocument.querySelector("[data-dialog-close]")?.parentElement;
    expect(header?.textContent, "it sits in the header, next to the title").toContain(
      "Project settings",
    );

    fireEvent.click(cross);
    expect(props.onClose).toHaveBeenCalled();
  });

  it("still closes on Escape — the cross took no path away", () => {
    const props = baseProps();
    render(<ProjectSettingsDialog {...props} />);
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape", code: "Escape" });
    expect(props.onClose).toHaveBeenCalled();
  });

  it("gives the freed band to the BODY rather than leaving it blank", () => {
    const { container } = render(
      <ProjectSettingsDialog {...baseProps()} initialSection="commands" />,
    );
    // The two heights move together — the dialog's own floor and the section that actually scrolls.
    const body = container.ownerDocument.querySelector("[data-commands-section]") as HTMLElement;
    expect(body.className, "the commands body grew by the footer's band").toContain("h-[32rem]");
    const floor = body.parentElement as HTMLElement;
    expect(floor.className, "and the dialog's floor followed it").toContain("min-h-[32rem]");
  });
});
