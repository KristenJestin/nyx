/**
 * AGENT-SESSION RESUME PARKS (PRD-5 task #5) — the Electron core-host mirror of the
 * Tauri `PendingResumes` + the injection half of `register_terminal_pty`.
 *
 * At boot, the host runs `NyxCore.resumeScanOnBoot()` (sweep stale sessions → gather
 * candidates → pure `decide_resume`) and PARKS each `claude --resume <id>` command keyed
 * by its terminal RECORD id. When that terminal's PTY next spawns (the front remounts
 * it), the park is DRAINED (one-shot) and the command is written into the fresh shell —
 * exactly once, at the first opening — so the agent conversation resumes. A terminal
 * opened bare (no park) injects nothing.
 *
 * A park whose injection FAILS (the PTY write errored) marks its session `resume_failed`
 * so the next launch will not retry (parity with the Tauri `mark_session_resume_failed`).
 */
import type { NyxCoreInstance, ResumePark } from "./napi";

export class ResumeParks {
  /** record_id → parked resume. One-shot: drained on the first PTY spawn. */
  private readonly byRecord = new Map<string, ResumePark>();

  constructor(private readonly core: NyxCoreInstance) {}

  /** Park a batch of resumes (the boot-scan result), keyed by terminal record id. */
  setAll(parks: ResumePark[]): void {
    for (const p of parks) this.byRecord.set(p.terminalId, p);
  }

  /** Whether `recordId` has a parked resume waiting. */
  has(recordId: string): boolean {
    return this.byRecord.has(recordId);
  }

  /**
   * DRAIN the parked resume for `recordId` (one-shot) and return its injectable line
   * (`command` + trailing carriage return so the shell runs it and stays interactive), or
   * null if none is parked. The session id is retained on the returned handle so a failed
   * write can be reported via {@link markFailed}.
   */
  take(recordId: string): { bytes: Buffer; sessionId: string } | null {
    const park = this.byRecord.get(recordId);
    if (!park) return null;
    this.byRecord.delete(recordId);
    return { bytes: Buffer.from(`${park.command}\r`, "utf8"), sessionId: park.sessionId };
  }

  /** Report that a parked resume could not be injected — mark the session failed. */
  markFailed(sessionId: string): void {
    void this.core.markResumeFailed(sessionId).catch(() => {
      /* best-effort: the next launch simply re-scans */
    });
  }
}
