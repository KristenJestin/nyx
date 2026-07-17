import { useCallback, useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";
import type { TerminalSpecs, TerminalSpecsSession } from "@/bridge";

import type { Spec } from "../specs/types";

/**
 * `use-terminal-specs` — the terminal↔Spec OVERLAY read model: a terminal's ACTIVE session
 * summary + the Spec SET that session is working (via the phase-1 `terminal_specs` db-command),
 * kept live off the backend's coarse pushes with NO polling — the SAME data-flow pattern as
 * `use-specs` / `use-spec-detail`.
 *
 * It subscribes to the TWO existing coarse topics that gate the overlay (PRD "temps réel sans
 * polling"):
 *   - `specs://changed` — the auto-map/un-map (a session working / clearing a Spec) AND every
 *     spec/task mutation push this, so the overlay's Spec SET + each Spec's status refresh live.
 *   - `agent-sessions://changed` — the session lifecycle (start / clear / end) pushes this, so
 *     the overlay's LIVENESS axis (`session.state`) and its very presence (no session ⇒ no
 *     overlay) refresh live.
 *
 * A `null` terminal disables the hook (no session, empty set) — the non-regression floor: a
 * terminal with no active session resolves to `{ session: null, specs: [] }` and the overlay
 * renders nothing.
 */
export interface UseTerminalSpecs {
  /** The terminal's ACTIVE session, or `null` (no session ⇒ no overlay). Its `state` is the
   *  liveness axis (`active` while alive) and `agent_kind` the provider (generic). */
  session: TerminalSpecsSession | null;
  /** The Specs of that session's set. Empty when there is no session / an empty set. */
  specs: readonly Spec[];
  /** `true` until the first resolution lands (for a real terminal). */
  loading: boolean;
}

/** Resolve a terminal's active session + its Spec set and keep them live. */
export function useTerminalSpecs(terminalId: string | null): UseTerminalSpecs {
  const enabled = terminalId !== null;
  const [session, setSession] = useState<TerminalSpecsSession | null>(null);
  const [specs, setSpecs] = useState<readonly Spec[]>([]);
  const [loading, setLoading] = useState<boolean>(enabled);

  const load = useCallback(async () => {
    if (terminalId === null) {
      setSession(null);
      setSpecs([]);
      setLoading(false);
      return;
    }
    try {
      const res = await nyxBridge.invoke<TerminalSpecs>("terminal_specs", { terminalId });
      setSession(res.session);
      // The contract keeps `specs` as `unknown[]` so the bridge does not depend on the Specs UI
      // layer; the entries ARE `nyx_core::db::Spec` rows (id / title / status / phases / …).
      setSpecs((res.specs as readonly Spec[]) ?? []);
    } catch {
      setSession(null);
      setSpecs([]);
    } finally {
      setLoading(false);
    }
  }, [terminalId]);

  // Initial load (and reload when the terminal changes). A load that resolves after the effect
  // was cancelled is dropped so it never clobbers a newer terminal's state.
  useEffect(() => {
    let cancelled = false;
    if (terminalId === null) {
      setSession(null);
      setSpecs([]);
      setLoading(false);
      return () => {
        cancelled = true;
      };
    }
    setLoading(true);
    void nyxBridge
      .invoke<TerminalSpecs>("terminal_specs", { terminalId })
      .then((res) => {
        if (cancelled) return;
        setSession(res.session);
        setSpecs((res.specs as readonly Spec[]) ?? []);
      })
      .catch(() => {
        if (cancelled) return;
        setSession(null);
        setSpecs([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [terminalId]);

  // Live: re-pull on `specs://changed` (map/un-map + spec/task mutations) AND
  // `agent-sessions://changed` (session lifecycle / liveness) — the "sans polling" path.
  // StrictMode-safe teardown of BOTH subscriptions.
  useEffect(() => {
    if (!enabled) return;
    let torndown = false;
    const unlistens: Array<() => void> = [];
    const onChanged = () => {
      if (torndown) return;
      void load().catch(() => {});
    };
    for (const topic of ["specs://changed", "agent-sessions://changed"] as const) {
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

  return { session, specs, loading };
}
