import { useCallback } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useActiveProject } from "@/components/terminals/active-project";

/** What a home row asks for when it is clicked: a scope to be in, and a runtime object to open. */
export interface HomeFocusTarget {
  /**
   * The row's project, or `null` — a loose terminal belongs to none and needs no switch. It is
   * also `null` on a SYSTÈME row, for the opposite reason: that row HAS a container and the
   * container is not a project. `system` is what tells the two apart.
   */
  readonly projectId: string | null;
  /**
   * Does the row belong to the SYSTÈME singleton? Read off the snapshot's own marking
   * (`HomeProject.kind === "system"`, joined by the backend), never guessed here.
   */
  readonly system: boolean;
  /** The deep-link: the terminal to select, or the managed command to open. */
  readonly search: { readonly terminal?: string; readonly command?: string };
}

/**
 * `use-home-focus` — the ONE path a home row takes to land on its object, in every scope.
 *
 * ## Why this is not `enterProject`
 *
 * The rail's `enterProject` (`base-layout.tsx`) navigates to `/` with `search: {}` — it has to,
 * because it is a project SWITCH and any `?terminal=`/`?command=` still in the url belongs to the
 * project being LEFT (review `01M069MWRNR1PEZZW7YTPAHSKK`: a stale deep-link there makes
 * `<TerminalsPage>` wait forever for a target that will never appear, and suppresses its
 * last-terminal restore, landing on an empty deck).
 *
 * The home needs the exact opposite: it switches scope AND carries a deep-link, and the two are
 * the same act. Routed through `enterProject`, the deep-link would be ERASED by the very
 * navigation meant to carry it — so the home poses the scope and navigates with its own search, in
 * one call. `home-page.test.tsx` covers that erasure explicitly.
 *
 * ## Why the order does not matter
 *
 * The landing page applies the deep-link as soon as the target appears in the entered scope (both
 * `<TerminalsPage>` and `<SystemScopePage>` gate the effect on the target EXISTING), and their
 * auto-select-first is SUPPRESSED while a deep-link is outstanding. So the selection lands
 * whichever of the two — the scope state or the routed search — commits first. The tolerance is
 * the page's; the wrong path is still wrong.
 *
 * ## A SYSTÈME row goes to the Système SCOPE, at `/system`
 *
 * This is the defect the third scope was introduced to close. The cross-project home lists the
 * singleton's terminals and services — `home_snapshot`'s global read joins with no filter at all,
 * deliberately: the host is exactly what one does not want to miss from a transverse view — and
 * clicking one of those rows used to call `setActiveProjectId(<the Système project>)` and navigate
 * to `/`. That project is NOT in `useProjects` (`db::list_projects` filters `kind = 'user'`), so
 * the rail's restore effect saw an entered project that "no longer exists", fell back to another
 * one, and ejected the user onto a stranger's Terminals page. The row is now routed to the scope
 * that actually holds it, with its deep-link intact.
 *
 * ## …and why the other rows are no longer SCOPE-NEUTRAL either
 *
 * This hook used to pose the active project and leave the SCOPE alone, deliberately: clicking a
 * row of the cross-project home read as "show me this terminal", not as "take me out of the
 * transverse view", and the foundation pass escalated the call rather than making it. THE USER
 * RULED THE OTHER WAY (review `01M1ECXAXVGHJWPWA019QAER6G`): « quand je clique en etant sur all
 * sur un agent par exemple depot ici, il m'envoi bien sur la page depot mais je suis toujours
 * considere en all, je dirais qu'il faut que ca switch le projet selection sur le truc que j'ai
 * clique ». Clicking a row IS entering its project, so it leaves the global scope like every other
 * gesture that means "take me into this project".
 *
 * It leaves the scope WITHOUT going through `enterProject`, for the reason above: that path
 * navigates with `search: {}` and would erase the deep-link this hook exists to carry.
 * `setScopeKind` and `setActiveProjectId` are two independent setters on the same context, so the
 * row can take both halves of "enter this project" and still route with its own search — which is
 * exactly what `enterProject` cannot do.
 *
 * ## The row with NO project
 *
 * A session on a LOOSE terminal belongs to no project (a frozen decision: it shows unmarked, never
 * hidden), so there is nothing to enter — the click STAYS in the scope it was made from. Anything
 * else would be an arbitrary switch: the row named a terminal, not a container, and dropping the
 * entered project to `null` would empty the three pages that read it. The terminal is reachable
 * from whichever scope is up, so the deep-link alone is the whole answer here.
 */
export function useHomeFocus(): (target: HomeFocusTarget) => void {
  const { activeProjectId, setActiveProjectId, setScopeKind } = useActiveProject();
  const navigate = useNavigate();
  return useCallback(
    (target: HomeFocusTarget) => {
      // A SYSTÈME ROW: the scope is the singleton's and its screen is `/system`, which carries the
      // same `?terminal=` / `?command=` deep-links. The entered project is left ALONE — it is
      // where the rail puts the user back, and the singleton has no id to put there anyway.
      if (target.system) {
        setScopeKind("system");
        void navigate({ to: "/system", search: target.search });
        return;
      }
      // ENTER the row's project — both halves of it: the project itself when it is not the one we
      // are in, and the way OUT of whatever other scope we are in, which is what the user asked
      // for. A row with NO project (a loose terminal) has neither to give and changes nothing (see
      // the module doc).
      if (target.projectId !== null) {
        if (target.projectId !== activeProjectId) setActiveProjectId(target.projectId);
        // `setScopeKind("project")` is a no-op in the project scope, so this is one expression for
        // both homes rather than a branch on which page we are on.
        setScopeKind("project");
      }
      void navigate({ to: "/", search: target.search });
    },
    [activeProjectId, navigate, setActiveProjectId, setScopeKind],
  );
}
