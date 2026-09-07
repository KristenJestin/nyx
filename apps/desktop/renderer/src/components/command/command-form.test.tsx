import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { CommandForm } from "./command-form";
import type { CommandFormValues, ManagedCommand } from "./use-commands";

function sourced(over: Partial<ManagedCommand> = {}): ManagedCommand {
  return {
    id: "c1",
    project_id: "p1",
    name: "build",
    command: "bun run build",
    subfolder: null,
    restart_on_startup: false,
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    source_kind: "package_json",
    source_package_json_path: "/repo/package.json",
    source_script_name: "build",
    source_script_command_snapshot: "tsc -b",
    package_manager: "bun",
    description: null,
    ...over,
  };
}

const WORKSPACE = "/home/kris/repo";

function template(over: Partial<ManagedCommand>): ManagedCommand {
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
    source_kind: null,
    source_package_json_path: null,
    source_script_name: null,
    source_script_command_snapshot: null,
    package_manager: null,
    description: null,
    ...over,
  };
}

describe("<CommandForm> subfolder folder-picker (review finding 01KV5PE686…)", () => {
  it("stores a WORKSPACE-RELATIVE subfolder when the picked folder is inside the workspace", async () => {
    // The picker returns an ABSOLUTE path inside the workspace; the form must
    // relativize it (the backend rejects any absolute subfolder).
    const pick = vi.fn(async () => `${WORKSPACE}/packages/api`);
    render(
      <CommandForm workspacePath={WORKSPACE} onSubmit={vi.fn()} onCancel={vi.fn()} pick={pick} />,
    );

    fireEvent.click(screen.getByRole("button", { name: /pick run subfolder/i }));

    const field = screen.getByLabelText("Run subfolder") as HTMLInputElement;
    await waitFor(() => expect(field.value).toBe("packages/api"));
    // No inline error in the success path.
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("submits the relativized subfolder so the backend accepts it", async () => {
    const pick = vi.fn(async () => `${WORKSPACE}/web`);
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    render(
      <CommandForm workspacePath={WORKSPACE} onSubmit={onSubmit} onCancel={vi.fn()} pick={pick} />,
    );

    fireEvent.change(screen.getByLabelText("Command name"), { target: { value: "dev" } });
    fireEvent.change(screen.getByLabelText("Command line"), { target: { value: "pnpm dev" } });
    fireEvent.click(screen.getByRole("button", { name: /pick run subfolder/i }));
    await waitFor(() =>
      expect((screen.getByLabelText("Run subfolder") as HTMLInputElement).value).toBe("web"),
    );

    fireEvent.click(screen.getByRole("button", { name: /^create$/i }));
    // TanStack Form submits asynchronously (validation runs first), so the
    // onSubmit callback fires on a microtask — await it.
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(expect.objectContaining({ subfolder: "web" })),
    );
  });

  it("REFUSES a folder outside the workspace: inline error, field left unchanged", async () => {
    const pick = vi.fn(async () => "/home/kris/other-project");
    render(
      <CommandForm workspacePath={WORKSPACE} onSubmit={vi.fn()} onCancel={vi.fn()} pick={pick} />,
    );

    const field = screen.getByLabelText("Run subfolder") as HTMLInputElement;
    expect(field.value).toBe("");
    fireEvent.click(screen.getByRole("button", { name: /pick run subfolder/i }));

    // An inline error appears and nothing was stored in the field.
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(/must be inside the workspace/i),
    );
    expect(field.value).toBe("");
  });

  it("clears the picker error when the subfolder is edited manually", async () => {
    const pick = vi.fn(async () => "/elsewhere");
    render(
      <CommandForm workspacePath={WORKSPACE} onSubmit={vi.fn()} onCancel={vi.fn()} pick={pick} />,
    );

    fireEvent.click(screen.getByRole("button", { name: /pick run subfolder/i }));
    await waitFor(() => expect(screen.getByRole("alert")).toBeInTheDocument());

    // A manual relative entry still works and clears the error.
    fireEvent.change(screen.getByLabelText("Run subfolder"), { target: { value: "apps/site" } });
    expect(screen.queryByRole("alert")).toBeNull();
    expect((screen.getByLabelText("Run subfolder") as HTMLInputElement).value).toBe("apps/site");
  });
});

