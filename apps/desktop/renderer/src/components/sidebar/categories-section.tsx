import { useCallback, useRef, useState, type FormEvent } from "react";
import { CheckIcon, PencilIcon, Trash2Icon, XIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { CategoryDot, RenameConflictNotice } from "@/components/specs/category-visuals";
import { useCategoryActions } from "@/components/specs/use-category-actions";
import { useProjectCategories } from "@/components/specs/use-project-categories";
import type { SpecCategory } from "@/components/specs/types";

/**
 * ONE row of the vocabulary: the colour dot, the human name (editable in place), the STABLE slug
 * (presented, never editable), and the actions this row is allowed.
 *
 * NOTHING RE-PULLS BY HAND after a write: every one of these commands pushes `specs://changed`,
 * which {@link useProjectCategories} is already subscribed to — so the list is fresh through the
 * same channel the Spec-detail selector rides, and there is no local cache to patch.
 */
function CategoryRow({
  category,
  projectId,
  actions,
}: {
  category: SpecCategory;
  projectId: string;
  actions: ReturnType<typeof useCategoryActions>;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(category.name);
  const renameButtonRef = useRef<HTMLButtonElement>(null);

  /** Leave the rename form and hand the focus back to the control that opened it. */
  const close = useCallback(() => {
    setEditing(false);
    // The button re-mounts in the same commit, so the focus can be restored straight away.
    queueMicrotask(() => renameButtonRef.current?.focus());
  }, []);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    // A blank name is REFUSED BY THE SERVICE with its own sentence; the only thing guarded here is
    // sending a rename that changes nothing.
    if (draft.trim() === category.name) {
      close();
      return;
    }
    if (await actions.rename(projectId, category.id, draft)) close();
  };

  return (
    <li className="group/category flex items-center gap-2 border-b border-border/40 px-3 py-2 last:border-b-0">
      {/* The SAME dot the Spec-detail chip draws — the `--group-N` validation behind it lives once,
          in `components/specs/category-visuals.tsx`. */}
      <CategoryDot colorToken={category.color_token} />

      {editing ? (
        <form className="flex min-w-0 flex-1 items-center gap-1.5" onSubmit={submit}>
          <Input
            autoFocus
            value={draft}
            aria-label={`New name for the category ${category.slug}`}
            disabled={actions.pending}
            className="min-w-0 flex-1"
            onChange={(e) => {
              setDraft(e.target.value);
              actions.clearFeedback();
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") close();
            }}
          />
          <Button type="submit" size="xs" loading={actions.gesture === "rename"}>
            Save
          </Button>
          <Button
            type="button"
            size="xs"
            variant="ghost"
            disabled={actions.pending}
            onClick={() => {
              actions.clearFeedback();
              close();
            }}
          >
            Cancel
          </Button>
        </form>
      ) : (
        <>
          <span className="min-w-0 flex-1 truncate text-sm text-foreground/90">
            {category.name}
          </span>
          {/* THE STABLE IDENTITY, PRESENTED AND NOT EDITABLE. There is no field for it and the
              rename command does not even take one — a slug is minted once and never rewritten,
              because it is what the agent harness reads to decide a Spec's discipline. */}
          <span className="shrink-0 font-mono text-2xs text-muted-foreground/60">
            {category.slug}
          </span>
          {/* The actions appear on hover and on keyboard focus — `focus-within` is what keeps them
              reachable by TAB alone, since `opacity-0` leaves them in the tab order. */}
          <span className="flex shrink-0 items-center gap-0.5 opacity-0 transition focus-within:opacity-100 group-hover/category:opacity-100">
            <Button
              ref={renameButtonRef}
              variant="ghost"
              size="icon-xs"
              disabled={actions.pending}
              aria-label={`Rename the category ${category.name}`}
              onClick={() => {
                actions.clearFeedback();
                setDraft(category.name);
                setEditing(true);
              }}
            >
              <PencilIcon />
            </Button>
            {/* NO DELETE ON A SYSTEM CATEGORY — the control is ABSENT, not disabled: there is no
                gesture to offer. `system` is the BACKEND's verdict on the slug
                (`db::SYSTEM_CATEGORY_SLUGS`), never a list of four literals kept here, and the
                refusal itself lives in `category::delete` — this is the courtesy on top of it. */}
            {category.system ? null : (
              <Button
                variant="ghost-destructive"
                size="icon-xs"
                disabled={actions.pending}
                aria-label={`Delete the category ${category.name}`}
                onClick={() => void actions.remove(projectId, category.id)}
              >
                <Trash2Icon />
              </Button>
            )}
          </span>
        </>
      )}
    </li>
  );
}

export interface CategoriesSectionProps {
  /** The project whose vocabulary is curated (`null` = no project targeted). */
  projectId: string | null;
}

/**
 * `<CategoriesSection>` — the **Categories** pane of the project-settings modal: the project's
 * "genre of work" vocabulary (PDR-0017), and the place a human CURATES IT.
 *
 * # Which half of PDR-0017 this is
 *
 * The agent classifies a Spec and MINTS an entry when nothing fits — that half is the MCP surface,
 * and since the 2026-09-02 amendment its mint is valid as it stands (there is no confirmation, and
 * no proposed/confirmed badge anywhere). What a human owns is the VOCABULARY: create an entry,
 * rename one, delete one. That is this screen, and it is the only one that offers a creation — the
 * Spec-detail selector deliberately does not, because typing into a search field is choosing among
 * what exists.
 *
 * # Not one rule of the domain is restated here
 *
 * The slug is DERIVED from the name by the service (collision-safe, and never one of nyx's four),
 * the blank name is refused there, and both refusals of a deletion — a system category, and one a
 * Spec still carries, whose sentence NAMES THE COUNT — are pronounced by `nyx_core::category` and
 * shown here VERBATIM. The Delete control is omitted on a system row, which is a courtesy on top of
 * the service's refusal and not a substitute for it; and even that omission reads the backend's own
 * `system` verdict rather than a copy of the four slugs.
 *
 * # Four states, each rendered explicitly
 *
 * LOADING (the vocabulary has not answered), EMPTY (the project has none), ERROR (the read or a
 * write was refused — the authority's sentence, in a live region) and the RENAME CONFLICT (the
 * rename landed and the name is now shared). None of them degrade into "nothing on screen".
 */
export function CategoriesSection({ projectId }: CategoriesSectionProps) {
  const { categories, loading, error: readError } = useProjectCategories(projectId);
  const actions = useCategoryActions();
  const [draft, setDraft] = useState("");
  const createRef = useRef<HTMLInputElement>(null);

  const feedback = actions.error ?? readError;

  const submitCreate = async (event: FormEvent) => {
    event.preventDefault();
    if (projectId === null) return;
    // The blank refusal is the SERVICE's sentence, so an empty field simply does not submit rather
    // than inventing a second wording for it.
    if (draft.trim().length === 0) return;
    const made = await actions.create(projectId, draft);
    if (made !== null) {
      setDraft("");
      createRef.current?.focus();
    }
  };

  return (
    <section>
      <h3 className="text-base font-semibold text-foreground">Categories</h3>
      <p className="mt-1 mb-4 text-sm text-muted-foreground">
        The project's vocabulary for the genre of work a Spec carries. Agents classify from this
        list and add to it when nothing fits; you curate it here. The
        <code className="mx-1">slug</code>
        is the stable identity agents read — it never changes, even on a rename.
      </p>

      {/* CREATE — the one gesture this screen has that the Spec detail does not. */}
      <form onSubmit={(e) => void submitCreate(e)} className="mb-4 flex items-center gap-2">
        <Input
          ref={createRef}
          value={draft}
          aria-label="New category name"
          placeholder="New category name…"
          disabled={projectId === null || actions.pending}
          className="flex-1"
          onChange={(e) => {
            setDraft(e.target.value);
            actions.clearFeedback();
          }}
        />
        <Button
          type="submit"
          size="sm"
          loading={actions.gesture === "create"}
          disabled={projectId === null || actions.pending || draft.trim().length === 0}
        >
          <CheckIcon />
          Add
        </Button>
      </form>

      {loading ? (
        <div className="flex items-center gap-2 py-1 text-muted-foreground" role="status">
          <Spinner aria-hidden />
          <span className="text-xs">Loading categories…</span>
        </div>
      ) : categories.length === 0 && readError === null ? (
        <EmptyState>This project has no category yet.</EmptyState>
      ) : (
        <ul
          aria-label="Project categories"
          className={cn("overflow-hidden rounded-lg border border-border/60 bg-card")}
        >
          {categories.map((category) => (
            <CategoryRow
              key={category.id}
              category={category}
              projectId={projectId ?? ""}
              actions={actions}
            />
          ))}
        </ul>
      )}

      {/* THE REFUSAL — the authority's own sentence, never re-worded. */}
      {feedback !== null ? (
        <p role="alert" className="mt-3 flex items-start gap-1 text-xs text-destructive">
          <XIcon className="mt-0.5 size-3 shrink-0" aria-hidden />
          {feedback}
        </p>
      ) : null}

      {/* THE RENAME CONFLICT — the shared notice of `components/specs/category-visuals.tsx`.
          Renders nothing when there is none. */}
      <RenameConflictNotice conflict={actions.conflict} className="mt-3" />
    </section>
  );
}
