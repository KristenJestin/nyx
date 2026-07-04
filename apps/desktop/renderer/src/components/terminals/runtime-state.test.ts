import { describe, expect, it } from "vitest";

import {
  activeAgentSession,
  notifiedRunState,
  projectRunState,
  serviceRunState,
  terminalRunState,
  workspaceRunState,
} from "./runtime-state";
import type {
  WorkspaceRuntimeService,
  WorkspaceRuntimeSession,
  WorkspaceRuntimeSnapshot,
  WorkspaceRuntimeTerminal,
} from "./use-workspace-runtime";

function session(id: string, state: string): WorkspaceRuntimeSession {
  return {
    id,
    terminal_id: "t1",
    workspace_id: "ws1",
    agent_kind: "claude_code",
    external_session_id: `ext-${id}`,
    state,
  };
}

function terminal(overrides: Partial<WorkspaceRuntimeTerminal> = {}): WorkspaceRuntimeTerminal {
  return {
    id: "t1",
    cwd: "/repo",
    label: null,
    workspace_id: "ws1",
    status: "alive",
    exec_state: "idle",
    exec_state_unread: false,
    sessions: [],
    ...overrides,
  };
}

function service(overrides: Partial<WorkspaceRuntimeService> = {}): WorkspaceRuntimeService {
  return {
    id: "s1",
    workspace_id: "ws1",
    name: "dev",
    command: "npm run dev",
    run_state: "idle",
    exit_code: null,
    unread: false,
    ...overrides,
  };
}

function snapshot(
  terminals: WorkspaceRuntimeTerminal[],
  services: WorkspaceRuntimeService[],
): WorkspaceRuntimeSnapshot {
  return {
    workspace: { id: "ws1", name: "root", path: "/repo" },
    terminals,
    agent_sessions: terminals.flatMap((t) => t.sessions),
    services,
  };
}

describe("terminalRunState / serviceRunState", () => {
  it("reads the terminal exec_state straight through", () => {
    expect(terminalRunState(terminal({ exec_state: "running" }))).toBe("running");
    expect(terminalRunState(terminal({ exec_state: "error" }))).toBe("error");
  });

  it("reads the service factual run_state straight through", () => {
    expect(serviceRunState(service({ run_state: "success" }))).toBe("success");
    expect(serviceRunState(service({ run_state: "running" }))).toBe("running");
  });
});

