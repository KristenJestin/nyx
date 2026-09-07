import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { emit, mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";
import type { WorkspaceRuntimeSnapshot } from "@/components/runtime/use-workspace-runtime";
import { SystemScopePage } from "./system-scope-page";

/** The absolute home the phase-1 bootstrap pins the scope at — never `~` on the wire. */
const HOME = "/home/kris";

function systemProject(): ProjectRecord {
  return {
    id: "sys-project",
    name: "System",
    collapsed: false,
    created_at: 0,
    updated_at: 0,
    resume_agent_sessions: false,
    kind: "system",
  };
}

function systemWorkspace(): WorkspaceRecord {
  return {
    id: "sys-ws",
    project_id: "sys-project",
    name: "system",
    path: HOME,
    branch: null,
    is_root: true,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
  };
}

/**
 * The Système workspace's runtime aggregate. `namedTerminal` toggles a MANUAL label on the second
 * terminal, so a test can tell the label path from the cwd-fallback path.
 */
function snapshot(extraTerminalIds: readonly string[] = []): WorkspaceRuntimeSnapshot {
  return {
    workspace: { id: "sys-ws", name: "system", path: HOME },
    terminals: [
      // NO label: the row falls back to the cwd — which is the ABSOLUTE home, and must READ `~`.
      {
        id: "sys-term",
        cwd: HOME,
        label: null,
        workspace_id: "sys-ws",
        status: "alive",
        exec_state: "idle",
        exec_state_unread: false,
        sessions: [],
      },
      // A named one, to prove the `~` rendering touches the FALLBACK only.
      {
        id: "sys-term-2",
        cwd: HOME,
        label: "logs",
        workspace_id: "sys-ws",
        status: "alive",
        exec_state: "idle",
        exec_state_unread: false,
        sessions: [],
      },
      ...extraTerminalIds.map((id) => ({
        id,
        cwd: HOME,
        label: null,
        workspace_id: "sys-ws",
        status: "alive" as const,
        exec_state: "idle" as const,
        exec_state_unread: false,
        sessions: [],
      })),
    ],
    agent_sessions: [],
    services: [
      {
        id: "sys-inst",
        command_id: "sys-cmd",
        workspace_id: "sys-ws",
        name: "palbank services",
        command: "systemctl --user start palbank.target",
        run_state: "running",
        exit_code: null,
        unread: false,
      },
    ],
  };
}

type ScopeAnswer = "ok" | "null" | "error" | "pending";

/**
 * The backend the Système page reads: its dedicated scope read, the workspace runtime, the terminal
 * CRUD `useTerminals` drives, and the command reads. `calls` records everything so a test can
 * assert WHICH command a gesture invoked — the point of several of them.
 */
function installBackend(
  calls: Array<{ cmd: string; args: Record<string, unknown> }> = [],
  scopeAnswer: ScopeAnswer = "ok",
) {
  let createSeq = 0;
  const created: string[] = [];
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args: a });
      switch (cmd) {
        case "system_scope_detail":
          if (scopeAnswer === "null") return null;
          if (scopeAnswer === "pending") return new Promise(() => {});
          if (scopeAnswer === "error") throw new Error("Système scope is incoherent");
          return { project: systemProject(), workspace: systemWorkspace(), home: HOME };
        case "workspace_runtime_snapshot":
          return snapshot(created);
        case "list_projects":
          return [];
        case "list_workspaces":
          return [];
        case "list_terminals":
          return [];
        case "pty_spawn":
          return 100;
        case "command_list":
        case "command_group_list":
        case "command_instance_list":
        case "command_import_scripts":
        case "agent_active_sessions":
        case "agent_activity_snapshot":
          return [];
        case "create_system_terminal": {
          createSeq += 1;
          const id = `sys-new-${createSeq}`;
          created.push(id);
          setTimeout(() => void emit("terminals://changed"), 0);
          return {
            id,
            cwd: HOME,
            label: null,
            status: "alive",
            order_index: 0,
            created_at: 0,
            updated_at: 0,
            closed_at: null,
            workspace_id: "sys-ws",
          };
        }
        default:
          return undefined;
      }
    },
    { shouldMockEvents: true },
  );
}

/**
 * The sidebar rail of the page (`aria-label="System"`), once it has rendered AND its runtime
 * aggregate has landed — the rail appears on the identity read, the rows on the runtime one, and
 * asserting between the two is how a test reads an empty tree that is merely early.
 */
