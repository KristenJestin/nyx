import type * as React from "react";
import { Menu as BaseMenu } from "@base-ui/react/menu";
import { Tooltip as BaseTooltip } from "@base-ui/react/tooltip";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";

/**
 * A small reusable dropdown `<Menu>` built on Base UI's `Menu`, styled to the
 * design system (popover tokens, soft shadow) and ANIMATED with Motion — the
 * same rationale as the dialog: Base UI's CSS `data-[starting-style]` transition
 * does not reliably fire in WebKitGTK, so we let Motion own the popup's
 * enter/exit (a quick fade + subtle scale/rise from the trigger).
 *
 * The trigger is supplied via the `render` prop so the caller's element (an
 * icon-only `<Button>`) stays a real button with its own `aria-label` while
 * gaining the menu wiring (`aria-haspopup`, `aria-expanded`, roving focus,
 * full keyboard navigation). Items close the menu on click by default.
 *
 * TOOLTIP: pass `tooltip` to get a hover/focus tooltip on the kebab. We compose
 * it by nesting `Menu.Trigger → Tooltip.Trigger → <Button>` on the SAME element
 * (NOT by wrapping the whole Menu trigger in a separate Tooltip component, which
 * would swallow the Menu trigger's props/ref and stop the menu from opening).
 *
 * REDUCED MOTION: the transition collapses to zero-duration under
 * `prefers-reduced-motion`. Chrome only — never the xterm viewport.
 */

export interface MenuProps {
  /** The trigger element (typically an icon-only `<Button>` kebab). */
  trigger: React.ReactElement<Record<string, unknown>>;
  /** Optional tooltip label shown on hover/focus of the trigger. */
  tooltip?: React.ReactNode;
  /** The menu body — compose with `<MenuItem>` / `<MenuSeparator>`. */
  children: React.ReactNode;
  /** Preferred side of the trigger to open on (default "bottom"). */
  side?: "top" | "right" | "bottom" | "left";
  /** Alignment along that side (default "end" — right-aligned under a kebab). */
  align?: "start" | "center" | "end";
}

export function Menu({ trigger, tooltip, children, side = "bottom", align = "end" }: MenuProps) {
  const reduced = useReducedMotion();

  const popup = (
    <BaseMenu.Portal>
      <BaseMenu.Positioner side={side} align={align} sideOffset={4}>
        <BaseMenu.Popup
          className={cn(
            "z-50 min-w-44 origin-[var(--transform-origin)] rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-lg outline-none",
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
          {children}
        </BaseMenu.Popup>
      </BaseMenu.Positioner>
    </BaseMenu.Portal>
  );

  // No tooltip: the Button is the Menu trigger directly.
  if (tooltip == null) {
    return (
      <BaseMenu.Root>
        <BaseMenu.Trigger render={trigger} />
        {popup}
      </BaseMenu.Root>
    );
  }

  // With a tooltip: compose BOTH onto the SAME button element by nesting render
  // props — Menu.Trigger renders a Tooltip.Trigger which renders the Button. So
  // one button is the menu trigger AND the tooltip anchor, and neither swallows
  // the other's wiring.
  return (
    <BaseMenu.Root>
      <BaseTooltip.Root>
        <BaseMenu.Trigger render={<BaseTooltip.Trigger render={trigger} />} />
        <BaseTooltip.Portal>
          <BaseTooltip.Positioner side="bottom" sideOffset={6}>
            <BaseTooltip.Popup
              className={cn(
                "z-50 max-w-56 rounded-md border border-border bg-popover px-2 py-1 text-xs text-popover-foreground shadow-md outline-none select-none",
                "transition-opacity duration-150 ease-out",
                "data-[starting-style]:opacity-0 data-[ending-style]:opacity-0",
                "motion-reduce:transition-none",
              )}
            >
              {tooltip}
            </BaseTooltip.Popup>
          </BaseTooltip.Positioner>
        </BaseTooltip.Portal>
      </BaseTooltip.Root>
      {popup}
    </BaseMenu.Root>
  );
}

export interface MenuItemProps extends BaseMenu.Item.Props {
  /** Leading icon slot. */
  icon?: React.ReactNode;
  /** Tint the item destructive (e.g. Delete). */
  destructive?: boolean;
}

/** A single menu row: icon + label, keyboard-highlightable, closes on click. */
export function MenuItem({
  icon,
  destructive = false,
  className,
  children,
  ...props
}: MenuItemProps) {
  return (
    <BaseMenu.Item
      className={cn(
        "flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-sm outline-none select-none",
        "data-[highlighted]:bg-sidebar-accent data-[highlighted]:text-sidebar-accent-foreground",
        destructive
          ? "text-destructive data-[highlighted]:bg-destructive data-[highlighted]:text-destructive-foreground"
          : "text-popover-foreground",
        className,
      )}
      {...props}
    >
      {icon && <span className="flex size-4 shrink-0 items-center">{icon}</span>}
      <span className="min-w-0 flex-1 truncate">{children}</span>
    </BaseMenu.Item>
  );
}

/** A thin separator between groups of menu items. */
export function MenuSeparator({ className }: { className?: string }) {
  return <BaseMenu.Separator className={cn("my-1 h-px bg-border", className)} />;
}
