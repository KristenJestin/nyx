// The REAL app CSS: the detail's pane is laid out by TRUE Tailwind utilities (`h-svh`, the
// `grid-cols-[minmax(18rem,22rem)_1fr]` track, `min-h-0 flex-1`, `h-full`). Without them the pane
// has no height and "does the preview fill it and centre in it?" cannot even be asked.
// `vitest.config.ts` wires `@tailwindcss/vite`, so this `@import "tailwindcss"` genuinely compiles.
import "@/globals.css";

import { RouterProvider } from "@tanstack/react-router";
import { render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { page } from "vitest/browser";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";
import { createAppRouter } from "@/router";

import type { DesignComponentRow, DesignRow } from "./types";

/**
 * THE PRESENTATION OF THE DETAIL'S LIVE PREVIEW (finding `01KYSA1N8SDT012R6E9XFXFJB0`), measured in
 * a real Chromium.
 *
 * The user's report was a picture: on the composant detail the instance sat GLUED TO THE TOP-LEFT of
 * an immense pane whose background was WHITE, inside a dark app. Both halves came from the vignette
 * fix (`01KYPEG16E95WADT2VMCYBPCK5`) never being extended here — that finding had put the detail
 * explicitly out of scope — so the pane still carried a hard `bg-white` and passed no `center`.
 *
 * Only a browser can settle it: GEOMETRY and COMPUTED COLOUR do not exist in jsdom. Two things are
 * measured, and they are measured on the REAL page (the router, the real pane, the real CSS):
 *
 *   * the frame's own BOX — it FILLS the pane instead of shrink-wrapping the instance (that
 *     auto-sizing is what left a 36 px band at the top of a 600 px pane);
 *   * inside a MIRROR of the very `srcdoc` the page built, mounted at the frame's MEASURED size:
 *     where the instance actually lands, and what Chromium computes for the canvas colour.
 *
 * The mirror is necessary and does not weaken the measure: the real preview is
 * `sandbox="allow-scripts"` — an opaque origin whose `contentDocument` is unreachable BY DESIGN, and
 * that is not relaxed here. The mirror runs the same document, under its own strict CSP, in the same
 * box the real pane gave the real frame.
 */

/** The project's ingested token layer: an explicit, checkable dark canvas. */
const TOKENS = JSON.stringify({
  variables: { "--background": "rgb(11, 15, 25)", "--foreground": "rgb(226, 232, 240)" },
  dark: {},
});

/**
 * A layer shaped like the REAL one (finding `01KYSVEZMJP1Z8V7SYJ8YC5H9W`): a LIGHT default AND a
 * DARK override, the way sandbox's ingested layer carries 103 light variables and 48 dark ones
 * (`--background` = `oklch(99.107% …)` light, `oklch(18% …)` dark; `--card`, `--muted`,
 * `--foreground` likewise). The values here are the project's real ones so the assertion is on the
 * design system's own numbers, not on a convenient fixture.
 */
const TOKENS_LIGHT_AND_DARK = JSON.stringify({
  variables: {
    "--background": "oklch(99.107% 0.00011 271.152)",
    "--foreground": "oklch(0.2046 0 0)",
  },
  dark: {
    "--background": "oklch(18% 0.00411 285.902)",
    "--foreground": "oklch(0.9288 0.0126 255.5078)",
  },
});

const DESIGN: DesignRow = {
  id: "d1",
  project_id: "p1",
  content: "",
  theme_tokens_json: TOKENS,
  created_at: 0,
  updated_at: 0,
};

const BASE: DesignComponentRow = {
  id: "c1",
  project_id: "p1",
  name: "Button",
  source_repository_id: null,
  source_path: "src/ui/Button.tsx",
  visual_hash: null,
  template: `<button id="probe" class="px-4 py-2">{{label}}</button>`,
  params_json: JSON.stringify([
    { name: "label", type: "text", required: true, preview: "Enregistrer" },
  ]),
  states_json: JSON.stringify(["default"]),
  created_at: 0,
  updated_at: 0,
  scanned_input_hash: null,
  orphaned_at: null,
};

/**
 * A FULL-PAGE component — a `dialog` overlay, far bigger than the pane on BOTH axes. This is the
 * case the finding warns about: centring must not CROP it. `safe center` is what guarantees that.
 */
const DIALOG: DesignComponentRow = {
  ...BASE,
  name: "Dialog",
  template:
    `<div id="probe" style="width:1600px;height:1400px;border:2px solid currentColor">` +
    `<h2 id="dialog-title">{{label}}</h2></div>`,
};

let currentComponent: DesignComponentRow = BASE;
/** The project's stored token layer for the test at hand (light-only, or light + dark). */
let currentTokens: string = TOKENS;

function installBackend() {
  (nyxBridge as FakeNyxBridge).onAnyCommand((cmd, args) => {
    switch (cmd) {
      case "list_projects":
        return [
          {
            id: "p1",
            name: "Project",
            key: "NYX",
            collapsed: false,
            created_at: 0,
            updated_at: 0,
            resume_agent_sessions: false,
          },
        ];
      case "list_workspaces":
        return [];
      case "design_get":
        return { ...DESIGN, theme_tokens_json: currentTokens };
      case "design_component_get":
        return currentComponent;
      case "design_render": {
        const { params } = JSON.parse((args as { compositionJson: string }).compositionJson) as {
          params: Record<string, unknown>;
        };
        return {
          markup: currentComponent.template.replace(/\{\{(\w+)\}\}/g, (_m, k: string) =>
            String(params[k] ?? ""),
          ),
        };
      }
      default:
        return undefined;
    }
  });
}

/** The REAL preview frame of the REAL detail page, once the render resolved. */
async function realFrame(): Promise<HTMLIFrameElement> {
  const frame = (await screen.findByTitle(
    `${currentComponent.name} live preview`,
  )) as HTMLIFrameElement;
  // The opaque sandbox is NOT relaxed by this spec — that is why the mirror below exists.
  expect(frame.getAttribute("sandbox")).toBe("allow-scripts");
  return frame;
}

/**
 * The perceived LIGHTNESS (0 = black, 1 = white) of a computed colour, whichever notation Chromium
 * kept: it preserves `oklch()` as-is (its first component IS the lightness) and serializes the rest
 * as `rgb()`. This is what lets an assertion be about the SURFACE — "dark" / "light" — instead of
 * about a string the browser is free to reformat.
 */
function lightnessOf(computed: string): number {
  const nums = computed.match(/[\d.]+%?/g) ?? [];
  if (/^oklch\(/i.test(computed.trim())) {
    const raw = nums[0] ?? "0";
    return raw.endsWith("%") ? Number.parseFloat(raw) / 100 : Number.parseFloat(raw);
  }
  const [r = 0, g = 0, b = 0] = nums.map((n) => Number.parseFloat(n));
  return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
}

/** Mount the page's own `srcdoc` at the EXACT box the real pane gave the real frame. */
function mirrorAt(srcdoc: string, width: number, height: number): HTMLIFrameElement {
  const mirror = document.createElement("iframe");
  mirror.style.width = `${width}px`;
  mirror.style.height = `${height}px`;
  mirror.style.border = "0";
  mirror.srcdoc = srcdoc;
  document.body.appendChild(mirror);
  return mirror;
}

afterEach(() => {
  (nyxBridge as FakeNyxBridge).reset();
  document.body.replaceChildren();
  localStorage.clear();
  currentComponent = BASE;
  currentTokens = TOKENS;
});

/**
 * A DESKTOP viewport, because the pane's width is part of what is measured: the detail is a
 * `grid-cols-[minmax(18rem,22rem)_1fr]`, so at the browser project's default (mobile-ish) width the
 * `1fr` preview track collapses to nothing and there is no pane to centre in.
 */
const VIEWPORT: [number, number] = [1280, 800];

it("centres the instance in the detail pane, on the canvas the project's tokens define", async () => {
  await page.viewport(...VIEWPORT);
  installBackend();
  render(<RouterProvider router={createAppRouter(["/design/c1"])} />);

  const frame = await realFrame();
  const box = frame.getBoundingClientRect();

  // 1. THE BOX FILLS THE PANE. A 36 px button in a full-height pane: the frame is an order of
  //    magnitude taller than its content, which is only true because it stopped auto-sizing to it.
  expect(box.height).toBeGreaterThan(200);
  expect(box.width).toBeGreaterThan(200);
  const pane = frame.parentElement!.parentElement!;
  expect(Math.abs(box.height - pane.getBoundingClientRect().height)).toBeLessThanOrEqual(2);

  // 2. …and NO container between the frame and the page paints a literal white plate.
  for (let el: HTMLElement | null = frame; el !== null; el = el.parentElement) {
    expect(el.className).not.toContain("bg-white");
  }

  const mirror = mirrorAt(frame.getAttribute("srcdoc") ?? "", box.width, box.height);
  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      const win = mirror.contentWindow;
      const probe = doc?.getElementById("probe");
      expect(probe).toBeTruthy();
      // The shim ran, so this is the FINAL state of the document (chrome included).
      expect(doc?.body.dataset.nyxState).toBe("default");

      // 3. THE CANVAS IS THE PROJECT'S — the `--background` token, never white.
      const bodyStyle = win!.getComputedStyle(doc!.body);
      expect(bodyStyle.backgroundColor).toBe("rgb(11, 15, 25)");
      expect(bodyStyle.color).toBe("rgb(226, 232, 240)");

      // 4. CENTRED on both axes of the real pane (the whole point of the report).
      const rect = probe!.getBoundingClientRect();
      expect(Math.abs(rect.left + rect.width / 2 - box.width / 2)).toBeLessThanOrEqual(2);
      expect(Math.abs(rect.top + rect.height / 2 - box.height / 2)).toBeLessThanOrEqual(2);

      // 5. …and the DETAIL keeps its interaction chrome: the dashed affordances stay (the vignette
      //    is what drops them). Nothing of this finding touched that.
      expect(doc?.documentElement.outerHTML).toContain("dashed");
    },
    { timeout: 10000, interval: 100 },
  );
});

