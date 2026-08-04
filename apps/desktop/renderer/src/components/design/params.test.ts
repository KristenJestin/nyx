import { describe, expect, test } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";

import { parseAxes } from "./axes";
import {
  defaultParamValues,
  DEPRECATED_PARAM_TYPE,
  editableParams,
  inertBooleanParams,
  PARAM_TYPES,
  paramRendersEmpty,
  parseParams,
  paramsShadowingAxis,
  paramsWithDeprecatedStateType,
  paramsWithoutPreviewValue,
  parseStates,
  previewParamValues,
  previewValue,
  representativeComposition,
  requiredParamsWithoutPreview,
  unsubstitutedParams,
} from "./params";
import { componentNature, type DesignComponentRow } from "./types";

function component(overrides: Partial<DesignComponentRow>): DesignComponentRow {
  return {
    id: "c1",
    project_id: "p1",
    name: "Card",
    source_repository_id: null,
    source_path: null,
    visual_hash: null,
    template: "",
    params_json: null,
    states_json: null,
    created_at: 0,
    updated_at: 0,
    scanned_input_hash: null,
    orphaned_at: null,
    ...overrides,
  };
}

describe("componentNature", () => {
  test("a component with a source path is DS, a greenfield one is NEW", () => {
    expect(componentNature(component({ source_path: "src/Button.tsx" }))).toBe("ds");
    expect(componentNature(component({ source_path: null }))).toBe("new");
  });
});

describe("parseStates", () => {
  test("parses strings and {name} objects, tolerates a bad blob", () => {
    expect(parseStates('["idle","loading"]')).toEqual(["idle", "loading"]);
    expect(parseStates('[{"name":"error"}]')).toEqual(["error"]);
    expect(parseStates("nope")).toEqual([]);
    expect(parseStates(null)).toEqual([]);
  });
});

