// The REAL Tailwind utilities (the vitest config compiles them for a spec that imports
// `globals.css`), so the overlay's `absolute inset-y-0 right-0 z-20 items-stretch` geometry and its
// `pointer-events-*` classes actually apply — without them there is no band to hit-test.
import "@/globals.css";

import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";
import { userEvent } from "vitest/browser";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";

import type { Spec } from "../specs/types";
import { SpecOverlay } from "./spec-overlay";
import { ACTIVE_SESSION, detail, spec } from "./test-fixtures";

/**
 * BROWSER-MODE HIT-TESTING spec (real Chromium via Playwright, headless) for the terminal↔Spec
 * overlay's closed languette column.
 *
 * The defect it pins (PRD "overlay Spec casse le scroll"): the overlay root is `absolute inset-y-0
 * right-0 z-20 flex items-stretch`, so its languette COLUMN — 36px wide (`w-9`, from `<OverlayTab>`)
 * — is STRETCHED to the full pane height even though the languette itself is `justify-center`-ed in
 * the middle. While that column was `pointer-events-auto` it captured every click landing in the
 * right-edge band, INCLUDING the empty zones above/below the languette. What those zones cover does
 * not belong to the overlay at all: the terminal's own scroll-to-bottom button
 * (`components/terminal/terminal.tsx`, `absolute right-3 bottom-3 z-10`) and the outermost 8px
 * gutter where the global `0.5rem` scrollbar of the surface underneath lives (`globals.css`, on the
 * `*` selector). The one exception — the drawer's OWN bar, once its stack overflows — is the subject
 * of the last test here.
 *
 * jsdom cannot express this: it has no layout and no hit-testing, so the interception is only
 * observable in a browser that composites. `document.elementFromPoint()` IS the hit test the mouse
 * performs — it honours `pointer-events`, stacking and geometry — so probing it at the exact
 * coordinates of the masked controls is the counterfactual: before the fix every probe in the band
 * returns the overlay column; after it, the probe reaches the control underneath while the languette
 * itself stays hittable.
 */

/** The pane the overlay is anchored over — kept inside the browser-mode viewport. */
const PANE_W = 380;
const PANE_H = 320;
/** The terminal's scroll-to-bottom button geometry (`right-3 bottom-3` = 12px insets, `icon-sm`). */
const BTN_INSET = 12;
const BTN_SIZE = 28;
/** Width of the languette column (`w-9` on `<OverlayTab>`). */
const BAND_W = 36;
/** Width of the global scrollbar defined on `*` in `globals.css` (`0.5rem`). */
const SCROLLBAR_W = 8;

/**
 * In the browser suite there is no Electron preload, so `@/bridge` already resolves `nyxBridge` to a
 * live {@link FakeNyxBridge} (its documented test/SSR safety net). We drive THAT instance directly
 * instead of the `vi.mock`-based `test-harness` shim, so the spec never depends on module mocking.
 */
const fake = nyxBridge as FakeNyxBridge;

/** Serve the overlay's read models for the mapped Specs (one for a rail, N for a drawer stack). */
function serve(...specs: Spec[]) {
  fake.onAnyCommand((cmd, args) => {
    if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs };
    if (cmd === "spec_detail") {
      return detail(specs.find((s) => s.id === (args?.specId as string)) ?? specs[0]);
    }
    return null;
  });
}

/**
 * A fixed, pane-shaped host: `position: fixed` so it is a containing block for the overlay's
 * `absolute inset-y-0 right-0` root, exactly like the terminal container (`relative h-full w-full`).
 * It carries an underlay standing for the terminal surface and a stand-in for the terminal's
 * scroll-to-bottom button at ITS geometry (inline styles, so the probe never depends on which
 * utilities Tailwind happened to emit for this file).
 *
 * The underlay is a REAL SCROLL CONTAINER (`overflow-y: auto` over content taller than the pane),
 * not a bare `inset-0` div: step 2 of the PRD's repro is "clicking the scrollbar track does
 * nothing", so the surface under the band has to actually OWN a scrollbar — the global `0.5rem` one
 * `globals.css` puts on `*` — for the gutter probe to mean anything. Over a non-scrollable underlay
 * that probe would only re-test what `bandMiddle` already tests (the band lets clicks through).
 */
