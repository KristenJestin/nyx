import { render } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { page, userEvent } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

// The REAL Tailwind utilities: the palette is a portalled, fixed-position overlay and the terminal
// under it is sized by `h-full`, so without the app's stylesheet neither has a real box.
import "@/globals.css";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";
import type { GlobalSearchHit, GlobalSearchKind } from "@/bridge/contract";
import { Terminal } from "@/components/terminal/terminal";
import { __resetTerminalStore } from "@/components/terminal/terminal-store";
import { ActiveProjectProvider } from "@/components/terminals/active-project";

import { SearchPalette } from "./search-palette";

/**
 * THE CHORD, IN A REAL BROWSER, OVER A REAL TERMINAL.
 *
 * jsdom can dispatch a `keydown` at `document` and watch a hook fire — but that is not the claim
 * this PRD makes. The claim is that `Mod+K` reaches the palette WHILE A TERMINAL HOLDS THE
 * KEYBOARD, and a terminal holding the keyboard is precisely what jsdom cannot stage: xterm's
 * custom key handler, its helper textarea, the propagation it stops and the byte it would otherwise
 * put on the PTY are all real-DOM behaviour. So this spec runs in Browser Mode with a live xterm:
 * the keystrokes come from the browser's own keyboard (`userEvent`), through xterm's handler, and
 * the PTY writes are counted on the far side.
 *
 * The backend is the {@link FakeNyxBridge} (no Electron, no sidecar, no real shell) — this proves
 * the WIRING under a real event loop and a real focus model, not the capability against a live
 * shell, which is the CDP gate's job.
 */
const backend = nyxBridge as unknown as FakeNyxBridge;

/** Everything the terminal's keystrokes put on the wire, decoded. Must stay EMPTY for `Mod+K`. */
let ptyWrites = "";

const decoder = new TextDecoder();

function hit(kind: GlobalSearchKind, over: Partial<GlobalSearchHit> = {}): GlobalSearchHit {
  return {
    kind,
    id: `${kind}-1`,
    project_id: "p1",
    project_name: "Alpha",
    workspace_id: null,
    workspace_name: null,
    spec_id: null,
    terminal_id: null,
    label: `${kind} label`,
    detail: null,
    extract: null,
    human_key: null,
    score: 0.5,
    updated_at: Date.now(),
    ...over,
  };
}

const HITS: GlobalSearchHit[] = [
  hit("spec", { id: "s1", label: "Recent spec" }),
  hit("spec", { id: "s2", label: "Older spec" }),
];

let host: HTMLElement;

beforeEach(async () => {
  await page.viewport(900, 700);
  ptyWrites = "";
  window.localStorage.clear();
  backend.reset();
  let nextPtyId = 300;
  backend.onAnyCommand((cmd, args) => {
    if (cmd === "pty_spawn") return nextPtyId++;
    if (cmd === "pty_write") {
      const bytes = Array.isArray(args?.data) ? (args.data as number[]) : [];
      ptyWrites += decoder.decode(Uint8Array.from(bytes));
      return null;
    }
    // The wire ANSWER: the hits plus `knowledge_empty` — the install holds Specs here.
    if (cmd === "global_search") return { hits: HITS, knowledge_empty: false };
    if (cmd === "list_terminals") return [];
    if (cmd === "terminal_info") return { cwd: null, foreground: null };
    return null;
  });
  host = document.createElement("div");
  host.style.cssText = "position:fixed;top:0;left:0;width:600px;height:400px";
  document.body.appendChild(host);
});

afterEach(() => {
  __resetTerminalStore();
  host.remove();
  document.body.replaceChildren();
  window.localStorage.clear();
});

/**
 * Poll `predicate` until it holds. The deadline is GENEROUS on purpose (the same reasoning as
 * `vitest.setup.ts`'s `asyncUtilTimeout`): the browser project runs beside the jsdom one, a starved
 * worker can spend most of a wait unscheduled, and every predicate here returns the instant it is
 * true — so a wider deadline costs a green run nothing and only stops a red one from blaming the
 * clock. Focus restoration, in particular, waits on a Motion exit that is frame-driven.
 */
async function waitFor(predicate: () => boolean, timeoutMs = 15_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  return predicate();
}

/** The palette's query field, or `null` while the palette is closed. */
function paletteInput(): HTMLInputElement | null {
  return document.querySelector<HTMLInputElement>("input[role='combobox']");
}

