// Load xterm's own stylesheet exactly as the app entry (main.tsx) does. It carries
// the LOAD-BEARING canvas-positioning rules; the render layers fall into normal
// flow without it (FEEDBACK.md #3). Importing it here lets the browser suite
// assert those rules actually apply to the live DOM.
import "@xterm/xterm/css/xterm.css";

import { render } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import type { Terminal as XTerm } from "@xterm/xterm";
import { page } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

import { Terminal } from "./terminal";

/**
 * BROWSER-MODE render test (real Chromium via Playwright, headless).
 *
 * Unlike the jsdom unit suite, this runs in a browser that actually PAINTS, so
 * it is the only place we can:
 *   1. prove the `@xterm/addon-webgl` renderer got a REAL WebGL context (not the
 *      DOM/canvas fallback);
 *   2. pin a visual baseline with `toMatchScreenshot`, so any flash/layout/render
 *      regression of `<Terminal>` shows up as a screenshot diff.
 *
 * The Tauri backend does not exist in the browser, so we mock its IPC exactly
 * like the unit suite. We do NOT rely on the PTY event stream for the visible
 * output — we write a fixed byte sequence straight into the xterm instance via
 * the `onInstance` seam, which makes the baseline deterministic (no shell, no
 * timing, no prompt noise).
 */

const SPAWNED_ID = 7;

/** Records every invoke so a test can assert a `pty_resize` (the SIGWINCH path) fired. */
let ipcCalls: { cmd: string; args: Record<string, unknown> }[] = [];

beforeEach(() => {
  ipcCalls = [];
  // Resolve pty_spawn so usePty's start() completes without a real backend;
  // every other command (write/resize/close) is a harmless no-op here.
  mockIPC(
    (cmd, args) => {
      ipcCalls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      return cmd === "pty_spawn" ? SPAWNED_ID : null;
    },
    { shouldMockEvents: true },
  );
});

afterEach(() => {
  // Clear any test host nodes without assigning HTML strings.
  document.body.replaceChildren();
});

/**
 * Read the WebGL context off the xterm WebGL renderer's canvas.
 *
 * The WebGL addon appends its own canvases under the terminal element; the GL
 * canvas is the one whose `getContext('webgl2'|'webgl')` returns a live
 * context. We probe every canvas under the terminal and return the first real
 * GL context found (or null if only 2d/none exist — i.e. the DOM/canvas
 * fallback path).
 */
function findWebglContext(root: HTMLElement): { vendor: string; renderer: string } | null {
  const canvases = root.querySelectorAll("canvas");
  for (const canvas of canvases) {
    // Don't *create* a context — only read one that the addon already made.
    // Asking for the same type returns the existing context if present.
    const gl =
      (canvas.getContext("webgl2") as WebGL2RenderingContext | null) ??
      (canvas.getContext("webgl") as WebGLRenderingContext | null);
    if (gl && typeof gl.getParameter === "function") {
      return {
        vendor: String(gl.getParameter(gl.VENDOR)),
        renderer: String(gl.getParameter(gl.RENDERER)),
      };
    }
  }
  return null;
}

