import { render } from "@testing-library/react";
import { page } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

// The REAL Tailwind utilities and the REAL tokens — load-bearing here: this spec measures the
// PIXELS the block is painted with, so the stylesheet has to be the app's.
import "@/globals.css";

import { capture, contrast } from "@/lib/test-pixels";
import { GroupBlock } from "./group-block";

/**
 * WHAT A BLOCK COSTS AND WHAT IT DRAWS, MEASURED IN REAL CHROMIUM.
 *
 * The user's rule has been a tightrope since review 01M0ABR6V501D0SPR2BYBVZC4A: the block must
 * DELIMIT — you have to see which rows share a group — without HIERARCHISING — a solo terminal must
 * not look secondary next to a group. Both halves are claims about what reaches the eye, so both
 * are measured on the real pixels of a real screenshot rather than asserted on class strings.
 *
 * ## What changed, and why the numbers left this file (human review 01M12MW1DV1V455EWBMV048TE6)
 *
 * The block used to answer the rule with a BORDER plus a FILL, and this spec froze both. Neither
 * exists any more: the delimitation is now the FOLDER, the INDENT and the GUIDE LINE, and the fill
 * appears at hover only (variant B+). So the border/fill readings became readings of their ABSENCE.
 *
 * And the head's height left too. It was frozen at 20 px, then 22, then 32 — a number re-derived by
 * hand at every round, which is precisely what the review put a stop to: « c'est pas forcément 32 ».
 * The head is a LINE now, and "as tall as a line" is measured against a real line in
 * `terminals/sidebar-line.browser.test.tsx`. What is left HERE is what only the block can say: what
 * it costs OVER its rows, and that the cost is exactly its own head — no padding, no border, no
 * margin, nothing else.
 */

let host: HTMLElement;

beforeEach(async () => {
  await page.viewport(900, 600);
  host = document.createElement("div");
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  host.style.width = "260px";
  host.style.padding = "12px";
  // The Terminals sidebar's own surface (`<SidebarShell>`: `bg-muted/40` over `bg-background`).
  host.className = "bg-background";
  document.body.appendChild(host);
});

afterEach(() => {
  host.remove();
});

/** A row, exactly as the sidebar paints one. */
function Row({ name }: { name: string }) {
  return (
    <div data-row={name} className="flex items-center gap-2 px-2 py-1.5 text-sm text-foreground">
      <span className="size-1.5 rounded-full bg-muted-foreground" />
      <span>{name}</span>
    </div>
  );
}

/**
 * AT REST THE BLOCK DRAWS ONE MARK, AND IT IS THE GUIDE (variant B+).
 *
 * The border and the resting fill are gone: they were a second and a third signal for a contour the
 * indent already draws, and a frame plus a fill hierarchises — which is the one thing the rule
 * forbids. What is left at rest is a 1 px guide line at 42 % of the tone, and this checks BOTH
 * halves on the pixels: nothing where the box used to be, something where the tree mark is.
 */
it("draws NO box at rest — and the guide line is the one mark it does draw", async () => {
  render(
    <div className="flex flex-col gap-0.5">
      <Row name="solo term" />
      <GroupBlock groupId="g1" color={null} name={null} open onToggle={() => {}}>
        <Row name="multi term A" />
        <Row name="multi term B" />
      </GroupBlock>
    </div>,
    { container: host },
  );

  const block = host.querySelector("[data-group-block]") as HTMLElement;
  const box = block.getBoundingClientRect();
  const at = await capture();

  // The surface OUTSIDE the block, its top EDGE (where the border used to be) and its INTERIOR just
  // below (where the fill used to be). Sampled mid-edge so no corner rounding is in the way.
  const surface = at(box.left + box.width / 2, box.top - 6);
  const edge = at(box.left + box.width / 2, box.top);
  const interior = at(box.left + box.width / 2, box.top + 4);

  expect(edge, "no border: the block's edge IS the surface").toEqual(surface);
  expect(interior, "no fill: its interior IS the surface").toEqual(surface);

  // …and the ONE mark it does draw. Read across the guide's own column, on a member's line.
  const guide = (block.querySelector("[data-group-guide]") as HTMLElement).getBoundingClientRect();
  const member = (
    host.querySelector("[data-row='multi term B']") as HTMLElement
  ).getBoundingClientRect();
  const y = member.top + member.height / 2;
  let guideInk = 1;
  for (let x = guide.left - 1; x <= guide.right + 1; x += 0.5) {
    guideInk = Math.max(guideInk, contrast(at(x, y), surface));
  }
  expect(guideInk, `guide ink ${guideInk.toFixed(2)}:1 against the surface`).toBeGreaterThan(1.05);
});

/**
 * WHAT A BLOCK COSTS OVER ITS ROWS — and it is exactly ONE LINE.
 *
 * The number is DERIVED, never written: the overhead is measured against the block's own head, so
 * this stays true whatever a line ends up measuring (human review 01M12MW1DV1V455EWBMV048TE6 —
 * « c'est pas forcément 32 »). What it really asserts is that the head is the ONLY thing a block
 * adds: no padding, no border, no outer margin, no gap of its own. That is what makes a folded
 * group of four commands cost one line where the open one costs five, and it is what a stray
 * `my-1` or `py-0.5` would silently take back.
 */
