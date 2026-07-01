import { useCallback, useRef, useState } from "react";
import { isBridgeError } from "@/bridge";

import { toast } from "@/components/ui/toast";
import { AddWorkspaceDialog } from "./add-workspace-dialog";
import { ProjectDialog, type ProjectDialogMode } from "./project-dialog";
import { basename, pickDirectory } from "./folder-picker";
import { DEFAULT_ROOT_LABEL, defaultWorkspaceLabel } from "./project-item.utils";
import type { ProjectTree, ProjectWithRoot, WorkspaceRecord } from "./use-projects";

/** Extract the real backend reason from a caught bridge/IPC error (see FEEDBACK #9). */
function errorReason(e: unknown, fallback: string): string {
  return isBridgeError(e) ? e.message : typeof e === "string" ? e : fallback;
}

export interface UseManualAddDeps {
  createProject: (name: string, rootPath: string, rootName?: string) => Promise<ProjectWithRoot>;
  createWorkspace: (projectId: string, name: string, path: string) => Promise<WorkspaceRecord>;
  /** Rename a project's display name (backend `update_project`). */
  updateProject?: (id: string, name: string) => Promise<void>;
  /** Toggle a project's resume-agent-sessions opt-in (PRD-5 #5). */
  setProjectResumeAgentSessions?: (id: string, resume: boolean) => Promise<void>;
  /** Delete a project + its workspaces (terminals detached, kept). */
  deleteProject?: (id: string) => Promise<void>;
  /** Delete a single (non-root) workspace (its instances cascade, terminals detached). */
  deleteWorkspace?: (id: string) => Promise<void>;
  /** Folder picker; injectable so tests stub it instead of the Tauri plugin. */
  pick?: (title?: string) => Promise<string | null>;
}

export interface UseManualAdd {
  /** Add a project: pick a folder → the create modal (editable name) → confirm. */
  addProject: () => Promise<void>;
  /** Add a workspace to `tree`'s project: pick a folder → name dialog → create. */
  addWorkspace: (tree: ProjectTree) => Promise<void>;
  /** Open the project EDIT (rename) modal for `tree`'s project. */
  editProject: (tree: ProjectTree) => void;
  /** Open the project DELETE confirmation modal for `tree`'s project. */
  removeProject: (tree: ProjectTree) => void;
  /**
   * Open the workspace DELETE confirmation modal for a single (non-root)
   * `workspace`. No-op for the root workspace (the backend rejects it; the UI
   * never offers the action there) — guarded so a stray call is inert.
   */
  removeWorkspace: (workspace: WorkspaceRecord) => void;
  /** The mounted dialogs (render once near the app root). */
  dialog: React.ReactNode;
}

/** Internal state of the add-workspace name dialog. */
interface WorkspaceDialogState {
  projectId: string;
  projectName: string;
  path: string;
  defaultName: string;
}

/** Internal state of the project create/edit/delete dialog. */
interface ProjectDialogState {
  mode: ProjectDialogMode;
  /**
   * The entity the `delete` flow targets (default `"project"`). `"workspace"`
   * reuses the same destructive-confirm dialog but removes a single workspace via
   * `deleteWorkspace`; create/edit are always projects.
   */
  entity?: "project" | "workspace";
  /** Present for `create`: the picked folder path. */
  path?: string;
  /** Present for `edit`/project-`delete`: the target project's id. */
  projectId?: string;
  /** Present for workspace-`delete`: the target workspace's id. */
  workspaceId?: string;
  defaultName: string;
  /** Present for `edit`: the project's current resume-agent-sessions opt-in. */
  resumeAgentSessions?: boolean;
}

/**
 * `useManualAdd` — the folder-picker-driven add/edit/delete flows for the sidebar
 * head and the per-project actions (PRD-2 Phase 2 + dogfood review).
 *
 *  - **addProject**: opens the native folder picker; on a pick, opens the project
 *    CREATE modal (folder shown read-only + an editable display NAME, pre-filled
 *    with the folder basename), mirroring add-workspace. On confirm it calls
 *    `create_project(name, root_path, rootName)` with the root workspace seeded
 *    to the smart `"main"` default (editable later), NOT the folder name.
 *  - **addWorkspace**: picker → a name dialog (pre-filled with a short
 *    distinguishing label — the path segment relative to the project root, else
 *    the basename) → `create_workspace`. A backend duplicate-path rejection is
 *    surfaced inline.
 *  - **editProject** / **removeProject**: open the project EDIT (rename) /
 *    DELETE-confirm modal.
 */
