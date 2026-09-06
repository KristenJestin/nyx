import { useEffect, useState } from "react";

import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { MicroLabel } from "@/components/ui/tree-primitives";
import { GROUP_OPTIONS, GroupSwatches } from "./group-swatches";
import type { GroupColor, PaneGroup } from "./pane-groups";

/**
 * THE NAME + COLOUR PANEL — ONE component, for a group of TERMINALS and a group of COMMANDS alike.
 *
 * ## The rule the user posed (review 01M0ABR6V501D0SPR2BYBVZC4A, major)
 *
 *  - NOTHING IS ASKED AT CREATION. A group is born from a split, grey and nameless, and stays
 *    usable that way indefinitely. This panel never opens by itself.
 *  - NAME AND COLOUR ARE INDEPENDENT: name without colouring, colour without naming, or both.
 *  - THE FIRST SWATCH IS NOT A COLOUR. It is the ABSENCE of a choice — the neutral `--identity-0` —
 *    and it is what a fresh group has selected when the panel opens.
 *  - CLEARING KEEPS THE GROUP. Emptying the field removes the name; taking the first swatch back
 *    removes the colour. Neither conditions the group's existence.
 *  - "Ungroup all" dissolves the group: every member becomes solo, and NOTHING is closed.
 *  - "Close all terminals" is its DESTRUCTIVE twin (human review 01M1312M70TP8TJWT4TG505S0J): the
 *    members are really closed and the group disappears with them. Offered only where there is
 *    something to close — the terminals' sidebar hands `onCloseAll`, the commands screens do not.
 *
 * ## Where it opens from
 *
 * ONE PLACE, ON BOTH SIDES (review 01M0BPP79090KAQ9ZAFJ23ZRTX): the block's own HEAD — its `…`
 * trigger (`group-block.tsx`). Nothing else opens it any more: the row's "Group…" entry and the
 * right-click on a member are gone, because the group is an entity and is settled from itself. The
 * TERMINALS sidebar, the COMMANDS sidebar and the commands settings screen all mount THIS component
 * from that one trigger — one panel, one behaviour, so a group of commands and a group of terminals
 * can never drift apart.
 *
 * ## Closing changes nothing by itself
 *
 * Every edit is committed as it is made, and an edit that would be a no-op is not emitted at all —
 * so opening the panel and closing it leaves the group untouched (the caller's collection object is
 * returned unchanged, by reference).
 */
export function GroupPanel({
  open,
  group,
  onClose,
  onRename,
  onRecolor,
  onDissolve,
  onCloseAll,
}: {
  open: boolean;
  /** The group being edited — `null` closes the panel (it has nothing to edit). */
  group: PaneGroup | null;
  onClose: () => void;
  /** Set (or clear, with `null`) the group's name. */
  onRename: (groupId: string, name: string | null) => void;
  /** Set (or clear, with `null`) the group's colour. */
  onRecolor: (groupId: string, color: GroupColor | null) => void;
  /** Dissolve the group — every member becomes solo. The members SURVIVE. */
  onDissolve: (groupId: string) => void;
  /**
   * CLOSE the group's members for real, and the group with them (human review
   * 01M1312M70TP8TJWT4TG505S0J). ABSENT ⇒ the action is not offered at all, which is how the
   * COMMANDS side stays out of it: a command group's members are project-level templates, not
   * running things this panel could close, and the finding says so explicitly — signal the
   * equivalent, do not implement it. The terminals' sidebar is the one surface that supplies it.
   */
  onCloseAll?: (groupId: string) => void;
}) {
  // The field is DRAFT state: it follows the group while the panel is closed, and the user's typing
  // while it is open. Seeded from the group's current name — empty for a group nobody named — so
  // what the user edits is what they were looking at.
  const [draft, setDraft] = useState(group?.name ?? "");
  const groupId = group?.id ?? null;
  useEffect(() => {
    if (open) setDraft(group?.name ?? "");
    // Re-seed on OPEN and on a group change, never on every keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, groupId]);

  if (group == null) return null;

  /** Commit the field: a trimmed non-empty value names the group, anything else clears the name. */
  const commitName = () => {
    const trimmed = draft.trim();
    const next = trimmed.length === 0 ? null : trimmed;
    // No-op edits are not emitted — that is what makes "open it and close it" a nothing.
    if (next !== group.name) onRename(group.id, next);
  };

  return (
    <Dialog.Root open={open} onOpenChange={(next) => (next ? undefined : onClose())}>
      <Dialog.Portal>
        <DialogBackdrop />
        <DialogPopup aria-label="Group" className="w-[17rem] max-w-[calc(100vw-2rem)] gap-0 p-0">
          <div className="flex flex-col gap-1 px-3 pt-3">
            <Dialog.Title render={<MicroLabel>Group</MicroLabel>} />
            <Input
              aria-label="Group name (optional)"
              placeholder="name (optional)"
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              onBlur={commitName}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  commitName();
                } else if (event.key === "Escape") {
                  event.preventDefault();
                  setDraft(group.name ?? "");
                }
              }}
              className="h-7 text-sm"
            />
          </div>

          <div className="flex flex-col gap-1.5 px-3 pt-3">
            <MicroLabel>Colour</MicroLabel>
            {/* THE SHARED SWATCH ROW (`group-swatches.tsx`) — the neutral-first rule and the
                "a no-op is not emitted" rule live in it, so this panel and the commands modal's
                folder detail cannot answer "which colours does a group have" differently. */}
            <GroupSwatches
              options={GROUP_OPTIONS}
              value={group.color}
              onSelect={(color) => onRecolor(group.id, color)}
            />
          </div>

          {/* THE TWO EXITS, AND THEY ARE NOT THE SAME ONE (human review 01M1312M70TP8TJWT4TG505S0J).
              Verbatim: « je peux pas supprimer tout un groupe, y a l'option un ungroup mais y pas
              l'option tout close ». The panel offered only the dissolution, so the only way to be
              rid of a group's terminals was to ungroup it and then close each survivor by hand.

              THE DISTINCTION IS THE PRD'S OWN, one level up: closing a PANE is not closing a
              TERMINAL. Its twin belongs here — DISSOLVE keeps every terminal alive and drops the
              group around them; CLOSE ALL really closes them, and the group goes with them because
              a group with no member does not exist.

              THE REGISTER MOVED WITH IT. "Ungroup all" used to wear `ghost-destructive`, which was
              the wrong word for it: nothing is lost by ungrouping, every terminal keeps running and
              the user can re-make the group with one drag. It is an ordinary action now, and the
              destructive register is spent where something is actually destroyed — which is also
              what makes the two visually distinct rather than two red rows one above the other. */}
          <div className="mt-3 flex flex-col border-t border-border px-1 py-1">
            <Button
              variant="ghost"
              size="sm"
              className="w-full justify-start"
              onClick={() => {
                onDissolve(group.id);
                onClose();
              }}
            >
              Ungroup all
            </Button>
            {/* NO CONFIRMATION, and that is a VERIFIED alignment rather than an omission: closing a
                terminal from its row's × (`workspace-runtime-tree.tsx`) and Ctrl+W both close it
                outright, with no dialog. A confirmation here would invent a rule this app does not
                have, and would make the group's terminals harder to close than the same terminals
                one at a time. */}
            {onCloseAll != null ? (
              <Button
                variant="ghost-destructive"
                size="sm"
                className="w-full justify-start"
                onClick={() => {
                  onCloseAll(group.id);
                  onClose();
                }}
              >
                Close all terminals
              </Button>
            ) : null}
          </div>
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
