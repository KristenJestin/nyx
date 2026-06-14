import { useState } from "react";
import { ChevronRightIcon, PlusIcon } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { CollapsibleSection } from "./collapsible-section";
import { SortableTerminalList } from "./sortable-terminal-list";
import { StatusDot } from "./run-state";
import { itemTransition } from "./item-motion";
import type { CommandRecord } from "./use-projects";
import type { TerminalRecord } from "./use-terminals";

export interface WorkspaceSubsectionsProps {
  /** Terminals bound to this workspace (via `workspace_id`), in sidebar order. */
  terminals: TerminalRecord[];
  /** Record id of the globally-active terminal (highlighted if it's here). */
  activeId: string | null;
  /** Live record→PTY id map for the auto label (see `<AppSidebar>`). */
  ptyIds?: Map<string, number | null>;
  /**
   * Commands for this workspace. Empty/absent until PRD-3 populates them — when
   * empty the COMMANDS subsection is NOT rendered at all (empty-state polish).
   */
  commands?: CommandRecord[];
  /** Record id of the active command row (drives the shared ActiveRail). */
  activeCommandId?: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  /** Select a command row (rail glides to it; no create here). */
  onSelectCommand?: (id: string) => void;
  /** Launch a new terminal scoped to THIS workspace (cwd = workspace.path). */
  onNewTerminal: () => void;
  /**
   * Persist a new order for THIS workspace's terminals (the dragged id sequence,
   * within-workspace only). Optional so the subsections still render without
   * reorder wiring (isolation tests).
   */
  onReorderTerminals?: (ids: string[]) => void;
}

/**
 * The TYPED subsections under a workspace — the v6 Terminals/Commands bands, all
 * text in ENGLISH:
 *
 *  - **TERMINALS**: a quiet label header + a compact `+` that launches a terminal
 *    IN THIS workspace. It is ALWAYS OPEN — there is NO chevron and NO collapse
 *    (finding 01KV3CNH1HVAX8RG08GYSEWJFG: a prior round wrongly made it
 *    collapsible by misreading a cut-off message; the user wants it permanently
 *    expanded). The body is the workspace's terminals as a drag-sortable
 *    `<SortableTerminalList>` (dnd-kit) whose closed rows run a height-collapse
 *    EXIT and whose survivors reflow up in normal flow. Empty → a muted hint.
 *  - **COMMANDS**: rendered ONLY when commands exist (hidden until PRD-3 feeds
 *    it). It KEEPS its chevron/collapse. No `+`. Each row is a `<CommandRow>`
 *    carrying a lead `<StatusDot>` (run-state) + the shared selection rail.
 *
 * (Project and workspace bands keep their own collapse — only the typed Terminals
 * subsection is non-collapsible.)
 */
export function WorkspaceSubsections({
  terminals,
  activeId,
  ptyIds,
  commands = [],
  activeCommandId,
  onSelect,
  onClose,
  onSelectCommand,
  onNewTerminal,
  onReorderTerminals,
}: WorkspaceSubsectionsProps) {
  const reduced = useReducedMotion();
  const [cmdsOpen, setCmdsOpen] = useState(true);

  return (
    // FULL-WIDTH band: no left inset, so terminal rows span the sidebar
    // edge-to-edge like the project/workspace header bands.
    <div className="flex flex-col gap-0.5">
      {/* --- TERMINALS (ALWAYS OPEN: no chevron, no collapse — finding F) ---- */}
      <div>
        <div className="group/sub flex items-center gap-1 pr-1 pl-1">
          <span className="flex min-w-0 flex-1 items-center gap-1 py-0.5 select-none">
            <span className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">
              Terminals
            </span>
          </span>
          <Tooltip label="New terminal in this workspace">
            <Button
              variant="ghost"
              size="icon-xs"
              aria-label="New terminal in workspace"
              onClick={onNewTerminal}
              className="size-5 opacity-0 transition group-hover/sub:opacity-100 focus-visible:opacity-100"
            >
              <PlusIcon />
            </Button>
          </Tooltip>
        </div>
        {terminals.length > 0 ? (
          // Drag-sortable list (dnd-kit) wrapped in AnimatePresence so a closed
          // row runs its height-collapse exit and the survivors reflow up.
          <SortableTerminalList
            terminals={terminals}
            activeId={activeId}
            ptyIds={ptyIds}
            onSelect={onSelect}
            onClose={onClose}
            onReorder={onReorderTerminals}
            className="flex flex-col pt-0.5"
          />
        ) : (
          // Empty TERMINALS: a subtle, intentional-looking muted hint.
          <p className="px-2 py-1 text-xs text-muted-foreground/70 italic select-none">
            No terminals — + to start
          </p>
        )}
      </div>

      {/* --- COMMANDS (hidden until it has content; fed in PRD-3) ------ */}
      {commands.length > 0 && (
        <div>
          <button
            type="button"
            aria-expanded={cmdsOpen}
            onClick={() => setCmdsOpen((v) => !v)}
            className="flex w-full items-center gap-1 rounded-md px-1 py-0.5 text-left select-none hover:bg-sidebar-accent/30"
          >
            <motion.span
              aria-hidden
              animate={{ rotate: cmdsOpen ? 90 : 0 }}
              transition={itemTransition(reduced)}
              className="flex shrink-0 items-center text-muted-foreground"
            >
              <ChevronRightIcon className="size-3" />
            </motion.span>
            <span className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">
              Commands
            </span>
          </button>
          <CollapsibleSection open={cmdsOpen}>
            <ul className="flex flex-col gap-0.5 pt-0.5">
              {commands.map((c) => (
                <CommandRow
                  key={c.id}
                  command={c}
                  active={c.id === activeCommandId}
                  onSelect={onSelectCommand}
                />
              ))}
            </ul>
          </CollapsibleSection>
        </div>
      )}
    </div>
  );
}

interface CommandRowProps {
  command: CommandRecord;
  active: boolean;
  onSelect?: (id: string) => void;
}

/**
 * `<CommandRow>` — a single command in the COMMANDS subsection. Selection is the
 * shared `<ActiveRail>` (a `layoutId` bar that FLIPs here when active) + the v6
 * dimmed/active model, exactly like a terminal row (one selection channel across
 * both). Lead glyph is the run-state `<StatusDot>` (idle live this PRD). No create
 * affordance here; a click selects the command. Controls are PRD-3.
 */
function CommandRow({ command, active, onSelect }: CommandRowProps) {
  const state = command.state ?? "idle";
  return (
    <li>
      <button
        type="button"
        onClick={() => onSelect?.(command.id)}
        aria-current={active ? "true" : undefined}
        data-rail-row
        className={cn(
          "group relative flex w-full items-center gap-2 rounded-md py-1 pr-2 pl-5.5 text-left text-sm transition select-none",
          active
            ? "font-medium text-sidebar-foreground opacity-100 hover:bg-sidebar-accent/40"
            : "text-sidebar-foreground/70 opacity-60 hover:bg-sidebar-accent/40 hover:opacity-90",
        )}
      >
        {/* Selection is the single MEASURED rail (see `useActiveRail`); this row
            just flags itself active via `aria-current` + `data-rail-row`. */}
        <StatusDot state={state} className="relative" />
        <span className="min-w-0 flex-1 truncate">{command.label}</span>
      </button>
    </li>
  );
}
