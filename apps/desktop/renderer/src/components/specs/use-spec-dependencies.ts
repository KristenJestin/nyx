import { useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { SpecDependencyView } from "./types";

/** The empty dependency view (no edges, zero badge counts) — the loading / no-deps default. */
const EMPTY_VIEW: SpecDependencyView = {
  blocked_by: [],
  unblocks: [],
  related: [],
  blocked_by_count: 0,
  unblocks_count: 0,
};

/**
 * `use-spec-dependencies` — the LIVE Spec↔Spec dependency view of one Spec (`spec_dependency_view`):
 * the directional `blocked_by` / `unblocks` edges + their badge counts, kept fresh off the coarse
 * `specs://changed` push with NO polling (mirror of `use-spec-detail`). Backs the Spec-detail
 * dependency badges. A `null` id, a backend rejection, or a shell that returns nothing all degrade to
 * the EMPTY view (zero badges) — the header never breaks on a missing read.
 */
export function useSpecDependencies(specId: string | null): SpecDependencyView {
  const [view, setView] = useState<SpecDependencyView>(EMPTY_VIEW);

  useEffect(() => {
    if (specId === null) {
      setView(EMPTY_VIEW);
      return;
    }
    let cancelled = false;
    const load = () =>
      nyxBridge
        .invoke<SpecDependencyView | null>("spec_dependency_view", { specId })
        .then((next) => {
          if (!cancelled) setView(next ?? EMPTY_VIEW);
        })
        .catch(() => {
          if (!cancelled) setView(EMPTY_VIEW);
        });
    void load();

    // Live: re-pull on `specs://changed` (a MCP `spec_link`/`spec_unlink` mutation) — no polling.
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

  return view;
}
