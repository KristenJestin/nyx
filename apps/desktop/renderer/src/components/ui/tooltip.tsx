import type * as React from "react";
import { Tooltip as BaseTooltip } from "@base-ui/react/tooltip";

import { cn } from "@/lib/utils";

/**
 * A small reusable tooltip built on Base UI's `Tooltip`, styled to the design
 * system (popover tokens, soft shadow, an arrow) with a sensible open delay.
 *
 * Use it to wrap an icon-only button so its purpose is discoverable on hover /
 * focus. The wrapped element becomes the tooltip TRIGGER via Base UI's
 * `render` prop, so the trigger keeps its own props (the button stays a real
 * button with its `aria-label`) AND gains the tooltip wiring + `aria-describedby`.
 *
 * REDUCED MOTION: the popup's fade is a CSS transition with a `motion-reduce:`
 * escape hatch (mapped to `prefers-reduced-motion: reduce`), so the project's
 * "respect reduced motion" rule holds for tooltips too.
 */
export interface TooltipProps {
  /** The tooltip text (also the accessible description of the trigger). */
  label: React.ReactNode;
  /** The trigger element (typically an icon-only `<Button>`). */
  children: React.ReactElement<Record<string, unknown>>;
  /** Open delay in ms (default 350 — snappy but not twitchy). */
  delay?: number;
  /** Preferred side of the trigger to render on (default "right"). */
  side?: "top" | "right" | "bottom" | "left";
}

export function Tooltip({ label, children, delay = 350, side = "right" }: TooltipProps) {
  return (
    <BaseTooltip.Root>
      <BaseTooltip.Trigger delay={delay} render={children} />
      <BaseTooltip.Portal>
        <BaseTooltip.Positioner side={side} sideOffset={6}>
          <BaseTooltip.Popup
            className={cn(
              "z-50 max-w-56 rounded-md border border-border bg-popover px-2 py-1 text-xs text-popover-foreground shadow-md outline-none select-none",
              // Quick fade keyed on Base UI's open/closed transition hooks.
              "transition-opacity duration-150 ease-out",
              "data-[starting-style]:opacity-0 data-[ending-style]:opacity-0",
              "motion-reduce:transition-none",
            )}
          >
            <BaseTooltip.Arrow className="text-popover">
              <svg width="10" height="5" viewBox="0 0 10 5" aria-hidden>
                <path d="M0 0 L5 5 L10 0 Z" fill="currentColor" />
              </svg>
            </BaseTooltip.Arrow>
            {label}
          </BaseTooltip.Popup>
        </BaseTooltip.Positioner>
      </BaseTooltip.Portal>
    </BaseTooltip.Root>
  );
}
