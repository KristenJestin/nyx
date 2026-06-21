import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { AppSidebar, railKey } from "./app-sidebar";
import {
  showWorkspaceSection,
  workspaceDisplayLabel,
  defaultWorkspaceLabel,
} from "./project-item.utils";
import type { ProjectTree, WorkspaceRecord } from "./use-projects";
import type { TerminalRecord } from "./use-terminals";

function ws(
  id: string,
  projectId: string,
  name: string,
  path: string,
  isRoot: boolean,
): WorkspaceRecord {
  return {
    id,
    project_id: projectId,
    name,
    path,
    branch: null,
    is_root: isRoot,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
  };
}

function tree(projectId: string, name: string, workspaces: WorkspaceRecord[]): ProjectTree {
  return {
    project: { id: projectId, name, collapsed: false, created_at: 0, updated_at: 0 , resume_agent_sessions: false },
    workspaces,
  };
}

function term(id: string, cwd: string, workspaceId: string | null): TerminalRecord {
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
    workspace_id: workspaceId,
    workspace_binding_mode: "manual",
  };
}

function noop() {}

function renderSidebar(
  projects: ProjectTree[],
  terminals: TerminalRecord[] = [],
  overrides: Partial<Parameters<typeof AppSidebar>[0]> = {},
) {
  const onNewTerminal = vi.fn();
  const onNewLooseTerminal = vi.fn();
  const onAddProject = vi.fn();
  const onAddWorkspace = vi.fn();
  const onEditProject = vi.fn();
  const onDeleteProject = vi.fn();
  render(
    <AppSidebar
      projects={projects}
      terminals={terminals}
      activeId={null}
      onSelect={noop}
      onClose={noop}
      onNewTerminal={onNewTerminal}
      onNewLooseTerminal={onNewLooseTerminal}
      onAddProject={onAddProject}
      onAddWorkspace={onAddWorkspace}
      onEditProject={onEditProject}
      onDeleteProject={onDeleteProject}
      {...overrides}
    />,
  );
  return {
    onNewTerminal,
    onNewLooseTerminal,
    onAddProject,
    onAddWorkspace,
    onEditProject,
    onDeleteProject,
  };
}

describe("railKey (selection-rail key follows the active terminal OR command)", () => {
  it("uses the active TERMINAL id when one is active", () => {
    expect(railKey("term-1", null)).toBe("term-1");
    // A terminal being active wins even if a command id lingers (it should not).
    expect(railKey("term-1", "cmd-9")).toBe("term-1");
  });

  it("falls back to the active COMMAND id when no terminal is active", () => {
    // This is the fix: while a command is active the sidebar forces activeId=null,
    // so the rail key must follow activeCommandId — otherwise command→command keeps
    // the key null both times and the rail never re-glides (review 01KV6F1B…).
    expect(railKey(null, "cmd-1")).toBe("cmd-1");
    // command→command: the key CHANGES, so `useActiveRail` re-glides.
    expect(railKey(null, "cmd-1")).not.toBe(railKey(null, "cmd-2"));
  });

  it("is null when nothing is selected", () => {
    expect(railKey(null, null)).toBeNull();
    expect(railKey(null, undefined)).toBeNull();
  });
});

describe("showWorkspaceSection (optional workspace section)", () => {
  it("is HIDDEN for a mono-(root)workspace project", () => {
    expect(showWorkspaceSection([ws("w", "p", "root", "/p", true)])).toBe(false);
  });
  it("is VISIBLE when the project has more than one workspace", () => {
    expect(
      showWorkspaceSection([
        ws("w1", "p", "root", "/p", true),
        ws("w2", "p", "feat", "/p/feat", false),
      ]),
    ).toBe(true);
  });
});

describe("workspaceDisplayLabel (relabel root as 'main', no duplication)", () => {
  it("shows the ROOT as 'main' regardless of its backend default/folder name", () => {
    // Backend default 'root', a stale copy of the project name, or empty all
    // collapse to the smart 'main'.
    expect(workspaceDisplayLabel(ws("w", "p", "root", "/p", true), "music-manager")).toBe("main");
    expect(workspaceDisplayLabel(ws("w", "p", "music-manager", "/p", true), "music-manager")).toBe(
      "main",
    );
    expect(workspaceDisplayLabel(ws("w", "p", "", "/p", true), "proj")).toBe("main");
  });
  it("honours a user-set custom root name", () => {
    expect(workspaceDisplayLabel(ws("w", "p", "primary", "/p", true), "proj")).toBe("primary");
  });
  it("shows a non-root workspace's own name", () => {
    expect(workspaceDisplayLabel(ws("w", "p", "feature-x", "/p/feat", false), "proj")).toBe(
      "feature-x",
    );
  });
});

