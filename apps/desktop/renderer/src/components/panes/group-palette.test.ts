import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { GROUP_COLOR_COUNT } from "./pane-groups";

/**
 * THE CATEGORICAL GROUP PALETTE (review 01M0ABR6V501D0SPR2BYBVZC4A, major).
 *
 * A group's optional colour needed six tokens this project did not have: its six colours are
 * SEMANTIC (`--primary`, `--destructive`, `--success`, `--warning`, `--info`, `--branch`) and stay
 * reserved — a red group would read as an error — and `--chart-1..5` are greyscale, so they cannot
 * tell two groups apart. These specs pin the palette that was created, straight out of
 * `globals.css`: a token that drifts is a group that changes colour under the user.
 */
/** The renderer's source root — this spec reads the REAL stylesheet, not a copy of its values. */
const SRC = join(process.cwd(), "src");
const CSS = readFileSync(join(SRC, "globals.css"), "utf8");

/** The declared value of a custom property (its FIRST declaration — the `:root` one). */
function token(name: string): string {
  const match = CSS.match(new RegExp(`^\\s*${name}:\\s*([^;]+);`, "m"));
  expect(match, `${name} must be declared in globals.css`).not.toBeNull();
  return (match as RegExpMatchArray)[1].trim();
}

/** The `oklch(L C H)` triple of a token. */
function oklch(name: string): { l: number; c: number; h: number } {
  const value = token(name);
  const match = value.match(/^oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*\)$/);
  expect(match, `${name} must be a plain oklch triple (got ${value})`).not.toBeNull();
  const [, l, c, h] = match as RegExpMatchArray;
  return { l: Number(l), c: Number(c), h: Number(h) };
}

/** The shortest distance between two hues on the 360° circle. */
function hueGap(a: number, b: number): number {
  const raw = Math.abs(a - b) % 360;
  return Math.min(raw, 360 - raw);
}

const GROUP_TOKENS = Array.from({ length: GROUP_COLOR_COUNT }, (_, i) => `--group-${i + 1}`);
const SEMANTIC_TOKENS = [
  "--primary",
  "--destructive",
  "--success",
  "--warning",
  "--info",
  "--branch",
];

describe("the six --group-N tokens", () => {
  it("exist with the arbitrated values", () => {
    expect(GROUP_TOKENS.map(token)).toEqual([
      "oklch(0.76 0.09 322)",
      "oklch(0.76 0.09 232)",
      "oklch(0.76 0.09 152)",
      "oklch(0.76 0.09 62)",
      "oklch(0.76 0.09 12)",
      "oklch(0.76 0.09 272)",
    ]);
  });

  it("hold ONE lightness and ONE chroma across the six — only the hue varies", () => {
    for (const name of GROUP_TOKENS) {
      const { l, c } = oklch(name);
      expect(l, `${name} lightness`).toBe(0.76);
      expect(c, `${name} chroma`).toBe(0.09);
    }
  });

  it("spreads its six hues far enough apart to tell two groups apart", () => {
    const hues = GROUP_TOKENS.map((name) => oklch(name).h);
    expect(new Set(hues).size).toBe(GROUP_COLOR_COUNT);
    // The arbitrated hues are 12 / 62 / 152 / 232 / 272 / 322; sorted, their smallest gap on the
    // circle is 40°. (The finding's own wording said "60° apart", which the values it MANDATES do
    // not satisfy — the values are the ones validated on the mockups, so they win, and what is
    // pinned here is the property they really have.)
    const sorted = [...hues].sort((a, b) => a - b);
    const gaps = sorted.map((h, i) => hueGap(h, sorted[(i + 1) % sorted.length]));
    expect(Math.min(...gaps)).toBeGreaterThanOrEqual(40);
  });

  it("is HALF as saturated as every semantic token — that gap is what keeps a group off the alert register", () => {
    const semanticChromas = SEMANTIC_TOKENS.map((name) => oklch(name).c);
    expect(Math.min(...semanticChromas)).toBeGreaterThan(0.09);
    for (const name of GROUP_TOKENS) {
      expect(oklch(name).c).toBeLessThan(Math.min(...semanticChromas));
    }
  });

  it("is a palette the project did not already have — `--chart-1..5` are greyscale", () => {
    for (const chart of ["--chart-1", "--chart-2", "--chart-3", "--chart-4", "--chart-5"]) {
      expect(oklch(chart).c, `${chart} carries no hue`).toBe(0);
    }
  });
});

describe("--group-0, the DEFAULT that is not a colour", () => {
  it("is the design system's existing neutral, oklch(0.556 0 0) — and NOT --primary", () => {
    expect(token("--group-0")).toBe("oklch(0.556 0 0)");
    expect(token("--group-0")).not.toBe(token("--primary"));
    // It is literally the value `--ring` carries in the dark theme (the neutral the finding names).
    const darkBlock = CSS.slice(CSS.indexOf(".dark {"));
    expect(darkBlock).toContain("--ring: oklch(0.556 0 0);");
  });

  it("carries no hue at all, so an uncoloured group cannot read as a coloured one", () => {
    expect(oklch("--group-0").c).toBe(0);
  });
});

describe("no semantic token ever colours a group", () => {
  // The block itself is the NEUTRAL brick (`components/panes/`, shared by terminals and
  // commands); the row that wears its tint in the sidebar list stays with the terminals surface.
  const sources = [
    join(SRC, "components", "panes", "group-block.tsx"),
    join(SRC, "components", "terminals", "sortable-terminal-rows.tsx"),
  ].map((file) => readFileSync(file, "utf8"));

  it("the block's tint comes from --group-N only", () => {
    for (const source of sources) {
      for (const semantic of SEMANTIC_TOKENS) {
        expect(source, `${semantic} must not tint a group`).not.toContain(semantic);
      }
    }
  });

  it("and there is no raw colour in the block either", () => {
    for (const source of sources) {
      expect(source).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
      expect(source).not.toMatch(/\brgba?\(/);
      expect(source).not.toMatch(/\bhsla?\(/);
    }
  });
});
