import { useCallback, useLayoutEffect, useRef, type RefObject } from "react";
import { useReducedMotion } from "motion/react";

/**
 * `useActiveRail` + `<SelectionRail>` — the SELECTION channel rebuilt as a single
 * MEASURED bar (the v6 proto's approach), replacing the Motion `layoutId` rail.
 *
 * Why measured again: the `layoutId` rail FLIPed the bar between rows by re-mounting
 * a shared-layout element in the active row — elegant, but dnd-kit REMOUNTS rows
 * mid-drag, which broke that FLIP (the rail jumped / vanished). A measured rail only
 * reads the DOM, so nothing dnd-kit does to the rows can desync it.
 *
 * How it works: ONE `<span>` lives in a `position: relative` HOST that spans the
 * rows (see `<AppSidebar>`). We read the active row's VISIBLE box and drive the
 * bar's `top`/`height` via CSS vars. "Visible" = the row's rect INTERSECTED with
 * every clipping ancestor (a collapsing band's `overflow:hidden`, the scroll area)
 * up to the host — so as a band collapses the bar SHRINKS with it in real time
 * instead of hanging full-size until the row finally unmounts. We re-measure on
 * every layout change (selection, collapse, add/close, reorder, scroll, resize) via
 * a MutationObserver + ResizeObserver + scroll/resize listeners, each after layout
 * settles (rAF). Layout tracking is INSTANT (frame-by-frame, to ride the band's own
 * animation); only a SELECTION change glides (CSS transition). First paint is
 * instant; reduced motion ⇒ no glide.
 */

/** Top/bottom inset (px) so the bar reads as a centered accent, not a full block. */
const RAIL_INSET = 7;
const RAIL_TRANSITION =
  "top .22s cubic-bezier(.4,0,.2,1), height .22s cubic-bezier(.4,0,.2,1), opacity .18s";

/** Whether an element clips its overflow (so it can hide part of a descendant). */
function clips(overflow: string): boolean {
  return (
    overflow === "hidden" || overflow === "auto" || overflow === "scroll" || overflow === "clip"
  );
}

/**
 * The active row to put the bar on. While a drag is in progress, dnd-kit lifts the
 * dragged row (marked `data-dnd-dragging`, possibly into a portal outside the host)
 * and drops an inert CLONE (`data-dnd-placeholder`, which keeps the cloned
 * `aria-current`) into its slot. So: if the ACTIVE row is the one being dragged,
 * follow that lifted element (searched on the whole document); otherwise take the
 * active row inside the host, explicitly EXCLUDING the placeholder clone (which
 * would otherwise win the query and sit on a hidden/wrong box).
 */
function findActiveRow(host: HTMLElement): HTMLElement | null {
  const dragged = document.querySelector<HTMLElement>(
    '[data-dnd-dragging][data-rail-row][aria-current="true"]',
  );
  if (dragged) return dragged;
  return host.querySelector<HTMLElement>(
    '[data-rail-row][aria-current="true"]:not([data-dnd-placeholder])',
  );
}

/**
 * The active row's VISIBLE vertical span: its rect, clamped to every clipping
 * ancestor up to `host`. As a collapsing band (overflow:hidden, height→0) shrinks,
 * its rect bottom rises and clamps the row → the visible height falls to 0.
 */
function visibleSpan(el: HTMLElement, host: HTMLElement): { top: number; height: number } {
  const r = el.getBoundingClientRect();
  let top = r.top;
  let bottom = r.bottom;
  for (let node = el.parentElement; node && node !== host; node = node.parentElement) {
    if (clips(getComputedStyle(node).overflowY)) {
      const nr = node.getBoundingClientRect();
      top = Math.max(top, nr.top);
      bottom = Math.min(bottom, nr.bottom);
    }
  }
  return { top, height: Math.max(0, bottom - top) };
}

