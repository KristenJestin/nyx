import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

// Mock the toast helper so the delete flows can be asserted to fire the right
// variant with the REAL backend reason — without mounting the toast manager/viewport.
const toastMock = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
  info: vi.fn(),
  warning: vi.fn(),
}));
vi.mock("@/components/ui/toast", () => ({ toast: toastMock }));

import { useManualAdd, type UseManualAddDeps } from "./use-manual-add";
import { basename } from "./folder-picker";
import type { ProjectTree, WorkspaceRecord } from "./use-projects";

function ws(id: string, name: string, path: string, isRoot: boolean): WorkspaceRecord {
  return {
    id,
    project_id: "p1",
    name,
    path,
    branch: null,
    is_root: isRoot,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
  };
}

function tree(id: string, name: string, workspaces: WorkspaceRecord[] = []): ProjectTree {
  return {
    project: {
      id,
      name,
      collapsed: false,
      created_at: 0,
      updated_at: 0,
      resume_agent_sessions: false,
    },
    workspaces,
  };
}

/**
 * Tiny harness that mounts `useManualAdd` and exposes its triggers plus the
 * rendered dialogs, so the flows can be driven without the full manager.
 */
function Harness({
  deps,
  target,
  targetWs,
}: {
  deps: UseManualAddDeps;
  target: ProjectTree;
  /** Workspace the `remove-workspace` trigger targets (omit → the button is inert). */
  targetWs?: WorkspaceRecord;
}) {
  const { addProject, addWorkspace, editProject, removeProject, removeWorkspace, dialog } =
    useManualAdd(deps);
  return (
    <div>
      <button onClick={() => void addProject()}>add-project</button>
      <button onClick={() => void addWorkspace(target)}>add-workspace</button>
      <button onClick={() => editProject(target)}>edit-project</button>
      <button onClick={() => removeProject(target)}>delete-project</button>
      <button onClick={() => targetWs && removeWorkspace(targetWs)}>remove-workspace</button>
      {dialog}
    </div>
  );
}

describe("basename (default name from a picked folder)", () => {
  it("returns the last segment for POSIX and Windows paths", () => {
    expect(basename("/home/kris/my-app")).toBe("my-app");
    expect(basename("C:\\Users\\kris\\my-app")).toBe("my-app");
    expect(basename("/home/kris/my-app/")).toBe("my-app");
  });
  it("returns empty for a root with no segment", () => {
    expect(basename("/")).toBe("");
  });
});

describe("useManualAdd — add project (opens a create modal)", () => {
  it("picks a folder, opens the modal prefilled, creates with root name 'main' on confirm", async () => {
    const createProject = vi.fn().mockResolvedValue({ project: tree("p", "x").project, root: {} });
    const pick = vi.fn().mockResolvedValue("/home/kris/cool-app");

    render(
      <Harness
        deps={{ createProject, createWorkspace: vi.fn(), pick }}
        target={tree("p1", "Proj")}
      />,
    );
    fireEvent.click(screen.getByText("add-project"));

    // The CREATE modal opens with the folder shown + the name prefilled.
    const nameInput = (await screen.findByLabelText(/project name/i)) as HTMLInputElement;
    expect(nameInput.value).toBe("cool-app");
    expect(screen.getByText("/home/kris/cool-app")).toBeInTheDocument();

    // Confirm → create_project(displayName, path, "main").
    fireEvent.click(screen.getByRole("button", { name: /add project/i }));
    await waitFor(() =>
      expect(createProject).toHaveBeenCalledWith("cool-app", "/home/kris/cool-app", "main"),
    );
  });

  it("does nothing when the picker is cancelled (null)", async () => {
    const createProject = vi.fn();
    const pick = vi.fn().mockResolvedValue(null);
    render(
      <Harness
        deps={{ createProject, createWorkspace: vi.fn(), pick }}
        target={tree("p1", "Proj")}
      />,
    );
    fireEvent.click(screen.getByText("add-project"));
    await new Promise((r) => setTimeout(r, 10));
    expect(createProject).not.toHaveBeenCalled();
    expect(screen.queryByLabelText(/project name/i)).toBeNull();
  });
});

describe("useManualAdd — edit project (rename modal)", () => {
  it("opens the rename modal prefilled and calls updateProject on save", async () => {
    const updateProject = vi.fn().mockResolvedValue(undefined);
    render(
      <Harness
        deps={{
          createProject: vi.fn(),
          createWorkspace: vi.fn(),
          updateProject,
          pick: vi.fn(),
        }}
        target={tree("p1", "Old Name")}
      />,
    );
    fireEvent.click(screen.getByText("edit-project"));

    const nameInput = (await screen.findByLabelText(/project name/i)) as HTMLInputElement;
    expect(nameInput.value).toBe("Old Name");
    fireEvent.change(nameInput, { target: { value: "New Name" } });
    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    await waitFor(() => expect(updateProject).toHaveBeenCalledWith("p1", "New Name"));
  });
});

describe("useManualAdd — delete project (confirm modal)", () => {
  it("opens a destructive confirm and calls deleteProject only on confirm", async () => {
    const deleteProject = vi.fn().mockResolvedValue(undefined);
    render(
      <Harness
        deps={{
          createProject: vi.fn(),
          createWorkspace: vi.fn(),
          deleteProject,
          pick: vi.fn(),
        }}
        target={tree("p1", "Doomed")}
      />,
    );
    fireEvent.click(screen.getByText("delete-project"));

    // The confirm step explains terminals survive; the destructive action button.
    await screen.findByText(/become\s+loose/i);
    const confirm = screen.getByRole("button", { name: /^delete$/i });
    fireEvent.click(confirm);
    await waitFor(() => expect(deleteProject).toHaveBeenCalledWith("p1"));
  });
});

