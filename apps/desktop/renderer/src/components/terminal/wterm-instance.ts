import { WTerm } from "@wterm/dom";
import "@wterm/dom/css";

import { nyxBridge, type Unsubscribe } from "@/bridge";
import { loadGhosttyCore } from "./ghostty-core";
import { computeGrid, hasUsableBox, measureCell, type CellMetrics } from "./measure-cell";
import { applyWtermTheme, ensureTerminalFontLoaded } from "./wterm-theme";

/** Fallback grid used to spawn the PTY when the pane has no real box yet. */
const FALLBACK_COLS = 80;
const FALLBACK_ROWS = 24;

/**
 * Anti-jitter dead-band (px). If the viewport is within this many pixels of the bottom,
 * a stick is a NO-OP — no `scrollTop` write, no reflow. Small (a couple of px) so it
 * only absorbs the sub-pixel/rounding residue of "already at bottom", never a real
 * scroll-up. See {@link WtermInstance.scheduleScrollToBottom}.
 */
const JITTER_THRESHOLD_PX = 2;

/** Payload of the backend `pty://output` event (raw bytes as a JSON number[]). */
interface PtyOutputPayload {
  id: number;
  bytes: number[];
}

/** Payload of the backend `pty://exit` event. */
interface PtyExitPayload {
  id: number;
  code: number | null;
}

/** Options to build one live terminal instance. */
export interface WtermInstanceOptions {
  /** Persistent terminal RECORD id (SQLite `terminals.id`) this session binds to. */
  recordId: string;
  /** Working dir for the spawned shell (`undefined` → backend default). */
  cwd?: string;
  /**
   * Read-only DEAD HISTORY bytes (prior scrollback + separator) written into the
   * terminal ONCE, before the PTY output listener and before spawn, so restored
   * history sits ABOVE the live prompt. NEVER sent to the PTY.
   */
  deadHistory?: string;
  /** Notified with the live PTY id once the spawn resolves (and `null` on exit). */
  onPtyId?: (id: number | null) => void;
  /** Notified with the terminal's title as OSC 0/2 updates it. */
  onTitle?: (title: string) => void;
}

/**
 * One LIVE terminal, held IMPERATIVELY outside React — the unit the module-level
 * store keeps alive across navigation.
 *
 * It owns:
 *  - a persistent HOST `<div>` (`el`) that carries the wterm DOM projection. This
 *    node is DETACHED (removed from a pane container) on navigation and REATTACHED
 *    later — the state survives because it lives in the ghostty WASM core, not the
 *    DOM (which is only a rAF projection). We NEVER `destroy()` on detach.
 *  - a `WTerm` bound to the ghostty core, with `autoResize:false` (mandatory
 *    guard-rail: wterm's own observer would resize to 1×1 when hidden/detached and
 *    destroy the grid). Sizing is APP-DRIVEN via `observeResize` with a 0×0 guard.
 *  - the PTY session (spawn / keystrokes → `pty_write` / `pty://output` →
 *    `write` / resize → `pty_resize` / flow-control ack), spawned once on `init`
 *    and torn down only on `dispose` (real close), never on detach.
 */
export class WtermInstance {
  /** The persistent host node the wterm DOM lives in. Detached/reattached, never rebuilt. */
  readonly el: HTMLDivElement;
  readonly recordId: string;

  private term: WTerm | null = null;
  private ptyId: number | null = null;
  private cols = FALLBACK_COLS;
  private rows = FALLBACK_ROWS;
  private cell: CellMetrics | null = null;
  private disposed = false;
  private spawned = false;

  private readonly cwd?: string;
  private readonly deadHistory?: string;
  private onPtyId?: (id: number | null) => void;
  private onTitle?: (title: string) => void;

