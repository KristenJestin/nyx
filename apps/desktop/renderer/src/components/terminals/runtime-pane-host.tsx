import { useMemo } from "react";

import { useCommandGroupsContext } from "@/components/command/command-groups-context";
import { CommandOutputPanel } from "@/components/command/command-output-panel";
import { CommandPaneHost } from "@/components/command/command-pane-host";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { SpecOverlay } from "@/components/terminal-spec-overlay/spec-overlay";
import { TerminalSpecsProvider } from "@/components/terminal-spec-overlay/terminal-specs-provider";
import { CommandIdleEmpty } from "./command-idle-empty";
import { PaneTreeHost } from "./pane-tree-host";
import { EmptyHint } from "@/components/ui/tree-primitives";
import { useRuntimeSelection } from "./runtime-selection";
import type { PaneLayoutController } from "./use-pane-layout";
import type {
  WorkspaceRuntimeService,
  WorkspaceRuntimeTerminal,
} from "@/components/runtime/use-workspace-runtime";

/**
 * `<RuntimePaneHost>` — the Terminals page's MAIN surface, driven by the shared page runtime
 * selection:
 *
 *  - a selected TERMINAL shows the TILING. The mono-visible deck is gone: `<PaneTreeHost>` mounts
 *    ONE live xterm per terminal of the ENTERED PROJECT (across all its workspaces) exactly like
 *    the deck did — PTY + buffer alive, a hidden terminal merely has no box — and lays out the
 *    tree's leaves side by side. Several terminals are therefore VISIBLE at once, and exactly one
 *    is FOCUSED (the keyboard, the Spec surcouche and the sidebar selection follow it). Still no
 *    second terminal implementation: same `<Terminal>`, same store, same ResizeObserver/FitAddon.
 *  - a selected SERVICE that is IDLE (no live run, nothing to stream) shows the MINIMAL empty
 *    state (`<CommandIdleEmpty>`: a discrete state label over the terminal surface) instead of a
 *    bare black pane — the validated empty state. Once it is running/settled its output streams
 *    through the read-only [`<CommandOutputPanel>`] (rehydrate + live stream, no stdin), the
 *    same watch-only surface the command view uses.
 *  - a selected service that BELONGS TO A COMMAND GROUP shows the WHOLE group side by side
 *    ([`<CommandPaneHost>`]), in the group's disposition order — the commands' half of the
 *    multi-pane model, on the terminals' engine. The single-service branches above are what a
 *    SOLO command falls back to.
 *  - no selection shows a muted placeholder.
 *
 * No business logic: the panes are fed straight from the live aggregates. The tiling host needs
 * the `TerminalRecord` shape; we PROJECT the aggregates' snake_case runtime terminals onto it
 * (id / cwd / label / status) — a pure, memoized adapter, not a second source of truth. The
 * `terminals` prop is the FLAT list of every workspace's terminals so the host spans the whole
 * project and a selection switch across workspaces never remounts a terminal; `services` is the
 * matching flat list so the selected service's idle empty state reads its live run-state.
 */
/** The "no terminal to resolve" list, hoisted so it is referentially stable across renders. */
const EMPTY_IDS: readonly string[] = [];

