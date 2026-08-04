/**
 * The wire SHAPES of the v2.7 DESIGN-SYSTEM read models the native design-system UI consumes — the
 * `design` row and the `design_components` catalogue served by the sidecar `db_command` dispatch
 * over the `nyx_core::db` authority (Phase 1). These mirror the Rust structs field-for-field
 * (snake_case, as they cross the JSON boundary), the same convention as the Spec read models.
 */

/** A `design` row (the wire shape of `nyx_core::db::Design`, snake_case). One per project. */
export interface DesignRow {
  readonly id: string;
  readonly project_id: string;
  /** The DESIGN.md markdown (couleurs / typo / spacing / guidelines). */
  readonly content: string;
  /** The derived Tailwind theme tokens (JSON string), `null` until derived. Feeds `@theme`. */
  readonly theme_tokens_json: string | null;
  readonly created_at: number;
  readonly updated_at: number;
}

/** A `design_components` row (the wire shape of `nyx_core::db::DesignComponent`, snake_case). */
export interface DesignComponentRow {
  readonly id: string;
  readonly project_id: string;
  readonly name: string;
  /** The source repo the component was inventoried from (`null` = greenfield / `[NEW]`). */
  readonly source_repository_id: string | null;
  /** The source path relative to the repo (`null` = greenfield / `[NEW]`). */
  readonly source_path: string | null;
  /**
   * The md5 of the component BUNDLE's source files AS OF ITS LAST SYNTHESIS (review #40), `null`
   * until an agent synthesizes it — the incremental key (an unchanged bundle is never re-synthesized).
   */
  readonly visual_hash: string | null;
  /**
   * The logic-less Handlebars template compiled in the sidecar. EMPTY until an agent SYNTHESIZES the
   * component's bundle (review #40: nyx discovers the bundles, the agent writes the template).
   */
  readonly template: string;
  /** The component's editable params (JSON string), `null` when unset. */
  readonly params_json: string | null;
  /** The component's declared states (JSON string — a state is a param), `null` when unset. */
  readonly states_json: string | null;
  readonly created_at: number;
  readonly updated_at: number;
  /**
   * The BUNDLE's md5 as the LAST INVENTORY observed it on disk (v35), `null` when that scan did not
   * see the source (it vanished, its repo was not walked, or the row is a greenfield `[NEW]`).
   * Compared with `visual_hash` — the state at the last SYNTHESIS — it says whether the component
   * awaits a re-synthesis, without touching the disk.
   */
  readonly scanned_input_hash: string | null;
  /**
   * Epoch ms at which an inventory IN A POSITION TO JUDGE found this component's source GONE while
   * it carried a synthesis; `null` when it is not orphaned. Such a row is never destroyed
   * automatically — an LLM synthesis is reviewed work, and a scan can silently fail to read a file,
   * so an absence it observes is not a proof. It is FLAGGED here instead, and removing it is an
   * explicit gesture (the card's delete).
   */
  readonly orphaned_at: number | null;
}

/**
 * The NATURE of a catalogue component, DERIVED from its provenance (PDR-0010's `[DS]` / `[NEW]`):
 * a component inventoried from a real front (it has a `source_path`) is `[DS]` existing; a
 * greenfield one the agent proposed (no source) is `[NEW]` — a candidate to promote.
 */
export type ComponentNature = "ds" | "new";

/** Classify a component as `[DS]` (has a source) or `[NEW]` (greenfield / proposed). */
export function componentNature(c: DesignComponentRow): ComponentNature {
  return c.source_path ? "ds" : "new";
}

/**
 * Is this component an ORPHAN — a `[DS]` row whose source file the last inventory found GONE, while
 * it carried a synthesis? The inventory keeps such a row rather than destroying it (review
 * 01KYHPFDE21W0EMVX4J87RHGCV: a partially failed scan used to delete synthesized components and
 * their templates outright), so the catalogue must SHOW it — otherwise it is a stale entry nobody
 * can tell apart from a live one.
 */
export function isOrphan(c: DesignComponentRow): boolean {
  return c.orphaned_at != null;
}
