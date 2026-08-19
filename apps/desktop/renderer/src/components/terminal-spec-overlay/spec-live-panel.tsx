import { ChevronRightIcon, FileTextIcon } from "lucide-react";

import { Spinner } from "@/components/ui/spinner";
import { Badge } from "@/components/ui/badge";

import { specKey, statusMeta } from "../specs/spec-meta";
import { MicroLabel } from "../specs/spec-primitives";
import { SpecTab } from "../specs/tabs/spec-tab";
import { TasksTab } from "../specs/tabs/tasks-tab";
import { parsePhases, type Spec } from "../specs/types";
import { useSpecDetail } from "../specs/use-spec-detail";
import { OverlayPanelActions } from "./overlay-panel-actions";
import { OverlayPrototypes } from "./overlay-prototypes";

/**
 * `<SpecLivePanel>` — the OPEN tiroir of the creation overlay (B): the Spec being AUTHORED,
 * shown building itself in real time. Order = prototype `spec-live` round v6:
 *
 *  1. Header — a doc icon + human key/title + the Spec's REAL status badge (`statusMeta`, the same
 *     Draft/Ready/In progress/… chip as the rail and the Spec detail page). NO agent logo, NO "go
 *     to the Spec" button (the drawer IS the live view).
 *  2. The Spec's real sections (Problem / Solution / … ), reusing `<SpecTab>` so they fill in
 *     LIVE as the agent writes them (the detail re-pulls on `specs://changed`).
 *  3. "Decomposed tasks" — the task decomposition rendered FIDÈLE by reusing `<TasksTab>` (same
 *     roll-up + phase cards + rows as the real Spec detail and the rail A panel).
 *  4. Footer — a STATUS line only (phase/task tally). No CTA (PRD: footer = statut).
 *
 * The whole panel is driven by `useSpecDetail`, which re-fetches on `specs://changed` with NO
 * polling — so sections and streamed tasks appear as the agent creates them.
 */
export function SpecLivePanel({ spec, onClose }: { spec: Spec; onClose: () => void }) {
  const { detail, loading } = useSpecDetail(spec.id);
  const title = spec.title || `#${spec.number}`;
  // The REAL Spec status (Draft / Ready / In progress / …) — the SAME global `statusMeta` the rail
  // and the Spec detail page use, NOT a bespoke "Creating"/"Ready" tag. The creation↔dev MODE (which
  // overlay shows) is decided upstream in `overlayMode`; only this visible BADGE changed.
  const status = statusMeta(spec.status);

  const phaseCount = parsePhases(spec.phases).length;
  const taskCount = detail?.tasks.length ?? 0;
  const footer = `${status.label} · ${phaseCount || 1} ${phaseCount === 1 ? "phase" : "phases"} · ${taskCount} ${taskCount === 1 ? "task" : "tasks"}`;

  return (
    <div
      // Docks flush to the terminal's right edge inside the width-animated shell. NO left border /
      // own shadow: the shell casts the left-edge shadow, so the ACTIVE languette fused to this
      // panel's left edge reads seamless (no shared border). `bg-card/95` is the shared fused surface.
      //
      // A PLAIN box, deliberately: the panel's `role="dialog"` + its accessible name live on the
      // container `spec-drawer-overlay` mounts around this one (the element carrying
      // `data-overlay-panel` and the id the languette's `aria-controls` points at). This used to be a
      // named `<section>`, i.e. a second named region nested inside the panel for the same content —
      // the double naming the a11y review removed. Do NOT re-add a role or a label here.
      className="pointer-events-auto flex h-full w-full flex-col bg-card/95 backdrop-blur"
    >
      {/* 1. Header: doc icon + key/title + REAL status badge (+ a chevron to close). */}
      <header className="flex shrink-0 items-center gap-2 border-b border-border/60 px-4 py-3">
        <FileTextIcon className="size-4 shrink-0 text-muted-foreground" />
        <span className="font-mono text-2xs text-muted-foreground">
          {specKey(detail?.workspace_context?.project.key ?? "", spec.number)}
        </span>
        <span className="min-w-0 flex-1 truncate font-heading text-sm font-semibold text-foreground">
          {title}
        </span>
        <Badge variant={status.variant} className="font-mono">
          {status.label}
        </Badge>
        {/* Status-aware quick actions (open the Spec / open the review) — between the badge and
            the close chevron; a click navigates AWAY, never closes/toggles the panel. */}
        <OverlayPanelActions spec={spec} />
        <button
          type="button"
          onClick={onClose}
          aria-label="Close live spec panel"
          className="grid size-6 shrink-0 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
        >
          <ChevronRightIcon className="size-4" />
        </button>
      </header>

      {/* `overflow-x-hidden` + `min-w-0` clamp the reused `<SpecTab>`/`<TasksTab>` to the panel's
          width — bounded on the host pane, `min(380, max(45% of pane, 260))` (`overlay-motion`), the
          260px floor being exactly the point below which this reused content stops being readable:
          no horizontal scrollbar, no clipped text, no empty gutter. */}
      <div className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-3 py-4">
        {detail ? (
          <div className="flex min-w-0 flex-col gap-6">
            {/* 2. Real sections, filling in live (reused `<SpecTab>`). The drawer header already
                carries the key/title/status, so the SpecTab banner is suppressed here (no duplicate
                banner, and no per-word title staircase — a risk that only grows as the bounded
                width narrows towards its 260px floor). */}
            <SpecTab detail={detail} showBanner={false} />
            {/* 2 bis. The Spec's PROTOTYPE PAGES (finding `01KYW342EE5DN6F70YB8VSTJR8`) — the proto
                is an annexe of the Spec, proposed WHILE it is authored, so it belongs on this live
                panel too. Each row opens the workbench on ITS page (`?page=<slug>`); a Spec with no
                prototype renders nothing here at all. */}
            <OverlayPrototypes specId={spec.id} />
            {/* 3. Decomposed tasks, streamed live (reused `<TasksTab>`). */}
            <div>
              <MicroLabel className="mb-2 block px-1">Decomposed tasks</MicroLabel>
              <TasksTab detail={detail} />
            </div>
          </div>
        ) : loading ? (
          <div className="flex h-full items-center justify-center">
            <Spinner className="size-4 text-muted-foreground" />
          </div>
        ) : (
          <p className="px-1 text-sm text-muted-foreground">This Spec could not be loaded.</p>
        )}
      </div>

      {/* 4. Footer — status only, no CTA. */}
      <footer className="shrink-0 border-t border-border/60 px-4 py-2.5">
        <MicroLabel>{footer}</MicroLabel>
      </footer>
    </div>
  );
}
