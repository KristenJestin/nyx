import { useCallback, useState } from "react";
import { errorText } from "@/bridge";
import { XCircleIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/toast";
import { ProjectIdentity } from "./project-identity";
import { ProjectKeyField } from "./project-key-field";

/**
 * The project's VISUAL IDENTITY, as this pane needs it: the three stored fields plus the two
 * writes. Threaded from the caller (which owns `useProjects`) rather than read here, so the
 * optimistic tree update repaints every `<ProjectMark>` — rail, home, cross-project rows — the
 * moment a swatch is pressed, with no reload and no second source of truth.
 */
export interface GlobalSectionIdentity {
  /** The stored accent (a palette token name), or `null`. */
  accent: string | null;
  /** The stored glyph token, or `null`. Mutually exclusive with {@link imagePath}. */
  icon: string | null;
  /** The stored picture path (the copy nyx owns), or `null`. */
  imagePath: string | null;
  /** Persist an accent — `null` clears it. */
  onAccentChange: (accent: string | null) => void | Promise<void>;
  /** Persist the mark — a glyph, a picture, or `null` to drop it. */
  onMarkChange: (mark: { icon: string } | { image: string } | null) => void | Promise<void>;
}

export interface GlobalSectionProps {
  /** The project id whose settings are edited (null disables the controls). */
  projectId: string | null;
  /** The project's current display name (seeds the rename field). */
  projectName: string;
  /**
   * The project's current KEY — the prefix its Specs are referenced by (`NYX-42`). Threaded from
   * the caller (which owns `useProjects`) like the name, so the field re-seeds from the tree once
   * the write lands rather than from a local copy that could outlive a refusal.
   *
   * OPTIONAL, and its absence DROPS the field — the same rule this dialog already applies to whole
   * sections: a caller with no key to wire (and therefore no `onKeyChange`) gets no input, never a
   * disabled one.
   */
  projectKey?: string;
  /** The project's current `resume_agent_sessions` opt-in. */
  resumeAgentSessions: boolean;
  /**
   * Persist a rename. Resolves on success; rejects (string/Error) on failure so the
   * section can surface it inline and keep the edited value for a retry.
   */
  onRename: (name: string) => Promise<void>;
  /**
   * Persist a KEY change. Resolves on success; REJECTS with the backend's own sentence (malformed,
   * or already taken) so the field prints it under the input. Required alongside
   * {@link GlobalSectionProps.projectKey} — a key on screen with no way to write it is the
   * disabled control this dialog does not do.
   */
  onKeyChange?: (key: string) => Promise<void>;
  /**
   * Persist a resume-opt-in toggle (takes effect immediately, independent of Save).
   * May return a promise so this section can toast the real reason on failure.
   */
  onResumeChange: (resume: boolean) => void | Promise<void>;
  /**
   * Whether to render the Name/rename field. Defaults to `true`. Set `false` at an
   * entry point that must NOT expose project rename yet (phase-1 shell: rename/delete
   * belong to the PRD-B kebab CRUD) — only the resume toggle is shown then. `onRename`
   * is still required so the default (rename-enabled) callers stay unchanged.
   */
  showRename?: boolean;
  /**
   * The project's visual identity — the accent + mark block. REQUIRED wherever the pane is
   * rendered: it is the only surface through which a human dresses a project, so a caller that
   * forgot to wire it would silently take the capability away again.
   */
  identity: GlobalSectionIdentity;
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
 *  - **Key** — the prefix every Spec of the project is referenced by (`NYX-42`), until now derived
 *    from the name at creation and changeable only by editing the database. See
 *    [`ProjectKeyField`] for what it validates, what it warns about, and why the write is the one
 *    on this pane that is NOT optimistic.
 *  - **Identity** — the accent + mark block ([`ProjectIdentity`]), the only surface through which
 *    a human dresses a project. It is the pane's DOMINANT object (the one framed element, and the
 *    largest) but it sits UNDER the name, which is what the round-2 feedback settled: the section
 *    opens on what the project is CALLED, then on what it LOOKS like. Every choice persists on the
 *    press — no Save of its own.
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
  projectKey,
  resumeAgentSessions,
  onRename,
  onKeyChange,
  onResumeChange,
  showRename = true,
  identity,
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
      const reason = errorText(e, "Could not rename the project.");
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
        toast.error(errorText(e, "Could not update the resume setting."));
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

      {/* Rename — hidden where the entry point must not expose project rename yet
          (phase-1 shell; rename/delete = PRD-B kebab CRUD). */}
      {showRename && (
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
      )}

      {/* THE KEY, right under the NAME — the project's two identifiers, one for humans reading the
          sidebar and one for everything that references a Spec. Above the identity block for the
          same reason the name is: the section opens on what the project IS, then on what it looks
          like. Absent (with no `onKeyChange`) it renders nothing, the same rule as the rename. */}
      {projectKey !== undefined && onKeyChange && (
        <ProjectKeyField projectId={projectId} projectKey={projectKey} onSave={onKeyChange} />
      )}

      {/* Identity — the pane's dominant object, under the name and above the toggle (the order
          arbitrated on the round-3 prototype). */}
      <ProjectIdentity
        projectId={projectId}
        projectName={projectName}
        accent={identity.accent}
        icon={identity.icon}
        imagePath={identity.imagePath}
        onAccentChange={identity.onAccentChange}
        onMarkChange={identity.onMarkChange}
      />

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
