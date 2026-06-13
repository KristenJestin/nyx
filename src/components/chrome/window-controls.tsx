import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { MinusIcon, SquareIcon, XIcon } from "lucide-react";

import { Button } from "@/components/ui/button";

/**
 * Decide whether the custom window controls (min / max / close) are shown,
 * from the raw `NYX_WINDOW_CONTROLS` env value.
 *
 * Pure so it is unit-testable without touching the runtime env. The contract:
 * controls are VISIBLE by default; only the exact string `"0"` hides them. Any
 * other value (including unset/empty) keeps them visible — a permissive default
 * so the window is never left uncloseable by an unexpected value. This mirrors
 * the Rust-side `controls_visible_from_env`; the front reads the resolved value
 * via the `window_controls_visible` command (below).
 *
 * @param raw the raw env value (undefined when unset).
 */
export function windowControlsVisible(raw: string | undefined): boolean {
  return raw !== "0";
}

/**
 * Resolve, at RUNTIME, whether the window controls should render — by asking the
 * Rust backend, which reads the OS env var `NYX_WINDOW_CONTROLS` (`=0` hides;
 * unset/any other value = visible). The webview front cannot read `process.env`,
 * so the backend is the only place the RAW (non-Vite-prefixed) env reaches us;
 * the value would otherwise be frozen at build time. Defaults to VISIBLE on any
 * failure (no backend / IPC error) so the window is never left uncloseable.
 *
 * Returns `true` while the async call is in flight (default = visible), then the
 * resolved value. Re-fetches only on mount — the toggle is fixed at launch.
 */
export function useWindowControlsVisible(): boolean {
  const [visible, setVisible] = useState(true);
  useEffect(() => {
    let cancelled = false;
    void invoke<boolean>("window_controls_visible")
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
 * frameless chrome. Each button is wired to the live Tauri window API
 * (`@tauri-apps/api/window`): minimize, toggle-maximize (restore when already
 * maximized), and close. Sits OUTSIDE the drag region so clicks aren't consumed
 * by `data-tauri-drag-region`.
 *
 * Errors from the window IPC are swallowed: a failed `minimize` must never crash
 * the chrome (e.g. if the call races window teardown).
 */
export function WindowControls() {
  const minimize = useCallback(() => {
    void getCurrentWindow().minimize().catch(() => {});
  }, []);
  const toggleMaximize = useCallback(() => {
    void getCurrentWindow().toggleMaximize().catch(() => {});
  }, []);
  const close = useCallback(() => {
    void getCurrentWindow().close().catch(() => {});
  }, []);

  return (
    <div className="flex items-center gap-0.5">
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label="Minimize window"
        onClick={minimize}
      >
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
        onClick={close}
      >
        <XIcon />
      </Button>
    </div>
  );
}

export default WindowControls;
