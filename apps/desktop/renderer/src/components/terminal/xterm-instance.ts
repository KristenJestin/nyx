import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal as XTerm, type ITerminalOptions } from "@xterm/xterm";

import { nyxBridge, type Unsubscribe } from "@/bridge";
import { isTerminalNavChord } from "@/components/sidebar/use-terminal-shortcuts";
import { copySelection, isCopyChord, isPasteChord, pasteFromClipboard } from "./terminal-clipboard";
import { reconcileTerminalGeometry } from "./terminal-geometry";
import {
  createTerminalLinkHandler,
  isExternalHttpUrl,
  openTerminalLink,
} from "./terminal-links";
import { ensureTerminalFontLoaded, resolveThemeFromCss } from "./xterm-theme";

/**
 * Default xterm options for a NORMAL terminal: native scroll, generous
 * scrollback, no custom bottom-anchoring, no blocks — the deliberate anti-flash
 * configuration (kept verbatim from the pre-store `<Terminal>` so the render is
 * byte-for-byte the same, only its OWNER moved out of React).
 */
const DEFAULT_OPTIONS: ITerminalOptions = {
  // Local echo stays OFF: the PTY echoes typed characters back.
  convertEol: false,
  cursorBlink: true,
  cursorStyle: "block",
  // Bundled Fira Code (see globals.css). Naming only fonts absent on
  // Linux/WebKitGTK made xterm measure a cell width that didn't match the glyph
  // → "t e s t" spacing; we bundle the face and gate the first fit on its load
  // (see ensureTerminalFontLoaded + `reconcile`) so measurement matches paint.
  fontFamily: '"Fira Code Variable", "Fira Code", ui-monospace, "Liberation Mono", monospace',
  fontSize: 14,
  letterSpacing: 0,
  scrollback: 10_000,
  // No `theme` here — derived from the design-system CSS palette in the
  // constructor (see resolveThemeFromCss) so the canvas matches the shell.
};

/**
 * The byte sequence written to the PTY when the user presses `Shift+Enter`.
 *
 * `Shift+Enter` must insert a NEWLINE in the running app (e.g. Claude Code / Ink)
 * instead of SUBMITTING. xterm has no notion of Shift+Enter — a plain `Enter`
 * sends `\r`, and Shift+Enter would send the same `\r` (read as "submit"). So we
 * intercept the chord and write `\x1b\r` (ESC + CR — read as Meta/Alt+Enter,
 * which Ink interprets as "insert newline"). Kept in ONE named constant so there
 * is a single place to flip if the chosen encoding turns out wrong.
 */
const SHIFT_ENTER_SEQUENCE = "\x1b\r";

/**
 * Whether `e` is the `Shift+Enter` chord — the physical Enter key (`e.code`,
 * covering the main `Enter` and numpad `NumpadEnter`) with Shift held and NO
 * other modifier. A plain `Enter` (no Shift) stays untouched so it still submits.
 */
function isShiftEnterChord(e: KeyboardEvent): boolean {
  const isEnterKey = e.code === "Enter" || e.code === "NumpadEnter" || e.key === "Enter";
  return isEnterKey && e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey;
}

/**
 * Whether `e` is a BARE Enter — no modifier at all. The relaunch chord of an exited pane, kept
 * apart from {@link isShiftEnterChord} so the two can never both claim the same keystroke.
 */
export function isPlainEnter(e: KeyboardEvent): boolean {
  const isEnterKey = e.code === "Enter" || e.code === "NumpadEnter" || e.key === "Enter";
  return isEnterKey && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey;
}

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

/**
 * WHAT IS LEFT of a terminal whose process is GONE — the state the pane's "Exited" bar reads.
 *
 * A pane whose shell exited used to be a DEAD END: the instance wrote its grey `[process exited]`
 * line, dropped its `ptyId`, and the `spawned` latch made a second shell impossible for the life
 * of the record. The only way back was to close the terminal and lose the pane. So the exit is
 * now a STATE the instance publishes rather than a terminus it falls into, and {@link
 * XtermInstance.respawn} is the way out of it — same pane, same record.
 */
export interface TerminalExit {
  /** The process exit code, or `null` when the backend could not read one. */
  readonly code: number | null;
  /** Whether a RELAUNCH was attempted from this state and failed (the bar says so, and stays). */
  readonly relaunchFailed?: boolean;
}

