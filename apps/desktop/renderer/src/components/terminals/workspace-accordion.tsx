import { useEffect, useState } from "react";
import {
  ChevronRightIcon,
  FolderGit2Icon,
  GitBranchIcon,
  MoreVerticalIcon,
  PencilIcon,
  Trash2Icon,
} from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { Button } from "@/components/ui/button";
import { Menu, MenuItem } from "@/components/ui/menu";
import { CollapsibleSection } from "@/components/sidebar/collapsible-section";
import { itemTransition } from "@/components/sidebar/item-motion";
import { useAgentSessions } from "@/components/sidebar/use-agent-sessions";
import type { WorkspaceRecord } from "@/components/sidebar/use-projects";
import {
  LINE_GLYPH_SIZE,
  rollupState,
  SidebarLine,
  SidebarLineActions,
  SidebarLineBand,
  SidebarLineGlyph,
  SidebarLineLabel,
} from "@/components/ui/sidebar-line";
import { useRevealWorkspace } from "./reveal-workspace";
import { workspaceRunState } from "./runtime-state";
import { useConsumedNotifications } from "./use-consumed-notifications";
import { WorkspaceRuntimeTree } from "./workspace-runtime-tree";
import type { WorkspaceRuntimeSnapshot } from "@/components/runtime/use-workspace-runtime";

/**
 * `<WorkspaceAccordion>` — one workspace of the entered project, as an accordion item: a
 * header — the SHARED `<SidebarLine>`: chevron, icon, name, and the rollup run-state pastille in
 * the icon's corner — over its runtime tree body. The
 * workspace is the runtime PIVOT in the DB (PDR-0008) but not a screen — here it is a
 * grouping INSIDE the project's Terminals page.
 *
 * The header icon distinguishes the ROOT workspace (a folder) from a branch workspace; the
 * rollup dot folds the workspace's whole runtime to one glanceable state (see
 * `workspaceRunState`). The body is `<WorkspaceRuntimeTree>` when the snapshot is loaded, else
 * a muted loading line. Every colour/size is a design-system token.
 *
 * IT OPENS BY RANK, OR BY REVEAL. The page starts the FIRST workspace open (`defaultOpen`) and
 * leaves the rest closed; from there the band is the user's to fold. The one other thing that opens
 * it is a REVEAL (`./reveal-workspace`): the search palette announcing that the user is landing on
 * an entity inside this workspace, so the band cannot be the thing hiding it. That signal is
 * ephemeral and consumed on arrival — it never becomes a stored preference, and a fold performed
 * after it is honoured.
 *
 * LIFECYCLE ACTIONS (review `01M04WQEQDDPH7NQEK9MPYCXKS`): the header carries a hover-revealed
 * kebab with the workspace's OWN rename/remove. Before it, both lived only in the PROJECT's
 * right-click menu — so from a worktree there was, in the user's words, "aucun moment" to rename
 * or drop it: one had to leave the worktree, find its project's tile and pick it out of a list.
 * The kebab is a SIBLING of the toggle button, never nested inside it (a button cannot contain a
 * button), which is also why the toggle keeps `aria-expanded` for itself.
 *
 * ON A ROOT WORKSPACE THERE IS NO KEBAB (review `01M0A7QGBK43MZB10RF51QR6TS`) — not a disabled
 * entry, not the three dots. The root cannot be removed on its own (it leaves with its project)
 * and its rename went with it by the user's explicit call, so the trigger would open onto nothing
 * actionable. Same rule at the other entry point, the project rail's per-workspace groups
 * (`base-layout.tsx`).
 */
