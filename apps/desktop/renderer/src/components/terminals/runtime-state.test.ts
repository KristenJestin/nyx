import { describe, expect, it } from "vitest";

import type { AgentActivity } from "@/components/sidebar/use-agent-sessions";
import {
  activeAgentSession,
  notifiedRunState,
  projectRunState,
  serviceRunState,
  terminalLineState,
  terminalRunState,
  workspaceRunState,
} from "./runtime-state";
import type {
  WorkspaceRuntimeService,
  WorkspaceRuntimeSession,
  WorkspaceRuntimeSnapshot,
  WorkspaceRuntimeTerminal,
} from "@/components/runtime/use-workspace-runtime";

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
    command_id: "c1",
    workspace_id: "ws1",
    name: "dev",
    command: "npm run dev",
    subfolder: null,
    description: null,
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

/** A per-terminal live agent activity (the `byActivity.get(id)` shape). */
function activity(overrides: Partial<AgentActivity> = {}): AgentActivity {
  return {
    activity: "idle",
    readyUnread: false,
    errorUnread: false,
    pluginOutdated: false,
    waitingOn: null,
    tool: null,
    ...overrides,
  };
}

describe("terminalRunState / serviceRunState", () => {
  it("falls back to exec_state when the terminal is not busy", () => {
    expect(terminalRunState(terminal({ exec_state: "error" }))).toBe("error");
    expect(terminalRunState(terminal({ exec_state: "idle" }))).toBe("idle");
    // `busy` absent (a pure-DB / MCP snapshot with no live overlay) reads as not busy.
    expect(terminalRunState(terminal({ exec_state: "success", busy: undefined }))).toBe("success");
    expect(terminalRunState(terminal({ exec_state: "idle", busy: false }))).toBe("idle");
  });

  it("review T1: the OS busy bit lights `running` on the LINE, over exec_state", () => {
    // A foreground command (busy) reads `running` even though OSC 133 `exec_state` never
    // carries `running` (it only settles to success/error). This is the restored terminal mode.
    expect(terminalRunState(terminal({ busy: true, exec_state: "idle" }))).toBe("running");
    // Busy takes priority over a stale settled exec_state (a new foreground command started).
    expect(terminalRunState(terminal({ busy: true, exec_state: "success" }))).toBe("running");
    expect(terminalRunState(terminal({ busy: true, exec_state: "error" }))).toBe("running");
  });

  it("reads the service factual run_state straight through", () => {
    expect(serviceRunState(service({ run_state: "success" }))).toBe("success");
    expect(serviceRunState(service({ run_state: "running" }))).toBe("running");
  });
});

