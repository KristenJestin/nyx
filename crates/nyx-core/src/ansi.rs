//! Shell-agnostic ANSI / terminal control-sequence stripping.
//!
//! Extracted from the Tauri `mcp_tools::strip_ansi` so BOTH shells (and the shared MCP
//! tool dispatch in [`crate::mcp_tools_core`]) render a single cleaned `output` field
//! through the IDENTICAL logic. Bounded CSI/OSC scanning so a malformed/unterminated
//! escape mid-buffer can never swallow the rest of the output.

/// Tab stops are every 8 columns — the universal terminal default, and the one `\t` is
/// expanded against here. nyx has no per-terminal tab-stop table (nothing sets DECST8C /
/// HTS through this path), so a single constant is the whole model.
const TAB_STOP: usize = 8;

/// The most spaces ONE cursor-advance sequence may contribute. A real
/// `SerializeAddon` frame never jumps further than the terminal is wide (a few hundred
/// columns), so this truncates nothing legitimate; it exists so a corrupted or hostile
/// `ESC[2000000000C` costs a bounded allocation instead of the whole heap.
const MAX_ADVANCE: usize = 4096;

/// Read the repeat count of a CUF parameter body. `""` (omitted) and `"0"` both mean
/// **1** — the VT rule for a `Ps` whose default is 1. ANY other shape (several parameters,
/// a private marker like `?`, an intermediate byte) yields `None`, which the caller treats
/// as "not a cursor advance I understand" and drops silently, exactly as before.
fn advance_count(body: &str) -> Option<usize> {
    if body.is_empty() {
        return Some(1);
    }
    if !body.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // A count that does not fit a usize is as good as "absurdly large" — clamp rather than
    // refuse, so the words on either side still end up separated.
    let n: usize = body.parse().unwrap_or(MAX_ADVANCE);
    Some(n.clamp(1, MAX_ADVANCE))
}

/// Remove ANSI/terminal control sequences from `input`, returning the readable text.
///
/// Handles:
/// - **CSI** (`ESC [ … final`): bounded param/intermediate scan (≤64 bytes) ending at a
///   final byte `0x40..=0x7E`; a malformed run (no final byte / too long) is left as text.
/// - **OSC** (`ESC ] … terminator`): consumes to `BEL` or `ST` (`ESC \`).
/// - **Two-char escapes** (`ESC (`, `ESC )`, …): drops the single following byte.
///
/// A NO-OP ON CLEAN TEXT — including its tabs, which are text: it removes control SEQUENCES and
/// changes nothing else. Preserves UTF-8. The terminal-frame reading, which reconstitutes a GRID
/// and therefore does expand tabs, is [`strip_ansi_frame`].
///
/// # Why this is LOSSY-AWARE about the cursor (the joined-words bug)
///
/// One CSI does not merely *decorate* text, it **stands for blank columns**, and deleting it
/// deletes the blanks: **CUF** (`ESC [ n C`, cursor forward) — `SerializeAddon` (the xterm addon
/// that re-serializes the grid nyx persists as scrollback) deliberately encodes a run of cells that
/// were never painted as a forward jump rather than as spaces. Lossless for a terminal replaying
/// the blob; destructive the moment the CSI is dropped for TEXT.
///
/// Dropping it is what produced the measured corruption of 2026-09-01: an agent's answer
/// came back with whole lines glued into one word (`"depot est uneCLIlocal-first"`) while the
/// neighbouring lines were intact — the difference being simply which blanks the TUI had
/// actually PAINTED and which it had skipped over. So a CUF is emitted as `n` spaces. Every OTHER
/// CSI is still removed without residue.
///
/// # Why ECH is NOT one of them (#K7)
///
/// **ECH** (`ESC [ n X`, erase characters) blanks n cells IN PLACE and does **not move the
/// cursor** — which is exactly how it differs from CUF, and why treating the two alike doubled the
/// blanks. `SerializeAddon` emits `ESC[nX` followed by `ESC[nC` for the SAME run of cells whenever
/// the current background differs from the default (`SerializeAddon.ts`: erase the run with the
/// live background, then step over it). Rendering both as n spaces AND advancing `col` twice turned
/// one blank cell into two, an indent of 4 into 8, and shifted every downstream tab stop — the very
/// corruption this function exists to repair, re-created for coloured regions (diff hunks, a
/// dialog's selected line, ncurses status bars).
///
/// So ECH is consumed and emits nothing: inside a frame the CUF that follows already carries the
/// blanks, and an ECH standing alone erases cells at the END of a line, whose disappearance costs a
/// reader nothing.
///
/// This is deliberately NOT a terminal emulator: there is no grid, no cursor addressing, no
/// back-tracking. `ESC[H`, `ESC[2J`, `ESC[nD` and friends still vanish, so a heavily repainted
/// frame is approximated, not reproduced (pixel-perfect TUI capture is explicitly out of
/// scope). What it buys is the thing a reader needs — the words stay separated.
pub fn strip_ansi(input: &str) -> String {
    clean(input, false)
}

