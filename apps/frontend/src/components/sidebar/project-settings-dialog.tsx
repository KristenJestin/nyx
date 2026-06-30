import { useState, type ComponentType } from "react";
import { SettingsIcon, SlidersHorizontalIcon, TerminalSquareIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";
import { CommandsSection } from "@/components/command/commands-section";
import { GlobalSection } from "./global-section";

/** Which detail pane of the project-settings modal is shown. */
export type ProjectSettingsSection = "global" | "commands";

interface SectionDef {
  id: ProjectSettingsSection;
  label: string;
  icon: ComponentType<{ className?: string }>;
}

/**
 * The project-settings sections, in rail order. Global first (the project-level
 * settings), then Commands (the managed-command templates). Adding a future section
 * is a matter of appending a `SectionDef` here + rendering its pane in the detail
 * switch — the rail, selection state and navigation are generic.
 */
const SECTIONS: readonly SectionDef[] = [
  { id: "global", label: "Global", icon: SlidersHorizontalIcon },
  { id: "commands", label: "Commands", icon: TerminalSquareIcon },
];

export interface ProjectSettingsDialogProps {
  /** Whether the modal is shown (controlled by the sidebar action). */
  open: boolean;
  /** The project id whose settings are edited (null while no project is targeted). */
  projectId: string | null;
  /** The project's current display name (header + Global rename seed). */
  projectName: string;
  /** The project's current `resume_agent_sessions` opt-in (Global toggle). */
  resumeAgentSessions: boolean;
  /**
   * A workspace of the project to scan for package.json imports (typically the
   * root). `null` disables the Commands Import tab.
   */
  importWorkspaceId?: string | null;
  /** Absolute path of that workspace (threaded to the command form's folder picker). */
  workspacePath?: string | null;
  /** Which section to open on (defaults to Global). */
  initialSection?: ProjectSettingsSection;
  /** Persist a project rename (Global). */
  onRename: (name: string) => Promise<void>;
  /** Persist the resume-opt-in toggle (Global). May return a promise so the section
   *  can toast the real reason on failure. */
  onResumeChange: (resume: boolean) => void | Promise<void>;
  /** Dismiss the modal (Close / backdrop / Escape). */
  onClose: () => void;
}

/**
 * `<ProjectSettingsDialog>` — the project-settings modal: a **left section rail**
 * (Global / Commands) + a **right detail pane**, mirroring the global
 * [`SettingsDialog`] layout but scoped to ONE project.
 *
 * It supersedes the former standalone "Manage commands" modal: the Commands content
 * is migrated UNCHANGED into the Commands pane (the shared [`CommandsSection`]), and
 * the new Global pane finally exposes the project-level settings that were only
 * reachable through the rename dialog (or, for `resume_agent_sessions`, not at all —
 * it previously required a manual database edit). Both rename and the resume toggle
 * are wired to the SAME backend commands the rest of the app uses (`update_project` /
 * `set_project_resume_agent_sessions`).
 *
 * The active section is seeded from `initialSection` on each open (the entry point
 * picks Global for "Project settings" / Commands for "Manage commands"). The dialog
 * is keyed by the caller so it remounts per open and the seed re-applies.
 */
export function ProjectSettingsDialog({
  open,
  projectId,
  projectName,
  resumeAgentSessions,
  importWorkspaceId,
  workspacePath,
  initialSection = "global",
  onRename,
  onResumeChange,
  onClose,
}: ProjectSettingsDialogProps) {
  const [activeSection, setActiveSection] = useState<ProjectSettingsSection>(initialSection);

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <Dialog.Portal>
        <DialogBackdrop />
        <DialogPopup className="flex max-h-[calc(100vh-4rem)] w-[min(46rem,calc(100vw-2rem))] flex-col overflow-hidden p-0">
          {/* Header */}
          <div className="flex items-center gap-2.5 border-b border-border px-5 py-4">
            <SettingsIcon className="size-4 shrink-0 text-muted-foreground" />
            <Dialog.Title className="flex items-center gap-2 text-base font-semibold">
              Project settings
              <span className="font-normal text-muted-foreground">— {projectName}</span>
            </Dialog.Title>
          </div>

          {/* Body: left section rail + right detail pane. */}
          <div className="flex min-h-0 flex-1">
            {/* Left rail of sections (Global / Commands). */}
            <nav
              aria-label="Project settings sections"
              className="w-44 shrink-0 border-r border-border bg-muted/30 p-2"
            >
              <ul className="flex flex-col gap-0.5">
                {SECTIONS.map((section) => {
                  const Icon = section.icon;
                  const selected = activeSection === section.id;
                  return (
                    <li key={section.id}>
                      <button
                        type="button"
                        aria-current={selected ? "page" : undefined}
                        onClick={() => setActiveSection(section.id)}
                        className={cn(
                          "flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-sm font-medium outline-none transition-colors",
                          "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
                          selected
                            ? "bg-secondary text-secondary-foreground"
                            : "text-muted-foreground hover:bg-muted hover:text-foreground",
                        )}
                      >
                        <Icon className="size-4 shrink-0 opacity-80" />
                        {section.label}
                      </button>
                    </li>
                  );
                })}
              </ul>
            </nav>

            {/* Right detail pane: the selected section. */}
            <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-5 py-5">
              {activeSection === "global" ? (
                <GlobalSection
                  projectId={projectId}
                  projectName={projectName}
                  resumeAgentSessions={resumeAgentSessions}
                  onRename={onRename}
                  onResumeChange={onResumeChange}
                />
              ) : (
                <CommandsSection
                  active={open && activeSection === "commands"}
                  projectId={projectId}
                  importWorkspaceId={importWorkspaceId}
                  workspacePath={workspacePath}
                />
              )}
            </div>
          </div>

          {/* Footer */}
          <div className="flex justify-end border-t border-border px-5 py-3.5">
            <Dialog.Close
              render={
                <Button variant="outline" size="sm">
                  Close
                </Button>
              }
              onClick={onClose}
            />
          </div>
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
