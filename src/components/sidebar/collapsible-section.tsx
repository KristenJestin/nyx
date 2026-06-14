import { AnimatePresence, motion, useReducedMotion } from "motion/react";

import { sectionTransition, sectionVariants } from "./section-motion";

export interface CollapsibleSectionProps {
  /** Whether the body is shown. Driven by the caller (controlled). */
  open: boolean;
  /** The collapsible content. Rendered only while `open` (presence-animated). */
  children: React.ReactNode;
  className?: string;
}

/**
 * `<CollapsibleSection>` — the animated expand/collapse body shared by the
 * sidebar spine (a project's workspace list, a workspace's subsections). Built
 * on Motion's `AnimatePresence` + an animated `height: auto` (see
 * `section-motion`), honouring `prefers-reduced-motion`. Header/toggle chrome is
 * the caller's; this owns ONLY the animated reveal of the body.
 *
 * `overflow-hidden` is essential: it clips the content while height animates so
 * nothing spills during the collapse. Chrome-only — never the xterm viewport.
 */
export function CollapsibleSection({ open, children, className }: CollapsibleSectionProps) {
  const reduced = useReducedMotion();
  return (
    <AnimatePresence initial={false}>
      {open && (
        <motion.div
          // `layout`-free: this region animates its OWN height; the parent list
          // reflows naturally as it grows/shrinks.
          initial="collapsed"
          animate="expanded"
          exit="collapsed"
          variants={sectionVariants}
          transition={sectionTransition(reduced)}
          className={className}
          style={{ overflow: "hidden" }}
        >
          {children}
        </motion.div>
      )}
    </AnimatePresence>
  );
}
