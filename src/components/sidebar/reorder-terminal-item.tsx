import { GripVerticalIcon } from "lucide-react";
import { Reorder, useDragControls, useReducedMotion } from "motion/react";

import { itemTransition, itemVariants } from "./item-motion";
import {
  TerminalItemBody,
  terminalRowClassName,
  type TerminalItemProps,
} from "./terminal-item";

export type ReorderTerminalItemProps = TerminalItemProps;

/**
 * `<ReorderTerminalItem>` — a terminal row made drag-sortable with motion's
 * `Reorder.Item` (no third-party DnD library). It registers with the
 * surrounding `Reorder.Group` by its record id (`value`), and `Reorder.Item`
 * drives BOTH the live drag follow AND the `layout`-based reflow when items are
 * added/removed/reordered — a single, coherent motion with nothing else
 * fighting it.
 *
 * `dragListener={false}` + `dragControls` mean ONLY the grip handle starts a
 * drag (its `onPointerDown` calls `controls.start`), so clicking/double-clicking
 * the name to select/rename never fights the drag gesture. The presentational
 * content lives in the shared `<TerminalItemBody>`, which is also usable without
 * a Reorder context (the isolation tests render `<TerminalItem>` directly).
 */
export function ReorderTerminalItem(props: ReorderTerminalItemProps) {
  const controls = useDragControls();
  const reduced = useReducedMotion();

  return (
    <Reorder.Item
      value={props.record.id}
      dragListener={false}
      dragControls={controls}
      variants={itemVariants}
      initial="initial"
      animate="animate"
      transition={itemTransition(reduced)}
      whileDrag={{ opacity: 0.6 }}
      aria-current={props.active ? "true" : undefined}
      className={terminalRowClassName(props.active)}
    >
      <TerminalItemBody
        {...props}
        dragHandle={
          <span
            aria-label="Reorder terminal"
            onPointerDown={(e) => controls.start(e)}
            className="flex shrink-0 cursor-grab touch-none items-center text-muted-foreground/40 transition-colors group-hover:text-muted-foreground"
          >
            <GripVerticalIcon className="size-3.5" />
          </span>
        }
      />
    </Reorder.Item>
  );
}

export default ReorderTerminalItem;
