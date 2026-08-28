import { renderHook, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { bridgeFake } from "@/bridge/test-harness";
import { describe, expect, it, vi } from "vitest";

import { useLazyResume } from "./use-lazy-resume";

/** Build a `workspace_restore_plan` shape with the given resume_sessions. */
function plan(workspaceId: string, resumeSessions: unknown[]) {
  return {
    workspace_id: workspaceId,
    resume_sessions: resumeSessions,
    restart_on_startup: [],
  };
}

describe("use-lazy-resume (renderer half of the lazy resume)", () => {
  it("consumes workspace_restore_plan and pushes ONLY the resumable sessions as parks", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () =>
      plan("ws-1", [
        // resumable → pushed as a park (terminalId/sessionId/command).
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: "claude --resume ext-1",
          resume: true,
          uncertain: false,
          skip_reason: null,
        },
        // toggle OFF / non-candidate → NOT pushed (no injection).
        {
          terminal_id: "t-2",
          session_id: "s-2",
          agent_kind: "claude_code",
          resume_command: null,
          resume: false,
          uncertain: false,
          skip_reason: "option_off",
        },
      ]),
    );
    const parkArgs = vi.fn();
    fake.on("set_resume_parks", (args) => {
      parkArgs(args);
      return null;
    });

    renderHook(() => useLazyResume(["ws-1"]));

    await waitFor(() => expect(parkArgs).toHaveBeenCalledTimes(1));
    expect(parkArgs).toHaveBeenCalledWith({
      parks: [{ terminalId: "t-1", sessionId: "s-1", command: "claude --resume ext-1" }],
    });
  });

  it("auto-parks EVERY resumable session, including an `uncertain` one (crash-UI reverted)", async () => {
    // v2.5.8 revert: there is no longer a confident/doubtful partition. A crash is a clean
    // shutdown (no `unknown` sessions), and `uncertain` is informational only — lazy resumes
    // EVERY `resume: true` session silently (gated by the toggle + the #28 inject gate). So an
    // `uncertain: true` resumable session is now ALSO parked, not held back for a (removed) panel.
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () =>
      plan("ws-1", [
        // A plain (active) resumable session.
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: "claude --resume ext-1",
          resume: true,
          uncertain: false,
          skip_reason: null,
        },
        // An `uncertain` resumable session → now ALSO auto-parked (no partition anymore).
        {
          terminal_id: "t-2",
          session_id: "s-2",
          agent_kind: "claude_code",
          resume_command: "claude --resume ext-2",
          resume: true,
          uncertain: true,
          skip_reason: null,
        },
      ]),
    );
    const parkArgs = vi.fn();
    fake.on("set_resume_parks", (args) => {
      parkArgs(args);
      return null;
    });

    renderHook(() => useLazyResume(["ws-1"]));

    await waitFor(() => expect(parkArgs).toHaveBeenCalledTimes(1));
    // BOTH resumable sessions are parked — the `uncertain` one is no longer excluded.
    expect(parkArgs).toHaveBeenCalledWith({
      parks: [
        { terminalId: "t-1", sessionId: "s-1", command: "claude --resume ext-1" },
        { terminalId: "t-2", sessionId: "s-2", command: "claude --resume ext-2" },
      ],
    });
  });

  it("still injects under React StrictMode (dev double-invoke) — pushes the parks EXACTLY ONCE", async () => {
    // Regression guard: <React.StrictMode> mounts→unmounts→remounts the hook in dev. A prior
    // version marked pushedRef before the await AND cancelled the resolved push on the StrictMode
    // cleanup, so run1's invoke bailed (`if (cancelled) return`) while run2 skipped the already-
    // marked workspace → set_resume_parks was NEVER called (empty resume in `tauri dev`; prod
    // StrictMode is inert, hiding the bug). The wrapper reproduces the double-invoke; the hook must
    // still push the resumable parks exactly once — no missed injection, and no double injection.
    const fake = bridgeFake();
    const planReads = vi.fn();
    fake.on("workspace_restore_plan", (args) => {
      planReads(args);
      return plan("ws-1", [
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: "claude --resume ext-1",
          resume: true,
          uncertain: false,
          skip_reason: null,
        },
      ]);
    });
    const parkArgs = vi.fn();
    fake.on("set_resume_parks", (args) => {
      parkArgs(args);
      return null;
    });

    renderHook(() => useLazyResume(["ws-1"]), { wrapper: StrictMode });

    await waitFor(() => expect(parkArgs).toHaveBeenCalledTimes(1));
    expect(parkArgs).toHaveBeenCalledWith({
      parks: [{ terminalId: "t-1", sessionId: "s-1", command: "claude --resume ext-1" }],
    });
    // The ref guard survives the StrictMode remount, so the plan is read once and the parks are
    // pushed once — never twice.
    expect(planReads).toHaveBeenCalledTimes(1);
    expect(parkArgs).toHaveBeenCalledTimes(1);
  });

  it("pushes nothing when no session is resumable (toggle OFF everywhere)", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () =>
      plan("ws-1", [
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: null,
          resume: false,
          uncertain: false,
          skip_reason: "option_off",
        },
      ]),
    );
    const setParks = vi.fn(() => null);
    fake.on("set_resume_parks", setParks);

    renderHook(() => useLazyResume(["ws-1"]));

    // Let the plan read resolve, then assert NO park push happened.
    await waitFor(() =>
      expect(fake.calls.some((c) => c.command === "workspace_restore_plan")).toBe(true),
    );
    await Promise.resolve();
    expect(setParks).not.toHaveBeenCalled();
  });

  it("reads + pushes each workspace AT MOST ONCE per app run (idempotent driver)", async () => {
    const fake = bridgeFake();
    const planReads = vi.fn();
    fake.on("workspace_restore_plan", (args) => {
      planReads(args);
      return plan("ws-1", [
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: "claude --resume ext-1",
          resume: true,
          uncertain: false,
          skip_reason: null,
        },
      ]);
    });
    fake.on("set_resume_parks", () => null);

    const { rerender } = renderHook(({ ids }: { ids: string[] }) => useLazyResume(ids), {
      initialProps: { ids: ["ws-1"] },
    });
    await waitFor(() => expect(planReads).toHaveBeenCalledTimes(1));

    // A re-render with the SAME workspace must NOT re-read/re-push (the ref guard holds).
    rerender({ ids: ["ws-1"] });
    await Promise.resolve();
    expect(planReads).toHaveBeenCalledTimes(1);
  });

  it("never throws when the restore-plan read fails (best-effort)", async () => {
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () => {
      throw new Error("sidecar not ready");
    });
    const setParks = vi.fn(() => null);
    fake.on("set_resume_parks", setParks);

    // The hook swallows the rejection — the page never sees an error.
    expect(() => renderHook(() => useLazyResume(["ws-1"]))).not.toThrow();
    await waitFor(() =>
      expect(fake.calls.some((c) => c.command === "workspace_restore_plan")).toBe(true),
    );
    expect(setParks).not.toHaveBeenCalled();
  });

  // --- A tentative is CONSUMED only when it went through --------------------------------
  //
  // The hook claims a workspace BEFORE the await (that is what keeps React StrictMode's second
  // run from launching a second invoke) and RELEASES the claim when the attempt did not go
  // through. These tests pin both halves against the ONE hook instance — a fresh `renderHook`
  // would build a fresh ref set and retry even WITHOUT the release, so it would prove nothing.
  // Re-running the effect with a new `workspaceIds` identity is what discriminates.

  /** A RESUMABLE plan entry (`resume: true` + a command) — the shape the hook turns into a park. */
  function resumableEntry(terminal: string, session: string, command: string) {
    return {
      terminal_id: terminal,
      session_id: session,
      agent_kind: "claude_code",
      resume_command: command,
      resume: true,
      uncertain: false,
      skip_reason: null,
    };
  }

  it("re-attempts a workspace whose park PUSH failed, on the next run of the effect", async () => {
    const fake = bridgeFake();
    const planReads = vi.fn();
    fake.on("workspace_restore_plan", (args) => {
      planReads(args);
      return plan("ws-1", [resumableEntry("t-1", "s-1", "claude --resume ext-1")]);
    });
    const pushes = vi.fn();
    fake.on("set_resume_parks", (args) => {
      pushes(args);
      // Only the FIRST push fails — a transient sidecar hiccup, exactly the case that used to
      // consume the workspace for the whole run and kill every later resume.
      if (pushes.mock.calls.length === 1) throw new Error("sidecar refused the batch");
      return null;
    });

    const { rerender } = renderHook(({ ids }: { ids: string[] }) => useLazyResume(ids), {
      initialProps: { ids: ["ws-1"] },
    });
    await waitFor(() => expect(pushes).toHaveBeenCalledTimes(1));

    // A NEW array identity re-runs the effect. The failed attempt must have released `ws-1`, so a
    // SECOND read + push go out; if the failure had been swallowed the counts would stay at 1.
    rerender({ ids: ["ws-1"] });
    await waitFor(() => expect(pushes).toHaveBeenCalledTimes(2));
    expect(planReads).toHaveBeenCalledTimes(2);
    expect(pushes).toHaveBeenLastCalledWith({
      parks: [{ terminalId: "t-1", sessionId: "s-1", command: "claude --resume ext-1" }],
    });
  });

  it("re-attempts a workspace whose restore-plan READ failed, on the next run of the effect", async () => {
    const fake = bridgeFake();
    const planReads = vi.fn();
    fake.on("workspace_restore_plan", (args) => {
      planReads(args);
      if (planReads.mock.calls.length === 1) throw new Error("sidecar not ready");
      return plan("ws-1", [resumableEntry("t-1", "s-1", "claude --resume ext-1")]);
    });
    const pushes = vi.fn(() => null);
    fake.on("set_resume_parks", pushes);

    const { rerender } = renderHook(({ ids }: { ids: string[] }) => useLazyResume(ids), {
      initialProps: { ids: ["ws-1"] },
    });
    await waitFor(() => expect(planReads).toHaveBeenCalledTimes(1));
    expect(pushes).not.toHaveBeenCalled();

    // The read failed, so `ws-1` was released: the next effect run reads it again AND — this time
    // succeeding — parks the resume the first attempt never got to.
    rerender({ ids: ["ws-1"] });
    await waitFor(() => expect(pushes).toHaveBeenCalledTimes(1));
    expect(planReads).toHaveBeenCalledTimes(2);
    expect(pushes).toHaveBeenCalledWith({
      parks: [{ terminalId: "t-1", sessionId: "s-1", command: "claude --resume ext-1" }],
    });
  });

  it("never retries a failed attempt on its own — no timer re-arms it", async () => {
    // Releasing the claim makes a workspace RE-TRYABLE, not RETRIED. The next attempt rides the
    // next run of the effect and nothing else: no backoff, no interval, no recursive re-attempt.
    const fake = bridgeFake();
    const planReads = vi.fn();
    fake.on("workspace_restore_plan", (args) => {
      planReads(args);
      throw new Error("sidecar not ready");
    });
    fake.on("set_resume_parks", () => null);

    // Fake timers go in BEFORE the render, or a timer the hook arms would be a REAL one this test
    // never gets to see fire (advancing a fake clock installed afterwards proves nothing).
    // `shouldAdvanceTime` keeps `waitFor`'s own polling alive underneath them.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      renderHook(() => useLazyResume(["ws-1"]));
      await waitFor(() => expect(planReads).toHaveBeenCalledTimes(1));
      // Ten virtual minutes past the failure, flushing microtasks along the way: a timer-driven OR
      // microtask-driven self-retry would have fired inside that window.
      await vi.advanceTimersByTimeAsync(10 * 60 * 1000);
      expect(planReads).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("never re-pushes a workspace whose push SUCCEEDED, even when the effect runs again", async () => {
    // The mirror property of the two tests above: releasing a FAILED claim must not weaken the
    // idempotence of a SUCCESSFUL one. The second effect run adds `ws-2`, whose own read is the
    // observable event we wait on — so the assertion on `ws-1` is made AFTER the effect has
    // demonstrably re-run and settled, not after an arbitrary flush.
    const fake = bridgeFake();
    const reads: unknown[] = [];
    fake.on("workspace_restore_plan", (args) => {
      const id = (args as { workspaceId: string }).workspaceId;
      reads.push(id);
      return plan(id, [resumableEntry(`t-${id}`, `s-${id}`, `claude --resume ext-${id}`)]);
    });
    const pushes: Array<{ parks: Array<{ terminalId: string }> }> = [];
    fake.on("set_resume_parks", (args) => {
      pushes.push(args as { parks: Array<{ terminalId: string }> });
      return null;
    });

    const { rerender } = renderHook(({ ids }: { ids: string[] }) => useLazyResume(ids), {
      initialProps: { ids: ["ws-1"] },
    });
    await waitFor(() => expect(pushes).toHaveLength(1));

    rerender({ ids: ["ws-1", "ws-2"] });
    await waitFor(() => expect(reads.filter((id) => id === "ws-2")).toHaveLength(1));
    await waitFor(() =>
      expect(pushes.filter((p) => p.parks[0]?.terminalId === "t-ws-2")).toHaveLength(1),
    );
    // `ws-1` went through the first time, so it stays consumed: read once, pushed once.
    expect(reads.filter((id) => id === "ws-1")).toHaveLength(1);
    expect(pushes.filter((p) => p.parks[0]?.terminalId === "t-ws-1")).toHaveLength(1);
  });

  // --- The traces: a failure that says which failure, a verdict per candidate ------------

  it("traces a failed plan READ and a failed park PUSH as two DISTINCT lines", async () => {
    // Two pannes, two messages. Before this, one empty `catch {}` covered both, so a plan that
    // never came back and a batch that was not accepted left the exact same trace: none.
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const fake = bridgeFake();
    const planReads = vi.fn();
    fake.on("workspace_restore_plan", (args) => {
      planReads(args);
      // Attempt 1: the READ fails. Attempt 2: the read succeeds and the PUSH fails.
      if (planReads.mock.calls.length === 1) throw new Error("sidecar not ready");
      return plan("ws-1", [resumableEntry("t-1", "s-1", "claude --resume ext-1")]);
    });
    const pushes = vi.fn(() => {
      throw new Error("sidecar refused the batch");
    });
    fake.on("set_resume_parks", pushes);

    const { rerender } = renderHook(({ ids }: { ids: string[] }) => useLazyResume(ids), {
      initialProps: { ids: ["ws-1"] },
    });
    await waitFor(() => expect(warn).toHaveBeenCalledTimes(1));
    rerender({ ids: ["ws-1"] });
    await waitFor(() => expect(warn).toHaveBeenCalledTimes(2));

    const lines = warn.mock.calls.map((c) => String(c[0]));
    const readLine = lines[0] as string;
    const pushLine = lines[1] as string;
    // Each line names WHICH half failed, and the workspace it concerns.
    expect(readLine).toContain("reading the restore plan FAILED");
    expect(readLine).toContain("workspace ws-1");
    expect(readLine).toContain("sidecar not ready");
    expect(pushLine).toContain("pushing 1 resume park(s) FAILED");
    expect(pushLine).toContain("workspace ws-1");
    expect(pushLine).toContain("sidecar refused the batch");
    // Distinct, not merely different by their error text: neither line carries the other's phase.
    expect(readLine).not.toContain("resume park(s) FAILED");
    expect(pushLine).not.toContain("reading the restore plan FAILED");
  });

  it("publishes ONE verdict per candidate session, naming its terminal and session", async () => {
    // `decide_resume` builds a `ResumeSkipReason` on every non-resume path "so the caller can
    // log/observe", and `skip_reason_tag` gives it a stable label that rides in
    // `resume_sessions[].skip_reason`. This hook WAS the caller that threw it away.
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    // The tags of `agent_resume::skip_reason_tag` — the ONLY reason vocabulary there is. That there
    // are exactly seven of them, all distinct, is pinned on the Rust side by
    // `agent_resume::tests::skip_reason_tag_is_stable_and_distinct_for_every_variant`; what THIS
    // test holds is that each one reaches the log intact and on a line of its own.
    const tags = [
      "option_off",
      "closed_voluntarily",
      "session_ended",
      "already_resume_failed",
      "no_conversation",
      "unsupported_target",
      "no_resume_command",
    ];
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () =>
      plan("ws-1", [
        resumableEntry("t-live", "s-live", "claude --resume ext-live"),
        ...tags.map((tag, i) => ({
          terminal_id: `t-${i}`,
          session_id: `s-${i}`,
          agent_kind: "claude_code",
          resume_command: null,
          resume: false,
          uncertain: false,
          skip_reason: tag,
        })),
      ]),
    );
    fake.on("set_resume_parks", () => null);

    renderHook(() => useLazyResume(["ws-1"]));
    await waitFor(() =>
      expect(fake.calls.some((c) => c.command === "set_resume_parks")).toBe(true),
    );

    const lines = info.mock.calls.map((c) => String(c[0]));
    // The resumed session says so, and is tied to its terminal + session.
    const resumed = lines.filter((l) => l.includes("terminal t-live") && l.includes("RESUME."));
    expect(resumed).toHaveLength(1);
    expect(resumed[0]).toContain("session s-live");
    expect(resumed[0]).toContain("workspace ws-1");
    // Each skip produces its OWN line, carrying its own tag verbatim and naming the terminal +
    // session it concerns — so no two of them read alike.
    const skipLines = tags.map((tag, i) => {
      const match = lines.filter((l) => l.includes(`terminal t-${i}`));
      expect(match, `exactly one verdict line for the ${tag} candidate`).toHaveLength(1);
      expect(match[0]).toContain(`session s-${i}`);
      expect(match[0]).toContain(`SKIP — ${tag}.`);
      return match[0] as string;
    });
    expect(new Set(skipLines).size, "no two skip verdicts read alike").toBe(tags.length);
  });

  it("says NOTHING TO RESUME on its own line, and reports no failure", async () => {
    // "Rien à reprendre" is a complete, legitimate outcome — not a panne. It used to leave through
    // a bare `return`, which made an empty plan look exactly like a broken one.
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const fake = bridgeFake();
    fake.on("workspace_restore_plan", () =>
      plan("ws-1", [
        {
          terminal_id: "t-1",
          session_id: "s-1",
          agent_kind: "claude_code",
          resume_command: null,
          resume: false,
          uncertain: false,
          skip_reason: "option_off",
        },
      ]),
    );
    const setParks = vi.fn(() => null);
    fake.on("set_resume_parks", setParks);

    renderHook(() => useLazyResume(["ws-1"]));
    await waitFor(() =>
      expect(info.mock.calls.some((c) => String(c[0]).includes("NOTHING TO RESUME"))).toBe(true),
    );

    const nothing = info.mock.calls
      .map((c) => String(c[0]))
      .filter((l) => l.includes("NOTHING TO RESUME"));
    expect(nothing).toHaveLength(1);
    expect(nothing[0]).toContain("workspace ws-1");
    expect(nothing[0]).toContain("1 candidate session(s) in the plan, none resumable");
    // Nothing was parked, and nothing was reported as a failure — the read went through.
    expect(setParks).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
  });
});