/**
 * The finding's explicit caveat: "un composant PLEINE PAGE (un dialog) doit rester lisible — le
 * centrage ne doit pas le rogner". `safe center` is the mechanism — centred WHILE IT FITS, aligned
 * to the START as soon as it does not — so an oversized instance is never cut on BOTH sides: its
 * top-left (the title, the first control) is exactly where a page would put it.
 */
it("does not crop a FULL-PAGE component: safe center falls back to the start edge", async () => {
  await page.viewport(...VIEWPORT);
  currentComponent = DIALOG;
  installBackend();
  render(<RouterProvider router={createAppRouter(["/design/c1"])} />);

  const frame = await realFrame();
  const box = frame.getBoundingClientRect();
  const mirror = mirrorAt(frame.getAttribute("srcdoc") ?? "", box.width, box.height);

  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      const probe = doc?.getElementById("probe");
      const title = doc?.getElementById("dialog-title");
      expect(probe).toBeTruthy();
      expect(title).toBeTruthy();

      // The 1600 × 1400 instance overflows the pane, so `safe center` START-aligns it: its own
      // origin is at the frame's padding, NOT at a negative offset (which is what a plain `center`
      // would do — losing the title on the left and the top).
      const rect = probe!.getBoundingClientRect();
      expect(rect.left).toBeGreaterThanOrEqual(0);
      expect(rect.top).toBeGreaterThanOrEqual(0);
      // …so the dialog's own heading is INSIDE the visible box, i.e. still readable.
      const t = title!.getBoundingClientRect();
      expect(t.left).toBeGreaterThanOrEqual(0);
      expect(t.top).toBeGreaterThanOrEqual(0);
      expect(t.left).toBeLessThan(box.width);
      expect(t.top).toBeLessThan(box.height);

      // And the frame never SCROLLS to accommodate it: the pane keeps its exact size (the clip is
      // the same guarantee the tile has), so the 1600 px never widen the viewport.
      expect(Math.abs((doc?.documentElement.clientWidth ?? 0) - box.width)).toBeLessThanOrEqual(1);
      expect(doc?.body.getBoundingClientRect().width).toBeLessThanOrEqual(box.width + 0.5);
    },
    { timeout: 10000, interval: 100 },
  );
});

