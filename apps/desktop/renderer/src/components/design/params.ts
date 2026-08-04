/**
 * The canonical PARAM model shared by the catalogue (task #10) and the composant detail (task #11)
 * of the v2.7 design-system UI (PDR-0010). A `design_components` row carries its editable params as
 * `params_json` and its VARIANCE AXES as `states_json`.
 *
 * **THE RULE (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`), and there is only one:** an AXIS OF VARIANCE
 * goes in the axes ([`./axes`]), never in the params; a SUBSTITUTED VALUE goes in the params, never
 * in the axes. `variant` and `size` are axes; so is a two-valued `loading`. `label` and `count` are
 * params. The model used to expose four overlapping representations of the same notion (param /
 * state / CVA axis / boolean) with no rule saying which was which — this module now owns exactly one
 * half of it:
 *
 *   * `params_json` — an array of PARAM DESCRIPTORS
 *     `[{ name, type, label?, required?, default?, preview?, options? }]`, i.e. VALUES the template
 *     substitutes. A `type` is `text` | `html` (a content param may be plain text OR raw html —
 *     PDR-0010) | `boolean` | `number` | `select`, plus the DEPRECATED `state`. An object map
 *     `{ name: { type, default } }` is also tolerated (normalised to the array form).
 *   * `states_json` — the component's AXES, parsed by [`./axes`]. A param whose name IS an axis is a
 *     DUPLICATE declaration of one notion: it is dropped from the form (the axis control is the real
 *     one) and flagged, here and at the write.
 *
 * Parsing is DEFENSIVE (a malformed blob yields an empty/again-sane result, never a throw) so a
 * bad component never breaks the catalogue or the detail.
 */

import {
  axisDefaults,
  parseAxes,
  primaryAxisStates,
  templateConditions,
  templateSubstitutes,
  type VarianceAxis,
} from "./axes";

/** The type of an editable param. `html` renders raw (`{{{p}}}`), `text` escaped (`{{p}}`). */
export type ParamType = "text" | "html" | "state" | "boolean" | "number" | "select";

/**
 * The WHOLE vocabulary of a descriptor's `type` — six words, and nothing else (finding
 * `01KYPSG0HPG2D28CMJEXRKNEVK`). It is the same list the Prototype manual now spells out and the
 * write path (`design_refresh_component` / `proto_author_component`) REFUSES a synthesis outside of.
 * Kept here as data so the UI and the manual can never drift into two different vocabularies.
 */
export const PARAM_TYPES: readonly ParamType[] = [
  "text",
  "html",
  "state",
  "boolean",
  "number",
  "select",
];

/**
 * The DEPRECATED type (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`): `state` used to mean "THE component's
 * active state", a variance AXIS smuggled into the params — the exact ambiguity that made a variant
 * declarable twice. It is still PARSED (rows carry it) and still REFUSED-FREE at the write, but it
 * is warned about everywhere it appears: an axis belongs in `states_json`.
 */
export const DEPRECATED_PARAM_TYPE: ParamType = "state";

/**
 * One editable param of a component (the descriptor the detail form renders a control for).
 *
 * THREE FIELDS, THREE DIFFERENT QUESTIONS (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`). The model used to
 * have only `default`, and the Prototype manual asked for "a realistic default per param" — a
 * CONFUSION of two things a component does not say at once:
 *
 *   * `required` — MUST the caller provide this param? A `button`'s label is mandatory: the
 *     component has NO label of its own. Writing `default: "Enregistrer"` to make the vignette look
 *     right LIES about the component's contract, and that lie is read by whoever composes with it.
 *   * `default` — the value the component itself falls back to WHEN THE SOURCE SAYS SO (an `@Input`
 *     with an initialiser, a React prop default, a `defaultValue` in the variants file). It is a
 *     FACT about the component, never an illustration.
 *   * `preview` — an EXAMPLE value, used ONLY to render a preview (the catalogue vignette and the
 *     detail's representative instance). It is neither the component's default nor a production
 *     value, and it is never persisted onto a composition node.
 *
 * Without the split there were only two outcomes, both bad: either the catalogue lies, or a required
 * param renders as the empty string and the tile is blank (a coloured pill with no label).
 */
