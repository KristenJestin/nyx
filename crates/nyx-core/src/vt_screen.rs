//! Headless VT screen model (v2.2.5 — terminal persistence, PRD
//! `01KWMDBKW7GZ0X310TK64QEN32`, Phase 2).
//!
//! ## Why this exists
//!
//! The sidecar has always persisted a terminal's *scrollback* — an opaque TEXT blob the
//! renderer serialized from xterm and handed back to [`crate::db::persist_scrollback`]
//! (`terminals.scrollback`, bounded to the last ~256 KiB). That is enough to REPLAY a
//! line-based history, but it is NOT enough to restore a full-screen TUI: a program like
//! `vim`/`htop`/`claude` paints into the terminal's **alternate screen**, whose contents
//! live entirely off the scrollback (the alt-screen is never appended to history). After a
//! restart, replaying the scrollback therefore drops the TUI's actual screen.
//!
//! This module is the missing piece: a **headless VT emulator** the sidecar drives with the
//! same raw PTY bytes it already streams to the renderer, maintaining the *current screen*
//! (a grid of cells with colors/attrs, the cursor, and the main ↔ alternate screen split)
//! so a faithful [`ScreenSnapshot`] can be captured for lossless re-hydration at restart.
//!
//! ## Division of labour (PRD)
//!
//! The PRD is explicit that the **correctness of VT rendering** at runtime is ghostty's job
//! (in the renderer, via wterm) — this model is NOT a second renderer. Its sole purpose is a
//! **restore-time snapshot** that is faithful enough to bring a TUI back on screen. So it is
//! deliberately bounded and pragmatic:
//!   * it tracks a fixed `rows × cols` grid for BOTH the main and the alternate screen;
//!   * it handles the control sequences that move/erase/style the grid and switch screens —
//!     the ones that decide "what is on screen right now";
//!   * it is **independent of the front lib** (no wterm/xterm/ghostty type crosses its API):
//!     the snapshot is plain serde data (`serde` derive), round-trippable to JSON/MessagePack.
//!
//! It is NOT a byte-perfect terminal: exotic sequences it does not recognize are skipped
//! (never corrupting the grid), and pixel-level fidelity / full scrollback reflow are out of
//! scope (the sidecar keeps the separate bounded scrollback for history).

use serde::{Deserialize, Serialize};

/// A single terminal color. `Default` is the terminal's default fg/bg (no explicit color);
/// `Indexed` is a 0–255 palette index (the 16 ANSI colors + the 6×6×6 cube + grayscale);
/// `Rgb` is a 24-bit truecolor triple. Front-independent: the renderer maps these to its own
/// palette on restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Color {
    /// The terminal default (no SGR color set).
    #[default]
    Default,
    /// A palette index 0..=255 (SGR 30-37/40-47, 90-97/100-107, and 38;5/48;5).
    Indexed { index: u8 },
    /// 24-bit truecolor (SGR 38;2 / 48;2).
    Rgb { r: u8, g: u8, b: u8 },
}

/// The visual attributes of a cell, packed into a small bitset so a snapshot stays compact.
/// Serialized as an integer (the raw bits) so the wire form is stable and lib-independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Attrs(pub u16);

impl Attrs {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const BLINK: u16 = 1 << 4;
    pub const REVERSE: u16 = 1 << 5;
    pub const HIDDEN: u16 = 1 << 6;
    pub const STRIKE: u16 = 1 << 7;

    fn set(&mut self, bit: u16, on: bool) {
        if on {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }

    /// Whether a given attribute bit is set.
    pub fn contains(self, bit: u16) -> bool {
        self.0 & bit != 0
    }
}

/// One screen cell: the character it holds plus its styling. A blank cell is a space with the
/// default pen. `width` is the display width (1 for normal, 2 for a wide/CJK char); the cell
/// immediately to the right of a wide char is a **spacer** (`width == 0`) so the grid stays a
/// fixed `cols`-wide array while the wide glyph still occupies two columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// The character in this cell (a space for a blank cell).
    pub ch: char,
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Packed visual attributes.
    pub attrs: Attrs,
    /// Display width: 1 (normal), 2 (wide glyph occupies this + the next column), or 0 (the
    /// right-hand spacer column of a wide glyph).
    pub width: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::default(),
            width: 1,
        }
    }
}

/// The current "pen": the color/attribute state new characters are written with. Reset by
/// `SGR 0` and mutated by the other SGR codes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Pen {
    fg: Color,
    bg: Color,
    attrs: Attrs,
}

/// One rectangular buffer of cells (`rows × cols`) plus the cursor within it. The emulator
/// holds two — the main screen and the alternate screen — and switches the ACTIVE one when a
/// program enters/leaves the alt-screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenBuffer {
    pub rows: usize,
    pub cols: usize,
    /// Row-major cells: `cells[row * cols + col]`. Always exactly `rows * cols` long.
    pub cells: Vec<Cell>,
    /// Cursor row (0-based, clamped to `0..rows`).
    pub cursor_row: usize,
    /// Cursor column (0-based, clamped to `0..cols`).
    pub cursor_col: usize,
}

impl ScreenBuffer {
    fn new(rows: usize, cols: usize) -> Self {
        ScreenBuffer {
            rows,
            cols,
            cells: vec![Cell::default(); rows * cols],
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    #[inline]
    fn idx(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }

    fn clear_all(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
    }

    /// The row at `row` as a trimmed `String` (trailing blanks removed) — a convenience for
    /// tests and for a text-only fidelity check of a snapshot.
    pub fn row_text(&self, row: usize) -> String {
        if row >= self.rows {
            return String::new();
        }
        let start = row * self.cols;
        let mut s: String = self.cells[start..start + self.cols]
            .iter()
            .filter(|c| c.width != 0) // skip wide-char spacers
            .map(|c| c.ch)
            .collect();
        let trimmed = s.trim_end();
        s.truncate(trimmed.len());
        s
    }
}

/// A serializable snapshot of a terminal's CURRENT screen state — the payload the sidecar
/// persists / hands the renderer for lossless re-hydration. Deliberately **front-lib
/// independent**: plain serde data, round-trippable to JSON or MessagePack, carrying enough
/// to repaint a TUI (both screen buffers, which one is active, the cursor).
///
/// A schema `version` is embedded so a future model change can be migrated rather than
/// mis-parsed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenSnapshot {
    /// Snapshot schema version (currently 1).
    pub version: u16,
    /// The main-screen buffer (the normal scrolling screen).
    pub main: ScreenBuffer,
    /// The alternate-screen buffer, present ONLY when the terminal is currently in the
    /// alt-screen (a full-screen TUI). `None` on the main screen.
    pub alt: Option<ScreenBuffer>,
    /// Whether the alternate screen is the ACTIVE one right now (a TUI is on screen).
    pub alt_active: bool,
    /// Whether the cursor is visible (DECTCEM). Restored so a TUI that hid its cursor stays
    /// hidden.
    pub cursor_visible: bool,
}

/// The current schema version of a [`ScreenSnapshot`].
pub const SNAPSHOT_VERSION: u16 = 1;

impl ScreenSnapshot {
    /// Serialize this snapshot into a **re-hydration byte stream** — a sequence of standard VT
    /// / ANSI control sequences that, when written into a FRESH terminal, reconstruct this
    /// screen (main content, and the alternate screen if a TUI was on it), cursor position and
    /// visibility included.
    ///
    /// This is the crux of the restart protocol (v2.2.5): the renderer does NOT need to
    /// understand the snapshot's grid model. It creates a fresh wterm/ghostty terminal and
    /// simply `write()`s these bytes — ghostty (the VT authority per the PRD) rebuilds the exact
    /// grid, alt-screen included. So the sidecar owns the *snapshot*, ghostty owns *correctness*,
    /// and the wire payload is **front-lib independent** (plain VT bytes, not a wterm/xterm API).
    ///
    /// The emitted stream:
    ///   1. resets attributes + clears the screen and homes the cursor (a clean canvas);
    ///   2. paints the MAIN buffer row by row with SGR runs, then positions the main cursor;
    ///   3. if the terminal was in the alt-screen, enters it (DECSET 1049), paints the ALT
    ///      buffer, and positions the alt cursor;
    ///   4. applies cursor visibility (DECTCEM) last.
    ///
    /// Idempotent by construction: writing it into any fresh terminal yields the same screen,
    /// and it starts with a reset+clear so re-applying it over an existing screen is safe too.
    pub fn to_rehydration_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // Clean canvas: soft reset attributes, clear the (main) screen, home the cursor.
        out.extend_from_slice(b"\x1b[0m\x1b[2J\x1b[H");
        // Paint the main buffer (the history the user sees when a TUI exits).
        paint_buffer(&mut out, &self.main);