function mountHost(): { host: HTMLElement; underlay: HTMLElement; button: HTMLElement } {
  const host = document.createElement("div");
  host.setAttribute("data-testid", "pane-host");
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  host.style.width = `${PANE_W}px`;
  host.style.height = `${PANE_H}px`;
  document.body.appendChild(host);

  const underlay = document.createElement("div");
  underlay.setAttribute("data-testid", "pane-underlay");
  underlay.style.position = "absolute";
  underlay.style.inset = "0";
  underlay.style.overflowY = "auto";
  // The overflowing content is a 1px-wide sliver parked on the LEFT, so it makes the underlay scroll
  // without ever becoming the element a right-edge probe lands on.
  const filler = document.createElement("div");
  filler.setAttribute("data-testid", "pane-underlay-filler");
  filler.style.width = "1px";
  filler.style.height = `${PANE_H * 4}px`;
  underlay.appendChild(filler);
  host.appendChild(underlay);

  const button = document.createElement("button");
  button.setAttribute("data-testid", "scroll-to-bottom");
  button.style.position = "absolute";
  button.style.right = `${BTN_INSET}px`;
  button.style.bottom = `${BTN_INSET}px`;
  button.style.width = `${BTN_SIZE}px`;
  button.style.height = `${BTN_SIZE}px`;
  button.style.zIndex = "10";
  host.appendChild(button);

  return { host, underlay, button };
}

/** Poll until `pred()` holds or we time out; returns whether it held. */
async function waitFor(pred: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return pred();
}

/** Resolve on the next PAINT — the sampling unit of {@link waitForRestingTab}. */
function nextFrame(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => resolve());
  });
}

/**
 * Whether anything under `surface` is still PLAYING. The motion runtime accelerates what it can onto
 * the browser's own timeline, so a running arrival is observable through `getAnimations()`.
 *
 * The status point's `motion-safe:animate-pulse` is excluded: it is INFINITE by design (the
 * "authoring live" breath, emitted by `spec-drawer-overlay` on the DrawerTab's status point and by
 * `spec-rail-overlay` on a live rail segment — `overlay-motion` documents it as the continuous
 * counterpart of the one-shot arrival ring), so waiting on it would never return.
 *
 * That branch is LOAD-BEARING, not defensive: a live draft really does keep a running
 * `iterations === Infinity` CSSAnimation in this subtree at EVERY sampled frame, so without it
 * `isAnimating()` would never report false and `waitForRestingTab` would burn its whole budget —
 * failing the `mountOverlay` precondition of both drawer tests. Do NOT drop it.
 *
 * What `overlay-tab` itself animates is the one-shot arrival ring (`overlayArrivalVariants`, 0.9s):
 * FINITE, and precisely the animation this guard must keep waiting on.
 */
function isAnimating(surface: Element): boolean {
  return surface.getAnimations({ subtree: true }).some((animation) => {
    const timing = animation.effect?.getComputedTiming();
    if (timing?.iterations === Number.POSITIVE_INFINITY) return false;
    return animation.playState === "running";
  });
}