export interface ParamDescriptor {
  readonly name: string;
  readonly type: ParamType;
  readonly label?: string;
  /** The caller MUST provide it — the component carries no value of its own. Absent → `false`. */
  readonly required?: boolean;
  /** The component's OWN fallback, only when the source declares one. */
  readonly default?: unknown;
  /** An example value used ONLY to render an APERÇU. Never a default, never persisted. */
  readonly preview?: unknown;
  /** Allowed values for `select` / `state`. */
  readonly options?: readonly string[];
  /**
   * The `type` the SYNTHESIS actually wrote, when it is NOT one of the six (finding
   * `01KYPSG0HPG2D28CMJEXRKNEVK`) — `"enum"`, `"string"`, `"color"`… The descriptor is still coerced
   * to `text` (a stored row must keep rendering: the parse is defensive by contract), but the raw
   * word is KEPT so the UI can SAY it. It used to be dropped on the floor: `options` went with it
   * and a constrained choice came out as a free-text field, with nothing anywhere saying so.
   *
   * A new write can no longer produce one — the tools refuse it — so this only ever describes a row
   * written before that guard. Absent = the type was legal (or omitted, which means `text`).
   */
  readonly coercedFrom?: string;
  /**
   * This param has the SAME NAME as a declared AXIS (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`) — one
   * notion declared twice, and the param copy is the decorative one (a param is substituted as a
   * VALUE; an axis value is a whole SET of classes). The descriptor is kept so the UI can NAME the
   * duplicate, but it is not rendered as a control: [`editableParams`] drops it, and the axis's own
   * control is the real one.
   */
  readonly shadowsAxis?: boolean;
}

/** The special param name that carries the component's active state (PDR-0010: "l'état = param"). */
export const STATE_PARAM = "state";

