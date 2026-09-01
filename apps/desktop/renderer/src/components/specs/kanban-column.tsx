import { ChevronLeftIcon } from "lucide-react";
import { motion } from "motion/react";

import { statusMeta } from "./spec-meta";
import { SpecCardView } from "./spec-card";
import { Dot, MicroLabel } from "./spec-primitives";
import type { SpecListEntry, SpecStatus } from "./types";

const COLUMN_SPRING = { type: "spring", duration: 0.45, bounce: 0.2 } as const;

export interface BoardColumn {
  readonly status: SpecStatus;
  readonly specs: readonly SpecListEntry[];
}

/**
 * `<KanbanColumn>` — one status column on the board. Placement reproduced from the
 * prototype's `kanban-column`: an expanded state (fixed header = status dot + label + count
 * + chevron, then a scrolling cards area, or an "Empty" caption) that grows to fill, and a
 * COLLAPSED state showing a vertical label the user clicks to re-expand. Rewritten against
 * the design system — token colours only, the app's Motion spring idiom, no prototype
 * markup/pixels.
 */
export function KanbanColumn({
  col,
  collapsed,
  onToggle,
  showProject = false,
}: {
  col: BoardColumn;
  collapsed: boolean;
  onToggle: () => void;
  /** CROSS-PROJECT scope: each card opens on its project's mark. Passed straight to the card. */
  showProject?: boolean;
}) {
  const meta = statusMeta(col.status);
  return (
    <motion.div
      initial={false}
      // The column NAMES itself in the DOM: the board is the same page in two scopes, and
      // "the columns did not move when the scope flipped" is exactly what a test of that has to
      // be able to ask (`board-scope.test.tsx`).
      data-testid={`kanban-column-${col.status}`}
      // Motion doesn't reliably animate flex-basis; basis stays a static class (basis-12 =
      // 48px, the collapsed width) + shrink-0, and only flex-grow is animated (0 ↔ 1).
      animate={{ flexGrow: collapsed ? 0 : 1 }}
      transition={COLUMN_SPRING}
      className="relative flex min-w-0 shrink-0 basis-12 flex-col overflow-hidden rounded-xl border border-border/50 bg-muted/10"
    >
      {/* Expanded view — fixed header, the cards area scrolls on its own. */}
      <motion.div
        animate={{ opacity: collapsed ? 0 : 1 }}
        transition={{ duration: 0.18, delay: collapsed ? 0 : 0.14 }}
        className={`flex h-full w-full min-h-0 flex-col ${collapsed ? "pointer-events-none" : ""}`}
      >
        <button
          type="button"
          onClick={onToggle}
          className="flex shrink-0 items-center gap-2 px-3 pb-2 pt-3 text-left transition-colors hover:bg-muted/60"
        >
          <Dot tone={meta.dot} />
          <MicroLabel>{meta.label}</MicroLabel>
          <span className="font-mono text-xs text-muted-foreground/70">{col.specs.length}</span>
          <ChevronLeftIcon className="ml-auto size-3.5 text-muted-foreground/50" />
        </button>
        <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-2 pb-2">
          {col.specs.map((s) => (
            <SpecCardView key={s.id} spec={s} showProject={showProject} />
          ))}
          {col.specs.length === 0 ? (
            <p className="px-2 py-6 text-center text-xs text-muted-foreground/40">Empty</p>
          ) : null}
        </div>
      </motion.div>

      {/* Collapsed view — vertical label, always mounted, fades in. */}
      <motion.button
        type="button"
        onClick={onToggle}
        aria-hidden={!collapsed}
        tabIndex={collapsed ? 0 : -1}
        animate={{ opacity: collapsed ? 1 : 0 }}
        transition={{ duration: 0.18 }}
        className={`absolute inset-0 flex flex-col items-center gap-3 py-3 transition-colors ${
          collapsed ? "hover:bg-muted/60" : "pointer-events-none"
        }`}
      >
        <Dot tone={meta.dot} className="shrink-0" />
        <MicroLabel className="[writing-mode:vertical-rl]">{meta.label}</MicroLabel>
        <span className="font-mono text-xs text-muted-foreground/70">{col.specs.length}</span>
      </motion.button>
    </motion.div>
  );
}
