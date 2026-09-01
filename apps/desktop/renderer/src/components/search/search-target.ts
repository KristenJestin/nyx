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
 */

/** Where a hit lands: the project to ENTER first, plus the route (and its params/search). */
export type SearchTarget =
  | {
      readonly projectId: string;
      readonly to: "/";
      /** The `/` deep-links. `{}` = the page's own default (restore the last terminal). */
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
}

/**
 * Resolve the route ONE hit opens. The `projectId` is ALWAYS the hit's owner project, including on
 * both fallbacks: "the nearest existing surface" is nearest *within the entity's own project*, and
 * the caller activates it before navigating.
 */
export function resolveSearchTarget(
  hit: GlobalSearchHit,
  { terminalAlive }: SearchTargetContext,
): SearchTarget {
  const projectId = hit.project_id;
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
