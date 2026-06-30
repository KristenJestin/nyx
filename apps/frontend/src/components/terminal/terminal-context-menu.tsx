import { useCallback, useRef, useState } from "react";
import { Menu as BaseMenu } from "@base-ui/react/menu";
import { ClipboardCopy, ClipboardPaste } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { MenuItem } from "@/components/ui/menu";

/**
 * `<TerminalContextMenu>` — a right-click Copy/Paste menu for the terminal
 * surface, anchored at the cursor.
 *
 * Why a bespoke component (vs. the kebab `<Menu>` in `ui/menu`): that one opens
 * from a fixed trigger element; a context menu must open at the click point. We
 * drive Base UI's `Menu.Root` in CONTROLLED mode and feed the `Positioner` a
 * VIRTUAL anchor whose `getBoundingClientRect` is a zero-size box at the cursor —
 * the standard Floating UI / Base UI way to place a popup at coordinates without a
 * real anchor element.
 *
 * It wraps the terminal as a transparent layer: `onContextMenu` on the wrapper
 * captures the right-click, records the coordinates, opens the menu, and prevents
 * the OS/Chromium default menu. The two items call back into the same clipboard
 * helpers the keyboard chords use, so right-click and `Ctrl+Shift+C/V` share one
 * implementation. "Paste" is always enabled (we can't synchronously know the
 * clipboard contents without reading it); "Copy" is disabled when there is no
 * selection so it never clobbers the clipboard with an empty string.
 */

export interface TerminalContextMenuProps {
  /** The terminal surface to wrap (the xterm container). */
  children: React.ReactNode;
  /** Whether there is currently a selection (drives the Copy item's enabled state). */
  hasSelection: () => boolean;
  /** Copy the current selection to the clipboard. */
  onCopy: () => void;
  /** Paste the clipboard contents into the terminal. */
  onPaste: () => void;
  className?: string;
}

export function TerminalContextMenu({
  children,
  hasSelection,
  onCopy,
  onPaste,
  className,
}: TerminalContextMenuProps) {
  const reduced = useReducedMotion();
  const [open, setOpen] = useState(false);
  // The cursor point the menu anchors to, kept in a ref so the virtual anchor's
  // getBoundingClientRect reads the latest value without re-creating the object.
  const point = useRef({ x: 0, y: 0 });
  // Snapshot of "is there a selection?" taken at open time — drives the Copy item.
  const [copyEnabled, setCopyEnabled] = useState(false);

  const onContextMenu = useCallback(
    (e: React.MouseEvent) => {
      // Replace the native context menu with ours.
      e.preventDefault();
      point.current = { x: e.clientX, y: e.clientY };
      setCopyEnabled(hasSelection());
      setOpen(true);
    },
    [hasSelection],
  );

  // A zero-size virtual anchor at the cursor; Floating UI positions the popup
  // against it exactly like a real element.
  const anchor = useRef({
    getBoundingClientRect: () =>
      ({
        x: point.current.x,
        y: point.current.y,
        width: 0,
        height: 0,
        top: point.current.y,
        right: point.current.x,
        bottom: point.current.y,
        left: point.current.x,
      }) as DOMRect,
  });

  return (
    <BaseMenu.Root open={open} onOpenChange={setOpen} modal={false}>
      {/* The wrapper is the right-click surface; it must fill its parent so the
          whole terminal area is covered. */}
      <div className={cn("contents", className)} onContextMenu={onContextMenu}>
        {children}
      </div>
      <BaseMenu.Portal>
        <BaseMenu.Positioner anchor={anchor.current} side="bottom" align="start" sideOffset={2}>
          <BaseMenu.Popup
            className={cn(
              "z-50 min-w-40 origin-[var(--transform-origin)] rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-lg outline-none",
            )}
            render={
              <motion.div
                initial={{ opacity: 0, scale: 0.96, y: -4 }}
                animate={{ opacity: 1, scale: 1, y: 0 }}
                transition={
                  reduced
                    ? { duration: 0 }
                    : { type: "spring", stiffness: 520, damping: 38, mass: 0.7 }
                }
              />
            }
          >
            <MenuItem icon={<ClipboardCopy />} disabled={!copyEnabled} onClick={onCopy}>
              Copy
            </MenuItem>
            <MenuItem icon={<ClipboardPaste />} onClick={onPaste}>
              Paste
            </MenuItem>
          </BaseMenu.Popup>
        </BaseMenu.Positioner>
      </BaseMenu.Portal>
    </BaseMenu.Root>
  );
}

export default TerminalContextMenu;
