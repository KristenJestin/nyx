import { useCallback, useEffect, useRef } from "react";
import { FitAddon } from "@xterm/addon-fit";
import type { IDisposable, Terminal as XTerm } from "@xterm/xterm";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Payload of the backend `pty://output` event. `bytes` is a JSON byte array. */
interface PtyOutputPayload {
  id: number;
  bytes: number[];
}

/** Payload of the backend `pty://exit` event. */
interface PtyExitPayload {
  id: number;
  code: number | null;
}

export interface UsePtyOptions {
  /**
   * Working directory for the spawned shell. `undefined` lets the backend use
   * its default (it inherits nyx's cwd, i.e. home/current).
   */
  cwd?: string;
  /**
   * The PERSISTENT terminal RECORD id (SQLite `terminals.id`) this session is
   * bound to. Passed straight through to `pty_spawn` so the backend can map the
   * live pty_id → terminal record id and address the durable record for
   * exec-state (PRD-2.1). `undefined` for a record-less standalone terminal (the
   * socle / unit harness): the backend then records no mapping and skips
   * exec-state. This is pure plumbing — the record id is NOT a new identity model.
   */
  recordId?: string;
  /**
   * Called with the resolved live PTY id once the spawn completes, and with
   * `null` when the session ends/teardown clears it. Used by the auto-naming
   * layer (it needs the PTY id to read `terminal_info`). Optional — the socle /
   * tests don't need it.
   */
  onPtyId?: (id: number | null) => void;
  /**
   * Read-only DEAD HISTORY bytes (prior scrollback + separator) to write into the
   * terminal ONCE, as the VERY FIRST thing the session does — BEFORE the
   * `pty://output` listener is attached and BEFORE `pty_spawn` — so the restored
   * history is guaranteed to sit ABOVE the live shell's first prompt (finding
   * 01KV3CPAG2KTV413C4RVNH6TVN: previously the async dead-history effect could
   * lose the race to the PTY's first output, landing history BELOW the live input
   * so the user couldn't type). NEVER sent to the PTY — `start()` writes it to
   * xterm only. Empty/undefined → nothing written (a brand-new terminal).
   */
  deadHistory?: string;
}

/**
 * The live state of one PTY session, kept on a ref so it survives React's
 * StrictMode setup → cleanup → setup double-invoke on the same fiber.
 */
interface PtySession {
  /** The xterm instance this session is bound to. */
  term: XTerm;
  /** Resolved PTY id (null while the spawn is still pending / after exit). */
  id: number | null;
  /** True once a spawn has been issued for this session (dedupe guard). */
  spawnIssued: boolean;
  /** True once teardown ran for real (not a StrictMode throwaway). */
  torndown: boolean;
  /**
   * Set when a size resync was requested before the spawn resolved (id still
   * null). `start()` honours it right after the spawn so the PTY adopts the
   * terminal's current cols/rows even if the resync raced ahead of the spawn.
   */
  pendingResync: boolean;
  /**
   * Bumped on every effect setup. Cleanup captures the value it saw; the
   * deferred teardown only fires if no later setup bumped it (i.e. this really
   * was the last cleanup — a real unmount, not a StrictMode throwaway).
   */
  generation: number;
  /**
   * Output events that arrived BEFORE the spawn resolved (while `id` was still
   * null, so they could not be routed). `start()` replays the ones tagged with
   * our resolved id right after the spawn, then clears this — so early shell
   * output (the first prompt) is never dropped if it races ahead of `pty_spawn`.
   */
  pendingOutput: PtyOutputPayload[];
  unlistenOutput?: UnlistenFn;
  unlistenExit?: UnlistenFn;
  disposables: IDisposable[];
  /** Notify the consumer when the live PTY id resolves / clears (auto-naming). */
  onPtyId?: (id: number | null) => void;
}

