import { useCallback, useEffect, useRef, useState } from "react";
import { useReducedMotion } from "motion/react";
import type { Terminal as XTerm } from "@xterm/xterm";

/**
 * "At bottom" tolerance in ROWS. xterm's `buffer.active.viewportY` is the top row
 * of the visible viewport and `baseY` is the top row of the LAST page; when they
 * are equal the viewport shows the live bottom. One row of slack absorbs an
 * off-by-one during a print/resize so the affordance doesn't flicker while the
 * terminal is effectively pinned to the bottom.
 */
export const AT_BOTTOM_SLACK_ROWS = 1;

/**
 * PURE: is the viewport within `slack` rows of the bottom? `baseY >= viewportY`
 * always holds (you cannot scroll below the last page), so this is a one-sided
 * check. Extracted so the at-bottom decision is exercisable without a real xterm
 * (no layout / scrollback needed), the same testable-core pattern as
 * `reconcileTerminalGeometry`.
 */
export function computeAtBottom(
  viewportY: number,
  baseY: number,
  slack: number = AT_BOTTOM_SLACK_ROWS,
): boolean {
  return viewportY >= baseY - slack;
}

export interface AtBottom {
  /** True while the viewport is pinned to (within a row of) the live bottom. */
  atBottom: boolean;
  /** Smoothly (or instantly under reduced-motion) scroll the viewport to the bottom. */
  scrollToBottom: () => void;
}

/**
 * Track whether an xterm viewport is scrolled to the bottom, and expose a smooth
 * scroll-to-bottom (FEEDBACK #14). The `atBottom` flag drives a floating "jump to
 * bottom" button: it is recomputed on every `onScroll` (the user scrolled) AND
 * every `onRender` (a print or resize moved `baseY`), so the button appears the
 * moment the user scrolls up and disappears as soon as they are back at the live
 * bottom — even while output keeps streaming.
 *
 * `scrollToBottom` animates with an ease-out rAF loop (xterm's own
 * `scrollToBottom()` is instant); under `prefers-reduced-motion` it jumps
 * instantly. Any in-flight animation is cancelled on a new call and on unmount.
 */
export function useAtBottom(instance: XTerm | null): AtBottom {
  const [atBottom, setAtBottom] = useState(true);
  const reduced = useReducedMotion();
  const rafRef = useRef(0);

  useEffect(() => {
    if (!instance) {
      setAtBottom(true);
      return;
    }
    const recompute = () => {
      const buf = instance.buffer.active;
      setAtBottom(computeAtBottom(buf.viewportY, buf.baseY));
    };
    recompute();
    // onScroll: a user/programmatic scroll. onRender: a print/resize repaint that
    // can move baseY (new output) → the button must re-hide once back at bottom.
    const disposables = [instance.onScroll(recompute), instance.onRender(recompute)];
    return () => {
      for (const d of disposables) d.dispose();
    };
  }, [instance]);

  const scrollToBottom = useCallback(() => {
    if (!instance) return;
    cancelAnimationFrame(rafRef.current);
    if (reduced || typeof requestAnimationFrame !== "function") {
      instance.scrollToBottom();
      return;
    }
    // Ease-out: each frame closes ~30% of the remaining distance (min 1 row), so the
    // motion decelerates into the bottom instead of a hard jump.
    const step = () => {
      const buf = instance.buffer.active;
      const remaining = buf.baseY - buf.viewportY;
      if (remaining <= 0) return;
      instance.scrollLines(Math.max(1, Math.ceil(remaining * 0.3)));
      rafRef.current = requestAnimationFrame(step);
    };
    rafRef.current = requestAnimationFrame(step);
  }, [instance, reduced]);

  // Cancel any in-flight scroll animation on unmount.
  useEffect(
    () => () => {
      if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(rafRef.current);
    },
    [],
  );

  return { atBottom, scrollToBottom };
}
