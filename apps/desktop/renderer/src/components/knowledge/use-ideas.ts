import { useCallback, useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";

import type { IdeaListItem, IdeaSearchResult, IdeaThemeGroup } from "./types";

/**
 * `use-ideas` — the LIVE Idées read model: a project's THEME groups (`idea_themes`, for the group
 * headers' counter + status mini-dots and the theme facet) + the ENRICHED backlog rows (`idea_list`,
 * each row with its linked Specs), kept fresh off the backend's unsolicited coarse pushes with NO
 * polling — the same data-flow pattern as `use-specs`.
 *
 * It subscribes to TWO coarse topics:
 *   - `ideas://changed` — the PRIMARY: every idea maintenance MCP tool (add / close / merge / link…)
 *     returns `ChangedTopic::Ideas`, so an agent's backlog upkeep refreshes the view immediately.
 *   - `specs://changed` — kept so a linked Spec's status change (which the row/drawer show) re-pulls.
 */
export interface UseIdeas {
  /** The project's theme groups (backlog counts + per-status breakdown). */
  themes: readonly IdeaThemeGroup[];
  /** The project's bounded backlog rows, each enriched with its linked Specs. */
  ideas: readonly IdeaListItem[];
  /** `true` until the first load resolves (for a valid project). */
  loading: boolean;
  /** `true` when the last load failed (the backend rejected). */
  error: boolean;
  /** Re-pull the themes + rows on demand (the same load the live event triggers). */
  refresh: () => Promise<void>;
}

/** Load a project's ideas (themes + rows) and keep them live. A `null` project disables the hook. */
export function useIdeas(projectId: string | null): UseIdeas {
  const enabled = projectId !== null;
  const [themes, setThemes] = useState<readonly IdeaThemeGroup[]>([]);
  const [ideas, setIdeas] = useState<readonly IdeaListItem[]>([]);
  const [loading, setLoading] = useState<boolean>(enabled);
  const [error, setError] = useState<boolean>(false);

  const load = useCallback(async () => {
    if (projectId === null) {
      setThemes([]);
      setIdeas([]);
      setLoading(false);
      setError(false);
      return;
    }
    try {
      const [themeRows, ideaRows] = await Promise.all([
        nyxBridge.invoke<IdeaThemeGroup[]>("idea_themes", { projectId }),
        nyxBridge.invoke<IdeaListItem[]>("idea_list", { projectId }),
      ]);
      setThemes(themeRows ?? []);
      setIdeas(ideaRows ?? []);
      setError(false);
    } catch {
      setError(true);
    } finally {
      setLoading(false);
    }
  }, [projectId]);

  // Initial load (and reload when the project changes). A load that resolves after the effect was
  // cancelled is dropped so it never clobbers a newer project's state.
  useEffect(() => {
    let cancelled = false;
    if (projectId === null) {
      setThemes([]);
      setIdeas([]);
      setLoading(false);
      setError(false);
      return () => {
        cancelled = true;
      };
    }
    setLoading(true);
    void Promise.all([
      nyxBridge.invoke<IdeaThemeGroup[]>("idea_themes", { projectId }),
      nyxBridge.invoke<IdeaListItem[]>("idea_list", { projectId }),
    ])
      .then(([themeRows, ideaRows]) => {
        if (cancelled) return;
        setThemes(themeRows ?? []);
        setIdeas(ideaRows ?? []);
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
  }, [projectId]);

  // Live: re-pull on the coarse `ideas://changed` (agent backlog upkeep) AND `specs://changed`
  // (a linked Spec's status) invalidations — the "sans polling" path. StrictMode-safe teardown.
  useEffect(() => {
    if (!enabled) return;
    let torndown = false;
    const unlistens: Array<() => void> = [];
    const onChanged = () => {
      if (torndown) return;
      void load().catch(() => {});
    };
    for (const topic of ["ideas://changed", "specs://changed"] as const) {
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

  return { themes, ideas, loading, error, refresh: load };
}

/**
 * `use-idea-search` — the BACKEND FTS5 + fuzzy search (`idea_search`), debounced. Resolves the SET
 * of idea ids matching `query` (so the caller filters its loaded rows to those, keeping the enriched
 * row/drawer data), or `null` when the query is blank ("no search — show everything"). Wiring the
 * scored search through the bridge is the "recherche" of the Idées toolbar; the grouping + facet
 * stay client-side over the loaded rows.
 */
export function useIdeaSearch(projectId: string | null, query: string): ReadonlySet<string> | null {
  const [matchIds, setMatchIds] = useState<ReadonlySet<string> | null>(null);

  useEffect(() => {
    const q = query.trim();
    if (projectId === null || q === "") {
      setMatchIds(null);
      return;
    }
    let cancelled = false;
    // Debounce so a fast typist does not fire a search per keystroke.
    const handle = setTimeout(() => {
      void nyxBridge
        .invoke<IdeaSearchResult[]>("idea_search", { projectId, query: q, limit: 50 })
        .then((hits) => {
          if (!cancelled) setMatchIds(new Set((hits ?? []).map((h) => h.id)));
        })
        .catch(() => {
          if (!cancelled) setMatchIds(new Set());
        });
    }, 180);
    return () => {
      cancelled = true;
      clearTimeout(handle);
    };
  }, [projectId, query]);

  return matchIds;
}
