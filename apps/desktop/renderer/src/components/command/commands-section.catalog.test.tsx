import type { ReactNode } from "react";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/components/ui/toast", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));

/**
 * THE DRAG SEAM — identical to `command-groups.test.tsx`'s and to the terminals' list's, because a
 * real pointer drag is undrivable under jsdom (no layout ⇒ nothing for dnd-kit to collide against).
 * The provider is stubbed to capture `onDragEnd`; `move`, `dropIntent` and every controller call
 * downstream stay REAL, and here they run against the REAL `useCommandGroups` hook and the REAL
 * `command_group_*` / `command_reorder` routes.
 */
const dnd = vi.hoisted(() => ({
  onDragEnd: null as null | ((event: unknown) => void),
  onDragOver: null as null | ((event: unknown) => void),
}));

vi.mock("@dnd-kit/react", () => ({
  DragDropProvider: ({
    children,
    onDragEnd,
    onDragOver,
  }: {
    children: ReactNode;
    onDragEnd: (event: unknown) => void;
    onDragOver: (event: unknown) => void;
  }) => {
    dnd.onDragEnd = onDragEnd;
    // The drop commits THE PENDANT THAT WAS ON SCREEN rather than re-deriving it from the dragend
    // event, so a drag has to be WALKED here too: hover, then release.
    dnd.onDragOver = onDragOver;
    return <>{children}</>;
  },
}));

vi.mock("@dnd-kit/react/sortable", () => ({
  useSortable: () => ({ ref: () => {}, handleRef: () => {}, isDragging: false }),
}));

import { SidebarLineFamilyGlyph } from "@/components/ui/sidebar-line";
import { CommandsSection } from "./commands-section";
import type { CommandGroup } from "./use-command-groups";
import type { ManagedCommand } from "./use-commands";

/** The inert `manager` the RECORD branch of `move` dereferences for the pointer position. */
const MANAGER = { dragOperation: { shape: null, position: { current: { x: 0, y: 0 } } } };

/**
 * Walk one drop through the screen: HOVER the target (which computes the pendant), then RELEASE.
 * Both halves matter — the release commits what the hover put on screen.
 */
function fire(event: unknown): void {
  if (!dnd.onDragEnd || !dnd.onDragOver) {
    throw new Error("the drag handlers were not captured — the provider never rendered");
  }
  dnd.onDragOver(event);
  dnd.onDragEnd(event);
}

/**
 * Drag `sourceId` onto `targetId` through the captured handler — an EDGE drop, i.e. one of the four
 * position gestures. With no target box and no drag point, `readMergeTarget` cannot measure a band
 * and answers `null`, which is exactly what "released beside the line" means.
 */
function dragOnto(sourceId: string, targetId: string): void {
  fire({
    canceled: false,
    operation: {
      canceled: false,
      source: { id: sourceId, manager: MANAGER },
      target: { id: targetId },
    },
  });
}

/**
 * Drop `sourceId` on the MIDDLE BAND of `targetId` — gesture 5, the birth (human review
 * 01M1313GN2ESW0BCM8S1G0ZY2P). The band is a DOM reading, so the fake carries the two values it is
 * computed from: the target's box and the drag's live point, here at its dead centre.
 */
function dropOnMiddleOf(sourceId: string, targetId: string): void {
  const height = 32;
  fire({
    canceled: false,
    operation: {
      canceled: false,
      source: { id: sourceId, manager: MANAGER },
      target: {
        id: targetId,
        element: { getBoundingClientRect: () => ({ top: 0, height }) } as unknown as Element,
      },
      position: { current: { x: 0, y: height / 2 } },
    },
  });
}

/**
 * THE REAL COMMANDS SCREEN, NOT THE GROUPING COMPONENT ALONE (review finding
 * 01M0ZN7GYHQWCRTPBZARVC1FJM). `<CommandGroupSection>` was fully written and fully tested in
 * isolation while NOTHING mounted it: the composition of a command group was unreachable by hand,
 * which is precisely the asymmetry finding #90 (01M0ABR6V501D0SPR2BYBVZC4A) forbids — the agent
 * could create, compose and arrange a group over MCP and the user could not undo any of it.
 *
 * So this file mounts `<CommandsSection>` — the body the project-settings modal renders, and the
 * only one there is since the standalone commands modal was deleted (finding
 * 01M132YSBKJ5J22VG60P5GN7W0) — and drives the gestures end to end through the REAL
 * `useCommandGroups` hook and the REAL `command_group_*` IPC routes (doubled here by an in-memory
 * backend that enforces the same rules `db.rs` does). If the mount regresses, these fail.
 */

