import { useCallback, useId, useState, type CSSProperties } from "react";
import { ImageIcon, ImageOffIcon, Loader2Icon, XCircleIcon } from "lucide-react";

import { errorText, nyxBridge } from "@/bridge";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/toast";
import { GroupSwatches, type SwatchNaming } from "@/components/panes/group-swatches";
import {
  PROJECT_ACCENTS,
  PROJECT_ICONS,
  ProjectMark,
  projectAccentColor,
  projectImageSrc,
} from "./project-mark";

/**
 * `<ProjectIdentity>` — THE GESTURE. The one surface through which a human dresses a project:
 * an accent, and a mark that is either a glyph or a picture.
 *
 * Everything under it already existed and was reachable only by an AGENT (the MCP
 * `set_project_settings`) or by editing the database by hand: the columns, the writes, and
 * `<ProjectMark>` which draws the result on the rail, the home and every cross-project row. This
 * block is what finally makes them a human's.
 *
 * ## Placement (round-3 prototype `project-identity-picker` / `apercu`)
 *
 * ONE framed card, the dominant object of the Global pane — but UNDER the Name field, not opening
 * the section. Two columns:
 *
 *  - LEFT, what the choice PRODUCES, CENTRED in its column: the 80 px mark, the project's name,
 *    and the single `Remove identity` gesture. No summary line, no caption, and — since the user
 *    drove the real app — no *In context* preview either: the mark among rail neighbours and on a
 *    home row was a demonstration of something already visible behind the modal, for half a
 *    column.
 *  - RIGHT, the CHOICES, in two sub-blocks of the same rank: the accent row, then the MARK — the
 *    twenty-eight glyphs under their own heading, and the IMAGE under a second heading of its own,
 *    with the picture button and the inline refusal message.
 *
 * ## The rules this block carries
 *
 *  - **THE FIRST ACCENT SWATCH ERASES.** It writes `NULL`, never `identity-0` — the absence has
 *    one spelling in the database. That is not decided here: {@link PROJECT_ACCENTS} leads with
 *    `null` and this block simply hands what it is given to the write.
 *  - **ICON AND IMAGE ARE ONE SLOT.** Picking either clears the other, on screen and in the
 *    database (`set_project_mark` writes both columns; the `projects` CHECK forbids the pair).
 *    Pressing the glyph a project already wears REMOVES it — a shortcut, not a second button.
 *    The two headings on the right are a SEPARATION OF PRESENTATION only: one `role="group"`
 *    still spans them both, and both descriptions say the exclusion out loud.
 *  - **THE ACCENT SURVIVES THE PICTURE.** A picture COVERS the accented tile (`object-cover`); it
 *    does not grey the palette out and does not clear the column, so removing the picture brings
 *    the colour back. What says so is the accent row itself — still live, still showing the worn
 *    swatch as pressed, while an image is on the mark.
 *  - **NO FILENAME, ANYWHERE.** The copy is stored as `<projectId>~<revision>.<ext>`, so the name
 *    of the file the human picked does not exist in the database and could not be shown again
 *    after a reload. Printing it once would be a lie with a delay on it. The 80 px mark is the
 *    only witness — and the `<revision>` is what makes it a HONEST one when the picture is replaced
 *    by another of the same format: the path changes, so every drawing of it repaints and a source
 *    this block had recorded as unloadable stops being blacklisted (`project-image.ts`).
 *  - **ONE CONTROL PER GESTURE.** The IMAGE sub-block holds exactly one clickable thing: a button
 *    carrying a thumbnail and its label. There is no second, clickable copy of the picture beside
 *    it, and no third drawing of it — the mark on the left is the canonical preview.
 *  - **NO SAVE BUTTON.** Every choice persists on the press, on the `onResumeChange` model. The
 *    Name field's Save is the pre-existing rename and is untouched.
 */

