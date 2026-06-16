/**
 * Folder-picker seam for the manual add-project / add-workspace flow (PRD-2
 * Phase 2, ZDZ). Wraps the Tauri dialog plugin's `open({ directory: true })` so
 * the rest of the front depends on a tiny, mockable surface (the unit tests
 * stub `pickDirectory` rather than the plugin).
 *
 * The plugin is lazily imported so the module graph does not pull the dialog
 * plugin into non-picker code paths, and so a jsdom test that never calls
 * `pickDirectory` needs no plugin mock.
 */

/**
 * Open the native folder picker and resolve with the chosen absolute path, or
 * `null` if the user cancelled. `title` labels the OS dialog. Single selection,
 * directories only.
 */
export async function pickDirectory(title?: string): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({ directory: true, multiple: false, title });
  // With `directory:true, multiple:false` the plugin returns a single path or
  // null. Guard the array shape defensively in case of a plugin/version quirk.
  if (Array.isArray(selected)) return selected[0] ?? null;
  return selected ?? null;
}

/**
 * The last path segment of an absolute folder path — the human-friendly default
 * NAME for a picked project/workspace (e.g. `C:\Users\kris\my-app` → `my-app`,
 * `/home/kris/my-app/` → `my-app`). Splits on BOTH separators so a Windows or
 * POSIX path both yield the folder name. Returns `""` when there is no segment
 * (a drive/filesystem root), letting the caller fall back to the backend default.
 * Pure → unit-testable.
 */
export function basename(path: string): string {
  const parts = path.split(/[/\\]+/).filter(Boolean);
  return parts.length > 0 ? parts[parts.length - 1] : "";
}

/**
 * True for a Windows-style absolute path: a drive-letter prefix (`C:\…`, `c:/…`)
 * or any backslash separator. POSIX paths are `/`-only. Used to decide whether the
 * containment comparison must fold case (NTFS is case-insensitive AND the backend
 * stores the workspace path lowercased via `pathnorm`).
 */
function isWindowsStylePath(path: string): boolean {
  return path.includes("\\") || /^[a-zA-Z]:/.test(path);
}

/**
 * Compute the path of `picked` RELATIVE to `workspacePath`, as a POSIX-ish
 * (`/`-separated) string the backend's `resolve_subfolder` accepts (relative, no
 * leading `..`). Both inputs are expected to be ABSOLUTE folder paths; the
 * comparison is segment-based and separator-agnostic so a Windows (`\`) or POSIX
 * (`/`) path both work. On Windows the comparison is CASE-INSENSITIVE (NTFS is
 * case-insensitive, and the backend stores the workspace path lowercased while the
 * native picker returns OS casing); on POSIX it stays case-sensitive.
 *
 * Returns:
 * - `""` when `picked` IS the workspace root (run at the root, no subfolder);
 * - a relative `/`-joined string (e.g. `packages/api`) when `picked` is strictly
 *   INSIDE the workspace;
 * - `null` when `picked` is OUTSIDE the workspace (a different root/drive, or a
 *   sibling/ancestor that would require a leading `..`) — the caller surfaces an
 *   inline error and leaves the field untouched, since the backend would reject
 *   such a subfolder at launch.
 *
 * Pure → unit-testable. No filesystem access (no symlink resolution); the backend
 * still applies its own canonical containment + existence checks before spawning.
 */
export function relativeToWorkspace(workspacePath: string, picked: string): string | null {
  const wsParts = workspacePath.split(/[/\\]+/).filter(Boolean);
  const pickedParts = picked.split(/[/\\]+/).filter(Boolean);

  // Fold case for Windows-style paths only: the stored workspace path is
  // lowercased by the backend's `pathnorm` while the native picker returns OS
  // casing, so a case-sensitive compare would reject every valid in-workspace pick
  // at the drive letter (`C:` vs `c:`). POSIX stays case-sensitive (the filesystem
  // is). `norm` is applied to BOTH sides so a Windows pick still matches a
  // lowercased workspace path.
  const fold = isWindowsStylePath(workspacePath) || isWindowsStylePath(picked);
  const norm = (segment: string) => (fold ? segment.toLowerCase() : segment);

  // `picked` must be a descendant-or-equal of the workspace: every workspace
  // segment must prefix the picked segments. Anything else (shorter, divergent
  // root/drive, sibling) is outside → null.
  if (pickedParts.length < wsParts.length) return null;
  for (let i = 0; i < wsParts.length; i++) {
    if (norm(pickedParts[i]) !== norm(wsParts[i])) return null;
  }
  // The remaining segments form the relative path (original picked casing); empty
  // = the workspace root.
  return pickedParts.slice(wsParts.length).join("/");
}
