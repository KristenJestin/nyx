import { useCallback } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useActiveProject } from "@/components/terminals/active-project";

/** What a home row asks for when it is clicked: a project to be in, and a runtime object to open. */
export interface HomeFocusTarget {
  /** The row's project, or `null` — a loose terminal belongs to none and needs no switch. */
  readonly projectId: string | null;
  /** The `/` deep-link: the terminal to select, or the managed command to open. */
  readonly search: { readonly terminal?: string; readonly command?: string };
}

/**
 * `use-home-focus` — the ONE path a home row takes to land on its object, in either scope.
 *
 * ## Why this is not `enterProject`
 *
 * The rail's `enterProject` (`base-layout.tsx`) navigates to `/` with `search: {}` — it has to,
 * because it is a project SWITCH and any `?terminal=`/`?command=` still in the url belongs to the
 * project being LEFT (review `01M069MWRNR1PEZZW7YTPAHSKK`: a stale deep-link there makes
 * `<TerminalsPage>` wait forever for a target that will never appear, and suppresses its
 * last-terminal restore, landing on an empty deck).
 *
 * The home needs the exact opposite: it switches project AND carries a deep-link, and the two are
 * the same act. Routed through `enterProject`, the deep-link would be ERASED by the very
 * navigation meant to carry it — so the home poses the active project and navigates with its own
 * search, in one call. `home-page.test.tsx` covers that erasure explicitly.
 *
 * ## Why the order does not matter
 *
 * `<TerminalsPage>` applies the deep-link as soon as the target appears in the entered scope
 * (`deepLinkExists` gates the effect), and its auto-select-first is SUPPRESSED while a deep-link
 * is outstanding. So the selection lands whichever of the two — the project state or the routed
 * search — commits first. The tolerance is the page's; the wrong path is still wrong.
 *
 * ## …and why it is no longer SCOPE-NEUTRAL
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
 * navigates with `search: {}` and would erase the deep-link this hook exists to carry. `setGlobal
 * Scope` and `setActiveProjectId` are two independent setters on the same context, so the row can
 * take both halves of "enter this project" and still route with its own search — which is exactly
 * what `enterProject` cannot do.
 *
 * ## The row with NO project
 *
 * A session on a LOOSE terminal belongs to no project (a frozen decision: it shows unmarked, never
 * hidden), so there is no project to enter and no scope to leave — the click STAYS in the global
 * scope. Anything else would be an arbitrary switch: the row named a terminal, not a project, and
 * dropping the entered project to `null` would empty the three pages that read it. The terminal is
 * reachable from whichever scope is up, so the deep-link alone is the whole answer here.
 */
export function useHomeFocus(): (target: HomeFocusTarget) => void {
  const { activeProjectId, setActiveProjectId, setGlobalScope } = useActiveProject();
  const navigate = useNavigate();
  return useCallback(
    (target: HomeFocusTarget) => {
      // ENTER the row's project — both halves of it: the project itself when it is not the one we
      // are in, and the way OUT of the global scope, which is what the user asked for. A row with
      // NO project (a loose terminal) has neither to give and changes nothing (see the module doc).
      if (target.projectId !== null) {
        if (target.projectId !== activeProjectId) setActiveProjectId(target.projectId);
        // `setGlobalScope(false)` is a no-op in the project scope, so this is one expression for
        // both homes rather than a branch on which page we are on.
        setGlobalScope(false);
      }
      void navigate({ to: "/", search: target.search });
    },
    [activeProjectId, navigate, setActiveProjectId, setGlobalScope],
  );
}
