import { describe, expect, test } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";

import {
  activeAxisValues,
  axisDefaults,
  conditionsOnUnknownAxes,
  inertAxisValues,
  LEGACY_AXIS,
  parseAxes,
  primaryAxisStates,
  statesWithoutCase,
  templateConditions,
  templateStateCases,
  templateSubstitutes,
} from "./axes";

/**
 * THE SHARED CROSS-LANGUAGE FIXTURE (finding `01KYSNQWQD1324E0NA0172M66J`).
 *
 * The variance rule exists twice — here and in `crates/nyx-core/src/mcp_tools_design.rs` — and this
 * suite used to RE-TYPE the canonical example by hand, so it could not fail: it drifted,
 * which is exactly how the two implementations ended up reading the map form in two different
 * orders. `contract/design-fixtures.json` is now the CASES, read by both suites at test time (a
 * plain data file: nothing generates it, and neither side builds the other). A case added to it
 * fails BOTH suites until BOTH implementations agree, and the Rust side keeps its canonical case
 * moored to the write path's own checks.
 */
interface VarianceFixtureCase {
  readonly id: string;
  readonly why: string;
  readonly canonical?: boolean;
  readonly template: string;
  readonly states_json: string;
  readonly expect: {
    readonly axes: string[];
    readonly primary_axis_values: string[];
    readonly state_cases: string[];
    readonly substituted_axes: string[];
    readonly unknown_condition_axes: string[];
    readonly inert: { axis: string; value: string }[];
  };
}

const FIXTURE_CASES = designFixtures.variance_cases as unknown as VarianceFixtureCase[];

/**
 * THE VARIANCE MODEL (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`) — named AXES whose values carry their
 * own concrete classes. These tests pin the two things the model must never lose: it EXPRESSES the
 * real design systems (button variant × size, badge variant × shape, avatar type × shape × size, a
 * structural boolean), and it keeps reading the FLAT format 173 stored components are written in.
 */
describe("parseAxes — the accepted shapes", () => {
  test("the CANONICAL object form: named axes, values carrying their classes", () => {
    const axes = parseAxes(
      JSON.stringify({
        axes: [
          {
            name: "variant",
            label: "Variante",
            default: "ghost",
            values: [
              { name: "primary", classes: "bg-primary text-primary-foreground" },
              { name: "ghost", classes: "bg-transparent hover:bg-accent" },
            ],
          },
          { name: "size", values: [{ name: "md", classes: "h-9 px-4" }, "lg"] },
        ],
      }),
    );
    expect(axes.map((a) => a.name)).toEqual(["variant", "size"]);
    expect(axes[0]?.label).toBe("Variante");
    // The declared `default` wins; without one it is the FIRST value.
    expect(axes[0]?.defaultValue).toBe("ghost");
    expect(axes[1]?.defaultValue).toBe("md");
    expect(axes[0]?.values[0]?.classes).toBe("bg-primary text-primary-foreground");
    // A bare string value is a value with no classes — the shorthand.
    expect(axes[1]?.values[1]).toEqual({ name: "lg", label: undefined, classes: undefined });
  });

  test("the bare ARRAY of axis objects — an entry with `values` IS an axis", () => {
    const axes = parseAxes(
      JSON.stringify([
        { name: "type", values: ["image", "initials"] },
        { name: "shape", values: ["circle", "square"] },
        { name: "size", values: ["sm", "md", "lg"] },
      ]),
    );
    // `avatar` — three orthogonal axes, and NOT 12 enumerated combinations.
    expect(axes.map((a) => a.name)).toEqual(["type", "shape", "size"]);
    expect(axisDefaults(axes)).toEqual({ type: "image", shape: "circle", size: "sm" });
  });

  test("the tolerated MAP form", () => {
    const axes = parseAxes(JSON.stringify({ variant: ["default", "outline"], shape: ["pill"] }));
    expect(axes.map((a) => a.name)).toEqual(["variant", "shape"]);
    expect(primaryAxisStates(axes)).toEqual(["default", "outline"]);
  });

  /**
   * RETRO-COMPATIBILITY — the whole point. A component stored in the FLAT format keeps its exact
   * previous behaviour: ONE anonymous axis, no classes, driven by `data-nyx-state-case`.
   */
  test("the LEGACY flat list — one anonymous axis, class-less, unchanged", () => {
    for (const blob of [
      '["default","destructive"]',
      '[{"name":"default"},{"name":"destructive"}]',
    ]) {
      const axes = parseAxes(blob);
      expect(axes).toHaveLength(1);
      expect(axes[0]?.name).toBe(LEGACY_AXIS);
      expect(axes[0]?.legacy).toBe(true);
      expect(primaryAxisStates(axes)).toEqual(["default", "destructive"]);
      expect(axes[0]?.values.every((v) => v.classes === undefined)).toBe(true);
    }
  });

  test("a malformed or empty blob declares NOTHING, and never throws", () => {
    expect(parseAxes(null)).toEqual([]);
    expect(parseAxes("")).toEqual([]);
    expect(parseAxes("nope")).toEqual([]);
    expect(parseAxes("42")).toEqual([]);
    expect(parseAxes('{"axes":[]}')).toEqual([]);
    // An axis with no usable value is not an axis.
    expect(parseAxes('{"axes":[{"name":"variant","values":[]}]}')).toEqual([]);
  });

  test("`classes` is tolerated as an array, and `class` as an alias", () => {
    const axes = parseAxes(
      JSON.stringify({
        axes: [
          {
            name: "v",
            values: [
              { name: "a", classes: ["h-9", "px-4"] },
              { name: "b", class: "  h-11   px-6 " },
            ],
          },
        ],
      }),
    );
    expect(axes[0]?.values.map((v) => v.classes)).toEqual(["h-9 px-4", "h-11 px-6"]);
  });
});

