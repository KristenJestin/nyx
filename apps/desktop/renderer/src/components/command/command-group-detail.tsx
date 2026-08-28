import { useEffect, useState, type CSSProperties } from "react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tooltip } from "@/components/ui/tooltip";
import { SidebarLineFamilyGlyph } from "@/components/ui/sidebar-line";
import { MicroLabel } from "@/components/ui/tree-primitives";
import { GroupSwatches } from "@/components/panes/group-swatches";
import {
  COMMAND_GROUP_SHAPES,
  type CommandGroupShape,
  type GroupColor,
} from "@/components/panes/pane-groups";
import { paneRects, MIN_PANE_HEIGHT_PX, MIN_PANE_WIDTH_PX } from "@/components/panes/pane-geometry";
import { commandLayoutFits, measureCommandHost } from "./command-floor";
import { commandGroupTree, type CommandRow } from "./command-group-tree";
import { DetailHeader, DetailSeparator } from "./command-detail-chrome";
import type { CommandGroup } from "./use-command-groups";

/**
 * THE FOLDER'S DETAIL PANEL — where a group is named, coloured, SHAPED and dissolved.
 *
 * ## What moved here, and why it had to (finding 01M132XFZ5KY8QMJ79HZZSBV3J)
 *
 * The layout editor used to live INSIDE every block of the list, three levels down (modal →
 * grouping section → block) and about 110 px per group: a project with three groups stopped being
 * readable, and the six thumbnails plus a 40 px preview were squeezed into a column that also had
 * to hold the member rows. The name and the colour were somewhere else again — in a second dialog
 * opened OVER the modal, from the block's `…`.
 *
 * Three editors for one object, in three placements. They are one panel now, and it is the panel
 * the folder's own line selects. What that buys is not tidiness: the preview has room to be 150 px
 * instead of 40 and to ANIMATE between shapes, and it sits beside the very list whose order drives
 * it — which is what makes « the order IS the disposition » observable instead of merely written.
 *
 * ## Every edit is committed as it is made
 *
 * No Save button, exactly as `group-panel.tsx` has always worked (« every edit is committed as it
 * is made »): a master/detail with a dirty state makes changing the selection a trap. A no-op edit
 * is not emitted at all, so opening a folder and closing it leaves it untouched.
 */
export function CommandGroupDetail({
  group,
  members,
  onRename,
  onRecolor,
  onPickShape,
  onDissolve,
}: {
  group: CommandGroup;
  /** The group's members, already in DISPOSITION order — the list's own ordering. */
  members: readonly CommandRow[];
  onRename: (name: string | null) => void;
  onRecolor: (color: GroupColor | null) => void;
  onPickShape: (shape: CommandGroupShape) => void;
  onDissolve: () => void;
}) {
  // DRAFT state for the name, seeded from the group and re-seeded when the SELECTION moves to
  // another folder — the same shape `group-panel.tsx` uses, so the two cannot drift.
  const [draft, setDraft] = useState(group.name ?? "");
  useEffect(() => {
    setDraft(group.name ?? "");
  }, [group.id, group.name]);

  /** Commit the field: a trimmed non-empty value names the group, anything else clears the name. */
  const commitName = () => {
    const trimmed = draft.trim();
    const next = trimmed.length === 0 ? null : trimmed;
    if (next !== group.name) onRename(next);
  };

  const tone = { "--group-tone": `var(--group-${group.color ?? 0})` } as CSSProperties;

  return (
    <div style={tone} className="flex flex-col">
      <DetailHeader
        // The FOLDER family glyph, from its one source — the same open folder the block draws.
        icon={<SidebarLineFamilyGlyph family="folder-open" className="text-[var(--group-tone)]" />}
        title={
          group.name?.trim() ? (
            <span className="text-[color-mix(in_oklch,var(--group-tone)_88%,var(--foreground))]">
              {group.name.trim()}
            </span>
          ) : (
            // NO NAME, NO WORDS (human review 01M0CY3F075ACP4TFBK1KJ49X8) — a group nobody named is
            // designated by what it IS, never by an invented noun dressed up as its name.
            <span className="text-muted-foreground">Folder</span>
          )
        }
        subtitle={`${members.length} commands, side by side. They start and stop together from the sidebar.`}
        action={
          // "Ungroup all" under its plain register: nothing is lost by dissolving — every command
          // survives and one drag re-makes the folder — so the destructive tone is NOT spent here
          // (the same arbitrage `group-panel.tsx` states).
          <Button type="button" variant="outline" size="xs" onClick={onDissolve}>
            Ungroup all
          </Button>
        }
      />

      <label className="flex flex-col gap-1" htmlFor={`group-name-${group.id}`}>
        <span className="text-xs font-medium text-foreground">Name</span>
        <Input
          id={`group-name-${group.id}`}
          aria-label="Group name (optional)"
          placeholder="Sans nom"
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
        />
        <span className="text-xs text-muted-foreground">
          Optional. A folder with no name is still a folder.
        </span>
      </label>

      <div className="mt-3.5 flex flex-col gap-1.5">
        <span className="text-xs font-medium text-foreground">Colour</span>
        {/* THE SHARED SWATCH ROW — the very control `group-panel.tsx` mounts. */}
        <GroupSwatches value={group.color} onSelect={onRecolor} />
      </div>

      <DetailSeparator />

      <MicroLabel className="mb-2">Layout</MicroLabel>
      <ShapeBar group={group} members={members} onPick={onPickShape} />
      <LayoutPreview group={group} members={members} />
      <p className="mt-2 text-xs leading-snug text-muted-foreground">
        The numbers are the members' order in the list.{" "}
        <span className="font-medium text-foreground">Drag a line inside the folder</span> to change
        it — the order IS the disposition, and under « main left » moving a line to the top promotes
        it to the big pane.
      </p>
    </div>
  );
}