/**
 * THE CALL SITE IS THE PROPERTY. `terminals-page.tsx` states, next to its `useLazyResume(...)`, that
 * THIS IS THE ONLY ONE and that the fact is load-bearing: the whole lazy resume is driven from that
 * mount and from nowhere else, so a boot log showing no resume is explained by that page not being
 * mounted rather than by a defect. The claim was true and held by nothing — a second call site
 * (another page, a layout, a boot effect) would race this one and double the injections, and the
 * sentence would silently become false.
 *
 * So it is counted, over the renderer sources themselves. Tests are excluded — they call the hook
 * many times on purpose — and so is the module that DEFINES it.
 */
describe("the lazy resume's single production call site", () => {
  it("is called from exactly one place in the renderer, and it is the terminals page", async () => {
    const { readdirSync, readFileSync } = await import("node:fs");
    const { join, relative, resolve, sep } = await import("node:path");

    const SRC = resolve(__dirname, "..", "..");
    const sources: string[] = [];
    const walk = (dir: string) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const full = join(dir, entry.name);
        if (entry.isDirectory()) {
          walk(full);
        } else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)) {
          sources.push(full);
        }
      }
    };
    walk(SRC);
    expect(
      sources.length,
      "no renderer source was scanned — the walk found nothing",
    ).toBeGreaterThan(10);

    const callers = sources
      .filter((file) => !file.endsWith(`${sep}use-lazy-resume.ts`))
      .filter((file) => /\buseLazyResume\s*\(/.test(readFileSync(file, "utf8")))
      .map((file) => relative(SRC, file).split(sep).join("/"));

    expect(
      callers,
      "the lazy resume is no longer driven from exactly one place. `terminals-page.tsx` says it is, " +
        "and the whole explanation of a boot that resumes nothing rests on it: a second caller races " +
        "this one and injects twice. Either remove it, or rewrite that comment and this test together",
    ).toEqual(["components/terminals/terminals-page.tsx"]);
  });
});
