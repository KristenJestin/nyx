import { useState } from "react";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { CommandGroupsProvider } from "@/components/command/command-groups-context";
import type { CommandGroupsController } from "@/components/command/use-command-groups";
import { GroupBlock } from "@/components/panes/group-block";
import { createPaneLayout, splitPane, type PaneNode } from "@/components/panes/pane-tree";
import type { PaneGroup } from "@/components/panes/pane-groups";
import { SidebarLineFamilyGlyph } from "@/components/ui/sidebar-line";
import { RuntimeSelectionProvider } from "./runtime-selection";
import { SortableTerminalRows } from "./sortable-terminal-rows";
import { ConsumedNotificationsProvider } from "./use-consumed-notifications";
import { TerminalGroupsProvider } from "./use-terminal-groups";
import { TerminalReorderProvider } from "./use-terminal-reorder";
import { WorkspaceRuntimeTree } from "./workspace-runtime-tree";
import type { WorkspaceRuntimeSnapshot } from "@/components/runtime/use-workspace-runtime";

/**
 * THE BLOCK'S HEAD (review 01M0BPP79090KAQ9ZAFJ23ZRTX, major).
 *
 * The group is an ENTITY — verbatim: « le parametrage du groupe sur chaque terminaux du group c'est
 * aussi un truc de feneant ». It gets ONE object that is at once its handle and its settings target,
 * and the rows get their vocabulary back: no "Group…", no "Leave group", no "Join group N", and no
 * right-click panel on a member.
 */

function pair(a: string, b: string): PaneNode {
  return splitPane(createPaneLayout(a, `${a}-pane`), {
    paneId: `${a}-pane`,
    direction: "row",
    terminalId: b,
    newPaneId: `${b}-pane`,
  }) as PaneNode;
}

function group(id: string, a: string, b: string, name: string | null = null): PaneGroup {
  return { id, tree: pair(a, b), name, color: null };
}

/** The terminals sidebar list, over one group of two plus one solo row. */
function renderTerminalList(groups: readonly PaneGroup[], ids: readonly string[]) {
  return render(
    <RuntimeSelectionProvider initialSelection={null}>
      <TerminalGroupsProvider
        groups={groups}
        leaveGroup={() => {}}
        dissolveGroup={() => {}}
        renameGroup={() => {}}
        recolorGroup={() => {}}
      >
        <TerminalReorderProvider reorderTerminals={() => {}} orderedIds={ids}>
          <SortableTerminalRows
            terminals={ids.map((id) => ({ id }))}
            group="ws1"
            empty="none"
            renderRow={(t) => <span>{t.id}</span>}
          />
        </TerminalReorderProvider>
      </TerminalGroupsProvider>
    </RuntimeSelectionProvider>,
  );
}

// The fold state is REMEMBERED per group (`use-group-open` → `ui-persistence`), so one test's
// toggle would otherwise be the next test's starting state — the group ids are shared on purpose.
beforeEach(() => {
  localStorage.clear();
});