it("costs its rows plus exactly ONE head — nothing else, on BOTH surfaces", () => {
  render(
    <div className="flex flex-col gap-0.5">
      <Row name="solo term" />
      {/* TERMINALS: the block drags, so its folder is also a handle, and it keeps its `…`. */}
      <GroupBlock
        groupId="g1"
        color={null}
        name={null}
        open
        onToggle={() => {}}
        handleRef={() => {}}
        onOpenPanel={() => {}}
      >
        <Row name="multi term A" />
        <Row name="multi term B" />
      </GroupBlock>
      {/* COMMANDS: no drag and no `…` — the same head, two controls lighter. */}
      <GroupBlock groupId="cg1" color={null} name="build" open onToggle={() => {}}>
        <Row name="cmd A" />
        <Row name="cmd B" />
      </GroupBlock>
    </div>,
    { container: host },
  );

  for (const id of ["g1", "cg1"]) {
    const block = host.querySelector(`[data-group-block='${id}']`) as HTMLElement;
    const head = host.querySelector(`[data-group-head='${id}']`) as HTMLElement;
    const body = host.querySelector(`[data-group-body='${id}']`) as HTMLElement;
    const style = getComputedStyle(block);
    const margins = Number.parseFloat(style.marginTop) + Number.parseFloat(style.marginBottom);
    const overhead =
      block.getBoundingClientRect().height + margins - body.getBoundingClientRect().height;
    expect(
      Math.round(overhead),
      `${id}: a block costs ${overhead}px over its body, its head is ${head.getBoundingClientRect().height}px`,
    ).toBe(Math.round(head.getBoundingClientRect().height));
  }

  // AND THE CONTROLS DID NOT PAY FOR IT (review 01M0BZ6Y5H6YJB4327B01EWCTT). The head's two
  // controls are the design system's smallest icon button and must stay the SAME target — the band
  // can never be won back at the hand's expense on one of them. Compared to EACH OTHER, so a
  // change to that button's scale keeps this green while a change to one control alone does not.
  const folder = host.querySelector("[data-group-head='g1'] [data-group-folder]") as HTMLElement;
  const trigger = host.querySelector(
    "[data-group-head='g1'] button[aria-label^='Group options']",
  ) as HTMLElement;
  const boxOf = (el: HTMLElement) => {
    const b = el.getBoundingClientRect();
    return [Math.round(b.width), Math.round(b.height)];
  };
  expect(boxOf(folder), "the folder's target vs the `…`'s").toEqual(boxOf(trigger));
  // …and neither is smaller than the glyph it wraps: a target never shrinks below its own mark.
  const glyph = (folder.querySelector("span.relative") as HTMLElement).getBoundingClientRect();
  expect(boxOf(folder)[0]).toBeGreaterThanOrEqual(Math.round(glyph.width));
});

/**
 * NO NAME, NO WORDS — MEASURED, AND ON THE PIXELS (human review 01M0CY3F075ACP4TFBK1KJ49X8).
 *
 * Verbatim: « le message unnamed group non, si pas de nom, on affiche rien ». The DOM half is easy
 * to assert and easy to fool (an "empty" label can still paint a dash, a space, a dimmed word), so
 * the label zone of an anonymous head is also READ OFF A CAPTURE: between its folder and its `…`
 * there must be no ink at all, while the same band of a NAMED head carries its name.
 *
 * And the head must not MOVE for it — same band, same controls at the same offsets. That half is an
 * equality between the two heads and carries no number, which is how it caught the real regression
 * this round: an empty label has no line box, so the nameless head was shorter than the named one
 * (28 px against 32) until the spacer was given the line's own height.
 */
