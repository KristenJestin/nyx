// The REAL Tailwind utilities (the vitest config compiles them for a spec that imports
// `globals.css`), so the overlay's `absolute inset-y-0 right-0` geometry and its `pointer-events-*`
// classes actually apply — the click targets below are aimed with the real mouse, so the real
// layout has to be there for them to land where the test says they land.
import "@/globals.css";

import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";
import { userEvent } from "vitest/browser";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";

import type { Spec } from "../specs/types";
import { SpecOverlay } from "./spec-overlay";
import { ACTIVE_SESSION, detail, spec } from "./test-fixtures";

/**
 * BROWSER-MODE FOCUS-RESTORATION spec (real Chromium via Playwright, headless) for the overlay
 * panel's OUTSIDE-CLICK dismissal — the one dismissal gesture whose focus outcome jsdom cannot
 * express at all.
 *
 * WHY IT EXISTS (audit finding `01M06TCF3CZ8YS8NA386X0T7M3`): the outside-click listener runs on
 * `mousedown` in the CAPTURE phase, i.e. BEFORE the browser's own default action for that mousedown.
 * That default action then OVERWRITES the focus in BOTH branches — a focusable target takes it, a
 * dead zone sends it to `document.body` — so a `tab.focus()` issued from the capture handler is a
 * plain no-op in a real browser, while jsdom (which runs no default action at all) reports it as a
 * success. The restoration is therefore DEFERRED past the default action and conditioned on the
 * click having taken NO focus (`use-overlay-panel`), and this file is what measures that both
 * branches land where the decision says they must:
 *
 *  - FOCUSABLE target (the terminal's own textarea is one) ⇒ the TARGET keeps the focus. The panel's
 *    non-modal contract forbids stealing it back; the click is the user aiming at the terminal.
 *  - DEAD ZONE (nothing focusable under the cursor) ⇒ the LANGUETTE takes it, never `document.body`.
 *    That is the keyboard user's place in the page, and the sole reason the restoration exists.
 *
 * The COUNTERFACTUAL this file pins is precise: undo the deferral (restore straight from the capture
 * handler) and BOTH assertions below go red — the first would find the languette focused instead of
 * the target only if the restoration also stopped being conditional, and the second would find
 * `body`. jsdom cannot go red on either, which is exactly why the sibling assertion in
 * `overlay-a11y.test.tsx` documents its own scope instead of claiming to cover this.
 */

/** The pane the overlay is anchored over — kept inside the browser-mode viewport. */
const PANE_W = 420;
const PANE_H = 320;

/**
 * In the browser suite there is no Electron preload, so `@/bridge` already resolves `nyxBridge` to a
 * live {@link FakeNyxBridge} (its documented test/SSR safety net). We drive THAT instance directly
 * instead of the `vi.mock`-based `test-harness` shim, so the spec never depends on module mocking.
 */
const fake = nyxBridge as FakeNyxBridge;

/** Serve the overlay's read models for one mapped Spec. */
function serve(s: Spec) {
  fake.onAnyCommand((cmd) => {
    if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [s] };
    if (cmd === "spec_detail") return detail(s);
    return null;
  });
}

/** Poll until `pred()` holds or we time out; returns whether it held. */
async function waitFor(pred: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return pred();
}

/**
 * A fixed, pane-shaped host carrying the two outside-click targets this file contrasts, both LEFT of
 * the overlay's 36px right-edge band so neither is ever covered by the languette column:
 *
 *  - `terminal-input`, a `<textarea>` — the FOCUSABLE branch. It is the shape of the real thing: the
 *    xterm viewport's own helper textarea is what a click in the terminal area actually focuses.
 *  - `terminal-dead`, a plain `<div>` — the DEAD-ZONE branch. Nothing under the cursor can take the
 *    focus, so Chromium runs its focus-fixup step and parks the focus on `document.body`.
 */
