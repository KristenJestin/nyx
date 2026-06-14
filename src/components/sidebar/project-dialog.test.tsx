import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ProjectDialog } from "./project-dialog";

describe("<ProjectDialog> create / edit / delete modes", () => {
  it("CREATE: shows the folder + editable name, confirms with the edited name", () => {
    const onConfirm = vi.fn();
    render(
      <ProjectDialog
        open
        mode="create"
        path="/home/kris/cool-app"
        defaultName="cool-app"
        onConfirm={onConfirm}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByRole("heading", { name: "Add project" })).toBeInTheDocument();
    expect(screen.getByText("/home/kris/cool-app")).toBeInTheDocument();
    const name = screen.getByLabelText(/project name/i) as HTMLInputElement;
    expect(name.value).toBe("cool-app");
    fireEvent.change(name, { target: { value: "Cool App" } });
    fireEvent.click(screen.getByRole("button", { name: /add project/i }));
    expect(onConfirm).toHaveBeenCalledWith("Cool App");
  });

  it("EDIT: prefilled name, confirms with the new name (no folder shown)", () => {
    const onConfirm = vi.fn();
    render(
      <ProjectDialog
        open
        mode="edit"
        defaultName="Old Name"
        onConfirm={onConfirm}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByText("Rename project")).toBeInTheDocument();
    expect(screen.queryByText(/folder/i)).toBeNull();
    const name = screen.getByLabelText(/project name/i) as HTMLInputElement;
    expect(name.value).toBe("Old Name");
    fireEvent.change(name, { target: { value: "New Name" } });
    fireEvent.click(screen.getByRole("button", { name: /save/i }));
    expect(onConfirm).toHaveBeenCalledWith("New Name");
  });

  it("DELETE: a destructive confirm; explains terminals survive; confirm fires", () => {
    const onConfirm = vi.fn();
    render(
      <ProjectDialog
        open
        mode="delete"
        defaultName="Doomed"
        onConfirm={onConfirm}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByRole("heading", { name: "Delete project" })).toBeInTheDocument();
    // It reassures the user that terminals are kept (become loose).
    expect(screen.getByText(/become\s+loose/i)).toBeInTheDocument();
    // No editable name input in delete mode.
    expect(screen.queryByLabelText(/project name/i)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /^delete$/i }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("Cancel dismisses without confirming", () => {
    const onCancel = vi.fn();
    const onConfirm = vi.fn();
    render(
      <ProjectDialog open mode="edit" defaultName="x" onConfirm={onConfirm} onCancel={onCancel} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /cancel/i }));
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
  });
});
