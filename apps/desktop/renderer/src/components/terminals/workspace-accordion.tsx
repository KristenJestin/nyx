import { useState } from "react";
import { ChevronRightIcon, FolderGit2Icon, GitBranchIcon } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { CollapsibleSection } from "@/components/sidebar/collapsible-section";
import { itemTransition } from "@/components/sidebar/item-motion";
import { useAgentSessions } from "@/components/sidebar/use-agent-sessions";
import type { WorkspaceRecord } from "@/components/sidebar/use-projects";
import { StatusDot } from "./status-dot";
import { workspaceRunState } from "./runtime-state";
import { useConsumedNotifications } from "./use-consumed-notifications";
import { WorkspaceRuntimeTree } from "./workspace-runtime-tree";
import type { WorkspaceRuntimeSnapshot } from "./use-workspace-runtime";

/**
 * `<WorkspaceAccordion>` — one workspace of the entered project, as an accordion item: a
 * header (chevron + icon + name + rollup run-state dot) over its runtime tree body. The
 * workspace is the runtime PIVOT in the DB (PDR-0008) but not a screen — here it is a
 * grouping INSIDE the project's Terminals page.
 *
 * The header icon distinguishes the ROOT workspace (a folder) from a branch workspace; the
 * rollup dot folds the workspace's whole runtime to one glanceable state (see
 * `workspaceRunState`). The body is `<WorkspaceRuntimeTree>` when the snapshot is loaded, else
 * a muted loading line. Every colour/size is a design-system token.
 */
export function WorkspaceAccordion({
  workspace,
  snapshot,
  defaultOpen = false,
  onNewTerminal,
  onNewCommand,
}: {
  workspace: WorkspaceRecord;
  /** The workspace's live runtime aggregate, or `null` until loaded. */
  snapshot: WorkspaceRuntimeSnapshot | null;
  defaultOpen?: boolean;
  /** Open a NEW terminal in THIS workspace (the tree's Terminals "+"). */
  onNewTerminal?: () => void;
  /** Open the project's command manager to CREATE a command (the tree's Commands gear). */
  onNewCommand?: () => void;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const reduced = useReducedMotion();
  const Icon = workspace.is_root ? FolderGit2Icon : GitBranchIcon;
  // The agent `waiting` (blocked-on-user) axis comes from the SAME `agent_activity_snapshot`
  // read model the sidebar rows use, folded into the workspace rollup so a hosted agent
  // awaiting input turns this accordion's dot yellow (finding: workspace + project dots).
  const { byActivity } = useAgentSessions();
  // The consumed-notifications registry (the SAME one the rows use) makes the rollup
  // notification-aware: a settled success/error the user has already visited stops contributing
  // to this accordion's dot, exactly as it stops contributing to its own row's dot.
  const { isConsumed } = useConsumedNotifications();
  const rollup = snapshot
    ? workspaceRunState(snapshot, (terminalId) => byActivity.get(terminalId)?.activity, isConsumed)
    : "idle";

  return (
    <div className="flex flex-col border-b border-border/60">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="group flex items-center gap-2 bg-muted/50 px-3 py-2 text-left text-sm font-medium text-foreground transition-colors hover:bg-muted"
      >
        <motion.span
          aria-hidden
          animate={{ rotate: open ? 90 : 0 }}
          transition={itemTransition(reduced)}
          className="flex shrink-0 items-center text-muted-foreground"
        >
          <ChevronRightIcon className="size-3.5" />
        </motion.span>
        <Icon className="size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate">{workspace.name}</span>
        {/* The rollup dot is a SUMMARY for a COLLAPSED workspace: once the accordion is OPEN
            (deployed) its rows are visible and each carries its own dot, so the aggregate would
            be redundant — masked while open, shown only when replied/collapsed (finding). It is
            still subject to `isQuietState` (StatusDot renders nothing when idle). */}
        <StatusDot state={open ? "idle" : rollup} size="sm" className="shrink-0" />
      </button>

      <CollapsibleSection open={open}>
        {snapshot ? (
          <WorkspaceRuntimeTree
            snapshot={snapshot}
            onNewTerminal={onNewTerminal}
            onNewCommand={onNewCommand}
          />
        ) : (
          <p className="px-4 py-2 text-sm italic text-muted-foreground/70">Loading runtime…</p>
        )}
      </CollapsibleSection>
    </div>
  );
}
