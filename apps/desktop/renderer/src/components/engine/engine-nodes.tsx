import { createContext, useContext } from "react";
import { Handle, Position, type Node, type NodeProps, type NodeTypes } from "@xyflow/react";
import { RotateCcwIcon } from "lucide-react";

import { cn } from "@/lib/utils";

import type { EngineFlowData, EngineGroupData, EnginePillData } from "./build-graph";
import { metaFor, TONE_CLASSES } from "./node-meta";

/**
 * A clickable `{{…}}` MARKER carried by a graph node: a spawn node's `sub_agent:<role>` or a gate's
 * `check:<command>` (annexes `engine-graph` + `context-effecteur`). Clicking a `sub_agent` marker
 * opens the effector inspector; a `check` marker points at the gate's command.
 */
export interface EngineMarker {
  readonly kind: "sub_agent" | "check";
  readonly id: string;
  readonly nodeId: string;
}

/** The interaction hooks the canvas provides to its custom nodes (kept out of the pure layout data
 *  so `build-graph` stays React-free and testable). */
export interface EngineInteraction {
  readonly onMarker?: (marker: EngineMarker) => void;
  /** Open the TYPE-ADAPTED inspector for a node (finding #32). Every leaf/decision node and every
   *  composite GROUP is clickable; the callback selects it. */
  readonly onSelectNode?: (nodeId: string) => void;
  /** CROSS-HIGHLIGHT hover (finding #35): hovering a graph node reports its id (and `null` on leave)
   *  so the inspector's children list can light up the matching entry — and vice-versa. */
  readonly onHoverNode?: (nodeId: string | null) => void;
}

export const EngineInteractionContext = createContext<EngineInteraction>({});

/** A small marker CHIP — the clickable `{{sub_agent:id}}` / `{{check:id}}` token on a node. */
function MarkerChip({
  marker,
  toneText,
  toneBorder,
  toneBg,
}: {
  marker: EngineMarker;
  toneText: string;
  toneBorder: string;
  toneBg: string;
}) {
  const { onMarker } = useContext(EngineInteractionContext);
  const label = `${marker.kind}:${marker.id}`;
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        onMarker?.(marker);
      }}
      data-marker-kind={marker.kind}
      data-marker-id={marker.id}
      className={cn(
        "inline-flex max-w-full items-center gap-1 truncate rounded-md border px-1.5 py-0.5 font-mono text-2xs font-semibold outline-none transition-shadow focus-visible:ring-2",
        toneText,
        toneBorder,
        toneBg,
      )}
    >
      {label}
    </button>
  );
}

/**
 * A custom FLOW node — a leaf/decision (`spawn` / `gate` / `judge` / `selector` / `transition` / … ).
 * Icon + tone are per-TYPE (design-system tokens only); a `spawn` node carries its `sub_agent:<role>`
 * marker and a `gate` its `check:<command>` markers, both clickable. The whole node is CLICKABLE —
 * it opens its type-adapted inspector (finding #32). It carries three token-based interaction states
 * (finding #33): HOVER feedback, a distinct SELECTED ring (inspector open), and the RUNTIME cursor
 * (a `tone.ring` + pulse dot when `data.current`).
 */