describe("the active combination", () => {
  const AXES = parseAxes(
    JSON.stringify({
      axes: [
        { name: "variant", values: ["primary", "destructive"] },
        { name: "size", values: ["md", "lg"] },
      ],
    }),
  );

  test("one value per axis, coerced to something the axis really declares", () => {
    expect(activeAxisValues(AXES, { variant: "destructive", size: "lg" })).toEqual({
      variant: "destructive",
      size: "lg",
    });
    // A stale / hand-written / missing value falls back to the axis default — never to nothing.
    expect(activeAxisValues(AXES, { variant: "gone", label: "x" })).toEqual({
      variant: "primary",
      size: "md",
    });
  });
});

describe("the structural mechanisms", () => {
  test("reads every `data-nyx-state-case`, both quotings, without duplicates", () => {
    const t =
      `<b data-nyx-state-case="default"></b><b data-nyx-state-case='ghost'></b>` +
      `<b data-nyx-state-case="default"></b>`;
    expect(templateStateCases(t)).toEqual(["default", "ghost"]);
    expect(templateStateCases(null)).toEqual([]);
  });

  /** The generalized `data-nyx-if`: a condition on an AXIS VALUE, not on a boolean. */
  test("parses data-nyx-if / if-not, with OR inside a clause and AND between clauses", () => {
    expect(templateConditions(`<span data-nyx-if="loading=on"></span>`)).toEqual([
      { axis: "loading", values: ["on"], negated: false },
    ]);
    expect(
      templateConditions(`<span data-nyx-if="variant=primary|ghost, size=lg"></span>`),
    ).toEqual([
      { axis: "variant", values: ["primary", "ghost"], negated: false },
      { axis: "size", values: ["lg"], negated: false },
    ]);
    expect(templateConditions(`<span data-nyx-if-not="variant=link"></span>`)).toEqual([
      { axis: "variant", values: ["link"], negated: true },
    ]);
    // Junk is ignored, never thrown on.
    expect(templateConditions(`<span data-nyx-if="loading"></span>`)).toEqual([]);
  });

  test("templateSubstitutes finds the axis-as-a-value escape hatch, and only it", () => {
    expect(templateSubstitutes(`<b class="btn btn-{{state}}"></b>`, "state")).toBe(true);
    expect(templateSubstitutes(`<b data-x="{{{ variant }}}"></b>`, "variant")).toBe(true);
    // A param that merely STARTS with the axis name is not the axis.
    expect(templateSubstitutes(`<b>{{stateLabel}}</b>`, "state")).toBe(false);
    expect(templateSubstitutes("", "state")).toBe(false);
  });

  test("names the first-axis values the template renders no case for (the EMPTY aperçu)", () => {
    const t = `<b data-nyx-state-case="default"></b><b data-nyx-state-case="ghost"></b>`;
    expect(statesWithoutCase(t, ["default", "ghost"])).toEqual([]);
    expect(statesWithoutCase(t, ["default", "outline", "link"])).toEqual(["outline", "link"]);
    // No case at all is the OTHER defect — `inertAxisValues` names it.
    expect(statesWithoutCase(`<b></b>`, ["default"])).toEqual([]);
  });

  test("names the conditions that point at an axis the component does not declare", () => {
    const axes = parseAxes('{"axes":[{"name":"loading","values":["off","on"]}]}');
    expect(conditionsOnUnknownAxes(axes, `<i data-nyx-if="loading=on"></i>`)).toEqual([]);
    expect(conditionsOnUnknownAxes(axes, `<i data-nyx-if="busy=yes"></i>`)).toEqual(["busy"]);
  });
});

