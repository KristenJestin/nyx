/**
 * EXEC-STATE PERSISTENCE (PRD-5 task #1) — the Electron core-host mirror of the Tauri
 * bridge's `persist_and_emit_exec_state` + `normalize_exec_state_on_exit`.
 *
 * The napi PTY EMITS decoded OSC 133 command-END transitions (success/error) as raw
 * callbacks; this module turns each into the SAME two-step the Tauri bridge runs:
 *
 *   1. PERSIST FIRST — write the transition to the DB (`NyxCore.setExecState`, an
 *      AsyncTask off the Node loop) so the DB is the authority for the badge after a
 *      restart, and a listener re-reading the row on the event sees the committed
 *      value. The write returns the stamped `updatedAt`.
 *   2. THEN EMIT — `terminal://exec-state` with the persisted `unread` + `updatedAt`.
 *      A persist that updated NO row (unknown terminal id) SKIPS the emit — we never
 *      announce a state the DB does not hold (parity with the Tauri guard).
 *
 * OSC 133 was retrograded to RESULT ANNOTATION: a `D` end is always an UNREAD settled
 * notification (`success`/`error`); it NEVER drives `running` (the OS busy signal owns
 * that — see `busy-state.ts`). On PTY exit, a stale persisted `running` (an older
 * build's artefact) is settled to idle, defensively, exactly like the Tauri bridge.
 */
import type { ExecStateEvent, NyxCoreInstance } from "./napi";
import type { ElectronEventSink } from "./event-sink";

/** The DB exec-state vocabulary (mirrors `nyx_core::db::STATE_*`). */
const STATE_IDLE = "idle";
const STATE_RUNNING = "running";

export class ExecStatePersister {
  constructor(
    private readonly core: NyxCoreInstance,
    private readonly events: ElectronEventSink,
  ) {}

  /**
   * Handle one decoded OSC 133 exec-state transition for a record-backed terminal: a
   * `D` end (success/error) is an UNREAD settled notification. Persist it then emit
   * the persisted shape. A record-less PTY (`terminalId === null`) has no sidebar
   * badge to drive, so it is dropped here (the Tauri pump skips it identically).
   */
  onOscTransition(terminalId: string | null, ev: ExecStateEvent): void {
    if (!terminalId) return;
    // The napi addon already mapped exit 0 → "success", else → "error". A settled `D`
    // end is always unread (the user has not yet viewed the result).
    void this.persistAndEmit(terminalId, ev.state, ev.exitCode ?? null, true);
  }

  /**
   * Settle a stale `running` to idle when a terminal's PTY EXITS (defensive — OSC 133
   * no longer posts `running`, but an older build's row might). A SETTLED state
   * (success/error) or idle is left untouched. Mirrors `normalize_exec_state_on_exit`.
   */
  async normalizeOnExit(terminalId: string | null): Promise<void> {
    if (!terminalId) return;
    const row = await this.core.getTerminal(terminalId).catch(() => null);
    if (row?.execState === STATE_RUNNING) {
      // Not unread: there is no result to notify (the command never reported an exit).
      await this.persistAndEmit(terminalId, STATE_IDLE, null, false);
    }
  }

  /**
   * PERSIST then EMIT (the DB write happens FIRST). Skips the emit when no row was
   * updated (unknown id), so we never announce a state the DB does not hold. The
   * emitted event carries the persisted `unread` + the stamped `updatedAt`.
   */
  private async persistAndEmit(
    terminalId: string,
    state: string,
    exitCode: number | null,
    unread: boolean,
  ): Promise<void> {
    try {
      const persist = await this.core.setExecState(terminalId, state, exitCode, unread);
      if (!persist.updated) return; // unknown id — do not emit.
      this.events.ptyExecState(terminalId, state, exitCode, unread, persist.updatedAt);
    } catch {
      // A persist failure must not crash the host; the badge simply does not update
      // (best-effort, exactly like the Tauri `.ok()` swallow).
    }
  }
}