/**
 * Wire an xterm instance to a backend PTY for a full, live terminal:
 * spawn at mount, keystrokes → `pty_write`, `pty://output` → `term.write`,
 * resize → `pty_resize`, and a clean teardown (`unlisten` + `pty_close`) at
 * unmount. `pty://exit` is surfaced as a final notice without crashing.
 *
 * StrictMode-safe. React runs the effect twice in dev (setup → cleanup →
 * setup). Three independent mechanisms cooperate; note carefully which one is
 * actually load-bearing for the "exactly one spawn" guarantee:
 *
 *  1. Session reuse + `spawnIssued`: the session lives on a ref keyed by the
 *     terminal instance, so the second setup REUSES it. Since the reused
 *     session already has `spawnIssued=true`, the effect does NOT call `start()`
 *     a second time. This is an OPTIMISATION, not the sole guarantee — see (3).
 *  2. Generation guard: the cleanup defers its teardown and only runs it if no
 *     later setup bumped the generation, so the StrictMode throwaway cleanup
 *     never closes the surviving PTY, while a real unmount does run `pty_close`.
 *  3. The `torndown` bail in `start()` is the GUARANTEE against a double spawn.
 *     `start()` is async — it awaits two `listen()` calls before
 *     `invoke('pty_spawn')`. StrictMode's throwaway cleanup runs synchronously
 *     in between and sets `torndown=true`, so even if a second `start()` is in
 *     flight (e.g. with the reuse guard of (1) removed) it bails at
 *     `if (session.torndown) return` before reaching `pty_spawn`. Removing (1)
 *     alone keeps the single spawn (the bail catches it); only removing BOTH
 *     (1) and the `torndown` bail produces two `pty_spawn` calls. The unit test
 *     verifies this — see use-pty.test.tsx.
 *
 * Returns a stable `resyncSize()` callback. Call it to push the terminal's
 * CURRENT cols/rows to the PTY out-of-band — i.e. independently of xterm's
 * `onResize` event. This closes a timing gap: an authoritative fit can change
 * cols/rows (e.g. the cell metric shifting once the real monospace face loads)
 * BEFORE the `onResize` handler is registered (it is wired only after
 * `pty_spawn` resolves). Without resync, that font-driven resize would be lost
 * and the PTY would stay pinned at the spawn-time size. `pty_resize` is
 * idempotent on the backend, so a redundant resync is a no-op.
 *
 * @param term the xterm instance to drive (null until it is created)
 * @param fitAddon the fit addon already loaded into `term`, used to size the PTY
 * @param options spawn options (cwd)
 * @returns `resyncSize()` — pushes the terminal's current size to the PTY now
 *   (if the session id is known) or defers it to just after spawn (if not).
 */