it("renders <Terminal> with a REAL WebGL context and matches the visual baseline", async () => {
  // Fixed-size host so the screenshot is stable across runs/machines.
  const host = document.createElement("div");
  host.setAttribute("data-testid", "terminal-host");
  host.style.width = "640px";
  host.style.height = "360px";
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  document.body.appendChild(host);

  let term: XTerm | null = null;
  render(<Terminal onInstance={(t) => (term = t ?? term)} />, {
    container: host,
  });

  // Wait for the xterm instance + its WebGL addon to be attached and the GL
  // canvas to exist. The addon loads in an effect AFTER open(), so poll.
  const deadline = Date.now() + 5000;
  let gl: ReturnType<typeof findWebglContext> = null;
  while (Date.now() < deadline) {
    if (term) gl = findWebglContext(host);
    if (gl) break;
    await new Promise((r) => setTimeout(r, 50));
  }

  expect(term, "xterm instance must be created").not.toBeNull();

  // ── WebGL assertion ────────────────────────────────────────────────────────
  // This suite runs under headless Chromium via Playwright (see vitest.config.ts),
  // whose GL backend is SwiftShader — a real *software* WebGL context (ANGLE over
  // SwiftShader), NOT the DOM/canvas fallback. So we EXPECT a real context here
  // and assert it. If a future CI image runs a browser with NO GL at all, `gl`
  // would be null and this fails loudly rather than silently passing on the DOM
  // fallback — at which point the skip the PRD allows would be documented and
  // applied. As verified locally, Chromium provides WebGL, so this is a real
  // PASS, not a skip.
  expect(
    gl,
    "the @xterm/addon-webgl renderer must obtain a real WebGL context " +
      "(not the DOM/canvas fallback). If this is null, the headless browser " +
      "provides no WebGL — see terminal.browser.test.tsx for the documented skip.",
  ).not.toBeNull();

  // The terminal now renders in the bundled Fira Code variable face. The font
  // must be fully loaded before we screenshot, otherwise the baseline is taken
  // against the fallback face and is unstable across runs. Load the exact face
  // the component uses (14px "Fira Code Variable") and await fonts.ready.
  await document.fonts.load('14px "Fira Code Variable"');
  await document.fonts.ready;

  // Write a fixed, known sequence so the baseline is deterministic.
  const t = term as unknown as XTerm;
  t.write("nyx WebGL render check\r\n");
  t.write("\x1b[32mgreen\x1b[0m \x1b[31mred\x1b[0m \x1b[34mblue\x1b[0m\r\n");
  t.write("12345 67890 ABCDE fghij\r\n");

  // Give xterm a couple of frames to flush its async write + GL paint.
  await new Promise((r) => requestAnimationFrame(() => r(null)));
  await new Promise((r) => setTimeout(r, 100));

  // ── Visual regression baseline ──────────────────────────────────────────────
  // First run writes the baseline to the gitignored root .vitest-screenshots/
  // dir (see vitest.config.ts); subsequent runs diff against it. A
  // render/flash/layout regression makes the diff fail.
  await expect(page.getByTestId("terminal-host")).toMatchScreenshot("terminal-webgl-render");
});

it("focuses the xterm input when it becomes the active pane (typing fix, finding 01KV1J6C0T2581VSY2P2HKMS6D)", async () => {
  // Regression for the CRITICAL "cannot type in some terminals" bug: selecting a
  // terminal flips `active` but must ALSO move keyboard focus to that pane's
  // xterm, or its hidden textarea never receives keystrokes. We mount it INACTIVE,
  // park focus on a stand-in for the sidebar button the user just clicked, then
  // ACTIVATE the terminal and assert focus MOVES to the xterm helper textarea —
  // i.e. typing now reaches this terminal. (xterm grabs focus on `open()`, so the
  // load-bearing assertion is the post-activate one, after we steal focus back.)
  const host = document.createElement("div");
  host.style.width = "320px";
  host.style.height = "200px";
  document.body.appendChild(host);

  // A real element standing in for the sidebar row button the user clicked.
  const stealFocus = document.createElement("button");
  stealFocus.textContent = "sidebar";
  document.body.appendChild(stealFocus);

  let term: XTerm | null = null;
  const { rerender } = render(<Terminal active={false} onInstance={(t) => (term = t ?? term)} />, {
    container: host,
  });

  // Wait for the xterm instance to exist.
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline && !term) {
    await new Promise((r) => setTimeout(r, 25));
  }
  expect(term, "xterm instance must be created").not.toBeNull();

  // Move focus to the "sidebar button" — exactly what happens when the user
  // clicks a sidebar row to select a terminal (focus lands on that button, NOT
  // the terminal). This is the precondition under which the bug manifested.
  stealFocus.focus();
  expect(document.activeElement).toBe(stealFocus);

  // Activate it (what selecting it in the sidebar does to the deck).
  rerender(<Terminal active onInstance={(t) => (term = t ?? term)} />);

  // The focus effect defers one animation frame (the pane was display:none).
  // Poll until the xterm helper textarea is the active element.
  const focusDeadline = Date.now() + 3000;
  let focused = false;
  while (Date.now() < focusDeadline) {
    const ae = document.activeElement as HTMLElement | null;
    if (ae && ae.classList.contains("xterm-helper-textarea")) {
      focused = true;
      break;
    }
    await new Promise((r) => requestAnimationFrame(() => r(null)));
  }
  expect(focused, "activating a terminal must focus its xterm input so typing reaches it").toBe(
    true,
  );
});

