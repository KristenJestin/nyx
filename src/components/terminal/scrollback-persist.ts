import { useEffect, useRef } from "react";
import { SerializeAddon } from "@xterm/addon-serialize";
import type { Terminal as XTerm } from "@xterm/xterm";
import { invoke } from "@tauri-apps/api/core";

/**
 * Upper bound (in LINES) on the scrollback we serialize and persist per
 * terminal. Aligned with xterm's `scrollback: 10_000` option (see
 * `terminal.tsx`): there is no point persisting more history than the live
 * buffer can hold, and bounding here keeps the serialized blob — and therefore
 * the SQLite row — from growing without limit on a chatty terminal. The backend
 * ALSO bounds (by bytes, `db::MAX_SCROLLBACK_BYTES`); this line cap is the
 * front-side bound the task requires, aligned with the renderer's own scrollback.
 */
export const SCROLLBACK_MAX_LINES = 10_000;

/**
 * Keep only the LAST `max` newline-separated rows of `s` (the recent tail — what
 * the user wants to see on restore). Splits on `\n`, so a CRLF stream keeps the
 * trailing `\r` on each kept row, exactly as xterm serialized it. Pure →
 * unit-tested. Returns `s` unchanged when it already fits.
 */
export function boundToLines(s: string, max: number): string {
  if (max <= 0) return "";
  // A fast path that also avoids splitting a huge string when it clearly fits:
  // count newlines cheaply first would be premature — split is fine at this cap.
  const lines = s.split("\n");
  if (lines.length <= max) return s;
  return lines.slice(lines.length - max).join("\n");
}

/** Persist a serialized scrollback blob for a terminal record (debounced caller). */
export type PersistFn = (recordId: string, serialized: string) => void;

/**
 * The slice of `@xterm/addon-serialize` this module depends on: an
 * `ITerminalAddon` (so `loadAddon` can `activate` it) that also serializes the
 * buffer. Narrowed to an interface so tests inject a deterministic fake.
 */
export interface SerializeAddonLike {
  serialize(): string;
  activate(terminal: XTerm): void;
  dispose(): void;
}

export interface ScrollbackPersisterOptions {
  /** The terminal RECORD id (SQLite row) this scrollback belongs to. */
  recordId: string;
  /** Produce the current serialized scrollback (e.g. the SerializeAddon). */
  serialize: () => string;
  /** Sink for a bounded snapshot (e.g. `invoke('persist_scrollback', …)`). */
  persist: PersistFn;
  /** Quiet-period (ms) after the last activity before a snapshot is taken. */
  debounceMs?: number;
  /** Line cap applied to the serialized blob before persisting. */
  maxLines?: number;
}

/**
 * The imperative surface a `<Terminal>` drives to persist its scrollback.
 *  - `schedule()` — call on terminal activity (output/input). DEBOUNCED: a burst
 *    coalesces into a single snapshot taken once the terminal goes quiet. This is
 *    the load-bearing "never persist per byte" guarantee.
 *  - `flush()` — snapshot+persist NOW (tab close / app close), bypassing the
 *    debounce, and cancel any pending debounced snapshot so we write once.
 *  - `dispose()` — cancel any pending snapshot (teardown; no late write).
 */
export interface ScrollbackPersister {
  schedule(): void;
  flush(): void;
  dispose(): void;
}

/**
 * Build a debounced scrollback persister. Framework-agnostic (no React, no DOM)
 * so the debounce + bounding logic is unit-tested directly with fake timers and
 * a spy `persist`. The hook below wires it to an xterm instance.
 */
export function createScrollbackPersister(
  options: ScrollbackPersisterOptions,
): ScrollbackPersister {
  const {
    recordId,
    serialize,
    persist,
    debounceMs = 750,
    maxLines = SCROLLBACK_MAX_LINES,
  } = options;

  let timer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;

  const cancel = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };

  const snapshot = () => {
    // Serialize → bound to the line cap → persist. One write per call.
    const bounded = boundToLines(serialize(), maxLines);
    persist(recordId, bounded);
  };

  const schedule = () => {
    if (disposed) return;
    // Trailing-edge debounce: every activity resets the timer, so we snapshot
    // once the terminal has been quiet for `debounceMs` — NOT once per byte.
    cancel();
    timer = setTimeout(() => {
      timer = null;
      if (disposed) return;
      snapshot();
    }, debounceMs);
  };

  const flush = () => {
    if (disposed) return;
    // Immediate write (close paths). Cancel the pending debounce first so the
    // burst that triggered the close does not also fire a second, later write.
    cancel();
    snapshot();
  };

  const dispose = () => {
    disposed = true;
    cancel();
  };

  return { schedule, flush, dispose };
}

