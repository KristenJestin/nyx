import { useState, type ReactNode } from "react";
import { fireEvent, render } from "@testing-library/react";
import { page, userEvent } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

// The REAL Tailwind utilities (the vitest config compiles them for a spec that imports
// `globals.css`). LOAD-BEARING here: the pane slot and `<Terminal>`'s container are sized by
// `h-full` / `p-2.5`, so without this stylesheet the terminal's box is its own CONTENT height and
// every geometry assertion below would measure the test's markup instead of the app's layout.
import "@/globals.css";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";
import { __resetTerminalStore, getTerminal } from "@/components/terminal/terminal-store";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import type { Spec } from "@/components/specs/types";
import { SpecOverlay } from "@/components/terminal-spec-overlay/spec-overlay";
import { TerminalSpecsProvider } from "@/components/terminal-spec-overlay/terminal-specs-provider";
import { PaneTreeHost } from "./pane-tree-host";
import {
  canSplitPane,
  CONTROL_BUTTON_PX,
  CONTROL_GAP_PX,
  CONTROL_INSET_PX,
  MIN_PANE_HEIGHT_PX,
  MIN_PANE_WIDTH_PX,
  paneControlsWidthPx,
  SEPARATOR_THICKNESS_PX,
  type HostSize,
} from "@/components/panes/pane-geometry";
import {
  closePane,
  createPaneLayout,
  paneLeaves,
  setSplitRatio,
  splitPane,
  type PaneNode,
} from "@/components/panes/pane-tree";

/**
 * BROWSER-MODE tiling test (real Chromium via Playwright, headless) — the half jsdom cannot
 * assert: jsdom has NO layout engine, so every box is 0×0 there and "the panes really are side by
 * side, each with a real box" can only be proven here.
 *
 * It mounts FOUR live terminals in a nested tiling and checks that each pane has a non-empty box,
 * that the boxes do not overlap and tile the host, and that each terminal's persistent xterm node
 * is attached exactly once — the geometric contract the PTY resize pipeline then rides on.
 */

/**
 * THE BACKEND DOUBLE, in browser mode: `@/bridge/test-harness`'s `vi.mock("@/bridge")` does NOT
 * apply here — vitest only hoists `vi.mock` written IN the test file — so `mockIPC` would install a
 * handler on a fake nothing imports. It is not needed either: without the Electron preload's
 * `window.nyxCore`, `@/bridge` already resolves to a live {@link FakeNyxBridge}, so the double IS
 * `nyxBridge` and we drive it directly.
 */
const backend = nyxBridge as unknown as FakeNyxBridge;

/** A PTY grid — what `pty_spawn` / `pty_resize` carry. */
interface PtySize {
  cols: number;
  rows: number;
}

/** The SIZE each terminal's PTY was spawned at — the first `pty_spawn`, keyed by terminal id. */
const spawnSizes = new Map<string, PtySize>();
/** How many `pty_spawn` calls each terminal id got — a DOUBLE spawn is the tiling's cardinal sin. */
const spawnCounts = new Map<string, number>();
/** Which terminal owns each live PTY id, so a `pty_resize` can be attributed back to its pane. */
const ptyOwners = new Map<number, string>();
/** The LAST `pty_resize` each terminal received — the size its shell is really running at. */
const resizes = new Map<string, PtySize>();

beforeEach(async () => {
  // The viewport must CONTAIN the 1200×700 host: `getBoundingClientRect` does not care, but
  // `document.elementFromPoint` — the review's own instrument — returns `null` outside it, which
  // would read as "nothing there" instead of measuring what a click really reaches.
  await page.viewport(1400, 900);
  spawnSizes.clear();
  spawnCounts.clear();
  ptyOwners.clear();
  resizes.clear();
  backend.reset();
  let nextPtyId = 100;
  backend.onAnyCommand((cmd, args) => {
    if (cmd === "pty_spawn") {
      // Keep the FIRST spawn per terminal: the size the shell really starts at (a later
      // `pty_resize` cannot un-print a prompt already wrapped at the wrong width).
      const terminalId = typeof args?.terminalId === "string" ? args.terminalId : "";
      if (terminalId !== "" && !spawnSizes.has(terminalId)) {
        spawnSizes.set(terminalId, { cols: Number(args?.cols), rows: Number(args?.rows) });
      }
      spawnCounts.set(terminalId, (spawnCounts.get(terminalId) ?? 0) + 1);
      // DISTINCT ids (not one shared constant): that is what lets a later `pty_resize`, which
      // only carries the PTY id, be attributed back to the terminal — and hence to its pane.
      const ptyId = nextPtyId++;
      ptyOwners.set(ptyId, terminalId);
      return ptyId;
    }
    if (cmd === "pty_resize") {
      const owner = ptyOwners.get(Number(args?.id));
      if (owner != null) resizes.set(owner, { cols: Number(args?.cols), rows: Number(args?.rows) });
      return null;
    }
    if (cmd === "list_terminals") return [];
    if (cmd === "terminal_info") return { cwd: null, foreground: null };
    return null;
  });
});

afterEach(() => {
  __resetTerminalStore();
  document.body.replaceChildren();
});

function record(id: string): TerminalRecord {
  return {
    id,
    cwd: `/repo/${id}`,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  };
}

function mountHost(width = 1200, height = 700): HTMLElement {
  const host = document.createElement("div");
  host.style.width = `${width}px`;
  host.style.height = `${height}px`;
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  document.body.appendChild(host);
  return host;
}

async function waitFor(pred: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 50));
  }
  return pred();
}

/** `t1 | (t2 / t3)` plus a fourth pane under t1 — a nested four-pane tiling. */
function fourPanes(): PaneNode {
  let root: PaneNode = createPaneLayout("t1", "p1");
  root = splitPane(root, {
    paneId: "p1",
    direction: "row",
    terminalId: "t2",
    newPaneId: "p2",
  }) as PaneNode;
  root = splitPane(root, {
    paneId: "p2",
    direction: "column",
    terminalId: "t3",
    newPaneId: "p3",
  }) as PaneNode;
  root = splitPane(root, {
    paneId: "p1",
    direction: "column",
    terminalId: "t4",
    newPaneId: "p4",
  }) as PaneNode;
  return root;
}

/** The size {@link mountHost} gives every spec here — the yardstick of the absolute pane floor. */
const HOST: HostSize = { width: 1200, height: 700 };

/**
 * A SEVEN-PANE tiling — the size of the user's report — built ONLY through splits the absolute
 * floor allows ({@link canSplitPane} is asserted before each one). Four columns of 300px, three of
 * them halved vertically: every pane lands at 300×350 or 300×700, comfortably over 180×120.
 */
function sevenPanes(): PaneNode {
  let root: PaneNode = createPaneLayout("t1", "p1");
  let next = 1;
  const cut = (paneId: string, direction: "row" | "column") => {
    expect(canSplitPane(root, paneId, direction, HOST), `${paneId} ${direction} is allowed`).toBe(
      true,
    );
    next += 1;
    root = splitPane(root, {
      paneId,
      direction,
      terminalId: `t${next}`,
      newPaneId: `p${next}`,
    }) as PaneNode;
  };
  cut("p1", "row"); // p1 | p2            → 600 / 600
  cut("p1", "row"); // (p1 | p3) | p2     → 300 / 300 / 600
  cut("p2", "row"); // … | (p2 | p4)      → four 300px columns
  cut("p1", "column"); // p1 / p5
  cut("p3", "column"); // p3 / p6
  cut("p2", "column"); // p2 / p7
  return root;
}

/** Every VISIBLE pane's measured box, keyed by terminal id — the real Chromium reading. */
function paneBoxes(host: HTMLElement): Map<string, DOMRect> {
  const out = new Map<string, DOMRect>();
  for (const pane of host.querySelectorAll<HTMLElement>(
    '[data-terminal-id][data-visible="true"]',
  )) {
    out.set(pane.dataset.terminalId as string, pane.getBoundingClientRect());
  }
  return out;
}

/**
 * A tiling whose panes really CLOSE — `onClosePane` runs the real `closePane` transition, so
 * "the button did something" is n → n-1 panes, not a spy call.
 */
function LiveTiling({ terminals, initial }: { terminals: TerminalRecord[]; initial: PaneNode }) {
  const [layout, setLayout] = useState<PaneNode>(initial);
  const [focused, setFocused] = useState<string>((paneLeaves(initial)[0] as { id: string }).id);
  return (
    <PaneTreeHost
      terminals={terminals}
      layout={layout}
      focusedPaneId={focused}
      onFocusPane={setFocused}
      onSplitPane={() => {}}
      onClosePane={(paneId) => setLayout((current) => closePane(current, paneId))}
      onResizeSplit={(splitId, ratio) =>
        setLayout((current) => setSplitRatio(current, splitId, ratio))
      }
    />
  );
}

/**
 * The host driven by REAL layout state — what the page does: a separator hands back a ratio and
 * `setSplitRatio` produces the next tree. Both phases are applied the same way here; which of them
 * is PERSISTED is `use-pane-layout`'s business (asserted in its own spec), not the host's.
 */
function Tiling({
  terminals,
  initial,
  focusedPaneId = "p3",
  renderPaneOverlay,
}: {
  terminals: TerminalRecord[];
  initial: PaneNode;
  /** Which pane holds the focus — only matters to the specs that mount a focused overlay. */
  focusedPaneId?: string;
  renderPaneOverlay?: (terminalId: string) => ReactNode;
}) {
  const [layout, setLayout] = useState<PaneNode>(initial);
  return (
    <PaneTreeHost
      terminals={terminals}
      layout={layout}
      focusedPaneId={focusedPaneId}
      onFocusPane={() => {}}
      onSplitPane={() => {}}
      onClosePane={() => {}}
      onResizeSplit={(splitId, ratio) =>
        setLayout((current) => setSplitRatio(current, splitId, ratio))
      }
      renderPaneOverlay={renderPaneOverlay}
    />
  );
}

/** A PointerEvent as a real device would deliver it (the fields the handlers read). */
function pointerEvent(type: string, x: number, y: number, buttons: number): PointerEvent {
  return new PointerEvent(type, {
    pointerId: 1,
    pointerType: "mouse",
    isPrimary: true,
    button: 0,
    buttons,
    clientX: x,
    clientY: y,
    bubbles: true,
    cancelable: true,
  });
}

/** The centre of an element, in client coordinates. */
function centreOf(element: HTMLElement): { x: number; y: number } {
  const box = element.getBoundingClientRect();
  return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
}

/** The live grid of each terminal that has one, keyed by record id. */
function liveGrids(ids: readonly string[]): Map<string, PtySize> {
  const out = new Map<string, PtySize>();
  for (const id of ids) {
    const term = getTerminal(id)?.terminal;
    if (term) out.set(id, { cols: term.cols, rows: term.rows });
  }
  return out;
}

