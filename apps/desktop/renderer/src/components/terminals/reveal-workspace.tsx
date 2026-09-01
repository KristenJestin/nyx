import { createContext, type ReactNode, useCallback, useContext, useMemo, useState } from "react";

/**
 * THE WORKSPACE REVEAL — a one-shot, EPHEMERAL "the user is arriving on this workspace, show it".
 *
 * ## The problem it solves
 *
 * The global search palette promises to land the user ON an entity. Three of the seven kinds live
 * inside a workspace (an agent session, the terminal reached through it, a command instance), and a
 * workspace is drawn as an accordion band on the Terminals page. Land on the right page with that
 * band CLOSED and the promise is broken: the user's own words — « j'ai demandé à aller sur le
 * workspace scratch de la commande machin, mais le workspace est fermé donc c'est bizarre ». The
 * band was closed because `<WorkspaceAccordion>` opens by RANK (`defaultOpen={index === 0}`), and
 * `scratch` was not first.
 *
 * ## Why a signal and NOT the persisted `collapsed` flag
 *
 * `workspaces.collapsed` exists, and `useProjects().setWorkspaceCollapsed` writes it — but NOTHING
 * MOUNTED READS IT. Its only readers (`sidebar/workspace-item.tsx`, `sidebar/project-item.tsx`)
 * belong to `<TerminalManager>`, the legacy shell PDR-0025 unmounted. Writing it on a jump would
 * have been an invisible no-op.
 *
 * Wiring the accordion onto that flag instead was rejected on PRODUCT grounds, not technical ones:
 * the column defaults to `collapsed = false`, so reading it would flip every install's default from
 * "the first workspace is open" to "all of them are", a change nobody asked for and one that hurts
 * exactly the projects with the most workspaces.
 *
 * So the reveal is a SIGNAL, not a state:
 *  - it writes NOTHING to the database and emits NO backend event — a jump is a navigation, not a
 *    preference the user expressed;
 *  - it is CONSUMED on arrival (`clearReveal`), so it opens the band once and then stops existing.
 *    A band the user collapses by hand afterwards STAYS collapsed: there is no longer a signal to
 *    re-open it, and nothing has been remembered anywhere;
 *  - outside a jump the Terminals page behaves exactly as before, rank-default included.
 *
 * ## Why it does not throw without a provider, unlike `active-project`
 *
 * `useActiveProject` throws, because the entered project is a selection every consumer REQUIRES.
 * A reveal is an optional affordance layered on top of a band that already knows how to open and
 * close itself: with no provider the context is inert and the accordion simply behaves as it always
 * did. That keeps `<WorkspaceAccordion>` mountable on its own — in a test, in the catalogue —
 * without dragging the shell in.
 */
interface RevealWorkspaceContextValue {
  /** The workspace to open on arrival, or `null` when there is nothing to reveal. */
  readonly revealWorkspaceId: string | null;
  /** Ask for `id`'s band to be open when the user gets there. `null` cancels a pending reveal. */
  readonly revealWorkspace: (id: string | null) => void;
  /**
   * CONSUME the reveal for `id` — called by the band that just honoured it. Scoped to `id` so a
   * late consumer can never swallow a NEWER reveal aimed at another workspace.
   */
  readonly clearReveal: (id: string) => void;
}

/**
 * The inert default: no reveal pending, and both mutators are no-ops. It is what a
 * `<WorkspaceAccordion>` mounted outside the shell sees, and it makes "no provider" mean "no
 * reveal" rather than a crash.
 */
const INERT: RevealWorkspaceContextValue = {
  revealWorkspaceId: null,
  revealWorkspace: () => {},
  clearReveal: () => {},
};

const RevealWorkspaceContext = createContext<RevealWorkspaceContextValue>(INERT);

export function RevealWorkspaceProvider({ children }: { children: ReactNode }) {
  const [revealWorkspaceId, setRevealWorkspaceId] = useState<string | null>(null);

  const revealWorkspace = useCallback((id: string | null) => {
    setRevealWorkspaceId(id != null && id.length > 0 ? id : null);
  }, []);

  // Functional update, and GUARDED on the id: two bands could in principle read the same signal in
  // one commit (a remount racing an update), and only the one the reveal actually named may clear
  // it. Stable identity so the consuming effect does not re-run on every provider render.
  const clearReveal = useCallback((id: string) => {
    setRevealWorkspaceId((current) => (current === id ? null : current));
  }, []);

  const value = useMemo<RevealWorkspaceContextValue>(
    () => ({ revealWorkspaceId, revealWorkspace, clearReveal }),
    [revealWorkspaceId, revealWorkspace, clearReveal],
  );
  return (
    <RevealWorkspaceContext.Provider value={value}>{children}</RevealWorkspaceContext.Provider>
  );
}

export function useRevealWorkspace(): RevealWorkspaceContextValue {
  return useContext(RevealWorkspaceContext);
}