/** Options to build one live terminal instance. */
export interface XtermInstanceOptions {
  /** Persistent terminal RECORD id (SQLite `terminals.id`) this session binds to. */
  recordId: string;
  /** Working dir for the spawned shell (`undefined` → backend default). */
  cwd?: string;
  /** Extra xterm options merged over {@link DEFAULT_OPTIONS} at construction. */
  options?: ITerminalOptions;
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
  /**
   * Notified when the process EXITS (with the {@link TerminalExit}) and again with `null` the
   * moment a {@link XtermInstance.respawn} takes the pane back to a live shell. The store mirrors
   * it into its reactive slice, which is what the pane's bar renders from — so the bar survives a
   * detach/reattach and a route change exactly as the buffer does.
   */
  onExit?: (exit: TerminalExit | null) => void;
}

/**
 * Pick the cols/rows to spawn the PTY at — the fix for the "TUI wrapped to ~8
 * columns" bug (FEEDBACK #24).
 *
 * A terminal built while its element has no real box (0×0 — a hidden/detached
 * pane) would let `FitAddon.proposeDimensions()` size from the literal `100%`
 * (~8 cols); spawning there hard-wraps a resumed `claude`'s whole turn to 8
 * columns, frozen forever in the scrollback. So we trust the fit ONLY when the
 * element has a real box; otherwise we spawn at the sane FALLBACK (80×24) and let
 * the reconcile push the real size once the pane is shown. Pure + total →
 * unit-testable with fakes.
 */
export function chooseSpawnDimensions(
  element: { clientWidth: number; clientHeight: number } | null | undefined,
  proposeDimensions: () => { cols?: number; rows?: number } | undefined,
  fallback: { cols: number; rows: number },
): { cols: number; rows: number } {
  const hasBox = !!element && element.clientWidth > 0 && element.clientHeight > 0;
  const dims = hasBox ? proposeDimensions() : undefined;
  return { cols: dims?.cols ?? fallback.cols, rows: dims?.rows ?? fallback.rows };
}

/** How many MACROTASKS the spawn waits for a first non-zero box before giving up on measuring. */
const MEASURE_TICK_BUDGET = 3;

/**
 * The CEILING on the replay mute — five seconds, an order of magnitude above the longest parse a
 * dead-history or screen-rehydration replay has ever taken.
 *
 * Its only job is to make the worst case finite. The mute is normally released by xterm's own
 * parse-completion callback; a callback that never fired would leave the pane deaf to the keyboard
 * forever, with nothing on screen to say so. Generous enough that it never pre-empts a real parse,
 * and therefore never re-opens the phantom-keystroke window it exists beside.
 */
const REPLAY_MUTE_MAX_MS = 5_000;

/**
 * The default clock of {@link awaitMeasuredBox} — a MACROTASK, deliberately not `requestAnimationFrame`.
 *
 * Reading `clientWidth` FORCES a synchronous layout, so a pane that the browser has already been
 * told about measures correctly without waiting for a paint: one turn of the event loop is enough
 * for React's commit to be visible to layout. Waiting on frames instead would cost EVERY terminal
 * that can never be measured (a `display:none` pane, a detached node, jsdom) a real ~50ms before
 * its `pty_spawn` — a delay in front of the shell itself, paid by the majority for the minority.
 *
 * WHY THE BUDGET IS 3 AND NOT 1. "One turn is enough" is about the COMMIT that is already
 * scheduled: once React has flushed the layout that gives the pane its rectangle, the very next
 * macrotask sees it. It is NOT a promise that the commit has been flushed by the time
 * `awaitMeasuredBox` is called — a split mounts the leaf and its terminal in the SAME pass, and
 * the effect that spawns can run before the layout effect that sizes the box, or one microtask
 * queue drain ahead of it. Three turns is that slack, small enough to stay invisible (~0–3ms of
 * timer granularity, not ~50ms of frames) and bounded so a genuinely unmeasurable terminal pays
 * a couple of ticks and no more.
 */
function nextTick(cb: () => void): void {
  setTimeout(cb, 0);
}