function EngineFlowNode({ data }: NodeProps) {
  const d = data as EngineFlowData;
  const { onSelectNode, onHoverNode } = useContext(EngineInteractionContext);
  const meta = metaFor(d.nodeType);
  const tone = TONE_CLASSES[meta.tone];
  const Icon = meta.icon;
  return (
    <div
      data-node-type={d.nodeType}
      data-current={d.current ? "true" : undefined}
      data-selected={d.selected ? "true" : undefined}
      data-hovered={d.hovered ? "true" : undefined}
      aria-current={d.current ? "step" : undefined}
      role="button"
      tabIndex={0}
      onClick={() => onSelectNode?.(d.nodeId)}
      onMouseEnter={() => onHoverNode?.(d.nodeId)}
      onMouseLeave={() => onHoverNode?.(null)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelectNode?.(d.nodeId);
        }
      }}
      className={cn(
        "relative w-[254px] cursor-pointer rounded-xl border border-border bg-card px-2.5 py-2 text-card-foreground shadow-sm outline-none transition-shadow",
        "border-l-[3px]",
        tone.border,
        // HOVER feedback (finding #33) — subtle ring + lift, only when not already ringed by
        // selection / the runtime cursor.
        !d.selected && !d.current && "hover:shadow-md hover:ring-1 hover:ring-ring/40",
        // SELECTED (inspector open) — a reinforced NEUTRAL ring that wins over the runtime-current
        // ring; the current pulse dot below still marks the live cursor, so the two never collapse.
        d.selected
          ? "ring-2 ring-ring ring-offset-1 ring-offset-background shadow-md"
          : d.current && [
              "ring-2 ring-offset-1 ring-offset-background",
              tone.ring,
              "shadow-md",
              // The RUNNING node breathes: a static ring reads the same as the "selected" ring, so the
              // live cursor was indistinguishable from whatever you last clicked. The pulse is the one
              // cue that says "this is happening NOW" without another colour.
              "animate-pulse",
            ],
        // CROSS-HIGHLIGHT (finding #35) — the children list drove a hover onto this node; a ring lit
        // even without the pointer over it (the selected ring still wins).
        d.hovered && !d.selected && "ring-2 ring-ring/70 ring-offset-1 ring-offset-background shadow-md",
        "focus-visible:ring-2 focus-visible:ring-ring",
      )}
    >
      <Handle type="target" position={Position.Top} className="!size-1.5 !border-0 !bg-border" />
      {d.current ? (
        <span
          aria-label="current node"
          className={cn(
            "absolute right-2 top-2 size-2 animate-pulse rounded-full",
            tone.bg,
            tone.text,
          )}
        />
      ) : null}
      <div className="flex items-center gap-2">
        <span
          className={cn(
            "flex size-6 shrink-0 items-center justify-center rounded-md border",
            tone.border,
            tone.bg,
            tone.text,
          )}
        >
          <Icon className="size-3.5" aria-hidden />
        </span>
        <span className="min-w-0 flex-1 truncate font-mono text-xs font-semibold text-foreground">
          {d.label || meta.kindLabel}
        </span>
        <span className="shrink-0 font-mono text-2xs uppercase tracking-wide text-muted-foreground">
          {meta.kindLabel}
        </span>
      </div>

      {(d.nodeType === "spawn_subagent" && d.agentId) ||
      (d.nodeType === "gate_check" && (d.commandIds?.length ?? 0) > 0) ? (
        <div className="mt-1.5 flex flex-wrap gap-1 pl-8">
          {d.nodeType === "spawn_subagent" && d.agentId ? (
            <MarkerChip
              marker={{ kind: "sub_agent", id: d.agentId, nodeId: d.nodeId }}
              toneText={tone.text}
              toneBorder={tone.border}
              toneBg={tone.bg}
            />
          ) : null}
          {d.nodeType === "gate_check"
            ? d.commandIds?.map((id) => (
                <MarkerChip
                  key={id}
                  marker={{ kind: "check", id, nodeId: d.nodeId }}
                  toneText={tone.text}
                  toneBorder={tone.border}
                  toneBg={tone.bg}
                />
              ))
            : null}
        </div>
      ) : null}
      <Handle type="source" position={Position.Bottom} className="!size-1.5 !border-0 !bg-border" />
    </div>
  );
}

/**
 * A GROUP container node — a `loop` (dashed) or `sequence` (solid) box drawn AROUND its children. Its
 * corner CHIP leads with the composite TYPE (`sequence` / `loop`) so the group is never mistaken for
 * its own workflow name / the start pill (finding #30), and the chip is the CLICKABLE handle that
 * opens the composite inspector (finding #32). A token ring marks the SELECTED state (finding #33).
 *
 * A `loop` ALSO carries a reboucle indicator on its header — a red, dashed `↺` chip with its `until`
 * exit condition (`data.loopBack`) — so the loop-back reads on the CONTAINER, contained inside the box,
 * instead of a floating back-edge that overflowed to the side (finding #39). The red + dashed treatment
 * keeps the reboucle semantics; the arc that overflowed is gone.
 */
