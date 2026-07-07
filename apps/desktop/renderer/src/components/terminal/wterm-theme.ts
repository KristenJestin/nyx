import chroma from "chroma-js";

/**
 * wterm THEME + FONT helpers. wterm renders to the DOM (not a WebGL canvas) and
 * derives its palette from CSS custom properties on the `.wterm` element
 * (`--term-bg`, `--term-fg`, `--term-color-0..15`, `--term-font-*` — see
 * `@wterm/dom`'s `terminal.css`). So "theming" is just writing those variables
 * onto the host element from the design-system tokens; there is no per-instance
 * theme object to build (that was the xterm model — removed with xterm).
 *
 * This replaces the old `xterm-theme.ts`: same DESIGN-SYSTEM-DERIVED colours (no
 * hardcoded palette in the markup) and the same monospace-load gate, but mapped
 * onto wterm's CSS-variable surface.
 */

/**
 * Sane fallback colours used only if the CSS palette cannot be resolved (no
 * `getComputedStyle`, or a token that resolves to an empty/garbage value). Dark
 * background + light foreground so we NEVER render an unreadable black-on-black
 * or an unstyled white flash. A floor only — the live path resolves the real
 * tokens.
 */
export const FALLBACK_THEME = { background: "#0a0a0a", foreground: "#e6e6e6" } as const;

/**
 * Resolve a CSS custom property to a `#rrggbb` colour string.
 *
 * The design-system tokens are authored in `oklch(...)`, which neither wterm's
 * CSS variables nor a raw copy round-trip predictably across engines. We hand
 * the raw token to chroma-js, which parses oklch (and hex/named/rgb) and renders
 * the final sRGB `#rrggbb` in pure JS — no canvas/paint round-trip. Alpha is
 * dropped (the terminal surface is opaque).
 *
 * Returns `null` (→ caller falls back) if there is no DOM, the token is empty,
 * or chroma cannot parse it, so we never feed the terminal an unusable colour.
 */
export function resolveCssColor(varName: string): string | null {
  if (typeof document === "undefined") return null;
  const getStyle = typeof getComputedStyle === "function" ? getComputedStyle : null;
  if (!getStyle) return null;

  const raw = getStyle(document.documentElement).getPropertyValue(varName).trim();
  if (!raw) return null;

  try {
    return chroma(raw).hex("rgb");
  } catch {
    return null;
  }
}

/** The design-system background/foreground, resolved from the live CSS palette. */
export function resolveThemeFromCss(): { background: string; foreground: string } {
  return {
    background: resolveCssColor("--background") ?? FALLBACK_THEME.background,
    foreground: resolveCssColor("--foreground") ?? FALLBACK_THEME.foreground,
  };
}

/**
 * The monospace stack the terminal renders with — the bundled Fira Code face
 * first (see `globals.css`), matching the old xterm `fontFamily`. wterm measures
 * a cell from a live probe span, so the family must be the one actually rendered
 * or the grid columns misalign.
 */
export const TERMINAL_FONT_FAMILY =
  '"Fira Code Variable", "Fira Code", ui-monospace, "Liberation Mono", monospace';
export const TERMINAL_FONT_SIZE = 14;

/**
 * Write the design-system palette + font onto a `.wterm` host element as the
 * `--term-*` CSS variables wterm reads. Called once when an instance's element
 * is created (the colours are resolved against the live `.dark` palette then).
 * Background/foreground come from the tokens; the ANSI 0..15 slots keep wterm's
 * built-in defaults (a future palette pass can map them too) so we never leave a
 * slot unset.
 */
export function applyWtermTheme(element: HTMLElement): void {
  const { background, foreground } = resolveThemeFromCss();
  element.style.setProperty("--term-bg", background);
  element.style.setProperty("--term-fg", foreground);
  const cursor = resolveCssColor("--foreground") ?? foreground;
  element.style.setProperty("--term-cursor", cursor);
  element.style.setProperty("--term-font-family", TERMINAL_FONT_FAMILY);
  element.style.setProperty("--term-font-size", `${TERMINAL_FONT_SIZE}px`);
  // Kill wterm's default rounded card look (shadow/radius/padding): inside the
  // app pane the terminal is full-bleed, styled by the pane container, not a
  // floating card.
  element.style.borderRadius = "0";
  element.style.boxShadow = "none";
  element.style.padding = "0";
}

/**
 * Ensure the terminal's monospace face is fully loaded by the browser before the
 * cell is measured. An unloaded web font renders as a fallback whose advance
 * width differs from the real glyph; measuring during that window gives a wrong
 * cell width → misaligned columns. We `load()` the exact family at the exact
 * size, then await `fonts.ready`. Resolves (never rejects) even where
 * `document.fonts` is absent (jsdom) so callers can always `await` it.
 */
export async function ensureTerminalFontLoaded(
  fontFamily: string = TERMINAL_FONT_FAMILY,
  fontSize: number = TERMINAL_FONT_SIZE,
): Promise<void> {
  const fonts = (document as Document & { fonts?: FontFaceSet }).fonts;
  if (!fonts) return;
  try {
    const primary = fontFamily.split(",")[0]?.trim() ?? fontFamily;
    await fonts.load(`${fontSize}px ${primary}`);
    await fonts.ready;
  } catch {
    // best-effort: a parse failure must not block the terminal from rendering.
  }
}
