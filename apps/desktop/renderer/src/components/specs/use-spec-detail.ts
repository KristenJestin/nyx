import { useCallback, useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { SpecDetail } from "./types";

/**
 * `use-spec-detail` — the LIVE PRD-detail read model: a Spec's full detail (the Spec, its
 * active revision, its tasks + dependency edges, and the DERIVED workspace context) via the
 * `spec_detail` db-command, kept fresh off the backend's coarse pushes with NO polling — the
 * same data-flow pattern as `use-specs` / `use-project-runtime`.
 *
 * The detail's spec / tasks tabs consume `detail.revision` + `detail.tasks`; the activity /
 * runtime tabs consume `detail.workspace_context` (the phase-3 Spec→workspace link) to load
 * the workspace's timeline / runtime. It subscribes to TWO coarse topics (v2.4, T9):
 *   - `specs://changed` — the PRIMARY: every Phase-5 authoring/transition MCP tool returns
 *     `ChangedTopic::Specs` (→ `specs://changed`), so a MCP mutation refreshes the detail
 *     (status / sections / tasks / counters) IMMEDIATELY.
 *   - `workspaces://changed` — kept so the DERIVED workspace context (project + repos) still
 *     re-pulls when the underlying workspace/repos change.
 */

export interface UseSpecDetail {
  detail: SpecDetail | null;
  loading: boolean;
  /** `true` when the Spec could not be loaded (unknown id / backend rejected). */
  error: boolean;
  refresh: () => Promise<void>;
}

/** Load a Spec's detail and keep it live. A `null` id disables the hook. */
export function useSpecDetail(specId: string | null): UseSpecDetail {
  const enabled = specId !== null;
  const [detail, setDetail] = useState<SpecDetail | null>(null);
  const [loading, setLoading] = useState<boolean>(enabled);
  const [error, setError] = useState<boolean>(false);

  const load = useCallback(async () => {
    if (specId === null) {
      setDetail(null);
      setLoading(false);
      setError(false);
      return;
    }
    try {
      const next = await nyxBridge.invoke<SpecDetail>("spec_detail", { specId });
      setDetail(next);
      setError(false);
    } catch {
      setError(true);
    } finally {
      setLoading(false);
    }
  }, [specId]);

  useEffect(() => {
    let cancelled = false;
    if (specId === null) {
      setDetail(null);
      setLoading(false);
      setError(false);
      return () => {
        cancelled = true;
      };
    }
    setLoading(true);
    void nyxBridge
      .invoke<SpecDetail>("spec_detail", { specId })
      .then((next) => {
        if (cancelled) return;
        setDetail(next);
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
  }, [specId]);

  // Live: re-pull on the coarse `specs://changed` (every MCP spec/task mutation) AND
  // `workspaces://changed` (the derived workspace context) invalidations (StrictMode-safe
  // teardown of BOTH subscriptions).
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

  return { detail, loading, error, refresh: load };
}