/// [`strip_ansi`] for a TERMINAL FRAME — the `SerializeAddon` blob nyx persists as a terminal's
/// scrollback — where a `\t` is ALSO a blank-columns sequence and is expanded to the next
/// [`TAB_STOP`].
///
/// # Why the detab belongs here and not to [`strip_ansi`] (#K8)
///
/// A frame is a GRID re-serialized: everything in it stands for cells, and a tab that moves the
/// cursor without painting anything is the same kind of skipped blank as a CUF. Reconstituting it
/// is part of reading the screen.
///
/// A managed COMMAND's captured output is the opposite: a byte stream, line by line, already
/// faithful. Detabbing it silently rewrote a v1-frozen contract —
/// `get_command_output(grep: "^\\d+\\t\\d+\\t")` over `git diff --numstat` matched NOTHING because
/// the grep is applied to the cleaned form even when the caller asked `strip_ansi: false`, and any
/// TSV read with the default came back with its columns turned into runs of spaces.
///
/// Hence the split, along the line the two readings actually differ on: `read_terminal` /
/// `wait_for_terminal` call THIS, `get_command_output` / `wait_for_command` call [`strip_ansi`].
/// A real frame never contains a `\t` anyway (a tab moves the cursor without painting a cell, so
/// the serializer emits the cells, not the tab), which is why this costs the frame path nothing.
pub fn strip_ansi_frame(input: &str) -> String {
    clean(input, true)
}

