import { useCallback, useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";
import type { ActiveScope } from "@/components/terminals/active-project";

import type { IdeaListItem, IdeaSearchResult, IdeaThemeGroup } from "./types";

/**
 * The `{ projectId }` a read is made with — OMITTED for the cross-project scope, because the
 * dispatch reads `arg_opt_str("projectId")` and takes "absent" as "every project" (the same shape
 * `home_snapshot` and `spec_list` take). Shared by the three idea reads so the LIST, the THEMES
 * and the SEARCH can never end up in different scopes.
 */
function scopeArgs(projectId: string | null): Record<string, string> {
  return projectId === null ? {} : { projectId };
}

/**
 * Read the SCOPE the way both hooks below need it: the project to narrow to (`null` = every
 * project), and whether there is anything to read at all.
 *
 * `null` has ONE meaning in the app's scope type — "no project is loaded" — and that case disables
 * the read rather than widening it. "Every project" is the OTHER case, and it is spelled by the
 * scope's own `kind`.
 */
function readScope(scope: ActiveScope): { projectId: string | null; enabled: boolean } {
  if (scope.kind === "all") return { projectId: null, enabled: true };
  // SYSTÈME reads NOTHING here. The singleton is a runtime container — terminals and managed
  // commands — and holds no idea backlog; there is no project id to narrow to either, since it is
  // not one of the projects. The scope is not reachable from this page (its section-nav is empty),
  // and saying so explicitly is what keeps its `null` from being read as "every project".
  if (scope.kind === "system") return { projectId: null, enabled: false };
  return { projectId: scope.projectId, enabled: scope.projectId !== null };
}

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

/**
 * Load the ideas (themes + rows) of a SCOPE and keep them live.
 *
 * The scope is the app's own {@link ActiveScope}, not a `projectId: string | null` — see
 * {@link readScope}. In the cross-project scope the two reads are made ONCE, with no project
 * filter, and each row comes back carrying its project's identity; never a call per project.
 */
export function useIdeas(scope: ActiveScope): UseIdeas {
  const { projectId, enabled } = readScope(scope);
  const [themes, setThemes] = useState<readonly IdeaThemeGroup[]>([]);
  const [ideas, setIdeas] = useState<readonly IdeaListItem[]>([]);
  const [loading, setLoading] = useState<boolean>(enabled);
  const [error, setError] = useState<boolean>(false);

  const load = useCallback(async () => {
    if (!enabled) {
      setThemes([]);
      setIdeas([]);
      setLoading(false);
      setError(false);
      return;
    }
    try {
      const args = scopeArgs(projectId);
      const [themeRows, ideaRows] = await Promise.all([
        nyxBridge.invoke<IdeaThemeGroup[]>("idea_themes", args),
        nyxBridge.invoke<IdeaListItem[]>("idea_list", args),
      ]);
      setThemes(themeRows ?? []);
      setIdeas(ideaRows ?? []);
      setError(false);
    } catch {
      setError(true);
    } finally {
      setLoading(false);
    }
  }, [enabled, projectId]);

  // Initial load (and reload when the scope changes). A load that resolves after the effect was
  // cancelled is dropped so it never clobbers a newer scope's state.
  useEffect(() => {
    let cancelled = false;
    if (!enabled) {
      setThemes([]);
      setIdeas([]);
      setLoading(false);
      setError(false);
      return () => {
        cancelled = true;
      };
    }
    setLoading(true);
    const args = scopeArgs(projectId);
    void Promise.all([
      nyxBridge.invoke<IdeaThemeGroup[]>("idea_themes", args),
      nyxBridge.invoke<IdeaListItem[]>("idea_list", args),
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
  }, [enabled, projectId]);

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
export function useIdeaSearch(scope: ActiveScope, query: string): ReadonlySet<string> | null {
  const { projectId, enabled } = readScope(scope);
  const [matchIds, setMatchIds] = useState<ReadonlySet<string> | null>(null);

  useEffect(() => {
    const q = query.trim();
    if (!enabled || q === "") {
      setMatchIds(null);
      return;
    }
    let cancelled = false;
    // Debounce so a fast typist does not fire a search per keystroke.
    const handle = setTimeout(() => {
      void nyxBridge
        // THE SEARCH FOLLOWS THE LIST'S SCOPE, through the same {@link scopeArgs}. A query that
        // stayed inside the entered project while the page listed every project's ideas would be
        // a silent trap: rows would vanish for no reason the user can see.
        .invoke<IdeaSearchResult[]>("idea_search", {
          ...scopeArgs(projectId),
          query: q,
          limit: 50,
        })
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
  }, [enabled, projectId, query]);

  return matchIds;
}
