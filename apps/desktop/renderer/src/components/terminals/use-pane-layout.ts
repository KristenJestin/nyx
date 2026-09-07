import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { errorText } from "@/bridge/error-text";
import { toast } from "@/components/ui/toast";
import { useRuntimeSelection } from "./runtime-selection";
import {
  loadPaneCollection,
  restorePaneCollection,
  savePaneCollection,
} from "./pane-layout-persistence";
import {
  closeGroupPane,
  createGroupFromSplit,
  detachTerminal,
  dissolveGroup as dissolveGroupIn,
  EMPTY_COLLECTION,
  findGroup,
  groupedTerminalIds,
  groupInsertAnchor,
  groupOfTerminal,
  insertIntoGroup as insertIntoGroupIn,
  movePaneInGroup,
  nextAppendDirection,
  orderGroupMembers as orderGroupMembersIn,
  pruneCollection,
  setGroupColor as setGroupColorIn,
  setGroupName as setGroupNameIn,
  splitInGroup,
  type GroupColor,
  type PaneCollection,
  type PaneGroup,
} from "@/components/panes/pane-groups";
import {
  createPaneLayout,
  findPane,
  findPaneByTerminal,
  movePaneToEdge,
  newPaneId,
  paneAfterClose,
  paneLeaves,
  paneTerminalIds,
  setSplitRatio as setSplitRatioTree,
  type PaneEdge,
  type PaneLeaf,
  type PaneNode,
  type SplitDirection,
} from "@/components/panes/pane-tree";
import {
  canSplitPane,
  layoutFitsFloor,
  MIN_PANE_HEIGHT_PX,
  MIN_PANE_WIDTH_PX,
  paneNeighbor,
  type FocusDirection,
  type HostSize,
  type SplitResizePhase,
} from "@/components/panes/pane-geometry";
import { loadLastTerminalId } from "./ui-persistence";
import type { WorkspaceRuntimeTerminal } from "@/components/runtime/use-workspace-runtime";

/**
 * The throwaway ids the BIRTH's floor probe is built from. A group of two is the frame split once,
 * and `canSplitPane` needs a tree and a pane to measure — these name that probe rather than letting
 * two call sites mint two different ones.
 */
const BIRTH_PROBE_TERMINAL = "__birth-probe__";
const BIRTH_PROBE_PANE = "__birth-probe-pane__";

/** The minimal workspace shape the split needs (a slice of `WorkspaceRecord`). */
export interface SplitWorkspace {
  readonly id: string;
  readonly path: string;
  readonly is_root: boolean;
}

/**
 * WHERE a split creates its terminal — the pure resolution, so the fallback chain is provable
 * without mounting the page:
 *  1. the SAME workspace as the focused pane's terminal (the expected case);
 *  2. the project's ROOT workspace (else its first) when that binding is missing or STALE — a
 *    terminal whose workspace was deleted must still be splittable;
 *  3. `null` when the project has NO workspace at all — the caller REFUSES the split with an
 *    explicit message instead of spawning a terminal attached to nothing (finding #26).
 */
export function resolveSplitWorkspace(
  terminal: { readonly workspace_id: string | null } | null | undefined,
  workspaces: readonly SplitWorkspace[],
): { id: string; path: string } | null {
  const boundId = terminal?.workspace_id ?? null;
  const bound = boundId == null ? undefined : workspaces.find((w) => w.id === boundId);
  const target = bound ?? workspaces.find((w) => w.is_root) ?? workspaces[0];
  return target ? { id: target.id, path: target.path } : null;
}

/**
 * The DEFAULT host measurement: the tiling host's own box, found by the `data-pane-host` attribute
 * `<PaneTreeHost>` already stamps on it. It is read ON DEMAND (at the instant a split is asked
 * for), never stored — there is nothing to go stale, and a page with no tiling mounted yet simply
 * measures nothing and the floor cannot refuse (see {@link canSplitPane}).
 *
 * It is injectable ({@link usePaneLayout}'s `measureHost`) so the refusal is provable without a
 * layout engine: jsdom gives every box 0×0, so a test hands the real numbers in directly.
 */
function measurePaneHost(): HostSize | null {
  if (typeof document === "undefined") return null;
  const host = document.querySelector("[data-pane-host]");
  if (host == null) return null;
  const box = host.getBoundingClientRect();
  return box.width > 0 && box.height > 0 ? { width: box.width, height: box.height } : null;
}

/**
 * WHAT THE MAIN AREA SHOWS. Exactly one of:
 *  - a SOLO terminal, filling the frame — it has no group and therefore no tree of its own, so the
 *    display carries the one-leaf node it is painted with (kept in state so its pane id is stable
 *    across renders and the focus can address it);
 *  - a GROUP, showing that group's whole tree.
 *
 * This is what makes the tiling ESCAPABLE: picking a solo terminal in the sidebar SHOWS it alone
 * instead of substituting it into the focused pane, which is what v1 did and had no way back from.
 */
export type PaneDisplay =
  | { readonly kind: "solo"; readonly terminalId: string; readonly tree: PaneLeaf }
  | { readonly kind: "group"; readonly groupId: string };

/** The collection, what is on screen, and the ONE focused pane — one state so they cannot disagree. */
interface PaneLayoutState {
  readonly collection: PaneCollection;
  readonly display: PaneDisplay | null;
  readonly focusedPaneId: string | null;
  /**
   * This state is a MID-DRAG PREVIEW of a separator: shown, laid out and resized like any other,
   * but NOT written down — the `commit` that ends the interaction produces the persisted state.
   * Carried IN the state (rather than in a ref) so the persist effect can never disagree with the
   * tree it is looking at, and so an abandoned drag cannot leave persistence muted: the very next
   * non-preview transition is a normal, persisted one.
   */
  readonly preview?: boolean;
}

/** The tree actually painted for `display` — a group's own tree, or the solo terminal's one leaf. */
export function displayTree(
  collection: PaneCollection,
  display: PaneDisplay | null,
): PaneNode | null {
  if (display == null) return null;
  if (display.kind === "solo") return display.tree;
  return findGroup(collection, display.groupId)?.tree ?? null;
}

/**
 * SHOW `terminalId`: its group when it has one (focusing THAT terminal's pane inside it), else the
 * terminal alone. The single entry point of the navigation rule — the sidebar, the deep-link and
 * the restore all go through it, so none of them can invent a different behaviour.
 *
 * `reuse` lets a caller keep an existing solo node (and therefore its pane id) when the terminal
 * shown does not change.
 */