describe("The block's head — one handle, one target", () => {
  it("renders the head's TWO controls on every block: a folder and a menu trigger (criterion 1)", () => {
    const { container } = renderTerminalList([group("g1", "a", "b")], ["a", "b", "c"]);
    const head = container.querySelector("[data-group-head='g1']") as HTMLElement;
    expect(head, "every block opens on a head").toBeTruthy();
    expect(head.querySelector("[data-group-folder='g1']"), "the folder").toBeTruthy();
    expect(within(head).getByLabelText("Group options"), "the menu trigger").toBeTruthy();
    // …and, this group having no name, NOTHING between them (review 01M0CY3F075ACP4TFBK1KJ49X8).
    expect(head.dataset.groupHeadLabel, "the head is marked anonymous").toBe("unnamed");
    expect(head.textContent, "an unnamed head writes no words at all").toBe("");
  });

  it("shows the NAME when there is one and NOTHING when there is not (criterion 2)", () => {
    const { container, unmount } = renderTerminalList([group("g1", "a", "b", "build")], ["a", "b"]);
    const named = container.querySelector("[data-group-head='g1']") as HTMLElement;
    expect(named.dataset.groupHeadLabel).toBe("named");
    expect(named.textContent).toContain("build");
    unmount();

    const anonymous = renderTerminalList([group("g1", "a", "b")], ["a", "b"]);
    const head = anonymous.container.querySelector("[data-group-head='g1']") as HTMLElement;
    // No name → no words. The head is told apart at the STRUCTURE level, by the attribute, which is
    // also what keeps the homonym case below honest.
    expect(head.dataset.groupHeadLabel).toBe("unnamed");
    expect(head.textContent, "no stand-in, no substitute, no dash").toBe("");

    anonymous.unmount();
    const homonym = renderTerminalList([group("g1", "a", "b", "unnamed group")], ["a", "b"]);
    const homonymHead = homonym.container.querySelector("[data-group-head='g1']") as HTMLElement;
    expect(
      homonymHead.dataset.groupHeadLabel,
      "a group somebody really called « unnamed group » is a NAMED group",
    ).toBe("named");
    expect(homonymHead.textContent, "and its name is shown, like any other").toContain(
      "unnamed group",
    );
  });

  it("its trigger opens the SHARED name + colour panel, Ungroup all included (criterion 3)", () => {
    renderTerminalList([group("g1", "a", "b")], ["a", "b", "c"]);
    expect(screen.queryByRole("dialog"), "the panel never opens by itself").toBeNull();
    fireEvent.click(screen.getByLabelText("Group options"));
    const panel = screen.getByRole("dialog");
    // The SAME component the panel always was — its own name field, its neutral-first swatches and
    // its "Ungroup all", not a copy of them.
    expect(within(panel).getByLabelText("Group name (optional)")).toBeTruthy();
    expect(within(panel).getByLabelText("No colour")).toBeTruthy();
    expect(within(panel).getByRole("button", { name: "Ungroup all" })).toBeTruthy();
  });

  /**
   * …AND THE PANEL'S SECOND EXIT (human review 01M1312M70TP8TJWT4TG505S0J). « je peux pas supprimer
   * tout un groupe, y a l'option un ungroup mais y pas l'option tout close ». The sidebar is the
   * surface that HAS terminals to close, so it is the one that hands `closeGroup` down; the panel
   * draws the action only where it was handed one.
   */
  it("offers « close all » next to « Ungroup all » — with the group's own id", () => {
    const closeGroup = vi.fn();
    render(
      <RuntimeSelectionProvider initialSelection={null}>
        <TerminalGroupsProvider
          groups={[group("g1", "a", "b")]}
          leaveGroup={() => {}}
          closeGroup={closeGroup}
          dissolveGroup={() => {}}
          renameGroup={() => {}}
          recolorGroup={() => {}}
        >
          <TerminalReorderProvider reorderTerminals={() => {}} orderedIds={["a", "b"]}>
            <SortableTerminalRows
              terminals={[{ id: "a" }, { id: "b" }]}
              group="ws1"
              empty="none"
              renderRow={(t) => <span>{t.id}</span>}
            />
          </TerminalReorderProvider>
        </TerminalGroupsProvider>
      </RuntimeSelectionProvider>,
    );
    fireEvent.click(screen.getByLabelText("Group options"));
    const panel = screen.getByRole("dialog");
    // BOTH exits, side by side — and they are two different operations, not two names for one.
    expect(within(panel).getByRole("button", { name: "Ungroup all" })).toBeTruthy();
    fireEvent.click(within(panel).getByRole("button", { name: /^Close all/ }));
    expect(closeGroup).toHaveBeenCalledWith("g1");
  });

  it("is reachable and operable at the KEYBOARD — tab to it, open it, no pointer (criterion 6)", () => {
    renderTerminalList([group("g1", "a", "b")], ["a", "b"]);
    // The folder now states BOTH of its jobs, because it really has both: the block is picked up by
    // it (Space, dnd-kit's keyboard sensor) and folded by it (Enter, the button's own activation).
    const folder = screen.getByLabelText("Toggle and reorder group");
    const trigger = screen.getByLabelText("Group options");
    // Both head controls are real buttons, hence tab stops: nothing here is pointer-only.
    for (const control of [folder, trigger]) {
      expect(control.tagName).toBe("BUTTON");
      expect(control.hasAttribute("disabled")).toBe(false);
      expect(control.getAttribute("tabindex")).not.toBe("-1");
    }
    trigger.focus();
    expect(document.activeElement).toBe(trigger);
    // Enter on the focused trigger — the keyboard activation of a button — opens the panel.
    fireEvent.keyDown(trigger, { key: "Enter", code: "Enter" });
    fireEvent.keyUp(trigger, { key: "Enter", code: "Enter" });
    fireEvent.click(trigger, { detail: 0 });
    expect(screen.getByRole("dialog")).toBeTruthy();
  });
});