describe("terminalLineState (two-mode LINE dot — review T2)", () => {
  const agentTerm = terminal({ sessions: [session("a1", "active")] });
  const opts = { terminalUnread: false, selected: false };

  it("AGENT MODE: working → running, waiting → waiting (live, always shown)", () => {
    expect(terminalLineState(agentTerm, activity({ activity: "working" }), opts)).toBe("running");
    expect(terminalLineState(agentTerm, activity({ activity: "waiting" }), opts)).toBe("waiting");
    // A live agent state shows even on the selected/viewed row (never a consumable notification).
    expect(
      terminalLineState(agentTerm, activity({ activity: "waiting" }), {
        terminalUnread: false,
        selected: true,
      }),
    ).toBe("waiting");
  });

  it("AGENT MODE: idle agent shows the settled ready (green) / error (red, priority) notification", () => {
    expect(terminalLineState(agentTerm, activity({ readyUnread: true }), opts)).toBe("success");
    expect(
      terminalLineState(agentTerm, activity({ readyUnread: true, errorUnread: true }), opts),
    ).toBe("error");
    // Suppressed while the agent line is the viewed row (seen), and when nothing is unread.
    expect(
      terminalLineState(agentTerm, activity({ readyUnread: true }), {
        terminalUnread: false,
        selected: true,
      }),
    ).toBe("idle");
    expect(terminalLineState(agentTerm, activity({ activity: "idle" }), opts)).toBe("idle");
  });

  it("AGENT MODE PRIMES over the terminal mode (busy/exec_state ignored on an agent line)", () => {
    const busyAgent = terminal({
      busy: true,
      exec_state: "success",
      exec_state_unread: true,
      sessions: [session("a1", "active")],
    });
    // Agent working → running (not from busy); agent waiting → waiting (over the success exec_state).
    expect(terminalLineState(busyAgent, activity({ activity: "waiting" }), opts)).toBe("waiting");
  });

  it("TERMINAL MODE (no session): falls back to busy/exec_state (task T1)", () => {
    expect(
      terminalLineState(terminal({ busy: true }), activity({ activity: "working" }), opts),
    ).toBe("running");
    expect(
      terminalLineState(terminal({ exec_state: "success" }), undefined, {
        terminalUnread: true,
        selected: false,
      }),
    ).toBe("success");
  });

  it("AGENT MODE with NO live activity entry (idle at the prompt) → idle, NOT running — busy IGNORED", () => {
    // The BUG this fixes: a fresh/idle Claude session sits at the prompt with no activity entry
    // (`byActivity.get(id) === undefined`) while it is the foreground process (OS `busy` ON). A
    // line that CARRIES an agent stays in AGENT mode and shows an EMPTY dot — never the
    // busy-derived `running`.
    const busyIdleAgent = terminal({ busy: true, sessions: [session("a1", "active")] });
    expect(terminalLineState(busyIdleAgent, undefined, opts)).toBe("idle");
    // Same for a resumed agent with a stale settled exec_state and no activity entry: agent mode
    // ignores exec_state too (no phantom green from a persisted result), so the dot is empty.
    const resumed = terminal({
      busy: true,
      exec_state: "success",
      exec_state_unread: true,
      sessions: [session("a1", "active")],
    });
    expect(terminalLineState(resumed, undefined, { terminalUnread: true, selected: false })).toBe(
      "idle",
    );
  });

  it("AGENT MODE via PASSIVE detection (detected_agent_kind, no session) → busy IGNORED → idle", () => {
    // A hand-launched agent adopted by passive foreground detection (review T3: no hook session
    // yet) is STILL an agent line — its busy bit is ignored and, with no activity entry, the dot
    // is empty (not the busy `running`).
    const detected = terminal({ busy: true, detected_agent_kind: "claude_code", sessions: [] });
    expect(terminalLineState(detected, undefined, opts)).toBe("idle");
    // Its live activity still primes the dot when present.
    expect(terminalLineState(detected, activity({ activity: "working" }), opts)).toBe("running");
  });

  it("NON-REGRESSION: a NON-agent terminal (no session, no detected agent) + busy → running (T1)", () => {
    // A plain PTY with a foreground command must STILL light the busy `running` dot — only an
    // agent line ignores busy. `activity` is irrelevant (there is no agent to own it).
    expect(terminalLineState(terminal({ busy: true }), undefined, opts)).toBe("running");
    expect(terminalLineState(terminal({ busy: true }), activity({ activity: "idle" }), opts)).toBe(
      "running",
    );
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

  it("without an agent-activity lookup, an agent terminal has no activity → idle (busy IGNORED)", () => {
    // A terminal hosting a live agent session with NO activity lookup has no activity entry, so
    // (exactly as on its line) it is in AGENT mode with an empty dot — the OS busy bit is IGNORED,
    // it does NOT fall back to a busy-derived running. An idle-exec_state agent rolls up idle.
    const withAgent = terminal({ exec_state: "idle", sessions: [session("a1", "active")] });
    expect(workspaceRunState(snapshot([withAgent], []))).toBe("idle");
    // Even busy, an agent with no activity entry contributes idle (the bug this fixes: the rollup
    // used to read the busy bit and show running for a foreground agent).
    const busyAgent = terminal({ busy: true, sessions: [session("a1", "active")] });
    expect(workspaceRunState(snapshot([busyAgent], []))).toBe("idle");
  });

  it("folds an agent BLOCKED ON THE USER (waiting) into the rollup as yellow", () => {
    // A terminal hosting a live agent session whose runtime activity is `waiting` contributes
    // `waiting` (yellow) even though its own exec_state is idle — reached ONLY via the lookup.
    const t = terminal({ id: "t-wait", exec_state: "idle", sessions: [session("a1", "active")] });
    const lookup = (id: string) =>
      id === "t-wait" ? activity({ activity: "waiting" }) : undefined;
    expect(workspaceRunState(snapshot([t], []), lookup)).toBe("waiting");
  });

  it("waiting OUTRANKS error (a human on the critical path wins the rollup)", () => {
    // waiting > error > success > running — the reordered STATE_PRIORITY. An agent awaiting
    // input alongside a failed (unread) service still rolls up to yellow.
    const t = terminal({ id: "t-wait", exec_state: "idle", sessions: [session("a1", "active")] });
    const lookup = (id: string) =>
      id === "t-wait" ? activity({ activity: "waiting" }) : undefined;
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
    // No live agent session / detected agent on this terminal → it is in TERMINAL mode, so the
    // agent activity is IGNORED (never fabricate an agent state for a plain PTY). Its exec_state
    // (running) carries.
    const plain = terminal({ id: "t-plain", exec_state: "running", sessions: [] });
    const lookup = () => activity({ activity: "waiting" });
    expect(workspaceRunState(snapshot([plain], []), lookup)).toBe("running");
  });

  it("an agent that is WORKING contributes running (from its activity, NOT the busy bit)", () => {
    // A live agent whose activity is `working` contributes `running` — the SAME dot its line
    // shows — even when its own exec_state is idle (the running comes from the agent turn, not a
    // busy/exec_state fallback). waiting is reserved for blocked-on-user.
    const t = terminal({
      id: "t-work",
      exec_state: "idle",
      sessions: [session("a1", "active")],
    });
    const lookup = () => activity({ activity: "working" });
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

// ── AGENT-AWARE rollup (the fix: the rollup agregates the SAME agent-aware projection each LINE
// shows — `terminalLineState`, selected:false — not the OS busy bit / raw exec_state. A single
// terminal-agent that has REPLIED (ready/green) must roll up GREEN, not running/blue). ──
describe("workspaceRunState — agent-aware rollup (rollup agrees with the line)", () => {
  // A busy foreground agent terminal (claude in the foreground → OS `busy` ON) with a live session.
  const busyAgent = (overrides: Partial<WorkspaceRuntimeTerminal> = {}) =>
    terminal({ id: "t-agent", busy: true, sessions: [session("a1", "active")], ...overrides });
  const agentActivity = (act: AgentActivity | undefined) => (id: string) =>
    id === "t-agent" ? act : undefined;

  it("(a) a SINGLE terminal-agent that REPLIED (ready, busy) → success, NOT running", () => {
    // The user's exact bug: one terminal, agent responded (line dot green). readyUnread primes the
    // rollup to `success`; the busy bit is IGNORED. Only success+idle present → success wins.
    const t = busyAgent();
    const lookup = agentActivity(activity({ activity: "idle", readyUnread: true }));
    expect(workspaceRunState(snapshot([t], []), lookup)).toBe("success");
  });

  it("(b) a terminal-agent IDLE at the prompt (busy, no activity) → idle, NOT running", () => {
    // A busy foreground agent with NO activity entry contributes idle (busy ignored), exactly as
    // on its line — so a single idle agent rolls the whole workspace up to idle (no dot).
    const t = busyAgent();
    expect(workspaceRunState(snapshot([t], []), agentActivity(undefined))).toBe("idle");
  });

  it("(c) a terminal-agent WORKING → running", () => {
    const t = busyAgent();
    const lookup = agentActivity(activity({ activity: "working" }));
    expect(workspaceRunState(snapshot([t], []), lookup)).toBe("running");
  });

  it("(d) a terminal-agent WAITING (blocked on user) → waiting", () => {
    const t = busyAgent();
    const lookup = agentActivity(activity({ activity: "waiting" }));
    expect(workspaceRunState(snapshot([t], []), lookup)).toBe("waiting");
  });

  it("(e) a terminal-agent that ERRORED (errorUnread) → error", () => {
    const t = busyAgent();
    const lookup = agentActivity(activity({ activity: "idle", errorUnread: true }));
    expect(workspaceRunState(snapshot([t], []), lookup)).toBe("error");
  });

  it("(f) MIX: agent READY + a NON-agent terminal BUSY → success (success > running)", () => {
    // The agent's green outranks a plain busy terminal's blue via the shared STATE_PRIORITY.
    const agent = busyAgent();
    const plainBusy = terminal({ id: "t-plain", busy: true, sessions: [] });
    const lookup = agentActivity(activity({ activity: "idle", readyUnread: true }));
    expect(workspaceRunState(snapshot([agent, plainBusy], []), lookup)).toBe("success");
  });

  it("(g) MIX: agent WAITING + another agent ERROR → waiting (waiting > error)", () => {
    const waiter = terminal({ id: "t-agent", busy: true, sessions: [session("a1", "active")] });
    const errer = terminal({ id: "t-err", busy: true, sessions: [session("a2", "active")] });
    const lookup = (id: string) =>
      id === "t-agent"
        ? activity({ activity: "waiting" })
        : id === "t-err"
          ? activity({ activity: "idle", errorUnread: true })
          : undefined;
    expect(workspaceRunState(snapshot([waiter, errer], []), lookup)).toBe("waiting");
  });

  it("(h) NON-REGRESSION: a NON-agent terminal BUSY (alone) → running", () => {
    // A plain PTY with a foreground command still lights the busy `running` — only an agent line
    // ignores busy. The activity lookup is irrelevant (no agent to own it).
    const plain = terminal({ id: "t-plain", busy: true, sessions: [] });
    expect(workspaceRunState(snapshot([plain], []), agentActivity(undefined))).toBe("running");
  });

  it("(i) a REPLIED agent whose ready has been READ (readyUnread false) → no longer contributes → idle", () => {
    // Once the agent's ready notification is read (the source clears `readyUnread`, e.g. on focus
    // via `agent_mark_ready_read`), the agent contributes idle — exactly as its line dot clears.
    const t = busyAgent();
    const lookup = agentActivity(activity({ activity: "idle", readyUnread: false }));
    expect(workspaceRunState(snapshot([t], []), lookup)).toBe("idle");
  });

  it("projectRunState mirrors workspaceRunState for the agent-aware rollup (single workspace)", () => {
    // The project dot agrees with the workspace dot by construction — a single-workspace project
    // hosting a replied agent rolls up green on the rail too, and a working agent → running.
    const ready = busyAgent();
    const readyLookup = agentActivity(activity({ activity: "idle", readyUnread: true }));
    const readyWs = snapshot([ready], []);
    expect(projectRunState([readyWs], readyLookup)).toBe(workspaceRunState(readyWs, readyLookup));
    expect(projectRunState([readyWs], readyLookup)).toBe("success");
    const workingLookup = agentActivity(activity({ activity: "working" }));
    expect(projectRunState([snapshot([busyAgent()], [])], workingLookup)).toBe("running");
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
    const lookup = (id: string) =>
      id === "t-wait" ? activity({ activity: "waiting" }) : undefined;
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