function asRecord(value: unknown): Record<string, unknown> | null {
  return value != null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/**
 * The PRIMARY axis's value names — what the legacy `states` chain still consumes (the chip-bar,
 * `window.__nyxStates`, `data-nyx-state-case`). It is a thin read over [`parseAxes`], so a
 * component stored in the FLAT format (`["default","destructive"]` — 173 of them) yields exactly
 * what it used to, and one stored as named axes yields its FIRST axis.
 */
export function parseStates(statesJson: string | null | undefined): string[] {
  return primaryAxisStates(parseAxes(statesJson));
}

/** Coerce one raw descriptor entry into a normalized [`ParamDescriptor`], or `null` if unusable. */
function coerceDescriptor(name: string, raw: unknown): ParamDescriptor | null {
  if (!name) return null;
  const rec = asRecord(raw);
  const rawType = rec?.type;
  const known = typeof rawType === "string" && (PARAM_TYPES as readonly string[]).includes(rawType);
  const type: ParamType = known ? (rawType as ParamType) : "text";
  // A type outside the six is still COERCED to `text` — a stored row must keep rendering — but the
  // word is kept (finding `01KYPSG0HPG2D28CMJEXRKNEVK`), so the form can say "declared `enum`,
  // rendered as text" instead of passing a constrained choice off as a free field. An OMITTED type
  // is not a coercion: it has always meant `text`.
  const coercedFrom =
    known || rawType === undefined || rawType === null
      ? undefined
      : typeof rawType === "string"
        ? rawType
        : JSON.stringify(rawType);
  const options = Array.isArray(rec?.options)
    ? (rec.options as unknown[]).filter((o): o is string => typeof o === "string")
    : undefined;
  return {
    name,
    type,
    label: typeof rec?.label === "string" ? rec.label : undefined,
    // RETRO-COMPATIBLE: a descriptor written before the split carries neither field — it is
    // OPTIONAL (`required` absent → false) and has no example (`preview` absent → the `default`
    // resolves the aperçu exactly as it used to).
    required: rec?.required === true,
    default: rec?.default,
    preview: rec?.preview,
    options,
    coercedFrom,
  };
}

/**
 * Parse `params_json` into normalized descriptors. Accepts either the array form `[{ name, ... }]`
 * or the object map `{ name: { ... } }`.
 *
 * IT NO LONGER SYNTHESIZES A `state` PARAM (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`). It used to append
 * one whenever the component declared states — the single sélecteur d'état, and the very confusion
 * this model removes: a variance axis is NOT a param. The AXES are their own model ([`./axes`]) with
 * ONE CONTROL PER AXIS, and their active values live BESIDE the params in the same value map (keyed
 * by axis name), so the substitution chain (`{{state}}`, `{{variant}}`) and the per-node persistence
 * are exactly as before.
 *
 * A descriptor whose name IS an axis is kept but FLAGGED [`ParamDescriptor.shadowsAxis`]: one notion
 * declared twice, which the UI names and the write path warns about.
 */
export function parseParams(
  paramsJson: string | null | undefined,
  axes: readonly VarianceAxis[] = [],
): ParamDescriptor[] {
  const out: ParamDescriptor[] = [];
  if (paramsJson && paramsJson.trim()) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(paramsJson);
    } catch {
      parsed = null;
    }
    if (Array.isArray(parsed)) {
      for (const entry of parsed) {
        const rec = asRecord(entry);
        const name = typeof rec?.name === "string" ? rec.name : null;
        if (name) {
          const d = coerceDescriptor(name, entry);
          if (d) out.push(d);
        }
      }
    } else {
      const map = asRecord(parsed);
      if (map) {
        for (const [name, raw] of Object.entries(map)) {
          const d = coerceDescriptor(name, raw);
          if (d) out.push(d);
        }
      }
    }
  }
  if (axes.length === 0) return out;
  const axisNames = new Set(axes.map((a) => a.name));
  return out.map((d) => (axisNames.has(d.name) ? { ...d, shadowsAxis: true } : d));
}

/**
 * The params the form renders a control for — everything an AXIS does not already own, and
 * everything the TEMPLATE actually substitutes (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point A).
 *
 * Almost every synthesis declares a passthrough `class`, and on 8 of the 9 components of a real
 * catalogue the template never carried `{{class}}` anywhere: typing `opacity-20` into it moved
 * nothing, ever, and the field was presented exactly like the one field that DID work. A control
 * that cannot act is a promise the surface cannot keep — so it is not offered, and
 * [`unsubstitutedParams`] names the declaration instead, once, where the reader can act on it.
 *
 * `template` is OPTIONAL and an absent one changes nothing: a caller with no template in hand (or a
 * component whose synthesis is still empty) keeps every declared param, because "the template
 * substitutes it nowhere" cannot be asserted from a template nobody has.
 */
export function editableParams(
  descriptors: readonly ParamDescriptor[],
  template?: string | null,
): ParamDescriptor[] {
  const wired = wiredPredicate(template);
  return descriptors.filter((d) => !d.shadowsAxis && wired(d.name));
}

/**
 * Does the template READ this name — substituted as a value (`{{label}}`) or gating an element
 * (`data-nyx-if="loading=on"`)? A template that does not exist yet reads EVERYTHING: nothing can be
 * asserted about a synthesis nobody has written.
 */
function wiredPredicate(template: string | null | undefined): (name: string) => boolean {
  const markup = template ?? "";
  if (markup.trim() === "") return () => true;
  const gated = new Set(templateConditions(markup).map((c) => c.axis));
  return (name) => gated.has(name) || templateSubstitutes(markup, name);
}

/**
 * The declared params the template substitutes NOWHERE — the fields that could never have done
 * anything (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point A).
 *
 * It is a DEFECT OF THE SYNTHESIS, not of the composition: the descriptor announces a value the
 * component accepts, and the markup has no place to put it. Only the agent that read the source can
 * fix it — either by substituting the param, or by dropping the declaration. Empty template → empty
 * list: a component nobody has synthesized yet declares nothing that could be wrong.
 */
