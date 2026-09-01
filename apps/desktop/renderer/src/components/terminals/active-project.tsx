import { createContext, type ReactNode, useCallback, useContext, useMemo, useState } from "react";

import { loadGlobalScope, saveGlobalScope, saveLastProjectId } from "./ui-persistence";

/**
 * THE APP'S SCOPE — what "everything on this screen" is scoped to, for every page at once.
 *
 * Two things live here, and they are not the same thing:
 *
 *  - THE ENTERED PROJECT (`activeProjectId`) — which project the user has entered from the left
 *    rail. One enters a PROJECT, never a workspace (PDR-0008): the workspace stays the runtime
 *    pivot in the database and appears only as a grouping INSIDE the entered project.
 *  - THE GLOBAL SCOPE (`globalScope`) — whether the app is reading ACROSS every project. It used
 *    to exist only as the home's `?scope=all` url parameter, which made "all projects" a variant
 *    of ONE page rather than a mode of the app: the notion evaporated the moment you left the
 *    home. It is an application state now, so every page can read it (user feedback: « quand le
 *    all est selectionner c'est vraiment all […] si je vais sur toute les autres pages en etant
 *    sur le "all project" ca m'affiches les pages normalement avec tout »).
 *
 * ## What a page reads: {@link ActiveScope}, never the two fields separately
 *
 * `activeProjectId === null` means "NO project is loaded" — a fresh install, or the short window
 * before the rail resolves the entered one — and three pages turn that into an empty state
 * (`board-page.tsx`, `knowledge-page.tsx`, `catalogue-page.tsx`). It does NOT mean "all projects",
 * and recycling it for that would show those three their empty screen instead of the cross-project
 * content. So the two questions a page has ("am I global?" and "which project?") are answered by
 * ONE discriminated value, {@link ActiveScope}, in which the global case simply HAS no project id
 * to misread. That is the whole guard, and it is structural rather than a convention.
 *
 * ## The two scopes are exclusive, and the rail is the only thing that switches between them
 *
 * You are in a project, or you are in all of them. The ENTERED project is REMEMBERED while the
 * global scope is on (it has to be: the rail still has to know where to put you back, and the
 * restore effect in `<BaseLayout>` would otherwise re-enter a project and kick you straight out),
 * but it is not the SCOPE — `scope.kind === "all"` carries no `projectId` at all.
 *
 * The scope moves in exactly four places, all of them the user's own gesture:
 *   - the rail's `All projects` tile → `setGlobalScope(true)` (`base-layout.tsx`);
 *   - the rail's project tiles and the section-nav's `Home`, i.e. every gesture that means "take
 *     me INTO this project" → `setGlobalScope(false)` (`base-layout.tsx`'s `enterProject`);
 *   - a ROW of the cross-project home, clicked (`use-home-focus.ts`) — see below;
 *   - the `/home` route, which MIRRORS its own `?scope=` into this state (`home-page.tsx`), so a
 *     deep-linked `/home?scope=all` still enters the mode and the url of that one page can never
 *     disagree with the app.
 *
 * `setActiveProjectId` is itself scope-NEUTRAL: it only says which project is entered, and a
 * caller that merely FOLLOWS a route (`use-spec-project-sync`, opening a Spec from the
 * cross-project board) calls it alone — dropping the user out of the transverse view because they
 * opened a Spec from it would be the switch nobody asked for. A gesture that really means "enter
 * this project" calls BOTH setters.
 *
 * `use-home-focus` is now in the second camp, and this is a REVERSAL: it was written scope-neutral
 * on purpose (clicking a row said "show me this terminal", not "leave the transverse view") and
 * the foundation pass escalated the call. The user ruled that clicking a row of the cross-project
 * home IS entering its project (review `01M1ECXAXVGHJWPWA019QAER6G`), so the hook sets both — WITHOUT
 * going through `enterProject`, whose `search: {}` would erase the deep-link the row is carrying.
 * A row with no project at all (a loose terminal) stays in the global scope: there is no project
 * to enter. The reasoning lives in that file; this list is the register.
 *
 * ## Persistence
 *
 * Both halves are REMEMBERED (renderer localStorage) so a relaunch restores where the user left
 * off. The project id is a HINT the rail validates against the live tree (a deleted project falls
 * back cleanly); the scope flag needs no validation — "all projects" is true of every tree — so it
 * is seeded straight into this provider's state.
 */

