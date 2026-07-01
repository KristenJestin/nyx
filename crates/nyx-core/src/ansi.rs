//! Shell-agnostic ANSI / terminal control-sequence stripping.
//!
//! Extracted from the Tauri `mcp_tools::strip_ansi` so BOTH shells (and the shared MCP
//! tool dispatch in [`crate::mcp_tools_core`]) render a single cleaned `output` field
//! through the IDENTICAL logic. Bounded CSI/OSC scanning so a malformed/unterminated
//! escape mid-buffer can never swallow the rest of the output.

/// Remove ANSI/terminal control sequences from `input`, returning the readable text.
///
/// Handles:
/// - **CSI** (`ESC [ … final`): bounded param/intermediate scan (≤64 bytes) ending at a
///   final byte `0x40..=0x7E`; a malformed run (no final byte / too long) is left as text.
/// - **OSC** (`ESC ] … terminator`): consumes to `BEL` or `ST` (`ESC \`).
/// - **Two-char escapes** (`ESC (`, `ESC )`, …): drops the single following byte.
///
/// A no-op on clean text; preserves UTF-8.
pub fn strip_ansi(input: &str) -> String {
    const ESC: char = '\u{1b}';
    const BEL: char = '\u{7}';

    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != ESC {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                const CSI_MAX: usize = 64;
                let mut n = 0;
                while let Some(&p) = chars.peek() {
                    if ('\u{40}'..='\u{7e}').contains(&p) {
                        chars.next(); // final byte — end of the CSI
                        break;
                    }
                    if n >= CSI_MAX || !('\u{20}'..='\u{3f}').contains(&p) {
                        break; // not a valid CSI body (or too long) → leave as text
                    }
                    chars.next();
                    n += 1;
                }
            }
            Some(']') => {
                while let Some(p) = chars.next() {
                    if p == BEL {
                        break;
                    }
                    if p == ESC {
                        if matches!(chars.peek(), Some('\\')) {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            Some(_) => {}
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_sgr_colors_keeping_text() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
    }

    #[test]
    fn noop_on_clean_text_and_preserves_utf8() {
        assert_eq!(strip_ansi("héllo wörld"), "héllo wörld");
    }

    #[test]
    fn removes_osc_title_sequence() {
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}done"), "done");
    }
}
