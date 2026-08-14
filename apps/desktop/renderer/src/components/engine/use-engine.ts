import { useCallback, useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";
import type {
  EngineEffectorContext,
  EngineWorkflow,
  EngineWorkflowGraph,
  EngineWorkflowRun,
} from "@/bridge";

/**
 * The engine graph READ hooks (v2.8 Phase 6). They wrap the four `engine_*` bridge read commands —
 * the SAME `nyx_core::db` engine accessors + `render_agent_context` the MCP tools serve (one
 * authority for UI + MCP). The graph is READ-ONLY (authoring stays MCP-first), but it is LIVE: the
 * graph + run hooks re-pull on the coarse invalidations ({@link useEngineInvalidation}) rather than
 * staying frozen on their mount-time snapshot.
 */

/**
 * Re-run `onChanged` on the coarse invalidations that move the engine view — the "sans polling" path:
 *   - `workflows://changed` — an agent authoring mutation (`workflow_*` / `node_*`) re-built the tree;
 *   - `specs://changed` — a `nyx_next` tick advanced a run (cursor + blackboard).
 * StrictMode-safe teardown (an unlisten that resolves after unmount is dropped immediately).
 */
function useEngineInvalidation(enabled: boolean, onChanged: () => void) {
  useEffect(() => {
    if (!enabled) return;
    let torndown = false;
    const unlistens: Array<() => void> = [];
    const handler = () => {
      if (!torndown) onChanged();
    };
    for (const topic of ["workflows://changed", "specs://changed"] as const) {
      void nyxBridge.subscribe(topic, handler).then((un) => {
        if (torndown) {
          void Promise.resolve(un()).catch(() => {});
          return;
        }
        unlistens.push(un);
      });
    }
    return () => {
      torndown = true;
      for (const un of unlistens) void Promise.resolve(un()).catch(() => {});
    };
  }, [enabled, onChanged]);
}

/** The workflows the picker offers: the built-in defaults + (when a project is entered) its own. */
export function useEngineWorkflows(projectId: string | null): {
  workflows: readonly EngineWorkflow[];
  loading: boolean;
  error: boolean;
} {
  const [workflows, setWorkflows] = useState<readonly EngineWorkflow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(false);
    nyxBridge
      .invoke<EngineWorkflow[]>("engine_workflow_list", projectId ? { projectId } : {})
      .then((rows) => {
        if (!cancelled) setWorkflows(rows ?? []);
      })
      .catch(() => {
        if (!cancelled) setError(true);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [projectId]);

  return { workflows, loading, error };
}

/** One workflow + its nodes (for tree reconstruction). `null` workflowId disables the hook. */
export function useEngineGraph(workflowId: string | null): {
  graph: EngineWorkflowGraph | null;
  loading: boolean;
  error: boolean;
} {
  const [graph, setGraph] = useState<EngineWorkflowGraph | null>(null);
  const [loading, setLoading] = useState(workflowId !== null);
  const [error, setError] = useState(false);

  const load = useCallback(() => {
    if (workflowId === null) {
      setGraph(null);
      setLoading(false);
      setError(false);
      return () => {};
    }
    let cancelled = false;
    setLoading(true);
    setError(false);
    nyxBridge
      .invoke<EngineWorkflowGraph>("engine_workflow_graph", { workflowId })
      .then((g) => {
        if (!cancelled) setGraph(g ?? null);
      })
      .catch(() => {
        if (!cancelled) setError(true);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [workflowId]);

  useEffect(() => load(), [load]);
  // Live: an agent authoring mutation re-builds the tree → re-pull it.
  useEngineInvalidation(workflowId !== null, load);

  return { graph, loading, error };
}

/** A workflow's runs, newest-first, plus the LATEST (the runtime view reads it). Re-pulled live on
 *  the coarse invalidations, so the runtime overlay tracks a running `nyx_next` loop. */
export function useEngineRuns(workflowId: string | null): {
  runs: readonly EngineWorkflowRun[];
  latest: EngineWorkflowRun | null;
  loading: boolean;
  refresh: () => void;
} {
  const [runs, setRuns] = useState<readonly EngineWorkflowRun[]>([]);
  const [loading, setLoading] = useState(workflowId !== null);

  const load = useCallback(() => {
    if (workflowId === null) {
      setRuns([]);
      setLoading(false);
      return () => {};
    }
    let cancelled = false;
    setLoading(true);
    nyxBridge
      .invoke<EngineWorkflowRun[]>("engine_run_list", { workflowId })
      .then((rows) => {
        if (!cancelled) setRuns(rows ?? []);
      })
      .catch(() => {
        if (!cancelled) setRuns([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [workflowId]);

  useEffect(() => load(), [load]);
  // Live: a `nyx_next` tick persists the run's new cursor + blackboard and pushes `specs://changed`.
  useEngineInvalidation(workflowId !== null, load);

  return { runs, latest: runs[0] ?? null, loading, refresh: load };
}

/**
 * A SPEC's runs, newest-first, across EVERY workflow (`engine_spec_runs`) — the Spec-side twin of
 * {@link useEngineRuns}. The run modal (opened from the Spec detail page and from the dev overlay)
 * starts from a Spec, not from a workflow: a Spec is driven by its ndev run PLUS the child-workflow
 * runs ndev spawns. Live on the same coarse invalidations, so the modal tracks a running `nyx_next`
 * loop. `null` specId disables the hook.
 */
export function useSpecRuns(specId: string | null): {
  runs: readonly EngineWorkflowRun[];
  latest: EngineWorkflowRun | null;
  loading: boolean;
} {
  const [runs, setRuns] = useState<readonly EngineWorkflowRun[]>([]);
  const [loading, setLoading] = useState(specId !== null);

  const load = useCallback(() => {
    if (specId === null) {
      setRuns([]);
      setLoading(false);
      return () => {};
    }
    let cancelled = false;
    setLoading(true);
    nyxBridge
      .invoke<EngineWorkflowRun[]>("engine_spec_runs", { specId })
      .then((rows) => {
        if (!cancelled) setRuns(rows ?? []);
      })
      .catch(() => {
        if (!cancelled) setRuns([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [specId]);

  useEffect(() => load(), [load]);
  useEngineInvalidation(specId !== null, load);

  // `latest` is the newest ROOT run — a run with no parent. A Spec under build also carries the CHILD
  // runs its graph delegated to (the coder sub-workflow), and those are NEWER, so "the newest run"
  // would open on a sub-flow instead of the flow the human actually launched. Falls back to the newest
  // of any kind so an older DB (child runs predating `parent_run_id`) still shows something.
  const root = runs.find((r) => r.parent_run_id == null) ?? runs[0] ?? null;
  return { runs, latest: root, loading };
}

/**
 * The rendered EFFECTOR context for the inspector. Fetches as soon as a spawn node's `role` is known
 * — WITHOUT a `specId` too (finding #35): with no run bound the backend returns the STATIC role
 * template (the sub-agent's brief, its run-specific parts marked "resolved at run"); with a `specId`
 * (a run bound) it returns the LIVE enriched manual. Only `role === null` (a spawn with no `agent_id`)
 * resolves to `null`.
 *
 * `subject` is the spawn node's own `subject`: when it names a Spec section the backend narrows the
 * manual to THAT section, so the inspector shows the very text the run forwards — not all six.
 */
export function useEffectorContext(
  role: string | null,
  specId: string | null,
  subject: string | null = null,
): { context: string | null; loading: boolean; error: boolean } {
  const [context, setContext] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);

  useEffect(() => {
    if (role === null) {
      setContext(null);
      setLoading(false);
      setError(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(false);
    // `specId` present → the live run's context; absent → the static role template.
    nyxBridge
      .invoke<EngineEffectorContext>("engine_effector_context", {
        role,
        ...(specId ? { specId } : {}),
        ...(subject ? { subject } : {}),
      })
      .then((res) => {
        if (!cancelled) setContext(res?.context ?? null);
      })
      .catch(() => {
        if (!cancelled) setError(true);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [role, specId, subject]);

  return { context, loading, error };
}