describe("useManualAdd — remove workspace (confirm modal, #19)", () => {
  // The module-level `toastMock` persists across tests (this file doesn't enable
  // global clearMocks), so reset call history before each so the success/error
  // assertions only see THIS test's calls.
  beforeEach(() => vi.clearAllMocks());

  // The confirm button is labelled "Remove workspace" for the workspace variant
  // (the project delete uses "Delete"), so its presence proves the dialog opened in
  // the WORKSPACE shape — robust against the description text being split by a <span>.
  function deps(deleteWorkspace: UseManualAddDeps["deleteWorkspace"]): UseManualAddDeps {
    return { createProject: vi.fn(), createWorkspace: vi.fn(), deleteWorkspace, pick: vi.fn() };
  }

  it("opens a workspace-specific confirm and calls deleteWorkspace + toasts success on confirm", async () => {
    const deleteWorkspace = vi.fn().mockResolvedValue(undefined);
    const target = tree("p1", "Proj", [
      ws("w-root", "main", "/p", true),
      ws("w2", "feature", "/p/feat", false),
    ]);
    render(
      <Harness deps={deps(deleteWorkspace)} target={target} targetWs={target.workspaces[1]} />,
    );
    fireEvent.click(screen.getByText("remove-workspace"));

    const confirm = await screen.findByRole("button", { name: /^remove workspace$/i });
    fireEvent.click(confirm);

    await waitFor(() => expect(deleteWorkspace).toHaveBeenCalledWith("w2"));
    expect(toastMock.success).toHaveBeenCalledWith(expect.stringContaining("feature"));
    expect(toastMock.error).not.toHaveBeenCalled();
  });

  it("surfaces a backend rejection (running command) as an error toast + inline alert, keeps the dialog open", async () => {
    const deleteWorkspace = vi
      .fn()
      .mockRejectedValue(
        "this workspace has a running command — stop it before removing the workspace",
      );
    const target = tree("p1", "Proj", [
      ws("w-root", "main", "/p", true),
      ws("w2", "feature", "/p/feat", false),
    ]);
    render(
      <Harness deps={deps(deleteWorkspace)} target={target} targetWs={target.workspaces[1]} />,
    );
    fireEvent.click(screen.getByText("remove-workspace"));

    fireEvent.click(await screen.findByRole("button", { name: /^remove workspace$/i }));

    await waitFor(() =>
      expect(toastMock.error).toHaveBeenCalledWith(expect.stringContaining("running command")),
    );
    // The rejection keeps the modal open with the real reason inline.
    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(toastMock.success).not.toHaveBeenCalled();
  });

  it("is a no-op for the ROOT workspace (no dialog, no backend call)", async () => {
    const deleteWorkspace = vi.fn();
    const target = tree("p1", "Proj", [ws("w-root", "main", "/p", true)]);
    render(
      <Harness deps={deps(deleteWorkspace)} target={target} targetWs={target.workspaces[0]} />,
    );
    fireEvent.click(screen.getByText("remove-workspace"));

    await new Promise((r) => setTimeout(r, 10));
    expect(deleteWorkspace).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: /^remove workspace$/i })).toBeNull();
  });
});

describe("useManualAdd — add workspace", () => {
  it("prefills a distinguishing default (relative path segment) and creates on confirm", async () => {
    const createWorkspace = vi.fn().mockResolvedValue({});
    const pick = vi.fn().mockResolvedValue("/home/kris/proj/apps/web");

    render(
      <Harness
        deps={{ createProject: vi.fn(), createWorkspace, pick }}
        target={tree("p1", "Proj", [ws("w", "main", "/home/kris/proj", true)])}
      />,
    );
    fireEvent.click(screen.getByText("add-workspace"));

    const nameInput = (await screen.findByLabelText(/workspace name/i)) as HTMLInputElement;
    // Nested under the root → the distinguishing segment, not the bare basename.
    expect(nameInput.value).toBe("apps/web");
    expect(screen.getByText("/home/kris/proj/apps/web")).toBeInTheDocument();

    fireEvent.change(nameInput, { target: { value: "web" } });
    fireEvent.click(screen.getByRole("button", { name: /add workspace/i }));

    await waitFor(() =>
      expect(createWorkspace).toHaveBeenCalledWith("p1", "web", "/home/kris/proj/apps/web"),
    );
  });

  it("surfaces a backend duplicate-path rejection inline and keeps the dialog open", async () => {
    const createWorkspace = vi.fn().mockRejectedValue("UNIQUE constraint failed: workspaces.path");
    const pick = vi.fn().mockResolvedValue("/home/kris/proj/dup");

    render(
      <Harness
        deps={{ createProject: vi.fn(), createWorkspace, pick }}
        target={tree("p1", "Proj", [ws("w", "main", "/home/kris/proj", true)])}
      />,
    );
    fireEvent.click(screen.getByText("add-workspace"));
    await screen.findByLabelText(/workspace name/i);
    fireEvent.click(screen.getByRole("button", { name: /add workspace/i }));

    await waitFor(() => expect(screen.getByRole("alert")).toBeInTheDocument());
    expect(screen.getByLabelText(/workspace name/i)).toBeInTheDocument();
  });
});