/**
 * WAIT for `element` to have a real box — the other half of the spawn-size gate, and what makes a
 * pane BORN BY A SPLIT spawn at its own size.
 *
 * {@link chooseSpawnDimensions} is a pure verdict on ONE instant, and it is deliberately
 * pessimistic: no box ⇒ the 80×24 fallback. That is right for a pane that is genuinely hidden, and
 * wrong for a pane that simply has not been LAID OUT YET — which is exactly the split's case, since
 * the new leaf's rectangle is created in the same commit that mounts its terminal. Reading the box
 * in that instant yields 0×0, and the shell would start at 80×24 in a pane that is nothing like
 * 80×24 — a resumed agent's first turn is then hard-wrapped at the wrong width, frozen in the
 * scrollback forever (the very defect FEEDBACK #24 was about).
 *
 * So the spawn is DEFERRED — a few ticks at most — until the first measurable box:
 *  - already measurable ⇒ resolves `true` IMMEDIATELY, without yielding at all;
 *  - measurable within the budget ⇒ `true` (the split case);
 *  - never measurable (a `display:none` pane, a detached node, a headless DOM) ⇒ `false` after
 *    {@link MEASURE_TICK_BUDGET} macrotasks, and the caller keeps the sane fallback.
 *
 * THE CLOCK IS A MACROTASK, NOT A FRAME — and the names say so. An earlier revision called the
 * budget `frames` and its clock `nextFrame` while feeding both from {@link nextTick}, so a caller
 * writing `{ frames: 5 }` believed it was asking for 5 paints (~80ms) and was asking for 5 turns of
 * the event loop (~5ms). The option is `ticks` / `scheduleTick` for that reason: the unit in the
 * signature is the unit that is actually spent.
 *
 * The budget is BOUNDED and small on purpose: this sits in front of `pty_spawn`, so it must cost a
 * hidden terminal a couple of ticks, never a stall. `scheduleTick` is injectable so the wait is
 * testable without a layout engine.
 */
export function awaitMeasuredBox(
  element: { clientWidth: number; clientHeight: number } | null | undefined,
  options?: { ticks?: number; scheduleTick?: (cb: () => void) => void },
): Promise<boolean> {
  const measurable = () => !!element && element.clientWidth > 0 && element.clientHeight > 0;
  if (measurable()) return Promise.resolve(true);
  const budget = options?.ticks ?? MEASURE_TICK_BUDGET;
  const scheduleTick = options?.scheduleTick ?? nextTick;
  return new Promise<boolean>((resolve) => {
    // `left` counts the ticks STILL TO SPEND, and one is spent by the very `scheduleTick` below —
    // so it starts one short of the budget. Decrementing after the exhaustion test instead made
    // the wait cost `budget + 1` macrotasks, and `{ ticks: 0 }` ("do not wait") still yielded one.
    let left = budget - 1;
    const tick = () => {
      if (measurable()) {
        resolve(true);
        return;
      }
      if (left <= 0) {
        resolve(false);
        return;
      }
      left -= 1;
      scheduleTick(tick);
    };
    if (budget <= 0) {
      resolve(false);
      return;
    }
    scheduleTick(tick);
  });
}

/**
 * One LIVE xterm terminal, held IMPERATIVELY outside React — the unit the
 * module-level {@link ./terminal-store store} keeps alive across navigation.
 *
 * It owns:
 *  - a persistent HOST `<div>` (`el`) that xterm is `open()`-ed into ONCE. This
 *    node is DETACHED (removed from a pane container) on navigation and
 *    REATTACHED later; the SCREEN STATE survives because it lives in xterm's
 *    buffer, and the DOM renderer redraws from that buffer on reattach. We NEVER
 *    `dispose()` on detach.
 *  - the `Terminal` (default DOM renderer — NO WebGL, so there is no GL context to
 *    lose across a detach/reattach) + a `FitAddon`.
 *  - the PTY session (spawn / keystrokes → `pty_write` / `pty://output` →
 *    `term.write` / resize → `pty_resize` / flow-control ack), spawned once on
 *    construction and torn down only on {@link dispose} (real close), never on
 *    detach. Because the `pty://output` subscription lives HERE (module-level,
 *    not in a React effect), output keeps flowing into the buffer even while the
 *    pane is detached — that is the keep-alive guarantee.
 */
export class XtermInstance {
  /** The persistent host node xterm lives in. Detached/reattached, never rebuilt. */
  readonly el: HTMLDivElement;
  readonly recordId: string;

  private readonly term: XTerm;
  private readonly fitAddon: FitAddon;
  private ptyId: number | null = null;
  private disposed = false;
  private spawned = false;
  private pendingResync = false;

  private readonly cwd?: string;
  /**
   * Whether prior scrollback was written as read-only DEAD HISTORY at construction. It is the
   * restore authority when present, so the screen re-hydration below stands down: its repaint
   * starts by clearing the screen, which would wipe the history just written.
   */
  private readonly hasDeadHistory: boolean;
  private onPtyId?: (id: number | null) => void;
  private onTitle?: (title: string) => void;
  private onExit?: (exit: TerminalExit | null) => void;
  /** The exit this pane is SITTING IN, or `null` while a shell is live. See {@link TerminalExit}. */
  private exit: TerminalExit | null = null;
  /** Whether a {@link respawn} is in flight — the re-entrancy guard on the relaunch. */
  private respawning = false;
  /**
   * The link the mouse is currently OVER, or `null`. Maintained by both link paths (the regex
   * scanner and the OSC 8 handler) and read by the right-click menu, which has coordinates and no
   * way to ask xterm "what link is at this point".
   */
  private hovered: string | null = null;

