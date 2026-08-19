import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

// The open panel header renders `<OverlayPanelActions>`, which calls `useNavigate`; this suite mounts
// `<SpecOverlay>` with no router.
vi.mock("@tanstack/react-router", async (importActual) => ({
  ...(await importActual<typeof import("@tanstack/react-router")>()),
  useNavigate: () => () => Promise.resolve(),
}));

import {
  OVERLAY_PANEL_MAX_WIDTH,
  OVERLAY_PANEL_MIN_WIDTH,
  OVERLAY_PANEL_PANE_RATIO,
  overlayPanelWidth,
} from "./overlay-motion";
import { SpecOverlay } from "./spec-overlay";
import { ACTIVE_SESSION, detail, spec } from "./test-fixtures";

/**
 * The OPEN panel's BOUNDED width (review finding `01M06QRFPMK670J95Z8TYM2M80`): a fixed 380px panel
 * hid 79% of the terminal on a ~480px pane. The panel stays SUPERPOSED — the terminal is never
 * reflowed, refitted or SIGWINCH'd — and only its width is now bounded on the HOST PANE:
 * `min(380, max(45% of pane, 260))`.
 *
 * Two levels of proof: the pure resolver (below), then the resolver actually WIRED to the measured
 * pane, read off the DOM through the shell's frozen inner box.
 */

/**
 * A pane-shaped render container whose measured width is `width` px.
 *
 * NO file-level `afterEach` tears these down, deliberately (`vitest.setup.ts` explains the trap):
 * vitest runs `afterEach` hooks LAST-REGISTERED-FIRST, so a hook declared HERE would run BEFORE the
 * global one — i.e. detach the container, and the React tree still mounted and alive inside it,
 * before `cleanup()` ever unmounts it. The effects of `useOverlayPanelDisclosure` (a document-level
 * mousedown listener) and `useOverlayPaneWidth` (a `ResizeObserver` on the pane) would then be torn
 * down from an already-detached node. It is also unnecessary: RTL's global `cleanup()` unmounts and
 * REMOVES any container that is a direct child of `document.body` — which every container below is —
 * on top of the explicit `container.remove()` the `it.each` loop does between its own iterations.
 */
function paneContainer(width: number): HTMLElement {
  const container = document.createElement("div");
  document.body.appendChild(container);
  // jsdom has no layout: the overlay measures its host pane with `getBoundingClientRect`, so THIS is
  // where the pane width comes from. (`offsetParent` is null in jsdom, so the overlay falls back to
  // the parent element — this container.)
  container.getBoundingClientRect = () =>
    ({ width, height: 600, top: 0, left: 0, right: width, bottom: 600, x: 0, y: 0 }) as DOMRect;
  return container;
}

/** Serve a single draft ⇒ the tiroir; a single dev Spec ⇒ the rail. */
function serve(status: "draft" | "in_progress") {
  const s = spec({ id: "s1", number: 1, title: "Width spec", status });
  const commands: string[] = [];
  mockIPC((cmd) => {
    commands.push(cmd);
    if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [s] };
    if (cmd === "spec_detail") return detail(s, []);
    // `<OverlayPrototypes>` asks for the Spec's prototype pages; `null` = no prototype ⇒ it renders
    // nothing (a valid state, not an error).
    if (cmd === "proto_tree") return null;
    throw new Error(`unexpected ${cmd}`);
  });
  return commands;
}

/** The shell's inner box — the one FROZEN at the resolved pixel width. */
function innerEl(): HTMLElement {
  return document.querySelector("[data-overlay-panel-inner]") as HTMLElement;
}

describe("overlayPanelWidth — the bounded rule, as a pure resolver", () => {
  it("floors at 260px, tracks 45% of the pane, and never exceeds 380px", () => {
    // The three measurements of the finding.
    expect(overlayPanelWidth(480)).toBe(260);
    expect(overlayPanelWidth(760)).toBe(342);
    expect(overlayPanelWidth(845)).toBe(380);
    // BETWEEN the two knees the width IS the pane's share — anchored on the constant, not on the
    // literals above. 342/380 alone would stay green if the slope moved and those two points
    // happened to be re-tuned with it, and `OVERLAY_PANEL_PANE_RATIO` is the one bound of the rule
    // with no other consumer, so this is also what keeps its export honest.
    for (const pane of [600, 700, 760, 800, 844]) {
      expect(overlayPanelWidth(pane), `pane ${pane}px`).toBe(
        Math.round(pane * OVERLAY_PANEL_PANE_RATIO),
      );
    }
    // Above the knee it stays capped; below it, floored.
    expect(overlayPanelWidth(1920)).toBe(OVERLAY_PANEL_MAX_WIDTH);
    expect(overlayPanelWidth(320)).toBe(OVERLAY_PANEL_MIN_WIDTH);
    // An unmeasured pane (0) still yields a readable panel, never a sliver.
    expect(overlayPanelWidth(0)).toBe(OVERLAY_PANEL_MIN_WIDTH);
    // The ceiling holds for every pane width.
    for (let pane = 0; pane <= 4000; pane += 37) {
      expect(overlayPanelWidth(pane)).toBeLessThanOrEqual(OVERLAY_PANEL_MAX_WIDTH);
      expect(overlayPanelWidth(pane)).toBeGreaterThanOrEqual(OVERLAY_PANEL_MIN_WIDTH);
    }
  });
});

