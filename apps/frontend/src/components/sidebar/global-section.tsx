import { useCallback, useState } from "react";
import { isBridgeError } from "@/bridge";
import { XCircleIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/toast";

/** Extract the real backend reason from a caught bridge/IPC error (see FEEDBACK #9). */
function errorReason(e: unknown, fallback: string): string {
  return isBridgeError(e) ? e.message : typeof e === "string" ? e : fallback;
}

export interface GlobalSectionProps {
  /** The project id whose settings are edited (null disables the controls). */
  projectId: string | null;
  /** The project's current display name (seeds the rename field). */
  projectName: string;
  /** The project's current `resume_agent_sessions` opt-in. */
  resumeAgentSessions: boolean;
  /**
   * Persist a rename. Resolves on success; rejects (string/Error) on failure so the
   * section can surface it inline and keep the edited value for a retry.
   */
  onRename: (name: string) => Promise<void>;
  /**
   * Persist a resume-opt-in toggle (takes effect immediately, independent of Save).
   * May return a promise so this section can toast the real reason on failure.
   */
  onResumeChange: (resume: boolean) => void | Promise<void>;
}

/**
 * `<GlobalSection>` — the **Global** pane of the project-settings modal: the
 * project-level settings that were, until now, only reachable through the app's
 * rename dialog (or, for `resume_agent_sessions`, not exposed at all — it had to be
 * flipped by editing the database by hand). This is their UI home.
 *
 *  - **Name** — an editable field with a Save action. Validation mirrors the rename
 *    dialog: a trimmed, non-empty value is required (Save is disabled otherwise), and
 *    a no-op rename (unchanged after trim) is also disabled.
 *  - **Resume agent sessions** — a toggle that, when on, makes nyx re-attach this
 *    project's terminals' active Claude sessions at relaunch (exact `--resume`) rather
 *    than opening a bare shell. Persisted immediately on flip (independent of Save).
 *
 * Controlled-from-props: the parent owns the authoritative `projectName` /
 * `resumeAgentSessions` (optimistically reflected on the tree) and the persistence
 * callbacks, so the toggle/name stay in sync with the sidebar.
 */
export function GlobalSection({
  projectId,
  projectName,
  resumeAgentSessions,
  onRename,
  onResumeChange,
}: GlobalSectionProps) {
  const [name, setName] = useState(projectName);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const trimmed = name.trim();
  // Save is enabled only for a non-empty value that actually changes the name (after
  // trim), and not while a save is in flight or the project id is missing.
  const canSave = Boolean(projectId) && trimmed.length > 0 && trimmed !== projectName && !saving;

  const save = useCallback(async () => {
    if (!canSave) return;
    setSaving(true);
    setError(null);
    try {
      await onRename(trimmed);
      toast.success("Project renamed");
    } catch (e) {
      const reason = errorReason(e, "Could not rename the project.");
      setError(reason);
      toast.error(reason);
    } finally {
      setSaving(false);
    }
  }, [canSave, onRename, trimmed]);

  // Persist the resume-opt-in toggle, toasting the outcome. The flip is optimistic
  // (the parent reflects it on the tree immediately); a backend failure surfaces the
  // real reason via the error toast.
  const toggleResume = useCallback(
    async (checked: boolean) => {
      try {
        await onResumeChange(checked);
        toast.success(checked ? "Agent-session resume enabled" : "Agent-session resume disabled");
      } catch (e) {
        toast.error(errorReason(e, "Could not update the resume setting."));
      }
    },
    [onResumeChange],
  );

  return (
    <section>
      <h3 className="text-base font-semibold text-foreground">Global</h3>
      <p className="mt-1 mb-4 text-sm text-muted-foreground">
        Project-level settings. They apply to every workspace of this project.
      </p>

      {/* Rename */}
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
        className="flex flex-col gap-1.5"
      >
        <label htmlFor="project-name" className="text-xs font-medium text-muted-foreground">
          Name
        </label>
        <div className="flex items-center gap-2">
          <Input
            id="project-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            aria-label="Project name"
            disabled={!projectId}
            className="flex-1"
          />
          <Button type="submit" size="sm" loading={saving} disabled={!canSave}>
            Save
          </Button>
        </div>
        {error && (
          <p role="alert" className="mt-1 flex items-center gap-1 text-xs text-destructive">
            <XCircleIcon className="size-3 shrink-0" />
            {error}
          </p>
        )}
      </form>

      {/* Resume agent sessions */}
      <label
        htmlFor="resume-agent-sessions"
        className="mt-5 flex items-start justify-between gap-3"
      >
        <span className="flex flex-col gap-0.5">
          <span className="text-sm font-medium text-foreground">Resume agent sessions</span>
          <span className="text-xs text-muted-foreground">
            On relaunch, resume this project's active Claude sessions (exact
            <code className="mx-1">--resume</code>) instead of a bare shell. Off by default.
          </span>
        </span>
        <Switch
          id="resume-agent-sessions"
          aria-label="Resume agent sessions on relaunch"
          checked={resumeAgentSessions}
          disabled={!projectId}
          onCheckedChange={(checked) => void toggleResume(checked)}
        />
      </label>
    </section>
  );
}