/** The accent row's vocabulary — {@link GroupSwatches} with thirteen options instead of seven. */
const ACCENT_NAMING: SwatchNaming = {
  row: "Accent",
  none: "No accent",
  // A hue at one lightness and one chroma has no name; "Accent 7" is honest, an invented colour
  // name would not be. The `title` carries the STORED TOKEN — read from the offered list rather
  // than re-spelled here, so the tooltip cannot drift from what the press actually writes.
  tone: (index) => `Accent ${index}`,
  title: (index) => (index === 0 ? "No accent" : (PROJECT_ACCENTS[index] ?? "")),
};

/**
 * A glyph token as a human name: `pen-tool` → `Pen tool`. DERIVED, not a second registry — a
 * hand-written label map would be twenty-eight lines to keep in step with `PROJECT_ICONS` for
 * nothing, and the day they drift the grid says one thing and the screen reader another.
 */
export function projectIconLabel(token: string): string {
  const words = token.replace(/[-_]+/g, " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** Open the OS picture picker for `projectId` and resolve the path of the COPY nyx keeps. */
function pickProjectImageFile(projectId: string): Promise<string | null> {
  return nyxBridge.pickProjectImage(projectId);
}

/**
 * THE MARK VOCABULARY, written once and worn by BOTH spellings of the slot — the twenty-eight
 * glyph tiles and the single image control.
 *
 * They are constants rather than two hand-written class lists because the task that merged the
 * thumbnail into its button (`01M1VP0PM0PVRAKV88K098RP8N`) asks for the image control to carry the
 * accent ring "exactly as a tile of the grid carries it — same visual vocabulary, same group". Two
 * copies of a ring drift on the first tweak to one of them; one constant cannot, and a spec can
 * assert that the two controls literally share it.
 */
const MARK_CONTROL =
  "flex cursor-pointer items-center rounded-[9px] border border-input bg-background text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground disabled:pointer-events-none disabled:opacity-64 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background";

/**
 * THE RING — what says "this is the mark this project wears", on a tile as on the image button.
 * EXPORTED for the spec that pins the sharing: a test that re-spelled these three classes would go
 * green on a ring that had drifted away from the grid's, which is the one thing it exists to catch.
 */
export const MARK_CONTROL_SELECTED =
  "border-[var(--identity-sel)] bg-[color-mix(in_oklch,var(--identity-sel)_14%,transparent)] text-[var(--identity-sel)]";

/**
 * A SUB-BLOCK'S HEADING — a short label and, on its baseline, the sentence saying what the control
 * under it does. Three of them now: *Accent*, *Mark*, *Image*.
 *
 * IT IS A COMPONENT RATHER THAN THREE COPIES OF A `<div>` because "of the same level" is the whole
 * point of the last one. The finding that split *Mark* in two
 * (`01M1VKVXY8VV0R76ZK0S2DCGCN`) asked for a second heading OF THE SAME RANK as the first, and a
 * rank spelled out in three places is a rank that drifts on the first tweak to one of them.
 */
function SettingHeading({
  label,
  description,
  className,
}: {
  label: string;
  description: string;
  className?: string;
}) {
  return (
    <div className={cn("flex items-baseline gap-2", className)}>
      <span className="text-2xs font-semibold uppercase tracking-wide text-muted-foreground">
        {label}
      </span>
      <span className="text-xs text-muted-foreground">{description}</span>
    </div>
  );
}

export interface ProjectIdentityProps {
  /** The project being dressed (`null` disables every control). */
  projectId: string | null;
  /** Its display name — the initial of the neutral tile, and the name shown in the preview. */
  projectName: string;
  /** Its stored accent (a palette token name), or `null`. */
  accent: string | null;
  /** Its stored glyph token, or `null`. Mutually exclusive with {@link imagePath}. */
  icon: string | null;
  /** Its stored picture path (the COPY nyx owns), or `null`. */
  imagePath: string | null;
  /** Persist an accent. `null` CLEARS. Rejections are toasted with the backend's own reason. */
  onAccentChange: (accent: string | null) => void | Promise<void>;
  /** Persist the mark — a glyph, a picture, or `null` to drop it. */
  onMarkChange: (mark: { icon: string } | { image: string } | null) => void | Promise<void>;
  /**
   * Open the picture picker; resolves the COPY's absolute path, `null` when the human CANCELLED,
   * and REJECTS with a readable sentence when the file was refused (extension, size, unreadable).
   * Injectable so a test can rehearse the three outcomes without the shell.
   */
  pickImage?: (projectId: string) => Promise<string | null>;
}

export function ProjectIdentity({
  projectId,
  projectName,
  accent,
  icon,
  imagePath,
  onAccentChange,
  onMarkChange,
  pickImage = pickProjectImageFile,
}: ProjectIdentityProps) {
  // The REFUSAL of a picked file, shown in line under the control it came from. It is NOT a toast:
  // it belongs next to the button that produced it, and it is the node the removed caption used to
  // sit beside — the caption went, this stayed.
  const [imageError, setImageError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);
  // The thumbnail's FAILED SOURCE rather than a boolean — the `<ProjectMark>` reflex: pointing the
  // project at another picture retries instead of staying blacklisted, and no reset is needed when
  // the parent's `imagePath` changes underneath.
  const [failedThumb, setFailedThumb] = useState<string | null>(null);
  // The id of the image control's DESCRIPTION — what says, to a reader, the thing the ring says to
  // an eye. Minted per instance rather than written as a literal: the modal is not the only place
  // this block could ever be mounted, and two identical `id`s would point both buttons at one.
  const stateId = useId();

  const disabled = projectId == null;
  const dressed = accent != null || icon != null || imagePath != null;
  const project = { name: projectName, accent, icon, image_path: imagePath };

  // The SELECTION colour of the mark grid: the project's own accent, so a chosen glyph is outlined
  // in the colour it will actually wear; a plain foreground wash when there is no accent yet.
  const selectionTone =
    projectAccentColor(accent) ?? "color-mix(in oklch, var(--foreground) 70%, transparent)";

  // Persist and, on refusal, say the backend's real reason. There is no success toast: the mark
  // repaints everywhere at once, which is a better acknowledgement than a banner.
  const persist = useCallback(async (write: () => void | Promise<void>, fallback: string) => {
    try {
      await write();
    } catch (e) {
      toast.error(errorText(e, fallback));
    }
  }, []);

  const chooseIcon = (token: string) => {
    setImageError(null);
    // Pressing the glyph already worn REMOVES it. Pressing another one REPLACES the mark —
    // including a picture, which `set_project_mark` clears in the same write.
    void persist(
      () => onMarkChange(icon === token ? null : { icon: token }),
      "Could not update the project mark.",
    );
  };

  const chooseImage = async () => {
    if (projectId == null || picking) return;
    setImageError(null);
    setPicking(true);
    let copied: string | null = null;
    try {
      copied = await pickImage(projectId);
    } catch (e) {
      // A REFUSAL — the file was not one nyx accepts. Nothing was written, nothing was copied, and
      // the message is the main process's own sentence. A CANCEL resolves `null` instead and is
      // silent, which is the whole reason the two outcomes are shaped differently.
      setImageError(errorText(e, "That image could not be used."));
      setPicking(false);
      return;
    }
    setPicking(false);
    if (copied == null) return;
    await persist(() => onMarkChange({ image: copied }), "Could not update the project mark.");
  };

  const removeIdentity = () => {
    setImageError(null);
    // ONE gesture, all three columns. The mark goes first so the picture's copy is dropped by the
    // same call that dereferences it, then the accent.
    void persist(async () => {
      await onMarkChange(null);
      await onAccentChange(null);
    }, "Could not remove the project identity.");
  };

  const thumbSrc = projectImageSrc(imagePath);

  return (
    <div
      // The ONE framed element of the pane, and its biggest object — but it sits under Name, which
      // is what round 2 settled: dominant, not first.
      style={{ "--identity-sel": selectionTone } as CSSProperties}
      className="mt-5 flex overflow-hidden rounded-xl border border-border bg-muted/30"
    >
      {/* LEFT — what the choice produces. THREE things, CENTRED on both axes: the mark at 80 px,
          the project's name, and the one removal gesture. It used to carry an *In context* preview
          under them — the mark among rail neighbours, then on a home row — and the user who drove
          the real app asked for it to go: it spent half the column demonstrating something they
          could already see on the rail behind the modal. What it demonstrated in particular, that
          an accent SURVIVES under a picture, is now said by the choices themselves (the accent row
          stays live and stays selected while an image is worn) rather than mimed. */}
      <div className="flex w-58 shrink-0 flex-col items-center justify-center gap-2.5 border-r border-border px-4 py-4.5 text-center">
        <ProjectMark
          project={project}
          data-testid="identity-preview-mark"
          className="size-20 rounded-[18px] text-[30px]"
        />
        <span className="text-sm font-semibold text-foreground">{projectName}</span>

        <Button
          type="button"
          size="sm"
          variant="ghost-destructive"
          className="mt-3"
          disabled={disabled || !dressed}
          onClick={removeIdentity}
        >
          <XCircleIcon />
          Remove identity
        </Button>
      </div>

      {/* RIGHT — the choices. */}
      <div className="min-w-0 flex-1 px-4.5 py-4">
        <section>
          <SettingHeading
            label="Accent"
            description="Tints the mark wherever the project appears. Applied as you pick."
          />
          {/* THE SHARED SWATCH ROW, not a second colour picker: the same component the command
              groups use, handed the project vocabulary. Its first option is `null`, so pressing it
              ERASES rather than storing `identity-0`. */}
          <GroupSwatches
            className="mt-2.5"
            options={PROJECT_ACCENTS}
            naming={ACCENT_NAMING}
            disabled={disabled}
            value={accent}
            onSelect={(next) =>
              void persist(() => onAccentChange(next), "Could not update the project accent.")
            }
          />
        </section>

        {/* THE MARK — ONE choice, shown under TWO headings.

            The user drove the real app and said the picture looked like a twenty-ninth tile: a
            single *Mark* heading chapeaued the glyph grid AND the image row, so nothing on screen
            said that a picture is a different KIND of answer to the same question. It now has its
            own heading and its own sentence, at the SAME RANK as the glyphs'
            ({@link SettingHeading}).
            THE SPLIT IS PRESENTATION ONLY, and two things hold it to that. Structurally, ONE
            `role="group"` still spans both sub-blocks, so a screen reader enters a single setting
            rather than two independent ones. In WORDS, the sentence that carried the exclusion —
            *An icon, or an image — one slot, one choice* — was not dropped when it was split: BOTH
            descriptions now say it, each from its own side, and the shared tail is what tells a
            reader who meets only one of them that the other exists.
            The GLYPH tiles stay TOGGLES (`aria-pressed`), not radios: pressing the current one
            removes the mark, and a radio that un-checks itself on a second press is a lie. The
            image control is NOT one of them — see its own note below. */}
        <section className="mt-4">
          <div role="group" aria-label="Mark">
            <SettingHeading
              label="Mark"
              description="A shape from nyx's own set. Setting one clears the image — one slot, one choice."
            />
            <div className="mt-2.5 grid grid-cols-7 gap-2">
              {Object.entries(PROJECT_ICONS).map(([token, Glyph]) => {
                const selected = icon === token;
                return (
                  <button
                    key={token}
                    type="button"
                    aria-label={projectIconLabel(token)}
                    aria-pressed={selected}
                    title={projectIconLabel(token)}
                    data-project-icon={token}
                    disabled={disabled}
                    onClick={() => chooseIcon(token)}
                    className={cn(
                      MARK_CONTROL,
                      "size-8.5 justify-center",
                      selected && MARK_CONTROL_SELECTED,
                    )}
                  >
                    <Glyph className="size-4.5" />
                  </button>
                );
              })}
            </div>

            {/* THE IMAGE SUB-BLOCK. `mt-5` rather than the grid's own `mt-2.5`: the gap is what
                detaches the picture from the four rows of glyphs above it, so it stops reading as a
                twenty-ninth tile.

                ONE HEADING, ONE CONTROL, ONE MESSAGE — and the wrapper is what lets a spec say
                "one clickable thing in the IMAGE block" as a count rather than as a hopeful
                `queryBy…`. */}
            <div className="mt-5" data-testid="identity-image-block">
              <SettingHeading
                label="Image"
                description="A picture of your own, copied into nyx. Setting one clears the icon — one slot, one choice."
              />

              {/* THE ONE CONTROL OF THIS BLOCK — thumbnail on the left, label on the right.
                  It used to be TWO: a clickable thumbnail and a button beside it, both opening the
                  same picker. The user drove the real app and said so — "I can click on both things
                  to change the image, it's pretty weird" — and the thumbnail was on top of that a
                  THIRD drawing of the same picture, the 80 px mark in the left column being the
                  canonical one. It could not simply be deleted, because it carried the RING: the
                  only thing on screen saying that the picture, and not one of the glyphs above, is
                  the mark this project wears. So the two were merged, and the ring came with it.

                  ITS RING IS THE GRID'S RING, not a lookalike: {@link MARK_CONTROL} and
                  {@link MARK_CONTROL_SELECTED} are the same two constants the tiles above wear, so
                  "same visual vocabulary" is a fact about the markup.

                  IT IS STILL NOT A TOGGLE, and gains no `aria-pressed` from the merge. Pressing it
                  inverts nothing — it opens the OS picker — and a toggle that no press ever
                  releases is exactly the lie the tiles avoid by not being radios; an audit settled
                  that once already. THE RING IS A VISUAL STATE, and what carries it for a screen
                  reader is a DESCRIPTION (`aria-describedby`) rather than the name: the name stays
                  the ACTION, which is what a voice user says out loud and what a reader needs in
                  order to know what a press will do. The description is the sentence the ring
                  draws, and in the broken case it also says the thing only a colour said before. */}
              <div className="mt-2.5 flex items-center gap-2.5">
                <button
                  type="button"
                  data-testid="identity-image-control"
                  aria-describedby={imagePath != null ? stateId : undefined}
                  aria-busy={picking || undefined}
                  disabled={disabled || picking}
                  onClick={() => void chooseImage()}
                  className={cn(
                    MARK_CONTROL,
                    // The HEIGHT OF A TILE (`h-8.5`), so the two spellings of the slot sit on one
                    // visual scale; the padding is asymmetric because the thumbnail hugs the ring
                    // on the left while the label needs air on the right.
                    "h-8.5 gap-2 pr-2.5 pl-1.5 text-xs font-medium",
                    imagePath != null && MARK_CONTROL_SELECTED,
                  )}
                >
                  <span className="flex size-6 shrink-0 items-center justify-center overflow-hidden rounded-[6px]">
                    {picking ? (
                      // No `<Spinner>`: it announces itself (`role="status"`, `aria-label`), which
                      // would splice "Loading" into this button's name-from-content while the OS
                      // dialog is open. The busy state is on the button, said once.
                      <Loader2Icon aria-hidden className="size-4 animate-spin" />
                    ) : imagePath == null ? (
                      <ImageIcon aria-hidden className="size-4 opacity-80" />
                    ) : thumbSrc && failedThumb !== thumbSrc ? (
                      <img
                        src={thumbSrc}
                        alt=""
                        className="size-full object-cover"
                        onError={() => setFailedThumb(thumbSrc)}
                      />
                    ) : (
                      // A picture that is SET but unreadable. Not a broken square, and not the
                      // initial either: this control still has to say a picture is what the project
                      // wears, otherwise `Remove identity` looks like it has nothing to remove.
                      <ImageOffIcon aria-hidden className="size-4 text-destructive" />
                    )}
                  </span>
                  {imagePath != null ? "Replace image…" : "Choose image…"}
                </button>
                {/* OUTSIDE the button on purpose: inside, this text would join its name-from-content
                    and the name would stop being the action. */}
                {imagePath != null && (
                  <span id={stateId} className="sr-only">
                    {thumbSrc && failedThumb !== thumbSrc
                      ? "This picture is the mark this project wears."
                      : "This picture is the mark this project wears, but its file could not be loaded."}
                  </span>
                )}
              </div>

              {imageError != null && (
                <p
                  role="alert"
                  className="mt-2.5 flex items-center gap-1.5 text-xs text-destructive"
                >
                  <XCircleIcon className="size-3.5 shrink-0" />
                  {imageError}
                </p>
              )}
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}