/**
 * THE VARIANT OF THE DESIGN SYSTEM THAT IS ON SCREEN (finding `01KYSVEZMJP1Z8V7SYJ8YC5H9W`).
 *
 * The report, made three times, was "l'aperçu est blanc dans une app sombre". It was NOT a
 * hard-coded background — the previous passes removed those. The layer carried BOTH variants all
 * along (sandbox: 103 light variables, 48 dark ones) and the frame emitted BOTH blocks; nothing ever
 * marked the frame's document dark, so `:root` — the light half — was the one that applied.
 *
 * Only a browser settles it: `:root` and `.dark` have the SAME specificity, so which one wins is a
 * CASCADE fact, not a string fact. Chromium is asked what it actually computed for the canvas.
 */
it("follows the app's appearance: the canvas is the project's OWN dark token", async () => {
  await page.viewport(...VIEWPORT);
  currentTokens = TOKENS_LIGHT_AND_DARK;
  installBackend();
  // nyx's own appearance — the single switch the preview follows (`index.html` ships `.dark`).
  document.documentElement.classList.add("dark");
  render(<RouterProvider router={createAppRouter(["/design/c1"])} />);

  const frame = await realFrame();
  const srcdoc = frame.getAttribute("srcdoc") ?? "";
  // The document is MARKED — and both of the project's blocks are still there, verbatim.
  expect(srcdoc).toContain(`<html lang="en" class="dark">`);
  expect(srcdoc).toContain("oklch(99.107% 0.00011 271.152)");
  expect(srcdoc).toContain("oklch(18% 0.00411 285.902)");

  const box = frame.getBoundingClientRect();
  const mirror = mirrorAt(srcdoc, box.width, box.height);
  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      const win = mirror.contentWindow;
      expect(doc?.getElementById("probe")).toBeTruthy();
      const bodyStyle = win!.getComputedStyle(doc!.body);
      // THE DARK VARIANT, computed. `oklch(18% …)` is a very dark surface; the light one
      // (`oklch(99.107% …)`) is all but white. Assert the LIGHTNESS Chromium resolved, so a cascade
      // regression cannot hide behind a class that is present but overruled.
      expect(lightnessOf(bodyStyle.backgroundColor)).toBeLessThan(0.3);
      // …and the text follows the same block, so the instance stays readable ON that canvas.
      expect(lightnessOf(bodyStyle.color)).toBeGreaterThan(0.7);
      // The variant is real, so nothing claims otherwise beside the preview.
      expect(document.querySelector('[data-testid="theme-variant-notice"]')).toBeNull();
    },
    { timeout: 10000, interval: 100 },
  );
});

