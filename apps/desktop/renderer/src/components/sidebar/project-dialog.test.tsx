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

/**
 * The two destructive confirms must each describe THEIR OWN command, because the two commands
 * do OPPOSITE things to the live runtime (review `01M0A7QGBK43MZB10RF51QR6TS`).
 *
 * The bug these guard: the workspace confirm carried the Delete-project sentence verbatim —
 * "Any open terminals are kept (they just become loose, unattached terminals) — nothing is
 * closed." True of `delete_project`, the exact opposite of `workspace_delete`. So the user
 * clicked an IRREVERSIBLE confirm believing no session would be lost.
 *
 * What each command actually executes (read at the source, not copied from the other dialog):
 *
 *  - `workspace_delete` — the sidecar bridge STOPS every running command instance and CLOSES
 *    every interactive terminal PTY of the workspace before delegating
 *    (`crates/nyx-sidecar/src/runtime/db_command.rs`), and `usecase::remove_workspace` then
 *    DELETES the workspace's terminal records and agent sessions inside the removal
 *    transaction (`crates/nyx-core/src/usecase.rs`) — deliberately, rather than letting the
 *    `ON DELETE SET NULL` FK detach them.
 *  - `delete_project` — the same bridge STOPS every command instance of the project (review
 *    `01M0AJ4F6RMS4CHWQ5B6DB47RC`: the FK cascade erases the `command_instances` rows that are
 *    the stop handles, so the kill must precede the delete), but runs NO `pty.close_record`.
 *    Its workspaces go by FK CASCADE and every terminal's `workspace_id` is
 *    `ON DELETE SET NULL`, so the terminals SURVIVE, detached.
 *
 * Hence the sentences agree on the commands and DIVERGE on the terminals, asserted here in
 * BOTH directions so the destructive terminal claim can never be pasted onto the project
 * confirm, nor the "loose terminals" promise onto the workspace one.
 */
describe("<ProjectDialog> destructive confirms match what their command executes", () => {
  /** The whole popup's text, whitespace-normalized (the copy is split across styled spans). */
  const confirmText = () => (document.body.textContent ?? "").replace(/\s+/g, " ");

  it("REMOVE WORKSPACE (`workspace_delete`): announces the closed terminals and stopped commands", () => {
    render(
      <ProjectDialog
        open
        mode="delete"
        entity="workspace"
        defaultName="feat/x"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByRole("heading", { name: "Remove workspace" })).toBeInTheDocument();
    const text = confirmText();
    // It states the destruction the command performs.
    expect(text).toMatch(/open terminals are closed/i);
    expect(text).toMatch(/running commands are stopped/i);
    // …and it NEVER carries the Delete-project promise, which is false here.
    expect(text).not.toMatch(/nothing is closed/i);
    expect(text).not.toMatch(/become loose/i);
    // The one reassurance that IS true stays: nyx forgets the entry, the folder survives.
    expect(text).toMatch(/folder on disk is not deleted/i);
  });

  it("DELETE PROJECT (`delete_project`): announces the stopped commands, still promises loose terminals", () => {
    render(
      <ProjectDialog open mode="delete" defaultName="Doomed" onConfirm={vi.fn()} onCancel={vi.fn()} />,
    );
    expect(screen.getByRole("heading", { name: "Delete project" })).toBeInTheDocument();
    const text = confirmText();
    // The bridge kill is user-visible, so the confirm says it.
    expect(text).toMatch(/running commands are stopped/i);
    // `delete_project` really does leave every terminal alive and detached.
    expect(text).toMatch(/become loose/i);
    expect(text).toMatch(/no terminal is closed/i);
    // …so it must NOT inherit the workspace confirm's TERMINAL destruction claim.
    expect(text).not.toMatch(/terminals are closed/i);
    // …nor keep the old blanket promise, which the command kill made false.
    expect(text).not.toMatch(/nothing is closed/i);
  });
});