export function useActiveRail(activeKey: string | null) {
  const reduced = useReducedMotion();
  const hostRef = useRef<HTMLDivElement>(null);
  const railRef = useRef<HTMLSpanElement>(null);
  // True for the ~glide window after a selection change. The instant tracking loop
  // reads it so the style mutations a selection causes (badge clear, active class)
  // GLIDE the bar instead of snapping it. Set in the selection effect (a
  // useLayoutEffect, which runs BEFORE the MutationObserver microtask).
  const gliding = useRef(false);
  const glideTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Compute the bar's target (top/height relative to the host), or null if it
  // should be hidden (no active row, collapsed away, or clipped to nothing).
  const compute = useCallback((): { top: number; height: number } | null => {
    const host = hostRef.current;
    if (!host) return null;
    const active = findActiveRow(host);
    if (!active || active.offsetParent === null) return null;
    const span = visibleSpan(active, host);
    const height = span.height - RAIL_INSET * 2;
    if (height <= 0) return null;
    return { top: span.top - host.getBoundingClientRect().top + RAIL_INSET, height };
  }, []);

  const apply = useCallback(
    (t: { top: number; height: number } | null, glide: boolean) => {
      const rail = railRef.current;
      if (!rail) return;
      if (!t) {
        rail.style.opacity = "0";
        return;
      }
      rail.style.transition = glide && !reduced ? RAIL_TRANSITION : "none";
      rail.style.setProperty("--rail-top", `${t.top}px`);
      rail.style.setProperty("--rail-h", `${t.height}px`);
      rail.style.opacity = "1";
    },
    [reduced],
  );

  // A layout change (collapse/add/close/reorder/scroll/resize) can animate the
  // active row's position over many frames WITHOUT mutating anything we could
  // observe per-frame. So a trigger starts a rAF loop that RE-MEASURES every frame
  // (instant, riding the animation) until the bar is stable for a few frames.
  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let loop = 0;
    let stable = 0;
    let prev = "";
    const sig = (t: { top: number; height: number } | null) =>
      t ? `${Math.round(t.top)}:${Math.round(t.height)}` : "hidden";
    let wasDragging = false;
    const tick = () => {
      const dragging = !!document.querySelector("[data-dnd-dragging]");
      // A drag just ended: open a glide window so the bar SETTLES to its final slot
      // smoothly instead of popping into place the instant the drag is torn down.
      if (wasDragging && !dragging) {
        gliding.current = true;
        if (glideTimer.current) clearTimeout(glideTimer.current);
        glideTimer.current = setTimeout(() => {
          gliding.current = false;
        }, 260);
      }
      wasDragging = dragging;
      const t = compute();
      // Instant while tracking a layout animation, but GLIDE during the window after
      // a selection change OR a drop (so neither snaps the bar over its transition).
      apply(t, gliding.current);
      const s = sig(t);
      if (s === prev) {
        // Keep polling for the whole drag even when the pointer holds still — the
        // dragged element is often portaled out of the host, so its moves don't
        // trip the MutationObserver; only stop once stable AND no drag is active.
        if (++stable > 3 && !dragging) {
          loop = 0;
          return;
        }
      } else {
        stable = 0;
        prev = s;
      }
      loop = requestAnimationFrame(tick);
    };
    const schedule = () => {
      if (loop) return;
      stable = 0;
      prev = "";
      loop = requestAnimationFrame(tick);
    };
    schedule();
    const ro = new ResizeObserver(schedule);
    ro.observe(host);
    const mo = new MutationObserver(schedule);
    // NB: `aria-current` / `class` are NOT observed — selection (which toggles the
    // active row's class) is owned by the glide effect below; observing them here
    // would start the instant loop and snap instead of glide.
    mo.observe(host, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["style"],
    });
    host.addEventListener("scroll", schedule, true);
    window.addEventListener("resize", schedule);
    return () => {
      cancelAnimationFrame(loop);
      ro.disconnect();
      mo.disconnect();
      host.removeEventListener("scroll", schedule, true);
      window.removeEventListener("resize", schedule);
    };
  }, [compute, apply]);

  // Selection change → glide the bar to the new row. Open a glide window so the
  // instant tracking loop (woken by the selection's own style mutations) glides
  // too, instead of snapping over this transition.
  useLayoutEffect(() => {
    gliding.current = true;
    if (glideTimer.current) clearTimeout(glideTimer.current);
    glideTimer.current = setTimeout(() => {
      gliding.current = false;
    }, 260);
    const raf = requestAnimationFrame(() => apply(compute(), true));
    return () => cancelAnimationFrame(raf);
  }, [activeKey, compute, apply]);

  // Clear the glide timer on unmount.
  useLayoutEffect(() => {
    return () => {
      if (glideTimer.current) clearTimeout(glideTimer.current);
    };
  }, []);

  return { hostRef, railRef };
}

/**
 * The single magenta selection bar. Positioned by `useActiveRail` via the
 * `--rail-top` / `--rail-h` CSS vars; lives in the rail HOST (a `relative`
 * container spanning the rows). The ONLY magenta in a row — selection is otherwise
 * the row's text weight/opacity, kept orthogonal to the run-state dot/badge.
 */
export function SelectionRail({ railRef }: { railRef: RefObject<HTMLSpanElement | null> }) {
  return (
    <span
      ref={railRef}
      aria-hidden
      className="pointer-events-none absolute left-1 z-10 w-0.5 rounded-full bg-primary"
      style={{ top: "var(--rail-top, 0px)", height: "var(--rail-h, 0px)", opacity: 0 }}
    />
  );
}

export default SelectionRail;