function EngineGroupNode({ data }: NodeProps) {
  const d = data as EngineGroupData;
  const { onSelectNode, onHoverNode } = useContext(EngineInteractionContext);
  const meta = metaFor(d.nodeType);
  const tone = TONE_CLASSES[meta.tone];
  const Icon = meta.icon;
  const isLoop = d.nodeType === "loop";
  // The group's OWN label, shown only when it adds information over the type chip (e.g. `body`,
  // `phases`) — never a bare echo of the type, and never a re-print of the workflow name (that lives
  // ONCE on the start pill; the root group keeps only its type — finding #30 stays: start ≠ group).
  const subLabel =
    d.label && d.label !== meta.kindLabel && d.label !== d.rootLabel ? d.label : null;
  return (
    <div
      data-group-type={d.nodeType}
      data-selected={d.selected ? "true" : undefined}
      data-hovered={d.hovered ? "true" : undefined}
      className={cn(
        "pointer-events-none h-full w-full rounded-2xl border",
        isLoop ? "border-dashed" : "border-solid",
        tone.border,
        tone.bg,
        d.selected && "ring-2 ring-ring ring-offset-1 ring-offset-background",
        // CROSS-HIGHLIGHT (finding #35) — hovered via its entry in the inspector's children list.
        d.hovered && !d.selected && "ring-2 ring-ring/70 ring-offset-1 ring-offset-background",
      )}
    >
      <button
        type="button"
        onClick={() => onSelectNode?.(d.nodeId)}
        onMouseEnter={() => onHoverNode?.(d.nodeId)}
        onMouseLeave={() => onHoverNode?.(null)}
        className={cn(
          "pointer-events-auto absolute -top-2.5 left-4 inline-flex cursor-pointer items-center gap-1.5 rounded-full border bg-background px-2 py-0.5 font-mono text-2xs font-semibold outline-none transition-colors",
          tone.border,
          tone.text,
          "hover:border-ring hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
          d.selected && "border-ring text-foreground ring-1 ring-ring",
        )}
      >
        <Icon className="size-3" aria-hidden />
        <span className="uppercase tracking-wide">{meta.kindLabel}</span>
        {subLabel ? (
          <span className="font-normal normal-case text-muted-foreground">· {subLabel}</span>
        ) : null}
      </button>
      {isLoop ? (
        <span
          data-loop-back=""
          aria-label={d.loopBack ? `loops back until ${d.loopBack}` : "loops back"}
          className="pointer-events-none absolute -top-2.5 right-4 inline-flex max-w-[70%] items-center gap-1 truncate rounded-full border border-dashed border-destructive/50 bg-background px-2 py-0.5 font-mono text-2xs font-semibold text-destructive"
        >
          <RotateCcwIcon className="size-3 shrink-0" aria-hidden />
          {d.loopBack ? (
            <span className="truncate font-normal normal-case">{d.loopBack}</span>
          ) : (
            <span className="uppercase tracking-wide">loops</span>
          )}
        </span>
      ) : null}
    </div>
  );
}

/** A synthetic START / TERMINAL pill bracketing the run. The START pill leads with an explicit
 *  "Start" entry-point badge so it is never confused with the root composite group that shares the
 *  workflow name (finding #30) — the workflow name follows only as muted context. */
function EnginePillNode({ data }: NodeProps) {
  const d = data as EnginePillData;
  const meta = metaFor(d.nodeType);
  const tone = TONE_CLASSES[meta.tone];
  const Icon = meta.icon;
  const isStart = d.nodeType === "start";
  return (
    <div
      data-pill-type={d.nodeType}
      className={cn(
        "inline-flex w-[210px] items-center justify-center gap-2 rounded-full border px-3 py-1.5 font-mono text-xs font-semibold",
        tone.border,
        tone.bg,
        tone.text,
      )}
    >
      {d.nodeType === "terminal" ? (
        <Handle type="target" position={Position.Top} className="!size-1.5 !border-0 !bg-border" />
      ) : null}
      <Icon className="size-3.5 shrink-0" aria-hidden />
      {isStart ? (
        <>
          <span className="shrink-0 uppercase tracking-wide">Start</span>
          <span className="min-w-0 flex-1 truncate font-normal text-muted-foreground">
            · {d.label}
          </span>
        </>
      ) : (
        <span className="truncate">{d.label}</span>
      )}
      {isStart ? (
        <Handle
          type="source"
          position={Position.Bottom}
          className="!size-1.5 !border-0 !bg-border"
        />
      ) : null}
    </div>
  );
}

/** The custom node-type registry passed to `<ReactFlow nodeTypes>`. */
export const engineNodeTypes: NodeTypes = {
  engineFlow: EngineFlowNode,
  engineGroup: EngineGroupNode,
  enginePill: EnginePillNode,
};

export { EngineFlowNode, EngineGroupNode, EnginePillNode };
export type { Node };