        if self.alt_active {
            // Enter the alternate screen and paint the TUI grid there. DECSET 1049 is the
            // save-cursor+use-alt variant every modern TUI uses, so the renderer's ghostty
            // ends up in exactly the alt-screen state it would be in live.
            out.extend_from_slice(b"\x1b[?1049h");
            out.extend_from_slice(b"\x1b[0m\x1b[2J\x1b[H");
            if let Some(alt) = &self.alt {
                paint_buffer(&mut out, alt);
            }
        }

        // Cursor visibility LAST so a TUI that hid its cursor stays hidden after restore.
        if self.cursor_visible {
            out.extend_from_slice(b"\x1b[?25h");
        } else {
            out.extend_from_slice(b"\x1b[?25l");
        }
        out
    }
}

/// Paint one screen buffer into an ANSI byte stream: each non-blank row is positioned with a
/// CUP, then its cells are emitted with SGR runs (a new SGR is written only when the pen
/// changes, keeping the stream compact). Trailing blank cells on a row are skipped. Ends by
/// positioning the cursor at the buffer's cursor cell and resetting the pen.
fn paint_buffer(out: &mut Vec<u8>, buf: &ScreenBuffer) {
    let mut cur_pen: Option<(Color, Color, Attrs)> = None;
    for row in 0..buf.rows {
        // The last non-blank column of this row (skip painting trailing blanks).
        let start = row * buf.cols;
        let row_cells = &buf.cells[start..start + buf.cols];
        let last = row_cells.iter().rposition(|c| {
            c.width != 0
                && (c.ch != ' '
                    || c.fg != Color::Default
                    || c.bg != Color::Default
                    || c.attrs != Attrs::default())
        });
        let Some(last) = last else { continue };
        // Position at the row start (1-based CUP).
        out.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
        let mut col = 0;
        while col <= last {
            let cell = &row_cells[col];
            if cell.width == 0 {
                // Right-hand spacer of a wide glyph already emitted with the wide char.
                col += 1;
                continue;
            }
            let pen = (cell.fg, cell.bg, cell.attrs);
            if cur_pen != Some(pen) {
                emit_sgr(out, cell.fg, cell.bg, cell.attrs);
                cur_pen = Some(pen);
            }
            let mut buf4 = [0u8; 4];
            out.extend_from_slice(cell.ch.encode_utf8(&mut buf4).as_bytes());
            col += 1;
        }
        // Reset the pen at the end of the row so a colored run does not bleed to the next.
        out.extend_from_slice(b"\x1b[0m");
        cur_pen = Some((Color::Default, Color::Default, Attrs::default()));
    }
    // Finally, place the cursor where the buffer's cursor is (1-based CUP).
    out.extend_from_slice(
        format!("\x1b[{};{}H", buf.cursor_row + 1, buf.cursor_col + 1).as_bytes(),
    );
}

/// Emit the SGR sequence that sets exactly this pen (reset first, then attrs, then colors), so
/// the run that follows is styled correctly regardless of the prior pen.
fn emit_sgr(out: &mut Vec<u8>, fg: Color, bg: Color, attrs: Attrs) {
    let mut codes: Vec<String> = vec!["0".to_string()]; // reset baseline
    if attrs.contains(Attrs::BOLD) {
        codes.push("1".into());
    }
    if attrs.contains(Attrs::DIM) {
        codes.push("2".into());
    }
    if attrs.contains(Attrs::ITALIC) {
        codes.push("3".into());
    }
    if attrs.contains(Attrs::UNDERLINE) {
        codes.push("4".into());
    }
    if attrs.contains(Attrs::BLINK) {
        codes.push("5".into());
    }
    if attrs.contains(Attrs::REVERSE) {
        codes.push("7".into());
    }
    if attrs.contains(Attrs::HIDDEN) {
        codes.push("8".into());
    }
    if attrs.contains(Attrs::STRIKE) {
        codes.push("9".into());
    }
    match fg {
        Color::Default => {}
        Color::Indexed { index } if index < 8 => codes.push(format!("{}", 30 + index)),
        Color::Indexed { index } if index < 16 => codes.push(format!("{}", 90 + index - 8)),
        Color::Indexed { index } => codes.push(format!("38;5;{index}")),
        Color::Rgb { r, g, b } => codes.push(format!("38;2;{r};{g};{b}")),
    }
    match bg {
        Color::Default => {}
        Color::Indexed { index } if index < 8 => codes.push(format!("{}", 40 + index)),
        Color::Indexed { index } if index < 16 => codes.push(format!("{}", 100 + index - 8)),
        Color::Indexed { index } => codes.push(format!("48;5;{index}")),
        Color::Rgb { r, g, b } => codes.push(format!("48;2;{r};{g};{b}")),
    }
    out.extend_from_slice(format!("\x1b[{}m", codes.join(";")).as_bytes());
}

/// A headless VT emulator: feed it the raw PTY output bytes (the same stream the sidecar
/// pumps to the renderer) and it maintains the current screen so [`Self::snapshot`] can
/// capture a faithful, serializable image for restore.
///
/// Bounded + pragmatic (see the module docs): recognized control sequences move/erase/style
/// the grid and switch main ↔ alt screen; unrecognized ones are skipped without corrupting
/// the grid.
#[derive(Debug, Clone)]
pub struct VtScreen {
    rows: usize,
    cols: usize,
    main: ScreenBuffer,
    alt: ScreenBuffer,
    /// `true` while the alternate screen is active (a full-screen TUI is painting).
    alt_active: bool,
    pen: Pen,
    /// Saved cursor (DECSC/DECRC) for the ACTIVE screen kind. Index 0 = main, 1 = alt.
    saved_cursor: [(usize, usize); 2],
    saved_pen: [Pen; 2],
    cursor_visible: bool,
    /// DEFERRED autowrap (the VT "last-column" behaviour): set when a glyph lands in the final
    /// column, so the cursor stays visually on that column but the NEXT printable char wraps to
    /// the next line first. Any explicit cursor motion (CR, CUP, backspace, …) clears it. This
    /// is what makes a full-width line followed by more text wrap correctly instead of
    /// overwriting the last cell.
    pending_wrap: bool,
    /// Parser state machine + its accumulated bytes.
    state: ParseState,
    /// Bytes accumulated for the current CSI/OSC/ESC sequence.
    seq: Vec<u8>,
    /// Partial UTF-8 bytes carried across a chunk boundary.
    utf8: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ParseState {
    /// Normal ground state: printable chars go to the grid.
    Ground,
    /// Saw ESC, awaiting the next byte to classify the sequence.
    Escape,
    /// Inside a CSI (`ESC [ … final`).
    Csi,
    /// Inside an OSC (`ESC ] … ST/BEL`) — consumed, not applied to the grid.
    Osc,
    /// A charset-designation escape (`ESC ( B`, `ESC ) 0`, …): swallow exactly ONE following
    /// byte (the charset id) then return to ground, so the id never prints as text.
    Charset,
}

impl VtScreen {
    /// A fresh emulator sized `rows × cols` (both at least 1). Starts on the main screen with
    /// a visible cursor at the home position and a default pen.
    pub fn new(rows: u16, cols: u16) -> Self {
        let rows = (rows as usize).max(1);
        let cols = (cols as usize).max(1);
        VtScreen {
            rows,
            cols,
            main: ScreenBuffer::new(rows, cols),
            alt: ScreenBuffer::new(rows, cols),
            alt_active: false,
            pen: Pen::default(),
            saved_cursor: [(0, 0); 2],
            saved_pen: [Pen::default(); 2],
            cursor_visible: true,
            pending_wrap: false,
            state: ParseState::Ground,
            seq: Vec::new(),
            utf8: Vec::new(),
        }
    }

