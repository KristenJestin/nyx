import { PlusIcon } from "lucide-react";

import type { GlobalSearchHit, GlobalSearchKind } from "@/bridge/contract";

/**
 * THE ACTION REGISTRY — what one can DO with a found entity, declared once, per entity KIND.
 *
 * ## Why a registry and not a branch in the palette
 *
 * The palette used to be a way to GO somewhere. It is also a way to CREATE now — the user's own
 * shape for it: « tu crées une liste à côté sur toutes les actions possibles et ça les remonte sur
 * le projet / workspace correspondant ». So the actions are a LIST beside the results, each entry
 * naming the kind it applies to; when an entity surfaces, the entries matching its kind surface with
 * it. Adding a third action later is ONE MORE ENTRY in the table below — never a new branch in the
 * palette, never a new row component. That is the exact pattern `sidebar/agent-providers` already
 * carries for the agent logos, and it is followed here for the same reason: a table can be extended
 * without being understood, a branch cannot.
 *
 * ## What an entry is NOT allowed to be
 *
 * An action is an AFFORDANCE DERIVED from a hit, not a hit. Two consequences the palette enforces
 * and this module makes possible:
 *
 *  - it is not SEARCHABLE by its own label. Nothing here reaches the backend, nothing here is
 *    matched against the query: `Open a new terminal` appears under `scratch` because `scratch` was
 *    found, and would never appear because the word "terminal" was typed;
 *  - it does not COMPETE with the entities for the answer's room. The caps (8 per type, 40 overall)
 *    are the aggregator's and apply to the entities alone; the actions are expanded AFTER them, on
 *    the client, so an action can never evict a result.
 *
 * ## The two entries of this round, and where the work actually happens
 *
 * NOTHING IS CREATED HERE. Each `run` calls back into {@link SearchActionContext}, which the palette
 * fills with the app's EXISTING creation paths — `newTerminalInWorkspace` (the Terminals page's own,
 * reached through the one-shot `terminals/pending-terminal` channel) and `addWorkspace` (the rail's
 * `useManualAdd` picker → name-dialog flow, handed down by `<BaseLayout>`). A second creation path
 * would be a second truth about what "a new workspace" means; there is one, and this table only
 * names it.
 */

/**
 * The capabilities an action may use. The palette supplies them; the registry only names them, so
 * an entry can be read without knowing how the app creates anything.
 */
export interface SearchActionContext {
  /**
   * Open a NEW terminal in `workspaceId` AND ENTER IT — the user asked for both halves («
   * ajouter ET entrer »), so this lands the user inside the terminal it creates, exactly as the
   * workspace band's own "+" does.
   */
  readonly newTerminalInWorkspace: (workspaceId: string, projectId: string) => void;
  /**
   * Add a workspace to `projectId`: the native folder picker, then the name dialog, then the
   * creation — the rail's flow, unchanged. Cancelling the picker creates nothing.
   */
  readonly addWorkspaceToProject: (projectId: string) => void;
}

/** One entry of the registry: the kind it applies to, what it reads like, and what it does. */
export interface SearchAction {
  /** Stable identity — the React key of the row, and what a test names. */
  readonly id: string;
  /** The entity KIND this action applies to. An entity surfaces its kind's actions, and only those. */
  readonly kind: GlobalSearchKind;
  /**
   * The glyph. `PlusIcon` on both entries today, and deliberately: what an action line must say at
   * a glance is "this CREATES something", which no entity glyph in `search-meta` says. The field
   * stays per-entry so a future action that is not a creation can carry its own.
   */
  readonly icon: typeof PlusIcon;
  /**
   * The line's text — VERB FIRST, so it reads as something one DOES rather than as an entity's
   * name. It is a CONSTANT, not a function of the hit: the line draws the entity's own context
   * block at its right edge (`audio player · [M] Music Manager`), so naming the target here would
   * say the same thing twice on one line. It was `Open a new terminal in ${hit.label}` while the
   * line had no context to say it for it.
   */
  readonly label: string;
  /** Perform it. Everything it needs comes from {@link SearchActionContext}. */
  readonly run: (hit: GlobalSearchHit, ctx: SearchActionContext) => void;
}

/**
 * THE LIST. Order matters only within one kind — the actions of a hit are drawn under it in the
 * order they appear here.
 */
export const SEARCH_ACTIONS: readonly SearchAction[] = [
  {
    id: "workspace:new-terminal",
    kind: "workspace",
    icon: PlusIcon,
    label: "Open a new terminal",
    // `hit.id` IS the workspace id on this kind (`db::recent_search_workspaces` selects
    // `w.id AS id` and `w.id AS workspace_id`); `workspace_id` is preferred all the same, so the
    // entry reads as "the workspace this hit lives in" rather than as a coincidence of the query.
    run: (hit, ctx) => ctx.newTerminalInWorkspace(hit.workspace_id ?? hit.id, hit.project_id),
  },
  {
    id: "project:new-workspace",
    kind: "project",
    icon: PlusIcon,
    label: "Add a workspace",
    run: (hit, ctx) => ctx.addWorkspaceToProject(hit.project_id),
  },
];

/**
 * The actions that apply to one hit — the whole of the "which affordances ride with this entity"
 * rule, and a plain filter on the table above. Total: a kind with no entry yields none, which is
 * what every kind but two does today.
 */
export function searchActionsFor(hit: GlobalSearchHit): readonly SearchAction[] {
  return SEARCH_ACTIONS.filter((action) => action.kind === hit.kind);
}
