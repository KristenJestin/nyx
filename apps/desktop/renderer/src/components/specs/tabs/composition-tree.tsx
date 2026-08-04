import { ChevronRightIcon, PackageIcon, ZapIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import type { RenderedCssSlot } from "../../design/use-design-render";
import { componentNature, type DesignComponentRow } from "../../design/types";
import { compositionNodes, nodeKeyDepth, nodeKeyLeaf } from "../composition-nodes";
import { NODE_NATURE_DS, NODE_NATURE_INLINE, NODE_NATURE_NEW } from "../proto-types";

/**
 * `<CompositionTree>` — the NAMED TREE of the composition ON SCREEN: the page displayed, in the
 * variant displayed, node by node, each classified by its nature.
 *
 * # It used to describe something else entirely (finding `01KZ3JN5KJQWJTV2XNT604CWCT`, CRITICAL)
 *
 * This pane read `proto_composition` — the FROZEN elected composition the coder seam exposes off the
 * design LOCK. That read model answers, per page pinned in the LOCKED round, the composition of the
 * variant elected IN THAT ROUND. None of those three things is what the stage shows:
 *
 *  - not the same VARIANT — the stage shows whatever the variant strip has selected, the seam shows
 *    what was elected when the design was frozen. So the tree did not move one row when the reviewer
 *    went from variant A to B to C, and it listed components (`content-card`, `table-empty-state`)
 *    the rendering never names;
 *  - not the same page VERSION — the frozen round pins a version, the stage displays the one the
 *    rail resolves, so a header reading `v2` sat over a tree describing `v1`;
 *  - not necessarily the same PAGE SET — a page the frozen round does not pin has no entry at all,
 *    which is how the second page of a prototype got an empty pane and the sentence "This page is
 *    not pinned in the frozen round" WHILE the rail ticked it retained and the strip badged it
 *    `Retained by default` (finding `01KZ3JPVDT7RXEC3P9PVSJ95CN`). Two of those three said the
 *    truth: the page IS retained in the CURRENT round; the pane was reading the FROZEN one and
 *    talking about it as if the reviewer had asked.
 *
 * The visible casualty was `Open`: it named a node of a composition the params editor was not
 * showing, so the editor answered — correctly — that the node is not among the editable ones. The
 * warning was never the defect; this tree was. So the tree is derived HERE, from the very
 * `composition_json` the stage renders, and the two cannot drift: there is one composition on this
 * screen now, not two.
 *
 * # OPENABLE IS EDITABLE, by construction
 *
 * A row offers `Open` exactly when {@link NodeParamsPanel} can bind it — both answer the same
 * question, "does the catalogue carry this node's component?", off the same catalogue rows. A tree
 * that proposes a node the form cannot open is the defect above, so the affordance is not a second
 * judgement: it is the same one.
 *
 * # The natures are the coder seam's, to the letter
 *
 * `[DS]` / `[NEW]` / `[inline]` are classified exactly as `nyx_core::db::classify_elected_nodes`
 * does, so what a reviewer reads here and what a coder later receives cannot disagree — including
 * the rule that matters most (review #41): the nature is the component's ORIGIN, not its presence in
 * the catalogue. A greenfield component the agent AUTHORED has a catalogue row and no `source_path`,
 * and it is still `[NEW]` — it still has to be built. A structural node (no component, no inline
 * markup — it only groups children) carries none of the three natures and is omitted, as it is
 * there.
 */

/**
 * The node's own name and its placement DEPTH are READ OFF the `node_key` path — so both come from
 * the module that OWNS that convention, never from a `split("/")` re-typed here (finding
 * `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 2). The separator is written once, in one place, and
 * `contract/design-fixtures.json` (`node_key`) pins it against the Rust half.
 */
const leafLabel = nodeKeyLeaf;
const nodeDepth = nodeKeyDepth;

type NatureVariant = "muted" | "warning" | "source";

/**
 * The NATURE label + its Badge variant — the 3 PDR natures, each a DISTINCT signifier:
 *  - `[DS]` (muted) — a component that EXISTS in the app (carries a `componentRef` → clickable);
 *  - `[NEW]` (warning) — a component that must be BUILT: a candidate to promote;
 *  - `[inline]` (source) — bespoke one-off markup (not a component).
 */
function natureBadge(nature: string): { label: string; variant: NatureVariant } {
  switch (nature) {
    case NODE_NATURE_DS:
      return { label: "[DS]", variant: "muted" };
    case NODE_NATURE_NEW:
      return { label: "[NEW]", variant: "warning" };
    default:
      return { label: "[inline]", variant: "source" };
  }
}

/**
 * OPENING a node = opening THAT PLACEMENT (finding `01KYSNWNDG6HBFJX017RE203SZ`). What names a
 * placement is the PAGE it belongs to, the VARIANT on screen and the node's own `node_key` — a
 * component id alone would only ever open the generic preview, which is not what one asks for when
 * clicking a node of the tree.
 *
 * WHERE IT LANDS moved (finding `01KYT10VE8624MKQ18AFHSE0W0`): this used to NAVIGATE to
 * `/design/$componentId?variant=&node=`, because the per-node editing lived in the composant detail.
 * It lives in the PROTOTYPE page now — the very surface this view is embedded in — so the gesture no
 * longer leaves the page: the host selects the node in its own editor. `componentId` travels along
 * because it is what makes a node openable at all.
 */
export type OpenNode = (target: {
  pageId: string;
  componentId: string;
  variantId: string | null;
  nodeKey: string;
}) => void;

/** ONE node of the shown composition, classified — the renderer's twin of `db::ElectedNode`. */
export interface CompositionTreeNode {
  /** The `/`-joined node path (`Root/Filters/Reset`) — the anchor every other surface names it by. */
  readonly nodeKey: string;
  /** `ds` | `new` | `inline` — the 3 PDR natures. */
  readonly nature: string;
  /** The component NAME this node instantiates (`ds`/`new`); `null` for an `inline` node. */
  readonly component: string | null;
  /** The catalogue row id — what makes the node openable; `null` when the catalogue carries none. */
  readonly componentRef: string | null;
  /** The SCOPE ID of the CSS exception this node's component carries; `null` for nearly every node. */
  readonly cssScope: string | null;
}

/**
 * CLASSIFY the composition ON SCREEN into its nodes — pure, total, and keyed on nothing but the
 * composition, the catalogue and the exceptions the render itself reported.
 *
 * The CSS scope is READ OFF THE RENDER (`cssSlots[].component` → `scope_id`) rather than re-derived:
 * the sidecar decided which components carry an exception and stamped `data-nyx-css` in the markup
 * for exactly those, so taking its answer is what keeps the mark in the frame and the flag on the
 * row talking about the same thing. A slot belongs to the COMPONENT — every placement of it points
 * at the one entry, none of them owns it (finding `01KYVMTWB71R0D0CT1SV88J8DX`).
 */
export function compositionTreeNodes(
  compositionJson: string | null,
  components: readonly DesignComponentRow[],
  cssSlots: readonly RenderedCssSlot[] = [],
): CompositionTreeNode[] {
  const byName = new Map(components.map((c) => [c.name, c]));
  const scopeByComponent = new Map(cssSlots.map((s) => [s.component, s.scope_id]));
  const out: CompositionTreeNode[] = [];
  for (const node of compositionNodes(compositionJson)) {
    if (node.inline) {
      out.push({
        nodeKey: node.nodeKey,
        nature: NODE_NATURE_INLINE,
        component: null,
        componentRef: null,
        cssScope: null,
      });
      continue;
    }
    // A STRUCTURAL node carries none of the 3 natures — it only groups children, and its placement
    // lives in the tree's shape. Omitted, exactly as the coder seam omits it.
    if (node.component === null) continue;
    const row = byName.get(node.component) ?? null;
    out.push({
      nodeKey: node.nodeKey,
      // The ORIGIN, not the catalogue membership: an authored greenfield row is still `[NEW]`.
      nature: row !== null && componentNature(row) === "ds" ? NODE_NATURE_DS : NODE_NATURE_NEW,
      component: node.component,
      componentRef: row?.id ?? null,
      cssScope: scopeByComponent.get(node.component) ?? null,
    });
  }
  return out;
}

/**
 * ONE node row of the named tree (= placement): indented by its path depth, its NATURE badge, its
 * name. A node whose component the catalogue carries is a BUTTON that OPENS that placement's params;
 * a `[NEW]` with no catalogue entry, and an `[inline]` node, are plain rows — there is no form to
 * open on them.
 *
 * THE ROW SAYS EACH THING ONCE (finding `01KZ3AN7VVAPXMYFQG4AP441BC`). It used to carry a "To build"
 * hint beside the `[NEW]` badge, and the page block repeated the same names under a "To build"
 * heading below the tree — the same fact three times within a few centimetres. `[NEW]` IS "not built
 * yet, someone has to build it": the badge is the signifier, so the gloss and the recap are gone.
 */
function NodeRow({
  node,
  pageId,
  variantId,
  editing = false,
  onOpen,
}: {
  node: CompositionTreeNode;
  pageId: string;
  variantId: string | null;
  /**
   * Is THIS the placement the form below is bound to (finding `01KZ41ZM4ZKR3F0WB53WSFJB0F`)? The
   * tree and the form are on screen together and nothing joined them: one clicked a row and the row
   * went back to looking like every other one.
   *
   * It is shown by the ROW ITSELF — a primary left edge and the row's own surface — not by a third
   * badge. Two badges already live on this line, and they answer different questions: `[DS]`/`[NEW]`
   * is what the node IS, `Open` is what clicking it DOES. What is open is a STATE of the row, so the
   * row carries it, the way a selected item in any list does.
   */
  editing?: boolean;
  onOpen: OpenNode;
}) {
  const { label, variant } = natureBadge(node.nature);
  const depth = nodeDepth(node.nodeKey);
  const name = node.component ?? leafLabel(node.nodeKey);
  const componentRef = node.componentRef;

  const inner = (
    <>
      <Badge variant={variant} className="shrink-0">
        {label}
      </Badge>
      <span className="min-w-0 flex-1 truncate text-foreground">{name}</span>
      {/* THE CSS EXCEPTION THIS PLACEMENT LANDS ON — flagged off the render's own list, so the
          reviewer walking the tree inherits the exception instead of discovering it. The node
          points; the component owns (finding `01KYVMTWB71R0D0CT1SV88J8DX`). */}
      {node.cssScope != null ? (
        <ZapIcon
          aria-hidden
          data-testid={`proto-comp-node-css-${node.nodeKey}`}
          className="size-3 shrink-0 text-warning"
        />
      ) : null}
      {componentRef != null ? (
        <span className="flex shrink-0 items-center gap-0.5 text-2xs text-muted-foreground">
          Open
          <ChevronRightIcon aria-hidden className="size-3" />
        </span>
      ) : null}
    </>
  );

  const indent = { paddingLeft: `${depth * 16 + 8}px` };

  if (componentRef != null) {
    return (
      <button
        type="button"
        data-testid={`proto-comp-node-${node.nodeKey}`}
        // `aria-current` is the a11y half of the same statement: this is the row the form is on.
        aria-current={editing ? "true" : undefined}
        data-editing={editing || undefined}
        onClick={() =>
          onOpen({ pageId, componentId: componentRef, variantId, nodeKey: node.nodeKey })
        }
        style={indent}
        className={cn(
          "flex w-full items-center gap-2 rounded-md border py-1 pr-2 text-left text-sm transition-colors",
          editing
            ? "border-primary/40 border-l-2 border-l-primary bg-primary/10 font-medium"
            : "border-transparent hover:border-border/60 hover:bg-muted/30",
        )}
      >
        {inner}
      </button>
    );
  }

  return (
    <div
      data-testid={`proto-comp-node-${node.nodeKey}`}
      style={indent}
      className="flex w-full items-center gap-2 py-1 pr-2 text-sm"
    >
      {inner}
    </div>
  );
}

export function CompositionTree({
  compositionJson,
  variantLabel,
  pageId,
  variantId,
  components,
  componentsLoading = false,
  cssSlots = [],
  editingNodeKey = null,
  onOpenNode,
}: {
  /** The composition the STAGE is rendering — the shown variant's, verbatim. */
  compositionJson: string | null;
  /** The shown variant's label, stated in the heading so the tree says WHICH variant it describes. */
  variantLabel?: string | null;
  pageId: string;
  /** The shown variant's id — the other half of a placement's identity ({@link OpenNode}). */
  variantId: string | null;
  /** The project's catalogue — what decides each node's nature and whether it opens. */
  components: readonly DesignComponentRow[];
  /**
   * Is the catalogue still loading? The verdict WAITS for it: before it answers, every node looks
   * un-catalogued, so a tree drawn then would badge the whole page `[NEW]` and offer no `Open` at
   * all. The same rule the params editor applies before it declares a placement missing.
   */
  componentsLoading?: boolean;
  /** The CSS exceptions THIS render met — the source of each row's `⚡` flag. */
  cssSlots?: readonly RenderedCssSlot[];
  /**
   * The `node_key` the params FORM is currently bound to — its row is distinguished here (finding
   * `01KZ41ZM4ZKR3F0WB53WSFJB0F`). It comes from the panel that owns the selection, not from the
   * click this tree emitted: a click can resolve to nothing (a node the form cannot bind), and a row
   * marked on the ask rather than on the outcome would be a lie in exactly that case.
   */
  editingNodeKey?: string | null;
  onOpenNode: OpenNode;
}) {
  const nodes = compositionTreeNodes(compositionJson, components, cssSlots);

  return (
    <section data-testid="proto-composition" aria-label="Composition" className="flex flex-col">
      {/* THE HEADING NAMES WHAT IT DESCRIBES. It used to read `Elected composition` over a tree of
          the frozen round — a title that was true of the read model and false of everything on
          screen. The variant is stated here because it is the one thing that makes the tree
          verifiable: the reviewer can see the strip's selection and this label agree. */}
      <div className="mb-2 flex items-center gap-2">
        <PackageIcon aria-hidden className="size-4 text-muted-foreground" />
        <h3 className="font-heading text-sm font-semibold text-foreground">Composition</h3>
        {variantLabel != null && variantLabel.length > 0 ? (
          <Badge variant="muted" className="shrink-0" data-testid="proto-composition-variant">
            {variantLabel}
          </Badge>
        ) : null}
      </div>

      {componentsLoading ? (
        <div className="flex justify-center py-6 text-muted-foreground">
          <Spinner className="text-muted-foreground/50" />
        </div>
      ) : compositionJson === null ? (
        <p data-testid="proto-composition-no-variant" className="text-xs text-muted-foreground">
          This page has no variant to show.
        </p>
      ) : nodes.length === 0 ? (
        <p data-testid="proto-composition-empty" className="text-xs text-muted-foreground">
          This composition places no component.
        </p>
      ) : (
        <ul className="flex flex-col">
          {nodes.map((node) => (
            <li key={node.nodeKey}>
              <NodeRow
                node={node}
                pageId={pageId}
                variantId={variantId}
                editing={node.nodeKey === editingNodeKey}
                onOpen={onOpenNode}
              />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
