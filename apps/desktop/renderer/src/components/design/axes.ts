/**
 * THE VARIANCE MODEL of a catalogue component — **named AXES, each value carrying its own concrete
 * classes** (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`).
 *
 * # The defect this replaces
 *
 * The model used to expose FOUR overlapping representations of the same notion — a `param`, a
 * `state`, a CVA axis and a boolean — with no rule saying which was which. Four symptoms followed,
 * and they were one defect: booleans were INERT (nothing but a state case could change a rendering,
 * and a boolean has no case), only ONE axis was pilotable (`states_json` was a FLAT list), a variant
 * was AMBIGUOUS (a param of type `state`? a declared state? both?), and the template was VERBOSE
 * (one complete rendering per case: 8 buttons for a `button`, 48 for `variant × size × loading`).
 *
 * # The rule, in one line
 *
 * **What makes a component vary is ALWAYS AN AXIS.** `variant` is an axis, `size` is an axis, and
 * `loading` / `disabled` are axes with TWO VALUES. A param is a SUBSTITUTED VALUE and nothing else.
 *
 * # The shape (`states_json`, three accepted forms — all parsed here)
 *
 * ```json
 * { "axes": [
 *     { "name": "variant", "label": "Variante", "default": "primary", "values": [
 *         { "name": "primary",   "classes": "bg-primary text-primary-foreground" },
 *         { "name": "destructive", "classes": "bg-destructive text-destructive-foreground" } ] },
 *     { "name": "size", "values": [
 *         { "name": "md", "classes": "h-9 px-4 text-sm" },
 *         { "name": "lg", "classes": "h-11 px-6 text-base" } ] },
 *     { "name": "loading", "values": [
 *         { "name": "off" },
 *         { "name": "on", "classes": "pointer-events-none opacity-70" } ] } ] }
 * ```
 *
 *   * the OBJECT form above (`{ axes: [...] }`) — the canonical one;
 *   * the bare ARRAY of axis objects (`[{ name, values }, …]`) — an entry carrying a `values` array
 *     IS an axis, which is what tells the two array forms apart;
 *   * the object MAP `{ "variant": ["primary", "ghost"] }` — tolerated, classes-less;
 *   * and the LEGACY FLAT LIST `["default", "destructive"]` / `[{ "name": "…" }]`, which is read as
 *     ONE anonymous axis named [`LEGACY_AXIS`] whose values carry no classes — exactly the previous
 *     behaviour, driven by `data-nyx-state-case` as before. 173 components are stored that way:
 *     they must keep rendering, so this is not a fallback, it is a supported form.
 *
 * # Two mechanisms, one model (and they compose)
 *
 *   * CLASSES — the SHIM applies the active value's classes onto the axis's targets (an element
 *     carrying `data-nyx-axis="variant size"`, or the component's root elements by default). ONE
 *     element in the template, orthogonal axes for free, NO cartesian product.
 *   * STRUCTURE — what classes cannot do: an element that only EXISTS in one value (the spinner of
 *     `loading=on`). That is `data-nyx-state-case` (kept, first axis) generalized as
 *     `data-nyx-if="loading=on"` / `data-nyx-if-not="variant=link"` — a condition on an AXIS
 *     VALUE, not on a boolean. See [`templateConditions`].
 *
 * Everything here is PURE and DEFENSIVE (a malformed blob yields an empty result, never a throw), so
 * the whole model is unit-testable without a browser and no stored row can break the catalogue.
 */

/** One value of an axis, with the CONCRETE classes the shim puts on the target for it. */
export interface AxisValue {
  readonly name: string;
  /** What the control shows for this value (default: its name). */
  readonly label?: string;
  /**
   * The resolved CVA classes of THIS value — a plain space-separated Tailwind string
   * (`"bg-primary text-primary-foreground"`), never a `cva()` call nor a class expression. Absent
   * means the value changes nothing BY ITSELF: it then needs a structural case, or it is INERT
   * ([`inertAxisValues`]).
   */
  readonly classes?: string;
}

/** One named axis of variance — `variant`, `size`, or a two-valued `loading`. */
export interface VarianceAxis {
  readonly name: string;
  /** What the control shows above the values (default: the axis name). */
  readonly label?: string;
  readonly values: readonly AxisValue[];
  /** The value an aperçu opens on: the declared `default`, else the first value. */
  readonly defaultValue: string;
  /** This axis came from the LEGACY flat `states_json` — one anonymous axis, no classes. */
  readonly legacy?: boolean;
}

