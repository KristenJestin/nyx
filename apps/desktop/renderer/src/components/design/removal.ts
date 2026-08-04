/**
 * What `design_component_delete` REALLY answers — the wire shape of
 * `nyx_core::design::ComponentRemoval` (finding `01KYSHSPGSPC98C49X68MWY60D`, backend half
 * `01KYSCX4PNKTMX17YVG9XD8KBZ`).
 *
 * # Why this exists
 *
 * A component's identity IS its source. Deleting the ROW of a component whose file still matches the
 * project's bundle recipe is UNDONE by the next `design_inventory`, which re-discovers the very same
 * file as a `pending` component under a NEW id. The gesture used to answer a bare `{ deleted: true }`
 * and the `/design` card reported nothing at all — so the UI PROMISED a removal that did not last.
 *
 * The command now returns the removal's real EFFECT (`ephemeral` + the discovery's `verdict` on the
 * source path + which repo / which recipe judged it), what it WROTE (`excluded` patterns,
 * `recipe_materialized`) and, above all, `warnings[]`: the backend's own sentences, which name the
 * problem AND the correction. The renderer NEVER re-writes those sentences — only the backend knows
 * the verdict and the recipe, and a paraphrase drifts from the truth the moment either changes. This
 * module's whole job is to get that text out of the JSON safely and to make the verdict CODES
 * readable; the wording stays the backend's.
 *
 * A REFUSAL (a pinned entry, a glob character in the path, an exclusion budget that is full) crosses
 * the bridge as a `command` error instead, and NOTHING is deleted — see `parseRemovalRefusal`.
 */

/**
 * The verdict the discovery holds on a component's source path — `EntryVerdict::label()`:
 * `elected` / `pinned` / `pruned_dir` / `excluded` / `no_rule` / `no_marker` / `missing` /
 * `unreadable`, plus `greenfield` for a row that has NO on-disk source at all. Kept OPEN (a plain
 * string) on purpose: a verdict the backend adds tomorrow must still print, not be narrowed away.
 */
export type RemovalVerdict = string;

/** The wire shape of `nyx_core::design::RemovalEffect` (snake_case, as it crosses JSON). */
export interface RemovalEffect {
  /** Will the next `design_inventory` DISCOVER this source again (as a new pending row)? */
  readonly ephemeral: boolean;
  /** The discovery's verdict on the source path, or `greenfield` for a row with no source. */
  readonly verdict: RemovalVerdict;
  /** The repo the source resolved under (`null` for a greenfield, or a repo that left). */
  readonly repo: string | null;
  /** The row's repo-relative source path (`null` for a greenfield `[NEW]`). */
  readonly source_path: string | null;
  /** Where the recipe that judged the source came from (`custom` / `globs_override` / `stack_default`). */
  readonly recipe_source: string | null;
}

/** The wire shape of `nyx_core::design::ComponentRemoval`. */
export interface ComponentRemoval {
  /** Did the `DELETE` really affect the row? */
  readonly deleted: boolean;
  /** What the removal achieves — computed against the state that will really be scanned. */
  readonly effect: RemovalEffect;
  /** The `exclude` patterns this removal ADDED to the project's recipe. */
  readonly excluded: readonly string[];
  /** The project ran on a default recipe, and it was MATERIALIZED to carry the exclusion. */
  readonly recipe_materialized: boolean;
  /** NOTHING was done, and why (normally surfaced as a bridge error instead). */
  readonly refused: string | null;
  /** THE BACKEND'S OWN SENTENCES: what the human must be told, verbatim. */
  readonly warnings: readonly string[];
}

/** A removal effect for a response that carried none (defensive default: claims nothing). */
const UNKNOWN_EFFECT: RemovalEffect = {
  ephemeral: false,
  verdict: "",
  repo: null,
  source_path: null,
  recipe_source: null,
};

function stringOrNull(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value : null;
}

/** The non-empty strings of an array value, in order (anything else → no entry). */
function stringList(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) =>
    typeof entry === "string" && entry.trim() !== "" ? [entry] : [],
  );
}

