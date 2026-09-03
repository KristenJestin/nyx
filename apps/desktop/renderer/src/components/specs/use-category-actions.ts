import { useCallback, useState } from "react";

import { errorText, nyxBridge, type BackendCommand } from "@/bridge";
import { toast } from "@/components/ui/toast";

import type { SpecCategory } from "./types";

/** Which human gesture is in flight — the gate, naming itself rather than five free booleans. */
export type CategoryGesture = "assign" | "unassign" | "rename" | "create" | "remove";

/**
 * The RENAME CONFLICT, as the backend reported it: the rename LANDED (PDR-0017 allows a homonym —
 * names are labels), and the categories that now answer that same name can no longer be told apart
 * by it. `slugs` is `category_rename`'s `homonym_slugs`, i.e. literally the candidate list the
 * domain resolver would refuse an agent with.
 */
export interface RenameConflict {
  /** The name that is now shared. */
  readonly name: string;
  /** The stable slugs that now answer it — the only thing telling them apart. */
  readonly slugs: readonly string[];
}

export interface CategoryActions {
  /** A write is in flight — the gate that shuts every action so a double-submit cannot race. */
  readonly pending: boolean;
  /** WHICH gesture is in flight, for the surface that shows a busy state on one control. */
  readonly gesture: CategoryGesture | null;
  /** The last gesture's refusal (the backend's own sentence), or `null`. */
  readonly error: string | null;
  /** The last rename's homonym conflict, or `null`. Cleared by the next gesture. */
  readonly conflict: RenameConflict | null;
  /** Drop the inline refusal AND the conflict notice (the user acknowledged them). */
  readonly clearFeedback: () => void;
  /** ASSIGN an existing category to the Spec, by its exact id. */
  readonly assign: (specId: string, categoryId: string) => Promise<boolean>;
  /** REMOVE the Spec's category — a first-class write, not a no-op. */
  readonly unassign: (specId: string) => Promise<boolean>;
  /**
   * RENAME a category's human label (the settings' Categories section). The slug is not an
   * argument and is never touched.
   */
  readonly rename: (projectId: string, categoryId: string, name: string) => Promise<boolean>;
  /**
   * CREATE a vocabulary entry for the project (the settings' Categories section). The slug is
   * DERIVED by the service from the name — it is not an argument here or anywhere.
   */
  readonly create: (projectId: string, name: string) => Promise<SpecCategory | null>;
  /**
   * DELETE a vocabulary entry. The service refuses a system category and one a Spec still carries,
   * naming the count; this hook only surfaces the sentence.
   */
  readonly remove: (projectId: string, categoryId: string) => Promise<boolean>;
}

/**
 * `useCategoryActions` — the FIVE human gestures of PDR-0017, each a single db-command over the
 * SAME `nyx_core::category` service the agent's MCP tools call. ONE hook for two surfaces, because
 * the thing that would otherwise be copied is not the list of gestures but the way a refusal is
 * surfaced (verbatim, inline + toast) and the gate that stops a double-submit. The split between
 * the two is the one the second human review of the category block drew — what acts on the SPEC,
 * and what acts on the PROJECT'S VOCABULARY:
 *
 *   * the SPEC DETAIL uses `assign` and `unassign`, and nothing else — classifying the Spec in
 *     front of you, and un-classifying it, are the only two things that surface offers;
 *   * the PROJECT SETTINGS' Categories section uses `create`, `rename` and `remove` — curating the
 *     VOCABULARY itself, which is a deliberate act on the project and lives in exactly one place.
 *
 * There is no `confirm`: PDR-0017 as amended on 2026-09-02 retired the proposed/confirmed axis —
 * the agent has full rights and its mint is valid as it stands.
 *
 * # It validates NOTHING, and that is the point
 *
 * Every rule lives in the service and is written exactly once: the SAME-PROJECT refusal of an
 * assignment, the project scope of a rename, the slug-then-name resolution and its
 * ambiguity refusal, the immutability of the slug, the refusal of a blank name. A refused write
 * comes back as a `command` BridgeError carrying the authority's own sentence, which this hook
 * SURFACES verbatim (inline + toast) rather than re-word or pre-empt. Re-checking any of it here
 * would be a second, driftable copy of the domain — the one thing PDR-0017 forbids.
 *
 * # No optimistic patching
 *
 * Each write pushes `specs://changed`, on which `use-spec-detail` and `use-project-categories`
 * already re-pull. The callers additionally `refresh()` the detail on success so the chip is
 * correct the instant the gesture resolves, without waiting for the round-trip of a broadcast —
 * both paths are the same existing channel (`nyxBridge.invoke("spec_detail")`), and neither
 * mutates a local cache by hand.
 */
export function useCategoryActions(): CategoryActions {
  const [gesture, setGesture] = useState<CategoryGesture | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState<RenameConflict | null>(null);

  const clearFeedback = useCallback(() => {
    setError(null);
    setConflict(null);
  }, []);

  /**
   * Run ONE gesture: raise the gate, clear the previous feedback, and either resolve the command's
   * payload or turn its refusal into the inline sentence + a toast. The five gestures differ by
   * their command, their args and their failure sentence — nothing else — so they share this.
   */
  const run = useCallback(
    async <T>(
      which: CategoryGesture,
      command: BackendCommand,
      args: Record<string, unknown>,
      failure: string,
    ): Promise<T | null> => {
      setGesture(which);
      setError(null);
      setConflict(null);
      try {
        return await nyxBridge.invoke<T>(command, args);
      } catch (e) {
        const detail = errorText(e);
        setError(detail);
        toast.error(failure, { description: detail });
        return null;
      } finally {
        setGesture(null);
      }
    },
    [],
  );

  const assign = useCallback(
    async (specId: string, categoryId: string) =>
      (await run(
        "assign",
        "spec_category_assign",
        { specId, categoryId },
        "The category could not be assigned.",
      )) !== null,
    [run],
  );

  const unassign = useCallback(
    async (specId: string) =>
      (await run(
        "unassign",
        "spec_category_unassign",
        { specId },
        "The category could not be removed.",
      )) !== null,
    [run],
  );

  const rename = useCallback(
    async (projectId: string, categoryId: string, name: string) => {
      const answer = await run<{ category: SpecCategory; homonym_slugs: string[] }>(
        "rename",
        "category_rename",
        { projectId, categoryId, name },
        "The category could not be renamed.",
      );
      if (answer === null) return false;
      // The conflict the BACKEND detected, kept as-is. The renderer never decides when two names
      // are "the same name" — accents, case and whitespace folding are the service's rule.
      const slugs = answer.homonym_slugs ?? [];
      setConflict(slugs.length > 1 ? { name: answer.category.name, slugs } : null);
      return true;
    },
    [run],
  );

  const create = useCallback(
    async (projectId: string, name: string) => {
      // NO `slug` ARGUMENT, and none to compute: `category_create` derives it through the same
      // `slugify_for` the agent's mint goes through — collision-safe, and never a system slug.
      const answer = await run<{ category: SpecCategory }>(
        "create",
        "category_create",
        { projectId, name },
        "The category could not be created.",
      );
      return answer?.category ?? null;
    },
    [run],
  );

  const remove = useCallback(
    async (projectId: string, categoryId: string) =>
      (await run(
        "remove",
        "category_delete",
        { projectId, categoryId },
        "The category could not be deleted.",
      )) !== null,
    [run],
  );

  return {
    pending: gesture !== null,
    gesture,
    error,
    conflict,
    clearFeedback,
    assign,
    unassign,
    rename,
    create,
    remove,
  };
}