describe("parseParams", () => {
  test("normalizes the array form and keeps text/html types", () => {
    const params = parseParams(
      JSON.stringify([
        { name: "title", type: "text", default: "Hi" },
        { name: "body", type: "html", default: "<p>x</p>" },
      ]),
      [],
    );
    expect(params.map((p) => [p.name, p.type])).toEqual([
      ["title", "text"],
      ["body", "html"],
    ]);
  });

  test("accepts the object-map form", () => {
    const params = parseParams(JSON.stringify({ title: { type: "text" } }), []);
    expect(params[0]?.name).toBe("title");
  });

  /**
   * THE RULE (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`): an AXIS is never a param. This used to append a
   * synthetic `state` descriptor whenever the component declared states — the single sélecteur
   * d'état, and the very ambiguity the axes model removes. The axes are their own model with one
   * control each; the params are values.
   */
  test("does NOT synthesize a state param — an axis is not a param", () => {
    expect(parseParams(null, parseAxes('["idle","loading"]'))).toEqual([]);
  });

  test("flags a param that DUPLICATES an axis, and keeps it out of the form", () => {
    const axes = parseAxes('{"axes":[{"name":"variant","values":["primary","ghost"]}]}');
    const params = parseParams(
      JSON.stringify([
        { name: "variant", type: "select", options: ["primary", "ghost"] },
        { name: "label", type: "text" },
      ]),
      axes,
    );
    expect(paramsShadowingAxis(params)).toEqual(["variant"]);
    // The axis control is the real one — the duplicate never becomes a second field.
    expect(editableParams(params).map((p) => p.name)).toEqual(["label"]);
  });

  /**
   * A FIELD THAT COULD NEVER ACT IS NOT OFFERED (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point A).
   * On a real catalogue, 9 components declared a passthrough `class` and exactly ONE template
   * carried `{{class}}`: typing into the other eight moved nothing, ever, and the field looked
   * exactly like the one that worked.
   */
  test("offers no control for a param the template reads NOWHERE, and names it", () => {
    const params = parseParams(
      JSON.stringify([
        { name: "label", type: "text" },
        { name: "class", type: "text" },
      ]),
      [],
    );
    const template = "<button>{{label}}</button>";
    expect(editableParams(params, template).map((p) => p.name)).toEqual(["label"]);
    expect(unsubstitutedParams(params, template)).toEqual(["class"]);

    // The SAME param, on the one template that does substitute it, is a real field again.
    const wired = '<i class="{{class}}">{{label}}</i>';
    expect(editableParams(params, wired).map((p) => p.name)).toEqual(["label", "class"]);
    expect(unsubstitutedParams(params, wired)).toEqual([]);
  });

  test("a param the template only GATES an element on is wired, not dead", () => {
    // `data-nyx-if="loading=on"` reads the name without ever substituting it — dropping such a
    // control would remove one that works.
    const params = parseParams(JSON.stringify([{ name: "loading", type: "boolean" }]), []);
    const template = '<button><i data-nyx-if="loading=on"></i></button>';
    expect(editableParams(params, template).map((p) => p.name)).toEqual(["loading"]);
    expect(unsubstitutedParams(params, template)).toEqual([]);
  });

  test("asserts nothing against a template that does not exist yet", () => {
    // A component the inventory found and nobody described: every declared param survives, because
    // "the template reads it nowhere" cannot be said of a template nobody wrote.
    const params = parseParams(JSON.stringify([{ name: "class", type: "text" }]), []);
    expect(editableParams(params, "").map((p) => p.name)).toEqual(["class"]);
    expect(editableParams(params).map((p) => p.name)).toEqual(["class"]);
    expect(unsubstitutedParams(params, null)).toEqual([]);
  });

  test("names the params still declaring the DEPRECATED `state` type", () => {
    const params = parseParams(JSON.stringify([{ name: "mode", type: "state" }]), []);
    expect(paramsWithDeprecatedStateType(params)).toEqual(["mode"]);
  });

  /**
   * SYMPTOM #1 of the finding: "les params booléens sont inertes — le toggle est décoratif". A
   * boolean is substituted as the text true/false: it carries no classes and gates no element. It is
   * only alive as an AXIS (or as the subject of a `data-nyx-if`).
   */
  test("names the BOOLEAN params that can change nothing", () => {
    const params = parseParams(
      JSON.stringify([
        { name: "loading", type: "boolean" },
        { name: "disabled", type: "boolean" },
      ]),
      [],
    );
    // `loading` is declared as an axis → alive. `disabled` is nothing → inert.
    expect(
      inertBooleanParams(params, parseAxes('{"axes":[{"name":"loading","values":["off"]}]}'), []),
    ).toEqual(["disabled"]);
    // …and a boolean the template gates with data-nyx-if is alive too.
    expect(inertBooleanParams(params, [], ["disabled"])).toEqual(["loading"]);
  });

  /**
   * AN UNKNOWN TYPE IS STILL COERCED, BUT NO LONGER LOST (finding `01KYPSG0HPG2D28CMJEXRKNEVK`).
   * A row stored before the write-time guard must keep rendering — the parse is defensive by
   * contract — so `"enum"` still becomes a `text` control; what changes is that the declared word
   * survives in `coercedFrom`, which is what lets the form say it instead of passing a constrained
   * choice off as a free field.
   */
  test("coerces an unknown type to text and REMEMBERS the word the synthesis wrote", () => {
    const params = parseParams(
      JSON.stringify([
        { name: "buttonVariant", type: "enum", options: ["default", "destructive"] },
        { name: "weird", type: 42 },
        { name: "label", type: "text" },
        { name: "untyped" },
      ]),
      [],
    );
    expect(params.map((p) => [p.name, p.type, p.coercedFrom])).toEqual([
      ["buttonVariant", "text", "enum"],
      ["weird", "text", "42"],
      // A KNOWN type, and an OMITTED one (which has always meant `text`), are not coercions.
      ["label", "text", undefined],
      ["untyped", "text", undefined],
    ]);
  });

  test("every documented type survives the parse untouched", () => {
    const params = parseParams(JSON.stringify(PARAM_TYPES.map((t) => ({ name: t, type: t }))), []);
    expect(params.map((p) => p.type)).toEqual([...PARAM_TYPES]);
    expect(params.every((p) => p.coercedFrom === undefined)).toBe(true);
  });

  /**
   * THE ARRIMAGE of the six types (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 1). This list lived
   * at THREE places joined by nothing: the Rust constant the write path REFUSES outside of, this
   * one, and the prose of the Prototype role manual. A seventh type accepted by Rust alone would be
   * stored and then silently rendered HERE as a free-text field with its `options` dropped — which
   * is exactly the regression finding `01KYPSG0HPG2D28CMJEXRKNEVK` already paid for once.
   *
   * BOTH DIRECTIONS, and in ORDER: a type added to the fixture with no entry here fails, and one
   * added here with no fixture line fails too. Rust replays the same section in
   * `mcp_tools_design::tests::the_shared_fixture_pins_the_six_param_types`, and the MANUAL's own
   * bullets are extracted from the rendered text in
   * `mcp_context::tests::the_manual_teaches_exactly_the_six_param_types_the_write_path_accepts`.
   */
  test("declares exactly the types the shared fixture pins, in the same order", () => {
    const fixture = designFixtures.param_types;
    expect([...PARAM_TYPES]).toEqual(fixture.types);
    expect(DEPRECATED_PARAM_TYPE).toBe(fixture.deprecated);
    // The renderer COERCES an unknown type towards `text` because it only DISPLAYS a row already
    // stored; the write path REFUSES it. The fixture states both, so neither reads as a second
    // opinion about the other — and `text` must itself be pinned for the coercion to be legal.
    expect(fixture.unknown_type_is_refused).toBe(true);
    expect(fixture.types).toContain("text");
    const [coerced] = parseParams(
      JSON.stringify([{ name: "align", type: "a_type_from_the_future" }]),
      [],
    );
    expect([coerced.type, coerced.coercedFrom]).toEqual(["text", "a_type_from_the_future"]);
  });
});