/**
 * THE OTHER HALF, and the one that must never lie: a layer WITHOUT a dark map.
 *
 * There is no dark variant to select — the design system only ever defined the light one. Painting a
 * dark canvas under it would put the component's own dark text on a dark surface, i.e. destroy the
 * very thing the preview exists for; inventing dark values would stop showing the design system. So
 * the frame renders the light variant, unchanged and faithful, and the page SAYS SO next to the
 * `Showing` caption. What is forbidden is the third option: rendering light in silence.
 */
it("a light-only layer keeps its light variant — and the page says so", async () => {
  await page.viewport(...VIEWPORT);
  currentTokens = TOKENS; // light variables, `dark: {}` — nothing to select.
  installBackend();
  document.documentElement.classList.add("dark");
  render(<RouterProvider router={createAppRouter(["/design/c1"])} />);

  const frame = await realFrame();
  const srcdoc = frame.getAttribute("srcdoc") ?? "";
  expect(srcdoc).not.toContain(`class="dark"`);
  expect(srcdoc).not.toContain("color-scheme:dark");

  // The rendering is NAMED, where the rendering is claimed.
  const notice = await screen.findByTestId("theme-variant-notice");
  expect(notice.textContent?.toLowerCase()).toContain("light variant");
  expect(notice.getAttribute("title") ?? "").toContain("dark");

  const box = frame.getBoundingClientRect();
  const mirror = mirrorAt(srcdoc, box.width, box.height);
  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      const win = mirror.contentWindow;
      expect(doc?.getElementById("probe")).toBeTruthy();
      // Exactly the tokens the project declares — nothing repainted over them.
      const style = win!.getComputedStyle(doc!.body);
      expect(style.backgroundColor).toBe("rgb(11, 15, 25)");
      expect(style.color).toBe("rgb(226, 232, 240)");
    },
    { timeout: 10000, interval: 100 },
  );
});

