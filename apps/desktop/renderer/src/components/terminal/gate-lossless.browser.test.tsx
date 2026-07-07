// Load wterm's stylesheet exactly as the app entry (main.tsx) does, so the live
// DOM render carries the real `.wterm` / `.term-grid` / `.term-row` rules.
import "@wterm/dom/css";

import { render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";

import { nyxBridge } from "@/bridge";
import { Terminal } from "./terminal";
import { getTerminal, __resetTerminalStore } from "./terminal-store";

/**
 * Install the backend double on the LIVE `nyxBridge` the terminal instances use.
 *
 * In Vitest BROWSER mode, `@/bridge`'s `vi.mock` shim (`test-harness`) does NOT
 * shadow the `nyxBridge` export the way it does under jsdom — the code under test
 * keeps the module's own fallback `FakeNyxBridge` (there is no Electron
 * `window.nyxCore` in the browser). `FakeNyxBridge` exposes `onAnyCommand`, so we
 * install our catch-all directly on THE instance the terminal calls. This is what
 * lets us drive the real restart re-hydration path (`terminal_screen_rehydration`)
 * with the real ghostty core.
 */
interface CatchAllBridge {
  onAnyCommand(handler: (command: string, args?: Record<string, unknown>) => unknown): unknown;
  reset?(): void;
  calls?: ReadonlyArray<{ command: string }>;
}
function installBackend(handler: (cmd: string, args?: Record<string, unknown>) => unknown): void {
  (nyxBridge as unknown as CatchAllBridge).onAnyCommand(handler);
}
/** How many `pty_spawn` invokes the terminals have made so far (respawn detector). */
function spawnCount(): number {
  const calls = (nyxBridge as unknown as CatchAllBridge).calls ?? [];
  return calls.filter((c) => c.command === "pty_spawn").length;
}

/**
 * THE v2.2.5 VALIDATION GATE — nav-lossless + restart-lossless, end-to-end.
 *
 * PRD `01KWMDBKW7GZ0X310TK64QEN32`, Phase 3, task `01KWMDBKWA3KBKGFR8B036APC1`.
 *
 * This is the bout-à-bout demonstration that "plus rien ne se perd", run in a
 * REAL Chromium (Playwright, headless) with the REAL ghostty WASM core — the only
 * place the wterm+ghostty screen state actually exists. It ties together the two
 * halves the unit/sidecar suites prove in isolation:
 *
 *   1. NAV LOSSLESS — a full route detach→reattach cycle (the `<Terminal>` pane
 *      UNMOUNTS on navigation and REMOUNTS on return, not merely `display:none`).
 *      We open a shell terminal AND a TUI (alt-screen), navigate AWAY (unmount)
 *      and BACK (remount), and assert the buffer, the alt-screen frame AND the
 *      cursor survive intact — and that the SAME store instance came back (zero
 *      reconstruction: no respawn, same object identity, its persistent DOM node
 *      re-parented into the fresh pane).
 *
 *   2. RESTART LOSSLESS — a brand-new instance (as after killing/relaunching the
 *      app) re-hydrates from the sidecar's VT SNAPSHOT. We mock the sidecar's
 *      `terminal_screen_rehydration` to return the exact ANSI byte stream
 *      `ScreenSnapshot::to_rehydration_bytes` emits for a TUI on the alt-screen
 *      (reset+clear, enter alt-screen, repaint, position cursor). The fresh wterm
 *      writes those bytes and the ghostty core repaints the alt-screen TUI
 *      FAITHFULLY — not just the scrollback — with the cursor where it was.
 *
 * The sidecar half (a real PTY running a real TUI → a faithful VT snapshot → the
 * disk round-trip → rehydration bytes) is proven by the Rust integration tests
 * (`nyx-sidecar` `pty.rs::snapshot_captures_alt_screen_tui`,
 * `snapshot_store.rs::rehydration_bytes_carry_the_alt_screen_tui`) and the core
 * `nyx-core` `vt_screen.rs` round-trip tests. This gate closes the loop on the
 * RENDERER side with the real core, so the demonstration is genuinely end-to-end.
 */

const SPAWNED_ID = 42;

/**
 * The ANSI re-hydration byte stream the SIDECAR hands the renderer for a TUI that
 * was on the alt-screen at shutdown, byte-for-byte the shape of
 * `nyx_core::vt_screen::ScreenSnapshot::to_rehydration_bytes`:
 *   - reset attrs + clear the main screen + home (clean canvas);
 *   - enter the alt-screen, reset+clear+home again;
 *   - repaint the alt frame (here: a couple of TUI rows);
 *   - position the cursor and set its visibility.
 * We build it as a Uint8Array (what `terminal_screen_rehydration` returns as
 * `{ bytes: number[] }`) so the fresh core reconstructs the exact alt-screen.
 */
const TUI_MARKER = "GATE-RESTART-TUI";
function tuiRehydrationBytes(): number[] {
  const enc = new TextEncoder();
  const parts: string[] = [
    "\x1b[0m\x1b[2J\x1b[H", // clean canvas (main screen)
    "\x1b[?1049h", // enter alt-screen
    "\x1b[0m\x1b[2J\x1b[H", // clean alt canvas
    "\x1b[1;1H" + TUI_MARKER, // repaint row 1
    "\x1b[2;1H-- INSERT --", // repaint row 2 (a vim-like status line)
    "\x1b[3;5H", // position the cursor at row 3, col 5 (1-based)
    "\x1b[?25h", // cursor visible
  ];
  return Array.from(enc.encode(parts.join("")));
}

/** The record ids each test uses, so the IPC mock can target rehydration per id. */
let rehydrationById: Record<string, number[]> = {};

beforeEach(() => {
  rehydrationById = {};
  installBackend((cmd, args) => {
    if (cmd === "pty_spawn") return SPAWNED_ID;
    // The restart re-hydration path: hand back the persisted VT snapshot bytes
    // for this record id (empty when none — the fresh-terminal fallback).
    if (cmd === "terminal_screen_rehydration") {
      const recordId = String((args as { recordId?: string })?.recordId ?? "");
      return { bytes: rehydrationById[recordId] ?? [] };
    }
    if (cmd === "list_terminals") return [];
    if (cmd === "terminal_info") return { cwd: null, foreground: null };
    return null;
  });
});

afterEach(() => {
  __resetTerminalStore();
  (nyxBridge as unknown as CatchAllBridge).reset?.();
  document.body.replaceChildren();
});

function mountHost(w = 640, h = 360): HTMLElement {
  const host = document.createElement("div");
  host.style.width = `${w}px`;
  host.style.height = `${h}px`;
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  document.body.appendChild(host);
  return host;
}

/** Poll until the wterm DOM grid has been built inside `host`. */
async function waitForRender(host: HTMLElement): Promise<void> {
  const deadline = Date.now() + 8000;
  while (Date.now() < deadline) {
    if (host.querySelector(".wterm .term-grid .term-row")) return;
    await new Promise((r) => setTimeout(r, 50));
  }
}

async function waitFor(pred: () => boolean, timeoutMs = 6000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 50));
  }
  return pred();
}

