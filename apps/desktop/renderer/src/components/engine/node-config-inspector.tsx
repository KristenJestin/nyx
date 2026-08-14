import { Fragment, useState } from "react";
import { ChevronUpIcon, CornerDownRightIcon, XIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import type { EngineNode } from "@/bridge";

import { describeBranchCondition, parseNodeConfig } from "./build-graph";
import { describeNode } from "./describe-node";
import { metaFor, TONE_CLASSES, type EngineNodeType } from "./node-meta";

/**
 * The TYPE-ADAPTED node inspector (v2.8 Phase 6). It no longer SERIALISES a node — it EXPLAINS it
 * (finding #35): a NATURAL-LANGUAGE sentence composed from the node's typed config sits at the top
 * ("Reads the Spec status and writes it to `spec_status`.", "Runs `typecheck`; blocks the run until
 * they pass.", …), the composite's ORDERED children stay listed + drillable, and the raw `config`
 * JSON moves behind a collapsed "Raw config" accordion for authors. The sentence is composed by the
 * pure {@link describeNode}, which reuses the same readable-condition helpers the graph edges/loop
 * headers use — so the wording never drifts between the graph and the inspector.
 *
 * Hovering a child in the list CROSS-HIGHLIGHTS its node in the graph and vice-versa (finding #35),
 * via the shared `hoveredNodeId` the page owns. Read-only — authoring stays MCP-first.
 */
export function NodeConfigInspector({
  node,
  childNodes,
  allNodes,
  onSelectNode,
  onClose,
  hoveredNodeId = null,
  onHoverNode,
  followAction,
}: {
  /** The selected node (its raw row — `type_` + `label` + `config` JSON string). */
  node: EngineNode;
  /** For a composite, its ORDERED child rows (`parent_id = node.id`), else empty. */
  childNodes: readonly EngineNode[];
  /** The workflow's WHOLE node list — so a composite child (a nested selector / sequence) can show ITS
   *  own children indented, making the grouping legible in the panel the way the graph draws it. */
  allNodes?: readonly EngineNode[];
  /** Drill into a composite child (open its own inspector). */
  onSelectNode: (nodeId: string) => void;
  onClose: () => void;
  /** The node currently HOVERED anywhere (graph or list) — highlights the matching child entry. */
  hoveredNodeId?: string | null;
  /** Report a hovered child up to the page so the graph node lights up (finding #35). */
  onHoverNode?: (nodeId: string | null) => void;
  /** A node that POINTS ELSEWHERE (today: `sub_workflow`) gets a real button to follow it — the host
   *  words it for what it does (open the flow's structure / descend into the child run). A click on
   *  the node alone would be an invisible affordance. */
  followAction?: { label: string; onFollow: () => void };
}) {
  const type = node.type_ as EngineNodeType;
  const meta = metaFor(node.type_);
  const tone = TONE_CLASSES[meta.tone];
  const Icon = meta.icon;
  const config = parseNodeConfig(node.config);
  const sentence = describeNode(node.type_, node.config, childNodes);
  const isComposite = type === "sequence" || type === "loop" || type === "selector";

  return (
    <div className="flex h-full flex-col overflow-hidden">
      {/* Header — the node identity: a human title + its type (muted secondary text, not a badge —
          the title already carries the kind) + its own label. */}
      <div className="flex items-start gap-2 border-b border-border p-3">
        <span
          className={cn(
            "flex size-7 shrink-0 items-center justify-center rounded-lg border",
            tone.border,
            tone.bg,
            tone.text,
          )}
        >
          <Icon className="size-4" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          {/* The node's own NAME leads (that is what identifies it); the TYPE follows as the muted
              secondary. Naming the kind twice ("Selector SELECTOR") said nothing — the name does. */}
          <div className="flex items-baseline gap-2">
            <h2 className="min-w-0 truncate font-heading text-sm font-semibold text-foreground">
              {node.label || TYPE_TITLE[type] || meta.kindLabel}
            </h2>
            <span className="shrink-0 font-mono text-2xs uppercase tracking-wide text-muted-foreground">
              {meta.kindLabel}
            </span>
          </div>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close inspector"
          className="rounded-md p-1 text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
        >
          <XIcon className="size-4" />
        </button>
      </div>

      {/* Body — the natural-language explanation, then the composite's children, then the raw config. */}
      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-3">
        {sentence ? (
          <p data-testid="node-explanation" className="text-xs leading-relaxed text-foreground">
            <Explanation text={sentence} />
          </p>
        ) : (
          <p data-testid="node-explanation" className="text-2xs italic text-muted-foreground">
            No description for this node — its raw configuration is below.
          </p>
        )}

        {followAction ? (
          <button
            type="button"
            data-testid="follow-sub-workflow"
            onClick={followAction.onFollow}
            className="flex w-full items-center justify-between gap-2 rounded-md border border-primary/40 bg-primary/10 px-2.5 py-2 text-left text-2xs font-semibold text-primary outline-none transition-colors hover:bg-primary/20 focus-visible:ring-2 focus-visible:ring-ring"
          >
            {followAction.label}
            <CornerDownRightIcon className="size-3.5 shrink-0" aria-hidden />
          </button>
        ) : null}

        {type === "selector" ? (
          <BranchList
            childRows={childNodes}
            selectorConfig={node.config}
            allNodes={allNodes}
            onSelectNode={onSelectNode}
            hoveredNodeId={hoveredNodeId}
            onHoverNode={onHoverNode}
          />
        ) : isComposite ? (
          <ChildrenList
            childRows={childNodes}
            allNodes={allNodes}
            onSelectNode={onSelectNode}
            hoveredNodeId={hoveredNodeId}
            onHoverNode={onHoverNode}
          />
        ) : null}

        {/* The N31 caveat stays where a gate's commands are read: they run non-interactively. It
            applies just as much to a SLOT gate — the commands the project bound to it run through the
            very same PTY path — so the caveat follows the gate, not the way it happens to name them. */}
        {type === "gate_check" &&
        (stringArray(config.command_ids).length > 0 || typeof config.slot === "string") ? (
          <p className="text-2xs text-muted-foreground">
            Gate commands run non-interactively and do not source your shell rc (no mise/nvm/bun/direnv).
            Make each command self-contained — use an absolute path or a wrapper that sources the env —
            or it may be red here yet green when run by hand in a terminal.
          </p>
        ) : null}
      </div>

      {/* RAW CONFIG is PINNED to the bottom of the panel and opens UPWARD (it grows into the space the
          body gives up), with an animated expand/collapse — an author's fallback that never pushes the
          explanation out of view. */}
      <RawConfig config={config} />
    </div>
  );
}

/** Human-readable panel titles per node type (the badge keeps the raw vocabulary). */
const TYPE_TITLE: Partial<Record<EngineNodeType, string>> = {
  gate_check: "Quality gate",
  judge: "Judge",
  transition: "State transition",
  ask_human: "Human question",
  sequence: "Sequence",
  loop: "Loop",
  selector: "Selector",
  wait: "Wait",
  note: "Note",
  produce: "Produce",
  spawn_subagent: "Sub-agent",
  sub_workflow: "Sub-workflow",
  transition_task: "Task transition",
};

/** Splits a describer sentence into its STATIC prose and its two kinds of DYNAMIC span: a backtick
 *  `identifier` (blackboard key / state / command id) and a «quoted prompt» (author-written text). */
const DYNAMIC_SPAN = /(`[^`]*`|«[^»]*»)/g;

/**
 * Render a describer sentence: static prose stays plain, and everything that comes FROM THE NODE'S
 * CONFIG is set apart, because that is the part that actually varies per node —
 *   - `identifier` → a mono chip, bold, in the primary accent the graph uses for its branch labels;
 *   - «prompt»     → bold prose (it is a sentence, not a token, so it keeps the reading font).
 */
function Explanation({ text }: { text: string }) {
  const parts = text.split(DYNAMIC_SPAN).filter((p) => p !== "");
  return (
    <>
      {parts.map((part, i) => {
        if (part.length > 1 && part.startsWith("`") && part.endsWith("`")) {
          return (
            <code
              key={i}
              className="rounded bg-muted px-1 py-0.5 font-mono font-bold text-primary"
            >
              {part.slice(1, -1)}
            </code>
          );
        }
        if (part.length > 1 && part.startsWith("«") && part.endsWith("»")) {
          return (
            <strong key={i} className="font-semibold text-foreground">
              {part}
            </strong>
          );
        }
        return <Fragment key={i}>{part}</Fragment>;
      })}
    </>
  );
}

/** One child ROW — drillable, and a hover CROSS-HIGHLIGHT target both ways (finding #35): hovering it
 *  lights up its node in the graph (`onHoverNode`), and a node hovered in the graph lights up this row
 *  (`hoveredNodeId`). Shared by the plain children list and the selector's branch list. */
function ChildRow({
  child,
  allNodes,
  onSelectNode,
  hoveredNodeId,
  onHoverNode,
}: {
  child: EngineNode;
  /** The whole node list, so a COMPOSITE child can show its own children indented beneath it. */
  allNodes?: readonly EngineNode[];
  onSelectNode: (nodeId: string) => void;
  hoveredNodeId: string | null;
  onHoverNode?: (nodeId: string | null) => void;
}) {
  const childMeta = metaFor(child.type_);
  const childTone = TONE_CLASSES[childMeta.tone];
  const ChildIcon = childMeta.icon;
  const hovered = hoveredNodeId === child.id;
  // A composite child GROUPS nodes of its own — the graph draws that as a box / a fork, so the panel
  // must not flatten it to a single opaque row. Its children are listed INDENTED beneath it.
  const grandChildren = (allNodes ?? [])
    .filter((n) => n.parent_id === child.id)
    .sort((a, b) => a.order_index - b.order_index);
  return (
    <div className="flex flex-col gap-1">
      <button
        type="button"
        data-hovered={hovered ? "true" : undefined}
        onClick={() => onSelectNode(child.id)}
        onMouseEnter={() => onHoverNode?.(child.id)}
        onMouseLeave={() => onHoverNode?.(null)}
        className={cn(
          "flex w-full items-center gap-2 rounded-md border bg-card px-2 py-1.5 text-left outline-none transition-colors hover:border-ring focus-visible:ring-2 focus-visible:ring-ring",
          hovered ? "border-ring ring-1 ring-ring/60" : "border-border",
        )}
      >
        <span
          className={cn(
            "flex size-5 shrink-0 items-center justify-center rounded border",
            childTone.border,
            childTone.bg,
            childTone.text,
          )}
        >
          <ChildIcon className="size-3" aria-hidden />
        </span>
        <span className="min-w-0 flex-1 truncate font-mono text-2xs text-foreground">
          {child.label || childMeta.kindLabel}
        </span>
        <span className="shrink-0 font-mono text-2xs uppercase tracking-wide text-muted-foreground">
          {childMeta.kindLabel}
        </span>
      </button>
      {grandChildren.length > 0 ? (
        <ul
          data-testid="nested-children"
          className="ml-3 flex flex-col gap-1 border-l border-border pl-2"
        >
          {grandChildren.map((gc) => {
            const gcMeta = metaFor(gc.type_);
            const gcHovered = hoveredNodeId === gc.id;
            const GcIcon = gcMeta.icon;
            return (
              <li key={gc.id}>
                <button
                  type="button"
                  data-hovered={gcHovered ? "true" : undefined}
                  onClick={() => onSelectNode(gc.id)}
                  onMouseEnter={() => onHoverNode?.(gc.id)}
                  onMouseLeave={() => onHoverNode?.(null)}
                  className={cn(
                    "flex w-full items-center gap-1.5 rounded px-1.5 py-1 text-left outline-none transition-colors hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring",
                    gcHovered && "bg-muted",
                  )}
                >
                  <GcIcon
                    className={cn("size-3 shrink-0", TONE_CLASSES[gcMeta.tone].text)}
                    aria-hidden
                  />
                  <span className="min-w-0 flex-1 truncate font-mono text-2xs text-muted-foreground">
                    {gc.label || gcMeta.kindLabel}
                  </span>
                  <span className="shrink-0 font-mono text-2xs uppercase tracking-wide text-muted-foreground/70">
                    {gcMeta.kindLabel}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      ) : null}
    </div>
  );
}

/**
 * A SELECTOR's children rendered as what they ACTUALLY are: the ordered BRANCHES of an if-chain. Each
 * branch shows its CONDITION on top (`IF spec_status = ready`, the default reading `ELSE`) with the
 * child it routes to just below, pointed at by a `↳`. This replaces the flat "CHILDREN (n)" list whose
 * rows said nothing about WHY a branch is taken, and it removes the duplication with the top sentence
 * (which no longer re-enumerates the conditions — see `describeNode`). Rows stay drillable + hover
 * cross-highlighted with the graph (finding #35).
 */
function BranchList({
  childRows,
  selectorConfig,
  allNodes,
  onSelectNode,
  hoveredNodeId,
  onHoverNode,
}: {
  childRows: readonly EngineNode[];
  selectorConfig: string;
  allNodes?: readonly EngineNode[];
  onSelectNode: (nodeId: string) => void;
  hoveredNodeId: string | null;
  onHoverNode?: (nodeId: string | null) => void;
}) {
  if (childRows.length === 0) {
    return <p className="text-2xs italic text-muted-foreground">No branches.</p>;
  }
  return (
    <ol data-testid="selector-branches" className="flex flex-col gap-2.5">
      {childRows.map((child, index) => {
        // The branch that routes to this child; an `always` / missing condition IS the default arm.
        const raw = describeBranchCondition(selectorConfig, index);
        const isDefault = raw === null || raw === "always";
        return (
          <li key={child.id} className="flex flex-col gap-1">
            <div className="flex items-baseline gap-1.5">
              <span className="shrink-0 font-mono text-2xs font-semibold uppercase tracking-wide text-muted-foreground">
                {isDefault ? "else" : "if"}
              </span>
              {isDefault ? null : (
                <code
                  data-testid="child-branch-condition"
                  title={raw}
                  className="min-w-0 truncate rounded bg-muted px-1 py-0.5 font-mono text-xs font-bold text-primary"
                >
                  {raw}
                </code>
              )}
            </div>
            <div className="flex items-center gap-1">
              <CornerDownRightIcon
                aria-hidden
                className="size-3.5 shrink-0 text-muted-foreground"
              />
              <div className="min-w-0 flex-1">
                <ChildRow
                  child={child}
                  allNodes={allNodes}
                  onSelectNode={onSelectNode}
                  hoveredNodeId={hoveredNodeId}
                  onHoverNode={onHoverNode}
                />
              </div>
            </div>
          </li>
        );
      })}
    </ol>
  );
}

/** A non-selector composite's ORDERED children (sequence / loop) — a plain drillable list. */
function ChildrenList({
  childRows,
  allNodes,
  onSelectNode,
  hoveredNodeId,
  onHoverNode,
}: {
  childRows: readonly EngineNode[];
  allNodes?: readonly EngineNode[];
  onSelectNode: (nodeId: string) => void;
  hoveredNodeId: string | null;
  onHoverNode?: (nodeId: string | null) => void;
}) {
  return (
    <div>
      <div className="mb-1 text-2xs font-semibold uppercase tracking-wide text-muted-foreground">
        Children ({childRows.length})
      </div>
      {childRows.length > 0 ? (
        <ul className="flex flex-col gap-1">
          {childRows.map((child) => (
            <li key={child.id}>
              <ChildRow
                child={child}
                allNodes={allNodes}
                onSelectNode={onSelectNode}
                hoveredNodeId={hoveredNodeId}
                onHoverNode={onHoverNode}
              />
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-2xs italic text-muted-foreground">No children.</p>
      )}
    </div>
  );
}

/**
 * The RAW config JSON — the authoring fallback (finding #35), PINNED to the panel's bottom edge and
 * opening UPWARD: its toggle stays put while the JSON grows above it, so opening it never scrolls the
 * explanation away. The expand/collapse is ANIMATED with the `grid-rows 0fr → 1fr` technique, which
 * transitions to the content's natural height (a plain `height:auto` cannot be animated).
 */
function RawConfig({ config }: { config: Record<string, unknown> }) {
  const [open, setOpen] = useState(false);
  return (
    <div data-testid="raw-config" data-open={open ? "true" : undefined} className="border-t border-border">
      {/* The JSON pane sits ABOVE the toggle in DOM order — so it expands upward from the bottom bar. */}
      <div
        className={cn(
          "grid transition-[grid-template-rows] duration-200 ease-out",
          open ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
        )}
      >
        <div className="overflow-hidden">
          <pre
            data-testid="raw-config-json"
            className="max-h-56 overflow-auto whitespace-pre-wrap break-words px-3 py-2 font-mono text-2xs leading-relaxed text-muted-foreground"
          >
            {toJson(config)}
          </pre>
        </div>
      </div>
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between gap-2 px-3 py-2 text-2xs font-semibold uppercase tracking-wide text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
      >
        Raw config
        <ChevronUpIcon
          aria-hidden
          className={cn(
            "size-3.5 transition-transform duration-200 ease-out",
            open && "rotate-180",
          )}
        />
      </button>
    </div>
  );
}

/** A config value coerced to a `string[]` (non-strings dropped). */
function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
}

/** Pretty-print a JSON-able config value for display. */
function toJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
