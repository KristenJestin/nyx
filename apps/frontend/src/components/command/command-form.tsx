import { useId, useState } from "react";
import { useForm } from "@tanstack/react-form";
import { z } from "zod";
import {
  AlertTriangleIcon,
  FolderIcon,
  Link2Icon,
  Link2OffIcon,
  RefreshCwIcon,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { pickDirectory, relativeToWorkspace } from "@/components/sidebar/folder-picker";
import type { CommandFormValues, ManagedCommand } from "./use-commands";

export interface CommandFormProps {
  /** When editing, the template being edited (pre-fills the fields). */
  editing?: ManagedCommand | null;
  /**
   * Absolute path of the workspace the command runs in. Used to relativize the
   * folder-picker's absolute result into a workspace-relative `subfolder` (the
   * only shape the backend accepts). Omitted (e.g. no workspace known yet)
   * disables the picker's relativization — it still works as a manual text field.
   */
  workspacePath?: string | null;
  /** True while the create/update command is in flight. */
  submitting?: boolean;
  /** A submission error to surface inline (e.g. duplicate name). */
  error?: string | null;
  /**
   * When editing a package.json-sourced command, the live on-disk script value
   * if it DRIFTED from what this command was last synced to (else `null`). Drives
   * the passive "changed in package.json" line + the Resync target — purely
   * informational, never an implicit rewrite.
   */
  driftValue?: string | null;
  /** Submit the (validated) form values. */
  onSubmit: (values: CommandFormValues) => void;
  /** Cancel the edit/create (collapses the inline form). */
  onCancel: () => void;
  /**
   * RESYNC the command to the source script's current raw body, KEEPING the link.
   * Present (rendered) only when editing a sourced command. Resolves after the
   * backend rewrite + re-list; the form is then re-seeded by the parent.
   */
  onResync?: (id: string) => Promise<void>;
  /**
   * UNLINK the package.json source (detach). Present only when editing a sourced
   * command. Sits on the LEFT of the action row.
   */
  onUnlink?: (id: string) => Promise<void>;
  /** Injectable folder picker (tests stub it instead of the Tauri plugin). */
  pick?: (title?: string) => Promise<string | null>;
}

/**
 * Zod v4 schema for the command form (review T2). `name`/`command` are required
 * (trimmed, non-empty); `subfolder` is optional; `restart_on_startup` is a bool.
 * Wired into TanStack Form as the `onChange` validator (a Standard Schema), so the
 * field validity — and therefore the submit gate (`canSubmit`) — derives from this
 * one declaration instead of a hand-rolled `canSubmit` boolean.
 */
const commandSchema = z.object({
  name: z.string().trim().min(1, "A name is required."),
  command: z.string().trim().min(1, "A command is required."),
  subfolder: z.string(),
  restart_on_startup: z.boolean(),
});

type CommandFormFields = z.input<typeof commandSchema>;

/**
 * `<CommandForm>` — the create / edit body for a project command template, shown
 * INLINE IN PLACE inside the command card (no floating card above the list).
 * Fields: `name`, `command`, an OPTIONAL `subfolder` (run path relative to the
 * workspace) with a native FOLDER PICKER, and a `restart_on_startup` toggle.
 *
 * Built on **@tanstack/react-form** with a **Zod v4** schema (`commandSchema`) as
 * the validator (review T2): the form store owns the field state + submit gate
 * (`canSubmit`), replacing the manual `useState` mirrors + the hand-rolled
 * `canSubmit` boolean. Each field is scaffolded by our `ui/field` `Field` (the
 * label/control/error wrapper, stacked or inline-split) and rendered through our
 * `ui/input` + `ui/switch` wrappers (never `@base-ui/react/*` directly). The folder
 * picker still relativizes its absolute result against the workspace and writes the
 * field via the form store.
 *
 * SOURCE ACTIONS LIVE HERE (review T2): when editing a package.json-sourced
 * command, the form shows a source-control block — the link reference, a passive
 * "changed in package.json" drift line when the script moved, and a single
 * **Resync** button that adopts the current raw script value while KEEPING the
 * link. The action row puts **Unlink from package.json** on the LEFT and
 * Cancel / Save on the RIGHT. There is no "reset to script runner". Manually
 * editing the command and saving DETACHES the source (handled by the backend
 * `command_update`); Resync does not.
 *
 * The Save button is contextual ("Create" / "Save") and disabled until the schema
 * validates (name + command non-empty).
 */
export function CommandForm({
  editing,
  workspacePath,
  submitting = false,
  error,
  driftValue,
  onSubmit,
  onCancel,
  onResync,
  onUnlink,
  pick = pickDirectory,
}: CommandFormProps) {
  // Inline error from the folder-picker when the chosen folder is OUTSIDE the
  // workspace (cleared on the next successful pick / manual edit).
  const [pickError, setPickError] = useState<string | null>(null);
  // A source action (resync / unlink) in flight, so we can show a spinner + lock
  // the buttons. `null` = idle.
  const [sourceBusy, setSourceBusy] = useState<null | "resync" | "unlink">(null);
  const [sourceError, setSourceError] = useState<string | null>(null);
  // Stable, unique ids so each `ui/field` `Field` (the wrapping `<label htmlFor>`)
  // explicitly associates with its control via htmlFor/id (a11y
  // label-has-associated-control) — the aria-labels stay as the accessible names
  // the tests/screen-readers query by.
  const nameId = useId();
  const commandId = useId();
  const subfolderId = useId();
  // The Switch is a Base UI `Switch.Root` (a non-native `role="switch"` span), so
  // there is no implicit label↔control link. We give it an explicit id and point
  // the wrapping `Field` (`<label htmlFor>`) at it so screen readers (and the
  // label-has-associated-control lint) associate them. The aria-label stays as the
  // stable accessible name the tests/screen-readers query by.
  const restartId = useId();

  const form = useForm({
    defaultValues: {
      name: editing?.name ?? "",
      command: editing?.command ?? "",
      subfolder: editing?.subfolder ?? "",
      restart_on_startup: editing?.restart_on_startup ?? false,
    } satisfies CommandFormFields,
    // `onMount` seeds the validity (so an EMPTY create form starts with the submit
    // button disabled — the old `canSubmit` gate); `onChange` re-validates live.
    validators: { onMount: commandSchema, onChange: commandSchema },
    onSubmit: ({ value }) => {
      onSubmit({
        name: value.name.trim(),
        command: value.command.trim(),
        subfolder: value.subfolder.trim(),
        restart_on_startup: value.restart_on_startup,
      });
    },
  });

  const hasSource = Boolean(editing?.source_script_name && editing?.source_package_json_path);
  const submitLabel = editing ? "Save" : "Create";

  const pickSubfolder = async () => {
    const picked = await pick("Select the run subfolder");
    if (!picked) return;
    // The picker returns an ABSOLUTE path. The backend (`resolve_subfolder`)
    // REJECTS any absolute subfolder outright — even one inside the workspace —
    // so we must relativize the picked path against the workspace before storing
    // it. A folder OUTSIDE the workspace cannot be expressed as an accepted
    // relative path (it would need a leading `..`), so we refuse it inline and
    // leave the field untouched. With no known workspace path we can't relativize
    // safely → also refuse rather than store an absolute path the backend rejects.
    if (!workspacePath) {
      setPickError("The folder must be inside the workspace.");
      return;
    }
    const relative = relativeToWorkspace(workspacePath, picked);
    if (relative === null) {
      setPickError("The folder must be inside the workspace.");
      return;
    }
    setPickError(null);
    form.setFieldValue("subfolder", relative);
  };

  const runSource = async (action: "resync" | "unlink", fn: () => Promise<void>) => {
    setSourceBusy(action);
    setSourceError(null);
    try {
      await fn();
      // The parent re-lists + re-seeds the form on success (resync rewrites the
      // command; unlink turns the card hand-authored).
    } catch (e) {
      setSourceError(typeof e === "string" ? e : "The source action failed.");
    } finally {
      setSourceBusy(null);
    }
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        e.stopPropagation();
        void form.handleSubmit();
      }}
      className="flex flex-col gap-3"
    >
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <form.Field name="name">
          {(field) => (
            <Field htmlFor={nameId} label="Name">
              <Input
                id={nameId}
                autoFocus
                value={field.state.value}
                onChange={(e) => field.handleChange(e.target.value)}
                onBlur={field.handleBlur}
                aria-label="Command name"
              />
            </Field>
          )}
        </form.Field>

        <form.Field name="command">
          {(field) => (
            <Field htmlFor={commandId} label="Command">
              <Input
                id={commandId}
                value={field.state.value}
                onChange={(e) => field.handleChange(e.target.value)}
                onBlur={field.handleBlur}
                aria-label="Command line"
                className="font-mono"
              />
            </Field>
          )}
        </form.Field>
      </div>

      <form.Field name="subfolder">
        {(field) => (
          <Field htmlFor={subfolderId} label="Subfolder (optional)" error={pickError}>
            <div className="flex items-center gap-2">
              <Input
                id={subfolderId}
                value={field.state.value}
                onChange={(e) => {
                  field.handleChange(e.target.value);
                  // A manual edit clears any prior out-of-workspace picker error.
                  setPickError(null);
                }}
                aria-label="Run subfolder"
                placeholder="Run from the workspace root"
                className="flex-1 font-mono"
              />
              <Button
                type="button"
                variant="outline"
                size="icon-sm"
                aria-label="Pick run subfolder"
                onClick={() => void pickSubfolder()}
              >
                <FolderIcon />
              </Button>
            </div>
          </Field>
        )}
      </form.Field>

      <form.Field name="restart_on_startup">
        {(field) => (
          <Field
            htmlFor={restartId}
            layout="inline"
            label="Restart on startup"
            description="Relaunch when the workspace opens"
          >
            <Switch
              id={restartId}
              checked={field.state.value}
              onCheckedChange={(checked) => field.handleChange(checked)}
              aria-label="Restart on startup"
            />
          </Field>
        )}
      </form.Field>

      {/* Source-control block — only when editing a package.json-sourced command.
          Read-only link reference + a passive drift line + the Resync action. */}
      {hasSource && editing && (
        <div className="flex flex-col gap-2 rounded-md border border-border bg-muted/50 p-2.5">
          <div className="flex items-center gap-2">
            <Link2Icon className="size-3 shrink-0 text-muted-foreground" />
            <span className="min-w-0 flex-1 truncate font-mono text-xs text-foreground/80">
              Linked to package.json · scripts.{editing.source_script_name}
            </span>
          </div>

          {driftValue != null && (
            <p
              data-testid="drift-line"
              className="flex items-start gap-1.5 text-xs leading-snug text-warning"
            >
              <AlertTriangleIcon className="mt-px size-3 shrink-0" />
              <span>
                Changed in package.json — now <code className="font-mono">{driftValue}</code>.
                Resync to adopt it; the link is kept.
              </span>
            </p>
          )}

          {sourceError && (
            <p role="alert" className="text-xs text-destructive">
              {sourceError}
            </p>
          )}

          <div>
            <Button
              type="button"
              variant="outline"
              size="xs"
              loading={sourceBusy === "resync"}
              disabled={sourceBusy !== null}
              onClick={() =>
                void runSource("resync", () => onResync?.(editing.id) ?? Promise.resolve())
              }
            >
              <RefreshCwIcon />
              Resync from package.json
            </Button>
          </div>
        </div>
      )}

      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}

      {/* Action row: Unlink on the LEFT (sourced only), Cancel / Save on the RIGHT. */}
      <div className="mt-1 flex items-center justify-between gap-2">
        <div className="flex items-center">
          {hasSource && editing && (
            <Button
              type="button"
              variant="ghost-destructive"
              size="xs"
              loading={sourceBusy === "unlink"}
              disabled={sourceBusy !== null}
              onClick={() =>
                void runSource("unlink", () => onUnlink?.(editing.id) ?? Promise.resolve())
              }
            >
              <Link2OffIcon />
              Unlink from package.json
            </Button>
          )}
        </div>
        <div className="flex gap-2">
          <Button type="button" variant="outline" size="sm" onClick={onCancel}>
            Cancel
          </Button>
          <form.Subscribe selector={(state) => state.canSubmit}>
            {(canSubmit) => (
              <Button
                type="submit"
                size="sm"
                loading={submitting}
                disabled={!canSubmit || submitting}
              >
                {submitLabel}
              </Button>
            )}
          </form.Subscribe>
        </div>
      </div>
    </form>
  );
}
