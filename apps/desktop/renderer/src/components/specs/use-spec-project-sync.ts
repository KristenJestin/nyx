import { useEffect, useRef } from "react";

import { useActiveProject } from "@/components/terminals/active-project";

import type { SpecDetail } from "./types";

/**
 * `use-spec-project-sync` — a LOADED Spec is the authority on which project the shell is in.
 *
 * The gap (PRD fix 2, deep-link half): the entered project lives in `<ActiveProjectProvider>` and
 * only ever moved when the user clicked a rail tile. So OPENING a Spec directly — a `/work/$specId`
 * URL restored at launch, a link out of an agent message, a back/forward between two Specs of
 * DIFFERENT projects — showed that Spec's page while the rail still highlighted the PREVIOUS
 * project. Everything project-scoped around the page (the rail's selection, the prototype's design
 * system + component catalogue, the terminal target of the Runtime tab) then read the wrong key.
 *
 * The rule is deliberately one-way and URL-silent: the ROUTE wins, the selection follows.
 *  - Source of truth: `detail.spec.project_id` — the Spec's OWNER project, not the optional dev
 *    workspace's (a Spec can have no workspace at all, and the two are the same project when it
 *    has one).
 *  - It NEVER navigates and never rewrites the URL: a deep-link keeps its route (that is the whole
 *    point of the deep-link), so there is no redirect and therefore NO navigation loop.
 *  - It stays quiet until the read model lands: while `detail` is `null` (loading) or the Spec is
 *    UNKNOWN (the hook's `error` state, `detail` stays `null`), the active project is left exactly
 *    as it was and the page shows its existing error state.
 *
 * IT FIRES ON THE SPEC, NOT ON THE SELECTION — and that is load-bearing, not an optimisation. The
 * obvious form ("whenever the two disagree, correct the selection") FIGHTS the rail: entering
 * another project from a Spec page moves the selection first and only THEN routes away (the shell's
 * `enterProject`), so a selection-watching effect would see the disagreement in between and drag the
 * user straight back into the Spec's project — the rail click would silently do nothing. So the
 * write is keyed on the OWNER project of the Spec that is loaded: a newly loaded Spec (deep-link,
 * back/forward, remount) syncs once, and a deliberate rail move afterwards is left alone.
 *
 * Shared by the three Spec-owned screens — detail (`/work/$specId`), review
 * (`/work/$specId/review`) and prototype (`/work/$specId/prototype`) — so they cannot drift. It is
 * also the ONLY sync path: the Runtime tab used to call `setActiveProjectId` inline before each of
 * its two deep-links, which is now redundant (its page is already synced by this hook, one level up)
 * and was a second, duplicated definition of "which project a Spec puts you in".
 */
export function useSpecProjectSync(detail: SpecDetail | null): void {
  const { setActiveProjectId } = useActiveProject();
  const ownerProjectId = detail?.spec.project_id ?? null;
  /** The owner project this mount has already applied — what makes the write once-per-Spec. */
  const applied = useRef<string | null>(null);

  useEffect(() => {
    if (ownerProjectId === null) return;
    if (applied.current === ownerProjectId) return;
    applied.current = ownerProjectId;
    setActiveProjectId(ownerProjectId);
  }, [ownerProjectId, setActiveProjectId]);
}
