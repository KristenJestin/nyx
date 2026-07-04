import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
} from "react";

/**
 * `use-consumed-notifications` — the page-local memory of which runtime notifications the user has
 * already CONSUMED this session (finding 53). A success/error dot is an "unseen result" marker
 * that must vanish once its row is visited AND stay gone afterwards.
 *
 * The backend mark-read (`terminal_exec_mark_read` / `command_acknowledge`) PERSISTS the clear,
 * but those commands do NOT broadcast a runtime-collection `*://changed` topic, so the page's
 * runtime snapshot is not re-pulled on a mark-read — its `exec_state_unread` / `unread` stays
 * stale until an unrelated event re-pulls it. Without a local record, leaving then returning to a
 * consumed terminal would resurface its (already-read) dot. This tiny registry closes that gap:
 * a visited row's id is remembered so the dot stays consumed for the rest of the session,
 * regardless of snapshot staleness (the persisted clear covers the next fresh snapshot). Keyed by
 * a `kind:id` string so terminals and services never collide.
 */
interface ConsumedContextValue {
  /** Whether `id` of `kind` has been consumed (visited) this session. */
  readonly isConsumed: (kind: "terminal" | "service", id: string) => boolean;
  /** Mark `id` of `kind` consumed (idempotent). */
  readonly consume: (kind: "terminal" | "service", id: string) => void;
}

const ConsumedContext = createContext<ConsumedContextValue | null>(null);

export function ConsumedNotificationsProvider({ children }: { children: ReactNode }) {
  const [consumed, setConsumed] = useState<ReadonlySet<string>>(() => new Set());
  const consumedRef = useRef(consumed);
  consumedRef.current = consumed;

  const consume = useCallback((kind: "terminal" | "service", id: string) => {
    const key = `${kind}:${id}`;
    if (consumedRef.current.has(key)) return;
    setConsumed((prev) => {
      if (prev.has(key)) return prev;
      const next = new Set(prev);
      next.add(key);
      return next;
    });
  }, []);

  const value = useMemo<ConsumedContextValue>(
    () => ({
      isConsumed: (kind, id) => consumed.has(`${kind}:${id}`),
      consume,
    }),
    [consumed, consume],
  );

  return <ConsumedContext.Provider value={value}>{children}</ConsumedContext.Provider>;
}

/**
 * Read the consumed-notifications registry. Returns a NO-OP fallback when no provider is mounted
 * (nothing consumed, `consume` inert) so a component can be rendered/tested in isolation without
 * wrapping it; the page mounts the real provider.
 */
export function useConsumedNotifications(): ConsumedContextValue {
  return useContext(ConsumedContext) ?? NOOP;
}

const NOOP: ConsumedContextValue = {
  isConsumed: () => false,
  consume: () => {},
};