    /// Current grid dimensions (`rows`, `cols`).
    pub fn size(&self) -> (usize, usize) {
        (self.rows, self.cols)
    }

    /// Whether the alternate screen is currently active (a full-screen TUI is on screen).
    pub fn alt_active(&self) -> bool {
        self.alt_active
    }

    /// The currently active screen buffer (mutable).
    fn active_mut(&mut self) -> &mut ScreenBuffer {
        if self.alt_active {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    /// The currently active screen buffer.
    fn active(&self) -> &ScreenBuffer {
        if self.alt_active {
            &self.alt
        } else {
            &self.main
        }
    }

    /// Index into `saved_cursor`/`saved_pen` for the active screen (0 = main, 1 = alt).
    fn save_slot(&self) -> usize {
        usize::from(self.alt_active)
    }

    /// Feed a chunk of raw PTY output bytes, updating the current screen. Safe to call with
    /// arbitrary chunk boundaries: partial UTF-8 and partial escape sequences are carried to
    /// the next call.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.feed_byte(b);
        }
    }

    fn feed_byte(&mut self, b: u8) {
        match self.state {
            ParseState::Ground => self.ground_byte(b),
            ParseState::Escape => self.escape_byte(b),
            ParseState::Csi => self.csi_byte(b),
            ParseState::Osc => self.osc_byte(b),
            // Swallow exactly one charset-id byte, then resume ground parsing.
            ParseState::Charset => self.state = ParseState::Ground,
        }
    }

    fn ground_byte(&mut self, b: u8) {
        match b {
            0x1b => {
                // ESC — flush any partial UTF-8, begin an escape sequence.
                self.utf8.clear();
                self.state = ParseState::Escape;
                self.seq.clear();
            }
            0x07 => { /* BEL — no visual effect on the grid */ }
            0x08 => {
                // Backspace: move cursor left one column (no erase). Clears a pending wrap.
                self.utf8.clear();
                self.pending_wrap = false;
                let buf = self.active_mut();
                if buf.cursor_col > 0 {
                    buf.cursor_col -= 1;
                }
            }
            0x09 => {
                // Tab: advance to the next multiple of 8, clamped to the last column.
                self.utf8.clear();
                self.pending_wrap = false;
                let cols = self.cols;
                let buf = self.active_mut();
                let next = ((buf.cursor_col / 8) + 1) * 8;
                buf.cursor_col = next.min(cols - 1);
            }
            b'\n' | 0x0b | 0x0c => {
                // LF / VT / FF: move down one row, scrolling at the bottom. Consumes a pending
                // wrap (the row change already satisfies it).
                self.utf8.clear();
                self.pending_wrap = false;
                self.line_feed();
            }
            b'\r' => {
                // CR: cursor to column 0. Clears a pending wrap.
                self.utf8.clear();
                self.pending_wrap = false;
                self.active_mut().cursor_col = 0;
            }
            _ => self.text_byte(b),
        }
    }

    /// Accumulate a (possibly multi-byte UTF-8) printable char and, once complete, write it.
    fn text_byte(&mut self, b: u8) {
        if b < 0x80 {
            // ASCII printable (control bytes handled above); a stray control byte here is
            // ignored rather than printed.
            self.utf8.clear();
            if b >= 0x20 {
                self.put_char(b as char);
            }
            return;
        }
        // Multi-byte UTF-8: accumulate until we have a full scalar.
        self.utf8.push(b);
        match std::str::from_utf8(&self.utf8) {
            Ok(s) => {
                if let Some(ch) = s.chars().next() {
                    self.put_char(ch);
                }
                self.utf8.clear();
            }
            Err(e) if e.error_len().is_none() && self.utf8.len() < 4 => {
                // Incomplete but still valid prefix: wait for more bytes.
            }
            Err(_) => {
                // Invalid sequence: drop it (never corrupt the grid with replacement noise).
                self.utf8.clear();
            }
        }
    }

    /// Write one character at the cursor with the current pen, advancing the cursor and
    /// wrapping / scrolling at the edges. Wide (CJK/emoji) chars take two columns.
    fn put_char(&mut self, ch: char) {
        let w = char_width(ch);
        // Zero-width (combining) chars: skip to keep the model simple (exact combining-mark
        // shaping is the renderer's job; out of scope for a restore snapshot).
        if w == 0 {
            return;
        }
        let cols = self.cols;
        // A wide glyph that cannot fit even on a fresh line (1-col terminal): skip it rather
        // than corrupt the grid.
        if w > cols {
            return;
        }
        // DEFERRED autowrap: a prior glyph filled the last column and left the cursor parked
        // there. The first printable char after that wraps to a fresh line before landing.
        if self.pending_wrap {
            self.pending_wrap = false;
            self.active_mut().cursor_col = 0;
            self.line_feed();
        }
        // A wide glyph that would straddle the right edge also wraps first.
        if self.active().cursor_col + w > cols {
            self.active_mut().cursor_col = 0;
            self.line_feed();
        }
        let pen = self.pen;
        let (row, col) = {
            let buf = self.active();
            (buf.cursor_row, buf.cursor_col)
        };
        let buf = self.active_mut();
        let base = buf.idx(row, col);
        buf.cells[base] = Cell {
            ch,
            fg: pen.fg,
            bg: pen.bg,
            attrs: pen.attrs,
            width: w as u8,
        };
        if w == 2 && col + 1 < cols {
            // Right-hand spacer of a wide glyph.
            let spacer = buf.idx(row, col + 1);
            buf.cells[spacer] = Cell {
                ch: ' ',
                fg: pen.fg,
                bg: pen.bg,
                attrs: pen.attrs,
                width: 0,
            };
        }
        // Advance the cursor. If the glyph reached the right edge, park at the last column and
        // arm the deferred wrap (the next printable char will wrap). Otherwise move right.
        let new_col = col + w;
        if new_col >= cols {
            self.active_mut().cursor_col = cols - 1;
            self.pending_wrap = true;
        } else {
            self.active_mut().cursor_col = new_col;
        }
    }

    /// Line feed: move the cursor down one row, scrolling the buffer up when already at the
    /// bottom row (content that scrolls off the top is dropped — the sidecar's separate
    /// scrollback keeps history; this model is the on-screen grid only).
    fn line_feed(&mut self) {
        let rows = self.rows;
        let cols = self.cols;
        let buf = self.active_mut();
        if buf.cursor_row + 1 < rows {
            buf.cursor_row += 1;
        } else {
            // Scroll up by one line: drop the top row, blank the new bottom row.
            buf.cells.drain(0..cols);
            buf.cells.extend(std::iter::repeat_n(Cell::default(), cols));
        }
    }

    // --- Escape / CSI / OSC parsing ---------------------------------------

