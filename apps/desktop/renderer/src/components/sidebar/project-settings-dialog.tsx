import { useState, type ComponentType } from "react";
import { SettingsIcon, SlidersHorizontalIcon, TagIcon, TerminalSquareIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Dialog, DialogBackdrop, DialogCloseX, DialogPopup } from "@/components/ui/dialog";
import { CommandsSection } from "@/components/command/commands-section";
import { CategoriesSection } from "./categories-section";
import { GlobalSection, type GlobalSectionIdentity } from "./global-section";

/** Which detail pane of the project-settings modal is shown. */
export type ProjectSettingsSection = "global" | "categories" | "commands";

interface SectionDef {
  id: ProjectSettingsSection;
  label: string;
  icon: ComponentType<{ className?: string }>;
}

/**
 * The project-settings sections, in rail order. Global first (the project-level
 * settings), then Categories (the project's genre-of-work vocabulary — PDR-0017's
 * human half), then Commands (the managed-command templates). Adding a future section
 * is a matter of appending a `SectionDef` here + rendering its pane in the detail
 * switch — the rail, selection state and navigation are generic.
 *
 * RESTRICTING the list is generic too, and that is what makes this ONE dialog serve both the
 * project scope and the Système singleton: a section is in the rail when — and only when — the
 * caller wired it. See [`ProjectSettingsDialogProps.globalSection`].
 */
const SECTIONS: readonly SectionDef[] = [
  { id: "global", label: "Global", icon: SlidersHorizontalIcon },
  { id: "categories", label: "Categories", icon: TagIcon },
  { id: "commands", label: "Commands", icon: TerminalSquareIcon },
];

/**
 * The Global pane's wiring — the project-level settings it persists. Its PRESENCE is what puts
 * `Global` in the rail: a caller with nothing to wire there (the Système singleton, whose backend
 * refuses both `update_project` and `set_project_resume_agent_sessions`) omits this object and the
 * section leaves the LIST. It is never rendered disabled.
 */
export interface ProjectSettingsGlobalPane {
  /** The project's current `resume_agent_sessions` opt-in (the pane's toggle). */
  resumeAgentSessions: boolean;
  /**
   * Whether the pane exposes the project rename field. Defaults to `true`. The phase-1 shell entry
   * point passes `false` so only the resume toggle is shown (rename/delete belong to the PRD-B
   * kebab CRUD); other callers keep the field via the default.
   */
  showRename?: boolean;
  /**
   * The project's VISUAL IDENTITY — the accent + mark block. Required alongside the rest of the
   * pane: a project scope that wires Global wires all of it, and there is no other surface where
   * a human can dress a project.
   */
  identity: GlobalSectionIdentity;
  /** Persist a project rename. */
  onRename: (name: string) => Promise<void>;
  /** Persist the resume-opt-in toggle. May return a promise so the section can toast the real
   *  reason on failure. */
  onResumeChange: (resume: boolean) => void | Promise<void>;
}

export interface ProjectSettingsDialogProps {
  /** Whether the modal is shown (controlled by the sidebar action). */
  open: boolean;
  /** The project id whose settings are edited (null while no project is targeted). */
  projectId: string | null;
  /** The project's display name (header subject, Global rename seed, Commands copy). */
  projectName: string;
  /**
   * The Global pane, wired — or OMITTED to drop the section from the rail entirely (Système).
   * There is no prop combination that renders it inert: no wiring, no section.
   */
  globalSection?: ProjectSettingsGlobalPane;
  /**
   * The Categories pane — the project's genre-of-work vocabulary (PDR-0017). `true` by default
   * because every USER project has Specs to classify; a caller whose container holds none (the
   * Système singleton, which carries commands and terminals and no Spec at all) passes `false`
   * and the section leaves the LIST, exactly like an unwired `globalSection`. Never rendered
   * disabled — a rail entry means a pane that works.
   */
  showCategories?: boolean;
  /**
   * A workspace of the project to scan for package.json imports (typically the
   * root). Omitted/`null` disables the Commands Import entry — AND the
   * `command_import_scripts` read behind it. On the Système scope that read is a walk of the
   * user's HOME, so the omission there is a feature; see [`SystemCommandsDialog`].
   */
  importWorkspaceId?: string | null;
  /** Absolute path of that workspace (threaded to the command form's folder picker). */
  workspacePath?: string | null;
  /** Which section to open on. Defaults to the FIRST section the caller wired. */
  initialSection?: ProjectSettingsSection;
  /**
   * The modal's title — and the stem of its two accessible names (`Close <title>` on the header's
   * cross, `<title> sections` on the rail). Defaults to `"Project settings"`.
   */
  title?: string;
  /** The muted subject after the title. Defaults to `projectName`; `null` renders none. */
  subject?: string | null;
  /** The header's icon. Defaults to the settings gear. */
  icon?: ComponentType<{ className?: string }>;
  /** Dismiss the modal (Close / backdrop / Escape). */
  onClose: () => void;
}

