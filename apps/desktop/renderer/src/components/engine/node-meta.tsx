import {
  ArrowRightLeftIcon,
  BotIcon,
  ClockIcon,
  FlagIcon,
  GaugeIcon,
  GitForkIcon,
  LayersIcon,
  ListChecksIcon,
  ListIcon,
  MessageCircleQuestionIcon,
  PackageIcon,
  PlayIcon,
  RepeatIcon,
  ScaleIcon,
  SearchIcon,
  SplitIcon,
  StickyNoteIcon,
  type LucideIcon,
} from "lucide-react";

/**
 * The behavior-tree node vocabulary the engine graph paints (v2.8 Phase 6). It mirrors the nyx-core
 * `db::NODE_TYPES` CHECK — the 3 COMPOSITES + the 8 LEAVES — PLUS the two SYNTHETIC pills the graph
 * adds for readability (`start` = the workflow entry, `terminal` = its end). The renderer maps each
 * to an icon + a semantic TONE (a design-system token, never a raw colour) and a rendering CATEGORY:
 *   - `flow`  — a custom leaf-style node (spawn / gate / judge / selector / transition / … ).
 *   - `group` — a container GROUP node (loop / sequence) drawn AROUND its children.
 *   - `pill`  — a synthetic start/terminal marker.
 */
export type EngineNodeType =
  // composites → group nodes
  | "sequence"
  | "loop"
  // composites → rendered as a decision FLOW node (not a container)
  | "selector"
  // leaves → flow nodes
  | "spawn_subagent"
  | "gate_check"
  | "judge"
  | "transition"
  | "query"
  | "fork_spec"
  | "sub_workflow"
  | "transition_task"
  | "wait"
  | "note"
  | "ask_human"
  | "produce"
  // synthetic pills
  | "start"
  | "terminal";

/** How a node is drawn on the canvas. */
export type NodeCategory = "flow" | "group" | "pill";

/**
 * A semantic TONE — resolves ONLY to design-system tokens (`info` / `destructive` / `warning` /
 * `primary` / `success` / `muted`), so the graph follows the theme (light/dark) and never bakes a
 * literal colour (the annexes' hex are a layout reference, not pixels to copy).
 */
export type NodeTone =
  | "info"
  | "destructive"
  | "warning"
  | "primary"
  | "branch"
  | "success"
  | "muted";

/** The token class-sets for each tone. `text`/`border`/`bg` paint the node; `ring` is the runtime
 *  highlight of the current node. */
export const TONE_CLASSES: Record<
  NodeTone,
  { text: string; border: string; bg: string; ring: string }
> = {
  info: { text: "text-info", border: "border-info/40", bg: "bg-info/10", ring: "ring-info" },
  // The FORK hue. `selector` used to share `primary` with `sub_workflow`, so a branch in the graph and
  // a whole child run looked alike — the two most structural nodes, indistinguishable at a glance.
  branch: {
    text: "text-branch",
    border: "border-branch/40",
    bg: "bg-branch/10",
    ring: "ring-branch",
  },
  destructive: {
    text: "text-destructive",
    border: "border-destructive/40",
    bg: "bg-destructive/10",
    ring: "ring-destructive",
  },
  warning: {
    text: "text-warning",
    border: "border-warning/40",
    bg: "bg-warning/10",
    ring: "ring-warning",
  },
  primary: {
    text: "text-primary",
    border: "border-primary/40",
    bg: "bg-primary/10",
    ring: "ring-primary",
  },
  success: {
    text: "text-success",
    border: "border-success/40",
    bg: "bg-success/10",
    ring: "ring-success",
  },
  muted: {
    text: "text-muted-foreground",
    border: "border-border",
    bg: "bg-muted/40",
    ring: "ring-muted-foreground",
  },
};

/** The per-type metadata the custom nodes render from. */
export interface NodeMeta {
  readonly icon: LucideIcon;
  readonly tone: NodeTone;
  /** The short vocabulary label shown as the node's kind badge. */
  readonly kindLabel: string;
  readonly category: NodeCategory;
}

/** The full node-type → metadata table (icon + tone + kind label + category). */
export const NODE_META: Record<EngineNodeType, NodeMeta> = {
  sequence: { icon: ListIcon, tone: "muted", kindLabel: "sequence", category: "group" },
  loop: { icon: RepeatIcon, tone: "success", kindLabel: "loop", category: "group" },
  selector: { icon: SplitIcon, tone: "branch", kindLabel: "selector", category: "flow" },
  spawn_subagent: { icon: BotIcon, tone: "info", kindLabel: "spawn", category: "flow" },
  gate_check: { icon: GaugeIcon, tone: "destructive", kindLabel: "gate_check", category: "flow" },
  judge: { icon: ScaleIcon, tone: "warning", kindLabel: "judge", category: "flow" },
  transition: {
    icon: ArrowRightLeftIcon,
    tone: "destructive",
    kindLabel: "transition",
    category: "flow",
  },
  // A nyx-own fact read (B2) — reads verifiable state onto the blackboard.
  query: { icon: SearchIcon, tone: "info", kindLabel: "query", category: "flow" },
  // A nyx-own Spec fork (N13) — a sensitive mutation, hence the warning tone.
  fork_spec: { icon: GitForkIcon, tone: "warning", kindLabel: "fork_spec", category: "flow" },
  // Opens a CHILD run on another flow (v2.8 lot 2) — structurally the heaviest leaf there is, so it
  // must never fall back to the `note` default and read as an inert marker.
  sub_workflow: {
    icon: LayersIcon,
    tone: "primary",
    kindLabel: "sub_workflow",
    category: "flow",
  },
  // Moves a TASK's state (start/done/block/skip) — a real mutation, like its Spec-level sibling.
  transition_task: {
    icon: ListChecksIcon,
    tone: "destructive",
    kindLabel: "transition_task",
    category: "flow",
  },
  wait: { icon: ClockIcon, tone: "muted", kindLabel: "wait", category: "flow" },
  note: { icon: StickyNoteIcon, tone: "muted", kindLabel: "note", category: "flow" },
  ask_human: {
    icon: MessageCircleQuestionIcon,
    tone: "warning",
    kindLabel: "ask_human",
    category: "flow",
  },
  produce: { icon: PackageIcon, tone: "success", kindLabel: "produce", category: "flow" },
  start: { icon: PlayIcon, tone: "primary", kindLabel: "start", category: "pill" },
  terminal: { icon: FlagIcon, tone: "success", kindLabel: "terminal", category: "pill" },
};

/** Whether a DB node `type` is a COMPOSITE the graph draws as a GROUP container (loop / sequence). */
export function isGroupType(type: string): boolean {
  return type === "loop" || type === "sequence";
}

/** Resolve a DB node `type` string to its metadata, falling back to the muted `note` styling for an
 *  unknown/forward-compatible type so an unrecognised node still paints rather than crashing. */
export function metaFor(type: string): NodeMeta {
  return NODE_META[type as EngineNodeType] ?? NODE_META.note;
}