function template(over: Partial<ManagedCommand>): ManagedCommand {
  return {
    id: "c1",
    project_id: "p1",
    name: "check",
    command: "bun run check",
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

const TEMPLATES: ManagedCommand[] = [
  template({ id: "c1", name: "check", command: "bun run check", order_index: 0 }),
  template({ id: "c2", name: "build", command: "bun run build", order_index: 1 }),
  template({ id: "c3", name: "lint", command: "bun run lint", order_index: 2 }),
];

/**
 * The `command_group_*` half of the backend, in memory, with the transitions `db.rs` enforces: a
 * command belongs to zero or one group, a group holds at least two members (falling to one
 * DISSOLVES it), and the ordered member list IS the disposition.
 */
function installIpc(templates: ManagedCommand[] = TEMPLATES) {
  let groups: CommandGroup[] = [];
  // The command LIST has an order of its own now (`command_reorder`), so the double has to hold it:
  // "a command dropped out of a group stays where it fell" is only observable end to end if the
  // re-list comes back in the persisted order.
  let list = [...templates];
  let seq = 0;
  const routes: { cmd: string; args: Record<string, unknown> }[] = [];
  /** Every call, whatever the route — the ported specs assert on updates and imports. */
  const calls: { cmd: string; args: Record<string, unknown> }[] = [];
  const detach = (commandId: string) => {
    groups = groups
      .map((g) =>
        g.command_ids.includes(commandId)
          ? { ...g, command_ids: g.command_ids.filter((id) => id !== commandId) }
          : g,
      )
      .filter((g) => g.command_ids.length >= 2);
  };
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    calls.push({ cmd, args: a });
    if (cmd.startsWith("command_group_") || cmd === "command_reorder")
      routes.push({ cmd, args: a });
    switch (cmd) {
      case "command_list":
        return list;
      case "command_update":
        list = list.map((t) =>
          t.id === a.id
            ? {
                ...t,
                name: a.name as string,
                command: a.command as string,
                restart_on_startup: a.restartOnStartup as boolean,
              }
            : t,
        );
        return null;
      case "command_reorder": {
        const ids = a.ids as string[];
        list = ids
          .map((id) => list.find((t) => t.id === id))
          .filter((t): t is ManagedCommand => t != null);
        return null;
      }
      case "command_import_scripts":
        return [];
      case "command_group_list":
        return groups;
      case "command_group_create": {
        const ids = a.commandIds as string[];
        for (const id of ids) detach(id);
        groups = [
          ...groups,
          {
            id: `g${++seq}`,
            project_id: a.projectId as string,
            name: null,
            color: null,
            layout_shape: "auto",
            command_ids: [...ids],
          },
        ];
        return groups[groups.length - 1];
      }
      case "command_group_add": {
        detach(a.commandId as string);
        groups = groups.map((g) =>
          g.id === a.groupId ? { ...g, command_ids: [...g.command_ids, a.commandId as string] } : g,
        );
        return null;
      }
      case "command_group_remove":
        detach(a.commandId as string);
        return null;
      case "command_group_dissolve":
        groups = groups.filter((g) => g.id !== a.groupId);
        return null;
      case "command_group_labels":
        groups = groups.map((g) =>
          g.id === a.groupId
            ? {
                ...g,
                name: "name" in a ? (a.name as string | null) : g.name,
                color: "color" in a ? (a.color as CommandGroup["color"]) : g.color,
              }
            : g,
        );
        return null;
      case "command_group_layout":
        groups = groups.map((g) =>
          g.id === a.groupId ? { ...g, command_ids: [...(a.commandIds as string[])] } : g,
        );
        return null;
      case "command_group_shape":
        groups = groups.map((g) =>
          g.id === a.groupId ? { ...g, layout_shape: a.shape as CommandGroup["layout_shape"] } : g,
        );
        return null;
      default:
        return null;
    }
  });
  return { state: () => groups, routes, calls };
}

function renderScreen(templates: ManagedCommand[] = TEMPLATES) {
  const backend = installIpc(templates);
  const result = render(
    <CommandsSection active projectId="p1" importWorkspaceId={null} workspacePath="/repo" />,
  );
  return { ...result, ...backend };
}