describe("workspaceRunState", () => {
  it("is idle for an empty workspace", () => {
    expect(workspaceRunState(snapshot([], []))).toBe("idle");
  });

  it("rolls up the most attention-worthy state across terminals and services", () => {
    // A settled (UNREAD) success service outranks a running terminal (reordered priority: a
    // settled unseen result wins over a busy one — (running + unread success) → green).
    expect(
      workspaceRunState(
        snapshot(
          [terminal({ exec_state: "running" })],
          [service({ run_state: "success", unread: true })],
        ),
      ),
    ).toBe("success");
    // An unread error outranks running.
    expect(
      workspaceRunState(
        snapshot(
          [terminal({ exec_state: "running" })],
          [service({ run_state: "error", unread: true })],
        ),
      ),
    ).toBe("error");
  });

  it("without an agent-activity lookup, folds terminals + services only (exec-state axis)", () => {
    // A terminal hosting a live agent session but idle exec_state contributes only its
    // exec_state when no activity lookup is supplied — the historical exec-state-only rollup.
    const withAgent = terminal({ exec_state: "idle", sessions: [session("a1", "active")] });
    expect(workspaceRunState(snapshot([withAgent], []))).toBe("idle");
  });

  it("folds an agent BLOCKED ON THE USER (waiting) into the rollup as yellow", () => {
    // A terminal hosting a live agent session whose runtime activity is `waiting` contributes
    // `waiting` (yellow) even though its own exec_state is idle — reached ONLY via the lookup.
    const t = terminal({ id: "t-wait", exec_state: "idle", sessions: [session("a1", "active")] });
    const lookup = (id: string) => (id === "t-wait" ? ("waiting" as const) : undefined);
    expect(workspaceRunState(snapshot([t], []), lookup)).toBe("waiting");
  });

  it("waiting OUTRANKS error (a human on the critical path wins the rollup)", () => {
    // waiting > error > success > running — the reordered STATE_PRIORITY. An agent awaiting
    // input alongside a failed (unread) service still rolls up to yellow.
    const t = terminal({ id: "t-wait", exec_state: "idle", sessions: [session("a1", "active")] });
    const lookup = (id: string) => (id === "t-wait" ? ("waiting" as const) : undefined);
    expect(
      workspaceRunState(snapshot([t], [service({ run_state: "error", unread: true })]), lookup),
    ).toBe("waiting");
  });

  it("success OUTRANKS running (a settled UNREAD result wins over a busy one)", () => {
    // The reorder moved success above running: (running + unread success) → green, not blue.
    expect(
      workspaceRunState(
        snapshot(
          [terminal({ exec_state: "running" })],
          [service({ run_state: "success", unread: true })],
        ),
      ),
    ).toBe("success");
  });

  it("a NON-agent terminal never contributes waiting, even if the lookup reports one", () => {
    // No live agent session on this terminal → `activeAgentSession` is undefined → the waiting
    // axis is skipped regardless of the activity lookup (never fabricate waiting for a PTY).
    const plain = terminal({ id: "t-plain", exec_state: "running", sessions: [] });
    const lookup = () => "waiting" as const;
    expect(workspaceRunState(snapshot([plain], []), lookup)).toBe("running");
  });

  it("an agent that is WORKING (not waiting) contributes only its exec_state, not yellow", () => {
    // A live agent whose activity is `working` is busy, not blocked — no yellow. Its terminal
    // exec_state (running here) carries; waiting is reserved for blocked-on-user.
    const t = terminal({
      id: "t-work",
      exec_state: "running",
      sessions: [session("a1", "active")],
    });
    const lookup = () => "working" as const;
    expect(workspaceRunState(snapshot([t], []), lookup)).toBe("running");
  });

  // ── NOTIFICATION-AWARE rollup (finding: consumed/unread threaded into the aggregate) ──

  it("a settled success stops contributing once CONSUMED (visited) → rollup goes idle", () => {
    // The ONLY leaf is a settled, still-unread success terminal → rollup is green. Marking that
    // terminal consumed (visited/read) drops its contribution, so the workspace rolls up to
    // idle — no dot — exactly as the terminal's own row dot disappears.
    const t = terminal({ id: "t-ok", exec_state: "success", exec_state_unread: true });
    expect(workspaceRunState(snapshot([t], []), undefined)).toBe("success");
    const consumed = (kind: "terminal" | "service", id: string) =>
      kind === "terminal" && id === "t-ok";
    expect(workspaceRunState(snapshot([t], []), undefined, consumed)).toBe("idle");
  });

  it("a settled success stops contributing once READ (unread=false) even without a consumed lookup", () => {
    // A success the snapshot already reports as read (`exec_state_unread: false`) never
    // contributes to the rollup — the notification-aware projection folds it to idle.
    const t = terminal({ id: "t-ok", exec_state: "success", exec_state_unread: false });
    expect(workspaceRunState(snapshot([t], []))).toBe("idle");
  });

  it("keeps the rollup GREEN when ANOTHER settled success is still unread/unconsumed", () => {
    // Two settled successes: one consumed, one still unread → the workspace stays green because
    // the OTHER unread success still contributes. Consuming one leaf must not clear the whole.
    const consumedTerm = terminal({ id: "t-seen", exec_state: "success", exec_state_unread: true });
    const freshTerm = terminal({ id: "t-fresh", exec_state: "success", exec_state_unread: true });
    const consumed = (kind: "terminal" | "service", id: string) =>
      kind === "terminal" && id === "t-seen";
    expect(workspaceRunState(snapshot([consumedTerm, freshTerm], []), undefined, consumed)).toBe(
      "success",
    );
  });

  it("a RUNNING leaf ALWAYS contributes (blue) regardless of consumed/unread", () => {
    // `running` is a LIVE state, never a notification — a consumed lookup that would match it
    // must not silence it. The rollup stays blue.
    const t = terminal({ id: "t-run", exec_state: "running", exec_state_unread: true });
    const consumedAll = () => true;
    expect(workspaceRunState(snapshot([t], []), undefined, consumedAll)).toBe("running");
  });

  it("threads the consumed axis to SERVICES too (a read service success stops contributing)", () => {
    // A settled, unread service success rolls up green; consuming that service drops it to idle.
    const svc = service({ id: "s-ok", run_state: "success", unread: true });
    expect(workspaceRunState(snapshot([], [svc]))).toBe("success");
    const consumed = (kind: "terminal" | "service", id: string) =>
      kind === "service" && id === "s-ok";
    expect(workspaceRunState(snapshot([], [svc]), undefined, consumed)).toBe("idle");
  });
});

