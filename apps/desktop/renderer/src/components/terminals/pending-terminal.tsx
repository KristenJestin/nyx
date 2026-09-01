import { createContext, type ReactNode, useCallback, useContext, useMemo, useState } from "react";

/**
 * THE PENDING TERMINAL — a one-shot, EPHEMERAL "open a new terminal in this workspace, and put the
 * user in it". Sibling of `reveal-workspace`, and built the same way for the same reason.
 *
 * ## Why a signal rather than a call
 *
 * The palette's `Open a new terminal in …` action must go through the app's EXISTING creation path,
 * `useTerminalActions().newTerminalInWorkspace(workspaceId, path)` — the very one the workspace
 * band's "+" calls. That path does not live on the shell: it is created by `<TerminalsBody>`
 * (`terminals/terminals-page.tsx`), because "…and ENTER it" is a selection in the PAGE's runtime
 * (`useRuntimeSelection`), which is what makes the new terminal the focused pane. The palette, by
 * contrast, is mounted on the always-mounted `<BaseLayout>` and fires from any route.
 *
 * Calling it directly would therefore have required either re-creating a terminal here (`create` +
 * `attach`: a SECOND creation path, two truths about what "a new terminal" is) or hoisting the
 * page's selection onto the shell (a far larger change than an affordance warrants). So the palette
 * REQUESTS, and the page — the one place that can both create and enter — performs.
 *
 * ## What it is not
 *
 * It PERSISTS NOTHING: no column, no backend event, nothing carried across a relaunch. It is
 * CONSUMED on arrival (`clearPendingTerminal`), so it opens exactly one terminal and then stops
 * existing; a page that mounts later with no signal behaves exactly as before. Like the reveal, it
 * does not throw without a provider — the default is inert, so `<TerminalsPage>` stays mountable on
 * its own, in a test or in the catalogue, with no shell around it.
 */
/**
 * A standing request: the workspace to open a terminal in, and THE PROJECT THAT OWNS IT.
 *
 * The project is what BOUNDS the request in time. Without it the page could only ever say "I do
 * not hold this workspace" — which is also what it says while the target project is still
 * loading — so a request naming a workspace that will never appear (deleted between the search and
 * the activation, a navigation that did not land) stayed standing forever, and fired the day the
 * user reached that project by some other route. Creating a terminal is not idempotent the way
 * opening a band is: a stale reveal is invisible, a stale creation is a pane nobody asked for.
 *
 * With the project named, the page can tell the two apart: once IT is the owning project and the
 * workspace is still not there, the request is unhonourable rather than early, and is dropped.
 */
interface PendingTerminalRequest {
  readonly workspaceId: string;
  readonly projectId: string;
}

interface PendingTerminalContextValue {
  /** The standing request, or `null` when nothing is pending. */
  readonly pendingTerminal: PendingTerminalRequest | null;
  /** Ask for a new terminal in `workspaceId`, owned by `projectId`. `null` cancels a request. */
  readonly requestTerminal: (workspaceId: string | null, projectId?: string | null) => void;
  /**
   * CONSUME the request for `workspaceId` — called by the page that is about to honour it, BEFORE
   * it creates anything, so the effect can never run twice. Also called to DROP a request the
   * owning project has proven unhonourable. Scoped to the id so a late consumer cannot swallow a
   * NEWER request aimed at another workspace.
   */
  readonly clearPendingTerminal: (workspaceId: string) => void;
}

/** The inert default: nothing pending, both mutators no-ops. */
const INERT: PendingTerminalContextValue = {
  pendingTerminal: null,
  requestTerminal: () => {},
  clearPendingTerminal: () => {},
};

const PendingTerminalContext = createContext<PendingTerminalContextValue>(INERT);

export function PendingTerminalProvider({ children }: { children: ReactNode }) {
  const [pendingTerminal, setPendingTerminal] = useState<PendingTerminalRequest | null>(null);

  // A request without an owning project is refused rather than stored unbounded: it is the project
  // that lets the page later decide the request is unhonourable instead of merely early.
  const requestTerminal = useCallback((workspaceId: string | null, projectId?: string | null) => {
    setPendingTerminal(
      workspaceId != null && workspaceId.length > 0 && projectId != null && projectId.length > 0
        ? { workspaceId, projectId }
        : null,
    );
  }, []);

  // Functional update, GUARDED on the id — the same rule as `reveal-workspace`'s `clearReveal`:
  // only the consumer the request actually named may clear it.
  const clearPendingTerminal = useCallback((workspaceId: string) => {
    setPendingTerminal((current) => (current?.workspaceId === workspaceId ? null : current));
  }, []);

  const value = useMemo<PendingTerminalContextValue>(
    () => ({ pendingTerminal, requestTerminal, clearPendingTerminal }),
    [pendingTerminal, requestTerminal, clearPendingTerminal],
  );
  return (
    <PendingTerminalContext.Provider value={value}>{children}</PendingTerminalContext.Provider>
  );
}

export function usePendingTerminal(): PendingTerminalContextValue {
  return useContext(PendingTerminalContext);
}
