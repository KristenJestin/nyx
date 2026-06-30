import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowDownIcon } from "lucide-react";
import { FitAddon } from "@xterm/addon-fit";
import type { ITerminalOptions, Terminal as XTerm } from "@xterm/xterm";
import { useXTerm } from "react-xtermjs";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { isTerminalNavChord } from "@/components/sidebar/use-terminal-shortcuts";
import { copySelection, isCopyChord, isPasteChord, pasteFromClipboard } from "./terminal-clipboard";
import { TerminalContextMenu } from "./terminal-context-menu";
import { buildDeadHistory } from "./dead-history";
import { reconcileTerminalGeometry } from "./terminal-geometry";
import { computeAtBottom, useAtBottom } from "./use-at-bottom";
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

/**
 * The byte sequence written to the PTY when the user presses `Shift+Enter`.
 *
 * GOAL: `Shift+Enter` must insert a NEWLINE in the running app (e.g. Claude
 * Code / Ink) instead of SUBMITTING. xterm has no notion of Shift+Enter — on a
 * plain `Enter` it sends `\r`, and Shift+Enter would send the exact same `\r`,
 * which Claude Code reads as "submit". So we intercept the chord and write this
 * sequence to the PTY instead of the default `\r`.
 *
 * `\x1b\r` is ESC + CR — read as Meta/Alt+Enter, which Claude Code / Ink
 * interpret as "insert newline" (the common convention for terminals without
 * the kitty keyboard protocol).
 *
 * ⚠️ TO VALIDATE EMPIRICALLY in the GUI. If Claude Code does NOT treat `\x1b\r`
 * as a newline, change THIS one constant — likely fallbacks: `"\n"` (raw LF) or
 * `"\x1bOM"` / `"\x1b\n"`. The truly generic fix is the kitty keyboard protocol
 * (disambiguated key events), which is out of scope here — see FEEDBACK #13.
 * Keeping the sequence in a single named constant means there is exactly ONE
 * place to flip if the chosen encoding turns out wrong.
 */
const SHIFT_ENTER_SEQUENCE = "\x1b\r";

/**
 * Whether `e` is the `Shift+Enter` chord. We match the physical Enter key
 * (`e.code`, covering the main `Enter` and the numpad `NumpadEnter`) with Shift
 * held and NO other modifier — a plain `Enter` (no Shift) must stay untouched so
 * it still submits as `\r`, and `Ctrl/Alt/Meta+Enter` are left to their own
 * handling. `code` is layout-stable (more reliable than `key` here), with a
 * `key === "Enter"` fallback for environments that don't populate `code`.
 */