  private unlistenOutput?: Unsubscribe;
  private unlistenExit?: Unsubscribe;
  private resizeObserver: ResizeObserver | null = null;
  /**
   * Observes the wterm `.term-grid`'s child/text mutations — i.e. the REAL DOM render,
   * which wterm defers behind `_scheduleRender` (setTimeout(0) → rAF → `_doRender` →
   * `renderer.render`). The write-hook's own double-rAF can land a stick BEFORE that
   * deferred render adds the final rows, leaving a one-batch residue below the fold; the
   * grid mutation is the ground truth that the new rows are actually painted, so a stick
   * scheduled from HERE (when following) always lands after the last render. Debounced
   * through the SAME single {@link scheduleScrollToBottom} frame (rAF + cancel) as every
   * other caller, so it never races and never sticks per byte. Armed in
   * {@link observeResize}, disconnected in {@link stopObserving} / {@link dispose}.
   */
  private gridObserver: MutationObserver | null = null;
  /**
   * STICKY-BOTTOM intent (regression fix, review 01KWVGFK9DG3FKDWENQHC1K5Z7). Whether
   * the view should FOLLOW the bottom (a normal terminal) as opposed to staying where
   * the user scrolled up in the scrollback. A terminal starts following the bottom;
   * it is turned OFF only when the user scrolls up, and back ON when they return to the
   * bottom. It is the STANDING intent that survives a detach (where `el.scrollTop` is
   * reset to 0 by the DOM removal and can no longer be read), so a reattach re-glues to
   * the bottom instead of parking at the top.
   */
  private followBottom = true;
  /**
   * True from ATTACH until the deferred bottom-restore has actually run. A just-
   * reattached node has `scrollTop` reset to 0 by the DOM removal, and SEVERAL
   * reconciles fire during the post-attach layout settle — all while `scrollTop` is
   * still 0 and the deferred restore has not landed yet. Re-reading the scroll
   * position in that window would clobber the preserved {@link followBottom} intent to
   * `false`, so we SUPPRESS intent capture (and just act on the preserved intent) until
   * the restore lands and `scrollTop` is meaningful again.
   */
  private reattachSettling = false;
  /**
   * Handle of the single in-flight {@link scheduleScrollToBottom} frame (the OUTER
   * rAF, then re-pointed at the INNER rAF once the outer fires), or `null` when none
   * is pending. Kept so a re-schedule, a detach ({@link stopObserving}) or a
   * {@link dispose} can `cancelAnimationFrame` the PRIOR stick before it lands —
   * otherwise an obsolete stick from an earlier reconcile could re-glue the view to
   * the bottom AFTER the user (or a test) scrolled up, corrupting the `followBottom`
   * intent. At most ONE stick is ever in flight.
   */
  private scrollFrame: number | null = null;
  /**
   * The `el.scrollHeight` at the LAST completed stick (the anti-jitter memory, finding
   * 01KWVQ65SG8QCMKK88TGS94821). A stick only re-glues when the buffer has actually
   * GROWN taller since then — i.e. real NEW LINES appeared. An in-place echo (a
   * character typed onto the prompt row) and the blinking cursor mutate the grid DOM
   * but keep the SAME number of rows, so `scrollHeight` is unchanged → no stick → no
   * jitter. `-1` means "never sticked yet" (first stick always applies). It is NOT
   * consulted while a reattach is settling: a detach reset `scrollTop` to 0 with the
   * same `scrollHeight`, so the deferred bottom-restore must run on the preserved intent
   * regardless of growth.
   */
  private lastScrollHeight = -1;
  /** Output buffered before the spawn resolved (id still unknown → cannot route). */
  private pendingOutput: PtyOutputPayload[] = [];
  private readonly encoder = new TextEncoder();
  private initPromise: Promise<void> | null = null;
  /** Bound `scroll` handler on {@link el}, kept so {@link dispose} can detach it. */
  private readonly onViewportScroll: () => void;

  constructor(options: WtermInstanceOptions) {
    this.recordId = options.recordId;
    this.cwd = options.cwd;
    this.deadHistory = options.deadHistory;
    this.onPtyId = options.onPtyId;
    this.onTitle = options.onTitle;

    // The persistent host. `h-full w-full` so it fills whatever pane it is
    // reattached into; `applyWtermTheme` sets the palette + strips wterm's card.
    this.el = document.createElement("div");
    this.el.style.width = "100%";
    this.el.style.height = "100%";
    // `data-terminal-node` (NOT `data-terminal-id`) so the persistent node never
    // collides with the deck pane wrapper's `data-terminal-id` selector.
    this.el.dataset.terminalNode = this.recordId;

    // Track the standing follow-bottom intent from LIVE user scrolling. Without a
    // resize, `reconcileSize` never runs, so before this the intent could only change on
    // resize/detach — meaning the output-side sticky-bottom hook could not tell a
    // deliberate scroll-up (which must be preserved) from streaming drift (which must be
    // recollected). This listener re-reads the intent whenever the viewport scrolls, so a
    // user who scrolls up turns following OFF (the hook then leaves them alone) and a user
    // who returns to the bottom turns it back ON. Suppressed while a reattach is settling
    // (`scrollTop` is a meaningless post-detach 0 then). The programmatic re-glue
    // (`el.scrollTop = scrollHeight`) also fires this, but only ever re-affirms
    // `followBottom = true`, so it is self-consistent.
    this.onViewportScroll = () => {
      if (this.reattachSettling || !this.term || !this.cell) return;
      this.followBottom = this.isNearBottom(this.cell.rowHeight);
    };
    this.el.addEventListener("scroll", this.onViewportScroll, { passive: true });
  }

  /**
   * Build the ghostty core + WTerm and spawn the PTY. Idempotent — the store
   * calls it once when the instance is created. Never re-runs on reattach.
   */
  init(): Promise<void> {
    if (this.initPromise) return this.initPromise;
    this.initPromise = this._init();
    return this.initPromise;
  }

