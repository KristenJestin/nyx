import { useMemo } from "react";

import { cn } from "@/lib/utils";
import { CommandOutputPanel } from "./command-output-panel";
import { CommandRunEmpty } from "./command-run-empty";
import {
  commandRunPhase,
  commandRunPhaseLabel,
  useCommandRanThisSession,
} from "./command-run-phase";
import {
  paneRects,
  paneRectStyle,
  paneSeparators,
  separatorStyle,
} from "@/components/panes/pane-geometry";
import type { WorkspaceRuntimeService } from "@/components/runtime/use-workspace-runtime";
import { commandGroupTree } from "./command-group-tree";
import type { CommandGroup } from "./use-command-groups";

/**
 * `<CommandPaneHost>` — a GROUP of managed commands, side by side.
 *
 * ## The engine is the terminals' engine (review 01M0ABR6V501D0SPR2BYBVZC4A)
 *
 * There is no second layout model here. The tree comes from `autoLayoutTree` (via
 * {@link commandGroupTree}) — the same automatic placement a terminal joining a group gets — and
 * the rectangles come from {@link paneRects} / {@link paneRectStyle}, the very functions
 * `<PaneTreeHost>` lays terminals out with. The 180×120px floor is the same one, enforced where
 * the layout is COMPOSED (the settings screen) rather than at a split, because a commands group
 * has no split.
 *
 * ## What is deliberately ABSENT
 *
 * NO split control, NO close-pane control, NO draggable separator — the user's rule: a commands
 * group is composed and arranged ONLY in the settings screen, never at the pointer in the render.
 * That is why this host renders no affordance at all: there is nothing to omit, because none exists.
 *
 * ## The SEAM is drawn, and only drawn (review 01M0AT7KJV4G3C6GQNB36AV3N4)
 *
 * "Same as the terminals" and "no pointer gesture changes the layout" cannot both hold for a
 * DRAGGABLE separator — dragging IS such a gesture. The orchestrator's arbitrage splits the two:
 * the DELIMITATION is kept (two adjacent panes must be told apart exactly like two terminals), the
 * MANIPULATION is dropped. So the seam here is the terminals' resting hairline — the same
 * `ring/85` trait at the same computed boundary — carried by an element that has no
 * `role="separator"`, no `tabindex`, no resize cursor, no pointer handler and `pointer-events-none`
 * on top. An affordance offered on an inert band would be a lie, which is the very argument
 * `pane-tree-host.tsx:57-59` already makes for a read-only terminal tiling.
 */
export function CommandPaneHost({
  group,
  services,
  className,
}: {
  /** The group on screen — its ORDERED members are the disposition. */
  group: CommandGroup;
  /** The instances of THIS workspace, keyed by their template id upstream. */
  services: readonly WorkspaceRuntimeService[];
  className?: string;
}) {
  // The panes, in disposition order: a member with no instance in this workspace simply has no
  // pane (a template is materialised per workspace; a group is project data).
  const panes = useMemo(() => {
    const byTemplate = new Map(services.map((service) => [service.command_id, service]));
    return group.command_ids
      .map((commandId) => byTemplate.get(commandId))
      .filter((service): service is WorkspaceRuntimeService => service != null);
  }, [group.command_ids, services]);

  // The tree is (ordered members + chosen shape) — `commandGroupTree` is the ONE table both this
  // host and the settings screen's preview read, so what was picked there is what is laid out here.
  const tree = useMemo(
    () =>
      commandGroupTree(
        panes.map((service) => service.command_id),
        group.layout_shape,
      ),
    [panes, group.layout_shape],
  );

  if (panes.length === 0) return null;
  // Fewer than two live members is not a tiling — show the one there is, full frame.
  if (tree == null) {
    return (
      <div className={cn("relative h-full w-full", className)} data-command-pane-host="true">
        <CommandPane service={panes[0]} />
      </div>
    );
  }

  const placements = paneRects(tree);
  return (
    <div
      className={cn("relative h-full w-full", className)}
      data-command-pane-host="true"
      data-command-pane-count={placements.length}
    >
      {placements.map((placement) => {
        const service = panes.find((s) => s.command_id === placement.terminalId);
        if (service == null) return null;
        return (
          <div
            key={placement.paneId}
            data-command-pane-id={placement.paneId}
            data-command-id={service.command_id}
            style={paneRectStyle(placement.rect)}
            className="overflow-hidden outline outline-1 -outline-offset-1 outline-border"
          >
            <CommandPane service={service} />
          </div>
        );
      })}
      {/* THE SEAMS, LAST so they paint over the tiles' edges — see the header: drawn like the
          terminals' resting hairline, and INERT. `aria-hidden` because there is nothing to operate;
          `pointer-events-none` so hit-testing can never reach them; the box is ONE pixel wide, not
          the terminals' 8px grab band, because there is no grab. */}
      {paneSeparators(tree).map((seam) => (
        <div
          key={seam.splitId}
          aria-hidden="true"
          data-command-seam={seam.splitId}
          data-direction={seam.direction}
          style={separatorStyle(seam, COMMAND_SEAM_THICKNESS_PX)}
          className="pointer-events-none z-30 bg-ring/85"
        />
      ))}
    </div>
  );
}

/**
 * The painted width of a commands seam. The terminals' `SEPARATOR_THICKNESS_PX` is a HIT area with
 * a 1px trait inside it; with no hit to serve, the element IS the trait.
 */
const COMMAND_SEAM_THICKNESS_PX = 1;

/**
 * One pane: the command's live output UNDER a line that names its run phase, or the minimal empty
 * state when there is nothing to stream.
 *
 * WHY THE LINE (review 01M0ASWCH977S4J4XCC0B8N89N). Side by side, raw outputs do not say which
 * command failed, nor with what code: `<CommandInfoBar>` carries that, and it is mounted by
 * `<CommandView>`, never by this host. `commandRunPhaseLabel` already had the sentence — including
 * "Finished with an error — exit N" — and no caller reached it for the running / succeeded / failed
 * phases. This is that caller: one discrete line per pane, naming the command and its phase.
 */
function CommandPane({ service }: { service: WorkspaceRuntimeService }) {
  const ranThisSession = useCommandRanThisSession(service.id, service.run_state === "running");
  const phase = commandRunPhase({ state: service.run_state, ranThisSession });
  if (service.run_state === "idle") return <CommandRunEmpty service={service} />;
  return (
    <div className="flex h-full w-full flex-col">
      <div
        data-command-pane-phase={phase}
        className="flex shrink-0 items-baseline gap-2 border-border/70 border-b px-2 py-0.5 text-2xs"
      >
        <span className="truncate font-mono text-muted-foreground">{service.name}</span>
        <span className="ml-auto shrink-0 text-muted-foreground/70">
          {commandRunPhaseLabel(phase, { exitCode: service.exit_code })}
        </span>
      </div>
      <CommandOutputPanel key={service.id} instanceId={service.id} className="min-h-0 flex-1" />
    </div>
  );
}