export function useManualAdd({
  createProject,
  createWorkspace,
  updateProject,
  setProjectResumeAgentSessions,
  deleteProject,
  deleteWorkspace,
  pick = pickDirectory,
}: UseManualAddDeps): UseManualAdd {
  const [wsDialog, setWsDialog] = useState<WorkspaceDialogState | null>(null);
  const [projDialog, setProjDialog] = useState<ProjectDialogState | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Stable React `key`s for the two dialogs across the OPEN→CLOSE transition.
  //
  // Each dialog must be remounted PER OPEN so its editable name re-initializes
  // from the fresh `defaultName` (no derived-state-in-effect reset). We do that
  // with a per-open GENERATION counter folded into the key — but the key must
  // NOT change when the dialog CLOSES: if it did, flipping the state to `null`
  // would swap the key and React would HARD-UNMOUNT the popup instantly,
  // destroying the Motion `motion.div` before its exit animation can run, so the
  // close would pop rather than fade (finding 01KV1SCHYESHDHHGX4X87H97CK).
  //
  // So we bump the generation on each closed→open EDGE only, and otherwise hold
  // the key steady. Result: every fresh open gets a NEW key (fresh remount, name
  // re-initialized) AND the key is stable through the close (the same instance
  // animates out; Base UI then unmounts it via the dialog's `actionsRef` once
  // Motion's exit completes).
  const wsGenRef = useRef(0);
  const wsWasOpenRef = useRef(false);
  if (wsDialog !== null && !wsWasOpenRef.current) wsGenRef.current += 1;
  wsWasOpenRef.current = wsDialog !== null;
  const wsKey = `ws:${wsGenRef.current}`;

  const projGenRef = useRef(0);
  const projWasOpenRef = useRef(false);
  if (projDialog !== null && !projWasOpenRef.current) projGenRef.current += 1;
  projWasOpenRef.current = projDialog !== null;
  const projKey = `proj:${projGenRef.current}`;

  // --- Project create/edit/delete -----------------------------------------

  const addProject = useCallback(async () => {
    const path = await pick("Select a project folder");
    if (!path) return; // cancelled
    setError(null);
    setProjDialog({
      mode: "create",
      path,
      defaultName: basename(path) || "project",
    });
  }, [pick]);

  const editProject = useCallback((tree: ProjectTree) => {
    setError(null);
    setProjDialog({
      mode: "edit",
      projectId: tree.project.id,
      defaultName: tree.project.name,
      resumeAgentSessions: tree.project.resume_agent_sessions,
    });
  }, []);

  const removeProject = useCallback((tree: ProjectTree) => {
    setError(null);
    setProjDialog({
      mode: "delete",
      projectId: tree.project.id,
      defaultName: tree.project.name,
    });
  }, []);

  const removeWorkspace = useCallback((workspace: WorkspaceRecord) => {
    // The root workspace can NEVER be removed on its own (the backend rejects it and
    // the sidebar hides the action there); guard so a stray call is inert.
    if (workspace.is_root) return;
    setError(null);
    setProjDialog({
      mode: "delete",
      entity: "workspace",
      workspaceId: workspace.id,
      defaultName: workspace.name,
    });
  }, []);

  const confirmProject = useCallback(
    async (name: string) => {
      if (!projDialog) return;
      setSubmitting(true);
      setError(null);
      try {
        if (projDialog.mode === "create" && projDialog.path) {
          // The project takes the (edited) display name; the root workspace gets
          // the smart "main" default — never the folder name (kills the
          // Image-3/4 duplication). Both are editable afterwards.
          await createProject(name, projDialog.path, DEFAULT_ROOT_LABEL);
          toast.success(`Project “${name}” created`);
        } else if (projDialog.mode === "edit" && projDialog.projectId) {
          await updateProject?.(projDialog.projectId, name);
          toast.success("Project renamed");
        } else if (
          projDialog.mode === "delete" &&
          projDialog.entity === "workspace" &&
          projDialog.workspaceId
        ) {
          await deleteWorkspace?.(projDialog.workspaceId);
          toast.success(`Workspace “${projDialog.defaultName}” removed`);
        } else if (projDialog.mode === "delete" && projDialog.projectId) {
          await deleteProject?.(projDialog.projectId);
          toast.success(`Project “${projDialog.defaultName}” deleted`);
        }
        setProjDialog(null); // success → close
      } catch (e) {
        // The error toast carries the REAL backend reason (e.g. a delete refused
        // because a command is still running, or removing the root workspace); a
        // trimmed line stays inline too.
        const reason = errorReason(e, "Could not complete the action. Please try again.");
        setError(reason);
        toast.error(reason);
      } finally {
        setSubmitting(false);
      }
    },
    [projDialog, createProject, updateProject, deleteProject, deleteWorkspace],
  );

  // --- Workspace add --------------------------------------------------------

  const addWorkspace = useCallback(
    async (tree: ProjectTree) => {
      const path = await pick(`Add a workspace to ${tree.project.name}`);
      if (!path) return; // cancelled
      setError(null);
      // Seed a short distinguishing default: the path segment relative to the
      // project root when nested under it, else the folder basename.
      const rootPath = tree.workspaces.find((w) => w.is_root)?.path ?? "";
      const defaultName = defaultWorkspaceLabel(path, rootPath) || basename(path) || "workspace";
      setWsDialog({
        projectId: tree.project.id,
        projectName: tree.project.name,
        path,
        defaultName,
      });
    },
    [pick],
  );

  const confirmWorkspace = useCallback(
    async (name: string) => {
      if (!wsDialog) return;
      setSubmitting(true);
      setError(null);
      try {
        await createWorkspace(wsDialog.projectId, name, wsDialog.path);
        setWsDialog(null); // success → close
        toast.success(`Workspace “${name}” added`);
      } catch (e) {
        // Surface the backend rejection (e.g. duplicate path in this project) both
        // inline (kept open so the user can pick a different folder) and as a toast.
        const reason = errorReason(
          e,
          "Could not add this folder (it may already be a workspace in this project).",
        );
        setError(reason);
        toast.error(reason);
      } finally {
        setSubmitting(false);
      }
    },
    [wsDialog, createWorkspace],
  );

  const dialog = (
    <>
      <AddWorkspaceDialog
        // Remount per picked path so the editable name re-initializes from the
        // fresh `defaultName` on each pick (no derived-state-in-effect reset).
        // The key is RETAINED while closing (see `wsGenRef`) so the exit can
        // animate instead of the popup being hard-unmounted on close.
        key={wsKey}
        open={wsDialog !== null}
        projectName={wsDialog?.projectName ?? ""}
        path={wsDialog?.path ?? ""}
        defaultName={wsDialog?.defaultName ?? ""}
        error={error}
        submitting={submitting}
        onConfirm={(name) => void confirmWorkspace(name)}
        onCancel={() => {
          setWsDialog(null);
          setError(null);
        }}
      />
      <ProjectDialog
        // Remount per flow so the editable name re-initializes correctly; the
        // key is RETAINED while closing (see `projGenRef`) so the exit animates
        // out instead of the popup being hard-unmounted on close.
        key={projKey}
        open={projDialog !== null}
        mode={projDialog?.mode ?? "create"}
        entity={projDialog?.entity}
        path={projDialog?.path}
        defaultName={projDialog?.defaultName ?? ""}
        resumeAgentSessions={projDialog?.resumeAgentSessions}
        onResumeAgentSessionsChange={
          setProjectResumeAgentSessions
            ? (resume) => {
                if (!projDialog?.projectId) return;
                const id = projDialog.projectId;
                // Reflect the toggle locally so the switch tracks immediately, then
                // persist (takes effect at once, independent of the name Save). The
                // toast surfaces the outcome (the real reason on a backend failure).
                setProjDialog((prev) => (prev ? { ...prev, resumeAgentSessions: resume } : prev));
                void setProjectResumeAgentSessions(id, resume).then(
                  () =>
                    toast.success(
                      resume ? "Agent-session resume enabled" : "Agent-session resume disabled",
                    ),
                  (e: unknown) =>
                    toast.error(errorReason(e, "Could not update the resume setting.")),
                );
              }
            : undefined
        }
        error={error}
        submitting={submitting}
        onConfirm={(name) => void confirmProject(name)}
        onCancel={() => {
          setProjDialog(null);
          setError(null);
        }}
      />
    </>
  );

  return { addProject, addWorkspace, editProject, removeProject, removeWorkspace, dialog };
}