/**
 * `<ProjectSettingsDialog>` — THE command/settings modal of the app: a **left section rail**
 * (Global / Categories / Commands, whichever the caller wired) + a **right detail pane**,
 * mirroring the global [`SettingsDialog`] layout but scoped to ONE project.
 *
 * It supersedes the former standalone "Manage commands" modal: the Commands content
 * is migrated UNCHANGED into the Commands pane (the shared [`CommandsSection`]), and
 * the new Global pane finally exposes the project-level settings that were only
 * reachable through the rename dialog (or, for `resume_agent_sessions`, not at all —
 * it previously required a manual database edit). Both rename and the resume toggle
 * are wired to the SAME backend commands the rest of the app uses (`update_project` /
 * `set_project_resume_agent_sessions`).
 *
 * ## ONE chassis, a variable rail (human review 01M1EP31GWDR6FJEXY6E8CVDZZ, finding Y1)
 *
 * The Système scope used to carry a modal of its own — 104 lines that re-declared the popup, the
 * header, the dismissal and the sizing of THIS one to mount the very same body. Two envelopes to
 * keep in step for a single way of managing commands, and the Système one would have fallen behind
 * the day the other moved. There is one envelope now, and what the Système caller changes is the
 * LIST OF SECTIONS: it wires no [`ProjectSettingsGlobalPane`], so `Global` is not in the rail —
 * not present-and-disabled, absent. That matters because the singleton's backend refuses BOTH of
 * that pane's controls (`require_user_project` on the rename AND on the resume toggle), and the
 * PRD's criterion is "no surface offering them", not "a greyed-out one".
 *
 * The active section is seeded from `initialSection` on each open (the entry point
 * picks Global for "Project settings" / Commands for "Manage commands"), falling back to the first
 * section the caller actually wired. The dialog is keyed by the caller so it remounts per open and
 * the seed re-applies.
 */
export function ProjectSettingsDialog({
  open,
  projectId,
  projectName,
  globalSection,
  showCategories = true,
  importWorkspaceId,
  workspacePath,
  initialSection,
  title = "Project settings",
  subject,
  icon: HeaderIcon = SettingsIcon,
  onClose,
}: ProjectSettingsDialogProps) {
  // THE RAIL, REDUCED TO WHAT IS WIRED. `Global` needs its persistence callbacks to exist at all,
  // so a caller that has none simply does not get the entry — the one-line restriction the
  // `SECTIONS` comment promises, and the guarantee that no inert pane can be reached. `Categories`
  // follows the same rule with the only shape it can take (it wires nothing but the `projectId`):
  // a container with no Specs to classify opts out and the entry is ABSENT, not greyed.
  const sections = SECTIONS.filter((section) => {
    if (section.id === "global") return globalSection != null;
    if (section.id === "categories") return showCategories;
    return true;
  });
  const seeded =
    initialSection != null && sections.some((section) => section.id === initialSection)
      ? initialSection
      : (sections[0]?.id ?? "commands");
  const [activeSection, setActiveSection] = useState<ProjectSettingsSection>(seeded);
  const headerSubject = subject === undefined ? projectName : subject;

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
            same dialog a little wider, which is the accepted side of that trade.

            The rail is a fixed `w-44` WHATEVER its entry count, so the budget is the same for the
            one-section Système rail as for the two-section project one — 60rem stays exactly right
            in both. */}
        <DialogPopup className="flex max-h-[calc(100vh-4rem)] w-[min(60rem,calc(100vw-2rem))] flex-col overflow-hidden p-0">
          {/* Header — and the ONE dismissal control, at its end (human review
              01M140VQ4S434P2DMCJSTDR4VM). It replaces a footer that held a whole band at the bottom
              of the screen for a single "Close", and it is the shared `<DialogCloseX>` rather than a
              second hand-written cross. Escape and the outside press still close the dialog: the
              cross is a third path, not a substitute. */}
          <div className="flex items-center gap-2.5 border-b border-border px-5 py-4">
            <HeaderIcon className="size-4 shrink-0 text-muted-foreground" />
            <Dialog.Title className="flex items-center gap-2 text-base font-semibold">
              {title}
              {headerSubject != null && (
                <span className="font-normal text-muted-foreground">— {headerSubject}</span>
              )}
            </Dialog.Title>
            <DialogCloseX label={`Close ${title.toLowerCase()}`} onClick={onClose} />
          </div>

          {/* Body: left section rail + right detail pane. The floor is what keeps the Commands
              master/detail from collapsing to the height of whatever is selected — the catalogue
              must have somewhere to scroll.

              32rem, not 28: the footer's band (its `py-3.5`, its `sm` button and its border, ~4rem)
              is handed to the body rather than simply taken off the dialog (human review
              01M140VQ4S434P2DMCJSTDR4VM — « la hauteur reprise doit profiter au corps »). It stays
              in lockstep with `<CommandsSection>`'s own height, which is what actually scrolls. */}
          <div className="flex min-h-[32rem] flex-1">
            {/* Left rail of sections — whichever of Global / Categories / Commands the caller
                wired. */}
            <nav
              aria-label={`${title} sections`}
              className="w-44 shrink-0 border-r border-border bg-muted/30 p-2"
            >
              <ul className="flex flex-col gap-0.5">
                {sections.map((section) => {
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
            {activeSection === "global" && globalSection != null ? (
              <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-5 py-5">
                <GlobalSection
                  projectId={projectId}
                  projectName={projectName}
                  resumeAgentSessions={globalSection.resumeAgentSessions}
                  onRename={globalSection.onRename}
                  onResumeChange={globalSection.onResumeChange}
                  showRename={globalSection.showRename}
                  identity={globalSection.identity}
                />
              </div>
            ) : activeSection === "categories" ? (
              // Same wrapper as Global — a form-shaped pane that owns its padding and its scroll.
              // The list can outgrow the dialog on a project with a rich vocabulary, and that is
              // what has to scroll, not the rail beside it.
              <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-5 py-5">
                <CategoriesSection projectId={projectId} />
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
