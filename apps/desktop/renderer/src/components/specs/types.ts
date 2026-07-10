/**
 * The wire SHAPES of the Spec-domain read models the native Specs UI consumes — the
 * `spec_list` (board) and `spec_detail` (PRD detail) db-commands served by the sidecar's
 * `db_command` dispatch over the `nyx_core::db` Spec/revision/task/workspace-context model
 * (v2.4 phases 1-3). These mirror the Rust structs field-for-field (snake_case, as they
 * cross the JSON boundary), the same convention as `WorkspaceRuntimeSnapshot`.
 *
 * The read wiring (adding `spec_list`/`spec_detail` to the sidecar dispatch + the contract)
 * is IN Phase-4 scope: the tabs "consomment le modèle Spec/tasks + le lien workspace", and
 * the read path did not exist yet. The Phase-5 MCP authoring/transition tools are OUT of
 * scope and reuse the SAME `nyx_core::db` accessors (one authority for UI + MCP, PDR-0005).
 */

/** Canonical `spec_status` (`nyx_core::db::SPEC_STATUS_*`). The board columns key on it. */
export type SpecStatus =
  | "draft"
  | "ready"
  | "in_progress"
  | "blocked"
  | "in_review"
  | "completed"
  | "cancelled"
  | "archived";

/** Canonical `priority` (`nyx_core::db`). Shared by Specs and tasks. */
export type SpecPriority = "urgent" | "high" | "med" | "low";

/** Canonical `task_status` (`nyx_core::db::TASK_STATUS_*`). */
export type TaskStatus =
  | "todo"
  | "ready"
  | "in_progress"
  | "blocked"
  | "in_review"
  | "done"
  | "cancelled";

/** A `specs` row (the wire shape of `nyx_core::db::Spec`, snake_case). */
export interface Spec {
  readonly id: string;
  readonly project_id: string;
  readonly workspace_id: string | null;
  readonly title: string;
  readonly slug: string | null;
  readonly number: number;
  readonly priority: string;
  readonly status: string;
  readonly mode: string;
  /** Ordered phase plan, a JSON string `[{number,name}]` (PDR-0022); `"[]"` = flat. */
  readonly phases: string;
  readonly current_revision_number: number;
  readonly position: number;
  readonly created_at: number;
  readonly updated_at: number;
  readonly completed_at: number | null;
  readonly archived_at: number | null;
}

/** The `spec_list` entry: a Spec + its board-card derivations (`spec_list` db-command). */
export interface SpecListEntry extends Spec {
  /** The project's Linear prefix — the card's human key is `project_key-number`. */
  readonly project_key: string;
  /** Tasks reaching `done` (the progress bar's numerator). */
  readonly tasks_done: number;
  /** Total tasks of the Spec (the denominator). */
  readonly tasks_total: number;
}

/** A `spec_revisions` row (the wire shape of `nyx_core::db::SpecRevision`). */
export interface SpecRevision {
  readonly id: string;
  readonly spec_id: string;
  readonly revision_number: number;
  readonly revision_status: string;
  readonly title: string;
  readonly problem: string;
  readonly solution: string;
  readonly implementation_decisions: string;
  readonly testing_decisions: string;
  readonly user_stories: string;
  readonly out_of_scope: string;
  readonly change_reason: string | null;
  readonly created_at: number;
}

/** A `tasks` row (the wire shape of `nyx_core::db::Task`). `type_` maps to the wire `type_`. */
export interface Task {
  readonly id: string;
  readonly project_id: string;
  readonly spec_id: string | null;
  readonly parent_task_id: string | null;
  readonly kind: string;
  readonly executor: string;
  readonly type_: string;
  readonly status: string;
  readonly priority: string;
  readonly severity: string | null;
  readonly title: string;
  readonly description: string | null;
  readonly acceptance_criteria: string | null;
  readonly verification_command: string | null;
  readonly phase: number | null;
  readonly position: number;
  readonly blocked_reason: string | null;
  readonly skip_reason: string | null;
  readonly created_at: number;
  readonly updated_at: number;
  readonly completed_at: number | null;
}

/** A `task_dependencies` edge (the wire shape of `nyx_core::db::TaskDependency`). */
export interface TaskDependency {
  readonly task_id: string;
  readonly depends_on_task_id: string;
  readonly type_: string;
  readonly created_at: number;
}

/** A `workspaces` row (the wire shape of `nyx_core::db::Workspace`; `root_path` → `path`). */
export interface WorkspaceRef {
  readonly id: string;
  readonly project_id: string;
  readonly name: string;
  readonly path: string;
  readonly branch: string | null;
  readonly is_root: boolean;
  readonly folder_removed_at: number | null;
}

/** A `projects` row subset (the wire shape of `nyx_core::db::Project`). */
export interface ProjectRef {
  readonly id: string;
  readonly name: string;
  readonly key: string;
}

/** A `repositories` row (the wire shape of `nyx_core::db::Repository`). */
export interface RepositoryRef {
  readonly id: string;
  readonly project_id: string;
  readonly name: string;
  readonly relative_path: string;
  readonly optional: boolean;
}

/**
 * The DERIVED workspace context of a Spec (`nyx_core::db::SpecWorkspaceContext`): the dev
 * workspace the Spec is anchored on, plus the PROJECT and REPOS derived THROUGH it (never
 * stored on the Spec, PDR-0013). `null` on a Spec with no anchor yet.
 */
export interface SpecWorkspaceContext {
  readonly spec_id: string;
  readonly workspace: WorkspaceRef;
  readonly project: ProjectRef;
  readonly repos: readonly RepositoryRef[];
}

/** The `spec_detail` read model (the `spec_detail` db-command). */
export interface SpecDetail {
  readonly spec: Spec;
  readonly revision: SpecRevision;
  readonly tasks: readonly Task[];
  readonly dependencies: readonly TaskDependency[];
  readonly workspace_context: SpecWorkspaceContext | null;
}

/** A phase entry parsed from `Spec.phases` (the JSON `[{number,name}]`, PDR-0022). */
export interface SpecPhase {
  readonly number: number;
  readonly name: string;
}

/** Parse `Spec.phases` (a JSON string) to a phase list; `[]` on empty/malformed. */
export function parsePhases(phasesJson: string): SpecPhase[] {
  try {
    const parsed = JSON.parse(phasesJson);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (p): p is SpecPhase =>
          typeof p === "object" &&
          p !== null &&
          typeof (p as SpecPhase).number === "number" &&
          typeof (p as SpecPhase).name === "string",
      )
      .map((p) => ({ number: p.number, name: p.name }));
  } catch {
    return [];
  }
}
