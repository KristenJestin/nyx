/**
 * The wire SHAPES of the v2.6 KNOWLEDGE (Idées) read models the native Idées view consumes —
 * the `idea_themes` (group headers + facet), `idea_list` (backlog rows, enriched with linked
 * Specs) and `idea_search` (FTS5 + fuzzy scored) db-commands served by the sidecar's `db_command`
 * dispatch over the `nyx_core::db` / `nyx_core::idea` model (v2.6 phases 1-3). These mirror the
 * Rust structs field-for-field (snake_case, as they cross the JSON boundary), the same convention
 * as the Specs read models (`components/specs/types.ts`).
 *
 * The reads are wired in Phase 4 (this task): the Idées view "consomme le modèle idées + le
 * regroupement par thème + la recherche"; the read path did not exist yet. The MCP idea tools
 * (add/close/merge/link…) are the WRITE authority and reuse the SAME `nyx_core::db` accessors
 * (one authority for UI + MCP, PDR-0005).
 */

/**
 * Canonical `ideas.status` (`nyx_core::db::IDEA_STATUS_*`). The status filter keys on it.
 * `open`/`planned` are LIVE; `implemented` (Spec-driven / manual), `closed` (manual fermeture par
 * constat) and `dropped` are TERMINAL.
 */
export type IdeaStatus = "open" | "planned" | "implemented" | "closed" | "dropped";

/**
 * A Spec LINKED to an idea (`nyx_core::db::LinkedSpec`): the link `kind` (`addresses` = the Spec
 * traite l'idée | `related` = contexte seul) + the Spec's human-facing `title`/`status`/`number`.
 * Rendered as the idea's "Specs liées" (row badges + the detail drawer).
 */
export interface LinkedSpec {
  readonly spec_id: string;
  /** `addresses` | `related`. */
  readonly kind: string;
  readonly title: string;
  readonly status: string;
  readonly number: number;
}

/** An `ideas` row (the wire shape of `nyx_core::db::Idea`, snake_case). */
export interface Idea {
  readonly id: string;
  readonly project_id: string;
  readonly title: string;
  readonly body: string;
  /** `open` | `planned` | `implemented` | `dropped`. */
  readonly status: string;
  /** The by-theme grouping axis (`null` = ungrouped). */
  readonly theme: string | null;
  /** `session` | `spec` | `manual` — how the idea was captured. */
  readonly source: string;
  readonly created_at: number;
  readonly updated_at: number;
  /** Epoch ms the idea reached a terminal state (`null` while live). */
  readonly closed_at: number | null;
  /** The fermeture motif (`null` while live). */
  readonly closed_reason: string | null;
}

/**
 * The `idea_list` entry: an `Idea` ENRICHED with its linked Specs — the dense-table ROW (its Spec
 * badges) AND the detail drawer source (body + linked Specs + closure reason from one item).
 */
export interface IdeaListItem extends Idea {
  readonly linked_specs: readonly LinkedSpec[];
}

/**
 * The `idea_themes` entry (`nyx_core::db::IdeaThemeGroup`): one THEME group of the backlog — the
 * `theme` (`null` = ungrouped), the `total`, and the per-STATUS breakdown. Backs a group header
 * (counter + status mini-dots) and the theme facet.
 */
export interface IdeaThemeGroup {
  readonly theme: string | null;
  readonly total: number;
  readonly open: number;
  readonly planned: number;
  readonly implemented: number;
  readonly closed: number;
  readonly dropped: number;
}

/**
 * The `idea_search` hit (`nyx_core::idea::IdeaSearchResult`): the idea's identity + a match
 * `extract` + the blended `score` (FTS5 + fuzzy), plus its linked Specs. Drives the search box.
 */
export interface IdeaSearchResult {
  readonly id: string;
  readonly title: string;
  readonly extract: string;
  readonly score: number;
  readonly status: string;
  readonly theme: string | null;
  readonly linked_specs: readonly LinkedSpec[];
}

/** The label used for the ungrouped (`theme = null`) bucket in the UI. */
export const UNGROUPED_THEME = "Ungrouped";

/** A theme's display key: the theme name, or the ungrouped sentinel for `null`. */
export function themeKey(theme: string | null): string {
  return theme ?? UNGROUPED_THEME;
}
