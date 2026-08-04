import { Link } from "@tanstack/react-router";
import { ExternalLinkIcon, SlidersHorizontalIcon } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { errorText, nyxBridge } from "@/bridge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";

import { activeAxisValues, axisDefaults, parseAxes } from "../../design/axes";
import { AxisField } from "../../design/axis-field";
import { nodeContractIssue } from "../../design/composition-contract";
import { CssSlotCard, cssSlotOfComponent } from "../../design/css-slot-card";
import { NodeContractNotice } from "../../design/node-contract-notice";
import { ParamField } from "../../design/param-field";
import { defaultParamValues, editableParams, parseParams } from "../../design/params";
import type { DesignComponentRow } from "../../design/types";
import type { RenderedCssSlot } from "../../design/use-design-render";
import { compositionNodes } from "../composition-nodes";
import type { PrototypeVariant } from "../proto-types";

/**
 * `<NodeParamsPanel>` — EDIT ONE NODE of the composition on screen (finding
 * `01KYT10VE8624MKQ18AFHSE0W0`).
 *
 * # Where this comes from
 *
 * This capability used to live in the composant DETAIL (`/design/$componentId`), as a
 * « USED IN PROTOTYPES » list: every composition node instantiating that component, selectable,
 * pre-parameterized and saveable back. It made the detail speak of two different things at once —
 * the catalogue's vocabulary and the protos' placements — and it asked the user to go and fix a
 * composition from a page where the composition is not visible.
 *
 * The user ARBITRATED: move it, do not drop it. So it lives HERE, under the frame that renders the
 * very composition a Save writes into, and EVERYTHING followed:
 *
 *   * that node's OWN params — seeded from the component's `default` values, NEVER from its
 *     `preview` examples: a preview value is an illustration for a vignette, and this form is
 *     PERSISTED (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`). What the node itself carries wins over both;
 *   * the NODE CONTRACT at the READ ([`nodeContractIssue`]) — judged on what is STORED on the node,
 *     and at the WRITE (`proto_variant_set_node_params`'s own `warnings`, the backend's verbatim
 *     verdict on what it just persisted);
 *   * the per-node SAVE itself;
 *   * WHAT IS RENDERED, said out loud: the frame above renders the STORED composition, so an edit
 *     that is not saved is not in it. That is the same honesty the detail's `Showing` cartouche
 *     carried, restated for this surface.
 *
 * # THIS PANEL HAS NO PICKER OF ITS OWN (finding `01KZ3AN7VVAPXMYFQG4AP441BC`)
 *
 * It used to open with a ROW OF CHIPS, one per editable node, above the form — and the ELECTED
 * COMPOSITION tree right below listed THE SAME NODES. Two lists of one thing, in one pane. The
 * user's verdict: the chips go, the tree is what one clicks. So the panel is a FORM, never a list:
 * every selection reaches it as a {@link RequestedUsage} from something that already shows the
 * nodes — the RENDER itself (a click in the page), or the named tree below it. Nothing here
 * re-states what those two already display.
 *
 * `requestedUsage` keeps the outcome the deep-link was written for (finding
 * `01KYSNWNDG6HBFJX017RE203SZ`): a placement the composition on screen does not carry is NAMED —
 * never silently replaced by "nothing selected", which would let the user believe they are looking
 * at the node they clicked.
 *
 * # What a REFUSED save looks like
 *
 * A variant that is the FROZEN elected composition of a design-locked round is IMMUTABLE (PDR-0009:
 * one iterates by opening a NEW round). The backend refuses the write and says so, naming the locked
 * round and the cure; this panel prints that sentence VERBATIM rather than paraphrasing a rule it
 * does not own.
 */