export function RuntimePaneHost({
  terminals,
  services,
  tiling,
}: {
  terminals: readonly WorkspaceRuntimeTerminal[];
  services: readonly WorkspaceRuntimeService[];
  /**
   * The page's TILING state (`usePaneLayout`) — which terminals are on screen, where, and which
   * pane is focused. It is owned ABOVE this component because the page also feeds it to the
   * idle-unload sweep (whose protected set is every VISIBLE pane, not the selection).
   *
   * OMITTED ⇒ there is no tiling and therefore nothing to show: the terminal region falls back to
   * its placeholder. The service surfaces below are unaffected — they do not tile.
   */
  tiling?: PaneLayoutController;
}) {
  const { selected } = useRuntimeSelection();

  // Project the aggregates' runtime terminals onto the host's `TerminalRecord` shape. Only the
  // fields it reads (id / cwd / label / status) carry meaning; the record-lifecycle fields the
  // aggregate does not track default to inert values (no scrollback → no dead history, order is
  // irrelevant since the host pins its slots by id internally).
  const paneTerminals = useMemo<TerminalRecord[]>(
    () =>
      terminals.map((terminal) => ({
        id: terminal.id,
        cwd: terminal.cwd,
        label: terminal.label,
        scrollback: "",
        status: terminal.status,
        order_index: 0,
        created_at: 0,
        updated_at: 0,
        closed_at: null,
        workspace_id: terminal.workspace_id,
        exec_state: terminal.exec_state,
      })),
    [terminals],
  );

  // A selected SERVICE hides the tiling and shows its output; the tiling stays mounted underneath
  // so its terminals keep their PTYs, their buffers and their layout alive.
  const activeServiceId = selected?.kind === "service" ? selected.id : null;
  const activeService = useMemo(
    () =>
      activeServiceId != null ? (services.find((s) => s.id === activeServiceId) ?? null) : null,
    [activeServiceId, services],
  );

  // An IDLE service has no live run and nothing to stream — show the MINIMAL empty state (a
  // discrete state label over the terminal surface, the validated placement) instead of the
  // read-only output pane. A `running`/settled service (or one whose snapshot we somehow lack)
  // streams through the output panel as before.
  const showIdleEmpty = activeService != null && activeService.run_state === "idle";

  /**
   * A SELECTED SERVICE THAT BELONGS TO A GROUP SHOWS THE GROUP, NOT ITSELF (finding #90,
   * 01M0ABR6V501D0SPR2BYBVZC4A: "les commandes reçoivent le multi-pane, sur le MÊME modèle de
   * groupes que les terminaux"). Picking any member opens the whole disposition side by side —
   * there is no "which member did I click" to remember, because the group IS the surface. A solo
   * command, or a group whose live members fall below two in this workspace, keeps the single
   * output panel.
   *
   * …AND SO DOES THE FOLDER LINE ITSELF (human review 01M14112THEB4094B2G614SXR5). The group can now
   * be the SELECTION — « c'est trop chelou que je doive cliquer que un des 3 pour avoir le 3 » — so
   * the same host is reached without any member being viewed. The two paths converge here on one
   * `activeGroup`, so what is drawn cannot depend on how the group was pointed at.
   *
   * The groups come from `<CommandGroupsProvider>`, which the page mounts over this host: no
   * second read, and an MCP edit repaints here through the same `commands://changed` broadcast.
   */
  const commandGroups = useCommandGroupsContext();
  const selectedGroup = selected?.kind === "command-group" ? selected : null;
  const activeGroup = useMemo(() => {
    const group =
      selectedGroup != null
        ? (commandGroups.groups.find((candidate) => candidate.id === selectedGroup.id) ?? null)
        : activeService != null
          ? commandGroups.groupOf(activeService.command_id)
          : null;
    return group != null && group.command_ids.length >= 2 ? group : null;
  }, [selectedGroup, activeService, commandGroups]);

  // The group is PROJECT data; an instance is per WORKSPACE. The host is fed only the instances of
  // ONE workspace — the selected member's, or the one the selected FOLDER was drawn in — so a member
  // that is not materialised here simply has no pane instead of borrowing another workspace's run.
  const paneWorkspaceId = selectedGroup?.workspaceId ?? activeService?.workspace_id ?? null;
  const groupServices = useMemo(
    () =>
      paneWorkspaceId == null
        ? []
        : services.filter((service) => service.workspace_id === paneWorkspaceId),
    [paneWorkspaceId, services],
  );

  // WHETHER A COMMAND SURFACE IS ON SCREEN AT ALL — the one predicate the tiling hides behind. A
  // group selection whose group has DISSOLVED under it resolves to no surface, and the tiling comes
  // back rather than the frame going blank.
  const commandSurface = activeServiceId != null || activeGroup != null;

  return (
    <div className="relative h-full w-full min-w-0 bg-background">
      <div className={commandSurface ? "hidden" : "h-full w-full"}>
        {paneTerminals.length > 0 && tiling != null ? (
          // ONE RESOLVER FOR THE WHOLE TILING (review finding 01M11WF11C0H8G2MG515FBT6RD). The
          // provider holds a single `terminal_specs` resolution — one `specs://changed` and one
          // `agent-sessions://changed` subscription — for EVERY visible terminal, and each
          // `<SpecOverlay>` below reads its own slice out of it. Handing it `[]` while a SERVICE is
          // the selected surface takes the resolver away entirely (no subscription, no read),
          // exactly as unmounting the single overlay used to.
          <TerminalSpecsProvider
            terminalIds={commandSurface ? EMPTY_IDS : tiling.visibleTerminalIds}
          >
            <PaneTreeHost
              terminals={paneTerminals}
              layout={tiling.layout}
              focusedPaneId={tiling.focusedPaneId}
              onFocusPane={tiling.focusPane}
              onSplitPane={tiling.splitPane}
              onClosePane={tiling.closePane}
              onResizeSplit={tiling.resizeSplit}
              onMovePane={tiling.movePane}
              canMovePane={tiling.canMovePane}
              // The terminal↔Spec surcouche, anchored INSIDE ITS OWN pane's box (that container is
              // the positioning block for its `absolute` sub-overlays) rather than over the whole
              // main area — which is what made it straddle the neighbours once panes existed. It
              // renders NOTHING when the terminal has no mapped Spec (non-regression), so a pane
              // with no session shows nothing at all. EVERY visible pane gets one: the surcouche
              // belongs to the terminal working the Spec, not to whichever pane holds the keyboard.
              renderPaneOverlay={
                commandSurface ? undefined : (terminalId) => <SpecOverlay terminalId={terminalId} />
              }
            />
          </TerminalSpecsProvider>
        ) : (
          <div className="flex h-full w-full items-center justify-center">
            <EmptyHint>No terminals to show.</EmptyHint>
          </div>
        )}
      </div>
      {activeGroup != null ? (
        <CommandPaneHost key={activeGroup.id} group={activeGroup} services={groupServices} />
      ) : activeServiceId != null ? (
        showIdleEmpty ? (
          <CommandIdleEmpty service={activeService} />
        ) : (
          <CommandOutputPanel key={activeServiceId} instanceId={activeServiceId} />
        )
      ) : null}
    </div>
  );
}
