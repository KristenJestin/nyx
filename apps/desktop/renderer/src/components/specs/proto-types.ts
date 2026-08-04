/**
 * The wire SHAPES of the v2.7 PROTOTYPE read-model the Spec-detail proto surface consumes — the
 * `proto_tree` read model served by the sidecar `db_command` dispatch (review #24, the data layer
 * T25-T29 render). These mirror the `nyx_core::db` `ProtoTree` structs field-for-field (snake_case,
 * as they cross the JSON boundary), the same convention as the Spec / design read models. T25 only
 * renders the CURRENT/elected composition per page; the rounds / election / feedback fields are
 * carried faithfully for the later views (T26-T29) but not driven here.
 */

/** A `prototypes` row (the wire shape of `nyx_core::db::Prototype`, snake_case). One per Spec. */
export interface Prototype {
  readonly id: string;
  readonly spec_id: string;
  readonly description: string | null;
  readonly created_at: number;
  readonly updated_at: number;
  /** Soft-removed (archived) prototype, `null` = live. */
  readonly archived_at: number | null;
}

/** A `prototype_pages` row — a page of the prototype (the nav unit, keyed by `slug`). */
export interface PrototypePage {
  readonly id: string;
  readonly prototype_id: string;
  readonly slug: string;
  readonly title: string;
  readonly position: number;
  readonly created_at: number;
}

/** A `prototype_page_versions` row — a FROZEN iteration of a page (`archived_at` set = superseded). */
export interface PrototypePageVersion {
  readonly id: string;
  readonly page_id: string;
  readonly label: string;
  readonly summary: string | null;
  readonly created_at: number;
  readonly archived_at: number | null;
}

/** A `prototype_variants` row — one candidate composition of a page version. */
export interface PrototypeVariant {
  readonly id: string;
  readonly page_version_id: string;
  readonly label: string;
  readonly title: string;
  /** The serialized component tree (NOT an HTML blob, PDR-0010) — fed to `design_render`. */
  readonly composition_json: string;
  /** The declared states (JSON array of names — the `window.__nyxStates` port), `null` when none. */
  readonly states_json: string | null;
  /** The AGENT's within-version recommendation (`is_main`, PDR-0009). */
  readonly is_main: boolean;
  readonly position: number;
  readonly created_at: number;
}

/** A `prototype_rounds` row — a round of the rollback history (T26). */
export interface PrototypeRound {
  readonly id: string;
  readonly prototype_id: string;
  readonly label: string;
  readonly summary: string | null;
  readonly position: number;
  /** When set, the round was PRESENTED (frozen); `null` = still in construction. */
  readonly presented_at: number | null;
  readonly created_at: number;
}

/** A `prototype_round_pages` row — the manifest entry pinning a page's version + the user's RETENU. */
export interface PrototypeRoundPage {
  readonly id: string;
  readonly round_id: string;
  readonly page_id: string;
  readonly page_version_id: string;
  readonly position: number;
  /** The USER's RETENU for this page in this round (≠ the agent's `is_main`), `null` = not chosen. */
  readonly chosen_variant_id: string | null;
  readonly decision_rationale: string | null;
  readonly decided_by: string | null;
  readonly decided_at: number | null;
  readonly created_at: number;
}

/**
 * ONE page of a round's MANIFEST, resolved (`proto_round_manifest`, wire shape of
 * `nyx_core::db::RoundManifestPage`) — what a round pinned for that page, and what was retained in
 * it. The pair {@link ProtoTree} resolves for the CURRENT round, available for ANY round: that is
 * what makes a PAST round consultable (finding `01KZ3AP34K1PKS6W0E75RP7DF6`), since the tree
 * already carries every version and variant a page ever had, archived ones included.
 */
export interface RoundManifestPage {
  /** The manifest entry: the page VERSION that round froze + the USER's RETENU on it. */
  readonly entry: PrototypeRoundPage;
  /** The RETENU in THAT round: the explicit choice, else the mono-variant "retained by default". */
  readonly effective_variant_id: string | null;
}

/** `prototype_feedback.status` — the comment is OPEN (not yet addressed). */
export const FEEDBACK_STATUS_OPEN = "open";
/** `prototype_feedback.status` — the comment was RESOLVED. */
export const FEEDBACK_STATUS_RESOLVED = "resolved";
/** `prototype_feedback.anchor_status` — the anchored node still exists → the comment is CARRIED
 * across rounds ("porté"). */
