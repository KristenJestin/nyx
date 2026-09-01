/**
 * `system-home-path` — the DISPLAY of a Système cwd, and only its display.
 *
 * The Système scope runs everything from the platform HOME, and the backend value is, always and
 * everywhere, the ABSOLUTE home (`system_scope_detail.home`, the workspace's `root_path`, the cwd
 * every `create_system_terminal` writes). Nothing here is ever sent back: `~` is a rendering, not a
 * value — a path handed to the backend as `~` would be a literal folder named "~", which is exactly
 * the bug the phase-1 bootstrap refuses to create ("jamais de fallback `.` ou `~`").
 *
 * So this module is one pure function, used by the sidebar's default-cwd foot and by the terminal
 * rows that fall back to their cwd for a name. It is deliberately NOT a general path utility: it
 * knows about one folder, the one the scope was bootstrapped at.
 */

/** Strip trailing separators and normalise `\` to `/` so the two platforms compare alike. */
function normalise(path: string): string {
  return path.replace(/\\/g, "/").replace(/\/+$/, "");
}

/**
 * Render `cwd` RELATIVE TO the Système home: the home itself becomes `~`, a descendant becomes
 * `~/sub/dir`, anything else is returned untouched (it is not the scope's home, so abbreviating it
 * would be a lie).
 *
 * The comparison is case-INSENSITIVE and separator-agnostic: Windows hands the same folder back as
 * `C:\Users\x` or `c:/Users/x` depending on who resolved it, and a display that flickers between
 * `~` and an absolute path on that difference is worse than no abbreviation at all. The slice is
 * taken from the ORIGINAL string, so the tail keeps the casing the backend gave it.
 *
 * `home` may be `null` (the scope is not loaded yet) — the cwd then shows as-is.
 */
export function systemCwdLabel(cwd: string, home: string | null | undefined): string {
  if (home == null) return cwd;
  const normalisedHome = normalise(home);
  if (normalisedHome.length === 0) return cwd;
  const normalisedCwd = normalise(cwd);
  const lowerHome = normalisedHome.toLowerCase();
  const lowerCwd = normalisedCwd.toLowerCase();
  if (lowerCwd === lowerHome) return "~";
  if (lowerCwd.startsWith(`${lowerHome}/`))
    return `~/${normalisedCwd.slice(normalisedHome.length + 1)}`;
  return cwd;
}

/** The label the sidebar foot shows for the scope's DEFAULT cwd — always the home, always `~`. */
export const SYSTEM_DEFAULT_CWD_LABEL = "~";
