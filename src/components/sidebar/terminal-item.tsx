import { SquareTerminalIcon, XIcon } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { resolveDisplayName, useAutoLabel, useShellSuffix } from "./auto-label";
import { itemTransition, itemVariants } from "./item-motion";
import { TerminalStateBadge } from "./run-state";
import { useRunningDebounce } from "./use-running-debounce";
import { SidebarItemContent, sidebarRowClassName } from "./sidebar-item";
import { agentProviderFor } from "./agent-providers";
import { useTerminalAgentKind } from "./use-agent-sessions";
import type { TerminalRecord } from "./use-terminals";

/**
 * The human label for a terminal item, given a record + its list index, using
 * only the record (no live auto label). Kept as a thin wrapper over
 * `resolveDisplayName` for the static title (chrome bar) and existing tests:
 * manual `label` → cwd basename → `Terminal <n>`. The live, auto-named label
 * (cwd + foreground program) is resolved inside `<TerminalItemBody>` via
 * `useAutoLabel`; this static form is the no-auto fallback.
 */
export function displayName(record: TerminalRecord, index: number): string {
  return resolveDisplayName(record, index, null);
}

export interface TerminalItemProps {
  record: TerminalRecord;
  index: number;
  active: boolean;
  /**
   * Live PTY id for this terminal (or null if not spawned yet). Drives the auto
   * label (cwd + foreground program) read from `terminal_info`.
   */
  ptyId?: number | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
}

/**
 * The className for the VISUAL terminal row — the clickable inner element inside
 * `<TerminalItem>`'s height-collapsing `motion.li` wrapper. `dragging` is kept for
 * the legacy `<ReorderTerminalItem>` (no longer rendered live) and is a no-op for
 * the live row.
 *
 * `cursor-pointer`: the WHOLE row is one click target — a click ANYWHERE selects
 * it (finding 01KV3CND2GKZCG8MQCHF7W32Q5: no dead zone), since the row element
 * itself owns the click (no inner button whose bounds a click could miss). Drag
 * reorder was removed with Motion's Reorder (it could not coexist with a clean
 * height-collapse animation — see `item-motion.ts`).
 *
 * SELECTION CHANNEL: there is NO magenta BACKGROUND fill and no per-row magenta —
 * the magenta lives only in the SINGLE shared `<ActiveRail>` (a `layoutId`
 * element) rendered inside the active row, which Motion FLIPs between rows.
 * Selection is expressed on the row via the v6 DIMMED/ACTIVE model: inactive rows
 * sit at reduced opacity, the active row is full-opacity and its name goes
 * bold/white. `relative` anchors the rail. `overflow-hidden` is REQUIRED so the
 * enter/exit height collapse clips the row content.
 */
export function terminalRowClassName(active: boolean, dragging = false): string {
  // The row shape is now the SHARED `sidebarRowClassName` (the one gabarit used by
  // both terminal and command items — finding 01KV63TBV7…). `dragging` is the only
  // terminal-specific modifier left (the legacy drag-ghost opacity).
  return cn(sidebarRowClassName(active), dragging && "opacity-60");
}

/**
 * The INNER content of a terminal row — the lead glyph (with run-state badge),
 * the name (+ shell suffix), and the hover-revealed close (`x`) — WITHOUT the row
 * element itself. Wrapper-agnostic so it can live inside either a plain
 * `motion.li` (`<TerminalItem>`, used in isolation tests) or a `Reorder.Item`
 * (`<ReorderTerminalItem>`, used by the live sidebar — the WHOLE row is the drag
 * affordance, no separate grip).
 *
 * Selection is owned by the ROW (a click anywhere selects — see the wrappers);
 * this body renders the active rail and the controls only. There is NO inline
 * rename here any more (finding 01KV3CNPDMBDWYKZZKPJ8RWKQX removed double-click
 * editing entirely; renaming will return as a proper flow later). The displayed
 * name is AUTO-COMPUTED (cwd basename + foreground program, live from
 * `terminal_info`) unless the user set a MANUAL label, which always wins. The
 * close button stops propagation so clicking `x` closes without also selecting.
 */