export function WorkspaceAccordion({
  workspace,
  snapshot,
  defaultOpen = false,
  onNewTerminal,
  onNewCommand,
  onOpenCommandDetails,
  onRename,
  onRemove,
}: {
  workspace: WorkspaceRecord;
  /** The workspace's live runtime aggregate, or `null` until loaded. */
  snapshot: WorkspaceRuntimeSnapshot | null;
  defaultOpen?: boolean;
  /** Open a NEW terminal in THIS workspace (the tree's Terminals "+"). */
  onNewTerminal?: () => void;
  /** Open the project's command manager to CREATE a command (the tree's Commands gear). */
  onNewCommand?: () => void;
  /**
   * Open the command manager ON ONE COMMAND (by template id) — the row-level `…` ▸ Details.
   * Passed straight through to the runtime tree, which owns the row.
   */
  onOpenCommandDetails?: (commandId: string) => void;
  /**
   * Open the RENAME dialog for this workspace (`rename_workspace`). Never reached on a ROOT
   * workspace: the kebab that would offer it is not rendered there at all.
   */
  onRename?: (workspace: WorkspaceRecord) => void;
  /**
   * Open the destructive-confirm flow for this workspace (`workspace_delete`). Non-root only —
   * the backend refuses to remove a root (it leaves with its project), and rather than showing
   * that as a disabled row the whole kebab is withheld on a root (see `hasActions`).
   */
  onRemove?: (workspace: WorkspaceRecord) => void;
}) {
  // THE BAND'S OWN OPEN STATE — local, and deliberately not persisted. `defaultOpen` is the
  // Terminals page's rank rule (the first workspace of the project starts open), and a toggle from
  // here is the user's, remembered for as long as the band is mounted and no longer.
  //
  // ONE THING ELSE CAN OPEN IT: a REVEAL — the global search palette signalling that the user is
  // arriving on an entity that lives in THIS workspace. Honoured in the initial state, not only in
  // the effect below, so a band mounted by the very navigation that raised the signal is open in
  // its FIRST paint: the target must be there when the page appears, never a beat later.
  const { revealWorkspaceId, clearReveal } = useRevealWorkspace();
  const [open, setOpen] = useState(() => defaultOpen || revealWorkspaceId === workspace.id);
  // …and the same signal for a band ALREADY mounted (a jump inside the project one is already in,
  // where no remount happens). It is CONSUMED here — `clearReveal` scopes the clear to this
  // workspace — so it opens the band exactly once: a collapse the user performs afterwards stays,
  // and a later remount of this band is not re-opened by a signal that already did its work.
  // Nothing is written anywhere; see `./reveal-workspace` for why this is a signal and not the
  // persisted `collapsed` flag.
  useEffect(() => {
    if (revealWorkspaceId !== workspace.id) return;
    setOpen(true);
    clearReveal(workspace.id);
  }, [revealWorkspaceId, workspace.id, clearReveal]);
  const reduced = useReducedMotion();
  const Icon = workspace.is_root ? FolderGit2Icon : GitBranchIcon;
  // The live per-terminal agent activity from the SAME `agent_activity_snapshot` read model the
  // sidebar rows + the terminal LINES use, folded into the workspace rollup so a hosted agent
  // contributes its AGENT-AWARE state (working→running, waiting→yellow, ready→green, error→red;
  // busy ignored) — the SAME projection each line shows — not the OS busy bit (finding: the rollup
  // must agree with the lines). The FULL activity is threaded (not just the kind) so the agent's
  // settled ready/error notification remounts to this accordion's dot too.
  const { byActivity } = useAgentSessions();
  // The consumed-notifications registry (the SAME one the rows use) makes the rollup
  // notification-aware: a settled success/error the user has already visited stops contributing
  // to this accordion's dot, exactly as it stops contributing to its own row's dot.
  const { isConsumed } = useConsumedNotifications();
  const rollup = snapshot
    ? workspaceRunState(snapshot, (terminalId) => byActivity.get(terminalId), isConsumed)
    : "idle";

  // The kebab is worth mounting only when the page threaded at least one lifecycle action —
  // AND never on the ROOT workspace (review `01M0A7QGBK43MZB10RF51QR6TS`). The root used to open
  // a menu holding an active Rename and a DISABLED Remove with its reason: a dead end that cost
  // a click to learn there was nothing to do. Its lifecycle is the project's — it is created and
  // removed with it — so the trigger itself is gone, and the disabled entry and its note go with
  // it. Rendering nothing here is the whole point: a kebab that opens onto one greyed row is
  // worse than no kebab.
  const hasActions = !workspace.is_root && (onRename != null || onRemove != null);

  return (
    <div className="flex flex-col border-b border-border/60">
      {/* THE WORKTREE IS A LINE TOO (human review 01M12MW1DV1V455EWBMV048TE6). It keeps its own
          SURFACE — the `bg-muted/50` band and the bottom rule that separate one workspace from the
          next — and takes its GEOMETRY from `<SidebarLine>` like every other family: same glyph
          size, same insets, same gap, same height, same corner pastille. It used to hand-roll
          `gap-2 px-3 py-2` and hang a larger dot at the far END of the band, which is the kind of
          near-miss that made the parity worth making structural.
          The band is a `<button>` (nothing inside it is interactive), so the header still toggles
          as one tab stop; the lifecycle kebab rides the line's own trailing slot
          (`<SidebarLineActions>`) — a SIBLING of the band, never nested in it, since a button
          cannot contain a button. */}
      <SidebarLine
        className="rounded-none bg-muted/50 pr-2 font-medium text-foreground hover:bg-muted"
        actions={
          hasActions ? (
            <SidebarLineActions>
              <Menu
                tooltip="Workspace actions"
                trigger={
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    aria-label={`Workspace actions for ${workspace.name}`}
                    className="size-6"
                  >
                    <MoreVerticalIcon />
                  </Button>
                }
              >
                {onRename && (
                  <MenuItem
                    icon={<PencilIcon className="size-4" />}
                    onClick={() => onRename(workspace)}
                    aria-label={`Rename workspace ${workspace.name}`}
                  >
                    Rename workspace
                  </MenuItem>
                )}
                {/* No root special-casing left: `hasActions` already keeps the whole kebab off a
                    root workspace, so every workspace that reaches here can really be removed. */}
                {onRemove && (
                  <MenuItem
                    destructive
                    icon={<Trash2Icon className="size-4" />}
                    onClick={() => onRemove(workspace)}
                    aria-label={`Remove workspace ${workspace.name}`}
                  >
                    Remove workspace
                  </MenuItem>
                )}
              </Menu>
            </SidebarLineActions>
          ) : null
        }
      >
        <SidebarLineBand
          render={
            <button
              type="button"
              aria-expanded={open}
              // NAMED explicitly (mirroring the old sidebar's workspace band): with a kebab now
              // sitting next to it, "the button whose text is the workspace name" no longer
              // identifies one element — the toggle and its actions menu would both answer to it.
              aria-label={`Toggle workspace ${workspace.name}`}
              onClick={() => setOpen((v) => !v)}
              className="text-left"
            />
          }
        >
          <motion.span
            aria-hidden
            animate={{ rotate: open ? 90 : 0 }}
            transition={itemTransition(reduced)}
            className="flex shrink-0 items-center text-muted-foreground"
          >
            <ChevronRightIcon className="size-3.5" />
          </motion.span>
          {/* The rollup pastille is a SUMMARY for a COLLAPSED workspace: once the accordion is OPEN
              its rows are visible and each carries its own dot, so the aggregate would be redundant.
              That rule is `rollupState`, shared with the group folders rather than restated here,
              and it still rides `isQuietState` (an idle workspace shows nothing at all). */}
          <SidebarLineGlyph
            icon={<Icon className={LINE_GLYPH_SIZE} />}
            state={rollupState(open, rollup)}
          />
          <SidebarLineLabel>{workspace.name}</SidebarLineLabel>
        </SidebarLineBand>
      </SidebarLine>

      <CollapsibleSection open={open}>
        {snapshot ? (
          <WorkspaceRuntimeTree
            snapshot={snapshot}
            onNewTerminal={onNewTerminal}
            onNewCommand={onNewCommand}
            onOpenCommandDetails={onOpenCommandDetails}
            // The live agent-activity lookup (review T2) — the SAME `byActivity` map that feeds
            // this accordion's rollup dot, threaded to each terminal LINE so a hosted agent shows
            // its working/waiting/ready dot on the line, not just at the rollup.
            agentActivity={(terminalId) => byActivity.get(terminalId)}
          />
        ) : (
          <p className="px-4 py-2 text-sm italic text-muted-foreground/70">Loading runtime…</p>
        )}
      </CollapsibleSection>
    </div>
  );
}