    fn escape_byte(&mut self, b: u8) {
        match b {
            b'[' => {
                self.state = ParseState::Csi;
                self.seq.clear();
            }
            b']' => {
                self.state = ParseState::Osc;
                self.seq.clear();
            }
            b'7' => {
                // DECSC — save cursor + pen.
                let slot = self.save_slot();
                let buf = self.active();
                self.saved_cursor[slot] = (buf.cursor_row, buf.cursor_col);
                self.saved_pen[slot] = self.pen;
                self.state = ParseState::Ground;
            }
            b'8' => {
                // DECRC — restore cursor + pen.
                self.pending_wrap = false;
                let slot = self.save_slot();
                let (r, c) = self.saved_cursor[slot];
                self.pen = self.saved_pen[slot];
                let buf = self.active_mut();
                buf.cursor_row = r.min(buf.rows - 1);
                buf.cursor_col = c.min(buf.cols - 1);
                self.state = ParseState::Ground;
            }
            b'M' => {
                // Reverse index: move up one row, scrolling down at the top.
                self.pending_wrap = false;
                self.reverse_index();
                self.state = ParseState::Ground;
            }
            b'c' => {
                // RIS — full reset.
                self.hard_reset();
                self.state = ParseState::Ground;
            }
            // Charset-designation escapes (`ESC ( B`, `ESC ) 0`, …): the next byte is the
            // charset id — swallow it via the one-shot `Charset` state so it never prints.
            b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' => {
                self.state = ParseState::Charset;
            }
            _ => {
                // Unknown escape: back to ground without applying anything.
                self.state = ParseState::Ground;
            }
        }
    }

    fn csi_byte(&mut self, b: u8) {
        // A CSI body is params/intermediates (0x20..=0x3f) terminated by a final (0x40..=0x7e).
        if (0x40..=0x7e).contains(&b) {
            self.apply_csi(b);
            self.state = ParseState::Ground;
            self.seq.clear();
        } else if (0x20..=0x3f).contains(&b) {
            // Bounded so a runaway sequence can't grow unbounded.
            if self.seq.len() < 128 {
                self.seq.push(b);
            }
        } else {
            // A control byte inside a CSI: abort the sequence (do not corrupt the grid).
            self.state = ParseState::Ground;
            self.seq.clear();
        }
    }

    fn osc_byte(&mut self, b: u8) {
        // OSC ends at BEL or ST (ESC \). We only need to CONSUME it (titles / hyperlinks /
        // OSC 133 don't affect the grid image). Track ESC to catch the ST terminator.
        if b == 0x07 {
            self.state = ParseState::Ground;
            self.seq.clear();
        } else if b == 0x1b {
            // Possible ST: mark so the next byte (`\`) terminates.
            self.seq.push(b);
        } else if b == b'\\' && self.seq.last() == Some(&0x1b) {
            self.state = ParseState::Ground;
            self.seq.clear();
        } else if self.seq.len() < 4096 {
            self.seq.push(b);
        }
        // else: overrun — keep consuming until a terminator, without growing the buffer.
    }

    /// Apply a completed CSI whose final byte is `final_byte` and whose param/intermediate
    /// bytes are in `self.seq`.
    fn apply_csi(&mut self, final_byte: u8) {
        // Any CSI that repositions/edits the cursor invalidates a deferred autowrap. SGR (`m`)
        // and the private mode toggles do not move the cursor, but clearing here unconditionally
        // is harmless (the flag is only consulted by the next printable char, which re-arms it).
        self.pending_wrap = false;
        let is_private = self.seq.first() == Some(&b'?');
        let param_bytes = if is_private {
            &self.seq[1..]
        } else {
            &self.seq[..]
        };
        let params = parse_params(param_bytes);
        let p1 = params.first().copied().flatten();

        match final_byte {
            b'A' => self.move_cursor(-(p1.unwrap_or(1).max(1) as isize), 0), // CUU
            b'B' | b'e' => self.move_cursor(p1.unwrap_or(1).max(1) as isize, 0), // CUD
            b'C' | b'a' => self.move_cursor(0, p1.unwrap_or(1).max(1) as isize), // CUF
            b'D' => self.move_cursor(0, -(p1.unwrap_or(1).max(1) as isize)), // CUB
            b'E' => {
                // Cursor next line: down n, col 0.
                self.move_cursor(p1.unwrap_or(1).max(1) as isize, 0);
                self.active_mut().cursor_col = 0;
            }
            b'F' => {
                // Cursor previous line: up n, col 0.
                self.move_cursor(-(p1.unwrap_or(1).max(1) as isize), 0);
                self.active_mut().cursor_col = 0;
            }
            b'G' | b'`' => {
                // CHA — cursor to column n (1-based).
                let col = p1.unwrap_or(1).max(1) - 1;
                let cols = self.cols;
                self.active_mut().cursor_col = (col as usize).min(cols - 1);
            }
            b'd' => {
                // VPA — cursor to row n (1-based).
                let row = p1.unwrap_or(1).max(1) - 1;
                let rows = self.rows;
                self.active_mut().cursor_row = (row as usize).min(rows - 1);
            }
            b'H' | b'f' => {
                // CUP — cursor to (row;col), 1-based, defaults 1.
                let row = params.first().copied().flatten().unwrap_or(1).max(1) - 1;
                let col = params.get(1).copied().flatten().unwrap_or(1).max(1) - 1;
                let (rows, cols) = (self.rows, self.cols);
                let buf = self.active_mut();
                buf.cursor_row = (row as usize).min(rows - 1);
                buf.cursor_col = (col as usize).min(cols - 1);
            }
            b'J' => self.erase_display(p1.unwrap_or(0)),
            b'K' => self.erase_line(p1.unwrap_or(0)),
            b'L' => self.insert_lines(p1.unwrap_or(1).max(1) as usize),
            b'M' => self.delete_lines(p1.unwrap_or(1).max(1) as usize),
            b'P' => self.delete_chars(p1.unwrap_or(1).max(1) as usize),
            b'@' => self.insert_blanks(p1.unwrap_or(1).max(1) as usize),
            b'X' => self.erase_chars(p1.unwrap_or(1).max(1) as usize),
            b'm' => self.apply_sgr(&params),
            b'h' if is_private => self.set_private_modes(&params, true),
            b'l' if is_private => self.set_private_modes(&params, false),
            b's' => {
                // Save cursor (ANSI.SYS variant).
                let slot = self.save_slot();
                let buf = self.active();
                self.saved_cursor[slot] = (buf.cursor_row, buf.cursor_col);
                self.saved_pen[slot] = self.pen;
            }
            b'u' => {
                // Restore cursor (ANSI.SYS variant).
                let slot = self.save_slot();
                let (r, c) = self.saved_cursor[slot];
                self.pen = self.saved_pen[slot];
                let buf = self.active_mut();
                buf.cursor_row = r.min(buf.rows - 1);
                buf.cursor_col = c.min(buf.cols - 1);
            }
            // Everything else (DSR, DECSTBM scroll region, cursor style, …): skip. These do
            // not change the visible grid image enough to matter for a restore snapshot.
            _ => {}
        }
    }

    fn move_cursor(&mut self, drow: isize, dcol: isize) {
        let (rows, cols) = (self.rows, self.cols);
        let buf = self.active_mut();
        let r = (buf.cursor_row as isize + drow).clamp(0, rows as isize - 1);
        let c = (buf.cursor_col as isize + dcol).clamp(0, cols as isize - 1);
        buf.cursor_row = r as usize;
        buf.cursor_col = c as usize;
    }

    fn reverse_index(&mut self) {
        let cols = self.cols;
        let buf = self.active_mut();
        if buf.cursor_row > 0 {
            buf.cursor_row -= 1;
        } else {
            // Scroll down: insert a blank line at the top, drop the bottom.
            let total = buf.cells.len();
            buf.cells.truncate(total - cols);
            let mut new_top = vec![Cell::default(); cols];
            new_top.append(&mut buf.cells);
            buf.cells = new_top;
        }
    }

    /// ED — erase in display. 0: cursor→end, 1: start→cursor, 2/3: whole screen.
    fn erase_display(&mut self, mode: u32) {
        let (rows, cols) = (self.rows, self.cols);
        let buf = self.active_mut();
        let cursor = buf.idx(buf.cursor_row, buf.cursor_col);
        match mode {
            0 => {
                for c in &mut buf.cells[cursor..] {
                    *c = Cell::default();
                }
            }
            1 => {
                for c in &mut buf.cells[..=cursor.min(rows * cols - 1)] {
                    *c = Cell::default();
                }
            }
            _ => buf.clear_all(),
        }
    }

