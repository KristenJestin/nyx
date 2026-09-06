import { PencilIcon, ServerCogIcon, SquareTerminalIcon, XIcon } from "lucide-react";
import { render } from "@testing-library/react";
import { page, userEvent } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

// The REAL Tailwind utilities and the REAL tokens: this spec measures the PIXELS every family is
// laid out with, so the stylesheet has to be the app's.
import "@/globals.css";

import { capture, captureSettled, luminance, type Pixel } from "@/lib/test-pixels";
import { Button } from "@/components/ui/button";
import { GroupBlock } from "@/components/panes/group-block";
import { LINE_GLYPH_SIZE } from "@/components/ui/sidebar-line";
import { RuntimeRow } from "./runtime-row";
import { RuntimeSelectionProvider } from "./runtime-selection";

/**
 * EVERY FAMILY IS THE SAME LINE — MEASURED, AND AGAINST EACH OTHER (human review
 * 01M12MW1DV1V455EWBMV048TE6).
 *
 * ## What this spec refuses to do, and why
 *
 * It contains NO expected height, NO expected inset, NO expected glyph size. Verbatim from the
 * review: « l'idée est de dire que la ligne qui montre un dossier a la même taille qu'un terminal,
 * l'icône même taille etc, c'est pas forcément 32 ». A spec that froze `height === 32` would fail
 * the day the line is legitimately retuned AND pass on the day one family quietly stopped following
 * the others — it would be measuring the wrong thing in both directions.
 *
 * So every assertion is an EQUALITY between families rendered in ONE tree: a terminal, an agent, a
 * managed command, a folder of terminals, a folder of commands. Retune `<SidebarLine>` and this
 * spec stays green; let any one family drift and it goes red, whatever the numbers are that day.
 *
 * ## What "the same line" has to mean, to be worth asserting
 *
 * Five readings, because those are the five ways two lines can look misaligned:
 *  - the BAND's height — a line is as tall as another line;
 *  - the GLYPH's box — same size, so a folder is a terminal glyph's twin and not a near-miss;
 *  - the GLYPH's x — same left inset, so the column of icons is a column;
 *  - the LABEL's x — same gap after the glyph, so the column of names is a column;
 *  - the PASTILLE's box and offset from its glyph — so an agent's "waiting" dot and a closed
 *    folder's rollup are ONE object rather than two that resemble each other.
 */

let host: HTMLElement;

beforeEach(async () => {
  await page.viewport(900, 600);
  host = document.createElement("div");
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  host.style.width = "280px";
  host.className = "bg-background";
  document.body.appendChild(host);
});

afterEach(() => {
  host.remove();
});

/** Every family of the runtime tree, side by side, in one render. */
function renderEveryFamily() {
  return render(
    <RuntimeSelectionProvider initialSelection={null}>
      <div className="flex flex-col gap-0.5">
        {/* TERMINAL — a plain line, quiet. */}
        <div data-family="terminal">
          <RuntimeRow
            selection={{ kind: "terminal", id: "t1" }}
            icon={<SquareTerminalIcon className={LINE_GLYPH_SIZE} />}
            label="a terminal"
            state="idle"
          />
        </div>
        {/* AGENT — the SAME line: another glyph, and a live pastille in its corner. */}
        <div data-family="agent">
          <RuntimeRow
            selection={{ kind: "terminal", id: "t2" }}
            icon={<SquareTerminalIcon className={LINE_GLYPH_SIZE} />}
            label="an agent"
            state="waiting"
          />
        </div>
        {/* COMMAND — the same line with a service glyph. */}
        <div data-family="command">
          <RuntimeRow
            selection={{ kind: "service", id: "s1" }}
            icon={<ServerCogIcon className={LINE_GLYPH_SIZE} />}
            label="a command"
            state="running"
          />
        </div>
        {/* FOLDER OF TERMINALS — draggable (a handleRef), open. */}
        <GroupBlock
          groupId="terminal-folder"
          color={null}
          name="a terminal group"
          open
          onToggle={() => {}}
          handleRef={() => {}}
          onOpenPanel={() => {}}
        >
          <span>member</span>
        </GroupBlock>
        {/* FOLDER OF COMMANDS — no drag, closed, and therefore carrying its rollup pastille. */}
        <GroupBlock
          groupId="command-folder"
          color={null}
          name="a command group"
          open={false}
          onToggle={() => {}}
          rollup="waiting"
        >
          <span>member</span>
        </GroupBlock>
        {/* A NAMELESS FOLDER — the default, and the case that actually broke. A group nobody named
            writes nothing where a name would be, and a band whose height came from its text was
            then SHORTER than every other line (28 px against 32). It is a family here so the
            equality below covers it and it cannot come back. */}
        <GroupBlock
          groupId="nameless-folder"
          color={null}
          name={null}
          open
          onToggle={() => {}}
          handleRef={() => {}}
        >
          <span>member</span>
        </GroupBlock>
      </div>
    </RuntimeSelectionProvider>,
    { container: host },
  );
}