  private async _init(): Promise<void> {
    const core = await loadGhosttyCore();
    if (this.disposed) return;

    applyWtermTheme(this.el);
    await ensureTerminalFontLoaded();
    if (this.disposed) return;

    const term = new WTerm(this.el, {
      core,
      cols: this.cols,
      rows: this.rows,
      // MANDATORY guard-rail: disable wterm's built-in ResizeObserver. Its
      // formula resizes to 1×1 on a 0×0 box (a hidden/detached pane), which
      // destroys the grid. The app owns sizing (see `observeResize`) with a
      // 0×0 guard instead.
      autoResize: false,
      cursorBlink: true,
      onData: (data) => this.writeToPty(data),
      onTitle: (title) => this.onTitle?.(title),
    });
    await term.init();
    if (this.disposed) {
      term.destroy();
      return;
    }
    this.term = term;

    // Measure a cell now the DOM + font are ready, so the first app-driven
    // resize lands on a correct grid.
    this.cell = measureCell(this.el);

    // Reconcile NOW that the core exists. `attachTerminal` may have armed
    // `observeResize` (and fired its one-shot reconcile) BEFORE this async init
    // finished, when `this.term` was still null and the reconcile no-op'd. The
    // parent's box is stable, so the ResizeObserver would not fire again on its own
    // — we would stay frozen at the init grid (~24 rows). This first-fill reconcile
    // makes the terminal fill the pane immediately, with no window resize needed.
    // If not yet attached (no parent), `measureAvailableBox` returns null and the
    // 0×0 guard skips; the observer's reconcile on the next attach fills it.
    this.reconcileSize();

    await this.startPty();
  }

  /** Set up PTY listeners and spawn the shell exactly once. */
  private async startPty(): Promise<void> {
    if (this.spawned || this.disposed) return;
    this.spawned = true;

    // RESTART RE-HYDRATION (v2.2.5): before anything else, ask the sidecar for this
    // terminal's persisted VT SCREEN as an ANSI byte stream and write it into the
    // fresh terminal. Ghostty (the VT authority) reconstructs the exact grid — a
    // full-screen TUI on the alt-screen INCLUDED, not just the scrollback. The bytes
    // are self-resetting (reset + clear + repaint), so this is safe and idempotent,
    // and it is wterm-only (never sent to the PTY). When the sidecar has no snapshot
    // (a brand-new terminal, or a run with no prior screen) it returns empty bytes and
    // we fall back to the read-only dead-history text below.
    const rehydrated = await this.rehydrateScreen();
    if (this.disposed) return;

    // Dead history — before the output listener and the spawn — so the restored block
    // sits ABOVE the live prompt. Skipped when a full VT screen was already restored
    // above (the snapshot supersedes the flat scrollback text). wterm-only.
    if (!rehydrated && this.deadHistory && this.term) this.term.write(this.deadHistory);

    // Subscribe before spawning so no early output is dropped. Until the spawn
    // resolves we don't know our id, so buffer and replay after.
    this.unlistenOutput = await nyxBridge.subscribe<PtyOutputPayload>("pty://output", (p) => {
      if (this.disposed) return;
      if (this.ptyId === null) {
        this.pendingOutput.push(p);
        return;
      }
      if (p.id !== this.ptyId) return;
      this.writeOutput(p);
    });

    this.unlistenExit = await nyxBridge.subscribe<PtyExitPayload>("pty://exit", (p) => {
      if (this.disposed || this.ptyId === null || p.id !== this.ptyId) return;
      const code = p.code;
      this.term?.write(
        `\r\n\x1b[90m[process exited${code === null ? "" : ` with code ${code}`}]\x1b[0m\r\n`,
      );
      this.ptyId = null;
      this.onPtyId?.(null);
    });

    if (this.disposed) {
      this.safeUnlisten();
      return;
    }

    const id = await nyxBridge.invoke<number>("pty_spawn", {
      cwd: this.cwd,
      cols: this.cols,
      rows: this.rows,
      terminalId: this.recordId,
    });
    if (this.disposed) {
      void nyxBridge.invoke("pty_close", { id }).catch(() => {});
      return;
    }
    this.ptyId = id;
    this.onPtyId?.(id);

    // Replay buffered output that arrived before the id was known.
    if (this.pendingOutput.length > 0) {
      const buffered = this.pendingOutput;
      this.pendingOutput = [];
      for (const p of buffered) if (p.id === id) this.writeOutput(p);
    }
  }

