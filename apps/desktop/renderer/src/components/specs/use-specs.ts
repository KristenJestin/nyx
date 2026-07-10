import { useCallback, useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { SpecListEntry } from "./types";

/**
 * `use-specs` — the LIVE board read model: the LIVE Specs of a project (via the `spec_list`
 * db-command) with each Spec's task tally, kept fresh off the backend's unsolicited coarse
 * pushes with NO polling. Same data-flow pattern as `use-project-runtime` (the terminals
 * runtime hook): load from the shared read model on the sidecar, re-pull on the coarse
 * invalidation topics, StrictMode-safe teardown.
 *
 * It subscribes to TWO coarse topics (v2.4, T9 — "temps réel"):
 *   - `specs://changed` — the PRIMARY: every Phase-5 authoring/transition MCP tool
 *     (`spec_create`/`spec_update_section`/`spec_transition`/`task_add`/`task_transition`/…)
 *     returns `ChangedTopic::Specs`, which the sidecar emits as `specs://changed`. This is
 *     what refreshes the board (status/tasks/counters) immediately after a MCP mutation.
 *   - `workspaces://changed` — the identity/tree topic the domain-event-driven `PushEventSink`
 *     already pushes, kept so a Spec's derived workspace anchoring still nudges a re-pull.
 */

export interface UseSpecs {
  /** The project's LIVE Specs (board entries). Empty until first loaded / when no project. */
  specs: readonly SpecListEntry[];
  /** `true` until the first load resolves (for a valid project). */
  loading: boolean;
  /** `true` when the last load failed (the backend rejected). */
  error: boolean;
  /** Re-pull the list on demand (the same load the live event triggers). */
  refresh: () => Promise<void>;
}

/**
 * Load the Specs of `projectId` and keep them live. A `null` project disables the hook
 * (empty list, not loading). A load that resolves after the effect was cancelled is dropped
 * so it never clobbers a newer project's state.
 */
export function useSpecs(projectId: string | null): UseSpecs {
  const enabled = projectId !== null;
  const [specs, setSpecs] = useState<readonly SpecListEntry[]>([]);
  const [loading, setLoading] = useState<boolean>(enabled);
  const [error, setError] = useState<boolean>(false);

  const load = useCallback(async () => {
    if (projectId === null) {
      setSpecs([]);
      setLoading(false);
      setError(false);
      return;
    }
    try {
      const rows = await nyxBridge.invoke<SpecListEntry[]>("spec_list", { projectId });
      setSpecs(rows);
      setError(false);
    } catch {
      setError(true);
    } finally {
      setLoading(false);
    }
  }, [projectId]);

  // Initial load (and reload when the project changes).
  useEffect(() => {
    let cancelled = false;
    if (projectId === null) {
      setSpecs([]);
      setLoading(false);
      setError(false);
      return () => {
        cancelled = true;
      };
    }
    setLoading(true);
    void nyxBridge
      .invoke<SpecListEntry[]>("spec_list", { projectId })
      .then((rows) => {
        if (cancelled) return;
        setSpecs(rows);
        setError(false);
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

  // Live: re-pull on the coarse `specs://changed` (every MCP spec/task mutation) AND
  // `workspaces://changed` (the Spec's derived workspace anchoring) invalidations — the
  // "sans polling" path. StrictMode-safe teardown of BOTH subscriptions.
  useEffect(() => {
    if (!enabled) return;
    let torndown = false;
    const unlistens: Array<() => void> = [];
    const onChanged = () => {
      if (torndown) return;
      void load().catch(() => {});
    };
    for (const topic of ["specs://changed", "workspaces://changed"] as const) {
      void nyxBridge.subscribe(topic, onChanged).then((un) => {
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
  }, [enabled, load]);

  return { specs, loading, error, refresh: load };
}