/**
 * INERTIA — the generalization of "a declared state with no `data-nyx-state-case`" to the whole
 * model. It is what finally gives the declared BOOLEAN a verdict: two class-less values that gate
 * nothing ARE a decorative toggle, and nyx now says so.
 */
describe("inertAxisValues", () => {
  test("a value carrying CLASSES is alive — that is the new mechanism", () => {
    const axes = parseAxes(
      '{"axes":[{"name":"variant","values":[{"name":"a","classes":"bg-primary"},{"name":"b","classes":"bg-destructive"}]}]}',
    );
    expect(inertAxisValues(axes, `<button class="rounded">{{label}}</button>`)).toEqual([]);
  });

  test("a value with neither classes nor a case is INERT, and is named axis by value", () => {
    const axes = parseAxes(
      '{"axes":[{"name":"variant","values":[{"name":"a","classes":"bg-primary"},"b"]},{"name":"loading","values":["off","on"]}]}',
    );
    // `variant.b` is bare, but its sibling `a` carries classes: switching between them moves pixels,
    // so NEITHER is inert (finding `01KYT103MXTKFBZMKF968FGW9P`). `loading` carries none on either
    // value and nothing conditions on it — that is the real inert case, still reported.
    expect(inertAxisValues(axes, `<button class="rounded">{{label}}</button>`)).toEqual([
      { axis: "loading", value: "off" },
      { axis: "loading", value: "on" },
    ]);
  });

  test("a STRUCTURAL case rescues a class-less value — the spinner of loading=on", () => {
    const axes = parseAxes('{"axes":[{"name":"loading","values":["off","on"]}]}');
    // `on` is named by the condition; `off` is rescued too, because the SAME condition decides it:
    // the spinner exists at `on` and is gone at `off`, so switching really does change the pixels.
    expect(
      inertAxisValues(axes, `<button>{{label}}<i data-nyx-if="loading=on">…</i></button>`),
    ).toEqual([]);
    // `data-nyx-if-not` is the same condition written the other way round — it must not diverge.
    expect(
      inertAxisValues(axes, `<button><span data-nyx-if-not="loading=on">x</span></button>`),
    ).toEqual([]);
    expect(
      inertAxisValues(
        axes,
        `<button><i data-nyx-if="loading=on">…</i><span data-nyx-if-not="loading=on">x</span></button>`,
      ),
    ).toEqual([]);
  });

  /**
   * THE DISCRIMINATION RULE, the exact twin of the write-path validator's `condition_discriminates`
   * (finding `01KYSCY4J8JANZEJPYME86B1VH`). A condition CREDITS a value when it renders that value
   * differently from another value of the SAME axis — never merely when it NAMES it. Asking the
   * wrong question made the panel reproach the very shape the write path accepts.
   */
  describe("a condition credits what it DISCRIMINATES, not what it names", () => {
    /**
     * THE canonical component — NOT a copy of it. It comes from the shared fixture, which the Rust
     * guard `the_canonical_component_passes_the_checks_the_write_path_applies` runs through the
     * write path's own checks. The hand-typed copy that used to sit here had already drifted (two
     * variant values instead of three, different classes) without a single test noticing — finding
     * `01KYSNQWQD1324E0NA0172M66J`.
     */
    const CANONICAL_CASE = FIXTURE_CASES.find((c) => c.canonical)!;
    const CANONICAL_AXES = parseAxes(CANONICAL_CASE.states_json);

    test("the canonical component reports NOTHING", () => {
      expect(inertAxisValues(CANONICAL_AXES, CANONICAL_CASE.template)).toEqual([]);
    });

    /**
     * The REAL inert case is written with a boolean bare on BOTH sides: the canonical
     * `loading` no longer qualifies, because its `on` carries classes and those classes DISCRIMINATE
     * the bare `off` (finding `01KYT103MXTKFBZMKF968FGW9P`).
     */
    const BARE_BOOLEAN = parseAxes(
      JSON.stringify({
        axes: [
          {
            name: "variant",
            values: [
              { name: "primary", classes: "bg-primary" },
              { name: "outline", classes: "border" },
            ],
          },
          { name: "loading", values: [{ name: "off" }, { name: "on" }] },
        ],
      }),
    );

    test("the REAL inert case still bites: nothing conditions on the axis", () => {
      expect(
        inertAxisValues(BARE_BOOLEAN, `<button class="rounded-md">{{label}}</button>`),
      ).toEqual([
        { axis: "loading", value: "off" },
        { axis: "loading", value: "on" },
      ]);
    });

    test("a condition true at EVERY value of its axis credits nothing", () => {
      // `loading=off|on` is always true: the element is there whatever the axis does, which is the
      // same inertia as no condition at all.
      expect(
        inertAxisValues(
          BARE_BOOLEAN,
          `<button class="rounded-md"><i data-nyx-if="loading=off|on">◌</i>{{label}}</button>`,
        ),
      ).toEqual([
        { axis: "loading", value: "off" },
        { axis: "loading", value: "on" },
      ]);
    });

    test("a condition credits ITS OWN axis only", () => {
      const axes = parseAxes(
        JSON.stringify({
          axes: [
            { name: "variant", values: ["primary", "ghost"] },
            { name: "loading", values: [{ name: "off" }, { name: "on", classes: "opacity-70" }] },
          ],
        }),
      );
      // `loading` is credited twice over (a condition names it, AND one of its values carries
      // classes); neither credit reaches `variant`, whose two bare values nothing discriminates.
      expect(
        inertAxisValues(axes, `<button><i data-nyx-if="loading=on">◌</i>{{label}}</button>`),
      ).toEqual([
        { axis: "variant", value: "primary" },
        { axis: "variant", value: "ghost" },
      ]);
    });
  });

  /**
   * THE SAME DISCRIMINATION RULE, APPLIED TO THE OTHER MECHANISM (finding
   * `01KYT103MXTKFBZMKF968FGW9P`) — the exact twin of the write-path validator's
   * `classes_discriminate`. Observed on `checkbox`: `enabled` bare beside `disabled` carrying
   * `opacity-50` was reported inert, although it is the ABSENCE of `opacity-50` that DEFINES
   * `enabled`. The manual teaches that one never invents classes on a baseline to silence a warning,
   * and this false positive pushed towards exactly that.
   */
  describe("a sibling value's CLASSES credit a bare one, on their own axis", () => {
    test("the reported checkbox shape reports NOTHING", () => {
      const axes = parseAxes(
        '{"axes":[{"name":"disabled","values":[{"name":"enabled"},{"name":"disabled","classes":"opacity-50 pointer-events-none"}]}]}',
      );
      expect(
        inertAxisValues(
          axes,
          `<label class="inline-flex"><input type="checkbox"/>{{label}}</label>`,
        ),
      ).toEqual([]);
    });

    test("…and the WHOLLY bare axis is still reported — the warning stays useful", () => {
      const axes = parseAxes(
        '{"axes":[{"name":"disabled","values":[{"name":"enabled"},{"name":"disabled"}]}]}',
      );
      expect(
        inertAxisValues(
          axes,
          `<label class="inline-flex"><input type="checkbox"/>{{label}}</label>`,
        ),
      ).toEqual([
        { axis: "disabled", value: "enabled" },
        { axis: "disabled", value: "disabled" },
      ]);
    });

    test("the credit stops at the AXIS, never at the component", () => {
      const axes = parseAxes(
        '{"axes":[{"name":"variant","values":[{"name":"primary","classes":"bg-primary"},{"name":"ghost"}]},{"name":"loading","values":["off","on"]}]}',
      );
      expect(inertAxisValues(axes, `<button>{{label}}</button>`)).toEqual([
        { axis: "loading", value: "off" },
        { axis: "loading", value: "on" },
      ]);
    });
  });

  test("the LEGACY component: `data-nyx-state-case` still rescues the first axis", () => {
    const axes = parseAxes('["default","ghost"]');
    const t = `<b data-nyx-state-case="default"></b><b data-nyx-state-case="ghost"></b>`;
    expect(inertAxisValues(axes, t)).toEqual([]);
    // …and the degenerate one (no case, no class) reports every value — the original symptom.
    expect(inertAxisValues(axes, `<button class="bg-primary">{{label}}</button>`)).toEqual([
      { axis: "state", value: "default" },
      { axis: "state", value: "ghost" },
    ]);
    // The `{{state}}` substitution is the honest exception — the whole axis is alive.
    expect(inertAxisValues(axes, `<button class="btn btn-{{state}}">{{label}}</button>`)).toEqual(
      [],
    );
  });

  test("a component authored WITHOUT a template yet is judged on nothing", () => {
    expect(inertAxisValues(parseAxes('["a","b"]'), "")).toEqual([]);
    expect(inertAxisValues(parseAxes('["a","b"]'), null)).toEqual([]);
  });
});

