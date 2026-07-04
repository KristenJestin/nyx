import { createContext, type ReactNode, useContext, useMemo, useState } from "react";

/**
 * The Terminals page RUNTIME SELECTION — which runtime element (a terminal, or a service) the
 * user has focused in the tree. It is a SINGLE shared selection across the whole page (all the
 * entered project's workspaces), so the tree highlights the focused row AND the one main pane
 * shows the matching surface. Kept as a tiny context (no business logic) so the tree rows and
 * the pane host read one source of truth.
 */

/** A selectable runtime element, tagged by kind so the pane host knows what to render. */
export type RuntimeSelection =
  | { readonly kind: "terminal"; readonly id: string }
  | { readonly kind: "service"; readonly id: string };

interface RuntimeSelectionContextValue {
  readonly selected: RuntimeSelection | null;
  readonly select: (selection: RuntimeSelection) => void;
  /** Whether `candidate` is the current selection (kind + id match). */
  readonly isSelected: (candidate: RuntimeSelection) => boolean;
}

const RuntimeSelectionContext = createContext<RuntimeSelectionContextValue | null>(null);

export function RuntimeSelectionProvider({
  children,
  initialSelection = null,
}: {
  children: ReactNode;
  initialSelection?: RuntimeSelection | null;
}) {
  const [selected, setSelected] = useState<RuntimeSelection | null>(initialSelection);
  const value = useMemo<RuntimeSelectionContextValue>(
    () => ({
      selected,
      select: setSelected,
      isSelected: (candidate) =>
        selected != null && selected.kind === candidate.kind && selected.id === candidate.id,
    }),
    [selected],
  );
  return (
    <RuntimeSelectionContext.Provider value={value}>{children}</RuntimeSelectionContext.Provider>
  );
}

export function useRuntimeSelection(): RuntimeSelectionContextValue {
  const ctx = useContext(RuntimeSelectionContext);
  if (!ctx) {
    throw new Error("useRuntimeSelection must be used inside <RuntimeSelectionProvider>");
  }
  return ctx;
}
