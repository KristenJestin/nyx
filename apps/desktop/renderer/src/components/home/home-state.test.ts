import { describe, expect, it } from "vitest";

import { aggregateState, STATE_PRIORITY, type RunState } from "@/components/ui/status-dot";
import {
  compareHomeState,
  homeStateRank,
  HOME_STATE_RUN_STATE,
  type HomeSessionState,
} from "./home-state";

/**
 * THE RANK CONTRACT. The home sorts its flat, cross-project flow of agent rows by
 * `waiting > error > ready > working > idle`. That is not the home's order — it is `STATE_PRIORITY`,
 * the one every rollup in the app already folds through, in the home's vocabulary.
 *
 * The PRODUCTION code derives that order (`home-state.ts` reads it off `STATE_PRIORITY` and never
 * declares one). A test that derived it too would agree with itself and catch nothing, so
 * {@link CONTRACT_ORDER} below RESTATES the sequence, once — that literal IS the guard. The two
 * tests then check two independent derivations against it: the comparator the renderer sorts with,
 * and the projection of `STATE_PRIORITY` through the home's vocabulary. Reorder `STATE_PRIORITY`
 * (or remap a home state onto a different dot) and both fail. Its Rust twin is
 * `nyx_core::home::tests::attention_order_is_the_app_priority`, which pins the same sequence on the
 * side that actually performs the sort.
 */

/**
 * The five states, deliberately NOT in contract order: sorting an already-sorted array is a no-op,
 * so a comparator that ranked everything equal would slip through unnoticed.
 */
const HOME_STATES: HomeSessionState[] = ["idle", "ready", "waiting", "working", "error"];

/** THE FROZEN SEQUENCE — the one place it is written down, and the only thing both tests trust. */
const CONTRACT_ORDER: HomeSessionState[] = ["waiting", "error", "ready", "working", "idle"];

describe("home session-state rank", () => {
  it("orders the five states exactly as STATE_PRIORITY does", () => {
    const ordered = [...HOME_STATES].sort(compareHomeState);
    expect(ordered).toEqual(CONTRACT_ORDER);
  });

  it("takes its rank FROM STATE_PRIORITY — no second order is declared", () => {
    // Walk STATE_PRIORITY and keep the entries the home speaks: the result must BE the contract
    // sequence. This never consults the comparator, so it fails on a reorder of STATE_PRIORITY (or
    // a remap of HOME_STATE_RUN_STATE) even if `homeStateRank` were rewritten to agree with it.
    const fromPriority = STATE_PRIORITY.flatMap((run: RunState) =>
      HOME_STATES.filter((state) => HOME_STATE_RUN_STATE[state] === run),
    );
    expect(fromPriority).toEqual(CONTRACT_ORDER);
  });

  it("maps every home state onto a real dot state, and only `ready`/`working` are renamed", () => {
    for (const state of HOME_STATES) {
      const run = HOME_STATE_RUN_STATE[state];
      expect(STATE_PRIORITY).toContain(run);
      // The dot the home paints is the dot the rest of the app paints for that state.
      expect(aggregateState([run])).toBe(run);
    }
    expect(HOME_STATE_RUN_STATE.ready).toBe("success");
    expect(HOME_STATE_RUN_STATE.working).toBe("running");
  });

  it("ranks a human on the critical path above everything, idle below everything", () => {
    for (const state of HOME_STATES.filter((s) => s !== "waiting")) {
      expect(homeStateRank("waiting")).toBeLessThan(homeStateRank(state));
    }
    for (const state of HOME_STATES.filter((s) => s !== "idle")) {
      expect(homeStateRank("idle")).toBeGreaterThan(homeStateRank(state));
    }
  });
});
