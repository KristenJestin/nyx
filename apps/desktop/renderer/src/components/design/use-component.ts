import { useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { DesignComponentRow } from "./types";

/**
 * `useComponent` — ONE `design_components` row by id (the composant-detail read model, task #11) via
 * `design_component_get`. `null` id disables the hook; an unknown id resolves to `null`. Re-pulled on
 * `prototypes://changed` (an agent edit to the component's template/params reflects live).
 */
export interface UseComponent {
  component: DesignComponentRow | null;
  loading: boolean;
  error: boolean;
}

export function useComponent(componentId: string | null): UseComponent {
  const [component, setComponent] = useState<DesignComponentRow | null>(null);
  const [loading, setLoading] = useState<boolean>(componentId !== null);
  const [error, setError] = useState<boolean>(false);

  useEffect(() => {
    let cancelled = false;
    if (componentId === null) {
      setComponent(null);
      setLoading(false);
      setError(false);
      return () => {
        cancelled = true;
      };
    }
    const load = () => {
      setLoading(true);
      void nyxBridge
        .invoke<DesignComponentRow | null>("design_component_get", { componentId })
        .then((row) => {
          if (cancelled) return;
          setComponent(row ?? null);
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
  }, [componentId]);

  return { component, loading, error };
}