/**
 * Wait for the languette to come to REST, i.e. flush against the pane's right edge AND motionless.
 *
 * "Flush" ALONE is not rest: the shared arrival transition is a spring with `bounce: 0.15`
 * (`overlay-motion.ts`), i.e. UNDER-damped, so the surface OVERSHOOTS the flush position and comes
 * back — `|tab.right - host.right| < 0.5` is already true on the first zero-crossing, while the box
 * is still travelling at full speed. Probing there reads a transient geometry, which is exactly how
 * this file went flaky under CPU contention (the probes landed mid-overshoot and every one of them
 * hit the overlay column).
 *
 * So rest is sampled per FRAME (`requestAnimationFrame`, not a wall-clock timer): three consecutive
 * frames with a BYTE-IDENTICAL rect mean two whole frames elapsed with no movement. Under a starved
 * CPU that simply takes longer — it cannot report a false rest the way a `setTimeout` sampler can,
 * since a stalled compositor stalls the sampler too.
 */
async function waitForRestingTab(
  host: HTMLElement,
  timeoutMs = 5000,
): Promise<{ ok: boolean; seen: string }> {
  const deadline = Date.now() + timeoutMs;
  let previous = "";
  let stableFrames = 0;
  let seen = "no [data-overlay-tab] ever mounted";
  while (Date.now() < deadline) {
    await nextFrame();
    const tab = host.querySelector("[data-overlay-tab]");
    const surface = host.querySelector("[data-overlay-surface]");
    if (tab === null || surface === null) {
      previous = "";
      stableFrames = 0;
      continue;
    }
    const rect = tab.getBoundingClientRect();
    const frame = `${rect.top}|${rect.right}|${rect.bottom}|${rect.left}`;
    stableFrames = frame === previous ? stableFrames + 1 : 0;
    previous = frame;
    const overshoot = rect.right - host.getBoundingClientRect().right;
    const animating = isAnimating(surface);
    seen = `right offset ${overshoot.toFixed(2)}px, ${stableFrames} still frame(s), animating=${animating}`;
    if (Math.abs(overshoot) < 0.5 && !animating && stableFrames >= 2) return { ok: true, seen };
  }
  return { ok: false, seen };
}

/**
 * Mount the overlay over the host and wait for its arrival spring (surface + languette both slide in
 * from the right, `overlay-motion`) to come to REST flush against the pane's right edge — probing
 * mid-animation would read a transient offset, not the resting geometry ({@link waitForRestingTab}).
 */
async function mountOverlay(host: HTMLElement): Promise<HTMLElement> {
  const mount = document.createElement("div");
  host.appendChild(mount);
  render(<SpecOverlay terminalId="term-1" />, { container: mount });

  const settled = await waitForRestingTab(host);
  expect(
    settled.ok,
    `the overlay languette mounted and its arrival spring came to REST flush to the pane's right edge (last sample: ${settled.seen})`,
  ).toBe(true);
  return host.querySelector("[data-overlay-tab]") as HTMLElement;
}

/**
 * WHAT the mouse would hit at `(x, y)`, as a stable label — the real browser hit test
 * (`elementFromPoint` honours `pointer-events`, stacking and geometry). Labelling every probe
 * instead of asserting them one-by-one makes ONE assertion report the WHOLE repro: which of the
 * four right-edge targets the overlay swallows and which it lets through.
 */
function hitLabel(x: number, y: number): string {
  const el = document.elementFromPoint(x, y);
  if (el === null) return "none";
  if (el.closest("[data-overlay-tab]") !== null) return "overlay-languette";
  // The animated ROOT itself (`absolute inset-y-0 right-0`), named FIRST and apart: it also contains
  // a `[data-overlay-tab]`, so the column branch below would label it `overlay-languette-column` and
  // a failure would accuse the wrong box. The two are different defects — the root covering the band
  // means the `pointer-events-none` on the root regressed, the column covering it means the wrapper's
  // did.
  if (el.matches("[data-overlay-surface]")) return "overlay-surface-root";
  // The full-height wrapper the languette lives in — THE capteur of the defect. Named apart from the
  // rest of the chrome so the counterfactual points at the element responsible, not just "the
  // overlay".
  if (el.closest("[data-overlay-surface]") !== null) {
    return el.querySelector("[data-overlay-tab]") !== null
      ? "overlay-languette-column"
      : "overlay-chrome";
  }
  return el.getAttribute("data-testid") ?? el.tagName.toLowerCase();
}

