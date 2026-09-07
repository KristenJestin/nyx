import { useId, useState } from "react";

import { errorText } from "@/bridge";
import { useForm } from "@tanstack/react-form";
import { z } from "zod";
import { FolderIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { pickDirectory, relativeToWorkspace } from "@/components/sidebar/folder-picker";
import { CommandSourcePanel } from "./command-source-panel";
import { DetailSeparator } from "./command-detail-chrome";
import type { CommandFormValues, ManagedCommand } from "./use-commands";

/**
 * WHEN THE FIELDS ARE WRITTEN BACK.
 *
 *  - `submit` — the CREATION form: nothing exists yet, so there is a Create button and a Cancel.
 *  - `blur` — the EDITION panel: every field is committed as it LOSES FOCUS, and there is no Save
 *    button at all.
 *
 * The blur commit is the decision taken with the user, and it is the one `group-panel.tsx` already
 * takes for a group's name and colour (« every edit is committed as it is made »). The reason is
 * structural rather than stylistic: in a master/detail, a dirty form makes CHANGING THE SELECTION a
 * trap — either the edit is silently dropped or the user is stopped by a dialog for a field they
 * meant to abandon.
 *
 * THE ACCEPTED COST, stated so nobody re-litigates it: the backend REFUSES to edit a command that is
 * running in a workspace, so a committed field can come BACK to its previous value with an error
 * toast. The panel re-seeds from the stored template on failure, so what is on screen is always what
 * is stored — which is the property a Save button would have bought at the price above.
 */
export type CommandFormCommit = "submit" | "blur";

export interface CommandFormProps {
  /** When editing, the template being edited (pre-fills the fields). */
  editing?: ManagedCommand | null;
  /** See {@link CommandFormCommit}. Defaults to `submit` (the creation form). */
  commit?: CommandFormCommit;
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
   * the provenance panel's warning register — purely informational, never an implicit rewrite.
   */
  driftValue?: string | null;
  /** Submit the (validated) form values. */
  onSubmit: (values: CommandFormValues) => void;
  /** Cancel the create (collapses the form). Not offered in `blur` mode — there is nothing to cancel. */
  onCancel?: () => void;
  /**
   * RESYNC the command to the source script's current raw body, KEEPING the link.
   * Present (rendered) only when editing a sourced command. Resolves after the
   * backend rewrite + re-list; the form is then re-seeded by the parent.
   */
  onResync?: (id: string) => Promise<void>;
  /** UNLINK the package.json source (detach). Present only when editing a sourced command. */
  onUnlink?: (id: string) => Promise<void>;
  /** Injectable folder picker (tests stub it instead of the Tauri plugin). */
  pick?: (title?: string) => Promise<string | null>;
}

/**
 * Zod v4 schema for the command form (review T2). `name`/`command` are required
 * (trimmed, non-empty); `subfolder` and `description` are optional; `restart_on_startup` is a bool.
 * Wired into TanStack Form as the `onChange` validator (a Standard Schema), so the
 * field validity — and therefore the submit gate (`canSubmit`) — derives from this
 * one declaration instead of a hand-rolled `canSubmit` boolean.
 *
 * `description` is UNCONSTRAINED prose on purpose: the backend truncates it where it costs
 * something (the SessionStart block, capped at 200 characters) and stores it whole, so a length
 * rule here would only forbid what the reader already handles.
 */
const commandSchema = z.object({
  name: z.string().trim().min(1, "A name is required."),
  command: z.string().trim().min(1, "A command is required."),
  subfolder: z.string(),
  restart_on_startup: z.boolean(),
  description: z.string(),
});

type CommandFormFields = z.input<typeof commandSchema>;

/**
 * `<CommandForm>` — the create / edit body for a project command template, and the ONE placement
 * of both since the master/detail refonte (finding 01M132XFZ5KY8QMJ79HZZSBV3J): the creation used
 * to open at the top of the list and push everything down, the edition inside the card. They open
 * in the same panel now, so the difference between them is a `commit` mode and not a second layout.
 *
 * Fields: `name`, `command`, an OPTIONAL `subfolder` (run path relative to the workspace) with a
 * native FOLDER PICKER, an OPTIONAL multiline `description` — the NOTE FOR THE AGENT — and a
 * `restart_on_startup` toggle. The note sits between the subfolder and the toggle because it reads
 * as the last thing said ABOUT the command, before the one setting that changes its behaviour.
 *
 * The SAME form serves the project scope and the Système scope (`<SystemCommandsDialog>` mounts the
 * very same `<CommandsSection>`), so the note is offered on both without a second placement — which
 * is exactly what the shared-shell decision of review 01M1EP31GWDR6FJEXY6E8CVDZZ bought.
 *
 * Built on **@tanstack/react-form** with a **Zod v4** schema (`commandSchema`) as the validator: the
 * form store owns the field state + the submit gate (`canSubmit`). Each field is scaffolded by our
 * `ui/field` `Field` and rendered through our `ui/input` + `ui/switch` wrappers (never
 * `@base-ui/react/*` directly). The folder picker relativizes its absolute result against the
 * workspace and writes the field through the form store.
 *
 * THE PROVENANCE IS NOT DRAWN HERE any more — `<CommandSourcePanel>` is, once, with BOTH of its
 * answers attached to the link they act on. This file used to render a third rendering of the same
 * three facts and strand Unlink in its action row.
 */
export function CommandForm({
  editing,
  commit = "submit",
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
  const descriptionId = useId();
  // The Switch is a Base UI `Switch.Root` (a non-native `role="switch"` span), so
  // there is no implicit label↔control link. We give it an explicit id and point
  // the wrapping `Field` (`<label htmlFor>`) at it so screen readers (and the
  // label-has-associated-control lint) associate them. The aria-label stays as the
  // stable accessible name the tests/screen-readers query by.
  const restartId = useId();

  const seeded: CommandFormFields = {
    name: editing?.name ?? "",
    command: editing?.command ?? "",
    subfolder: editing?.subfolder ?? "",
    restart_on_startup: editing?.restart_on_startup ?? false,
    description: editing?.description ?? "",
  };

  const form = useForm({
    defaultValues: seeded,
    // `onMount` seeds the validity (so an EMPTY create form starts with the submit
    // button disabled — the old `canSubmit` gate); `onChange` re-validates live.
    validators: { onMount: commandSchema, onChange: commandSchema },
    onSubmit: ({ value }) => {
      onSubmit({
        name: value.name.trim(),
        command: value.command.trim(),
        subfolder: value.subfolder.trim(),
        restart_on_startup: value.restart_on_startup,
        description: value.description.trim(),
      });
    },
  });

  /**
   * COMMIT ON LEAVING A FIELD — the `blur` mode's whole behaviour.
   *
   * Two guards, and each removes a real misfire. The schema must PASS, or an emptied name would be
   * pushed to a backend that will reject it (and the field would then bounce back for a reason the
   * user did not cause). And the values must have CHANGED against what was seeded, so merely
   * TABBING THROUGH a panel writes nothing — which matters here more than usual, since a write on
   * a running command is refused and would raise an error toast for a field nobody touched.
   */
  const commitOnBlur = () => {
    if (commit !== "blur") return;
    const value = form.state.values;
    const parsed = commandSchema.safeParse(value);
    if (!parsed.success) return;
    const next: CommandFormValues = {
      name: value.name.trim(),
      command: value.command.trim(),
      subfolder: value.subfolder.trim(),
      restart_on_startup: value.restart_on_startup,
      description: value.description.trim(),
    };
    const unchanged =
      next.name === seeded.name.trim() &&
      next.command === seeded.command.trim() &&
      next.subfolder === seeded.subfolder.trim() &&
      next.restart_on_startup === seeded.restart_on_startup &&
      next.description === seeded.description.trim();
    if (unchanged) return;
    onSubmit(next);
  };

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
    // A pick is a deliberate edit that never blurs a field — it must commit on its own.
    commitOnBlur();
  };

  const runSource = async (action: "resync" | "unlink", fn: () => Promise<void>) => {
    setSourceBusy(action);
    setSourceError(null);
    try {
      await fn();
      // The parent re-lists + re-seeds the form on success (resync rewrites the
      // command; unlink turns the command hand-authored).
    } catch (e) {
      // The backend's OWN sentence when it has one (a pinned source, a missing script): it is
      // the only text that says what to do about it.
      setSourceError(errorText(e, "The source action failed."));
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
      <form.Field name="name">
        {(field) => (
          <Field htmlFor={nameId} label="Name">
            <Input
              id={nameId}
              autoFocus
              value={field.state.value}
              onChange={(e) => field.handleChange(e.target.value)}
              onBlur={() => {
                field.handleBlur();
                commitOnBlur();
              }}
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
              onBlur={() => {
                field.handleBlur();
                commitOnBlur();
              }}
              aria-label="Command line"
              className="font-mono"
            />
          </Field>
        )}
      </form.Field>

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
                onBlur={commitOnBlur}
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

      {/* THE NOTE FOR THE AGENT — the only field on this form whose reader is not the human filling
          it in. `command` and `subfolder` say what runs and where; this says WHEN and WHY, which is
          what an agent has no way to infer and what makes it start a command it should not (or skip
          one it should). The description under the field says who reads it, because a textarea
          labelled "Note" in a settings modal reads as a private annotation, and this one is not. */}
      <form.Field name="description">
        {(field) => (
          <Field
            htmlFor={descriptionId}
            label="Note for the agent (optional)"
            description="When and why to run this command — the agent reads it at the start of every session."
          >
            <Textarea
              id={descriptionId}
              value={field.state.value}
              onChange={(e) => field.handleChange(e.target.value)}
              onBlur={() => {
                field.handleBlur();
                commitOnBlur();
              }}
              aria-label="Note for the agent"
              placeholder="e.g. the test suite — replay it before saying the work is done."
            />
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
              onCheckedChange={(checked) => {
                field.handleChange(checked);
                // A switch has no blur to wait for — the flip IS the decision.
                if (commit === "blur") {
                  const value = form.state.values;
                  onSubmit({
                    name: value.name.trim(),
                    command: value.command.trim(),
                    subfolder: value.subfolder.trim(),
                    restart_on_startup: checked,
                    description: value.description.trim(),
                  });
                }
              }}
              aria-label="Restart on startup"
            />
          </Field>
        )}
      </form.Field>

      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}

      {/* THE PROVENANCE — the shared panel, with BOTH answers to a drift on the link they act on. */}
      {hasSource && editing && (
        <>
          <DetailSeparator className="my-1" />
          <CommandSourcePanel
            command={editing}
            driftValue={driftValue}
            busy={sourceBusy}
            error={sourceError}
            onResync={() =>
              void runSource("resync", () => onResync?.(editing.id) ?? Promise.resolve())
            }
            onUnlink={() =>
              void runSource("unlink", () => onUnlink?.(editing.id) ?? Promise.resolve())
            }
          />
        </>
      )}

      {/* The action row exists only where there is something to submit — see `CommandFormCommit`. */}
      {commit === "submit" && (
        <div className="mt-1 flex items-center justify-end gap-2">
          {onCancel && (
            <Button type="button" variant="outline" size="sm" onClick={onCancel}>
              Cancel
            </Button>
          )}
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
      )}
    </form>
  );
}