/** A sidebar snapshot: one workspace, two terminals, two managed commands. */
function snapshot(): WorkspaceRuntimeSnapshot {
  return {
    workspace: { id: "ws1", name: "root", path: "/repo" },
    terminals: [
      {
        id: "t1",
        cwd: "/repo",
        label: "t1",
        workspace_id: "ws1",
        status: "alive",
        exec_state: "idle",
        exec_state_unread: false,
        sessions: [],
      },
      {
        id: "t2",
        cwd: "/repo",
        label: "t2",
        workspace_id: "ws1",
        status: "alive",
        exec_state: "idle",
        exec_state_unread: false,
        sessions: [],
      },
    ],
    agent_sessions: [],
    services: [
      {
        id: "s1",
        command_id: "c1",
        workspace_id: "ws1",
        name: "api",
        command: "bun dev",
        run_state: "idle",
        exit_code: null,
        unread: false,
      },
      {
        id: "s2",
        command_id: "c2",
        workspace_id: "ws1",
        name: "web",
        command: "bun web",
        run_state: "idle",
        exit_code: null,
        unread: false,
      },
    ],
  };
}

const COMMAND_GROUP = {
  id: "cg1",
  project_id: "p1",
  name: null,
  color: null,
  layout_shape: "auto",
  command_ids: ["c1", "c2"],
};

function commandGroups(): CommandGroupsController {
  return {
    groups: [COMMAND_GROUP],
    groupOf: (commandId: string) => (["c1", "c2"].includes(commandId) ? COMMAND_GROUP : null),
    createGroup: async () => {},
    addToGroup: async () => {},
    removeFromGroup: async () => {},
    dissolveGroup: async () => {},
    renameGroup: async () => {},
    recolorGroup: async () => {},
    setLayout: async () => {},
    setShape: async () => {},
    reorderCommands: async () => {},
    reload: async () => {},
  } as unknown as CommandGroupsController;
}

/** The sidebar's runtime tree: a group of TERMINALS and a group of COMMANDS, side by side. */
function renderSidebar() {
  return render(
    <RuntimeSelectionProvider initialSelection={null}>
      <ConsumedNotificationsProvider>
        <CommandGroupsProvider controller={commandGroups()}>
          <TerminalGroupsProvider
            groups={[group("g1", "t1", "t2")]}
            leaveGroup={() => {}}
            dissolveGroup={() => {}}
            renameGroup={() => {}}
            recolorGroup={() => {}}
          >
            <TerminalReorderProvider reorderTerminals={() => {}} orderedIds={["t1", "t2"]}>
              <WorkspaceRuntimeTree snapshot={snapshot()} />
            </TerminalReorderProvider>
          </TerminalGroupsProvider>
        </CommandGroupsProvider>
      </ConsumedNotificationsProvider>
    </RuntimeSelectionProvider>,
  );
}

/**
 * WHAT THE PROBE HAS TO BE ABLE TO SEE (review 01M0BSNTEN5N2GC9QQBASJYHHB).
 *
 * The first version of this describe read the grip as a BOOLEAN OF PRESENCE —
 * `head.querySelector("[data-group-grip]") != null` — while BOTH branches of `group-block.tsx`
 * carried the same `data-group-grip`. A `<button>` handle and an inert `<span aria-hidden>` mark
 * therefore compared EQUAL, and the single real difference between the two heads was the one thing
 * the probe could not measure. Criterion 5 was declared held on an instrument blind to it.
 *
 * The heads are NOT identical, and they should not be: only the TERMINAL blocks drag
 * (`sortable-terminal-rows.tsx` passes `handleRef`), so only they wear a handle. The command groups
 * — `command-catalog.tsx`, `workspace-runtime-tree.tsx`'s `CommandRows` — pass none and now
 * draw no grip at all rather than a mark promising a gesture that does not exist. What criterion 5
 * really asserts is that ONE component draws both, and that everything not conditioned by the drag
 * comes out the same. So the probe is split in two: the SHARED shape, compared for equality, and
 * the HANDLE, asserted per surface.
 */