/** The five families' BANDS, keyed by name — a row's band, and a head's band. */
function bands(): Record<string, HTMLElement> {
  const bandOf = (root: Element) =>
    root.querySelector("[data-sidebar-line] > :first-child") as HTMLElement;
  const family = (name: string) =>
    bandOf(host.querySelector(`[data-family='${name}']`) as HTMLElement);
  const head = (groupId: string) =>
    (host.querySelector(`[data-group-head='${groupId}']`) as HTMLElement)
      .firstElementChild as HTMLElement;
  return {
    terminal: family("terminal"),
    agent: family("agent"),
    command: family("command"),
    "terminal folder": head("terminal-folder"),
    "command folder": head("command-folder"),
    "nameless folder": head("nameless-folder"),
  };
}

/** The glyph box inside a band — a plain line's `<span>`, or a folder's inside its button. */
function glyphOf(band: HTMLElement): HTMLElement {
  return band.querySelector("span.relative") as HTMLElement;
}

/** The first non-glyph child of a band: the label (or the inert spacer of a nameless group). */
function labelOf(band: HTMLElement): HTMLElement {
  const glyph = glyphOf(band);
  return [...band.children].find((child) => !child.contains(glyph)) as HTMLElement;
}

/** Round to a whole pixel — sub-pixel noise from font metrics is not a misalignment. */
const px = (n: number) => Math.round(n);

it("every family is laid out on the SAME line — height, glyph and label, compared to each other", () => {
  renderEveryFamily();
  const all = bands();
  const names = Object.keys(all);
  const reference = names[0];

  const shape = (band: HTMLElement) => {
    const box = band.getBoundingClientRect();
    const glyph = glyphOf(band).getBoundingClientRect();
    const label = labelOf(band).getBoundingClientRect();
    return {
      height: px(box.height),
      glyphSize: [px(glyph.width), px(glyph.height)],
      // Offsets are taken from the BAND's own left edge, so the reading is the template's inset and
      // not wherever in the page that family happens to sit.
      glyphFromLeft: px(glyph.left - box.left),
      labelFromLeft: px(label.left - box.left),
    };
  };

  const expected = shape(all[reference]);
  // A sanity floor on the reference itself: a line that measured zero would make every equality
  // below trivially true. No expected VALUE — only "it was really laid out".
  expect(expected.height, `the ${reference} line was not laid out`).toBeGreaterThan(0);
  expect(expected.glyphSize[0], `the ${reference} glyph was not laid out`).toBeGreaterThan(0);

  for (const name of names.slice(1)) {
    expect(shape(all[name]), `${name} vs ${reference} — ${JSON.stringify(expected)}`).toEqual(
      expected,
    );
  }
});

it("the corner pastille is ONE object across families — same box, same offset from its glyph", () => {
  renderEveryFamily();
  const all = bands();

  // The families that CARRY a pastille here: the agent (live `waiting`), the command (`running`)
  // and the CLOSED command folder (its rollup). The quiet terminal carries none — `isQuietState` —
  // and that is asserted too, since "no dot when there is nothing to say" is the same one rule.
  const dotShape = (band: HTMLElement) => {
    const glyph = glyphOf(band).getBoundingClientRect();
    const dot = band.querySelector("[data-state]") as HTMLElement | null;
    if (dot == null) return null;
    const box = dot.getBoundingClientRect();
    const style = getComputedStyle(dot.querySelector("span:last-child") as HTMLElement);
    return {
      size: [px(box.width), px(box.height)],
      // Hung off the glyph's bottom-right corner — the two numbers that place it.
      fromGlyphRight: px(box.right - glyph.right),
      fromGlyphBottom: px(box.bottom - glyph.bottom),
      // The ring is what makes it legible over the glyph it overlaps; it is part of the metrics.
      ring: `${style.boxShadow.includes("rgb") ? "ringed" : "none"}`,
    };
  };

  const agent = dotShape(all.agent);
  expect(agent, "the agent line carries a live pastille").not.toBeNull();
  expect(agent?.size[0], "and it was really laid out").toBeGreaterThan(0);

  for (const name of ["command", "command folder"]) {
    expect(dotShape(all[name]), `${name} pastille vs agent — ${JSON.stringify(agent)}`).toEqual(
      agent,
    );
  }

  // …and the QUIET family draws nothing at all: one display rule, inherited, not re-implemented.
  expect(dotShape(all.terminal), "an idle line is a clean line").toBeNull();
});

