import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { GROUP_COLOR_COUNT, IDENTITY_COLOR_COUNT } from "./pane-groups";

/**
 * THE CATEGORICAL IDENTITY PALETTE (review 01M0ABR6V501D0SPR2BYBVZC4A, major; widened to twelve by
 * ADR-0012).
 *
 * A peer identity's optional colour needed tokens this project did not have: its six colours are
 * SEMANTIC (`--primary`, `--destructive`, `--success`, `--warning`, `--info`, `--branch`) and stay
 * reserved — a red group would read as an error — and `--chart-1..5` are greyscale, so they cannot
 * tell two peers apart. These specs pin the palette that was created, straight out of
 * `globals.css`: a token that drifts is a group — or a project — that changes colour under the
 * user.
 */
/** The renderer's source root — this spec reads the REAL stylesheet, not a copy of its values. */
const SRC = join(process.cwd(), "src");
const CSS = readFileSync(join(SRC, "globals.css"), "utf8");

/** The DECLARED text of a custom property (its FIRST declaration — the `:root` one). */
function declaration(name: string): string {
  const match = CSS.match(new RegExp(`^\\s*${name}:\\s*([^;]+);`, "m"));
  expect(match, `${name} must be declared in globals.css`).not.toBeNull();
  return (match as RegExpMatchArray)[1].trim();
}

/**
 * What a token RESOLVES to, FOLLOWING `var(--…)` aliases. Since ADR-0012 the palette is declared
 * once as `--identity-N` and `--group-N` is an alias of it, so a spec that stopped at the
 * declaration would read `var(--identity-3)` and never check the colour a group actually gets. The
 * hop is what keeps "a group already coloured keeps exactly its colour" a MEASURED property rather
 * than a promise — and what would catch an alias silently re-pointed at another hue.
 */
