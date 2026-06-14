import { cn } from "@/lib/utils";
import { useWindowControlsVisible, WindowControls } from "@/components/chrome/window-controls";

export interface ChromeBarProps {
  /**
   * Discreet label of the active terminal, shown centered in the bar. Optional —
   * the bar is just a drag strip + controls when there is nothing to show.
   */
  title?: string;
  /**
   * Whether the min/max/close cluster is shown. When omitted, resolved at
   * runtime from the OS env `NYX_WINDOW_CONTROLS` (via the backend, default
   * visible; `=0` hides). Pass an explicit value to override (e.g. tests).
   */
  controlsVisible?: boolean;
  className?: string;
}

/**
 * `<ChromeBar>` — the THIN top chrome for the frameless window.
 *
 * It is deliberately minimal: a full-width drag region (`data-tauri-drag-region`,
 * which WRY interprets as "drag the window from here"), an optional discreet
 * active-item title, and the window controls cluster. There are NO tabs here —
 * terminal navigation lives entirely in the left sidebar (the top-tab model was
 * abandoned). Keeping the bar this thin is what makes the frameless window feel
 * native without an OS title bar.
 *
 * The controls sit outside the drag region so their clicks are not swallowed by
 * the drag handler.
 */
export function ChromeBar({ title, controlsVisible, className }: ChromeBarProps) {
  // Runtime-resolved default (OS env via backend); an explicit prop wins.
  const resolved = useWindowControlsVisible();
  const showControls = controlsVisible ?? resolved;
  return (
    <div
      data-tauri-drag-region
      className={cn(
        // h-9 thin strip; the drag region IS the whole bar so the user can grab
        // it anywhere not covered by a control.
        "flex h-9 w-full shrink-0 items-center justify-between gap-2 border-b border-border bg-background px-2 select-none",
        className,
      )}
    >
      {/* Left spacer keeps the title visually centered against the controls. */}
      <div data-tauri-drag-region className="flex-1 basis-0" aria-hidden="true" />
      {title ? (
        <span
          data-tauri-drag-region
          className="pointer-events-none truncate text-center text-xs text-muted-foreground"
        >
          {title}
        </span>
      ) : (
        <div data-tauri-drag-region className="flex-1 basis-0" aria-hidden="true" />
      )}
      <div className="flex flex-1 basis-0 items-center justify-end">
        {showControls ? <WindowControls /> : null}
      </div>
    </div>
  );
}

export default ChromeBar;
