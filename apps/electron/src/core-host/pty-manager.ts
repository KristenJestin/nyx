/**
 * The core-host's keyed PTY MANAGER — the Electron mirror of the Tauri
 * `PtyManager`. It owns every live `NyxPty`, keyed by its live `ptyId`, and is the
 * single place the host enforces the LOSSLESS FLOW CONTROL the PRD freezes
 * (annexe §E):
 *
 *   - **64 KiB chunking** — a coalesced flood chunk from the Rust pump is split into
 *     pieces no larger than `CHUNK_BYTES` before it crosses the IPC, so one event
 *     never carries a multi-MiB payload and the backlog is bounded to
 *     `HIGH_WATER + CHUNK_BYTES`.
 *   - **per-terminal credits** — the manager tracks each terminal's UNACKED output
 *     bytes. Crossing `HIGH_WATER` pauses that terminal's Rust reader
 *     (`setPaused(true)` → OS backpressure to the child); the renderer credits bytes
 *     back via `ack()` after `xterm.write` completes, and the reader resumes
 *     (`setPaused(false)`) once the backlog drops below `LOW_WATER`. Nothing is ever
 *     dropped — the data sits in the kernel PTY buffer while paused.
 *
 * The OSC 7 (cwd) and OSC 133 (exec-state) callbacks are forwarded straight to the
 * EventSink, keyed by both the live `ptyId` and the persistent `terminalId` the
 * spawn carried. The DB half of exec-state is phase 5; here we only relay.
 *
 * This module runs ONLY in the dedicated Node-pure host (it touches the `.node`),
 * never in main/renderer.
 */
import { CHUNK_BYTES, HIGH_WATER, LOW_WATER } from "../shared/host-protocol";
import type { ElectronEventSink } from "./event-sink";
import type { ExecStateEvent, NyxCoreInstance, NyxNapi, NyxPtyInstance } from "./napi";
import type { ExecStatePersister } from "./exec-state";
import type { ResumeParks } from "./resume-parks";

/** One `(terminalId, busy)` reading the busy-state poll loop consumes. */
export interface BusyReading {
  terminalId: string;
  busy: boolean;
}

/** A sink the manager notifies when a record-backed PTY exits (so the busy-state
 * tracker forgets it). Set by the lifecycle once the poller exists. */
export type PtyExitObserver = (terminalId: string) => void;

/** Per-terminal flow-control + identity state. */
interface PtyEntry {
  pty: NyxPtyInstance;
  /** The persistent terminal record id this PTY binds to (for exec-state), or null. */
  terminalId: string | null;
  /** Output bytes emitted to main but NOT yet acknowledged by the renderer. */
  unacked: number;
  /** Whether the Rust reader is currently paused (so we only toggle on transitions). */
  paused: boolean;
}

export class PtyManager {
  private readonly entries = new Map<number, PtyEntry>();
  /** Notified when a record-backed PTY exits (busy-state tracker cleanup). */
  private exitObserver: PtyExitObserver | null = null;
  /**
   * MCP `create_terminal` OPENING-COMMAND parks (PRD-5 review #68), keyed by terminal
   * RECORD id. The MCP `create_terminal` tool parks a `command` here; when that
   * terminal's PTY next spawns (the renderer mounts it on `terminals://changed`) the park
   * is DRAINED (one-shot) and the command is typed into the fresh shell — the SAME
   * mechanism as an agent resume, but for an agent-opened interactive terminal. A bare
   * `create_terminal` parks nothing.
   */
  private readonly openingParks = new Map<string, string>();

  constructor(
    private readonly napi: NyxNapi,
    private readonly events: ElectronEventSink,
    /** Persists + emits OSC 133 exec-state transitions (PRD-5 task #1). */
    private readonly execState: ExecStatePersister,
    /** Boot agent-session resume parks, injected at first respawn (PRD-5 task #5). */
    private readonly resumeParks: ResumeParks,
    /** The shared-core handle — holds the synchronous record↔live-PTY liveness registry the
     *  MCP dispatcher reads; the manager RETRACTS a record from it on PTY exit (Finding C). */
    private readonly core: NyxCoreInstance,
  ) {}

  /** Register the exit observer (the busy-state poller's `forget`). */
  onPtyExit(observer: PtyExitObserver): void {
    this.exitObserver = observer;
  }

  /**
   * A snapshot of `(terminalId, busy)` for every record-backed live PTY — the input to
   * the busy-state poll loop (mirrors `scan_busy_once`). A PTY whose busy bit is
   * undeterminable (`null`) is reported idle. Record-less PTYs (no durable id) have no
   * sidebar dot and are skipped.
   */
  busySnapshot(): BusyReading[] {
    const out: BusyReading[] = [];
    for (const e of this.entries.values()) {
      if (e.terminalId === null) continue;
      out.push({ terminalId: e.terminalId, busy: e.pty.busy() === true });
    }
    return out;
  }

