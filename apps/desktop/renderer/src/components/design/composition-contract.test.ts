import { describe, expect, it } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";

import {
  compositionContractReport,
  MAX_NODE_CONTRACT_ISSUES,
  nodeContractIssue,
} from "./composition-contract";

/**
 * THE NODE CONTRACT (finding `01KYQ0R9414Q5K63TBV9CCMC9J`) — a composition node's keys confronted
 * with the params AND axes the component it references declares. The reported shape: a button node
 * carrying `text: "Reset filters"` for a component wanting `buttonLabel` + a `variant` axis. It
 * rendered EMPTY, and nothing anywhere said why.
 */

/** The catalogue `Button`: `buttonLabel` is REQUIRED, `variant` is an AXIS. Neither is `text`. */
const BUTTON = {
  name: "Button",
  params_json: JSON.stringify([{ name: "buttonLabel", type: "text", required: true }]),
  states_json: JSON.stringify({
    axes: [{ name: "variant", values: [{ name: "ghost", classes: "bg-transparent" }] }],
  }),
};

describe("nodeContractIssue", () => {
  it("names an UNKNOWN key and a MISSING REQUIRED param as two distinct defects", () => {
    const issue = nodeContractIssue("Root/Filters/Reset", { text: "Reset filters" }, BUTTON);
    expect(issue).not.toBeNull();
    // (1) a key the component declares NOWHERE — the value is dropped.
    expect(issue?.unknownKeys).toEqual(["text"]);
    // (2) a REQUIRED param the node does not provide — the node renders empty.
    expect(issue?.missingRequired).toEqual(["buttonLabel"]);
    // …and what a rename must land on is listed: params first, then the axes.
    expect(issue?.declaredKeys).toEqual(["buttonLabel", "variant"]);
  });

  it("accepts an AXIS as a node key, and never counts the injected `children`", () => {
    expect(
      nodeContractIssue(
        "Root",
        { buttonLabel: "Reset filters", variant: "ghost", children: "<i/>" },
        BUTTON,
      ),
    ).toBeNull();
  });

  it("treats a required param provided EMPTY exactly like an omitted one", () => {
    const issue = nodeContractIssue("Root", { buttonLabel: "" }, BUTTON);
    expect(issue?.missingRequired).toEqual(["buttonLabel"]);
    expect(issue?.unknownKeys).toEqual([]);
  });

  it("says NOTHING about a component that declares nothing (a [NEW] with no synthesis yet)", () => {
    expect(
      nodeContractIssue(
        "Root",
        { anything: 1 },
        { name: "Fresh", params_json: null, states_json: null },
      ),
    ).toBeNull();
  });

  it("reads the LEGACY flat states_json as the `state` axis, so a legacy node stays valid", () => {
    const legacy = {
      name: "Badge",
      params_json: JSON.stringify([{ name: "label", type: "text" }]),
      states_json: JSON.stringify(["default", "destructive"]),
    };
    expect(nodeContractIssue("Root", { label: "x", state: "destructive" }, legacy)).toBeNull();
  });
});

describe("compositionContractReport", () => {
  it("walks the tree in placement order and ignores a node whose component is not catalogued", () => {
    const composition = JSON.stringify({
      key: "Root",
      children: [
        { component: "Button", key: "Reset", params: { text: "Reset filters" } },
        // A `[NEW]` — absent from the catalogue, so NOT a mismatch (it is a build candidate).
        { component: "NotCatalogued", key: "Soon", params: { whatever: 1 } },
        { component: "Button", key: "Apply", params: { buttonLabel: "Apply", variant: "ghost" } },
      ],
    });
    const report = compositionContractReport(composition, [BUTTON]);
    expect(report.total).toBe(1);
    expect(report.truncated).toBe(false);
    expect(report.issues[0]?.nodeKey).toBe("Root/Reset");
    expect(report.issues[0]?.component).toBe("Button");
  });

  it("stays BOUNDED on a big composition and counts what it stopped naming", () => {
    const children = Array.from({ length: 200 }, (_, i) => ({
      component: "Button",
      key: `b${i}`,
      params: { text: "x" },
    }));
    const report = compositionContractReport(JSON.stringify({ key: "Root", children }), [BUTTON]);
    expect(report.issues.length).toBe(MAX_NODE_CONTRACT_ISSUES);
    expect(report.total).toBe(200);
    expect(report.truncated).toBe(true);
  });

  /**
   * THE ARRIMAGE of WHAT THE CAP BOUNDS (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 3).
   *
   * This check is written twice — here for the HUMAN, and
   * `mcp_tools_design::composition_contract_warnings` for the AGENT — and both carried a cap of 12
   * while bounding two DIFFERENT things: NODES here, MESSAGES there. On a composition where every
   * node has two defects, the agent was told about six nodes and the human saw twelve: the two
   * halves of one surface naming two different sets of broken nodes, with nothing to say so.
   *
   * The fixture pins WHICH nodes are named, and Rust replays the same case in
   * `mcp_tools_design::tests::the_shared_fixture_pins_what_the_node_contract_cap_bounds`.
   */
  it("names the nodes the shared fixture pins — the cap bounds NODES, on both sides", () => {
    const fixture = designFixtures.node_contract;
    expect(MAX_NODE_CONTRACT_ISSUES).toBe(fixture.max_nodes);

    const { case: testCase } = fixture;
    const component = {
      name: testCase.component.name,
      params_json: testCase.component.params_json,
      states_json: testCase.component.states_json,
    };
    const composition = JSON.stringify({
      key: testCase.root_key,
      children: testCase.nodes.map((key) => ({
        component: component.name,
        key,
        params: testCase.node_params,
      })),
    });

    const report = compositionContractReport(composition, [component]);
    expect(
      report.issues.map((i) => i.nodeKey),
      testCase.why,
    ).toEqual(testCase.expected_named_nodes);
    expect(report.total).toBe(testCase.expected_total);
    expect(report.truncated).toBe(testCase.expected_truncated);
    // A NAMED node is named WHOLE — both of its defects, which is what makes bounding NODES rather
    // than messages the honest cap.
    for (const issue of report.issues) {
      expect(issue.unknownKeys).toEqual(testCase.expected_unknown_keys);
      expect(issue.missingRequired).toEqual(testCase.expected_missing_required);
    }
  });

  it("never throws on a null/malformed composition", () => {
    expect(compositionContractReport(null, [BUTTON]).issues).toEqual([]);
    expect(compositionContractReport("not json", [BUTTON]).issues).toEqual([]);
  });
});