it("an unnamed head writes NOTHING, and keeps the named head's band and controls — measured", async () => {
  render(
    <div className="flex flex-col gap-0.5">
      <GroupBlock
        groupId="named"
        color={null}
        name="build"
        open
        onToggle={() => {}}
        handleRef={() => {}}
        onOpenPanel={() => {}}
      >
        <Row name="A" />
        <Row name="B" />
      </GroupBlock>
      <GroupBlock
        groupId="anon"
        color={null}
        name={null}
        open
        onToggle={() => {}}
        handleRef={() => {}}
        onOpenPanel={() => {}}
      >
        <Row name="C" />
        <Row name="D" />
      </GroupBlock>
    </div>,
    { container: host },
  );

  const headOf = (id: string) => host.querySelector(`[data-group-head='${id}']`) as HTMLElement;
  const named = headOf("named");
  const anon = headOf("anon");

  // 1. THE STRING. The anonymous head carries no text node at all; the named one shows its name.
  expect(anon.dataset.groupHeadLabel).toBe("unnamed");
  expect(anon.textContent, "no stand-in, no substitute, no dash").toBe("");
  expect(named.dataset.groupHeadLabel).toBe("named");
  expect(named.textContent).toContain("build");

  // 2. THE BAND, compared to itself rather than to a number: losing the words neither buys height
  //    back nor costs it.
  expect(
    anon.getBoundingClientRect().height,
    `unnamed ${anon.getBoundingClientRect().height}px vs named ${named.getBoundingClientRect().height}px`,
  ).toBe(named.getBoundingClientRect().height);

  // 3. THE CONTROLS. Same size, same offsets from their own head's edges: the folder stays pinned
  //    at the start and the `…` at the end, so nothing shifts when the label disappears.
  const controls = (head: HTMLElement) => {
    const box = head.getBoundingClientRect();
    const folder = (
      head.querySelector("[data-group-folder]") as HTMLElement
    ).getBoundingClientRect();
    const trigger = (
      head.querySelector("button[aria-label^='Group options']") as HTMLElement
    ).getBoundingClientRect();
    return {
      folderFromLeft: Math.round(folder.left - box.left),
      folderSize: [Math.round(folder.width), Math.round(folder.height)],
      triggerFromRight: Math.round(box.right - trigger.right),
      triggerSize: [Math.round(trigger.width), Math.round(trigger.height)],
    };
  };
  const namedControls = controls(named);
  expect(controls(anon), `named ${JSON.stringify(namedControls)}`).toEqual(namedControls);

  // 4. THE PIXELS. Sample the label zone — between the folder's right edge and the trigger's left
  //    edge — across the head's middle line, and compare each reading with the surface behind it.
  //    Ink shows up as contrast; the anonymous head must have none.
  const at = await capture();
  const inkOf = (head: HTMLElement) => {
    const box = head.getBoundingClientRect();
    const folder = (
      head.querySelector("[data-group-folder]") as HTMLElement
    ).getBoundingClientRect();
    const trigger = (
      head.querySelector("button[aria-label^='Group options']") as HTMLElement
    ).getBoundingClientRect();
    const y = box.top + box.height / 2;
    // The reference is the same band, past the trigger: the bare surface, with nothing on it.
    const paper = at(box.right - 2, y);
    let ink = 1;
    for (let x = folder.right + 2; x < trigger.left - 2; x += 1) {
      ink = Math.max(ink, contrast(at(x, y), paper));
    }
    return ink;
  };

  const namedInk = inkOf(named);
  const anonInk = inkOf(anon);
  // The named head really does paint its name on that band (glyphs against the surface).
  expect(namedInk, `named label zone ink ${namedInk.toFixed(2)}:1`).toBeGreaterThan(1.5);
  // The anonymous one paints nothing: the band is the bare surface, edge to edge.
  expect(anonInk, `unnamed label zone ink ${anonInk.toFixed(2)}:1`).toBeLessThan(1.1);
});

it("a SOLO terminal does not read as secondary next to a group — the capture says so", async () => {
  render(
    <div className="flex flex-col gap-0.5">
      <Row name="solo term" />
      <GroupBlock groupId="g1" color={null} name={null} open onToggle={() => {}}>
        <Row name="multi term A" />
        <Row name="multi term B" />
      </GroupBlock>
    </div>,
    { container: host },
  );

  const solo = host.querySelector('[data-row="solo term"]') as HTMLElement;
  const member = host.querySelector('[data-row="multi term A"]') as HTMLElement;

  // NOTHING dims, shrinks or de-emphasises the solo row: same colour, same size, same weight, same
  // opacity. A block is a DELIMITER, not a promotion.
  const soloStyle = getComputedStyle(solo);
  const memberStyle = getComputedStyle(member);
  for (const property of ["color", "fontSize", "fontWeight", "opacity"] as const) {
    expect(soloStyle[property], `solo vs member ${property}`).toBe(memberStyle[property]);
  }
  expect(solo.getBoundingClientRect().height).toBe(member.getBoundingClientRect().height);

  // AND ON THE PIXELS: the text of the solo row is exactly as contrasted against its own background
  // as the text of a grouped row is against the block's — which, at rest, is the same surface.
  const at = await capture();
  const soloBox = solo.getBoundingClientRect();
  const memberBox = member.getBoundingClientRect();
  const soloDot = at(soloBox.left + 12, soloBox.top + soloBox.height / 2);
  const soloBg = at(soloBox.right - 6, soloBox.top + soloBox.height / 2);
  const memberDot = at(memberBox.left + 12, memberBox.top + memberBox.height / 2);
  const memberBg = at(memberBox.right - 6, memberBox.top + memberBox.height / 2);

  const soloContrast = contrast(soloDot, soloBg);
  const memberContrast = contrast(memberDot, memberBg);
  const message = `solo row ${soloContrast.toFixed(2)}:1 vs grouped row ${memberContrast.toFixed(2)}:1`;
  // The solo row is never the DIMMER of the two.
  expect(soloContrast, message).toBeGreaterThanOrEqual(memberContrast);
  // …and the gap stays negligible: under 10 % of the reading, i.e. not a visible hierarchy.
  expect(Math.abs(soloContrast - memberContrast) / soloContrast, message).toBeLessThan(0.1);
});
