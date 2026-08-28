import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from "react";

import { GroupPanel } from "@/components/panes/group-panel";
import {
  findGroup,
  groupOfTerminal,
  type GroupColor,
  type PaneGroup,
} from "@/components/panes/pane-groups";

/**
 * THE SIDEBAR'S VIEW OF THE GROUPS — which terminals share one, and the four edits a row can ask
 * for. A tiny context, mirroring `use-pane-mounting` / `use-terminal-close` next to it, so a
 * terminal row deep in the workspace accordion reads the model and acts on it without prop-drilling
 * through the accordion → tree → list chain.
 *
 * It carries NO state of its own: `usePaneLayout` owns the collection and every mutator here is
 * one of its callbacks. That is deliberate — a second copy of the membership would be a second
 * truth, and the whole model rests on there being exactly one.
 */
export interface TerminalGroupsValue {
  readonly groups: readonly PaneGroup[];
  /** The group holding `terminalId`, or `null` when it is SOLO. */
  readonly groupOf: (terminalId: string) => PaneGroup | null;
  /**
   * ACTIVATE A FOLDER — show its group, the way clicking a member shows it (human review
   * 01M14112THEB4094B2G614SXR5). `null` on a surface with no tiling behind it (a component test, the
   * legacy sidebar): the head then keeps folding on click rather than promising an activation that
   * would do nothing. It is `usePaneLayout`'s own `showGroup`, threaded — no second navigation rule.
   */
  readonly showGroup: ((groupId: string) => void) | null;
  /** The group ON SCREEN right now — what makes a folder line read as the selected one. */
  readonly shownGroupId: string | null;
  /** Pull a terminal OUT of its group — it becomes solo and is shown alone. */
  readonly leaveGroup: (terminalId: string) => void;
  /**
   * THE FOUR GESTURES OF THE LIST (review 01M0BPP79090KAQ9ZAFJ23ZRTX, critical) reach the model
   * through these three, and nothing else:
   *  - re-order INSIDE a block → {@link reorderGroupMembers};
   *  - drop a row OUT of a block → the pre-existing {@link leaveGroup};
   *  - drop a row INTO a block → {@link insertIntoGroup}, gated by {@link canInsertIntoGroup};
   *  - move a whole block → a pure LIST re-order, which touches no group at all.
   *
   * …and the FIFTH, since human review 01M1313GN2ESW0BCM8S1G0ZY2P: drop a row on the birth target →
   * {@link createGroupWith}, gated by {@link canCreateGroup}.
   */
  readonly reorderGroupMembers: (groupId: string, orderedIds: readonly string[]) => void;
  /** Put a terminal into a group AT the drop position (the split's path — the floor applies). */
  readonly insertIntoGroup: (groupId: string, terminalId: string, index: number) => void;
  /**
   * Whether the 180×120px floor allows that insertion. Asked DURING the drag so a refused drop is
   * shown refused before the release rather than failing silently after it (orchestrator arbitrage).
   */
  readonly canInsertIntoGroup: (groupId: string, index: number) => boolean;
  /**
   * THE FIFTH GESTURE — make a group of two out of terminals that ALREADY EXIST (human review
   * 01M1313GN2ESW0BCM8S1G0ZY2P). `null` on a surface that does not offer the birth, and that absence
   * is what keeps the dotted target off it: `group-drag.ts` says the slot is a key a list either has
   * or has not, and this is the same statement one level up. No terminal is created.
   */
  readonly createGroupWith: ((terminalId: string, partnerId: string) => void) | null;
  /** Whether the 180×120px floor allows a group to be BORN — asked DURING the drag, like the rest. */
  readonly canCreateGroup: () => boolean;
  /** Dissolve a group — every member becomes solo. Nothing is closed. */
  readonly dissolveGroup: (groupId: string) => void;
  /**
   * CLOSE every terminal of a group, and the group with them (human review
   * 01M1312M70TP8TJWT4TG505S0J) — the destructive twin of {@link dissolveGroup}, and the only one of
   * the two that touches the terminals themselves. `null` on a surface that has no close to offer
   * (a component test, the legacy sidebar): the panel then does not draw the action at all rather
   * than drawing one that would do nothing.
   */
  readonly closeGroup: ((groupId: string) => void) | null;
  readonly renameGroup: (groupId: string, name: string | null) => void;
  readonly recolorGroup: (groupId: string, color: GroupColor | null) => void;
  /**
   * Open the shared name + colour panel on a group. It NEVER opens by itself — a group is born
   * grey and nameless and stays usable that way, so this is only ever called by a user gesture
   * — and there is exactly ONE now: the `…` of the block's own head. A row does not speak for its
   * group any more (review 01M0BPP79090KAQ9ZAFJ23ZRTX).
   */
  readonly openPanel: (groupId: string) => void;
}

