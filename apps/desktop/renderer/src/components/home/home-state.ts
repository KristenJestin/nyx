import { aggregateState, STATE_PRIORITY, type RunState } from "@/components/ui/status-dot";

import type { HomeSnapshot } from "./types";

/**
 * The home's session-state vocabulary and its rank — the renderer half of a contract whose other
 * half is `nyx_core::home::Attention`.
 *
 * The backend derives each row's state from the activity nyx already observes and returns the flow
 * already sorted; this module exists so the renderer can read that state, paint it with the SAME
 * dot every other surface uses, and — the load-bearing part — prove it sorts by the SAME rank.
 *
 * THE RANK IS NOT DECLARED HERE. It is READ off `STATE_PRIORITY`, the array the terminal line, the
 * workspace accordion and the project rail dot already fold through. So there is no second order
 * to keep in sync: reorder `STATE_PRIORITY` and the home follows, or `home-state.test.ts` fails
 * saying the two no longer agree.
 */

/** The five states a home agent row can be in (mirrors `nyx_core::home::Attention::as_str`). */
export type HomeSessionState = "waiting" | "error" | "ready" | "working" | "idle";

/**
 * The home vocabulary projected onto the app's presentational {@link RunState} — the translation
 * that lets the home reuse the app's dot AND the app's rank instead of either one twice. Two names
 * differ and nothing else: the home's `ready` IS the dot's settled `success` (a finished turn
 * nobody read) and its `working` IS the dot's `running`.
 */
export const HOME_STATE_RUN_STATE: Readonly<Record<HomeSessionState, RunState>> = {
  waiting: "waiting",
  error: "error",
  ready: "success",
  working: "running",
  idle: "idle",
};

/**
 * The sort rank of a home state — 0 = most attention-hungry — taken from `STATE_PRIORITY` itself
 * via {@link HOME_STATE_RUN_STATE}. `waiting > error > ready > working > idle` is a CONSEQUENCE of
 * that array, never a literal typed out here.
 */
export function homeStateRank(state: HomeSessionState): number {
  const rank = STATE_PRIORITY.indexOf(HOME_STATE_RUN_STATE[state]);
  // An unknown state sorts LAST, never first. `indexOf` answers -1, which would float the row
  // ahead of `waiting` — the one rank that must stay at the top. The Rust twin degrades the same
  // way (`unwrap_or(ATTENTION_ORDER.len())`), so a divergence between the two vocabularies is
  // visible at the bottom of the list instead of masquerading as the most urgent line.
  return rank === -1 ? STATE_PRIORITY.length : rank;
}

/**
 * Compare two home states by rank — the comparator a client-side re-sort uses. Negative when `a`
 * wants more attention than `b`. The backend already returns the flow in this order (one authority
 * for the sort); this is here so a renderer that filters or merges rows cannot drift from it.
 */
export function compareHomeState(a: HomeSessionState, b: HomeSessionState): number {
  return homeStateRank(a) - homeStateRank(b);
}

/**
 * A WHOLE HOME SNAPSHOT folded to ONE state — the rollup the rail's `All projects` tile wears.
 *
 * It agregates EXACTLY what the home's rows paint, leaf by leaf, so the tile and the page it
 * routes to can never disagree about the same runtime:
 *
 *  - an agent SESSION contributes the state the backend already resolved, through the one
 *    translation this module owns ({@link HOME_STATE_RUN_STATE});
 *  - a SERVICE contributes its factual `run_state`, read 1:1 as its row does;
 *  - a bare TERMINAL contributes its settled `exec_state` only while UNREAD — the notification
 *    semantics every dot in the app follows, and the same expression its row uses;
 *  - the SPECS contribute nothing: they carry no dot on the page either.
 *
 * The fold is the shared {@link aggregateState}/`STATE_PRIORITY`, so there is no second rank here
 * any more than there is a second one in `projectRunState`. An empty snapshot rolls up to `idle`,
 * which `isQuietState` hides.
 *
 * THE SCOPE IS THE SNAPSHOT'S, and that is the load-bearing part for the cross-project tile: the
 * global `home_snapshot` LEFT-JOINs terminals → workspaces → projects with NO filter of any kind
 * (`nyx_core::db::home_sessions` & co.), so it sees EVERY project — a future `system` one
 * included, which `list_projects` / `list_workspaces` would not have shown. Building this rollup
 * on the rail's own project tree would have quietly excluded it.
 */
export function homeSnapshotRunState(snapshot: HomeSnapshot): RunState {
  return aggregateState([
    ...snapshot.sessions.map((s) => HOME_STATE_RUN_STATE[s.state]),
    ...snapshot.services.map((s) => s.run_state),
    ...snapshot.terminals.map((t) => (t.exec_state_unread ? t.exec_state : "idle")),
  ]);
}