describe("<CommandForm> restart_on_startup toggle", () => {
  it("renders the restart toggle and submits its (default off) value on create", async () => {
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    render(<CommandForm onSubmit={onSubmit} onCancel={vi.fn()} />);

    // The toggle is present and OFF by default for a fresh create.
    const toggle = screen.getByRole("switch", { name: /restart on startup/i });
    expect(toggle).toBeInTheDocument();
    expect(toggle).toHaveAttribute("aria-checked", "false");

    fireEvent.change(screen.getByLabelText("Command name"), { target: { value: "dev" } });
    fireEvent.change(screen.getByLabelText("Command line"), { target: { value: "pnpm dev" } });
    fireEvent.click(screen.getByRole("button", { name: /^create$/i }));
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(expect.objectContaining({ restart_on_startup: false })),
    );
  });

  it("associates the 'Restart on startup' label with the Switch control (a11y, react.doctor)", () => {
    const { container } = render(<CommandForm onSubmit={vi.fn()} onCancel={vi.fn()} />);
    // The wrapping <label> carries an explicit htmlFor that targets a real control
    // (Base UI's Switch places the passed `id` on its hidden labelable <input>),
    // so the label↔control association is explicit — not just an aria-label.
    const labels = Array.from(container.querySelectorAll("label[for]"));
    const restartLabel = labels.find((l) => /restart on startup/i.test(l.textContent ?? ""));
    expect(restartLabel).toBeTruthy();
    const id = restartLabel!.getAttribute("for")!;
    expect(id).toBeTruthy();
    // A control with that id exists in the form (the Switch's hidden input).
    const control = container.querySelector(`#${CSS.escape(id)}`);
    expect(control).not.toBeNull();
    // And the visible switch is still queryable by its accessible name.
    expect(screen.getByRole("switch", { name: /restart on startup/i })).toBeInTheDocument();
  });

  it("pre-fills the toggle from the edited template and submits the toggled value", async () => {
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    // Editing a template whose restart flag is ON: the toggle reflects it.
    render(
      <CommandForm
        editing={template({ restart_on_startup: true })}
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );
    const toggle = screen.getByRole("switch", { name: /restart on startup/i });
    expect(toggle).toHaveAttribute("aria-checked", "true");

    // Toggle it OFF, then save: the submitted value carries the new state.
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-checked", "false");
    fireEvent.click(screen.getByRole("button", { name: /^save$/i }));
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(expect.objectContaining({ restart_on_startup: false })),
    );
  });
});

/**
 * THE PROVENANCE IS THE SHARED PANEL NOW (finding 01M132XFZ5KY8QMJ79HZZSBV3J) — the form used to
 * draw a third rendering of the same three facts and strand Unlink in its action row, away from the
 * link it detaches. It mounts `<CommandSourcePanel>` instead, so there is ONE provenance block with
 * BOTH of its answers attached.
 */
describe("<CommandForm> — the package.json provenance it delegates", () => {
  it("shows the provenance ONLY when editing a sourced command", () => {
    // Hand-authored create: no source controls.
    const { unmount, container } = render(<CommandForm onSubmit={vi.fn()} onCancel={vi.fn()} />);
    expect(container.querySelector("[data-command-source]")).toBeNull();
    expect(screen.queryByRole("button", { name: /resync/i })).toBeNull();
    unmount();

    // Editing a sourced command: the shared panel, with both answers on the link.
    render(<CommandForm editing={sourced()} onSubmit={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByRole("button", { name: /resync from package.json/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^unlink$/i })).toBeInTheDocument();
    // No "reset to script runner" anywhere.
    expect(screen.queryByText(/reset to script runner/i)).toBeNull();
  });

  it("Resync calls onResync with the command id and keeps the form open", async () => {
    const onResync = vi.fn(async () => {});
    render(
      <CommandForm editing={sourced()} onSubmit={vi.fn()} onCancel={vi.fn()} onResync={onResync} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /resync from package.json/i }));
    await waitFor(() => expect(onResync).toHaveBeenCalledWith("c1"));
  });

  it("Unlink calls onUnlink with the command id", async () => {
    const onUnlink = vi.fn(async () => {});
    render(
      <CommandForm editing={sourced()} onSubmit={vi.fn()} onCancel={vi.fn()} onUnlink={onUnlink} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /^unlink$/i }));
    await waitFor(() => expect(onUnlink).toHaveBeenCalledWith("c1"));
  });

  it("shows the drift as TWO values side by side, with the adopt action", () => {
    render(
      <CommandForm
        editing={sourced()}
        driftValue="tsc -b --watch"
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByTestId("drift-imported").textContent).toContain("tsc -b");
    expect(screen.getByTestId("drift-line").textContent).toContain("tsc -b --watch");
    expect(screen.getByRole("button", { name: /take the disk version/i })).toBeInTheDocument();
  });
});

/**
 * THE TWO COMMIT MODES (finding 01M132XFZ5KY8QMJ79HZZSBV3J). A creation has a Create button because
 * nothing exists yet; an EDITION commits on leaving a field and has no Save at all — the decision
 * taken with the user, and the one `group-panel.tsx` already takes for a group's name and colour.
 */