/**
 * WHAT A PAGE READS. One value that answers "am I global?" and "which project?" at once, so that
 * neither answer can be spelled with the other's `null`.
 *
 *  - `{ kind: "all" }` — every project. There is no project id, by construction.
 *  - `{ kind: "project", projectId }` — the entered project, or `null` when NO project is loaded
 *    (fresh install / pre-load window). That `null` keeps its one historical meaning.
 */
export type ActiveScope =
  | { readonly kind: "all" }
  | { readonly kind: "project"; readonly projectId: string | null };

interface ActiveProjectContextValue {
  /**
   * The ENTERED project, or `null` when none is loaded. It keeps its value in the global scope —
   * it is where the rail puts you back — so it is NOT the thing to read to know the scope: read
   * {@link ActiveProjectContextValue.scope}.
   */
  readonly activeProjectId: string | null;
  /** Change the entered project. Scope-neutral on purpose — see the module doc. */
  readonly setActiveProjectId: (id: string | null) => void;
  /** THE SCOPE, as the one value a page renders from. */
  readonly scope: ActiveScope;
  /** `scope.kind === "all"`, for the rail and for a page's `showProject` flags. */
  readonly globalScope: boolean;
  /** Enter (or leave) the cross-project scope. Persisted, like the entered project. */
  readonly setGlobalScope: (global: boolean) => void;
}

const ActiveProjectContext = createContext<ActiveProjectContextValue | null>(null);

export function ActiveProjectProvider({
  children,
  initialProjectId = null,
  initialGlobalScope,
}: {
  children: ReactNode;
  initialProjectId?: string | null;
  /** Force the starting scope (tests). Omitted → the REMEMBERED one, i.e. the relaunch restore. */
  initialGlobalScope?: boolean;
}) {
  const [activeProjectId, setActiveProjectIdState] = useState<string | null>(initialProjectId);
  // The remembered scope is read ONCE, as the initial state: it needs no live tree to be valid, so
  // there is nothing to wait for and no restore effect to race with a deep-linked page.
  const [globalScope, setGlobalScopeState] = useState<boolean>(
    () => initialGlobalScope ?? loadGlobalScope(),
  );
  // Every entered-project change is REMEMBERED (renderer localStorage) so a relaunch — or a
  // return to Terminals — restores the LAST project the user was in, not the first. The rail
  // restore (`<BaseLayout>`) reads this hint back and honours it only when the project still
  // exists (a deleted project's stale id falls back cleanly to the first).
  const setActiveProjectId = useCallback((id: string | null) => {
    setActiveProjectIdState(id);
    saveLastProjectId(id);
  }, []);
  const setGlobalScope = useCallback((global: boolean) => {
    setGlobalScopeState(global);
    saveGlobalScope(global);
  }, []);
  const value = useMemo<ActiveProjectContextValue>(
    () => ({
      activeProjectId,
      setActiveProjectId,
      scope: globalScope ? { kind: "all" } : { kind: "project", projectId: activeProjectId },
      globalScope,
      setGlobalScope,
    }),
    [activeProjectId, setActiveProjectId, globalScope, setGlobalScope],
  );
  return <ActiveProjectContext.Provider value={value}>{children}</ActiveProjectContext.Provider>;
}

export function useActiveProject(): ActiveProjectContextValue {
  const ctx = useContext(ActiveProjectContext);
  if (!ctx) {
    throw new Error("useActiveProject must be used inside <ActiveProjectProvider>");
  }
  return ctx;
}
