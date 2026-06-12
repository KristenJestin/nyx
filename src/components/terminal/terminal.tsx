import { useEffect, useMemo, useRef } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import type { ITerminalOptions, Terminal as XTerm } from "@xterm/xterm";
import { formatHex } from "culori";
import { useXTerm } from "react-xtermjs";

import { cn } from "@/lib/utils";
import { usePty } from "./use-pty";

/**
 * Default xterm options for a NORMAL terminal: native scroll, generous
 * scrollback, no custom bottom-anchoring, no blocks. This is the deliberate
 * anti-flash configuration — we do not reintroduce anything inventive on the
 * live render.
 */
const DEFAULT_OPTIONS: ITerminalOptions = {
  // Local echo stays OFF: the PTY echoes typed characters back. Turning on
  // convertEol / local echo here would double-print everything.
  convertEol: false,
  cursorBlink: true,
  cursorStyle: "block",
  // Bundled Fira Code (see globals.css). The previous stack named only fonts
  // absent on Linux/WebKitGTK, so xterm measured a cell width that didn't match
  // the glyph actually rendered → "t e s t" spacing. We bundle the face and gate
  // the first fit on its load (see ensureTerminalFontLoaded + the fit effect
  // below) so the measurement always matches the rendered glyph.
  fontFamily:
    '"Fira Code Variable", "Fira Code", ui-monospace, "Liberation Mono", monospace',
  fontSize: 14,
  // Pin letterSpacing to 0 so react-xtermjs / xterm defaults never inject extra
  // inter-glyph spacing on top of a correctly-measured monospace cell.
  letterSpacing: 0,
  scrollback: 10_000,
  // NOTE: no `theme` here. The terminal's background/foreground are derived from
  // the design-system CSS palette at MOUNT (see resolveThemeFromCss + the theme
  // effect below), so the canvas matches the shell and there is no hardcoded
  // colour. Building it at mount (not as a module const) means it reflects the
  // active `.dark` palette resolved by the browser.
};

/**
 * Sane fallback theme used only if the CSS palette cannot be resolved (e.g. no
 * `getComputedStyle`, or a token that resolves to an empty/garbage value). Dark
 * background + light foreground so we NEVER render black-on-black or an unstyled
 * white flash. These mirror the previous hardcoded values and exist purely as a
 * floor — the live path resolves the real tokens.
 */
const FALLBACK_THEME = { background: "#0a0a0a", foreground: "#e6e6e6" } as const;

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
 * So we hand the raw token to culori's `formatHex`, which parses oklch (and hex,
 * named, rgb, …) and renders the final sRGB `#rrggbb` string in pure JS — no
 * canvas/paint round-trip. Alpha is dropped intentionally — the xterm
 * background/foreground are opaque.
 *
 * Returns `null` (→ caller falls back) if there is no DOM, the token is empty,
 * or culori cannot parse the value (`formatHex` returns `undefined`), so we
 * never feed xterm an unusable colour.
 */
function resolveCssColor(varName: string): string | null {
  if (typeof document === "undefined") return null;
  const getStyle =
    typeof getComputedStyle === "function" ? getComputedStyle : null;
  if (!getStyle) return null;

  const raw = getStyle(document.documentElement).getPropertyValue(varName).trim();
  if (!raw) return null;

  // culori parses the authored colour space (oklch, …) and renders concrete
  // sRGB `#rrggbb`; returns undefined for an empty/garbage token → null → caller
  // falls back to FALLBACK_THEME.
  return formatHex(raw) ?? null;
}

/**
 * Build the xterm theme (background/foreground) from the live CSS palette,
 * falling back per-channel to a safe dark theme so the terminal is never
 * unreadable (black-on-black) if a token fails to resolve.
 */
