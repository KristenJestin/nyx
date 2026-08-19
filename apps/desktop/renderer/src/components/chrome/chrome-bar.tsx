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
 * `<ChromeBar>` — the THIN top chrome for the frameless window (`frame: false`,
 * apps/desktop/src/main/window.ts). It is the ONLY title bar the window has, so it is
 * also the only way to move / minimize / maximize / close it with the mouse under
 * Windows, where no window manager offers those on an undecorated window.
 *
 * It is deliberately minimal: a full-width drag region, an optional discreet
 * active-item title, and the window controls cluster. There are NO tabs here —
 * terminal navigation lives entirely in the left sidebar (the top-tab model was
 * abandoned). Keeping the bar this thin is what makes the frameless window feel
 * native without an OS title bar.
 *
 * DRAG: `drag-region` sets `-webkit-app-region: drag` (globals.css), the protocol
 * Chromium/Electron uses to move a frameless window — the whole bar is grabbable, so
 * the user can move the window from anywhere not covered by a control. It used to
 * carry `data-tauri-drag-region` instead, which only WRY ever understood and which
 * moved nothing at all once the shell became Electron (Tauri left in PDR-0006).
 * The property is not inherited but the region is a UNION of boxes, so the children
 * of the strip stay draggable without repeating the class — while the controls
 * cluster explicitly opts back OUT with `no-drag-region` (in `<WindowControls>`),
 * because a drag region swallows the pointer and a button left inside one never fires.
 *
 * HEIGHT: `--chrome-bar-h` is the shared token — the `h-app` utility (globals.css), which is
 * what every full-height surface below the bar claims, subtracts exactly this, so the bar and
 * the space reserved for it cannot drift.
 */
export function ChromeBar({ title, controlsVisible, className }: ChromeBarProps) {
  // Runtime-resolved default (OS env via backend); an explicit prop wins.
  const resolved = useWindowControlsVisible();
  const showControls = controlsVisible ?? resolved;
  return (
    <div
      className={cn(
        // A thin strip (h-9 via the shared token); the drag region IS the whole bar.
        "drag-region flex h-[var(--chrome-bar-h,2.25rem)] w-full shrink-0 items-center justify-between gap-2 border-b border-border bg-background px-2 select-none",
        className,
      )}
    >
      {/* Left spacer keeps the title visually centered against the controls. */}
      <div className="flex-1 basis-0" aria-hidden="true" />
      {title ? (
        <span className="pointer-events-none truncate text-center text-xs text-muted-foreground">
          {title}
        </span>
      ) : (
        <div className="flex-1 basis-0" aria-hidden="true" />
      )}
      {/* The cell stays IN the drag region (it is bar surface the user can grab); only
          the controls cluster itself opts out — see `<WindowControls>`. */}
      <div className="flex flex-1 basis-0 items-center justify-end">
        {showControls ? <WindowControls /> : null}
      </div>
    </div>
  );
}

export default ChromeBar;
