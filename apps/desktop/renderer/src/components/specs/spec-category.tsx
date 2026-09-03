import { useMemo } from "react";
import { Combobox } from "@base-ui/react/combobox";
import { CheckIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { inputFieldClassName } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import { CategoryDot } from "./category-visuals";
import type { SpecCategory as Category, SpecDetail } from "./types";
import { useCategoryActions } from "./use-category-actions";
import { useProjectCategories } from "./use-project-categories";

/**
 * The item value that RETIRES the assignment — "No category" as a row of the list, so removing is
 * the same gesture as choosing, made in the same place.
 *
 * WHY A SENTINEL AND NOT `null`: Base UI already spends `null` on "the combobox cleared itself" —
 * that is what Escape and an emptied field produce — and a cancel must not un-classify the Spec.
 * The `onValueChange` guard below therefore drops every `null`, and this string is the only way a
 * removal can be expressed. It is deliberately not a shape a category id can take (ids are ULIDs).
 */
const NO_CATEGORY = "__nyx_no_category__";

/**
 * The category CHIP: the colour dot + the human name. Nothing else — PDR-0017 as amended on
 * 2026-09-02 retired the proposed/confirmed axis, so there is no curation badge to draw: what the
 * agent minted is valid as it stands.
 *
 * The dot is {@link CategoryDot}, shared with the settings' Categories section: the `--group-N`
 * validation behind it is written once, in `category-visuals.tsx`.
 */
function CategoryChip({ category, className }: { category: Category; className?: string }) {
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1.5", className)}>
      <CategoryDot colorToken={category.color_token} />
      <span className="min-w-0 truncate text-sm text-foreground/90">{category.name}</span>
    </span>
  );
}

/**
 * `<SpecCategoryPicker>` — the Spec detail's CATEGORY block: the one "genre of work" axis a Spec
 * carries (PDR-0017), shown and chosen. ONE LINE, ONE CONTROL.
 *
 * # The block is the value, and the value is the control
 *
 * The block renders exactly ONE interactive element, in both of its states:
 *
 *   * **FILLED** — the chip (colour dot + name). Nothing beside it: no slug, no rename, no remove,
 *     no separate edit icon. The chip IS the picker's trigger, so a click — or Enter/Space on it,
 *     since it is a native `<button>` — opens the list straight away, with no intermediate state
 *     where a closed select stands where the chip was.
 *   * **EMPTY** — the same trigger, reading "Choose a category". One click opens the same list.
 *
 * That is the answer to the second human review of this block (`01M1HEA29RRF962TY54BH736GG`). The
 * first delivery drew two pencils, the name AND the slug, `Rename`, `Remove`, and an edit icon that
 * swapped the chip for a CLOSED select — four controls and two extra readings of the same value,
 * where the user asked for one line and one gesture.
 *
 * # Choosing, and un-choosing, are the same gesture
 *
 * The list is the Base UI `Combobox` popup, searchable on the human NAME and on the STABLE SLUG,
 * with a **No category** row that retires the assignment. There is no separate `Remove` button:
 * removal is a choice like any other, made where the choices are. Escape closes and writes NOTHING
 * (see the `onValueChange` guard below); a selection closes and writes.
 *
 * # Renaming, creating and deleting are NOT here
 *
 * Curating the VOCABULARY — create, rename, delete — is a deliberate act on the project, not on the
 * Spec in front of you, and it lives in **Settings → Categories**
 * (`components/sidebar/categories-section.tsx`). Not one of those gestures is offered here, and the
 * Spec detail sends none of their commands.
 *
 * # The slug is not shown here any more
 *
 * `slug` is the STABLE IDENTITY the agent harness reads — `bug` is what puts a Spec under the
 * red-before-green discipline — and it stays visible where it can be acted upon: on every row of
 * the picker (which is also searchable by it) and in Settings → Categories, which explains it in
 * prose. The resting Spec line reads the category once, as a human label.
 *
 * # Not one rule of the domain is restated here
 *
 * The same-project refusal of an assignment and the project scope of every write are pronounced by
 * `nyx_core::category` and surfaced VERBATIM (see {@link useCategoryActions}). The only edge states
 * this block renders itself are the vocabulary's: LOADING (it has not answered), EMPTY VOCABULARY
 * (the project has none) — both inside the popup, where the choice would be — and the REFUSAL, in a
 * live region under the trigger.
 */