/**
 * THE GUIDE LINE SITS ON THE FOLDER'S OWN CENTRE AXIS.
 *
 * `LINE_CHILD_INDENT` and `LINE_GUIDE_X` are static classes derived from the band's inset plus half
 * a glyph, which is arithmetic nobody re-does at runtime — so it is CHECKED here, against the real
 * folder's real centre. Retune the band or the glyph without following through, and this fails.
 */
it("the guide line is on the folder glyph's centre, and the members clear it", () => {
  renderEveryFamily();
  const block = host.querySelector("[data-group-block='terminal-folder']") as HTMLElement;
  const folder = block.querySelector("[data-group-folder]") as HTMLElement;
  const glyph = glyphOf(folder.parentElement as HTMLElement).getBoundingClientRect();
  const guide = (block.querySelector("[data-group-guide]") as HTMLElement).getBoundingClientRect();
  const body = (block.querySelector("[data-group-body]") as HTMLElement).getBoundingClientRect();

  // The guide's own centre against the glyph's centre. A 1px rule cannot straddle a half-pixel, so
  // the tolerance is that half pixel and nothing more.
  const guideCentre = guide.left + guide.width / 2;
  const glyphCentre = glyph.left + glyph.width / 2;
  expect(
    Math.abs(guideCentre - glyphCentre),
    `guide at ${guideCentre} vs folder centre ${glyphCentre}`,
  ).toBeLessThanOrEqual(0.5);

  // The members start CLEAR of the guide — the indent is what makes the line readable as a tree
  // rather than as a rule struck through the rows.
  const member = (block.querySelector("[data-group-body] > span:last-child") ??
    block.querySelector("[data-group-body]")) as HTMLElement;
  expect(body.left, "the body is indented past its own guide").toBeLessThan(guide.left);
  expect(member.getBoundingClientRect().left).toBeGreaterThan(guide.right);
});

/* ───────────────────── THE REVEAL, AND THE GLYPH THAT NO LONGER HAS ONE ─────────────────────
 *
 * Human review 01M13118SE5M04J4M9SJGZYAAT and 01M1311AQJ3GNDBS51TP4P9YWE, measured together because
 * they are two readings of the same claim: WHAT a line shows on hover is decided in one place, and
 * a family neither adds a reveal of its own nor keeps a hover the others do not have.
 */

/** The computed `background-color` of an element — a reading, never a expected value. */
const bg = (el: Element) => getComputedStyle(el).backgroundColor;

/** A line's trailing cluster, found by its structural marker rather than by family. */
const clusterOf = (line: Element) =>
  line.querySelector("[data-sidebar-line-actions]") as HTMLElement;

/** A terminal ROW with controls, and a folder HEAD with its `…` — the two families that reveal. */
function renderRevealingFamilies() {
  return render(
    <RuntimeSelectionProvider initialSelection={null}>
      <div className="flex flex-col gap-0.5">
        <div data-family="terminal">
          <RuntimeRow
            selection={{ kind: "terminal", id: "t1" }}
            icon={<SquareTerminalIcon className={LINE_GLYPH_SIZE} />}
            label="a terminal"
            state="idle"
            actions={
              <>
                <Button variant="ghost" size="icon-xs" aria-label="Rename terminal a terminal">
                  <PencilIcon />
                </Button>
                <Button
                  variant="ghost-destructive"
                  size="icon-xs"
                  aria-label="Close terminal a terminal"
                >
                  <XIcon />
                </Button>
              </>
            }
          />
        </div>
        <GroupBlock
          groupId="terminal-folder"
          color={null}
          name="a terminal group"
          open
          onToggle={() => {}}
          handleRef={() => {}}
          onOpenPanel={() => {}}
        >
          <span>member</span>
        </GroupBlock>
      </div>
    </RuntimeSelectionProvider>,
    { container: host },
  );
}