export function showTerminal(
  collection: PaneCollection,
  terminalId: string,
  reuse: PaneDisplay | null = null,
): { display: PaneDisplay; focusedPaneId: string } {
  const group = groupOfTerminal(collection, terminalId);
  if (group != null) {
    const leaf = findPaneByTerminal(group.tree, terminalId);
    // `leaf` is non-null by construction (`groupOfTerminal` found the terminal in this very tree).
    const paneId = leaf?.id ?? paneLeaves(group.tree)[0].id;
    return { display: { kind: "group", groupId: group.id }, focusedPaneId: paneId };
  }
  if (reuse != null && reuse.kind === "solo" && reuse.terminalId === terminalId) {
    return { display: reuse, focusedPaneId: reuse.tree.id };
  }
  const tree = createPaneLayout(terminalId, newPaneId());
  return { display: { kind: "solo", terminalId, tree }, focusedPaneId: tree.id };
}

/**
 * Pin the focus onto a LIVE leaf of what is displayed: `preferred` when it still exists, else the
 * first pane, else nothing. Every state transition goes through this, so the focused pane id can
 * never point at a pane that was closed, pruned or dissolved.
 */
export function withFocus(
  collection: PaneCollection,
  display: PaneDisplay | null,
  preferred: string | null,
): PaneLayoutState {
  const tree = displayTree(collection, display);
  if (tree == null) return { collection, display: null, focusedPaneId: null };
  if (preferred != null && findPane(tree, preferred) != null) {
    return { collection, display, focusedPaneId: preferred };
  }
  return { collection, display, focusedPaneId: paneLeaves(tree)[0]?.id ?? null };
}

export interface PaneLayoutController {
  /** The live tiling of what is ON SCREEN (`null` while the project has no terminal). */
  readonly layout: PaneNode | null;
  readonly focusedPaneId: string | null;
  readonly focusedTerminalId: string | null;
  /** Every group of the project — what the sidebar draws its blocks from. */
  readonly groups: readonly PaneGroup[];
  /** Whether a GROUP (rather than a solo terminal) is on screen. */
  readonly showingGroupId: string | null;
  /**
   * SHOW A WHOLE GROUP, named by its id — what activating a folder LINE in the sidebar does (human
   * review 01M14112THEB4094B2G614SXR5). Until it existed a folder could only fold, so seeing a group
   * meant opening it and clicking a member; the group itself was not something one could point at.
   *
   * THE FOCUS GOES TO THE LAST FOCUSED MEMBER, ELSE THE FIRST — and NO new state carries that.
   * {@link withFocus} already keeps `focusedPaneId` when the pane still exists in what is displayed
   * and falls back to the first leaf otherwise, so handing it the CURRENT focused pane against the
   * group's own tree IS the rule, exactly worded. Re-showing the group you are already in therefore
   * changes nothing at all.
   */
  readonly showGroup: (groupId: string) => void;
  /** Every terminal ON SCREEN — every leaf of the pane tree, not just the focused one. */
  readonly visibleTerminalIds: readonly string[];
  readonly focusPane: (paneId: string) => void;
  readonly splitPane: (paneId: string, direction: SplitDirection) => void;
  readonly closePane: (paneId: string) => void;
  /** Move a separator: `preview` shows it live, `commit` ends the interaction and persists it. */
  readonly resizeSplit: (splitId: string, ratio: number, phase: SplitResizePhase) => void;
  /** Move the focus to the geometric neighbour in `direction` (no-op without one). */
  readonly focusNeighborPane: (direction: FocusDirection) => void;
  /** Close the FOCUSED pane (the keyboard twin of the pane's ×). */
  readonly closeFocusedPane: () => void;
  /** Pull a terminal OUT of its group — it becomes solo and is shown alone. */
  readonly leaveGroup: (terminalId: string) => void;
  /** Re-order a group's members (the sidebar's drag INSIDE a block) — membership is untouched. */
  readonly reorderGroupMembers: (groupId: string, orderedIds: readonly string[]) => void;
  /** Put a terminal into a group AT a position (the sidebar's drag INTO a block). */
  readonly insertIntoGroup: (groupId: string, terminalId: string, index: number) => void;
  /** Whether the 180×120px floor allows that insertion — asked DURING the drag, before the drop. */
  readonly canInsertIntoGroup: (groupId: string, index: number) => boolean;
  /**
   * MAKE A GROUP OF TWO out of terminals that ALREADY EXIST (the sidebar's fifth gesture, human
   * review 01M1313GN2ESW0BCM8S1G0ZY2P). No terminal is created; `terminalId` is the one being
   * dragged and ends up shown.
   */
  readonly createGroupWith: (terminalId: string, partnerId: string) => void;
  /** Whether the 180×120px floor allows a group to be BORN here — asked DURING the drag. */
  readonly canCreateGroup: () => boolean;
  /**
   * RESTRUCTURE the displayed tree: move the pane `draggedPaneId` next to `targetPaneId`, dropping
   * it on one of the target's FOUR edges (the view's drag, VS Code motif). A pure LAYOUT edit —
   * no terminal is created, closed or refocused away; the moved pane keeps its id and takes the
   * focus. Refused (tree untouched, message shown) when the result would put a pane under the
   * absolute floor.
   */
  readonly movePane: (draggedPaneId: string, targetPaneId: string, edge: PaneEdge) => void;
  /** Whether that drop clears the floor — asked DURING the drag, before the drop. */
  readonly canMovePane: (draggedPaneId: string, targetPaneId: string, edge: PaneEdge) => boolean;
  /** Dissolve a group — every member becomes solo ("Dégrouper tout"). */
  readonly dissolveGroup: (groupId: string) => void;
  /** Set (or clear, with `null`) a group's optional name. */
  readonly renameGroup: (groupId: string, name: string | null) => void;
  /** Set (or clear, with `null`) a group's optional colour. */
  readonly recolorGroup: (groupId: string, color: GroupColor | null) => void;
}