/**
 * The name of the axis a LEGACY flat `states_json` becomes. It is `state` on purpose: that is the
 * key the whole existing chain already uses (the `{{state}}` substitution a stored template may
 * carry, the param a composition node persisted, `window.__nyxActiveState`), so a component
 * stored before this model keeps working byte for byte.
 */
export const LEGACY_AXIS = "state";

function asRecord(value: unknown): Record<string, unknown> | null {
  return value != null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/** `classes` as a string or an array of strings (an agent writes both); `class` is an alias. */
function coerceClasses(rec: Record<string, unknown>): string | undefined {
  const raw = rec.classes ?? rec.class;
  const text = Array.isArray(raw)
    ? raw.filter((c): c is string => typeof c === "string").join(" ")
    : typeof raw === "string"
      ? raw
      : "";
  const normalized = text.trim().split(/\s+/).filter(Boolean).join(" ");
  return normalized ? normalized : undefined;
}

function coerceValue(raw: unknown): AxisValue | null {
  if (typeof raw === "string") return raw.trim() ? { name: raw } : null;
  const rec = asRecord(raw);
  const name = rec?.name;
  if (typeof name !== "string" || !name.trim()) return null;
  return {
    name,
    label: typeof rec?.label === "string" ? rec.label : undefined,
    classes: rec ? coerceClasses(rec) : undefined,
  };
}

function buildAxis(
  name: string,
  rawValues: readonly unknown[],
  extra?: Record<string, unknown>,
): VarianceAxis | null {
  if (!name.trim()) return null;
  const values: AxisValue[] = [];
  for (const raw of rawValues) {
    const v = coerceValue(raw);
    if (v && !values.some((existing) => existing.name === v.name)) values.push(v);
  }
  if (values.length === 0) return null;
  const declaredDefault = extra?.default;
  const defaultValue =
    typeof declaredDefault === "string" && values.some((v) => v.name === declaredDefault)
      ? declaredDefault
      : values[0]!.name;
  return {
    name,
    label: typeof extra?.label === "string" ? extra.label : undefined,
    values,
    defaultValue,
  };
}

/** ONE anonymous axis for the legacy flat form — the retro-compatible reading. */
function legacyAxes(names: readonly unknown[]): VarianceAxis[] {
  const axis = buildAxis(LEGACY_AXIS, names);
  return axis ? [{ ...axis, legacy: true }] : [];
}

/**
 * Parse `states_json` into the component's AXES, in every accepted form (see the module doc).
 *
 * THE ORDER IS PART OF THE READING, in every form: the FIRST axis is the one `data-nyx-state-case`
 * drives and the one the chip-bar pilots, so which axis comes first is a VERDICT, not a presentation
 * detail. All four forms are read in DOCUMENT ORDER — the MAP form included, where `Object.entries`
 * hands back the insertion order of the parsed JSON. The Rust twin (`parse_axes`,
 * `mcp_tools_design.rs`) used to read THAT ONE FORM ALPHABETICALLY, because a `serde_json::Map` is a
 * `BTreeMap`: on `{ "variant": …, "loading": … }` the write-path validator judged `loading` primary
 * while this panel piloted `variant` — one stored row, two verdicts (finding
 * `01KYSNPWWG9DA0AXP6D22GV5BK`). It re-reads the raw text in document order now, the write paths
 * canonicalize the map form to `{ "axes": [...] }`, and the shared fixture
 * `contract/design-fixtures.json` pins the two implementations together.
 */
export function parseAxes(statesJson: string | null | undefined): VarianceAxis[] {
  if (!statesJson || !statesJson.trim()) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(statesJson);
  } catch {
    return [];
  }
  let rawAxes: unknown[] | null = null;
  if (Array.isArray(parsed)) {
    // An entry carrying a `values` ARRAY is an axis; anything else is a legacy state NAME. One
    // axis-shaped entry is enough to read the whole blob as axes (a half-written one is reported
    // by the write-path warnings, not silently re-read as a list of names).
    const looksLikeAxes = parsed.some((e) => Array.isArray(asRecord(e)?.values));
    if (!looksLikeAxes) return legacyAxes(parsed);
    rawAxes = parsed;
  } else {
    const rec = asRecord(parsed);
    if (!rec) return [];
    if (Array.isArray(rec.axes)) rawAxes = rec.axes;
    else {
      // The tolerated MAP form `{ "variant": ["primary", "ghost"] }`, in DOCUMENT order — the same
      // order the Rust twin reads it in (see the doc above).
      const out: VarianceAxis[] = [];
      for (const [name, values] of Object.entries(rec)) {
        if (!Array.isArray(values)) continue;
        const axis = buildAxis(name, values);
        if (axis && !out.some((a) => a.name === axis.name)) out.push(axis);
      }
      return out;
    }
  }
  const out: VarianceAxis[] = [];
  for (const raw of rawAxes) {
    const rec = asRecord(raw);
    const name = rec?.name;
    if (typeof name !== "string" || !Array.isArray(rec?.values)) continue;
    const axis = buildAxis(name, rec.values, rec);
    if (axis && !out.some((a) => a.name === axis.name)) out.push(axis);
  }
  return out;
}

