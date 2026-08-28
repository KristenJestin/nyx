import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { act } from "@testing-library/react";
import { createElement } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it } from "vitest";

import { Spinner } from "./components/ui/spinner";

/**
 * THE THREE SURFACES THAT PAINT THE START-UP, AND THE ONE COLOUR THEY OWE EACH OTHER.
 *
 * Between the moment Electron shows the window and the moment React commits its first tree, the
 * user looks at three things in a row: the native window's `backgroundColor`, the static boot
 * splash in `index.html`, and the first React screen. No component can cover the first two — React
 * does not exist yet — so they are written by hand, in literals, in two files that know nothing
 * about `globals.css`. That is a KNOWING duplication of the dark theme's `--background`, and the
 * kind that rots silently: the day the token moves, nothing breaks, the window just flashes.
 *
 * This test is what breaks. It converts the theme tokens out of `globals.css` itself (oklch → sRGB
 * hex, the same conversion the browser does) and holds the two hand-written literals against the
 * result — so the failure names the colour the theme now wants, not merely that something differs.
 *
 * It also holds the shape the splash depends on:
 *   - the markup sits INSIDE `#root`, which is what lets `createRoot().render()` clear it in the
 *     same commit that inserts the first React tree (outside `#root` it would survive the mount and
 *     stay on screen over the app);
 *   - the bundle stays a DEFERRED module script, or it runs before the document is painted and the
 *     splash is not what the first paint shows;
 *   - it fetches NOTHING — a request added here lengthens exactly the wait it exists to cover;
 *   - the window is still created hidden and shown on `ready-to-show`, the thing that keeps the
 *     white flash away.
 *
 * MUTATION-CHECKED: change either literal, move `--background` in the `.dark` block, take the
 * splash out of `#root`, delete its markup, drop `position: fixed`, put a `<link>`/`<img>` inside
 * it, load the bundle as a classic script, drop `show: false` / `win.show()` — including by
 * COMMENTING it out — RENAME either CSS selector, or swap the spinner for a static glyph, and one
 * of the assertions below names it.
 *
 * WHY THE CSS IS READ AS RULES AND NOT AS TEXT. An earlier version of this file grepped the `<style>`
 * block for `position: fixed` / `background-color: var(…)` without ever tying a declaration to the
 * selector that applies it. Measured: renaming `#nyx-boot-splash` to `#nyx-boot-splash-dead` left
 * SEVEN tests green while the splash became an unpositioned, uncoloured 24px glyph in the top-left
 * corner — the whole shipped capability gone; renaming `html.dark` to `.nyx-unused` left it green
 * while the document canvas stopped being painted at all. So the sheet is parsed into
 * (selector → declarations) and every declaration is looked up ON THE ELEMENT `index.html` actually
 * contains, through `Element.matches`. A selector that names nothing now declares nothing.
 */

const RENDERER = resolve(__dirname, "..");
const INDEX_HTML = resolve(RENDERER, "index.html");
const GLOBALS_CSS = resolve(__dirname, "globals.css");
/** The Electron main process — a sibling workspace; the literal it duplicates lives here. */
const WINDOW_TS = resolve(RENDERER, "..", "src", "main", "window.ts");

/**
 * `index.html` WITH ITS PROSE COMMENTS REMOVED — the two `nyx:boot-splash` fences kept, since they
 * are markers, not prose. Same reason as `windowCode` below, and caught the same way: the comment
 * that explains the splash QUOTES `<div id="root"></div>`, and a structural search for `#root` found
 * the sentence instead of the element.
 */
const html = readFileSync(INDEX_HTML, "utf8").replace(/<!--[\s\S]*?-->/g, (comment) =>
  /^<!-- \/?nyx:boot-splash -->$/.test(comment) ? comment : "",
);
const css = readFileSync(GLOBALS_CSS, "utf8");

/**
 * `window.ts` WITH ITS COMMENTS REMOVED. Every assertion below reads this, never the raw file: the
 * comments next to these options SPELL OUT what they must contain (`show: false`, the old
 * `#000000`, the `ready-to-show` hand-over), so matching the raw text would let a prose sentence
 * stand in for the code. Caught by mutation — commenting `win.show()` out left the guard green.
 */