export function SpecCategoryPicker({
  detail,
  refresh,
}: {
  detail: SpecDetail;
  /** Re-pull the Spec detail — the existing `spec_detail` channel, never a local patch. */
  refresh: () => Promise<void>;
}) {
  const specId = detail.spec.id;
  const projectId = detail.spec.project_id;
  const current = detail.category;
  const { categories, loading, error: readError } = useProjectCategories(projectId);
  const actions = useCategoryActions();

  // The item VALUES are plain ids, not objects: a re-pull rebuilds the list, and an object identity
  // would stop matching the selected value on every refresh (Base UI compares with `Object.is`).
  // The label and the search text are derived from the id through this map.
  const byId = useMemo(() => new Map(categories.map((c) => [c.id, c])), [categories]);
  // `No category` FIRST — the row that retires the assignment sits where a reader looks for "none",
  // above the vocabulary rather than buried at the end of it.
  const ids = useMemo(() => [NO_CATEGORY, ...categories.map((c) => c.id)], [categories]);

  const feedback = actions.error ?? readError;

  const pick = async (next: string) => {
    if (next === NO_CATEGORY) {
      // Choosing "No category" when there is none is an answer, not a write.
      if (current === null) return;
      if (await actions.unassign(specId)) await refresh();
      return;
    }
    // RE-PICKING THE SAME CATEGORY IS NOT A WRITE — the popup closes on its own, and there is
    // nothing to send.
    if (next === current?.id) return;
    if (await actions.assign(specId, next)) await refresh();
  };

  return (
    <div className="flex flex-col gap-2">
      <Combobox.Root
        items={ids}
        // NEVER `null`: an un-classified Spec selects the "No category" row, so the list always
        // shows WHERE the Spec stands, and the cleared-value path below stays the only source of a
        // `null`.
        value={current?.id ?? NO_CATEGORY}
        // A `null` HERE IS THE COMBOBOX CLEARING ITSELF — Escape, or an emptied field — and NOT a
        // decision to un-classify the Spec. Removal is the "No category" ROW (`pick` above);
        // letting a stray clear reach it made Escape silently un-assign the category it was meant
        // to leave alone.
        onValueChange={(next) => {
          if (next !== null) void pick(next);
        }}
        itemToStringLabel={(id: string) =>
          id === NO_CATEGORY ? "No category" : (byId.get(id)?.name ?? id)
        }
        // Searchable on the LABEL and on the STABLE SLUG: a human who knows the identity the
        // harness reads must be able to type it. A dumb case-insensitive substring — no resolution
        // rule is reproduced here (the command names an exact id).
        filter={(id: string, query: string) => {
          const needle = query.trim().toLowerCase();
          if (needle.length === 0) return true;
          if (id === NO_CATEGORY) return "no category".includes(needle);
          const category = byId.get(id);
          if (category === undefined) return false;
          return (
            category.name.toLowerCase().includes(needle) ||
            category.slug.toLowerCase().includes(needle)
          );
        }}
        // A refusal belongs to the gesture that earned it: re-opening the list is a new attempt.
        onOpenChange={(open) => {
          if (open) actions.clearFeedback();
        }}
      >
        {/* THE ONE CONTROL. The chip is the trigger, so the value and the way to change it are the
            same element and the same click — the design system's ghost button is what makes it read
            as pressable, and being a NATIVE `<button>` is what makes Enter and Space open it.

            `aria-busy`, NOT `disabled`: a disabled element cannot hold the focus, and Base UI hands
            the focus back to this trigger when the popup closes — right as a write is in flight. */}
        <Combobox.Trigger
          render={
            <Button variant="ghost" size="sm" className="h-auto w-full justify-start px-2 py-1.5" />
          }
          aria-busy={loading || actions.pending}
          // NAMED EXPLICITLY, in both states: with the search field inside the popup the trigger
          // carries `role="combobox"`, and a combobox takes no name from its own content — so the
          // visible chip would announce as nothing at all without this.
          aria-label={
            current === null
              ? "Choose a category"
              : `Change the category — currently ${current.name}`
          }
        >
          {current === null ? (
            <span className="min-w-0 truncate text-sm text-muted-foreground">
              Choose a category
            </span>
          ) : (
            <CategoryChip category={current} />
          )}
        </Combobox.Trigger>

        <Combobox.Portal>
          <Combobox.Positioner sideOffset={4} align="start" className="z-50 outline-none">
            {/* The popup carries its own name because the search field lives INSIDE it: Base UI
                gives it `role="dialog"` in that composition, and a dialog without a name is
                announced as nothing. */}
            <Combobox.Popup
              aria-label="Category"
              className="max-h-64 w-[max(var(--anchor-width),14rem)] overflow-y-auto rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-lg outline-none"
            >
              {/* THE FIELD STYLING IS NAMED, NOT COPIED: `inputFieldClassName` is the design
                  system's one field string (`ui/input.tsx`). A copied class string is a token
                  change that never arrives.

                  IT IS MOUNTED IN EVERY STATE, loading included: Base UI focuses this field when
                  the popup opens, and unmounting it when the vocabulary lands would drop the focus
                  to `<body>` — which Base UI reads as a focus-out and closes the popup under the
                  user, right as the list they were waiting for arrives. */}
              <Combobox.Input
                aria-label="Search a category"
                placeholder="Search a category…"
                className={cn(inputFieldClassName, "mb-1 w-full")}
              />
              {loading ? (
                <div
                  className="flex items-center gap-2 px-2 py-1.5 text-muted-foreground"
                  role="status"
                >
                  <Spinner aria-hidden />
                  <span className="text-2xs">Loading categories…</span>
                </div>
              ) : (
                <>
                  {/* THE REFUSAL BELONGS WHERE THE CHOICE WOULD BE. `useProjectCategories` keeps
                      "the vocabulary is empty" and "the read was refused" as two distinct states
                      precisely so the block can say which one it is in — and this branch used to
                      answer the second one by DELETING the only sentence in the popup, leaving a
                      list holding nothing but "No category" and no reason anywhere. The one witness
                      was the alert under the trigger, which the portalled popup (`z-50`, anchored 4
                      px below that very trigger) covers while it is open.

                      A `role="status"` and not a second `role="alert"`: the sentence under the
                      trigger already announces the refusal, and announcing it twice is not twice as
                      accessible. This line is what the LIST is, stated where the list is. */}
                  {readError !== null ? (
                    <p role="status" className="px-2 py-1.5 text-2xs text-destructive">
                      The project's categories could not be read — {readError}
                    </p>
                  ) : categories.length === 0 ? (
                    <p className="px-2 py-1.5 text-2xs text-muted-foreground/60">
                      This project has no category yet. The agent mints one when it classifies a
                      Spec.
                    </p>
                  ) : null}
                  <Combobox.Empty className="px-2 py-3 text-2xs text-muted-foreground">
                    No category matches.
                  </Combobox.Empty>
                  <Combobox.List>
                    {(id: string) => {
                      const category = byId.get(id);
                      if (id !== NO_CATEGORY && category === undefined) return null;
                      return (
                        <Combobox.Item
                          key={id}
                          value={id}
                          // THE HIGHLIGHT IS `sidebar-accent`, THE ONLY LIST-HIGHLIGHT TOKEN NYX
                          // DECLARES. `--accent` is a name this design system does not carry
                          // (`globals.css` maps `--color-sidebar-accent` and nothing else), so
                          // Tailwind emitted NO rule at all for `bg-accent` and the row under
                          // ArrowDown was painted with exactly nothing. `ui/menu.tsx` already
                          // highlights every menu row with this pair; a list is a list.
                          className="flex cursor-default items-center gap-2 rounded-md px-2 py-1.5 outline-none data-highlighted:bg-sidebar-accent data-highlighted:text-sidebar-accent-foreground"
                        >
                          <Combobox.ItemIndicator className="shrink-0">
                            <CheckIcon className="size-3" aria-hidden />
                          </Combobox.ItemIndicator>
                          {/* THE REMOVAL, AS A CHOICE. There is no `Remove` button anywhere in the
                              block: un-classifying is picking "No category". */}
                          {category === undefined ? (
                            <span className="flex-1 text-sm text-muted-foreground">
                              No category
                            </span>
                          ) : (
                            <>
                              <CategoryChip category={category} className="flex-1" />
                              <span className="shrink-0 font-mono text-2xs text-muted-foreground/60">
                                {category.slug}
                              </span>
                            </>
                          )}
                        </Combobox.Item>
                      );
                    }}
                  </Combobox.List>
                </>
              )}
            </Combobox.Popup>
          </Combobox.Positioner>
        </Combobox.Portal>
      </Combobox.Root>

      {/* THE REFUSAL — the authority's own sentence, never re-worded. */}
      {feedback !== null ? (
        <p role="alert" className="text-2xs text-destructive">
          {feedback}
        </p>
      ) : null}
    </div>
  );
}