/** The six shapes as they are named on screen — the closed vocabulary, in its display order. */
const SHAPE_LABEL: Record<CommandGroupShape, string> = {
  auto: "auto",
  row: "row",
  column: "column",
  grid: "grid",
  "main-left": "main left",
  "main-top": "main top",
};

/**
 * THE SHAPE SELECTOR — six thumbnails, one per entry of the closed vocabulary.
 *
 * Each thumbnail draws the REAL rectangles the shape produces (`paneRects` over `commandGroupTree`),
 * so what is clicked is what the host will lay out, not a picture of it. A shape the 180×120px floor
 * refuses at the measured host is shown STRUCK THROUGH rather than hidden (the user's arbitrage):
 * hiding it would make the vocabulary look smaller than it is and give no reason for the absence.
 */
function ShapeBar({
  group,
  members,
  onPick,
}: {
  group: CommandGroup;
  members: readonly CommandRow[];
  onPick: (shape: CommandGroupShape) => void;
}) {
  const ids = members.map((command) => command.id);
  const current = group.layout_shape;
  // Measured ONCE for the whole bar: the six thumbnails must be judged against the same host, and a
  // per-thumbnail read would also be six layout flushes on every render.
  const host = measureCommandHost();
  return (
    <>
      <div data-group-shapes={group.id} className="flex flex-wrap items-center gap-1.5">
        {COMMAND_GROUP_SHAPES.map((shape) => {
          const tree = commandGroupTree(ids, shape);
          const fits = commandLayoutFits(ids, shape, host);
          return (
            <Tooltip
              key={shape}
              label={
                fits
                  ? `Layout ${SHAPE_LABEL[shape]}`
                  : `Layout ${SHAPE_LABEL[shape]} — a pane would fall under ${MIN_PANE_WIDTH_PX}×${MIN_PANE_HEIGHT_PX}px`
              }
            >
              <button
                type="button"
                data-layout-shape={shape}
                data-layout-shape-refused={fits ? undefined : "true"}
                aria-label={`Layout ${SHAPE_LABEL[shape]}`}
                aria-pressed={current === shape}
                disabled={!fits}
                onClick={() => onPick(shape)}
                className={cn(
                  "relative h-8 w-11.5 shrink-0 cursor-pointer rounded-sm border border-border bg-muted/50",
                  "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring",
                  current === shape && "border-[var(--group-tone)] ring-1 ring-[var(--group-tone)]",
                  !fits && "cursor-not-allowed opacity-30",
                )}
              >
                {tree == null
                  ? null
                  : paneRects(tree).map((placement) => (
                      <i
                        key={placement.paneId}
                        aria-hidden
                        style={{
                          position: "absolute",
                          left: `${placement.rect.left}%`,
                          top: `${placement.rect.top}%`,
                          width: `${placement.rect.width}%`,
                          height: `${placement.rect.height}%`,
                        }}
                        className="rounded-[1px] bg-[color-mix(in_oklch,var(--group-tone)_42%,transparent)] outline outline-[0.5px] -outline-offset-[0.5px] outline-[color-mix(in_oklch,var(--group-tone)_60%,transparent)]"
                      />
                    ))}
                {/* STRUCK THROUGH, NOT HIDDEN: the hatch says "this one exists and does not fit
                    here", which a missing thumbnail could not. */}
                {fits ? null : (
                  <i
                    aria-hidden
                    className="absolute inset-0 rounded-[1px] bg-[repeating-linear-gradient(45deg,transparent_0_3px,color-mix(in_oklch,var(--destructive)_45%,transparent)_3px_4px)]"
                  />
                )}
              </button>
            </Tooltip>
          );
        })}
      </div>
      <p className="mt-2 text-xs text-muted-foreground tabular-nums">
        {members.length} panes ·{" "}
        <span className="font-medium text-foreground">{SHAPE_LABEL[current]}</span>
      </p>
    </>
  );
}