/** The ONE list, as the screen renders it. */
function catalog(container: HTMLElement) {
  return container.querySelector("[data-command-list]") as HTMLElement | null;
}

/** Select a folder — the head's click, which is what opens its detail panel. */
function openGroup(container: HTMLElement, groupId: string): void {
  const head = container.querySelector(`[data-group-head='${groupId}']`);
  if (head == null) throw new Error(`no head for group ${groupId}`);
  fireEvent.click(head);
}

describe("the Commands screen is ONE list and ONE detail panel", () => {
  /**
   * THE LIST'S SHAPE DOES NOT DEPEND ON THE NUMBER (finding 01M132XFZ5KY8QMJ79HZZSBV3J). The
   * grouping used to be a SECOND SECTION that only existed from two commands on, so the screen
   * changed STRUCTURE with its content — and drew the same commands twice whenever it did.
   */
  it("draws every command ONCE, at any count, and never a second list", async () => {
    const { container, unmount } = renderScreen();
    await screen.findByText("check");
    await waitFor(() => expect(catalog(container)).not.toBeNull());
    for (const id of ["c1", "c2", "c3"]) {
      expect(container.querySelectorAll(`[data-command-row='${id}']`), id).toHaveLength(1);
    }
    // The second idiom is gone with the second list: no bordered card, no grip.
    expect(container.querySelector("[data-command-grouping]")).toBeNull();
    expect(container.querySelector("[data-command-grip]")).toBeNull();
    // And the count is written exactly once.
    expect(container.querySelectorAll("[data-command-count]")).toHaveLength(1);
    unmount();

    // ONE command: the SAME structure, one line shorter.
    const solo = renderScreen([TEMPLATES[0]]);
    await screen.findByText("check");
    await waitFor(() => expect(catalog(solo.container)).not.toBeNull());
    expect(solo.container.querySelectorAll("[data-command-row]")).toHaveLength(1);
  });

  /**
   * THE CATALOGUE DOES NOT RE-ORGANISE ITSELF AROUND THE SELECTION — the master/detail's promise.
   * Creating, editing and importing used to each move the list (a form at the top, a form INSIDE a
   * card, a tab that replaced it outright).
   */
  it("keeps the list identical while the panel changes under every selection", async () => {
    const { container } = renderScreen();
    await screen.findByText("check");
    const listShape = () =>
      Array.from(container.querySelectorAll("[data-command-row]")).map(
        (row) => (row as HTMLElement).dataset.commandRow,
      );
    const before = listShape();

    const panelKind = () =>
      (container.querySelector("[data-command-detail]") as HTMLElement).dataset.commandDetail;
    expect(panelKind()).toBe("none");

    fireEvent.click(container.querySelector("[data-command-select='c2']")!);
    expect(panelKind()).toBe("command");
    expect(listShape()).toEqual(before);

    fireEvent.click(screen.getByRole("button", { name: /new/i }));
    expect(panelKind()).toBe("new");
    expect(listShape(), "a creation does not push the list down").toEqual(before);
  });

  /**
   * COMMIT ON LEAVING A FIELD, END TO END (finding 01M132XFZ5KY8QMJ79HZZSBV3J). The switch has no
   * blur to wait for, so the flip IS the decision — and it must reach `command_update` with no
   * Save button anywhere. This is the round trip the deleted `<ProjectCommandsDialog>` spec used to
   * hold; it did it through a Save button that no longer exists.
   */
  it("persists a toggled restart_on_startup via command_update, with no Save button", async () => {
    const live = renderScreen();
    await screen.findByText("check");
    fireEvent.click(live.container.querySelector("[data-command-select='c1']")!);
    const toggle = await screen.findByRole("switch", { name: /restart on startup/i });
    expect(screen.queryByRole("button", { name: /^save$/i }), "the edition has no Save").toBeNull();

    fireEvent.click(toggle);

    await waitFor(() => {
      const updates = live.calls.filter((c) => c.cmd === "command_update");
      expect(updates).toHaveLength(1);
      expect(updates[0].args.id).toBe("c1");
      expect(updates[0].args.restartOnStartup).toBe(true);
    });
  });

  /**
   * THE HOST'S CLOSE RESETS THE TRANSIENT UI, so a reopen starts clean rather than on whatever was
   * last selected. `active` is the contract — the dialog flips it, and the section drops its
   * selection in the same commit.
   */
  it("drops the selection when the host surface deactivates", async () => {
    const backend = installIpc();
    const { container, rerender } = render(
      <CommandsSection active projectId="p1" importWorkspaceId={null} workspacePath="/repo" />,
    );
    await screen.findByText("check");
    fireEvent.click(container.querySelector("[data-command-select='c1']")!);
    expect(
      container.querySelector("[data-command-detail]")?.getAttribute("data-command-detail"),
    ).toBe("command");

    rerender(
      <CommandsSection
        active={false}
        projectId="p1"
        importWorkspaceId={null}
        workspacePath="/repo"
      />,
    );
    await waitFor(() =>
      expect(
        container.querySelector("[data-command-detail]")?.getAttribute("data-command-detail"),
      ).toBe("none"),
    );
    expect(backend.state()).toEqual([]);
  });

  it("CREATES, ENTERS, RE-SHAPES, RE-ARRANGES and EXITS — all five at the DRAG", async () => {
    const live = renderScreen();
    const section = await waitFor(() => {
      const found = catalog(live.container);
      expect(found).not.toBeNull();
      return found as HTMLElement;
    });

    // 1. CREATE — the fifth gesture: a command has no split, so a group is born by dropping one
    //    line on the MIDDLE of another (human review 01M1313GN2ESW0BCM8S1G0ZY2P). The partner is
    //    the line under the pointer, named by the ring before the release: c1 stays where it is and
    //    c2 joins it. The dotted slot this replaces chose the second member itself.
    expect(
      within(section).queryByText("Drop a command here to start a group"),
      "the permanent slot is gone",
    ).toBeNull();
    dropOnMiddleOf("c2", "c1");
    await waitFor(() => expect(live.state()).toHaveLength(1));
    expect(live.state()[0].command_ids).toEqual(["c1", "c2"]);

    // 2. ENTER — the third is dropped ON the first member, and lands THERE, not at the end. That
    //    insertion is what the removed "Add to group N" menu could not do.
    dragOnto("c3", "c1");
    await waitFor(() => expect(live.state()[0].command_ids).toEqual(["c3", "c1", "c2"]));

    // 3. RE-SHAPE — the tiling shape is chosen out of a closed vocabulary, and persisted. It lives
    //    in the FOLDER'S DETAIL PANEL now (finding 01M132XFZ5KY8QMJ79HZZSBV3J), not buried in the
    //    block: the folder's own line is what opens it.
    expect(section.querySelector("[data-layout-shape]"), "not in the block").toBeNull();
    openGroup(live.container, "g1");
    fireEvent.click(screen.getByLabelText("Layout column"));
    await waitFor(() => expect(live.state()[0].layout_shape).toBe("column"));

    // 4. RE-ARRANGE — a member dropped on another member re-orders the disposition, and the
    //    read-only preview follows it (it is the disposition, not a picture of it).
    const cellOrder = () =>
      Array.from(live.container.querySelectorAll("[data-layout-cell]")).map(
        (cell) => (cell as HTMLElement).dataset.layoutCell,
      );
    await waitFor(() => expect(cellOrder()).toEqual(["c3", "c1", "c2"]));
    dragOnto("c1", "c3");
    await waitFor(() => expect(live.state()[0].command_ids).toEqual(["c1", "c3", "c2"]));
    await waitFor(() => expect(cellOrder()).toEqual(["c1", "c3", "c2"]));

    // 5. EXIT — a member pulled out of the block leaves the group; the block is the only other
    //    entry in the root list, so dropping on it means "beside it", i.e. outside.
    dragOnto("c2", "block:g1");
    await waitFor(() => expect(live.state()[0].command_ids).toEqual(["c1", "c3"]));

    // Every gesture went over the REAL wire routes — no in-memory controller shortcut — INCLUDING
    // the list order, which nothing could write before this round.
    expect(live.routes.map((r) => r.cmd)).toEqual(
      expect.arrayContaining([
        "command_group_list",
        "command_group_create",
        "command_group_add",
        "command_group_shape",
        "command_group_layout",
        "command_group_remove",
        "command_reorder",
      ]),
    );
  });
});

