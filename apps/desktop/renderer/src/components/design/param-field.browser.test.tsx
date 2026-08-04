// The REAL app CSS: only a browser applying the TRUE Tailwind utilities (`flex-wrap`, `min-w-0`,
// `w-full`, `truncate`) can show whether a control overflows its panel. `vitest.config.ts` wires
// `@tailwindcss/vite`, so this `@import "tailwindcss"` genuinely compiles.
import "@/globals.css";

import { render } from "@testing-library/react";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { ParamField } from "./param-field";
import type { ParamDescriptor } from "./params";

/**
 * BROWSER-MODE overflow guard for the params panel of the composant detail (finding
 * `01KYPJ1SCHGZ2STSXZ4VCR1K33`).
 *
 * The user's report was a MEASUREMENT, not a matter of taste: "le truc est tellement long que ça
 * rajoute un scroll horizontal". The `state` param's options come from the SYNTHESIS — a `button`
 * declares 8 CVA variants, a `badge` 13 — and a one-row segmented control of them was wider than the
 * ~20 rem side panel, which scrolls on `overflow-y-auto` (a scroll container computes the OTHER axis
 * to `auto` too). jsdom has no layout, so only a real browser can pin this: the panel is reproduced
 * at its real width and its `scrollWidth` is compared to its `clientWidth`, for option counts well
 * past anything a real design system declares.
 */

/** The composant detail's left column: a fixed-width, vertically scrolling panel. */
function panelOf(node: ReactElement): HTMLElement {
  const panel = document.createElement("div");
  // `w-80` = 20 rem, inside the page's `minmax(18rem,22rem)` grid track.
  panel.className = "flex min-h-0 w-80 flex-col gap-4 overflow-y-auto pr-1";
  document.body.appendChild(panel);
  render(node, { container: panel });
  return panel;
}

/** N option names as long as the real ones (`ghost-destructive` is 17 characters). */
function options(n: number): string[] {
  const real = [
    "default",
    "destructive",
    "outline",
    "secondary",
    "accent",
    "ghost",
    "ghost-destructive",
    "link",
  ];
  return Array.from({ length: n }, (_v, i) => real[i % real.length] + (i < real.length ? "" : i));
}

function stateDescriptor(n: number): ParamDescriptor {
  return { name: "state", type: "state", options: options(n) };
}

afterEach(() => {
  document.body.replaceChildren();
});

describe("ParamField — the enumerated control never scrolls its panel sideways", () => {
  // 3 = a wrapped segmented picker; 8 = the reported case; 15 / 30 = well past any real synthesis.
  for (const count of [3, 8, 15, 30]) {
    it(`fits ${count} declared states inside the panel`, () => {
      const panel = panelOf(
        <ParamField descriptor={stateDescriptor(count)} value="default" onChange={() => {}} />,
      );

      // THE assertion: the panel has nothing to scroll horizontally…
      expect(panel.scrollWidth).toBeLessThanOrEqual(panel.clientWidth);
      // …and it did not push the overflow up to the document either.
      expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(
        document.documentElement.clientWidth,
      );
    });
  }

  /**
   * The two shapes of the control, measured: a SMALL option set stays a segmented picker (every
   * option visible, one click away) and simply WRAPS onto several rows; a LARGE one becomes a
   * dropdown, whose width is independent of the number of options.
   */
  it("wraps a small option set and switches to a dropdown for a large one", () => {
    // Six options whose names alone are ~6 × 110 px: a single row could not hold them in 20 rem.
    const long: ParamDescriptor = {
      name: "state",
      type: "state",
      options: Array.from({ length: 6 }, (_v, i) => `ghost-destructive-${i}`),
    };
    const wrapped = panelOf(
      <ParamField descriptor={long} value="ghost-destructive-0" onChange={() => {}} />,
    );
    const group = wrapped.querySelector('[role="group"]') as HTMLElement;
    expect(group).toBeTruthy();
    expect(getComputedStyle(group).flexWrap).toBe("wrap");
    // Wrapped = taller than one row of buttons, and never wider than the panel.
    const row = group.querySelector("button") as HTMLElement;
    expect(group.getBoundingClientRect().height).toBeGreaterThan(
      row.getBoundingClientRect().height + 1,
    );
    expect(group.scrollWidth).toBeLessThanOrEqual(group.clientWidth);

    const dropdown = panelOf(
      <ParamField descriptor={stateDescriptor(30)} value="default" onChange={() => {}} />,
    );
    const select = dropdown.querySelector("select") as HTMLSelectElement;
    expect(select).toBeTruthy();
    expect(select.options.length).toBe(30);
    expect(dropdown.querySelector('[role="group"]')).toBeNull();
    // A dropdown for 30 options is no wider than the panel that holds it.
    expect(select.getBoundingClientRect().width).toBeLessThanOrEqual(
      dropdown.getBoundingClientRect().width,
    );
  });
});