describe("<CommandForm> commit modes", () => {
  it("submit mode: the action row exists, and Save submits the edited values", async () => {
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    render(<CommandForm editing={sourced()} onSubmit={onSubmit} onCancel={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Command line"), { target: { value: "node build.js" } });
    fireEvent.click(screen.getByRole("button", { name: /^save$/i }));
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(expect.objectContaining({ command: "node build.js" })),
    );
  });

  it("blur mode: NO Save and NO Cancel — there is nothing left to press", () => {
    render(<CommandForm editing={sourced()} commit="blur" onSubmit={vi.fn()} />);
    expect(screen.queryByRole("button", { name: /^save$/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /^cancel$/i })).toBeNull();
  });

  it("blur mode: leaving a CHANGED field commits it", async () => {
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    render(<CommandForm editing={sourced()} commit="blur" onSubmit={onSubmit} />);
    const field = screen.getByLabelText("Command line");
    fireEvent.change(field, { target: { value: "node build.js" } });
    fireEvent.blur(field);
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(expect.objectContaining({ command: "node build.js" })),
    );
  });

  it("blur mode: merely TABBING THROUGH writes nothing", () => {
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    render(<CommandForm editing={sourced()} commit="blur" onSubmit={onSubmit} />);
    fireEvent.blur(screen.getByLabelText("Command name"));
    fireEvent.blur(screen.getByLabelText("Command line"));
    fireEvent.blur(screen.getByLabelText("Run subfolder"));
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("blur mode: an INVALID field is not pushed to a backend that would reject it", () => {
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    render(<CommandForm editing={sourced()} commit="blur" onSubmit={onSubmit} />);
    const field = screen.getByLabelText("Command name");
    fireEvent.change(field, { target: { value: "   " } });
    fireEvent.blur(field);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("blur mode: the switch has no blur to wait for — the flip IS the commit", async () => {
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    render(<CommandForm editing={sourced()} commit="blur" onSubmit={onSubmit} />);
    fireEvent.click(screen.getByLabelText("Restart on startup"));
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(expect.objectContaining({ restart_on_startup: true })),
    );
  });
});

/**
 * THE NOTE FOR THE AGENT — the one field on this form whose reader is not the person filling it in.
 *
 * What is pinned here is the round trip a user actually performs: a note is typed and it is
 * SUBMITTED (not silently dropped), a stored note SEEDS the field when the command is reopened
 * (else editing anything else would look like the note was never saved), and emptying the field
 * COMMITS the emptiness (`""` — which the mutation layer sends as `null`, erasing it). That last
 * case is the one a naive "only submit non-empty values" guard would break, leaving a note the user
 * deleted in every future session's context.
 */
describe("<CommandForm> note for the agent", () => {
  it("submits the note typed on a creation", async () => {
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    render(<CommandForm onSubmit={onSubmit} onCancel={vi.fn()} />);

    fireEvent.change(screen.getByLabelText("Command name"), { target: { value: "check" } });
    fireEvent.change(screen.getByLabelText("Command line"), { target: { value: "bun run check" } });
    fireEvent.change(screen.getByLabelText("Note for the agent"), {
      target: { value: "  The gate before any commit.  " },
    });
    fireEvent.click(screen.getByRole("button", { name: /^create$/i }));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({ description: "The gate before any commit." }),
      ),
    );
  });

  it("seeds the field from the stored note when editing", () => {
    render(
      <CommandForm
        editing={template({ description: "Slow — ask before starting it." })}
        commit="blur"
        onSubmit={vi.fn()}
      />,
    );
    expect((screen.getByLabelText("Note for the agent") as HTMLTextAreaElement).value).toBe(
      "Slow — ask before starting it.",
    );
  });

  it("blur mode: emptying the note COMMITS the emptiness, so it can be erased", async () => {
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    render(
      <CommandForm
        editing={template({ description: "Obsolete note." })}
        commit="blur"
        onSubmit={onSubmit}
      />,
    );
    const field = screen.getByLabelText("Note for the agent");
    fireEvent.change(field, { target: { value: "" } });
    fireEvent.blur(field);

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(expect.objectContaining({ description: "" })),
    );
  });

  it("blur mode: leaving the note untouched writes nothing", () => {
    const onSubmit = vi.fn<(v: CommandFormValues) => void>();
    render(
      <CommandForm
        editing={template({ description: "Unchanged." })}
        commit="blur"
        onSubmit={onSubmit}
      />,
    );
    fireEvent.blur(screen.getByLabelText("Note for the agent"));
    expect(onSubmit).not.toHaveBeenCalled();
  });
});
