import { useEffect, useRef } from "react";
import { SerializeAddon } from "@xterm/addon-serialize";
import type { Terminal as XTerm } from "@xterm/xterm";
import { nyxBridge } from "@/bridge";
import { RESTORE_SEPARATOR_LABEL } from "./dead-history";

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

/**
 * Matches a dead-history separator LINE in a serialized blob: the visible text
 * `── previous session ──` (RESTORE_SEPARATOR_LABEL), optionally wrapped in SGR
 * ANSI sequences (`\x1b[…m`) on either side of the label or its decorations, up
 * to and including the trailing `\r\n` that `buildDeadHistory` appends. The `g`
 * flag drives `matchAll` so we can target the LAST occurrence.
 *
 * Built from the shared label so the persist side stays in lock-step with the
 * restore side (`buildDeadHistory`): if the label changes, both move together.
 */
const SGR = "(?:\\x1b\\[[0-9;]*m)*";
const DEAD_HISTORY_SEPARATOR_RE = new RegExp(
  `${SGR}── ${SGR}${escapeRegExp(RESTORE_SEPARATOR_LABEL)}${SGR} ──${SGR}\\r?\\n`,
  "g",
);

/** Escape a literal string for safe inclusion in a RegExp source. Pure. */
function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Strip the injected dead-history from a serialized scrollback blob so it is not
 * re-persisted (and thus re-injected) on the next restore cycle.
 *
 * `buildDeadHistory` writes `<prior scrollback>\r\n── previous session ──\r\n`
 * into a restored xterm. The SerializeAddon then serializes the WHOLE buffer —
 * separator included — so without this step each restore would re-embed the
 * previous separator and stack a new one on top (the accumulation bug).
 *
 * Behaviour (pure → unit-tested):
 *  - One or more separators present → keep ONLY the content AFTER the LAST
 *    separator line: the live session below the divider.
 *  - EDGE CASE: if nothing but whitespace/ANSI follows the last separator
 *    (terminal closed immediately after a restore, no live output yet) → do NOT
 *    drop everything; keep the content BEFORE that separator so the previous
 *    session's history is never lost.
 *  - No separator at all → return the blob unchanged (a fresh terminal).
 */
export function stripDeadHistory(serialized: string): string {
  const matches = [...serialized.matchAll(DEAD_HISTORY_SEPARATOR_RE)];
  if (matches.length === 0) return serialized;

  const last = matches[matches.length - 1];
  const sepStart = last.index;
  const sepEnd = sepStart + last[0].length;

  const after = serialized.slice(sepEnd);
  // Live session exists below the divider → keep only that. Strip SGR ANSI
  // (the \x1b control char is intentional here) before testing for live text so
  // a stray colour reset does not read as "content".
  // eslint-disable-next-line no-control-regex
  if (after.replace(/\x1b\[[0-9;]*m/g, "").trim() !== "") return after;

  // Nothing live after the last separator: never lose the prior session. Keep
  // everything BEFORE this separator line.
  return serialized.slice(0, sepStart);
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
    // Serialize → strip the injected dead-history (so the restore separator is
    // not re-persisted and stacked next cycle) → bound to the line cap → persist.
    // One write per call.
    const bounded = boundToLines(stripDeadHistory(serialize()), maxLines);
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
  void nyxBridge.invoke("persist_scrollback", { id: recordId, serialized }).catch(() => {});
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
  // Wire the shell's window close hook via the bridge; a no-op under tests.
  void (async () => {
    try {
      const un = await nyxBridge.window.onCloseRequested(() => flush());
      if (cancelled) un();
      else unlistenTauri = un;
    } catch {
      // No shell window (tests) — the beforeunload fallback covers it.
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
