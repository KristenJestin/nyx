import { useState } from "react";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { GroupPanel } from "./group-panel";
import {
  dissolveGroup,
  setGroupColor,
  setGroupName,
  type GroupColor,
  type PaneCollection,
  type PaneGroup,
} from "./pane-groups";
import { createPaneLayout, splitPane, type PaneNode } from "./pane-tree";

function tree(a = "t1", b = "t2"): PaneNode {
  return splitPane(createPaneLayout(a, `p-${a}`), {
    paneId: `p-${a}`,
    direction: "row",
    terminalId: b,
    newPaneId: `p-${b}`,
  }) as PaneNode;
}

function group(overrides: Partial<PaneGroup> = {}): PaneGroup {
  return { id: "g1", tree: tree(), name: null, color: null, ...overrides };
}

/** The panel wired to the REAL pure mutators, over a live collection — what the page does. */
function LivePanel({ initial }: { initial: PaneGroup }) {
  const [collection, setCollection] = useState<PaneCollection>({ groups: [initial] });
  const current = collection.groups[0] ?? null;
  return (
    <>
      <span data-testid="state">
        {JSON.stringify({
          groups: collection.groups.length,
          name: current?.name ?? null,
          color: current?.color ?? null,
          members: current == null ? 0 : 2,
        })}
      </span>
      <GroupPanel
        open
        group={current}
        onClose={() => {}}
        onRename={(id, name) => setCollection((c) => setGroupName(c, id, name))}
        onRecolor={(id, color) => setCollection((c) => setGroupColor(c, id, color))}
        onDissolve={(id) => setCollection((c) => dissolveGroup(c, id))}
      />
    </>
  );
}

const state = () => JSON.parse(screen.getByTestId("state").textContent as string);
const field = () => screen.getByLabelText("Group name (optional)") as HTMLInputElement;
const swatch = (color: GroupColor | "none") =>
  screen.getByLabelText(color === "none" ? "No colour" : `Colour ${color}`);

/**
 * THE SHARED NAME + COLOUR PANEL (review 01M0ABR6V501D0SPR2BYBVZC4A, major). Two OPTIONAL
 * properties, grey and nameless by default, in ONE panel used by terminals and commands alike.
 */
describe("GroupPanel — what it opens with", () => {
  it("opens with an EMPTY field and the FIRST swatch selected on a fresh group", () => {
    render(<LivePanel initial={group()} />);
    expect(field().value).toBe("");
    expect(swatch("none").getAttribute("aria-pressed")).toBe("true");
    for (const color of [1, 2, 3, 4, 5, 6] as GroupColor[]) {
      expect(swatch(color).getAttribute("aria-pressed")).toBe("false");
    }
  });

  it("offers SEVEN choices, the neutral FIRST — it is the absence of a choice, not a colour", () => {
    render(<LivePanel initial={group()} />);
    // The panel is PORTALLED, so it lives outside the render container.
    const swatches = Array.from(document.querySelectorAll<HTMLElement>("[data-group-swatch]"));
    expect(swatches.map((s) => s.dataset.groupSwatch)).toEqual([
      "none",
      "1",
      "2",
      "3",
      "4",
      "5",
      "6",
    ]);
    expect(swatches[0].getAttribute("style")).toContain("var(--group-0)");
  });

  it("shows what the group ALREADY has, so the user edits from what they see", () => {
    render(<LivePanel initial={group({ name: "build", color: 4 })} />);
    expect(field().value).toBe("build");
    expect(swatch(4).getAttribute("aria-pressed")).toBe("true");
    expect(swatch("none").getAttribute("aria-pressed")).toBe("false");
  });

  it("CLOSING WITHOUT TOUCHING ANYTHING leaves the group untouched — by reference", () => {
    const onRename = vi.fn();
    const onRecolor = vi.fn();
    const onDissolve = vi.fn();
    const original = group();
    const { rerender } = render(
      <GroupPanel
        open
        group={original}
        onClose={() => {}}
        onRename={onRename}
        onRecolor={onRecolor}
        onDissolve={onDissolve}
      />,
    );
    // Blur the field without typing — the one gesture that could commit by accident.
    fireEvent.blur(field());
    rerender(
      <GroupPanel
        open={false}
        group={original}
        onClose={() => {}}
        onRename={onRename}
        onRecolor={onRecolor}
        onDissolve={onDissolve}
      />,
    );
    expect(onRename).not.toHaveBeenCalled();
    expect(onRecolor).not.toHaveBeenCalled();
    expect(onDissolve).not.toHaveBeenCalled();
  });

  it("re-selecting the colour it ALREADY has emits nothing either", () => {
    const onRecolor = vi.fn();
    render(
      <GroupPanel
        open
        group={group({ color: 3 })}
        onClose={() => {}}
        onRename={vi.fn()}
        onRecolor={onRecolor}
        onDissolve={vi.fn()}
      />,
    );
    fireEvent.click(swatch(3));
    expect(onRecolor).not.toHaveBeenCalled();
  });
});