/**
 * The FULL-HEIGHT band wrapper the languette lives in — the direct child of the animated surface
 * that `items-stretch` blows up to the whole pane. Resolved by walking UP from the languette instead
 * of taking `tab.parentElement`, because the two overlays nest differently: the rail's languette is
 * a direct child of that wrapper, while the drawer's sits one level deeper, inside the SCROLLER the
 * wrapper centres ({@link spec-drawer-overlay}).
 */
function bandColumn(tab: HTMLElement): HTMLElement {
  const surface = tab.closest("[data-overlay-surface]") as HTMLElement;
  const column = Array.from(surface.children).find((child) => child.contains(tab));
  expect(column, "the languette lives in a column of the overlay surface").toBeTruthy();
  return column as HTMLElement;
}

beforeEach(() => {
  fake.reset();
});

afterEach(() => {
  cleanup();
  fake.reset();
  document.body.replaceChildren();
});

/**
 * PIN which of the two overlays is actually on screen. `<SpecOverlay>` picks rail A or tiroir B from
 * the fixture's status alone (`overlayMode`, `overlay-status.ts`), and the assertion battery below is
 * shared, so nothing else in the body would notice if that routing drifted: the test TITLED "drawer"
 * would go on re-testing the rail, green, while `spec-drawer-overlay` lost its coverage. The marker
 * is the drawer stack's `aria-label="Live specs"` (emitted by `spec-drawer-overlay` only) plus the
 * languette's own label — "… live spec" on the tiroir, "… progress" on the rail.
 */
function expectSubject(host: HTMLElement, subject: "rail" | "drawer") {
  const stack = host.querySelector('[aria-label="Live specs"]');
  const tabLabel = host.querySelector("[data-overlay-tab]")?.getAttribute("aria-label") ?? "";
  if (subject === "drawer") {
    expect(stack, "the tiroir B stack (spec-drawer-overlay) is the mounted overlay").not.toBeNull();
    expect(tabLabel).toMatch(/live spec$/i);
  } else {
    expect(
      stack,
      "no tiroir B stack — rail A (spec-rail-overlay) is the mounted overlay",
    ).toBeNull();
    expect(tabLabel).toMatch(/progress$/i);
  }
}

/**
 * The FOUR probes that CARRY the PRD's repro, read at the exact coordinates of the controls the band
 * masks. Extracted so EVERY configuration runs the same battery — including the overflowing stack,
 * whose accepted limit is only distinguishable from a regression if both are measured identically.
 */
function bandProbes(host: HTMLElement, tab: HTMLElement, button: HTMLElement) {
  const pane = host.getBoundingClientRect();
  const tabRect = tab.getBoundingClientRect();
  const btnRect = button.getBoundingClientRect();
  // 6px above the languette — CLAMPED inside the pane: when the stack overflows, the first languette
  // starts at the very top of the pane and the raw point would fall outside the viewport, where
  // `elementFromPoint` returns null and would characterise nothing. On a stack that does not
  // overflow the languette is `justify-center`-ed, so the clamp never bites — and the two tests below
  // DO go red if it ever starts to. The clamp bites exactly when `tabRect.top < pane.top + 12`, which
  // is the threshold their `tabRect.top > pane.top + 12` guard asserts against, so "the clamp bit"
  // and "that guard failed" are the same event rather than two overlapping ones. (Measured margin
  // today: the languette sits 76px below that threshold on the drawer, 111px on the rail.)
  const aboveY = Math.max(tabRect.top - 6, pane.top + 6);
  return {
    // The scrollbar TRACK of the surface underneath (its outermost 8px), in the zone ABOVE the
    // languette — the "click on the track does nothing" of the repro.
    scrollbarGutter: hitLabel(pane.right - SCROLLBAR_W / 2, aboveY),
    // The MIDDLE of the band in that same zone (a click just left of the scrollbar).
    bandMiddle: hitLabel(pane.right - BAND_W / 2, aboveY),
    // The terminal's scroll-to-bottom button, in the zone BELOW the languette.
    scrollToBottom: hitLabel(btnRect.left + btnRect.width / 2, btnRect.top + btnRect.height / 2),
    // Non-regression: the languette itself, which carries its OWN `pointer-events-auto` (its rail
    // content / status dot inherit it as children of the button).
    languette: hitLabel(tabRect.left + tabRect.width / 2, tabRect.top + tabRect.height / 2),
  };
}

