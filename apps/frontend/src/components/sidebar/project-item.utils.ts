import type { WorkspaceRecord } from "./use-projects";

/**
 * Whether a project's workspace SECTION should be shown. Variant A's "optional
 * workspace section", refined by the dogfood review ("keep rows, relabel root"):
 *  - HIDDEN for a mono-(root)workspace project — the project row expands
 *    straight into the root workspace's typed subsections (stays SHALLOW; no
 *    extra "main" row), as before;
 *  - VISIBLE (the "main" root row + named workspace rows, each with its own
 *    header) when the project has more than one workspace.
 *
 * Pure → unit-testable without rendering.
 */
export function showWorkspaceSection(workspaces: WorkspaceRecord[]): boolean {
  return workspaces.length > 1;
}

/** The smart default label for the ROOT workspace ("main", editable). */
export const DEFAULT_ROOT_LABEL = "main";

/**
 * The DISPLAY label for a workspace row, per the elected "smart default,
 * editable" naming:
 *
 *  - the ROOT workspace is shown as **"main"** by default — NEVER the project
 *    name — so a multi-workspace project never repeats its name (the project
 *    name lives only in the project header). A user-set custom root name (one
 *    that is neither the backend default `"root"` nor a stale copy of the
 *    project name) is honoured.
 *  - a NON-root workspace shows its stored `name`, which the create flow seeds
 *    with a short distinguishing label (see {@link defaultWorkspaceLabel}).
 *
 * Pure → unit-testable. `projectName` lets us strip the legacy duplication where
 * the root's stored name equals the project name (Images 3/4).
 */
export function workspaceDisplayLabel(workspace: WorkspaceRecord, projectName: string): string {
  if (!workspace.is_root) return workspace.name;
  const n = workspace.name.trim();
  // Treat the backend default ("root"), an empty name, or a stale copy of the
  // project name as "no meaningful custom label" → show the smart "main".
  if (n === "" || n === "root" || n === projectName) return DEFAULT_ROOT_LABEL;
  return workspace.name;
}

/**
 * The smart DEFAULT label for a NEWLY added (non-root) workspace, given the
 * picked folder `path` and the project's `rootPath`:
 *  - the path SEGMENT relative to the project root when the workspace is nested
 *    UNDER it (e.g. root `/proj`, folder `/proj/apps/web` → `apps/web`), so the
 *    label distinguishes it within the project rather than being a bare,
 *    possibly-duplicated folder basename;
 *  - the folder BASENAME otherwise (the workspace is not under the root).
 *
 * Always renamable afterwards. Splits on BOTH separators so a Windows or POSIX
 * path both work. Pure → unit-testable.
 */
export function defaultWorkspaceLabel(path: string, rootPath: string): string {
  const segs = (p: string) => p.split(/[/\\]+/).filter(Boolean);
  const pathSegs = segs(path);
  const rootSegs = segs(rootPath);

  const basename = pathSegs.length > 0 ? pathSegs[pathSegs.length - 1] : "";

  // Nested under the root? (every root segment is a prefix of the path's, and
  // the path is strictly deeper.) Compare case-insensitively so Windows/macOS
  // path casing does not defeat the nesting check.
  const eq = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
  if (
    rootSegs.length > 0 &&
    pathSegs.length > rootSegs.length &&
    rootSegs.every((s, i) => eq(s, pathSegs[i]))
  ) {
    return pathSegs.slice(rootSegs.length).join("/");
  }
  return basename;
}
