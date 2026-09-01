import { useCallback, useEffect, useRef, useState } from "react";

import { nyxBridge } from "@/bridge";

import { EMPTY_HOME_SNAPSHOT, type HomeSnapshot } from "./types";

/**
 * The topics that INVALIDATE a home. Coarse on purpose, and every one of them is a channel the
 * backend already pushes — the home adds no event of its own and no polling:
 *
 *   * `agent-sessions://changed` — a session start/end AND every per-turn activity hook, so a
 *     row's state (and the question it is blocked on) lands as it happens;
 *   * `terminals://changed` — a terminal opened/closed/renamed, which moves both the sessions
 *     list and the bare-shell one;
 *   * `commands://changed` — every managed-command transition (the services panel);
 *   * `specs://changed` — every Spec/task mutation (the recent-specs panel);
 *   * `workspaces://changed` — the identity/tree topic: a project renamed or re-dressed changes
 *     the mark every cross-project row wears.
 */
const HOME_TOPICS = [
  "agent-sessions://changed",
  "terminals://changed",
  "commands://changed",
  "specs://changed",
  "workspaces://changed",
] as const;

/**
 * WHICH home is read — the renderer's spelling of `nyx_core::home::HomeScope`.
 *
 * `{ global: true }` is every project. `{ global: false, projectId }` is one project's, and its
 * `projectId` is NULLABLE for a reason that matters: the rail resolves the entered project one
 * IPC hop after the page mounts, and "no project entered YET" is not "every project". Reading the
 * global home in that window would flash another scope's rows on the project page, so the hook
 * simply does not read until it has a project — the page stays on its loading screen.
 */
export type HomeReadScope =
  | { readonly global: true }
  | { readonly global: false; readonly projectId: string | null };

/**
 * A read is only a SNAPSHOT when it carries the four families. `nyxBridge.invoke<HomeSnapshot>` is
 * an unchecked cast across the IPC boundary, and a payload that is not a snapshot used to reach
 * the consumers as if it were one — where the first `.map` threw and took the whole screen down.
 *
 * That mattered little while the only consumer was the `/home` page; it matters now that the rail
 * reads this in ITS OWN cross-project scope on EVERY screen (`base-layout.tsx`'s
 * `<AllProjectsRailDot>`), where one malformed answer would blank the entire shell. So a payload
 * that does not have the shape is treated as what it is — a FAILED read — through the path this
 * hook already has for one: keep the last good snapshot, raise `error`, and let the next
 * invalidation recover. The Rust ↔ TS shape itself stays the contract suite's job (`contract/`),
 * not a runtime validation: this only decides what to do when the answer is not one.
 */
function asSnapshot(value: unknown): HomeSnapshot | null {
  const candidate = value as Partial<HomeSnapshot> | null | undefined;
  if (!candidate) return null;
  const complete =
    Array.isArray(candidate.sessions) &&
    Array.isArray(candidate.services) &&
    Array.isArray(candidate.specs) &&
    Array.isArray(candidate.terminals);
  return complete ? (candidate as HomeSnapshot) : null;
}

export interface UseHomeSnapshot {
  /** The four families. {@link EMPTY_HOME_SNAPSHOT} until the first read resolves. */
  readonly snapshot: HomeSnapshot;
  /** `true` until the FIRST read of the current scope resolves. */
  readonly loading: boolean;
  /** `true` when the last read failed (the backend rejected). */
  readonly error: boolean;
}

/**
 * `use-home-snapshot` — the home's ONE read, live.
 *
 * The two pages are the same read model in two scopes ({@link HomeReadScope}), so this is one
 * hook, not two.
 *
 * Same data-flow as `use-specs` / `use-project-runtime`: load once, re-pull on the coarse
 * invalidation topics, StrictMode-safe teardown, and a read that resolves after the effect was
 * cancelled is DROPPED so a stale scope can never overwrite a newer one.
 *
 * A FAILED re-pull keeps the last good snapshot on screen and only raises {@link
 * UseHomeSnapshot.error}: blanking a populated page on a transient IPC error is worse than
 * showing data one tick old.
 */
export function useHomeSnapshot(scope: HomeReadScope): UseHomeSnapshot {
  // `projectId` is OMITTED (not sent as `null`) for the global scope: the dispatch reads
  // `arg_opt_str("projectId")`, and "absent" is what it takes as "every project".
  const projectId = scope.global ? null : scope.projectId;
  const ready = scope.global || scope.projectId !== null;

  const [snapshot, setSnapshot] = useState<HomeSnapshot>(EMPTY_HOME_SNAPSHOT);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);

  // THE SCOPE THE STATE BELONGS TO. Every read stamps the scope it was issued under, and a
  // response is applied only while that stamp still matches. Without it a read fired by an event
  // under the GLOBAL scope can resolve AFTER a project has been entered and paint every project's
  // rows onto the project home — a stale answer winning by arriving late. The mount effect's own
  // `cancelled` flag cannot cover that: the event-driven read lives in another closure.
  // Stamped in the effect below and never during render: a render React discards would otherwise
  // leave the stamp on a scope that was never committed, and the in-flight read of the scope that
  // WAS committed would be judged stale and dropped — leaving the page loading forever.
  const currentScope = useRef<string | null>(null);

  // The ONE read, used by the first pull of a scope and by every live refresh. Written once so a
  // guard added here cannot be missing from the other path.
  const load = useCallback(async () => {
    if (!ready) return;
    const issued = projectId ?? "*";
    // A response is stale the moment the scope it was issued under is no longer the current one.
    const fresh = () => currentScope.current === issued;
    try {
      const raw = await nyxBridge.invoke<HomeSnapshot>(
        "home_snapshot",
        projectId === null ? {} : { projectId },
      );
      if (!fresh()) return;
      const next = asSnapshot(raw);
      if (next === null) {
        setError(true);
        return;
      }
      setSnapshot(next);
      setError(false);
    } catch {
      if (fresh()) setError(true);
    } finally {
      if (fresh()) setLoading(false);
    }
  }, [projectId, ready]);

  // First read of a scope. The snapshot is RESET to empty on a scope change so the previous
  // scope's rows are never shown under the new scope's heading while the read is in flight.
  useEffect(() => {
    currentScope.current = ready ? (projectId ?? "*") : null;
    setSnapshot(EMPTY_HOME_SNAPSHOT);
    setLoading(true);
    void load();
  }, [load, projectId, ready]);

  // Live: re-pull on the coarse topics. No polling.
  useEffect(() => {
    let torndown = false;
    const unlistens: Array<() => void> = [];
    const onChanged = () => {
      if (torndown) return;
      void load().catch(() => {});
    };
    for (const topic of HOME_TOPICS) {
      void nyxBridge
        .subscribe(topic, onChanged)
        .then((un) => {
          if (torndown) {
            void Promise.resolve(un()).catch(() => {});
            return;
          }
          unlistens.push(un);
        })
        // No event bridge (a test host without one): the initial read still seeded the page.
        .catch(() => {});
    }
    return () => {
      torndown = true;
      for (const un of unlistens) void Promise.resolve(un()).catch(() => {});
    };
  }, [load]);

  return { snapshot, loading, error };
}