export function usePty(
  term: XTerm | null,
  fitAddon: FitAddon,
  options: UsePtyOptions = {},
): () => void {
  const { cwd, recordId, onPtyId, deadHistory } = options;
  const sessionRef = useRef<PtySession | null>(null);
  // Keep the latest onPtyId on a ref so the session always calls the current one
  // without re-running the spawn effect when the callback identity changes.
  const onPtyIdRef = useRef(onPtyId);
  onPtyIdRef.current = onPtyId;

  // Stable across renders: reads the live session off the ref each call, so it
  // is safe to capture in another effect's deps without re-running it.
  const resyncSize = useCallback(() => {
    const session = sessionRef.current;
    if (!session || session.torndown) return;
    resyncSession(session);
  }, []);

  useEffect(() => {
    if (!term) return;

    // Resolve the session for THIS terminal. A non-torndown session bound to the
    // same xterm instance is reused (StrictMode second setup); otherwise a fresh
    // one is created. Keying on the instance is the dedupe authority.
    let session = sessionRef.current;
    if (!session || session.term !== term || session.torndown) {
      session = {
        term,
        id: null,
        spawnIssued: false,
        torndown: false,
        pendingResync: false,
        generation: 0,
        pendingOutput: [],
        disposables: [],
        onPtyId: (id) => onPtyIdRef.current?.(id),
      };
      sessionRef.current = session;
    } else {
      // Reused: bump the generation so the throwaway cleanup's deferred teardown
      // (captured at the lower generation) bails out and keeps this PTY alive.
      session.generation += 1;
    }

    // Spawn dedupe at the effect level: a reused session already has
    // `spawnIssued=true`, so `start()` is not invoked twice. Note this guard is
    // NOT sufficient on its own to prevent a double `pty_spawn`: if the
    // session-reuse guard above were removed, each setup would build a fresh
    // session (`spawnIssued=false`) and call `start()` again — but the async
    // `torndown` bail inside `start()` (see below) still suppresses the
    // throwaway session's spawn. Both must be removed before a second
    // `pty_spawn` actually reaches the IPC. (Mutation-verified.)
    if (!session.spawnIssued) {
      session.spawnIssued = true;
      void start(session, fitAddon, cwd, recordId, deadHistory);
    }

    const myGeneration = session.generation;
    const boundSession = session;

    return () => {
      // Defer: a StrictMode re-setup runs synchronously after this and bumps
      // the generation, marking this cleanup as a throwaway.
      const captured = myGeneration;
      queueMicrotask(() => {
        if (boundSession.generation !== captured) return; // a later setup re-claimed it
        teardown(boundSession);
      });
    };
    // Only re-run when the terminal instance or cwd changes; re-running on every
    // render would respawn the shell.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [term, cwd]);

  return resyncSize;
}

/** Set up listeners and spawn the PTY for a fresh session. */
async function start(
  session: PtySession,
  fitAddon: FitAddon,
  cwd: string | undefined,
  recordId: string | undefined,
  deadHistory: string | undefined,
): Promise<void> {
  const { term } = session;
  const decode = (bytes: number[]): Uint8Array => Uint8Array.from(bytes);
  const encoder = new TextEncoder();

  // RESTORE ORDERING (finding 01KV3CPAG…): write the read-only dead history FIRST
  // — synchronously, before the `pty://output` listener exists and before
  // `pty_spawn` — so the restored "previous session" block is guaranteed to land
  // ABOVE the live shell's first prompt. Writing it from the React effect instead
  // raced the PTY's first output (which arrives via an async event that can beat
  // a late `instance`-dependent effect), landing history BELOW the live input so
  // the cursor sat above dead history and the user couldn't type. This write
  // NEVER reaches the PTY — it is xterm-only.
  if (deadHistory) term.write(deadHistory);

  // Subscribe BEFORE spawning so no early output is dropped. Until the spawn
  // resolves we don't know our id, so we can't route an event yet — buffer it
  // and let `start()` replay the matching chunks once the id is known (see the
  // drain after `pty_spawn`). Filtering on `id === null` alone would DROP output
  // that races ahead of the spawn round-trip (the first prompt under IPC load).
  session.unlistenOutput = await listen<PtyOutputPayload>("pty://output", (event) => {
    if (session.torndown) return;
    if (session.id === null) {
      session.pendingOutput.push(event.payload);
      return;
    }
    if (event.payload.id !== session.id) return;
    term.write(decode(event.payload.bytes));
  });

  session.unlistenExit = await listen<PtyExitPayload>("pty://exit", (event) => {
    if (session.torndown || session.id === null) return;
    if (event.payload.id !== session.id) return;
    const code = event.payload.code;
    term.write(
      `\r\n\x1b[90m[process exited${code === null ? "" : ` with code ${code}`}]\x1b[0m\r\n`,
    );
    session.id = null;
    session.onPtyId?.(null);
  });

  // LOAD-BEARING StrictMode dedupe: this is the bail that actually guarantees a
  // single `pty_spawn`. Because we awaited the two `listen()` calls above, the
  // StrictMode throwaway cleanup has had a chance to run synchronously and set
  // `torndown=true` on a discarded session before we reach `invoke('pty_spawn')`
  // below. Do NOT remove this guard on the assumption that the effect-level
  // reuse/`spawnIssued` dedupe covers it — it does not in the jsdom test
  // harness (mutation-verified: removing this bail AND the reuse guard yields
  // two spawns; removing either one alone still yields one).
  if (session.torndown) {
    safeCall(session.unlistenOutput);
    safeCall(session.unlistenExit);
    return;
  }

  // Size the PTY from the current fit (fall back to xterm's current dims).
  const dims = fitAddon.proposeDimensions();
  const cols = dims?.cols ?? term.cols;
  const rows = dims?.rows ?? term.rows;

  // Pass the persistent terminal record id (when bound) so the backend can map
  // the live pty_id → terminal record id for exec-state (PRD-2.1). `terminalId`
  // (camelCase) maps to the Rust `terminal_id` param; a record-less terminal
  // passes `undefined` and the backend records no mapping. This does NOT alter
  // the StrictMode spawn-dedupe (it is a plain extra argument to the same call).
  const id = await invoke<number>("pty_spawn", { cwd, cols, rows, terminalId: recordId });

  if (session.torndown) {
    // Torn down before the spawn resolved: close the orphan, don't leak.
    void invoke("pty_close", { id }).catch(() => {});
    return;
  }
  session.id = id;
  // Surface the resolved live PTY id (auto-naming needs it to read terminal_info).
  session.onPtyId?.(id);

  // Replay output that arrived before the spawn resolved (while id was null, so
  // it could not be routed and was buffered). Only chunks tagged with OUR id are
  // ours; the rest belonged to other PTYs (multi-terminal) and were already
  // written by their own listeners — drop them. Push order preserves the stream
  // order, and the synchronous transition at `session.id = id` guarantees no
  // event is both buffered here and written live (no double-write).
  if (session.pendingOutput.length > 0) {
    const buffered = session.pendingOutput;
    session.pendingOutput = [];
    for (const payload of buffered) {
      if (payload.id === id) term.write(decode(payload.bytes));
    }
  }

  // Keystrokes → PTY stdin (encoded to bytes for the byte-oriented backend).
  session.disposables.push(
    term.onData((data) => {
      if (session.id === null) return;
      void invoke("pty_write", {
        id: session.id,
        data: Array.from(encoder.encode(data)),
      }).catch(() => {});
    }),
  );

  // xterm resize (driven by FitAddon/ResizeObserver) → inform the PTY.
  session.disposables.push(
    term.onResize(({ cols: c, rows: r }) => {
      if (session.id === null) return;
      void invoke("pty_resize", { id: session.id, cols: c, rows: r }).catch(() => {});
    }),
  );

  // Cover the spawn-after-font-load order: if a resync was requested while the
  // spawn was still in flight (id was null), the `onResize` handler above did
  // not exist yet, so the font-driven resize was dropped. Now that the id is
  // known and the handler is wired, push the terminal's current size to the PTY
  // once. Idempotent — a no-op if the size already matches the spawn-time dims.
  if (session.pendingResync) {
    session.pendingResync = false;
    resyncSession(session);
  }
}

/**
 * Push the terminal's CURRENT cols/rows to the PTY, out-of-band from xterm's
 * `onResize` event. If the spawn has resolved (`id` known) it fires an
 * idempotent `pty_resize`; otherwise it records that the next spawn must resync
 * (so the font-driven size is not lost when the resync races ahead of spawn).
 * No-op on a torn-down session, so it never resizes a closed PTY after unmount.
 */
function resyncSession(session: PtySession): void {
  if (session.torndown) return;
  if (session.id === null) {
    // Spawn still pending — defer; `start()` resyncs right after it resolves.
    session.pendingResync = true;
    return;
  }
  const { cols, rows } = session.term;
  void invoke("pty_resize", { id: session.id, cols, rows }).catch(() => {});
}

/** Tear a session down for real: dispose listeners and close the PTY. */
function teardown(session: PtySession): void {
  if (session.torndown) return;
  session.torndown = true;
  for (const d of session.disposables) d.dispose();
  session.disposables = [];
  // `unlisten` invokes the event plugin under the hood; swallow failures so a
  // teardown racing app/test shutdown never surfaces an unhandled rejection.
  safeCall(session.unlistenOutput);
  safeCall(session.unlistenExit);
  if (session.id !== null) {
    const id = session.id;
    session.id = null;
    void invoke("pty_close", { id }).catch(() => {});
  }
  session.onPtyId?.(null);
}

/** Invoke a possibly-undefined callback, swallowing any throw/rejection. */
function safeCall(fn: (() => void | Promise<unknown>) | undefined): void {
  if (!fn) return;
  try {
    const r = fn();
    if (r && typeof (r as Promise<unknown>).catch === "function") {
      (r as Promise<unknown>).catch(() => {});
    }
  } catch {
    // ignore (e.g. mocks already cleared in a test teardown)
  }
}