describe("projectRunState", () => {
  it("is idle when the project has no loaded workspace runtime", () => {
    expect(projectRunState([])).toBe("idle");
  });

  it("folds every workspace rollup into one state (most attention-worthy wins)", () => {
    // Workspace A is quiet (idle); workspace B has a running terminal → the project rolls
    // up to running, so the rail dot reflects the whole project's runtime.
    const wsA = snapshot([terminal({ exec_state: "idle" })], []);
    const wsB = snapshot([terminal({ exec_state: "running" })], []);
    expect(projectRunState([wsA, wsB])).toBe("running");
    // An unread error in ANY workspace outranks a running elsewhere.
    const wsErr = snapshot([], [service({ run_state: "error", unread: true })]);
    expect(projectRunState([wsB, wsErr])).toBe("error");
  });

  it("agrees with the workspace rollup for a single-workspace project", () => {
    const ws = snapshot([terminal({ exec_state: "success", exec_state_unread: true })], []);
    expect(projectRunState([ws])).toBe(workspaceRunState(ws));
  });

  it("threads the agent waiting axis through so a blocked agent in ANY workspace → yellow", () => {
    // Workspace A is busy (running); workspace B hosts an agent blocked on the user. The
    // project rolls up to waiting (yellow) — the highest priority — via the shared lookup.
    const wsA = snapshot([terminal({ exec_state: "running" })], []);
    const waitTerm = terminal({
      id: "t-wait",
      exec_state: "idle",
      sessions: [session("a1", "active")],
    });
    const wsB = snapshot([waitTerm], []);
    const lookup = (id: string) => (id === "t-wait" ? ("waiting" as const) : undefined);
    expect(projectRunState([wsA, wsB], lookup)).toBe("waiting");
  });

  it("threads the consumed axis so a project's ONLY consumed success rolls up idle", () => {
    // A single-workspace project whose only leaf is a consumed success rolls up to idle — the
    // notification-aware axis reaches the project dot via the shared workspace rollup.
    const t = terminal({ id: "t-ok", exec_state: "success", exec_state_unread: true });
    const ws = snapshot([t], []);
    const consumed = (kind: "terminal" | "service", id: string) =>
      kind === "terminal" && id === "t-ok";
    expect(projectRunState([ws], undefined, consumed)).toBe("idle");
  });

  it("keeps the project GREEN when another workspace has an unread success", () => {
    // Workspace A's success is consumed; workspace B's success is still unread → the project
    // stays green (the unread one in B still contributes).
    const seen = terminal({ id: "t-seen", exec_state: "success", exec_state_unread: true });
    const fresh = terminal({ id: "t-fresh", exec_state: "success", exec_state_unread: true });
    const consumed = (kind: "terminal" | "service", id: string) =>
      kind === "terminal" && id === "t-seen";
    expect(
      projectRunState([snapshot([seen], []), snapshot([fresh], [])], undefined, consumed),
    ).toBe("success");
  });
});

describe("notifiedRunState (Attention-Inbox dot semantics)", () => {
  it("shows a settled success/error ONLY while unread AND not the viewed row", () => {
    // Unread + not selected → the notification dot shows.
    expect(notifiedRunState("success", true, false)).toBe("success");
    expect(notifiedRunState("error", true, false)).toBe("error");
  });

  it("hides the settled dot on the currently-selected row (you are on it → seen)", () => {
    expect(notifiedRunState("success", true, true)).toBe("idle");
    expect(notifiedRunState("error", true, true)).toBe("idle");
  });

  it("hides the settled dot once the result is read (consumed), selected or not", () => {
    expect(notifiedRunState("success", false, false)).toBe("idle");
    expect(notifiedRunState("error", false, true)).toBe("idle");
  });

  it("ALWAYS shows a live running state — it is not a notification (selected or unread irrelevant)", () => {
    expect(notifiedRunState("running", false, true)).toBe("running");
    expect(notifiedRunState("running", true, false)).toBe("running");
    expect(notifiedRunState("running", false, false)).toBe("running");
  });

  it("passes idle through unchanged", () => {
    expect(notifiedRunState("idle", false, false)).toBe("idle");
    expect(notifiedRunState("idle", true, true)).toBe("idle");
  });
});

describe("activeAgentSession", () => {
  it("returns undefined for a plain terminal", () => {
    expect(activeAgentSession(terminal())).toBeUndefined();
  });

  it("returns the newest non-ended session (agents live on their terminal)", () => {
    const t = terminal({
      sessions: [session("old", "ended"), session("new", "working")],
    });
    expect(activeAgentSession(t)?.id).toBe("new");
  });

  it("ignores an ended session", () => {
    const t = terminal({ sessions: [session("done", "ended")] });
    expect(activeAgentSession(t)).toBeUndefined();
  });
});