/** The two revealing families' LINES, keyed by name. */
function revealingLines(): Record<string, HTMLElement> {
  return {
    "terminal row": (host.querySelector("[data-family='terminal']") as HTMLElement).querySelector(
      "[data-sidebar-line]",
    ) as HTMLElement,
    "folder head": host.querySelector("[data-group-head='terminal-folder']") as HTMLElement,
  };
}

/** What must NOT move when the controls appear: the line's height and where its name starts. */
function templateOf(line: HTMLElement) {
  const box = line.getBoundingClientRect();
  const band = line.firstElementChild as HTMLElement;
  const label = labelOf(band).getBoundingClientRect();
  return { height: px(box.height), labelFromLeft: px(label.left - box.left) };
}

it("a line's controls are HIDDEN at rest and revealed on hover — same mechanic in every family", async () => {
  renderRevealingFamilies();
  const lines = revealingLines();
  const names = Object.keys(lines);

  // AT REST, and read for every family BEFORE any pointer moves — a reading taken after hovering a
  // neighbour would be measuring a transition on its way back rather than the resting state.
  const atRest = names.map((name) => ({
    name,
    opacity: getComputedStyle(clusterOf(lines[name])).opacity,
    template: templateOf(lines[name]),
  }));
  for (const family of atRest) {
    expect(family.opacity, `${family.name}: nothing showing at rest`).toBe("0");
  }

  for (const family of atRest) {
    const line = lines[family.name];
    await userEvent.hover(line);
    // `transition-opacity` means the value is interpolated: poll to the SETTLED state rather than
    // catching an arbitrary frame.
    await expect
      .poll(() => getComputedStyle(clusterOf(line)).opacity, {
        timeout: 2000,
      })
      .toBe("1");
    // …AND THE TEMPLATE DID NOT MOVE. This is the whole reason the reveal is an OPACITY: `hidden`
    // or `w-0` would read identically in the DOM and would shove the label sideways the instant the
    // pointer arrived. Measured, per family, against that family's own resting reading.
    expect(templateOf(line), `${family.name}: the line moved when its controls appeared`).toEqual(
      family.template,
    );
  }
});

it("…and the KEYBOARD reveals them too — focus-visible at the same rank as hover", async () => {
  renderRevealingFamilies();
  const lines = revealingLines();

  for (const name of Object.keys(lines)) {
    const cluster = clusterOf(lines[name]);
    // Walk the tab order from wherever focus is until it lands INSIDE the cluster. The walk itself
    // is half the assertion: a control revealed only by the pointer would not be reachable at all,
    // and this loop would run out of tabs.
    let reached = false;
    for (let i = 0; i < 12 && !reached; i += 1) {
      await userEvent.tab();
      reached = cluster.contains(document.activeElement);
    }
    expect(reached, `${name}: its controls are reachable by keyboard`).toBe(true);
    await expect.poll(() => getComputedStyle(cluster).opacity, { timeout: 2000 }).toBe("1");
  }
});

it("the folder glyph carries NO hover of its own, and the two families agree", async () => {
  renderEveryFamily();
  const folders = ["terminal-folder", "command-folder"].map((groupId) => {
    const head = host.querySelector(`[data-group-head='${groupId}']`) as HTMLElement;
    return { groupId, head, folder: head.querySelector("[data-group-folder]") as HTMLElement };
  });

  // Read EVERY resting value first — a folder's own fill, and the LINE's, which is the control.
  const atRest = folders.map(({ head, folder }) => ({ line: bg(head), folder: bg(folder) }));

  const hovered: string[] = [];
  for (const [i, { groupId, head, folder }] of folders.entries()) {
    await userEvent.hover(folder);
    // THE LINE REALLY REACTED. Without this the next assertion would be a reading of nothing: a
    // hover that never landed also leaves the glyph unchanged.
    await expect.poll(() => bg(head), { timeout: 2000 }).not.toBe(atRest[i].line);
    expect(bg(folder), `${groupId}: the glyph painted a hover of its own`).toBe(atRest[i].folder);
    hovered.push(bg(folder));
  }
  // …AND NO DIVERGENCE BETWEEN FAMILIES — the second half of the finding: the glyph's hover was not
  // only useless, it was DIFFERENT on each side, which is what proved nobody had decided it.
  expect(hovered[1], "the two families read the same").toBe(hovered[0]);
  expect(atRest[1].folder, "…at rest as well").toBe(atRest[0].folder);
});