describe("defaultWorkspaceLabel (smart distinguishing default)", () => {
  it("uses the path segment relative to the project root when nested", () => {
    expect(defaultWorkspaceLabel("/proj/apps/web", "/proj")).toBe("apps/web");
    expect(defaultWorkspaceLabel("C:\\proj\\apps\\web", "C:\\proj")).toBe("apps/web");
  });
  it("falls back to the folder basename when not under the root", () => {
    expect(defaultWorkspaceLabel("/elsewhere/thing", "/proj")).toBe("thing");
  });
});

describe("<AppSidebar> variant A spine", () => {
  it("renders an Add project button, a Settings gear (head), and a TERMINALS-footer new-terminal +", () => {
    const onAddProject = vi.fn();
    const onNewLooseTerminal = vi.fn();
    const onOpenSettings = vi.fn();
    render(
      <AppSidebar
        projects={[]}
        terminals={[]}
        activeId={null}
        onSelect={noop}
        onClose={noop}
        onNewTerminal={noop}
        onNewLooseTerminal={onNewLooseTerminal}
        onAddProject={onAddProject}
        onAddWorkspace={noop}
        onOpenSettings={onOpenSettings}
      />,
    );
    // Add project button works.
    fireEvent.click(screen.getByRole("button", { name: /add project/i }));
    expect(onAddProject).toHaveBeenCalledTimes(1);
    // The HEAD gear opens Settings (not a new terminal anymore).
    fireEvent.click(screen.getByRole("button", { name: /^settings$/i }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
    // The TERMINALS footer '+' is still there and opens a loose terminal.
    fireEvent.click(screen.getByRole("button", { name: /new unattached terminal/i }));
    expect(onNewLooseTerminal).toHaveBeenCalledTimes(1);
  });

  it("mono-workspace project shows NO workspace row and NO 'main' row (shallow)", () => {
    renderSidebar([tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)])]);
    // The project row exists (name shown once).
    expect(screen.getByText("Solo")).toBeInTheDocument();
    // No 'main' row (the mono-root stays shallow — root is implicit).
    expect(screen.queryByText("main")).toBeNull();
    // The TERMINALS subsection (a NON-collapsible plain label now — finding
    // 01KV3CNH1…) renders directly under the project. There are TWO "Terminals"
    // labels: the workspace subsection + the pinned footer band.
    expect(screen.getAllByText(/^terminals$/i).length).toBeGreaterThanOrEqual(1);
  });

  it("multi-workspace project shows a 'main' root row + named rows, name once", () => {
    renderSidebar([
      tree("p1", "music-manager", [
        ws("w1", "p1", "music-manager", "/multi", true), // legacy dup name
        ws("w2", "p1", "feature-x", "/multi/feat", false),
      ]),
    ]);
    // The project name appears exactly ONCE (the header), never repeated as a row.
    expect(screen.getAllByText("music-manager")).toHaveLength(1);
    // The root is relabeled 'main'; the other workspace keeps its name.
    expect(screen.getByText("main")).toBeInTheDocument();
    expect(screen.getByText("feature-x")).toBeInTheDocument();
    // Each workspace has its own TERMINALS subsection label (NON-collapsible, a
    // plain label now — finding 01KV3CNH1…): two subsections + the footer band =
    // at least two "Terminals" labels.
    expect(screen.getAllByText(/^terminals$/i).length).toBeGreaterThanOrEqual(2);
  });

  it("groups terminals under the correct workspace's Terminals subsection", () => {
    renderSidebar(
      [
        tree("p1", "Multi", [
          ws("w1", "p1", "root", "/multi", true),
          ws("w2", "p1", "feat", "/multi/feat", false),
        ]),
      ],
      [
        term("t1", "/rootcwd", "w1"),
        term("t2", "/featcwd", "w2"),
        term("t3", "/elsewhere", null), // unattached → in the loose TERMINALS section
      ],
    );
    expect(screen.getByText("rootcwd")).toBeInTheDocument(); // t1 under main
    expect(screen.getByText("featcwd")).toBeInTheDocument(); // t2 under feat
    // t3 (unattached) shows in the top-level TERMINALS section, not absent.
    expect(screen.getByText("elsewhere")).toBeInTheDocument();
  });

  it("collapses a project body when its header toggle is clicked", () => {
    renderSidebar([
      tree("p1", "Multi", [
        ws("w1", "p1", "root", "/multi", true),
        ws("w2", "p1", "feat", "/multi/feat", false),
      ]),
    ]);
    const projectToggle = screen
      .getAllByRole("button", { name: /multi/i })
      .find((b) => b.hasAttribute("aria-expanded"))!;
    expect(screen.getByText("feat")).toBeInTheDocument();
    expect(projectToggle).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(projectToggle);
    expect(projectToggle).toHaveAttribute("aria-expanded", "false");
  });

  it("restores a project band CLOSED from its persisted `collapsed` flag", () => {
    // A project persisted as collapsed mounts CLOSED (open = !collapsed) so the
    // disclosure survives a reload.
    const t = tree("p1", "Closed", [ws("w", "p1", "root", "/closed", true)]);
    t.project.collapsed = true;
    renderSidebar([t]);
    const projectToggle = screen
      .getAllByRole("button", { name: /closed/i })
      .find((b) => b.hasAttribute("aria-expanded"))!;
    expect(projectToggle).toHaveAttribute("aria-expanded", "false");
  });

  it("toggling a project band PERSISTS the new collapsed state via onSetProjectCollapsed", () => {
    const onSetProjectCollapsed = vi.fn();
    renderSidebar([tree("p1", "Persisty", [ws("w", "p1", "root", "/p", true)])], [], {
      onSetProjectCollapsed,
    });
    const projectToggle = screen
      .getAllByRole("button", { name: /persisty/i })
      .find((b) => b.hasAttribute("aria-expanded"))!;
    // Open → collapsing it persists collapsed=true.
    fireEvent.click(projectToggle);
    expect(onSetProjectCollapsed).toHaveBeenLastCalledWith("p1", true);
    // Re-opening persists collapsed=false.
    fireEvent.click(projectToggle);
    expect(onSetProjectCollapsed).toHaveBeenLastCalledWith("p1", false);
  });

  it("restores a workspace band CLOSED from its persisted `collapsed` flag + persists toggles", () => {
    const onSetWorkspaceCollapsed = vi.fn();
    const root = ws("w1", "p1", "root", "/multi", true);
    const feat = ws("w2", "p1", "feat", "/multi/feat", false);
    feat.collapsed = true; // the feat workspace was left collapsed
    renderSidebar([tree("p1", "Multi", [root, feat])], [], { onSetWorkspaceCollapsed });
    const featToggle = screen.getByRole("button", { name: /toggle workspace feat/i });
    // It mounts CLOSED (restored from collapsed=true).
    expect(featToggle).toHaveAttribute("aria-expanded", "false");
    // Opening it persists collapsed=false for that workspace id.
    fireEvent.click(featToggle);
    expect(onSetWorkspaceCollapsed).toHaveBeenLastCalledWith("w2", false);
  });

  it("clicking a workspace's Terminals + launches a terminal IN that workspace", () => {
    const { onNewTerminal } = renderSidebar([
      tree("p1", "Multi", [
        ws("w1", "p1", "root", "/multi", true),
        ws("w2", "p1", "feat", "/multi/feat", false),
      ]),
    ]);
    const plusButtons = screen.getAllByRole("button", {
      name: /new terminal in workspace/i,
    });
    expect(plusButtons).toHaveLength(2); // one per workspace
    fireEvent.click(plusButtons[1]); // feat's '+'
    expect(onNewTerminal).toHaveBeenCalledTimes(1);
    expect(onNewTerminal.mock.calls[0][0]).toMatchObject({
      id: "w2",
      path: "/multi/feat",
    });
  });

  it("the per-project edit/delete actions fire their handlers (via the kebab menu)", async () => {
    const { onEditProject, onDeleteProject } = renderSidebar([
      tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)]),
    ]);
    // The inline action icons are now consolidated into ONE hover-revealed kebab
    // menu (finding 01KV1NPRZV97GVT1GKWTT6NH25). Open it, then click Rename.
    const kebab = screen.getByRole("button", {
      name: /project actions for Solo/i,
    });
    // Base UI's Menu trigger opens on the pointer-down sequence (not a bare
    // click), so fire the full gesture.
    const openMenu = () => {
      fireEvent.pointerDown(kebab, { button: 0 });
      fireEvent.pointerUp(kebab, { button: 0 });
      fireEvent.click(kebab);
    };
    openMenu();
    fireEvent.click(await screen.findByRole("menuitem", { name: /rename project Solo/i }));
    expect(onEditProject).toHaveBeenCalledTimes(1);
    // Reopen the menu (it closed on the first item click) and click Delete.
    openMenu();
    fireEvent.click(await screen.findByRole("menuitem", { name: /delete project Solo/i }));
    expect(onDeleteProject).toHaveBeenCalledTimes(1);
  });

  it("the project kebab carries a 'Manage commands' action (PRD-3 modal trigger)", async () => {
    const onManageCommands = vi.fn();
    renderSidebar([tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)])], [], {
      onManageCommands,
    });
    const kebab = screen.getByRole("button", { name: /project actions for Solo/i });
    fireEvent.pointerDown(kebab, { button: 0 });
    fireEvent.pointerUp(kebab, { button: 0 });
    fireEvent.click(kebab);
    fireEvent.click(await screen.findByRole("menuitem", { name: /manage commands for Solo/i }));
    expect(onManageCommands).toHaveBeenCalledTimes(1);
  });

  it("renders the COMMANDS subsection from commandsByWorkspace + selects a command", () => {
    const onSelectCommand = vi.fn();
    const commandsByWorkspace = new Map([
      ["w", [{ id: "inst-1", label: "dev", state: "running" as const }]],
    ]);
    renderSidebar([tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)])], [], {
      commandsByWorkspace,
      onSelectCommand,
    });
    // The COMMANDS band appears (it was hidden when empty) with the command row.
    expect(screen.getByText("Commands")).toBeInTheDocument();
    const row = screen.getByText("dev");
    fireEvent.click(row);
    expect(onSelectCommand).toHaveBeenCalledWith("inst-1");
  });
});

