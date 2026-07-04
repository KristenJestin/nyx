import { useEffect, useRef } from "react";
import { nyxBridge } from "@/bridge";

/**
 * `use-mark-read` — CONSUME a row's unread "unseen result" notification when the row becomes the
 * viewed one (finding 53). When `active` (selected/focused) AND still `unread`, it fires the
 * matching backend mark-read command ONCE, so the settled success/error dot disappears and stays
 * gone. It REUSES the existing backend seams — no new backend:
 *   - a TERMINAL clears via `terminal_exec_mark_read` (`{ id }`) — the SAME command the old
 *     sidebar's `markRead` used;
 *   - a SERVICE clears via `command_acknowledge` (`{ instanceId }`) — the SAME command the old
 *     command view used to acknowledge a finished one-shot.
 * Both are idempotent + best-effort. Those commands do NOT broadcast a runtime-collection
 * `*://changed` topic, so the page's snapshot is not re-pulled on a mark-read; `onConsume` records
 * the visit in the page-local consumed registry so the dot stays gone even before a fresh snapshot
 * (the persisted clear covers the next one). A `ref` guards against re-firing for the same row
 * while it stays active (the effect re-runs on unrelated snapshot churn); it re-arms on deselect.
 */
export function useMarkReadOnActive(
  kind: "terminal" | "service",
  id: string,
  active: boolean,
  unread: boolean,
  onConsume?: () => void,
): void {
  // Remember the last id we cleared while active, so a re-render (new snapshot) does not re-invoke
  // the command for a row we already consumed. Reset when the row is no longer active.
  const clearedFor = useRef<string | null>(null);
  const onConsumeRef = useRef(onConsume);
  onConsumeRef.current = onConsume;

  useEffect(() => {
    if (!active) {
      clearedFor.current = null;
      return;
    }
    if (!unread) return;
    if (clearedFor.current === id) return;
    clearedFor.current = id;
    // Record the visit locally FIRST (so the dot hides immediately + stays hidden), then persist.
    onConsumeRef.current?.();
    const command = kind === "terminal" ? "terminal_exec_mark_read" : "command_acknowledge";
    const args = kind === "terminal" ? { id } : { instanceId: id };
    void nyxBridge.invoke(command, args).catch(() => {});
  }, [kind, id, active, unread]);
}