/** The value NAMES of an axis, in declaration order (what a control lists). */
export function axisValueNames(axis: VarianceAxis): string[] {
  return axis.values.map((v) => v.name);
}

/** The DEFAULT combination — one value per axis. What a vignette renders. */
export function axisDefaults(axes: readonly VarianceAxis[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const axis of axes) out[axis.name] = axis.defaultValue;
  return out;
}

/**
 * The ACTIVE combination read out of an edited value map: one entry per axis, coerced to a value the
 * axis really declares (a stale or hand-written value falls back to the default). The frame is
 * handed THIS, never the raw form state.
 */
export function activeAxisValues(
  axes: readonly VarianceAxis[],
  values: Record<string, unknown>,
): Record<string, string> {
  const out: Record<string, string> = {};
  for (const axis of axes) {
    const raw = values[axis.name];
    out[axis.name] =
      typeof raw === "string" && axis.values.some((v) => v.name === raw) ? raw : axis.defaultValue;
  }
  return out;
}

/**
 * The FIRST axis's value names — what the legacy `states` chain (the chip-bar,
 * `window.__nyxStates`, `data-nyx-state-case`) is fed. `data-nyx-state-case` is the structural
 * mechanism of the FIRST axis; the other axes use `data-nyx-if`.
 */
export function primaryAxisStates(axes: readonly VarianceAxis[]): string[] {
  return axes.length > 0 ? axisValueNames(axes[0]!) : [];
}

/**
 * Every `data-nyx-state-case="<name>"` a template carries — the FIRST axis's values it renders a
 * structural case for (the mechanism kept from finding `01KYPSG1HM2GPJMNH2SP3DDADH`). Both quotings
 * are read: the template is authored by an agent, not by a formatter.
 */
export function templateStateCases(template: string | null | undefined): string[] {
  if (!template) return [];
  const out: string[] = [];
  for (const m of template.matchAll(/data-nyx-state-case\s*=\s*(?:"([^"]*)"|'([^']*)')/g)) {
    const name = m[1] ?? m[2] ?? "";
    if (name && !out.includes(name)) out.push(name);
  }
  return out;
}

/** One clause of a `data-nyx-if` / `data-nyx-if-not` condition: an AXIS at one of N values. */
export interface AxisCondition {
  readonly axis: string;
  readonly values: readonly string[];
  /** It came from `data-nyx-if-not` — the element is HIDDEN when it matches. */
  readonly negated: boolean;
}

/** Parse one attribute body — `"loading=on"`, `"variant=primary|ghost, size=lg"`. */
function parseConditionBody(body: string, negated: boolean): AxisCondition[] {
  const out: AxisCondition[] = [];
  for (const clause of body.split(/[,;]/)) {
    const eq = clause.indexOf("=");
    if (eq < 0) continue;
    const axis = clause.slice(0, eq).trim();
    const values = clause
      .slice(eq + 1)
      .split("|")
      .map((v) => v.trim())
      .filter(Boolean);
    if (axis && values.length > 0) out.push({ axis, values, negated });
  }
  return out;
}

/**
 * Every STRUCTURAL condition a template declares — the generalized `data-nyx-if`. It is the ONE
 * thing classes cannot express: an element that EXISTS only for some values of an axis (the spinner
 * of `loading=on`, the chevron of `variant=dropdown`).
 *
 * GRAMMAR, deliberately tiny — a condition, never a language:
 *   * `data-nyx-if="loading=on"` — visible only when the `loading` axis is at `on`;
 *   * `data-nyx-if="variant=primary|ghost"` — one of several values (OR inside a clause);
 *   * `data-nyx-if="loading=on, size=lg"` — several axes at once (AND between clauses);
 *   * `data-nyx-if-not="variant=link"` — the complement.
 * No expressions, no nesting, no negation operator inside a clause.
 */
