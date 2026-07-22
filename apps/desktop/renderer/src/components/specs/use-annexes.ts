import { useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { AnnexListItem } from "./types";

export interface UseAnnexes {
  annexes: readonly AnnexListItem[];
  loading: boolean;
}

/**
 * `use-annexes` — the LIVE annexes of one Spec (`annexe_list`): the attachments (kind/title +
 * blob size + dedup hash), newest-first, kept fresh off the coarse `specs://changed` push with NO
 * polling. Backs the Spec-detail "Annexes" block. A `null` id / a backend rejection / a shell that
 * returns nothing degrade to an EMPTY list — the block reads as "no annexes", never as broken.
 */
export function useAnnexes(specId: string | null): UseAnnexes {
  const [annexes, setAnnexes] = useState<readonly AnnexListItem[]>([]);
  const [loading, setLoading] = useState<boolean>(specId !== null);

  useEffect(() => {
    if (specId === null) {
      setAnnexes([]);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    const load = () =>
      nyxBridge
        .invoke<AnnexListItem[]>("annexe_list", { specId })
        .then((rows) => {
          if (!cancelled) setAnnexes(rows ?? []);
        })
        .catch(() => {
          if (!cancelled) setAnnexes([]);
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
    void load();

    // Live: re-pull on `specs://changed` (a MCP `annexe_add`/`annexe_remove` mutation) — no polling.
    let torndown = false;
    let unlisten: (() => void) | null = null;
    void nyxBridge
      .subscribe("specs://changed", () => {
        if (!torndown) void load();
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
  }, [specId]);

  return { annexes, loading };
}
