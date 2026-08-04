import { useCallback, useEffect, useRef, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { DesignComponentRow } from "./types";

/**
 * `useComponents` — the CATALOGUE read model: the project's `design_components` (DS + NEW) plus the
 * set of component names REFERENCED by the project's proto compositions (the "liés-page" filter),
 * via the `design_component_list` db-command (v2.7 Phase 3, task #10). Portée PROJET. Kept fresh off
 * `prototypes://changed` (an agent proto edit can add a NEW component or link one to a page) with no
 * poll — the same pattern as `useSpecs`.
 */
export interface UseComponents {
  components: readonly DesignComponentRow[];
  /** Names used by at least one proto composition of the project (the liés-page set). */
  linkedNames: ReadonlySet<string>;
  loading: boolean;
  error: boolean;
  refresh: () => Promise<void>;
}

interface ComponentCatalogWire {
  components: DesignComponentRow[];
  linked_names: string[];
}

export function useComponents(projectId: string | null): UseComponents {
  const enabled = projectId !== null;
  const [components, setComponents] = useState<readonly DesignComponentRow[]>([]);
  const [linkedNames, setLinkedNames] = useState<ReadonlySet<string>>(new Set());
  const [loading, setLoading] = useState<boolean>(enabled);
  const [error, setError] = useState<boolean>(false);

  /**
   * The generation of the LAST read asked for — an answer that is no longer the one being waited on
   * is DROPPED. A catalogue is scoped to a PROJECT, and the project can change (or a
   * `prototypes://changed` can fire) before a read comes back: without this, the previous project's
   * components could land under the new project's name.
   */
  const generation = useRef(0);

  const load = useCallback(async () => {
    const mine = (generation.current += 1);
    if (projectId === null) {
      setComponents([]);
      setLinkedNames(new Set());
      setLoading(false);
      setError(false);
      return;
    }
    try {
      const res = await nyxBridge.invoke<ComponentCatalogWire>("design_component_list", {
        projectId,
      });
      if (generation.current !== mine) return;
      setComponents(res.components);
      setLinkedNames(new Set(res.linked_names));
      setError(false);
    } catch {
      if (generation.current !== mine) return;
      setError(true);
    } finally {
      if (generation.current === mine) setLoading(false);
    }
  }, [projectId]);

  useEffect(() => {
    if (projectId === null) {
      setComponents([]);
      setLinkedNames(new Set());
      setLoading(false);
      setError(false);
      return;
    }
    // ANOTHER project is another catalogue: what is in hand belongs to the previous one. (Only this
    // effect clears it — the subscription below re-reads the SAME project, and blanking the grid on
    // every agent edit would flash it.)
    setComponents([]);
    setLinkedNames(new Set());
    setError(false);
    setLoading(true);
    void load();
  }, [projectId, load]);

  // Live: re-pull on the coarse `prototypes://changed` invalidation. StrictMode-safe teardown.
  useEffect(() => {
    if (!enabled) return;
    let torndown = false;
    let unlisten: (() => void) | null = null;
    void nyxBridge
      .subscribe("prototypes://changed", () => {
        if (!torndown) void load().catch(() => {});
      })
      .then((un) => {
        if (torndown) void Promise.resolve(un()).catch(() => {});
        else unlisten = un;
      });
    return () => {
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, [enabled, load]);

  return { components, linkedNames, loading, error, refresh: load };
}
