import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type * as React from "react";
import { Tabs as BaseTabs } from "@base-ui/react/tabs";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { tabHeightTransition, tabPanelTransition, tabPanelVariants } from "./tabs-motion";

/** `useLayoutEffect` on the client, `useEffect` on the server (no SSR warning). */
const useIsomorphicLayoutEffect = typeof window !== "undefined" ? useLayoutEffect : useEffect;

/**
 * `Tabs` — a small reusable tab set built on **Base UI's `Tabs`** primitives
 * (Root / List / Tab / Indicator / Panel), styled to the design system (a
 * bottom-border rail with muted-foreground triggers that brighten when active).
 *
 * The active-tab underline is Base UI's `Tabs.Indicator`, positioned from the
 * geometry CSS vars it publishes (`--active-tab-left` / `--active-tab-width`) and
 * SLID between tabs with a short transform/width transition. The transition has a
 * `motion-reduce:` escape hatch (mapped to `prefers-reduced-motion: reduce`) so
 * the project's reduced-motion rule holds.
 *
 * The PANEL SWITCH itself is **Motion-animated**: `Tabs.AnimatedPanel` wraps the
 * active panel's content in `AnimatePresence mode="wait"` + a `motion.div` that
 * cross-fades with a small directional slide (see `tabs-motion`), keyed by the
 * active value and honouring `useReducedMotion` like the sidebar's
 * `CollapsibleSection` / `itemTransition`. So switching tabs animates the content
 * (out, then in), not just the underline — the modal's other load-bearing chrome
 * animations (dialog enter/exit, collapsible package.json) are Motion-driven in
 * their own components too.
 *
 * Composed like the shadcn-style `Button`: thin wrappers re-exporting the Base UI
 * parts with our classes, so callers build a tab set from one import:
 *
 *   <Tabs.Root value={tab} onValueChange={setTab}>
 *     <Tabs.List>
 *       <Tabs.Tab value="commands">Commands</Tabs.Tab>
 *       <Tabs.Tab value="import">Import</Tabs.Tab>
 *     </Tabs.List>
 *     <Tabs.AnimatedPanel activeValue={tab} value="commands">…</Tabs.AnimatedPanel>
 *     <Tabs.AnimatedPanel activeValue={tab} value="import">…</Tabs.AnimatedPanel>
 *   </Tabs.Root>
 */

function TabsList({ className, children, ...props }: BaseTabs.List.Props) {
  return (
    <BaseTabs.List
      className={cn("relative flex gap-6 border-b border-border", className)}
      {...props}
    >
      {children}
      <BaseTabs.Indicator
        className={cn(
          "absolute bottom-[-1px] left-0 h-0.5 rounded-full bg-primary",
          "translate-x-[var(--active-tab-left)] [width:var(--active-tab-width)]",
          "transition-[transform,width] duration-200 ease-out motion-reduce:transition-none",
        )}
      />
    </BaseTabs.List>
  );
}

function TabsTab({ className, children, ...props }: BaseTabs.Tab.Props) {
  return (
    <BaseTabs.Tab
      className={cn(
        "relative flex cursor-pointer items-center gap-2 border-none bg-transparent px-0.5 py-2.5 text-sm font-medium",
        "text-muted-foreground transition-colors outline-none",
        "hover:text-foreground/80 data-[selected]:text-foreground",
        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
        "[&_svg]:size-3.5 [&_svg]:shrink-0 [&_svg]:opacity-80",
        className,
      )}
      {...props}
    >
      {children}
    </BaseTabs.Tab>
  );
}

function TabsPanel({ className, ...props }: BaseTabs.Panel.Props) {
  return <BaseTabs.Panel className={cn("outline-none", className)} {...props} />;
}

export interface AnimatedTabPanelProps extends Omit<BaseTabs.Panel.Props, "value"> {
  /** This panel's tab value (the `Tabs.Tab` it pairs with). */
  value: string;
  /** The currently-active tab value (so the panel knows when it is shown). */
  activeValue: string;
}