function isShiftEnterChord(e: KeyboardEvent): boolean {
  const isEnterKey = e.code === "Enter" || e.code === "NumpadEnter" || e.key === "Enter";
  return isEnterKey && e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey;
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

  // Whether this pane currently has a NON-ZERO layout box. An inactive deck pane,
  // and the whole deck while a command view covers it, are `display:none` → 0×0.
  // We gate the WebGL attach on this (below) so the addon never bakes its glyph
  // atlas / GL viewport against a 0×0 surface — building it while hidden is what
  // produced the garbled render when the pane reappeared (#20/#23). Seeded `false`
  // and flipped by the ResizeObserver in the fit effect as the box gains/loses size.
  const [paneVisible, setPaneVisible] = useState(false);

  // REVEAL GATE (FEEDBACK #33). When a hidden pane becomes active it flips from
  // `display:none` to `display:block` and the renderer paints a FIRST frame at
  // STALE metrics (cell width / WebGL glyph atlas baked while hidden) — the
  // classic "t e s t" inter-character spacing — which is only corrected one rAF
  // later by the activation reconcile (fit → resize → clearWebglAtlas → refresh).
  // For that one-frame gap the user sees the wrong render → the ~0.25s flash.
  //
  // Fix: keep the inner xterm container visually HIDDEN (opacity-0) from the
  // moment this pane becomes active until its first post-activation reconcile has
  // run, then reveal it (opacity-100). So the stale-metrics frame is painted while
  // invisible and the user only ever sees the CORRECT frame (≤ ~1 frame later).
  // We use `opacity` (not `display:none`/`visibility:hidden`) deliberately: the
  // element keeps its layout box, so fit()/reconcile still measure the real size
  // and WebGL still attaches against a non-zero surface during the hidden window.
  //
  // This is a per-activation window only — it is reset to `false` when `active`
  // flips true (see the activation reconcile effect below) and set back to `true`
  // at the END of that reconcile. A steady, long-active terminal stays revealed
  // (the flag is true and nothing resets it on a plain re-render), so we never
  // hide a terminal that is already settled. A standalone `<Terminal active>` is
  // active from mount: the reconcile runs one rAF after mount and reveals it then
  // (an imperceptible ~1-frame delay, smoothed by the opacity transition below).
  const [reconciledSinceActivation, setReconciledSinceActivation] = useState(false);

  // Attach the WebGL renderer ONLY while this terminal is active AND actually
  // visible (disposed on blur OR when hidden), so N open terminals never exhaust
  // the browser's WebGL context pool and no context is ever built at 0×0. A hidden
  // pane reappearing flips `paneVisible` → the addon attaches fresh against the
  // real size. Returns `clearAtlas()` so the reconcile pipeline can rebuild the
  // glyph atlas (baked at attach against the then-current metrics).
  const clearWebglAtlas = useWebglAddon(instance ?? null, active && paneVisible);

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

  // RESUME STALE-CANVAS REPAINT (FEEDBACK #25). On restart a RESUMED `claude`
  // terminal can show a frame that looks complete but whose canvas is STALE: the
  // user can't scroll down (the viewport is correctly pinned at the bottom) and
  // must scroll up then down to force the right render. Root cause: xterm's
  // renderer PAUSES (RenderService._isPaused) via an IntersectionObserver while a
  // pane is display:none/0×0 or just made visible (the observer reports a frame+
  // later, async). While paused every repaint is swallowed (refreshRows early-
  // returns). The one-shot activation reconcile runs one rAF after activation and
  // can fire WHILE paused → its refresh+scroll are no-ops; meanwhile a resumed
  // `claude --resume` streams its frame over several async pty://output events
  // AFTER that reconcile (Ink uses DEC 2026 synchronized output), so the final
  // buffer state never gets an un-paused repaint → stale canvas until a manual
  // scroll (which works only because by then the observer has un-paused).
  //
  // Fix: an OUTPUT-DRIVEN debounced repaint-if-at-bottom. Each pty://output chunk
  // pings `onOutput` (below); we schedule a trailing-edge debounce that — ONLY
  // while active AND within a short SETTLE window after the pane became active —
  // repaints the visible rows and re-pins to the bottom, but ONLY if the live
  // buffer is still at the bottom. Rationale: steady-state streaming is repainted
  // by xterm itself once un-paused, so the settle window only covers the resume
  // race; the at-bottom gate means a user who scrolled up is never yanked down;
  // and we deliberately do NOT fit()/resyncSize()/clearWebglAtlas() here (those
  // are expensive and would thrash SIGWINCH — the activation reconcile owns them).
  const settleDeadlineRef = useRef(0);
  const repaintTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Open a ~2500ms settle window each time the pane becomes active — the span in
  // which the resume frame is still streaming and the renderer may still be paused.
  useEffect(() => {
    if (!active) return;
    settleDeadlineRef.current = performance.now() + 2500;
  }, [active]);

  // Clear any pending debounce on unmount (the instance may be detaching).
  useEffect(
    () => () => {
      if (repaintTimerRef.current !== null) clearTimeout(repaintTimerRef.current);
    },
    [],
  );

  // Wire the live PTY backend (spawn / IO / resize / teardown). StrictMode-safe.
  // `resyncSize` pushes the terminal's current cols/rows to the PTY out-of-band
  // from xterm's onResize event — used below to make the authoritative
  // post-font fit reach the PTY even if it raced the spawn. `deadHistory` is
  // written by the hook before spawn (see above) so its ordering vs. live output
  // is deterministic. `onOutput` drives the #25 resume repaint (see above).
  const resyncSize = usePty(instance, fitAddon, {
    cwd,
    recordId,
    onPtyId,
    deadHistory: deadHistoryPayload,
    onOutput: () => {
      // Gate cheaply on the OUTER conditions before arming the timer: only an
      // active pane within its post-activation settle window can be racing the
      // resume. Outside that, xterm's own un-paused repaint already covers it.
      if (!active || performance.now() > settleDeadlineRef.current) return;
      if (repaintTimerRef.current !== null) clearTimeout(repaintTimerRef.current);
      repaintTimerRef.current = setTimeout(() => {
        repaintTimerRef.current = null;
        try {
          if (!instance) return;
          // Read at-bottom FRESH from the live buffer (not the debounced/at-arm
          // value): a user who scrolled up between chunks must not be yanked down.
          const buf = instance.buffer.active;
          if (computeAtBottom(buf.viewportY, buf.baseY)) {
            instance.scrollToBottom();
            instance.refresh(0, Math.max(0, instance.rows - 1));
          }
        } catch {
          // Best-effort: a detached/closing instance must never throw out of a timer.
        }
      }, 80);
    },
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

  // Custom key handling on the xterm textarea. This one handler covers two
  // concerns; the return value decides whether xterm processes the key:
  //
  //  1. COPY / PASTE chords (Ctrl+Shift+C / Ctrl+Shift+V). xterm ships no
  //     copy/paste binding (it just forwards keys to the PTY), so we intercept
  //     these on `keydown`: copy the selection / paste from the clipboard, then
  //     return `false` so the chord is NOT sent to the PTY. Crucially this only
  //     matches WITH Shift — plain `Ctrl+C` still reaches the PTY as SIGINT, and
  //     `Ctrl+Shift+C` with no selection sends nothing parasitic. The clipboard
  //     work is async/best-effort (see ./terminal-clipboard); we fire it and
  //     swallow the result here (no logging of clipboard contents).
  //
  //  2. NAV chords (Ctrl/Cmd+T/W, Ctrl+Tab/PageUp/Down) pass THROUGH xterm
  //     untouched so they bubble to `document`, where the global TanStack Hotkeys
  //     listener handles them. Returning `false` tells xterm not to process the
  //     key (no PTY byte, no preventDefault/stopPropagation). Without this, xterm
  //     consumes the chord and stops propagation, so the shortcut would only fire
  //     when the sidebar — not a terminal — is focused.
  useEffect(() => {
    if (!instance) return;
    instance.attachCustomKeyEventHandler((e) => {
      // Only act on keydown so a chord is handled once (keyup/keypress repeat the
      // same modifiers and would double-fire the clipboard op).
      if (e.type === "keydown") {
        if (isCopyChord(e)) {
          // preventDefault so Chromium's native Ctrl+Shift+C can't fire alongside.
          e.preventDefault();
          void copySelection(instance);
          return false; // handled — do not also send ^C to the PTY.
        }
        if (isPasteChord(e)) {
          // preventDefault is LOAD-BEARING (FEEDBACK #5): `return false` tells xterm not
          // to process the key but does NOT cancel the DOM event, so Chromium's native
          // Ctrl+Shift+V paste still fires and xterm's own `paste` listener inserts a
          // SECOND copy. Cancelling the event kills the native paste → `term.paste()`
          // (in pasteFromClipboard) stays the single insertion path.
          e.preventDefault();
          void pasteFromClipboard(instance);
          return false; // handled — do not send the raw keystroke.
        }
        if (isShiftEnterChord(e)) {
          // Shift+Enter must INSERT A NEWLINE, not submit. xterm would otherwise
          // send the same `\r` as a plain Enter (→ the running app submits). We
          // write SHIFT_ENTER_SEQUENCE to the PTY instead. `instance.input()`
          // funnels through the SAME `onData` path as real keystrokes (see
          // usePty's `term.onData → pty_write`), so the bytes reach the PTY — not
          // just the screen. preventDefault + `return false` then suppress xterm's
          // default `\r`, so there is exactly ONE write (no double-send, cf. #5).
          e.preventDefault();
          instance.input(SHIFT_ENTER_SEQUENCE);
          return false; // handled — do not also send the default `\r`.
        }
      }
      // Yield nav chords (return false) so they bubble; let xterm handle the rest.
      return !isTerminalNavChord(e);
    });
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

  // WebGL is now managed by `useWebglAddon` (active+visible attach/dispose); see
  // `clearWebglAtlas` above.

  // GEOMETRY RECONCILIATION — the single, ordered pipeline (fit → resyncSize/PTY
  // resize+SIGWINCH → clearWebglAtlas → refresh → scrollToBottom) that makes the
  // pane's RENDER match its real pixel size. It is the fix for the whole
  // "dimensions / garbled render" family (FEEDBACK #20 + #23). The order and the
  // 0×0 gate live in `reconcileTerminalGeometry` (extracted + unit-tested in
  // `./terminal-geometry`); here we only bind it to this terminal's live deps.
  const reconcileGeometry = useCallback(() => {
    return reconcileTerminalGeometry({
      instance: instance ?? null,
      element: ref.current,
      fitAddon,
      resyncSize,
      clearWebglAtlas,
    });
  }, [instance, ref, fitAddon, resyncSize, clearWebglAtlas]);

  // Fit on open and keep the render reconciled as the container resizes / the pane
  // (re)appears. ResizeObserver is the single source of truth for sizing — no
  // manual layout math, no bottom anchor.
  //
  // LOAD-BEARING ordering for the "t e s t" spacing fix: the FIRST authoritative
  // fit must run AFTER the bundled monospace face is loaded, so xterm measures the
  // real glyph (not the fallback) when it computes the cell width. The reconcile
  // pipeline then rebuilds the WebGL glyph atlas (baked at open() against fallback
  // metrics) so it regenerates against the loaded font. Without this gating the
  // mis-spacing can persist (font-loading FOUT → bad measurement).
  useEffect(() => {
    if (!instance) return;
    const el = ref.current;
    if (!el) return;

    // Track whether the pane has a real box, so the WebGL attach is gated on it
    // (see `paneVisible`). Sync it immediately from the current layout, then keep
    // it in step from the observer below.
    const syncVisible = () => setPaneVisible(el.clientWidth > 0 && el.clientHeight > 0);
    syncVisible();

    // A bare fit (no atlas/refresh) for the very first frame: it only needs to give
    // the element dims for the PTY spawn (usePty reads proposeDimensions at spawn
    // time) so there is no 0×0 frame. The full, atlas-correct reconcile follows
    // once the face has loaded.
    const safeFit = () => {
      try {
        if (el.clientWidth > 0 && el.clientHeight > 0) fitAddon.fit();
      } catch {
        // ignore transient fit failures (no layout / detached)
      }
    };
    safeFit();

    let cancelled = false;

    // Resolve the live font family/size from the merged options so the load gate
    // targets exactly what xterm will measure.
    const fontFamily = mergedOptions.fontFamily ?? (DEFAULT_OPTIONS.fontFamily as string);
    const fontSize = mergedOptions.fontSize ?? (DEFAULT_OPTIONS.fontSize as number);

    void ensureTerminalFontLoaded(fontFamily, fontSize).then(() => {
      if (cancelled) return;
      // Real font loaded: run the full pipeline (fit measures the correct glyph,
      // PTY gets the real size + SIGWINCH, atlas rebuilds, screen repaints). The
      // ResizeObserver is NOT a backstop here — the element size is unchanged (only
      // the cell metric moved), so it would not re-fire.
      reconcileGeometry();
    });

    // The observer reconciles on EVERY size change — including a `display:none`
    // pane reappearing (a 0→N transition fires the observer). That is the backstop
    // for #23 (a terminal mounted hidden at boot, fitted only once it is shown) and
    // half of #20 (the deck reappearing from behind a command view). It also keeps
    // `paneVisible` in step so the WebGL addon attaches/detaches with real
    // visibility. The size-equal case of #20 (same window, atlas still stale, no
    // observer callback) is covered by the activation effect below.
    const observer = new ResizeObserver(() => {
      syncVisible();
      reconcileGeometry();
    });
    observer.observe(el);

    return () => {
      cancelled = true;
      observer.disconnect();
    };
  }, [instance, fitAddon, ref, mergedOptions, reconcileGeometry]);

  // A terminal can stay `active=true` while an ancestor hides the whole pane
  // (`display:none`, e.g. the deck behind a command view). In that shape the
  // activation reconcile may already have run and skipped the 0x0 box; when the
  // pane becomes measurable again we need an explicit post-visibility reconcile
  // even though `active` did not change.
  useEffect(() => {
    if (!instance || !active || !paneVisible) return;
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
      if (reconcileGeometry()) setReconciledSinceActivation(true);
    }) as unknown as number;
    return () => cancel(raf);
  }, [instance, active, paneVisible, reconcileGeometry]);

  // ACTIVATION RECONCILE (#20 + #23). When this terminal becomes the active/visible
  // pane we MUST rebuild geometry even if the element's pixel size did NOT change —
  // the ResizeObserver would then never fire, yet the WebGL atlas/canvas can be
  // stale from when the pane was hidden (the whole deck sat behind a CommandView, or
  // the terminal was mounted `display:none` at boot). Two cases:
  //   - coming back from a command view at the SAME window size → no resize event,
  //     no observer callback → the stale atlas is rendered as a "bouillie" (#20);
  //   - a resumed `claude` at boot whose pty never got its final rows/cols → the
  //     forced resyncSize here delivers the SIGWINCH that makes the TUI redraw (#23).
  // We defer one rAF so the pane is actually displayed + laid out (it was
  // `display:none`; an element has no box — and fit() throws — until it is shown),
  // mirroring the focus-on-activate effect above. No-op while inactive (a hidden
  // pane reconciles when it next becomes active) and self-guards on a 0×0 element.
  //
  // This effect also drives the REVEAL GATE (#33): becoming active immediately
  // marks the pane "not yet reconciled" (keeping it opacity-0 so the stale frame
  // is hidden), and once the rAF reconcile has run we mark it reconciled →
  // revealed. The reset is synchronous on the `active→true` transition so the
  // hidden window starts BEFORE the browser ever paints the freshly-revealed
  // (display:block) stale frame.
  useEffect(() => {
    if (!instance || !active) return;
    // Re-arm the gate for THIS activation: hide until the reconcile below runs.
    setReconciledSinceActivation(false);
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
      const ran = reconcileGeometry();
      // Geometry is now correct (atlas rebuilt + rows refreshed against the real
      // cell size): reveal the pane so the user sees only the CORRECT frame.
      if (ran) setReconciledSinceActivation(true);
    }) as unknown as number;
    return () => cancel(raf);
  }, [instance, active, reconcileGeometry]);

  // RIGHT-CLICK copy/paste — the same clipboard helpers as the keyboard chords,
  // surfaced through `<TerminalContextMenu>`. Stable callbacks so the menu's props
  // don't churn each render; each is a no-op while the instance is absent.
  const handleHasSelection = useCallback(
    () => (instance ? instance.hasSelection() : false),
    [instance],
  );
  const handleCopy = useCallback(() => {
    if (instance) void copySelection(instance);
  }, [instance]);
  const handlePaste = useCallback(() => {
    if (instance) void pasteFromClipboard(instance);
  }, [instance]);

  // Floating "jump to bottom" affordance (#14): track whether the viewport is
  // pinned to the live bottom and expose a smooth scroll-to-bottom. The button is
  // shown only while scrolled up; the hook recomputes on scroll AND on print/resize
  // so it appears/disappears live as output streams.
  const { atBottom, scrollToBottom } = useAtBottom(instance ?? null);

  // OUTER container carries the padding + background; the INNER `ref` div is
  // where xterm opens and is what the ResizeObserver/FitAddon measure. FitAddon
  // therefore sizes cols/rows to the INNER (padded) area, giving correct
  // dimensions plus a visual margin — no edge column/row gets clipped, and we do
  // NOT touch xterm's native scroll or reintroduce any custom bottom anchor.
  return (
    <TerminalContextMenu
      hasSelection={handleHasSelection}
      onCopy={handleCopy}
      onPaste={handlePaste}
    >
      <div className={cn("relative h-full w-full overflow-hidden bg-background p-2.5", className)}>
        {/* REVEAL GATE (#33): the inner xterm container is kept opacity-0 from the
            moment this pane becomes active until its first post-activation geometry
            reconcile has run, then revealed. This hides the stale-metrics first
            frame ("t e s t" spacing) that xterm paints when a hidden pane flips to
            display:block, so the user only ever sees the corrected render. We gate
            on `active` so an inactive (display:none) pane is irrelevant, and on
            `reconciledSinceActivation` so a steady, already-revealed terminal is
            never re-hidden. opacity (not display:none/visibility) keeps the layout
            box during the hidden window so fit()/WebGL still measure the real size.
            The short transition softens the reveal of the default-active socle. */}
        <div
          ref={ref}
          className={cn(
            "h-full w-full overflow-hidden transition-opacity duration-100",
            active && !reconciledSinceActivation ? "opacity-0" : "opacity-100",
          )}
        />
        {/* Jump-to-bottom: hidden (and click-through) while pinned to the bottom, so
            it never blocks the terminal when there is nothing to scroll back to. */}
        <Button
          size="icon-sm"
          variant="secondary"
          aria-label="Scroll to bottom"
          aria-hidden={atBottom}
          tabIndex={atBottom ? -1 : 0}
          onClick={scrollToBottom}
          className={cn(
            "absolute right-3 bottom-3 z-10 rounded-full shadow-md transition-opacity duration-150",
            atBottom ? "pointer-events-none opacity-0" : "opacity-100",
          )}
        >
          <ArrowDownIcon />
        </Button>
      </div>
    </TerminalContextMenu>
  );
}

export default Terminal;