it("…and the folder keeps its full pointer target, its cursor and its toggle role", () => {
  renderEveryFamily();
  const boxes = ["terminal-folder", "command-folder"].map((groupId) => {
    const head = host.querySelector(`[data-group-head='${groupId}']`) as HTMLElement;
    const folder = head.querySelector("[data-group-folder]") as HTMLElement;
    const glyph = glyphOf(folder).getBoundingClientRect();
    const target = folder.getBoundingClientRect();
    return {
      // The target OVERFLOWS the glyph on every side (`LINE_GLYPH_TARGET`'s `-m-1`) — the reading
      // that says the 24×24 press area survived, without naming 24.
      overflowsBy: [px(target.width - glyph.width), px(target.height - glyph.height)],
      cursor: getComputedStyle(folder).cursor,
      togglesAria: folder.getAttribute("aria-expanded") != null,
    };
  });
  expect(boxes[0].overflowsBy[0], "the folder is a bigger target than its glyph").toBeGreaterThan(
    0,
  );
  // The two families keep the same target and the same toggle role.
  expect(boxes[1].overflowsBy).toEqual(boxes[0].overflowsBy);
  expect([boxes[0].togglesAria, boxes[1].togglesAria]).toEqual([true, true]);
  // …AND THE SAME CURSOR (human review 01M140TP2F7X0DGMRC0RQQFHAF). « sur l'icon dossier y a
  // toujours le mouse drag, là à dégager ». The terminal folder read `grab` and the command folder
  // `pointer`, and that divergence was a leftover: the glyph WAS the handle once. Since the grip
  // became the whole head (01M1312FSV6MAR0P63SEG2RF6C) a grab cursor on the glyph designates a zone
  // with no special hold on the drag, and it names the wrong gesture — the glyph folds. This is the
  // REAL COMPUTED cursor in a real browser, read on both families, so a class that stopped resolving
  // would be caught as readily as a class that came back.
  expect([boxes[0].cursor, boxes[1].cursor], "one cursor, and it is a toggle's").toEqual([
    "pointer",
    "pointer",
  ]);
});

/**
 * THE BLOCK'S HOVER FILL AND THE LINE'S ADD UP — AND WHAT THAT ACTUALLY LOOKS LIKE, MEASURED
 * (human review 01M12MW1DV1V455EWBMV048TE6; the mockup names this as the trap to freeze).
 *
 * A group block tints itself on hover (`--group-tone` at 7 %), and a line inside it tints itself on
 * hover (`muted/60`). The line is INSIDE the block, so hovering a member paints both layers — and
 * the instruction is that the sum be MEASURED, not merely intended: « 7 % de ton + muted/60, et pas
 * davantage ».
 *
 * WHAT THE MEASUREMENT SAYS, and it is not quite what the mockup predicted. On the DEFAULT grey
 * (`--identity-0`) in the light theme those two layers land on the same value: `muted` at 60 % over
 * white and 7 % of the neutral over white are within a pixel level of each other, so compositing
 * one over the other changes nothing a screen can show. The extent of the group is still revealed —
 * hovering any member tints the whole block, which is the job — but "a member row reads lighter
 * than a solo row" is only true for a group that has been given a COLOUR. Both cases are frozen
 * below, on the pixels, so the claim in the code matches the claim on the screen.
 *
 * Readings are DISTANCES from the resting surface, never "lighter" or "darker": which way the tints
 * move is the theme's business, and the claim being made is not.
 */
function renderGroupedAndSolo(color: 3 | null) {
  return render(
    <RuntimeSelectionProvider initialSelection={null}>
      <div className="flex flex-col gap-0.5">
        <div data-family="solo">
          <RuntimeRow
            selection={{ kind: "terminal", id: "solo" }}
            icon={<SquareTerminalIcon className={LINE_GLYPH_SIZE} />}
            label="solo line"
            state="idle"
          />
        </div>
        <GroupBlock groupId="g" color={color} name="grouped" open onToggle={() => {}}>
          <div data-family="member">
            <RuntimeRow
              selection={{ kind: "terminal", id: "member" }}
              icon={<SquareTerminalIcon className={LINE_GLYPH_SIZE} />}
              label="member line"
              state="idle"
            />
          </div>
        </GroupBlock>
      </div>
    </RuntimeSelectionProvider>,
    { container: host },
  );
}

