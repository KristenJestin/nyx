import { useCallback, useEffect, useRef, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { ProtoTree } from "./proto-types";

/**
 * `useProtoTree` — the PROTOTYPE tree of a Spec (`proto_tree` db-command, review #24): the whole
 * tree (pages / versions / variants + rounds + election state + anchored comments), or `null` when
 * the Spec has NO prototype (a valid state — the tab's empty cue, not an error). A pure read shared
 * with the MCP `proto_*` authority (PDR-0005). Kept fresh off the coarse `prototypes://changed`
 * invalidation (an agent proto edit reflects live) with no poll — the same pattern as `useComponents`.
 */
export interface UseProtoTree {
  tree: ProtoTree | null;
  loading: boolean;
  error: boolean;
  refresh: () => Promise<void>;
}

export function useProtoTree(specId: string | null): UseProtoTree {
  const enabled = specId !== null;
  const [tree, setTree] = useState<ProtoTree | null>(null);
  const [loading, setLoading] = useState<boolean>(enabled);
  const [error, setError] = useState<boolean>(false);

  /**
   * The generation of the LAST read asked for. A read is a promise, and the Spec on screen can change
   * (or a `prototypes://changed` can fire) before one comes back — so an answer that is no longer the
   * one being waited on is DROPPED. Without it, walking from one Spec to another could land the
   * previous Spec's prototype under the new one's name, which is the one thing a tree may not do.
   */
  const generation = useRef(0);

  const load = useCallback(async () => {
    const mine = (generation.current += 1);
    if (specId === null) {
      setTree(null);
      setLoading(false);
      setError(false);
      return;
    }
    try {
      const res = await nyxBridge.invoke<ProtoTree | null>("proto_tree", { specId });
      if (generation.current !== mine) return;
      setTree(res ?? null);
      setError(false);
    } catch {
      if (generation.current !== mine) return;
      setError(true);
    } finally {
      if (generation.current === mine) setLoading(false);
    }
  }, [specId]);

  useEffect(() => {
    if (!enabled) {
      setTree(null);
      setLoading(false);
      setError(false);
      return;
    }
    // ANOTHER Spec is another prototype: the tree in hand belongs to the previous one and must not
    // be shown under this one's name while the read is in flight. (Only this effect clears it — the
    // subscription below re-reads the SAME Spec, and blanking there would flash the workbench.)
    setTree(null);
    setError(false);
    setLoading(true);
    void load();

    // Live: re-pull on the coarse `prototypes://changed` invalidation. StrictMode-safe teardown.
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

  return { tree, loading, error, refresh: load };
}