/**
 * THE IMPORT IS A PANEL, NOT A TAB (finding 01M132XFZ5KY8QMJ79HZZSBV3J) — so the list stays on
 * screen and what lands is WATCHED landing. The tab did the exact opposite: it replaced the
 * catalogue while importing into it.
 *
 * The parallel-with-isolation behaviour below is the deleted `<ProjectCommandsDialog>` spec's,
 * ported: one row's refusal must not abort the others, and the re-list must still run.
 */
describe("the import is an ENTRY of the list", () => {
  it("imports the selection in PARALLEL, isolating a per-row failure, without leaving the panel", async () => {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    let list: ManagedCommand[] = [];
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        calls.push({ cmd, args: a });
        switch (cmd) {
          case "command_list":
            return list;
          case "command_group_list":
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
            ];
          case "command_import_create": {
            if (a.name === "bad") throw "backend refused bad";
            const created = template({
              id: "imp",
              name: a.name as string,
              command: a.command as string,
              source_kind: "package_json",
              source_package_json_path: a.sourcePackageJsonPath as string,
              source_script_name: a.sourceScriptName as string,
              source_script_command_snapshot: a.sourceScriptCommandSnapshot as string,
              package_manager: a.packageManager as string,
            });
            list = [...list, created];
            return created;
          }
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );

    const { container } = render(
      <CommandsSection active projectId="p1" importWorkspaceId="w1" workspacePath="/repo" />,
    );
    // THE ENTRY IS IN THE LIST'S FOOT — no tab anywhere.
    const entry = await waitFor(() => {
      const el = container.querySelector("[data-command-import-entry]");
      expect(el).not.toBeNull();
      return el as HTMLElement;
    });
    expect(container.querySelectorAll("[role=tab]")).toHaveLength(0);
    fireEvent.click(entry);

    fireEvent.click(await screen.findByLabelText("Select script dev"));
    fireEvent.click(screen.getByLabelText("Select script bad"));
    fireEvent.click(screen.getByRole("button", { name: /import selected/i }));

    // BOTH imports were attempted (the failing one did not short-circuit the other)…
    await waitFor(() => {
      const imports = calls.filter((c) => c.cmd === "command_import_create");
      expect(imports).toHaveLength(2);
      expect(imports.map((c) => c.args.name).sort()).toEqual(["bad", "dev"]);
    });
    // …the re-list ran despite the per-row rejection…
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "command_list").length).toBeGreaterThan(1),
    );
    // …and NOTHING navigated away: the panel is still the import, with the catalogue beside it.
    expect(
      container.querySelector("[data-command-detail]")?.getAttribute("data-command-detail"),
    ).toBe("import");
    await waitFor(() =>
      expect(container.querySelector("[data-command-row]")?.textContent).toContain("dev"),
    );
  });
});

