import chroma from "chroma-js";

/**
 * Dead-history restore: turn a terminal's persisted scrollback into the exact
 * bytes we write into a freshly-spawned xterm so the user sees their PREVIOUS
 * session above a visual separator, with the new shell's live output starting
 * below it.
 *
 * CRITICAL CONTRACT: these bytes go to `xterm.write(...)` ONLY — never to the
 * PTY. The history is read-only: nothing here is ever sent to the shell's stdin
 * (which would re-run the old commands). See `<Terminal>`'s injection effect and
 * its test, which asserts the history never reaches `pty_write`.
 */

/** The human label shown on the separator line between old history and the new session. */
export const RESTORE_SEPARATOR_LABEL = "previous session";

/**
 * Parse a colour string into its `[r, g, b]` 0-255 channels via chroma-js, whose
 * `.rgb()` returns 8-bit channels directly (no manual scaling/clamping). chroma
 * parses `#rgb`/`#rrggbb` and any CSS colour; we only feed it design-token-derived
 * hex. Returns `null` for anything chroma cannot parse (it throws). Pure.
 */
export function hexToRgb(hex: string): [number, number, number] | null {
  try {
    return chroma(hex.trim()).rgb();
  } catch {
    return null;
  }
}

/**
 * Build the dead-history payload to write into a restored terminal:
 *   <prior scrollback><CRLF><dim coloured separator line "── previous session ──"><reset><CRLF>
 *
 * The separator is coloured with a 24-bit SGR sequence built from `color` (a
 * `#rrggbb` string DERIVED FROM A DESIGN-SYSTEM TOKEN at the call site — never a
 * hardcoded colour in the markup). It is reset (`\x1b[0m`) immediately after so
 * the colour cannot bleed into the live shell output that follows.
 *
 * Returns `""` when there is no meaningful prior scrollback (a blank/whitespace
 * blob) — a brand-new terminal has no history and gets no separator.
 *
 * Pure → unit-tested. `color` falling back to an unparseable value yields an
 * UNcoloured (but still labelled) separator rather than garbage.
 */
export function buildDeadHistory(scrollback: string, color: string): string {
  if (!scrollback || scrollback.trim() === "") return "";

  const rgb = hexToRgb(color);
  const open = rgb ? `\x1b[38;2;${rgb[0]};${rgb[1]};${rgb[2]}m` : "";
  const close = "\x1b[0m";

  const separator = `${open}── ${RESTORE_SEPARATOR_LABEL} ──${close}`;

  // History verbatim, then a newline to break from the last history line, the
  // separator on its own line, then a final CRLF so the new shell's first prompt
  // lands on the line BELOW the separator.
  return `${scrollback}\r\n${separator}\r\n`;
}