/**
 * WHICH workspaces answered the current runtime read — the difference between "this terminal is
 * GONE" and "nobody answered for its workspace".
 *
 * `use-project-runtime.ts:66` omits a REJECTED `workspace_runtime_snapshot` BY DESIGN ("a single
 * workspace failure must not blank the others"), and `db_command.rs:936` rejects exactly one
 * workspace at a time (`unknown workspace {id}` — deleting a worktree in another window is enough).
 * The flat terminal list handed to {@link usePaneLayout} is therefore INCOMPLETE, not empty, and a
 * reconcile that reads it as the truth prunes leaves whose terminals are alive.
 */
export interface RuntimeCoverage {
  /** The workspaces whose snapshot RESOLVED on the current read. */
  readonly resolvedWorkspaceIds: ReadonlySet<string>;
  /** Where each terminal was last SEEN, accumulated over this session's snapshots. */
  readonly workspaceOfTerminal: ReadonlyMap<string, string>;
  /** `true` when EVERY workspace of the project answered ⇒ the flat list IS exhaustive. */
  readonly complete: boolean;
}

/**
 * The ids of `candidateIds` that the live list does not carry AND that no answering workspace
 * disowned — the leaves a reconcile must KEEP because their absence is an absence of SIGNAL.
 *
 * A complete read (or no coverage claim at all) yields none: the live list is then exhaustive and
 * an absent terminal really is gone, which is the pre-existing behaviour, untouched. Under a
 * partial read, a terminal is spared when its workspace stayed silent — or when we have never seen
 * it, since we then cannot tell WHICH workspace owes it to us. Everything else still prunes: a
 * terminal whose own workspace answered without it is dead, silent neighbours or not.
 */
export function unreportedTerminalIds(
  candidateIds: readonly string[],
  liveIds: readonly string[],
  coverage: RuntimeCoverage | null,
): string[] {
  if (coverage == null || coverage.complete) return [];
  const live = new Set(liveIds);
  return candidateIds.filter((id) => {
    if (live.has(id)) return false;
    const workspaceId = coverage.workspaceOfTerminal.get(id);
    return workspaceId == null || !coverage.resolvedWorkspaceIds.has(workspaceId);
  });
}

/**
 * `use-pane-layout` — the Terminals page's TILING state: which terminals are on screen, where, and
 * which pane is focused. Since the groups model (review 01M0ABR6V501D0SPR2BYBVZC4A) that is a
 * COLLECTION of groups plus solo terminals rather than one permanent tree:
 *
 *  - RESTORE: the project's persisted GROUPS, reconciled against the live terminals, as soon as
 *    the runtime snapshot brings its first terminal (see `restorePaneCollection`), then the
 *    remembered terminal is SHOWN (its group, or itself alone);
 *  - SELECT: picking a terminal in the sidebar shows its group (focusing that member) or shows it
 *    ALONE. It NEVER substitutes a terminal into the focused pane — that v1 behaviour is what made
 *    the tiling inescapable and it has no caller left;
 *  - SPLIT: REFUSE outright when either half would fall under the absolute pane floor
 *    (180×120px — `canSplitPane`), with a visible message and NO terminal created; otherwise
 *    create the terminal FIRST and add the leaf ONLY on success. A split from a SOLO terminal
 *    CREATES a group of two; a split inside a group adds a leaf to that group's tree;
 *  - CLOSE PANE: a pure layout edit — the terminal keeps running and simply becomes solo; a group
 *    left with one member dissolves and that survivor is shown alone;
 *  - RECONCILE: a terminal that really disappeared loses its leaf (its tree compacts, and its group
 *    dissolves if it drops below two members).
 *
 * The collection is PERSISTED per project on every change (local UI preference, versioned JSON).
 */