/**
 * THE FAMILY'S GLYPH HAS ONE SOURCE (human review 01M140VRZJG0RE5VMG91D8TH3T).
 *
 * « Dans les settings commande, j'ai pas la même icons que dans la sidebar ? » — a command was drawn
 * `ServerCogIcon` in the sidebar and `TerminalIcon` here, so the same object wore two signs
 * depending on the screen. The glyph is a presentation decision of the same rank as the template and
 * the pastille, so it lives in `ui/sidebar-line.tsx` with them.
 *
 * MEASURED AGAINST THE SHARED BRICK, not against a literal icon: `group-head.test.tsx` holds the
 * sidebar to the very same reference, so the two screens are compared through it rather than through
 * a name either file could quietly change on its own.
 */
describe("a command wears the family glyph, on this screen too", () => {
  const familyGlyph = () => {
    const probe = render(<SidebarLineFamilyGlyph family="command" />);
    const svg = probe.container.querySelector("svg")?.outerHTML;
    probe.unmount();
    return svg;
  };

  it("draws the same glyph the sidebar draws — every line, and the detail header with them", async () => {
    const { container } = renderScreen();
    await screen.findByText("check");
    const reference = familyGlyph();
    expect(reference, "the shared brick draws something").toBeTruthy();

    for (const id of ["c1", "c2", "c3"]) {
      const lead = container.querySelector(
        `[data-command-select='${id}'] svg`,
      ) as SVGElement | null;
      expect(lead?.outerHTML, `${id}'s lead glyph`).toBe(reference);
    }

    // …AND THE PANEL ON THE RIGHT, which names the very same object as the line on the left.
    fireEvent.click(container.querySelector("[data-command-select='c1']") as HTMLElement);
    await waitFor(() => expect(container.querySelector("header")).not.toBeNull());
    const header = container.querySelector("header") as HTMLElement;
    expect(header.querySelector("svg")?.outerHTML, "the detail header wears it too").toBe(
      reference,
    );
  });
});
