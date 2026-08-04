import { useCallback, useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { DesignRow } from "./types";

/**
 * `useDesign` — the project's DESIGN row (theme tokens + DESIGN.md), via the `design_get`
 * db-command (v2.7 Phase 3, PDR-0010). `null` project disables the hook; a project with no design
 * yet resolves to `null` (a sane state, not an error). Re-pulled on `prototypes://changed` so a
 * theme edit reflects with no poll. The `theme_tokens_json` feeds the renderer's `@theme`.
 */
export interface UseDesign {
  design: DesignRow | null;
  loading: boolean;
  error: boolean;
  /**
   * Re-pull the row on demand (review #42). The `prototypes://changed` subscription already covers
   * the live case; this is for a caller that JUST wrote the theme (the tokens panel) and wants the
   * new tokens in the previews without waiting on the event round-trip.
   */
  refresh: () => void;
}

export function useDesign(projectId: string | null): UseDesign {
  const [design, setDesign] = useState<DesignRow | null>(null);
  const [loading, setLoading] = useState<boolean>(projectId !== null);
  const [error, setError] = useState<boolean>(false);
  const [reloadKey, setReloadKey] = useState(0);
  const refresh = useCallback(() => setReloadKey((k) => k + 1), []);

  useEffect(() => {
    let cancelled = false;
    if (projectId === null) {
      setDesign(null);
      setLoading(false);
      setError(false);
      return () => {
        cancelled = true;
      };
    }
    const load = () => {
      setLoading(true);
      void nyxBridge
        .invoke<DesignRow | null>("design_get", { projectId })
        .then((row) => {
          if (cancelled) return;
          setDesign(row ?? null);
          setError(false);
        })
        .catch(() => {
          if (!cancelled) setError(true);
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
    };
    load();

    let torndown = false;
    let unlisten: (() => void) | null = null;
    void nyxBridge
      .subscribe("prototypes://changed", () => {
        if (!torndown) load();
      })
      .then((un) => {
        if (torndown) void Promise.resolve(un()).catch(() => {});
        else unlisten = un;
      });
    return () => {
      cancelled = true;
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, [projectId, reloadKey]);

  return { design, loading, error, refresh };
}
