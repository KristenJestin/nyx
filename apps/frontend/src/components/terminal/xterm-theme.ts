import chroma from "chroma-js";

/**
 * Shared xterm THEME + FONT helpers for the interactive `<Terminal>` and the
 * read-only `<CommandOutputPanel>` (extracted to kill the verbatim duplication
 * flagged by review 01KV5PCBW3Y0VVFKNSK4K69EX8, mirroring the `use-webgl-addon`
 * extraction). Both surfaces derive their canvas colours from the design-system
 * CSS palette and gate their first fit on the bundled monospace face — that logic
 * lives here ONCE so a future palette-token or font-load change is made in a
 * single place.
 */

/**
 * Sane fallback theme used only if the CSS palette cannot be resolved (e.g. no
 * `getComputedStyle`, or a token that resolves to an empty/garbage value). Dark
 * background + light foreground so we NEVER render black-on-black or an unstyled
 * white flash. These mirror the previous hardcoded values and exist purely as a
 * floor — the live path resolves the real tokens.
 */
export const FALLBACK_THEME = { background: "#0a0a0a", foreground: "#e6e6e6" } as const;

/**
 * Resolve a CSS custom property to a `#rrggbb` colour string xterm can parse.
 *
 * The design-system tokens are authored in `oklch(...)`, which xterm's colour
 * parser does NOT understand. Note the SUBTLE trap: on current Chromium/WebKit
 * the obvious "coerce via the engine" tricks DON'T downconvert — both
 * `getComputedStyle(el).color` and canvas `ctx.fillStyle` now serialise back the
 * AUTHORED colour space (CSS Color 4), so an oklch token round-trips as a raw
 * `oklch(...)` string that xterm would reject. (Verified empirically in
 * Chromium.)
 *
 * So we hand the raw token to chroma-js, which parses oklch (and hex, named,
 * rgb, …) and renders the final sRGB `#rrggbb` string in pure JS — no
 * canvas/paint round-trip. Alpha is dropped intentionally — the xterm
 * background/foreground are opaque.
 *
 * Returns `null` (→ caller falls back) if there is no DOM, the token is empty,
 * or chroma-js cannot parse the value (it throws → we swallow it), so we never
 * feed xterm an unusable colour.
 */
export function resolveCssColor(varName: string): string | null {
  if (typeof document === "undefined") return null;
  const getStyle = typeof getComputedStyle === "function" ? getComputedStyle : null;
  if (!getStyle) return null;

  const raw = getStyle(document.documentElement).getPropertyValue(varName).trim();
  if (!raw) return null;

  // chroma-js parses the authored colour space (oklch, …) and renders concrete
  // sRGB `#rrggbb`; it THROWS on an empty/garbage token → null → caller falls
  // back to FALLBACK_THEME. `.hex("rgb")` forces 6 digits (drops any alpha).
  try {
    return chroma(raw).hex("rgb");
  } catch {
    return null;
  }
}

/**
 * Build the xterm theme (background/foreground) from the live CSS palette,
 * falling back per-channel to a safe dark theme so the terminal is never
 * unreadable (black-on-black) if a token fails to resolve.
 */
export function resolveThemeFromCss(): { background: string; foreground: string } {
  return {
    background: resolveCssColor("--background") ?? FALLBACK_THEME.background,
    foreground: resolveCssColor("--foreground") ?? FALLBACK_THEME.foreground,
  };
}

/**
 * Ensure the terminal's monospace face is fully loaded by the browser before
 * xterm measures a glyph. WebKitGTK (and any browser) renders an unloaded web
 * font as a fallback whose advance width differs from the real glyph; if xterm
 * fits/measures during that window the cell width is wrong → "t e s t" spacing.
 *
 * We explicitly `load()` the exact family at the exact size, then await the
 * global `fonts.ready`, so the first fit happens against the final metrics.
 * Resolves (never rejects) even where `document.fonts` is absent (older jsdom)
 * so callers can always `await` it without guarding.
 */
export async function ensureTerminalFontLoaded(
  fontFamily: string,
  fontSize: number,
): Promise<void> {
  const fonts = (document as Document & { fonts?: FontFaceSet }).fonts;
  if (!fonts) return; // no FontFaceSet (e.g. jsdom): nothing to gate on.
  try {
    // Quote-strip the primary family (the bundled face) and load it at the live
    // size; `load` parses a CSS `font` shorthand, hence the `<size>px <family>`.
    const primary = fontFamily.split(",")[0]?.trim() ?? fontFamily;
    await fonts.load(`${fontSize}px ${primary}`);
    await fonts.ready;
  } catch {
    // Loading is best-effort: a parse failure must not block the terminal from
    // rendering (it just falls back to the default metrics, as before).
  }
}
