import { useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { SpecLink } from "./types";

export interface UseSpecLinks {
  links: readonly SpecLink[];
  loading: boolean;
}

/**
 * `use-spec-links` — the LIVE EXTERNAL LINKS of one Spec (`annexe_link_list`): the `url` annexes
 * RESOLVED (label + the URL itself), newest-first, kept fresh off the coarse `specs://changed` push
 * with NO polling. It is the SAME read model as the annexes block, narrowed to one facet — a link
 * is not a second entity.
 *
 * A `null` id / a backend rejection / a shell that returns nothing degrade to an EMPTY list, which
 * the block renders as its explicit "no link yet" state — never as broken.
 *
 * The push it listens to is the one BOTH writers emit: the app's own `annexe_add`/`annexe_remove`
 * and the agent's MCP `annexe_*` tools. So a link the agent attaches appears here with no reload,
 * exactly like one added from this panel.
 */
export function useSpecLinks(specId: string | null): UseSpecLinks {
  const [links, setLinks] = useState<readonly SpecLink[]>([]);
  const [loading, setLoading] = useState<boolean>(specId !== null);

  useEffect(() => {
    if (specId === null) {
      setLinks([]);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    const load = () =>
      nyxBridge
        .invoke<SpecLink[]>("annexe_link_list", { specId })
        .then((rows) => {
          if (!cancelled) setLinks(rows ?? []);
        })
        .catch(() => {
          if (!cancelled) setLinks([]);
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
    void load();

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

  return { links, loading };
}