  private unlistenOutput?: Unsubscribe;
  private unlistenExit?: Unsubscribe;
  private resizeObserver: ResizeObserver | null = null;
  /** Output buffered before the spawn resolved (id still unknown → cannot route). */
  private pendingOutput: PtyOutputPayload[] = [];
  private readonly encoder = new TextEncoder();
  /**
   * How many REPLAYS of recorded history are still being parsed by the emulator — dead history
   * and screen re-hydration. Non-zero means [`writeToPty`] must stay shut. See [`replay`].
   */
  private replaying = 0;
  /** The safety deadlines of the replays still in flight — cleared on [`dispose`]. */
  private readonly replayTimers = new Set<number>();

  constructor(options: XtermInstanceOptions) {
    this.recordId = options.recordId;
    this.cwd = options.cwd;
    this.hasDeadHistory = !!options.deadHistory;
    this.onPtyId = options.onPtyId;
    this.onTitle = options.onTitle;
    this.onExit = options.onExit;

    // The persistent host. `100% × 100%` so it fills whatever pane it is
    // reattached into; `data-terminal-node` so it never collides with the deck
    // pane wrapper's `data-terminal-id` selector.
    this.el = document.createElement("div");
    this.el.style.width = "100%";
    this.el.style.height = "100%";
    this.el.dataset.terminalNode = this.recordId;

    this.fitAddon = new FitAddon();
    this.term = new XTerm({
      ...DEFAULT_OPTIONS,
      // OSC 8 HYPERLINKS — the ones an emitting program marks up explicitly. xterm parses them
      // itself and asks this handler what to do; the regex scanner below covers the other kind
      // (a URL a shell merely printed). Both end in `openTerminalLink`.
      linkHandler: createTerminalLinkHandler((uri) => {
        this.hovered = uri;
      }),
      ...options.options,
    });
    this.term.loadAddon(this.fitAddon);
    // PLAIN URLs in the output. The addon scans the text and makes a match clickable — xterm
    // decorates a link with an underline + pointer cursor on hover by itself, so there is no
    // styling to add here. Its `hover`/`leave` feed the same "what is under the cursor" cell the
    // OSC 8 handler does, which is what the right-click menu reads.
    this.term.loadAddon(
      new WebLinksAddon(
        (_event, uri) => {
          openTerminalLink(uri);
        },
        {
          hover: (_event, text) => {
            this.hovered = isExternalHttpUrl(text) ? text : null;
          },
          leave: () => {
            this.hovered = null;
          },
        },
      ),
    );
    // Open into the (still detached) host ONCE. xterm builds its DOM (incl. the
    // hidden helper textarea) inside `el`; a later `appendChild(el)` moves the
    // whole subtree into the pane. Output written before/after open lands in the
    // buffer regardless, so a detached instance keeps accumulating output.
    this.term.open(this.el);

    // Derive the canvas colours from the live CSS palette (no hardcoded colour),
    // unless the caller supplied an explicit theme.
    if (!options.options?.theme) {
      const { background, foreground } = resolveThemeFromCss();
      this.term.options.theme = { ...this.term.options.theme, background, foreground };
    }

    this.term.onTitleChange((title) => this.onTitle?.(title));
    this.term.onData((data) => this.writeToPty(data));
    // xterm resize (driven by FitAddon/ResizeObserver) → inform the PTY.
    this.term.onResize(() => this.resyncSize());
    this.term.attachCustomKeyEventHandler((e) => this.handleKey(e));

    // Dead history — written before any PTY output can arrive — so the restored "previous
    // session" block sits ABOVE the live prompt. xterm-only, and REPLAYED (see `replay`): the
    // history is a recording, and a recording must not talk back to the live shell.
    if (options.deadHistory) this.replay(options.deadHistory);

    // Spawn the shell once. Errors are swallowed so a spawn failure never breaks
    // the store (the pane just shows an empty terminal).
    void this.startPty().catch(() => {});
  }

  /** The underlying xterm instance (for React-side affordances: scroll button, persist). */
  get terminal(): XTerm {
    return this.term;
  }

  /** The live PTY id, or `null` while the spawn is pending / after exit. */
  get pty(): number | null {
    return this.ptyId;
  }

  /** The exit this pane is sitting in, or `null` while a shell is live. */
  get exitState(): TerminalExit | null {
    return this.exit;
  }

