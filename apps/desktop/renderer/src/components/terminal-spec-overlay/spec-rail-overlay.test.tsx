import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  BAR_TRACK,
  POINT_TONE,
  RAIL_BAR_HEIGHT_PX,
  RAIL_DOT_SIZE_PX,
  type BarSegment,
  type RailRollup,
} from "./overlay-status";
import { RailTabContent } from "./spec-rail-overlay";

/** Query a bar zone by its stable `data-rail-bar-zone` hook (done/active/blocked/skipped). */
function zone(el: HTMLElement, key: BarSegment["key"]): HTMLElement | null {
  return el.querySelector<HTMLElement>(`[data-rail-bar-zone="${key}"]`);
}
/** The continuous base-fill layer of a phase pill (`data-rail-fill`). */
function fill(el: HTMLElement): HTMLElement | null {
  return el.querySelector<HTMLElement>("[data-rail-fill]");
}
/** Every rendered phase pill (`data-rail-phase`), in order. */
function phases(el: HTMLElement): HTMLElement[] {
  return [...el.querySelectorAll<HTMLElement>("[data-rail-phase]")];
}
/** A bar rollup wrapping a single current-phase bar carrying the given zone segments. */
function barRollup(segments: BarSegment[]): RailRollup {
  return { items: [{ kind: "bar", pct: 0, segments }] };
}
/** The ordered `kind` of every phase item the rail rendered (via the `data-rail-kind` hook). */
function railKinds(el: HTMLElement): string[] {
  return [...el.querySelectorAll<HTMLElement>("[data-rail-phase]")].map(
    (n) => n.dataset.railKind ?? "",
  );
}

describe("<RailTabContent> — per-status stacked bar (done/active/blocked/skipped, todo empty)", () => {
  it("stacks done → active → blocked → skipped top→down with cumulative `top`, global tones", () => {
    // One task per status ⇒ 25% each ⇒ tops 0/25/50/75. Tones = the global task-status dot classes.
    const rollup = barRollup([
      { key: "done", tone: "bg-success", heightPct: 25, pulse: false },
      { key: "active", tone: "bg-warning", heightPct: 25, pulse: true },
      { key: "blocked", tone: "bg-destructive", heightPct: 25, pulse: false },
      { key: "skipped", tone: "bg-muted-foreground/30", heightPct: 25, pulse: false },
    ]);
    const { container } = render(<RailTabContent rollup={rollup} reduced={false} />);

    // DONE zone: success, anchored at the TOP (top 0), height = 25%.
    const done = zone(container, "done");
    expect(done?.className).toMatch(/bg-success/);
    expect(done?.style.top).toBe("0%");
    expect(done?.style.height).toBe("25%");

    // ACTIVE zone: warning, PULSING, stacked directly below done (top = 25%).
    const active = zone(container, "active");
    expect(active?.className).toMatch(/bg-warning/);
    expect(active?.className).toMatch(/animate-pulse/);
    expect(active?.style.top).toBe("25%");
    expect(active?.style.height).toBe("25%");

    // BLOCKED zone: destructive (RED) — no longer invisible/grey — stacked below active (top = 50%).
    const blocked = zone(container, "blocked");
    expect(blocked?.className).toMatch(/bg-destructive/);
    expect(blocked?.className).not.toMatch(/animate-pulse/);
    expect(blocked?.style.top).toBe("50%");
    expect(blocked?.style.height).toBe("25%");

    // SKIPPED zone: neutral, stacked below blocked (top = 75%).
    const skipped = zone(container, "skipped");
    expect(skipped?.className).toMatch(/bg-muted-foreground\/30/);
    expect(skipped?.style.top).toBe("75%");
    expect(skipped?.style.height).toBe("25%");
  });

  it("under reduced motion the ACTIVE zone is STATIC (rendered, but NO pulse)", () => {
    const rollup = barRollup([{ key: "active", tone: "bg-warning", heightPct: 40, pulse: true }]);
    const { container } = render(<RailTabContent rollup={rollup} reduced={true} />);

    const active = zone(container, "active");
    expect(active).not.toBeNull(); // the zone still shows
    expect(active?.className).not.toMatch(/animate-pulse/); // …just not pulsing
  });

  it("an absent status ⇒ NO segment ⇒ NO element for that zone", () => {
    // Only a done zone present ⇒ active/blocked/skipped elements are absent.
    const rollup = barRollup([{ key: "done", tone: "bg-success", heightPct: 100, pulse: false }]);
    const { container } = render(<RailTabContent rollup={rollup} reduced={false} />);

    expect(zone(container, "done")?.style.height).toBe("100%");
    expect(zone(container, "active")).toBeNull();
    expect(zone(container, "blocked")).toBeNull();
    expect(zone(container, "skipped")).toBeNull();
  });

  it("NO tasks (no segments) ⇒ an EMPTY bar (the track only, no zone elements)", () => {
    const { container } = render(<RailTabContent rollup={barRollup([])} reduced={false} />);

    expect(container.querySelectorAll("[data-rail-bar-zone]")).toHaveLength(0);
  });

  it("renders phase DOTS (done + remaining) around the bar without any bar zones on them", () => {
    const rollup: RailRollup = {
      items: [
        { kind: "done" },
        { kind: "bar", pct: 50, segments: [{ key: "done", tone: "bg-success", heightPct: 50, pulse: false }] },
        { kind: "remaining" },
      ],
    };
    const { container } = render(<RailTabContent rollup={rollup} reduced={false} />);

    // One bar (the current phase) with its done zone; the two dots carry no bar hooks.
    expect(container.querySelectorAll("[data-rail-bar-zone]")).toHaveLength(1);
    expect(zone(container, "done")?.style.height).toBe("50%");
  });
});