export const ANCHOR_STATUS_ANCHORED = "anchored";
/** `prototype_feedback.anchor_status` — the anchored node vanished → the comment is "à ré-ancrer"
 * (orphaned; surfaced for re-anchoring, code-review style). */
export const ANCHOR_STATUS_ORPHANED = "orphaned";
/** `prototype_feedback.author_kind` — a human USER authored the comment (the T28 default). */
export const FEEDBACK_AUTHOR_USER = "user";

/** A `prototype_feedback` row — an anchored pre-dev review comment (T28). */
export interface PrototypeFeedback {
  readonly id: string;
  readonly prototype_id: string;
  readonly origin_variant_id: string | null;
  readonly node_key: string | null;
  readonly anchor_status: string;
  readonly body: string;
  readonly status: string;
  readonly resolution_note: string | null;
  readonly resolution_via_variant_id: string | null;
  readonly resolved_at: number | null;
  readonly ignored_reason: string | null;
  readonly ignored_at: number | null;
  readonly author_kind: string;
  readonly author_ref: string | null;
  readonly created_at: number;
  readonly updated_at: number;
}

/** The `prototype_design_lock` row — the design FREEZE marker (élection figée). */
export interface PrototypeDesignLock {
  readonly prototype_id: string;
  readonly locked_round_id: string;
  readonly decided_by: string | null;
  readonly locked_at: number;
}

/** A page-VERSION with its variants (`is_main` marks the agent's reco). */
export interface ProtoTreeVersion {
  readonly version: PrototypePageVersion;
  readonly variants: readonly PrototypeVariant[];
}

/** A PAGE with its versions/variants + the current round's election state. */
export interface ProtoTreePage {
  readonly page: PrototypePage;
  /** The page's versions (newest-first; includes archived — the rollback history), each with variants. */
  readonly versions: readonly ProtoTreeVersion[];
  /** The current round's manifest entry (`chosen_variant_id` = the user's RETENU), `null` when unpinned. */
  readonly manifest_entry: PrototypeRoundPage | null;
  /** The EFFECTIVE retained variant for this page in the current round (RETENU, else "retenue d'office"). */
  readonly effective_variant_id: string | null;
}

/** The full PROTOTYPE tree of a Spec — the wire shape of `nyx_core::db::ProtoTree`. */
export interface ProtoTree {
  readonly prototype: Prototype;
  /** The prototype's rounds, oldest-first (the rollback history — T26). */
  readonly rounds: readonly PrototypeRound[];
  /** The CURRENT (mutable) round — the one election/comment writes target. */
  readonly current_round: PrototypeRound | null;
  /** The design FREEZE marker, `null` while not locked (élection figée). */
  readonly design_lock: PrototypeDesignLock | null;
  /** The prototype's pages (position-ordered), each with its versions/variants + election state. */
  readonly pages: readonly ProtoTreePage[];
  /** The OPEN pre-dev review comments (T28). */
  readonly open_feedback: readonly PrototypeFeedback[];
  /** The RESOLVED pre-dev review comments. */
  readonly resolved_feedback: readonly PrototypeFeedback[];
}

// --- The ELECTED COMPOSITION (the coder SEAM, T29) — the wire shape of `db::elected_composition` ---
// This mirrors the FROZEN coder-seam read model `proto_composition` serves (distinct from the LIVE
// `proto_tree` above): read off the design LOCK, per page pinned in the frozen round, the arbre nommé
// (placement) with each node classified by its NATURE, plus the [NEW] candidates + the pre-dev review.

/** A node's coder-contract NATURE — `[DS]`, an existing catalogue component (carries a `component_ref`). */
export const NODE_NATURE_DS = "ds";
/** A node's coder-contract NATURE — `[NEW]`, a proposed component not in the catalogue (a build candidate). */
export const NODE_NATURE_NEW = "new";
/** A node's coder-contract NATURE — `[inline]`, bespoke one-off markup (not a component). */
export const NODE_NATURE_INLINE = "inline";

/** A REFERENCE to the project's design-system — pointers only (the harness resolves the content). */
export interface DesignSystemRef {
  readonly project_id: string;
  /** The `design` row id, `null` when the project has no design-system yet (greenfield). */
  readonly design_id: string | null;
}