export function templateConditions(template: string | null | undefined): AxisCondition[] {
  if (!template) return [];
  const out: AxisCondition[] = [];
  for (const m of template.matchAll(
    /data-nyx-if(-not)?\s*=\s*(?:"([^"]*)"|'([^']*)')/g,
  ) as IterableIterator<RegExpMatchArray>) {
    const negated = m[1] === "-not";
    out.push(...parseConditionBody(m[2] ?? m[3] ?? "", negated));
  }
  return out;
}

/**
 * Does the template SUBSTITUTE this axis as a value (`{{variant}}` / `{{{variant}}}`)? The rarer,
 * legitimate way to vary with no classes and no case at all — `class="btn btn-{{state}}"` on a front
 * whose variants are plain class names. It is why an axis without classes is WARNED about and never
 * refused.
 */
export function templateSubstitutes(template: string | null | undefined, name: string): boolean {
  if (!template || !name) return false;
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`\\{\\{\\{?\\s*${escaped}\\s*\\}\\}\\}?`).test(template);
}

/**
 * Is this VALUE shown by that condition — does an element carrying it EXIST when the axis sits on
 * this value?
 */
function conditionShows(condition: AxisCondition, value: string): boolean {
  const named = condition.values.includes(value);
  return condition.negated ? !named : named;
}

/**
 * Does a `data-nyx-if` / `data-nyx-if-not` on THIS AXIS make THIS VALUE render differently from at
 * least one OTHER value of the same axis (finding `01KYSCY4J8JANZEJPYME86B1VH`)?
 *
 * This is what CREDITS A BASELINE. The manual's canonical boolean is `loading: [{ "name": "off" },
 * { "name": "on", "classes": … }]` with ONE conditional element (`data-nyx-if="loading=on"`), and
 * `off` deliberately carries nothing: the baseline IS the absence of classes, and what makes it
 * visible is the COMPLEMENT — the spinner that appears at `on` and is gone at `off`. Asking whether
 * the condition NAMES the value (the previous reading) reported that `off` as inert, so the panel
 * reproached exactly the shape the manual teaches. Asking instead whether the condition
 * DISCRIMINATES the value keeps the diagnostic exact in both directions:
 *
 *   * `data-nyx-if="loading=on"` over `{off, on}` → shown at `on`, hidden at `off`: both values are
 *     credited, because switching between them really does change the rendering;
 *   * a value on an axis NOTHING conditions on stays inert — the real defect, still reported;
 *   * so does a condition that cannot discriminate anything (`data-nyx-if="loading=off|on"` is
 *     always true): the element is there whatever the axis does, which is the same inertia.
 *
 * It is the exact twin of the write-path validator (`condition_discriminates`, `mcp_tools_design.rs`)
 * — the panel and the validator must never say different things about one stored component.
 */
function conditionDiscriminates(
  axis: VarianceAxis,
  value: string,
  conditions: readonly AxisCondition[],
): boolean {
  return conditions.some((c) => {
    if (c.axis !== axis.name) return false;
    const here = conditionShows(c, value);
    return axis.values.some(
      (other) => other.name !== value && conditionShows(c, other.name) !== here,
    );
  });
}

/**
 * Does ANOTHER value of the same axis carry CLASSES, and so DISCRIMINATE this bare one (finding
 * `01KYT103MXTKFBZMKF968FGW9P`)?
 *
 * The same rule as [`conditionDiscriminates`], applied to the OTHER mechanism. Observed on
 * `checkbox`: an axis with `enabled` bare beside `disabled` carrying `opacity-50` reported `enabled`
 * inert. It is not — choosing `enabled` rather than `disabled` really does change the rendering,
 * because it is the ABSENCE of `opacity-50` that defines it, exactly as it is the absence of classes
 * that defines a baseline `off`. The previous reading only credited a value through a `data-nyx-if` /
 * `-if-not`, so it reproached the very shape the manual teaches — and pushed the author towards the
 * one correction the manual explicitly forbids, INVENTING classes on a baseline to silence a warning.
 *
 * It stops at the AXIS, like every credit here: classes on one axis say nothing about another one.
 * The real inert case therefore survives intact — an axis where NO value carries classes, no
 * condition discriminates and no state-case exists is still reported.
 *
 * The exact twin of the write-path validator (`classes_discriminate`, `mcp_tools_design.rs`), pinned
 * against it by `contract/design-fixtures.json`.
 */