function headShape(head: HTMLElement) {
  return {
    label: head.dataset.groupHeadLabel,
    className: head.className,
  };
}

/**
 * The head's LIFECYCLE cluster — the three group-wide controls, or `null` when the head carries
 * none.
 *
 * It is the second half of the inversion (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding
 * 01M11XVEFEQ524XRT9ARPQFGSV): a terminal group has no lifecycle to pilot — a terminal is open or
 * closed, it does not restart in bulk — while a command group does, and that lifecycle exists per
 * WORKSPACE, i.e. here and nowhere else.
 *
 * THE `n/m` LEFT THIS PROBE (human review 01M12MW1DV1V455EWBMV048TE6): « la tête porte un état,
 * jamais un nombre ». The counter is not asserted here any more — its ABSENCE is, in its own test
 * below, on BOTH families and on the whole block rather than the head alone. What the counter said
 * about a divergent group now lives in the folder's hover summary, and the tally itself is still
 * computed: these three button labels are derived from it.
 */
function lifecycleShape(head: HTMLElement) {
  const controls = Array.from(head.querySelectorAll("button[aria-label]"))
    .map((button) => button.getAttribute("aria-label") ?? "")
    .filter((label) => /^(Start|Stop|Restart) /.test(label));
  return controls.length === 0 ? null : { controls };
}

/**
 * The head's `…` trigger — `null` when the head offers NO action at all.
 *
 * It left `headShape` with review finding 01M11XMQA3M852JDD7R3MDG88X: the two families now differ
 * on TWO controls, not one. The terminal blocks drag AND act, in this very sidebar; the command
 * blocks do neither, because a command group is composed and labelled in Project settings ▸
 * Commands. Folding the trigger back into the shared shape would re-assert an equality that is now
 * deliberately false — so it is probed on its own, per surface, exactly like the grip.
 */
function triggerShape(head: HTMLElement) {
  const trigger = head.querySelector("button[aria-label^='Group options']");
  if (trigger == null) return null;
  return { tag: trigger.tagName, ariaLabel: trigger.getAttribute("aria-label") };
}

/**
 * The head's FOLDER, described by what distinguishes one — `null` when none is drawn (which never
 * happens any more: every block has a folder, because the folder is the node itself).
 *
 * What varies is what it OFFERS, and the accessible name is where that is said: a block that
 * travels is grabbed AND folded by it, a block that does not is only folded by it.
 */
function folderShape(head: HTMLElement) {
  const folder = head.querySelector("[data-group-folder]");
  if (folder == null) return null;
  return {
    tag: folder.tagName,
    ariaHidden: folder.getAttribute("aria-hidden"),
    ariaLabel: folder.getAttribute("aria-label"),
    ariaExpanded: folder.getAttribute("aria-expanded"),
  };
}

