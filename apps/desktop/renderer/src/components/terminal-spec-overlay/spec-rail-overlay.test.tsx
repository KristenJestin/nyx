import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { taskStatusMeta } from "../specs/spec-meta";
import {
  RAIL_DOT_TONE,
  RAIL_NEEDS_TONE,
  RAIL_TRACK_TONE,
  type RailModel,
  type RailPhase,
  type RailSegment,
  type RailUnit,
} from "./overlay-status";
import { RailTabContent } from "./spec-rail-overlay";

/** Every rendered phase (`data-rail-phase`), in order. */
function phases(el: HTMLElement): HTMLElement[] {
  return [...el.querySelectorAll<HTMLElement>("[data-rail-phase]")];
}
/** The ordered `kind` of every rendered phase. */
function railKinds(el: HTMLElement): string[] {
  return phases(el).map((n) => n.dataset.railKind ?? "");
}
/** Every rendered plan/round unit (`data-rail-unit`), in order. */
function units(el: HTMLElement): HTMLElement[] {
  return [...el.querySelectorAll<HTMLElement>("[data-rail-unit]")];
}
/** The segment cells (`data-rail-seg`) of a unit, in order. */
function segsOf(unitEl: HTMLElement): HTMLElement[] {
  return [...unitEl.querySelectorAll<HTMLElement>("[data-rail-seg]")];
}

// --- segment fixtures: one per status, mirroring `segmentOf` (dot ↔ segment coherence) ---
const doneSeg: RailSegment = {
  state: "done",
  tone: taskStatusMeta("done").dot,
  transparent: false,
  pulse: false,
};
const progSeg: RailSegment = {
  state: "in_progress",
  tone: taskStatusMeta("in_progress").dot,
  transparent: false,
  pulse: true,
};
const blockedSeg: RailSegment = {
  state: "blocked",
  tone: taskStatusMeta("blocked").dot,
  transparent: false,
  pulse: true,
};
const needsSeg: RailSegment = {
  state: "needs_you",
  tone: RAIL_NEEDS_TONE,
  transparent: false,
  pulse: true,
};
const todoSeg: RailSegment = {
  state: "todo",
  tone: taskStatusMeta("todo").dot,
  transparent: true,
  pulse: false,
};

/** A current-phase bar with the given units. */
function current(us: RailUnit[]): Extract<RailPhase, { kind: "current" }> {
  return { kind: "current", units: us };
}
const unit = (over: Partial<RailUnit> = {}): RailUnit => ({
  key: "plan",
  rework: false,
  segments: [],
  ...over,
});
const model = (ps: RailPhase[]): RailModel => ({ phases: ps });

describe("<RailTabContent> — dots (past/future) are ALWAYS grey, never a status colour", () => {
  it("renders one grey filled dot per past/future phase, and NEVER a success/green dot", () => {
    const { container } = render(
      <RailTabContent
        model={model([
          { kind: "past", rounds: 0 },
          current([unit({ segments: [progSeg, todoSeg] })]),
          { kind: "future", rounds: 0 },
        ])}
      />,
    );
    expect(railKinds(container)).toEqual(["past", "current", "future"]);
    const dots = [...container.querySelectorAll<HTMLElement>("[data-rail-dot]")];
    expect(dots).toHaveLength(2); // one per past + future phase
    for (const dot of dots) {
      expect(dot.className).toContain(RAIL_DOT_TONE); // grey neutral
      expect(dot.className).not.toContain(taskStatusMeta("done").dot); // never green
    }
  });

  it("a reworked past phase shows N hollow round RINGS tied by a vertical trait", () => {
    const { container } = render(
      <RailTabContent
        model={model([{ kind: "past", rounds: 2 }, current([unit({ segments: [progSeg] })])])}
      />,
    );
    const past = phases(container)[0];
    expect(past.querySelectorAll("[data-rail-round-ring]")).toHaveLength(2);
    // The trait (tie) is present only when the phase carries rounds.
    expect(past.querySelector("[data-rail-tie]")).not.toBeNull();
  });

  it("a plain phase (0 rounds) has no rings and no tie", () => {
    const { container } = render(
      <RailTabContent
        model={model([{ kind: "future", rounds: 0 }, current([unit({ segments: [progSeg] })])])}
      />,
    );
    const future = phases(container).find((p) => p.dataset.railKind === "future")!;
    expect(future.querySelectorAll("[data-rail-round-ring]")).toHaveLength(0);
    expect(future.querySelector("[data-rail-tie]")).toBeNull();
  });
});