  /**
   * Fetch this terminal's persisted VT screen from the sidecar (as an ANSI byte
   * stream) and write it into wterm, restoring the full screen — a TUI's alt-screen
   * included — at restart. Returns whether a non-empty screen was restored (so the
   * caller can skip the flat dead-history fallback). Best-effort: any error, a missing
   * snapshot, or an empty payload restores nothing and returns `false`, so a terminal
   * with no prior screen just spawns fresh.
   */
  private async rehydrateScreen(): Promise<boolean> {
    if (!this.term) return false;
    try {
      const { bytes } = await nyxBridge.invoke<{ bytes: number[] }>("terminal_screen_rehydration", {
        recordId: this.recordId,
      });
      if (this.disposed || !this.term) return false;
      if (!bytes || bytes.length === 0) return false;
      this.term.write(Uint8Array.from(bytes));
      return true;
    } catch {
      // Re-hydration is an optimization on top of a fresh spawn — never fatal.
      return false;
    }
  }

  /** Write a PTY chunk into wterm, then ACK its bytes (flow-control credit). */
  private writeOutput(p: PtyOutputPayload): void {
    if (!this.term) return;
    this.term.write(Uint8Array.from(p.bytes));
    // STICKY BOTTOM on OUTPUT (follow-up review 01KWVMF7PCWM5S289TDKMSGFR3). wterm
    // only re-follows on `write` if `_isScrolledToBottom()` was true BEFORE the write
    // (a <5px threshold, wterm.js:83,102,164). Once the view has drifted (chained short
    // commands leave the prompt ~48px below the fold), wterm decouples and the lag
    // accrues — the stream stops tracking the bottom. Re-glue on EVERY output batch
    // (only when following) via the SAME single-frame scheduler as resize (rAF + cancel):
    // each batch re-plans the one stick, so no race, and when the stream settles the last
    // stick lands after the last render → glued to the bottom, no resize nudge needed.
    if (this.followBottom) this.scheduleScrollToBottom();
    // wterm's `write` is synchronous into the core (the DOM paints on the next
    // rAF), so the bytes are consumed once this returns — ack immediately. On
    // shells without flow control `ackPtyOutput` is a no-op.
    nyxBridge.ackPtyOutput(p.id, p.bytes.length);
  }

  /** Keystrokes (wterm `onData`) → PTY stdin, encoded to bytes. */
  private writeToPty(data: string): void {
    if (this.ptyId === null) return;
    void nyxBridge
      .invoke("pty_write", { id: this.ptyId, data: Array.from(this.encoder.encode(data)) })
      .catch(() => {});
  }

  /** Give this terminal keyboard focus (its hidden input is a child of `el`). */
  focus(): void {
    this.term?.focus();
  }

  /**
   * Read the terminal's SCREEN as plain text (scrollback + visible rows), joined
   * by newlines. Reads straight from the ghostty core (the source of truth, not
   * the DOM projection) so it reflects the real buffer even while detached. Used
   * by the deck's `__nyxDeck` seam (tests / e2e) to prove an inactive or
   * navigated-away terminal still received its output.
   */
  readScreenText(): string {
    const core = this.term?.bridge;
    if (!core) return "";
    const lines: string[] = [];
    const cols = core.getCols();
    const scrollbackCount = core.getScrollbackCount();
    for (let offset = 0; offset < scrollbackCount; offset++) {
      const len = core.getScrollbackLineLen(offset);
      let line = "";
      for (let col = 0; col < len; col++) {
        const cp = core.getScrollbackCell(offset, col).char;
        line += cp === 0 ? " " : String.fromCodePoint(cp);
      }
      lines.push(line.replace(/\s+$/, ""));
    }
    const rows = core.getRows();
    for (let row = 0; row < rows; row++) {
      let line = "";
      for (let col = 0; col < cols; col++) {
        const cp = core.getCell(row, col).char;
        line += cp === 0 ? " " : String.fromCodePoint(cp);
      }
      lines.push(line.replace(/\s+$/, ""));
    }
    return lines.join("\n");
  }

  /** Feed input as if typed (routes through `onData` → the PTY). Test/e2e seam. */
  sendInput(data: string): void {
    this.writeToPty(data);
  }

  /**
   * Write raw bytes/text into the terminal's SCREEN (the same path a `pty://output`
   * chunk takes), WITHOUT touching the PTY. Used by the browser render test to
   * paint a deterministic frame with no live shell. In production the PTY output
   * listener is the sole writer; this is a public seam, not a new data path.
   */
  writeToScreen(data: string | Uint8Array): void {
    if (!this.term) return;
    this.term.write(data);
    // Same OUTPUT path as `writeOutput`: re-glue to the bottom on this batch when
    // following, so the seam reproduces production's sticky-bottom-on-output behaviour
    // (the multi-batch scroll-follow browser test exercises exactly this).
    if (this.followBottom) this.scheduleScrollToBottom();
  }

  /** Whether the wterm core is currently on the alt-screen (a full-screen TUI). */
  usingAltScreen(): boolean {
    return this.term?.bridge?.usingAltScreen() ?? false;
  }