function mountHost(): { host: HTMLElement; input: HTMLTextAreaElement; dead: HTMLElement } {
  const host = document.createElement("div");
  host.setAttribute("data-testid", "pane-host");
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  host.style.width = `${PANE_W}px`;
  host.style.height = `${PANE_H}px`;
  document.body.appendChild(host);

  const input = document.createElement("textarea");
  input.setAttribute("data-testid", "terminal-input");
  input.style.position = "absolute";
  input.style.left = "0";
  input.style.top = "0";
  input.style.width = "160px";
  input.style.height = "80px";
  host.appendChild(input);

  const dead = document.createElement("div");
  dead.setAttribute("data-testid", "terminal-dead");
  dead.style.position = "absolute";
  dead.style.left = "0";
  dead.style.top = "120px";
  dead.style.width = "160px";
  dead.style.height = "80px";
  host.appendChild(dead);

  return { host, input, dead };
}

/**
 * Mount the overlay over the host, OPEN its panel with a real click on the languette, and hand back
 * both. The open state is awaited on the panel actually being mounted AND holding the focus — the
 * hook moves the focus INTO the panel on open, so starting from anywhere else would measure a
 * restoration that had nothing to restore.
 */
async function openPanel(host: HTMLElement): Promise<{ tab: HTMLElement; panel: HTMLElement }> {
  const mount = document.createElement("div");
  host.appendChild(mount);
  render(<SpecOverlay terminalId="term-1" />, { container: mount });

  const mounted = await waitFor(() => host.querySelector("[data-overlay-tab]") !== null);
  expect(mounted, "the overlay languette mounted").toBe(true);
  const tab = host.querySelector("[data-overlay-tab]") as HTMLElement;

  await userEvent.click(tab);
  const opened = await waitFor(() => {
    const panel = document.querySelector("[data-overlay-panel]");
    return panel !== null && document.activeElement === panel;
  });
  expect(opened, "the panel opened and took the focus").toBe(true);
  return { tab, panel: document.querySelector("[data-overlay-panel]") as HTMLElement };
}

/** A stable label for wherever the focus ended up, so a failure names it instead of dumping a node. */
function activeLabel(tab: HTMLElement): string {
  const active = document.activeElement;
  if (active === null) return "null";
  if (active === document.body) return "body";
  if (active === tab) return "overlay-languette";
  if (active.closest("[data-overlay-panel]") !== null) return "overlay-panel";
  return active.getAttribute("data-testid") ?? active.tagName.toLowerCase();
}

beforeEach(() => {
  fake.reset();
});

afterEach(() => {
  cleanup();
  fake.reset();
  document.body.replaceChildren();
});

it("outside click on a FOCUSABLE target: the panel closes and the TARGET keeps the focus", async () => {
  serve(spec({ id: "s1", number: 7, title: "Auth revamp", status: "in_progress" }));
  const { host, input } = mountHost();
  const { tab } = await openPanel(host);

  await userEvent.click(input);

  const closed = await waitFor(() => document.querySelector("[data-overlay-panel]") === null);
  expect(closed, "the outside click dismissed the panel").toBe(true);
  // The decision this pins: NOTHING is stolen from the terminal. The restoration is deferred past
  // the browser's default action AND conditioned on that action having taken no focus, so a target
  // that took it keeps it. A restoration issued from the capture handler would be overwritten here
  // anyway — this branch is the one the previous pass already described correctly.
  const settled = await waitFor(() => activeLabel(tab) === "terminal-input");
  expect(settled, `the clicked target keeps the focus (saw: ${activeLabel(tab)})`).toBe(true);
});

it("outside click on a DEAD ZONE: the panel closes and the LANGUETTE takes the focus, not body", async () => {
  serve(spec({ id: "s1", number: 7, title: "Auth revamp", status: "in_progress" }));
  const { host, dead } = mountHost();
  const { tab } = await openPanel(host);

  await userEvent.click(dead);

  const closed = await waitFor(() => document.querySelector("[data-overlay-panel]") === null);
  expect(closed, "the outside click dismissed the panel").toBe(true);
  // THE branch the finding measured as broken: Chromium's focus-fixup parks the focus on
  // `document.body` here, so a `tab.focus()` from the capture handler is overwritten and the
  // keyboard user loses their place. The deferred restoration fires after that fixup and gives the
  // languette back — the outcome the F2/F3 criterion asked for.
  const settled = await waitFor(() => activeLabel(tab) === "overlay-languette");
  expect(settled, `the languette takes the focus back (saw: ${activeLabel(tab)})`).toBe(true);
});
