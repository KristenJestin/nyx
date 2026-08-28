import { useCallback, useRef, useState } from "react";

/**
 * WHICH GROUP BLOCKS ARE FOLDED — held for a WHOLE LIST rather than inside each block.
 *
 * The obvious shape was a hook per block ("this block remembers whether it is open"), and it was
 * wrong for a reason that only shows up in the drag: a folded block UNMOUNTS its members, and
 * `group-drag.ts` has to know that to keep the block a valid drop target (`projectDrop`). A list
 * that cannot see its own blocks' fold state cannot tell the drag anything, and the state has to
 * live where the drag is wired — here.
 *
 * ## Persistence is INJECTED, not imported
 *
 * The sidebar remembers a fold across launches, through the Terminals shell's own registry
 * (`terminals/ui-persistence.ts`). The commands settings modal does not — a modal's state dies with
 * it, and reaching into `terminals/` from `command/` is the import edge finding
 * 01M0ZMHM9GSC39SCN2DBRQ8VX3 closed. So this module knows about REMEMBERING, never about WHERE, and
 * a caller with no `load`/`save` simply gets a fold that lasts as long as the screen.
 *
 * The default is the FAMILY's — open for terminals (a live workspace you look at), closed for
 * commands (a launcher you press) — and it only ever applies until the first fold: from then on the
 * group's own recorded preference wins.
 */
export interface GroupFolds {
  /** Whether this group's members are shown. */
  isOpen: (groupId: string) => boolean;
  /** Fold / unfold this group, recording the choice when the caller supplied a `save`. */
  toggle: (groupId: string) => void;
}

export function useGroupFolds({
  defaultOpen,
  load,
  save,
}: {
  defaultOpen: boolean;
  /** The recorded state of a group, or `null` when it has never been folded. */
  load?: (groupId: string) => boolean | null;
  /** Record a group's new state. */
  save?: (groupId: string, open: boolean) => void;
}): GroupFolds {
  // The states this session has SET. Anything absent falls back to what was recorded, then to the
  // family default.
  const [live, setLive] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  // Recorded states, read at most ONCE per group per mount: a storage read is synchronous, and this
  // is called during render for every block in the list.
  const recorded = useRef(new Map<string, boolean | null>());

  const resolve = useCallback(
    (groupId: string): boolean => {
      const set = live.get(groupId);
      if (set !== undefined) return set;
      if (!recorded.current.has(groupId)) {
        recorded.current.set(groupId, load?.(groupId) ?? null);
      }
      return recorded.current.get(groupId) ?? defaultOpen;
    },
    [live, load, defaultOpen],
  );

  const toggle = useCallback(
    (groupId: string) => {
      setLive((previous) => {
        const current =
          previous.get(groupId) ?? recorded.current.get(groupId) ?? load?.(groupId) ?? defaultOpen;
        const next = !current;
        save?.(groupId, next);
        return new Map(previous).set(groupId, next);
      });
    },
    [load, save, defaultOpen],
  );

  return { isOpen: resolve, toggle };
}
