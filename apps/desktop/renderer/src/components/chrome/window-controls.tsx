import { useCallback, useEffect, useState } from "react";
import { nyxBridge } from "@/bridge";
import { MinusIcon, SquareIcon, XIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { CloseWarningDialog } from "./close-warning-dialog";
import { fetchCloseWarnings, type CloseWarning } from "./close-warning";

/**
 * Decide whether the custom window controls (min / max / close) are shown,
 * from the raw `NYX_WINDOW_CONTROLS` env value.
 *
 * Pure so it is unit-testable without touching the runtime env. The contract:
 * controls are VISIBLE by default; only the exact string `"0"` hides them. Any
 * other value (including unset/empty) keeps them visible — a permissive default
 * so the window is never left uncloseable by an unexpected value. This mirrors
 * `windowControlsVisible()` in the shell's main process (src/main/env.ts); the front
 * reads the resolved value via the `window_controls_visible` command (below).
 *
 * @param raw the raw env value (undefined when unset).
 */
export function windowControlsVisible(raw: string | undefined): boolean {
  return raw !== "0";
}

/**
 * Resolve, at RUNTIME, whether the window controls should render — by asking the
 * shell's MAIN process, which reads the OS env var `NYX_WINDOW_CONTROLS` (`=0` hides;
 * unset/any other value = visible). `window_controls_visible` is answered there
 * directly from `windowControlsVisible()` (src/main/env.ts), never by a sidecar
 * round-trip. The sandboxed renderer cannot read `process.env`, so the main process is
 * the only place the RAW (non-Vite-prefixed) env reaches us; the value would otherwise
 * be frozen at build time. Defaults to VISIBLE on any failure (no backend / IPC error)
 * so the window is never left uncloseable.
 *
 * Returns `true` while the async call is in flight (default = visible), then the
 * resolved value. Re-fetches only on mount — the toggle is fixed at launch.
 */
export function useWindowControlsVisible(): boolean {
  const [visible, setVisible] = useState(true);
  useEffect(() => {
    let cancelled = false;
    void nyxBridge
      .invoke<boolean>("window_controls_visible")
      .then((v) => {
        if (!cancelled) setVisible(v);
      })
      .catch(() => {
        // Keep the permissive default (visible) on any IPC failure.
      });
    return () => {
      cancelled = true;
    };
  }, []);
  return visible;
}

/**
 * `<WindowControls>` — the minimize / maximize-restore / close cluster for the
 * frameless chrome. Each button is wired to the shell-agnostic `nyxBridge.window`
 * seam: minimize, toggle-maximize (restore when already maximized), and close. Under
 * Windows these three ARE the window's only mouse controls — the OS draws none on an
 * undecorated window — so they must never be inert.
 *
 * The cluster carries `no-drag-region` (`-webkit-app-region: no-drag`), cutting itself
 * OUT of the bar's drag region: a drag region swallows the pointer for the window-move
 * handler, so a button left inside one is dead on arrival.
 *
 * Errors from the window IPC are swallowed: a failed `minimize` must never crash
 * the chrome (e.g. if the call races window teardown).
 */
export function WindowControls() {
  // The live agent sessions a close would drop (PRD-5 #6). `null` = no pending close
  // prompt; a non-empty array opens the confirm dialog.
  const [warnings, setWarnings] = useState<CloseWarning[] | null>(null);

  const minimize = useCallback(() => {
    void nyxBridge.window.minimize().catch(() => {});
  }, []);
  const toggleMaximize = useCallback(() => {
    void nyxBridge.window.toggleMaximize().catch(() => {});
  }, []);

  /**
   * Actually close the window, having ALREADY cleared the close-warning below
   * (no live session, or the user confirmed).
   *
   * It goes through `confirmClose`, not `close`: the shell's main-process guard
   * WITHHOLDS every close and relays it back to the renderer's `<CloseGuard>`, which
   * runs the very same `agent_close_warnings` check. A plain `close()` from here would
   * therefore re-enter that guard and ask the user a SECOND time for the confirmation
   * just given. `confirmClose` is the documented way past the guard for a caller that
   * has already run it (see `WindowControls.confirmClose` in bridge/contract.ts).
   * Swallows IPC errors so a teardown race never throws.
   */
  const doClose = useCallback(() => {
    void nyxBridge.window.confirmClose().catch(() => {});
  }, []);

  /**
   * Close request: first ask the backend whether any LIVE agent session would be
   * dropped (a project that does NOT auto-resume). If none, close immediately; if some,
   * open the confirm dialog and let the user decide. Fail-open — `fetchCloseWarnings`
   * returns `[]` on error so a backend hiccup never traps the window.
   */
  const requestClose = useCallback(() => {
    void fetchCloseWarnings().then((w) => {
      if (w.length === 0) {
        doClose();
      } else {
        setWarnings(w);
      }
    });
  }, [doClose]);

  return (
    <div className="no-drag-region flex items-center gap-0.5">
      <Button variant="ghost" size="icon-sm" aria-label="Minimize window" onClick={minimize}>
        <MinusIcon />
      </Button>
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label="Maximize or restore window"
        onClick={toggleMaximize}
      >
        <SquareIcon />
      </Button>
      <Button
        variant="ghost-destructive"
        size="icon-sm"
        aria-label="Close window"
        onClick={requestClose}
      >
        <XIcon />
      </Button>

      <CloseWarningDialog
        open={warnings !== null}
        warnings={warnings ?? []}
        onConfirm={() => {
          setWarnings(null);
          doClose();
        }}
        onCancel={() => setWarnings(null)}
      />
    </div>
  );
}

export default WindowControls;
