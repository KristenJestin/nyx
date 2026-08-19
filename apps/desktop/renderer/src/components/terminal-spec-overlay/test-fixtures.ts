/**
 * `test-fixtures` — the SHARED `nyx_core::db` read-model builders of the terminal↔Spec overlay
 * suites. `spec()` / `task()` / `detail()` / {@link ACTIVE_SESSION} mirror exactly what the
 * `terminal_specs` and `spec_detail` commands return, so a test can drive `<SpecOverlay>` (or one of
 * its parts) through the bridge fake without a live backend.
 *
 * Declared ONCE, on the model of `components/review/test-fixtures.ts`: this is a non-`.test` module,
 * so neither vitest project picks it up as a suite (both `include` globs match `.test`/`.spec` files
 * only) and the overlay's suites — unit and browser alike — import it instead of each carrying their
 * own copy. The `Spec`/`Task` schema moves (`grill_state`, `round`, `from_comment_id` are recent
 * additions), and a copy per suite drifts one `tsc` error at a time. Deliberately NOT an enumeration
 * of the importing files: the list grew twice without this comment following, which is the same
 * drift by another route — `rg 'from "./test-fixtures"'` answers "who consumes it" exactly, and
 * cannot go stale.
 */
import type { TerminalSpecsSession } from "@/bridge";

import type { Spec, SpecDetail, SpecRevision, Task } from "../specs/types";

/** A `nyx_core::db::Spec` fixture (the shape `terminal_specs` returns per Spec). */
export function spec(overrides: Partial<Spec>): Spec {
  return {
    id: "s1",
    project_id: "p1",
    workspace_id: null,
    title: "Untitled spec",
    slug: null,
    number: 1,
    priority: "med",
    status: "in_progress",
    mode: "stepped",
    phases: "[]",
    current_revision_number: 1,
    position: 0,
    created_at: 0,
    updated_at: 0,
    completed_at: null,
    archived_at: null,
    ...overrides,
  };
}

/** A `tasks` row fixture. */
export function task(overrides: Partial<Task>): Task {
  return {
    id: "t1",
    project_id: "p1",
    spec_id: "s1",
    parent_task_id: null,
    kind: "spec",
    executor: "agent",
    type_: "",
    status: "todo",
    priority: "med",
    severity: null,
    title: "A task",
    description: null,
    acceptance_criteria: null,
    verification_command: null,
    phase: null,
    position: 0,
    blocked_reason: null,
    skip_reason: null,
    created_at: 0,
    updated_at: 0,
    completed_at: null,
    from_comment_id: null,
    grill_state: null,
    round: null,
    ...overrides,
  };
}

/**
 * A `spec_detail` read-model fixture. `tasks` defaults to the empty decomposition (what a test that
 * only cares about the closed languette needs) and `revision` overrides let a test set section
 * bodies.
 */
export function detail(
  s: Spec,
  tasks: Task[] = [],
  revision: Partial<SpecRevision> = {},
): SpecDetail {
  return {
    spec: s,
    revision: {
      id: "r1",
      spec_id: s.id,
      revision_number: 1,
      revision_status: "active",
      title: s.title,
      problem: "",
      solution: "",
      implementation_decisions: "",
      testing_decisions: "",
      user_stories: "",
      out_of_scope: "",
      change_reason: null,
      created_at: 0,
      ...revision,
    },
    tasks,
    dependencies: [],
    workspace_context: null,
  };
}

/**
 * The live session `terminal_specs` reports alongside the mapped Specs. Annotated with the BRIDGE
 * contract type like the three builders above — an inferred object literal would keep `tsc` green
 * when {@link TerminalSpecsSession} gains a field, and every consuming suite would serve an
 * incomplete `session` in silence, which is the exact drift this shared module exists to stop.
 */
export const ACTIVE_SESSION: TerminalSpecsSession = {
  id: "sess1",
  agent_kind: "claude_code",
  state: "active",
};
