import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { nyxBridge } from "@/bridge";
import type { ResumePark, WorkspaceRestorePlan } from "@/bridge";

/**
 * `use-restore-plan` — the RENDERER read of the post-crash RESTORE state (v2.5.5 phase 3), the
 * counterpart of the LAZY auto-resume (`use-lazy-resume`). Where `use-lazy-resume` silently drains
 * the CONFIDENT resumable sessions at terminal open, THIS provider surfaces the DOUBT for the user
 * to act on:
 *
 *   - the crash-reconcile `interrupted` command instances (`workspace_restore_plan.interrupted`),
 *     so a service row can badge itself `interrupted` (its backing process died with the old
 *     sidecar; NOT auto-relaunched — the user chooses);
 *   - the DOUBTFUL resumable agent sessions (`resume_sessions` with a `resume_command` AND
 *     `uncertain === true` — a post-crash `unknown`/stale candidate), so the restore UI
 *     (`<WorkspaceRestorePanel>`) can offer each one a RESUME (re-inject its `claude --resume` via
 *     the phase-2 `set_resume_parks` mechanism) or an IGNORE (dismiss — do not resume).
 *
 * ## The PARTITION with `use-lazy-resume`
 * The two consumers split `resume_sessions` on the `uncertain` flag and NEVER overlap: lazy
 * auto-drains the CONFIDENT sessions (`!uncertain`); this panel owns exactly the DOUBTFUL ones
 * (`uncertain`). Because lazy never auto-parks a doubtful session, an IGNORE here is truly
 * BLOCKING — no park is ever set for that session, so nothing is injected at its PTY spawn and it
 * is not resumed for this run. (The panel is only shown at all when the project resume toggle is
 * ON: a toggle-OFF workspace yields no resumable entry, so no resume control — the badges still
 * show, but reprise stays off, by design.)
 *
 * It REUSES the same `workspace_restore_plan` read model — no new backend, no third resume policy.
 * The plan is read AT MOST ONCE per workspace per app run (a ref guard, like `use-lazy-resume`),
 * because the crash `interrupted`/`unknown` set is a boot-time fact; a resumed/ignored session is
 * then dismissed LOCALLY so the panel updates without a re-read. A failed read simply yields no
 * restore surface for that workspace (best-effort; never throws to the page).
 *
 * `resume` is idempotent by construction: the sidecar's `set_resume_parks` one-shot guard makes a
 * re-park a no-op — and since the partition hands each session to a SINGLE consumer, no session is
 * ever pushed through both the panel's RESUME and lazy's auto-drain.
 */

/** One resumable agent session offered by the restore UI (a projected `WorkspaceResumeEntry`). */
export interface RestorableSession {
  readonly workspaceId: string;
  readonly terminalId: string;
  readonly sessionId: string;
  readonly agentKind: string;
  /** The exact `claude --resume <id>` line to re-inject on RESUME (from the restore plan). */
  readonly command: string;
}

interface RestorePlanContextValue {
  /** Whether a command instance id is in the crash-reconcile `interrupted` set (badge it). */
  readonly isInterrupted: (instanceId: string) => boolean;
  /** The resumable-and-not-yet-actioned agent sessions of a workspace (the restore UI list). */
  readonly restorablesForWorkspace: (workspaceId: string) => readonly RestorableSession[];
  /** RESUME a session: re-inject its `claude --resume` via `set_resume_parks`, then dismiss it. */
  readonly resume: (session: RestorableSession) => void;
  /** IGNORE a session: dismiss it from the restore UI WITHOUT resuming. */
  readonly ignore: (workspaceId: string, sessionId: string) => void;
}

const RestorePlanContext = createContext<RestorePlanContextValue | null>(null);

/**
 * Project the DOUBTFUL resumable `resume_sessions` entries of a plan to `RestorableSession`s. The
 * panel owns exactly the post-crash doubtful sessions — `resume === true` (a `resume_command`,
 * mirroring lazy's `resume` gate — no latent asymmetry) AND `uncertain === true` (an `unknown`/
 * stale candidate). The CONFIDENT (`!uncertain`) resumable sessions are auto-drained by
 * `use-lazy-resume` and must NOT appear here, so the two consumers never overlap.
 */