/**
 * The three surfaces the superposition is made of, all read at the SAME settled frame where the
 * pointer sits on the member: the block's tint ALONE (the indent gutter, which the members are
 * pushed clear of — that is what the indent is for), and the member's own surface (block + line).
 * Plus the two baselines: everything at rest, and a solo line hovered on its own.
 */
async function hoverReadings() {
  const lineIn = (family: string) =>
    (host.querySelector(`[data-family='${family}']`) as HTMLElement).querySelector(
      "[data-sidebar-line]",
    ) as HTMLElement;
  const solo = lineIn("solo");
  const member = lineIn("member");
  const probeOf = (el: HTMLElement) => () => {
    const box = el.getBoundingClientRect();
    return [box.right - 4, box.top + box.height / 2] as const;
  };
  const soloProbe = probeOf(solo);
  const memberProbe = probeOf(member);

  const rest = await capture();
  const soloRest = rest(...soloProbe());
  const memberRest = rest(...memberProbe());

  // Each capture WAITS for `transition-colors` to land: a screenshot taken mid-transition is an
  // arbitrary frame, and comparing two of those measures timing instead of design.
  await userEvent.hover(solo);
  const soloShot = await captureSettled(soloProbe);
  const soloHover = soloShot(...soloProbe());

  await userEvent.hover(member);
  const memberShot = await captureSettled(memberProbe);
  const memberHover = memberShot(...memberProbe());
  const blockBox = (
    host.querySelector("[data-group-block]") as HTMLElement
  ).getBoundingClientRect();
  const memberBox = member.getBoundingClientRect();
  const blockOnly = memberShot(blockBox.left + 4, memberBox.top + memberBox.height / 2);

  const base = luminance(soloRest);
  const tint = (p: Pixel) => Math.abs(luminance(p) - base);
  return { soloRest, memberRest, soloHover, memberHover, blockOnly, tint };
}

it("at rest the block paints NOTHING — a grouped line is pixel-identical to a solo one", async () => {
  renderGroupedAndSolo(null);
  const r = await hoverReadings();
  expect(r.memberRest, "variant B+: no border, no fill, nothing at rest").toEqual(r.soloRest);
});

it("hovering a member tints the WHOLE block, and never beyond the two declared layers", async () => {
  renderGroupedAndSolo(null);
  const { soloHover, memberHover, blockOnly, tint } = await hoverReadings();

  // 1. THE EXTENT IS REVEALED: hovering a member tints the block outside that member's own row.
  expect(tint(blockOnly), `block tint ${JSON.stringify(blockOnly)}`).toBeGreaterThan(0);
  // 2. A line's own hover registers on its own.
  expect(tint(soloHover), "the line's own hover really registers").toBeGreaterThan(0);
  // 3. NOTHING CANCELS: the member's surface is at least as tinted as the block under it.
  expect(tint(memberHover)).toBeGreaterThanOrEqual(tint(blockOnly));
  // 4. AND NOTHING ELSE CREEPS IN — « pas davantage ». The composite never exceeds the two declared
  //    layers taken together, so no third fill can be added here without this going red.
  expect(
    tint(memberHover),
    `member ${tint(memberHover).toFixed(4)} vs block ${tint(blockOnly).toFixed(4)} + line ${tint(soloHover).toFixed(4)}`,
  ).toBeLessThanOrEqual(tint(blockOnly) + tint(soloHover));
});

it("on a COLOURED group the two layers really compose — the member differs from both", async () => {
  renderGroupedAndSolo(3);
  const { soloHover, memberHover, blockOnly, tint } = await hoverReadings();

  // The tone is a real colour here, so the two layers are distinguishable and the superposition is
  // visible: the member's surface is neither the block's tint alone nor a solo line's hover.
  expect(
    tint(memberHover),
    `member ${tint(memberHover).toFixed(4)} vs block alone ${tint(blockOnly).toFixed(4)}`,
  ).toBeGreaterThan(tint(blockOnly));
  expect(
    tint(memberHover),
    `member ${tint(memberHover).toFixed(4)} vs solo hover ${tint(soloHover).toFixed(4)}`,
  ).toBeGreaterThan(tint(soloHover));
  // …and it is still bounded by the sum of the two: a tint on a tint, not a fill.
  expect(tint(memberHover)).toBeLessThanOrEqual(tint(blockOnly) + tint(soloHover));
});