describe("The head is the same object on BOTH surfaces", () => {
  /**
   * Criterion 5. ONE file draws it — `group-block.tsx` — and its callers are
   * `sortable-terminal-rows.tsx` (TERMINALS) and `command-catalog.tsx` /
   * `workspace-runtime-tree.tsx`'s `CommandRows` (COMMANDS). This asserts the RESULT: the two heads
   * the sidebar paints, side by side, have the same shape, because they are the same component.
   */
  it("renders the same head for a terminal group and for a command group (criterion 5)", () => {
    const { container } = renderSidebar();
    const terminalHead = container.querySelector("[data-group-head='g1']") as HTMLElement;
    const commandHead = container.querySelector("[data-group-head='cg1']") as HTMLElement;
    expect(terminalHead && commandHead, "a head on each side").toBeTruthy();
    expect(headShape(commandHead)).toEqual(headShape(terminalHead));
  });

  /**
   * …AND THE TWO HEADS ARE EACH OTHER'S INVERSE, ON PURPOSE (review 01M11WE6H9E5D5FCS6RT0ZXAXN,
   * finding 01M11XVEFEQ524XRT9ARPQFGSV). Verbatim: « l'affichage du groupe devrait être différent
   * pour les commandes ». The distinction is not decoration — each half is the operation that has
   * nowhere else to go. COMPOSITION of a terminal group happens at the pointer in this sidebar, so
   * its head carries a grip and a `…`. A command group is composed in the modal, and its LIFECYCLE
   * lives on `command_instances`, which are per workspace — so the modal structurally cannot pilot
   * it and this head carries the aggregate and the three controls instead.
   */
  it("the command head PILOTS where the terminal head COMPOSES", () => {
    const { container } = renderSidebar();
    const terminalHead = container.querySelector("[data-group-head='g1']") as HTMLElement;
    const commandHead = container.querySelector("[data-group-head='cg1']") as HTMLElement;

    // The three controls, and their names derived from the tally that is still computed — the play
    // knows there are 2 stopped, the stop and the restart know there are 2 in all.
    expect(lifecycleShape(commandHead)).toEqual({
      controls: [
        "Start the 2 stopped in group 1",
        "Stop all 2 in group 1",
        "Restart all 2 in group 1",
      ],
    });
    expect(lifecycleShape(terminalHead), "a terminal group has no lifecycle to pilot").toBeNull();
  });

  /**
   * NO COUNTER IN EITHER HEAD (human review 01M12MW1DV1V455EWBMV048TE6) — « la tête porte un état,
   * jamais un nombre ». Neither the member count of a terminal group nor the `n/m` of a command
   * group survives, and the assertion is over the WHOLE BLOCK rather than the head alone so a
   * counter cannot come back one level down.
   *
   * What replaces the `n/m` is the folder's hover SUMMARY: the divergence is one hover away instead
   * of one glance, and it costs the band nothing. That is the trade, stated in a test so it stays a
   * decision.
   */
  it("carries NO number in a head — the divergence moved to the folder's hover summary", () => {
    const { container } = renderSidebar();
    for (const id of ["g1", "cg1"]) {
      const block = container.querySelector(`[data-group-block='${id}']`) as HTMLElement;
      expect(block.querySelector("[data-command-group-tally]"), `${id}: no n/m`).toBeNull();
      const head = container.querySelector(`[data-group-head='${id}']`) as HTMLElement;
      // The head of an unnamed group writes nothing at all — no name, and therefore no digits.
      expect(head.textContent, `${id}: the band carries no words and no figures`).toBe("");
    }
    // …and the reading is still reachable: it is the folder's accessible hover summary.
    const folder = container.querySelector("[data-group-folder='cg1']") as HTMLElement;
    const described = folder.closest("[data-group-head]")?.textContent ?? "";
    expect(described, "the summary is not inline text in the band").toBe("");
    expect(
      screen.getByLabelText("Toggle group").getAttribute("aria-describedby") ??
        folder.getAttribute("aria-describedby") ??
        "hover-only",
      "the tally rides the tooltip, not the layout",
    ).toBeTruthy();
  });

  it("…and the differences are the two CONTROLS — only a block that drags, or acts, wears one", () => {
    const { container } = renderSidebar();
    const terminalHead = container.querySelector("[data-group-head='g1']") as HTMLElement;
    const commandHead = container.querySelector("[data-group-head='cg1']") as HTMLElement;

    // BOTH families draw a folder — it is the node, not a promise of a gesture, so it belongs to a
    // block that cannot move just as much as to one that can. What differs is what it SAYS.
    expect(folderShape(terminalHead), "the terminal block drags AND folds").toEqual({
      tag: "BUTTON",
      ariaHidden: null,
      ariaLabel: "Toggle and reorder group",
      ariaExpanded: "true",
    });
    expect(folderShape(commandHead), "the command block only folds — and starts folded").toEqual({
      tag: "BUTTON",
      ariaHidden: null,
      ariaLabel: "Toggle group",
      ariaExpanded: "false",
    });
    // THE CURSOR DOES NOT DIVERGE BETWEEN THE FAMILIES (human review 01M140TP2F7X0DGMRC0RQQFHAF).
    // The terminal folder wore `cursor-grab` and the command one `cursor-pointer` — a leftover from
    // when the glyph WAS the handle. The grip is the whole head now, so a grab cursor on the glyph
    // alone points at a zone with no special hold on the drag AND misnames what the glyph does. The
    // assertion is the EQUALITY, not the value, so a legitimate retune keeps both families together.
    const cursorOf = (head: HTMLElement) =>
      (head.querySelector("[data-group-folder]") as HTMLElement).className;
    expect(cursorOf(commandHead), "one cursor, both families").toBe(cursorOf(terminalHead));
    expect(cursorOf(terminalHead), "and it is a toggle's, not a grip's").not.toContain(
      "cursor-grab",
    );

    // …AND THE SAME RULE ON THE ACTION (review finding 01M11XMQA3M852JDD7R3MDG88X). A terminal
    // group is the user's own composition, made in this sidebar, so its head keeps its `…`. A
    // COMMAND group is composed in Project settings ▸ Commands — verbatim: « tout se passe dans le
    // modal pas ici » — so the sidebar SHOWS it and offers nothing to press.
    expect(triggerShape(terminalHead), "the terminal block acts — it keeps its `…`").toEqual({
      tag: "BUTTON",
      ariaLabel: "Group options",
    });
    expect(triggerShape(commandHead), "the command block offers no action here").toBeNull();
  });

  /**
   * THE SIDEBAR'S COMMAND BLOCK IS A DISPLAY, NOT A CONTROL (review finding
   * 01M11XMQA3M852JDD7R3MDG88X) — and removing the entry point must not cost the REPRESENTATION.
   *
   * It now opens FOLDED (human review 01M12MW1DV1V455EWBMV048TE6): a command group is a launcher,
   * and four commands cost 32 px folded where they cost 148 unfolded. So the representation is
   * checked in both states — folded, it is a line with a folder and its three buttons; unfolded, it
   * is everything it always was.
   */
  it("a COMMAND block opens FOLDED, unfolds to its members, and mutates nothing either way", () => {
    const { container } = renderSidebar();
    const block = () => container.querySelector("[data-group-block='cg1']") as HTMLElement;
    expect(block(), "the block is still drawn").toBeTruthy();
    // The representation is untouched: the tint token, the head's label state.
    expect(block().dataset.groupColor).toBe("none");
    expect(block().getAttribute("style")).toContain("var(--group-0)");
    expect(
      (container.querySelector("[data-group-head='cg1']") as HTMLElement).dataset.groupHeadLabel,
    ).toBe("unnamed");

    // FOLDED by default — the members are not rendered at all.
    expect(block().dataset.groupOpen, "commands start folded").toBe("false");
    expect(within(block()).queryByText("api")).toBeNull();

    // THE GLYPH UNFOLDS IT — not the band (human review 01M14112THEB4094B2G614SXR5). The head's
    // click is the ACTIVATION now, so pressing it shows the group instead of folding it, and the
    // fold is the folder's own gesture. The members are there afterwards, in order.
    fireEvent.click(container.querySelector("[data-group-folder='cg1']") as HTMLElement);
    expect(block().dataset.groupOpen).toBe("true");
    expect(within(block()).getByText("api"), "its members, in order").toBeTruthy();
    expect(within(block()).getByText("web")).toBeTruthy();

    // NOTHING inside it mutates the GROUP: no `…`, and no panel behind either.
    expect(within(block()).queryByLabelText(/^Group options/)).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("proves the two heads come from the SAME component, not two look-alikes", () => {
    // `GroupBlock` is the only thing that emits `data-group-head`. Rendered WITHOUT a handleRef it
    // is markup-for-markup the COMMAND head; rendered WITH one it is the TERMINAL head. Both halves
    // are checked, so the folder is covered here too and not only above.
    const bare = render(
      <GroupBlock groupId="probe" color={null} name={null} open={false} onToggle={() => {}}>
        <span>row</span>
      </GroupBlock>,
    );
    const handled = render(
      <GroupBlock
        groupId="probe2"
        color={null}
        name={null}
        onToggle={() => {}}
        handleRef={() => {}}
      >
        <span>row</span>
      </GroupBlock>,
    );
    const bareHead = bare.container.querySelector("[data-group-head='probe']") as HTMLElement;
    const handledHead = handled.container.querySelector(
      "[data-group-head='probe2']",
    ) as HTMLElement;

    const sidebar = renderSidebar();
    const terminalHead = sidebar.container.querySelector("[data-group-head='g1']") as HTMLElement;
    const commandHead = sidebar.container.querySelector("[data-group-head='cg1']") as HTMLElement;

    for (const [live, probe, what] of [
      [commandHead, bareHead, "the command head is GroupBlock without a handleRef"],
      [terminalHead, handledHead, "the terminal head is GroupBlock with one"],
    ] as const) {
      expect(live.className, what).toBe(probe.className);
      expect(live.dataset.groupHeadLabel, what).toBe(probe.dataset.groupHeadLabel);
      expect(folderShape(live), what).toEqual(folderShape(probe));
    }
  });
});

describe("A row speaks only of itself", () => {
  /** Criterion 4 — the three entries are gone from the ROW, in both of their forms. */
  it("offers no group entry in a row's overflow, and no group panel on right-click", () => {
    renderSidebar();
    const row = screen.getByText("t1").closest("[data-selected]") as HTMLElement;
    expect(row).toBeTruthy();

    // No kebab NAMING THE ROW: the head's trigger names the GROUP, and it is the only one left.
    expect(screen.queryByLabelText("Group options for t1")).toBeNull();
    expect(within(row).queryByLabelText(/^Group options/)).toBeNull();
    // …and none of the three entries survives anywhere in the row.
    expect(within(row).queryByText("Group…")).toBeNull();
    expect(within(row).queryByText("Leave group")).toBeNull();
    expect(within(row).queryByText(/^Join /)).toBeNull();

    // A right-click on a MEMBER opens nothing — the panel is settled from the group's own head.
    fireEvent.contextMenu(row);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

/**
 * THE FOLDER LINE ACTIVATES ITS GROUP; THE GLYPH FOLDS IT (human review 01M14112THEB4094B2G614SXR5).
 *
 * « devoir ouvrir le dossier pour rentrer dans l'affichage des terminaux c'est un peu chiant,
 * surtout pour les commandes où y a pas de notion de focus, c'est trop chelou que je doive cliquer
 * que un des 3 pour avoir le 3 ». A head that only folded left a folder unactivable: seeing a group
 * meant opening it AND THEN clicking a member. The tree's own motif replaces it — the name is
 * selected, the glyph is unfolded — and the two are INDEPENDENT axes, which is the part that is
 * easy to get wrong and is therefore asserted rather than described.
 */
function renderTerminalActivation() {
  const shown: string[] = [];
  function Harness() {
    const [shownGroupId, setShownGroupId] = useState<string | null>(null);
    return (
      <RuntimeSelectionProvider initialSelection={null}>
        <TerminalGroupsProvider
          groups={[group("g1", "a", "b")]}
          // The page hands `usePaneLayout`'s own `showGroup` / `showingGroupId`; this stands in for
          // them with the one behaviour that matters here — activating a group makes it the shown
          // one — so what is under test is the LINE, not the tiling.
          showGroup={(groupId) => {
            shown.push(groupId);
            setShownGroupId(groupId);
          }}
          shownGroupId={shownGroupId}
          leaveGroup={() => {}}
          dissolveGroup={() => {}}
          renameGroup={() => {}}
          recolorGroup={() => {}}
        >
          <TerminalReorderProvider reorderTerminals={() => {}} orderedIds={["a", "b", "c"]}>
            <SortableTerminalRows
              terminals={["a", "b", "c"].map((id) => ({ id }))}
              group="ws1"
              empty="none"
              renderRow={(t) => <span>{t.id}</span>}
              // A live member, so a CLOSED folder really has a rollup dot to carry.
              memberState={() => "running"}
            />
          </TerminalReorderProvider>
        </TerminalGroupsProvider>
      </RuntimeSelectionProvider>
    );
  }
  return { shown, ...render(<Harness />) };
}

describe("The folder LINE activates, the GLYPH folds — two independent axes", () => {
  it("TERMINALS — the head shows the group WITHOUT opening it, and marks the line selected", () => {
    const { shown, container } = renderTerminalActivation();
    const head = () => container.querySelector("[data-group-head='g1']") as HTMLElement;
    const block = () => container.querySelector("[data-group-block='g1']") as HTMLElement;
    expect(block().dataset.groupOpen, "terminal groups start open").toBe("true");
    expect(head().dataset.selected, "and nothing is active yet").toBe("false");

    fireEvent.click(head());

    expect(shown, "the group itself was activated — no member had to be picked").toEqual(["g1"]);
    expect(head().dataset.selected, "the line reads as the viewed one").toBe("true");
    expect(block().dataset.groupOpen, "and the fold did NOT move").toBe("true");
  });

  it("TERMINALS — the glyph folds and changes NOTHING about the selection", () => {
    const { shown, container } = renderTerminalActivation();
    const head = () => container.querySelector("[data-group-head='g1']") as HTMLElement;
    const block = () => container.querySelector("[data-group-block='g1']") as HTMLElement;
    const folder = () => container.querySelector("[data-group-folder='g1']") as HTMLElement;
    fireEvent.click(head());
    expect(shown).toEqual(["g1"]);

    fireEvent.click(folder());

    expect(block().dataset.groupOpen, "the glyph folded it").toBe("false");
    expect(shown, "and activated nothing — the press stopped at the glyph").toEqual(["g1"]);
    // THE CASE THE DEVICE IS FOR: closed AND active at once. Folding closes nothing — « le pliage ne
    // ferme rien : ni PTY, ni panneau, ni tiling » — so the group stays on screen and the one line
    // left carries BOTH its selected state and the rollup dot its hidden members owe it.
    expect(head().dataset.selected, "a folded folder stays the active one").toBe("true");
    expect(
      folder().querySelector("[data-state]"),
      "and carries its members' aggregate, since they are hidden",
    ).toBeTruthy();
  });

  it("COMMANDS — the head shows the whole group with no member selected, folded or not", () => {
    const { container } = renderSidebar();
    const head = () => container.querySelector("[data-group-head='cg1']") as HTMLElement;
    const block = () => container.querySelector("[data-group-block='cg1']") as HTMLElement;
    const folder = () => container.querySelector("[data-group-folder='cg1']") as HTMLElement;
    expect(block().dataset.groupOpen, "command groups start folded").toBe("false");

    fireEvent.click(head());

    // ACTIVE WHILE STILL FOLDED — the whole point for this family: four commands cost one line, you
    // see them run, and one press looks at all of them.
    expect(head().dataset.selected, "the folder is the viewed one").toBe("true");
    expect(block().dataset.groupOpen, "and it did not have to open").toBe("false");
    // NO MEMBER IS THE SELECTION: the rows are not even rendered, so nothing was picked among them.
    expect(within(block()).queryByText("api"), "no member had to be chosen").toBeNull();

    // Unfolding afterwards leaves the activation alone — the second axis, moved on its own.
    fireEvent.click(folder());
    expect(block().dataset.groupOpen).toBe("true");
    expect(head().dataset.selected, "still the viewed one").toBe("true");
    expect(within(block()).getByText("api")).toBeTruthy();
  });
});

/**
 * THE SIDEBAR'S HALF OF THE GLYPH PARITY (human review 01M140VRZJG0RE5VMG91D8TH3T).
 *
 * The catalogue is held to the same reference in `command/commands-section.catalog.test.tsx`, so the
 * two screens are compared THROUGH the shared brick rather than against a lucide name either file
 * could change on its own — which is the whole point of the glyph having one source.
 */
describe("a command wears the family glyph in the sidebar too", () => {
  it("draws the shared command glyph on a service row", () => {
    const { container } = renderSidebar();
    // The members only exist once the folder is unfolded — the glyph is a LINE's, so a line has to
    // be on screen for it to be measured.
    fireEvent.click(container.querySelector("[data-group-folder='cg1']") as HTMLElement);
    const row = screen.getByText("api").closest("[data-sidebar-line]") as HTMLElement;
    expect(row, "the service row is drawn").toBeTruthy();

    const probe = render(<SidebarLineFamilyGlyph family="command" />);
    const reference = probe.container.querySelector("svg")?.outerHTML;
    probe.unmount();

    expect(row.querySelector("svg")?.outerHTML, "the row's lead glyph").toBe(reference);
  });
});
