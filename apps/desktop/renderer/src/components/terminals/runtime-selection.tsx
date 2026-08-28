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
  | { readonly kind: "service"; readonly id: string }
  /**
   * A COMMAND GROUP, selected AS A GROUP (human review 01M14112THEB4094B2G614SXR5).
   *
   * A terminal group is activated through one of its members — one pane holds the keyboard, and the
   * tiling already remembers which. A command group has no such thing: « pour les commandes y a pas
   * de notion de focus, c'est trop chelou que je doive cliquer que un des 3 pour avoir le 3 ». So
   * the FOLDER itself is what gets selected here, and `<CommandPaneHost>` shows the whole
   * disposition without any member being the viewed one.
   *
   * `workspaceId` rides along because a group is PROJECT data while its instances are per WORKSPACE:
   * the sidebar draws one folder per workspace the group is materialised in, and the panes must be
   * that workspace's runs rather than another's.
   */
  | { readonly kind: "command-group"; readonly id: string; readonly workspaceId: string };

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