const windowCode = readFileSync(WINDOW_TS, "utf8")
  .replace(/\/\*[\s\S]*?\*\//g, "")
  .replace(/\/\/.*$/gm, "");

// --- oklch → sRGB hex -------------------------------------------------------------------------

/**
 * The CSS Color 4 conversion, verbatim: oklch → OKLab → linear sRGB → gamma-encoded sRGB. Written
 * out rather than pulled from a dependency because the whole point of this guard is to derive the
 * expected literal from the token WITHOUT trusting anything else to have done it.
 */
function oklchToHex(l: number, c: number, hDeg: number): string {
  const h = (hDeg * Math.PI) / 180;
  const a = c * Math.cos(h);
  const b = c * Math.sin(h);
  const lp = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const mp = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const sp = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const linear = [
    4.0767416621 * lp - 3.3077115913 * mp + 0.2309699292 * sp,
    -1.2684380046 * lp + 2.6097574011 * mp - 0.3413193965 * sp,
    -0.0041960863 * lp - 0.7034186147 * mp + 1.707614701 * sp,
  ];
  const channels = linear.map((v) => {
    const encoded = v <= 0.0031308 ? 12.92 * v : 1.055 * v ** (1 / 2.4) - 0.055;
    const byte = Math.round(Math.min(1, Math.max(0, encoded)) * 255);
    return byte.toString(16).padStart(2, "0");
  });
  return `#${channels.join("")}`;
}

/** The `.dark { … }` block of `globals.css` — the theme the shell ships (`<html class="dark">`). */
function darkBlock(): string {
  const start = css.indexOf(".dark {");
  expect(start, "`globals.css` no longer declares a `.dark { … }` token block").toBeGreaterThan(-1);
  const end = css.indexOf("\n}", start);
  expect(end, "the `.dark { … }` block of `globals.css` is not closed").toBeGreaterThan(start);
  return css.slice(start, end);
}

/** The sRGB hex of a dark-theme token declared as a bare `oklch(L C H)`. */
function darkTokenHex(token: string): string {
  const match = new RegExp(`${token}:\\s*oklch\\(([\\d.]+)\\s+([\\d.]+)\\s+([\\d.]+)\\)`).exec(
    darkBlock(),
  );
  expect(
    match,
    `\`${token}\` is no longer a bare \`oklch(L C H)\` in the \`.dark\` block of globals.css — ` +
      `the boot splash duplicates its sRGB value and can no longer derive it`,
  ).not.toBeNull();
  const [, l, c, h] = match as RegExpExecArray;
  return oklchToHex(Number(l), Number(c), Number(h));
}

// --- the boot-splash region of index.html -----------------------------------------------------

/**
 * Everything `index.html` fences between `<!-- nyx:boot-splash -->` and `<!-- /nyx:boot-splash -->`
 * — the inline stylesheet in the head AND the markup in `#root`, concatenated. The fences are what
 * scope this guard to the splash: the rest of the document (the favicon `<link>`, the module
 * `<script>`) is legitimately allowed the references the splash is not.
 *
 * MEMOISED, like [`splashStylesheet`] below: the source is a constant read once at import, so
 * re-scanning it on each of the dozen lookups the suite makes buys nothing. LAZILY, not at module
 * scope, because the `expect` here must fail INSIDE a test — at import it would be a collection
 * error whose message nobody attributes to the guard it belongs to.
 */
let regionsCache: string[] | undefined;
function splashRegions(): string[] {
  if (regionsCache) return regionsCache;
  const found = [...html.matchAll(/<!-- nyx:boot-splash -->([\s\S]*?)<!-- \/nyx:boot-splash -->/g)];
  expect(
    found.length,
    "index.html no longer fences its boot splash between `<!-- nyx:boot-splash -->` and " +
      "`<!-- /nyx:boot-splash -->` — expected the inline style and the markup",
  ).toBe(2);
  // CSS comments stripped, for the same reason the HTML ones already are: the notes inside the
  // splash quote the very declarations asserted below, and a quote is not a declaration.
  regionsCache = found.map((m) => m[1].replace(/\/\*[\s\S]*?\*\//g, ""));
  return regionsCache;
}

/**
 * `index.html` AS A DOCUMENT — the elements the selectors below are matched against. Parsed from the
 * comment-stripped source, so a splash that has been commented out is not there to match.
 */
const doc = new DOMParser().parseFromString(html, "text/html");

/** One parsed CSS rule of the splash's inline sheet. */
type SplashRule = { selector: string; declarations: Map<string, string> };

/**
 * The splash's inline stylesheet, parsed into RULES. At-rules carrying a nested block (`@keyframes`)
 * are lifted out first — their inner `to { … }` would otherwise read as a rule with the selector
 * `to`.
 *
 * MEMOISED: [`declaredOn`] asks it once per PROPERTY, so the whole sheet used to be re-extracted and
 * re-tokenised for each of the eight declarations asserted below. The input is immutable (a file
 * read once at import), so one parse answers them all.
 */
let stylesheetCache: SplashRule[] | undefined;
function splashStylesheet(): SplashRule[] {
  if (stylesheetCache) return stylesheetCache;
  const [region] = splashRegions();
  const sheet = /<style[^>]*>([\s\S]*?)<\/style>/.exec(region);
  expect(
    sheet,
    "the splash's fenced head region is no longer a single inline `<style>` block",
  ).not.toBeNull();
  const flat = (sheet as RegExpExecArray)[1].replace(
    /@[\w-]+[^{]*\{(?:[^{}]*\{[^{}]*\})*[^{}]*\}/g,
    "",
  );
  const rules: SplashRule[] = [];
  for (const [, selector, body] of flat.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const declarations = new Map<string, string>();
    for (const declaration of body.split(";")) {
      const colon = declaration.indexOf(":");
      if (colon < 0) continue;
      declarations.set(declaration.slice(0, colon).trim(), declaration.slice(colon + 1).trim());
    }
    rules.push({ selector: selector.trim(), declarations });
  }
  expect(rules.length, "the splash's `<style>` block declares no rule at all").toBeGreaterThan(0);
  stylesheetCache = rules;
  return rules;
}

/**
 * What the sheet declares for `property` ON `element` — `undefined` when NO rule that matches this
 * element declares it. That "matches" is the whole point: it is what a text search over the block
 * cannot do, and what makes a renamed selector a failure rather than a silent no-op.
 *
 * Last matching declaration wins. Not a full cascade (no specificity ordering) — the sheet is
 * fourteen declarations written by hand and declares each property once per element; a second rule
 * touching the same property on the same element is itself the thing to look at.
 */
function declaredOn(element: Element, property: string): string | undefined {
  let value: string | undefined;
  for (const rule of splashStylesheet()) {
    if (!element.matches(rule.selector)) continue;
    const declared = rule.declarations.get(property);
    if (declared !== undefined) value = declared;
  }
  return value;
}

/** The `#nyx-boot-splash` element `index.html` really contains — the subject of every rule below. */
function splashElement(): Element {
  const splash = doc.querySelector("#nyx-boot-splash");
  expect(splash, "`index.html` no longer contains a `#nyx-boot-splash` element").not.toBeNull();
  return splash as Element;
}

/** The `@keyframes <name> { … }` block of the splash sheet, or `undefined` when it declares none. */
function splashKeyframes(name: string): string | undefined {
  const [style] = splashRegions();
  const found = new RegExp(`@keyframes\\s+${name}\\s*\\{([\\s\\S]*?)\\n\\s*\\}`).exec(style);
  return found?.[1];
}

describe("boot splash — what the window paints before React exists", () => {
  it("paints a waiting state from index.html itself, inside #root", () => {
    // Read off the parsed DOCUMENT, not by scanning for `</div>`: the first `</div>` after
    // `<div id="root">` is the SPLASH's own closing tag, so the text version of this check held
    // whenever the splash existed at all — including outside `#root`.
    const root = doc.getElementById("root");
    expect(root, "index.html no longer opens `#root` as a container").not.toBeNull();
    const splash = splashElement();
    expect(
      (root as HTMLElement).contains(splash) && splash !== root,
      "the boot splash must live INSIDE `#root`: that is what makes " +
        "`createRoot().render()` clear it in the same commit that inserts the first React tree. " +
        "Outside it, it survives the mount and covers the app.",
    ).toBe(true);

    // It has to paint something, not just occupy the DOM: a full-window box and a glyph in it.
    // Each declaration is read ON the splash element, so it answers for the selector that carries
    // it — rename `#nyx-boot-splash` and every one of these becomes `undefined`.
    expect(
      declaredOn(splash, "position"),
      "nothing in the sheet gives `#nyx-boot-splash` a `position` — it lays out in the flow, which " +
        "at this instant is a strip the height of its content instead of the window",
    ).toBe("fixed");
    expect(
      declaredOn(splash, "inset"),
      "`#nyx-boot-splash` is fixed but not pinned to the viewport edges",
    ).toBe("0");
    expect(splash.querySelector("svg"), "the splash renders no visible glyph").not.toBeNull();
  });

  it("is painted BEFORE the bundle runs, because the bundle is a deferred module script", () => {
    // NOT a question of where the `<script>` sits in the source: Vite's production build hoists it
    // into `<head>` anyway (verified on `dist/index.html`). What puts the splash on screen first is
    // `type="module"`, which is deferred by spec — the parser builds and paints the document, THEN
    // runs the bundle. A classic (non-module) script would block parsing where it stands and the
    // splash would never be what the first paint shows.
    const scripts = [...html.matchAll(/<script\b([^>]*)>/g)].map((m) => m[1]);
    expect(
      scripts.length,
      "index.html loads no script at all — the app cannot boot",
    ).toBeGreaterThan(0);
    for (const attrs of scripts) {
      expect(
        attrs,
        `index.html loads a script that is neither \`type="module"\` nor \`defer\` (<script${attrs}>)` +
          " — it blocks the parser where it stands, and the boot splash is no longer what the first" +
          " paint shows",
      ).toMatch(/type="module"|\sdefer\b/);
    }
  });

  it("fetches nothing — no stylesheet, image, font or script inside the splash", () => {
    // The constructs that would make the browser issue a REQUEST. `xmlns` is deliberately absent
    // from the inline `<svg>` so no URL appears here at all.
    const fetching = /<link[\s>]|<img[\s>]|<script[\s>]|\ssrc=|\shref=|url\(|@import|@font-face/g;
    for (const region of splashRegions()) {
      const hits = region.match(fetching) ?? [];
      expect(
        hits,
        `the boot splash references an external resource (${hits.join(", ")}). It is shown to ` +
          "cover the load; a request added here lengthens exactly the wait it hides.",
      ).toEqual([]);
    }
  });

  it("uses the dark theme's own tokens, converted, and not a colour of its own", () => {
    // The two custom properties are declared ON `<html class="dark">` — the element `index.html`
    // itself carries. A rule that declares them under a selector matching nothing (measured:
    // `html.dark` → `.nyx-unused`) leaves the document canvas unpainted, and reads as `undefined`
    // here rather than as a string found somewhere in the block.
    const root = doc.documentElement;
    expect(
      declaredOn(root, "--nyx-boot-background"),
      "the boot background is not declared on the element `index.html` carries the theme on",
    ).toBe(darkTokenHex("--background"));
    expect(
      declaredOn(root, "--nyx-boot-foreground"),
      "the boot foreground is not declared on the element `index.html` carries the theme on",
    ).toBe(darkTokenHex("--muted-foreground"));

    // …AND ACTUALLY PAINTS WITH THEM. Declaring the right literal in a custom property proves
    // nothing on its own — the two surfaces that show it are the document canvas (before any
    // stylesheet exists) and the splash box itself, and both must read the same variable.
    expect(
      declaredOn(root, "background-color"),
      "the document canvas takes no `background-color` from `--nyx-boot-background`: the literal " +
        "above is then decoration, and the window paints whatever the UA defaults to",
    ).toBe("var(--nyx-boot-background)");
    const splash = splashElement();
    expect(
      declaredOn(splash, "background-color"),
      "the splash box takes no `background-color` from `--nyx-boot-background`",
    ).toBe("var(--nyx-boot-background)");
    expect(
      declaredOn(splash, "color"),
      "the splash's glyph no longer takes its colour from `--nyx-boot-foreground`",
    ).toBe("var(--nyx-boot-foreground)");
  });

  it("shows the design system's own spinner — same glyph, same motion, same colour", async () => {
    // The claim `index.html` makes next to this markup, held rather than asserted in prose: a
    // static `<rect>` in place of the lucide path, or a keyframe that no longer turns a full circle,
    // used to leave the whole file green.
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    await act(async () => {
      root.render(createElement(Spinner));
    });
    const shipped = host.querySelector("svg");
    expect(shipped, "`<Spinner>` renders no svg — this comparison has no reference").not.toBeNull();

    const splash = splashElement();
    const glyph = splash.querySelector("svg");
    expect(glyph, "the splash renders no svg").not.toBeNull();

    // SAME GLYPH — the path data itself, read off the component the router's pending screen uses.
    const paths = (svg: Element) =>
      [...svg.querySelectorAll("path")].map((p) => p.getAttribute("d"));
    expect(
      paths(glyph as Element),
      "the splash's glyph is not the one `<Spinner>` draws: the hand-over to the router's pending " +
        "screen becomes a change of shape",
    ).toEqual(paths(shipped as Element));
    expect(paths(glyph as Element).length, "the splash draws no path at all").toBeGreaterThan(0);

    // SAME MOTION — Tailwind's `animate-spin` is `1s linear infinite` over a full turn.
    const animation = declaredOn(glyph as Element, "animation");
    expect(
      animation,
      "the splash's glyph is not animated — `<Spinner>` spins, so the hand-over stutters",
    ).toBeDefined();
    const [name, ...timing] = (animation as string).split(/\s+/);
    expect(timing, `the splash's spin is not \`1s linear infinite\` (\`${animation}\`)`).toEqual([
      "1s",
      "linear",
      "infinite",
    ]);
    expect(
      splashKeyframes(name),
      `\`@keyframes ${name}\` does not end on a full turn — the glyph would jerk back partway`,
    ).toMatch(/transform:\s*rotate\(360deg\)/);

    // SAME COLOUR — the glyph paints with `currentColor`, and the box above sets that from
    // `--nyx-boot-foreground` (asserted in the previous test).
    expect(
      (glyph as Element).getAttribute("stroke"),
      "the splash's glyph carries a colour of its own instead of inheriting the box's",
    ).toBe("currentColor");

    root.unmount();
    host.remove();
  });

  it("paints the same colour as the native window, so the window's appearance has no flash", () => {
    const background = darkTokenHex("--background");
    const declared = /backgroundColor:\s*"(#[0-9a-fA-F]{6})"/.exec(windowCode);
    expect(declared, "`window.ts` no longer sets a hex `backgroundColor`").not.toBeNull();
    expect(
      (declared as RegExpExecArray)[1].toLowerCase(),
      "the window's `backgroundColor` and the dark theme's `--background` have drifted apart — " +
        "the window would paint one colour and the page another at the moment it appears",
    ).toBe(background);
  });

  it("is removed BY the first React commit, not left behind for a later effect to clear", async () => {
    // The splash carries no teardown code: it relies on `createRoot(container).render()` clearing
    // its container as part of the commit that inserts the first tree. Same commit = same DOM
    // mutation batch = the browser never gets a frame with an empty `#root` to paint. If a React
    // version ever stopped clearing, the splash would instead STAY, on top of the app.
    const [, markup] = splashRegions();
    const container = document.createElement("div");
    container.id = "root";
    container.innerHTML = markup;
    document.body.append(container);
    expect(
      container.querySelector("#nyx-boot-splash"),
      "index.html's splash markup did not produce a `#nyx-boot-splash` element",
    ).not.toBeNull();

    const root = createRoot(container);
    await act(async () => {
      root.render(createElement("main", { id: "first-react-screen" }));
    });

    expect(
      container.querySelector("#nyx-boot-splash"),
      "React's first commit did not clear `#root`: the boot splash survives the mount and now " +
        "covers the app — it needs explicit teardown",
    ).toBeNull();
    expect(container.querySelector("#first-react-screen")).not.toBeNull();

    root.unmount();
    container.remove();
  });

  it("keeps the window hidden until it can paint — the white flash stays gone", () => {
    expect(
      windowCode,
      "`show: false` is gone from the BrowserWindow options: the window would appear before the " +
        "renderer has painted, which is the white flash it was added to remove",
    ).toMatch(/show:\s*false/);
    expect(
      windowCode,
      "the window is no longer shown on `ready-to-show` — either it never shows, or it shows too " +
        "early",
    ).toMatch(/once\("ready-to-show",[\s\S]{0,120}?win\.show\(\)/);
  });
});