  /**
   * The cursor's `{ row, col, visible }` straight from the ghostty core (the
   * source of truth, not the DOM projection), or `null` when there is no core.
   * A thin, inert read seam alongside {@link readScreenText}/{@link usingAltScreen}
   * — used by the validation gate to prove the cursor survives a detach/reattach
   * and a restart re-hydration. Nothing in production reads it.
   */
  readCursor(): { row: number; col: number; visible: boolean } | null {
    const cursor = this.term?.bridge?.getCursor();
    return cursor ? { row: cursor.row, col: cursor.col, visible: cursor.visible } : null;
  }

  /**
   * APP-DRIVEN sizing. Observe the PANE CONTAINER's box (the parent of `el`, e.g.
   * the `terminal.tsx` pane with `p-2.5`) and resize the terminal (+ PTY) to fit,
   * GATED on a non-zero box (the mandatory 0×0 guard). A hidden or freshly-
   * reattached pane reports 0×0 → we skip, so we never issue a destructive 1×1
   * resize.
   *
   * We observe the PARENT, not `this.el`: with `autoResize:false` wterm's
   * `_lockHeight()` FIXES `el.style.height` once at init (to `rows×rowHeight`) and
   * `resize()` never re-locks it, so `el`'s height never changes and an observer on
   * `el` would never fire on a vertical pane growth. The parent's content-box is the
   * height-authoritative, unlocked source, so observing it reacts to both width AND
   * height changes of the pane. Called by `attachTerminal` AFTER `appendChild`, so
   * `this.el.parentElement` is defined here. When there is no parent (detached), we
   * fall back to observing `el` so a later layout still fires a reconcile.
   *
   * Returns a disposer; the store re-arms it on each reattach (which replaces the
   * prior observer) and cuts it on detach via {@link stopObserving}, and it is
   * cleared on {@link dispose}. Idempotent — replaces any prior observer.
   */
  observeResize(): () => void {
    this.stopObserving();
    // A (re)attach reset `el.scrollTop` to 0 (the DOM removal on detach discards it),
    // so reconciles during the post-attach settle must trust the preserved
    // `followBottom` intent instead of the meaningless post-append scroll position.
    // The flag is cleared by the deferred bottom-restore, once `scrollTop` is real
    // again. See `reconcileSize` / `scheduleScrollToBottom`.
    this.reattachSettling = true;
    // Follow the stream on the REAL DOM render, not just on resize (see field doc).
    this.observeGridRender();
    if (typeof ResizeObserver === "undefined") return () => {};
    const observer = new ResizeObserver(() => this.reconcileSize());
    observer.observe(this.el.parentElement ?? this.el);
    this.resizeObserver = observer;
    // Reconcile immediately in case the box is already laid out at attach time.
    this.reconcileSize();
    return () => this.stopObserving();
  }

  /**
   * Arm the {@link gridObserver} on wterm's `.term-grid` (the node `renderer.render`
   * mutates) so that once wterm's deferred render actually paints NEW rows we re-glue to
   * the bottom — but ONLY while {@link followBottom} holds, so a scrolled-up user is
   * never yanked down. Debounced through the single {@link scheduleScrollToBottom} frame.
   *
   * The observer fires on EVERY grid mutation — including an in-place echo (a keystroke
   * drawn onto the prompt row) and the blinking cursor (which adds/removes a DOM node).
   * Those keep the SAME row count, so the stick it schedules is a content-driven one
   * (default `forceToBottom:false`): it re-glues ONLY when `el.scrollHeight` actually grew
   * since the last stick (real new lines), and does NOTHING for an echo/cursor — this is
   * the anti-jitter fix (finding 01KWVQ65SG8QCMKK88TGS94821). The observer stays valuable
   * as a "a render happened" timing signal that lands the follow AFTER wterm's deferred
   * paint, which the write-hook's own double-rAF can miss.
   *
   * Idempotent — disconnects any prior observer first. No-op when the grid or the API is
   * absent (jsdom without MutationObserver falls back to the write-hook double-rAF).
   */
  private observeGridRender(): void {
    this.gridObserver?.disconnect();
    this.gridObserver = null;
    if (typeof MutationObserver === "undefined") return;
    const grid = this.el.querySelector(".term-grid");
    if (!grid) return;
    const observer = new MutationObserver(() => {
      if (this.disposed || !this.term) return;
      // Content-driven: only re-glue on a REAL growth (new lines), never on an echo /
      // cursor mutation (the anti-jitter guard lives in `scheduleScrollToBottom`).
      if (this.followBottom) this.scheduleScrollToBottom();
    });
    observer.observe(grid, { childList: true, characterData: true, subtree: true });
    this.gridObserver = observer;
  }