/**
 * Read a `design_component_delete` response. TOLERANT by construction: an older sidecar answering
 * the bare `{ deleted: true }`, a `null`, or anything unexpected yields a removal that WARNS about
 * nothing and claims nothing — the surface then says exactly what it knows, which is what the whole
 * finding is about. It never throws: a parse failure must not turn a successful delete into an error.
 */
export function parseComponentRemoval(value: unknown): ComponentRemoval {
  if (value == null || typeof value !== "object" || Array.isArray(value)) {
    return {
      deleted: false,
      effect: UNKNOWN_EFFECT,
      excluded: [],
      recipe_materialized: false,
      refused: null,
      warnings: [],
    };
  }
  const record = value as Record<string, unknown>;
  const rawEffect =
    record.effect != null && typeof record.effect === "object" && !Array.isArray(record.effect)
      ? (record.effect as Record<string, unknown>)
      : {};
  return {
    deleted: record.deleted === true,
    effect: {
      ephemeral: rawEffect.ephemeral === true,
      verdict: typeof rawEffect.verdict === "string" ? rawEffect.verdict : "",
      repo: stringOrNull(rawEffect.repo),
      source_path: stringOrNull(rawEffect.source_path),
      recipe_source: stringOrNull(rawEffect.recipe_source),
    },
    excluded: stringList(record.excluded),
    recipe_materialized: record.recipe_materialized === true,
    refused: stringOrNull(record.refused),
    warnings: stringList(record.warnings),
  };
}

/**
 * Is there anything the human MUST read after this removal? Only then does a notice appear: a
 * removal that is complete and final says nothing, because the row leaving the grid IS the report.
 */
export function removalNeedsTelling(removal: ComponentRemoval): boolean {
  return removal.warnings.length > 0 || removal.refused !== null;
}

/**
 * What the discovery's verdict MEANS, in one clause — the CODE stays visible beside it (it is the
 * backend's own word, and it is what an agent's `design_recipe` answers with), this only makes it
 * readable.
 *
 * AN UNKNOWN CODE RETURNS `null`, and the FALLBACK IS AT THE RENDU: the caller
 * (`catalogue-page.tsx`) already prints the raw code, and only appends `" — <gloss>"` when this
 * returns non-null — so a verdict added tomorrow shows as its bare code rather than as a guess, and
 * never as `undefined`. Echoing the code back from here would print it TWICE
 * (`verdict brand_new — brand_new`). That is what makes this function's contract the OPPOSITE of
 * {@link recipeSourceMeaning}'s, which DOES return the code: its caller prints the gloss ALONE
 * (`· jugé par …`), so there the code is the only thing left to show. The doc-comment used to
 * announce that fallback here too and invite the caller's null-check to be removed
 * (finding `01KYSNT38VZYJEAH4VEFJW8FHW`).
 */
export function verdictMeaning(verdict: RemovalVerdict): string | null {
  switch (verdict) {
    case "elected":
      return "the source is taken by the project's recipe";
    case "pinned":
      return "the source is PINNED in the recipe (a pin names its files and bypasses exclude)";
    case "pruned_dir":
      return "the source is in a directory the scan does not descend into";
    case "excluded":
      return "the source is already excluded by the recipe";
    case "no_rule":
      return "no entry rule of the recipe takes this source";
    case "no_marker":
      return "the source does not carry the content marker the rule requires";
    case "missing":
      return "the source cannot be found (file absent, or repo outside the workspace)";
    case "unreadable":
      return "the source could not be read — the scan decides nothing, so it assumes it comes back";
    case "greenfield":
      return "the component has no source on disk: the removal is definitive";
    default:
      return null;
  }
}

/**
 * Where the recipe that judged the source comes from (`RECIPE_SOURCE_*`). An unknown code IS
 * returned as itself here — unlike {@link verdictMeaning} — because the caller prints this clause
 * ALONE (`· judged by …`) and has nothing else to fall back on.
 */
export function recipeSourceMeaning(recipeSource: string): string {
  switch (recipeSource) {
    case "custom":
      return "the project's own recipe";
    case "globs_override":
      return "the project's globs override";
    case "stack_default":
      return "the detected stack's default recipe";
    default:
      return recipeSource;
  }
}
