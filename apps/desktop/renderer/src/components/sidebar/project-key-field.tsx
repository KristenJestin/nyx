import { useCallback, useEffect, useState } from "react";
import { errorText, nyxBridge } from "@/bridge";
import { TriangleAlertIcon, XCircleIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { toast } from "@/components/ui/toast";

/**
 * THE SHAPE OF A PROJECT KEY, on the renderer's side of the boundary: 2 to 10 characters, an
 * uppercase ASCII letter then uppercase letters or digits.
 *
 * WHY IT IS WRITTEN HERE AT ALL, given the backend refuses a malformed key anyway. The field
 * validates AS THE USER TYPES — a Save that only lights up on a well-formed key says what the rule
 * is without a sentence, and without a round-trip per keystroke. The backend stays the authority
 * (`db::set_project_key` re-checks the same rule and owns the uniqueness one, which no client can
 * know), so this is a mirror, never a gate.
 *
 * The mirror is PINNED to its original: `project-key-field.test.tsx` reads the bounds straight out
 * of `db.rs` and replays the boundary cases through this predicate, so widening the rule on one
 * side turns the other red instead of leaving a field that refuses what the domain accepts.
 */
export const PROJECT_KEY_MIN_LEN = 2;
export const PROJECT_KEY_MAX_LEN = 10;

/** Whether `key` has the shape a project key has — the mirror of Rust's `validate_project_key`. */
export function isValidProjectKey(key: string): boolean {
  return new RegExp(`^[A-Z][A-Z0-9]{${PROJECT_KEY_MIN_LEN - 1},${PROJECT_KEY_MAX_LEN - 1}}$`).test(
    key,
  );
}

export interface ProjectKeyFieldProps {
  /** The project whose key is edited (`null` disables the field). */
  projectId: string | null;
  /** The project's CURRENT stored key — seeds the input and is the yardstick for "changed". */
  projectKey: string;
  /**
   * Persist the key. Resolves on success; REJECTS with the backend's own sentence (a malformed
   * key, or one another project already holds) so this field can print it under the input.
   */
  onSave: (key: string) => Promise<void>;
  /**
   * How many Specs the project has, for the re-keying warning. `undefined` means "not counted yet"
   * and renders no warning — an absent count must not be read as zero, or the one gesture that
   * needs a warning would ship without one on a slow read. Omit it entirely and the field counts
   * for itself (the default), which is what the settings pane does.
   */
  specCount?: number;
}

/**
 * `<ProjectKeyField>` — the Global pane's KEY row: the prefix every Spec of this project is
 * referenced by (`NYX-42` is Spec 42 of the project keyed `NYX`).
 *
 * ## Why it is a field at all
 *
 * The key was DERIVED from the project's name at creation (`derive_project_key_stub`) and never
 * writable afterwards: `update_project` touches the name alone, the MCP tool did not expose it, and
 * this modal had no input. So a project created as "Nouveau projet" carried `NOUVEAUP` in front of
 * every Spec it would ever have, and the only remedy was editing SQLite by hand.
 *
 * ## What it does that a plain input does not
 *
 * **It upper-cases as you type.** The stored key is uppercase and the unique index is
 * case-SENSITIVE, so accepting `nyx` beside `NYX` would create two keys the database calls
 * different and every human calls the same. Refusing the keystroke would be the other answer;
 * transforming it is the one that never makes the user retype a word they spelled correctly.
 *
 * **It warns before it re-keys.** A Spec's human reference is DERIVED (`key || '-' || number`), not
 * stored — so changing the key re-keys every existing Spec AT ONCE, with nothing to migrate and
 * nothing to undo. `NYX-12` written in a commit message yesterday stops resolving. That is a real
 * consequence with no visible symptom, which is exactly the kind a form has to state BEFORE the
 * button, not after the toast.
 *
 * **It prints the refusal.** Uniqueness is the one rule a client cannot check, so "another project
 * already uses that key" arrives from the backend and is shown under the input — the only place the
 * user can learn it.
 */
export function ProjectKeyField({
  projectId,
  projectKey,
  onSave,
  specCount,
}: ProjectKeyFieldProps) {
  const [draft, setDraft] = useState(projectKey);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The project's Spec count, counted here when the caller does not supply one. `null` = not
  // answered yet (or the read failed), which renders no warning — see `specCount`'s doc.
  const [counted, setCounted] = useState<number | null>(null);

  useEffect(() => {
    if (specCount !== undefined || !projectId) return;
    let cancelled = false;
    void nyxBridge
      .invoke<readonly unknown[]>("spec_list", { projectId })
      .then((specs) => {
        if (!cancelled) setCounted(specs.length);
      })
      // A failed count costs the WARNING, never the field: the key stays editable and the backend
      // still owns every refusal. Silently degrading here is the right trade — the alternative is
      // an error message about a sentence the user never asked for.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [projectId, specCount]);

  const specs = specCount ?? counted;
  const trimmed = draft.trim();
  const changed = trimmed !== projectKey;
  const wellFormed = isValidProjectKey(trimmed);
  const canSave = Boolean(projectId) && wellFormed && changed && !saving;

  const save = useCallback(async () => {
    if (!canSave) return;
    setSaving(true);
    setError(null);
    try {
      await onSave(trimmed);
      toast.success(`Specs are now keyed ${trimmed}`);
    } catch (e) {
      // The backend's OWN sentence — "another project already uses that key" is a fact only it
      // holds, and paraphrasing it here would be a second, less accurate rule.
      const reason = errorText(e, "Could not change the project key.");
      setError(reason);
      toast.error(reason);
    } finally {
      setSaving(false);
    }
  }, [canSave, onSave, trimmed]);

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
      className="mt-4 flex flex-col gap-1.5"
    >
      <label htmlFor="project-key" className="text-xs font-medium text-muted-foreground">
        Key
      </label>
      <div className="flex items-center gap-2">
        <Input
          id="project-key"
          value={draft}
          // UPPERCASED ON THE WAY IN, never on the way out: what the user sees is what is stored.
          onChange={(e) => {
            setDraft(e.target.value.toUpperCase());
            setError(null);
          }}
          aria-label="Project key"
          disabled={!projectId}
          maxLength={PROJECT_KEY_MAX_LEN}
          className="w-32 font-mono"
        />
        {/* The visible word is "Save", like the name's — two rows of the same pane, one gesture.
            The ACCESSIBLE name is not: the Global pane now has two Save buttons, and a screen
            reader (or a test) asked for "Save" would have no way to tell which. */}
        <Button
          type="submit"
          size="sm"
          aria-label="Save project key"
          loading={saving}
          disabled={!canSave}
        >
          Save
        </Button>
      </div>
      <span className="text-xs text-muted-foreground">
        The prefix your Specs are referenced by — <code>{projectKey || "KEY"}-1</code>,{" "}
        <code>{projectKey || "KEY"}-2</code>. {PROJECT_KEY_MIN_LEN} to {PROJECT_KEY_MAX_LEN}{" "}
        characters: a letter, then letters or digits.
      </span>

      {/* THE RE-KEYING WARNING — inline, above the button's consequence rather than after it. It
          shows only when there is something to re-key AND the field actually differs from what is
          stored: a warning that is always on screen is a warning nobody reads. */}
      {changed && wellFormed && specs != null && specs > 0 && (
        <p className="mt-1 flex items-start gap-1.5 rounded-md border border-warning/30 bg-warning/5 px-2.5 py-1.5 text-xs leading-relaxed text-warning">
          <TriangleAlertIcon className="mt-px size-3 shrink-0" />
          <span>
            {/* The VERB agrees too, not just the noun: "1 Spec reference this project" is the
                half-pluralised sentence a `?:` on the noun alone leaves behind. */}
            {specs} {specs === 1 ? "Spec references" : "Specs reference"} this project:{" "}
            <code>{projectKey}-1</code> becomes <code>{trimmed}-1</code>. References written down
            elsewhere stop resolving.
          </span>
        </p>
      )}

      {error && (
        <p role="alert" className="mt-1 flex items-center gap-1 text-xs text-destructive">
          <XCircleIcon className="size-3 shrink-0" />
          {error}
        </p>
      )}
    </form>
  );
}