  /**
   * Disconnect the app-driven ResizeObserver, if armed. Called by the store on
   * DETACH (so no stale observer lingers on the now-removed parent) and by
   * {@link dispose}, and used internally by {@link observeResize} as a clean
   * replace. IDEMPOTENT — a second call (or a call while none is armed) is a
   * harmless no-op, so a detach followed by a dispose never double-disconnects.
   */
  stopObserving(): void {
    // Stop following the DOM render too: the grid observer must not linger on the node
    // being detached (its callback would re-glue after the intent was captured). Armed
    // again by `observeResize` on the next attach. Disconnected here (the shared detach
    // path) and in `dispose`. Cheap idempotent no-op when nothing is armed.
    this.gridObserver?.disconnect();
    this.gridObserver = null;
    // Only when an observer is actually armed do we treat this as a genuine DETACH and
    // capture the sticky-bottom intent WHILE the node is still attached with a valid
    // scroll position — detach (`el.remove()`) is about to reset `scrollTop` to 0, so
    // this is the last moment we can read whether the user was following the bottom.
    // The preserved intent lets the next reattach re-glue instead of parking at top.
    // We SKIP the capture when no observer is armed, so `observeResize`'s own
    // stop-then-rearm (fired AFTER `appendChild`, when `scrollTop` is a meaningless 0)
    // never clobbers the intent preserved from the prior detach.
    if (this.resizeObserver) {
      if (this.term && this.cell) this.followBottom = this.isNearBottom(this.cell.rowHeight);
      this.resizeObserver.disconnect();
      this.resizeObserver = null;
      // A genuine DETACH: cancel any pending stick so an obsolete scheduled scroll can
      // never fire after the node is removed (or after a fresh scroll position lands),
      // re-gluing the view to the bottom and corrupting the captured `followBottom`
      // intent. The next attach re-arms its own reconcile → schedule as needed.
      this.cancelScheduledScrollToBottom();
    }
  }

  /**
   * The AVAILABLE content-box (width AND height) the terminal should fill, measured
   * from the PANE CONTAINER (`el`'s parent), NOT from `el`. `el`'s height is
   * height-LOCKED by wterm's `_lockHeight()` (see the class doc / `observeResize`),
   * so its rect height is frozen at `rows×rowHeight` and can never report the pane
   * growing taller — measuring it would pin rows to ~24 forever. The parent's
   * content-box is the unlocked, height-authoritative source: `clientWidth/Height`
   * (border-box minus scrollbars) minus the parent's own padding gives the box the
   * `100%×100%` node is meant to fill. Returns `null` when detached (no parent) so
   * the caller keeps the current size.
   */
  private measureAvailableBox(): { width: number; height: number } | null {
    const parent = this.el.parentElement;
    if (!parent) return null;
    const cs = typeof getComputedStyle === "function" ? getComputedStyle(parent) : null;
    const padX = cs ? (parseFloat(cs.paddingLeft) || 0) + (parseFloat(cs.paddingRight) || 0) : 0;
    const padY = cs ? (parseFloat(cs.paddingTop) || 0) + (parseFloat(cs.paddingBottom) || 0) : 0;
    return {
      width: Math.max(0, parent.clientWidth - padX),
      height: Math.max(0, parent.clientHeight - padY),
    };
  }