  /**
   * The link the mouse is over right now, or `null`. Read by the right-click menu at the instant
   * it opens, which is the only moment the answer is meaningful.
   */
  get hoveredLink(): string | null {
    return this.hovered;
  }

  /** Set up the PTY listeners and spawn the shell exactly once. */
  private async startPty(): Promise<void> {
    if (this.spawned || this.disposed) return;
    this.spawned = true;

    // Subscribe BEFORE spawning so no early output is dropped. Until the spawn
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
      this.term.write(
        `\r\n\x1b[90m[process exited${code === null ? "" : ` with code ${code}`}]\x1b[0m\r\n`,
      );
      this.ptyId = null;
      // PUBLISH the exit rather than merely fall silent: the pane draws its "Exited · code N"
      // bar from this, and `respawn` is the way back out. The grey line above stays where it is —
      // it is scrollback, and a relaunched shell prints BELOW it, so the pane keeps its history.
      this.exit = { code };
      this.onPtyId?.(null);
      this.onExit?.(this.exit);
    });

    if (this.disposed) {
      this.safeUnlisten();
      return;
    }

    await this.spawnShell({ rehydrate: true });
  }

  /**
   * RELAUNCH a shell in THIS instance, after its process exited — the way out of the dead end.
   *
   * Same instance, same host node, same RECORD: the `pty_spawn` carries `terminalId`, so the
   * sidecar opens the new shell against the very same `terminals` row. That is what re-arms
   * everything keyed on the record — a parked `claude --resume` drains at this spawn exactly as it
   * would at the first one, with nothing here knowing about resumes.
   *
   * The buffer is NOT cleared and the screen is NOT re-hydrated: what the dead shell printed is
   * this pane's history, the grey `[process exited]` line is its epitaph, and the new shell prints
   * under both. (Re-hydration would begin with a screen clear and erase them.)
   *
   * Returns whether a shell is live afterwards. A failed relaunch RESTORES the bar — flagged
   * `relaunchFailed`, so the pane says so instead of silently offering the same dead button.
   */
  async respawn(): Promise<boolean> {
    if (this.disposed || this.respawning) return false;
    if (this.ptyId !== null) return false; // already live — nothing to relaunch.
    const previous = this.exit;
    this.respawning = true;
    // Clear the bar AT THE CLICK, not one round-trip later: the spawn can take a moment, and a
    // button that stays under the cursor invites a second press.
    this.exit = null;
    this.onExit?.(null);
    // Whatever was buffered belongs to the dead session's id and can never match the new one.
    this.pendingOutput = [];
    try {
      await this.spawnShell({ rehydrate: false });
    } catch {
      // Swallowed exactly like the construction spawn: the failure is REPORTED below.
    } finally {
      this.respawning = false;
    }
    if (this.ptyId !== null || this.disposed) return this.ptyId !== null;
    // The relaunch did not land. Put the pane back where it was, and say so.
    this.exit = { code: previous?.code ?? null, relaunchFailed: true };
    this.onExit?.(this.exit);
    return false;
  }

  /**
   * Measure, size and SPAWN the shell — the half of the boot that a {@link respawn} repeats. The
   * listeners are armed once, by {@link startPty}, and outlive every shell this pane hosts.
   */
  private async spawnShell({ rehydrate }: { rehydrate: boolean }): Promise<void> {
    // Give a pane that is being laid out RIGHT NOW its few macrotasks before judging its box: a leaf
    // born from a split is mounted in the same commit as its rectangle, so the box is 0×0 at this
    // instant and would send the shell to the 80×24 fallback in a pane that is nothing like it
    // (see `awaitMeasuredBox`). A genuinely hidden pane simply spends the budget and falls back,
    // exactly as before.
    await awaitMeasuredBox(this.term.element);
    if (this.disposed) {
      this.safeUnlisten();
      return;
    }

    // Size the PTY from the current fit — but ONLY when the pane has a real box
    // (see `chooseSpawnDimensions`), else spawn at the sane 80×24 fallback.
    const { cols, rows } = chooseSpawnDimensions(
      this.term.element,
      () => this.fitAddon.proposeDimensions(),
      { cols: this.term.cols, rows: this.term.rows },
    );

    // RE-HYDRATE the screen BEFORE the spawn, because `pty_spawn` on a record whose shell is
    // already running ATTACHES to it rather than starting a new one — an agent opened it over
    // MCP, or it survived a renderer reload — and that shell has been printing into a pane that
    // did not exist. The backend streams a terminal's output only once a view is attached, so
    // doing it here means the repaint can neither duplicate nor be duplicated by the live stream
    // that starts at the attach. A record with nothing to restore answers with no bytes.
    //
    // SKIPPED on a relaunch: the pane is already showing this record's whole life, and the
    // repaint's leading screen-clear would wipe it (see `respawn`).
    if (rehydrate) await this.rehydrateScreen();

    const id = await nyxBridge.invoke<number>("pty_spawn", {
      cwd: this.cwd,
      cols,
      rows,
      terminalId: this.recordId,
    });
    if (this.disposed) {
      void nyxBridge.invoke("pty_close", { id }).catch(() => {});
      return;
    }
    this.ptyId = id;
    this.onPtyId?.(id);

    // Replay output that arrived before the id was known (only OUR chunks).
    if (this.pendingOutput.length > 0) {
      const buffered = this.pendingOutput;
      this.pendingOutput = [];
      for (const p of buffered) if (p.id === id) this.writeOutput(p);
    }

    // If a resize was requested while the spawn was in flight, honour it now.
    if (this.pendingResync) {
      this.pendingResync = false;
      this.resyncSize();
    }
  }

  /**
   * Ask the backend for this record's screen re-hydration bytes and write them into xterm, so a
   * pane taking over a running shell shows what that shell is on instead of opening blank. The
   * stream is a self-resetting full repaint (alt-screen included), never sent to the PTY.
   *
   * Skipped when dead history was restored: that is already this pane's restore authority, and
   * the repaint's leading screen-clear would erase it. Best-effort — a backend that has nothing
   * to restore, or fails to answer, leaves the fresh terminal as it was.
   */
  private async rehydrateScreen(): Promise<void> {
    if (this.hasDeadHistory) return;
    try {
      const { bytes } = await nyxBridge.invoke<{ bytes: number[] }>("terminal_screen_rehydration", {
        recordId: this.recordId,
      });
      if (!this.disposed && bytes && bytes.length > 0) this.replay(Uint8Array.from(bytes));
    } catch {
      // Nothing to re-hydrate is the normal case for a brand-new terminal, and a failure here
      // must never cost the pane its shell.
    }
  }

  /** Write a PTY chunk into xterm, then ACK its bytes from the completion callback. */
  private writeOutput(p: PtyOutputPayload): void {
    const len = p.bytes.length;
    this.term.write(Uint8Array.from(p.bytes), () => {
      // Ack from xterm's completion callback (flow-control credit): the host only
      // counts a chunk consumed once xterm has parsed it. No-op on shells without
      // flow control.
      nyxBridge.ackPtyOutput(p.id, len);
    });
  }

  /**
   * Write RECORDED bytes into the emulator with the PTY answer-back MUTED for as long as the
   * parse lasts.
   *
   * A recording is not a conversation. Dead history and the screen re-hydration are bytes some
   * PAST shell printed, and among them can be a terminal QUERY — a shell probes its terminal at
   * startup with `ESC[6n` (DSR cursor position), and on Windows that probe is the first thing in
   * a view-less terminal's scrollback. xterm answers a query it parses, through the very
   * `onData` that carries the user's keystrokes, so replaying that history sends `ESC[1;1R` to
   * the LIVE shell — which is long past its own probe and sitting at a prompt. readline eats the
   * CSI and leaves the bare `R` on the input line, so the user's next command becomes `Rcd …`
   * and dies as `command not found`. Observed in the real app on a Système terminal (gate CDP
   * « Scope Système »); intermittent because `term.write` parses a SMALL history synchronously
   * (before the spawn publishes `ptyId`, which mutes the write by itself) and a LARGE one across
   * several turns of the event loop, straddling the spawn.
   *
   * Hence a counter rather than a boolean, released from xterm's own parse-completion callback:
   * the mute must outlive the `write` call, and replays can overlap. A keystroke typed during
   * the replay window is dropped with it — a lost keystroke on a pane that is still painting its
   * history is a far smaller harm than a phantom one injected into the shell.
   */
  private replay(bytes: string | Uint8Array): void {
    this.replaying += 1;
    // THE MUTE IS BOUNDED, by a deadline as well as by the callback (review
    // 01M15QP3HVYV9Q729ZYTNJB8T2, finding 01M15RH653B33VYMRKP7Y2XSR2). The release above is
    // xterm's parse-completion callback and nothing else; if it ever failed to fire, the pane
    // would be permanently DEAF to the keyboard with no signal of any kind — a worse failure than
    // the one this whole mute exists to prevent. There is no known way to make it not fire, and
    // that is exactly why the ceiling is cheap: it costs one timer, and it turns an unbounded,
    // silent, un-diagnosable failure into the isolated phantom keystroke we already accept.
    let released = false;
    const release = () => {
      if (released) return;
      released = true;
      this.replaying = Math.max(0, this.replaying - 1);
    };
    const deadline = window.setTimeout(() => {
      this.replayTimers.delete(deadline);
      release();
    }, REPLAY_MUTE_MAX_MS);
    this.replayTimers.add(deadline);
    this.term.write(bytes, () => {
      window.clearTimeout(deadline);
      this.replayTimers.delete(deadline);
      release();
    });
  }

  /** Keystrokes (xterm `onData`) → PTY stdin, encoded to bytes. */
  private writeToPty(data: string): void {
    // A replay in flight owns the parser: whatever the emulator emits right now is its ANSWER to
    // the recording, not the user's input. See [`replay`].
    if (this.replaying > 0) return;
    if (this.ptyId === null) return;
    void nyxBridge
      .invoke("pty_write", { id: this.ptyId, data: Array.from(this.encoder.encode(data)) })
      .catch(() => {});
  }

  /**
   * Custom key handling on the xterm textarea. Copy/paste chords (Ctrl+Shift+C/V)
   * and Shift+Enter are intercepted here; NAV chords pass THROUGH (return false)
   * so they bubble to the global hotkey listener; every other key is handled by
   * xterm. The return value decides whether xterm processes the key.
   */
  private handleKey(e: KeyboardEvent): boolean {
    if (e.type === "keydown") {
      // A pane sitting in an EXIT reads Enter as "relaunch" — the keyboard twin of the bar's
      // button, so a user who never leaves the keyboard is not sent hunting for the mouse. Checked
      // FIRST: there is no shell to send anything to, so nothing below could apply anyway.
      if (this.exit !== null && isPlainEnter(e)) {
        e.preventDefault();
        void this.respawn();
        return false;
      }
      if (isCopyChord(e)) {
        // preventDefault so Chromium's native Ctrl+Shift+C can't fire alongside.
        e.preventDefault();
        void copySelection(this.term);
        return false; // handled — do not also send ^C to the PTY.
      }
      if (isPasteChord(e)) {
        // preventDefault is LOAD-BEARING: `return false` stops xterm processing but
        // does NOT cancel the DOM event, so Chromium's native paste would insert a
        // second copy. Cancelling keeps `term.paste()` the single insertion path.
        e.preventDefault();
        void pasteFromClipboard(this.term);
        return false; // handled — do not send the raw keystroke.
      }
      if (isShiftEnterChord(e)) {
        // Shift+Enter must INSERT A NEWLINE, not submit. `input()` funnels through
        // the same `onData` → `pty_write` path; preventDefault + `return false`
        // suppress xterm's default `\r`, so there is exactly ONE write.
        e.preventDefault();
        this.term.input(SHIFT_ENTER_SEQUENCE);
        return false; // handled — do not also send the default `\r`.
      }
    }
    // Yield nav chords (return false) so they bubble; let xterm handle the rest.
    return !isTerminalNavChord(e);
  }

  /**
   * ATTACH the persistent node into a pane container and (re)arm the app-driven
   * geometry pipeline. If the node was previously attached elsewhere,
   * `appendChild` MOVES it (never cloned/rebuilt), so the xterm DOM — and the
   * focusable textarea that is its child — survives intact. The DOM renderer
   * redraws from the buffer on the reconcile below.
   */
  attach(container: HTMLElement): void {
    if (this.disposed) return;
    if (this.el.parentElement !== container) container.appendChild(this.el);
    this.observeResize();
  }

  /**
   * DETACH the node from its pane — the navigation path. Stop the resize observer
   * (so none lingers on the now-removed parent) and remove the node. We DO NOT
   * dispose: the xterm instance + its buffer stay in the store, and a later
   * {@link attach} reattaches the SAME node and redraws losslessly.
   */
  detach(): void {
    if (this.disposed) return;
    this.stopObserving();
    this.el.remove();
  }

  /**
   * APP-DRIVEN sizing. Observe the PANE CONTAINER's box (the parent of `el`) and
   * reconcile geometry (fit → PTY resize/SIGWINCH → repaint → scroll-to-bottom),
   * gated on a non-zero box (the 0×0 guard inside `reconcileTerminalGeometry`).
   * The first fit is gated on the bundled monospace face loading, so xterm
   * measures the real glyph (the "t e s t" spacing fix). Idempotent — replaces any
   * prior observer.
   */
  observeResize(): void {
    this.stopObserving();
    // A bare fit (no repaint) for the first frame so the element has dims for the
    // PTY spawn; the full reconcile follows once the face has loaded.
    this.safeFit();
    const fontFamily = (DEFAULT_OPTIONS.fontFamily as string) ?? "monospace";
    const fontSize = (DEFAULT_OPTIONS.fontSize as number) ?? 14;
    void ensureTerminalFontLoaded(fontFamily, fontSize).then(() => {
      if (!this.disposed) this.reconcile();
    });

    if (typeof ResizeObserver === "undefined") {
      this.reconcile();
      return;
    }
    const observer = new ResizeObserver(() => this.reconcile());
    observer.observe(this.el.parentElement ?? this.el);
    this.resizeObserver = observer;
    // Reconcile immediately in case the box is already laid out at attach time.
    this.reconcile();
  }

  /** Disconnect the app-driven ResizeObserver, if armed. Idempotent. */
  stopObserving(): void {
    this.resizeObserver?.disconnect();
    this.resizeObserver = null;
  }

  /** A bare fit (no repaint), guarded by the 0×0 box + a try/catch for jsdom. */
  private safeFit(): void {
    const el = this.el;
    try {
      if (el.clientWidth > 0 && el.clientHeight > 0) this.fitAddon.fit();
    } catch {
      // ignore transient fit failures (no layout / detached).
    }
  }

  /**
   * Run the ordered geometry pipeline (fit → resyncSize → repaint → scroll). The
   * WebGL-atlas step is a no-op here: this terminal uses xterm's DOM renderer, so
   * there is no glyph atlas to rebuild. Gated on a non-zero box inside
   * {@link reconcileTerminalGeometry}.
   */
  private reconcile(): boolean {
    return reconcileTerminalGeometry({
      instance: this.term,
      element: this.el,
      fitAddon: this.fitAddon,
      resyncSize: () => this.resyncSize(),
      // No WebGL renderer on this path → nothing to rebuild.
      clearWebglAtlas: () => {},
    });
  }

  /**
   * Push the terminal's CURRENT cols/rows to the PTY, out-of-band from xterm's
   * `onResize` event. If the spawn has resolved it fires an idempotent
   * `pty_resize`; otherwise it records that the next spawn must resync.
   */
  private resyncSize(): void {
    if (this.disposed) return;
    if (this.ptyId === null) {
      this.pendingResync = true;
      return;
    }
    const { cols, rows } = this.term;
    void nyxBridge.invoke("pty_resize", { id: this.ptyId, cols, rows }).catch(() => {});
  }

  /** Give this terminal keyboard focus (its hidden textarea is a child of `el`). */
  focus(): void {
    if (this.disposed) return;
    try {
      this.term.focus();
    } catch {
      // focus is best-effort: a detached/closing instance must never throw.
    }
  }

  /**
   * Read the whole xterm buffer (screen + scrollback) as one newline-joined
   * string. Used by the deck's `__nyxDeck` test/e2e seam to prove an inactive or
   * navigated-away terminal still received its output. Reads from the buffer (the
   * source of truth), so it works even while the pane is detached.
   */
  readScreenText(): string {
    const buf = this.term.buffer.active;
    let out = "";
    for (let i = 0; i < buf.length; i++) {
      const line = buf.getLine(i);
      if (line) out += line.translateToString(true) + "\n";
    }
    return out;
  }

  /** Feed input as if typed (routes through `onData` → the PTY). Test/e2e seam. */
  sendInput(data: string): void {
    if (this.disposed) return;
    this.term.input(data, true);
  }

  /**
   * REAL close: tear the PTY down, dispose xterm (frees its DOM), and remove the
   * host node. Called ONLY when a terminal is truly closed — NEVER on
   * navigation/detach. After this the instance is inert.
   */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.stopObserving();
    this.safeUnlisten();
    // The replay mute dies with the instance: a counter left non-zero would follow a re-created
    // pane through nothing, but a pending deadline firing into a disposed instance is noise, and
    // the invariant "no replay is in flight once disposed" is the one worth stating.
    for (const timer of this.replayTimers) window.clearTimeout(timer);
    this.replayTimers.clear();
    this.replaying = 0;
    if (this.ptyId !== null) {
      const id = this.ptyId;
      this.ptyId = null;
      void nyxBridge.invoke("pty_close", { id }).catch(() => {});
    }
    this.onPtyId?.(null);
    // A CLOSED terminal is not a finished one waiting to be relaunched: drop the exit so nothing
    // downstream keeps offering a "Relaunch" for a record that is on its way out.
    this.exit = null;
    this.onExit?.(null);
    try {
      this.term.dispose();
    } catch {
      // ignore teardown races.
    }
    this.el.remove();
    this.onPtyId = undefined;
    this.onTitle = undefined;
    this.onExit = undefined;
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