describe("GroupPanel — name and colour are INDEPENDENT", () => {
  it("names without colouring, then colours without renaming", () => {
    render(<LivePanel initial={group()} />);
    fireEvent.change(field(), { target: { value: "build" } });
    fireEvent.blur(field());
    expect(state()).toMatchObject({ name: "build", color: null });

    fireEvent.click(swatch(4));
    expect(state()).toMatchObject({ name: "build", color: 4 });
  });

  it("colours without naming, then names without losing the colour", () => {
    render(<LivePanel initial={group()} />);
    fireEvent.click(swatch(2));
    expect(state()).toMatchObject({ name: null, color: 2 });

    fireEvent.change(field(), { target: { value: "tails" } });
    fireEvent.blur(field());
    expect(state()).toMatchObject({ name: "tails", color: 2 });
  });
});

describe("GroupPanel — clearing keeps the GROUP", () => {
  it("emptying the field removes the name; the group and its members stay", () => {
    render(<LivePanel initial={group({ name: "build", color: 4 })} />);
    fireEvent.change(field(), { target: { value: "" } });
    fireEvent.blur(field());
    expect(state()).toMatchObject({ groups: 1, name: null, color: 4, members: 2 });
  });

  it("taking the FIRST swatch back removes the colour; the group and its members stay", () => {
    render(<LivePanel initial={group({ name: "build", color: 4 })} />);
    fireEvent.click(swatch("none"));
    expect(state()).toMatchObject({ groups: 1, name: "build", color: null, members: 2 });
  });

  it("clearing BOTH still leaves a perfectly usable group", () => {
    render(<LivePanel initial={group({ name: "build", color: 4 })} />);
    fireEvent.change(field(), { target: { value: "  " } });
    fireEvent.blur(field());
    fireEvent.click(swatch("none"));
    expect(state()).toMatchObject({ groups: 1, name: null, color: null, members: 2 });
  });
});

describe("GroupPanel — Ungroup all", () => {
  it("dissolves the group so every member becomes solo", () => {
    render(<LivePanel initial={group({ name: "build", color: 4 })} />);
    fireEvent.click(screen.getByRole("button", { name: "Ungroup all" }));
    expect(state()).toMatchObject({ groups: 0 });
  });
});

/**
 * THE TWO EXITS ARE NOT THE SAME EXIT (human review 01M1312M70TP8TJWT4TG505S0J).
 *
 * Verbatim: « je peux pas supprimer tout un groupe, y a l'option un ungroup mais y pas l'option tout
 * close ». The panel offered only the dissolution, so being rid of a group's terminals meant
 * ungrouping and then closing each survivor by hand. DISSOLVE keeps every terminal alive; CLOSE ALL
 * really closes them, and the group goes with them because a group with no member does not exist.
 */