  /**
   * Resize the terminal to the PANE CONTAINER's current content-box, if it has one,
   * then RE-LOCK `el`'s height so the node actually grows to fill the pane. The 0×0
   * guard (`hasUsableBox`) is the load-bearing protection: without it a
   * `display:none` or pre-layout pane (0×0) would resize the grid to 1×1 and lose
   * the screen.
   *
   * Why re-lock: with `autoResize:false`, wterm's `resize()` updates the grid but
   * never touches `el.style.height` (only its one-shot `_lockHeight()` at init
   * does). Left alone, `el` stays frozen at the init height (~24 rows) even as the
   * grid grows, leaving a dead band. `el` has `padding:0` (via `applyWtermTheme`),
   * so `rows×cell.rowHeight` is the exact box that fills the pane.
   */
  private reconcileSize(): void {
    if (!this.term) return;
    const box = this.measureAvailableBox();
    if (!hasUsableBox(box)) return; // 0×0 guard — never resize a hidden/detached pane.

    // Re-measure the cell (the face may have loaded after init); keep the last
    // known metrics if the probe has no box yet.
    const cell = measureCell(this.el) ?? this.cell;
    if (!cell) return;
    this.cell = cell;

    // STICKY BOTTOM (regression fix, review 01KWVGFK9DG3FKDWENQHC1K5Z7). Resolve,
    // BEFORE we mutate anything, whether the view should follow the bottom. `el` is
    // the `.wterm` scroll container (overflow-y:auto via `.has-scrollback`); wterm
    // only re-scrolls to the bottom on the next write/resize if `_isScrolledToBottom`
    // was true BEFORE that call (wterm.js:102,114,164). Both `term.resize()` and the
    // height re-lock below change `clientHeight`/`scrollHeight` while leaving
    // `scrollTop` STALE, so the view silently drops out of "at bottom" and wterm
    // stops following the prompt.
    //
    // While a reattach is still settling `el.scrollTop` was reset to 0 by the detach,
    // so its geometry is meaningless — we act on the PRESERVED `followBottom` intent
    // (captured in `stopObserving` while the old position was still valid) and do NOT
    // re-read it here. Once settled (a live resize) we read the current scroll position
    // and update the standing intent, so a user who scrolled up turns following off and
    // a user who returned to the bottom turns it back on.
    if (!this.reattachSettling) this.followBottom = this.isNearBottom(cell.rowHeight);

    const { cols, rows } = computeGrid(box.width, box.height, cell);
    if (cols !== this.cols || rows !== this.rows) {
      this.cols = cols;
      this.rows = rows;
      this.term.resize(cols, rows);
      if (this.ptyId !== null) {
        void nyxBridge.invoke("pty_resize", { id: this.ptyId, cols, rows }).catch(() => {});
      }
    }

    // RE-LOCK the host height to the grid so the node fills the pane vertically.
    // wterm's `resize()` never re-locks (only its init-time `_lockHeight()` does),
    // so without this the element stays frozen at the initial ~24-row height and a
    // dead band appears below. `el` has `padding:0`, so no extra to add. Idempotent:
    // we only write when the value actually changes, so this never thrashes layout
    // or re-triggers the parent observer needlessly.
    const lockedHeight = `${rows * cell.rowHeight}px`;
    if (this.el.style.height !== lockedHeight) this.el.style.height = lockedHeight;

    // If the standing intent is to follow the bottom, re-glue it — but only then, so a
    // user who had scrolled up into the scrollback keeps their position (no forced
    // jump). This is deferred to after wterm's async render (`_scheduleRender` →
    // setTimeout+rAF) paints the new grid, otherwise `scrollHeight` still reflects the
    // old rows. `forceToBottom:true`: a resize/reattach reconcile must re-glue even
    // though it added no NEW LINES (a reattach zeroed `scrollTop`; a pane resize changed
    // geometry) — the anti-jitter growth guard would otherwise skip it. Guard (a) still
    // keeps it a no-op when already at the bottom.
    if (this.followBottom) {
      this.scheduleScrollToBottom(true);
    } else {
      // Not following (a preserved scroll-up intent on reattach, or a live scroll-up):
      // nothing to re-glue, so the reattach has "settled" — resume reading the real
      // scroll position on subsequent reconciles.
      this.reattachSettling = false;
    }
  }

  /**
   * Whether the `.wterm` scroll viewport (`el`) is currently glued to (or within a
   * row of) the bottom. Mirrors wterm's own private `_isScrolledToBottom` intent but
   * with a one-row tolerance (`rowHeight`, min 5px — wterm's own threshold) so a
   * near-bottom view counts as "following". When there is no scrollback the box does
   * not scroll (`scrollHeight ≈ clientHeight`), which reads as at-bottom — exactly
   * what we want on the initial fill. `!this.term` (detached/disposed) reads false.
   */
  private isNearBottom(rowHeight: number): boolean {
    if (!this.term) return false;
    const threshold = Math.max(5, rowHeight);
    return this.el.scrollHeight - this.el.scrollTop - this.el.clientHeight < threshold;
  }

