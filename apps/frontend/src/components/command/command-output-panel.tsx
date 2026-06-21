import { useEffect, useMemo } from "react";
import { FitAddon } from "@xterm/addon-fit";
import type { ITerminalOptions, Terminal as XTerm } from "@xterm/xterm";
import { useXTerm } from "react-xtermjs";

import { cn } from "@/lib/utils";
import { useWebglAddon } from "@/components/terminal/use-webgl-addon";
import { ensureTerminalFontLoaded, resolveThemeFromCss } from "@/components/terminal/xterm-theme";
import { useCommandOutput } from "./use-command-output";

/**
 * Default xterm options for the READ-ONLY command output panel. The key
 * difference from the interactive `<Terminal>` is `disableStdin: true`: xterm
 * routes no keystroke to a (non-existent) data path and shows no live cursor —
 * the panel is watch-only. ANSI colour rendering is unchanged (the colours come
 * over the wire and are drawn by xterm), so a command's coloured output renders
 * with its colours. Selection + scroll remain available for reading.
 */
const READONLY_OPTIONS: ITerminalOptions = {
  convertEol: false,
  // No live shell here: hide the cursor and disable stdin so the panel reads as
  // an output surface, not an editable terminal.
  cursorBlink: false,
  cursorStyle: "bar",
  disableStdin: true,
  fontFamily: '"Fira Code Variable", "Fira Code", ui-monospace, "Liberation Mono", monospace',
  fontSize: 14,
  letterSpacing: 0,
  scrollback: 10_000,
};

// The xterm THEME + FONT helpers (resolveThemeFromCss, ensureTerminalFontLoaded,
// and the FALLBACK_THEME floor they use) are shared with `<Terminal>` via
// `@/components/terminal/xterm-theme` so both surfaces resolve their canvas
// colours + font-load gate from a single implementation.

export interface CommandOutputPanelProps {
  /**
   * The `command_instances.id` whose output to show. On change the panel rebuilds
   * its xterm (keyed in the parent) so a fresh instance starts from a clean buffer
   * and rehydrates that instance's history.
   */
  instanceId: string | null;
  /** Optional className for the full-bleed container (the parent gives it a size). */
  className?: string;
  /**
   * Surfaces the xterm instance to the parent / tests as it appears/disappears.
   * The seam the unit tests use to read the buffer and assert on rendered output.
   */
  onInstance?: (instance: XTerm | null) => void;
  /**
   * Whether this panel is the active/visible one. WebGL is attached ONLY while
   * active (disposed on blur), so many panels never exhaust the browser's WebGL
   * context pool. Defaults to `true` for a standalone panel / the test harness.
   */
  active?: boolean;
}

/**
 * `<CommandOutputPanel>` — the READ-ONLY xterm surface that renders a managed
 * command instance's output (T8). It reuses the terminal's xterm setup (WebGL via
 * `useWebglAddon`, fit addon, design-system theme, font-load gating) but is wired
 * to `useCommandOutput` — which rehydrates via `command_output` and streams
 * `command://output` filtered by `instanceId` — instead of `usePty`.
 *
 * READ-ONLY STRICT (the T8 contract): there is NO input path. `useCommandOutput`
 * wires no `term.onData`/`pty_write` and no resize→stdin, and `disableStdin` is
 * set on the xterm options, so the panel never sends a single byte to the process.
 * Selection + scroll (xterm-local, no backend traffic) stay available for reading.
 *
 * No animation anywhere in this viewport (the project directive): the xterm canvas
 * is never wrapped in a Motion element and nothing here animates the surface.
 */
export function CommandOutputPanel({
  instanceId,
  className,
  onInstance,
  active = true,
}: CommandOutputPanelProps) {
  // Stable across renders so `useXTerm`'s effect does not tear down + recreate the
  // terminal every render.
  const fitAddon = useMemo(() => new FitAddon(), []);
  const addons = useMemo(() => [fitAddon], [fitAddon]);
  const options = useMemo<ITerminalOptions>(() => ({ ...READONLY_OPTIONS }), []);

  const { ref, instance } = useXTerm({ options, addons });

  // WebGL only while active (disposed on blur). Returns `clearAtlas()` so the
  // post-font-load fit rebuilds the glyph atlas baked against fallback metrics.
  const clearWebglAtlas = useWebglAddon(instance ?? null, active);

  // The READ-ONLY output wiring: rehydrate via `command_output`, stream
  // `command://output` filtered by `instanceId`. NO input path (see the hook).
  useCommandOutput(instance ?? null, instanceId);

  // Surface the instance to the parent / tests as it appears/disappears.
  useEffect(() => {
    onInstance?.(instance ?? null);
    return () => onInstance?.(null);
  }, [instance, onInstance]);

  // Derive the terminal theme from the design-system palette at mount (oklch
  // tokens need the browser to convert; we resolve in an effect against the live
  // DOM). Re-runs if the terminal is recreated.
  useEffect(() => {
    if (!instance) return;
    const { background, foreground } = resolveThemeFromCss();
    instance.options.theme = { ...instance.options.theme, background, foreground };
  }, [instance]);

  // Fit on open and keep fitting as the container resizes. The first authoritative
  // fit runs AFTER the bundled monospace face loads so xterm measures the real
  // glyph (the "t e s t" spacing fix); then rebuild the WebGL atlas. NOTE: unlike
  // `<Terminal>` there is NO `resyncSize` — a command panel never informs the
  // process of its size (read-only strict: no resize → stdin backchannel).
  useEffect(() => {
    if (!instance) return;
    const el = ref.current;
    if (!el) return;

    const safeFit = () => {
      try {
        if (el.clientWidth > 0 && el.clientHeight > 0) fitAddon.fit();
      } catch {
        // ignore transient fit failures (no layout / detached)
      }
    };
    safeFit();

    let cancelled = false;
    const fontFamily = options.fontFamily ?? (READONLY_OPTIONS.fontFamily as string);
    const fontSize = options.fontSize ?? (READONLY_OPTIONS.fontSize as number);
    void ensureTerminalFontLoaded(fontFamily, fontSize).then(() => {
      if (cancelled) return;
      safeFit();
      clearWebglAtlas();
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
  }, [instance, fitAddon, ref, options, clearWebglAtlas]);

  return (
    <div className={cn("h-full w-full overflow-hidden bg-background p-2.5", className)}>
      <div ref={ref} className="h-full w-full overflow-hidden" />
    </div>
  );
}