/**
 * THE CROSS-LANGUAGE GUARD (finding `01KYSNQWQD1324E0NA0172M66J`). Every case of
 * `contract/design-fixtures.json` is replayed here AND, with the very same expectations, by
 * `mcp_tools_design.rs::tests::the_shared_fixture_pins_both_implementations_of_the_variance_rule`.
 * The two implementations of the variance rule can no longer drift apart in silence: a case that one
 * of them gets wrong fails a suite, and a case added to the fixture fails BOTH until both agree.
 */
describe("the shared Rust ⟷ TS fixture", () => {
  test("carries the cases the guard is worth having for", () => {
    expect(FIXTURE_CASES.length).toBeGreaterThanOrEqual(7);
    expect(FIXTURE_CASES.filter((c) => c.canonical)).toHaveLength(1);
  });

  for (const fixture of FIXTURE_CASES) {
    test(`${fixture.id} — ${fixture.why.split(".")[0]}`, () => {
      const axes = parseAxes(fixture.states_json);
      // The AXES, IN ORDER: the first one is the one `data-nyx-state-case` drives, so the order is
      // a verdict rather than a presentation detail.
      expect(axes.map((a) => a.name)).toEqual(fixture.expect.axes);
      expect(primaryAxisStates(axes)).toEqual(fixture.expect.primary_axis_values);
      expect(templateStateCases(fixture.template)).toEqual(fixture.expect.state_cases);
      expect(
        axes.filter((a) => templateSubstitutes(fixture.template, a.name)).map((a) => a.name),
      ).toEqual(fixture.expect.substituted_axes);
      expect(conditionsOnUnknownAxes(axes, fixture.template)).toEqual(
        fixture.expect.unknown_condition_axes,
      );
      // …and the DISCRIMINATION rule itself.
      expect(inertAxisValues(axes, fixture.template)).toEqual(fixture.expect.inert);
    });
  }
});