/**
 * Finding `01KYPJ1RC6WJX1Z9CPPV23FG27`: `required` and a PREVIEW value are two distinct notions the
 * single `default` used to conflate. A `button` has no default label — the label is MANDATORY — so
 * writing one as `default` lies about the contract, while leaving it empty renders a blank pill.
 */
describe("required + preview", () => {
  test("parses both fields, and defaults them the retro-compatible way", () => {
    const [required, legacy] = parseParams(
      JSON.stringify([
        { name: "label", type: "text", required: true, preview: "Enregistrer" },
        { name: "title", type: "text", default: "Chiffre d'affaires" },
      ]),
      [],
    );
    expect(required).toMatchObject({ required: true, preview: "Enregistrer" });
    expect(required?.default).toBeUndefined();
    // A descriptor written BEFORE the split stays valid: optional, no example.
    expect(legacy).toMatchObject({ required: false, default: "Chiffre d'affaires" });
    expect(legacy?.preview).toBeUndefined();
  });

  test("the aperçu resolves preview > default > rien", () => {
    const [withPreview, both, onlyDefault, nothing] = parseParams(
      JSON.stringify([
        { name: "a", type: "text", required: true, preview: "Enregistrer" },
        { name: "b", type: "text", default: "Fermer", preview: "Enregistrer" },
        { name: "c", type: "text", default: "Fermer" },
        { name: "d", type: "text", required: true },
      ]),
      [],
    );
    expect(previewValue(withPreview!)).toBe("Enregistrer");
    // The preview WINS over the default — it is the value chosen to illustrate.
    expect(previewValue(both!)).toBe("Enregistrer");
    expect(previewValue(onlyDefault!)).toBe("Fermer");
    // …and nothing is invented for the third case: it renders empty, and gets REPORTED.
    expect(previewValue(nothing!)).toBe("");
  });

  /**
   * The preview value is APERÇU DATA: it feeds the vignette and the detail's representative
   * instance, and it must never be mistaken for a value a composition node could be saved with.
   * The two maps are therefore different by construction.
   */
  test("preview values never leak into the map a composition is seeded with", () => {
    const descriptors = parseParams(
      JSON.stringify([
        { name: "label", type: "text", required: true, preview: "Enregistrer" },
        { name: "title", type: "text", default: "Fermer" },
      ]),
      [],
    );
    expect(previewParamValues(descriptors)).toEqual({ label: "Enregistrer", title: "Fermer" });
    // The DEFAULT map — what `proto_variant_set_node_params` may end up persisting — has no example.
    expect(defaultParamValues(descriptors)).toEqual({ label: "", title: "Fermer" });
  });

  describe("what is reported as missing", () => {
    const descriptors = parseParams(
      JSON.stringify([
        // 1. required WITH a preview value → renders, nothing to report.
        { name: "label", type: "text", required: true, preview: "Enregistrer" },
        // 2. required WITHOUT one → the definite defect.
        { name: "body", type: "html", required: true },
        // 3. optional with a default → renders, nothing to report.
        { name: "title", type: "text", default: "Chiffre d'affaires" },
        // 4. optional with nothing → a milder blank (the pre-`required` synthesis case).
        { name: "hint", type: "text" },
      ]),
      parseAxes('["default"]'),
    );
    /** Every one of them substituted BETWEEN tags — the position where empty is a hole. */
    const template =
      "<article><h2>{{label}}</h2><p>{{title}}</p><small>{{hint}}</small>{{{body}}}</article>";

    test("names the REQUIRED params the aperçu cannot render, and only those", () => {
      expect(requiredParamsWithoutPreview(descriptors)).toEqual(["body"]);
    });

    test("still names every blank that leaves a hole, so a legacy synthesis keeps its explanation", () => {
      expect(paramsWithoutPreviewValue(descriptors, template)).toEqual(["body", "hint"]);
    });

    test("a synthesis that gives each param what it needs reports nothing", () => {
      const complete = parseParams(
        JSON.stringify([
          { name: "label", type: "text", required: true, preview: "Enregistrer" },
          { name: "title", type: "text", default: "Chiffre d'affaires" },
        ]),
        parseAxes('["default"]'),
      );
      expect(requiredParamsWithoutPreview(complete)).toEqual([]);
      expect(paramsWithoutPreviewValue(complete, template)).toEqual([]);
    });
  });
});

