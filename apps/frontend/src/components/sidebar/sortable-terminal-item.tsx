import { motion, usePresence, useReducedMotion } from "motion/react";
import { useSortable } from "@dnd-kit/react/sortable";

import { itemTransition, itemVariants } from "./item-motion";
import { TerminalItemBody, terminalRowClassName, type TerminalItemProps } from "./terminal-item";

export type SortableTerminalItemProps = TerminalItemProps;

/**
 * `<SortableTerminalItem>` — a drag-sortable terminal row with open/close height
 * animations, Motion and dnd-kit on SEPARATE elements:
 *
 *  - the OUTER `<li>` is a PLAIN element dnd-kit owns (`useSortable`'s `ref`).
 *  - the INNER `<motion.div>` does the height/opacity ENTER + EXIT. It is driven by
 *    `usePresence` (not by being a direct `AnimatePresence` child) so the sortable
 *    `<li>` stays plain: on removal AnimatePresence keeps the `<li>` mounted,
 *    `isPresent` flips false, the inner div collapses, and `safeToRemove` unmounts.
 *
 * KNOWN LIMITATION: combining dnd-kit's reorder with Motion's mount/unmount
 * animation makes the list "sautiller" slightly on DROP — when the order commits,
 * React/AnimatePresence reconcile the reordered rows at the same moment dnd-kit
 * tears its drag down. This is an accepted trade-off (a clean drag is only possible
 * by dropping the add/close animation entirely — the bisection result). We keep the
 * animations and live with the small drop hitch.
 */
export function SortableTerminalItem(props: SortableTerminalItemProps) {
  const reduced = useReducedMotion();
  const { ref, isDragging } = useSortable({ id: props.record.id, index: props.index });
  const [isPresent, safeToRemove] = usePresence();

  return (
    <li
      ref={ref}
      onClick={() => props.onSelect(props.record.id)}
      aria-current={props.active ? "true" : undefined}
      data-rail-row
      className="list-none"
    >
      <motion.div
        variants={itemVariants}
        initial="initial"
        animate={isPresent ? "animate" : "exit"}
        transition={itemTransition(reduced)}
        onAnimationComplete={() => {
          if (!isPresent) safeToRemove?.();
        }}
        style={{ overflow: "hidden" }}
        className="pb-0.5"
      >
        <div className={terminalRowClassName(props.active, isDragging)}>
          <TerminalItemBody {...props} />
        </div>
      </motion.div>
    </li>
  );
}

export default SortableTerminalItem;