function classesDiscriminate(axis: VarianceAxis, value: string): boolean {
  return axis.values.some((other) => other.name !== value && Boolean(other.classes));
}

/** An axis value that can change NOTHING — the coordinates the UI and the warnings name. */
export interface InertAxisValue {
  readonly axis: string;
  readonly value: string;
}

/**
 * The axis values that are INERT — no `classes`, no structural case, no substitution: selecting one
 * cannot change a single pixel. This is the generalization of the old "declared state with no
 * `data-nyx-state-case`" check to the whole model, and it is what makes a declared boolean
 * (`loading` with two class-less values) report itself instead of being a decorative toggle.
 *
 * A value is NOT inert as soon as one of these holds:
 *   * it carries `classes` (the shim puts them on the target);
 *   * ANOTHER value of the same axis carries `classes` — switching between the two moves pixels, so
 *     it is the ABSENCE of those classes that defines this one (see [`classesDiscriminate`]);
 *   * the FIRST axis renders a `data-nyx-state-case` for it;
 *   * a `data-nyx-if` / `data-nyx-if-not` on its axis DISCRIMINATES it — the element it gates is
 *     there for this value and gone for another one of the same axis, or the reverse. A BASELINE
 *     carries nothing ON PURPOSE, and it is the COMPLEMENT that makes it visible, so the question is
 *     never whether a condition NAMES the value (see [`conditionDiscriminates`]);
 *   * the template substitutes the axis name itself (`{{state}}` — the escape hatch).
 *
 * The two DISCRIMINATION credits are one rule over two mechanisms, and both stop at the AXIS: an
 * axis where no value carries classes, nothing conditions and no case exists is the REAL inert case,
 * and it is still reported.
 *
 * A template that is EMPTY reports nothing: a `[NEW]` component is legitimately authored from its
 * axes alone, before anyone writes its markup.
 */
export function inertAxisValues(
  axes: readonly VarianceAxis[],
  template: string | null | undefined,
): InertAxisValue[] {
  if (!template || !template.trim()) return [];
  const cases = templateStateCases(template);
  const conditions = templateConditions(template);
  const out: InertAxisValue[] = [];
  axes.forEach((axis, index) => {
    if (templateSubstitutes(template, axis.name)) return;
    for (const value of axis.values) {
      if (value.classes) continue;
      // A SIBLING VALUE'S CLASSES discriminate this bare one just as a condition does (finding
      // `01KYT103MXTKFBZMKF968FGW9P`): `enabled` beside a `disabled` at `opacity-50` is defined by
      // the ABSENCE of those classes.
      if (classesDiscriminate(axis, value.name)) continue;
      if (index === 0 && cases.includes(value.name)) continue;
      // The COMPLEMENT counts (finding `01KYSCY4J8JANZEJPYME86B1VH`): a baseline carries nothing ON
      // PURPOSE, and it is the condition on its AXIS that makes it visible.
      if (conditionDiscriminates(axis, value.name, conditions)) continue;
      out.push({ axis: axis.name, value: value.name });
    }
  });
  return out;
}

/**
 * The FIRST axis's values the template renders NO `data-nyx-state-case` for, ONCE it renders
 * some — the PARTIAL break of the case contract (finding `01KYPSG1HM2GPJMNH2SP3DDADH`), which is
 * worse than inert: selecting a case-less value hides every other case, so the aperçu goes EMPTY.
 *
 * Returns nothing when the template carries no case at all — that whole-component defect is
 * [`inertAxisValues`]'s to name, and re-listing every value here would be noise.
 */
export function statesWithoutCase(
  template: string | null | undefined,
  states: readonly string[],
): string[] {
  const cases = templateStateCases(template);
  if (cases.length === 0) return [];
  return states.filter((s) => !cases.includes(s));
}

/** Conditions that name an axis the component does not declare — they can never be evaluated. */
export function conditionsOnUnknownAxes(
  axes: readonly VarianceAxis[],
  template: string | null | undefined,
): string[] {
  const known = new Set(axes.map((a) => a.name));
  const out: string[] = [];
  for (const c of templateConditions(template)) {
    if (!known.has(c.axis) && !out.includes(c.axis)) out.push(c.axis);
  }
  return out;
}