  /**
   * Re-glue the viewport to the bottom AFTER wterm's async render has painted the new
   * grid. wterm renders via `_scheduleRender` (setTimeout(0) → rAF), so the fresh
   * `scrollHeight` is only correct a frame or two later — we wait a double rAF. Guards
   * against a detach/dispose landing between now and the callback. Falls back to a
   * direct apply when `requestAnimationFrame` is unavailable (jsdom unit env).
   *
   * At most ONE stick is ever in flight: we CANCEL any previously-scheduled frame
   * before arming a new one (and {@link stopObserving}/{@link dispose} cancel too), so
   * an obsolete stick planned by an earlier reconcile can never fire AFTER a scroll
   * position changed since — which was the root cause of re-gluing the view to the
   * bottom after the user scrolled up. Only the LAST scheduled stick applies.
   *
   * ANTI-JITTER (finding 01KWVQ65SG8QCMKK88TGS94821). The stick is NEVER allowed to move
   * the viewport for an in-place echo (a keystroke drawn onto the prompt row) or the
   * blinking cursor. Two guards, applied to EVERY content-driven stick (the write hooks
   * `writeOutput`/`writeToScreen` AND the grid MutationObserver, all of which fire on an
   * echo/cursor mutation, not just on new lines):
   *  - (a) NO-OP when already at the bottom (gap ≤ {@link JITTER_THRESHOLD_PX}) — zero
   *    `scrollTop` write, zero reflow. During wterm's transient render this unconditional
   *    write was exactly what bounced the view.
   *  - (b) Off the bottom but the buffer did NOT grow taller since the last stick
   *    ({@link lastScrollHeight}) → NOT new output, just an in-place echo / cursor (same
   *    row count, unchanged `scrollHeight`) → do NOTHING.
   * Only a REAL content growth (new lines) — the case the stream/nav follow relies on —
   * re-glues. `forceToBottom` (the resize / reattach reconcile) bypasses guard (b): a
   * detach reset `scrollTop` to 0 with the same `scrollHeight`, and a pane resize changes
   * geometry without adding lines, yet both must re-glue when following. `forceToBottom`
   * still honours guard (a), so it too never writes `scrollTop` when already at bottom.
   */
  private scheduleScrollToBottom(forceToBottom = false): void {
    const stick = () => {
      this.scrollFrame = null;
      // Snapshot the reattach state BEFORE clearing it: a genuine reattach restore rides
      // in on the reconcile with `forceToBottom`, so guard (b) is bypassed for it anyway,
      // but keep the settling read explicit for clarity.
      const settling = this.reattachSettling;
      // The reattach (if any) has now settled: `el.scrollTop` is meaningful again, so
      // subsequent reconciles may resume reading the real scroll position. Clear this
      // even on the guarded no-op below, so a detached/disposed instance never stays
      // stuck suppressing intent capture.
      this.reattachSettling = false;
      if (this.disposed || !this.term || !this.el.isConnected) return;
      const scrollHeight = this.el.scrollHeight;
      const gap = scrollHeight - this.el.scrollTop - this.el.clientHeight;
      // Record the height we evaluated against, so the NEXT stick can tell real growth
      // (new lines) from an in-place echo / cursor blink (same height). Done regardless of
      // whether we scroll, so an already-at-bottom frame still refreshes the baseline.
      const grew = scrollHeight > this.lastScrollHeight;
      this.lastScrollHeight = scrollHeight;
      // (a) Already glued (within the small threshold): touching `scrollTop` would only
      // reflow for nothing — and during a transient render it is what made the view
      // bounce. Leave it alone.
      if (gap <= JITTER_THRESHOLD_PX) return;
      // (b) Off the bottom but the buffer did NOT grow: an in-place echo / cursor mutation
      // (same row count) — do NOT scroll. Exempt: a resize/reattach reconcile
      // (`forceToBottom`) and the reattach restore (`settling`), where geometry changed or
      // `scrollTop` was zeroed with the same height.
      if (!grew && !forceToBottom && !settling) return;
      this.el.scrollTop = scrollHeight;
    };
    // Kill any prior in-flight frame first, so we never end up with two sticks racing
    // (the double-rAF used to leak an uncancellable handle on every reconcile).
    this.cancelScheduledScrollToBottom();
    if (typeof requestAnimationFrame !== "function") {
      stick();
      return;
    }
    // Re-point `scrollFrame` at the INNER rAF once the outer fires, so a cancel at any
    // moment kills whichever of the two frames is currently pending.
    this.scrollFrame = requestAnimationFrame(() => {
      this.scrollFrame = requestAnimationFrame(stick);
    });
  }

  /**
   * Cancel the single in-flight {@link scheduleScrollToBottom} frame, if any. Called on
   * re-schedule (so only the latest stick survives), on DETACH ({@link stopObserving})
   * and on {@link dispose} (so no obsolete stick re-glues the view after the intent /
   * observation has stopped). Idempotent — a call with nothing pending is a no-op.
   */
  private cancelScheduledScrollToBottom(): void {
    if (this.scrollFrame !== null && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(this.scrollFrame);
    }
    this.scrollFrame = null;
  }

  /**
   * REAL close: tear the PTY down, destroy the wterm (frees the WASM core + its
   * DOM), and remove the host node. Called ONLY when a terminal is truly closed —
   * NEVER on navigation/detach. After this the instance is inert.
   */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.stopObserving();
    // Belt-and-suspenders: `stopObserving` already cancels when an observer is armed,
    // but a dispose without a live observer (never attached) still must not leave a
    // pending stick behind. Idempotent no-op when nothing is scheduled.
    this.cancelScheduledScrollToBottom();
    this.safeUnlisten();
    if (this.ptyId !== null) {
      const id = this.ptyId;
      this.ptyId = null;
      void nyxBridge.invoke("pty_close", { id }).catch(() => {});
    }
    this.onPtyId?.(null);
    // Drop the live scroll listener (it was attached once, in the constructor, on the
    // persistent node — so it is removed once, here, not on every detach).
    this.el.removeEventListener("scroll", this.onViewportScroll);
    // `destroy()` clears the element's children + frees the core. Only here.
    this.term?.destroy();
    this.term = null;
    this.el.remove();
    this.onPtyId = undefined;
    this.onTitle = undefined;
  }

  private safeUnlisten(): void {
    try {
      this.unlistenOutput?.();
    } catch {
      /* ignore teardown race */
    }
    try {
      this.unlistenExit?.();
    } catch {
      /* ignore teardown race */
    }
    this.unlistenOutput = undefined;
    this.unlistenExit = undefined;
  }
}
