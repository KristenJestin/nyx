import { describe, expect, it } from "vitest";

import { EMPTY_FILTERS, matches, tally, totalSelected } from "./board-filtering";
import type { SpecListEntry } from "./types";

/** A minimal `SpecListEntry` fixture — only the fields the pure filter reads matter. */
function spec(overrides: Partial<SpecListEntry>): SpecListEntry {
  return {
    id: "s1",
    project_id: "p1",
    workspace_id: null,
    title: "A spec",
    slug: null,
    number: 1,
    priority: "med",
    status: "draft",
    mode: "stepped",
    phases: "[]",
    current_revision_number: 1,
    position: 0,
    created_at: 0,
    updated_at: 0,
    completed_at: null,
    archived_at: null,
    project_key: "NYX",
    tasks_done: 0,
    tasks_total: 0,
    blocked_by_count: 0,
    unblocks_count: 0,
    ...overrides,
  };
}

describe("board filtering", () => {
  it("passes every Spec when no facet is selected", () => {
    expect(matches(spec({ priority: "urgent" }), EMPTY_FILTERS)).toBe(true);
    expect(matches(spec({ priority: "low" }), EMPTY_FILTERS)).toBe(true);
  });

  it("keeps only Specs whose priority is selected", () => {
    const filters = { priority: ["urgent", "high"] };
    expect(matches(spec({ priority: "urgent" }), filters)).toBe(true);
    expect(matches(spec({ priority: "high" }), filters)).toBe(true);
    expect(matches(spec({ priority: "med" }), filters)).toBe(false);
    expect(matches(spec({ priority: "low" }), filters)).toBe(false);
  });

  it("tallies Specs satisfying a predicate", () => {
    const specs = [
      spec({ priority: "urgent" }),
      spec({ priority: "urgent" }),
      spec({ priority: "low" }),
    ];
    expect(tally(specs, (s) => s.priority === "urgent")).toBe(2);
    expect(tally(specs, (s) => s.priority === "low")).toBe(1);
  });

  it("counts the total selected facet values", () => {
    expect(totalSelected(EMPTY_FILTERS)).toBe(0);
    expect(totalSelected({ priority: ["urgent", "low"] })).toBe(2);
  });
});