/**
 * The shared assertion battery, run against BOTH overlays (rail A and tiroir B) — they build the
 * same `inset-y-0 items-stretch` column from the same `<OverlayTab>`, so the defect and the fix are
 * one and the same in both files. `subject` is what makes each run name the file it covers.
 */
async function assertBandIsClickThrough(subject: "rail" | "drawer") {
  const { host, underlay, button } = mountHost();
  const tab = await mountOverlay(host);
  expectSubject(host, subject);

  const pane = host.getBoundingClientRect();
  const tabRect = tab.getBoundingClientRect();

  // The surface under the band REALLY carries a scrollbar: it is a live scroll container (the browser
  // honours a `scrollTop` write — a non-scrollable box would swallow it) whose content overflows, so
  // Chromium paints the `globals.css` 0.5rem bar in its outermost 8px. Without this the gutter probe
  // below aims at a track that does not exist.
  expect(getComputedStyle(underlay).overflowY).toBe("auto");
  expect(underlay.scrollHeight).toBeGreaterThan(underlay.clientHeight);
  underlay.scrollTop = 24;
  expect(underlay.scrollTop, "the underlay is a live scroll container, not just a styled box").toBe(
    24,
  );
  underlay.scrollTop = 0;

  // The column really is the full-height band the PRD describes: 36px wide, stretched to the whole
  // pane height, with the languette centred inside it ⇒ live empty zones above AND below it.
  // The ABOVE bound is `+12`, deliberately tighter than its `pane.bottom - 4` twin: 12px is the
  // `aboveY` clamp's own threshold in {@link bandProbes}, so any layout in which that clamp would
  // start displacing the two upper probes turns THIS line red first. Do not loosen it back to `+4`
  // for symmetry — the clamp only exists on the above side.
  const column = bandColumn(tab);
  expect(Math.round(tabRect.width)).toBe(BAND_W);
  expect(Math.round(column.getBoundingClientRect().height)).toBe(PANE_H);
  expect(tabRect.top).toBeGreaterThan(pane.top + 12);
  expect(tabRect.bottom).toBeLessThan(pane.bottom - 4);

  // The scroll-to-bottom button really is masked by the band (right:12px + 28px wide ⇒ it overlaps
  // the 36px column) and really is in the empty zone BELOW the languette.
  const btnRect = button.getBoundingClientRect();
  expect(btnRect.right).toBeGreaterThan(pane.right - BAND_W);
  expect(btnRect.top + btnRect.height / 2).toBeGreaterThan(tabRect.bottom);

  // The gutter probe aims at the middle of the underlay's OWN bar strip: the outermost 0.5rem of the
  // scroll container, which is where `globals.css` composites it.
  const gutterX = pane.right - SCROLLBAR_W / 2;
  const underlayRect = underlay.getBoundingClientRect();
  expect(gutterX).toBeGreaterThan(underlayRect.right - SCROLLBAR_W);
  expect(gutterX).toBeLessThan(underlayRect.right);

  // PRECONDITION of the whole battery, asserted on the STYLE before it is asserted through the
  // MOUSE: both full-height boxes that span the band — the animated root and the column it stretches
  // — are click-through. The probes below are the consequence; if this pair regresses, THIS is the
  // line that should name the cause, instead of four probes reporting the symptom ("everything hits
  // the overlay") without saying which box did it.
  const surface = tab.closest("[data-overlay-surface]") as HTMLElement;
  expect(getComputedStyle(surface).pointerEvents, "the overlay ROOT is click-through").toBe("none");
  expect(
    getComputedStyle(column).pointerEvents,
    "the full-height languette COLUMN is click-through",
  ).toBe("none");

  expect(bandProbes(host, tab, button)).toEqual({
    scrollbarGutter: "pane-underlay",
    bandMiddle: "pane-underlay",
    scrollToBottom: "scroll-to-bottom",
    languette: "overlay-languette",
  });
}

