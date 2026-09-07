import { STATE_PRIORITY, type RunState } from "@/components/ui/status-dot";

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
