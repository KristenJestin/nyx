import { useCallback, useEffect, useMemo, useState } from "react";
import { nyxBridge } from "@/bridge";

import { useRuntimeChangeSubscription } from "./use-runtime-live";
import type { WorkspaceRuntimeSnapshot } from "./use-workspace-runtime";

/**
 * `use-project-runtime` — the LIVE runtime read model for a whole PROJECT: the runtime
 * aggregate of EACH of its workspaces, kept live off the backend's unsolicited `*://changed`
 * pushes with NO polling. Rewritten (from the removed per-workspace cockpit hook) for the
 * project→workspaces Terminals page: one enters a PROJECT, whose workspaces are a grouping
 * INSIDE it (PDR-0008 — the workspace stays the runtime pivot in the DB, not a screen).
 *
 * It loads each workspace's aggregate from the SHARED read model (`workspace_runtime_snapshot`
 * — the same authority the MCP `workspace_runtime` tool serves, PDR-0005) and re-pulls ALL of
 * them off the shared live engine (`useRuntimeChangeSubscription` in `use-runtime-live.ts`, the
 * single source of truth for the invalidation topics + teardown) whenever the backend PUSHES a
 * coarse invalidation for a collection the aggregates contain. That re-pull-on-push is exactly
 * the "sans polling" contract — the page reflects a runtime mutation live because the backend
 * TOLD it, not on a timer. `workspaceIds` is a stable-joined key so a re-order/no-op does not
 * re-fetch.
 */

export interface UseProjectRuntime {
  /** Per-workspace runtime aggregate, keyed by workspace id. Absent until first loaded. */
  runtimeByWorkspace: ReadonlyMap<string, WorkspaceRuntimeSnapshot>;
  /** `true` until the first load of every workspace resolves. */
  loading: boolean;
  /** Re-pull every workspace's aggregate on demand (the same load the live events trigger). */
  refresh: () => Promise<void>;
}

/**
 * Load the runtime aggregate of every workspace in `workspaceIds` and keep them live. An empty
 * list disables the hook (empty map, not loading). StrictMode-safe: a re-pull that resolves
 * after the effect was cancelled is dropped so it never clobbers a newer target's state; each
 * subscription is torn down on cleanup.
 */
export function useProjectRuntime(workspaceIds: readonly string[]): UseProjectRuntime {
  // A STABLE key so the effects don't re-run on a fresh-but-equal array identity.
  const idsKey = useMemo(() => [...workspaceIds].sort().join(","), [workspaceIds]);
  const enabled = idsKey.length > 0;

  const [runtimeByWorkspace, setRuntimeByWorkspace] = useState<
    ReadonlyMap<string, WorkspaceRuntimeSnapshot>
  >(() => new Map());
  const [loading, setLoading] = useState<boolean>(enabled);

  const load = useCallback(async () => {
    if (!enabled) {
      setRuntimeByWorkspace(new Map());
      setLoading(false);
      return;
    }
    const ids = idsKey.split(",");
    // Load every workspace in parallel; a single workspace's failure must not blank the
    // others — a rejected snapshot is simply omitted (its next `*://changed` event recovers).
    const entries = await Promise.all(
      ids.map(async (id) => {
        try {
          const snapshot = await nyxBridge.invoke<WorkspaceRuntimeSnapshot>(
            "workspace_runtime_snapshot",
            { workspaceId: id },
          );
          return [id, snapshot] as const;
        } catch {
          return null;
        }
      }),
    );
    const next = new Map<string, WorkspaceRuntimeSnapshot>();
    for (const entry of entries) {
      if (entry) next.set(entry[0], entry[1]);
    }
    setRuntimeByWorkspace(next);
    setLoading(false);
  }, [enabled, idsKey]);

  // Initial load (and reload when the workspace set changes).
  useEffect(() => {
    let cancelled = false;
    if (!enabled) {
      setRuntimeByWorkspace(new Map());
      setLoading(false);
      return () => {
        cancelled = true;
      };
    }
    setLoading(true);
    void load().catch(() => {
      if (!cancelled) setLoading(false);
    });
    return () => {
      cancelled = true;
    };
  }, [load, enabled]);

  // Live: re-pull every workspace on any coarse invalidation — the "sans polling" path,
  // driven by the shared engine (single source of truth for topics + teardown). A re-pull
  // failure is swallowed (the next event recovers).
  const onChange = useCallback(() => {
    void load().catch(() => {});
  }, [load]);
  useRuntimeChangeSubscription(enabled, onChange);

  return { runtimeByWorkspace, loading, refresh: load };
}