async function sidebar(): Promise<HTMLElement> {
  const rail = await screen.findByRole("complementary", { name: "System" });
  await waitFor(() => expect(within(rail).queryByText(/Loading runtime/)).toBeNull());
  return rail;
}

describe("SystemScopePage", () => {
  it("shows a LOADING state while the dedicated scope read is in flight", async () => {
    installBackend([], "pending");
    render(<SystemScopePage />);
    expect(await screen.findByLabelText("Loading the System scope")).toBeTruthy();
    // Nothing of the scope is drawn yet — no false empty tree.
    expect(screen.queryByRole("complementary", { name: "System" })).toBeNull();
  });

  it("shows the backend's OWN reason (and a Retry) when the read FAILS", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls, "error");
    render(<SystemScopePage />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Système scope is incoherent");
    const retry = screen.getByRole("button", { name: "Retry" });
    calls.length = 0;
    fireEvent.click(retry);
    await waitFor(() => expect(calls.some((c) => c.cmd === "system_scope_detail")).toBe(true));
  });

  it("says so when the database has NO scope (a `null` read is not an error)", async () => {
    installBackend([], "null");
    render(<SystemScopePage />);
    expect(await screen.findByText(/not available on this database/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("reads the scope through `system_scope_detail`, never through list_projects", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    render(<SystemScopePage />);
    await sidebar();
    expect(calls.some((c) => c.cmd === "system_scope_detail")).toBe(true);
    // …and it composes the RUNTIME from the shared aggregate, keyed on the singleton's workspace.
    expect(
      calls.some((c) => c.cmd === "workspace_runtime_snapshot" && c.args.workspaceId === "sys-ws"),
    ).toBe(true);
  });

  it("re-reads the scope when the backend pushes a tree change (no polling)", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    render(<SystemScopePage />);
    await sidebar();
    const before = calls.filter((c) => c.cmd === "system_scope_detail").length;
    await emit("workspaces://changed");
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "system_scope_detail").length).toBeGreaterThan(before),
    );
  });

  it("says `System` ONCE in its head, and offers NO project chrome", async () => {
    installBackend();
    render(<SystemScopePage />);
    const rail = await sidebar();
    const heading = within(rail).getByRole("heading", { name: "System" });
    // ONCE. A `system` badge sat beside this title until finding X1 — the same word, an inch from
    // itself, left over from when Système was a scope to tell apart from a project in one header.
    // The match is anchored (`^system$`), so the `systemctl …` command below cannot pad the count:
    // put the badge back and this reads 2.
    const said = within(rail).getAllByText(/^system$/i);
    expect(said).toHaveLength(1);
    expect(said[0]).toBe(heading);
    // The four the backend refuses on the singleton are not offered at all — not disabled, absent.
    expect(screen.queryByRole("button", { name: "Project settings" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Add workspace" })).toBeNull();
    expect(screen.queryByRole("button", { name: /Rename project/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /Delete project/i })).toBeNull();
  });

  it("renders Terminals THEN Commands, with no workspace band between them", async () => {
    installBackend();
    render(<SystemScopePage />);
    const rail = await sidebar();
    const labels = within(rail)
      .getAllByText(/^(Terminals|Commands)$/)
      .map((n) => n.textContent);
    // The kicker under the title says "Terminals" too — the SECTION order is the tail of the list.
    expect(labels.slice(-2)).toEqual(["Terminals", "Commands"]);
    // NO workspace accordion: the Système workspace is an identity, not a folder to open.
    expect(screen.queryByRole("button", { name: /^Toggle workspace/ })).toBeNull();
  });

  it("shows the default cwd as `~` in the foot, with the ABSOLUTE home one hover away", async () => {
    installBackend();
    render(<SystemScopePage />);
    const rail = await sidebar();
    const foot = within(rail).getByText("default cwd").closest("p") as HTMLElement;
    expect(foot.textContent).toContain("~");
    expect(foot.getAttribute("title")).toBe(HOME);
  });

  it("reads an unnamed terminal's cwd as `~`, and leaves a NAMED one alone", async () => {
    installBackend();
    render(<SystemScopePage />);
    const rail = await sidebar();
    // The row whose cwd IS the home is NAMED `~` — read off its own controls, so the assertion
    // cannot be satisfied by the foot's `~`.
    expect(within(rail).getByRole("button", { name: "Close terminal ~" })).toBeTruthy();
    expect(within(rail).queryByText(HOME)).toBeNull();
    // …and a manual label is untouched by the rendering.
    expect(within(rail).getByText("logs")).toBeTruthy();
  });

  it("creates a terminal through the ATOMIC `create_system_terminal`, with no path on the wire", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    render(<SystemScopePage />);
    await sidebar();
    fireEvent.click(screen.getByRole("button", { name: "New terminal" }));
    await waitFor(() => expect(calls.some((c) => c.cmd === "create_system_terminal")).toBe(true));
    // NEVER the loose create-then-attach pair: that would pass through the unattached state the
    // scope must stay disjoint from.
    expect(calls.some((c) => c.cmd === "create_terminal")).toBe(false);
    expect(calls.some((c) => c.cmd === "attach_terminal")).toBe(false);
    // And the payload names no path at all — the use case reads the home itself, so a `~` can
    // never travel to the backend.
    const create = calls.find((c) => c.cmd === "create_system_terminal")!;
    expect(JSON.stringify(create.args)).not.toContain("~");
    expect(JSON.stringify(create.args)).not.toContain(HOME);
  });

  it("closes a terminal from its row", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    render(<SystemScopePage />);
    await sidebar();
    fireEvent.click(screen.getByRole("button", { name: "Close terminal logs" }));
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "close_terminal" && c.args.id === "sys-term-2")).toBe(
        true,
      ),
    );
  });

  it("pilots a command's lifecycle from its row (start / stop / relaunch)", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    render(<SystemScopePage />);
    await sidebar();
    expect(screen.getByRole("button", { name: "Start command" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Relaunch command" })).toBeTruthy();
    // The instance is `running` in the snapshot, so Stop is the live one.
    fireEvent.click(screen.getByRole("button", { name: "Stop command" }));
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "command_stop" && c.args.instanceId === "sys-inst")).toBe(
        true,
      ),
    );
  });

  it("offers NO `Clear output` control — the verb left the row (finding U2)", async () => {
    // Human review 01M1EC7Y2625Q07C5386GKHZFS / U2. Phase 3 put an eraser here and only here, which
    // is exactly the grief: the same command row exists on every project surface and one of them
    // behaved differently. The user chose removal over generalisation. What must NOT go with it is
    // any other human verb of the command — the lifecycle trio is asserted live just above, and the
    // create / edit / delete / group half is asserted by the manager tests below.
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    render(<SystemScopePage />);
    await sidebar();
    expect(screen.queryByRole("button", { name: /^Clear output/ })).toBeNull();
    expect(calls.some((c) => c.cmd === "command_clear_output")).toBe(false);
    // The trio is still on the row it left — removing the eraser removed the eraser.
    expect(screen.getByRole("button", { name: "Stop command" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Relaunch command" })).toBeTruthy();
  });

  it("opens the command MANAGER (create / edit / delete / group) with no Global pane", async () => {
    // ONE CHASSIS SINCE FINDING Y1 (human review 01M1EP31GWDR6FJEXY6E8CVDZZ): this manager IS
    // `<ProjectSettingsDialog>`, with its rail reduced to a single entry. What the scope drops is
    // the LIST ITEM, not the content of a section — the singleton's backend refuses BOTH of the
    // Global pane's controls (`update_project` for the rename, `set_project_resume_agent_sessions`
    // for the toggle), so a greyed-out pane would have been a surface offering what cannot work.
    installBackend();
    render(<SystemScopePage />);
    await sidebar();
    fireEvent.click(screen.getByRole("button", { name: "New command" }));
    expect(await screen.findByText("System commands")).toBeTruthy();
    // The rail is here — the shared one — and it holds Commands and nothing else.
    const rail = screen.getByRole("navigation", { name: "System commands sections" });
    const entries = within(rail).getAllByRole("button");
    expect(entries).toHaveLength(1);
    expect(entries[0]).toHaveTextContent("Commands");
    // NO Global: not an entry, not a pane, not a control of that pane.
    expect(screen.queryByRole("button", { name: "Global" })).toBeNull();
    expect(screen.queryByLabelText("Project name")).toBeNull();
    expect(screen.queryByRole("switch", { name: /resume agent sessions/i })).toBeNull();
    // The commands master/detail IS mounted (the body the project modal shares).
    expect(document.querySelector("[data-commands-section]")).toBeTruthy();
  });

  it("offers NO package.json import here, and never scans the HOME for one", async () => {
    // REVIEW FINDINGS 01M1C7EP2FHR6BEDS5ESSYGZ2B and 01M1C7EMPGY1YDXHC6F1TJPFWS. This scope's
    // workspace root IS the user's home, so "the scripts of the root workspace" meant walking the
    // home folder for manifests — meaningless for machine services, and measured at ~6 s warm on a
    // real home, ON the sidecar's single-threaded request loop, which is where "the system commands
    // take ten seconds to appear" came from. It is also the coherence fix: `list_importable_scripts`
    // was already closed to AGENTS on this scope for exactly this reason.
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend(calls);
    render(<SystemScopePage />);
    await sidebar();
    fireEvent.click(screen.getByRole("button", { name: "New command" }));
    await screen.findByText("System commands");
    await waitFor(() => expect(document.querySelector("[data-commands-section]")).toBeTruthy());

    expect(document.querySelector("[data-command-import-entry]")).toBeNull();
    expect(screen.queryByText(/Import from package\.json/)).toBeNull();
    // …and the read that would have walked the home was never issued.
    expect(calls.some((c) => c.cmd === "command_import_scripts")).toBe(false);
  });

  it("gives every control of the rail an accessible name (keyboard-reachable)", async () => {
    installBackend();
    render(<SystemScopePage />);
    const rail = await sidebar();
    for (const button of within(rail).getAllByRole("button")) {
      const name = button.getAttribute("aria-label")?.trim() || (button.textContent ?? "").trim();
      expect(name.length).toBeGreaterThan(0);
      // Nothing is pulled out of the tab order — the reveal of a row's actions is CSS opacity,
      // and `has-[:focus-visible]` puts the keyboard at the same rank as the pointer.
      expect(button.getAttribute("tabindex")).not.toBe("-1");
    }
  });
});

/**
 * THE DEEP-LINK — `/system?terminal=` / `?command=`, the route's own params (`router.tsx`), handed
 * down as props.
 *
 * It is the exact rule `<TerminalsPage>` applies, over the singleton's runtime instead of a
 * project's: apply ONCE per distinct target id (so a later selection is never yanked back), and
 * SUPPRESS the auto-select-first while a target is outstanding (so the restore never races the
 * deep-link onto the wrong pane). Written twice because the two screens are two pages; pinned twice
 * for the same reason.
 *
 * The reason it exists at all: the cross-project home lists the singleton's terminals and services,
 * and clicking one has to land ON that object rather than merely on the scope.
 */
describe("SystemScopePage — the deep-link", () => {
  /** The row whose `data-selected` is `true`, by its visible label. */
  function selectedRow(): HTMLElement | null {
    return document.querySelector<HTMLElement>('[data-selected="true"]');
  }

  it("selects the terminal named by `?terminal=`, over the auto-select-first", async () => {
    installBackend();
    render(<SystemScopePage deepLinkTerminalId="sys-term-2" />);
    await sidebar();
    // `sys-term` is FIRST in the snapshot, so the auto-select would have taken it. It did not.
    await waitFor(() => expect(selectedRow()).not.toBeNull());
    expect(within(selectedRow()!).queryByText("logs")).not.toBeNull();
  });

  it("selects the service named by `?command=` — its live output pane", async () => {
    installBackend();
    render(<SystemScopePage deepLinkCommandId="sys-inst" />);
    await sidebar();
    await waitFor(() =>
      expect(
        (
          screen.getByText("palbank services").closest("[data-selected]") as HTMLElement | null
        )?.getAttribute("data-selected"),
      ).toBe("true"),
    );
  });

  it("WAITS for a target that is not here yet — the index route's rule, cost included", async () => {
    // A terminal can appear a beat AFTER the page (the runtime is a second read), so the deep-link
    // is not abandoned on a first miss — and the auto-select stays suppressed meanwhile, or it
    // would race the deep-link onto the first pane and the arriving target would then yank the user
    // off whatever they had started reading.
    //
    // The cost is real, and it is `<TerminalsPage>`'s too: an id that will NEVER appear leaves an
    // empty deck. That is why the CALLERS keep it honest — `resolveSearchTarget` drops a
    // `?terminal=` at a dead terminal, and nothing routes a project's id to `/system`.
    installBackend();
    render(<SystemScopePage deepLinkTerminalId="not-in-this-scope" />);
    await sidebar();
    expect(selectedRow()).toBeNull();
  });

  it("takes NO deep-link at all as the plain page — the auto-select owns the entry", async () => {
    installBackend();
    render(<SystemScopePage />);
    await sidebar();
    await waitFor(() => expect(selectedRow()).not.toBeNull());
  });
});