export function unsubstitutedParams(
  descriptors: readonly ParamDescriptor[],
  template: string | null | undefined,
): string[] {
  if ((template ?? "").trim() === "") return [];
  const wired = wiredPredicate(template);
  return descriptors.filter((d) => !d.shadowsAxis && !wired(d.name)).map((d) => d.name);
}

/**
 * The params that DUPLICATE an axis (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`) — "variante déclarée à la
 * fois en param et en axe". Two controls over one notion, and the param one lies.
 */
export function paramsShadowingAxis(descriptors: readonly ParamDescriptor[]): string[] {
  return descriptors.filter((d) => d.shadowsAxis).map((d) => d.name);
}

/**
 * The params still declaring the DEPRECATED `state` type — a variance axis smuggled into the params.
 * The correction is always the same: move it to `states_json` as an axis.
 */
export function paramsWithDeprecatedStateType(descriptors: readonly ParamDescriptor[]): string[] {
  return descriptors
    .filter((d) => d.type === DEPRECATED_PARAM_TYPE && !d.shadowsAxis)
    .map((d) => d.name);
}

/**
 * The BOOLEAN params that can change NOTHING (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`, symptom #1 —
 * "les params booléens sont inertes : le toggle est décoratif"). A boolean is substituted as the
 * text `true`/`false` and nothing else: it carries no classes and gates no element. It is only alive
 * if it is an AXIS (two values, classes and/or a `data-nyx-if` case) — which is exactly the
 * correction the warning names.
 *
 * A boolean the template really substitutes somewhere (`aria-disabled="{{disabled}}"`) is still
 * listed: it is not USELESS, but it cannot make the component LOOK different, and that is what the
 * user is toggling it for.
 */
export function inertBooleanParams(
  descriptors: readonly ParamDescriptor[],
  axes: readonly VarianceAxis[],
  conditionAxes: readonly string[],
): string[] {
  const alive = new Set([...axes.map((a) => a.name), ...conditionAxes]);
  return descriptors.filter((d) => d.type === "boolean" && !alive.has(d.name)).map((d) => d.name);
}

/** A type-appropriate fallback value for a descriptor with no explicit default. */
function fallbackValue(d: ParamDescriptor): unknown {
  switch (d.type) {
    case "boolean":
      return false;
    case "number":
      return 0;
    case "state":
    case "select":
      return d.options?.[0] ?? "";
    default:
      return "";
  }
}

/** The default value of a param (its explicit `default`, else a type-appropriate fallback). */
export function defaultValue(d: ParamDescriptor): unknown {
  return d.default !== undefined ? d.default : fallbackValue(d);
}

/**
 * Build the default param map for a set of descriptors — the values a COMPOSITION starts from.
 *
 * DEFAULTS ONLY, never previews (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`): this map seeds a form whose
 * values can be PERSISTED onto a composition node (`proto_variant_set_node_params`), and an example
 * value invented to make a tile legible has no business being written into a real placement.
 */
export function defaultParamValues(
  descriptors: readonly ParamDescriptor[],
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const d of descriptors) out[d.name] = defaultValue(d);
  return out;
}

/**
 * The value to render an APERÇU with: `preview` if the synthesis gave one, else the component's own
 * `default`, else nothing (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`). That order is the whole point of
 * the split — a required param has no default BY DEFINITION, so only a preview value can make it
 * render; an optional param with a real default renders with the truth.
 *
 * `preview > default > rien`, and nothing else: the UI never invents a value.
 */
export function previewValue(d: ParamDescriptor): unknown {
  if (d.preview !== undefined) return d.preview;
  return defaultValue(d);
}

/**
 * The param map used to RENDER AN APERÇU — the catalogue vignette and the composant detail's
 * representative instance. Its twin [`defaultParamValues`] is what a persisted composition uses;
 * the two are deliberately different maps, so a preview value can never leak into a node.
 */
