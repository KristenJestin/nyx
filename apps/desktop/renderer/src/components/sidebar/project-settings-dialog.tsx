import { useState, type ComponentType } from "react";
import { SettingsIcon, SlidersHorizontalIcon, TerminalSquareIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Dialog, DialogBackdrop, DialogCloseX, DialogPopup } from "@/components/ui/dialog";
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
  /**
   * Whether the Global pane exposes the project rename field. Defaults to `true`. The
   * phase-1 shell entry point passes `false` so only the resume toggle is shown (rename/
   * delete belong to the PRD-B kebab CRUD); other callers keep the field via the default.
   */
  showRename?: boolean;
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
  showRename = true,
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
        {/* 60rem = 960px — the ONE useful width left, now that the standalone commands modal is
            gone (finding 01M132YSBKJ5J22VG60P5GN7W0). It is what the Commands master/detail needs:
            176px of rail + 272px of catalogue + ~510px of detail panel. The Global pane is the
            same dialog a little wider, which is the accepted side of that trade. */}
        <DialogPopup className="flex max-h-[calc(100vh-4rem)] w-[min(60rem,calc(100vw-2rem))] flex-col overflow-hidden p-0">
          {/* Header — and the ONE dismissal control, at its end (human review
              01M140VQ4S434P2DMCJSTDR4VM). It replaces a footer that held a whole band at the bottom
              of the screen for a single "Close", and it is the shared `<DialogCloseX>` rather than a
              second hand-written cross. Escape and the outside press still close the dialog: the
              cross is a third path, not a substitute. */}
          <div className="flex items-center gap-2.5 border-b border-border px-5 py-4">
            <SettingsIcon className="size-4 shrink-0 text-muted-foreground" />
            <Dialog.Title className="flex items-center gap-2 text-base font-semibold">
              Project settings
              <span className="font-normal text-muted-foreground">— {projectName}</span>
            </Dialog.Title>
            <DialogCloseX label="Close project settings" onClick={onClose} />
          </div>

          {/* Body: left section rail + right detail pane. The floor is what keeps the Commands
              master/detail from collapsing to the height of whatever is selected — the catalogue
              must have somewhere to scroll.

              32rem, not 28: the footer's band (its `py-3.5`, its `sm` button and its border, ~4rem)
              is handed to the body rather than simply taken off the dialog (human review
              01M140VQ4S434P2DMCJSTDR4VM — « la hauteur reprise doit profiter au corps »). It stays
              in lockstep with `<CommandsSection>`'s own height, which is what actually scrolls. */}
          <div className="flex min-h-[32rem] flex-1">
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

            {/* Right detail pane: the selected section.

                THE PADDING AND THE SCROLL BELONG TO THE SECTION, not to this wrapper. Global is a
                form and wants both; Commands is itself a master/detail whose catalogue must reach
                the rail's own border and whose two columns scroll INDEPENDENTLY — a scroll here
                would move the list under the pointer mid-drag, which is the one thing that list
                promises never to do. */}
            {activeSection === "global" ? (
              <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-5 py-5">
                <GlobalSection
                  projectId={projectId}
                  projectName={projectName}
                  resumeAgentSessions={resumeAgentSessions}
                  onRename={onRename}
                  onResumeChange={onResumeChange}
                  showRename={showRename}
                />
              </div>
            ) : (
              <CommandsSection
                active={open && activeSection === "commands"}
                projectId={projectId}
                projectName={projectName}
                importWorkspaceId={importWorkspaceId}
                workspacePath={workspacePath}
              />
            )}
          </div>
          {/* NO FOOTER. Its only content was a "Close" the header's cross now carries, and the band
              it occupied is given back to the body above (`min-h`, and `<CommandsSection>`'s own
              height) rather than left blank — the master/detail's two columns scroll inside it, so
              every pixel it gains is a line of catalogue or of panel. */}
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