export function TerminalItemBody({
  record,
  index,
  active,
  ptyId = null,
  onClose,
}: TerminalItemProps) {
  // Live auto label (debounced via the backend cache + a fixed poll). Manual
  // label takes precedence inside resolveDisplayName below.
  const auto = useAutoLabel(ptyId);
  const name = resolveDisplayName(record, index, auto);
  // Live shell/program suffix for the proto row (`web · zsh`). Same backend poll.
  const shell = useShellSuffix(ptyId);

  // Anti-flicker (finding #14): debounce the `running` indicator so an instant
  // command (`running`→`success` within a few ms) never flashes the dot. Settled
  // (`success`/`error`) and `idle` pass through immediately — only a sustained
  // `running` (> threshold) reveals the dot. Identical for active/inactive rows
  // (the debounce keys off the raw state only). Persistence/unread are untouched.
  const state = useRunningDebounce(record.exec_state ?? "idle");
  // Settled-badge visibility is driven by the PERSISTED unread flag (PRD-2.1),
  // not by live selection — so a viewed badge stays hidden after re-deselecting.
  const unread = record.exec_state_unread ?? false;

  // Provider-aware lead glyph (finding #55): while this terminal hosts a LIVE agent
  // session, show the agent's brand logo (Claude) instead of the generic terminal icon,
  // reverting the moment the session ends. The active `agent_kind` comes from the shared
  // agent-sessions context (one subscription for all rows); an unknown/absent kind →
  // `undefined` provider → the terminal icon. Reactive: the context updates on
  // `agent-sessions://changed`, so the icon swaps live.
  const agentKind = useTerminalAgentKind(record.id);
  const provider = agentProviderFor(agentKind);
  const LeadIcon = provider?.icon ?? SquareTerminalIcon;

  return (
    // The SHARED item layout (lead → name → actions). The magenta selection bar is
    // not per-row: a single MEASURED rail (see `useActiveRail` / `<SelectionRail>`)
    // tracks the active row by `[data-rail-row][aria-current]`.
    <SidebarItemContent
      // Lead glyph + run-state corner badge (the run-state channel, orthogonal to
      // selection). A settled badge shows only while the PERSISTED `exec_state_unread`
      // flag is set (PRD-2.1); `running` always pulses; `idle` shows nothing — see
      // <TerminalStateBadge>. The wrapper is NOT `aria-hidden` so the badge's
      // `role="status"` stays in the a11y tree; only the icon is hidden.
      lead={
        <span
          className="relative flex shrink-0 items-center"
          // Label the glyph when it is an agent logo so the swap is discoverable to AT
          // (the generic terminal icon stays decorative / aria-hidden).
          title={provider?.label}
        >
          <LeadIcon
            aria-hidden
            className={cn("size-3.5", active ? "text-sidebar-foreground" : "text-muted-foreground")}
          />
          <TerminalStateBadge state={state} unread={unread} active={active} />
        </span>
      }
      name={name}
      // Shell/program suffix ("· zsh") — muted, hidden while there's no room.
      suffix={shell && <span className="shrink-0 text-xs text-muted-foreground">· {shell}</span>}
      actions={
        <Button
          variant="ghost-destructive"
          size="icon-xs"
          aria-label={`Close terminal ${name}`}
          // Stop propagation so closing never also selects the row (the row owns
          // the select click). `onPointerDown` stop keeps the drag controller from
          // treating an x-click as the start of a row drag.
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            onClose(record.id);
          }}
          className={cn(
            "size-5 shrink-0 rounded opacity-0 transition focus-visible:opacity-100 group-hover:opacity-100",
            active && "opacity-100",
          )}
        >
          <XIcon className="size-3.5" />
        </Button>
      }
    />
  );
}

/**
 * The LIVE sidebar terminal row: a height-collapsing `motion.li` wrapper hosting
 * the clickable visual row + `<TerminalItemBody>`. This is THE row the sidebar
 * renders everywhere now (the old `Reorder.Item`-based `<ReorderTerminalItem>` was
 * dropped — Motion's Reorder could not coexist with a clean height collapse).
 *
 * TWO LEVELS on purpose:
 *  - the OUTER `motion.li` is the SINGLE animator (height 0↔auto, opacity, a tiny
 *    translate via `itemVariants`) and carries NO `layout` prop, so the animated
 *    height reflows neighbours + parent bands in normal flow with nothing to fight
 *    (the double-tp fix). `overflow-hidden` clips the content as it collapses;
 *  - the INNER `div` is the visual, clickable row (its `py` padding lives INSIDE
 *    the clip, so the collapse reaches a true 0 — no padding residual / leftover
 *    pop). A click anywhere on it selects the terminal.
 *
 * Animation is chrome-only and never touches the xterm viewport (a hard rule).
 */
export function TerminalItem(props: TerminalItemProps) {
  const reduced = useReducedMotion();
  return (
    <motion.li
      variants={itemVariants}
      initial="initial"
      animate="animate"
      exit="exit"
      transition={itemTransition(reduced)}
      onClick={() => props.onSelect(props.record.id)}
      aria-current={props.active ? "true" : undefined}
      data-rail-row
      className="overflow-hidden"
      style={{ listStyle: "none" }}
    >
      <div className={terminalRowClassName(props.active)}>
        <TerminalItemBody {...props} />
      </div>
    </motion.li>
  );
}

export default TerminalItem;