export function previewParamValues(
  descriptors: readonly ParamDescriptor[],
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const d of descriptors) out[d.name] = previewValue(d);
  return out;
}

/** Whether a resolved value renders as NOTHING (an empty aperçu — a pill with no label). */
function rendersEmpty(value: unknown): boolean {
  return value === "" || value == null;
}

/**
 * Does this param resolve to NOTHING in an aperçu? The FACT, per param, untriaged — the twin of the
 * `params[].renders_empty` the `design_preview` tool answers with for the agent.
 *
 * It is deliberately kept apart from the WARNING ([`paramsWithoutPreviewValue`]): the fact is the
 * fact, and every param that resolves empty goes on saying so. Only the ALARM is triaged.
 */
export function paramRendersEmpty(d: ParamDescriptor): boolean {
  return rendersEmpty(previewValue(d));
}

/**
 * Does `template` substitute `name` in TEXT-CONTENT position — BETWEEN tags, where what it yields is
 * READ — as opposed to inside a tag, as an attribute value?
 *
 * The scan tracks the `<` … `>` depth as it walks and reads `{{name}}` / `{{{name}}}` exactly as
 * [`templateSubstitutes`] does: THE CLOSING PAIR IS REQUIRED, so `{{label}` — a typo Handlebars
 * substitutes nothing for (finding `01KYSNPWWG9DA0AXP6D22GV5BK`) — is no site at all. A `<` inside an
 * attribute VALUE would fool it, but that is markup no synthesis writes unescaped, and the failure
 * mode is the safe one: the param is read as content and keeps its warning.
 *
 * Byte-for-byte the same walk as `substitutes_in_content` in `mcp_tools_design.rs`, and pinned to it
 * by `contract/design-fixtures.json` (`empty_param_cases`).
 */
function substitutesInContent(template: string, name: string): boolean {
  let inTag = false;
  let i = 0;
  while (i < template.length) {
    const c = template[i];
    if (c === "<") {
      inTag = true;
      i += 1;
    } else if (c === ">" && inTag) {
      inTag = false;
      i += 1;
    } else if (c === "{" && template[i + 1] === "{") {
      // The body starts after `{{` plus the optional third brace (the raw form `{{{p}}}`).
      const bodyAt = template[i + 2] === "{" ? i + 3 : i + 2;
      const end = template.indexOf("}}", bodyAt);
      if (end === -1) return false;
      if (!inTag && template.slice(bodyAt, end).trim() === name) return true;
      i = end + 2;
    } else {
      i += 1;
    }
  }
  return false;
}

/**
 * Does this param's EMPTINESS leave a hole a HUMAN WOULD SEE — the criterion that decides whether
 * "resolves to nothing" is worth a WARNING (finding `01KYVMTWVF3TH1XPEKV9MEBZTB`).
 *
 * Almost every synthesized component declares a passthrough `class`, which is EMPTY BY NATURE: it
 * exists so a CALLER can add classes, and its absence renders a perfectly complete button. A warning
 * that fires on essentially every component of every catalogue is the same as no warning at all.
 *
 * The criterion is NOT the name `class` — that would only hold until `wrapperClass`, `testId`,
 * `ariaLabel` or `style`. It is WHERE THE TEMPLATE PUTS THE PARAM:
 *
 *   * substituted in CONTENT (between tags) it supplies something a human READS —
 *     `<button>{{label}}</button>` empty is a coloured pill with no text, the blank tile the warning
 *     was written for. WARN.
 *   * substituted only INSIDE A TAG (an attribute value) it MODIFIES an element that renders
 *     identically without it. Empty is its state of rest. SILENT.
 *
 * `required` OVERRIDES the site reading: the descriptor itself asserts the component carries no value
 * of its own, so an empty one is a defect it DECLARED, attribute or not. That is also the safety net
 * — if the site reading is ever wrong, the params that matter most keep their warning.
 *
 * A param the template substitutes NOWHERE has no site at all and stays silent: "declared but never
 * substituted" is a different defect with a different correction, which the variance and node
 * contracts name.
 */
