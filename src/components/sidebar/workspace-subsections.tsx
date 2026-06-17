import { useState } from "react";
import { PlusIcon, Settings2Icon } from "lucide-react";

import { CommandControls } from "@/components/command/command-controls";
import { SortableTerminalList } from "./sortable-terminal-list";
import { StatusDot } from "./run-state";
import { SidebarItemContent, sidebarRowClassName } from "./sidebar-item";
import { SidebarSection } from "./sidebar-section";
import type { CommandRecord } from "./use-projects";
import type { ExecState, TerminalRecord } from "./use-terminals";

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
  /**
   * Open the "Manage commands" modal (the COMMANDS band's hover gear). Wired to the
   * EXISTING `project-commands-dialog` (its internals are out of scope — redesigned
   * separately). Optional so the subsections still render without the wiring.
   */
  onManageCommands?: () => void;
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
 * text in ENGLISH. Both bands now share ONE non-collapsible `<SidebarSection>`
 * (finding 01KV63TD5E…): a quiet uppercase label + a single hover-revealed action
 * icon, NO chevron / NO collapse on EITHER.
 *
 *  - **TERMINALS**: ALWAYS OPEN (no chevron — finding 01KV3CNH1HVAX8RG08GYSEWJFG).
 *    Its hover action is the `+` that launches a terminal IN THIS workspace. The
 *    body is the workspace's terminals as a drag-sortable `<SortableTerminalList>`
 *    (dnd-kit) whose closed rows height-collapse and whose survivors reflow up.
 *    Empty → a muted hint.
 *  - **COMMANDS**: rendered ONLY when commands exist. It NO LONGER carries a chevron
 *    / collapse — it is the SAME non-collapsible band as TERMINALS now. Its hover
 *    action is a GEAR that opens the manage-commands modal (the existing
 *    `project-commands-dialog`). Each row is a `<CommandRow>` carrying a lead
 *    `<StatusDot>` (run-state) + the shared selection rail.
 *
 * (Project and workspace bands keep their own collapse — the typed Terminals/
 * Commands subsections are the non-collapsible ones.)
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
  onManageCommands,
  onNewTerminal,
  onReorderTerminals,
}: WorkspaceSubsectionsProps) {
  return (
    // FULL-WIDTH band: no left inset, so terminal rows span the sidebar
    // edge-to-edge like the project/workspace header bands.
    <div className="flex flex-col gap-0.5">
      {/* --- TERMINALS (shared non-collapsible section; `+` action) ---------- */}
      <SidebarSection
        title="Terminals"
        action={{
          icon: <PlusIcon />,
          label: "New terminal in workspace",
          onClick: onNewTerminal,
        }}
      >
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
      </SidebarSection>

      {/* --- COMMANDS (shared non-collapsible section; gear opens the modal) --
          Rendered only when commands exist. The gear is shown only when there is a
          handler wired (`onManageCommands`). */}
      {commands.length > 0 && (
        <SidebarSection
          title="Commands"
          action={
            onManageCommands
              ? {
                  icon: <Settings2Icon />,
                  label: "Manage commands",
                  onClick: onManageCommands,
                }
              : undefined
          }
        >
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
        </SidebarSection>
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
 * `<CommandRow>` — a single command in the COMMANDS subsection. It uses the SHARED
 * sidebar item gabarit (`sidebarRowClassName` + `<SidebarItemContent>`), so a command
 * row is the SAME size/alignment as a terminal row: same lead/name/actions structure,
 * NO command-specific `pl-5.5` inset (finding 01KV63TBV7…). Selection is the shared
 * measured `<ActiveRail>` + the v6 dimmed/active model, exactly like a terminal row
 * (one selection channel). Lead glyph is the run-state `<StatusDot>`.
 *
 * The actions slot hosts the REUSED `<CommandControls>` (the SAME lifecycle commands
 * as the main view, with the same state gating: start when not running, stop when
 * running, relaunch always — finding 01KV63TEGB…). The buttons reveal on row hover
 * (like the terminal close `x`), and each stops propagation so acting never also
 * selects the row. A lifecycle failure (finding 01KV63TAG…) is reflected on the lead
 * dot so a refused action is visible even from the sidebar.
 *
 * SETTLED-BADGE / UNREAD (PRD-4 v4): the lead dot's SETTLED success/error emphasis is
 * the "unseen result" notification, driven by `command.unread` — mirroring 2.1's
 * `TerminalStateBadge` unread model. Once acknowledged (`unread=false`), the settled
 * badge HIDES (the dot falls back to the neutral idle fill) while the row STILL
 * reflects the FACTUAL state via `data-state` (and the command view/info bar keep
 * showing the true last-run outcome). A `running` dot always shows (it is live, not a
 * notification); a refused action forces the error dot regardless of unread.
 */
function CommandRow({ command, active, onSelect }: CommandRowProps) {
  const state = command.state ?? "idle";
  const unread = command.unread ?? false;
  // A refused lifecycle action surfaces on the lead dot (the row has no output
  // panel). Cleared when a fresh live state arrives.
  const [failed, setFailed] = useState(false);
  const [seenState, setSeenState] = useState(state);
  if (seenState !== state) {
    setSeenState(state);
    if (failed) setFailed(false);
  }
  // A refused action always shows the error dot. Otherwise the dot shows the FACTUAL
  // state, EXCEPT a SETTLED (success/error) result that has been acknowledged
  // (`!unread`): its notification badge hides, so the dot reverts to the neutral idle
  // fill. `running` is live (never a notification) so it always shows. The factual
  // state stays observable on `data-state` (see `<StatusDot>`), so the row still
  // reflects the true outcome even when the badge is hidden.
  const settled = state === "success" || state === "error";
  const dotState: ExecState = failed ? "error" : settled && !unread ? "idle" : state;

  return (
    <li>
      {/* The row is a `div role="button"` (NOT a `<button>`) so the lifecycle
          control buttons can nest inside it — a `<button>` may not contain buttons.
          This mirrors the terminal row, whose clickable element is also a non-button
          (the `<li>`/`div` owns the select click). Enter/Space activate it. */}
      <div
        role="button"
        tabIndex={0}
        onClick={() => onSelect?.(command.id)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onSelect?.(command.id);
          }
        }}
        aria-current={active ? "true" : undefined}
        data-rail-row
        // The SHARED row shape — identical to a terminal row.
        className={sidebarRowClassName(active)}
      >
        {/* Selection is the single MEASURED rail (see `useActiveRail`); this row
            just flags itself active via `aria-current` + `data-rail-row`. The lead
            dot carries the factual state on `data-state`; `state` here drives only the
            VISIBLE fill (settled badge hidden once acknowledged). */}
        <SidebarItemContent
          lead={<StatusDot state={dotState} factualState={state} className="relative" />}
          name={command.label}
          actions={
            <CommandControls
              instanceId={command.id}
              state={state}
              showDot={false}
              buttonSize="icon-xs"
              inRow
              onStateChange={() => setFailed(false)}
              onError={() => setFailed(true)}
              // Hover-reveal at the right edge (matches the terminal row's close).
              className="shrink-0 opacity-0 transition focus-within:opacity-100 group-hover:opacity-100"
            />
          }
        />
      </div>
    </li>
  );
}