    /// EL — erase in line. 0: cursor→eol, 1: bol→cursor, 2: whole line.
    fn erase_line(&mut self, mode: u32) {
        let cols = self.cols;
        let buf = self.active_mut();
        let row = buf.cursor_row;
        let col = buf.cursor_col;
        let start = row * cols;
        match mode {
            0 => {
                for c in &mut buf.cells[start + col..start + cols] {
                    *c = Cell::default();
                }
            }
            1 => {
                for c in &mut buf.cells[start..=start + col] {
                    *c = Cell::default();
                }
            }
            _ => {
                for c in &mut buf.cells[start..start + cols] {
                    *c = Cell::default();
                }
            }
        }
    }

    fn insert_lines(&mut self, n: usize) {
        let (rows, cols) = (self.rows, self.cols);
        let buf = self.active_mut();
        let row = buf.cursor_row;
        let n = n.min(rows - row);
        // Remove n lines from the bottom, insert n blank lines at the cursor row.
        let bottom_start = (rows - n) * cols;
        buf.cells.truncate(bottom_start);
        let insert_at = row * cols;
        let blanks = vec![Cell::default(); n * cols];
        buf.cells.splice(insert_at..insert_at, blanks);
    }

    fn delete_lines(&mut self, n: usize) {
        let (rows, cols) = (self.rows, self.cols);
        let buf = self.active_mut();
        let row = buf.cursor_row;
        let n = n.min(rows - row);
        let del_start = row * cols;
        let del_end = del_start + n * cols;
        buf.cells.drain(del_start..del_end);
        buf.cells
            .extend(std::iter::repeat_n(Cell::default(), n * cols));
    }

    fn delete_chars(&mut self, n: usize) {
        let cols = self.cols;
        let buf = self.active_mut();
        let row = buf.cursor_row;
        let col = buf.cursor_col;
        let start = row * cols;
        let line_end = start + cols;
        let del_from = start + col;
        let n = n.min(cols - col);
        buf.cells.drain(del_from..del_from + n);
        buf.cells
            .splice(line_end - n..line_end - n, vec![Cell::default(); n]);
    }

    fn insert_blanks(&mut self, n: usize) {
        let cols = self.cols;
        let buf = self.active_mut();
        let row = buf.cursor_row;
        let col = buf.cursor_col;
        let start = row * cols;
        let line_end = start + cols;
        let n = n.min(cols - col);
        let insert_at = start + col;
        buf.cells
            .splice(insert_at..insert_at, vec![Cell::default(); n]);
        // Truncate the line back to `cols` (drop the overflow past the right edge).
        buf.cells.drain(line_end..line_end + n);
    }

    fn erase_chars(&mut self, n: usize) {
        let cols = self.cols;
        let buf = self.active_mut();
        let row = buf.cursor_row;
        let col = buf.cursor_col;
        let start = row * cols + col;
        let n = n.min(cols - col);
        for c in &mut buf.cells[start..start + n] {
            *c = Cell::default();
        }
    }