  /**
   * Spawn a shell PTY and start streaming it. Returns the live `ptyId` the renderer
   * keys its xterm by. Output is chunked to ≤64 KiB and flow-controlled per the
   * credit loop; cwd/exec-state are forwarded to the sink.
   */
  spawn(opts: { cols: number; rows: number; cwd?: string; terminalId?: string }): number {
    const terminalId = opts.terminalId ?? null;

    // The napi constructor needs the callbacks up front, but the entry (which they
    // close over for accounting) is keyed by the id the constructor RETURNS. We
    // resolve the id immediately after construction and stash it in a holder the
    // callbacks read — the first output cannot arrive before the constructor
    // returns (the Rust pump thread delivers on the Node loop, i.e. after this
    // synchronous frame), so the holder is always populated by then.
    const holder: { id: number } = { id: -1 };

    const pty = new this.napi.NyxPty(
      opts.cols,
      opts.rows,
      opts.cwd ?? null,
      terminalId,
      // onData — the hot path: chunk + account + emit.
      (...args: unknown[]) => {
        const bytes = args[args.length - 1];
        if (Buffer.isBuffer(bytes) && bytes.length > 0) this.onData(holder.id, bytes);
      },
      // onExit
      (...args: unknown[]) => {
        const code = args[args.length - 1];
        this.onExit(holder.id, typeof code === "number" ? code : null);
      },
      // onCwd (OSC 7)
      (...args: unknown[]) => {
        const cwd = args[args.length - 1];
        if (typeof cwd === "string" && cwd.length > 0) this.onCwd(holder.id, cwd);
      },
      // onExecState (OSC 133)
      (...args: unknown[]) => {
        const ev = args[args.length - 1] as ExecStateEvent | undefined;
        if (ev && typeof ev.state === "string") this.onExecState(holder.id, ev);
      },
    );

    const id = pty.id();
    holder.id = id;
    this.entries.set(id, { pty, terminalId, unacked: 0, paused: false });

    // AGENT-SESSION RESUME INJECTION (PRD-5 task #5): if the boot scan parked a
    // `claude --resume <id>` for this terminal RECORD, drain it (one-shot) and write it
    // into the freshly-spawned shell — exactly once, at the first opening, the SAME
    // write path as a keystroke. A failed write marks the session `resume_failed` so the
    // next launch won't retry. Mirrors the injection half of `register_terminal_pty`.
    if (terminalId) {
      const resume = this.resumeParks.take(terminalId);
      if (resume) {
        try {
          pty.write(resume.bytes);
        } catch {
          this.resumeParks.markFailed(resume.sessionId);
        }
      }
      // MCP OPENING-COMMAND injection (PRD-5 review #68): if an agent's `create_terminal`
      // parked a command for this record, type it into the fresh shell exactly once (the
      // SAME write path as a keystroke), then the terminal stays interactive. Drained
      // one-shot so a re-spawn does not re-run it.
      const opening = this.openingParks.get(terminalId);
      if (opening !== undefined) {
        this.openingParks.delete(terminalId);
        try {
          pty.write(Buffer.from(`${opening}\r`, "utf8"));
        } catch {
          /* best-effort: a failed opening-command injection is not fatal */
        }
      }
    }
    return id;
  }

  /** Park an MCP `create_terminal` OPENING command keyed by terminal RECORD id, to be
   * injected when that terminal's PTY next spawns (drained one-shot in {@link spawn}). */
  parkOpeningCommand(terminalId: string, command: string): void {
    this.openingParks.set(terminalId, command);
  }

  /**
   * Write bytes into the LIVE PTY of a terminal RECORD (the MCP `send_to_terminal` path),
   * resolving the record id → its live entry. Returns `true` when written, `false` when no
   * live PTY is registered for the record (it has not spawned yet, or already exited).
   */
  writeToTerminal(terminalId: string, data: Buffer): boolean {
    for (const e of this.entries.values()) {
      if (e.terminalId === terminalId) {
        e.pty.write(data);
        return true;
      }
    }
    return false;
  }

  /**
   * Kill the LIVE PTY of a terminal RECORD (the MCP `close_terminal` path), and drop any
   * parked opening command for it. Idempotent — a no-op when no PTY is live. The record is
   * flipped closed in the DB by nyx-core BEFORE this; here we only retire the live shell.
   */
  closeTerminal(terminalId: string): void {
    this.openingParks.delete(terminalId);
    for (const e of this.entries.values()) {
      if (e.terminalId === terminalId) {
        e.pty.kill();
        return;
      }
    }
  }

  /** Write bytes to a live PTY. Throws if the id is unknown. */
  write(ptyId: number, data: Buffer): void {
    this.entry(ptyId).pty.write(data);
  }

  /** Resize a live PTY (idempotent). Unknown id is a no-op (it may have just exited). */
  resize(ptyId: number, cols: number, rows: number): void {
    this.entries.get(ptyId)?.pty.resize(cols, rows);
  }

