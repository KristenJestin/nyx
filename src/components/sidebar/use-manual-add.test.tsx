import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

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
    project: { id, name, collapsed: false, created_at: 0, updated_at: 0 },
    workspaces,
  };
}

/**
 * Tiny harness that mounts `useManualAdd` and exposes its triggers plus the
 * rendered dialogs, so the flows can be driven without the full manager.
 */
function Harness({ deps, target }: { deps: UseManualAddDeps; target: ProjectTree }) {
  const { addProject, addWorkspace, editProject, removeProject, dialog } = useManualAdd(deps);
  return (
    <div>
      <button onClick={() => void addProject()}>add-project</button>
      <button onClick={() => void addWorkspace(target)}>add-workspace</button>
      <button onClick={() => editProject(target)}>edit-project</button>
      <button onClick={() => removeProject(target)}>delete-project</button>
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