it("spawns each tiled pane's PTY at the PANE's own size, never the 80×24 fallback", async () => {
  const host = mountHost();
  // t1..t4 are tiled; t5 stays off-screen (`display:none`) — its box can never be measured, so it
  // is the control case that still spawns at the fallback.
  const terminals = [record("t1"), record("t2"), record("t3"), record("t4"), record("t5")];

  render(
    <PaneTreeHost
      terminals={terminals}
      layout={fourPanes()}
      focusedPaneId="p3"
      onFocusPane={() => {}}
      onSplitPane={() => {}}
      onClosePane={() => {}}
    />,
    { container: host },
  );

  const spawned = await waitFor(() => spawnSizes.size === 5);
  expect(spawned, `every terminal spawned (saw ${spawnSizes.size})`).toBe(true);

  for (const terminalId of ["t1", "t2", "t3", "t4"]) {
    const pane = host.querySelector<HTMLElement>(
      `[data-terminal-id="${terminalId}"]`,
    ) as HTMLElement;
    const box = pane.getBoundingClientRect();
    const size = spawnSizes.get(terminalId) as { cols: number; rows: number };

    // NOT the fallback: a pane created by a split must never start its shell at 80×24 — a resumed
    // agent would otherwise print its first turn wrapped at the wrong width, frozen in the
    // scrollback forever.
    //
    // WHAT THIS SPEC PINS is that OUTCOME — a real box, one grid per pane, never 80×24 — and only
    // it. NOT the mechanism: the macrotask WAIT `awaitMeasuredBox` performs is pinned, with its clock
    // injected, in `../terminal/terminal-store.test.ts` ("awaitMeasuredBox (a split pane never
    // spawns at the 80×24 fallback)"). Measured (review round 3): making `awaitMeasuredBox` return
    // `false` at once leaves this spec GREEN, because real Chromium has already laid the new pane
    // out by the time the spawn reads it. Chromium is what makes the outcome provable here; it is
    // also what makes the wait unobservable here.
    expect(`${size.cols}×${size.rows}`, `${terminalId} spawned at the fallback`).not.toBe("80×24");
    // A 600×350 quarter-pane holds FEWER columns and rows than the 80×24 default, so a grid that
    // matched the fallback in either axis could not have come from this box.
    expect(size.cols).toBeLessThan(80);
    expect(size.rows).toBeLessThan(24);

    // It IS this pane's grid: the DOM renderer laid out exactly `rows` rows, and the painted
    // screen fits inside the pane box while covering it up to the container padding
    // (`p-2.5` = 10px a side), xterm's scrollbar and a partial trailing cell.
    const rowEls = pane.querySelectorAll<HTMLElement>(".xterm-rows > div");
    expect(rowEls.length, `${terminalId} renders its spawned row count`).toBe(size.rows);
    const screen = (
      pane.querySelector<HTMLElement>(".xterm-screen") as HTMLElement
    ).getBoundingClientRect();
    const cellWidth = screen.width / size.cols;
    const cellHeight = screen.height / size.rows;
    expect(cellWidth, `${terminalId} has a measured cell`).toBeGreaterThan(0);
    expect(screen.width, `${terminalId} grid fits its pane`).toBeLessThanOrEqual(box.width);
    expect(screen.height, `${terminalId} grid fits its pane`).toBeLessThanOrEqual(box.height);
    expect(box.width - screen.width, `${terminalId} grid spans its pane`).toBeLessThanOrEqual(
      20 + 20 + cellWidth,
    );
    expect(box.height - screen.height, `${terminalId} grid spans its pane`).toBeLessThanOrEqual(
      20 + cellHeight,
    );

    // …and the live terminal still shows that very grid: the spawn size needed no correction.
    const live = getTerminal(terminalId)?.terminal;
    expect(live?.cols).toBe(size.cols);
    expect(live?.rows).toBe(size.rows);
  }

  // THE CONTROL: t5 is outside the tiling (`display:none`), so no box can ever be measured for it
  // and it keeps the sane 80×24 fallback — the very value the tiled panes must not have.
  expect(spawnSizes.get("t5")).toEqual({ cols: 80, rows: 24 });
});

it("lays out four LIVE terminals side by side, each with a real box and one xterm node", async () => {
  const host = mountHost();
  const terminals = [record("t1"), record("t2"), record("t3"), record("t4"), record("t5")];

  render(
    <PaneTreeHost
      terminals={terminals}
      layout={fourPanes()}
      focusedPaneId="p3"
      onFocusPane={() => {}}
      onSplitPane={() => {}}
      onClosePane={() => {}}
    />,
    { container: host },
  );

  const mounted = await waitFor(() => host.querySelectorAll("[data-terminal-id]").length === 5);
  expect(mounted, "every project terminal keeps a mounted slot").toBe(true);

  const visible = Array.from(
    host.querySelectorAll<HTMLElement>('[data-terminal-id][data-visible="true"]'),
  );
  expect(visible.length, "four panes are tiled").toBe(4);

  // Each visible pane has a REAL box and holds exactly one live xterm node.
  const rects = visible.map((pane) => {
    const box = pane.getBoundingClientRect();
    expect(box.width, `${pane.dataset.terminalId} has width`).toBeGreaterThan(100);
    expect(box.height, `${pane.dataset.terminalId} has height`).toBeGreaterThan(50);
    expect(pane.querySelectorAll("[data-terminal-node]").length).toBe(1);
    expect(pane.querySelectorAll(".xterm").length).toBe(1);
    return box;
  });

  // The panes TILE the host: no overlap, and their areas add up to the whole surface.
  for (let i = 0; i < rects.length; i++) {
    for (let j = i + 1; j < rects.length; j++) {
      const a = rects[i] as DOMRect;
      const b = rects[j] as DOMRect;
      const overlaps = a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
      expect(overlaps, "two panes must not overlap").toBe(false);
    }
  }
  const area = rects.reduce((sum, r) => sum + r.width * r.height, 0);
  expect(area).toBeGreaterThan(1200 * 700 * 0.98);

  // Exactly one pane is FOCUSED; a terminal outside the tiling stays mounted with no box.
  const focused = host.querySelectorAll('[data-focused="true"]');
  expect(focused.length).toBe(1);
  expect((focused[0] as HTMLElement).dataset.terminalId).toBe("t3");
  const offscreen = host.querySelector('[data-terminal-id="t5"]') as HTMLElement;
  expect(getComputedStyle(offscreen).display).toBe("none");
  expect(offscreen.querySelectorAll("[data-terminal-node]").length).toBe(1);
});

/**
 * Whether a COMPUTED colour paints anything — i.e. its alpha is not zero. Chromium resolves
 * Tailwind's `/50` opacity modifier (a `color-mix(… , transparent)`) to whatever colour space the
 * mix used, so the string can be `rgba(r, g, b, a)` OR `oklch(l c h / a)`: both forms are read
 * here, and a form with no explicit alpha is opaque by definition.
 */
function isPainted(color: string): boolean {
  if (color === "" || color === "transparent") return false;
  const rgba = /^rgba?\(([^)]+)\)$/.exec(color);
  if (rgba) {
    const parts = (rgba[1] as string).split(/[\s,/]+/).filter(Boolean);
    return parts.length < 4 || Number(parts[3]) > 0;
  }
  const slashed = /\/\s*([\d.]+)(%?)\s*\)$/.exec(color);
  if (slashed) return Number(slashed[1]) > 0;
  return true;
}

/* ─────────── THE RESTING SEAM: MEASURED, NOT ASSUMED (finding F3) ───────────
 *
 * The review measured the BAND (8×600, `rgba(0, 0, 0, 0)`) and read "invisible". The band's
 * transparency is deliberate; what carries the boundary is the `::before` HAIRLINE. So the
 * question — is the trait NOT PAINTED (a bug) or PAINTED BUT TOO FAINT (a contrast problem)? —
 * can only be settled by measuring the PSEUDO-ELEMENT, in a real engine, in the app's real theme.
 */

/** An sRGB sample, 0..255 per channel, alpha 0..1. */
interface Sample {
  r: number;
  g: number;
  b: number;
  a: number;
}

/**
 * A CSS colour resolved to sRGB by the ENGINE ITSELF, optionally composited over an opaque
 * `under`. Chromium hands `getComputedStyle` back Tailwind's `/50` modifier as an `oklch(… / a)`
 * string, so no regex can read it — a 1×1 canvas both parses and composites it exactly the way
 * the compositor does, which is the whole point of measuring rather than computing.
 */
function sample(color: string, under?: string): Sample {
  const canvas = document.createElement("canvas");
  canvas.width = 1;
  canvas.height = 1;
  const ctx = canvas.getContext("2d") as CanvasRenderingContext2D;
  ctx.clearRect(0, 0, 1, 1);
  if (under != null) {
    ctx.fillStyle = under;
    ctx.fillRect(0, 0, 1, 1);
  }
  ctx.fillStyle = color;
  ctx.fillRect(0, 0, 1, 1);
  const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
  return { r: r ?? 0, g: g ?? 0, b: b ?? 0, a: (a ?? 0) / 255 };
}

/** WCAG relative luminance of an OPAQUE sRGB sample. */
function luminance(c: Sample): number {
  const channel = (v: number) => {
    const s = v / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b);
}

/** WCAG contrast ratio between two opaque samples (1 … 21). */
function contrast(a: Sample, b: Sample): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * `read()` once it has STOPPED MOVING. The band carries `transition-colors`, so a computed colour
 * sampled the instant a state flips is still the PREVIOUS one interpolating towards the new — and
 * comparing two mid-transition readings would silently compare the same value with itself.
 */
async function settled(read: () => string, timeoutMs = 2000): Promise<string> {
  let previous = read();
  let stable = 0;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline && stable < 3) {
    await new Promise((r) => setTimeout(r, 40));
    const now = read();
    stable = now === previous ? stable + 1 : 0;
    previous = now;
  }
  return previous;
}

it("SEAM: the separator is VISIBLE at rest — no hover, no focus — while keeping its 8px band", async () => {
  // jsdom cannot answer this one: it paints nothing and knows no pseudo-elements, so "a user can
  // SEE where the panes meet before touching anything" is only provable with a real engine.
  const host = mountHost();
  const ids = ["t1", "t2", "t3", "t4"];
  render(<Tiling terminals={ids.map(record)} initial={fourPanes()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === 4), "every pane spawned").toBe(true);

  for (const direction of ["row", "column"] as const) {
    const separator = host.querySelector<HTMLElement>(
      `[data-direction="${direction}"]`,
    ) as HTMLElement;
    expect(separator, `the ${direction} split has a separator`).not.toBeNull();

    // NOTHING has been hovered, focused or dragged: the band itself is still fully transparent…
    const band = getComputedStyle(separator);
    expect(isPainted(band.backgroundColor), `${direction} band stays transparent at rest`).toBe(
      false,
    );

    // …yet the seam IS painted: a hairline with a real, non-transparent colour and a real box.
    const trait = getComputedStyle(separator, "::before");
    expect(trait.content, `${direction} seam is rendered`).not.toBe("none");
    expect(isPainted(trait.backgroundColor), `${direction} seam is painted at rest`).toBe(true);
    const traitSize = parseFloat(direction === "row" ? trait.width : trait.height);
    expect(traitSize, `${direction} seam has a box`).toBeGreaterThan(0);

    // THIN PAINT, FULL TARGET: the visible trait is a fraction of the band, and the band still
    // measures SEPARATOR_THICKNESS_PX on its own axis — the geometry the drag test rides on.
    expect(traitSize, `${direction} seam is thinner than the band`).toBeLessThan(
      SEPARATOR_THICKNESS_PX,
    );
    const box = separator.getBoundingClientRect();
    expect(
      direction === "row" ? box.width : box.height,
      `${direction} grab band keeps its ${SEPARATOR_THICKNESS_PX}px`,
    ).toBeCloseTo(SEPARATOR_THICKNESS_PX, 1);
  }
});

