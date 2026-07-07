import type { CellData, CursorState, TerminalCore, UnhandledSequence } from "@wterm/core";

const BLANK = 32; // space

/**
 * A lightweight `TerminalCore` fake for jsdom unit tests. The real ghostty core
 * is a WASM module that cannot be fetched/instantiated under jsdom, so tests
 * inject this via `setCoreFactory`.
 *
 * It records every write (decoded to text) in {@link written} — the primary
 * assertion surface ("this terminal received these bytes even while detached") —
 * AND maintains a MINIMAL linear screen grid from printable writes so the
 * `getCell`/`getRows`/`getCols` surface (and thus the deck's `__nyxDeck`
 * screen-text seam) reflects the visible content. It also tracks alt-screen
 * enter/leave from the DEC private-mode sequences a TUI uses. This is enough for
 * `WTerm.init()` + `write()` to run and for the store's contract tests to assert
 * on screen state without a real browser.
 */
export class FakeTerminalCore implements TerminalCore {
  /** Everything written (decoded to text) — the raw assertion surface. */
  written = "";
  private cols = 80;
  private rows = 24;
  private altScreen = false;
  private readonly decoder = new TextDecoder();
  /** The minimal screen grid: rows × cols of codepoints (0 = blank). */
  private grid: number[][] = [];
  private curRow = 0;
  private curCol = 0;

  private allocGrid(): void {
    this.grid = Array.from({ length: this.rows }, () => Array.from({ length: this.cols }, () => 0));
    this.curRow = 0;
    this.curCol = 0;
  }

  init(cols: number, rows: number): void {
    this.cols = cols;
    this.rows = rows;
    this.allocGrid();
  }
  resize(cols: number, rows: number): void {
    this.cols = cols;
    this.rows = rows;
    this.allocGrid();
  }
  writeString(str: string): void {
    this.written += str;
    if (str.includes("\x1b[?1049h") || str.includes("\x1b[?47h")) this.altScreen = true;
    if (str.includes("\x1b[?1049l") || str.includes("\x1b[?47l")) this.altScreen = false;
    // RIS (full reset) clears the screen model wherever it appears.
    if (str.includes("\x1bc")) this.allocGrid();

    // Paint printable runs onto the linear grid, interpreting CR/LF. This is NOT
    // a real VT parser (the real ghostty core is) — just enough for the
    // screen-text seam to reflect plain writes in tests. Strip OSC + CSI + RIS
    // escape sequences first so control bytes never land as glyphs.
    // The ESC (\x1b) / BEL (\x07) control bytes below are LOAD-BEARING: these
    // regexes exist precisely to strip VT escape sequences, so matching control
    // characters is intentional. `no-control-regex` flags the matched char
    // regardless of escape style (a `` rewrite would still fire), so we
    // disable it locally rather than obscure the intent.
    // oxlint-disable no-control-regex
    const cleaned = str
      .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "") // OSC
      .replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "") // CSI
      .replace(/\x1bc/g, ""); // RIS marker (already handled above)
    // oxlint-enable no-control-regex
    for (const ch of cleaned) {
      const code = ch.codePointAt(0) ?? 0;
      if (ch === "\r") {
        this.curCol = 0;
        continue;
      }
      if (ch === "\n") {
        this.curRow = Math.min(this.rows - 1, this.curRow + 1);
        this.curCol = 0;
        continue;
      }
      if (code < 32) continue; // other control chars (incl. stray ESC)
      if (this.curRow < this.rows && this.curCol < this.cols) {
        this.grid[this.curRow][this.curCol] = code;
      }
      this.curCol++;
      if (this.curCol >= this.cols) {
        this.curCol = 0;
        this.curRow = Math.min(this.rows - 1, this.curRow + 1);
      }
    }
  }
  writeRaw(data: Uint8Array): void {
    this.writeString(this.decoder.decode(data));
  }
  getCell(row: number, col: number): CellData {
    const char = this.grid[row]?.[col] ?? 0;
    return { char: char === 0 ? BLANK : char, fg: 256, bg: 256, flags: 0 };
  }
  isDirtyRow(): boolean {
    return true;
  }
  clearDirty(): void {}
  getCols(): number {
    return this.cols;
  }
  getRows(): number {
    return this.rows;
  }
  getCursor(): CursorState {
    return { row: this.curRow, col: this.curCol, visible: true };
  }
  cursorKeysApp(): boolean {
    return false;
  }
  bracketedPaste(): boolean {
    return false;
  }
  usingAltScreen(): boolean {
    return this.altScreen;
  }
  getTitle(): string | null {
    return null;
  }
  getResponse(): string | null {
    return null;
  }
  getScrollbackCount(): number {
    return 0;
  }
  getScrollbackCell(): CellData {
    return { char: BLANK, fg: 256, bg: 256, flags: 0 };
  }
  getScrollbackLineLen(): number {
    return 0;
  }
  getUnhandledSequences(): UnhandledSequence[] {
    return [];
  }
}
