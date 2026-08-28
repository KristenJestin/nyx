import { useState } from "react";
import { SquareTerminalIcon } from "lucide-react";
import { render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { page } from "vitest/browser";

// The REAL utilities and tokens: this spec measures PIXELS and RECTANGLES during a real drag, so
// the list must have the real geometry and the trait its real colour.
import "@/globals.css";

import { canSplitPane, type HostSize } from "@/components/panes/pane-geometry";
import {
  detachTerminal,
  findGroup,
  groupInsertAnchor,
  insertIntoGroup,
  orderGroupMembers,
  type PaneCollection,
} from "@/components/panes/pane-groups";
import {
  createPaneLayout,
  paneLeaves,
  splitPane,
  type PaneNode,
} from "@/components/panes/pane-tree";
import { RuntimeRow } from "./runtime-row";
import { RuntimeSelectionProvider } from "./runtime-selection";
import { SortableTerminalRows } from "./sortable-terminal-rows";
import { TerminalGroupsProvider } from "./use-terminal-groups";
import { TerminalReorderProvider } from "./use-terminal-reorder";

/**
 * WHERE THE ROW WILL FALL, MEASURED ON THE SCREEN (human review 01M0CY3F075ACP4TFBK1KJ49X8, major).
 *
 * Verbatim: « le glisser deplacer est tres chelou en fait genre la il m'affiche pas ou ca va
 * apparaitre alors que je suis clairement entre 2 terminaux », then « j'ai le meme pour dans un
 * groupe ».
 *
 * The specs that came before proved `data-drop-insertion` and `data-drop-armed` were SET, on the
 * right element — and the user still could not see where the row would land, because an attribute
 * in the DOM is not a trait on the screen. What this file measures instead is GEOMETRY, during a
 * real pointer drag in real Chromium: the rectangle of the trait against the rectangles of the two
 * rows it claims to separate, its painted colour against the surface behind it, and the number of
 * times the dragged row exists on screen while the gesture is running.
 *
 * A trait of zero height, off screen, behind another element or at the wrong ordinate fails here.
 */

let host: HTMLElement;
let mounted: { unmount: () => void } | null = null;

beforeEach(async () => {
  await page.viewport(1100, 900);
  host = document.createElement("div");
  host.style.width = "320px";
  host.className = "bg-background";
  document.body.appendChild(host);
});

afterEach(() => {
  // Unmount BEFORE detaching the host: React must be the one to tear its own tree down.
  mounted?.unmount();
  mounted = null;
  host.remove();
  // A spec that switched the theme must not hand it to the next one.
  document.documentElement.classList.remove("dark");
});

/** Grow a tree by splitting its LAST leaf, the way a group grows one member at a time. */
function grow(tree: PaneNode, terminalId: string): PaneNode {
  const leaves = paneLeaves(tree);
  return splitPane(tree, {
    paneId: leaves[leaves.length - 1].id,
    direction: leaves.length % 2 === 1 ? "row" : "column",
    terminalId,
    newPaneId: `${terminalId}-pane`,
  }) as PaneNode;
}

function groupOf(id: string, members: readonly string[]) {
  let tree: PaneNode = createPaneLayout(members[0], `${members[0]}-pane`);
  for (const member of members.slice(1)) tree = grow(tree, member);
  return { id, tree, name: null, color: null };
}

/** A roomy tiling host: the 180×120px floor refuses nothing here. */
const ROOMY: HostSize = { width: 1600, height: 1200 };

/** The list over a LIVE collection, wired exactly as `use-pane-layout` wires the real one. */
function renderList(initial: PaneCollection, ids: readonly string[]) {
  const live = { collection: initial, order: ids as readonly string[] };

  function Harness() {
    const [collection, setCollection] = useState(initial);
    const [order, setOrder] = useState<readonly string[]>(ids);
    live.collection = collection;
    live.order = order;
    return (
      <RuntimeSelectionProvider initialSelection={null}>
        <TerminalGroupsProvider
          groups={collection.groups}
          leaveGroup={(terminalId) => setCollection((c) => detachTerminal(c, terminalId))}
          reorderGroupMembers={(groupId, memberIds) =>
            setCollection((c) => orderGroupMembers(c, groupId, memberIds))
          }
          insertIntoGroup={(groupId, terminalId, index) =>
            setCollection((c) => insertIntoGroup(c, groupId, terminalId, index)?.collection ?? c)
          }
          canInsertIntoGroup={(groupId, index) => {
            const group = findGroup(live.collection, groupId);
            if (group == null) return false;
            const anchor = groupInsertAnchor(group, index);
            return canSplitPane(group.tree, anchor.paneId, anchor.direction, ROOMY);
          }}
          dissolveGroup={() => {}}
          renameGroup={() => {}}
          recolorGroup={() => {}}
        >
          <TerminalReorderProvider
            reorderTerminals={(_group, next) => setOrder(next)}
            orderedIds={order}
          >
            <SortableTerminalRows
              terminals={ids.map((id) => ({ id }))}
              group="ws1"
              empty="none"
              renderRow={(t) => (
                <RuntimeRow
                  selection={{ kind: "terminal", id: t.id }}
                  icon={<SquareTerminalIcon className="size-4" />}
                  label={<span data-row={t.id}>{t.id}</span>}
                  state="idle"
                />
              )}
            />
          </TerminalReorderProvider>
        </TerminalGroupsProvider>
      </RuntimeSelectionProvider>
    );
  }

  mounted = render(<Harness />, { container: host });

  const rowEl = (id: string) =>
    host.querySelector(`[data-row='${id}']`)?.closest("[data-terminal-row]") as HTMLElement;

  return {
    live,
    rowEl,
    rect: (id: string) => rowEl(id).getBoundingClientRect(),
    block: (groupId: string) =>
      host.querySelector(`[data-group-block='${groupId}']`) as HTMLElement | null,
    armed: (groupId: string) =>
      host.querySelector(`[data-group-block='${groupId}']`)?.hasAttribute("data-drop-armed") ===
      true,
    line: () => host.querySelector("[data-drop-insertion]") as HTMLElement | null,
    /**
     * WHAT THE TRAIT SAYS, in the list's own vocabulary: the item it is drawn against and on which
     * EDGE of it. The trait is an overlay inside the item's positioning wrapper, so the anchor is
     * that wrapper's own row/block — a `top` edge means "the row falls before it", `bottom` means
     * "after it".
     */
    lineAnchor: (): { id: string; edge: string } | null => {
      const line = host.querySelector("[data-drop-insertion]") as HTMLElement | null;
      if (line == null) return null;
      const wrapper = line.parentElement as HTMLElement;
      const block = wrapper.querySelector(":scope > [data-group-block]");
      const row = wrapper.querySelector(":scope > [data-terminal-row] [data-row]");
      const id = block?.getAttribute("data-group-block") ?? row?.getAttribute("data-row");
      return id == null ? null : { id, edge: line.dataset.dropInsertionEdge ?? "" };
    },
    /**
     * THE OTHER ANSWER (human review 01M1313GN2ESW0BCM8S1G0ZY2P) — the line the drop would make a
     * FOLDER with, ringed instead of trait-ed. A root line reads as three bands now: its two edges
     * insert (the trait says where), its middle merges (the ring says with whom). The two are
     * mutually exclusive on purpose, so "no trait" is no longer the same thing as "no answer".
     */
    mergeAnchor: (): string | null => {
      const marked = host.querySelector("[data-drop-merge], [data-drop-merge-refused]");
      return marked?.querySelector("[data-row]")?.getAttribute("data-row") ?? null;
    },
  };
}

/** Let the browser paint — dnd-kit schedules its moves and measurements on animation frames. */
function frames(count = 3): Promise<void> {
  return new Promise((resolve) => {
    let left = count;
    const tick = () => (left-- <= 0 ? resolve() : requestAnimationFrame(tick));
    requestAnimationFrame(tick);
  });
}

function pointer(type: string, x: number, y: number): PointerEvent {
  return new PointerEvent(type, {
    bubbles: true,
    cancelable: true,
    composed: true,
    pointerId: 1,
    pointerType: "mouse",
    isPrimary: true,
    button: 0,
    buttons: type === "pointerup" ? 0 : 1,
    clientX: x,
    clientY: y,
  });
}

/**
 * A REAL pointer drag that STAYS DOWN: press on `from`, walk in `direction` until `until` holds,
 * and return the live cursor plus a `release`. Everything this file asserts is asserted MID-GESTURE
 * — the trait only exists while the pointer is down — so the release is the caller's to make.
 */
async function dragUntil(
  from: Element,
  direction: -1 | 1,
  until: () => boolean,
  maxSteps = 40,
): Promise<{ reached: boolean; x: number; y: number; release: () => Promise<void> }> {
  const box = from.getBoundingClientRect();
  const x = box.left + box.width / 2;
  let y = box.top + box.height / 2;
  from.dispatchEvent(pointer("pointerdown", x, y));
  await frames(2);
  let reached = false;
  for (let i = 0; i < maxSteps && !reached; i += 1) {
    y += direction * 6;
    document.dispatchEvent(pointer("pointermove", x, y));
    await frames(2);
    // The 5px activation means the first step or two are not a drag yet.
    reached = i > 1 && until();
  }
  const cursor = { x, y };
  return {
    reached,
    ...cursor,
    release: async () => {
      document.dispatchEvent(pointer("pointerup", cursor.x, cursor.y));
      await frames(6);
    },
  };
}

/** A rectangle, as the failure messages quote it. */
function quote(name: string, r: DOMRect): string {
  return `${name}{top:${r.top.toFixed(1)} bottom:${r.bottom.toFixed(1)} h:${r.height.toFixed(1)} w:${r.width.toFixed(1)}}`;
}

/** One sRGB pixel. */
interface Pixel {
  r: number;
  g: number;
  b: number;
}

function luminance({ r, g, b }: Pixel): number {
  const lin = (c: number) => {
    const v = c / 255;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

function contrast(a: Pixel, b: Pixel): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * THE TRAIT'S OWN COLOUR, read off a capture — the strongest device row it painted, and why that
 * is the honest reading rather than the one at its geometric centre.
 *
 * The trait is a 2px line and its ordinate is FRACTIONAL (the list lays its rows out on a 2px gap,
 * over rows whose height is not an integer), so it lands across THREE device rows: two edges the
 * compositor covers partially and one it covers fully. A single sample at the centre rounds to
 * whichever of the three the arithmetic picks, and when that is a half-covered edge the sample is
 * the trait BLENDED WITH THE SURFACE — a reading about sub-pixel placement, not about what was
 * painted (it is how a magenta at 5.9:1 on the dark theme measured 2.7:1). Every FULLY covered row
 * is sampled instead and the strongest is the trait's colour; the whole scan is quoted on failure.
 */
function inkOf(
  at: (x: number, y: number) => Pixel,
  box: DOMRect,
  paper: Pixel,
): { y: number; pixel: Pixel; ratio: number; scan: string } {
  const x = box.left + box.width / 2;
  const rows: { y: number; pixel: Pixel; ratio: number }[] = [];
  for (let y = Math.floor(box.top); y <= Math.ceil(box.bottom); y += 1) {
    // A device row `y` covers [y, y+1): keep only the ones the trait covers WHOLE.
    if (y < box.top || y + 1 > box.bottom) continue;
    const pixel = at(x, y + 0.5);
    rows.push({ y, pixel, ratio: contrast(pixel, paper) });
  }
  // A trait too thin to cover one whole row would leave nothing to sample — read its centre and
  // let the ratio speak, rather than silently passing on an empty scan.
  if (rows.length === 0) {
    const pixel = at(x, box.top + box.height / 2);
    return { y: box.top, pixel, ratio: contrast(pixel, paper), scan: "no fully covered row" };
  }
  const best = rows.reduce((a, b) => (b.ratio > a.ratio ? b : a));
  return {
    ...best,
    scan: rows.map((r) => `y${r.y}=${r.ratio.toFixed(2)}:1`).join(" "),
  };
}

/** Grab the page as PIXELS — the capture this spec reads the trait's paint from. */
async function capture(): Promise<(x: number, y: number) => Pixel> {
  const shot = (await page.screenshot({ base64: true, save: false })) as unknown;
  const bytes =
    typeof shot === "string"
      ? Uint8Array.from(atob(shot), (c) => c.charCodeAt(0))
      : typeof (shot as { base64?: unknown }).base64 === "string"
        ? Uint8Array.from(atob((shot as { base64: string }).base64), (c) => c.charCodeAt(0))
        : Uint8Array.from(shot as ArrayLike<number>);
  const url = URL.createObjectURL(new Blob([bytes], { type: "image/png" }));
  const image = new Image();
  await new Promise<void>((resolve, reject) => {
    image.onload = () => resolve();
    image.onerror = () => reject(new Error("the capture did not decode"));
    image.src = url;
  });
  const canvas = document.createElement("canvas");
  canvas.width = image.width;
  canvas.height = image.height;
  const ctx = canvas.getContext("2d") as CanvasRenderingContext2D;
  ctx.drawImage(image, 0, 0);
  URL.revokeObjectURL(url);
  const scale = image.width / window.innerWidth;
  return (x, y) => {
    const data = ctx.getImageData(Math.round(x * scale), Math.round(y * scale), 1, 1).data;
    return { r: data[0], g: data[1], b: data[2] };
  };
}

/** One group of two, four bare rows: both a bare gap and an in-block gap to aim at. */
const oneGroupFourSolos = (): PaneCollection => ({ groups: [groupOf("g1", ["A", "B"])] });
const IDS = ["A", "B", "C", "D", "E", "F"];

describe("The insertion trait is WHERE THE ROW WILL FALL (criteria 1, 2)", () => {
  it("in the BARE list: the trait's rectangle lies between the two rows it separates", async () => {
    const list = renderList(oneGroupFourSolos(), IDS);

    // F walked up until the trait says "D takes the row before it".
    const drag = await dragUntil(list.rowEl("F"), -1, () => {
      const anchor = list.lineAnchor();
      return anchor?.id === "D" && anchor.edge === "top";
    });
    expect(drag.reached, "the trait reached the gap above D").toBe(true);

    const line = list.line() as HTMLElement;
    const lineBox = line.getBoundingClientRect();
    const above = list.rect("C");
    const below = list.rect("D");
    const message = `${quote("trait", lineBox)} vs ${quote("C", above)} and ${quote("D", below)}`;

    // A trait with no surface is not a trait.
    expect(lineBox.height, message).toBeGreaterThan(0);
    expect(lineBox.width, message).toBeGreaterThan(0);
    // …and it is ON SCREEN, inside the list's own column.
    expect(lineBox.top, message).toBeGreaterThan(0);
    expect(lineBox.left, message).toBeGreaterThanOrEqual(host.getBoundingClientRect().left);

    // THE ORDINATE: its middle sits in the gap between the row above and the row below — not on
    // one of them, not one gap further down.
    const middle = lineBox.top + lineBox.height / 2;
    expect(middle, message).toBeGreaterThanOrEqual(above.bottom);
    expect(middle, message).toBeLessThanOrEqual(below.top);

    await drag.release();
  });

  it("INSIDE a block, between two members: same reading, in the block's own column", async () => {
    const list = renderList(oneGroupFourSolos(), IDS);

    // C walked up until the trait says "B takes the row before it" — i.e. between A and B, inside
    // the block, which must also be ARMED (the drop lands in the group).
    const drag = await dragUntil(list.rowEl("C"), -1, () => {
      const anchor = list.lineAnchor();
      return anchor?.id === "B" && anchor.edge === "top" && list.armed("g1");
    });
    expect(drag.reached, "the trait reached the gap between the two members").toBe(true);

    const lineBox = (list.line() as HTMLElement).getBoundingClientRect();
    const above = list.rect("A");
    const below = list.rect("B");
    const message = `${quote("trait", lineBox)} vs ${quote("A", above)} and ${quote("B", below)}`;

    expect(lineBox.height, message).toBeGreaterThan(0);
    expect(lineBox.width, message).toBeGreaterThan(0);
    const middle = lineBox.top + lineBox.height / 2;
    expect(middle, message).toBeGreaterThanOrEqual(above.bottom);
    expect(middle, message).toBeLessThanOrEqual(below.top);
    // It is drawn INSIDE the block, not across the list.
    expect(
      (list.line() as HTMLElement).closest("[data-group-block]"),
      "the trait belongs to the block it points into",
    ).toBeTruthy();

    await drag.release();
  });
});

describe("What the eye actually gets (criteria 3, 4, 5)", () => {
  it("the dragged row exists exactly ONCE on screen for the whole gesture", async () => {
    const list = renderList(oneGroupFourSolos(), IDS);
    const seen: number[] = [];

    /** The copies of a row THE EYE GETS — a node that paints nothing is not one of them. */
    const visible = (id: string) =>
      Array.from(host.querySelectorAll<HTMLElement>(`[data-row='${id}']`))
        .map((label) => label.closest("[data-terminal-row]") as HTMLElement)
        .filter((row) => getComputedStyle(row).visibility !== "hidden");

    const drag = await dragUntil(list.rowEl("F"), -1, () => {
      seen.push(visible("F").length);
      // Walk the whole list, sampling at every step: one stray frame with a second copy is the
      // artefact the user photographed, and it must not exist at ANY point of the gesture.
      return list.lineAnchor()?.id === "g1";
    });
    expect(drag.reached, "the row travelled the whole list").toBe(true);

    expect(
      seen.every((count) => count === 1),
      `the dragged row was VISIBLE ${JSON.stringify(seen)} time(s) across the gesture`,
    ).toBe(true);

    // THE PLACEHOLDER IS A SLOT, NOT A COPY. dnd-kit's default feedback lifts the source into the
    // top layer and leaves a clone holding its place in the flow (`[data-dnd-placeholder]`) — which
    // is precisely what stops the six rows below from jumping up by a row height the instant the
    // gesture starts, the drift the walk specs at the bottom of this file forbid to a tenth of a
    // pixel. (The `move` feedback, which leaves nothing behind, would trade this criterion for that
    // one.) So the clone is allowed to EXIST and forbidden to be SEEN: there is exactly one, it is
    // `visibility: hidden`, and it is the only extra `[data-terminal-row]` in the list.
    const placeholders = host.querySelectorAll<HTMLElement>("[data-dnd-placeholder]");
    expect(placeholders.length, "exactly one placeholder: the source's own slot").toBe(1);
    expect(
      getComputedStyle(placeholders[0]).visibility,
      "the placeholder holds a slot and paints nothing",
    ).toBe("hidden");
    expect(
      host.querySelectorAll("[data-terminal-row]:not([data-dnd-placeholder])").length,
      "one wrapper per row, no more",
    ).toBe(IDS.length);

    // …and that ONE representation is OPAQUE. The lifted row flies over the rows it passes, so a
    // see-through one draws itself and the row underneath in the same place — which is what read as
    // a second, offset copy. Its own surface is what makes it a single object to the eye.
    const lifted = list.rowEl("F");
    const style = getComputedStyle(lifted);
    expect(lifted.dataset.dragging, "the lifted row is the drag source").toBe("true");
    expect(Number(style.opacity), `lifted row opacity ${style.opacity}`).toBeGreaterThanOrEqual(1);
    expect(style.backgroundColor, "the lifted row paints its own surface").not.toMatch(
      /rgba\([^)]*,\s*0\)$/,
    );
    expect(style.backgroundColor).not.toBe("transparent");

    await drag.release();
  });

  it("the trait is PAINTED: opaque colour, reachable at its centre, contrasted on the capture", async () => {
    const list = renderList(oneGroupFourSolos(), IDS);
    const drag = await dragUntil(list.rowEl("F"), -1, () => {
      const anchor = list.lineAnchor();
      return anchor?.id === "D" && anchor.edge === "top";
    });
    expect(drag.reached).toBe(true);

    const line = list.line() as HTMLElement;
    const style = getComputedStyle(line);
    // NOT TRANSPARENT: neither the colour nor the element is see-through.
    expect(style.backgroundColor, "the trait's computed background").not.toBe("transparent");
    expect(style.backgroundColor).not.toMatch(/rgba\([^)]*,\s*0\)$/);
    expect(Number(style.opacity), "the trait is not faded out").toBeGreaterThan(0.9);
    expect(style.visibility).toBe("visible");

    const box = line.getBoundingClientRect();
    // "NOTHING COVERS IT" IS NOT A HIT-TEST, and it cannot be one. The trait is
    // `pointer-events-none` BY DESIGN — an indicator the pointer can hit becomes a collision target
    // of the very gesture it describes — and `elementFromPoint` skips such elements, so it can
    // never return the trait however plainly the trait is drawn. That design property is asserted
    // here for what it is; whether anything covers the trait is read below, ON THE PIXELS, which is
    // the only place the question has an answer.
    expect(style.pointerEvents, "the trait never intercepts the gesture it describes").toBe("none");

    // AND ON THE PIXELS: the trait's own row of pixels stands out from the surface beside it.
    const at = await capture();
    const paper = at(box.left + box.width / 2, box.top - 8);
    const ink = inkOf(at, box, paper);
    expect(
      ink.ratio,
      `trait ${JSON.stringify(ink.pixel)} vs surface ${JSON.stringify(paper)} = ` +
        `${ink.ratio.toFixed(2)}:1 (scan: ${ink.scan})`,
    ).toBeGreaterThan(3);

    await drag.release();
  });

  /**
   * …AND IN THE DARK THEME (review 01M0D3203MY8RFXN9RHFSMEWMZ, finding 01M0D4HBQSS3W0MG6T70PG6GYV).
   * The assertion above runs on the DEFAULT theme, which is the light one — the very theme where
   * `primary` is at its most contrasted (near-black on white). The theme the trait could actually
   * fail on is the dark one, where `primary` becomes the magenta accent on a near-black surface, so
   * it is measured here too, on a real capture, at the same 3:1 floor. The observed ratio is quoted
   * in the message either way.
   */
  it("the trait is PAINTED in the DARK theme too, where `primary` is the magenta accent", async () => {
    document.documentElement.classList.add("dark");
    const list = renderList(oneGroupFourSolos(), IDS);
    const drag = await dragUntil(list.rowEl("F"), -1, () => {
      const anchor = list.lineAnchor();
      return anchor?.id === "D" && anchor.edge === "top";
    });
    expect(drag.reached, "the trait reached the gap above D on the dark theme").toBe(true);

    const box = (list.line() as HTMLElement).getBoundingClientRect();
    const at = await capture();
    const paper = at(box.left + box.width / 2, box.top - 8);
    const ink = inkOf(at, box, paper);
    expect(
      ink.ratio,
      `dark theme: trait ${JSON.stringify(ink.pixel)} vs surface ${JSON.stringify(paper)} = ` +
        `${ink.ratio.toFixed(2)}:1 (scan: ${ink.scan})`,
    ).toBeGreaterThan(3);

    await drag.release();
  });

  it("the block ARMS for a drop inside it and stays dark for one outside — same gesture", async () => {
    const list = renderList(oneGroupFourSolos(), IDS);

    // Position 1: inside the block.
    const drag = await dragUntil(list.rowEl("C"), -1, () => list.armed("g1"));
    expect(drag.reached, "the block armed while the drop lands inside it").toBe(true);
    expect(
      (list.line() as HTMLElement).closest("[data-group-block]"),
      "and the trait is inside the block",
    ).toBeTruthy();

    // Position 2, SAME gesture: walk back down until the drop lands outside every block.
    let y = drag.y;
    let outside = false;
    for (let i = 0; i < 40 && !outside; i += 1) {
      y += 6;
      document.dispatchEvent(pointer("pointermove", drag.x, y));
      await frames(2);
      const line = list.line();
      outside = line != null && line.closest("[data-group-block]") == null && !list.armed("g1");
    }
    expect(outside, "the block went dark and the trait left it").toBe(true);
    expect(list.block("g1")?.hasAttribute("data-drop-armed")).toBe(false);

    document.dispatchEvent(pointer("pointerup", drag.x, y));
    await frames(6);
  });
});

/**
 * THE TRAIT MUST BE RIGHT AT EVERY STEP, NOT AT ONE OF THEM (review 01M0D3203MY8RFXN9RHFSMEWMZ,
 * finding 01M0D4HBQF74WYQA8C07W82P46).
 *
 * The specs above stop the drag at the FIRST step where the trait says what they wanted to read —
 * `dragUntil` walks up to 40 steps and returns as soon as its predicate holds. An indicator that is
 * right 2 times out of 22 satisfies such a predicate exactly as surely as one that is right 22 times
 * out of 22: what is measured is an INSTANT, chosen for being good, never the STABILITY of the
 * gesture. That is how the CAUSE this file documents could be undone without a single test moving.
 *
 * THE CAUSE, and what guards it here. The trait used to be an ordinary element of the FLOW: showing
 * it displaced every row under it by 4px (its 2px plus the list's 2px gap), dnd-kit's collision was
 * then computed against rectangles that had moved BECAUSE of the answer it had just given, the trait
 * was removed, the rows came back, and the next frame restarted the cycle — right twice over 22
 * pointer steps. So the walks below record, at EVERY step, two things at once:
 *
 *  * WHAT THE TRAIT SAYS — the item it is drawn against and on which edge, plus its ordinate. The
 *    sequence must PROGRESS the way the pointer does and never come back to the dragged row's own
 *    position (the `F → self → E → self → D` oscillation a self-targeting source produces), and the
 *    steps with no trait at all are bounded by a quoted constant.
 *  * WHAT THE LIST MEASURES — the rectangle of every other row. Those must not move by one pixel
 *    for the whole gesture: an indicator that displaces the geometry its own detection is computed
 *    from is the defect itself, whatever a lucky instant may show.
 *
 * The recorded sequence is quoted in every failure message, so a regression shows its own trail.
 */

/** One step of a recorded walk: what the trait said, where it was drawn, what the list measured. */
interface WalkStep {
  anchor: { id: string; edge: string } | null;
  /** The ordinate of the trait's top edge, i.e. WHERE ON SCREEN it was drawn. */
  y: number | null;
  /**
   * The line the drop would make a FOLDER with — the OTHER answer, and the reason a step with no
   * trait is not automatically a blind one (human review 01M1313GN2ESW0BCM8S1G0ZY2P).
   */
  merge: string | null;
  /** `id → top` for every row the pointer is NOT carrying — the geometry dnd-kit collides against. */
  rows: ReadonlyMap<string, number>;
}

/** 15 consecutive steps is the floor the finding asks for; these walks take 18. */
const WALK_STEPS = 18;

/**
 * How many steps of a walk may show NO trait at all. Two are structural — the 5px activation
 * threshold means the first move is not a drag yet, and dnd-kit measures on the frame after — and we
 * allow one more for a slow frame. Beyond that the indicator is BLINKING, which is the flow-element
 * failure mode: it appears, it moves the rows, the collision misses, it is removed.
 */
const BLIND_STEPS_MAX = 3;

/**
 * Press on `sourceId`'s row and walk `WALK_STEPS` CONSECUTIVE 6px moves in `direction`, recording
 * the trait AND the list's geometry at every single one. Unlike `dragUntil` this never stops early:
 * the whole point is the sequence, not one of its members.
 *
 * 18 × 6px = 108px, which walks a six-row list from end to end without running off it — past the
 * end there is no target left, the projection is "nothing moved" and the trait legitimately returns
 * to the source, which is not the defect this reads for.
 */
async function walkAndRecord(
  list: ReturnType<typeof renderList>,
  ids: readonly string[],
  sourceId: string,
  direction: -1 | 1,
): Promise<{ walk: WalkStep[]; release: () => Promise<void> }> {
  const others = ids.filter((id) => id !== sourceId);
  const geometry = () => new Map(others.map((id) => [id, list.rect(id).top]));

  const from = list.rowEl(sourceId);
  const box = from.getBoundingClientRect();
  const x = box.left + box.width / 2;
  let y = box.top + box.height / 2;
  from.dispatchEvent(pointer("pointerdown", x, y));
  await frames(2);

  const walk: WalkStep[] = [];
  for (let i = 0; i < WALK_STEPS; i += 1) {
    y += direction * 6;
    document.dispatchEvent(pointer("pointermove", x, y));
    await frames(2);
    const line = list.line();
    walk.push({
      anchor: list.lineAnchor(),
      y: line == null ? null : line.getBoundingClientRect().top,
      merge: list.mergeAnchor(),
      rows: geometry(),
    });
  }
  const at = { x, y };
  return {
    walk,
    release: async () => {
      document.dispatchEvent(pointer("pointerup", at.x, at.y));
      await frames(6);
    },
  };
}

/** The recorded sequence, as every failure message quotes it. */
function trail(walk: readonly WalkStep[]): string {
  return walk
    .map((step, i) => {
      if (step.anchor != null) {
        return `${i}:${step.anchor.id}/${step.anchor.edge}@${(step.y ?? 0).toFixed(0)}`;
      }
      // A merge step is NOT a hole in the trail — it is the other answer, quoted as such.
      return step.merge == null ? `${i}:—` : `${i}:⊕${step.merge}`;
    })
    .join(" ");
}

/**
 * The whole reading of one walk, in one place so the bare list and the in-block drag are held to
 * exactly the same standard.
 */
function assertWalkProgresses(walk: readonly WalkStep[], sourceId: string, direction: -1 | 1) {
  const quoted = trail(walk);
  const seen = walk.filter((step) => step.anchor != null);

  // THE LIST DOES NOT MOVE UNDER THE TRAIT. Showing the indicator may not displace one single row:
  // dnd-kit resolves the collision against these very rectangles, so a trait that moves them is
  // answering a question it has just changed. Nothing else re-orders the rows during a drag (the
  // dragged one is excluded — the pointer carries it), so ANY drift here is the trait's doing.
  const before = walk[0].rows;
  for (const [i, step] of walk.entries()) {
    for (const [id, top] of step.rows) {
      const start = before.get(id) as number;
      expect(
        top,
        `showing the trait moved row ${id} by ${(top - start).toFixed(1)}px at step ${i} ` +
          `(${start.toFixed(1)} → ${top.toFixed(1)}) — the collision is computed against these ` +
          `rectangles, so the indicator is changing its own answer — ${quoted}`,
      ).toBeCloseTo(start, 1);
    }
  }

  // THE STEPS THAT SAY NOTHING AT ALL are bounded, and the bound is in the message.
  //
  // "Nothing at all" is what changed with gesture 5 (human review 01M1313GN2ESW0BCM8S1G0ZY2P): a
  // root line's MIDDLE band draws no trait ON PURPOSE, because the answer there is not "between two
  // lines" but "into a folder with this one", and it is said by a ring instead. Counting those as
  // blind would be reading the absence of one indicator as the absence of any — and would forbid the
  // very gesture. So a step is blind only when it shows NEITHER, which is still exactly the
  // flow-element failure mode this file was written for: the indicator appears, moves the rows, the
  // collision misses, it is removed.
  const blind = walk.filter((step) => step.anchor == null && step.merge == null).length;
  expect(
    blind,
    `${blind} of ${walk.length} steps announced NOTHING — no trait and no folder ring ` +
      `(bound: ${BLIND_STEPS_MAX}) — ${quoted}`,
  ).toBeLessThanOrEqual(BLIND_STEPS_MAX);

  // The trait leaves the dragged row's own position and NEVER comes back to it. This is the
  // `F → self → E → self → D` oscillation, which is what a source that is its own drop target
  // produces: every other frame projects to "nothing moved".
  const firstElsewhere = seen.findIndex((step) => step.anchor?.id !== sourceId);
  expect(
    firstElsewhere,
    `the trait never left ${sourceId}'s own position — ${quoted}`,
  ).toBeGreaterThanOrEqual(0);
  const returned = seen.slice(firstElsewhere).filter((step) => step.anchor?.id === sourceId);
  expect(
    returned.length,
    `the trait went back to ${sourceId}'s own position ${returned.length} time(s) after having ` +
      `left it — ${quoted}`,
  ).toBe(0);

  // It PROGRESSES: once the walk is under way the trait only ever travels the way the pointer does.
  // Nothing re-orders under it (asserted above), so a step backwards is the indicator fighting the
  // geometry, not the list moving.
  const ordinates = seen.slice(firstElsewhere).map((step) => step.y as number);
  for (let i = 1; i < ordinates.length; i += 1) {
    const [previous, current] = [ordinates[i - 1], ordinates[i]];
    const forward = direction === -1 ? current <= previous : current >= previous;
    expect(
      forward,
      `step ${i} sent the trait ${direction === -1 ? "back DOWN" : "back UP"} the list ` +
        `(${previous.toFixed(0)} → ${current.toFixed(0)}) — ${quoted}`,
    ).toBe(true);
  }

  // …and it really travels: a trait that never moves also never goes backwards.
  const stops = new Set(seen.map((step) => `${step.anchor?.id}/${step.anchor?.edge}`));
  expect(
    stops.size,
    `the trait only ever named ${stops.size} position(s) over ${walk.length} steps — ${quoted}`,
  ).toBeGreaterThanOrEqual(3);
}

describe("The trait is right at EVERY step of the gesture (review 01M0D3203MY8RFXN9RHFSMEWMZ)", () => {
  it(`in the BARE list: ${WALK_STEPS} consecutive steps, one direction, the trail read whole`, async () => {
    const list = renderList(oneGroupFourSolos(), IDS);
    const { walk, release } = await walkAndRecord(list, IDS, "F", -1);
    assertWalkProgresses(walk, "F", -1);
    await release();
  });

  it(`INSIDE a block: the same ${WALK_STEPS}-step reading among a group's members`, async () => {
    const list = renderList({ groups: [groupOf("g1", ["A", "B", "C", "D"])] }, IDS);
    const { walk, release } = await walkAndRecord(list, IDS, "D", -1);
    assertWalkProgresses(walk, "D", -1);
    // The walk really happened INSIDE the block: at least one step named a member of it.
    const members = walk.filter((step) => ["A", "B", "C"].includes(step.anchor?.id ?? ""));
    expect(
      members.length,
      `no step of the walk named a member of the block — ${trail(walk)}`,
    ).toBeGreaterThan(0);
    await release();
  });
});
