/**
 * PER-TERMINAL CPU%/RAM POLL LOOP (FEEDBACK #28) — modeled on the OS busy-state poll
 * (`busy-state.ts`), but for a terminal's resource consumption: the summed CPU% + RSS
 * of its PROCESS TREE (the shell + every descendant — `claude`, `npm`, `cargo`, …).
 *
 * Like `tcgetpgrp`, process-table stats are PULL-ONLY — there is no kernel push when a
 * tree's CPU/RAM changes — so the host SAMPLES every live record-backed terminal on a
 * fixed cadence and emits `terminal://stats` keyed by the persistent terminal record id.
 *
 * CROSS-PLATFORM: the actual sampling is `NyxProcStats.treeStatsBatch(shellPids)` over
 * `sysinfo` (Linux/macOS/Windows), which keeps ONE live `System` so CPU% deltas are
 * meaningful. A gone pid samples the all-zero reading (never an error), so a terminal
 * that just closed degrades to nothing rather than throwing.
 *
 * PERF (FEEDBACK #28): each tick makes ONE async `treeStatsBatch` call for ALL live
 * terminals — the full `/proc` scan happens ONCE per tick (not once per terminal) and on
 * a libuv WORKER thread (off the Node main loop), so it never freezes keystroke IPC. If a
 * sweep is still in flight when the interval fires, the tick is SKIPPED (the overlap
 * guard) — a slow scan can never pile up.
 *
 * To keep the IPC quiet, a tick emits for a terminal only when its DISPLAY-ROUNDED value
 * changed since the last emit (CPU to 0.1%, RAM to 1 MiB) — a tree idling at a steady
 * "0.0% · 12 MB" stops re-emitting until it moves. The first sight of any terminal is
 * always emitted so the row seeds its indicator.
 */
import type { ElectronEventSink } from "./event-sink";
import type { NyxProcStatsInstance } from "./napi";
import type { PtyManager } from "./pty-manager";

/**
 * Cadence of the stats poll. Slower than the busy dot (300ms): resource numbers are
 * informational, and `sysinfo` needs a non-trivial gap between refreshes for a stable
 * CPU% delta. ~1.5s matches the `terminal_info` auto-label cadence the PRD already runs.
 */
const STATS_POLL_INTERVAL_MS = 1500;

/** Bytes-per-MiB, the granularity at which a RAM change is considered "visible". */
const MIB = 1024 * 1024;

/**
 * Tracks the last EMITTED (display-rounded) reading per terminal so a `terminal://stats`
 * event fires only on a visible CHANGE (mirrors `BusyStateTracker`). A never-seen
 * terminal is always announced once (to seed the row's indicator).
 */
class StatsTracker {
  private readonly last = new Map<string, { cpu: number; mem: number }>();

  /** True when the rounded reading differs from the last emitted one (or first sight). */
  changed(terminalId: string, cpuPct: number, memBytes: number): boolean {
    const cpu = Math.round(cpuPct * 10) / 10; // 0.1% granularity
    const mem = Math.round(memBytes / MIB); // 1 MiB granularity
    const prev = this.last.get(terminalId);
    this.last.set(terminalId, { cpu, mem });
    if (prev === undefined) return true; // first sight: seed the row.
    return prev.cpu !== cpu || prev.mem !== mem;
  }

  forget(terminalId: string): void {
    this.last.delete(terminalId);
  }
}

export class StatsPoller {
  private readonly tracker = new StatsTracker();
  private timer: ReturnType<typeof setInterval> | null = null;
  /**
   * OVERLAP GUARD (FEEDBACK #28): `true` while a `treeStatsBatch` sweep is awaiting the
   * worker-thread scan. If the interval fires again before it resolves, that tick is
   * SKIPPED — a scan slower than the interval never queues up behind itself.
   */
  private inFlight = false;

  constructor(
    private readonly ptys: PtyManager,
    private readonly events: ElectronEventSink,
    /** The single host-owned introspector — kept ALIVE across ticks for CPU% deltas. */
    private readonly procStats: NyxProcStatsInstance,
  ) {}

  /** Start the poll loop (idempotent). Each tick is a bounded async sweep. */
  start(): void {
    if (this.timer) return;
    // `sweep` is async (it awaits the off-main-thread scan); a rejected promise is
    // already swallowed inside, but `void` makes the fire-and-forget explicit.
    this.timer = setInterval(() => void this.sweep(), STATS_POLL_INTERVAL_MS);
    // Don't keep the host process alive solely for the poll (parity with busy-state).
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
   * ONE sweep: gather every live record-backed terminal's shell pid, make a SINGLE
   * `treeStatsBatch` call (the full `/proc` scan runs ONCE per tick, on a libuv worker
   * thread — off the Node main loop, so keystrokes never freeze), then map each reading
   * back to its terminal id and emit `terminal://stats` for each VISIBLE change.
   *
   * OVERLAP GUARD: if a previous sweep's scan is still in flight, this tick is SKIPPED so
   * a scan slower than the interval can never pile up. The whole body is guarded so a
   * single unexpected failure degrades the tick (clears `inFlight`), never the loop.
   */
  async sweep(): Promise<void> {
    if (this.inFlight) return; // a previous sweep's scan hasn't resolved — skip this tick.

    // Snapshot the terminals to sample THIS tick. The result array is aligned to this
    // order, so we zip it back by index.
    const snapshot = this.ptys.statsSnapshot();
    if (snapshot.length === 0) return; // nothing live → no scan, no work.

    this.inFlight = true;
    try {
      const shellPids = snapshot.map((s) => s.shellPid);
      // ONE batch call for ALL terminals — the single off-main-thread /proc scan.
      const readings = await this.procStats.treeStatsBatch(shellPids);
      for (let i = 0; i < snapshot.length; i++) {
        const terminalId = snapshot[i].terminalId;
        // Defensive: a missing slot (length mismatch — should never happen) degrades to
        // zero rather than throwing mid-loop.
        const reading = readings[i];
        const cpuPct = reading?.cpuPct ?? 0;
        const memBytes = reading?.memBytes ?? 0;
        if (this.tracker.changed(terminalId, cpuPct, memBytes)) {
          this.events.ptyStats(terminalId, cpuPct, memBytes);
        }
      }
    } catch {
      // An unexpected scan failure degrades this whole tick to nothing — the next tick
      // retries from a clean state. Never throws out of the interval callback.
    } finally {
      this.inFlight = false;
    }
  }
}