/**
 * The INERT default: no group, and every edit a no-op. A surface mounted without the provider (the
 * legacy sidebar, a component test of one row) renders exactly as it did before groups existed.
 */
const INERT: TerminalGroupsValue = {
  groups: [],
  groupOf: () => null,
  // Same statement as `closeGroup` and `createGroupWith`: NOT an activation that does nothing, but
  // no activation at all — so a surface without the provider keeps the head's old fold-on-click.
  showGroup: null,
  shownGroupId: null,
  leaveGroup: () => {},
  reorderGroupMembers: () => {},
  insertIntoGroup: () => {},
  // Inert means "no group here", so nothing can be entered — never a silent yes.
  canInsertIntoGroup: () => false,
  // Same statement as `closeGroup` below: NOT a birth that does nothing, but no birth at all — so a
  // surface without the provider does not draw the dotted target either.
  createGroupWith: null,
  canCreateGroup: () => false,
  dissolveGroup: () => {},
  // Not "a close that does nothing" but "no close at all" — the panel reads the difference.
  closeGroup: null,
  renameGroup: () => {},
  recolorGroup: () => {},
  openPanel: () => {},
};

const TerminalGroupsContext = createContext<TerminalGroupsValue>(INERT);

export function TerminalGroupsProvider({
  groups,
  showGroup = INERT.showGroup,
  shownGroupId = INERT.shownGroupId,
  leaveGroup,
  reorderGroupMembers = INERT.reorderGroupMembers,
  insertIntoGroup = INERT.insertIntoGroup,
  canInsertIntoGroup = INERT.canInsertIntoGroup,
  createGroupWith = INERT.createGroupWith,
  canCreateGroup = INERT.canCreateGroup,
  dissolveGroup,
  closeGroup = INERT.closeGroup,
  renameGroup,
  recolorGroup,
  children,
}: Omit<
  TerminalGroupsValue,
  | "groupOf"
  | "openPanel"
  | "showGroup"
  | "shownGroupId"
  | "reorderGroupMembers"
  | "insertIntoGroup"
  | "canInsertIntoGroup"
  | "createGroupWith"
  | "canCreateGroup"
  | "closeGroup"
> &
  Partial<
    Pick<
      TerminalGroupsValue,
      | "showGroup"
      | "shownGroupId"
      | "reorderGroupMembers"
      | "insertIntoGroup"
      | "canInsertIntoGroup"
      | "createGroupWith"
      | "canCreateGroup"
      | "closeGroup"
    >
  > & { children: ReactNode }) {
  // WHICH group the shared panel is editing (`null` = closed). Held HERE, next to the mutators it
  // drives, so the panel is mounted exactly once for the whole sidebar instead of once per row.
  const [panelGroupId, setPanelGroupId] = useState<string | null>(null);
  const openPanel = useCallback((groupId: string) => setPanelGroupId(groupId), []);

  const value = useMemo<TerminalGroupsValue>(
    () => ({
      groups,
      groupOf: (terminalId) => groupOfTerminal({ groups }, terminalId),
      showGroup,
      shownGroupId,
      leaveGroup,
      reorderGroupMembers,
      insertIntoGroup,
      canInsertIntoGroup,
      createGroupWith,
      canCreateGroup,
      dissolveGroup,
      closeGroup,
      renameGroup,
      recolorGroup,
      openPanel,
    }),
    [
      groups,
      showGroup,
      shownGroupId,
      leaveGroup,
      reorderGroupMembers,
      insertIntoGroup,
      canInsertIntoGroup,
      createGroupWith,
      canCreateGroup,
      dissolveGroup,
      closeGroup,
      renameGroup,
      recolorGroup,
      openPanel,
    ],
  );

  // The panel reads the group LIVE from the collection, so an edit it commits is reflected in it
  // immediately; a group that dissolved under it (its last partner left) closes it.
  const editing = panelGroupId == null ? null : findGroup({ groups }, panelGroupId);
  return (
    <TerminalGroupsContext.Provider value={value}>
      {children}
      <GroupPanel
        open={editing != null}
        group={editing}
        onClose={() => setPanelGroupId(null)}
        onRename={renameGroup}
        onRecolor={recolorGroup}
        onDissolve={dissolveGroup}
        // `?? undefined` and not `closeGroup`: the panel tells "no close on this surface" from "a
        // close that does nothing" by the prop's ABSENCE, and `null` would be neither.
        onCloseAll={closeGroup ?? undefined}
      />
    </TerminalGroupsContext.Provider>
  );
}

/** The groups view — inert (no group, no-op edits) outside a provider. */
export function useTerminalGroups(): TerminalGroupsValue {
  return useContext(TerminalGroupsContext);
}