function emptyLeavesAVisibleHole(d: ParamDescriptor, template: string): boolean {
  return d.required === true || substitutesInContent(template, d.name);
}

/**
 * The REQUIRED params the aperçu cannot render: mandatory, and with neither a preview value nor a
 * default (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`). This is a DEFECT of the synthesis, and a definite
 * one — the component demands the value, so no rendering of it can be right without one.
 */
export function requiredParamsWithoutPreview(descriptors: readonly ParamDescriptor[]): string[] {
  return descriptors.filter((d) => d.required === true && paramRendersEmpty(d)).map((d) => d.name);
}

/**
 * The WARNING: the params whose aperçu value renders as NOTHING **and whose emptiness leaves a hole a
 * human would see** (finding `01KYPD8K75EC2AAY1KVAS2XZNX`, kept alive by `01KYPJ1RC6WJX1Z9CPPV23FG27`,
 * TRIAGED by `01KYVMTWVF3TH1XPEKV9MEBZTB`). The catalogue vignette renders ONE instance seeded with
 * the aperçu values, so such a param makes a `button` a coloured pill with no label and a `card` an
 * empty box — whether or not the synthesis marked it `required` (a synthesis written before the split
 * marks nothing, and its blank tile still needs an explanation).
 *
 * It used to report EVERY empty param, and therefore cried on the passthrough `class` almost every
 * synthesis declares — a warning that fires everywhere stops being read. It now applies
 * [`emptyLeavesAVisibleHole`]: `required`, or substituted at least once in CONTENT position. The
 * criterion is the TEMPLATE's, never a list of names, and `contract/design-fixtures.json`
 * (`empty_param_cases`) pins it to the Rust twin (`empty_leaves_a_visible_hole`).
 *
 * The FACT stays distinct from the WARNING: [`paramRendersEmpty`] goes on answering for every param,
 * triaged by nothing.
 *
 * The UI deliberately does NOT invent content in their place — a plausible label is a fact about
 * the real component, which only the SYNTHESIS (the agent reading the source) can know; the
 * Prototype manual asks for it explicitly. All the UI does with this list is NAME the gap, so an
 * empty-looking tile reads as "incomplete synthesis" rather than "broken renderer".
 */
export function paramsWithoutPreviewValue(
  descriptors: readonly ParamDescriptor[],
  /** The component's own template — the only thing the criterion reads. Absent → nothing to read. */
  template: string | null | undefined,
): string[] {
  const markup = template ?? "";
  return descriptors
    .filter((d) => paramRendersEmpty(d) && emptyLeavesAVisibleHole(d, markup))
    .map((d) => d.name);
}

/**
 * A REPRESENTATIVE single-instance composition for a component (task #10: "carte = 1 instance
 * représentative, pas la liste de params") — a one-node tree `{ component, params: <aperçu> }`,
 * serialized for `design_render`. It renders an APERÇU, so it uses the preview values
 * (`preview > default > rien`): a `button` whose mandatory label has an example finally shows a
 * label instead of an empty pill.
 *
 * The DEFAULT COMBINATION of the axes is folded into the same map (finding
 * `01KYQ0R84KVF6QVQXQ9YVG9Z8T`): a vignette renders the component at one value per axis, and a
 * template that substitutes an axis by name (`class="btn btn-{{state}}"` — the legacy escape hatch)
 * keeps resolving exactly as when the `state` param was synthesized here. The axes win over a param
 * of the same name: one notion, one value, and the axis is the real one.
 */
export function representativeComposition(
  componentName: string,
  descriptors: readonly ParamDescriptor[],
  axes: readonly VarianceAxis[] = [],
): string {
  return JSON.stringify({
    component: componentName,
    params: { ...previewParamValues(descriptors), ...axisDefaults(axes) },
  });
}
