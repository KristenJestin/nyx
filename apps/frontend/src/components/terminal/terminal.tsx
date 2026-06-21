import { useEffect, useMemo } from "react";
import { FitAddon } from "@xterm/addon-fit";
import type { ITerminalOptions, Terminal as XTerm } from "@xterm/xterm";
import { useXTerm } from "react-xtermjs";

import { cn } from "@/lib/utils";
import { isTerminalNavChord } from "@/components/sidebar/use-terminal-shortcuts";
import { buildDeadHistory } from "./dead-history";
import { usePty } from "./use-pty";
import { useScrollbackPersist } from "./scrollback-persist";
import { useWebglAddon } from "./use-webgl-addon";
import {
  ensureTerminalFontLoaded,
  FALLBACK_THEME,
  resolveCssColor,
  resolveThemeFromCss,
} from "./xterm-theme";

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
  fontFamily: '"Fira Code Variable", "Fira Code", ui-monospace, "Liberation Mono", monospace',
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

// The xterm THEME + FONT helpers (FALLBACK_THEME, resolveCssColor,
// resolveThemeFromCss, ensureTerminalFontLoaded) live in the shared
// `./xterm-theme` module so the interactive terminal and the read-only
// `<CommandOutputPanel>` derive their colours/metrics from one implementation.

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
  /**
   * Whether this terminal is the active/visible one. The WebGL renderer is
   * attached ONLY while active and disposed on blur, so many open terminals
   * never exhaust the browser's WebGL context pool (~16). Inactive terminals
   * keep their xterm instance + buffer alive and still receive output; they just
   * render with the default DOM/canvas renderer while hidden. Defaults to `true`
   * so a standalone `<Terminal>` (the socle, the tests) keeps WebGL.
   */
  active?: boolean;
  /**
   * The SQLite RECORD id this terminal is bound to. When set, the terminal's
   * scrollback is serialized + DEBOUNCED to the backend (`persist_scrollback`)
   * so it survives a close/restart. Omit for a record-less standalone terminal
   * (the socle / unit harness) — persistence is then a no-op.
   */
  recordId?: string;
  /**
   * Prior serialized scrollback to restore as READ-ONLY dead history. Written to
   * xterm once on mount (followed by a visual separator) so a re-spawned terminal
   * shows its previous session above a fresh shell. NEVER sent to the PTY — the
   * history is read-only, the new shell starts clean below the separator.
   */
  deadHistory?: string;
  /**
   * Called with the live PTY id once the shell spawns (and `null` on exit). The
   * sidebar's auto-naming needs the PTY id to read `terminal_info` for this
   * terminal's live cwd + foreground program.
   */
  onPtyId?: (id: number | null) => void;
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
  active = true,
  recordId,
  deadHistory,
  onPtyId,
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

  // Attach the WebGL renderer ONLY while this terminal is active (disposed on
  // blur), so N open terminals never exhaust the browser's WebGL context pool.
  // Returns `clearAtlas()` so the post-font-load fit can rebuild the glyph atlas
  // (which was baked at attach against the fallback metrics).
  const clearWebglAtlas = useWebglAddon(instance ?? null, active);

  // Build the read-only DEAD HISTORY payload (prior scrollback + a dim separator)
  // ONCE from the design-token muted-foreground colour. It is handed to `usePty`,
  // which writes it as the VERY FIRST bytes of the session — before the PTY's
  // output listener and before spawn — so the restored history is guaranteed to
  // sit ABOVE the live prompt and the input stays typable at the bottom (finding
  // 01KV3CPAG2KTV413C4RVNH6TVN). Memoized on `deadHistory` so a re-render does not
  // rebuild it (and so the value handed to `usePty` is stable). `""` when there is
  // no meaningful prior scrollback → nothing is written.
  const deadHistoryPayload = useMemo(() => {
    if (!deadHistory) return "";
    const sepColor = resolveCssColor("--muted-foreground") ?? FALLBACK_THEME.foreground;
    return buildDeadHistory(deadHistory, sepColor);
  }, [deadHistory]);

  // Wire the live PTY backend (spawn / IO / resize / teardown). StrictMode-safe.
  // `resyncSize` pushes the terminal's current cols/rows to the PTY out-of-band
  // from xterm's onResize event — used below to make the authoritative
  // post-font fit reach the PTY even if it raced the spawn. `deadHistory` is
  // written by the hook before spawn (see above) so its ordering vs. live output
  // is deterministic.
  const resyncSize = usePty(instance, fitAddon, {
    cwd,
    recordId,
    onPtyId,
    deadHistory: deadHistoryPayload,
  });

  // FOCUS-ON-ACTIVATE (fixes the critical "cannot type in some terminals" bug):
  // selecting a terminal in the sidebar only flips `active` — it does NOT move
  // keyboard focus to the newly-shown pane. Clicking the sidebar row leaves
  // focus on that <button> (or wherever it was), so the visible xterm's hidden
  // input never receives keystrokes and typing appears to do nothing in any
  // terminal the user switches to via the sidebar. xterm only captures input
  // while its textarea is focused, so we must focus the instance whenever it
  // becomes the active pane (also covers becoming active after a reorder, since
  // that re-runs with the same active instance and re-focuses it).
  //
  // We focus on the next frame: a freshly-activated pane was `display:none` (see
  // TerminalDeck) and an element cannot take focus while not displayed; the
  // style flip and this effect commit in the same React pass, so we defer one
  // rAF to focus after the pane is actually visible/laid-out.
  useEffect(() => {
    if (!instance || !active) return;
    let raf = 0;
    const frame =
      typeof requestAnimationFrame === "function"
        ? requestAnimationFrame
        : (cb: FrameRequestCallback) => setTimeout(() => cb(0), 0);
    const cancel =
      typeof cancelAnimationFrame === "function"
        ? cancelAnimationFrame
        : (id: number) => clearTimeout(id);
    raf = frame(() => {
      try {
        instance.focus();
      } catch {
        // focus is best-effort: a detached/closing instance must never throw.
      }
    }) as unknown as number;
    return () => cancel(raf);
  }, [instance, active]);

  // Serialize + DEBOUNCE this terminal's scrollback to the backend so it can be
  // restored after a close/restart (no-op when record-less). Persistence is
  // debounced (never per byte) and also flushes on tab/app close.
  useScrollbackPersist(instance ?? null, recordId ?? null);

  // RESTORE ordering note: the dead history is NOT written here any more. Writing
  // it from an `instance`-dependent effect raced the PTY's first output (an async
  // event) and could land the restored block BELOW the live prompt, so the input
  // sat above dead history and the user couldn't type (the Image-16 bug). It is
  // now written by `usePty.start()` as the first bytes of the session — before the
  // output listener and before spawn — so history is deterministically ABOVE the
  // live prompt. See `deadHistoryPayload` + `usePty`'s `deadHistory` option above.

  // Surface the instance to the parent (and to tests) as it appears/disappears.
  useEffect(() => {
    onInstance?.(instance ?? null);
    return () => onInstance?.(null);
  }, [instance, onInstance]);

  // Let app-level navigation chords (Ctrl/Cmd+T/W, Ctrl+Tab/PageUp/Down) pass
  // THROUGH xterm untouched so they bubble up to `document`, where the global
  // TanStack Hotkeys listener handles them. Returning `false` tells xterm not to
  // process the key (no PTY byte, no preventDefault/stopPropagation). Without
  // this, xterm consumes the chord and stops propagation, so the shortcut would
  // only fire when the sidebar — not a terminal — is focused.
  useEffect(() => {
    if (!instance) return;
    instance.attachCustomKeyEventHandler((e) => !isTerminalNavChord(e));
    return () => {
      // Restore the default (xterm handles every key) if the instance persists.
      instance.attachCustomKeyEventHandler(() => true);
    };
  }, [instance]);

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

  // WebGL is now managed by `useWebglAddon` (active-only attach/dispose); see
  // `clearWebglAtlas` above.

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
    const fontFamily = mergedOptions.fontFamily ?? (DEFAULT_OPTIONS.fontFamily as string);
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
      // The atlas baked at attach used the fallback metrics — clear it so WebGL
      // regenerates the glyph cache against the loaded font (no-op if inactive /
      // no live context).
      clearWebglAtlas();
      // RESTORE viewport (finding 01KV3CPAG…): after the authoritative fit, anchor
      // the viewport at the BOTTOM so the live prompt is in view (not scrolled up
      // above the restored dead history) and WebKitGTK does not leave a phantom
      // scrollbar from a stale viewport that scrolls nothing. Idempotent on a
      // fresh terminal (already at bottom). Best-effort: a detached instance must
      // never throw here.
      try {
        instance.scrollToBottom();
      } catch {
        // ignore (detached / closing instance)
      }
    });

    const observer = new ResizeObserver(() => safeFit());
    observer.observe(el);

    return () => {
      cancelled = true;
      observer.disconnect();
    };
  }, [instance, fitAddon, ref, mergedOptions, resyncSize, clearWebglAtlas]);

  // OUTER container carries the padding + background; the INNER `ref` div is
  // where xterm opens and is what the ResizeObserver/FitAddon measure. FitAddon
  // therefore sizes cols/rows to the INNER (padded) area, giving correct
  // dimensions plus a visual margin — no edge column/row gets clipped, and we do
  // NOT touch xterm's native scroll or reintroduce any custom bottom anchor.
  return (
    <div className={cn("h-full w-full overflow-hidden bg-background p-2.5", className)}>
      <div ref={ref} className="h-full w-full overflow-hidden" />
    </div>
  );
}

export default Terminal;