/** ONE node of an elected composition, flattened + classified by its NATURE (wire shape of `ElectedNode`). */
export interface ElectedNode {
  /** The `/`-joined node path (the `Root/Header/…` anchor convention) — its depth is the placement. */
  readonly node_key: string;
  /** `ds` | `new` | `inline` — the 3 PDR natures ({@link NODE_NATURE_DS}/`NEW`/`INLINE`). */
  readonly nature: string;
  /** The component NAME this node instantiates (`ds`/`new`); `null` for an `inline` node. */
  readonly component: string | null;
  /** The `design_components` id for a `[DS]` node (a design-system REFERENCE — the detail link target);
   * `null` for a `[NEW]` (not in the catalogue yet) or an `[inline]` node. */
  readonly component_ref: string | null;
  /** The node's params (the state is a param), carried so the harness sees the parameterization. */
  readonly params: Record<string, unknown>;
  /**
   * The SCOPE ID of the CSS exception this node's component carries — ABSENT for nearly every node
   * (the seam omits it rather than sending `null`).
   *
   * A POINTER into the page's exceptions, never a copy (finding `01KYVMTWB71R0D0CT1SV88J8DX`): a
   * `Spinner` placed thirty times has ONE exception, and duplicating it at each placement is how a
   * shared thing drifts. Its PRESENCE is what a display badges on the node — a placement is where
   * the exception LANDS, never what owns it.
   */
  readonly css_scope?: string | null;
}

/** The elected design for ONE page of the frozen round (wire shape of `ElectedPage`). */
export interface ElectedPage {
  readonly page_id: string;
  readonly slug: string;
  readonly title: string;
  readonly page_version_id: string;
  /** The EFFECTIVE retained variant, `null` when a multi-variant page still owes an arbitration (a gap). */
  readonly elected_variant_id: string | null;
  /** The elected variant's `composition_json` (the arbre nommé = placement); `null` when none retained. */
  readonly composition_json: string | null;
  /** The elected composition's nodes, flattened + classified by nature (the 3 natures explicit). */
  readonly nodes: readonly ElectedNode[];
  /** The DISTINCT `[NEW]` component names — the candidates the coder must BUILD (the derived-task anchor). */
  readonly new_components: readonly string[];
}

/** A RESOLVED pre-dev comment reduced to its ANCHOR — what the coder SEAM exposes of the review
 * (wire shape of `ResolvedFeedbackAnchor`). It says THAT a node was corrected before the election and
 * WHERE; it carries no `body`, no resolution note and no author. */
export interface ResolvedFeedbackAnchor {
  /** The comment's id — a POINTER (the full comment is readable on the live authoring surface only). */
  readonly id: string;
  /** THE ANCHOR: the tree-node key the comment was raised on; `null` for a global comment. */
  readonly node_key: string | null;
  /** {@link ANCHOR_STATUS_ANCHORED} | {@link ANCHOR_STATUS_ORPHANED} — the anchor's health. */
  readonly anchor_status: string;
  /** WHERE it was raised (a variant id), `null` when it was global. */
  readonly origin_variant_id: string | null;
  /** The variant the correction landed in, when the resolution named one. */
  readonly resolution_via_variant_id: string | null;
  /** WHEN it was resolved (epoch ms). */
  readonly resolved_at: number | null;
}

/** The FROZEN elected composition of a prototype — the coder SEAM (wire shape of `ElectedComposition`).
 * `proto_composition` returns `null` (not this) until the design is LOCKED (the "pas encore figée" cue). */
export interface ElectedComposition {
  readonly prototype_id: string;
  readonly spec_id: string;
  /** The FREEZE marker — proof this is a FROZEN election, and WHICH round is frozen. */
  readonly locked: PrototypeDesignLock;
  /** The design-system as a SEAM (reference, not injected content). */
  readonly design_ref: DesignSystemRef;
  /** One entry per page pinned in the frozen round, in manifest order. */
  readonly pages: readonly ElectedPage[];
  /** The RESOLVED pre-dev comments as ANCHORS ONLY — WHICH nodes were corrected before the election,
   * never WHAT was said (PDR-0009: the pre-dev review is NEVER transmitted to the coder, and this is
   * the seam a coder harness consumes — so the guarantee is structural). The bodies live on the LIVE
   * authoring surface ({@link ProtoTree}), which no coder reads. */
  readonly resolved_feedback: readonly ResolvedFeedbackAnchor[];
  /** How many pre-dev comments were RESOLVED (the review record as a count). */
  readonly resolved_feedback_count: number;
  /** How many pre-dev comments are STILL OPEN (a gap signal: the review is not fully resolved). */
  readonly open_feedback_count: number;
}