export interface NodeParamsPanelProps {
  /** The variant whose composition is displayed — the one a Save writes into. `null` = nothing to edit. */
  variant: PrototypeVariant | null;
  /** The project's catalogue, already pulled by the tab (a node is editable iff its component is in it). */
  components: readonly DesignComponentRow[];
  /** Has the catalogue ANSWERED? An empty list that never loaded proves no absence. */
  componentsLoading: boolean;
  /**
   * The placement a caller asked to open — a click in the RENDER, or a node of the elected
   * composition tree. It is the ONLY way a node gets selected here: the panel shows no list of its
   * own (finding `01KZ3AN7VVAPXMYFQG4AP441BC`). `null` when nobody asked.
   */
  requestedUsage: RequestedUsage | null;
  /**
   * The SCOPED CSS SLOTS the render met (`design_render`'s `css_slots[]`) — the strict escape
   * hatches the page really carries, ONE per component. The selected node's own is displayed under
   * its params ([`CssSlotCard`]), because the fourth constraint of a slot is that it be SIGNALLED to
   * the human reviewing the page. Default `[]`: a page with no exception shows nothing.
   */
  cssSlots?: readonly RenderedCssSlot[];
  /** Re-pull the proto tree after a write (the panel never mutates its own props). */
  onSaved?: () => void;
  /**
   * The node this panel is bound to, whenever it changes (finding `01KYW32N1BVDVTGS0WPMG2PJW2`).
   *
   * The RENDER is the composition, so the selection has to be legible IN the render: the host rings
   * the node in the frame. The panel owns the selection (it is the one that knows which nodes are
   * editable), so it says what it selected rather than the host guessing from the request it sent —
   * a request can also resolve to nothing, and a stale ring on the previous node would then be a lie.
   */
  onSelectedChange?: (nodeKey: string | null) => void;
}

/**
 * A placement someone asked this panel to open. The IDENTITY of the object is the request: the tab
 * builds a new one per click, so clicking the SAME node twice re-binds it (and re-answers a "not
 * found" notice) instead of being swallowed by a value comparison.
 */
export interface RequestedUsage {
  /** The variant the placement lives in — the pair `(variantId, nodeKey)` is what names a usage. */
  readonly variantId: string;
  /** The placement's `node_key` PATH inside that variant's composition. */
  readonly nodeKey: string;
}

/** ONE editable node: its path, the component it instantiates, and the params STORED on it. */
interface EditableNode {
  readonly nodeKey: string;
  readonly component: DesignComponentRow;
  readonly params: Readonly<Record<string, unknown>>;
}