/// The shared scanner behind [`strip_ansi`] and [`strip_ansi_frame`]. `detab` is the ONLY
/// difference between the two readings (see [`strip_ansi_frame`]).
fn clean(input: &str, detab: bool) -> String {
    const ESC: char = '\u{1b}';
    const BEL: char = '\u{7}';

    let mut out = String::with_capacity(input.len());
    // The column the NEXT emitted character lands on, counted in chars since the last line
    // break. Only `\t` reads it (tab stops are absolute), and only line breaks reset it.
    let mut col: usize = 0;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != ESC {
            match c {
                '\n' | '\r' => {
                    out.push(c);
                    col = 0;
                }
                '\t' if detab => {
                    // DETAB to the next stop — at least one space, never zero.
                    let width = TAB_STOP - (col % TAB_STOP);
                    for _ in 0..width {
                        out.push(' ');
                    }
                    col += width;
                }
                _ => {
                    out.push(c);
                    col += 1;
                }
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                const CSI_MAX: usize = 64;
                // The parameter/intermediate bytes, kept so a CUF can be given back its
                // blanks. The scan itself is byte-for-byte the bounded one it always was: a
                // malformed body (no final byte, or too long) still breaks out and leaves the
                // rest of the buffer as text.
                let mut body = String::new();
                let mut final_byte = None;
                while let Some(&p) = chars.peek() {
                    if ('\u{40}'..='\u{7e}').contains(&p) {
                        chars.next(); // final byte — end of the CSI
                        final_byte = Some(p);
                        break;
                    }
                    if body.len() >= CSI_MAX || !('\u{20}'..='\u{3f}').contains(&p) {
                        break; // not a valid CSI body (or too long) → leave as text
                    }
                    body.push(p);
                    chars.next();
                }
                // CUF (`C`) is the ONE final byte that MEANS blank columns the cursor stepped
                // over — give it back as spaces. ECH (`X`) blanks cells WITHOUT moving the
                // cursor, so it emits nothing and advances nothing (#K7). Every other final byte
                // is removed without residue, as before.
                if matches!(final_byte, Some('C')) {
                    if let Some(n) = advance_count(&body) {
                        for _ in 0..n {
                            out.push(' ');
                        }
                        col += n;
                    }
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

    /// THE TABLE — the sequences that stand for BLANK COLUMNS, each in the forms a real frame
    /// uses. CUF with a count, CUF with the count OMITTED (`Ps` defaults to 1), CUF with an
    /// explicit `0` (which the VT rule also reads as 1), ECH on its own and ECH followed by the CUF
    /// that pairs with it, and the tab.
    ///
    /// Read on [`strip_ansi_frame`] — the terminal-frame reading, the one that reconstitutes a grid
    /// and therefore expands tabs.
    #[test]
    fn blank_column_sequences_come_back_as_spaces() {
        let cases: &[(&str, &str)] = &[
            // CUF — `ESC [ n C`, the one SerializeAddon emits for cells never painted.
            ("une\u{1b}[1CCLI", "une CLI"),
            ("mot\u{1b}[4Csuivant", "mot    suivant"),
            // …with the parameter OMITTED: `ESC [ C` is n = 1.
            ("une\u{1b}[CCLI", "une CLI"),
            // …and an explicit 0, which VT reads as 1 (a `Ps` whose default is 1).
            ("une\u{1b}[0CCLI", "une CLI"),
            // ECH — `ESC [ n X` — blanks n cells IN PLACE and does NOT move the cursor, so the
            // `b` is written where the `a` left the cursor. This case used to read "a   b": the
            // erase was rendered as three spaces AND advanced the column, which is CUF's job, not
            // ECH's (#K7). A real terminal shows "ab".
            ("a\u{1b}[3Xb", "ab"),
            ("a\u{1b}[XB", "aB"),
            // …and the PAIR SerializeAddon actually emits for a run of cells on a non-default
            // background: erase with the live background (`X`), then step over it (`C`). ONE blank
            // cell must come back as ONE space — it used to come back as two.
            ("une\u{1b}[1X\u{1b}[1CCLI", "une CLI"),
            ("mot\u{1b}[4X\u{1b}[4Csuivant", "mot    suivant"),
            // TAB — expanded to the next 8-column stop, measured from the line start.
            ("a\tb", "a       b"),
            ("\tx", "        x"),
            ("12345678\tx", "12345678        x"),
            ("1234567\tx", "1234567 x"),
            // …and the column count restarts at every line break, not once per buffer.
            ("abcdefghij\r\na\tb", "abcdefghij\r\na       b"),
            // A CUF moves the column too, so a tab AFTER one lands on the right stop.
            ("a\u{1b}[5Cb\tc", "a     b c"),
            // …and an ECH does NOT, so the stop after one is unshifted.
            ("a\u{1b}[5X\u{1b}[5Cb\tc", "a     b c"),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                strip_ansi_frame(raw),
                *expected,
                "input {raw:?} must render as {expected:?}"
            );
        }
    }

    /// #K7 — A COLOURED REGION IS NOT DOUBLE-SPACED.
    ///
    /// `SerializeAddon` writes a run of blank cells as `ESC[nX ESC[nC` whenever the current
    /// background is not the default — a diff hunk, a dialog's selected line, an ncurses status
    /// bar. Reading `X` as if it were `C` doubled every one of those blanks and shifted the column
    /// for everything after, which is the corruption this module exists to repair.
    #[test]
    fn a_non_default_background_run_keeps_its_exact_width() {
        // The addon's real emission for "  DIFF  " indented by 4 on a coloured background.
        let frame = concat!(
            "\u{1b}[48;2;30;60;30m",
            "\u{1b}[4X\u{1b}[4C",
            "- ligne\u{1b}[2X\u{1b}[2Csupprimee",
            "\u{1b}[0m",
        );
        assert_eq!(strip_ansi_frame(frame), "    - ligne  supprimee");
        // The indent is FOUR columns, not eight — the assertion that names the bug.
        assert!(
            strip_ansi_frame(frame).starts_with("    -"),
            "an indent of 4 must not come back as 8: {:?}",
            strip_ansi_frame(frame)
        );
    }

    /// #K8 — `strip_ansi` KEEPS ITS PROMISE OF A NO-OP ON CLEAN TEXT, TABS INCLUDED.
    ///
    /// The command path (`get_command_output`, `wait_for_command`) reads a byte stream that is
    /// already line-faithful, and its `grep` is applied to the CLEANED form even when the caller
    /// asked for raw bytes. Detabbing there broke every pattern containing a `\t` — measured on
    /// `git diff --numstat` with `^\d+\t\d+\t`, which matched nothing — and silently rewrote any
    /// TSV read with the default `strip_ansi: true`.
    #[test]
    fn strip_ansi_leaves_tabs_alone_and_the_frame_reading_expands_them() {
        // The measured case: a numstat line, and the pattern an agent greps it with.
        let numstat = "12\t3\tsrc/main.rs\n0\t0\tREADME.md\n";
        assert_eq!(
            strip_ansi(numstat),
            numstat,
            "a no-op on clean text means the tabs too"
        );
        let re = regex::Regex::new(r"^\d+\t\d+\t").unwrap();
        assert_eq!(
            strip_ansi(numstat)
                .lines()
                .filter(|l| re.is_match(l))
                .count(),
            2,
            "a grep carrying a tab must match the cleaned form as it matches the raw one"
        );
        // Colour still goes, tabs still stay — the two are independent.
        assert_eq!(strip_ansi("\u{1b}[31ma\tb\u{1b}[0m"), "a\tb");
        // And the FRAME reading is the one that reconstitutes the grid.
        assert_eq!(strip_ansi_frame("a\tb"), "a       b");
    }

    /// NON-REGRESSION — every OTHER CSI is still removed WITHOUT residue. Only `C` means blank
    /// columns the cursor stepped over; a colour, a clear, a cursor home, a private DEC toggle, an
    /// erase-in-line, and a BACKWARD move all leave nothing behind, exactly as before.
    #[test]
    fn other_csi_sequences_are_still_removed_without_residue() {
        for raw in [
            "\u{1b}[0m",
            "\u{1b}[1;32m",
            "\u{1b}[38;2;153;153;153m",
            "\u{1b}[2J",
            "\u{1b}[H",
            "\u{1b}[K",
            "\u{1b}[?25l",
            "\u{1b}[?1049h",
            "\u{1b}[3D",
            "\u{1b}[10;20H",
        ] {
            assert_eq!(
                strip_ansi(&format!("a{raw}b")),
                "ab",
                "{raw:?} must vanish without residue"
            );
        }
        // A PRIVATE parameter on an otherwise cursor-forward final byte is not a plain CUF —
        // it is dropped rather than guessed at, so nothing spurious is inserted.
        assert_eq!(strip_ansi("a\u{1b}[?1Cb"), "ab");
        // A malformed CSI (no final byte) still leaves the buffer's remainder as text.
        assert_eq!(strip_ansi("a\u{1b}[38;5"), "a");
    }

    /// THE MEASURED BUG (2026-09-01 pilot run, PRD annex §3) — a `SerializeAddon` frame of an
    /// alt-screen TUI, read back through `strip_ansi`, must come out with every word SEPARATED.
    ///
    /// The frame below is written in `SerializeAddon`'s own encoding and reproduces the trace
    /// exactly: `xterm`'s serializer emits the cells a TUI actually PAINTED as text and the ones
    /// it merely skipped over as `ESC[nC`. That is why the corruption alternated — the first line
    /// of the measurement had lost every space (`"depot est uneCLIlocal-first(plusunepetiteUIweben"`)
    /// while the next kept them all. Here the two kinds of blank sit side by side on every line:
    /// the painted indent and the jumped-over word gaps.
    #[test]
    fn a_serialize_addon_alt_screen_frame_keeps_its_words_apart() {
        // Alt-screen preamble + a coloured, cursor-hidden frame, as the addon writes it.
        let frame = concat!(
            "\u{1b}[?1049h\u{1b}[?25l\u{1b}[H\u{1b}[2J",
            "  - Ce que c'est : depot est une\u{1b}[CCLI\u{1b}[Clocal-first\u{1b}[C(plus",
            "\u{1b}[Cune\u{1b}[Cpetite\u{1b}[CUI\u{1b}[Cweb\u{1b}[Cen\u{1b}[K\r\n",
            "    lecture seule via depot serve) qui transforme le travail des",
            "\u{1b}[Cagents\u{1b}[CIA\u{1b}[Cen\u{1b}[K\r\n",
            "    état de projet durable, stocké\u{1b}[Cdans\u{1b}[Cune\u{1b}[Cbase",
            "\u{1b}[CSQLite\u{1b}[Clocale\u{1b}[C—\u{1b}[Cau\u{1b}[Clieu\u{1b}[Cde\u{1b}[Cle\u{1b}[K\r\n",
            "\u{1b}[4Claisser\u{1b}[Cse\u{1b}[Cdissoudre\u{1b}[Cdans\u{1b}[Cl'historique",
            "\u{1b}[Cde\u{1b}[Cchat.\u{1b}[K\r\n",
            "\r\n",
            "\u{1b}[1mSENTINEL-ALPHA-OK\u{1b}[0m\r\n",
            "\u{1b}[38;2;153;153;153m✻ Worked\u{1b}[Cfor\u{1b}[C19s\u{1b}[C·\u{1b}[Cdone\u{1b}[C21:45\u{1b}[0m",
            "\u{1b}[?25h",
        );

        let cleaned = strip_ansi_frame(frame);

        assert_eq!(
            cleaned,
            concat!(
                "  - Ce que c'est : depot est une CLI local-first (plus une petite UI web en\r\n",
                "    lecture seule via depot serve) qui transforme le travail des agents IA en\r\n",
                "    état de projet durable, stocké dans une base SQLite locale — au lieu de le\r\n",
                "    laisser se dissoudre dans l'historique de chat.\r\n",
                "\r\n",
                "SENTINEL-ALPHA-OK\r\n",
                "✻ Worked for 19s · done 21:45",
            )
        );

        // THE ASSERTION THAT NAMES THE BUG: not one of the glued forms the measurement showed
        // survives, and the words the reader needs are there as words.
        for glued in [
            "uneCLI",
            "CLIlocal-first",
            "local-first(plus",
            "petiteUI",
            "UIweb",
            "desagents",
            "agentsIA",
            "stockédans",
            "baseSQLite",
            "localeau",
            "laissersedissoudre",
            "l'historiquede",
        ] {
            assert!(
                !cleaned.contains(glued),
                "the frame came back glued at {glued:?}: {cleaned}"
            );
        }
        for word in ["CLI", "local-first", "SQLite", "l'historique", "chat."] {
            assert!(
                cleaned.split_whitespace().any(|w| w == word),
                "{word:?} must stand as its own word"
            );
        }
        // The indentation the last line had lost entirely (its leading blanks were a CUF, not
        // painted spaces) is back — a line that starts at column 0 in the measurement.
        assert!(
            cleaned.contains("\n    laisser se dissoudre"),
            "the CUF-encoded indent is restored: {cleaned}"
        );
    }

    /// The bound: one absurd advance cannot balloon the output. A real frame never jumps further
    /// than the terminal is wide, so clamping truncates nothing legitimate — it only stops a
    /// corrupted count from allocating gigabytes.
    #[test]
    fn an_absurd_cursor_advance_is_clamped() {
        let cleaned = strip_ansi("a\u{1b}[2000000000Cb");
        assert_eq!(cleaned.len(), 2 + MAX_ADVANCE);
        assert!(cleaned.starts_with('a') && cleaned.ends_with('b'));
    }
}