    /// SGR — set graphic rendition (colors + attributes) on the pen.
    fn apply_sgr(&mut self, params: &[Option<u32>]) {
        if params.is_empty() {
            self.pen = Pen::default();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let code = params[i].unwrap_or(0);
            match code {
                0 => self.pen = Pen::default(),
                1 => self.pen.attrs.set(Attrs::BOLD, true),
                2 => self.pen.attrs.set(Attrs::DIM, true),
                3 => self.pen.attrs.set(Attrs::ITALIC, true),
                4 => self.pen.attrs.set(Attrs::UNDERLINE, true),
                5 | 6 => self.pen.attrs.set(Attrs::BLINK, true),
                7 => self.pen.attrs.set(Attrs::REVERSE, true),
                8 => self.pen.attrs.set(Attrs::HIDDEN, true),
                9 => self.pen.attrs.set(Attrs::STRIKE, true),
                22 => {
                    self.pen.attrs.set(Attrs::BOLD, false);
                    self.pen.attrs.set(Attrs::DIM, false);
                }
                23 => self.pen.attrs.set(Attrs::ITALIC, false),
                24 => self.pen.attrs.set(Attrs::UNDERLINE, false),
                25 => self.pen.attrs.set(Attrs::BLINK, false),
                27 => self.pen.attrs.set(Attrs::REVERSE, false),
                28 => self.pen.attrs.set(Attrs::HIDDEN, false),
                29 => self.pen.attrs.set(Attrs::STRIKE, false),
                30..=37 => {
                    self.pen.fg = Color::Indexed {
                        index: (code - 30) as u8,
                    }
                }
                39 => self.pen.fg = Color::Default,
                40..=47 => {
                    self.pen.bg = Color::Indexed {
                        index: (code - 40) as u8,
                    }
                }
                49 => self.pen.bg = Color::Default,
                90..=97 => {
                    self.pen.fg = Color::Indexed {
                        index: (code - 90 + 8) as u8,
                    }
                }
                100..=107 => {
                    self.pen.bg = Color::Indexed {
                        index: (code - 100 + 8) as u8,
                    }
                }
                38 | 48 => {
                    // Extended color: 38;5;n (indexed) or 38;2;r;g;b (truecolor).
                    let is_fg = code == 38;
                    let (color, consumed) = parse_extended_color(&params[i + 1..]);
                    if let Some(color) = color {
                        if is_fg {
                            self.pen.fg = color;
                        } else {
                            self.pen.bg = color;
                        }
                    }
                    i += consumed;
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// DECSET/DECRST private modes we care about: alt-screen (47/1047/1049) and cursor
    /// visibility (25). Others are ignored.
    fn set_private_modes(&mut self, params: &[Option<u32>], set: bool) {
        for p in params {
            match p {
                Some(25) => self.cursor_visible = set,
                Some(47) | Some(1047) => self.switch_alt(set, false),
                Some(1049) => self.switch_alt(set, true),
                _ => {}
            }
        }
    }

    /// Enter (`enter == true`) or leave the alternate screen. `save_restore` is the 1049
    /// variant: entering saves the cursor and CLEARS the alt buffer; leaving restores the
    /// saved cursor. Idempotent (entering while already in alt, or leaving while on main, is a
    /// no-op) so a snapshot is never corrupted by a doubled sequence.
    fn switch_alt(&mut self, enter: bool, save_restore: bool) {
        if enter == self.alt_active {
            return;
        }
        self.pending_wrap = false;
        if enter {
            if save_restore {
                // Save the MAIN cursor/pen so leaving restores exactly.
                self.saved_cursor[0] = (self.main.cursor_row, self.main.cursor_col);
                self.saved_pen[0] = self.pen;
            }
            // Clear the alternate buffer on entry (TUIs assume a blank alt-screen).
            self.alt.clear_all();
            self.alt.cursor_row = 0;
            self.alt.cursor_col = 0;
            self.alt_active = true;
        } else {
            self.alt_active = false;
            if save_restore {
                let (r, c) = self.saved_cursor[0];
                self.pen = self.saved_pen[0];
                self.main.cursor_row = r.min(self.main.rows - 1);
                self.main.cursor_col = c.min(self.main.cols - 1);
            }
        }
    }

    fn hard_reset(&mut self) {
        self.main = ScreenBuffer::new(self.rows, self.cols);
        self.alt = ScreenBuffer::new(self.rows, self.cols);
        self.alt_active = false;
        self.pen = Pen::default();
        self.saved_cursor = [(0, 0); 2];
        self.saved_pen = [Pen::default(); 2];
        self.cursor_visible = true;
        self.pending_wrap = false;
    }

    /// Resize the grid to `rows × cols` (both clamped to ≥1). Content is preserved where it
    /// overlaps the new dimensions (top-left anchored); new cells are blank. The cursor is
    /// clamped into the new bounds. A no-op if the size is unchanged.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = (rows as usize).max(1);
        let cols = (cols as usize).max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.pending_wrap = false;
        resize_buffer(&mut self.main, rows, cols);
        resize_buffer(&mut self.alt, rows, cols);
        self.rows = rows;
        self.cols = cols;
    }

    /// Capture the current screen as a serializable [`ScreenSnapshot`]. This is the payload
    /// the sidecar persists / hands the renderer for lossless restore. Cheap-ish (clones the
    /// active buffers); call it debounced, not per byte.
    pub fn snapshot(&self) -> ScreenSnapshot {
        ScreenSnapshot {
            version: SNAPSHOT_VERSION,
            main: self.main.clone(),
            alt: if self.alt_active {
                Some(self.alt.clone())
            } else {
                None
            },
            alt_active: self.alt_active,
            cursor_visible: self.cursor_visible,
        }
    }
}

/// Parse the numeric parameters of a CSI body (`;`-separated decimal, an empty field is
/// `None` so the caller can apply its default). Bytes outside `0-9;` end parsing.
fn parse_params(bytes: &[u8]) -> Vec<Option<u32>> {
    let mut out = Vec::new();
    let mut cur: Option<u32> = None;
    let mut seen_digit = false;
    for &b in bytes {
        match b {
            b'0'..=b'9' => {
                seen_digit = true;
                let d = (b - b'0') as u32;
                cur = Some(cur.unwrap_or(0).saturating_mul(10).saturating_add(d));
            }
            b';' => {
                out.push(if seen_digit { cur } else { None });
                cur = None;
                seen_digit = false;
            }
            // Sub-parameter separator (`:`) or intermediates: treat like a field break for
            // our purposes (we don't need sub-params for the modeled sequences).
            b':' => {
                out.push(if seen_digit { cur } else { None });
                cur = None;
                seen_digit = false;
            }
            _ => break,
        }
    }
    out.push(if seen_digit { cur } else { None });
    out
}

/// Parse an SGR extended-color tail beginning right AFTER the `38`/`48` code. Returns the
/// color (if well-formed) and how many extra params it consumed. Supports `5;n` (indexed) and
/// `2;r;g;b` (truecolor).
fn parse_extended_color(tail: &[Option<u32>]) -> (Option<Color>, usize) {
    match tail.first().copied().flatten() {
        Some(5) => {
            let idx = tail.get(1).copied().flatten();
            (idx.map(|i| Color::Indexed { index: i as u8 }), 2)
        }
        Some(2) => {
            let r = tail.get(1).copied().flatten().unwrap_or(0) as u8;
            let g = tail.get(2).copied().flatten().unwrap_or(0) as u8;
            let b = tail.get(3).copied().flatten().unwrap_or(0) as u8;
            (Some(Color::Rgb { r, g, b }), 4)
        }
        _ => (None, 0),
    }
}

/// Resize one buffer, preserving overlapping content (top-left anchored) and clamping the
/// cursor into the new bounds.
fn resize_buffer(buf: &mut ScreenBuffer, rows: usize, cols: usize) {
    let mut cells = vec![Cell::default(); rows * cols];
    for r in 0..rows.min(buf.rows) {
        for c in 0..cols.min(buf.cols) {
            cells[r * cols + c] = buf.cells[r * buf.cols + c].clone();
        }
    }
    buf.cells = cells;
    buf.rows = rows;
    buf.cols = cols;
    buf.cursor_row = buf.cursor_row.min(rows - 1);
    buf.cursor_col = buf.cursor_col.min(cols - 1);
}

/// Display width of a character: 0 (zero-width / combining), 2 (wide East-Asian / most
/// emoji), or 1 (everything else). A pragmatic, dependency-free classification — enough for a
/// restore snapshot's column accounting; the renderer owns exact shaping.
fn char_width(ch: char) -> usize {
    let c = ch as u32;
    // Combining marks / zero-width.
    if c == 0 {
        return 1;
    }
    if (0x0300..=0x036F).contains(&c) // combining diacritical marks
        || (0x200B..=0x200F).contains(&c) // zero-width space/joiners/marks
        || c == 0xFEFF
    {
        return 0;
    }
    if is_wide(c) {
        2
    } else {
        1
    }
}

/// Whether a codepoint is East-Asian Wide / Fullwidth (or a common wide emoji block). A
/// compact range check covering the ranges that matter for CJK + emoji restore fidelity.
fn is_wide(c: u32) -> bool {
    matches!(c,
        0x1100..=0x115F        // Hangul Jamo
        | 0x2329 | 0x232A      // angle brackets
        | 0x2E80..=0x303E      // CJK radicals, Kangxi, CJK symbols
        | 0x3041..=0x33FF      // Hiragana, Katakana, CJK symbols, enclosed
        | 0x3400..=0x4DBF      // CJK Ext A
        | 0x4E00..=0x9FFF      // CJK Unified Ideographs
        | 0xA000..=0xA4CF      // Yi
        | 0xAC00..=0xD7A3      // Hangul Syllables
        | 0xF900..=0xFAFF      // CJK Compatibility Ideographs
        | 0xFE30..=0xFE4F      // CJK Compatibility Forms
        | 0xFF00..=0xFF60      // Fullwidth Forms
        | 0xFFE0..=0xFFE6      // Fullwidth signs
        | 0x1F300..=0x1FAFF    // emoji & pictographs
        | 0x20000..=0x3FFFD    // CJK Ext B+ (supplementary ideographic plane)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a `&str` (the common case in tests: raw ANSI as text bytes).
    fn feed(vt: &mut VtScreen, s: &str) {
        vt.feed(s.as_bytes());
    }

    /// The whole active screen as newline-joined trimmed rows (blank trailing rows dropped),
    /// for readable text-fidelity assertions.
    fn screen_text(vt: &VtScreen) -> String {
        let buf = vt.active();
        let rows: Vec<String> = (0..buf.rows).map(|r| buf.row_text(r)).collect();
        let last = rows
            .iter()
            .rposition(|r| !r.is_empty())
            .map(|i| i + 1)
            .unwrap_or(0);
        rows[..last].join("\n")
    }

    #[test]
    fn plain_text_lands_on_the_grid() {
        let mut vt = VtScreen::new(5, 20);
        feed(&mut vt, "hello");
        assert_eq!(vt.active().row_text(0), "hello");
        assert_eq!(vt.active().cursor_col, 5);
        assert_eq!(vt.active().cursor_row, 0);
    }

    #[test]
    fn cr_lf_move_the_cursor_and_wrap_rows() {
        let mut vt = VtScreen::new(5, 20);
        feed(&mut vt, "line1\r\nline2");
        assert_eq!(vt.active().row_text(0), "line1");
        assert_eq!(vt.active().row_text(1), "line2");
        assert_eq!(vt.active().cursor_row, 1);
    }

    #[test]
    fn autowrap_past_the_right_edge() {
        let mut vt = VtScreen::new(3, 4);
        feed(&mut vt, "abcdef"); // 4-wide grid: "abcd" then wrap "ef"
        assert_eq!(vt.active().row_text(0), "abcd");
        assert_eq!(vt.active().row_text(1), "ef");
    }

    #[test]
    fn lf_at_bottom_scrolls_up() {
        let mut vt = VtScreen::new(2, 8);
        feed(&mut vt, "top\r\nmid\r\nbot"); // 3 logical lines into a 2-row grid
                                            // "top" scrolled off; the grid holds the last two lines.
        assert_eq!(vt.active().row_text(0), "mid");
        assert_eq!(vt.active().row_text(1), "bot");
    }

    #[test]
    fn cup_positions_the_cursor_1_based() {
        let mut vt = VtScreen::new(5, 10);
        feed(&mut vt, "\x1b[3;5HX"); // row 3, col 5 (1-based) -> (2,4)
        assert_eq!(vt.active().cells[vt.active().idx(2, 4)].ch, 'X');
    }

    #[test]
    fn cursor_movement_csi() {
        let mut vt = VtScreen::new(5, 10);
        feed(&mut vt, "abc");
        feed(&mut vt, "\x1b[2D"); // back 2 -> col 1
        assert_eq!(vt.active().cursor_col, 1);
        feed(&mut vt, "\x1b[2B"); // down 2 -> row 2
        assert_eq!(vt.active().cursor_row, 2);
    }

    #[test]
    fn erase_display_and_line() {
        let mut vt = VtScreen::new(3, 6);
        feed(&mut vt, "aaaaaa\r\nbbbbbb\r\ncccccc");
        // Home, erase whole screen (ED 2).
        feed(&mut vt, "\x1b[H\x1b[2J");
        assert_eq!(screen_text(&vt), "");
        // Rebuild, then erase from cursor to end-of-line (EL 0) mid-row.
        feed(&mut vt, "\x1b[Hxyzabc\x1b[1;4H\x1b[K");
        assert_eq!(vt.active().row_text(0), "xyz");
    }

    #[test]
    fn sgr_colors_and_attributes_apply_to_the_pen() {
        let mut vt = VtScreen::new(2, 20);
        feed(&mut vt, "\x1b[1;31mR\x1b[0mN");
        let r = &vt.active().cells[0];
        assert_eq!(r.ch, 'R');
        assert_eq!(r.fg, Color::Indexed { index: 1 });
        assert!(r.attrs.contains(Attrs::BOLD));
        let n = &vt.active().cells[1];
        assert_eq!(n.ch, 'N');
        assert_eq!(n.fg, Color::Default, "SGR 0 reset the pen");
        assert!(!n.attrs.contains(Attrs::BOLD));
    }

    #[test]
    fn sgr_truecolor_and_256_indexed() {
        let mut vt = VtScreen::new(1, 20);
        feed(&mut vt, "\x1b[38;2;10;20;30mT");
        assert_eq!(
            vt.active().cells[0].fg,
            Color::Rgb {
                r: 10,
                g: 20,
                b: 30
            }
        );
        let mut vt2 = VtScreen::new(1, 20);
        feed(&mut vt2, "\x1b[48;5;200mB");
        assert_eq!(vt2.active().cells[0].bg, Color::Indexed { index: 200 });
    }

    #[test]
    fn osc_titles_are_consumed_not_printed() {
        let mut vt = VtScreen::new(2, 20);
        feed(&mut vt, "\x1b]0;my title\x07visible");
        assert_eq!(vt.active().row_text(0), "visible");
    }

    #[test]
    fn charset_designation_is_swallowed() {
        let mut vt = VtScreen::new(1, 10);
        feed(&mut vt, "\x1b(Bok"); // ESC ( B  then "ok"
        assert_eq!(vt.active().row_text(0), "ok");
    }

    #[test]
    fn wide_cjk_char_takes_two_columns() {
        let mut vt = VtScreen::new(1, 10);
        feed(&mut vt, "a漢b"); // 漢 is wide
        let cells = &vt.active().cells;
        assert_eq!(cells[0].ch, 'a');
        assert_eq!(cells[0].width, 1);
        assert_eq!(cells[1].ch, '漢');
        assert_eq!(cells[1].width, 2);
        assert_eq!(cells[2].width, 0, "spacer after a wide glyph");
        assert_eq!(cells[3].ch, 'b');
        assert_eq!(vt.active().cursor_col, 4);
    }

    #[test]
    fn utf8_split_across_chunks_is_reassembled() {
        let mut vt = VtScreen::new(1, 10);
        let bytes = "é".as_bytes(); // 2 bytes
        vt.feed(&bytes[..1]);
        vt.feed(&bytes[1..]);
        assert_eq!(vt.active().row_text(0), "é");
    }

    // --- Alt-screen: the crux of the done-criteria (a TUI on the alt-screen) ------

    #[test]
    fn alt_screen_1049_switch_isolates_and_restores() {
        let mut vt = VtScreen::new(4, 20);
        // Main screen content.
        feed(&mut vt, "main line one\r\nmain line two");
        assert!(!vt.alt_active());

        // Enter the alt-screen (DECSET 1049) and paint a "TUI".
        feed(&mut vt, "\x1b[?1049h");
        assert!(vt.alt_active(), "entered the alternate screen");
        assert_eq!(screen_text(&vt), "", "alt-screen starts blank");
        feed(&mut vt, "\x1b[H\x1b[2JTUI HEADER\r\nrow A\r\nrow B");
        assert_eq!(vt.active().row_text(0), "TUI HEADER");

        // The MAIN buffer is untouched underneath.
        assert_eq!(vt.main.row_text(0), "main line one");

        // Leave the alt-screen: the main content is back, exactly.
        feed(&mut vt, "\x1b[?1049l");
        assert!(!vt.alt_active());
        assert_eq!(vt.active().row_text(0), "main line one");
        assert_eq!(vt.active().row_text(1), "main line two");
    }

    #[test]
    fn cursor_visibility_dectcem() {
        let mut vt = VtScreen::new(2, 10);
        assert!(vt.snapshot().cursor_visible);
        feed(&mut vt, "\x1b[?25l");
        assert!(!vt.snapshot().cursor_visible, "cursor hidden");
        feed(&mut vt, "\x1b[?25h");
        assert!(vt.snapshot().cursor_visible, "cursor shown again");
    }

    // --- Snapshot: serializable, alt-screen included, round-trips ----------------

    #[test]
    fn snapshot_includes_alt_screen_only_when_active() {
        let mut vt = VtScreen::new(3, 20);
        feed(&mut vt, "history");
        let snap_main = vt.snapshot();
        assert!(!snap_main.alt_active);
        assert!(
            snap_main.alt.is_none(),
            "no alt buffer while on the main screen"
        );
        assert_eq!(snap_main.main.row_text(0), "history");

        feed(&mut vt, "\x1b[?1049h\x1b[HTUI");
        let snap_alt = vt.snapshot();
        assert!(snap_alt.alt_active);
        let alt = snap_alt
            .alt
            .expect("alt buffer present while in alt-screen");
        assert_eq!(alt.row_text(0), "TUI");
        // The main buffer is still carried in the snapshot (history preserved underneath).
        assert_eq!(snap_alt.main.row_text(0), "history");
    }

    #[test]
    fn snapshot_round_trips_through_serde_losslessly() {
        let mut vt = VtScreen::new(4, 24);
        feed(
            &mut vt,
            "\x1b[?1049h\x1b[H\x1b[1;32mgreen bold\x1b[0m\r\nsecond 漢 line",
        );
        let snap = vt.snapshot();

        // Front-lib INDEPENDENT: the snapshot is plain serde data. A JSON round-trip (a
        // self-describing codec, no schema shared with any front lib) proves it is lossless and
        // that no wterm/xterm/ghostty type leaks into the shape. The sidecar re-encodes the SAME
        // `Serialize` impl over its MessagePack pipe (rmp-serde), so JSON losslessness here is a
        // faithful proxy for the on-wire codec too (rmp-serde is a sidecar dep, not a core dep).
        let text = serde_json::to_string(&snap).expect("serialize");
        let back: ScreenSnapshot = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, snap, "serde round-trip must be lossless");
        let bytes = serde_json::to_vec(&snap).expect("serialize bytes");
        let back2: ScreenSnapshot = serde_json::from_slice(&bytes).expect("deserialize bytes");
        assert_eq!(back2, snap, "byte round-trip must be lossless");

        // The alt-screen content, colors and attributes all survived the trip.
        let alt = back.alt.expect("alt present");
        assert_eq!(alt.row_text(0), "green bold");
        assert_eq!(alt.cells[0].fg, Color::Indexed { index: 2 });
        assert!(alt.cells[0].attrs.contains(Attrs::BOLD));
    }

    #[test]
    fn full_tui_repaint_is_faithful() {
        // Simulate what an app like htop/claude emits: enter alt-screen, clear, home, then
        // absolute-positioned rows with colors — the model must reproduce the grid.
        let mut vt = VtScreen::new(5, 30);
        feed(&mut vt, "shell prompt $ vim\r\n");
        feed(&mut vt, "\x1b[?1049h"); // vim enters alt-screen
        feed(&mut vt, "\x1b[2J\x1b[H"); // clear + home
        feed(&mut vt, "\x1b[1;1H  1 fn main() {");
        feed(&mut vt, "\x1b[2;1H  2     println!(\"hi\");");
        feed(&mut vt, "\x1b[3;1H  3 }");
        feed(&mut vt, "\x1b[5;1H\x1b[7m-- INSERT --\x1b[0m"); // reverse-video status line

        let snap = vt.snapshot();
        assert!(snap.alt_active);
        let alt = snap.alt.as_ref().unwrap();
        assert_eq!(alt.row_text(0), "  1 fn main() {");
        assert_eq!(alt.row_text(1), "  2     println!(\"hi\");");
        assert_eq!(alt.row_text(2), "  3 }");
        assert_eq!(alt.row_text(4), "-- INSERT --");
        // The status line is reverse-video.
        assert!(alt.cells[4 * alt.cols].attrs.contains(Attrs::REVERSE));
        // The shell history is preserved on the main screen for when vim exits.
        assert_eq!(snap.main.row_text(0), "shell prompt $ vim");
    }

    #[test]
    fn resize_preserves_overlapping_content() {
        let mut vt = VtScreen::new(3, 10);
        feed(&mut vt, "\x1b[Hrow0\r\nrow1\r\nrow2");
        vt.resize(4, 12);
        assert_eq!(vt.active().row_text(0), "row0");
        assert_eq!(vt.active().row_text(2), "row2");
        assert_eq!(vt.size(), (4, 12));
    }

    #[test]
    fn decsc_decrc_save_restore_cursor() {
        let mut vt = VtScreen::new(4, 10);
        feed(&mut vt, "\x1b[2;3H\x1b7"); // move to (1,2), DECSC save
        feed(&mut vt, "\x1b[Hmoved"); // move home, write
        feed(&mut vt, "\x1b8X"); // DECRC restore -> back to (1,2), write X
        assert_eq!(vt.active().cells[vt.active().idx(1, 2)].ch, 'X');
    }

    #[test]
    fn unknown_sequences_never_corrupt_the_grid() {
        let mut vt = VtScreen::new(2, 20);
        // A private DSR-ish sequence and a device attributes query: consumed, no text leak.
        feed(&mut vt, "\x1b[6n\x1b[>0cok");
        assert_eq!(vt.active().row_text(0), "ok");
    }

    #[test]
    fn insert_and_delete_chars() {
        let mut vt = VtScreen::new(1, 10);
        feed(&mut vt, "abcdef\x1b[1;1H"); // home
        feed(&mut vt, "\x1b[2P"); // delete 2 chars -> "cdef"
        assert_eq!(vt.active().row_text(0), "cdef");
        feed(&mut vt, "\x1b[1;1H\x1b[2@"); // insert 2 blanks at home
        assert_eq!(vt.active().row_text(0), "  cdef");
    }

    // --- Re-hydration byte stream (task #7: restore a TUI, not just history) -------

    /// Feeding a snapshot's re-hydration bytes into a FRESH VtScreen must reproduce the SAME
    /// screen — the model-level proof that the restart protocol is lossless. We assert on the
    /// re-parsed snapshot, so this holds for any VT that honors the emitted sequences (ghostty
    /// in the renderer being the production one).
    fn rehydrate(snap: &ScreenSnapshot, rows: u16, cols: u16) -> ScreenSnapshot {
        let mut fresh = VtScreen::new(rows, cols);
        fresh.feed(&snap.to_rehydration_bytes());
        fresh.snapshot()
    }

    #[test]
    fn rehydration_round_trips_main_screen_text() {
        let mut vt = VtScreen::new(4, 30);
        feed(&mut vt, "\x1b[Hline one\r\nline two\r\nthird");
        let snap = vt.snapshot();
        let back = rehydrate(&snap, 4, 30);
        assert!(!back.alt_active);
        assert_eq!(back.main.row_text(0), "line one");
        assert_eq!(back.main.row_text(1), "line two");
        assert_eq!(back.main.row_text(2), "third");
    }

    #[test]
    fn rehydration_restores_colors_and_attributes() {
        let mut vt = VtScreen::new(2, 20);
        feed(
            &mut vt,
            "\x1b[H\x1b[1;31mRED\x1b[0m \x1b[38;2;1;2;3mrgb\x1b[0m",
        );
        let snap = vt.snapshot();
        let back = rehydrate(&snap, 2, 20);
        assert_eq!(back.main.cells[0].ch, 'R');
        assert_eq!(back.main.cells[0].fg, Color::Indexed { index: 1 });
        assert!(back.main.cells[0].attrs.contains(Attrs::BOLD));
        // The truecolor run survived (find the 'r' of "rgb").
        let r_idx = (0..back.main.cols)
            .find(|&c| back.main.cells[c].ch == 'r')
            .expect("rgb present");
        assert_eq!(back.main.cells[r_idx].fg, Color::Rgb { r: 1, g: 2, b: 3 });
    }

    #[test]
    fn rehydration_restores_a_tui_on_the_alt_screen() {
        // The done-criterion scenario: a TUI (vim/claude) is on the alt-screen. The re-hydration
        // bytes must bring back the ALT grid, not just the shell history underneath it.
        let mut vt = VtScreen::new(5, 30);
        feed(&mut vt, "$ vim file\r\n"); // shell history on the main screen
        feed(&mut vt, "\x1b[?1049h\x1b[2J\x1b[H"); // vim enters alt-screen
        feed(&mut vt, "\x1b[1;1H  1 hello");
        feed(&mut vt, "\x1b[2;1H  2 world");
        feed(&mut vt, "\x1b[5;1H\x1b[7m-- INSERT --\x1b[0m");
        feed(&mut vt, "\x1b[?25l"); // vim hides the cursor
        let snap = vt.snapshot();

        let back = rehydrate(&snap, 5, 30);
        assert!(
            back.alt_active,
            "restored terminal is back in the alt-screen"
        );
        let alt = back.alt.expect("alt buffer restored");
        assert_eq!(alt.row_text(0), "  1 hello");
        assert_eq!(alt.row_text(1), "  2 world");
        assert_eq!(alt.row_text(4), "-- INSERT --");
        assert!(alt.cells[4 * alt.cols].attrs.contains(Attrs::REVERSE));
        assert!(
            !back.cursor_visible,
            "the TUI's hidden cursor stays hidden after restore"
        );
        // The shell history is still there underneath (visible when the TUI exits).
        assert_eq!(back.main.row_text(0), "$ vim file");
    }

    #[test]
    fn rehydration_is_idempotent() {
        // Applying the re-hydration bytes twice into the same terminal yields the same screen as
        // applying it once (the stream starts with reset+clear, so a replay is safe). This is the
        // "resume is idempotent" done-criterion at the byte-protocol level.
        let mut vt = VtScreen::new(3, 20);
        feed(&mut vt, "\x1b[?1049h\x1b[HTUI content\r\nsecond row");
        let snap = vt.snapshot();
        let bytes = snap.to_rehydration_bytes();

        let mut once = VtScreen::new(3, 20);
        once.feed(&bytes);
        let mut twice = VtScreen::new(3, 20);
        twice.feed(&bytes);
        twice.feed(&bytes);
        assert_eq!(
            once.snapshot(),
            twice.snapshot(),
            "re-applying the restore is idempotent"
        );
    }

    #[test]
    fn rehydration_restores_cursor_position() {
        let mut vt = VtScreen::new(5, 20);
        feed(&mut vt, "\x1b[3;7HX"); // land the cursor after X at (row2,col7)
        let snap = vt.snapshot();
        let back = rehydrate(&snap, 5, 20);
        assert_eq!(
            (back.main.cursor_row, back.main.cursor_col),
            (snap.main.cursor_row, snap.main.cursor_col),
            "cursor position is restored"
        );
    }
}