describe("<RailTabContent> — the CURRENT phase is a strip of per-task status segments", () => {
  it("renders one glued unit per plan/round, hatched only on rework units", () => {
    const { container } = render(
      <RailTabContent
        model={model([
          current([
            unit({ key: "plan", rework: false, segments: [doneSeg] }),
            unit({ key: "round-1", rework: true, segments: [progSeg] }),
          ]),
        ])}
      />,
    );
    const us = units(container);
    expect(us.map((u) => u.dataset.railUnit)).toEqual(["plan", "round-1"]);
    // Only the rework unit carries the hatched-bg marker.
    expect(us[0].hasAttribute("data-rail-rework")).toBe(false);
    expect(us[1].hasAttribute("data-rail-rework")).toBe(true);
  });

  it("segments of a unit are CONTIGUOUS — NO separator between adjacent task segments", () => {
    const { container } = render(
      // 2 done tasks in one unit: they merge into one block, no trait between them.
      <RailTabContent model={model([current([unit({ segments: [doneSeg, doneSeg] })])])} />,
    );
    const cells = segsOf(units(container)[0]);
    expect(cells).toHaveLength(2);
    for (const cell of cells) {
      // No border/trait on a segment — the split line only lives BETWEEN units, not tasks.
      expect(cell.className).not.toMatch(/border/);
    }
  });

  it("a thin trait separates the UNITS only (border-t on every unit but the first)", () => {
    const { container } = render(
      <RailTabContent
        model={model([
          current([
            unit({ key: "plan", rework: false, segments: [doneSeg] }),
            unit({ key: "round-1", rework: true, segments: [progSeg] }),
          ]),
        ])}
      />,
    );
    const us = units(container);
    // The first unit carries no top trait; the following unit draws the inter-unit separator.
    expect(us[0].className).not.toMatch(/border-t/);
    expect(us[1].className).toContain("border-t");
    expect(us[1].className).toContain("border-background");
  });

  it("renders ONE segment per task, coloured by its status; todo renders at opacity 0", () => {
    const { container } = render(
      <RailTabContent
        model={model([current([unit({ segments: [doneSeg, progSeg, todoSeg] })])])}
      />,
    );
    const cells = segsOf(units(container)[0]);
    expect(cells.map((c) => c.dataset.railSeg)).toEqual(["done", "in_progress", "todo"]);
    // done: green tone, static.
    expect(cells[0].className).toContain(taskStatusMeta("done").dot);
    expect(cells[0].className).not.toMatch(/animate-pulse/);
    // in_progress: amber tone (same as the panel dot), pulses.
    expect(cells[1].className).toContain(taskStatusMeta("in_progress").dot);
    expect(cells[1].className).toMatch(/animate-pulse/);
    // todo: opacity 0, no colour tone painted (the track shows through).
    expect(cells[2].className).toContain("opacity-0");
    expect(cells[2].className).not.toContain(taskStatusMeta("todo").dot);
  });

  it("in_progress at 0 done still shows the AMBER segment (the bug: a 0%-fill bar went fully grey)", () => {
    const { container } = render(
      <RailTabContent
        model={model([current([unit({ segments: [progSeg, todoSeg, todoSeg] })])])}
      />,
    );
    const cells = segsOf(units(container)[0]);
    // The amber in_progress segment is painted (visible) even with nothing done.
    expect(cells[0].dataset.railSeg).toBe("in_progress");
    expect(cells[0].className).toContain(taskStatusMeta("in_progress").dot);
    expect(cells[0].className).not.toContain("opacity-0");
    // The two todo segments are the transparent ones.
    expect(cells[1].className).toContain("opacity-0");
    expect(cells[2].className).toContain("opacity-0");
  });

  it("BLOQUÉ (blocked): the task's segment is red (destructive) and pulses; a done segment stays green/static", () => {
    const { container } = render(
      <RailTabContent model={model([current([unit({ segments: [doneSeg, blockedSeg] })])])} />,
    );
    const cells = segsOf(units(container)[0]);
    expect(cells[1].dataset.railSeg).toBe("blocked");
    expect(cells[1].className).toContain(taskStatusMeta("blocked").dot);
    expect(cells[1].className).toMatch(/animate-pulse/);
    expect(cells[0].className).toContain(taskStatusMeta("done").dot);
    expect(cells[0].className).not.toMatch(/animate-pulse/);
  });

  it("BESOIN DE TOI (kind=human): a magenta (primary) segment that pulses", () => {
    const { container } = render(
      <RailTabContent model={model([current([unit({ segments: [needsSeg] })])])} />,
    );
    const cell = segsOf(units(container)[0])[0];
    expect(cell.dataset.railSeg).toBe("needs_you");
    expect(cell.className).toContain(RAIL_NEEDS_TONE);
    expect(cell.className).toMatch(/animate-pulse/);
  });

  it("a rework unit HATCHES its coloured segments, but not the transparent (todo) ones", () => {
    const { container } = render(
      <RailTabContent
        model={model([
          current([unit({ key: "round-1", rework: true, segments: [doneSeg, todoSeg] })]),
        ])}
      />,
    );
    const cells = segsOf(units(container)[0]);
    expect(cells[0].style.backgroundImage).toMatch(/repeating-linear-gradient/); // coloured ⇒ hatched
    expect(cells[1].style.backgroundImage).toBe(""); // transparent todo ⇒ no hatch
  });

  it("an empty unit (all cancelled / no tasks) renders a bare track — no segments", () => {
    const { container } = render(
      <RailTabContent model={model([current([unit({ segments: [] })])])} />,
    );
    const u = units(container)[0];
    expect(segsOf(u)).toHaveLength(0);
    expect(u.className).toContain(RAIL_TRACK_TONE); // the neutral track shows through
  });

  it("under reduced motion an in_progress segment renders but does NOT pulse", () => {
    const { container } = render(
      <RailTabContent model={model([current([unit({ segments: [progSeg] })])])} reduced={true} />,
    );
    const cell = segsOf(units(container)[0])[0];
    expect(cell).not.toBeNull();
    expect(cell.className).not.toMatch(/animate-pulse/);
  });
});
