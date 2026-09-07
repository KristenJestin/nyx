import { createContext, type ReactNode, useCallback, useContext, useMemo, useState } from "react";

import { loadScopeKind, saveScopeKind, saveLastProjectId } from "./ui-persistence";

/**
 * THE APP'S SCOPE — what "everything on this screen" is scoped to, for every page at once.
 *
 * Two things live here, and they are not the same thing:
 *
 *  - THE ENTERED PROJECT (`activeProjectId`) — which project the user has entered from the left
 *    rail. One enters a PROJECT, never a workspace (PDR-0008): the workspace stays the runtime
 *    pivot in the database and appears only as a grouping INSIDE the entered project.
 *  - THE SCOPE KIND (`scope.kind`) — WHAT the app is reading: one project, every project, or the
 *    host the app itself runs on. It used to exist only as the home's `?scope=all` url parameter,
 *    which made "all projects" a variant of ONE page rather than a mode of the app: the notion
 *    evaporated the moment you left the home. It is an application state now, so every page can
 *    read it (user feedback: « quand le all est selectionner c'est vraiment all […] si je vais sur
 *    toute les autres pages en etant sur le "all project" ca m'affiches les pages normalement avec
 *    tout »).
 *
 * ## What a page reads: {@link ActiveScope}, never the two fields separately
 *
 * `activeProjectId === null` means "NO project is loaded" — a fresh install, or the short window
 * before the rail resolves the entered one — and three pages turn that into an empty state
 * (`board-page.tsx`, `knowledge-page.tsx`, `catalogue-page.tsx`). It does NOT mean "all projects",
 * and recycling it for that would show those three their empty screen instead of the cross-project
 * content. So the two questions a page has ("which scope am I in?" and "which project?") are
 * answered by ONE discriminated value, {@link ActiveScope}, in which the two project-less cases
 * simply HAVE no project id to misread. That is the whole guard, and it is structural rather than
 * a convention.
 *
 * ## The three scopes are exclusive, and the rail is the only thing that switches between them
 *
 * You are in a project, in all of them, or in Système. The ENTERED project is REMEMBERED while
 * another scope is up (it has to be: the rail still has to know where to put you back, and the
 * restore effect in `<BaseLayout>` would otherwise re-enter a project and kick you straight out),
 * but it is not the SCOPE — neither `{kind:"all"}` nor `{kind:"system"}` carries a `projectId`.
 *
 * The scope moves in exactly these places, all of them the user's own gesture:
 *   - the rail's `All projects` tile → `setScopeKind("all")` (`base-layout.tsx`);
 *   - the rail's project tiles and the section-nav's `Home`, i.e. every gesture that means "take
 *     me INTO this project" → `setScopeKind("project")` (`base-layout.tsx`'s `enterProject`);
 *   - the rail's SYSTÈME tile → `setScopeKind("system")` (`base-layout.tsx`'s `openSystemPage`);
 *   - a ROW of the cross-project home, clicked (`use-home-focus.ts`) — a row of the Système
 *     singleton enters the Système scope, any other row enters its project;
 *   - a HIT of the global search palette (`search-palette.tsx`), by the same rule;
 *   - the `/home` route, which MIRRORS its own `?scope=` into this state (`home-page.tsx`), and
 *     the `/system` route, which mirrors ITS OWN EXISTENCE the same way
 *     (`system-scope-page.tsx`), so a deep-linked page still enters the mode and the url can never
 *     disagree with the app.
 *
 * `setActiveProjectId` is itself scope-NEUTRAL: it only says which project is entered, and a
 * caller that merely FOLLOWS a route (`use-spec-project-sync`, opening a Spec from the
 * cross-project board) calls it alone — dropping the user out of the transverse view because they
 * opened a Spec from it would be the switch nobody asked for. A gesture that really means "enter
 * this project" calls BOTH setters.
 *
 * ## Persistence
 *
 * Both halves are REMEMBERED (renderer localStorage) so a relaunch restores where the user left
 * off. The project id is a HINT the rail validates against the live tree (a deleted project falls
 * back cleanly); the scope kind needs no validation against a project tree — "all projects" and
 * "the host" are true of every tree — so it is seeded straight into this provider's state.
 *
 * ## Système IS the third one — and that REVERSES finding U1, deliberately
 *
 * A `systemScope` boolean briefly sat beside the entered project (human review
 * `01M1EC7Y2625Q07C5386GKHZFS`, finding U1) and was removed: Système became a PAGE at `/system`, so
 * "which page am I on" was the ROUTER's answer and the entered project survived the visit
 * untouched. Tested that way in the real app the model failed on the other side: the rail showed a
 * project as entered AND selected while the screen was the host's, the section-nav offered six
 * project entries over a page that has none of them, and the entered project could never show its
 * activity dot because the rail believed one was looking at it.
 *
 * The human decision reverses U1: Système is a SCOPE, the third of the three, and it takes the
 * scope region's mark like the other two. What U1 got right SURVIVES — the entered project is
 * still remembered here, so clicking a project tile puts you back exactly where you were, and
 * `/system` stays a real route rather than a flag dispatching inside `/`. What it got wrong is
 * gone: the scope is a discriminated KIND, not a boolean beside a boolean, so the two
 * contradictory selections that made U1 necessary cannot even be spelled.
 */