it("F3: the resting hairline is PAINTED and READS against the terminals' own background", async () => {
  // THE APP'S REAL THEME. `index.html` ships `<html class="dark">`; vitest's own page does not, so
  // without this the measurement would be taken on the light palette the user never sees.
  document.documentElement.classList.add("dark");
  try {
    const host = mountHost();
    // ONE split, hence ONE seam: a nested tiling would put a crossing band over this one's centre,
    // and the real mouse move below would land on THAT instead — measuring the wrong element is
    // precisely the mistake this finding exists to avoid.
    const two = splitPane(createPaneLayout("t1", "p1"), {
      paneId: "p1",
      direction: "row",
      terminalId: "t2",
      newPaneId: "p2",
    }) as PaneNode;
    render(<Tiling terminals={["t1", "t2"].map(record)} initial={two} />, { container: host });
    expect(await waitFor(() => spawnSizes.size === 2), "every pane spawned").toBe(true);

    const separator = host.querySelector<HTMLElement>('[data-direction="row"]') as HTMLElement;
    const pane = host.querySelector<HTMLElement>('[data-terminal-id="t1"]') as HTMLElement;
    // The surface the seam is read AGAINST: the terminal's own painted background (xterm derives
    // it from the same design-system palette — `resolveThemeFromCss`), falling back to the page.
    const screen = pane.querySelector<HTMLElement>(".xterm-screen") ?? pane;
    const screenBg = getComputedStyle(screen).backgroundColor;
    const backdropColor =
      sample(screenBg).a === 1 ? screenBg : getComputedStyle(document.body).backgroundColor;
    const backdrop = sample(backdropColor);

    // (1) IS IT PAINTED? — the question the review left open, asked of the PSEUDO-ELEMENT.
    // `getComputedStyle` hands back a LIVE declaration, so every reading below is SNAPSHOT as a
    // string the moment it is taken: re-reading it after a hover would silently report the hover.
    const trait = getComputedStyle(separator, "::before");
    const restTrait = trait.backgroundColor;
    const restBand = getComputedStyle(separator).backgroundColor;
    expect(trait.content, "the hairline is rendered at all").not.toBe("none");
    expect(trait.position, "the hairline is positioned inside the band").toBe("absolute");
    expect(parseFloat(trait.width), "the hairline has a real width").toBeGreaterThan(0);
    expect(parseFloat(trait.height), "the hairline has a real height").toBeGreaterThan(0);
    expect(Number(trait.opacity), "the hairline is not faded out").toBe(1);
    expect(sample(restTrait).a, "the hairline is NOT transparent").toBeGreaterThan(0);

    // (2) DOES IT READ? — the composited hairline against the terminal's real background must
    // clear the WCAG 1.4.11 non-text floor (3:1), or "painted" is a technicality the eye misses.
    const ratio = contrast(sample(restTrait, backdropColor), backdrop);
    expect(
      ratio,
      `resting hairline ${restTrait} over ${backdropColor} ⇒ ${ratio.toFixed(2)}:1`,
    ).toBeGreaterThanOrEqual(3);
    // The BAND is still transparent at rest — the boundary is carried by the trait alone, which
    // is what the review measured on the band and read as "nothing there".
    expect(restBand).toBe("rgba(0, 0, 0, 0)");

    // The four values are DECLARED in one place — read them from the resting class list, before
    // the drag swaps the branch, so the progression can be stated as numbers.
    const restingClasses = separator.className;
    /** The hairline's declared strength for one variant (`""` = at rest), `/100` when unmodified. */
    const declared = (variant: string) => {
      const match = new RegExp(`(?:^|\\s)${variant}before:bg-ring(/\\d+)?(?:\\s|$)`).exec(
        restingClasses,
      );
      return match == null ? null : (match[1] ?? "/100");
    };
    /** The BAND's declared strength for one variant — `null` when it is transparent. */
    const declaredBand = (variant: string) => {
      const match = new RegExp(`(?:^|\\s)${variant}bg-ring(/\\d+)?(?:\\s|$)`).exec(restingClasses);
      return match == null ? null : (match[1] ?? "/100");
    };

    // (3) IT STRENGTHENS UNDER THE POINTER — measured, with a real mouse move, not inferred from
    // a class name: the hairline gains contrast AND the band stops being transparent.
    await userEvent.hover(separator);
    const hoveredBand = await settled(() => getComputedStyle(separator).backgroundColor);
    const hoveredTrait = getComputedStyle(separator, "::before").backgroundColor;
    expect(isPainted(hoveredBand), "the hovered band is painted").toBe(true);
    const hoveredRatio = contrast(sample(hoveredTrait, backdropColor), backdrop);
    expect(hoveredRatio, `hover ${hoveredTrait} ⇒ ${hoveredRatio.toFixed(2)}:1`).toBeGreaterThan(
      ratio,
    );

    // (4) …and DRAGGING is a state of its own again, stronger still on the band.
    const grip = centreOf(separator);
    fireEvent(separator, pointerEvent("pointerdown", grip.x, grip.y, 1));
    const draggingBand = await settled(() => getComputedStyle(separator).backgroundColor);
    const draggingTrait = getComputedStyle(separator, "::before").backgroundColor;
    expect(isPainted(draggingBand), "the dragging band is painted").toBe(true);
    expect(
      contrast(sample(draggingBand, backdropColor), backdrop),
      `dragging band ${draggingBand} must beat the hovered ${hoveredBand}`,
    ).toBeGreaterThan(contrast(sample(hoveredBand, backdropColor), backdrop));
    fireEvent(separator, pointerEvent("pointerup", grip.x, grip.y, 0));

    // FOUR DISTINCT (band, hairline) PAIRS — the progression the arbitration keeps. Three of them
    // are MEASURED above; focus-visible (which a synthetic focus cannot reliably match) is read
    // from the declaration that produces it. Compared as VALUES, with no state label mixed in, so
    // two identical states could not pass as different.
    const measured = [
      `${restBand} + ${restTrait}`,
      `${hoveredBand} + ${hoveredTrait}`,
      `${draggingBand} + ${draggingTrait}`,
    ];
    expect(new Set(measured).size, `three measured states — ${measured.join(" | ")}`).toBe(3);
    // MEASURED (dark theme, over oklch(0.145 0 0)):
    //   rest  rgba(0,0,0,0)            + oklab(0.556 0 0 / 0.85) ⇒ 3.30:1
    //   hover oklab(0.556 0 0 / 0.4)   + oklab(0.556 0 0 / 0.95) ⇒ 3.83:1
    //   drag  oklab(0.556 0 0 / 0.7)   + oklch(0.556 0 0)        ⇒ 4.18:1
    expect(declared(""), "the resting hairline is the strengthened value").toBe("/85");
    expect(declared("hover:"), "hover strengthens it further").toBe("/95");
    expect(declared("focus-visible:"), "focus-visible reaches the full token").toBe("/100");
    // …and focus-visible's own pair (`ring/60` band + full hairline) is none of the other three.
    expect(declaredBand("focus-visible:"), "focus-visible has its own band").toBe("/60");
    expect(declaredBand(""), "the resting band is transparent").toBeNull();
    expect(declaredBand("hover:"), "the hovered band is weaker than focus-visible's").toBe("/40");

    // (5) NO RAW COLOUR: every colour utility on the band names the `ring` design-system token.
    for (const token of restingClasses.split(/\s+/).filter((c) => /bg-/.test(c))) {
      expect(token, `${token} must be a design-system token`).toMatch(
        /(bg-ring(\/\d+)?|bg-transparent)$/,
      );
    }
  } finally {
    document.documentElement.classList.remove("dark");
  }
});

it("DRAGGING a separator re-sizes the panes and every visible PTY follows, coherently", async () => {
  const host = mountHost();
  const ids = ["t1", "t2", "t3", "t4"];
  render(<Tiling terminals={ids.map(record)} initial={fourPanes()} />, { container: host });

  expect(await waitFor(() => spawnSizes.size === 4), "every pane spawned").toBe(true);
  // Settle: each terminal has reached the grid its INITIAL box dictates.
  expect(
    await waitFor(() => ids.every((id) => (getTerminal(id)?.terminal.cols ?? 0) > 0)),
    "every pane has a measured grid",
  ).toBe(true);
  const before = liveGrids(ids);

  // The OUTERMOST separator: the vertical seam between the left column (t1 / t4) and the right
  // one (t2 / t3), at x = 600 of the 1200px host.
  const separator = host.querySelector<HTMLElement>('[data-direction="row"]') as HTMLElement;
  expect(separator, "the row split has a separator").not.toBeNull();
  const grip = centreOf(separator);
  expect(grip.x).toBeCloseTo(600, 0);

  fireEvent(separator, pointerEvent("pointerdown", grip.x, grip.y, 1));
  // WHILE the drag is live the host refuses text selection, so sweeping the pointer across four
  // terminals cannot leave any of them highlighted.
  expect(getComputedStyle(host.querySelector("[data-pane-host]") as HTMLElement).userSelect).toBe(
    "none",
  );
  fireEvent(separator, pointerEvent("pointermove", 800, grip.y, 1));
  fireEvent(separator, pointerEvent("pointerup", 800, grip.y, 0));

  // The panes really moved: the left column now owns two thirds of the width.
  const paneBox = (id: string) =>
    (host.querySelector(`[data-terminal-id="${id}"]`) as HTMLElement).getBoundingClientRect();
  expect(paneBox("t1").width).toBeCloseTo(800, 0);
  expect(paneBox("t2").width).toBeCloseTo(400, 0);

  // …and every visible terminal was RE-FITTED and pushed its new grid to its own PTY. The
  // ResizeObserver → FitAddon → `pty_resize` chain is asynchronous, so wait for it to settle.
  const settled = await waitFor(() =>
    ids.every((id) => {
      const term = getTerminal(id)?.terminal;
      const sent = resizes.get(id);
      if (term == null || sent == null) return false;
      // The grid must have MOVED (a stale value would make the equality below vacuously true)…
      if (term.cols === (before.get(id) as PtySize).cols) return false;
      // …and the PTY must have been told about exactly that grid.
      return sent.cols === term.cols && sent.rows === term.rows;
    }),
  );
  expect(settled, "every visible PTY was resized to its terminal's live grid").toBe(true);

  const after = liveGrids(ids);
  for (const id of ids) {
    const box = paneBox(id);
    const grid = after.get(id) as PtySize;
    const screen = (
      host.querySelector(`[data-terminal-id="${id}"] .xterm-screen`) as HTMLElement
    ).getBoundingClientRect();
    // COHERENT means: the grid the PTY was told about is the grid xterm painted, and that grid
    // fills this pane's NEW box (up to the container padding, the scrollbar and a partial cell).
    expect(
      host.querySelectorAll(`[data-terminal-id="${id}"] .xterm-rows > div`).length,
      `${id} paints its resized row count`,
    ).toBe(grid.rows);
    expect(screen.width, `${id} grid fits its resized pane`).toBeLessThanOrEqual(box.width);
    expect(screen.height, `${id} grid fits its resized pane`).toBeLessThanOrEqual(box.height);
    expect(box.width - screen.width, `${id} grid spans its resized pane`).toBeLessThanOrEqual(
      20 + 20 + screen.width / grid.cols,
    );
    expect(box.height - screen.height, `${id} grid spans its resized pane`).toBeLessThanOrEqual(
      20 + screen.height / grid.rows,
    );
    // No pane was re-created on the way: one instance, one PTY, one DOM attachment throughout.
    expect(spawnCounts.get(id), `${id} kept its single PTY across the resize`).toBe(1);
    expect(host.querySelectorAll(`[data-terminal-node="${id}"]`).length).toBe(1);
    // THIS DOES NOT PROVE THE MECHANISM, AND THE COMMENT THAT SAID SO WAS WRONG. The gesture
    // above is played with SYNTHETIC `PointerEvent`s (see `pointerEvent`: constructed and
    // dispatched, hence `isTrusted: false`), and a UA opens a native text selection only from a
    // TRUSTED press. On top of that the separator takes pointer capture and calls
    // `stopPropagation`, so xterm's own `mousedown` listener never sees the sweep at all. This
    // reading therefore answers "nothing is selected" WHETHER OR NOT `preventDefault()` is called:
    // it cannot go red on the mechanism it is placed next to, so it is not evidence for it.
    //
    // WHAT IT IS STILL WORTH: a guard against something selecting ANYWAY — a handler that calls
    // `term.selectAll()`, a stray programmatic range — which no other probe here would catch. Kept
    // for that, and for that only.
    //
    // THE MECHANISM IS PINNED IN JSDOM, where the press's outcome IS observable: the DRAG test in
    // `pane-tree-host.test.tsx` asserts that `fireEvent.pointerDown(separator, …)` returns
    // `false`, which happens exactly when `preventDefault()` ran on a cancelable event. Deleting
    // that call from the separator's `onPointerDown` turns that test red — verified by mutation.
    // Making the line below capable of red would take a TRUSTED drag (`userEvent` / CDP input)
    // instead of a dispatched event; `userSelect: none`, asserted mid-gesture above, is the other
    // half and that one is a real reading of the live style.
    expect(getTerminal(id)?.terminal.hasSelection(), `${id} was not selected by the drag`).toBe(
      false,
    );
  }
  // SAME CAVEAT, same honesty: a synthetic pointer sequence never opens a document selection in
  // the first place, so this range is collapsed by construction and stays collapsed with or
  // without the `preventDefault`-ed press. It is a cheap guard against a stray programmatic
  // range, NOT evidence that the press cancelled anything.
  expect(window.getSelection()?.isCollapsed ?? true, "the drag left no document selection").toBe(
    true,
  );

  // The widened column gained columns, the narrowed one lost some — the resize was real, and it
  // reached the two panes on EACH side of the seam (not just the ones the pointer touched).
  for (const id of ["t1", "t4"]) {
    expect((after.get(id) as PtySize).cols, `${id} widened`).toBeGreaterThan(
      (before.get(id) as PtySize).cols,
    );
  }
  for (const id of ["t2", "t3"]) {
    expect((after.get(id) as PtySize).cols, `${id} narrowed`).toBeLessThan(
      (before.get(id) as PtySize).cols,
    );
  }
});