export function NodeParamsPanel({
  variant,
  components,
  componentsLoading,
  requestedUsage,
  cssSlots = [],
  onSaved,
  onSelectedChange,
}: NodeParamsPanelProps) {
  /**
   * The nodes of the DISPLAYED composition that can be parameterized: a node referencing a component
   * the catalogue knows. An `[inline]` node has no component, and a `[NEW]` one has no declaration to
   * build a form from — neither is hidden by accident, there is simply nothing to edit on them.
   */
  const editableNodes = useMemo<readonly EditableNode[]>(() => {
    const byName = new Map(components.map((c) => [c.name, c]));
    const out: EditableNode[] = [];
    for (const node of compositionNodes(variant?.composition_json)) {
      if (node.component === null) continue;
      const component = byName.get(node.component);
      if (component === undefined) continue;
      out.push({ nodeKey: node.nodeKey, component, params: node.params });
    }
    return out;
  }, [variant?.composition_json, components]);

  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [values, setValues] = useState<Record<string, unknown>>({});
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  /**
   * THE NODE CONTRACT as the WRITE saw it (finding `01KYQ0R9414Q5K63TBV9CCMC9J`): what
   * `proto_variant_set_node_params` answers for the node it just persisted. The read-side notice
   * judges what is STORED; this one judges what was just SAVED, and it is the authority — the same
   * backend owns the catalogue.
   */
  const [saveWarnings, setSaveWarnings] = useState<readonly string[]>([]);
  /** The `node_key` of a requested placement this composition does NOT carry — said, never swallowed. */
  const [missingUsage, setMissingUsage] = useState<string | null>(null);

  const selected = useMemo(
    () => editableNodes.find((n) => n.nodeKey === selectedKey) ?? null,
    [editableNodes, selectedKey],
  );

  // The SELECTED node's component declaration — the axes and params its form is made of.
  const axes = useMemo(() => parseAxes(selected?.component.states_json), [selected]);
  const descriptors = useMemo(
    () => parseParams(selected?.component.params_json ?? null, axes),
    [selected, axes],
  );
  const fields = useMemo(() => editableParams(descriptors), [descriptors]);
  const combination = useMemo(() => activeAxisValues(axes, values), [axes, values]);

  /**
   * THE NODE CONTRACT at the READ — for the SELECTED node, judged on what is STORED on it (not on the
   * live form): the keys it carries that the component declares nowhere, and the `required` params it
   * does not provide. That is exactly the mismatch that used to show up only as an empty rendering.
   * The WHOLE-composition report stays where the whole composition is (the tab's own notice, above
   * the frame); this one is beside the node it judges.
   */
  const issue = useMemo(() => {
    if (selected === null) return null;
    return nodeContractIssue(selected.nodeKey, selected.params, {
      name: selected.component.name,
      params_json: selected.component.params_json,
      states_json: selected.component.states_json,
    });
  }, [selected]);

  /**
   * THE SCOPED CSS EXCEPTION the selected node lands on — its COMPONENT's, resolved by name, which
   * is the same relation `ElectedNode.css_scope` expresses as a pointer. The node never owns one:
   * this panel edits a PLACEMENT, and a slot is a property of the component behind it, so the card
   * says whose it is and offers no control (finding `01KYVVEQZW2Y2VY7ZAPK5RECZS`).
   */
  const slot = useMemo(
    () => cssSlotOfComponent(cssSlots, selected?.component.name),
    [cssSlots, selected],
  );

  /**
   * Bind a node → seed the form with the params of THAT placement.
   *
   * The base is the component's DEFAULTS, never its `preview` examples (finding
   * `01KYPJ1RC6WJX1Z9CPPV23FG27`): a preview value exists to make a vignette readable, and this form
   * is written back into a composition — so an example must never be able to reach a proto, nor,
   * through it, the coder seam. The node's own params win over both.
   */
  const selectNode = useCallback(
    (node: EditableNode) => {
      const nodeAxes = parseAxes(node.component.states_json);
      const nodeDescriptors = parseParams(node.component.params_json, nodeAxes);
      setSelectedKey(node.nodeKey);
      onSelectedChange?.(node.nodeKey);
      setSaveError(null);
      setSaveWarnings([]);
      // An explicit pick answers a pending "usage introuvable": the user chose what they look at.
      setMissingUsage(null);
      setValues({
        ...defaultParamValues(nodeDescriptors),
        ...axisDefaults(nodeAxes),
        ...node.params,
      });
    },
    [onSelectedChange],
  );

  /**
   * A NEW composition on screen (another page, another elected variant, a rollback) is another set
   * of placements: the selection cannot survive it, and neither can a form seeded from a node that is
   * no longer there.
   */
  const variantId = variant?.id ?? null;
  useEffect(() => {
    setSelectedKey(null);
    onSelectedChange?.(null);
    setValues({});
    setSaveError(null);
    setSaveWarnings([]);
    setMissingUsage(null);
  }, [variantId, onSelectedChange]);

  /**
   * …and BINDING the placement a caller named (finding `01KYSNWNDG6HBFJX017RE203SZ`, moved here by
   * `01KYT10VE8624MKQ18AFHSE0W0`). Handled ONCE per REQUEST — by object identity, so the same node
   * clicked twice is honoured twice, while a re-render (the tree re-pulls on every
   * `prototypes://changed`) never re-imposes a stale ask over the user's own selection.
   *
   * NOT FOUND IS AN OUTCOME, NOT A FALLBACK. Leaving the panel on whatever was selected — or on
   * nothing — while the caller asked for a placement is the very defect the deep-link fixes, so a
   * placement this composition does not carry is NAMED. Only a catalogue that ANSWERED proves an
   * absence: while it loads, every node looks un-editable, so the verdict waits.
   */
  const handledRequest = useRef<RequestedUsage | null>(null);
  useEffect(() => {
    if (requestedUsage === null || handledRequest.current === requestedUsage) return;
    if (variantId === requestedUsage.variantId) {
      const target = editableNodes.find((n) => n.nodeKey === requestedUsage.nodeKey);
      if (target !== undefined) {
        handledRequest.current = requestedUsage;
        selectNode(target);
        return;
      }
    }
    // The composition is here (it comes with the tree); only the catalogue can still be pending.
    if (componentsLoading) return;
    handledRequest.current = requestedUsage;
    setSelectedKey(null);
    onSelectedChange?.(null);
    setMissingUsage(requestedUsage.nodeKey);
  }, [requestedUsage, variantId, editableNodes, componentsLoading, selectNode, onSelectedChange]);

  const setParam = useCallback((name: string, value: unknown) => {
    setValues((prev) => (Object.is(prev[name], value) ? prev : { ...prev, [name]: value }));
  }, []);

  const save = useCallback(async () => {
    if (selected === null || variant === null) return;
    setSaving(true);
    setSaveError(null);
    setSaveWarnings([]);
    try {
      // The write ANSWERS with the node contract it checked: a key the component declares nowhere, a
      // `required` param the node does not provide. A warning never fails the save — the edit is
      // persisted either way — it says what will render empty instead of letting the frame go blank
      // in silence.
      const res = await nyxBridge.invoke<{ warnings?: string[] }>("proto_variant_set_node_params", {
        variantId: variant.id,
        nodeKey: selected.nodeKey,
        paramsJson: JSON.stringify(values),
      });
      setSaveWarnings(res.warnings ?? []);
      onSaved?.();
    } catch (e: unknown) {
      // VERBATIM — a refusal (a FROZEN design lock) names the locked round AND the cure, and this
      // surface owns neither.
      setSaveError(errorText(e));
    } finally {
      setSaving(false);
    }
  }, [selected, variant, values, onSaved]);

  return (
    <section data-testid="node-params-panel" aria-label="Node params" className="flex flex-col">
      {/* NO COUNT BADGE: it counted the chip row that used to sit under this heading, and that row
          is gone — the tree below is the list, and it counts itself. */}
      <div className="mb-2 flex items-center gap-2">
        <SlidersHorizontalIcon aria-hidden className="size-4 text-muted-foreground" />
        <h3 className="font-heading text-sm font-semibold text-foreground">Node params</h3>
      </div>

      {/* THE PLACEMENT THAT IS NOT THERE — a caller (the elected composition below) named a node this
          composition does not carry. Selecting nothing in silence would repeat the very defect the
          deep-link was written to fix, so the gap is named where the node would have been. */}
      {missingUsage !== null ? (
        <p
          data-testid="usage-not-found"
          className="mb-2 rounded-md border border-warning/30 bg-warning/5 px-2.5 py-1.5 text-2xs leading-relaxed text-warning"
        >
          The node <code className="font-mono">{missingUsage}</code> is not among the editable nodes
          of the composition on screen — it left this composition, the variant it lived in is not
          the one displayed, or the component it references is not in the catalogue. Nothing is
          bound: what is edited below is NOT that placement.
        </p>
      ) : null}

      {/* NOTHING BOUND YET. The two states are DIFFERENT verdicts and are not merged: a composition
          with no catalogue node has nothing to edit at all, while one with editable nodes is simply
          waiting to be pointed at. The second is one short line — the panel says WHERE to click and
          stops; it does not lecture about what a placement is. */}
      {editableNodes.length === 0 ? (
        <EmptyState variant="plain">
          This composition instantiates no catalogue component — there are no node params to edit
          here.
        </EmptyState>
      ) : selected === null ? (
        <EmptyState variant="plain">
          <span data-testid="node-params-pick-cue">
            Click a node — in the rendering, or in the composition below.
          </span>
        </EmptyState>
      ) : null}

      {selected !== null && variant !== null ? (
        <div className="mt-2 rounded-lg border border-border/50 bg-muted/10 p-2.5">
          {/* WHAT IS BEING EDITED, AND WHAT IS RENDERED — the two are not the same thing, and saying
              so is the whole point of this line: the frame above renders the composition AS STORED,
              so an unsaved edit is in this form and nowhere else. */}
          {/* IT NAMES THE NODE, AND NOTHING THE SCREEN ALREADY NAMES (finding
              `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`). It used to end with "as composed in <page> / <variant>",
              which restated the page title the header carries and the variant the strip selects and
              the composition tree badges — the page's title reached four places on one screen, and
              this was one of them. */}
          <p data-testid="node-edit-scope" className="mb-2 min-w-0 text-2xs text-muted-foreground">
            Editing <code className="font-mono text-foreground/80">{selected.nodeKey}</code> ·{" "}
            {selected.component.name}. The preview above renders this composition as STORED: save to
            see the edit there.
          </p>

          {/* THE WAY OUT TO THE COMPONENT ITSELF (finding `01KYW32N1BVDVTGS0WPMG2PJW2`). A node is a
              PLACEMENT of a catalogue component: this form edits the placement, and everything that
              belongs to the component — its template, its axes, its params declaration, its CSS
              exception — lives on its detail page. Without this link the reviewer who has just
              found the culprit by clicking it in the render has no way to reach the thing to fix. */}
          <Link
            to="/design/$componentId"
            params={{ componentId: selected.component.id }}
            data-testid="node-component-link"
            className="mb-2 inline-flex items-center gap-1.5 text-2xs text-info underline-offset-4 hover:underline"
          >
            <ExternalLinkIcon aria-hidden className="size-3" />
            {/* The component is NAMED one line above; the link only has to say where it goes. */}
            Open in the catalogue
          </Link>

          {/* What the SELECTED node owes its component and does not give it — beside the node it
              judges, on what is STORED (the whole-composition report lives above the frame). */}
          {issue !== null ? (
            <div className="mb-2">
              <NodeContractNotice report={{ issues: [issue], total: 1, truncated: false }} />
            </div>
          ) : null}

          {/* THE SCOPED CSS EXCEPTION this placement lands on — visible in review, which is the
              fourth constraint of a slot and not a nicety of a screen. Its owner is the component,
              and the card says so; a stored slot that no longer compiles shows as a NAMED refusal
              rather than disappearing. */}
          {slot !== null ? (
            <div className="mb-2">
              <CssSlotCard slot={slot} />
            </div>
          ) : null}

          {axes.length > 0 ? (
            <div className="mb-3 space-y-3">
              {axes.map((axis) => (
                <AxisField
                  key={axis.name}
                  axis={axis}
                  value={combination[axis.name] ?? axis.defaultValue}
                  onChange={(v) => setParam(axis.name, v)}
                />
              ))}
            </div>
          ) : null}

          {fields.length === 0 ? (
            <EmptyState variant="plain">
              {selected.component.name} declares no params — nothing to set on this node.
            </EmptyState>
          ) : (
            <div className="space-y-3">
              {fields.map((d) => (
                <ParamField
                  key={d.name}
                  descriptor={d}
                  value={values[d.name]}
                  onChange={(v) => setParam(d.name, v)}
                />
              ))}
            </div>
          )}

          <div className="mt-3 space-y-1.5">
            <Button type="button" onClick={() => void save()} disabled={saving} className="w-full">
              {saving ? "Saving…" : "Save to node"}
            </Button>
            {saveError ? (
              <p data-testid="node-save-error" className="text-2xs text-destructive">
                {saveError}
              </p>
            ) : null}
            {/* The WRITE's own verdict on the node it just persisted — the save succeeded, and this
                says what the saved params still owe the component. */}
            {saveWarnings.map((warning) => (
              <p
                key={warning}
                data-testid="node-contract-save-warning"
                className="rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-1.5 text-2xs leading-relaxed text-destructive/90"
              >
                {warning}
              </p>
            ))}
          </div>
        </div>
      ) : null}
    </section>
  );
}