/**
 * WHAT A PAGE READS. One value that answers "which scope?" and "which project?" at once, so that
 * neither answer can be spelled with the other's `null`.
 *
 *  - `{ kind: "all" }` — every project. There is no project id, by construction.
 *  - `{ kind: "system" }` — the Système singleton, the host the app itself runs on. It is not one
 *    of the projects (`db::list_projects` filters `kind = 'user'`), so it carries no project id
 *    either; the page that renders it reads the singleton through its own door (`useSystemScope`).
 *  - `{ kind: "project", projectId }` — the entered project, or `null` when NO project is loaded
 *    (fresh install / pre-load window). That `null` keeps its one historical meaning.
 */
export type ActiveScope =
  | { readonly kind: "all" }
  | { readonly kind: "system" }
  | { readonly kind: "project"; readonly projectId: string | null };

/** The discriminant on its own — what {@link ActiveProjectContextValue.setScopeKind} takes. */
export type ScopeKind = ActiveScope["kind"];

interface ActiveProjectContextValue {
  /**
   * The ENTERED project, or `null` when none is loaded. It keeps its value in the two other
   * scopes — it is where the rail puts you back — so it is NOT the thing to read to know the
   * scope: read {@link ActiveProjectContextValue.scope}.
   */
  readonly activeProjectId: string | null;
  /** Change the entered project. Scope-neutral on purpose — see the module doc. */
  readonly setActiveProjectId: (id: string | null) => void;
  /** THE SCOPE, as the one value a page renders from. */
  readonly scope: ActiveScope;
  /** `scope.kind === "all"`, for the rail and for a page's `showProject` flags. */
  readonly globalScope: boolean;
  /** `scope.kind === "system"` — the host scope, whose one screen is `/system`. */
  readonly systemScope: boolean;
  /** Move the app to a scope. THE one setter: there is no second way to say the same thing. */
  readonly setScopeKind: (kind: ScopeKind) => void;
}

const ActiveProjectContext = createContext<ActiveProjectContextValue | null>(null);

export function ActiveProjectProvider({
  children,
  initialProjectId = null,
  initialScopeKind,
}: {
  children: ReactNode;
  initialProjectId?: string | null;
  /** Force the starting scope (tests). Omitted → the REMEMBERED one, i.e. the relaunch restore. */
  initialScopeKind?: ScopeKind;
}) {
  const [activeProjectId, setActiveProjectIdState] = useState<string | null>(initialProjectId);
  // The remembered scope is read ONCE, as the initial state: it needs no live tree to be valid, so
  // there is nothing to wait for and no restore effect to race with a deep-linked page.
  const [scopeKind, setScopeKindState] = useState<ScopeKind>(
    () => initialScopeKind ?? loadScopeKind(),
  );
  // Every entered-project change is REMEMBERED (renderer localStorage) so a relaunch — or a
  // return to Terminals — restores the LAST project the user was in, not the first. The rail
  // restore (`<BaseLayout>`) reads this hint back and honours it only when the project still
  // exists (a deleted project's stale id falls back cleanly to the first).
  const setActiveProjectId = useCallback((id: string | null) => {
    setActiveProjectIdState(id);
    saveLastProjectId(id);
  }, []);
  const setScopeKind = useCallback((kind: ScopeKind) => {
    setScopeKindState(kind);
    saveScopeKind(kind);
  }, []);
  const value = useMemo<ActiveProjectContextValue>(() => {
    // The scope is BUILT from the kind, in ONE place: the two project-less kinds cannot acquire a
    // project id by accident, and the project kind cannot lose one.
    const scope: ActiveScope =
      scopeKind === "all"
        ? { kind: "all" }
        : scopeKind === "system"
          ? { kind: "system" }
          : { kind: "project", projectId: activeProjectId };
    return {
      activeProjectId,
      setActiveProjectId,
      scope,
      globalScope: scopeKind === "all",
      systemScope: scopeKind === "system",
      setScopeKind,
    };
  }, [activeProjectId, setActiveProjectId, scopeKind, setScopeKind]);
  return <ActiveProjectContext.Provider value={value}>{children}</ActiveProjectContext.Provider>;
}

export function useActiveProject(): ActiveProjectContextValue {
  const ctx = useContext(ActiveProjectContext);
  if (!ctx) {
    throw new Error("useActiveProject must be used inside <ActiveProjectProvider>");
  }
  return ctx;
}
