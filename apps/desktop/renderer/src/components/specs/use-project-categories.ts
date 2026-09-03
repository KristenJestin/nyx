import { useCallback, useEffect, useState } from "react";

import { errorText, nyxBridge } from "@/bridge";

import type { SpecCategory } from "./types";

export interface UseProjectCategories {
  /** The project's vocabulary in CURATED order (`position`, then `slug`) — the backend's order. */
  readonly categories: readonly SpecCategory[];
  /** The first read has not answered yet. */
  readonly loading: boolean;
  /** The read was refused (the backend's own sentence), or `null`. */
  readonly error: string | null;
}

/**
 * `use-project-categories` — the LIVE "genre of work" vocabulary of ONE project (PDR-0017), read
 * through the `category_list` db-command and kept fresh off the coarse `specs://changed` push with
 * NO polling. The read behind the Spec-detail category selector.
 *
 * # Order and content are the backend's, not this hook's
 *
 * The rows arrive in the project's CURATED order, each carrying its stable slug, its human label
 * and its `created_by_type` provenance — and no curation state, retired with the proposed/confirmed
 * axis (PDR-0017 amended 2026-09-02). Nothing is re-sorted, re-derived or filtered here: a hook
 * that decided which categories are "real" would be a second vocabulary next to the project's.
 *
 * # Why it re-pulls on `specs://changed`
 *
 * That is the topic BOTH writers already push: the agent MINTING a category on the fly through
 * `spec_set_category` / `spec_create(category=…)`, and the human's own curation commands
 * (`spec_category_assign`, `category_create`, `category_rename`, `category_delete`). So a category
 * the agent invents while working appears in this list with no reload — and so does one a human
 * adds or removes from the settings, in the Spec-detail selector, without either screen polling.
 * The same channel `use-spec-detail` rides — one push, every reader.
 *
 * # The error is SHOWN, not swallowed
 *
 * Unlike {@link import("./use-spec-links").useSpecLinks}, a failed read is NOT degraded to an empty
 * list: an empty vocabulary and an unreachable backend look identical on screen, and the selector
 * would silently offer nothing to choose from. The two states are kept apart so the block can say
 * which one it is in.
 */
export function useProjectCategories(projectId: string | null): UseProjectCategories {
  const [categories, setCategories] = useState<readonly SpecCategory[]>([]);
  const [loading, setLoading] = useState<boolean>(projectId !== null);
  const [error, setError] = useState<string | null>(null);

  const reset = useCallback(() => {
    setCategories([]);
    setLoading(false);
    setError(null);
  }, []);

  useEffect(() => {
    if (projectId === null) {
      reset();
      return;
    }
    let cancelled = false;
    setLoading(true);
    const load = () =>
      nyxBridge
        .invoke<SpecCategory[]>("category_list", { projectId })
        .then((rows) => {
          if (cancelled) return;
          setCategories(rows ?? []);
          setError(null);
        })
        .catch((e: unknown) => {
          if (!cancelled) setError(errorText(e));
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
  }, [projectId, reset]);

  return { categories, loading, error };
}