/**
 * THE DISPOSITION, AS A READ-ONLY PREVIEW (finding 01M11XTRXVVPZ9XMDW5ARWKWHA).
 *
 * These cells used to BE the handle — pick one, pick another, they swap — and the user's verdict was
 * that nothing said you were mid-gesture or what the second click would do. They are now exactly
 * what they always looked like: a faithful drawing of the tiling, each cell placed by {@link
 * paneRects} over the group's real tree and numbered with its rank in the member list, so the two
 * halves of the answer are legible side by side — THE LIST ORDERS, THE SHAPE DISPOSES.
 *
 * It is 150 px tall here, where it was 40 inside the block: the panel has the room the block never
 * had, and at this size a re-order is READABLE while it happens.
 */
function LayoutPreview({
  group,
  members,
}: {
  group: CommandGroup;
  members: readonly CommandRow[];
}) {
  const ids = members.map((command) => command.id);
  const tree = commandGroupTree(ids, group.layout_shape);
  if (tree == null) return null;
  const rank = new Map(ids.map((id, index) => [id, index + 1]));
  const nameOf = (id: string) => members.find((c) => c.id === id)?.name ?? id;
  return (
    <div
      data-group-layout={group.id}
      aria-hidden
      className={cn(
        "relative mt-2.5 h-[150px] w-full overflow-hidden rounded-md",
        "border border-[color-mix(in_oklch,var(--group-tone)_45%,transparent)]",
        "bg-[color-mix(in_oklch,var(--group-tone)_6%,transparent)]",
      )}
    >
      {paneRects(tree).map((placement) => (
        <div
          key={placement.paneId}
          data-layout-cell={placement.terminalId}
          style={{
            position: "absolute",
            left: `${placement.rect.left}%`,
            top: `${placement.rect.top}%`,
            width: `${placement.rect.width}%`,
            height: `${placement.rect.height}%`,
          }}
          className={cn(
            "flex items-center justify-center overflow-hidden text-xs",
            // The rectangles ANIMATE from one shape to the next, so what a shape does to the tiling
            // is SEEN rather than inferred from two still pictures.
            "transition-[left,top,width,height] duration-200 ease-out motion-reduce:transition-none",
            "border border-[color-mix(in_oklch,var(--group-tone)_35%,transparent)]",
            "bg-[color-mix(in_oklch,var(--group-tone)_10%,transparent)]",
          )}
        >
          <span className="absolute top-1 left-1.5 text-2xs text-muted-foreground tabular-nums">
            {rank.get(placement.terminalId)}
          </span>
          <span className="truncate px-1">{nameOf(placement.terminalId)}</span>
        </div>
      ))}
    </div>
  );
}