/** A Spec still being AUTHORED — the status that makes `<SpecOverlay>` mount the creation tiroir. */
function draftSpec(): Spec {
  return {
    id: "spec-1",
    project_id: "p1",
    workspace_id: null,
    title: "Tiling",
    slug: null,
    number: 1,
    priority: "medium",
    status: "draft",
    mode: "feature",
    phases: "[]",
    category_id: null,
    current_revision_number: 1,
    position: 0,
    created_at: 0,
    updated_at: 0,
    completed_at: null,
    archived_at: null,
  };
}

/**
 * `element`'s box once it has stopped moving — the overlay SLIDES IN from the terminal's right edge
 * (`overlaySurfaceVariants`), so measuring mid-spring would read a box the design never settles on.
 * Stable = three consecutive identical rects at full opacity.
 */
async function settledBox(element: HTMLElement): Promise<DOMRect> {
  let previous = element.getBoundingClientRect();
  let stable = 0;
  await waitFor(() => {
    const now = element.getBoundingClientRect();
    const still =
      getComputedStyle(element).opacity === "1" &&
      Math.abs(now.left - previous.left) < 0.5 &&
      Math.abs(now.right - previous.right) < 0.5;
    stable = still ? stable + 1 : 0;
    previous = now;
    return stable >= 3;
  });
  return element.getBoundingClientRect();
}

it("anchors a pane's Spec overlay INSIDE that pane's BOX, not over the main area", async () => {
  // jsdom can only prove the overlay is a DOM DESCENDANT of its pane — which stays true even if
  // the pane were not a positioning block and the `absolute` surface escaped to the main area.
  // Only a real layout engine can tell the two apart, so the claim is settled here.
  backend.on("terminal_specs", (args) =>
    args?.terminalId === "t1"
      ? {
          session: { id: "sess-1", agent_kind: "claude_code", state: "active" },
          specs: [draftSpec()],
        }
      : { session: null, specs: [] },
  );

  const host = mountHost();
  render(
    <PaneTreeHost
      terminals={["t1", "t2", "t3", "t4"].map(record)}
      layout={fourPanes()}
      // p1 is the TOP-LEFT quadrant: it touches NEITHER the host's right edge nor its bottom one,
      // so a surface anchored to the main area instead of the pane is geometrically visible.
      focusedPaneId="p1"
      onFocusPane={() => {}}
      onSplitPane={() => {}}
      onClosePane={() => {}}
      renderPaneOverlay={(terminalId) => <SpecOverlay terminalId={terminalId} />}
    />,
    { container: host },
  );

  expect(
    await waitFor(() => host.querySelector("[data-overlay-surface]") != null),
    "t1's Spec overlay mounted",
  ).toBe(true);
  // The renderer is called for all FOUR visible panes now, but only `t1` HAS a session — a pane
  // whose terminal carries no Spec renders nothing at all (the non-regression floor).
  expect(host.querySelectorAll("[data-overlay-surface]").length).toBe(1);

  const surface = host.querySelector("[data-overlay-surface]") as HTMLElement;
  const box = await settledBox(surface);
  const pane = (
    host.querySelector('[data-terminal-id="t1"]') as HTMLElement
  ).getBoundingClientRect();
  const main = host.getBoundingClientRect();

  // It is a real, visible surface…
  expect(box.width, "the overlay has a box").toBeGreaterThan(0);
  expect(box.height, "the overlay has a box").toBeGreaterThan(0);
  // …CONTAINED in the focused pane's box, edge by edge…
  expect(box.left, "overlay left inside the pane").toBeGreaterThanOrEqual(pane.left - 1);
  expect(box.top, "overlay top inside the pane").toBeGreaterThanOrEqual(pane.top - 1);
  expect(box.right, "overlay right inside the pane").toBeLessThanOrEqual(pane.right + 1);
  expect(box.bottom, "overlay bottom inside the pane").toBeLessThanOrEqual(pane.bottom + 1);

  // …and it is NOT the main area: the quarter-pane stops far short of the host on both axes, so an
  // overlay docked to the main area's right/bottom edge could not satisfy the bounds above.
  expect(pane.right, "the focused pane is not flush with the main area").toBeLessThan(
    main.right - 100,
  );
  expect(pane.bottom, "the focused pane is not flush with the main area").toBeLessThan(
    main.bottom - 100,
  );
  expect(box.right, "the overlay docks to the PANE's right edge").toBeCloseTo(pane.right, 0);
  expect(box.bottom, "the overlay ends at the PANE's bottom edge").toBeCloseTo(pane.bottom, 0);
});

/**
 * TWO PANES, TWO LANGUETTES (review finding 01M11WF11C0H8G2MG515FBT6RD).
 *
 * The surcouche used to be rendered for the FOCUSED pane only, so a terminal working a Spec went
 * mute the instant the keyboard moved elsewhere. What the user asked for is the obvious reading:
 * a Spec belongs to ITS terminal, so its languette lives in ITS pane whether that pane holds the
 * focus or not. Only a real layout engine can settle the two claims that matter here — that the two
 * surfaces coexist, and that each stays INSIDE its own rectangle rather than straddling the
 * neighbour — so they are settled in Chromium, against the app's own Tailwind.
 *
 * The mount is the PRODUCTION one: a single `<TerminalSpecsProvider>` over the host (the invariant
 * `runtime-pane-host.test.tsx` measures), and one `<SpecOverlay>` per visible pane under it.
 */
it("shows a languette on EVERY pane that has a Spec — focused or not, each in its own box", async () => {
  backend.on("terminal_specs", (args) => ({
    session: { id: `sess-${String(args?.terminalId)}`, agent_kind: "claude_code", state: "active" },
    specs: [draftSpec()],
  }));

  const host = mountHost();
  const { rerender } = render(
    <TerminalSpecsProvider terminalIds={["t1", "t2"]}>
      <Tiling
        terminals={["t1", "t2"].map(record)}
        initial={twoPanes()}
        focusedPaneId="p1"
        renderPaneOverlay={(terminalId) => <SpecOverlay terminalId={terminalId} />}
      />
    </TerminalSpecsProvider>,
    { container: host },
  );

  expect(
    await waitFor(() => host.querySelectorAll("[data-overlay-surface]").length === 2),
    "BOTH panes show their languette at the same time — this is the whole finding",
  ).toBe(true);

  // EACH ONE IS ANCHORED IN ITS OWN PANE: its `offsetParent` IS that pane's slot (the positioning
  // block), and its painted box is contained in the pane's box, edge by edge.
  const boxes = new Map<string, DOMRect>();
  for (const terminalId of ["t1", "t2"]) {
    const slot = host.querySelector(`[data-terminal-id="${terminalId}"]`) as HTMLElement;
    const surface = slot.querySelector("[data-overlay-surface]") as HTMLElement;
    expect(surface, `${terminalId} has its own overlay`).not.toBeNull();
    expect(surface.offsetParent, "the offsetParent is the PANE, never the page").toBe(slot);
    const box = await settledBox(surface);
    const pane = slot.getBoundingClientRect();
    expect(box.width, "the overlay has a real box").toBeGreaterThan(0);
    expect(box.left).toBeGreaterThanOrEqual(pane.left - 1);
    expect(box.right).toBeLessThanOrEqual(pane.right + 1);
    expect(box.top).toBeGreaterThanOrEqual(pane.top - 1);
    expect(box.bottom).toBeLessThanOrEqual(pane.bottom + 1);
    boxes.set(terminalId, box);
  }
  // …and NEITHER spills over the other: the left pane's surface stops before the right pane starts.
  const rightPane = (
    host.querySelector('[data-terminal-id="t2"]') as HTMLElement
  ).getBoundingClientRect();
  expect(
    (boxes.get("t1") as DOMRect).right,
    "t1's languette never reaches into t2's pane",
  ).toBeLessThanOrEqual(rightPane.left + 1);

  // FOCUSING THE OTHER PANE MAKES NONE OF THEM DISAPPEAR — the regression this finding is about.
  rerender(
    <TerminalSpecsProvider terminalIds={["t1", "t2"]}>
      <Tiling
        terminals={["t1", "t2"].map(record)}
        initial={twoPanes()}
        focusedPaneId="p2"
        renderPaneOverlay={(terminalId) => <SpecOverlay terminalId={terminalId} />}
      />
    </TerminalSpecsProvider>,
  );
  expect(
    (host.querySelector('[data-focused="true"]') as HTMLElement).getAttribute("data-terminal-id"),
    "the focus really moved",
  ).toBe("t2");
  expect(host.querySelectorAll("[data-overlay-surface]").length, "both languettes survive it").toBe(
    2,
  );

  // A PANE WITH NO SESSION SHOWS NOTHING — the non-regression floor, on the same mount.
  backend.on("terminal_specs", (args) =>
    args?.terminalId === "t1"
      ? {
          session: { id: "sess-1", agent_kind: "claude_code", state: "active" },
          specs: [draftSpec()],
        }
      : { session: null, specs: [] },
  );
  backend.emit("specs://changed", {});
  expect(
    await waitFor(() => host.querySelectorAll("[data-overlay-surface]").length === 1),
    "t2 lost its session and its languette with it; t1 keeps its own",
  ).toBe(true);
  expect(
    (host.querySelector("[data-overlay-surface]") as HTMLElement)
      .closest("[data-terminal-id]")
      ?.getAttribute("data-terminal-id"),
  ).toBe("t1");
});

/**
 * THE SAME CLASS AS F2, on the Spec surcouche (review finding).
 *
 * The PRD re-anchored the overlay on the FOCUSED PANE, so its right edge can now be a SEAM — and a
 * separator's `z-30` band is CENTRED on that seam, dropping 4 of its 8px INSIDE the pane, straight
 * over the `w-9` languette. At `z-20` the band won: a click in the languette's rightmost 4px (~11%
 * of the target) started a resize instead of opening the panel.
 *
 * The anchor spec above cannot catch it — it mounts `<PaneTreeHost>` WITHOUT `onResizeSplit`, so no
 * separator exists at all. This one wires both.
 */
