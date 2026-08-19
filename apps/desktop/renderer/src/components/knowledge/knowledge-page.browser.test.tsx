// The REAL app CSS must be loaded so the Idées layout paints with its true Tailwind
// utilities (grid tracks, `min-w-[clamp(...)]`, `overflow-*`, `h-app`). In browser mode
// only CSS a spec imports is applied (see `terminal.browser.test.tsx` importing xterm.css);
// `vitest.config.ts` wires `@tailwindcss/vite` so this `@import "tailwindcss"` truly compiles.
import "@/globals.css";

import { render } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { page } from "vitest/browser";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { createAppRouter } from "@/router";

import type { IdeaListItem, IdeaThemeGroup } from "./types";

/**
 * BROWSER-MODE horizontal-overflow guard for the Idées view (real Chromium via Playwright).
 *
 * The user-confirmed bug: a horizontal scrollbar scrolls the WHOLE window INCLUDING the left
 * rail — i.e. the overflow is at the DOCUMENT (`html`/`body`/`#root`) level, not the page. It
 * became PERMANENT once the drawer became a CSS-grid column whose content stays MOUNTED
 * (`lastShown`, inner `min-w-[clamp(30rem,45vw,42rem)]`) even when the drawer is CLOSED.
 *
 * Reasoning about the overflow chain failed four times, so this proves it with a REAL browser:
 * we render the ACTUAL shell (rail + `<Outlet/>` + KnowledgePage) at a fixed viewport width and
 * assert, in EVERY drawer state, that the document itself never gains a horizontal scroll
 * (`documentElement.scrollWidth <= clientWidth`). It walks the DOM to name the offending element
 * so the fix targets the measured source, not a guess.
 */

// The mockable bridge backend. In BROWSER mode a `vi.mock` declared in an IMPORTED helper
// (the shared `@/bridge/test-harness`) is NOT hoisted into the test's module graph, so the app
// falls back to a bare `FakeNyxBridge` with no stubs. We therefore mock `@/bridge` IN-FILE (the
// only reliably-hoisted form) and route every `invoke` through this mutable handler, which the
// test installs per run.
const backend = vi.hoisted(() => ({
  handler: (_cmd: string, _args?: Record<string, unknown>): unknown => undefined,
}));

vi.mock("@/bridge", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/bridge")>();
  const { FakeNyxBridge } = await import("@/bridge/fake");
  const fake = new FakeNyxBridge();
  fake.onAnyCommand((cmd, args) => backend.handler(cmd, args));
  return { ...actual, nyxBridge: fake, default: fake };
});

/** An `idea_list` row fixture (enriched with linked Specs). */
function idea(overrides: Partial<IdeaListItem>): IdeaListItem {
  return {
    id: "i0",
    project_id: "p1",
    title: "Untitled idea",
    body: "",
    status: "open",
    theme: "RBAC",
    source: "manual",
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    closed_reason: null,
    linked_specs: [],
    ...overrides,
  };
}

const THEMES: IdeaThemeGroup[] = [
  { theme: "RBAC", total: 3, open: 2, planned: 1, implemented: 0, closed: 0, dropped: 0 },
  { theme: "Mail", total: 1, open: 1, planned: 0, implemented: 0, closed: 0, dropped: 0 },
];

const IDEAS: IdeaListItem[] = [
  idea({
    id: "i1",
    title:
      "Unify the public-route whitelist behind a single decorator so a controller opts in once",
    body: "The whitelist currently duplicates the public routes; a decorator would centralise it.",
    status: "open",
    theme: "RBAC",
    source: "review",
    linked_specs: [
      {
        spec_id: "s1",
        kind: "addresses",
        title: "RBAC phase 2 — public decorator",
        status: "in_review",
        number: 2,
      },
    ],
  }),
  idea({
    id: "i2",
    title: "Narrow the OT writes to the smallest authorised scope",
    status: "planned",
    theme: "RBAC",
  }),
  idea({
    id: "i3",
    title: "Migrate Brevo transactional mail to Microsoft 365",
    status: "open",
    theme: "Mail",
  }),
];

const PROJECT = {
  id: "p1",
  name: "Project",
  key: "NYX",
  collapsed: false,
  created_at: 0,
  updated_at: 0,
  resume_agent_sessions: false,
};