it("rail overlay: the full-height languette band lets the right-edge controls through", async () => {
  serve(spec({ id: "s1", number: 7, title: "Auth revamp", status: "in_progress" }));
  await assertBandIsClickThrough("rail");
});

it("drawer overlay: the full-height languette band lets the right-edge controls through", async () => {
  serve(spec({ id: "s1", number: 4, title: "Payments spec", status: "draft" }));
  await assertBandIsClickThrough("drawer");
});

/**
 * The OTHER half of the decision — "wrappers plein écran none, CONTRÔLES VISIBLES auto". The drawer
 * column is the only one of the two that is a SCROLL CONTAINER (`overflow-y-auto`), so past ~6 live
 * drafts (`h-36` tiles + `gap-2` on this 320px pane) it paints its OWN 0.5rem bar (`globals.css`, on
 * `*`) inside the 36px band. That bar belongs to the overlay, and in Chromium a `pointer-events:
 * none` box is not hit-testable — scrollbar included — so making the whole column click-through
 * would have TAKEN AWAY an affordance instead of freeing the terminal's.
 *
 * Hence the split in `spec-drawer-overlay`: the full-height WRAPPER stays `pointer-events-none`
 * (that is what the two tests above probe, stack not overflowing) while the SCROLLER it centres
 * carries `pointer-events-auto`. This test pins the second half: an overflowing stack still scrolls
 * on a wheel AND its scrollbar strip is hittable.
 */
const OVERFLOWING_DRAFTS = 6;

