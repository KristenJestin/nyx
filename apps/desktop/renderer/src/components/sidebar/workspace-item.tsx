import { useState } from "react";
import { ChevronRightIcon, MoreVerticalIcon, Trash2Icon } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { Button } from "@/components/ui/button";
import { Menu, MenuItem } from "@/components/ui/menu";
import { CollapsibleSection } from "./collapsible-section";
import { WorkspaceSubsections } from "./workspace-subsections";
import { itemTransition } from "./item-motion";
import type { CommandRecord, WorkspaceRecord } from "./use-projects";
import type { TerminalRecord } from "./use-terminals";

export interface WorkspaceItemProps {
  workspace: WorkspaceRecord;
  /**
   * The label to DISPLAY for this workspace row (the "smart default" relabel —
   * `"main"` for the root, the distinguishing name otherwise). Falls back to
   * `workspace.name` when omitted.
   */
  displayLabel?: string;
  /** Terminals bound to this workspace, in sidebar order. */
  terminals: TerminalRecord[];
  /** Commands (PRD-3 instances) for this workspace; empty hides the COMMANDS band. */
  commands?: CommandRecord[];
  activeId: string | null;
  /** Record id of the active command row (drives the shared selection rail). */
  activeCommandId?: string | null;
  ptyIds?: Map<string, number | null>;
  /** Whether to render this workspace's own header row. Hidden for the implicit
   *  single-root case where the project row already stands in for the root. */
  showHeader?: boolean;
  /** Initial expanded state of the subsections (defaults to open). */
  defaultOpen?: boolean;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  /** Launch a new terminal in this workspace (cwd = workspace.path). */
  onNewTerminal: (workspace: WorkspaceRecord) => void;
  /** Select a command row in this workspace (mounts its view in the main pane). */
  onSelectCommand?: (id: string) => void;
  /** Open the manage-commands modal (the COMMANDS band's hover gear). */
  onManageCommands?: () => void;
  /**
   * Remove THIS workspace (opens the delete-confirm flow). When provided AND the
   * workspace is NOT the project root, the header shows a hover-revealed kebab
   * with a destructive "Remove workspace" action. The ROOT never offers it — the
   * backend rejects removing a root (delete the whole project instead), so the
   * action is hidden there to keep the UI honest.
   */
  onRemove?: (workspace: WorkspaceRecord) => void;
  /** Persist a new order for this workspace's terminals (within-workspace only). */
  onReorderTerminals?: (workspaceId: string, ids: string[]) => void;
  /**
   * Persist THIS workspace's open/closed band state so it survives a restart.
   * Called on every header toggle with the new `collapsed` value.
   */
  onSetCollapsed?: (id: string, collapsed: boolean) => void;
}

/**
 * `<WorkspaceItem>` — one workspace in the sidebar spine: a header row (its
 * display label + a chevron toggling the typed subsections) over the animated
 * Terminals/Commands subsections (`<WorkspaceSubsections>`).
 *
 * The label is the "smart default" relabel (`displayLabel`, e.g. `"main"` for the
 * root). There is NO inline rename any more (finding 01KV3CNPDMBDWYKZZKPJ8RWKQX
 * removed double-click editing across the sidebar; a proper rename flow returns
 * later). The label simply toggles the workspace's collapse — workspace bands
 * KEEP their collapse (only the typed Terminals subsection lost its chevron).
 *
 * `showHeader=false` renders the subsections WITHOUT the workspace header — used
 * by the project row when the project is mono-(root)workspace, so the workspace
 * section is hidden and the project expands straight into the root's subsections.
 */