it("F2-bis: the seam never covers the focused pane's Spec languette — z-order AND hit-test at its very edge", async () => {
  backend.on("terminal_specs", (args) =>
    args?.terminalId === "t1"
      ? {
          session: { id: "sess-1", agent_kind: "claude_code", state: "active" },
          specs: [draftSpec()],
        }
      : { session: null, specs: [] },
  );

  const host = mountHost();
  // TWO panes side by side, focus on the FIRST: its right edge IS the seam — the common tiling
  // geometry the finding describes, and the one the anchor spec cannot produce.
  render(
    <Tiling
      terminals={["t1", "t2"].map(record)}
      initial={twoPanes()}
      focusedPaneId="p1"
      renderPaneOverlay={(terminalId) => <SpecOverlay terminalId={terminalId} />}
    />,
    { container: host },
  );

  expect(
    await waitFor(() => host.querySelector("[data-overlay-tab]") != null),
    "the focused pane's Spec languette mounted",
  ).toBe(true);
  const surface = host.querySelector("[data-overlay-surface]") as HTMLElement;
  const tab = host.querySelector("[data-overlay-tab]") as HTMLElement;
  const seam = host.querySelector<HTMLElement>("[data-separator-id]") as HTMLElement;
  expect(seam, "a separator IS rendered (this spec wires onResizeSplit)").not.toBeNull();

  await settledBox(surface);
  const tabBox = tab.getBoundingClientRect();
  const seamBox = seam.getBoundingClientRect();

  // THE COLLISION IS REAL — asserted, not assumed: the band straddles the languette's right edge,
  // so the hit-tests below are exercised on genuinely contested pixels.
  expect(tabBox.width, "the languette is the w-9 strip").toBeCloseTo(36, 0);
  expect(seamBox.width, "the band is SEPARATOR_THICKNESS_PX thick").toBeCloseTo(
    SEPARATOR_THICKNESS_PX,
    0,
  );
  expect(seamBox.left, "the band bites into the languette").toBeLessThan(tabBox.right);
  expect(seamBox.right, "…and sticks out past it").toBeGreaterThan(tabBox.right);

  // (2) THE STANDING INVARIANT — the same z-order assertion the cluster carries.
  const surfaceZ = Number(getComputedStyle(surface).zIndex);
  const seamZ = Number(getComputedStyle(seam).zIndex);
  expect(Number.isFinite(surfaceZ), "the overlay surface carries a real z-index").toBe(true);
  expect(Number.isFinite(seamZ), "the separator carries a real z-index").toBe(true);
  expect(surfaceZ, `overlay z=${surfaceZ} must beat separator z=${seamZ}`).toBeGreaterThan(seamZ);
  // …and it still LOSES to the control cluster, whose F2 invariant outranks it where they meet.
  const cluster = host.querySelector("[data-pane-controls]") as HTMLElement;
  expect(Number(getComputedStyle(cluster).zIndex), "F2's cluster still wins").toBeGreaterThan(
    surfaceZ,
  );

  // (1) THE MEASUREMENT — the centre AND one pixel inside the right edge both reach the languette.
  for (const probe of [
    { x: tabBox.left + tabBox.width / 2, what: "its centre" },
    { x: tabBox.right - 1, what: "1px inside its right edge" },
  ]) {
    const hit = hitAt({ x: probe.x, y: tabBox.top + tabBox.height / 2 });
    expect(
      hit != null && (hit === tab || tab.contains(hit)),
      `a click at ${probe.what} reaches ${
        hit == null ? "nothing" : `<${hit.tagName.toLowerCase()} ${hit.className}>`
      }`,
    ).toBe(true);
    expect(
      (hit as Element).closest("[data-separator-id]"),
      `a click at ${probe.what} must never land on the seam`,
    ).toBeNull();
  }

  // (4) NON-REGRESSION — the band stays grabbable AND draggable everywhere the surcouche is not.
  const grip = { x: seamBox.left + seamBox.width / 2, y: seamBox.top + seamBox.height * 0.1 };
  expect(
    grip.y < tabBox.top || grip.y > tabBox.bottom,
    "the grip is clear of the languette (which is vertically centred)",
  ).toBe(true);
  expect(hitAt(grip), "the band itself answers there").toBe(seam);
  const widthBefore = (
    host.querySelector('[data-terminal-id="t1"]') as HTMLElement
  ).getBoundingClientRect().width;
  fireEvent(seam, pointerEvent("pointerdown", grip.x, grip.y, 1));
  fireEvent(seam, pointerEvent("pointermove", grip.x + 120, grip.y, 1));
  fireEvent(seam, pointerEvent("pointerup", grip.x + 120, grip.y, 0));
  const widthAfter = (
    host.querySelector('[data-terminal-id="t1"]') as HTMLElement
  ).getBoundingClientRect().width;
  expect(widthAfter, "the seam still drags from a point the surcouche does not cover").toBeCloseTo(
    widthBefore + 120,
    0,
  );
});

it("KEYBOARD: a focused separator steps the ratio, clamped, without touching the terminals", async () => {
  const host = mountHost();
  const ids = ["t1", "t2", "t3", "t4"];
  render(<Tiling terminals={ids.map(record)} initial={fourPanes()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === 4), "every pane spawned").toBe(true);

  const separator = host.querySelector<HTMLElement>('[data-direction="row"]') as HTMLElement;
  separator.focus();
  expect(document.activeElement, "the separator takes the keyboard").toBe(separator);

  const paneWidth = (id: string) =>
    (host.querySelector(`[data-terminal-id="${id}"]`) as HTMLElement).getBoundingClientRect().width;
  const start = paneWidth("t1");

  fireEvent.keyDown(separator, { key: "ArrowRight" });
  expect(paneWidth("t1"), "one arrow widens the first pane").toBeGreaterThan(start);

  // Hold the key against the bound: the ratio STOPS at 85%, it never walks a pane out of view.
  for (let i = 0; i < 60; i++) fireEvent.keyDown(separator, { key: "ArrowRight" });
  expect(paneWidth("t1")).toBeCloseTo(1200 * 0.85, 0);
  expect(separator.getAttribute("aria-valuenow")).toBe("85");
  // The opposite bound holds too — neither pane can be collapsed to nothing.
  for (let i = 0; i < 80; i++) fireEvent.keyDown(separator, { key: "ArrowLeft" });
  expect(paneWidth("t1")).toBeCloseTo(1200 * 0.15, 0);
  expect(separator.getAttribute("aria-valuenow")).toBe("15");

  // Not one keystroke reached a terminal: no PTY was re-spawned and none carries a selection.
  for (const id of ids) {
    expect(spawnCounts.get(id)).toBe(1);
    expect(getTerminal(id)?.terminal.hasSelection()).toBe(false);
  }
});

/* ─────────────── THE ABSOLUTE PANE FLOOR, MEASURED IN A REAL ENGINE (finding F1) ───────────────
 *
 * The user's reading (18 / 204 / 408 / 36 / 204 / 18 / 18 px on a 7-pane tiling) is a MEASUREMENT,
 * so its refutation has to be one too: jsdom would report 0×0 for every pane and prove nothing.
 */

it("FLOOR: a 7-pane tiling never puts a pane under 180 × 120px, even seams pushed to the stops", async () => {
  const host = mountHost();
  const ids = ["t1", "t2", "t3", "t4", "t5", "t6", "t7"];
  render(<Tiling terminals={ids.map(record)} initial={sevenPanes()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === ids.length), "every pane spawned").toBe(true);

  const report = (boxes: Map<string, DOMRect>) =>
    [...boxes.entries()]
      .map(([id, b]) => `${id}=${Math.round(b.width)}×${Math.round(b.height)}`)
      .join(" ");

  const initial = paneBoxes(host);
  // MEASURED in headless Chromium on a 1200×700 host:
  //   t1=300×350 t2=300×350 t3=300×350 t4=300×700 t5=300×350 t6=300×350 t7=300×350
  // — against the review's reading of 18 / 204 / 408 / 36 / 204 / 18 / 18.
  expect(initial.size, "seven panes are tiled").toBe(7);
  const widths = [...initial.values()].map((b) => b.width);
  const heights = [...initial.values()].map((b) => b.height);
  expect(Math.min(...widths), `at rest: ${report(initial)}`).toBeGreaterThanOrEqual(
    MIN_PANE_WIDTH_PX,
  );
  expect(Math.min(...heights), `at rest: ${report(initial)}`).toBeGreaterThanOrEqual(
    MIN_PANE_HEIGHT_PX,
  );

  // Now SHOVE every seam, both ways, with real pointer events — the gesture that produced the
  // 18px panes. The floor has to hold under it, not only at rest.
  const separators = Array.from(host.querySelectorAll<HTMLElement>("[data-separator-id]"));
  expect(separators.length, "six splits for seven panes").toBe(6);
  for (const separator of separators) {
    for (const target of [-5000, 5000]) {
      const grip = centreOf(separator);
      fireEvent(separator, pointerEvent("pointerdown", grip.x, grip.y, 1));
      fireEvent(separator, pointerEvent("pointermove", target, target, 1));
      fireEvent(separator, pointerEvent("pointerup", target, target, 0));

      const boxes = paneBoxes(host);
      expect(boxes.size, "no pane vanished").toBe(7);
      for (const [id, box] of boxes) {
        expect(
          box.width,
          `${id} after dragging ${separator.dataset.separatorId} to ${target}: ${report(boxes)}`,
        ).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX - 0.5);
        expect(
          box.height,
          `${id} after dragging ${separator.dataset.separatorId} to ${target}: ${report(boxes)}`,
        ).toBeGreaterThanOrEqual(MIN_PANE_HEIGHT_PX - 0.5);
      }
    }
  }
});

it("FLOOR: a separator announces ITS OWN course in aria-valuemin/max, not the constant 15 / 85", async () => {
  const host = mountHost();
  const ids = ["t1", "t2", "t3", "t4", "t5", "t6", "t7"];
  render(<Tiling terminals={ids.map(record)} initial={sevenPanes()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === ids.length), "every pane spawned").toBe(true);
  // The ARIA bounds are derived from the MEASURED host, so wait for the observer's first pass.
  const outer = host.querySelector<HTMLElement>('[data-direction="row"]') as HTMLElement;
  expect(
    await waitFor(() => outer.getAttribute("aria-valuemin") !== "15"),
    "the measured bounds replaced the constants",
  ).toBe(true);

  // THE OUTERMOST vertical seam: each side holds two 300px columns whose own 50/50 split means
  // that side cannot go under 360px. On a 1200px host that is 30% … 70% — the real course.
  expect(outer.getAttribute("aria-valuemin")).toBe("30");
  expect(outer.getAttribute("aria-valuemax")).toBe("70");
  expect(outer.getAttribute("aria-valuenow")).toBe("50");

  // …and the announcement is honest: End really stops there, and the pane it squeezes stops at
  // 180px rather than walking through the floor.
  outer.focus();
  for (let i = 0; i < 60; i++) fireEvent.keyDown(outer, { key: "ArrowRight" });
  expect(outer.getAttribute("aria-valuenow")).toBe("70");
  const squeezed = paneBoxes(host);
  for (const [id, box] of squeezed) {
    expect(box.width, `${id} at the stop`).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX - 0.5);
  }
  // The two right-hand columns are exactly at the floor — the drag really BUT on it.
  // MEASURED at the stop: t1=420 t2=180 t3=420 t4=180 t5=420 t6=420 t7=180 — the three squeezed
  // panes sit EXACTLY on the 180px floor, and the seam announced 30..70 all along.
  expect(Math.min(...[...squeezed.values()].map((b) => b.width))).toBeCloseTo(MIN_PANE_WIDTH_PX, 0);
});

/** Two panes side by side — the simplest tiling whose seam has a real course. */
function twoPanes(): PaneNode {
  return splitPane(createPaneLayout("t1", "p1"), {
    paneId: "p1",
    direction: "row",
    terminalId: "t2",
    newPaneId: "p2",
  }) as PaneNode;
}

/**
 * GRACEFUL DEGRADATION of the floor (review arbitrage) — MEASURED, because the claim is about
 * pixels the user can really reach.
 *
 * A window shrunk UNDER twice the floor makes the constraint unsatisfiable: both panes are already
 * below 180px and no ratio can fix that. The seam used to FREEZE there (aria-valuemin =
 * aria-valuemax, drag and keys inert), which removed the one manual recovery the user had. It now
 * hands the relative 15..85 course back, so a pane can still be carried out to a readable width.
 */