const raf = () => new Promise((r) => requestAnimationFrame(() => r(null)));

it("NAV LOSSLESS: a shell + a TUI survive a full route unmount→remount intact, with ZERO reconstruction", async () => {
  const host = mountHost();

  // --- Open two terminals: a plain shell and a full-screen TUI (alt-screen). ---
  // We mount the panes via <Terminal> (the SAME component a route mounts), then
  // paint deterministic frames straight into each core (the path a pty://output
  // chunk takes) so the assertion has no shell/timing noise.
  const { unmount } = render(
    <>
      <Terminal recordId="shell" active />
      <Terminal recordId="tui" active={false} />
    </>,
    { container: host },
  );
  await waitFor(() => host.querySelectorAll(".wterm .term-grid").length === 2);

  const shell = getTerminal("shell")!;
  const tui = getTerminal("tui")!;
  expect(shell, "shell instance exists").toBeDefined();
  expect(tui, "tui instance exists").toBeDefined();

  // Shell frame: some scrollback the user would not want to lose.
  shell.writeToScreen("nyx nav-lossless shell line\r\n");
  shell.writeToScreen("\x1b[32mGATE-SHELL-KEEP\x1b[0m\r\n");

  // TUI frame: enter the alt-screen and paint a full-screen redraw, then park the
  // cursor at a known spot (row 4, col 7 — 1-based) with the pen mid-attribute.
  tui.writeToScreen("\x1b[?1049h\x1b[2J\x1b[H");
  tui.writeToScreen("GATE-NAV-TUI-FRAME");
  tui.writeToScreen("\x1b[4;7H");
  await raf();
  await new Promise((r) => setTimeout(r, 120));

  // Both shells have spawned exactly once by now — the baseline for the respawn
  // detector below (a reconstruction on return would spawn again).
  await waitFor(() => spawnCount() === 2, 4000);
  const spawnsBefore = spawnCount();
  expect(spawnsBefore, "each terminal spawned exactly once before navigation").toBe(2);

  // Capture the pre-navigation truth: identities, buffers, alt-screen, cursor.
  const shellNodeBefore = shell.el;
  const tuiNodeBefore = tui.el;
  expect(tui.usingAltScreen(), "the TUI is on the alt-screen before navigation").toBe(true);
  const shellBufBefore = shell.readScreenText();
  const tuiBufBefore = tui.readScreenText();
  const tuiCursorBefore = tui.readCursor();
  expect(shellBufBefore).toContain("GATE-SHELL-KEEP");
  expect(tuiBufBefore).toContain("GATE-NAV-TUI-FRAME");
  expect(tuiCursorBefore, "cursor is readable before navigation").not.toBeNull();

  // --- NAVIGATE AWAY: unmount the whole subtree (a route change). The panes'
  // cleanup DETACHES each persistent node; it must NOT destroy the instances. ---
  unmount();
  await new Promise((r) => setTimeout(r, 100));

  // The instances SURVIVE the unmount in the module-level store (no host, no
  // destroy on navigation) — the load-bearing no-host property.
  expect(getTerminal("shell"), "shell survives navigation in the store").toBe(shell);
  expect(getTerminal("tui"), "tui survives navigation in the store").toBe(tui);
  // Their persistent nodes were DETACHED (removed from the DOM), never rebuilt.
  expect(shell.el.isConnected, "detached shell node is out of the DOM while navigated away").toBe(
    false,
  );
  expect(tui.el.isConnected, "detached tui node is out of the DOM while navigated away").toBe(
    false,
  );
  // Even while detached, the core still holds the full screen state.
  expect(tui.usingAltScreen(), "alt-screen persists while detached").toBe(true);
  expect(tui.readScreenText()).toContain("GATE-NAV-TUI-FRAME");

  // --- NAVIGATE BACK: remount fresh panes for the SAME record ids (route return).
  // `ensureTerminal` must return the EXISTING instances (idempotent — no respawn),
  // and `attachTerminal` re-parents their SAME persistent nodes into the new panes.
  const host2 = mountHost();
  render(
    <>
      <Terminal recordId="shell" active />
      <Terminal recordId="tui" active={false} />
    </>,
    { container: host2 },
  );
  await waitForRender(host2);
  await raf();
  await new Promise((r) => setTimeout(r, 120));

  const shellAfter = getTerminal("shell")!;
  const tuiAfter = getTerminal("tui")!;

  // ZERO reconstruction: same instance objects, SAME persistent DOM nodes.
  expect(shellAfter, "same shell instance after return (not reconstructed)").toBe(shell);
  expect(tuiAfter, "same tui instance after return (not reconstructed)").toBe(tui);
  expect(shellAfter.el, "shell reused its persistent node").toBe(shellNodeBefore);
  expect(tuiAfter.el, "tui reused its persistent node").toBe(tuiNodeBefore);
  // The persistent nodes were re-attached into the new host (not orphaned).
  expect(host2.contains(shellAfter.el), "shell node re-attached into the returned pane").toBe(true);
  expect(host2.contains(tuiAfter.el), "tui node re-attached into the returned pane").toBe(true);

  // State is INTACT: buffers equal, alt-screen still active, cursor unchanged.
  expect(shellAfter.readScreenText(), "shell buffer intact across navigation").toBe(shellBufBefore);
  expect(tuiAfter.usingAltScreen(), "TUI still on the alt-screen after return").toBe(true);
  expect(tuiAfter.readScreenText(), "TUI alt-screen frame intact across navigation").toBe(
    tuiBufBefore,
  );
  expect(tuiAfter.readCursor(), "TUI cursor intact across navigation").toEqual(tuiCursorBefore);

  // ZERO reconstruction, hard proof: no NEW `pty_spawn` fired on the reattach — the
  // reattach re-parented the existing node and never re-ran the shell (idempotent
  // `ensureTerminal`). The spawn count is unchanged across the whole nav cycle.
  expect(spawnCount(), "no terminal was respawned on navigation return").toBe(spawnsBefore);
});