describe("empty-state polish", () => {
  it("hides the COMMANDS subsection entirely when there are no commands", () => {
    renderSidebar([tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)])]);
    expect(screen.queryByText("Commands")).toBeNull();
  });

  it("shows a muted hint for an empty TERMINALS instead of a bare label", () => {
    renderSidebar([tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)])]);
    // The hint appears for the empty workspace subsection AND the empty pinned
    // footer (no loose terminals) — at least one is present.
    expect(screen.getAllByText(/no terminals/i).length).toBeGreaterThanOrEqual(1);
  });
});

describe("loose TERMINALS section (pinned footer)", () => {
  it("renders unattached terminals under the pinned TERMINALS footer", () => {
    renderSidebar(
      [tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)])],
      [term("loose1", "/loosecwd", null)],
    );
    // The pinned footer's TERMINALS band carries the new-unattached-terminal '+'
    // (the 'unattached' hint text was removed — finding 01KV3CP2S…).
    expect(screen.getByRole("button", { name: /new unattached terminal/i })).toBeInTheDocument();
    expect(screen.getByText("loosecwd")).toBeInTheDocument();
  });

  it("renders PROJECTS above the pinned TERMINALS footer in document order", () => {
    renderSidebar(
      [tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)])],
      [term("loose1", "/loosecwd", null)],
    );
    const projectsHeader = screen.getByText("Projects", { selector: "span" });
    // The pinned footer is identified by its new-unattached-terminal '+' button.
    const footerMarker = screen.getByRole("button", { name: /new unattached terminal/i });
    // DOCUMENT_POSITION_FOLLOWING (4) means Projects comes FIRST in document
    // order, i.e. above the pinned TERMINALS footer.
    expect(
      projectsHeader.compareDocumentPosition(footerMarker) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("loose terminals are drag-reorderable rows and a click selects them (01KV2V4AWT…)", () => {
    const onSelect = vi.fn();
    renderSidebar(
      [tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)])],
      [term("loose1", "/alpha", null), term("loose2", "/beta", null)],
      { onSelect, onReorderLooseTerminals: vi.fn() },
    );
    // Both loose terminals render as rows under the TERMINALS section.
    expect(screen.getByText("alpha")).toBeInTheDocument();
    expect(screen.getByText("beta")).toBeInTheDocument();
    // No separate grip handle (whole-item drag).
    expect(screen.queryByLabelText(/reorder terminal/i)).toBeNull();
    // A plain click on a loose row still selects it (drag doesn't swallow it).
    fireEvent.click(screen.getByText("alpha"));
    expect(onSelect).toHaveBeenCalledWith("loose1");
  });

  it("a terminal MOVES out of the loose section once it has a workspace_id", () => {
    const { rerender } = (() => {
      const r = render(
        <AppSidebar
          projects={[tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)])]}
          terminals={[term("t", "/movingcwd", null)]}
          activeId={null}
          onSelect={noop}
          onClose={noop}
          onNewTerminal={noop}
          onNewLooseTerminal={noop}
          onAddProject={noop}
          onAddWorkspace={noop}
        />,
      );
      return r;
    })();
    // Initially loose: appears once, under TERMINALS.
    expect(screen.getByText("movingcwd")).toBeInTheDocument();

    // After auto-attach reflects workspace_id=w, it groups under the workspace
    // and no longer appears in the loose section (still rendered exactly once).
    rerender(
      <AppSidebar
        projects={[tree("p1", "Solo", [ws("w", "p1", "root", "/solo", true)])]}
        terminals={[term("t", "/movingcwd", "w")]}
        activeId={null}
        onSelect={noop}
        onClose={noop}
        onNewTerminal={noop}
        onNewLooseTerminal={noop}
        onAddProject={noop}
        onAddWorkspace={noop}
      />,
    );
    expect(screen.getAllByText("movingcwd")).toHaveLength(1);
    // The pinned footer no longer lists it as a loose row — but the footer band
    // itself stays (its new-unattached-terminal '+' is still present), since it
    // is pinned.
    expect(screen.getByRole("button", { name: /new unattached terminal/i })).toBeInTheDocument();
  });
});