/**
 * THE PREVIEW MAY NEVER GROW PAST ITS PANE (finding `01KYSVFKJTYF7NY5BNGMCJX4JE`).
 *
 * The instance came back OFF-CENTRE — horizontally only, vertically fine. Measured in Chromium, the
 * frame itself was innocent: its body, its `#depot-root` and the instance were centred to the pixel
 * (ratio 0.500 on raw screenshot pixels, at 1887 px and at 2560 px). The drift was the HOST's. The
 * preview column is the `1fr` cell of `grid-cols-[minmax(18rem,22rem)_1fr]`, and with
 * `min-width:auto` a grid cell's floor is its MIN-CONTENT — here the `Showing` caption, which is
 * `truncate`, i.e. `white-space:nowrap`. A long caption became that floor: at a 1560 px viewport the
 * track blew from 1084 px to 1534.8 px, the pane ran 387 px PAST the window's right edge, and the
 * instance — still perfectly centred in a frame WIDER THAN WHAT IS VISIBLE — sat 193 px right of the
 * visible middle, vertically untouched. Exactly the reported picture.
 *
 * Nothing reported it: `globals.css` clips the document (`html,body,#root{overflow-x:clip}`), which
 * kills the scrollbar AND clamps `scrollWidth`, so the overflow left no trace whatsoever. Hence this
 * test measures the BOX, not a scroll metric: the pane must stay inside the window and the frame
 * inside the pane. It BITES — drop `min-w-0` from the preview column and it fails.
 *
 * WHAT MAKES THE CAPTION THE FLOOR changed twice. Finding `01KYT10VE8624MKQ18AFHSE0W0` took the
 * usages off this page, so the caption named the COMPONENT — as unbounded as a `node_key`, since a
 * name comes from somebody else's source tree. Finding `01KZ3JW5A63TJWX83V8ZKX62CN` then took the
 * NAME out of the caption too (it was the third statement of it inside 200 px), so the caption is a
 * FIXED sentence now — but it is still `truncate`, i.e. `white-space:nowrap`, and it is still the
 * min-content of the `1fr` cell. What breaks the layout is the RATIO between that min-content and
 * the track it becomes the floor of, never an absolute count of pixels: the viewport is therefore
 * narrowed to the width where the fixed sentence alone blows the track out. Drop `min-w-0` from the
 * preview column and this fails, exactly as it did on a 1560 px window with a long name.
 */
it("keeps the preview inside its pane (and the instance centred) under a nowrap caption", async () => {
  await page.viewport(700, 950);
  currentComponent = {
    ...BASE,
    // A component name is whatever the inventory found on disk — nothing bounds it. It no longer
    // reaches the caption, and it may not widen anything from the title either.
    name: "DashboardOperationsConsolidatedFiltersActionBarSubmitInnerButtonSurface",
  };
  installBackend();
  render(<RouterProvider router={createAppRouter(["/design/c1"])} />);

  const frame = await realFrame();
  // The caption is REALLY on screen — otherwise this test would pass on the wrong page.
  const scope = await screen.findByTestId("preview-scope");
  await vi.waitFor(
    () => expect(scope.textContent ?? "").toContain("the component alone, on its preview values"),
    { timeout: 10000, interval: 100 },
  );

  const box = frame.getBoundingClientRect();
  const pane = frame.parentElement!.parentElement!;
  const paneBox = pane.getBoundingClientRect();

  // 1. THE PANE STAYS IN THE WINDOW. This is the assertion the blowout fails: the pane's right edge
  //    ran to 1922.8 in a 1560 px window, and `overflow-x: clip` made that invisible.
  expect(paneBox.right).toBeLessThanOrEqual(window.innerWidth + 1);
  expect(paneBox.left).toBeGreaterThanOrEqual(0);
  // 2. …and the FRAME stays in the pane, so the box the instance centres in IS the box one sees.
  expect(box.right).toBeLessThanOrEqual(paneBox.right + 1);
  expect(box.width).toBeLessThanOrEqual(paneBox.width + 1);

  // 3. …so the instance is centred where the user looks: in the VISIBLE pane, on both axes.
  const mirror = mirrorAt(frame.getAttribute("srcdoc") ?? "", box.width, box.height);
  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      const probe = doc?.getElementById("probe");
      expect(probe).toBeTruthy();
      const rect = probe!.getBoundingClientRect();
      const centreInWindow = box.left + rect.left + rect.width / 2;
      const paneCentre = paneBox.left + paneBox.width / 2;
      expect(Math.abs(centreInWindow - paneCentre)).toBeLessThanOrEqual(2);
      expect(Math.abs(rect.top + rect.height / 2 - box.height / 2)).toBeLessThanOrEqual(2);
    },
    { timeout: 10000, interval: 100 },
  );
});
