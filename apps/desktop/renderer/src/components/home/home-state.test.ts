import { describe, expect, it } from "vitest";

import { aggregateState, STATE_PRIORITY, type RunState } from "@/components/ui/status-dot";
import {
  compareHomeState,
  homeSnapshotRunState,
  homeStateRank,
  HOME_STATE_RUN_STATE,
  type HomeSessionState,
} from "./home-state";
import { EMPTY_HOME_SNAPSHOT, type HomeSnapshot } from "./types";

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

/**
 * THE SNAPSHOT ROLLUP — the state the rail's `All projects` tile wears.
 *
 * It folds what the home's ROWS paint (a session's resolved state, a service's factual one, a
 * terminal's settled state while unread) through the SAME `aggregateState`, so the tile and the
 * page can never disagree. What is proven here is the fold; that the snapshot it folds sees every
 * project — a `system` one included — is proven where the read is made
 * (`terminals/all-projects-dot.test.tsx`).
 */

const PROJECT = { id: "p1", name: "nyx", accent: null, icon: null, image_path: null };
const WORKSPACE = { id: "ws1", name: "nyx-v2", branch: null };

function snapshot(parts: Partial<HomeSnapshot>): HomeSnapshot {
  return { ...EMPTY_HOME_SNAPSHOT, ...parts };
}

describe("homeSnapshotRunState", () => {
  it("rolls an empty snapshot up to the quiet idle", () => {
    expect(homeSnapshotRunState(EMPTY_HOME_SNAPSHOT)).toBe("idle");
  });

  it("takes the highest-ranked leaf, in the app's own priority", () => {
    const rolled = homeSnapshotRunState(
      snapshot({
        // A working agent (running), a failed service (error) and a waiting agent (waiting): the
        // human on the critical path outranks the rest, as STATE_PRIORITY says.
        sessions: [
          {
            session_id: "s1",
            terminal_id: "t1",
            agent_kind: "claude_code",
            state: "working",
            secondary: null,
            label: null,
            cwd: "/",
            started_at: 0,
            workspace: WORKSPACE,
            project: PROJECT,
          },
          {
            session_id: "s2",
            terminal_id: "t2",
            agent_kind: "claude_code",
            state: "waiting",
            secondary: null,
            label: null,
            cwd: "/",
            started_at: 0,
            workspace: WORKSPACE,
            project: PROJECT,
          },
        ],
        services: [
          {
            instance_id: "i1",
            name: "dev",
            run_state: "error",
            exit_code: 1,
            unread: true,
            workspace: WORKSPACE,
            project: PROJECT,
          },
        ],
      }),
    );
    expect(rolled).toBe("waiting");
  });

  it("counts a bare terminal's settled state only while it is UNREAD, as its row does", () => {
    const terminal = (exec_state: "error" | "success", exec_state_unread: boolean) => ({
      terminal_id: "t9",
      label: null,
      cwd: "/",
      exec_state,
      exec_state_unread,
      workspace: WORKSPACE,
      project: PROJECT,
    });
    expect(homeSnapshotRunState(snapshot({ terminals: [terminal("error", true)] }))).toBe("error");
    // Read: the row's dot disappears, and so does its contribution here — no second notif rule.
    expect(homeSnapshotRunState(snapshot({ terminals: [terminal("error", false)] }))).toBe("idle");
  });

  it("ignores the specs — they carry no dot on the page either", () => {
    const rolled = homeSnapshotRunState(
      snapshot({
        specs: [
          {
            spec_id: "sp1",
            key: "NYX-1",
            title: "Something",
            status: "in_progress",
            updated_at: 0,
            tasks_done: 1,
            tasks_total: 3,
            phase: null,
            project: PROJECT,
          },
        ],
      }),
    );
    expect(rolled).toBe("idle");
  });
});