it("RESTART LOSSLESS: a fresh instance re-hydrates a TUI faithfully from the sidecar VT snapshot", async () => {
  // Simulate the post-restart boot: the sidecar has a persisted VT snapshot for
  // this record id (a TUI that was on the alt-screen at shutdown). A BRAND-NEW
  // instance is created (no prior in-memory state — the app was killed), and its
  // `startPty` first asks the sidecar for the re-hydration bytes.
  rehydrationById["restarted-tui"] = tuiRehydrationBytes();

  const host = mountHost(560, 320);
  render(<Terminal recordId="restarted-tui" active />, { container: host });
  await waitForRender(host);

  const term = getTerminal("restarted-tui")!;
  expect(term, "the restarted instance exists").toBeDefined();

  // The instance fetched the snapshot bytes and wrote them into the fresh core.
  // Wait until the real ghostty core has replayed them onto the alt-screen.
  const restored = await waitFor(
    () => term.usingAltScreen() && term.readScreenText().includes(TUI_MARKER),
  );
  expect(
    restored,
    "the fresh terminal must repaint the alt-screen TUI from the VT snapshot (not just scrollback)",
  ).toBe(true);

  // FAITHFUL restore: it is on the alt-screen (a TUI, not the main scrollback),
  // the TUI content is back, and the cursor was positioned by the snapshot bytes.
  expect(term.usingAltScreen(), "restored terminal is back on the alt-screen").toBe(true);
  const buf = term.readScreenText();
  expect(buf, "the alt-screen TUI content is restored").toContain(TUI_MARKER);
  expect(buf, "the TUI status line is restored too").toContain("-- INSERT --");

  const cursor = term.readCursor();
  expect(cursor, "the cursor is readable after re-hydration").not.toBeNull();
  // The snapshot positioned the cursor at row 3, col 5 (1-based) → (2, 4) 0-based.
  expect(cursor!.row, "cursor row restored from the snapshot").toBe(2);
  expect(cursor!.col, "cursor col restored from the snapshot").toBe(4);
  expect(cursor!.visible, "cursor visibility restored from the snapshot").toBe(true);

  // The DOM projection reflects the restored frame (a real paint, not a canvas).
  const gridText = host.querySelector(".term-grid")?.textContent ?? "";
  expect(gridText).toContain(TUI_MARKER);
});