it("drawer overlay: an OVERFLOWING stack keeps its own scrollbar scrollable and hittable", async () => {
  serve(
    ...Array.from({ length: OVERFLOWING_DRAFTS }, (_, i) =>
      spec({ id: `s${i + 1}`, number: i + 1, title: `Draft ${i + 1}`, status: "draft" }),
    ),
  );
  const { host, button } = mountHost();
  const tab = await mountOverlay(host);
  expectSubject(host, "drawer");
  const mounted = await waitFor(
    () => host.querySelectorAll("[data-overlay-tab]").length === OVERFLOWING_DRAFTS,
  );
  expect(mounted, `the drawer stacked all ${OVERFLOWING_DRAFTS} drafts`).toBe(true);

  // The scroller is the drawer's OWN box (`aria-label="Live specs"`), nested inside the full-height
  // band wrapper — and it really overflows, so Chromium really paints a bar in it.
  const wrapper = bandColumn(tab);
  const scroller = wrapper.querySelector('[aria-label="Live specs"]') as HTMLElement;
  expect(scroller, "the drawer mounted its stack container").not.toBeNull();
  expect(scroller.scrollHeight).toBeGreaterThan(scroller.clientHeight);

  // THE OTHER reason the wrapper and the scroller are two boxes (see the split's note in
  // `spec-drawer-overlay`), which the probe battery below cannot see — it reads the same labels
  // either way. With `justify-center` and `overflow-y-auto` on ONE box, an overflowing stack is
  // centred around the box's middle while scrolling can only expose what lies BELOW the origin: the
  // first languettes are laid out ABOVE `scrollTop === 0` and no gesture brings them back (half the
  // pile permanently unreachable). Nothing has scrolled the stack at this point — the wheel is
  // further down and `scrollTop === 0` is asserted there — so the first languette must be entirely
  // inside its scroller. Recollapse the two boxes into one and this line goes red.
  const firstTab = host.querySelector("[data-overlay-tab]") as HTMLElement;
  expect(
    firstTab.getBoundingClientRect().top,
    "at scrollTop=0 the FIRST languette of the pile is inside the scroller, not clipped above it",
  ).toBeGreaterThanOrEqual(scroller.getBoundingClientRect().top);

  // THE rework: the full-height wrapper is the click-through one, the SCROLLER re-arms itself.
  expect(getComputedStyle(wrapper).pointerEvents).toBe("none");
  expect(getComputedStyle(scroller).pointerEvents).toBe("auto");

  // THE KNOWN BOUNDARY, pinned as a CHARACTERISATION — the SAME four probes as the two tests above,
  // on a stack that FILLS the pane. These values are NOT the target: they are the measured, accepted
  // limit of the guarantee (see the "KNOWN BOUNDARY" note in `spec-drawer-overlay`). Once the stack
  // fills the pane the band has NO empty zone left, so the opaque `pointer-events-auto` languettes
  // and the scroller they live in cover the terminal's controls again, exactly as before the fix —
  // the blocker is the languette itself (frozen decision), not the wrapper/scroller split, which the
  // two assertions above prove is intact here too. Freezing the labels is what makes a CHANGE of
  // this state — someone making the case traversable, or breaking it further — turn the suite red
  // instead of passing unnoticed. Probed BEFORE the wheel below, so the stack is still at
  // `scrollTop === 0` and the coordinates mean what they say.
  expect(bandProbes(host, tab, button)).toEqual({
    scrollbarGutter: "overlay-languette",
    bandMiddle: "overlay-languette",
    scrollToBottom: "overlay-languette",
    languette: "overlay-languette",
  });

  // (a) A wheel over a languette advances the column's scrollTop. `userEvent.wheel` drives the REAL
  // Chromium mouse wheel (a synthetic `WheelEvent` would never scroll anything), so this is the
  // user's gesture, hit-testing included.
  expect(scroller.scrollTop).toBe(0);
  await userEvent.wheel(tab, { delta: { y: 200 } });
  const scrolled = await waitFor(() => scroller.scrollTop > 0);
  expect(scrolled, "a wheel over a languette scrolls the stack").toBe(true);

  // (b) The scrollbar STRIP is hittable. The scroller is flush to the pane's right edge and this
  // Chromium composites its bar as an OVERLAY scrollbar (it steals no layout width — `offsetWidth
  // === clientWidth`), so the bar is painted over the outermost 0.5rem of the band itself. Probing
  // `pane.right - 4` inside the stack's vertical extent must therefore reach the overlay COLUMN —
  // the box whose `pointer-events` decides whether the thumb is draggable at all. The y is taken in
  // a `gap-2` between two stacked languettes so the point tests the COLUMN's own surface rather than
  // a tab's; under `pointer-events: none` it reads `pane-underlay` and the thumb is dead.
  const pane = host.getBoundingClientRect();
  const strip = scroller.getBoundingClientRect();
  expect(Math.round(strip.right)).toBe(Math.round(pane.right));
  const tabRects = Array.from(host.querySelectorAll("[data-overlay-tab]"), (t) =>
    t.getBoundingClientRect(),
  );
  const gapY = tabRects
    .slice(1)
    .map((r, i) => (tabRects[i].bottom + r.top) / 2)
    .find((y) => y > strip.top + 1 && y < strip.bottom - 1);
  expect(gapY, "a gap between two stacked languettes is visible inside the strip").toBeDefined();
  expect(hitLabel(pane.right - SCROLLBAR_W / 2, gapY as number)).toBe("overlay-languette-column");
});