/** Route every backend command so the full shell + Idées view mount cleanly with a project entered. */
function installBackend(): void {
  backend.handler = (cmd, args) => {
    switch (cmd) {
      case "list_projects":
        return [PROJECT];
      case "list_workspaces":
      case "list_terminals":
        return [];
      case "idea_themes":
        return THEMES;
      case "idea_list":
        return IDEAS;
      case "idea_search": {
        const q = String((args as { query?: string })?.query ?? "").toLowerCase();
        return IDEAS.filter((i) => i.title.toLowerCase().includes(q)).map((i) => ({ id: i.id }));
      }
      default:
        return undefined;
    }
  };
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function waitFor(pred: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await sleep(25);
  }
  return pred();
}

/** The grid split element (the list↔drawer split whose columns animate). */
function gridSplit(): HTMLElement | null {
  return (
    Array.from(document.querySelectorAll<HTMLElement>("div")).find((d) =>
      (d.getAttribute("class") ?? "").includes("transition-[grid-template-columns]"),
    ) ?? null
  );
}

const drawerClosed = () =>
  (gridSplit()?.getAttribute("class") ?? "").includes("grid-cols-[minmax(0,1fr)_0rem]");

/**
 * Walk the whole tree and collect every element that itself overflows horizontally
 * (`scrollWidth > clientWidth`) OR whose right edge escapes the viewport — the diagnostic that
 * NAMES the offending node(s) instead of guessing.
 */
function overflowingElements(): string[] {
  const out: string[] = [];
  const vw = window.innerWidth;
  for (const el of Array.from(document.querySelectorAll<HTMLElement>("*"))) {
    const overflowsSelf = el.scrollWidth - el.clientWidth > 1;
    const rect = el.getBoundingClientRect();
    const escapesViewport = rect.right - vw > 1;
    if (overflowsSelf || escapesViewport) {
      const cls = (el.getAttribute("class") ?? "").slice(0, 140);
      out.push(
        `${el.tagName.toLowerCase()} sw=${el.scrollWidth} cw=${el.clientWidth} ` +
          `right=${Math.round(rect.right)} vw=${vw} class="${cls}"`,
      );
    }
  }
  return out;
}

/** Log + return the document's horizontal metrics for a labelled state. */
function reportOverflow(label: string): { scrollWidth: number; clientWidth: number } {
  const el = document.documentElement;
  const d = { scrollWidth: el.scrollWidth, clientWidth: el.clientWidth };

  console.log(
    `[overflow:${label}] documentElement scrollWidth=${d.scrollWidth} clientWidth=${d.clientWidth} innerWidth=${window.innerWidth}`,
  );
  for (const line of overflowingElements()) console.log(`[overflow:${label}]   ${line}`);
  return d;
}

beforeEach(() => {
  localStorage.clear();
  installBackend();
});

afterEach(() => {
  document.body.replaceChildren();
  localStorage.clear();
});

it("never lets the whole document scroll horizontally — drawer closed → open → closed", async () => {
  // A fixed, real viewport width in the band where the drawer's `45vw` clamp is live
  // (0.45·1280 = 576px, between 30rem and 42rem) — the width the user reported the rail scroll at.
  await page.viewport(1280, 800);
  render(<RouterProvider router={createAppRouter(["/knowledge"])} />);

  // (a) Drawer CLOSED — the initial load (rail + list, no idea selected yet).
  const rowFound = await waitFor(
    () => document.querySelector('[data-testid="idea-row-i1"]') != null,
  );
  expect(rowFound, "the seeded ideas must render (backend wired, project entered)").toBe(true);
  await document.fonts.ready;
  await sleep(80);
  const closedInitial = reportOverflow("closed-initial");
  expect(
    closedInitial.scrollWidth,
    "document must not scroll horizontally with the drawer CLOSED (initial)",
  ).toBeLessThanOrEqual(closedInitial.clientWidth);

  // (b) Drawer OPEN — select an idea; wait for the aside + the grid-column transition to settle.
  (document.querySelector('[data-testid="idea-row-i1"]') as HTMLElement).click();
  await waitFor(() => document.querySelector('[aria-label="Idea detail"]') != null);
  await sleep(400);
  const open = reportOverflow("open");
  expect(
    open.scrollWidth,
    "document must not scroll horizontally with the drawer OPEN",
  ).toBeLessThanOrEqual(open.clientWidth);

  // (c) Drawer CLOSED again — the content stays MOUNTED (`lastShown`) behind the 0-width track,
  // which is exactly the state the reported overflow became PERMANENT in.
  (document.querySelector('[aria-label="Close idea detail"]') as HTMLElement).click();
  await waitFor(() => drawerClosed());
  await sleep(400);
  const closedAgain = reportOverflow("closed-reopened");
  expect(
    closedAgain.scrollWidth,
    "document must not scroll horizontally with the drawer CLOSED after having been open",
  ).toBeLessThanOrEqual(closedAgain.clientWidth);
});