it("RESTART with NO snapshot spawns a fresh terminal (fallback, not a crash)", async () => {
  // A brand-new terminal (or a run with no prior screen) → the sidecar returns
  // empty bytes; re-hydration restores nothing and the terminal just spawns fresh.
  const host = mountHost(400, 240);
  render(<Terminal recordId="fresh-no-snapshot" active />, { container: host });
  await waitForRender(host);

  const term = getTerminal("fresh-no-snapshot")!;
  expect(term, "a fresh terminal still comes up when there is no snapshot").toBeDefined();
  // No snapshot → not driven onto the alt-screen by re-hydration.
  expect(term.usingAltScreen(), "no snapshot means no alt-screen restore").toBe(false);
  // And it is a live, usable core (its grid is rendered).
  expect(
    host.querySelector(".wterm .term-grid"),
    "the fresh terminal rendered its grid",
  ).not.toBeNull();
});

/**
 * The `.term-row` elements that carry the LIVE screen rows (excludes the
 * `.term-scrollback-row` history rows). wterm's Renderer projects each grid row
 * into one `<div class="term-row">` whose `textContent` is the rendered line
 * (out-of-bounds cells render as spaces). The DOM projection is the natively
 * selectable/searchable surface — the ghostty core exposes NO find/search API
 * (see `@wterm/core` `TerminalCore`: only cell/scrollback accessors), so
 * selection and find are BROWSER-NATIVE over this DOM, exactly as they are for
 * the real user.
 */
function liveRows(host: HTMLElement): HTMLElement[] {
  return Array.from(
    host.querySelectorAll<HTMLElement>(".wterm .term-grid .term-row:not(.term-scrollback-row)"),
  );
}

/** Poll until one of `host`'s live `.term-row`s renders `needle` in its text. */
async function waitForRowText(host: HTMLElement, needle: string): Promise<boolean> {
  return waitFor(() => liveRows(host).some((row) => (row.textContent ?? "").includes(needle)));
}

/**
 * Map a row-global column (an offset into a row's `.textContent`) to the concrete
 * `(text node, offset)` it lands in. wterm renders each row as a sequence of
 * per-attribute `<span>`s, so a plain `indexOf` on the row's text does not point
 * at a single node; this walks the row's text nodes to resolve a real DOM
 * boundary a Range can use.
 */
function textNodeAt(row: HTMLElement, column: number): { node: Text; offset: number } | null {
  const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
  let seen = 0;
  let node = walker.nextNode() as Text | null;
  while (node) {
    const len = node.data.length;
    if (column <= seen + len) return { node, offset: column - seen };
    seen += len;
    node = walker.nextNode() as Text | null;
  }
  return null;
}