function resolveThemeFromCss(): { background: string; foreground: string } {
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
async function ensureTerminalFontLoaded(
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

export interface TerminalProps {
  /**
   * Optional className for the full-bleed container. The container fills its
   * parent; the parent is responsible for giving it a size.
   */
  className?: string;
  /** Override / extend the default xterm options. */
  options?: ITerminalOptions;
  /**
   * Working directory for the spawned shell. `undefined` lets the backend pick
   * its default (it inherits nyx's cwd, i.e. home/current).
   */
  cwd?: string;
  /**
   * Called with the xterm instance once it is created (and with `null` when it
   * is disposed). Useful for parents that need to drive the terminal directly;
   * also the seam used by unit tests to assert on the xterm buffer.
   */
  onInstance?: (instance: XTerm | null) => void;
}

/**
 * `<Terminal>` — a single xterm.js v6 instance with the WebGL renderer (clean
 * canvas fallback when WebGL is unavailable) and the fit addon.
 *
 * Mounting is idempotent and the instance is disposed on unmount, so the
 * component survives `React.StrictMode`'s double-mount in dev without leaking a
 * second terminal. Backend wiring (PTY spawn/IO) is layered on top in a later
 * slice; here the instance is self-contained and can be exercised with mocked
 * bytes via `instance.write(...)`.
 */
export function Terminal({
  className,
  options,
  cwd,
  onInstance,
}: TerminalProps) {
  // Memoize so `useXTerm`'s effect (deps: [options, addons]) does NOT re-run on
  // every render — re-running would tear down and recreate the terminal.
  const mergedOptions = useMemo<ITerminalOptions>(
    () => ({ ...DEFAULT_OPTIONS, ...options }),
    [options],
  );

  // FitAddon is loaded via useXTerm (no GL context needed). One instance, stable
  // across renders, recreated only if the terminal itself is recreated.
  const fitAddon = useMemo(() => new FitAddon(), []);
  const addons = useMemo(() => [fitAddon], [fitAddon]);

  const { ref, instance } = useXTerm({ options: mergedOptions, addons });

  // Live WebGL addon (or null when on the DOM/canvas fallback). Held on a ref so
  // the font-gating fit effect can rebuild the glyph atlas once the real font is
  // loaded — the atlas baked at open() uses the fallback metrics otherwise.
  const webglRef = useRef<WebglAddon | null>(null);

  // Wire the live PTY backend (spawn / IO / resize / teardown). StrictMode-safe.
  // `resyncSize` pushes the terminal's current cols/rows to the PTY out-of-band
  // from xterm's onResize event — used below to make the authoritative
  // post-font fit reach the PTY even if it raced the spawn.
  const resyncSize = usePty(instance, fitAddon, { cwd });

  // Surface the instance to the parent (and to tests) as it appears/disappears.
  useEffect(() => {
    onInstance?.(instance ?? null);
    return () => onInstance?.(null);
  }, [instance, onInstance]);

  // Derive the terminal theme from the design-system CSS palette AT MOUNT, so
  // the canvas background/foreground match the shell (`bg-background`) with no
  // hardcoded colour. We resolve here (in an effect, against the live DOM) rather
  // than in DEFAULT_OPTIONS because the tokens are `oklch(...)` and need the
  // browser to convert them to an xterm-parseable `rgb()` — which requires a real
  // document. A caller-supplied `options.theme` takes precedence (we don't
  // override an explicit theme). Re-runs if the terminal is recreated.
  const callerTheme = options?.theme;
  useEffect(() => {
    if (!instance) return;
    if (callerTheme) return; // explicit override: leave it untouched.
    const { background, foreground } = resolveThemeFromCss();
    // Merge so we don't clobber any xterm theme defaults we don't set here.
    instance.options.theme = { ...instance.options.theme, background, foreground };
  }, [instance, callerTheme]);

  // Load the WebGL renderer ourselves, AFTER the terminal is open()ed by
  // useXTerm, so there is a real DOM/canvas to attach to. On any failure (no
  // WebGL, context creation throws) we dispose the addon and fall back to the
  // default DOM/canvas renderer — cleanly, without a thrown error reaching the
  // console.
  useEffect(() => {
    if (!instance) return;

    let webgl: WebglAddon | undefined;

    try {
      webgl = new WebglAddon();
      // If the GL context is lost at runtime, drop WebGL and let xterm fall
      // back to its default renderer instead of rendering nothing.
      webgl.onContextLoss(() => {
        webgl?.dispose();
        webgl = undefined;
        webglRef.current = null;
      });
      instance.loadAddon(webgl);
      webglRef.current = webgl;
    } catch {
      // WebGL unavailable (headless / blocklisted GPU / jsdom): clean fallback.
      webgl?.dispose();
      webgl = undefined;
      webglRef.current = null;
    }

    return () => {
      webgl?.dispose();
      webglRef.current = null;
    };
    // Effect is keyed on the terminal instance so WebGL re-loads if the
    // terminal is recreated (e.g. StrictMode remount).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instance]);

  // Fit on open and keep fitting as the container resizes. ResizeObserver is the
  // single source of truth for sizing — no manual layout math, no bottom anchor.
  //
  // LOAD-BEARING ordering for the "t e s t" spacing fix: the FIRST authoritative
  // fit must run AFTER the bundled monospace face is loaded, so xterm measures
  // the real glyph (not the fallback) when it computes the cell width. We then
  // rebuild the WebGL glyph atlas, which was baked at open() against the fallback
  // metrics, so it regenerates against the loaded font. Without this gating the
  // mis-spacing can persist (font-loading FOUT → bad measurement).
  useEffect(() => {
    if (!instance) return;
    const el = ref.current;
    if (!el) return;

    const safeFit = () => {
      // fit() throws if the element has no layout yet (0x0); guard it so a
      // transient zero-size container never crashes the app.
      try {
        if (el.clientWidth > 0 && el.clientHeight > 0) {
          fitAddon.fit();
        }
      } catch {
        // ignore transient fit failures (no layout / detached)
      }
    };

    // Immediate best-effort fit so the element has dims for the PTY spawn
    // (usePty reads proposeDimensions at spawn time) and there is no 0x0 frame.
    // The authoritative, font-correct fit follows once the face has loaded.
    safeFit();

    let cancelled = false;

    // Resolve the live font family/size from the merged options so the load
    // gate targets exactly what xterm will measure.
    const fontFamily =
      mergedOptions.fontFamily ?? (DEFAULT_OPTIONS.fontFamily as string);
    const fontSize = mergedOptions.fontSize ?? (DEFAULT_OPTIONS.fontSize as number);

    void ensureTerminalFontLoaded(fontFamily, fontSize).then(() => {
      if (cancelled) return;
      // Now the real font is loaded: this fit measures the correct glyph width.
      safeFit();
      // Push the (possibly font-corrected) cols/rows straight to the PTY. The
      // fit above emits xterm's onResize, but that handler is only wired AFTER
      // pty_spawn resolves; if the font load beat the spawn, that resize would
      // be lost. resyncSize is event-independent — if the spawn is done it fires
      // an idempotent pty_resize now; if not, usePty defers it to just after the
      // spawn. The ResizeObserver is NOT a backstop here (the element size is
      // unchanged — only the cell metric moved — so it would not re-fire).
      resyncSize();
      // The atlas baked at open() used the fallback metrics — clear it so WebGL
      // regenerates the glyph cache against the loaded font.
      try {
        webglRef.current?.clearTextureAtlas();
      } catch {
        // atlas rebuild is best-effort; never let it crash the render.
      }
    });

    const observer = new ResizeObserver(() => safeFit());
    observer.observe(el);

    return () => {
      cancelled = true;
      observer.disconnect();
    };
  }, [instance, fitAddon, ref, mergedOptions, resyncSize]);

  // OUTER container carries the padding + background; the INNER `ref` div is
  // where xterm opens and is what the ResizeObserver/FitAddon measure. FitAddon
  // therefore sizes cols/rows to the INNER (padded) area, giving correct
  // dimensions plus a visual margin — no edge column/row gets clipped, and we do
  // NOT touch xterm's native scroll or reintroduce any custom bottom anchor.
  return (
    <div
      className={cn(
        "h-full w-full overflow-hidden bg-background p-2.5",
        className,
      )}
    >
      <div ref={ref} className="h-full w-full overflow-hidden" />
    </div>
  );
}

export default Terminal;