describe("representativeComposition", () => {
  test("builds a one-node tree seeded with the APERÇU param values", () => {
    const axes = parseAxes('["idle"]');
    const descriptors = parseParams(
      JSON.stringify([{ name: "label", type: "text", default: "OK" }]),
      axes,
    );
    const json = representativeComposition("Button", descriptors, axes);
    // The DEFAULT COMBINATION of the axes rides in the same map (finding
    // `01KYQ0R84KVF6QVQXQ9YVG9Z8T`), so a template substituting `{{state}}` keeps resolving.
    expect(JSON.parse(json)).toEqual({
      component: "Button",
      params: { label: "OK", state: "idle" },
    });
  });

  test("the vignette renders the DEFAULT combination of EVERY axis", () => {
    const axes = parseAxes(
      JSON.stringify({
        axes: [
          { name: "variant", default: "ghost", values: ["primary", "ghost"] },
          { name: "size", values: [{ name: "md" }, { name: "lg" }] },
        ],
      }),
    );
    const descriptors = parseParams(
      JSON.stringify([{ name: "label", type: "text", preview: "OK" }]),
      axes,
    );
    expect(JSON.parse(representativeComposition("Button", descriptors, axes))).toEqual({
      component: "Button",
      params: { label: "OK", variant: "ghost", size: "md" },
    });
  });

  /**
   * THE END OF THE EMPTY VIGNETTE: a `button` whose label is REQUIRED (hence has no default) used to
   * render as a coloured pill with nothing in it. Its preview value is what the tile now shows —
   * without that value ever becoming the component's declared default.
   */
  test("renders a required param from its preview value", () => {
    const descriptors = parseParams(
      JSON.stringify([{ name: "label", type: "text", required: true, preview: "Enregistrer" }]),
      parseAxes('["default"]'),
    );
    expect(
      JSON.parse(representativeComposition("Button", descriptors, parseAxes('["default"]'))),
    ).toEqual({
      component: "Button",
      params: { label: "Enregistrer", state: "default" },
    });
  });

  test("defaultParamValues falls back per type when no default is given", () => {
    const descriptors = parseParams(
      JSON.stringify([
        { name: "count", type: "number" },
        { name: "on", type: "boolean" },
        { name: "text", type: "text" },
      ]),
      [],
    );
    expect(defaultParamValues(descriptors)).toEqual({ count: 0, on: false, text: "" });
  });
});

/**
 * Finding `01KYPD8K75EC2AAY1KVAS2XZNX`: a param the synthesis left without a usable value renders as
 * the EMPTY STRING, which is what makes a vignette look blank (a `button` with no label). The UI
 * never invents content in its place — it only has to be able to NAME the gap.
 *
 * TRIAGED by finding `01KYVMTWVF3TH1XPEKV9MEBZTB`: it fired on essentially every component of every
 * catalogue, because almost every synthesis declares a passthrough `class` that is empty BY NATURE.
 * The criterion is the TEMPLATE's — `required`, or substituted at least once BETWEEN tags — and it
 * is pinned to the Rust twin by `contract/design-fixtures.json`.
 */