it("NATIVE SELECTION: the rendered screen text is selectable via window.getSelection over .term-row", async () => {
  // Paint known content into a REAL ghostty core, let wterm project it to the DOM,
  // then select it the way a user drags across the screen — a browser-native Range
  // over the `.term-row` text nodes — and assert the selection returns that text.
  const host = mountHost(560, 320);
  render(<Terminal recordId="select-native" active />, { container: host });
  await waitForRender(host);

  const term = getTerminal("select-native")!;
  const KNOWN = "SELECTABLE-NATIVE-TEXT";
  term.writeToScreen(`${KNOWN}\r\n`);
  await raf();
  const painted = await waitForRowText(host, KNOWN);
  expect(painted, "the known line is projected into a .term-row").toBe(true);

  // Find the row that carries the known text and select JUST that row's content
  // with a native Range (start of the row → end of the row), the same primitive a
  // drag-select produces.
  const row = liveRows(host).find((r) => (r.textContent ?? "").includes(KNOWN));
  expect(row, "a .term-row carries the known text").toBeDefined();

  const selection = window.getSelection();
  expect(selection, "the browser exposes a Selection").not.toBeNull();
  selection!.removeAllRanges();
  const range = document.createRange();
  range.selectNodeContents(row!);
  selection!.addRange(range);

  // The native selection returns the rendered text — proof the screen is
  // selectable as real DOM, not an opaque canvas.
  const selected = selection!.toString();
  expect(selected, "window.getSelection() returns the rendered row text").toContain(KNOWN);
  // The Range's own extracted text agrees (independent of the live Selection object).
  expect(range.toString(), "the DOM Range spans the selectable text").toContain(KNOWN);

  selection!.removeAllRanges();
});

it("NATIVE FIND: a known occurrence is found and positioned in the rendered buffer", async () => {
  // wterm/ghostty expose NO find/search method (checked `@wterm/core` d.ts), so
  // find is browser-native over the DOM projection — walk the rendered rows'
  // textContent to LOCATE a known occurrence and POSITION it (row index + column),
  // which is exactly what a native find/highlight would key off.
  const host = mountHost(560, 320);
  render(<Terminal recordId="find-native" active />, { container: host });
  await waitForRender(host);

  const term = getTerminal("find-native")!;
  const NEEDLE = "FINDABLE-NEEDLE-7X";
  // Put the needle on the third line so the match carries a non-trivial row index.
  term.writeToScreen("first line of output\r\n");
  term.writeToScreen("second line of output\r\n");
  term.writeToScreen(`a line containing the ${NEEDLE} token\r\n`);
  await raf();
  const painted = await waitForRowText(host, NEEDLE);
  expect(painted, "the needle line is projected into a .term-row").toBe(true);

  // Native find: scan the rendered rows for the needle and record where it sits.
  const rows = liveRows(host);
  let matchRow = -1;
  let matchCol = -1;
  for (let i = 0; i < rows.length; i++) {
    const text = rows[i].textContent ?? "";
    const col = text.indexOf(NEEDLE);
    if (col !== -1) {
      matchRow = i;
      matchCol = col;
      break;
    }
  }

  // The occurrence is FOUND (not -1) and POSITIONED at a real row/column — enough
  // to drive a native highlight/scroll-to-match. Positionable == findable.
  expect(matchRow, "the needle is found in a rendered row").toBeGreaterThanOrEqual(0);
  expect(matchCol, "the needle is positioned at a real column in that row").toBeGreaterThanOrEqual(
    0,
  );
  expect(
    rows[matchRow].textContent ?? "",
    "the found row actually contains the needle at the reported column",
  ).toContain(NEEDLE);
  expect(
    (rows[matchRow].textContent ?? "").slice(matchCol, matchCol + NEEDLE.length),
    "the reported position slices out exactly the needle",
  ).toBe(NEEDLE);

  // And the match is browser-selectable at that position (find → highlight), tying
  // (b) back to the native-selection primitive. wterm splits a row into per-attribute
  // `<span>`s, so map the row-global column to the concrete (text node, offset) it
  // falls in by walking the row's text nodes.
  const boundary = textNodeAt(rows[matchRow], matchCol);
  const end = textNodeAt(rows[matchRow], matchCol + NEEDLE.length);
  expect(boundary, "the match start maps to a real text node").not.toBeNull();
  expect(end, "the match end maps to a real text node").not.toBeNull();
  const range = document.createRange();
  range.setStart(boundary!.node, boundary!.offset);
  range.setEnd(end!.node, end!.offset);
  expect(range.toString(), "the found range highlights exactly the needle").toBe(NEEDLE);
});
