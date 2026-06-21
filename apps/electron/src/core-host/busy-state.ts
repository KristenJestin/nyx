/**
 * OS BUSY-STATE POLL LOOP (PRD-5 task #1, decision 1-B) — the Electron core-host
 * mirror of the Tauri bridge's `start_busy_state_loop` + `scan_busy_once` +
 * `BusyStateTracker`.
 *
 * `tcgetpgrp` (the foreground-process-group read behind `Pty::is_busy`) is PULL-ONLY —
 * there is no kernel notification when it changes — so the host SAMPLES the busy bit
 * of every record-backed PTY on a fixed cadence and emits `terminal://busy-state` ONLY
 * on a TRANSITION (the tracker diffs the last emitted value). This is the AUTHORITY
 * for the running dot, derived LIVE from the OS and INDEPENDENT of OSC 133: a
 * force-quit/restore can never leave a phantom running (a restored terminal with no
 * foreground command samples idle by construction).
 *
 * NON-UNIX (Windows): ConPTY exposes no foreground process group, so `busy()` returns
 * `null` (treated as idle); the loop runs but emits nothing — Windows busy/idle is
 * explicitly out of scope, exactly as on the Tauri side.
 */
import type { ElectronEventSink } from "./event-sink";
import type { PtyManager } from "./pty-manager";

/**
 * Cadence of the busy-state poll (matches the Tauri `BUSY_POLL_INTERVAL` = 300ms).
 * Snappy for the dot while keeping the per-tick syscall cost negligible (one
 * `tcgetpgrp` per open terminal).
 */
const BUSY_POLL_INTERVAL_MS = 300;

/**
 * Tracks the last busy value EMITTED per persistent terminal id so a `terminal://
 * busy-state` is emitted only on a CHANGE (mirrors `BusyStateTracker`). A never-seen
 * id is announced ONLY on its first BUSY (idle is the implicit default the front
 * already shows), so boot/restore never emits a redundant `false` for every idle
 * terminal.
 */
class BusyStateTracker {
  private readonly last = new Map<string, boolean>();

  changed(terminalId: string, busy: boolean): boolean {
    const prev = this.last.get(terminalId);
    this.last.set(terminalId, busy);
    if (prev === undefined) return busy; // first sight: announce only the first BUSY.
    return prev !== busy;
  }

  forget(terminalId: string): void {
    this.last.delete(terminalId);
  }
}

export class BusyStatePoller {
  private readonly tracker = new BusyStateTracker();
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor(
    private readonly ptys: PtyManager,
    private readonly events: ElectronEventSink,
  ) {}

  /** Start the poll loop (idempotent). Each tick is a bounded sweep. */
  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => this.sweep(), BUSY_POLL_INTERVAL_MS);
    // Don't keep the host process alive solely for the poll (parity with the Tauri
    // loop having no teardown handle — the process owns exactly one of these).
    this.timer.unref?.();
  }

  /** Stop the loop (shutdown). */
  stop(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  /** Drop a terminal's tracked value (on PTY exit) so the table doesn't grow. */
  forget(terminalId: string): void {
    this.tracker.forget(terminalId);
  }

  /**
   * ONE sweep: snapshot the busy bit of every record-backed PTY, diff against the last
   * emitted value, and emit `terminal://busy-state` for each TRANSITION only. A PTY
   * whose busy bit cannot be derived (`null` — non-Unix / master closed) is treated as
   * idle (a terminal with no live foreground group is not running anything).
   */
  sweep(): void {
    for (const { terminalId, busy } of this.ptys.busySnapshot()) {
      if (this.tracker.changed(terminalId, busy)) {
        this.events.ptyBusyState(terminalId, busy);
      }
    }
  }
}
