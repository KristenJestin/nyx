import { useEffect, useRef } from "react";
import { WebglAddon } from "@xterm/addon-webgl";
import type { Terminal as XTerm } from "@xterm/xterm";

/**
 * The slice of the WebGL addon this hook depends on. Narrowing to an interface
 * (rather than the concrete `WebglAddon`) lets tests inject a fake factory so
 * the attach/dispose CYCLE is exercised in jsdom — which has no real WebGL
 * context. The real context is only painted in Browser Mode (phase 4).
 */
export interface WebglLike {
  dispose(): void;
  onContextLoss(cb: () => void): void;
  clearTextureAtlas?(): void;
}

export interface UseWebglOptions {
  /** Factory for the WebGL addon. Defaults to the real `@xterm/addon-webgl`. */
  factory?: () => WebglLike;
}

/** The default factory: a real `@xterm/addon-webgl` instance. */
function defaultFactory(): WebglLike {
  return new WebglAddon() as unknown as WebglLike;
}

/**
 * Manage the WebGL renderer for ONE terminal so it is attached ONLY while the
 * terminal is ACTIVE (visible/focused), and disposed the instant it is not.
 *
 * Why: browsers cap the number of live WebGL contexts (~16). With many open
 * terminals, attaching WebGL to every one exhausts the pool and the browser
 * starts dropping contexts (`context-loss`) on the older ones — visible as
 * garbage/blank panes. By attaching the context to the active terminal alone and
 * releasing it on blur, at most ONE WebGL context exists at a time regardless of
 * how many terminals are open, so 15+ terminals never exhaust the pool. Inactive
 * terminals keep their xterm INSTANCE and BUFFER fully alive (they just render
 * with xterm's default DOM/canvas renderer while hidden, and keep receiving
 * output) — only the GPU context is freed.
 *
 * The effect is keyed on `[instance, active]`: it attaches when both are truthy
 * and its cleanup disposes the addon, so a transition active→inactive (blur)
 * disposes, and inactive→active (focus) attaches a fresh addon. Re-renders that
 * don't change `active` do NOT churn the context. Robust to a factory throwing
 * (WebGL unavailable / blocklisted) — it degrades to the default renderer
 * without surfacing an error.
 *
 * @param instance the xterm instance (null until created).
 * @param active whether this terminal is the active/visible one.
 * @param options injectable WebGL factory (defaults to the real addon).
 */
export function useWebglAddon(
  instance: XTerm | null,
  active: boolean,
  options: UseWebglOptions = {},
): () => void {
  const { factory = defaultFactory } = options;
  // Hold the live addon so the cleanup disposes exactly the one we attached.
  const addonRef = useRef<WebglLike | null>(null);

  useEffect(() => {
    if (!instance || !active) return;

    let addon: WebglLike | undefined;
    try {
      addon = factory();
      // On a runtime context loss, drop WebGL so xterm falls back to its default
      // renderer instead of painting nothing; clear our ref so cleanup is a
      // no-op on an already-disposed addon.
      addon.onContextLoss(() => {
        addon?.dispose();
        addon = undefined;
        addonRef.current = null;
      });
      (instance as unknown as { loadAddon(a: WebglLike): void }).loadAddon(addon);
      addonRef.current = addon;
    } catch {
      // WebGL unavailable (headless / blocklisted GPU / jsdom): clean fallback to
      // the default renderer; nothing to dispose.
      addon?.dispose();
      addon = undefined;
      addonRef.current = null;
    }

    return () => {
      addon?.dispose();
      addon = undefined;
      addonRef.current = null;
    };
    // factory is intentionally not a dep: it is stable per terminal and re-running
    // on its identity would needlessly churn the GL context.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instance, active]);

  // Rebuild the WebGL glyph atlas (no-op if no live context). The terminal's
  // post-font-load fit calls this so the atlas baked against fallback metrics is
  // regenerated against the loaded font. Stable across renders.
  return useRef(() => {
    try {
      addonRef.current?.clearTextureAtlas?.();
    } catch {
      // atlas rebuild is best-effort; never let it crash the render.
    }
  }).current;
}