describe("the overlays resolve that width from the MEASURED host pane", () => {
  it.each([
    ["drawer", "draft", /open width spec live spec/i] as const,
    ["rail", "in_progress", /open width spec progress/i] as const,
  ])(
    "%s: pane 480 ⇒ 260px, pane 760 ⇒ 342px, pane 845 ⇒ 380px",
    async (_subject, status, tabName) => {
      for (const [pane, expected] of [
        [480, 260],
        [760, 342],
        [845, 380],
        [1440, 380], // beyond the knee the cap holds
      ] as const) {
        serve(status);
        const container = paneContainer(pane);
        const { unmount } = render(<SpecOverlay terminalId="term-1" />, { container });

        const tab = await screen.findByRole("button", { name: tabName });
        fireEvent.click(tab);
        await waitFor(() => expect(document.querySelector("[data-overlay-panel]")).not.toBeNull());

        expect(innerEl().style.width, `pane ${pane}px`).toBe(`${expected}px`);
        unmount();
        container.remove();
      }
    },
  );

  it("recomputes the width when the PANE is resized (not just the window)", async () => {
    // jsdom stubs `ResizeObserver` to a no-op, so drive a controllable one: the pane can be resized
    // without the window ever changing (a collapsed sidebar, a split), which is exactly why the
    // overlay observes the pane instead of listening for window resizes.
    let notify: (() => void) | null = null;
    class ControllableResizeObserver {
      constructor(callback: () => void) {
        notify = callback;
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    vi.stubGlobal("ResizeObserver", ControllableResizeObserver);
    try {
      serve("draft");
      let paneWidth = 1440;
      const container = document.createElement("div");
      document.body.appendChild(container);
      container.getBoundingClientRect = () =>
        ({
          width: paneWidth,
          height: 600,
          top: 0,
          left: 0,
          right: paneWidth,
          bottom: 600,
        }) as DOMRect;
      render(<SpecOverlay terminalId="term-1" />, { container });

      fireEvent.click(await screen.findByRole("button", { name: /open width spec live spec/i }));
      await waitFor(() => expect(document.querySelector("[data-overlay-panel]")).not.toBeNull());
      expect(innerEl().style.width).toBe("380px");

      // The pane shrinks under the panel: the bound is re-applied without a remount.
      paneWidth = 520;
      expect(notify).not.toBeNull();
      (notify as unknown as () => void)();
      await waitFor(() => expect(innerEl().style.width).toBe("260px"));
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("keeps the panel SUPERPOSED — clipped shell, frozen inner, and no terminal resize command", async () => {
    const commands = serve("draft");
    const container = paneContainer(760);
    render(<SpecOverlay terminalId="term-1" />, { container });

    const tab = await screen.findByRole("button", { name: /open width spec live spec/i });
    const root = container.querySelector("[data-overlay-surface]") as HTMLElement;
    // The overlay floats OVER the pane: it is absolutely positioned, so opening it cannot reflow the
    // terminal underneath.
    expect(root.className).toMatch(/\babsolute\b/);

    fireEvent.click(tab);
    await waitFor(() => expect(document.querySelector("[data-overlay-panel]")).not.toBeNull());

    // The shell clips a FIXED-width inner: that pair is what guarantees no horizontal overflow while
    // the width animates.
    const shell = document.querySelector("[data-overlay-panel-shell]") as HTMLElement;
    expect(shell.className).toMatch(/overflow-hidden/);
    expect(innerEl().style.width).toBe("342px");

    // Closing runs the reverse animation and unmounts the panel — still no pane resize.
    fireEvent.click(screen.getByRole("button", { name: /close live spec panel/i }));
    await waitFor(() => expect(document.querySelector("[data-overlay-panel]")).toBeNull());

    // No xterm refit / PTY SIGWINCH is triggered by opening or closing: the overlay only ever reads
    // Spec data, and never touches the terminal's pty.
    expect(commands.filter((cmd) => /resize|pty/.test(cmd))).toEqual([]);
  });
});