/** xterm's helper textarea — the element that actually holds the keyboard in a terminal. */
function xtermTextarea(): HTMLTextAreaElement {
  return document.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement;
}

/** The rows currently drawn, in DOM order. */
function rows(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>("[role='option']")];
}

/** Mount ONE live terminal under the palette, inside a real router. */
function mountShell() {
  const rootRoute = createRootRoute({
    component: () => (
      <ActiveProjectProvider>
        <SearchPalette />
        <Outlet />
      </ActiveProjectProvider>
    ),
  });
  const mk = (path: string, text: string) =>
    createRoute({
      getParentRoute: () => rootRoute,
      path,
      component: () => (
        <div className="h-full">
          <div>{text}</div>
          <div className="h-72">
            <Terminal recordId="term-1" />
          </div>
        </div>
      ),
    });
  const routeTree = rootRoute.addChildren([mk("/", "TERMINALS"), mk("/work/$specId", "SPEC")]);
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  render(<RouterProvider router={router} />, { container: host });
  return router;
}

/**
 * Mount, wait for a LIVE terminal (xterm attached AND its PTY spawned), and give it the keyboard —
 * the state the chord is opened from.
 *
 * The warm-up keystroke is not decoration. `pty_spawn` resolves asynchronously and a keystroke
 * typed before it lands is DROPPED, so a spec that counts bytes has to prove the pipe is open
 * before it counts anything — otherwise a slow spawn (this suite runs beside the jsdom one) reads
 * as a missing byte and the failure names the wrong culprit.
 */
async function mountWithLiveTerminal() {
  const router = mountShell();
  expect(await waitFor(() => xtermTextarea() != null), "the xterm mounted").toBe(true);
  xtermTextarea().focus();
  expect(await waitFor(() => document.activeElement === xtermTextarea())).toBe(true);
  await userEvent.keyboard("x");
  expect(await waitFor(() => ptyWrites === "x"), "the terminal is live").toBe(true);
  return router;
}

it("opens from a FOCUSED TERMINAL, and writes NOT ONE BYTE to the PTY", async () => {
  // The warm-up above already proved this terminal writes what it is given — so the emptiness
  // asserted after the chord means something.
  await mountWithLiveTerminal();

  await userEvent.keyboard("{Control>}k{/Control}");
  expect(await waitFor(() => paletteInput() != null), "the palette opened").toBe(true);
  // xterm YIELDED the chord: no `\x0b` (Ctrl+K's control byte), nothing at all beyond the `x`.
  expect(ptyWrites).toBe("x");
  // …and the keyboard moved into the query field.
  expect(await waitFor(() => document.activeElement === paletteInput())).toBe(true);
});

it("walks the suggestions with ↓ ↑ and opens one with ↵, without a mouse", async () => {
  const router = await mountWithLiveTerminal();
  await userEvent.keyboard("{Control>}k{/Control}");
  expect(await waitFor(() => rows().length === 2), "the suggestions rendered").toBe(true);
  // Nothing is preselected while there is no query — `↓` is what arms the list.
  expect(rows().every((r) => r.getAttribute("aria-selected") === "false")).toBe(true);

  await userEvent.keyboard("{ArrowDown}");
  expect(await waitFor(() => rows()[0]?.getAttribute("aria-selected") === "true")).toBe(true);
  await userEvent.keyboard("{ArrowDown}");
  expect(await waitFor(() => rows()[1]?.getAttribute("aria-selected") === "true")).toBe(true);
  await userEvent.keyboard("{ArrowUp}");
  expect(await waitFor(() => rows()[0]?.getAttribute("aria-selected") === "true")).toBe(true);

  await userEvent.keyboard("{Enter}");
  expect(
    await waitFor(() => router.state.location.pathname === "/work/s1"),
    "↵ opened the active Spec",
  ).toBe(true);
});

it("gives the keyboard BACK to the terminal on Escape", async () => {
  await mountWithLiveTerminal();
  await userEvent.keyboard("{Control>}k{/Control}");
  expect(await waitFor(() => document.activeElement === paletteInput())).toBe(true);

  await userEvent.keyboard("{Escape}");
  expect(await waitFor(() => paletteInput() == null), "the palette closed").toBe(true);
  expect(
    await waitFor(() => document.activeElement === xtermTextarea()),
    "the terminal has the keyboard again",
  ).toBe(true);
  // And it is really usable again, not merely focused.
  await userEvent.keyboard("y");
  expect(await waitFor(() => ptyWrites.endsWith("y"))).toBe(true);
});
