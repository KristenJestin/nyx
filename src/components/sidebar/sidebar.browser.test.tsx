import { render } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { StrictMode } from "react";
import { afterEach, beforeEach, expect, it } from "vitest";

import { Sidebar } from "./sidebar";
import type { TerminalRecord } from "./use-terminals";

/**
 * BROWSER-MODE smoke test for the terminal close/add path under StrictMode, in
 * real Chromium (Playwright). It mounts the motion `Reorder` list, closes the
 * middle row, adds one, then closes the active row, asserting the list settles
 * to the right rows each time without throwing.
 *
 * History note: an earlier "nyx (Not Responding)" freeze on close looked like a
 * frontend animation bug but was actually a Windows ConPTY teardown DEADLOCK in
 * the Rust backend (`Pty::drop` joining a reader thread that never unblocked) —
 * see `src-tauri/src/pty.rs`. It was frontend-INDEPENDENT, so no frontend test
 * could have caught it. This test guards the React/StrictMode render path of the
 * Reorder list (gross regressions, infinite render loops); the backend deadlock
 * is guarded by the timeout-based Rust tests.
 */

beforeEach(() => {
  mockIPC(
    (cmd) => {
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

function noop() {}

function sidebar(terminals: TerminalRecord[], activeId: string) {
  return (
    <StrictMode>
      <Sidebar
        terminals={terminals}
        activeId={activeId}
        onSelect={noop}
        onClose={noop}
        onCreate={noop}
        onReorder={noop}
        onRename={noop}
      />
    </StrictMode>
  );
}

async function waitForRows(host: HTMLElement, count: number, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (host.querySelectorAll("li").length === count) return true;
    await new Promise((r) => setTimeout(r, 25));
  }
  return host.querySelectorAll("li").length === count;
}

it("renders + closes/adds rows under StrictMode without throwing (Chromium smoke)", async () => {
  const host = document.createElement("div");
  host.style.width = "224px";
  host.style.height = "320px";
  document.body.appendChild(host);

  const t = [
    record(1, "/home/kris/work/api"),
    record(2, "/home/kris/work/web"),
    record(3, "/home/kris/work/docs"),
    record(4, "/home/kris/work/cli"),
  ];
  const { rerender } = render(sidebar([t[0], t[1], t[2]], "1"), {
    container: host,
  });
  expect(await waitForRows(host, 3), "three rows mount").toBe(true);

  // Close the MIDDLE terminal (the index-shift case), then add, then close the
  // ACTIVE row — the list should settle to the right count each time.
  rerender(sidebar([t[0], t[2]], "1"));
  expect(await waitForRows(host, 2), "middle row closes").toBe(true);

  rerender(sidebar([t[0], t[2], t[3]], "1"));
  expect(await waitForRows(host, 3), "added row appears").toBe(true);

  rerender(sidebar([t[2], t[3]], "3"));
  expect(await waitForRows(host, 2), "active row closes").toBe(true);
});
