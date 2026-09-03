import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

import type { SpecDetail } from "../types";
import { ActivityTab } from "./activity-tab";

/** A minimal `spec_detail` fixture — the Activity tab only reads `detail.spec.id`. */
function detailFixture(): SpecDetail {
  return {
    spec: {
      id: "s1",
      project_id: "p1",
      workspace_id: "w1",
      title: "Spec",
      slug: null,
      number: 1,
      priority: "med",
      status: "in_progress",
      mode: "stepped",
      phases: "[]",
      category_id: null,
      current_revision_number: 1,
      position: 0,
      created_at: 0,
      updated_at: 0,
      completed_at: null,
      archived_at: null,
    },
    revision: {
      id: "r1",
      spec_id: "s1",
      revision_number: 1,
      revision_status: "active",
      title: "Spec",
      problem: "",
      solution: "",
      implementation_decisions: "",
      testing_decisions: "",
      user_stories: "",
      out_of_scope: "",
      change_reason: null,
      created_at: 0,
    },
    tasks: [],
    dependencies: [],
    workspace_context: null,
    category: null,
  };
}

/** Convenience: build an entry with a JSON payload (defaulting the `source` to `mcp`). */
function entry(
  sequence: number,
  event_type: string,
  aggregate: string,
  payload: Record<string, unknown>,
  source = "mcp",
): Record<string, unknown> {
  return {
    sequence,
    event_type,
    aggregate,
    source,
    payload: JSON.stringify(payload),
    created_at: 0,
  };
}