it("DEGRADED FLOOR: a window shrunk under 2 × the floor hands the 15..85 course back (pointer AND keys)", async () => {
  // 400px still holds two panes at the 180px floor — the NOMINAL case is measured FIRST, so the
  // strict px clamp delivered by F1 is proven unchanged before the degraded one is exercised.
  const host = mountHost(400, 400);
  const ids = ["t1", "t2"];
  render(<Tiling terminals={ids.map(record)} initial={twoPanes()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === 2), "both panes spawned").toBe(true);

  const seam = host.querySelector<HTMLElement>('[data-direction="row"]') as HTMLElement;
  const width = (id: string) =>
    (host.querySelector(`[data-terminal-id="${id}"]`) as HTMLElement).getBoundingClientRect().width;
  const announced = () => [seam.getAttribute("aria-valuemin"), seam.getAttribute("aria-valuemax")];
  const drag = (toX: number) => {
    const grip = centreOf(seam);
    fireEvent(seam, pointerEvent("pointerdown", grip.x, grip.y, 1));
    fireEvent(seam, pointerEvent("pointermove", toX, grip.y, 1));
    fireEvent(seam, pointerEvent("pointerup", toX, grip.y, 0));
  };

  // (1) NOMINAL — 400px ≥ 2 × 180. The px floor is the tighter band and it is what is announced:
  // 180/400 = 45% … 55%. MEASURED: aria 45 / 55, panes 200 + 200px.
  expect(
    await waitFor(() => seam.getAttribute("aria-valuemin") === "45"),
    `the measured bounds replaced the constants (announced ${announced().join(" / ")})`,
  ).toBe(true);
  expect(announced()).toEqual(["45", "55"]);
  drag(5000);
  // MEASURED at the stop: t1 = 220px, t2 = 180px — the drag BUTS on the floor, F1 unchanged.
  expect(width("t1"), "the strict clamp still stops the drag at 55%").toBeCloseTo(220, 0);
  expect(width("t2"), "the squeezed pane sits exactly on the floor").toBeCloseTo(
    MIN_PANE_WIDTH_PX,
    0,
  );

  // (2) THE THRESHOLD, JUST ABOVE — 361px: still satisfiable (both sides fit at 180px), so the
  // band stays the strict one. It has NOT collapsed: it is [49.86%, 50.14%], i.e. 1px of real
  // travel. The two ANNOUNCED bounds are equal only because they are published as WHOLE PERCENTS
  // (`pane-tree-host.tsx`'s `aria-value*`, an assumed limit documented there) — the band truly
  // collapses at 360px exactly, where 180 + 180 leaves the single feasible ratio 0.5.
  host.style.width = "361px";
  expect(
    await waitFor(() => seam.getAttribute("aria-valuemin") === "50"),
    `just above the threshold, announced ${announced().join(" / ")}`,
  ).toBe(true);
  expect(announced()).toEqual(["50", "50"]);
  // …and the seam REALLY moves inside that band — which is exactly what the equal bounds hide.
  // The rounding is the ONLY reason they read as frozen, so the two are asserted side by side.
  seam.focus();
  // The seam is parked at 55% from step (1); ArrowLeft carries it down to the band's LOW end,
  // where the FIRST pane sits exactly on the px floor.
  fireEvent.keyDown(seam, { key: "ArrowLeft" });
  fireEvent.keyDown(seam, { key: "ArrowLeft" });
  expect(
    await waitFor(() => Math.abs(width("t1") - MIN_PANE_WIDTH_PX) < 0.5),
    `the seam reaches the px floor at 361px (t1 = ${width("t1")}px)`,
  ).toBe(true);
  const beforeKey = width("t1");
  // …and ONE ArrowRight moves it back up by the ~1px the whole-percent announcement rounds away.
  fireEvent.keyDown(seam, { key: "ArrowRight" });
  expect(
    await waitFor(() => width("t1") > beforeKey + 0.5),
    `ArrowRight must still move the seam at 361px (${beforeKey}px → ${width("t1")}px) even though aria announces ${announced().join(" / ")}`,
  ).toBe(true);
  // …stopping on the OTHER pane's floor, not on the announced 50%.
  expect(width("t2"), "the squeezed pane lands exactly on the floor").toBeCloseTo(
    MIN_PANE_WIDTH_PX,
    0,
  );

  // (3) THE THRESHOLD, JUST BELOW — 359px: 2 × 180 no longer fits. UNSATISFIABLE ⇒ the relative
  // course comes back, and the announcement says so instead of publishing two equal bounds.
  host.style.width = "359px";
  expect(
    await waitFor(() => seam.getAttribute("aria-valuemin") === "15"),
    `just below the threshold, announced ${announced().join(" / ")}`,
  ).toBe(true);
  expect(announced()).toEqual(["15", "85"]);
  expect(announced()[0], "the degraded bounds are never equal").not.toBe(announced()[1]);

  // …and the course is REAL, not just announced. Push the seam hard left first, so the pane the
  // user needs to read is the crushed one — the very situation the frozen band trapped them in.
  drag(-5000);
  const before = width("t1");
  expect(before, "the pane starts crushed, well under the floor").toBeLessThan(MIN_PANE_WIDTH_PX);
  // MEASURED: t1 = 54px before the drag, 305px after, on a 359px host.
  drag(5000);
  const after = width("t1");
  expect(after, "the drag really moved the seam").toBeGreaterThan(before + 50);
  expect(after, "the pane is carried back to a readable width").toBeGreaterThanOrEqual(
    MIN_PANE_WIDTH_PX,
  );
  // Best-effort is not unbounded: the other pane keeps the relative 15% and never collapses.
  expect(width("t2")).toBeCloseTo(359 * 0.15, 0);

  // (4) THE KEYBOARD, in that same degraded case — every key that was inert now displaces.
  seam.focus();
  expect(document.activeElement, "the seam takes the keyboard").toBe(seam);
  for (const key of ["ArrowLeft", "ArrowRight", "PageUp", "PageDown", "Home", "End"]) {
    // Park the seam mid-course first, so each key has room to move in BOTH directions.
    fireEvent.keyDown(seam, { key: "Home" });
    for (let i = 0; i < 17; i++) fireEvent.keyDown(seam, { key: "ArrowRight" });
    const start = width("t1");
    fireEvent.keyDown(seam, { key });
    expect(width("t1"), `${key} moves the seam in the degraded case`).not.toBeCloseTo(start, 0);
  }
  // MEASURED at the two ends: Home → t1 = 54px, End → t1 = 305px, on a 359px host.
  fireEvent.keyDown(seam, { key: "Home" });
  expect(width("t1")).toBeCloseTo(359 * 0.15, 0);
  fireEvent.keyDown(seam, { key: "End" });
  expect(width("t1")).toBeCloseTo(359 * 0.85, 0);
});

/* ───────── NO PANE CONTROL COVERED OR OUT OF FRAME (finding F2, critical) ─────────
 *
 * The review's measurement was `elementFromPoint` at the centre of every control of every pane:
 * 4 panes out of 7 had at least one button that resolved to ANOTHER command — "split right"
 * reaching "close terminal". Only a real engine hit-tests, so the refutation lives here.
 */

/** Every control button of every pane, keyed by the pane element that owns it. */
function controlButtons(host: HTMLElement): { pane: HTMLElement; button: HTMLElement }[] {
  const out: { pane: HTMLElement; button: HTMLElement }[] = [];
  for (const pane of host.querySelectorAll<HTMLElement>(
    '[data-terminal-id][data-visible="true"]',
  )) {
    for (const button of pane.querySelectorAll<HTMLElement>("[data-pane-controls] button")) {
      out.push({ pane, button });
    }
  }
  return out;
}

/** What a click at `point` would REALLY reach, walked back to the button that owns it. */
function hitAt(point: { x: number; y: number }): Element | null {
  return document.elementFromPoint(point.x, point.y);
}

it("F2: not ONE control of a 7-pane tiling is covered or out of its frame (elementFromPoint)", async () => {
  const host = mountHost();
  const ids = ["t1", "t2", "t3", "t4", "t5", "t6", "t7"];
  render(<LiveTiling terminals={ids.map(record)} initial={sevenPanes()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === ids.length), "every pane spawned").toBe(true);
  // The cluster's shape follows the MEASURED pane box, so wait for the host's first measurement.
  expect(
    await waitFor(() => host.querySelectorAll("[data-pane-controls]").length === 7),
    "every visible pane carries a control cluster",
  ).toBe(true);

  const check = (label: string) => {
    const controls = controlButtons(host);
    let covered = 0;
    for (const { pane, button } of controls) {
      const paneBox = pane.getBoundingClientRect();
      const box = button.getBoundingClientRect();

      // (2) INSIDE THE FRAME — every edge, not just the visible part. A button clipped away by
      // the pane's `overflow-hidden` still ate the click; comparing rects is what catches that.
      expect(box.left, `${label}: ${button.ariaLabel} left of its pane`).toBeGreaterThanOrEqual(
        paneBox.left - 0.5,
      );
      expect(box.top, `${label}: ${button.ariaLabel} above its pane`).toBeGreaterThanOrEqual(
        paneBox.top - 0.5,
      );
      expect(box.right, `${label}: ${button.ariaLabel} right of its pane`).toBeLessThanOrEqual(
        paneBox.right + 0.5,
      );
      expect(box.bottom, `${label}: ${button.ariaLabel} below its pane`).toBeLessThanOrEqual(
        paneBox.bottom + 0.5,
      );

      // (1) THE CLICK LANDS ON IT — the review's own measurement, button by button.
      const hit = hitAt({ x: box.left + box.width / 2, y: box.top + box.height / 2 });
      const reached = hit != null && (hit === button || button.contains(hit));
      if (!reached) covered += 1;
      expect(
        reached,
        `${label}: clicking "${button.ariaLabel}" reaches ${
          hit == null
            ? "nothing"
            : `<${hit.tagName.toLowerCase()} ${hit.getAttribute("aria-label") ?? hit.getAttribute("role") ?? hit.className}>`
        }`,
      ).toBe(true);
    }
    expect(covered, `${label}: covered controls`).toBe(0);
    return controls.length;
  };

  // MEASURED: 7 panes × 3 controls = 21 buttons, 0 covered, 0 out of frame.
  expect(check("at rest"), "7 panes × 3 controls").toBe(21);

  // …and again with every seam shoved to its stop, where the panes are at the 180px floor and the
  // clusters sit as close to a seam as the geometry ever allows.
  for (const separator of host.querySelectorAll<HTMLElement>("[data-separator-id]")) {
    const grip = centreOf(separator);
    fireEvent(separator, pointerEvent("pointerdown", grip.x, grip.y, 1));
    fireEvent(separator, pointerEvent("pointermove", 5000, 5000, 1));
    fireEvent(separator, pointerEvent("pointerup", 5000, 5000, 0));
  }
  expect(check("with every seam at its stop"), "still 21 buttons").toBe(21);
});

/**
 * THE TAILWIND ↔ CONSTANT COUPLING (review finding), measured where it can actually be caught.
 *
 * `CONTROL_BUTTON_PX` / `CONTROL_GAP_PX` / `CONTROL_INSET_PX` are hand-written MIRRORS of `size-6`,
 * `gap-1` and `top-1 right-1`. The unit spec only exercises the arithmetic, and the "inside the
 * frame" assertions above stay true long after the constants have started lying — so a `size-6` →
 * `size-7` change would leave `paneControlsMode` announcing "full" for a width that no longer fits.
 * Only the RENDERED box can refute that, and only in a real engine.
 */