export function WorkspaceItem({
  workspace,
  displayLabel,
  terminals,
  commands,
  activeId,
  activeCommandId,
  ptyIds,
  showHeader = true,
  defaultOpen = true,
  onSelect,
  onClose,
  onNewTerminal,
  onSelectCommand,
  onManageCommands,
  onRemove,
  onReorderTerminals,
  onSetCollapsed,
}: WorkspaceItemProps) {
  // Initialize from the PERSISTED `collapsed` flag (open = !collapsed) so the
  // disclosure is restored on reload; `defaultOpen` is the no-flag fallback.
  const [open, setOpen] = useState(
    workspace.collapsed != null ? !workspace.collapsed : defaultOpen,
  );
  const reduced = useReducedMotion();

  const label = displayLabel ?? workspace.name;

  // Toggle the band AND persist the new disclosure (open → collapsed=true).
  const toggleOpen = () => {
    setOpen((v) => {
      const next = !v;
      onSetCollapsed?.(workspace.id, !next);
      return next;
    });
  };

  const subsections = (
    <WorkspaceSubsections
      terminals={terminals}
      commands={commands}
      activeId={activeId}
      activeCommandId={activeCommandId}
      ptyIds={ptyIds}
      onSelect={onSelect}
      onClose={onClose}
      onSelectCommand={onSelectCommand}
      onManageCommands={onManageCommands}
      onNewTerminal={() => onNewTerminal(workspace)}
      onReorderTerminals={(ids) => onReorderTerminals?.(workspace.id, ids)}
    />
  );

  // No header (implicit single-root): the subsections render directly, always
  // visible (the project row owns the collapse above this point).
  if (!showHeader) {
    return <div className="pt-0.5">{subsections}</div>;
  }

  // Collapsed-band counter: total terminals in this workspace, shown on the 1-line
  // collapsed band so the user can see activity without expanding.
  const count = terminals.length;

  // The destructive "Remove workspace" action is offered ONLY for a non-root
  // workspace with a handler. The root can never be removed on its own (the
  // backend rejects it — delete the whole project instead), so the action is
  // simply absent there (root guard reflected in the UI).
  const canRemove = onRemove != null && !workspace.is_root;

  return (
    // NO `layout` prop: the rows animate a REAL height collapse (see
    // `item-motion.ts`), so this band's size follows in NORMAL DOCUMENT FLOW and
    // siblings reflow on their own. A `layout` projection here would be a SECOND
    // animator over that flow — the double-tp we removed.
    <motion.li className="mt-0.5 flex flex-col">
      {/* Quiet WORKSPACE sub-band (proto's `.wband`): a faint fill + uppercase
          micro-label, distinctly lighter than the project band. The toggle and
          the kebab are SIBLINGS (a button cannot nest a button), so the toggle
          button itself carries `aria-expanded`. */}
      <div className="group flex items-center rounded-md bg-sidebar-foreground/3 hover:bg-sidebar-foreground/6">
        <button
          type="button"
          aria-expanded={open}
          onClick={toggleOpen}
          aria-label={`Toggle workspace ${label}`}
          className="flex min-w-0 flex-1 items-center gap-1.5 px-1.5 py-1 text-left text-sidebar-foreground select-none"
        >
          <motion.span
            aria-hidden
            animate={{ rotate: open ? 90 : 0 }}
            transition={itemTransition(reduced)}
            className="flex shrink-0 items-center text-muted-foreground"
          >
            <ChevronRightIcon className="size-3" />
          </motion.span>
          <span className="min-w-0 flex-1 truncate text-xs font-semibold tracking-wider text-muted-foreground uppercase">
            {label}
          </span>
          {!open && count > 0 && (
            <span className="shrink-0 pr-1 text-xs text-muted-foreground/60 tabular-nums">
              {count}
            </span>
          )}
        </button>

        {/* Hover-revealed kebab carrying the destructive workspace action (mirrors
            the project header's kebab). Absent for the root. */}
        {canRemove && (
          <div className="shrink-0 pr-1 opacity-0 transition group-hover:opacity-100 focus-within:opacity-100 data-popup-open:opacity-100">
            <Menu
              tooltip="Workspace actions"
              trigger={
                <Button
                  variant="ghost"
                  size="icon-xs"
                  aria-label={`Workspace actions for ${label}`}
                  className="size-5"
                >
                  <MoreVerticalIcon />
                </Button>
              }
            >
              <MenuItem
                destructive
                icon={<Trash2Icon className="size-4" />}
                onClick={() => onRemove?.(workspace)}
                aria-label={`Remove workspace ${label}`}
              >
                Remove workspace
              </MenuItem>
            </Menu>
          </div>
        )}
      </div>
      <CollapsibleSection open={open} className="pt-0.5">
        {subsections}
      </CollapsibleSection>
    </motion.li>
  );
}
