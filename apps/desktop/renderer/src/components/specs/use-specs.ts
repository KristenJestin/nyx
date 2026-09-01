import { useCallback, useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";
import type { ActiveScope } from "@/components/terminals/active-project";

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
 * Load the Specs of a SCOPE and keep them live.
 *
 * The scope is the app's own {@link ActiveScope}, not a `projectId: string | null` — and that is
 * the point. `null` has ONE meaning here, "no project is loaded" (a fresh install, or the window
 * before the rail resolves the entered one), which disables the hook. "Every project" is a
 * DIFFERENT case, spelled `{ kind: "all" }`, and it makes ONE call with no project filter — never
 * a loop of per-project calls. Encoding both on the same `null` is the ambiguity the app's scope
 * type exists to remove (`components/terminals/active-project.tsx`).
 *
 * A load that resolves after the effect was cancelled is dropped so it never clobbers a newer
 * scope's state.
 */
export function useSpecs(scope: ActiveScope): UseSpecs {
  const global = scope.kind === "all";
  // The project the read is narrowed to, or `null` for the cross-project read.
  const projectId = global ? null : scope.projectId;
  // NO project loaded yet — not "every project". The hook says nothing rather than reading
  // the whole app's Specs onto a page that means to show one project's.
  const enabled = global || projectId !== null;
  const [specs, setSpecs] = useState<readonly SpecListEntry[]>([]);
  const [loading, setLoading] = useState<boolean>(enabled);
  const [error, setError] = useState<boolean>(false);

  const load = useCallback(async () => {
    if (!enabled) {
      setSpecs([]);
      setLoading(false);
      setError(false);
      return;
    }
    try {
      // `projectId` is OMITTED (not sent as `null`) for the cross-project read: the dispatch
      // reads `arg_opt_str("projectId")`, and "absent" is what it takes as "every project".
      const rows = await nyxBridge.invoke<SpecListEntry[]>(
        "spec_list",
        projectId === null ? {} : { projectId },
      );
      setSpecs(rows);
      setError(false);
    } catch {
      setError(true);
    } finally {
      setLoading(false);
    }
  }, [enabled, projectId]);

  // Initial load (and reload when the scope changes).
  useEffect(() => {
    let cancelled = false;
    if (!enabled) {
      setSpecs([]);
      setLoading(false);
      setError(false);
      return () => {
        cancelled = true;
      };
    }
    setLoading(true);
    void nyxBridge
      .invoke<SpecListEntry[]>("spec_list", projectId === null ? {} : { projectId })
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
  }, [enabled, projectId]);

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
