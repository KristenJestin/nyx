import type { ReactNode } from "react";
import { render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The dnd-kit `onDragEnd` GLUE of `<SortableTerminalRows>` (task `01KXBNX51EPQ69MVKYG4W960HD`,
 * follow-up finding `01KXCCYT26ZFAJJ5M9XRV5HF4Z`). The reorder GATE drives `reorderTerminals`
 * through the e2e seam and the persistence unit starts at `reorderTerminals` — so the glue itself
 * (`move(ids, event)` → `sameOrder` guard → callback) was exercised by NOTHING. The WebDriver
 * pointer sensor is undrivable in the Electron/CDP harness, but the `onDragEnd` computation is pure
 * JS: we mock dnd-kit's `<DragDropProvider>` to a pass-through that CAPTURES the `onDragEnd` the
 * component wires, render the REAL component, then fire a SYNTHETIC dnd-kit `dragend` event through
 * it. `@dnd-kit/helpers`'s `move` and the `sameOrder` guard stay REAL — only the (untestable)
 * pointer machinery is stubbed, so a regression in `move()`, the guard, or the callback wiring
 * (group + reordered ids) is caught.
 */

/**
 * The minimal shape of the dnd-kit `dragend` event the handler + `move` read: a top-level
 * `canceled` (the handler's early-out) plus an `operation` carrying the dragged `source` and the
 * drop `target` (what `move` resolves the new index order from).
 */
interface FakeDragEndEvent {
  canceled: boolean;
  operation: {
    canceled: boolean;
    source: { id: string } | null;
    target: { id: string } | null;
  };
}

// Captured lazily when the mocked provider renders; `vi.hoisted` so the holder exists by the time
// the mock factory (hoisted above imports) runs.
const dnd = vi.hoisted(() => ({ onDragEnd: null as null | ((event: unknown) => void) }));

vi.mock("@dnd-kit/react", () => ({
  DragDropProvider: ({
    children,
    onDragEnd,
  }: {
    children: ReactNode;
    onDragEnd: (event: unknown) => void;
  }) => {
    dnd.onDragEnd = onDragEnd;
    return <>{children}</>;
  },
}));

// `useSortable` needs the real DragDropManager context (absent under the stubbed provider) and a
// live DOM node; the rows only read `{ ref, isDragging }`, so a no-op is enough to render.
vi.mock("@dnd-kit/react/sortable", () => ({
  useSortable: () => ({ ref: () => {}, isDragging: false }),
}));

import { SortableTerminalRows } from "./sortable-terminal-rows";
import { TerminalReorderProvider } from "./use-terminal-reorder";

/** Fire the captured `onDragEnd` with a synthetic dnd-kit event (throws if none captured). */
function fireDragEnd(event: FakeDragEndEvent): void {
  if (!dnd.onDragEnd) throw new Error("onDragEnd was not captured — the provider never rendered");
  dnd.onDragEnd(event);
}

/** A `dragend` that drags `sourceId` onto `targetId` (both live), the drop `move` reorders on. */
function drop(sourceId: string, targetId: string): FakeDragEndEvent {
  return {
    canceled: false,
    operation: { canceled: false, source: { id: sourceId }, target: { id: targetId } },
  };
}

describe("SortableTerminalRows — onDragEnd glue (dnd-kit)", () => {
  const reorderTerminals = vi.fn();

  beforeEach(() => {
    reorderTerminals.mockClear();
    dnd.onDragEnd = null;
  });

  function renderRows(group: string | null) {
    return render(
      <TerminalReorderProvider reorderTerminals={reorderTerminals} orderedIds={["A", "B", "C"]}>
        <SortableTerminalRows
          terminals={[{ id: "A" }, { id: "B" }, { id: "C" }]}
          group={group}
          empty="none"
          renderRow={(t) => <span>{t.id}</span>}
        />
      </TerminalReorderProvider>,
    );
  }

  it("a WORKSPACE drag that CHANGES the order commits reorderTerminals(group, reordered ids)", () => {
    renderRows("ws1");
    // Drag A onto B → move([A,B,C], A→B) = [B,A,C]; the order changed → the callback fires with it.
    fireDragEnd(drop("A", "B"));
    expect(reorderTerminals).toHaveBeenCalledTimes(1);
    expect(reorderTerminals).toHaveBeenCalledWith("ws1", ["B", "A", "C"]);
  });

  it("reorders the LOOSE group (group=null) with the moved order", () => {
    renderRows(null);
    // Drag C onto A → move([A,B,C], C→A) = [C,A,B].
    fireDragEnd(drop("C", "A"));
    expect(reorderTerminals).toHaveBeenCalledTimes(1);
    expect(reorderTerminals).toHaveBeenCalledWith(null, ["C", "A", "B"]);
  });

  it("a NO-OP drop (source === target ⇒ sameOrder) never calls reorderTerminals (guard)", () => {
    renderRows("ws1");
    // Dropping A onto itself yields the SAME order → the `sameOrder` guard skips the commit.
    fireDragEnd(drop("A", "A"));
    expect(reorderTerminals).not.toHaveBeenCalled();
  });

  it("a CANCELED drag is ignored (no reorder even if source/target differ)", () => {
    renderRows("ws1");
    fireDragEnd({
      canceled: true,
      operation: { canceled: true, source: { id: "A" }, target: { id: "C" } },
    });
    expect(reorderTerminals).not.toHaveBeenCalled();
  });
});
