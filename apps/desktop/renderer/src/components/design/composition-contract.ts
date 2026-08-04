/**
 * THE NODE CONTRACT — a composition NODE confronted with the component it REFERENCES (finding
 * `01KYQ0R9414Q5K63TBV9CCMC9J`).
 *
 * # The defect this closes
 *
 * A node's keys were confronted with NOTHING. A button node carrying `text: "Reset filters"` and
 * `props.variant`, referencing a component that declares `buttonLabel` + a `state` axis, rendered
 * EMPTY — and nothing anywhere said why. Silence on invalid data, which is the same trap as every
 * other one of this round: the composition looks written, the aperçu looks broken, and the two facts
 * are never connected.
 *
 * # The two defects, kept apart
 *
 * Their corrections are opposite, so they are never merged into one message:
 *
 *   * an UNKNOWN KEY — the node PROVIDES a key the component declares neither as a param
 *     ([`./params`]) nor as an axis ([`./axes`]). Nothing substitutes it: the value is dropped.
 *     The fix is to RENAME it (or to declare it).
 *   * a MISSING REQUIRED param — the component marks it `required`, meaning it carries no value of
 *     its own, and the node does not provide one. The fix is to PROVIDE it (or to admit the
 *     component really has a fallback and drop `required`).
 *
 * # What it deliberately does NOT do
 *
 * It never maps a name onto a neighbouring one. `text` → `buttonLabel` is a GUESS, and a confident
 * wrong value is worse than the silence it replaces. It names the mismatch and lets a human or the
 * agent correct it.
 *
 * A component that declares NOTHING (no params, no axes — a `[NEW]` authored from its name alone, or
 * a row awaiting its synthesis) has no contract to confront: it yields no issue at all rather than
 * flagging every key of every node that uses it.
 *
 * This mirrors the Rust `nyx_core::mcp_tools_design::node_contract_warnings` /
 * `composition_contract_warnings` (the WRITE-path half of the same check) rule for rule, so the
 * surface and the tools can never disagree about what a node owes its component. Everything here is
 * PURE and DEFENSIVE — a malformed blob yields an empty result, never a throw.
 */

import { compositionNodes } from "../specs/composition-nodes";
import { parseAxes } from "./axes";
import { parseParams } from "./params";

/** The catalogue rows this module needs of a component — its DECLARED params and axes. */
export interface ComponentContract {
  readonly name: string;
  readonly params_json: string | null;
  readonly states_json: string | null;
}

/** What ONE node gets wrong against its component — the two defects, side by side and distinct. */
export interface NodeContractIssue {
  /** The `/`-joined node path (`Root/Filters/Reset`) — the node the user/agent must open. */
  readonly nodeKey: string;
  /** The component the node references. */
  readonly component: string;
  /** Keys the node provides that the component declares NOWHERE (neither a param nor an axis). */
  readonly unknownKeys: readonly string[];
  /** `required` params of the component the node does not provide (it renders EMPTY). */
  readonly missingRequired: readonly string[];
  /** Everything the component DOES declare — what a rename must land on. */
  readonly declaredKeys: readonly string[];
}

/**
 * How many mismatching NODES are listed before the report stops naming them. A 300-node composition
 * where every node is wrong must not turn the surface into a wall of the same sentence — the same
 * bound the Rust side applies (`MAX_NODE_CONTRACT_WARNINGS`), for the same reason.
 *
 * IT BOUNDS **NODES**, and both sides say so now (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 3).
 * The Rust half carried the same 12 while bounding MESSAGES, so on a composition where every node
 * has two defects the agent was told about six nodes and the human saw twelve — one surface, two
 * different sets of broken nodes. `contract/design-fixtures.json` (`node_contract`) pins which nodes
 * are named and both suites replay it.
 */
export const MAX_NODE_CONTRACT_ISSUES = 12;

/** The renderer injects `children` itself — it is never a key the component has to declare. */
const RESERVED_KEY = "children";

/**
 * Does the node PROVIDE a usable value for this key? Absent, `null`, `undefined` and `""` are all
 * "not provided": a required param resolved to the empty string renders exactly like an omitted
 * one — nothing — and that is the defect being named.
 */
function provides(params: Readonly<Record<string, unknown>>, name: string): boolean {
  const value = params[name];
  if (value == null) return false;
  return value !== "";
}

/**
 * CONFRONT one node's keys with a component's declared params + axes. Returns `null` when the node
 * honours the contract, or when the component declares nothing to honour.
 */
export function nodeContractIssue(
  nodeKey: string,
  params: Readonly<Record<string, unknown>>,
  component: ComponentContract,
): NodeContractIssue | null {
  const axes = parseAxes(component.states_json);
  const descriptors = parseParams(component.params_json, axes);
  if (axes.length === 0 && descriptors.length === 0) return null;

  const declaredKeys: string[] = [];
  for (const name of [...descriptors.map((d) => d.name), ...axes.map((a) => a.name)]) {
    if (!declaredKeys.includes(name)) declaredKeys.push(name);
  }
  const unknownKeys = Object.keys(params).filter(
    (k) => k !== RESERVED_KEY && !declaredKeys.includes(k),
  );
  const missingRequired = descriptors
    .filter((d) => d.required === true && !provides(params, d.name))
    .map((d) => d.name);
  if (unknownKeys.length === 0 && missingRequired.length === 0) return null;
  return { nodeKey, component: component.name, unknownKeys, missingRequired, declaredKeys };
}

/** The BOUNDED report of a whole composition — the issues named, plus how many were left out. */
export interface CompositionContractReport {
  /** At most [`MAX_NODE_CONTRACT_ISSUES`] mismatching nodes, in placement order. */
  readonly issues: readonly NodeContractIssue[];
  /** How many mismatching nodes there are in total (≥ `issues.length`). */
  readonly total: number;
  /** `total > issues.length` — the report stopped naming them. */
  readonly truncated: boolean;
}

/**
 * CONFRONT a whole `composition_json` with the catalogue — the READ half of the signal, so a
 * composition that renders empty SAYS why while the user is looking at it. Depth-first from the
 * root, so the report reads in placement order.
 *
 * A node referencing a component ABSENT from the catalogue is not a mismatch: that is a `[NEW]`, and
 * the composition already reports it as a build candidate.
 */
export function compositionContractReport(
  compositionJson: string | null | undefined,
  catalogue: readonly ComponentContract[],
): CompositionContractReport {
  const byName = new Map(catalogue.map((c) => [c.name, c]));
  const issues: NodeContractIssue[] = [];
  let total = 0;
  for (const node of compositionNodes(compositionJson)) {
    if (node.component === null) continue;
    const component = byName.get(node.component);
    if (component === undefined) continue;
    const issue = nodeContractIssue(node.nodeKey, node.params, component);
    if (issue === null) continue;
    total += 1;
    if (issues.length < MAX_NODE_CONTRACT_ISSUES) issues.push(issue);
  }
  return { issues, total, truncated: total > issues.length };
}