export function usePaneLayout({
  projectId,
  terminals,
  workspaces,
  createTerminal,
  measureHost = measurePaneHost,
  resolvedWorkspaceIds = null,
}: {
  readonly projectId: string;
  /** Every live terminal of the entered project (the flat runtime list). */
  readonly terminals: readonly WorkspaceRuntimeTerminal[];
  readonly workspaces: readonly SplitWorkspace[];
  /** Create a terminal in `workspaceId` at `path`, resolving to its RECORD id. */
  readonly createTerminal: (workspaceId: string, path: string) => Promise<string>;
  /** The tiling host's px box — the yardstick of the ABSOLUTE pane floor. Injectable for tests. */
  readonly measureHost?: () => HostSize | null;
  /**
   * The workspaces whose `workspace_runtime_snapshot` RESOLVED on the read that produced
   * `terminals` — i.e. `useProjectRuntime`'s `runtimeByWorkspace` keys. It is what makes
   * `terminals` readable as an exhaustive list (every workspace in) or as a partial one (a
   * rejected snapshot is silently omitted, `use-project-runtime.ts:66`), and therefore what
   * separates "this terminal is gone" from "nobody answered for it" — see {@link RuntimeCoverage}.
   *
   * `null` (the default) is NO coverage claim: `terminals` is then taken as exhaustive, which is
   * the behaviour that predates the partial-snapshot fix. The page passes the real set.
   */
  readonly resolvedWorkspaceIds?: ReadonlySet<string> | null;
}): PaneLayoutController {
  const { selected, select } = useRuntimeSelection();
  const [state, setState] = useState<PaneLayoutState>({
    collection: EMPTY_COLLECTION,
    display: null,
    focusedPaneId: null,
  });

  // Live mirrors so the imperative handlers below read the CURRENT state/terminals without being
  // re-created on every snapshot push (a churning callback would remount nothing, but it would
  // defeat the memoized host).
  const stateRef = useRef(state);
  stateRef.current = state;
  const terminalsRef = useRef(terminals);
  terminalsRef.current = terminals;
  const workspacesRef = useRef(workspaces);
  workspacesRef.current = workspaces;
  const createTerminalRef = useRef(createTerminal);
  createTerminalRef.current = createTerminal;
  const measureHostRef = useRef(measureHost);
  measureHostRef.current = measureHost;

  // WHERE each terminal was last SEEN, accumulated (never forgotten) across this session's
  // snapshots. It is what lets a leaf whose terminal vanished be traced back to the workspace that
  // owes it — the whole difference between a CLOSED terminal and an UNANSWERED one.
  const workspaceOfTerminalRef = useRef<Map<string, string>>(new Map());
  for (const t of terminals) {
    if (t.workspace_id != null) workspaceOfTerminalRef.current.set(t.id, t.workspace_id);
  }
  const coverageRef = useRef<RuntimeCoverage | null>(null);
  coverageRef.current =
    resolvedWorkspaceIds == null
      ? null
      : {
          resolvedWorkspaceIds,
          workspaceOfTerminal: workspaceOfTerminalRef.current,
          // The project's OWN workspaces are the yardstick: the read is exhaustive only when every
          // one of them answered.
          complete: workspaces.every((w) => resolvedWorkspaceIds.has(w.id)),
        };

  const liveIds = useMemo(() => terminals.map((t) => t.id), [terminals]);
  const liveKey = liveIds.join(",");
  // A STABLE dep for the reconcile: a workspace can recover (or fall silent) without the live id
  // list moving at all, and that flip changes what may be pruned.
  const coverageKey =
    resolvedWorkspaceIds == null ? "*" : [...resolvedWorkspaceIds].sort().join(",");
  const selectedTerminalId = selected?.kind === "terminal" ? selected.id : null;

  const layout = useMemo(
    () => displayTree(state.collection, state.display),
    [state.collection, state.display],
  );

  const focusedTerminalId = useMemo(() => {
    if (layout == null || state.focusedPaneId == null) return null;
    return findPane(layout, state.focusedPaneId)?.terminalId ?? null;
  }, [layout, state.focusedPaneId]);

  // RESTORE, once, as soon as the project has at least one live terminal (the runtime snapshot
  // arrives after the first render). Before that there is nothing valid to restore ONTO — and
  // restoring an empty collection would immediately persist it over the stored groups.
  const restoredRef = useRef(false);
  useEffect(() => {
    if (restoredRef.current || liveIds.length === 0) return;
    restoredRef.current = true;
    // The measured host goes IN: it is what lets the restore pull a hand-edited ratio back into the
    // band a drag could have reached (review round 3). An unmounted/unmeasured tiling hands `null`
    // and the restore stays exactly what it was.
    // The stored leaves the CURRENT read cannot disown (their workspace stayed silent, or we have
    // never seen them) are protected alongside the live ones: restoring against a partial first
    // snapshot would otherwise drop them for good, the restore being one-shot.
    const stored = loadPaneCollection(projectId);
    const spared = unreportedTerminalIds(
      stored == null ? [] : groupedTerminalIds(stored),
      liveIds,
      coverageRef.current,
    );
    // `stored` is handed BACK IN: the restore would otherwise re-read the key, re-parse it and
    // re-walk every tree through `isPaneCollection` a second time, on the very mount that also
    // spawns the terminals — and the two reads could disagree if the key moved between them.
    const collection = restorePaneCollection(
      projectId,
      [...liveIds, ...spared],
      measureHostRef.current(),
      stored,
    );
    // WHAT to show is the same hint the single-pane shell uses: this project's last active
    // terminal, when it is still live, else the first one. `showTerminal` then decides between its
    // group and showing it alone — the one navigation rule, reused rather than re-derived.
    const remembered = loadLastTerminalId(projectId);
    const target = remembered != null && liveIds.includes(remembered) ? remembered : liveIds[0];
    const shown = showTerminal(collection, target);
    setState({ collection, display: shown.display, focusedPaneId: shown.focusedPaneId });
    // `liveKey` (not `liveIds`) so a snapshot re-push with the same ids does not re-run this.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId, liveKey]);

  // A terminal CREATED by a split is in a group before the runtime snapshot has re-pulled, so the
  // reconcile below must not mistake it for a stale leaf. It is protected for exactly ONE snapshot
  // push: if it still has not appeared by the next one, the creation is lost and its pane goes.
  const pendingTerminalsRef = useRef<Set<string>>(new Set());

  // RECONCILE the collection with the live terminal set: a terminal closed by the user, by an agent
  // over MCP or in another window loses its leaf, its tree COMPACTS, and a group left with one
  // member dissolves (that member becoming solo).
  //
  // An EMPTY live set is NOT a reconcile signal — it is an ABSENCE of signal. `useProjectRuntime`
  // silently drops a rejected `workspace_runtime_snapshot`, so an unavailable/restarting sidecar
  // makes the flat terminal list fall to `[]` while every terminal is still alive. Reconciling
  // against it would prune EVERY leaf, dissolve every group, and the persist effect below would
  // immediately `removeItem` the project's stored groups — an unrecoverable loss, since the restore
  // runs once. So we keep the collection untouched and wait for a snapshot that actually carries
  // terminals (the page already shows "No terminals to show." on an empty runtime list — the
  // `EmptyHint` branch of `runtime-pane-host.tsx`, so nothing stale is painted meanwhile).
  //
  // A PARTIAL live set is the SAME absence, one notch finer, and it is the common one: the drop is
  // per WORKSPACE (`use-project-runtime.ts:66`) and the backend rejects workspaces one at a time
  // (`db_command.rs:936` — deleting a worktree in another window is enough), so the usual shape of
  // the failure is an INCOMPLETE list, which the guard above cannot see. `unreportedTerminalIds`
  // therefore spares the leaves no ANSWERING workspace disowned, and nothing else: a terminal whose
  // own workspace answered without it is still pruned, silent neighbours or not.
  useEffect(() => {
    if (!restoredRef.current || liveIds.length === 0) return;
    const pending = pendingTerminalsRef.current;
    pendingTerminalsRef.current = new Set();
    const current = stateRef.current;
    // A PARTIAL read is the same absence of signal as a blank one, one notch finer: only the
    // leaves an ANSWERING workspace failed to list are really gone (see `unreportedTerminalIds`).
    const protectedIds = [
      ...liveIds,
      ...pending,
      ...unreportedTerminalIds(
        groupedTerminalIds(current.collection),
        liveIds,
        coverageRef.current,
      ),
    ];
    const collection = pruneCollection(current.collection, protectedIds);
    // The DISPLAY may have died with it: a solo terminal that was closed, or a group that
    // dissolved. Fall back to the focused terminal when it survived, else the first live one.
    const displayed =
      current.display == null
        ? null
        : current.display.kind === "solo"
          ? protectedIds.includes(current.display.terminalId)
            ? current.display
            : null
          : findGroup(collection, current.display.groupId) != null
            ? current.display
            : null;
    if (collection === current.collection && displayed === current.display) return;
    if (displayed != null) {
      setState(withFocus(collection, displayed, current.focusedPaneId));
      return;
    }
    // `liveIds[0]` exists: the `liveIds.length === 0` guard opening this effect returned already.
    const fallback =
      current.display != null &&
      current.display.kind === "solo" &&
      protectedIds.includes(current.display.terminalId)
        ? current.display.terminalId
        : liveIds[0];
    const shown = showTerminal(collection, fallback);
    setState({ collection, display: shown.display, focusedPaneId: shown.focusedPaneId });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [liveKey, coverageKey]);

  // PERSIST every change (after the restore, so the initial empty collection never wipes the stored
  // groups) — EXCEPT a mid-drag preview: a separator streams one state per frame and only its final
  // `commit` deserves a write. The commit flips `preview` back to false, so it re-runs this effect
  // even when the collection itself did not change on that last frame.
  useEffect(() => {
    if (!restoredRef.current || state.preview) return;
    savePaneCollection(projectId, state.collection);
  }, [projectId, state.collection, state.preview]);

  // The sidebar SELECTION mirrored onto the tiling — the ONE navigation rule: a terminal picked in
  // the tree shows its GROUP (focused on that member) or shows ALONE. Nothing is ever substituted
  // into another pane.
  useEffect(() => {
    if (selectedTerminalId == null || !liveIds.includes(selectedTerminalId)) return;
    const current = stateRef.current;
    // Already the focused terminal of what is on screen ⇒ nothing to do (avoids re-minting a solo
    // pane id, which would churn the tiling on every snapshot push).
    const tree = displayTree(current.collection, current.display);
    if (tree != null && current.focusedPaneId != null) {
      const focusedLeaf = findPane(tree, current.focusedPaneId);
      if (focusedLeaf?.terminalId === selectedTerminalId) return;
    }
    const shown = showTerminal(current.collection, selectedTerminalId, current.display);
    setState({
      collection: current.collection,
      display: shown.display,
      focusedPaneId: shown.focusedPaneId,
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedTerminalId, liveKey]);

  // FOCUS a pane (pointer press / focus-in): the sidebar follows THIS terminal.
  const focusPane = useCallback(
    (paneId: string) => {
      const current = stateRef.current;
      const tree = displayTree(current.collection, current.display);
      if (tree == null || current.focusedPaneId === paneId) return;
      const leaf = findPane(tree, paneId);
      if (leaf == null) return;
      setState({ ...current, focusedPaneId: paneId, preview: false });
      select({ kind: "terminal", id: leaf.terminalId });
    },
    [select],
  );

  // SPLIT: resolve the workspace, CREATE the terminal, and only THEN place the leaf. The order is
  // the atomicity: a refused/failed creation returns before any layout mutation, so nothing is
  // ever left holding a pane with no terminal.
  const splitPane = useCallback(
    (paneId: string, direction: SplitDirection) => {
      void (async () => {
        const current = stateRef.current;
        const tree = displayTree(current.collection, current.display);
        if (tree == null || current.display == null) return;
        const pane = findPane(tree, paneId);
        if (pane == null) return;

        // THE ABSOLUTE FLOOR, checked BEFORE anything is created: a split that would leave either
        // half under 180×120px is REFUSED outright. The refusal is deliberately the FIRST thing in
        // the sequence, so `createTerminal` is never called and the backend receives nothing —
        // there is no terminal to orphan and nothing is touched. A refusal is never silent: the
        // user aimed at a command and must be told why nothing happened.
        if (!canSplitPane(tree, paneId, direction, measureHostRef.current())) {
          toast.error(
            direction === "row"
              ? `This pane is too narrow to split — a pane needs at least ${MIN_PANE_WIDTH_PX}px of width.`
              : `This pane is too short to split — a pane needs at least ${MIN_PANE_HEIGHT_PX}px of height.`,
          );
          return;
        }

        const source = terminalsRef.current.find((t) => t.id === pane.terminalId) ?? null;
        const target = resolveSplitWorkspace(source, workspacesRef.current);
        if (target == null) {
          toast.error("No workspace in this project — add one before splitting a pane.");
          return;
        }

        let terminalId: string;
        try {
          terminalId = await createTerminalRef.current(target.id, target.path);
        } catch (e) {
          // The sidecar's own sentence (workspace gone, spawn refused, quota) is the only useful
          // thing here — the fallback only speaks when the rejection carries no message.
          toast.error(errorText(e, "Could not create the terminal for the new pane."));
          return;
        }

        // THE TERMINAL EXISTS FROM HERE ON, whatever happens to the LAYOUT — and that asymmetry is
        // the one thing the four fall-throughs below must not swallow. The `await` yielded, so the
        // page may have moved underneath: the display switched to another solo terminal (a snapshot
        // push, a deep-link, a click), a reconcile pruned the pane being split, the group dissolved.
        // Each of those made this function `return` in silence, and the user was left with a spawned
        // PTY, no split, and no sentence — the same "nothing happened" a refused split shows, except
        // that here something did.
        //
        // So the abandonment is ANNOUNCED, and it names BOTH facts: the pane is not there, the
        // terminal is. It deliberately does NOT re-select the new terminal — the common cause of
        // getting here is the user navigating elsewhere ON PURPOSE while the spawn was in flight,
        // and yanking the selection back would fight the very gesture that caused this.
        const abandonSplit = () =>
          toast.error(
            "The pane could not be added — the terminal was created and is in the sidebar.",
          );

        // Re-read the state: the await above yielded, and a reconcile may have run meanwhile.
        const latest = stateRef.current;
        const latestTree = displayTree(latest.collection, latest.display);
        if (latest.display == null || latestTree == null) return abandonSplit();
        if (findPane(latestTree, paneId) == null) return abandonSplit();
        const created = newPaneId();

        if (latest.display.kind === "solo") {
          // A split from a SOLO terminal is how a group is BORN: the two terminals become its
          // first two members and the group takes the screen.
          const born = createGroupFromSplit(latest.collection, {
            terminalId: latest.display.terminalId,
            newTerminalId: terminalId,
            direction,
            paneId,
            newPaneId: created,
          });
          if (born == null) return abandonSplit();
          pendingTerminalsRef.current.add(terminalId);
          setState({
            collection: born.collection,
            display: { kind: "group", groupId: born.group.id },
            focusedPaneId: born.paneId,
          });
          select({ kind: "terminal", id: terminalId });
          return;
        }

        const next = splitInGroup(latest.collection, {
          groupId: latest.display.groupId,
          paneId,
          direction,
          terminalId,
          newPaneId: created,
        });
        if (next == null) return abandonSplit();
        // Shield the brand-new terminal from the reconcile until the snapshot lists it.
        pendingTerminalsRef.current.add(terminalId);
        setState({
          collection: next.collection,
          display: latest.display,
          focusedPaneId: next.paneId,
        });
        select({ kind: "terminal", id: terminalId });
      })();
    },
    [select],
  );

  // CLOSE A PANE — a LAYOUT edit only: the terminal keeps running (its instance, PTY and record are
  // untouched; the sidebar still lists it) and simply becomes SOLO. The freed space goes to the
  // sibling subtree, which also takes the focus when the closed pane had it.
  //
  // ON A SOLO TERMINAL THIS IS INERT (the arbitrage of the shortcuts finding): a solo terminal has
  // exactly one pane, and "closing" it would leave the main area showing nothing while the terminal
  // is still alive in the sidebar — a state with no way back that the user never asked for. Closing
  // the TERMINAL is a different, existing action (the row's ×, Ctrl+W).
  const closePane = useCallback(
    (paneId: string) => {
      const current = stateRef.current;
      if (current.display == null || current.display.kind === "solo") return;
      const group = findGroup(current.collection, current.display.groupId);
      if (group == null) return;
      const nextFocus =
        current.focusedPaneId === paneId
          ? paneAfterClose(group.tree, paneId)
          : current.focusedPaneId;
      const collection = closeGroupPane(current.collection, group.id, paneId);
      if (collection === current.collection) return;
      // The group may have DISSOLVED (it was a pair): show the survivor alone.
      const survivor = findGroup(collection, group.id);
      if (survivor == null) {
        const closed = findPane(group.tree, paneId)?.terminalId ?? null;
        const remaining = paneLeaves(group.tree)
          .map((leaf) => leaf.terminalId)
          .find((id) => id !== closed);
        if (remaining == null) return;
        const shown = showTerminal(collection, remaining);
        setState({ collection, display: shown.display, focusedPaneId: shown.focusedPaneId });
        select({ kind: "terminal", id: remaining });
        return;
      }
      const next = withFocus(collection, current.display, nextFocus);
      setState(next);
      const leaf = next.focusedPaneId == null ? null : findPane(survivor.tree, next.focusedPaneId);
      if (leaf != null) select({ kind: "terminal", id: leaf.terminalId });
    },
    [select],
  );

  // RESIZE a split — the separators' only transition, and a pure LAYOUT edit: no terminal is
  // created, closed or refocused, and the PTY sizes are NOT touched here. Moving the ratio moves
  // the panes' boxes, and each terminal's own ResizeObserver → FitAddon → `pty_resize` derives the
  // new grid from that box — the existing and only geometry pipeline (no second measuring path).
  //
  // A `preview` frame is applied but flagged so the persist effect skips it; the `commit` that
  // ends the interaction clears the flag and is the single write. `setSplitRatio` clamps and
  // treats an unknown split id as a no-op, so nothing here can produce an out-of-band ratio.
  const resizeSplit = useCallback((splitId: string, ratio: number, phase: SplitResizePhase) => {
    setState((current) => {
      // Only a GROUP has splits — a solo terminal is one leaf and has no separator to move.
      if (current.display == null || current.display.kind !== "group") return current;
      const group = findGroup(current.collection, current.display.groupId);
      if (group == null) return current;
      const tree = setSplitRatioTree(group.tree, splitId, ratio);
      const preview = phase === "preview";
      // Nothing moved AND the phase is unchanged ⇒ no re-render at all (a drag that jitters within
      // one pixel, or a key pressed against a bound, must not churn the tree).
      if (tree === group.tree && !!current.preview === preview) return current;
      const groups = current.collection.groups.map((g) => (g.id === group.id ? { ...g, tree } : g));
      return { ...current, collection: { groups }, preview };
    });
  }, []);

  /** Apply a pure collection edit and re-pin what is displayed onto `showId`. */
  const applyAndShow = useCallback(
    (collection: PaneCollection, showId: string) => {
      const shown = showTerminal(collection, showId);
      setState({ collection, display: shown.display, focusedPaneId: shown.focusedPaneId });
      select({ kind: "terminal", id: showId });
    },
    [select],
  );

  // LEAVE the group — the row's "Sortir du groupe". The terminal quits its group's tree (which
  // compacts, and dissolves when only one member is left), becomes SOLO, and is shown alone.
  const leaveGroup = useCallback(
    (terminalId: string) => {
      const current = stateRef.current;
      const collection = detachTerminal(current.collection, terminalId);
      if (collection === current.collection) return;
      applyAndShow(collection, terminalId);
    },
    [applyAndShow],
  );

  // JOIN — there is no separate one (review 01M0BSNTEN5N2GC9QQBASJYHHB). It existed for the sidebar
  // row's "Rejoindre" menu entry, which is gone: joining a group IS the drag now (gesture 3), and it
  // lands where it was DROPPED. `insertIntoGroup` below covers the old "automatic position" as its
  // end-index case, floor check included — see `pane-groups.ts`.

  // DISSOLVE — the panel's "Dégrouper tout": every member becomes solo. What stays on screen is
  // the terminal that was focused, now alone.
  const dissolveGroup = useCallback(
    (groupId: string) => {
      const current = stateRef.current;
      const group = findGroup(current.collection, groupId);
      if (group == null) return;
      const collection = dissolveGroupIn(current.collection, groupId);
      const focusedLeaf =
        current.focusedPaneId == null ? null : findPane(group.tree, current.focusedPaneId);
      const show = focusedLeaf?.terminalId ?? paneLeaves(group.tree)[0].terminalId;
      applyAndShow(collection, show);
    },
    [applyAndShow],
  );

  // RE-ORDER a group's members — the sidebar's gesture 1 (drop a row elsewhere inside its own
  // block). Membership is untouched and the tree keeps its structure and its ratios: only which
  // terminal sits in which rectangle changes, so a drag in the list never rebuilds a tiling the
  // user has already sized. What is displayed is left exactly as it is.
  //
  // THE FOCUS FOLLOWS THE TERMINAL, NOT THE RECTANGLE (review finding 01M11WR6JTPGAP4ABK0RWXB6XK).
  // `withMemberOrder` permutes the OCCUPANTS while FREEZING the pane ids, so re-conducting
  // `focusedPaneId` verbatim hands the keyboard — and the focus ring, and the Spec surcouche — to
  // whichever terminal the permutation moved INTO that rectangle, while the sidebar stays selected
  // on the previous one: three surfaces, three different answers to "which terminal am I on". So we
  // read the focused pane's occupant BEFORE the permutation and re-pin the focus on the rectangle
  // that hosts that SAME terminal after it. The ARBITRAGE this assumes: the focus ring travels with
  // the terminal across the tiling (rather than staying on the rectangle the user last clicked) —
  // which is precisely what re-aligns the sidebar selection, since that selection names a terminal.
  //
  // A reorder of a group that is NOT on screen leaves the displayed tree untouched, so the lookup
  // resolves to the very same pane and this is a no-op — no special case needed.
  const reorderGroupMembers = useCallback((groupId: string, orderedIds: readonly string[]) => {
    setState((current) => {
      const collection = orderGroupMembersIn(current.collection, groupId, orderedIds);
      if (collection === current.collection) return current;
      const before = displayTree(current.collection, current.display);
      const occupant =
        before == null || current.focusedPaneId == null
          ? null
          : (findPane(before, current.focusedPaneId)?.terminalId ?? null);
      // `preview: false` — a spread of `current` would CARRY a stale mid-drag flag forward and the
      // persist effect would skip this write. `preview` is only ever cleared by the `commit` that
      // ends a separator gesture, and a gesture can end WITHOUT one (the separator unmounts while
      // the pointer is down — a reconcile pruned a terminal, so no pointerup/cancel/lostcapture
      // reaches the handle). This transition is not a preview of anything, so it says so.
      if (occupant == null) return { ...current, collection, preview: false };
      const after = displayTree(collection, current.display);
      const pane = after == null ? null : findPaneByTerminal(after, occupant);
      return {
        ...current,
        collection,
        focusedPaneId: pane?.id ?? current.focusedPaneId,
        preview: false,
      };
    });
  }, []);

  /**
   * Whether a terminal may be dropped INTO `groupId` at `index` — the ABSOLUTE FLOOR, asked on the
   * exact anchor the insertion would split. It is the SAME question `splitPane` asks (`canSplitPane`
   * on the same pane and direction), which is what makes "entering a group" the split's path rather
   * than a second way in. The sidebar calls it DURING the drag so the refusal is visible before the
   * release, not after it.
   */
  const canInsertIntoGroup = useCallback((groupId: string, index: number) => {
    const group = findGroup(stateRef.current.collection, groupId);
    if (group == null) return false;
    const anchor = groupInsertAnchor(group, index);
    return canSplitPane(group.tree, anchor.paneId, anchor.direction, measureHostRef.current());
  }, []);

  /**
   * THE BIRTH TARGET'S OWN FLOOR QUESTION (human review 01M1313GN2ESW0BCM8S1G0ZY2P).
   *
   * A group of two born in the sidebar is, geometrically, the frame split once — so the question is
   * the SAME one `splitPane` asks, on the same predicate, at the same direction the birth will use.
   * Nothing here is a second rule: `canSplitPane` over a one-leaf probe tree IS "can this host hold
   * two panes side by side". Asked DURING the drag, so the dotted slot can refuse before the release
   * instead of the drop failing silently after it.
   */
  const canCreateGroup = useCallback(() => {
    const probe = createPaneLayout(BIRTH_PROBE_TERMINAL, BIRTH_PROBE_PANE);
    return canSplitPane(
      probe,
      BIRTH_PROBE_PANE,
      nextAppendDirection(probe),
      measureHostRef.current(),
    );
  }, []);

  /**
   * MAKE A GROUP OUT OF TWO TERMINALS THAT ALREADY EXIST — the fifth gesture, mounted on the
   * sidebar (human review 01M1313GN2ESW0BCM8S1G0ZY2P). Verbatim: « si je veux créer un groupe avec
   * les terminaux déjà existant sans avoir de groupe préalable comment ça se passe ? ». It could
   * not: a terminal group was born from a SPLIT and from nothing else, so two terminals that were
   * already there had no way of being put together.
   *
   * THIS IS NOT A SECOND BIRTH PATH. It calls {@link createGroupFromSplit} — the one and only birth
   * — with the direction a group of two is laid out in anyway (`nextAppendDirection` of a single
   * leaf), so the pair comes out as a tree indistinguishable from a pair born of a pointer split.
   * No terminal is created: both already exist, which is the whole point of the gesture.
   *
   * THE DETACH IS THE MODEL'S OWN RULE, not a special case: a terminal belongs to at most one group,
   * so a member dropped on the birth target leaves its old group (which dissolves if it was a pair)
   * and founds the new one, in one gesture. `createGroupFromSplit` REFUSES a terminal that is still
   * placed somewhere, which is exactly why the detach happens first rather than after.
   */
  const createGroupWith = useCallback(
    (terminalId: string, partnerId: string) => {
      if (terminalId === partnerId) return;
      const current = stateRef.current;
      // The floor is asked one last time at the commit — the host may have been resized while the
      // row was in the air — and ANNOUNCED, because this is where the gesture ends. `splitPane`'s
      // own refusal is the sentence: a birth is a split of the frame.
      if (!canCreateGroup()) {
        toast.error(
          `This project's pane is too narrow for a group — a pane needs at least ${MIN_PANE_WIDTH_PX}px of width.`,
        );
        return;
      }
      const detached = detachTerminal(detachTerminal(current.collection, terminalId), partnerId);
      const born = createGroupFromSplit(detached, {
        terminalId: partnerId,
        newTerminalId: terminalId,
        direction: nextAppendDirection(createPaneLayout(partnerId, BIRTH_PROBE_PANE)),
      });
      if (born == null) return;
      // What stays on screen is the terminal the HAND was holding, in the group it just founded.
      applyAndShow(born.collection, terminalId);
    },
    [applyAndShow, canCreateGroup],
  );

  // ENTER a group at a POSITION — the sidebar's gesture 3. The floor is re-asked here so a commit
  // can never slip past it, but it is NOT announced again: the refusal was already shown during the
  // drag, and nothing moved.
  const insertIntoGroup = useCallback(
    (groupId: string, terminalId: string, index: number) => {
      if (!canInsertIntoGroup(groupId, index)) return;
      const current = stateRef.current;
      const inserted = insertIntoGroupIn(current.collection, groupId, terminalId, index);
      if (inserted == null) return;
      applyAndShow(inserted.collection, terminalId);
    },
    [applyAndShow, canInsertIntoGroup],
  );

  /**
   * The CANDIDATE tree a view-drop would produce — the one place the gesture becomes a tree
   * operation, shared by `canMovePane` (during the hover) and `movePane` (at the drop), so both
   * judge the SAME result. `null` when there is nothing to restructure (no group on screen, an
   * unknown pane, the pane dropped onto itself).
   */
  const moveCandidate = useCallback(
    (draggedPaneId: string, targetPaneId: string, edge: PaneEdge): PaneNode | null => {
      const current = stateRef.current;
      if (current.display == null || current.display.kind !== "group") return null;
      const tree = displayTree(current.collection, current.display);
      if (tree == null) return null;
      return movePaneToEdge(tree, draggedPaneId, targetPaneId, edge);
    },
    [],
  );

  // Whether a view-drop clears the floor — asked DURING the drag so the indicator never shows for
  // a drop that would be refused. The question is about the RESULT (`layoutFitsFloor` on the
  // candidate tree at the measured host), not about the target alone: detaching the dragged leaf
  // first GROWS its neighbour, and the answer must price the layout the user would actually get.
  const canMovePane = useCallback(
    (draggedPaneId: string, targetPaneId: string, edge: PaneEdge) => {
      const candidate = moveCandidate(draggedPaneId, targetPaneId, edge);
      if (candidate == null) return false;
      return layoutFitsFloor(candidate, measureHostRef.current());
    },
    [moveCandidate],
  );

  // MOVE A PANE — the view's restructure gesture. The floor is RE-ASKED here so a commit can never
  // slip past it (the same belt-and-braces as `insertIntoGroup`); the refusal is announced, since
  // the release itself gives no other feedback that nothing happened. No terminal is created,
  // closed or detached from the store: the leaf changes PLACE, keeping its id — which is what the
  // focus follows — and the xterm/PTY instance behind it is untouched (the host's flat slots make
  // that a pure re-positioning, zero remount).
  const movePane = useCallback(
    (draggedPaneId: string, targetPaneId: string, edge: PaneEdge) => {
      const candidate = moveCandidate(draggedPaneId, targetPaneId, edge);
      if (candidate == null) return;
      if (!layoutFitsFloor(candidate, measureHostRef.current())) {
        toast.error(
          edge === "left" || edge === "right"
            ? `This drop would make a pane too narrow — a pane needs at least ${MIN_PANE_WIDTH_PX}px of width.`
            : `This drop would make a pane too short — a pane needs at least ${MIN_PANE_HEIGHT_PX}px of height.`,
        );
        return;
      }
      const current = stateRef.current;
      if (current.display == null || current.display.kind !== "group") return;
      const next = movePaneInGroup(
        current.collection,
        current.display.groupId,
        draggedPaneId,
        targetPaneId,
        edge,
      );
      if (next == null) return;
      setState(withFocus(next.collection, current.display, next.paneId));
      // The sidebar follows the moved pane, exactly as a pointer focus would.
      const tree = displayTree(current.collection, current.display);
      const moved = tree == null ? null : findPane(tree, draggedPaneId);
      if (moved != null) select({ kind: "terminal", id: moved.terminalId });
    },
    [moveCandidate, select],
  );

  const renameGroup = useCallback((groupId: string, name: string | null) => {
    setState((current) => {
      const collection = setGroupNameIn(current.collection, groupId, name);
      // `preview: false` for the same reason as `reorderGroupMembers` above: a rename is not a
      // mid-drag frame, and inheriting a stale flag would make the persist effect skip it.
      if (collection === current.collection) return current;
      return { ...current, collection, preview: false };
    });
  }, []);

  const recolorGroup = useCallback((groupId: string, color: GroupColor | null) => {
    setState((current) => {
      const collection = setGroupColorIn(current.collection, groupId, color);
      // Same as `renameGroup`: a recolour is a persisted edit, never a preview frame.
      if (collection === current.collection) return current;
      return { ...current, collection, preview: false };
    });
  }, []);

  // KEYBOARD FOCUS MOVE (Alt+arrows): the GEOMETRIC neighbour of the focused pane. Without one in
  // that direction the focus does not move — no wrap, no jump to the far side.
  const focusNeighborPane = useCallback(
    (direction: FocusDirection) => {
      const current = stateRef.current;
      const tree = displayTree(current.collection, current.display);
      if (tree == null || current.focusedPaneId == null) return;
      const neighbor = paneNeighbor(tree, current.focusedPaneId, direction);
      if (neighbor == null) return;
      focusPane(neighbor);
    },
    [focusPane],
  );

  const closeFocusedPane = useCallback(() => {
    const paneId = stateRef.current.focusedPaneId;
    if (paneId != null) closePane(paneId);
  }, [closePane]);

  // ACTIVATE A FOLDER: show its group, focused on the member that last held the keyboard inside it
  // (see {@link PaneLayoutController.showGroup}). The sidebar SELECTION follows the pane, exactly as
  // it does when a pane is focused with the pointer — the tree's highlight and the tiling's focus
  // have one source and this adds no second one.
  const showGroup = useCallback(
    (groupId: string) => {
      const current = stateRef.current;
      if (findGroup(current.collection, groupId) == null) return;
      const next = withFocus(current.collection, { kind: "group", groupId }, current.focusedPaneId);
      const tree = displayTree(next.collection, next.display);
      if (tree == null || next.focusedPaneId == null) return;
      setState(next);
      const leaf = findPane(tree, next.focusedPaneId);
      if (leaf != null) select({ kind: "terminal", id: leaf.terminalId });
    },
    [select],
  );

  const visible = useMemo(() => paneTerminalIds(layout), [layout]);

  return {
    layout,
    focusedPaneId: state.focusedPaneId,
    focusedTerminalId,
    groups: state.collection.groups,
    showingGroupId: state.display?.kind === "group" ? state.display.groupId : null,
    showGroup,
    visibleTerminalIds: visible,
    focusPane,
    splitPane,
    closePane,
    resizeSplit,
    focusNeighborPane,
    closeFocusedPane,
    leaveGroup,
    reorderGroupMembers,
    insertIntoGroup,
    canInsertIntoGroup,
    createGroupWith,
    canCreateGroup,
    movePane,
    canMovePane,
    dissolveGroup,
    renameGroup,
    recolorGroup,
  };
}