describe("paramsWithoutPreviewValue", () => {
  test("names the params whose aperçu value renders as nothing, and only those", () => {
    const descriptors = parseParams(
      JSON.stringify([
        { name: "label", type: "text" },
        { name: "body", type: "html" },
        { name: "title", type: "text", default: "Enregistrer" },
        { name: "count", type: "number" },
        { name: "on", type: "boolean" },
        { name: "size", type: "select", options: ["sm", "lg"] },
        { name: "empty", type: "select" },
      ]),
      parseAxes('["default","loading"]'),
    );
    const template =
      "<div><b>{{label}}</b>{{{body}}}<h1>{{title}}</h1><i>{{count}}</i><u>{{on}}</u>" +
      "<s>{{size}}</s><em>{{empty}}</em></div>";
    // Text/html with no default, and a select with no option to fall back on. NOT number/boolean
    // (0 / false render), NOT an explicit default, NOT the state param (its options seed it).
    expect(paramsWithoutPreviewValue(descriptors, template)).toEqual(["label", "body", "empty"]);
  });

  test("a fully-defaulted synthesis leaves no gap to report", () => {
    const descriptors = parseParams(
      JSON.stringify([{ name: "label", type: "text", default: "Enregistrer" }]),
      parseAxes('["default"]'),
    );
    expect(paramsWithoutPreviewValue(descriptors, "<b>{{label}}</b>")).toEqual([]);
  });

  /**
   * THE TRIAGE, BITING IN BOTH DIRECTIONS ON ONE AND THE SAME DESCRIPTOR SET. The only thing that
   * differs between the two assertions is the TEMPLATE — the site the param is substituted at — which
   * is the whole claim: the criterion is not a name, and it is not the descriptor either.
   */
  test("a passthrough goes silent where a label still cries, on the same descriptors", () => {
    const descriptors = parseParams(
      JSON.stringify([
        { name: "class", type: "text" },
        { name: "label", type: "text" },
      ]),
      [],
    );
    // BOTH resolve empty — the FACT is untouched, and it is what the untriaged read still answers.
    expect(descriptors.filter(paramRendersEmpty).map((d) => d.name)).toEqual(["class", "label"]);
    // `class` inside the tag, `label` between the tags → only the label is worth an alarm.
    expect(
      paramsWithoutPreviewValue(descriptors, '<button class="btn {{class}}">{{label}}</button>'),
    ).toEqual(["label"]);
    // MUTATION: move `class` into content and it cries; move `label` into an attribute and it stops.
    expect(
      paramsWithoutPreviewValue(descriptors, '<button aria-label="{{label}}">{{class}}</button>'),
    ).toEqual(["class"]);
  });

  /**
   * `required` OVERRIDES the site: the descriptor asserts the component carries no value of its own,
   * so an empty one is a defect it DECLARED — attribute or not. It is also the safety net if the site
   * reading is ever wrong, which is why the two degrees still nest.
   */
  test("required overrides the site wherever the template lands it", () => {
    const descriptors = parseParams(
      JSON.stringify([{ name: "href", type: "text", required: true }]),
      [],
    );
    expect(paramsWithoutPreviewValue(descriptors, '<a href="{{href}}">read</a>')).toEqual(["href"]);
    // The stricter degree is a SUBSET of the triaged one, still.
    expect(requiredParamsWithoutPreview(descriptors)).toEqual(["href"]);
  });

  /** No template to read (a component discovered but never synthesized) → no site, so no alarm. */
  test("stays silent when there is no template at all, except for what is required", () => {
    const descriptors = parseParams(
      JSON.stringify([
        { name: "label", type: "text" },
        { name: "href", type: "text", required: true },
      ]),
      [],
    );
    expect(paramsWithoutPreviewValue(descriptors, null)).toEqual(["href"]);
    expect(paramsWithoutPreviewValue(descriptors, "")).toEqual(["href"]);
  });

  /**
   * THE SHARED FIXTURE, replayed here — and, with the very same expectations, by
   * `mcp_tools_design.rs::tests::the_shared_fixture_pins_when_an_empty_param_is_worth_a_warning`.
   * Two implementations of one judgement; a case added to the file must fail whichever of them
   * disagrees, which is what stops the UI's alarm and the agent's from drifting apart again.
   */
  test("replays the shared fixture's empty-param cases", () => {
    const cases = designFixtures.empty_param_cases.cases;
    expect(cases.length).toBeGreaterThan(0);
    for (const c of cases) {
      const descriptors = parseParams(
        JSON.stringify([{ name: c.param.name, type: "text", required: c.param.required }]),
        [],
      );
      // Every case is a param that RESOLVES EMPTY — the fact — so what the case decides is the
      // WARNING alone.
      expect(descriptors.every(paramRendersEmpty)).toBe(true);
      const warned = paramsWithoutPreviewValue(descriptors, c.template);
      expect(warned.includes(c.param.name), `${c.id}: ${c.why}`).toBe(c.expect_warned);
    }
  });
});
