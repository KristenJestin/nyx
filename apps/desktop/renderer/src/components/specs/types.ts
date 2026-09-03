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
  /**
   * The Spec's ONE genre-of-work category (PDR-0017), or `null` when it carries none — a
   * legitimate state, not an error. This is the raw FK; the detail read model resolves it into
   * {@link SpecCategory} so a chip can be drawn without a second round-trip.
   */
  readonly category_id: string | null;
  readonly current_revision_number: number;
  readonly position: number;
  readonly created_at: number;
  readonly updated_at: number;
  readonly completed_at: number | null;
  readonly archived_at: number | null;
}

/** The `spec_list` entry: a Spec + its board-card derivations (`spec_list` db-command). */
/**
 * WHOSE Spec it is — the identity `<ProjectMark>` paints, and the renderer half of
 * `nyx_core::home::HomeProject`, the one wire shape every cross-project read model carries (the
 * home's rows and `IdeaListItem` carry the same one, so a project cannot wear two faces).
 */
export interface SpecProject {
  readonly id: string;
  readonly name: string;
  readonly accent: string | null;
  readonly icon: string | null;
  readonly image_path: string | null;
}

export interface SpecListEntry extends Spec {
  /** The project's Linear prefix — the card's human key is `project_key-number`. */
  readonly project_key: string;
  /**
   * The project's IDENTITY, which the card and the table row wear as a mark in the cross-project
   * scope. `null` for a Spec whose project row has vanished (the card stays, unmarked) — and
   * OPTIONAL on the wire so an entry built before this field existed (a fixture, an older payload)
   * simply reads as "no mark".
   */
  readonly project?: SpecProject | null;
  /** Tasks reaching `done` (the progress bar's numerator). */
  readonly tasks_done: number;
  /** Total tasks of the Spec (the denominator). */
  readonly tasks_total: number;
  /** v2.6 — the "blocked by N" badge count (directional prerequisites), surfaced on the card. */
  readonly blocked_by_count: number;
  /** v2.6 — the "unblocks M" badge count (directional dependents), surfaced on the card. */
  readonly unblocks_count: number;
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
  /**
   * Review-FINDING fields (v21, PDR-0016) — a finding is a `tasks kind=review` row carrying
   * these ported columns (the `review_tasks` table is deprecated). `null` on a non-finding task.
   * `from_comment_id` = the comment a finding was promoted from; `grill_state` =
   * `proposed`|`structured`|`validated`.
   */
  readonly from_comment_id: string | null;
  readonly grill_state: string | null;
  /**
   * Rework ROUND (v22, PDR-0022) — the sub-phase axis. `null` = the plan (round 0); `n≥1` = a
   * rework pass rattached to `phase` (rendered 2.1, 4.1… in the rail). PER-PHASE numbering.
   */
  readonly round: number | null;
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

/**
 * One category of a project's "genre of work" vocabulary (the wire shape of
 * `nyx_core::mcp_tools_spec::category_payload`, v2.9 / PDR-0017) — served by the `category_list`,
 * `category_create`, `spec_category_assign`, `category_rename` and `category_delete` db-commands,
 * and embedded RESOLVED in {@link SpecDetail}.
 *
 * `slug` is the STABLE IDENTITY: minted once, never rewritten, and the only thing the agent harness
 * reads (`bug` is what puts a Spec under the red-before-green discipline). It is PRESENTED by the
 * app and editable by nobody. `name` is the human label — free to change, and free to become a
 * homonym of another category's name, which is why the two are separate fields and not one.
 *
 * **NO CURATION STATE.** PDR-0017 as amended on 2026-09-02 retired the proposed/confirmed axis: the
 * agent has full rights over the classification, so a category it mints is valid as it stands and
 * there is nothing for this type to branch on. `created_by_type` remains — the provenance is a
 * fact — and it is what the settings' Categories section reads to know which rows are the four
 * undeletable system ones.
 */
export interface SpecCategory {
  readonly id: string;
  /** The immutable machine identity. Displayed, never edited. */
  readonly slug: string;
  /** The editable human label. */
  readonly name: string;
  /** A `--group-N` design token WITHOUT its `--` prefix, or `null`. Never a literal colour. */
  readonly color_token: string | null;
  /** `system` | `agent` | `human` — who minted the ROW (not who took the decision). */
  readonly created_by_type: string;
  /**
   * Epoch ms the row was minted, VESTIGIAL since the 2026-09-02 amendment: always set, read by
   * nobody. Kept because the backend still carries the raw column (see
   * `nyx_core::db::Category::confirmed_at`, which names the future cleanup).
   */
  readonly confirmed_at: number | null;
  /**
   * Whether the slug is one of nyx's four (`feature` / `bug` / `refactor` / `chore`) — the
   * BACKEND'S verdict (`db::SYSTEM_CATEGORY_SLUGS`, the same list `category::delete` refuses on),
   * not a list of literals repeated here. The settings' Categories section reads it to omit the
   * Delete control on exactly those rows; the refusal itself stays the service's.
   */
  readonly system: boolean;
}

/** The `spec_detail` read model (the `spec_detail` db-command). */
export interface SpecDetail {
  readonly spec: Spec;
  readonly revision: SpecRevision;
  readonly tasks: readonly Task[];
  readonly dependencies: readonly TaskDependency[];
  /**
   * The OWNER project's identity — what the detail's context rail names in clear in the
   * cross-project scope, next to its `<ProjectMark>`.
   *
   * It is NOT {@link SpecWorkspaceContext.project}: that one is derived through the dev workspace,
   * so it is absent on any Spec not yet anchored and carries no accent/icon/image. This field
   * comes off `spec.project_id` on the Rust side, in the one `HomeProject` wire shape every
   * cross-project read model carries. `null` for a Spec whose project row has vanished (the rail
   * then names no project at all) — and OPTIONAL on the wire, so a payload built before the field
   * existed reads as "no project" rather than as a crash.
   */
  readonly project?: SpecProject | null;
  readonly workspace_context: SpecWorkspaceContext | null;
  /**
   * The Spec's category, RESOLVED (id + slug + name + colour token + provenance — and no curation
   * state, see {@link SpecCategory}); `null` when it has none.
   */
  readonly category: SpecCategory | null;
}

/**
 * The `annexes.kind` value an EXTERNAL LINK is stored under (`nyx_core::db::ANNEX_KIND_URL`). A
 * link is not a model of its own: it is an annexe whose CONTENT is the URL string and whose
 * `title` is the label, which is why it survives a fork with the rest of them.
 */
export const ANNEX_KIND_URL = "url";

/**
 * A `annexes` LIST row (the wire shape of `nyx_core::db::AnnexListItem`, v2.6): a Spec attachment
 * (kind/title) + its content-addressed blob's `byte_len` (a size hint) + the dedup `content_hash`,
 * WITHOUT the blob content. Rendered in the Spec-detail "Documents" block (`annexe_list`
 * db-command).
 *
 * For a `kind: "url"` row the blob content IS the URL, which this list deliberately does not ship
 * — the "Links" block reads {@link SpecLink} (the `annexe_link_list` db-command) instead.
 */
export interface AnnexListItem {
  readonly id: string;
  readonly spec_id: string;
  /** `doc` | `artefact` | `url` | `prototype`. */
  readonly kind: string;
  readonly title: string;
  /** The dedup key / pointer into the content-addressed blob store. */
  readonly content_hash: string;
  readonly byte_len: number;
  readonly created_at: number;
}

/**
 * One EXTERNAL LINK of a Spec (the wire shape of the `annexe_link_list` db-command, v50): a `url`
 * annexe RESOLVED — the attachment row plus the URL read out of the content-addressed store.
 *
 * There is no `spec_links` table: this is `annexes` + `annex_blobs` seen through one facet. Which
 * is why a link needs no fork handling — the attachment hangs off the Spec's LOGICAL id, so a
 * forked revision keeps it without a copy.
 */
export interface SpecLink {
  readonly id: string;
  readonly spec_id: string;
  /** The LABEL. Equals {@link url} when the link was stored without one (`annexes.title` is NOT NULL). */
  readonly title: string;
  /** The absolute http(s) URL (the annexe's stored content). */
  readonly url: string;
  readonly created_at: number;
}

/**
 * One edge of a Spec's dependency view (the wire shape of `nyx_core::db::SpecDepEdge`, v2.6): the
 * OTHER Spec (id + `title`/`number`/`status`) + the link `kind` (`blocks`/`after`/`related`) + `note`.
 */
export interface SpecDepEdge {
  readonly spec_id: string;
  readonly kind: string;
  readonly note: string | null;
  readonly title: string;
  readonly number: number;
  readonly status: string;
}

/**
 * A Spec's full dependency VIEW (the wire shape of `nyx_core::db::SpecDependencyView`, v2.6): the
 * directional `blocked_by` / `unblocks` edges (their `_count` are the badge numbers) + the `related`
 * context links. Consumed by the Spec-detail dependency badges (`spec_dependency_view` db-command).
 */
export interface SpecDependencyView {
  readonly blocked_by: readonly SpecDepEdge[];
  readonly unblocks: readonly SpecDepEdge[];
  readonly related: readonly SpecDepEdge[];
  readonly blocked_by_count: number;
  readonly unblocks_count: number;
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