it("derives the xterm theme from the CSS palette tokens as a parseable hex (oklch → #rrggbb)", async () => {
  // Pins the F3 fix: the tokens are authored in oklch(), which xterm cannot
  // parse, and on current Chromium/WebKit the naive getComputedStyle/fillStyle
  // round-trip serialises oklch back unchanged. So the component converts to a
  // real sRGB hex in JS via chroma-js (in resolveCssColor). This runs
  // in a real browser engine so the conversion is exercised end-to-end.
  const host = document.createElement("div");
  host.style.width = "320px";
  host.style.height = "200px";
  document.body.appendChild(host);

  // Force the dark palette so --background/--foreground are the dark tokens.
  document.documentElement.classList.add("dark");

  let term: XTerm | null = null;
  render(<Terminal onInstance={(t) => (term = t ?? term)} />, {
    container: host,
  });

  // The theme is applied in a mount effect; poll until xterm reports it.
  const deadline = Date.now() + 3000;
  let theme: { background?: string; foreground?: string } | undefined;
  while (Date.now() < deadline) {
    theme = (term as unknown as XTerm | null)?.options.theme;
    if (theme?.background && theme.background !== "#0a0a0a") break;
    await new Promise((r) => setTimeout(r, 25));
  }

  expect(term, "xterm instance must be created").not.toBeNull();
  const bg = theme?.background ?? "";
  const fg = theme?.foreground ?? "";

  // Must be a hex xterm parses — NOT a raw oklch() string.
  expect(bg).toMatch(/^#[0-9a-f]{6}$/i);
  expect(fg).toMatch(/^#[0-9a-f]{6}$/i);
  expect(bg).not.toMatch(/oklch/i);
  expect(fg).not.toMatch(/oklch/i);
  // The dark --background token oklch(0.145 0 0) resolves to a near-black; the
  // --foreground token oklch(0.985 0 0) resolves to a near-white. Sanity-check
  // the contrast so we never ship black-on-black.
  const lum = (h: string) =>
    parseInt(h.slice(1, 3), 16) + parseInt(h.slice(3, 5), 16) + parseInt(h.slice(5, 7), 16);
  expect(lum(bg)).toBeLessThan(lum(fg));
});

it("reconciles geometry when a hidden pane reappears: re-attaches WebGL + pushes a PTY resize (SIGWINCH) — #20/#23", async () => {
  // The dimensions/garbled-render fix in the real engine. We reproduce the #20
  // shape: the terminal pane is mounted but its CONTAINER is display:none (exactly
  // what the deck does for an inactive terminal, and what wraps the whole deck
  // behind a CommandView). While hidden the WebGL renderer must NOT be attached
  // (no context built at 0×0). When the pane is shown again, the activation
  // reconcile must (a) re-attach a live WebGL context and (b) push a `pty_resize`
  // so the child gets a SIGWINCH and redraws — even though the size is unchanged.
  const host = document.createElement("div");
  host.style.width = "480px";
  host.style.height = "300px";
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  document.body.appendChild(host);

  // The pane wrapper the deck controls: start HIDDEN (display:none → 0×0 box).
  const pane = document.createElement("div");
  pane.style.width = "100%";
  pane.style.height = "100%";
  pane.style.display = "none";
  host.appendChild(pane);

  let term: XTerm | null = null;
  let ptyId: number | null = null;
  // active=true but the wrapper is display:none, so the element box is 0×0 — the
  // exact stale-geometry condition. WebGL must stay OFF while hidden.
  render(
    <Terminal
      active
      onInstance={(t) => (term = t ?? term)}
      onPtyId={(id) => {
        ptyId = id;
      }}
    />,
    { container: pane },
  );

  // Let the instance create + the spawn resolve all the way through usePty, so a
  // later geometry resync has a concrete PTY id to target.
  const spawnDeadline = Date.now() + 5000;
  while (Date.now() < spawnDeadline && ptyId !== SPAWNED_ID) {
    await new Promise((r) => setTimeout(r, 25));
  }
  expect(term, "xterm instance must be created").not.toBeNull();
  expect(ptyId, "pty_spawn must resolve before revealing the hidden pane").toBe(SPAWNED_ID);

  // While hidden (0×0) no WebGL context should exist — building one at 0×0 is the
  // bug. (The DOM/canvas fallback may exist; findWebglContext only counts a real
  // GL context.)
  expect(
    findWebglContext(host),
    "no WebGL context must be built while the pane is display:none (0×0)",
  ).toBeNull();

  // Reveal the pane (what selecting the terminal / closing the command view does).
  const resizesBefore = ipcCalls.filter((c) => c.cmd === "pty_resize").length;
  pane.style.display = "block";

  // The activation reconcile (rAF) + the ResizeObserver 0→N transition now run the
  // pipeline: WebGL re-attaches and a pty_resize is pushed. Poll for both.
  const deadline = Date.now() + 5000;
  let gl: ReturnType<typeof findWebglContext> = null;
  let resized = false;
  while (Date.now() < deadline) {
    gl = findWebglContext(host);
    resized = ipcCalls.filter((c) => c.cmd === "pty_resize").length > resizesBefore;
    if (gl && resized) break;
    await new Promise((r) => setTimeout(r, 50));
  }

  expect(
    gl,
    "showing a hidden pane must (re)attach a live WebGL context against the real size",
  ).not.toBeNull();
  expect(
    resized,
    "showing a hidden pane must push a pty_resize so the child gets a SIGWINCH and redraws",
  ).toBe(true);
  // The reattached context must be healthy.
  const canvases = host.querySelectorAll("canvas");
  let lost = false;
  for (const c of canvases) {
    const ctx =
      (c.getContext("webgl2") as WebGL2RenderingContext | null) ??
      (c.getContext("webgl") as WebGLRenderingContext | null);
    if (ctx && typeof ctx.isContextLost === "function" && ctx.isContextLost()) lost = true;
  }
  expect(lost, "the reattached WebGL context must not be lost").toBe(false);
});

it("positions the renderer canvases absolutely (xterm.css loaded — guards the 'click jumps the content' bug)", async () => {
  // Regression for FEEDBACK.md #3: when `@xterm/xterm/css/xterm.css` is NOT
  // imported, `.xterm-screen` and its <canvas> layers default to `position:
  // static` and fall into NORMAL FLOW — the canvases stack vertically, so the
  // viewport goes mostly black with stray glyphs at the top and the real content
  // is pushed down, only "repairing" on the full repaint a selection drag forces.
  // The fix imports xterm.css globally (see main.tsx). This proves the
  // load-bearing rule (`.xterm-screen canvas { position: absolute }`) is actually
  // in effect on the live DOM in a real browser.
  const host = document.createElement("div");
  host.style.width = "320px";
  host.style.height = "200px";
  document.body.appendChild(host);

  let term: XTerm | null = null;
  render(<Terminal onInstance={(t) => (term = t ?? term)} />, { container: host });

  // Poll until xterm has opened and the renderer appended a <canvas> under
  // `.xterm-screen` (the WebGL/canvas layer mounts in an effect after open()).
  const deadline = Date.now() + 5000;
  let canvas: HTMLCanvasElement | null = null;
  let screen: HTMLElement | null = null;
  while (Date.now() < deadline) {
    screen = host.querySelector(".xterm-screen");
    canvas = screen?.querySelector("canvas") ?? null;
    if (canvas) break;
    await new Promise((r) => setTimeout(r, 50));
  }

  expect(term, "xterm instance must be created").not.toBeNull();
  expect(screen, "the .xterm-screen element must exist").not.toBeNull();
  expect(canvas, "the renderer must append a <canvas> under .xterm-screen").not.toBeNull();

  // The load-bearing rules from xterm.css. Without the stylesheet these are
  // `static` and the canvases stack in flow (the reported bug).
  expect(getComputedStyle(screen as HTMLElement).position).toBe("relative");
  expect(getComputedStyle(canvas as HTMLCanvasElement).position).toBe("absolute");
});