/**
 * `Tabs.AnimatedPanel` — a `Tabs.Panel` whose content is **Motion-animated** on the
 * tab switch. The Base UI `Tabs.Panel` keeps its a11y wiring (`role="tabpanel"`,
 * `aria-labelledby`) and is kept mounted so its content can animate OUT when it
 * stops being active; an `AnimatePresence mode="wait"` renders the content (a
 * `motion.div`) only while this panel is the active one, cross-fading it with a
 * small directional slide (`tabs-motion`). Honours `useReducedMotion` — reduced
 * motion collapses to an instant swap — like `CollapsibleSection`.
 *
 * `mode="wait"` makes the swap a single clean pass (the outgoing content settles
 * before the incoming appears); each panel owns its own region so the two never
 * fight over layout.
 */
function AnimatedTabPanel({
  value,
  activeValue,
  className,
  children,
  ...props
}: AnimatedTabPanelProps) {
  const reduced = useReducedMotion();
  const active = activeValue === value;
  return (
    <BaseTabs.Panel value={value} keepMounted className={cn("outline-none", className)} {...props}>
      <AnimatePresence mode="wait" initial={false}>
        {active && (
          <motion.div
            key={value}
            variants={tabPanelVariants}
            initial="initial"
            animate="enter"
            exit="exit"
            transition={tabPanelTransition(reduced)}
            className="h-full"
          >
            {children}
          </motion.div>
        )}
      </AnimatePresence>
    </BaseTabs.Panel>
  );
}

export interface AnimatedTabsHeightProps {
  /**
   * Re-measure the content whenever this changes — pass the active tab value so a
   * tab SWITCH (which swaps the visible panel) re-reads the new content height.
   */
  deps: unknown;
  className?: string;
  children: React.ReactNode;
}

/**
 * `Tabs.AnimatedHeight` — wraps the tab PANELS region and animates ITS HEIGHT when
 * the active panel's content is taller/shorter than the previous one's, so the
 * enclosing modal grows/shrinks SMOOTHLY on a tab switch instead of snapping to
 * the new content height instantly (review finding). The content fade is already
 * handled per-panel by `AnimatedPanel`; this fixes the HEIGHT jump on top of it.
 *
 * HOW: an inner wrapper is measured (its natural height) and the outer `motion.div`
 * animates `height` to that value — the same measured-height approach as the
 * sidebar's `CollapsibleSection` (an animated height with `overflow-hidden` clip),
 * sharing its spring (`tabHeightTransition`). A `ResizeObserver` keeps the target
 * in sync as the content (and thus its natural height) changes through the panel
 * cross-fade. Honours `useReducedMotion` (instant height, no spring). When the
 * platform has no `ResizeObserver` (jsdom in unit tests) it falls back to a plain
 * `height: auto` wrapper — content stays fully visible, just unanimated.
 */
function AnimatedTabsHeight({ deps, className, children }: AnimatedTabsHeightProps) {
  const reduced = useReducedMotion();
  const innerRef = useRef<HTMLDivElement | null>(null);
  // `null` height = not yet measured ⇒ render at natural `height: auto` so the
  // content is never clipped before the first measure (and in environments
  // without ResizeObserver, e.g. jsdom).
  const [height, setHeight] = useState<number | null>(null);

  useIsomorphicLayoutEffect(() => {
    const el = innerRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const measure = () => setHeight(el.offsetHeight);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
    // `deps` (the active tab value) is the sole trigger: it forces a re-measure on
    // a tab switch so the height animates to the newly-shown panel's content.
  }, [deps]);

  return (
    <motion.div
      animate={{ height: height ?? "auto" }}
      transition={tabHeightTransition(reduced)}
      style={{ overflow: "hidden" }}
      className={className}
    >
      <div ref={innerRef}>{children}</div>
    </motion.div>
  );
}

/**
 * A small count pill shown beside a tab label (e.g. the number of commands /
 * importable scripts), matching the muted badge look used across the modal.
 */
export function TabCount({ children }: { children: React.ReactNode }) {
  return (
    <span className="rounded-full bg-muted px-1.5 text-xs leading-relaxed font-medium text-muted-foreground">
      {children}
    </span>
  );
}

export const Tabs = {
  Root: BaseTabs.Root,
  List: TabsList,
  Tab: TabsTab,
  Panel: TabsPanel,
  AnimatedPanel: AnimatedTabPanel,
  AnimatedHeight: AnimatedTabsHeight,
};