it("F2: the RENDERED cluster measures exactly what paneControlsWidthPx prices it at", async () => {
  const host = mountHost();
  const ids = ["t1", "t2", "t3", "t4"];
  render(<LiveTiling terminals={ids.map(record)} initial={fourPanes()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === 4), "every pane spawned").toBe(true);
  expect(
    await waitFor(() => host.querySelectorAll('[data-pane-controls-mode="full"]').length === 4),
    "every pane carries the full cluster",
  ).toBe(true);

  for (const pane of host.querySelectorAll<HTMLElement>(
    '[data-terminal-id][data-visible="true"]',
  )) {
    const paneBox = pane.getBoundingClientRect();
    const cluster = pane.querySelector("[data-pane-controls]") as HTMLElement;
    const buttons = Array.from(cluster.querySelectorAll<HTMLElement>("button"));
    expect(buttons, "three commands in the full cluster").toHaveLength(3);

    for (const button of buttons) {
      const box = button.getBoundingClientRect();
      // `size-6` IS 24px — the constant `paneControlsWidthPx` prices the cluster with.
      expect(box.width, `${button.ariaLabel} width mirrors CONTROL_BUTTON_PX`).toBeCloseTo(
        CONTROL_BUTTON_PX,
        1,
      );
      expect(box.height, `${button.ariaLabel} height mirrors CONTROL_BUTTON_PX`).toBeCloseTo(
        CONTROL_BUTTON_PX,
        1,
      );
    }

    // `right-1` / `top-1` ARE 4px, on the pane's own frame.
    const first = buttons[0] as HTMLElement;
    const last = buttons[buttons.length - 1] as HTMLElement;
    expect(
      paneBox.right - last.getBoundingClientRect().right,
      "the cluster's right inset mirrors CONTROL_INSET_PX",
    ).toBeCloseTo(CONTROL_INSET_PX, 1);
    expect(
      first.getBoundingClientRect().top - paneBox.top,
      "the cluster's top inset mirrors CONTROL_INSET_PX",
    ).toBeCloseTo(CONTROL_INSET_PX, 1);
    // `gap-1` IS 4px — measured BETWEEN two rendered buttons, not read off a class name.
    const second = buttons[1] as HTMLElement;
    expect(
      second.getBoundingClientRect().left - first.getBoundingClientRect().right,
      "the gap between two buttons mirrors CONTROL_GAP_PX",
    ).toBeCloseTo(CONTROL_GAP_PX, 1);

    // …and the whole thing costs exactly what the pure function charges for it: the RENDERED
    // cluster spans `paneControlsWidthPx(3)` px from the pane's right edge. That number is what
    // `MIN_PANE_WIDTH_PX = 180` was sized to hold (pinned in pane-geometry.test.ts).
    expect(
      paneBox.right - first.getBoundingClientRect().left + CONTROL_INSET_PX,
      "the rendered cluster costs paneControlsWidthPx(3)",
    ).toBeCloseTo(paneControlsWidthPx(3), 1);
  }
});

/**
 * THE SAME COUPLING, AT THE ONE WIDTH WHERE THE OVERRIDE IS LOAD-BEARING (review finding).
 *
 * The spec above runs at the suite's 1400px viewport, and `icon-xs` is `size-7 sm:size-6`: above
 * the `sm` breakpoint the design system ITSELF renders 24px, so that spec measures 24px whether
 * the cluster's own `size-6` is there or not — MEASURED: mutating it to `size-7` leaves it green.
 * UNDER the breakpoint `icon-xs` falls back to its base `size-7` (28px) and the override is the
 * only thing keeping the button at `CONTROL_BUTTON_PX` — which is exactly the case
 * `pane-tree-host.tsx` cites to justify the override, and exactly what `paneControlsWidthPx`
 * (a NON-responsive px budget) is priced on. So it is measured here.
 */
it("F2: the cluster still measures CONTROL_BUTTON_PX UNDER the sm breakpoint", async () => {
  await page.viewport(600, 900);
  // The premise, asserted rather than assumed: `sm:` (40rem = 640px) is really inactive here, so
  // `icon-xs`'s responsive half cannot be what holds the size.
  expect(
    window.matchMedia("(min-width: 40rem)").matches,
    "the sm breakpoint must be INACTIVE for this spec to mean anything",
  ).toBe(false);

  const host = mountHost(560, 400);
  render(<LiveTiling terminals={["t1", "t2"].map(record)} initial={twoPanes()} />, {
    container: host,
  });
  expect(await waitFor(() => spawnSizes.size === 2), "both panes spawned").toBe(true);
  expect(
    await waitFor(() => host.querySelectorAll('[data-pane-controls-mode="full"]').length === 2),
    "both panes carry the full cluster",
  ).toBe(true);

  for (const pane of host.querySelectorAll<HTMLElement>(
    '[data-terminal-id][data-visible="true"]',
  )) {
    const paneBox = pane.getBoundingClientRect();
    const cluster = pane.querySelector("[data-pane-controls]") as HTMLElement;
    const buttons = Array.from(cluster.querySelectorAll<HTMLElement>("button"));
    expect(buttons, "three commands in the full cluster").toHaveLength(3);
    for (const button of buttons) {
      const box = button.getBoundingClientRect();
      expect(box.width, `${button.ariaLabel} width mirrors CONTROL_BUTTON_PX under sm`).toBeCloseTo(
        CONTROL_BUTTON_PX,
        1,
      );
      expect(
        box.height,
        `${button.ariaLabel} height mirrors CONTROL_BUTTON_PX under sm`,
      ).toBeCloseTo(CONTROL_BUTTON_PX, 1);
    }
    // …and the whole cluster still costs what the pure function charges — the number the 180px
    // pane floor was sized to hold, at EVERY window width since the budget is not responsive.
    const first = buttons[0] as HTMLElement;
    expect(
      paneBox.right - first.getBoundingClientRect().left + CONTROL_INSET_PX,
      "the rendered cluster costs paneControlsWidthPx(3) under sm too",
    ).toBeCloseTo(paneControlsWidthPx(3), 1);
  }
});

it("F2: the cluster stacks ABOVE the separator — the z-index bug, independent of pane width", async () => {
  const host = mountHost();
  const ids = ["t1", "t2", "t3", "t4"];
  render(<LiveTiling terminals={ids.map(record)} initial={fourPanes()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === 4), "every pane spawned").toBe(true);
  expect(await waitFor(() => host.querySelector("[data-pane-controls]") != null)).toBe(true);

  // A pane slot is `position:absolute` with `z-index:auto` — NOT a stacking context — so the
  // cluster and the separators compete in the HOST's context and these two numbers really order
  // them. At z-20 the cluster lost to every seam it touched; it must now win.
  const cluster = host.querySelector("[data-pane-controls]") as HTMLElement;
  const separator = host.querySelector("[data-separator-id]") as HTMLElement;
  const clusterZ = Number(getComputedStyle(cluster).zIndex);
  const separatorZ = Number(getComputedStyle(separator).zIndex);
  expect(Number.isFinite(clusterZ), "the cluster carries a real z-index").toBe(true);
  expect(Number.isFinite(separatorZ), "the separator carries a real z-index").toBe(true);
  expect(clusterZ, `cluster z=${clusterZ} must beat separator z=${separatorZ}`).toBeGreaterThan(
    separatorZ,
  );

  // …and on every CONTESTED pixel — wherever a control's box meets a seam's band, whether the two
  // merely TOUCH (today's geometry: `CONTROL_INSET_PX` = 4 equals `SEPARATOR_THICKNESS_PX` / 2) or
  // genuinely CROSS — the CONTROL is what a click reaches. Sampling the touching pairs too is what
  // keeps this loop from running empty: with crossings alone it would assert nothing at all.
  const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
  let crossings = 0;
  let contested = 0;
  for (const { button } of controlButtons(host)) {
    const box = button.getBoundingClientRect();
    for (const band of host.querySelectorAll<HTMLElement>("[data-separator-id]")) {
      const bandBox = band.getBoundingClientRect();
      const left = Math.max(box.left, bandBox.left);
      const right = Math.min(box.right, bandBox.right);
      const top = Math.max(box.top, bandBox.top);
      const bottom = Math.min(box.bottom, bandBox.bottom);
      // Disjoint on either axis (a gap wider than a rounding error) ⇒ nothing is contested.
      if (right - left < -0.5 || bottom - top < -0.5) continue;
      contested += 1;
      if (right - left > 1 && bottom - top > 1) crossings += 1;
      // The BUTTON's own pixel closest to the band — kept half a pixel inside its box, so the
      // question asked is exactly "does the seam steal the control's edge?".
      const hit = hitAt({
        x: clamp((left + right) / 2, box.left + 0.5, box.right - 0.5),
        y: clamp((top + bottom) / 2, box.top + 0.5, box.bottom - 0.5),
      });
      expect(
        hit != null && (hit === button || button.contains(hit)),
        `the seam ${band.dataset.separatorId} took "${button.ariaLabel}"'s contested edge`,
      ).toBe(true);
    }
  }
  // MEASURED: cluster z-index = 40, separator z-index = 30, 8 contested button/band pairs and 0
  // genuine crossings in this fixture — the cluster's 4px inset EQUALS the band's half-thickness,
  // so the two touch without crossing. Both counts are PINNED, not merely reported: `crossings`
  // is a constant a future inset/thickness change would break (the review's finding — a count is
  // `>= 0` by construction, so that shape of assertion could never fail), and `contested` proves
  // the hit-test above ran on a NON-EMPTY sample instead of quietly skipping every pair.
  expect(crossings, "no button/band pair crosses at today's inset and thickness").toBe(0);
  expect(contested, "the hit-test above ran on a non-empty sample").toBe(8);
});

it("F2: the separator stays grabbable and draggable everywhere a cluster does not sit", async () => {
  const host = mountHost();
  const ids = ["t1", "t2", "t3", "t4"];
  render(<LiveTiling terminals={ids.map(record)} initial={fourPanes()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === 4), "every pane spawned").toBe(true);
  expect(await waitFor(() => host.querySelector("[data-pane-controls]") != null)).toBe(true);

  const separator = host.querySelector<HTMLElement>('[data-direction="row"]') as HTMLElement;
  const band = separator.getBoundingClientRect();
  // A point ON the band that no cluster covers (they hug the panes' TOP-right corners) and that no
  // OTHER seam crosses (a `column` split's band runs across this one at its own boundary). A
  // quarter down the band is clear of both — and the assertions below prove it rather than assume.
  const grip = { x: band.left + band.width / 2, y: band.top + band.height * 0.25 };
  const covers = (box: DOMRect) =>
    grip.x >= box.left && grip.x <= box.right && grip.y >= box.top && grip.y <= box.bottom;
  for (const { button } of controlButtons(host)) {
    expect(covers(button.getBoundingClientRect()), "the grip is clear of every control").toBe(
      false,
    );
  }
  for (const other of host.querySelectorAll<HTMLElement>("[data-separator-id]")) {
    if (other === separator) continue;
    expect(covers(other.getBoundingClientRect()), "the grip is clear of every other seam").toBe(
      false,
    );
  }
  expect(hitAt(grip), "the band itself answers there").toBe(separator);

  // …and it really drags from that point.
  const widthBefore = (
    host.querySelector('[data-terminal-id="t1"]') as HTMLElement
  ).getBoundingClientRect().width;
  fireEvent(separator, pointerEvent("pointerdown", grip.x, grip.y, 1));
  fireEvent(separator, pointerEvent("pointermove", grip.x + 120, grip.y, 1));
  fireEvent(separator, pointerEvent("pointerup", grip.x + 120, grip.y, 0));
  const widthAfter = (
    host.querySelector('[data-terminal-id="t1"]') as HTMLElement
  ).getBoundingClientRect().width;
  expect(widthAfter, "the seam moved from a point the cluster does not cover").toBeCloseTo(
    widthBefore + 120,
    0,
  );
});

/**
 * A tiling holding a pane FAR too narrow for the three-button cluster (60px of a 1200px host) —
 * the shape the relative-only clamp used to allow, and the one the overflow fallback exists for.
 * Built by hand: the guarded split path refuses to produce it (F1), which is exactly the point.
 */
function narrowAndWide(): PaneNode {
  return {
    kind: "split",
    id: "s1",
    direction: "row",
    ratio: 0.05,
    children: [
      { kind: "leaf", id: "p1", terminalId: "t1" },
      {
        kind: "split",
        id: "s2",
        direction: "row",
        ratio: 0.5,
        children: [
          { kind: "leaf", id: "p2", terminalId: "t2" },
          { kind: "leaf", id: "p3", terminalId: "t3" },
        ],
      },
    ],
  };
}

