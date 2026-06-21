import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ProjectCommandsDialog } from "./project-commands-dialog";
import type { DiscoveredScript, ManagedCommand } from "./use-commands";

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
    ...over,
  };
}

interface IpcSpy {
  calls: { cmd: string; args: Record<string, unknown> }[];
  callsTo: (cmd: string) => { cmd: string; args: Record<string, unknown> }[];
}

function installIpc(opts: { templates?: ManagedCommand[]; scripts?: DiscoveredScript[] }): IpcSpy {
  const spy: IpcSpy = {
    calls: [],
    callsTo: (cmd) => spy.calls.filter((c) => c.cmd === cmd),
  };
  const list: ManagedCommand[] = [...(opts.templates ?? [])];
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      spy.calls.push({ cmd, args: a });
      switch (cmd) {
        case "command_list":
          return list;
        case "command_import_scripts":
          return opts.scripts ?? [];
        case "command_create": {
          const created = template({
            id: `new-${list.length}`,
            name: a.name as string,
            command: a.command as string,
          });
          list.push(created);
          return created;
        }
        case "command_import_create": {
          const created = template({
            id: `imp-${list.length}`,
            name: a.name as string,
            command: a.command as string,
            source_kind: "package_json",
            source_package_json_path: a.sourcePackageJsonPath as string,
            source_script_name: a.sourceScriptName as string,
            source_script_command_snapshot: a.sourceScriptCommandSnapshot as string,
            package_manager: a.packageManager as string,
          });
          list.push(created);
          return created;
        }
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  return spy;
}

describe("<ProjectCommandsDialog> — validated tabs layout (review round v2)", () => {
  beforeEach(() => {
    installIpc({ templates: [] });
  });

  it("does not render while closed", () => {
    render(
      <ProjectCommandsDialog open={false} projectId="p1" projectName="Demo" onClose={vi.fn()} />,
    );
    expect(screen.queryByText(/Commands — Demo/)).toBeNull();
  });

  it("opens with the project title and lists existing commands", async () => {
    installIpc({ templates: [template({ name: "lint", command: "pnpm lint" })] });
    render(<ProjectCommandsDialog open projectId="p1" projectName="Demo" onClose={vi.fn()} />);
    expect(screen.getByText(/Commands — Demo/)).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("lint")).toBeInTheDocument());
    expect(screen.getByText("pnpm lint")).toBeInTheDocument();
  });

  it("shows a Commands tab and (with a workspace) an Import tab", async () => {
    render(
      <ProjectCommandsDialog
        open
        projectId="p1"
        projectName="Demo"
        importWorkspaceId="w1"
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByRole("tab", { name: /commands/i })).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: /import from package\.json/i })).toBeInTheDocument(),
    );
  });

  it("opens the inline create form IN PLACE (no floating card) and creates a command", async () => {
    const spy = installIpc({ templates: [] });
    render(<ProjectCommandsDialog open projectId="p1" projectName="Demo" onClose={vi.fn()} />);
    // The create form is collapsed by default → fields absent.
    expect(screen.queryByLabelText("Command name")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /new command/i }));
    await waitFor(() => expect(screen.getByLabelText("Command name")).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("Command name"), { target: { value: "test" } });
    fireEvent.change(screen.getByLabelText("Command line"), { target: { value: "pnpm test" } });
    fireEvent.click(screen.getByRole("button", { name: /^create$/i }));
    await waitFor(() => {
      const c = spy.callsTo("command_create");
      expect(c).toHaveLength(1);
      expect(c[0].args.name).toBe("test");
    });
    await waitFor(() => expect(screen.getByText("pnpm test")).toBeInTheDocument());
  });

  it("edits a command IN PLACE inside its own card", async () => {
    installIpc({ templates: [template({ name: "dev", command: "pnpm dev" })] });
    render(<ProjectCommandsDialog open projectId="p1" projectName="Demo" onClose={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("dev")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: /edit dev/i }));
    // The edit form appears (in place) seeded with the command's value.
    await waitFor(() =>
      expect((screen.getByLabelText("Command line") as HTMLInputElement).value).toBe("pnpm dev"),
    );
  });

  it("persists a toggled restart_on_startup via command_update when editing", async () => {
    const spy = installIpc({
      templates: [template({ name: "dev", command: "pnpm dev", restart_on_startup: false })],
    });
    render(<ProjectCommandsDialog open projectId="p1" projectName="Demo" onClose={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("dev")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: /edit dev/i }));
    await waitFor(() =>
      expect(screen.getByRole("switch", { name: /restart on startup/i })).toBeInTheDocument(),
    );
    const toggle = screen.getByRole("switch", { name: /restart on startup/i });
    fireEvent.click(toggle);
    fireEvent.click(screen.getByRole("button", { name: /^save$/i }));
    await waitFor(() => {
      const u = spy.callsTo("command_update");
      expect(u).toHaveLength(1);
      expect(u[0].args.id).toBe("c1");
      expect(u[0].args.restartOnStartup).toBe(true);
    });
  });

  it("a sourced command exposes its read-only provenance under a disclosure", async () => {
    installIpc({
      templates: [
        template({
          name: "build",
          command: "bun run build",
          source_kind: "package_json",
          source_package_json_path: "/repo/package.json",
          source_script_name: "build",
          source_script_command_snapshot: "tsc",
          package_manager: "bun",
        }),
      ],
    });
    render(<ProjectCommandsDialog open projectId="p1" projectName="Demo" onClose={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("build")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: /toggle source for build/i }));
    await waitFor(() => expect(screen.getByText("/repo/package.json")).toBeInTheDocument());
    expect(screen.getByText("package.json · scripts.build")).toBeInTheDocument();
  });

  it("imports a selected script via the FOOTER 'Import selected' (Import tab only)", async () => {
    const spy = installIpc({
      templates: [],
      scripts: [
        {
          proposed_name: "dev",
          script_name: "dev",
          default_command: "pnpm dev",
          script_command_snapshot: "vite --host",
          subfolder: "",
          package_json_path: "/repo/package.json",
          package_manager: "pnpm",
        },
      ],
    });
    render(
      <ProjectCommandsDialog
        open
        projectId="p1"
        projectName="Demo"
        importWorkspaceId="w1"
        onClose={vi.fn()}
      />,
    );
    // On the Commands tab, no Import button in the footer.
    expect(screen.queryByRole("button", { name: /import selected/i })).toBeNull();
    // Switch to the Import tab.
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: /import from package\.json/i })).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole("tab", { name: /import from package\.json/i }));
    await waitFor(() => expect(screen.getByLabelText("Select script dev")).toBeInTheDocument());
    // Now the footer shows Import selected.
    fireEvent.click(screen.getByLabelText("Select script dev"));
    const importBtn = await screen.findByRole("button", { name: /import selected/i });
    fireEvent.click(importBtn);
    await waitFor(() => expect(spy.callsTo("command_import_create")).toHaveLength(1));
  });

  it("imports the selected scripts in PARALLEL, isolating a per-row failure (react.doctor)", async () => {
    // Two selectable scripts. The backend REJECTS `bad` but accepts `dev`; a
    // single row's failure must NOT abort the other import, and the re-list / tab
    // hop must still happen (the run is driven by Promise.all over per-row
    // catch-wrapped invokes, not a sequential await-in-loop that stops on throw).
    const spy: { calls: { cmd: string; args: Record<string, unknown> }[] } = { calls: [] };
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        spy.calls.push({ cmd, args: a });
        switch (cmd) {
          case "command_list":
            return [];
          case "command_import_scripts":
            return [
              {
                proposed_name: "dev",
                script_name: "dev",
                default_command: "pnpm dev",
                script_command_snapshot: "vite",
                subfolder: "",
                package_json_path: "/repo/package.json",
                package_manager: "pnpm",
              },
              {
                proposed_name: "bad",
                script_name: "bad",
                default_command: "pnpm bad",
                script_command_snapshot: "boom",
                subfolder: "",
                package_json_path: "/repo/package.json",
                package_manager: "pnpm",
              },
            ] satisfies DiscoveredScript[];
          case "command_import_create":
            if (a.name === "bad") throw "backend refused bad";
            return template({ id: "imp", name: a.name as string, command: a.command as string });
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );

    render(
      <ProjectCommandsDialog
        open
        projectId="p1"
        projectName="Demo"
        importWorkspaceId="w1"
        onClose={vi.fn()}
      />,
    );
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: /import from package\.json/i })).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole("tab", { name: /import from package\.json/i }));
    await waitFor(() => expect(screen.getByLabelText("Select script dev")).toBeInTheDocument());

    fireEvent.click(screen.getByLabelText("Select script dev"));
    fireEvent.click(screen.getByLabelText("Select script bad"));
    fireEvent.click(await screen.findByRole("button", { name: /import selected/i }));

    // BOTH imports were attempted (the failing one did not short-circuit the other)
    // and the post-import re-list ran despite the per-row rejection.
    await waitFor(() => {
      const imports = spy.calls.filter((c) => c.cmd === "command_import_create");
      expect(imports).toHaveLength(2);
      expect(imports.map((c) => c.args.name).sort()).toEqual(["bad", "dev"]);
    });
    // The re-list (refresh) fired → proves the run did not throw out of importRows.
    await waitFor(() =>
      expect(spy.calls.filter((c) => c.cmd === "command_list").length).toBeGreaterThan(1),
    );
  });

  it("resets transient UI on close so a reopen starts clean (no effect-on-open flicker)", async () => {
    const onClose = vi.fn();
    render(<ProjectCommandsDialog open projectId="p1" projectName="Demo" onClose={onClose} />);
    // Open the inline create form (transient state).
    fireEvent.click(screen.getByRole("button", { name: /new command/i }));
    await waitFor(() => expect(screen.getByLabelText("Command name")).toBeInTheDocument());

    // Close via the footer Close button → the handler resets transient UI in the
    // SAME commit as the close (not deferred to a `useEffect(open)`), so the create
    // form collapses immediately.
    fireEvent.click(screen.getByRole("button", { name: /^close$/i }));
    expect(onClose).toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByLabelText("Command name")).toBeNull());
  });

  it("already-imported scripts are greyed and not selectable in the import table (T3)", async () => {
    installIpc({
      // A project command sourced from /repo/package.json scripts.dev.
      templates: [
        template({
          name: "dev",
          command: "pnpm dev",
          source_kind: "package_json",
          source_package_json_path: "/repo/package.json",
          source_script_name: "dev",
          source_script_command_snapshot: "vite",
          package_manager: "pnpm",
        }),
      ],
      scripts: [
        {
          proposed_name: "dev",
          script_name: "dev",
          default_command: "pnpm dev",
          script_command_snapshot: "vite",
          subfolder: "",
          package_json_path: "/repo/package.json",
          package_manager: "pnpm",
        },
        {
          proposed_name: "build",
          script_name: "build",
          default_command: "pnpm build",
          script_command_snapshot: "tsc",
          subfolder: "",
          package_json_path: "/repo/package.json",
          package_manager: "pnpm",
        },
      ],
    });
    render(
      <ProjectCommandsDialog
        open
        projectId="p1"
        projectName="Demo"
        importWorkspaceId="w1"
        onClose={vi.fn()}
      />,
    );
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: /import from package\.json/i })).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole("tab", { name: /import from package\.json/i }));
    await waitFor(() => expect(screen.getByTestId("already-imported")).toBeInTheDocument());
    // The dev row is disabled; the build row is selectable. Base UI's Checkbox is
    // a `role="checkbox"` span using `aria-disabled` rather than the DOM attr.
    expect(screen.getByLabelText("Select script dev")).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByLabelText("Select script build")).not.toHaveAttribute(
      "aria-disabled",
      "true",
    );
  });

  it("has a neutral Close button (always present) that dismisses the modal", async () => {
    const onClose = vi.fn();
    render(<ProjectCommandsDialog open projectId="p1" projectName="Demo" onClose={onClose} />);
    fireEvent.click(screen.getByRole("button", { name: /^close$/i }));
    expect(onClose).toHaveBeenCalled();
  });
});
