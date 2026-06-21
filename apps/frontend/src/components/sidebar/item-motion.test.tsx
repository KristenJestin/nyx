import { render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { AnimatePresence } from "motion/react";
import { describe, expect, it, vi } from "vitest";

import { itemTransition, itemVariants } from "./item-motion";
import { TerminalItem } from "./terminal-item";
import type { TerminalRecord } from "./use-terminals";

describe("itemTransition (reduced-motion aware)", () => {
  it("returns a tween (duration, eased) when motion is allowed", () => {
    // The enter/exit uses a TWEEN so the height/opacity ramp shares one
    // easing/duration window.
    const t = itemTransition(false) as Record<string, unknown>;
    expect(t.type).toBeUndefined();
    expect(t.duration).toBe(0.2);
    expect(Array.isArray(t.ease)).toBe(true);
  });

  it("returns an instant (duration 0) transition when reduced motion is set", () => {
    const t = itemTransition(true) as Record<string, unknown>;
    expect(t.duration).toBe(0);
    expect(t.ease).toBeUndefined();
  });

  it("treats null (unknown preference) as motion allowed", () => {
    const t = itemTransition(null) as Record<string, unknown>;
    expect(t.duration).toBe(0.2);
  });
});

describe("itemVariants (enter + exit: fade + height collapse)", () => {
  it("ENTERS with a fade + height grow that settles at the final slot", () => {
    // The new row reveals AT ITS FINAL SLOT: height 0→auto, opacity 0→1, and the
    // neighbours make room as it grows.
    expect(itemVariants.initial).toMatchObject({ opacity: 0, height: 0 });
    expect(itemVariants.animate).toMatchObject({ opacity: 1, height: "auto" });
  });

  it("EXITS by collapsing height→0 + fading so the rows below can follow up", () => {
    // The removed row collapses (height→0) + fades; the animated height shrinks
    // the row in normal flow so the survivors slide up over the same window.
    expect(itemVariants).toHaveProperty("exit");
    expect(itemVariants.exit).toMatchObject({ opacity: 0, height: 0 });
  });

  it("has NO transform props (x/y/scale) — dnd-kit owns the row transform", () => {
    // Motion must animate ONLY height/opacity here. The row's `transform` belongs
    // to dnd-kit during a drag/reflow; a transform variant would fight it on the
    // same CSS property (the conflict that retired Motion's Reorder — item-motion.ts).
    for (const phase of [itemVariants.initial, itemVariants.animate, itemVariants.exit]) {
      const v = phase as Record<string, unknown>;
      expect(v.x).toBeUndefined();
      expect(v.y).toBeUndefined();
      expect(v.scale).toBeUndefined();
    }
  });
});

/**
 * REGRESSION GUARD for the double-tp / active-row-shrink that kept coming back
 * (finding 01KV3CMX5HVEEVA42ZEW486M0K). The root cause was TWO animators on the
 * row at once: the `height: 0→auto` variant AND a `layout` (FLIP) projection
 * (intrinsic to the old `Reorder.Item`). The fix is ONE animator for height — the
 * row is a plain `motion.li` animating height in normal flow — and it MUST clip
 * its content (`overflow-hidden`) so the collapse is clean. Drag-reorder moved to
 * dnd-kit, which only touches `transform` (a different CSS property), so it cannot
 * reintroduce the conflict.
 */
describe("row animation: single height animator (no double-tp / no shrink) — regression", () => {
  function rec(id: string): TerminalRecord {
    return {
      id,
      cwd: "/x",
      label: null,
      scrollback: "",
      status: "alive",
      order_index: 0,
      created_at: 0,
      updated_at: 0,
      closed_at: null,
    };
  }

  it("the row wrapper clips its content while collapsing (overflow-hidden)", () => {
    // `overflow-hidden` on the row's motion wrapper is required so the height
    // collapse does not spill content.
    mockIPC((cmd) => (cmd === "terminal_info" ? { cwd: "/x", foreground: "bash" } : null));
    render(
      <ul>
        <AnimatePresence initial={false}>
          <TerminalItem
            record={rec("1")}
            index={0}
            active={false}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </AnimatePresence>
      </ul>,
    );
    expect(screen.getByRole("listitem").className).toContain("overflow-hidden");
  });

  it("a removed row collapses then UNMOUNTS in a single pass (no second teleport / leftover)", async () => {
    // Single-animator proof at the unit level: under reduced motion the exit
    // collapse settles instantly, so after removal the row must be GONE — one
    // pass, no leftover ghost row that a competing animator could leave mid-flight.
    // Reduced motion is forced via matchMedia so Motion resolves the exit
    // synchronously.
    mockIPC((cmd) => (cmd === "terminal_info" ? { cwd: "/x", foreground: "bash" } : null));
    const mql = (q: string): MediaQueryList =>
      ({
        matches: q.includes("reduce"),
        media: q,
        onchange: null,
        addEventListener: () => {},
        removeEventListener: () => {},
        addListener: () => {},
        removeListener: () => {},
        dispatchEvent: () => false,
      }) as unknown as MediaQueryList;
    vi.stubGlobal("matchMedia", mql);

    function List({ ids }: { ids: string[] }) {
      return (
        <ul>
          <AnimatePresence initial={false}>
            {ids.map((id, i) => (
              <TerminalItem
                key={id}
                record={rec(id)}
                index={i}
                active={false}
                onSelect={vi.fn()}
                onClose={vi.fn()}
              />
            ))}
          </AnimatePresence>
        </ul>
      );
    }

    const { rerender } = render(<List ids={["1", "2"]} />);
    expect(screen.getAllByRole("listitem")).toHaveLength(2);

    // Remove row "1" → its exit runs; under reduced motion it collapses + unmounts
    // immediately, leaving exactly the one survivor (no second teleport / ghost).
    rerender(<List ids={["2"]} />);
    await waitFor(() => expect(screen.getAllByRole("listitem")).toHaveLength(1));

    vi.unstubAllGlobals();
  });
});