describe("<RailTabContent> — CONTINUOUS morph phase items (one element, height/radius, no crossfade)", () => {
  // 5 phases, phase 3 current ⇒ [done, done, bar, remaining, remaining].
  const timeline: RailRollup = {
    items: [
      { kind: "done" },
      { kind: "done" },
      { kind: "bar", pct: 50, segments: [{ key: "done", tone: "bg-success", heightPct: 50, pulse: false }] },
      { kind: "remaining" },
      { kind: "remaining" },
    ],
  };

  it("renders EXACTLY ONE phase item per rollup item, in order, each tagged with its kind", () => {
    const { container } = render(<RailTabContent rollup={timeline} reduced={false} />);
    // One motion.div (the `data-rail-phase` hook) per phase — same count, same order as the rollup.
    expect(container.querySelectorAll("[data-rail-phase]")).toHaveLength(timeline.items.length);
    expect(railKinds(container)).toEqual(["done", "done", "bar", "remaining", "remaining"]);
  });

  it("has NO dot/bar crossfade views — each phase is ONE continuous element with a single base fill", () => {
    const { container } = render(<RailTabContent rollup={timeline} reduced={false} />);

    // The old `<AnimatePresence mode="wait">` crossfade is GONE: no `data-rail-view` layers exist.
    expect(container.querySelectorAll("[data-rail-view]")).toHaveLength(0);
    // Every phase pill carries EXACTLY ONE always-mounted base-fill layer (the continuous content).
    for (const phase of phases(container)) {
      expect(phase.querySelectorAll("[data-rail-fill]")).toHaveLength(1);
    }
    // The current phase (`bar`) is the ONLY one carrying stacked zones; the dots carry none.
    const bar = phases(container).find((p) => p.dataset.railKind === "bar");
    expect(bar?.querySelector("[data-rail-bar-zone='done']")).not.toBeNull();
    expect(container.querySelectorAll("[data-rail-bar-zone]")).toHaveLength(1);
  });

  it("the SAME element animates HEIGHT + border-RADIUS by kind (dot 6px round ↔ bar 64px rect)", () => {
    const { container } = render(<RailTabContent rollup={timeline} reduced={false} />);
    const bar = phases(container).find((p) => p.dataset.railKind === "bar");
    const dot = phases(container).find((p) => p.dataset.railKind === "done");

    // The morph is a SIZE change of the SAME pill: the current-phase bar springs to the tall gauge
    // height, the finished-phase dot to the small dot height — never a swap of two representations.
    expect(bar?.style.height).toBe(`${RAIL_BAR_HEIGHT_PX}px`);
    expect(dot?.style.height).toBe(`${RAIL_DOT_SIZE_PX}px`);
    // The border-radius animates too: a fully-round dot (size ÷ 2) vs a softer rounded-rect bar.
    expect(dot?.style.borderRadius).toBe(`${RAIL_DOT_SIZE_PX / 2}px`);
    expect(bar?.style.borderRadius).not.toBe(dot?.style.borderRadius);
  });

  it("the base fill paints the dominant tone by kind (done=green, remaining=neutral, bar=track)", () => {
    const { container } = render(<RailTabContent rollup={timeline} reduced={false} />);
    const fills = phases(container).map((phase) => fill(phase));
    // Items 0,1 = done dots (green success); items 3,4 = remaining dots (visible neutral).
    expect(fills[0]?.className).toContain(POINT_TONE.done);
    expect(fills[1]?.className).toContain(POINT_TONE.done);
    expect(fills[3]?.className).toContain("bg-muted-foreground/60");
    expect(fills[4]?.className).toContain("bg-muted-foreground/60");
    // The current phase (`bar`) base fill is the neutral TRACK behind its stacked zones.
    expect(fills[2]?.className).toContain(BAR_TRACK);
  });

  it("under reduced motion the phase items still render (bar + dots) and the active zone is STATIC", () => {
    const rollup: RailRollup = {
      items: [
        { kind: "done" },
        {
          kind: "bar",
          pct: 40,
          segments: [{ key: "active", tone: "bg-warning", heightPct: 40, pulse: true }],
        },
        { kind: "remaining" },
      ],
    };
    const { container } = render(<RailTabContent rollup={rollup} reduced={true} />);

    // The morph still yields one item per phase (instant, but present).
    expect(railKinds(container)).toEqual(["done", "bar", "remaining"]);
    // The active zone shows but does NOT pulse under reduced motion.
    const active = zone(container, "active");
    expect(active).not.toBeNull();
    expect(active?.className).not.toMatch(/animate-pulse/);
  });
});