/**
 * Wire scrollback persistence to an xterm instance for one terminal record.
 *
 * Attaches a `SerializeAddon`, then DEBOUNCES a snapshot on every terminal write
 * (output is the dominant source of scrollback growth; keystrokes are covered by
 * the echo they trigger). The snapshot is bounded to the line cap and sent to
 * the backend via `persist_scrollback`. It also flushes synchronously on UNMOUNT
 * (tab close) and registers an APP-CLOSE flush via `registerAppCloseFlush` so the
 * restored history is current after a clean shutdown. Per the task: persistence
 * is DEBOUNCED only — never one write per byte.
 *
 * `recordId` keys the persisted row; `serializeAddonFactory` and `persist` are
 * injectable so the hook is exercisable in jsdom without the real addon/IPC.
 */
export function useScrollbackPersist(
  instance: XTerm | null,
  recordId: string | null,
  options: {
    persist?: PersistFn;
    /**
     * Factory for the serialize addon. Must be an xterm `ITerminalAddon`-shaped
     * object (it is passed to `loadAddon`, which calls `activate`) that also
     * exposes `serialize()`. Defaults to the real `@xterm/addon-serialize`.
     */
    serializeAddonFactory?: () => SerializeAddonLike;
    debounceMs?: number;
  } = {},
): void {
  const {
    persist = defaultPersist,
    serializeAddonFactory = defaultSerializeAddonFactory,
    debounceMs,
  } = options;

  // Keep the live persister on a ref so the app-close listener (registered once)
  // always flushes the CURRENT one.
  const persisterRef = useRef<ScrollbackPersister | null>(null);

  useEffect(() => {
    // No instance or no record id (record-less standalone terminal): persistence
    // is a no-op — there is nowhere to persist to.
    if (!instance || recordId === null) return;

    let addon: SerializeAddonLike | undefined;
    try {
      addon = serializeAddonFactory();
      (instance as unknown as { loadAddon(a: unknown): void }).loadAddon(addon);
    } catch {
      // SerializeAddon unavailable: persistence is best-effort, degrade quietly.
      addon = undefined;
    }

    const persister = createScrollbackPersister({
      recordId,
      serialize: () => addon?.serialize() ?? "",
      persist,
      ...(debounceMs !== undefined ? { debounceMs } : {}),
    });
    persisterRef.current = persister;

    // Debounce a snapshot on every parsed write (the output that grows the
    // scrollback). `onWriteParsed` fires after a chunk is applied to the buffer,
    // so the serialized snapshot reflects it.
    const sub = instance.onWriteParsed(() => persister.schedule());

    // Flush on app close so the restored history is up to date after a clean
    // shutdown. Best-effort; see registerAppCloseFlush.
    const unregister = registerAppCloseFlush(() => persisterRef.current?.flush());

    return () => {
      sub.dispose();
      unregister();
      // Tab close: take a final snapshot, then tear down (no late write).
      persister.flush();
      persister.dispose();
      persisterRef.current = null;
      addon?.dispose();
    };
    // recordId/instance identity drive the wiring; the injected fns are stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [instance, recordId]);
}

/** Default sink: persist the bounded scrollback over the Tauri IPC. */
function defaultPersist(recordId: string, serialized: string): void {
  void invoke("persist_scrollback", { id: recordId, serialized }).catch(() => {});
}

/** Default addon factory: a real `@xterm/addon-serialize`. */
function defaultSerializeAddonFactory(): SerializeAddonLike {
  return new SerializeAddon() as unknown as SerializeAddonLike;
}

/**
 * Register `flush` to run when the app window is about to close, so each
 * terminal takes a final scrollback snapshot before nyx exits (best-effort).
 *
 * Uses Tauri's window `onCloseRequested` when available (the real app) and falls
 * back to the DOM `beforeunload` event (tests / non-Tauri). Returns an
 * unregister fn. Failures are swallowed: persistence on shutdown is best-effort —
 * the SQLite record from earlier debounced snapshots is the floor.
 */
function registerAppCloseFlush(flush: () => void): () => void {
  // DOM fallback / belt-and-braces: also flush on beforeunload.
  const onBeforeUnload = () => flush();
  if (typeof window !== "undefined") {
    window.addEventListener("beforeunload", onBeforeUnload);
  }

  let unlistenTauri: (() => void) | null = null;
  let cancelled = false;
  // Lazily wire the Tauri window close hook; ignore if the API is absent.
  void (async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const un = await getCurrentWindow().onCloseRequested(() => flush());
      if (cancelled) un();
      else unlistenTauri = un;
    } catch {
      // Not running under Tauri (tests) — the beforeunload fallback covers it.
    }
  })();

  return () => {
    cancelled = true;
    if (typeof window !== "undefined") {
      window.removeEventListener("beforeunload", onBeforeUnload);
    }
    unlistenTauri?.();
  };
}