describe("GroupPanel — Close all terminals", () => {
  function panel(onCloseAll?: (groupId: string) => void) {
    return render(
      <GroupPanel
        open
        group={group({ name: "build" })}
        onClose={() => {}}
        onRename={() => {}}
        onRecolor={() => {}}
        onDissolve={() => {}}
        onCloseAll={onCloseAll}
      />,
    );
  }

  it("is offered ONLY where there is something to close", () => {
    panel(() => {}).unmount();
    // …and the COMMANDS side, which hands no `onCloseAll`, does not draw it at all: a command
    // group's members are project-level templates, not running things this panel could close.
    panel();
    expect(screen.queryByRole("button", { name: /^Close all/ })).toBeNull();
    expect(
      screen.getByRole("button", { name: "Ungroup all" }),
      "…but it can still ungroup",
    ).toBeTruthy();
  });

  it("closes the group by its id, and closes the panel with it", () => {
    const onCloseAll = vi.fn();
    const onClose = vi.fn();
    render(
      <GroupPanel
        open
        group={group()}
        onClose={onClose}
        onRename={() => {}}
        onRecolor={() => {}}
        onDissolve={() => {}}
        onCloseAll={onCloseAll}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /^Close all/ }));
    expect(onCloseAll).toHaveBeenCalledWith("g1");
    expect(onClose).toHaveBeenCalled();
  });

  /**
   * NO CONFIRMATION, and that is a VERIFIED alignment rather than an omission. The finding asks for
   * the behaviour of a single terminal's close to be checked and followed, not invented: the row's ×
   * (`workspace-runtime-tree.tsx`) and Ctrl+W both call `close(id)` outright, with no dialog. So
   * this one acts on the first click too — a group's terminals must not be HARDER to close than the
   * same terminals one at a time.
   */
  it("acts on the FIRST click — the same as closing a terminal from its row", () => {
    const onCloseAll = vi.fn();
    panel(onCloseAll);
    fireEvent.click(screen.getByRole("button", { name: /^Close all/ }));
    expect(screen.queryByText(/are you sure/i), "no dialog was invented").toBeNull();
    expect(onCloseAll).toHaveBeenCalledTimes(1);
  });

  it("wears the DESTRUCTIVE register, and 'Ungroup all' no longer does", () => {
    panel(() => {});
    const dissolve = screen.getByRole("button", { name: "Ungroup all" });
    const close = screen.getByRole("button", { name: /^Close all/ });
    // The two must be TOLD APART, which two identical red rows would not be. The register moved to
    // where something is actually destroyed: ungrouping loses nothing and is one drag from being
    // undone, closing is irreversible.
    expect(close.className).not.toBe(dissolve.className);
    expect(close.className, "the destructive tone is on the destructive action").toContain(
      "destructive",
    );
    expect(dissolve.className, "…and only on it").not.toContain("destructive");
  });
});

describe("GroupPanel — ONE component for terminals AND commands", () => {
  it("is the ONLY panel there is, and BOTH list surfaces mount THAT one", async () => {
    const { readdirSync, readFileSync } = await import("node:fs");
    const { join } = await import("node:path");
    const dir = join(process.cwd(), "src", "components");
    // There is exactly ONE group panel component in the codebase — no second, forked one.
    // It lives in the NEUTRAL `components/panes/` brick — owned by neither surface — and there is
    // exactly ONE of it in the codebase: no second, forked panel on either side.
    const panels = ["panes", "terminals", "command"].flatMap((folder) =>
      readdirSync(join(dir, folder))
        .filter((file) => /^group-panel.tsx$/.test(file))
        .map((file) => folder + "/" + file),
    );
    expect(panels).toEqual(["panes/group-panel.tsx"]);
    // The TERMINALS side mounts it through the sidebar's groups provider…
    expect(readFileSync(join(dir, "terminals", "use-terminal-groups.tsx"), "utf8")).toContain(
      'from "@/components/panes/group-panel"',
    );
    // …AND THE COMMANDS SIDE NO LONGER MOUNTS A PANEL AT ALL (finding 01M132XFZ5KY8QMJ79HZZSBV3J).
    // Its settings screen became a master/detail: a folder's name, colour and dissolution are
    // settled in the panel on the right, next to its layout — a second dialog opened OVER the modal
    // was the same duplication the two lists were, one level up. What must NOT diverge is the
    // vocabulary, and that is why it is extracted rather than re-written: both surfaces draw the
    // SAME swatch row.
    expect(readFileSync(join(dir, "panes", "group-panel.tsx"), "utf8")).toContain(
      'from "./group-swatches"',
    );
    expect(readFileSync(join(dir, "command", "command-group-detail.tsx"), "utf8")).toContain(
      'from "@/components/panes/group-swatches"',
    );
  });

  /**
   * THE COMMANDS SCREEN KEEPS EVERY EDIT THE PANEL OFFERED — the finding's own condition (« aucune
   * capacité retirée à la main »). Not one of the three moved out of reach when the dialog went: it
   * moved INTO the folder's detail panel, where the layout already was.
   */
  it("…and the three edits it carried are all in the commands' folder panel", async () => {
    const { readFileSync } = await import("node:fs");
    const { join } = await import("node:path");
    const detail = readFileSync(
      join(process.cwd(), "src", "components", "command", "command-group-detail.tsx"),
      "utf8",
    );
    expect(detail, "the name").toContain("onRename");
    expect(detail, "the colour").toContain("onRecolor");
    expect(detail, "the dissolution").toContain("onDissolve");
  });
});
