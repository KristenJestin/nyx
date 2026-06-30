import { render } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { page } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

import { ChromeBar } from "./chrome-bar";
import { AppSidebar } from "@/components/sidebar/app-sidebar";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";

/**
 * BROWSER-MODE chrome + sidebar test (real Chromium via Playwright, headless).
 *
 * Two things only a real browser can show:
 *   1. the Motion animations on the CHROME (the sidebar rows are `motion.li`
 *      inside `<AnimatePresence>`; Motion applies inline transform/opacity styles
 *      as it animates) are actually present — in jsdom Motion no-ops its visual
 *      side effects, so "Motion is wired" is only observable in a painting
 *      browser;
 *   2. a visual-regression BASELINE of the chrome bar + sidebar via
 *      `toMatchScreenshot`, so a layout/flash/styling regression of the app
 *      CHROME (never the xterm viewport) surfaces as a screenshot diff.
 *
 * Per project rule, animations live on the chrome ONLY — never the xterm
 * viewport — so we screenshot the chrome + sidebar, not a terminal pane.
 */

beforeEach(() => {
  // The chrome bar reads `window_controls_visible`; the sidebar reads
  // `terminal_info` for auto-labels. Resolve both so nothing throws.
  mockIPC(
    (cmd) => {
      if (cmd === "window_controls_visible") return true;
      if (cmd === "terminal_info") return { cwd: null, foreground: null };
      return null;
    },
    { shouldMockEvents: true },
  );
});

afterEach(() => {
  document.body.replaceChildren();
});

function record(id: number, cwd: string): TerminalRecord {
  return {
    id: String(id),
    cwd,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: id,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    // No workspace → these render in the loose TERMINALS section of <AppSidebar>,
    // which is the same `Reorder` row motion (`ReorderTerminalItem`) we screenshot.
    workspace_id: null,
  };
}

const TERMINALS = [
  record(1, "/home/kris/work/api"),
  record(2, "/home/kris/work/web"),
  record(3, "/home/kris/work/docs"),
];

function noop() {}

function mountChrome(): HTMLElement {
  const host = document.createElement("div");
  host.setAttribute("data-testid", "chrome-host");
  // A fixed, app-shaped frame so the screenshot is stable across runs: thin
  // chrome bar on top, sidebar on the left.
  host.style.width = "420px";
  host.style.height = "320px";
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  host.style.display = "flex";
  host.style.flexDirection = "column";
  host.style.overflow = "hidden";
  document.body.appendChild(host);

  render(
    <div className="flex h-full w-full flex-col bg-background">
      <ChromeBar title="api" controlsVisible />
      <div className="flex min-h-0 flex-1">
        {/* The WHOLE app sidebar (renamed from ProjectSidebar). No projects → the
            terminals render in the loose TERMINALS section as `Reorder` rows, so
            this still screenshots the chrome + the motion-animated row list. */}
        <AppSidebar
          projects={[]}
          terminals={TERMINALS}
          activeId={"1"}
          onSelect={noop}
          onClose={noop}
          onNewTerminal={noop}
          onNewLooseTerminal={noop}
          onAddProject={noop}
          onAddWorkspace={noop}
        />
      </div>
    </div>,
    { container: host },
  );
  return host;
}

async function waitFor(pred: () => boolean, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return pred();
}

// The TERMINAL rows are the `<li>`s that carry a per-row "Close terminal"
// button (the loose TERMINALS rows). We scope to those so the empty-state
// "No projects yet" <li> (rendered by <AppSidebar> when projects=[]) is not
// counted as a terminal row.
const terminalRows = (host: HTMLElement) =>
  Array.from(host.querySelectorAll<HTMLLIElement>("li")).filter((li) =>
    li.querySelector('[aria-label^="Close terminal"]'),
  );

it("renders Motion-animated sidebar rows (chrome animations actually present)", async () => {
  const host = mountChrome();

  // One drag-sortable row per terminal.
  const rows = await waitFor(() => terminalRows(host).length === TERMINALS.length);
  expect(rows, "one animated row per terminal").toBe(true);

  const items = terminalRows(host);
  expect(items.length).toBe(TERMINALS.length);

  // dnd-kit owns the outer <li>; Motion owns the inner wrapper so drag transforms
  // and open/close height animation do not fight each other. In a real browser
  // Motion writes inline styles on that inner wrapper (jsdom no-ops them). Assert
  // the wrapper carries a Motion-managed style — the observable proof the chrome
  // still animates the rows.
  for (const li of items) {
    const motionWrapper = li.firstElementChild;
    expect(motionWrapper, "a sortable row must have a Motion wrapper").not.toBeNull();
    const inline = motionWrapper?.getAttribute("style") ?? "";
    expect(
      /opacity|transform|height|will-change/.test(inline),
      `a Motion-animated sidebar row wrapper must carry an inline animated style, ` +
        `got style="${inline}"`,
    ).toBe(true);
  }
});

it("matches the chrome + sidebar visual baseline (toMatchScreenshot)", async () => {
  mountChrome();

  // Wait for the terminal rows to be present + Motion to settle to its resting
  // state so the baseline is taken against the final layout (not mid-spring).
  const host = document.querySelector<HTMLElement>('[data-testid="chrome-host"]')!;
  await waitFor(() => terminalRows(host).length === TERMINALS.length);
  // Fonts must be loaded before the screenshot or the baseline is unstable.
  await document.fonts.ready;
  // Let the enter spring settle.
  await new Promise((r) => setTimeout(r, 400));

  // First run writes the baseline into the GITIGNORED root .vitest-screenshots/
  // dir (see vitest.config.ts resolveScreenshotPath); later runs diff against
  // it. A chrome layout/styling regression makes the diff fail. The baseline is
  // NOT committed (CLAUDE.md §4) — it regenerates locally on first run.
  await expect(page.getByTestId("chrome-host")).toMatchScreenshot("chrome-sidebar");
});
