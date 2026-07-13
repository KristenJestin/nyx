import { useEffect, useState } from "react";
import { nyxBridge } from "@/bridge";

import { CloseWarningDialog } from "./close-warning-dialog";
import { fetchCloseWarnings, type CloseWarning } from "./close-warning";

/**
 * `<CloseGuard>` — the OS-close interception for LIVE agent sessions (PRD v2.5.6 #1).
 *
 * nyx's window is frameless and its min/max/close controls belong to the user's WM —
 * so the CLOSE gesture arrives as an OS "close requested" signal, not an in-app button.
 * The Electron shell WITHHOLDS that close and relays it here (`window.onCloseRequested`);
 * this guard then asks the backend whether any LIVE agent session would be dropped (a
 * project that does NOT auto-resume, via `agent_close_warnings`). When none, it proceeds
 * immediately (`window.confirmClose`); when some, it opens the confirm dialog listing
 * each session and lets the user decide — "Close anyway" proceeds, "Keep open" leaves
 * the window untouched.
 *
 * It is mounted ONCE on the always-mounted navigation shell (`<BaseLayout>`), so the
 * guard is live on every screen. Fail-OPEN: `fetchCloseWarnings` returns `[]` on any IPC
 * hiccup, so a backend problem proceeds with the close rather than trapping the window.
 * Reuses the v1 `CloseWarningDialog` + `fetchCloseWarnings` verbatim — no new UI.
 */
export function CloseGuard() {
  // The live agent sessions a close would drop. `null` = no pending close prompt; a
  // non-empty array opens the confirm dialog.
  const [warnings, setWarnings] = useState<CloseWarning[] | null>(null);

  useEffect(() => {
    let disposed = false;
    let unsub: (() => void) | null = null;

    void nyxBridge.window
      .onCloseRequested(() => {
        // The OS asked to close: run the guard. Fail-open — `[]` proceeds.
        void fetchCloseWarnings().then((w) => {
          if (disposed) return;
          if (w.length === 0) {
            void nyxBridge.window.confirmClose();
          } else {
            setWarnings(w);
          }
        });
      })
      .then((un) => {
        // The effect may have torn down before the async subscription resolved.
        if (disposed) un();
        else unsub = un;
      })
      .catch(() => {
        // No shell close hook (tests / older preload) — nothing to guard.
      });

    return () => {
      disposed = true;
      unsub?.();
    };
  }, []);

  return (
    <CloseWarningDialog
      open={warnings !== null}
      warnings={warnings ?? []}
      onConfirm={() => {
        setWarnings(null);
        void nyxBridge.window.confirmClose();
      }}
      onCancel={() => setWarnings(null)}
    />
  );
}

export default CloseGuard;