function token(name: string): string {
  let value = declaration(name);
  for (let hop = 0; hop < 4; hop += 1) {
    const alias = value.match(/^var\(\s*(--[a-z0-9-]+)\s*\)$/i);
    if (!alias) return value;
    value = declaration(alias[1]!);
  }
  throw new Error(`${name} resolves through more than four aliases — that is a loop, not a rename`);
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
const IDENTITY_TOKENS = Array.from(
  { length: IDENTITY_COLOR_COUNT },
  (_, i) => `--identity-${i + 1}`,
);
const SEMANTIC_TOKENS = [
  "--primary",
  "--destructive",
  "--success",
  "--warning",
  "--info",
  "--branch",
];

describe("the six --group-N tokens (through the alias, since ADR-0012)", () => {
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

/**
 * THE SIBLING SPEC, AND THE REASON IT HAD TO EXIST. The block above derives its token names from
 * `GROUP_COLOR_COUNT`, which since ADR-0012 is the command groups' BOUND, not the palette's size:
 * left alone it would stay green while `--identity-7..12` — the six hues a PROJECT can pick and no
 * group can — went unchecked. Green would have stopped meaning "palette verified".
 */
describe("the twelve --identity-N tokens", () => {
  it("exist with the arbitrated values, the historic six FIRST and unmoved", () => {
    expect(IDENTITY_TOKENS.map(token)).toEqual([
      // 1..6 — arbitrated on the mockups, and they do not move by a single degree: a group already
      // coloured (in SQLite or in localStorage) keeps exactly its colour.
      "oklch(0.76 0.09 322)",
      "oklch(0.76 0.09 232)",
      "oklch(0.76 0.09 152)",
      "oklch(0.76 0.09 62)",
      "oklch(0.76 0.09 12)",
      "oklch(0.76 0.09 272)",
      // 7..12 — interleaved into the widest gaps by ADR-0012, DECLARED out of hue order so two
      // adjacent swatches in the picker row are never two adjacent hues.
      "oklch(0.76 0.09 37)",
      "oklch(0.76 0.09 192)",
      "oklch(0.76 0.09 297)",
      "oklch(0.76 0.09 92)",
      "oklch(0.76 0.09 347)",
      "oklch(0.76 0.09 122)",
    ]);
  });

  it("hold ONE lightness and ONE chroma across the twelve — only the hue varies", () => {
    for (const name of IDENTITY_TOKENS) {
      const { l, c } = oklch(name);
      expect(l, `${name} lightness`).toBe(0.76);
      expect(c, `${name} chroma`).toBe(0.09);
    }
  });

  it("spreads its twelve hues at least 25° apart — the demonstrated ceiling for twelve", () => {
    const hues = IDENTITY_TOKENS.map((name) => oklch(name).h);
    expect(new Set(hues).size).toBe(IDENTITY_COLOR_COUNT);
    // WHY 25° AND NOT THE 40° THE SIX HAD: the six existing hues are frozen, so the six new ones
    // may only be inserted into the gaps they leave. ADR-0012 proves 25° is the MAXIMUM smallest
    // gap reachable that way — above it (26.67°) the available gaps absorb four insertions, not
    // six. It is a CEILING, not a comfort: a thirteenth hue would have to change dimension (shape,
    // glyph, pattern), because raising the chroma is forbidden by the alert reserve below and
    // raising the lightness contrast is forbidden by equiluminance.
    const sorted = [...hues].sort((a, b) => a - b);
    const gaps = sorted.map((h, i) => hueGap(h, sorted[(i + 1) % sorted.length]));
    expect(Math.min(...gaps)).toBeGreaterThanOrEqual(25);
  });

  it("keeps every hue OFF the alert register — chroma under the least saturated semantic token", () => {
    const floor = Math.min(...SEMANTIC_TOKENS.map((name) => oklch(name).c));
    for (const name of IDENTITY_TOKENS) {
      expect(oklch(name).c, `${name} chroma`).toBeLessThan(floor);
    }
  });

  it("is declared ONCE, not per theme — a token reads the same in light and in dark", () => {
    // The tone was chosen to read on both, so there is no `.dark` override; an accent painted
    // `var(--identity-3)` is therefore the SAME hue whichever theme is on. A redeclaration would
    // silently make it two colours — and `token()` above, which reads the FIRST declaration,
    // would keep saying it is one.
    const darkBlock = CSS.slice(CSS.indexOf(".dark {"));
    for (const name of ["--identity-0", ...IDENTITY_TOKENS]) {
      expect(darkBlock, `${name} must not be re-declared per theme`).not.toContain(`${name}:`);
    }
  });

  it("declares the six NEW tokens in big steps, not in hue order", () => {
    // The declaration ORDER is a property of its own: the picker paints the tokens in index order,
    // so two consecutive indices must not be two neighbouring hues — otherwise the two closest
    // colours of the palette end up side by side in the row, which is the worst possible place for
    // them. ADR-0012 chose the order 37 / 192 / 297 / 92 / 347 / 122 for exactly that, and the
    // step never falls below 105° from `--identity-6` onwards (the junction included). The six
    // HISTORIC tokens are not held to it: their order is frozen, and 62 → 12 is a 50° step.
    const tail = IDENTITY_TOKENS.slice(GROUP_COLOR_COUNT - 1).map((name) => oklch(name).h);
    const steps = tail.slice(1).map((h, i) => hueGap(h, tail[i]!));
    expect(Math.min(...steps)).toBeGreaterThanOrEqual(105);
  });
});

describe("the --group-N aliases stop at the command groups' bound", () => {
  it("aliases exactly 0..GROUP_COLOR_COUNT and NOT one token further", () => {
    for (let n = 0; n <= GROUP_COLOR_COUNT; n += 1) {
      expect(declaration(`--group-${n}`)).toBe(`var(--identity-${n})`);
    }
    // The alias is the command groups' VIEW of the palette, and their SQL CHECK is `1..=6`.
    // Aliasing a seventh would offer a group a colour its own write path refuses.
    expect(CSS).not.toContain(`--group-${GROUP_COLOR_COUNT + 1}:`);
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
