import { GripVerticalIcon } from "lucide-react";
import { useSortable } from "@dnd-kit/react/sortable";

import { cn } from "@/lib/utils";
import { ProjectItem, type ProjectItemProps } from "./project-item";

export interface SortableProjectItemProps extends ProjectItemProps {
  /** This project's position in the rendered list (dnd-kit sort index). */
  index: number;
}

/**
 * `<SortableProjectItem>` — one drag-sortable project band (FEEDBACK #11). Mirrors
 * `<SortableTerminalItem>`: the OUTER element dnd-kit owns is a PLAIN element
 * (`useSortable`'s `ref`), and a dedicated GRIP — wired to dnd-kit's `handleRef`
 * — is the drag affordance, injected into `<ProjectItem>`'s band header.
 *
 * Why a handle (not a whole-row drag like the terminal rows): a project band's
 * header is a dense interactive surface (the disclosure toggle spans its width,
 * plus a kebab menu). Scoping the drag to a grip keeps the toggle's click and the
 * kebab unambiguous while reusing the SAME dnd-kit library + drag/drop pattern.
 *
 * The OUTER `<li>` is the PLAIN element dnd-kit owns (`useSortable`'s `ref`);
 * `<ProjectItem>` (a `motion.div`) renders inside it. dnd-kit drives the `<li>`'s
 * `transform` during a drag/reflow while Motion (inside `<ProjectItem>`) owns only
 * its band's own open/close — separate elements, nothing to fight (same split as
 * the terminal rows).
 */
export function SortableProjectItem({ index, ...projectProps }: SortableProjectItemProps) {
  const { ref, handleRef, isDragging } = useSortable({
    id: projectProps.tree.project.id,
    index,
  });

  return (
    <li ref={ref} className={cn("list-none", isDragging && "opacity-60")}>
      <ProjectItem
        {...projectProps}
        dragHandle={
          <button
            ref={handleRef}
            type="button"
            aria-label={`Reorder project ${projectProps.tree.project.name}`}
            // The grip is the drag surface: quiet until the band is hovered, then a
            // grab cursor. `touch-none` lets the pointer sensor own the gesture.
            className="flex shrink-0 cursor-grab touch-none items-center self-stretch pl-1.5 text-muted-foreground/40 opacity-0 transition group-hover:opacity-100 active:cursor-grabbing"
          >
            <GripVerticalIcon className="size-3.5" />
          </button>
        }
      />
    </li>
  );
}

export default SortableProjectItem;
