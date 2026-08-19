import { readdirSync, readFileSync } from "node:fs";
import { join, relative, resolve } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * THE ONE VOCABULARY FOR "FULL HEIGHT" (review finding `01M0524Q2DFWKNDC6HQQ2BGFDE`).
 *
 * The window is frameless and draws its own chrome strip (`<ChromeBar>`), so the height a routed
 * page may occupy is the viewport MINUS that bar. That subtraction is stated ONCE, as a named
 * project utility — `@utility h-app` in `globals.css` — and every full-height surface under the
 * shell claims it.
 *
 * It used to be expressed by RE-DEFINING Tailwind's own `h-svh` in an unlayered rule. That worked
 * (an unlayered rule beats `@layer utilities` at any specificity) but it was undetectable and
 * partial: the utility's VARIANTS (`md:h-svh`, `group-hover:h-svh` — emitted under their own
 * selectors) and the rest of its FAMILY (`min-h-svh`, `h-dvh`, `h-screen`) kept meaning the FULL
 * viewport, so the same name would have stood for two heights depending on whether a variant was
 * attached, and nothing at the ~20 call-sites said which. Neither `tsc` nor `oxlint` can see that.
 *
 * This test is what sees it. It is a lint the suite carries, because the rule it enforces is a
 * layout rule: UNDER THIS SHELL, "full height" IS `h-app`. A viewport-height utility coming back
 * (by copy-paste from any other codebase, which is how the first one arrived) makes exactly one
 * page one bar too tall — a defect that shows up as a scrollbar on the WHOLE document and hides
 * that page's last rows below the fold.
 *
 * MUTATION-CHECKED: put `h-svh`, `md:h-svh`, `min-h-screen` or `h-dvh` in any renderer source and
 * this test names the file and the line; drop the `@utility h-app` declaration and it says so.
 */

const SRC = resolve(__dirname);
const GLOBALS_CSS = resolve(SRC, "globals.css");

/**
 * The ONLY boxes allowed to span the FULL window (bar INCLUDED) are the two shell roots, because
 * they are the ones that DRAW the chrome bar. They say so with the arbitrary value `h-[100svh]`,
 * never with `h-svh`/`h-screen` — the point being that a reader sees an exception, spelled out.
 */
const FULL_WINDOW_ROOTS = [
  "components/terminals/base-layout.tsx",
  "components/sidebar/terminal-manager.tsx",
];

/** Every `.ts`/`.tsx` of the renderer that is PRODUCT code (a test may assert on any class name). */
function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      sourceFiles(path, out);
      continue;
    }
    if (!/\.tsx?$/.test(entry.name)) continue;
    if (/\.(test|spec)\.tsx?$/.test(entry.name)) continue;
    if (entry.name.endsWith(".d.ts")) continue;
    out.push(path);
  }
  return out;
}

/** `file:line` for every line of `files` matching `pattern`, ignoring the ones inside a comment. */
function offenders(pattern: RegExp, files: readonly string[]): string[] {
  const hits: string[] = [];
  for (const file of files) {
    const rel = relative(SRC, file).replace(/\\/g, "/");
    // `\r?\n`, not `\n`: a CRLF checkout would leave the `\r` at the end of every line, and a
    // trailing `\r` is a line terminator to a JS regex — `//.*$` would then match NOTHING and the
    // comment-stripping below would silently stop working (this very guard caught it).
    const lines = readFileSync(file, "utf8").split(/\r?\n/);
    lines.forEach((line, i) => {
      // A COMMENT may NAME the rule — globals.css's is quoted in a few places on purpose.
      const code = line.replace(/\/\/.*$/, "").replace(/^\s*\*.*$/, "");
      if (pattern.test(code)) hits.push(`${rel}:${i + 1}: ${line.trim()}`);
    });
  }
  return hits;
}

const FILES = sourceFiles(SRC);

describe("full height under the chrome bar is spelled h-app", () => {
  /** There IS product source to scan — a broken walk must not pass by finding nothing. */
  it("scans the renderer's own sources", () => {
    expect(FILES.length).toBeGreaterThan(100);
    expect(FILES.map((f) => relative(SRC, f).replace(/\\/g, "/"))).toContain("router.tsx");
  });

  /**
   * The declaration itself: `@utility` (Tailwind v4's extension point) and not a raw `.h-svh` rule
   * outside every layer. `@utility` is what makes the VARIANTS come from this same declaration, and
   * what removes the dependency on an unlayered rule still winning after a Tailwind upgrade.
   */
  it("globals.css declares h-app as a Tailwind utility, and re-defines no third-party one", () => {
    const css = readFileSync(GLOBALS_CSS, "utf8");
    expect(css).toMatch(/@utility h-app \{\s*height: calc\(100svh - var\(--chrome-bar-h\)\);\s*\}/);
    // No rule re-defining a viewport-height utility of Tailwind's own (layered or not).
    const redefinitions = css
      .split(/\r?\n/)
      .map((line, i) => [line, i + 1] as const)
      .filter(([line]) => /^\s*\.(?:min-|max-)?h-(?:svh|dvh|lvh|screen)\s*[,{]/.test(line))
      .map(([line, n]) => `globals.css:${n}: ${line.trim()}`);
    expect(redefinitions).toEqual([]);
  });

  /**
   * And the call-sites. `h-svh`/`h-dvh`/`h-lvh`/`h-screen` — with or without a variant prefix, in
   * their `min-`/`max-` forms too — all mean the FULL viewport, which under this shell is one
   * chrome bar too tall for anything but the shell root.
   */
  it("no surface sizes itself to the full viewport", () => {
    expect(offenders(/\b(?:min-|max-)?h-(?:svh|dvh|lvh|screen)\b/, FILES)).toEqual([]);
  });

  /**
   * The escape hatch stays where it is understood: `h-[100svh]` is the FULL window (bar included),
   * which is true of the shell roots and of nothing else. A page claiming it would be exactly one
   * bar too tall — the very defect `h-app` exists to prevent.
   */
  it("only the shell roots claim the full window", () => {
    const claimers = offenders(/h-\[100svh\]/, FILES).map((hit) => hit.split(":")[0]);
    expect([...new Set(claimers)].sort()).toEqual([...FULL_WINDOW_ROOTS].sort());
  });
});