  /** Close (kill) a live PTY. The reader EOFs, the pump fires exit, and the entry is
   * removed on that exit. Unknown id is a no-op. */
  close(ptyId: number): void {
    this.entries.get(ptyId)?.pty.kill();
  }

  /**
   * Credit `bytes` of acknowledged output back to a terminal (the renderer consumed
   * them via `xterm.write`). Drops the unacked backlog and resumes the Rust reader
   * once it falls below the low-water mark. Unknown id (already exited) is a no-op.
   */
  ack(ptyId: number, bytes: number): void {
    const e = this.entries.get(ptyId);
    if (!e) return;
    e.unacked = Math.max(0, e.unacked - bytes);
    if (e.paused && e.unacked < LOW_WATER) {
      e.paused = false;
      e.pty.setPaused(false);
    }
  }

  /** Kill + forget every live PTY (ordered shutdown). */
  killAll(): void {
    for (const e of this.entries.values()) {
      try {
        e.pty.kill();
      } catch {
        // best-effort teardown
      }
    }
    this.entries.clear();
  }

  /** Number of live PTYs (lifecycle "active work" gate). */
  get liveCount(): number {
    return this.entries.size;
  }

  /**
   * The LIVE auto-label introspection of a terminal by its live `ptyId` (the
   * `terminal_info` command's backend — auto-label / shell-suffix revival). Resolves the
   * ptyId → its `NyxPty` and reads `{ cwd, foreground }` straight from the kernel (Linux
   * `/proc`, keyed by the shell pid + foreground pgid the napi owns). On Windows (no
   * `/proc`) both fields are `null` WITHOUT erroring — a clean degradation the per-second
   * poll never spams. An unknown/exited ptyId yields the empty reading (never a throw, so
   * the poll degrades gracefully when a terminal just closed).
   */
  terminalInfo(ptyId: number): { cwd: string | null; foreground: string | null } {
    const e = this.entries.get(ptyId);
    if (!e) return { cwd: null, foreground: null };
    const info = e.pty.terminalInfo();
    return { cwd: info.cwd ?? null, foreground: info.foreground ?? null };
  }

  // --- internals ------------------------------------------------------------

  private entry(ptyId: number): PtyEntry {
    const e = this.entries.get(ptyId);
    if (!e) throw new Error(`no live PTY with id ${ptyId}`);
    return e;
  }

  /**
   * Hot output path. Split the (possibly multi-MiB) coalesced chunk into ≤64 KiB
   * pieces, emit each as an ordered `pty-output` event, and grow the unacked
   * backlog. Crossing the high-water mark pauses the Rust reader — the rest of this
   * chunk is still emitted (bounded: at most one coalesced chunk beyond the mark),
   * but the reader will not produce MORE until the renderer acks below low-water.
   */
  private onData(ptyId: number, bytes: Buffer): void {
    const e = this.entries.get(ptyId);
    if (!e) return;
    for (let off = 0; off < bytes.length; off += CHUNK_BYTES) {
      const piece = bytes.subarray(off, Math.min(off + CHUNK_BYTES, bytes.length));
      e.unacked += piece.length;
      this.events.ptyOutput(ptyId, piece);
    }
    // Pause once the backlog reaches the high-water mark (only on the transition).
    if (!e.paused && e.unacked >= HIGH_WATER) {
      e.paused = true;
      e.pty.setPaused(true);
    }
  }

  private onExit(ptyId: number, code: number | null): void {
    const e = this.entries.get(ptyId);
    this.entries.delete(ptyId);
    this.events.ptyExit(ptyId, code);
    // Exec-state (PRD-5 #1): a shell/PTY exit must not leave a stale `running` badge.
    // Settle a still-`running` record to idle (defensive; a settled result survives).
    const terminalId = e?.terminalId ?? null;
    if (terminalId) {
      void this.execState.normalizeOnExit(terminalId);
      // RETRACT the record↔live-PTY liveness binding (Finding C parity): the shell is gone,
      // so the MCP `send_to_terminal` must now return `invalid_state` and `list_terminals`
      // report `live: false` — mirrors the Tauri `TerminalPtyMap::clear` on PTY exit. Done
      // here (not only on the renderer's null-register) so a crashed/exited shell is retracted
      // even if the front never publishes the unbind.
      this.core.unregisterTerminalPty(terminalId);
      // The busy dot's authority is the OS signal, so forget this terminal's tracked
      // busy value (a future re-spawn re-evaluates clean).
      this.exitObserver?.(terminalId);
    }
  }

  private onCwd(ptyId: number, cwd: string): void {
    const e = this.entries.get(ptyId);
    this.events.ptyCwd(ptyId, e?.terminalId ?? null, cwd);
  }

  private onExecState(ptyId: number, ev: ExecStateEvent): void {
    const e = this.entries.get(ptyId);
    // PERSIST then EMIT (DB is the authority for the badge after a restart). A
    // record-less PTY is dropped inside the persister.
    this.execState.onOscTransition(e?.terminalId ?? null, ev);
  }
}
