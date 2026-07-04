import { useMemo } from "react";

import { CommandOutputPanel } from "@/components/command/command-output-panel";
import { TerminalDeck } from "@/components/sidebar/terminal-deck";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { CommandIdleEmpty } from "./command-idle-empty";
import { EmptyHint } from "./tree-primitives";
import { useRuntimeSelection } from "./runtime-selection";
import type { WorkspaceRuntimeService, WorkspaceRuntimeTerminal } from "./use-workspace-runtime";

/**
 * `<RuntimePaneHost>` — the Terminals page's MAIN surface, driven by the shared page runtime
 * selection:
 *
 *  - a selected TERMINAL shows its LIVE xterm pane. We reuse the real [`<TerminalDeck>`] — the
 *    exact multi-terminal host the shell uses — so every live terminal in the ENTERED PROJECT
 *    (across all its workspaces) stays mounted (PTY + buffer + scrollback alive) and only the
 *    selected one is visible. No second terminal implementation, no duplicated xterm wiring.
 *  - a selected SERVICE that is IDLE (no live run, nothing to stream) shows the MINIMAL empty
 *    state (`<CommandIdleEmpty>`: a discrete state label over the terminal surface) instead of a
 *    bare black pane — the validated empty state. Once it is running/settled its output streams
 *    through the read-only [`<CommandOutputPanel>`] (rehydrate + live stream, no stdin), the
 *    same watch-only surface the command view uses.
 *  - no selection shows a muted placeholder.
 *
 * No business logic: the panes are fed straight from the live aggregates. The deck needs the
 * `TerminalRecord` shape; we PROJECT the aggregates' snake_case runtime terminals onto it
 * (id / cwd / label / status) — a pure, memoized adapter, not a second source of truth. The
 * `terminals` prop is the FLAT list of every workspace's terminals so the deck spans the whole
 * project and a selection switch across workspaces never remounts a terminal; `services` is the
 * matching flat list so the selected service's idle empty state reads its live run-state.
 */
export function RuntimePaneHost({
  terminals,
  services,
}: {
  terminals: readonly WorkspaceRuntimeTerminal[];
  services: readonly WorkspaceRuntimeService[];
}) {
  const { selected } = useRuntimeSelection();

  // Project the aggregates' runtime terminals onto the deck's `TerminalRecord` shape. Only the
  // fields the deck reads (id / cwd / label / status) carry meaning; the record-lifecycle
  // fields the aggregate does not track default to inert values (no scrollback → no dead
  // history, order is irrelevant since the deck sorts panes by id internally).
  const deckTerminals = useMemo<TerminalRecord[]>(
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

  // The deck shows the selected terminal (all others stay mounted-but-hidden so their PTY +
  // buffer survive a selection switch). A selected SERVICE hides the deck and shows its output;
  // the deck stays mounted underneath so its terminals keep their PTYs alive.
  const activeTerminalId = selected?.kind === "terminal" ? selected.id : null;
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

  return (
    <div className="relative h-full w-full min-w-0 bg-background">
      <div className={activeServiceId != null ? "hidden" : "h-full w-full"}>
        {deckTerminals.length > 0 ? (
          <TerminalDeck terminals={deckTerminals} activeId={activeTerminalId} />
        ) : (
          <div className="flex h-full w-full items-center justify-center">
            <EmptyHint>No terminals to show.</EmptyHint>
          </div>
        )}
      </div>
      {activeServiceId != null ? (
        showIdleEmpty ? (
          <CommandIdleEmpty service={activeService} />
        ) : (
          <CommandOutputPanel key={activeServiceId} instanceId={activeServiceId} />
        )
      ) : null}
    </div>
  );
}
