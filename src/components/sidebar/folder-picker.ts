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