describe("ActivityTab", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("consumes `spec_activity` (keyed on the Spec id) and renders spec + task activity", async () => {
    const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "spec_activity") {
        return {
          spec_id: "s1",
          entries: [
            entry(1, "spec.created", "spec", { spec_id: "s1" }),
            entry(2, "spec.section_updated", "spec", { spec_id: "s1", section: "problem" }),
            entry(3, "task.added", "task", { task_id: "t1", spec_id: "s1" }),
            entry(4, "task.transitioned", "task", {
              task_id: "t1",
              spec_id: "s1",
              from: "todo",
              to: "in_progress",
            }),
            entry(5, "spec.transitioned", "spec", { spec_id: "s1", from: "draft", to: "ready" }),
          ],
        };
      }
      return undefined;
    });

    render(<ActivityTab detail={detailFixture()} />);

    // Spec creation, section edit, task add + transition, and the Spec transition all render —
    // NOT just a project.created line. Enriched labels come from the event payloads.
    expect(await screen.findByText("Spec created")).toBeTruthy();
    expect(screen.getByText("Section updated: problem")).toBeTruthy();
    expect(screen.getByText("Task added")).toBeTruthy();
    expect(screen.getByText("Task transitioned todo → in progress")).toBeTruthy();
    expect(screen.getByText("Spec transitioned draft → ready")).toBeTruthy();

    // The feed is SPEC-SCOPED: the read was keyed on the Spec id.
    const activityCall = calls.find((c) => c.cmd === "spec_activity");
    expect(activityCall?.args).toMatchObject({ specId: "s1" });
  });

  it("renders NEWEST first (descending sequence), not the backend's oldest-first order", async () => {
    mockIPC((cmd) => {
      if (cmd === "spec_activity") {
        return {
          spec_id: "s1",
          // The read model returns oldest-first; the tab must invert to newest-first.
          entries: [
            entry(1, "spec.created", "spec", { spec_id: "s1" }),
            entry(2, "task.added", "task", { task_id: "t1", spec_id: "s1" }),
            entry(3, "spec.transitioned", "spec", { spec_id: "s1", from: "draft", to: "ready" }),
          ],
        };
      }
      return undefined;
    });

    const { container } = render(<ActivityTab detail={detailFixture()} />);
    await screen.findByText("Spec created");

    const labels = Array.from(container.querySelectorAll("ol li")).map((li) =>
      (li.textContent ?? "").trim(),
    );
    // The most recent (sequence 3) is at the TOP; the oldest (sequence 1) at the BOTTOM.
    expect(labels[0]).toContain("Spec transitioned draft → ready");
    expect(labels[1]).toContain("Task added");
    expect(labels[2]).toContain("Spec created");
  });

  it("shows a DISTINCT per-event-type icon in the circle alongside the SOURCE badge", async () => {
    mockIPC((cmd) => {
      if (cmd === "spec_activity") {
        return {
          spec_id: "s1",
          entries: [
            entry(1, "spec.created", "spec", { spec_id: "s1" }),
            entry(2, "spec.section_updated", "spec", { spec_id: "s1", section: "problem" }),
            entry(3, "spec.transitioned", "spec", { spec_id: "s1", from: "draft", to: "ready" }),
            entry(4, "spec.forked", "spec", { spec_id: "s1", from: 1, to: 2 }),
            entry(5, "spec.workspace_changed", "spec", { spec_id: "s1", from: null, to: "w1" }),
            entry(6, "task.added", "task", { task_id: "t1", spec_id: "s1" }),
            entry(7, "task.transitioned", "task", {
              task_id: "t1",
              spec_id: "s1",
              from: "todo",
              to: "in_progress",
              verb: "start",
            }),
            entry(8, "task.dependency_added", "task", { task_id: "t1", spec_id: "s1" }),
          ],
        };
      }
      return undefined;
    });

    const { container } = render(<ActivityTab detail={detailFixture()} />);
    await screen.findByText("Spec created");

    // Each event type maps to its own lucide icon (rendered as an svg with a `lucide-*` class).
    expect(container.querySelector(".lucide-file-plus")).toBeTruthy(); // spec.created
    expect(container.querySelector(".lucide-pencil")).toBeTruthy(); // spec.section_updated
    expect(container.querySelector(".lucide-workflow")).toBeTruthy(); // spec.transitioned
    expect(container.querySelector(".lucide-git-fork")).toBeTruthy(); // spec.forked
    expect(container.querySelector(".lucide-anchor")).toBeTruthy(); // spec.workspace_changed
    expect(container.querySelector(".lucide-list-plus")).toBeTruthy(); // task.added
    expect(container.querySelector(".lucide-arrow-right-left")).toBeTruthy(); // task.transitioned
    expect(container.querySelector(".lucide-link")).toBeTruthy(); // task.dependency_added

    // The generic `activity` glyph is NOT used for a known type anymore.
    expect(container.querySelector(".lucide-activity")).toBeNull();

    // The SOURCE badge is BACK: every entry shows WHO acted (all `mcp` here → "MCP").
    expect(screen.getAllByText("MCP").length).toBe(8);
  });

  it("renders a clean SOURCE badge for each origin (mcp/ui/system + a title-cased fallback)", async () => {
    mockIPC((cmd) => {
      if (cmd === "spec_activity") {
        return {
          spec_id: "s1",
          entries: [
            entry(1, "spec.created", "spec", { spec_id: "s1" }, "mcp"),
            entry(2, "spec.section_updated", "spec", { spec_id: "s1", section: "problem" }, "ui"),
            entry(3, "task.added", "task", { task_id: "t1", spec_id: "s1" }, "system"),
            entry(4, "task.dependency_added", "task", { task_id: "t1", spec_id: "s1" }, "cron"),
          ],
        };
      }
      return undefined;
    });

    render(<ActivityTab detail={detailFixture()} />);
    await screen.findByText("Spec created");

    // `mcp` → "MCP", `ui` → "User", `system` → "System"; anything else is title-cased ("Cron").
    expect(screen.getByText("MCP")).toBeTruthy();
    expect(screen.getByText("User")).toBeTruthy();
    expect(screen.getByText("System")).toBeTruthy();
    expect(screen.getByText("Cron")).toBeTruthy();
  });

  it("enriches the label with the transition verb + reason, and labels forks / re-anchoring", async () => {
    mockIPC((cmd) => {
      if (cmd === "spec_activity") {
        return {
          spec_id: "s1",
          entries: [
            entry(1, "spec.forked", "spec", { spec_id: "s1", from: 1, to: 2 }),
            entry(2, "spec.workspace_changed", "spec", { spec_id: "s1", from: null, to: "w1" }),
            entry(3, "spec.workspace_changed", "spec", { spec_id: "s1", from: "w1", to: null }),
            entry(4, "task.transitioned", "task", {
              task_id: "t1",
              spec_id: "s1",
              from: "in_progress",
              to: "blocked",
              verb: "block",
              reason: "waiting on API",
            }),
            entry(5, "task.transitioned", "task", {
              task_id: "t2",
              spec_id: "s1",
              from: "todo",
              to: "cancelled",
              verb: "skip",
              reason: "obsolete",
            }),
            entry(6, "task.transitioned", "task", {
              task_id: "t3",
              spec_id: "s1",
              from: "in_progress",
              to: "done",
              verb: "verify",
            }),
          ],
        };
      }
      return undefined;
    });

    render(<ActivityTab detail={detailFixture()} />);

    expect(await screen.findByText("Spec forked: revision 1 → 2")).toBeTruthy();
    expect(screen.getByText("Spec anchored to a workspace")).toBeTruthy();
    expect(screen.getByText("Spec workspace detached")).toBeTruthy();
    expect(screen.getByText("Task blocked: waiting on API")).toBeTruthy();
    expect(screen.getByText("Task skipped: obsolete")).toBeTruthy();
    expect(screen.getByText("Task verified")).toBeTruthy();
  });

  it("names the CATEGORY an assignment set, and says REMOVED when it was cleared", async () => {
    mockIPC((cmd) => {
      if (cmd === "spec_activity") {
        return {
          spec_id: "s1",
          entries: [
            entry(1, "spec.category_set", "spec", {
              spec_id: "s1",
              category_id: "c1",
              slug: "bug",
            }),
            // The REMOVAL — `category_id: null` is how the writer spells it. The generic fallback
            // used to render this as "Spec category set", i.e. the opposite of what happened.
            entry(2, "spec.category_set", "spec", {
              spec_id: "s1",
              category_id: null,
              slug: null,
            }),
          ],
        };
      }
      return undefined;
    });

    const { container } = render(<ActivityTab detail={detailFixture()} />);

    expect(await screen.findByText("Category removed")).toBeTruthy();
    expect(screen.getByText("Category set to bug")).toBeTruthy();
    expect(screen.queryByText("Spec category set")).toBeNull();
    // …and the type has its own glyph rather than the generic activity one.
    expect(container.querySelector(".lucide-tag")).toBeTruthy();
    expect(container.querySelector(".lucide-activity")).toBeNull();
  });

  it("shows an empty state when the Spec has no activity yet", async () => {
    mockIPC((cmd) => {
      if (cmd === "spec_activity") return { spec_id: "s1", entries: [] };
      return undefined;
    });

    render(<ActivityTab detail={detailFixture()} />);
    await waitFor(() => expect(screen.getByText("No activity recorded yet.")).toBeTruthy());
  });
});
