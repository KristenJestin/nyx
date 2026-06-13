import { render } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { page } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

import { ChromeBar } from "./chrome-bar";
import { Sidebar } from "@/components/sidebar/sidebar";
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
        <Sidebar
          terminals={TERMINALS}
          activeId={"1"}
          onSelect={noop}
          onClose={noop}
          onCreate={noop}
          onReorder={noop}
          onRename={noop}
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

it("renders Motion-animated sidebar rows (chrome animations actually present)", async () => {
  const host = mountChrome();

  // One <li> per terminal — these are `motion.li` rows inside <AnimatePresence>.
  const rows = await waitFor(
    () => host.querySelectorAll("li").length === TERMINALS.length,
  );
  expect(rows, "one animated row per terminal").toBe(true);

  const items = host.querySelectorAll<HTMLLIElement>("li");
  expect(items.length).toBe(TERMINALS.length);

  // Motion drives these rows: as it animates the enter (opacity 0→1, height
  // 0→auto) it writes inline styles on the element. In a real browser those
  // inline styles are present (in jsdom Motion no-ops them). We assert each row
  // carries a Motion-managed inline style — the observable proof the animation
  // is wired on the chrome (and not a static, un-animated list).
  for (const li of items) {
    const inline = li.getAttribute("style") ?? "";
    expect(
      /opacity|transform|height|will-change/.test(inline),
      `a Motion-animated sidebar row must carry an inline animated style, ` +
        `got style="${inline}"`,
    ).toBe(true);
  }
});

it("matches the chrome + sidebar visual baseline (toMatchScreenshot)", async () => {
  mountChrome();

  // Wait for the rows to be present + Motion to settle to its resting state so
  // the baseline is taken against the final layout (not mid-spring).
  await waitFor(
    () =>
      document.querySelectorAll('[data-testid="chrome-host"] li').length ===
      TERMINALS.length,
  );
  // Fonts must be loaded before the screenshot or the baseline is unstable.
  await document.fonts.ready;
  // Let the enter spring settle.
  await new Promise((r) => setTimeout(r, 400));

  // First run writes the baseline into the GITIGNORED root .vitest-screenshots/
  // dir (see vitest.config.ts resolveScreenshotPath); later runs diff against
  // it. A chrome layout/styling regression makes the diff fail. The baseline is
  // NOT committed (CLAUDE.md §4) — it regenerates locally on first run.
  await expect(page.getByTestId("chrome-host")).toMatchScreenshot(
    "chrome-sidebar",
  );
});
