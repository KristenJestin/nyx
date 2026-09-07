import type { GlobalSearchHit } from "@/bridge/contract";

/**
 * The renderer's half of the global-search contract: TYPED TARGET → ROUTE.
 *
 * The backend answers an IDENTITY (`{ kind, id, project_id, … }`) and never a url — the routes
 * belong to the renderer and change with it, so the mapping lives here and nowhere else. This
 * module is deliberately PURE: no bridge, no router, no React. It is the whole of the "where does
 * a hit land" rule, which is why the seven kinds and their fallbacks are unit-testable without
 * mounting anything.
 *
 * NO NEW ROUTE, NO NEW SEARCH PARAM (the PRD's hard constraint). Half of the entity types have no
 * surface of their own, and v1 creates none: they navigate to the NEAREST EXISTING container.
 *
 *   spec      → `/work/$specId`
 *   task      → `/work/$specId` (its Spec, default tab — there is no `?task=`)
 *   idea      → `/knowledge`
 *   workspace → `/`
 *   project   → `/`
 *   command   → `/?command=<id>`   (the Spec Runtime tab's existing deep-link — `<id>` is the
 *                                  command INSTANCE, which is what `WorkspaceRuntimeService.id`
 *                                  is; a template id matches no service and opens nothing)
 *   session   → `/?terminal=<id>`  (idem), or `/` when its terminal is no longer alive
 *
 * TWO FALLBACKS, both landing on `/` OF THE RIGHT PROJECT rather than on an error or a blank page:
 *  - a TASK whose `spec_id` is null (`tasks.spec_id` IS nullable — a standalone/review task hangs
 *    off no Spec) has no `/work/$specId` to open;
 *  - a SESSION whose terminal is gone would deep-link `?terminal=` at an id `<TerminalsPage>` will
 *    wait for forever, and that pending deep-link also SUPPRESSES the page's last-terminal restore
 *    — i.e. an empty deck. Dropping the search param is what makes it a landing instead of a trap.
 *
 * ## THE SYSTÈME SIDE-DOOR
 *
 * `global_search` filters no project, so a hit can belong to the Système singleton — a terminal
 * hosting an agent at the platform home, one of the host's managed commands, the singleton's own
 * workspace or project row. Those have no `/` to land on: `/` is the ENTERED project's Terminals,
 * and the singleton is absent from `list_projects` (`kind = 'user'`), so posing it as the entered
 * project made the rail's restore effect "recover" onto a stranger's page. They land on `/system`
 * instead, in the Système SCOPE, carrying the same deep-links — the exact correction
 * `use-home-focus` makes for the cross-project home's rows.
 *
 * The three knowledge kinds are unreachable there by construction: the singleton holds no Spec, no
 * task and no idea.
 */

/** Where a hit lands: the scope to enter first, plus the route (and its params/search). */
export type SearchTarget =
  | {
      readonly projectId: string;
      readonly to: "/";
      /** The `/` deep-links. `{}` = the page's own default (restore the last terminal). */
      readonly search: { readonly terminal?: string; readonly command?: string };
    }
  | {
      /**
       * NO `projectId` — deliberately. The Système scope carries none (`ActiveScope`'s
       * `{kind:"system"}`), and a branch that offered one would invite back the very
       * `setActiveProjectId(<the singleton>)` this route exists to replace.
       */
      readonly to: "/system";
      readonly search: { readonly terminal?: string; readonly command?: string };
    }
  | {
      readonly projectId: string;
      readonly to: "/work/$specId";
      readonly params: { readonly specId: string };
    }
  | { readonly projectId: string; readonly to: "/knowledge" };

/** What the caller must know about the world before a target can be resolved. */
export interface SearchTargetContext {
  /**
   * Is the SESSION hit's terminal still alive? Resolved by the caller against the global
   * `list_terminals` set at activation time (never per keystroke). Ignored for the six other
   * kinds. `false` on an unknown/failed lookup — the fallback is a landing, so being wrong here
   * costs a deep-link, never an error.
   */
  readonly terminalAlive: boolean;
  /**
   * The SYSTÈME singleton's project id, or `null` on a database that has none — handed down by
   * `<BaseLayout>`, which already holds the `system_scope_detail` read the rail's tile needs.
   *
   * A hit whose `project_id` is this one belongs to the HOST, not to a project. This is an
   * IDENTITY comparison against the singleton's own authoritative read, never a guess about names
   * or paths — but it is still a comparison, and the home's rows do better: `home_snapshot` marks
   * each row's project `kind` at join time, so nothing there has to be compared at all. The search
   * payload (`SearchHit`) carries no such mark yet; giving it one means threading a column through
   * the seven candidate queries and the protocol contract, which is a change of its own.
   */
  readonly systemProjectId: string | null;
}

/**
 * Resolve the route ONE hit opens. The `projectId` is ALWAYS the hit's owner project, including on
 * both fallbacks: "the nearest existing surface" is nearest *within the entity's own project*, and
 * the caller activates it before navigating.
 */
export function resolveSearchTarget(
  hit: GlobalSearchHit,
  { terminalAlive, systemProjectId }: SearchTargetContext,
): SearchTarget {
  const projectId = hit.project_id;
  // THE HOST'S OWN ENTITIES, routed to the host's own screen. Resolved BEFORE the kind switch: the
  // kinds that can land here would otherwise each need the same branch, and one of them would
  // eventually be forgotten.
  if (systemProjectId != null && projectId === systemProjectId) {
    switch (hit.kind) {
      case "command":
        return { to: "/system", search: { command: hit.id } };
      case "session":
        return hit.terminal_id != null && hit.terminal_id !== "" && terminalAlive
          ? { to: "/system", search: { terminal: hit.terminal_id } }
          : { to: "/system", search: {} };
      default:
        // The singleton's own workspace / project row — and, defensively, any kind that ever
        // reaches here: the scope's screen is the honest landing, with no deep-link.
        return { to: "/system", search: {} };
    }
  }
  // `search` is stated on the branches landing on `/`, and only there: that route is the one
  // carrying `?command=` / `?terminal=`, so a target that means "no deep-link" has to SAY the empty
  // search rather than leave the previous one standing. `/work/$specId` and `/knowledge` accept no
  // search parameter at all, which is why those two branches omit it — an absence to read as "this
  // route has none", not as a forgotten reset.
  switch (hit.kind) {
    case "spec":
      return { projectId, to: "/work/$specId", params: { specId: hit.id } };
    case "task":
      // A task opens ITS SPEC (default tab). No Spec ⇒ the project's `/`.
      return hit.spec_id != null && hit.spec_id !== ""
        ? { projectId, to: "/work/$specId", params: { specId: hit.spec_id } }
        : { projectId, to: "/", search: {} };
    case "idea":
      return { projectId, to: "/knowledge" };
    case "command":
      // `hit.id` is a `command_instances.id` — the launchable id, and the one
      // `<TerminalsBody>` compares `?command=` against. The backend answers one hit per
      // instance for exactly this reason.
      return { projectId, to: "/", search: { command: hit.id } };
    case "session":
      // The terminal is the ONLY way to a session; gone ⇒ land on `/` with NO stale deep-link.
      return hit.terminal_id != null && hit.terminal_id !== "" && terminalAlive
        ? { projectId, to: "/", search: { terminal: hit.terminal_id } }
        : { projectId, to: "/", search: {} };
    case "workspace":
    case "project":
      return { projectId, to: "/", search: {} };
  }
}
