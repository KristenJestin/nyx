import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";

import { nyxBridge } from "@/bridge";
import type { TerminalSpecs } from "@/bridge";

import type { Spec } from "../specs/types";
import type { UseTerminalSpecs } from "./use-terminal-specs";

/**
 * `terminal-specs-provider` — ONE resolver for ALL the terminals on screen.
 *
 * ## Why it exists (review finding 01M11WF11C0H8G2MG515FBT6RD)
 *
 * The Spec surcouche used to be rendered for the FOCUSED pane only, so a terminal that carried a
 * Spec but did not hold the keyboard showed NOTHING — three panes running three agents showed one
 * languette. What made that shortcut tempting is the PRD's own invariant: ONE live resolver, never
 * N. But that invariant is about the SUBSCRIPTION, not about the display, and the naive repair —
 * a `<SpecOverlay>` per pane, each running its own {@link useTerminalSpecs} — is exactly what it
 * forbids: N `terminal_specs` resolvers, N `specs://changed` subscriptions and N
 * `agent-sessions://changed` subscriptions.
 *
 * ## The shape retained, and why
 *
 * Three shapes were priced:
 *
 *  1. **a resolver per pane** — REFUSED: N subscriptions on each of the two topics, i.e. the very
 *     cost the PRD put out of scope.
 *  2. **a single resolver + `createPortal` into each pane's DOM node** — one subscription, but the
 *     ANCHORING has to be refabricated: the overlay's positioning block is its pane slot
 *     (`spec-rail-overlay.tsx`'s `absolute inset-y-0 right-0`, clipped by the slot's
 *     `overflow-hidden`), so a portal needs a live registry of pane elements plus an extra commit
 *     before anything can be painted. More moving parts, and the anchoring — the part the criteria
 *     pin — stops being structural.
 *  3. **a single resolver ABOVE the host, the rendering left where it is** — RETAINED. The overlay
 *     stays a plain React child of its pane slot, so the anchoring is untouched by construction
 *     (`offsetParent` is the pane, `overflow-hidden` still clips it, {@link OVERLAY_SURFACE_Z}
 *     still stacks it); only the DATA moves up. One {@link useTerminalSpecsSet} instance holds ONE
 *     `specs://changed` + ONE `agent-sessions://changed` subscription for the whole tiling, and
 *     hands each `<SpecOverlay>` its own slice.
 *
 * ## What it does NOT collapse
 *
 * `terminal_specs` takes ONE terminal id, so a set of N visible terminals still costs N invokes per
 * refresh. That is a READ, not a subscription: it opens no channel, holds nothing live, and N is
 * bounded by the tiling gate (four panes). Collapsing it to one call would mean a new batched
 * read-command across the contract, the sidecar and `nyx_core` — out of this finding's scope, and
 * not what the invariant is about.
 *
 * The provider is INERT on an empty id list: no subscription, no invoke, an empty map. That is how
 * "a selected SERVICE takes the resolver away" keeps holding — the caller hands `[]` instead of
 * unmounting anything.
 */
export type TerminalSpecsSet = ReadonlyMap<string, UseTerminalSpecs>;

/** What a terminal the resolver has not answered for yet reads as: nothing, so no overlay. */
const UNRESOLVED: UseTerminalSpecs = { session: null, specs: [], loading: false };

/**
 * The shared resolution, or `null` when NO provider is mounted — in which case `<SpecOverlay>`
 * falls back to resolving its own terminal (the pre-existing single-overlay behaviour, which the
 * component's own specs still exercise).
 */
const TerminalSpecsContext = createContext<TerminalSpecsSet | null>(null);

/** The shared resolution of the enclosing {@link TerminalSpecsProvider}, or `null` without one. */
export function useSharedTerminalSpecs(): TerminalSpecsSet | null {
  return useContext(TerminalSpecsContext);
}

/**
 * Resolve the ACTIVE session + mapped Spec set of EVERY terminal in `terminalIds`, kept live off
 * the two coarse topics with NO polling — the same data-flow as {@link useTerminalSpecs}, with the
 * subscriptions opened ONCE for the whole set instead of once per terminal.
 */
export function useTerminalSpecsSet(terminalIds: readonly string[]): TerminalSpecsSet {
  // A STABLE dep: the set's identity, not the array's. A snapshot re-push with the same ids must
  // not re-resolve, and a pane added/removed must.
  // The separator is a COMMA — the same key `use-pane-layout` builds for the very same set
  // (`liveIds.join(",")`), and terminal ids are UUIDs, so the key stays injective. It used to be a
  // LITERAL NUL character typed into the source, which made git classify this WHOLE FILE as binary:
  // the module shipped with no diff, no blame and no three-way merge (the commit’s own diffstat
  // reads `Bin 0 -> 7351 bytes`).
  const key = terminalIds.join(",");
  const enabled = terminalIds.length > 0;
  const idsRef = useRef<readonly string[]>(terminalIds);
  idsRef.current = terminalIds;
  const [resolved, setResolved] = useState<TerminalSpecsSet>(() => new Map());
  // Only the LATEST load may publish: a `specs://changed` burst starts several in flight and an
  // older one landing last would resurrect a stale answer.
  const runRef = useRef(0);

  const load = useCallback(async () => {
    const run = ++runRef.current;
    const ids = [...idsRef.current];
    const entries = await Promise.all(
      ids.map(async (terminalId): Promise<[string, UseTerminalSpecs]> => {
        try {
          const res = await nyxBridge.invoke<TerminalSpecs>("terminal_specs", { terminalId });
          // The contract keeps `specs` as `unknown[]` so the bridge does not depend on the Specs UI
          // layer; the entries ARE `nyx_core::db::Spec` rows.
          return [
            terminalId,
            { session: res.session, specs: (res.specs as readonly Spec[]) ?? [], loading: false },
          ];
        } catch {
          return [terminalId, UNRESOLVED];
        }
      }),
    );
    if (run !== runRef.current) return;
    setResolved(new Map(entries));
  }, []);

  // (Re)resolve when the SET changes — a pane opened, closed, or filled with another terminal.
  useEffect(() => {
    if (!enabled) {
      runRef.current += 1;
      setResolved((current) => (current.size === 0 ? current : new Map()));
      return;
    }
    void load().catch(() => {});
    // `key` (not the array) so an identical set does not re-run this.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, enabled, load]);

  // THE ONE PAIR OF SUBSCRIPTIONS, for the whole tiling. StrictMode-safe teardown of both.
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

  return resolved;
}

/**
 * Mount ONE resolver over a tiling and publish its answers to every `<SpecOverlay>` underneath.
 * `terminalIds` is the VISIBLE set — hand `[]` to take the resolver away entirely.
 */
export function TerminalSpecsProvider({
  terminalIds,
  children,
}: {
  terminalIds: readonly string[];
  children: ReactNode;
}) {
  const resolved = useTerminalSpecsSet(terminalIds);
  return <TerminalSpecsContext.Provider value={resolved}>{children}</TerminalSpecsContext.Provider>;
}