it("F2: too narrow for three buttons ⇒ ONE overflow button with the very same commands", async () => {
  const host = mountHost();
  const ids = ["t1", "t2", "t3"];
  render(<LiveTiling terminals={ids.map(record)} initial={narrowAndWide()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === 3), "every pane spawned").toBe(true);
  expect(
    await waitFor(() => host.querySelector('[data-pane-controls-mode="overflow"]') != null),
    "the narrow pane fell back to the overflow menu",
  ).toBe(true);

  const narrow = host.querySelector('[data-terminal-id="t1"]') as HTMLElement;
  const wide = host.querySelector('[data-terminal-id="t2"]') as HTMLElement;
  expect(narrow.getBoundingClientRect().width).toBeCloseTo(60, 0);
  expect(wide.getBoundingClientRect().width).toBeCloseTo(570, 0);

  // BELOW the width: exactly ONE button, and it is inside the frame.
  const narrowCluster = narrow.querySelector("[data-pane-controls]") as HTMLElement;
  expect(narrowCluster.dataset.paneControlsMode).toBe("overflow");
  const overflowButtons = Array.from(narrowCluster.querySelectorAll("button"));
  expect(overflowButtons).toHaveLength(1);
  const trigger = overflowButtons[0] as HTMLElement;
  expect(trigger.ariaLabel).toBe("Pane commands");
  // ALIGNED with the product's three other `<Menu>` kebabs (review finding): a design-system
  // `<Button>` (hence its focus ring, not the browser's outline) carrying the MENU's own tooltip
  // prop — so NO native `title` bubble — and really wired as the menu's trigger.
  expect(trigger.className, "the trigger is the design system's <Button>").toContain(
    "focus-visible:ring-2",
  );
  expect(trigger.getAttribute("title"), "no native OS tooltip on the kebab").toBeNull();
  expect(trigger.getAttribute("aria-expanded"), "it is the menu's trigger").toBe("false");
  const triggerBox = trigger.getBoundingClientRect();
  const narrowBox = narrow.getBoundingClientRect();
  expect(triggerBox.left).toBeGreaterThanOrEqual(narrowBox.left - 0.5);
  expect(triggerBox.right).toBeLessThanOrEqual(narrowBox.right + 0.5);
  // …and a click really reaches IT, not the pane behind or the seam beside it.
  const hit = hitAt({
    x: triggerBox.left + triggerBox.width / 2,
    y: triggerBox.top + triggerBox.height / 2,
  });
  expect(hit != null && (hit === trigger || trigger.contains(hit))).toBe(true);

  // ABOVE the width: the three buttons, unchanged.
  const wideCluster = wide.querySelector("[data-pane-controls]") as HTMLElement;
  expect(wideCluster.dataset.paneControlsMode).toBe("full");
  expect(Array.from(wideCluster.querySelectorAll("button")).map((b) => b.ariaLabel)).toEqual([
    "Split pane right",
    "Split pane down",
    "Close pane",
  ]);

  // THE SAME COMMANDS: opening the fallback lists all three, by the same names.
  fireEvent.click(trigger);
  expect(
    await waitFor(() => document.querySelectorAll('[role="menuitem"]').length === 3),
    "the overflow menu lists the three commands",
  ).toBe(true);
  expect(
    Array.from(document.querySelectorAll('[role="menuitem"]')).map((item) =>
      (item.textContent ?? "").trim(),
    ),
  ).toEqual(["Split pane right", "Split pane down", "Close pane"]);
});

it("F2: 'Close pane' really closes, from the full cluster AND from the overflow menu", async () => {
  const host = mountHost();
  const ids = ["t1", "t2", "t3"];
  render(<LiveTiling terminals={ids.map(record)} initial={narrowAndWide()} />, { container: host });
  expect(await waitFor(() => spawnSizes.size === 3), "every pane spawned").toBe(true);
  expect(
    await waitFor(() => host.querySelector('[data-pane-controls-mode="overflow"]') != null),
  ).toBe(true);

  const tiled = () => host.querySelectorAll('[data-terminal-id][data-visible="true"]').length;
  expect(tiled()).toBe(3);

  // FROM THE OVERFLOW MENU — the narrow pane, whose only way in is the `…` fallback.
  const trigger = host.querySelector<HTMLElement>(
    '[data-pane-controls-mode="overflow"] button',
  ) as HTMLElement;
  fireEvent.click(trigger);
  const menuItem = (label: string) =>
    Array.from(document.querySelectorAll<HTMLElement>('[role="menuitem"]')).find(
      (item) => (item.textContent ?? "").trim() === label,
    ) ?? null;
  expect(await waitFor(() => menuItem("Close pane") != null), "the menu opened").toBe(true);
  fireEvent.click(menuItem("Close pane") as HTMLElement);
  expect(await waitFor(() => tiled() === 2), "the overflow menu closed the pane (3 → 2)").toBe(
    true,
  );

  // FROM THE FULL CLUSTER — a wide pane, three real buttons.
  const fullClose = host.querySelector<HTMLElement>(
    '[data-pane-controls-mode="full"] button[aria-label="Close pane"]',
  ) as HTMLElement;
  expect(fullClose, "a full cluster is still on screen").not.toBeNull();
  fireEvent.click(fullClose);
  expect(await waitFor(() => tiled() === 1), "the cluster closed the pane (2 → 1)").toBe(true);
  // The LAST pane is kept, so its close control is gone rather than inert.
  expect(host.querySelector('button[aria-label="Close pane"]')).toBeNull();
});

/**
 * THE FOCUSED PANE'S STROKE, MEASURED IN REAL CHROMIUM (review 01M0ABR6V501D0SPR2BYBVZC4A, minor).
 *
 * "It can be quite hard to know which one I am focused in" — 1px of `ring/70` was not enough.
 * The user's arbitrage on the mockups was BORDER ONLY, thicker: no per-pane header, no dimming of
 * the neighbours. jsdom would only echo the class string back; this reads the computed stroke and
 * the real rectangles, which is the only place the claim means anything.
 */
it("draws the focused pane's outline THICKER and LIGHTER than its neighbours', measured", async () => {
  const host = mountHost();
  const terminals = [record("t1"), record("t2"), record("t3"), record("t4")];
  render(<Tiling terminals={terminals} initial={fourPanes()} focusedPaneId="p3" />, {
    container: host,
  });
  await waitFor(() => paneBoxes(host).size === 4);

  const styleOf = (terminalId: string) =>
    getComputedStyle(host.querySelector(`[data-terminal-id="${terminalId}"]`) as HTMLElement);
  // p3 holds t3 — the focused pane.
  const focused = styleOf("t3");
  const resting = styleOf("t1");

  const px = (value: string) => Number.parseFloat(value);
  expect(px(focused.outlineWidth), "the focused stroke is 2px").toBe(2);
  expect(px(resting.outlineWidth), "a resting stroke is 1px").toBe(1);
  expect(px(focused.outlineWidth)).toBeGreaterThan(px(resting.outlineWidth));

  // …and MORE CONTRASTED against the surface behind it. "Lighter" is theme-relative (on a light
  // theme the readable stroke is the DARKER one), so what is measured is the perceptual distance
  // from the surface the pane really paints, with the stroke's own alpha composited in — a
  // 10%-opaque white hairline is barely there whatever its nominal lightness says.
  //
  // Chromium hands colours back as `oklch(...)` or `rgb(...)` depending on how they were authored;
  // both are reduced to ONE scale (OKLab lightness, exact for the greys this palette uses).
  const lightness = (colour: string): { l: number; alpha: number } => {
    const nums = (colour.match(/[0-9.]+/g) ?? []).map(Number);
    if (colour.startsWith("oklch")) {
      const alpha = colour.includes("/") || nums.length > 3 ? (nums[3] ?? 1) : 1;
      return { l: nums[0] ?? 0, alpha };
    }
    const [r = 0, g = 0, b = 0, a = 1] = nums;
    const lin = (c: number) => {
      const v = c / 255;
      return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
    };
    // OKLab's L is the cube root of the linear luminance for a neutral — exact for these greys.
    return { l: Math.cbrt(0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)), alpha: a };
  };
  // The surface a pane really paints: the terminal container's own `bg-background`.
  const surfaceL = lightness(
    getComputedStyle(host.querySelector("[data-terminal-container]") as HTMLElement)
      .backgroundColor,
  ).l;
  /** How far a stroke really sits from that surface, once its own transparency is applied. */
  const separation = (colour: string): number => {
    const { l, alpha } = lightness(colour);
    return Math.abs(alpha * (l - surfaceL));
  };
  expect(focused.outlineColor).not.toBe(resting.outlineColor);
  expect(
    separation(focused.outlineColor),
    `focused ${focused.outlineColor} must out-contrast resting ${resting.outlineColor} on a surface at L=${surfaceL}`,
  ).toBeGreaterThan(separation(resting.outlineColor));
});

it("does NOT move the geometry by a pixel: the same rectangles with and against the focus", async () => {
  const host = mountHost();
  const terminals = [record("t1"), record("t2"), record("t3"), record("t4")];
  const { rerender } = render(
    <Tiling terminals={terminals} initial={fourPanes()} focusedPaneId="p3" />,
    { container: host },
  );
  await waitFor(() => paneBoxes(host).size === 4);
  const withFocusOnP3 = [...paneBoxes(host)].map(([id, box]) => [
    id,
    box.left,
    box.top,
    box.width,
    box.height,
  ]);

  // Move the focus to another pane — an outline is out of the flow, so NOTHING may move.
  rerender(<Tiling terminals={terminals} initial={fourPanes()} focusedPaneId="p1" />);
  await waitFor(() => paneBoxes(host).size === 4);
  const withFocusOnP1 = [...paneBoxes(host)].map(([id, box]) => [
    id,
    box.left,
    box.top,
    box.width,
    box.height,
  ]);
  expect(withFocusOnP1).toEqual(withFocusOnP3);

  // The 180×120px floor is still held by every pane of this tiling.
  for (const [, box] of paneBoxes(host)) {
    expect(box.width).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX);
    expect(box.height).toBeGreaterThanOrEqual(MIN_PANE_HEIGHT_PX);
  }
});

it("marks the focus with the stroke ALONE — no pane header, no dimmed neighbours", async () => {
  const host = mountHost();
  const terminals = [record("t1"), record("t2"), record("t3"), record("t4")];
  render(<Tiling terminals={terminals} initial={fourPanes()} focusedPaneId="p3" />, {
    container: host,
  });
  await waitFor(() => paneBoxes(host).size === 4);

  // NO PER-PANE HEADER: the terminal's own container fills its pane top to bottom — a header
  // would eat a strip of it. (The floating pane CONTROLS are an overlay, not a header: they are
  // `absolute` and take no height of their own.)
  for (const terminalId of ["t1", "t2", "t3", "t4"]) {
    const pane = host.querySelector(`[data-terminal-id="${terminalId}"]`) as HTMLElement;
    const surface = pane.querySelector(`[data-terminal-container="${terminalId}"]`) as HTMLElement;
    const paneBox = pane.getBoundingClientRect();
    const surfaceBox = surface.getBoundingClientRect();
    expect(surfaceBox.height, `${terminalId}: the terminal fills its pane's height`).toBe(
      paneBox.height,
    );
    expect(surfaceBox.top, `${terminalId}: nothing sits above the terminal`).toBe(paneBox.top);
  }

  // NO DIMMING: every unfocused pane is fully opaque and unfiltered, so what the user watches out
  // of the corner of their eye keeps its legibility.
  for (const terminalId of ["t1", "t2", "t4"]) {
    const style = getComputedStyle(
      host.querySelector(`[data-terminal-id="${terminalId}"]`) as HTMLElement,
    );
    expect(style.opacity).toBe("1");
    expect(style.filter === "none" || style.filter === "").toBe(true);
  }
});
