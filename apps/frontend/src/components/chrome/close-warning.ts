import { nyxBridge } from "@/bridge";

/**
 * One close-warning entry from the backend `agent_close_warnings` command (PRD-5 #6).
 * Mirrors `bridge::CloseWarningEntry`: a LIVE agent session a close would silently
 * drop (a project that does NOT auto-resume). `message` is the ready-to-show line
 * (names the agent + terminal + workspace); the structured fields are for grouping/keys.
 */
export interface CloseWarning {
  terminal_id: string;
  agent_kind: string;
  message: string;
}

/**
 * Fetch the agent-session CLOSE WARNINGS (PRD-5 #6). An EMPTY array means "no warning
 * needed — close freely". On any IPC failure we deliberately return `[]` (fail-OPEN):
 * a backend hiccup must never trap the user in an un-closeable window. Pure-ish wrapper
 * over the backend command so the close guard / tests have one entry point.
 */
export async function fetchCloseWarnings(): Promise<CloseWarning[]> {
  try {
    const result = await nyxBridge.invoke<CloseWarning[]>("agent_close_warnings");
    // Defensive: coerce any non-array (e.g. a mock/stub returning null) to `[]` so
    // the caller can always read `.length` — a missing backend must close FREELY,
    // never trap the window.
    return Array.isArray(result) ? result : [];
  } catch {
    return [];
  }
}