function toRestorables(plan: WorkspaceRestorePlan): RestorableSession[] {
  return (plan.resume_sessions ?? [])
    .filter((entry) => entry.resume && entry.resume_command != null && entry.uncertain)
    .map((entry) => ({
      workspaceId: plan.workspace_id,
      terminalId: entry.terminal_id,
      sessionId: entry.session_id,
      agentKind: entry.agent_kind,
      command: entry.resume_command as string,
    }));
}

/**
 * Whether a value read from the bridge is a USABLE restore plan. A shell (or a test fake) can
 * resolve `undefined`/`null`/a malformed value for `workspace_restore_plan`; storing that would
 * blow up the render (reading `.interrupted` off `undefined`), so a non-object is treated as
 * "no plan for this workspace" and never stored.
 */
function isRestorePlan(value: unknown): value is WorkspaceRestorePlan {
  return typeof value === "object" && value !== null;
}

export function RestorePlanProvider({
  workspaceIds,
  children,
}: {
  workspaceIds: readonly string[];
  children: ReactNode;
}) {
  // The fetched plan per workspace (id → plan). Absent until its read resolves.
  const [plans, setPlans] = useState<ReadonlyMap<string, WorkspaceRestorePlan>>(() => new Map());
  // Session ids the user has ACTIONED (resumed or ignored) — hidden from the restore UI. A ref
  // MIRROR keeps the callbacks stable without stale closures over the state.
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(() => new Set());

  // Per-app-run guard: each workspace's plan is read AT MOST ONCE (a ref survives StrictMode's
  // dev remount, like `use-lazy-resume`), because the crash-interrupted set is a boot-time fact.
  const readRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    for (const workspaceId of workspaceIds) {
      if (readRef.current.has(workspaceId)) continue;
      readRef.current.add(workspaceId);
      void (async () => {
        try {
          const plan = await nyxBridge.invoke<unknown>("workspace_restore_plan", {
            workspaceId,
          });
          if (!isRestorePlan(plan)) return;
          setPlans((prev) => {
            const next = new Map(prev);
            next.set(workspaceId, plan);
            return next;
          });
        } catch {
          // Best-effort: a failed restore-plan read means no restore surface for this workspace
          // this run (a relaunch re-scans). Never surface an error to the page.
        }
      })();
    }
  }, [workspaceIds]);

  const dismiss = useCallback((sessionId: string) => {
    setDismissed((prev) => {
      if (prev.has(sessionId)) return prev;
      const next = new Set(prev);
      next.add(sessionId);
      return next;
    });
  }, []);

  const resume = useCallback(
    (session: RestorableSession) => {
      // Re-inject via the phase-2 park mechanism (idempotent one-shot on the sidecar) and dismiss
      // the entry optimistically. Best-effort: a failed push never throws to the page.
      const parks: ResumePark[] = [
        { terminalId: session.terminalId, sessionId: session.sessionId, command: session.command },
      ];
      void nyxBridge.invoke("set_resume_parks", { parks }).catch(() => {});
      dismiss(session.sessionId);
    },
    [dismiss],
  );

  const ignore = useCallback(
    (_workspaceId: string, sessionId: string) => {
      dismiss(sessionId);
    },
    [dismiss],
  );

  const value = useMemo<RestorePlanContextValue>(() => {
    const interrupted = new Set<string>();
    for (const plan of plans.values()) {
      for (const id of plan.interrupted ?? []) interrupted.add(id);
    }
    return {
      isInterrupted: (instanceId) => interrupted.has(instanceId),
      restorablesForWorkspace: (workspaceId) => {
        const plan = plans.get(workspaceId);
        if (!plan) return [];
        return toRestorables(plan).filter((s) => !dismissed.has(s.sessionId));
      },
      resume,
      ignore,
    };
  }, [plans, dismissed, resume, ignore]);

  return <RestorePlanContext.Provider value={value}>{children}</RestorePlanContext.Provider>;
}

/**
 * Read the restore-plan registry. Returns an INERT fallback when no provider is mounted (nothing
 * interrupted, no restorables, actions no-op) so a row/panel can render in isolation (and in
 * tests) without wrapping it; the Terminals page mounts the real provider.
 */
export function useRestorePlan(): RestorePlanContextValue {
  return useContext(RestorePlanContext) ?? NOOP;
}

const NOOP: RestorePlanContextValue = {
  isInterrupted: () => false,
  restorablesForWorkspace: () => [],
  resume: () => {},
  ignore: () => {},
};
