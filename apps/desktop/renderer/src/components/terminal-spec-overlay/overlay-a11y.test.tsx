import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

// The open panel headers render `<OverlayPanelActions>`, which calls `useNavigate`. This suite mounts
// `<SpecOverlay>` with NO router, so stub `useNavigate` to a no-op (navigation targets are asserted
// in `overlay-panel-actions.test.tsx`); all other router exports stay real.
vi.mock("@tanstack/react-router", async (importActual) => ({
  ...(await importActual<typeof import("@tanstack/react-router")>()),
  useNavigate: () => () => Promise.resolve(),
}));

import type { Spec } from "../specs/types";
import { SpecOverlay } from "./spec-overlay";
import { ACTIVE_SESSION, detail, spec, task } from "./test-fixtures";

/**
 * ACCESSIBILITY spec of the terminal↔Spec overlay chrome — the screen-reader + keyboard contract the
 * human review (`01M06QJR3YMJFWAJSTW332Z44Y`) measured as missing. It covers BOTH overlays (rail A
 * and tiroir B) because the chrome is shared, and it is kept apart from `spec-overlay.test.tsx`
 * (which pins the visual/motion model) so an a11y regression names itself.
 *
 * The three decided properties it pins:
 *  - DISCLOSURE, not toggle: `aria-expanded` + `aria-controls`, never `aria-pressed` (F1).
 *  - NON-MODAL dialog: `role="dialog"` + a name, focus moved INTO the panel and RESTORED to the
 *    languette, but NO `aria-modal` and NO focus trap — the terminal stays keyboard-usable (F2).
 *  - Escape belongs to the TERMINAL: it dismisses only when focus is INSIDE the panel (F3).
 */

/** Serve one dev Spec ⇒ rail A. Returns the fixture. */
function serveRail(): Spec {
  const devSpec = spec({ id: "s1", number: 7, title: "Auth revamp", status: "in_progress" });
  mockIPC((cmd) => {
    if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [devSpec] };
    if (cmd === "spec_detail") return detail(devSpec, [task({ id: "t1", title: "Wire it" })]);
    throw new Error(`unexpected ${cmd}`);
  });
  return devSpec;
}

/**
 * Serve one dev Spec ⇒ rail A, with `spec_detail` NEVER resolving — the rail parked in its LOADING
 * state (the languette shows its spinner and the panel cannot mount). Returns the fixture.
 */
function serveRailLoadingForever(): Spec {
  const devSpec = spec({ id: "s1", number: 7, title: "Auth revamp", status: "in_progress" });
  mockIPC((cmd) => {
    if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [devSpec] };
    if (cmd === "spec_detail") return new Promise<never>(() => {});
    throw new Error(`unexpected ${cmd}`);
  });
  return devSpec;
}

/** Serve `count` draft Specs ⇒ tiroir B (a stack when `count > 1`). */
function serveDrawer(count = 1): Spec[] {
  const drafts = Array.from({ length: count }, (_, i) =>
    spec({ id: `s${i + 1}`, number: i + 1, title: `Draft ${i + 1}`, status: "draft" }),
  );
  mockIPC((cmd, args) => {
    if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: drafts };
    if (cmd === "spec_detail") {
      return detail(drafts.find((d) => d.id === (args?.specId as string)) ?? drafts[0], []);
    }
    // The open panel's header carries the Run action, and the outside-click spec below opens the
    // REAL orchestration run modal through it. No run for this Spec ⇒ the modal's empty state, which
    // is all this suite needs: what it asserts is WHERE the modal is mounted, not what it shows.
    if (cmd === "engine_spec_runs") return [];
    throw new Error(`unexpected ${cmd}`);
  });
  return drafts;
}

/** The single mounted panel, or `null`. */
function panelEl(): HTMLElement | null {
  return document.querySelector<HTMLElement>("[data-overlay-panel]");
}

/** Wait until a panel is mounted, and return it. */
async function openPanel(): Promise<HTMLElement> {
  await waitFor(() => expect(panelEl()).not.toBeNull());
  return panelEl() as HTMLElement;
}

describe("overlay a11y — F1: the languette is a DISCLOSURE (aria-expanded + aria-controls)", () => {
  it("rail: flips aria-expanded and points aria-controls at the panel that opens", async () => {
    serveRail();
    render(<SpecOverlay terminalId="term-1" />);

    const tab = await screen.findByRole("button", { name: /open auth revamp progress/i });
    // CLOSED: expanded=false, and NO `aria-pressed` (a disclosure is not a two-state action button).
    expect(tab).toHaveAttribute("aria-expanded", "false");
    expect(tab).not.toHaveAttribute("aria-pressed");
    const controls = tab.getAttribute("aria-controls");
    expect(controls).toBeTruthy();
    // The controlled id is legitimately UNMOUNTED while closed (the panel lives under
    // `<AnimatePresence>`); the attribute stays put — that is the disclosure pattern.
    expect(document.getElementById(controls as string)).toBeNull();

    fireEvent.click(tab);
    const panel = await openPanel();
    expect(tab).toHaveAttribute("aria-expanded", "true");
    expect(document.getElementById(controls as string)).toBe(panel);
  });

  it("drawer: flips aria-expanded and points aria-controls at the panel that opens", async () => {
    serveDrawer();
    render(<SpecOverlay terminalId="term-1" />);

    const tab = await screen.findByRole("button", { name: /open draft 1 live spec/i });
    expect(tab).toHaveAttribute("aria-expanded", "false");
    expect(tab).not.toHaveAttribute("aria-pressed");
    const controls = tab.getAttribute("aria-controls");
    expect(controls).toBeTruthy();
    expect(document.getElementById(controls as string)).toBeNull();

    fireEvent.click(tab);
    const panel = await openPanel();
    expect(tab).toHaveAttribute("aria-expanded", "true");
    expect(document.getElementById(controls as string)).toBe(panel);
  });

  it("rail: NEVER announces expanded while the panel aria-controls names is unmounted", async () => {
    // The rail is the only overlay that can split "the user clicked" from "the panel is mounted":
    // its panel waits for `spec_detail`. Parked in that window (a detail that never resolves), the
    // languette must still read CLOSED — announcing "expanded" while `aria-controls` points at an id
    // nothing has mounted sends a screen reader looking for a region that does not exist.
    serveRailLoadingForever();
    render(<SpecOverlay terminalId="term-1" />);

    const tab = await screen.findByRole("button", { name: /open auth revamp progress/i });
    const controls = tab.getAttribute("aria-controls") as string;
    fireEvent.click(tab);

    // The click really landed in the LOADING window: the rail is showing its spinner and no panel is
    // mounted. Without this the invariant below would hold vacuously on a rail that never opened.
    expect(within(tab).getByRole("status", { name: /loading/i })).toBeInTheDocument();
    expect(panelEl()).toBeNull();

    // THE INVARIANT, sampled ACROSS the window (the state is stable — `spec_detail` never settles —
    // so a handful of macrotask-spaced samples covers it): expanded ⇒ the controlled panel exists.
    for (let i = 0; i < 5; i++) {
      await new Promise((resolve) => setTimeout(resolve, 10));
      expect(panelEl(), "the panel is still unmounted (detail never resolves)").toBeNull();
      expect(tab, `sample ${i}`).toHaveAttribute("aria-expanded", "false");
      expect(document.getElementById(controls)).toBeNull();
    }
  });

  it("drawer: each stacked languette controls ITS OWN panel id — only the open one is expanded", async () => {
    serveDrawer(3);
    render(<SpecOverlay terminalId="term-1" />);

    const first = await screen.findByRole("button", { name: /open draft 1 live spec/i });
    const second = screen.getByRole("button", { name: /open draft 2 live spec/i });
    const ids = [first, second].map((t) => t.getAttribute("aria-controls"));
    expect(new Set(ids).size).toBe(2); // deterministic AND per-Spec

    fireEvent.click(second);
    const panel = await openPanel();
    expect(second).toHaveAttribute("aria-expanded", "true");
    expect(first).toHaveAttribute("aria-expanded", "false");
    expect(document.getElementById(ids[1] as string)).toBe(panel);
    expect(document.getElementById(ids[0] as string)).toBeNull();
  });
});

describe("overlay a11y — F2: the open panel is a NON-MODAL dialog, announced and focused", () => {
  it("rail: the panel is a named dialog, focused on open, with NO aria-modal", async () => {
    serveRail();
    render(<SpecOverlay terminalId="term-1" />);

    const tab = await screen.findByRole("button", { name: /open auth revamp progress/i });
    fireEvent.click(tab);
    const panel = await openPanel();

    // The role + the NAME live on the SAME element as `data-overlay-panel` and the disclosure id.
    expect(panel).toHaveAttribute("role", "dialog");
    expect(panel).toHaveAccessibleName(/auth revamp progress/i);
    // NON-MODAL is DECIDED, not forgotten: no `aria-modal`, so the terminal underneath is not
    // announced as inert and stays reachable.
    expect(panel).not.toHaveAttribute("aria-modal");
    // Focus lands on the CONTAINER, not on the first command (which navigates away).
    await waitFor(() => expect(document.activeElement).toBe(panel));
    expect(panel).toHaveAttribute("tabindex", "-1");
  });

  it("drawer: the panel is a named dialog with NO nested named region left inside it", async () => {
    serveDrawer();
    render(<SpecOverlay terminalId="term-1" />);

    fireEvent.click(await screen.findByRole("button", { name: /open draft 1 live spec/i }));
    const panel = await openPanel();

    expect(panel).toHaveAttribute("role", "dialog");
    expect(panel).toHaveAccessibleName(/draft 1 live spec/i);
    expect(panel).not.toHaveAttribute("aria-modal");
    await waitFor(() => expect(document.activeElement).toBe(panel));

    // ONE named element for the panel: the `<section aria-label>` that used to wrap the live panel's
    // content is gone, so no second named region nests inside the dialog for the same content.
    expect(panel.querySelectorAll("[role='region']")).toHaveLength(0);
    expect(panel.querySelectorAll("section[aria-label]")).toHaveLength(0);
    expect(screen.queryAllByRole("dialog")).toHaveLength(1);
  });

  it.each([
    ["rail", serveRail, /open auth revamp progress/i] as const,
    ["drawer", () => serveDrawer(), /open draft 1 live spec/i] as const,
  ])(
    "%s: focuses the panel when the languette is operated from the KEYBOARD too",
    async (_subject, serve, tabName) => {
      serve();
      render(<SpecOverlay terminalId="term-1" />);

      // A keyboard activation of a <button> is "focus, then a click event raised by Enter/Space" —
      // jsdom does not synthesise that click from `keyDown`, so the two halves are driven explicitly.
      const tab = await screen.findByRole("button", { name: tabName });
      tab.focus();
      expect(document.activeElement).toBe(tab);
      fireEvent.click(tab);

      const panel = await openPanel();
      await waitFor(() => expect(document.activeElement).toBe(panel));
    },
  );

  it("rail: the close chevron dismisses and returns focus to the languette", async () => {
    serveRail();
    render(<SpecOverlay terminalId="term-1" />);

    const tab = await screen.findByRole("button", { name: /open auth revamp progress/i });
    fireEvent.click(tab);
    await openPanel();

    fireEvent.click(screen.getByRole("button", { name: /close progress panel/i }));
    await waitFor(() => expect(panelEl()).toBeNull());
    expect(document.activeElement).toBe(tab);
  });

  it("drawer: the close chevron AND a re-click on the languette both return focus to it", async () => {
    serveDrawer();
    render(<SpecOverlay terminalId="term-1" />);

    const tab = await screen.findByRole("button", { name: /open draft 1 live spec/i });
    fireEvent.click(tab);
    await openPanel();
    fireEvent.click(screen.getByRole("button", { name: /close live spec panel/i }));
    await waitFor(() => expect(panelEl()).toBeNull());
    expect(document.activeElement).toBe(tab);

    // Re-opening, then re-clicking the ACTIVE languette (it is the panel's close handle).
    fireEvent.click(tab);
    const panel = await openPanel();
    await waitFor(() => expect(document.activeElement).toBe(panel));
    fireEvent.click(tab);
    await waitFor(() => expect(panelEl()).toBeNull());
    expect(document.activeElement).toBe(tab);
  });

  it("does NOT trap focus — Tab from the last command is untouched and focus may leave the panel", async () => {
    serveDrawer();
    render(
      <>
        {/* A control OUTSIDE the panel, standing for the terminal's own focusables. */}
        <button type="button" data-testid="outside">
          outside
        </button>
        <SpecOverlay terminalId="term-1" />
      </>,
    );

    fireEvent.click(await screen.findByRole("button", { name: /open draft 1 live spec/i }));
    const panel = await openPanel();

    // The LAST command of the panel is its close chevron. Tab from there is NOT intercepted: the
    // overlay neither cancels the key nor redirects the focus back into the panel (no trap).
    const close = screen.getByRole("button", { name: /close live spec panel/i });
    close.focus();
    const notCancelled = fireEvent.keyDown(close, { key: "Tab" });
    expect(notCancelled).toBe(true); // `fireEvent` returns false only when preventDefault was called
    expect(document.activeElement).toBe(close);

    // Focus really can LEAVE the panel — and leaving does not dismiss it (non-modal: reading the
    // Spec while typing in the terminal is the point).
    const outside = screen.getByTestId("outside");
    outside.focus();
    expect(document.activeElement).toBe(outside);
    expect(panelEl()).toBe(panel);
  });
});

describe("overlay a11y — F3: Escape and outside clicks dismiss (and give focus back)", () => {
  it.each([
    ["rail", serveRail, /open auth revamp progress/i] as const,
    ["drawer", () => serveDrawer(), /open draft 1 live spec/i] as const,
  ])(
    "%s: Escape with focus INSIDE the panel dismisses it and restores focus",
    async (_subject, serve, tabName) => {
      serve();
      render(<SpecOverlay terminalId="term-1" />);

      const tab = await screen.findByRole("button", { name: tabName });
      fireEvent.click(tab);
      const panel = await openPanel();
      await waitFor(() => expect(document.activeElement).toBe(panel));

      fireEvent.keyDown(panel, { key: "Escape" });
      await waitFor(() => expect(panelEl()).toBeNull());
      expect(document.activeElement).toBe(tab);
    },
  );

  it.each([
    ["rail", serveRail, /open auth revamp progress/i] as const,
    ["drawer", () => serveDrawer(), /open draft 1 live spec/i] as const,
  ])(
    "%s: a click outside the panel and outside the languette dismisses it",
    async (_subject, serve, tabName) => {
      serve();
      render(
        <>
          {/* Stands for the terminal surface the overlay floats over — clicking back into it is the
            reflex gesture that must dismiss the panel. It is deliberately NOT focusable, so it is
            the DEAD-ZONE branch of the restoration (see the assertion below). The empty
            `pointer-events-none` bands of the languette column resolve to this same target in a real
            browser — `overlay-hit-testing.browser.test.tsx` measures that they hit the surface
            underneath, never the overlay — so they are the same case as this one. */}
          <div data-testid="terminal-area">terminal</div>
          <SpecOverlay terminalId="term-1" />
        </>,
      );

      const tab = await screen.findByRole("button", { name: tabName });
      fireEvent.click(tab);
      await openPanel();

      fireEvent.mouseDown(screen.getByTestId("terminal-area"));
      await waitFor(() => expect(panelEl()).toBeNull());
      // SCOPE OF THIS ASSERTION — it covers the DEAD-ZONE branch ONLY, and it can only ever cover
      // that one here: jsdom does not run the browser's default mousedown action, so nothing in this
      // environment ever gives the focus to the clicked target (nor throws it back to `body`). The
      // stand-in above being non-focusable, a real browser lands on exactly the same state, and the
      // restoration is deferred past the default action precisely so it does. The OTHER branch — a
      // FOCUSABLE target keeps the focus, the languette takes nothing — is unobservable in jsdom and
      // is measured for real in `overlay-focus-restore.browser.test.tsx`, which pins both.
      await waitFor(() => expect(document.activeElement).toBe(tab));
    },
  );

  it.each([
    ["rail", serveRail, /open auth revamp progress/i] as const,
    ["drawer", () => serveDrawer(), /open draft 1 live spec/i] as const,
  ])(
    "%s: a click OUTSIDE THE HOST PANE (sidebar, window chrome) dismisses it too",
    async (_subject, serve, tabName) => {
      serve();
      render(<SpecOverlay terminalId="term-1" />);

      const tab = await screen.findByRole("button", { name: tabName });
      fireEvent.click(tab);
      await openPanel();

      // Appended to `document.body`, i.e. OUTSIDE the render container the overlay resolves as its
      // host pane — the sidebar and the window chrome sit there in the app. "Outside" is outside the
      // OVERLAY, not merely outside the pane: the listener is bound to `document`.
      const sidebar = document.createElement("button");
      sidebar.type = "button";
      sidebar.setAttribute("data-testid", "sidebar");
      document.body.appendChild(sidebar);
      try {
        fireEvent.mouseDown(sidebar);
        await waitFor(() => expect(panelEl()).toBeNull());
      } finally {
        sidebar.remove();
      }
    },
  );

  it("drawer: a mousedown on the STACK's own scroller keeps the panel and the focus put", async () => {
    serveDrawer(3);
    render(<SpecOverlay terminalId="term-1" />);

    fireEvent.click(await screen.findByRole("button", { name: /open draft 1 live spec/i }));
    const panel = await openPanel();
    await waitFor(() => expect(document.activeElement).toBe(panel));

    // The SCROLLER of the pile (`spec-drawer-overlay`, `aria-label="Live specs"`) is the third piece
    // of interactive overlay chrome, next to the panel and the languettes: it stays
    // `pointer-events-auto` so its own scrollbar keeps a draggable thumb. A mousedown on that bar,
    // or in a gutter between two languettes, targets THIS element and no languette — dragging the
    // pile that COMMANDS the panel must not close it.
    const scroller = screen.getByLabelText("Live specs");
    expect(scroller.querySelector("[data-overlay-tab]")).not.toBeNull(); // it really is the pile's box
    fireEvent.mouseDown(scroller);
    expect(panelEl()).toBe(panel);
    expect(document.activeElement).toBe(panel);
  });

  it.each([
    ["rail", serveRail, /open auth revamp progress/i] as const,
    ["drawer", () => serveDrawer(), /open draft 1 live spec/i] as const,
  ])(
    "%s: Escape with focus in the TERMINAL neither dismisses nor is swallowed",
    async (_subject, serve, tabName) => {
      serve();
      render(
        <>
          {/* Focusable stand-in for the terminal surface: Escape is a WORKING key there (shells, vim,
            TUIs), so it must reach it untouched while the panel is open. */}
          <div data-testid="terminal-area" tabIndex={-1}>
            terminal
          </div>
          <SpecOverlay terminalId="term-1" />
        </>,
      );

      fireEvent.click(await screen.findByRole("button", { name: tabName }));
      const panel = await openPanel();

      const terminal = screen.getByTestId("terminal-area");
      terminal.focus();
      expect(document.activeElement).toBe(terminal);

      // The event must travel all the way up (nothing in the overlay listens on document/window and
      // nothing cancels it) AND leave the panel open.
      const seen: KeyboardEvent[] = [];
      const spy = (event: Event) => seen.push(event as KeyboardEvent);
      document.addEventListener("keydown", spy);
      try {
        const notCancelled = fireEvent.keyDown(terminal, { key: "Escape" });
        expect(notCancelled).toBe(true);
      } finally {
        document.removeEventListener("keydown", spy);
      }
      expect(seen).toHaveLength(1);
      expect(seen[0].defaultPrevented).toBe(false);
      expect(panelEl()).toBe(panel);

      // Belt and braces: an Escape raised from anywhere else (no overlay ancestor at all) is inert too.
      fireEvent.keyDown(document.body, { key: "Escape" });
      expect(panelEl()).toBe(panel);
    },
  );

  it("a click on the languette stays a SINGLE toggle — no dismiss-then-reopen", async () => {
    serveDrawer(2);
    render(<SpecOverlay terminalId="term-1" />);

    const first = await screen.findByRole("button", { name: /open draft 1 live spec/i });
    const second = screen.getByRole("button", { name: /open draft 2 live spec/i });
    fireEvent.click(first);
    await openPanel();

    // Record every panel MOUNT from here on: the outside-click listener runs on `mousedown`, i.e.
    // BEFORE the languette's own click, so a languette wrongly treated as "outside" would show up
    // as an extra mount (close, then re-open) instead of one clean transition.
    const mounts: string[] = [];
    const observer = new MutationObserver((records) => {
      for (const record of records) {
        for (const node of record.addedNodes) {
          if (node instanceof Element && node.matches("[data-overlay-panel]")) {
            mounts.push(node.getAttribute("data-overlay-panel-spec") ?? "");
          }
        }
      }
    });
    observer.observe(document.body, { childList: true, subtree: true });
    try {
      // (a) Re-click the ACTIVE languette with the FULL mouse gesture ⇒ it closes, and stays closed.
      fireEvent.mouseDown(first);
      fireEvent.click(first);
      await waitFor(() => expect(panelEl()).toBeNull());
      expect(mounts).toEqual([]);

      // (b) Click ANOTHER languette ⇒ exactly one panel opens (the new Spec's), no intermediate one.
      fireEvent.mouseDown(second);
      fireEvent.click(second);
      await waitFor(() => expect(panelEl()).not.toBeNull());
      expect(mounts).toEqual(["s2"]);
    } finally {
      observer.disconnect();
    }
    expect(second).toHaveAttribute("aria-expanded", "true");
    expect(first).toHaveAttribute("aria-expanded", "false");
  });

  it("keeps the panel open for its OWN commands and for the REAL run modal portalled out of the pane", async () => {
    serveDrawer();
    render(<SpecOverlay terminalId="term-1" />);

    fireEvent.click(await screen.findByRole("button", { name: /open draft 1 live spec/i }));
    const panel = await openPanel();

    // A command INSIDE the panel navigates away; dismissing under it would tear the panel down mid
    // click and could unmount the component the click is still travelling through.
    const openSpec = screen.getByRole("button", { name: /open the spec/i });
    fireEvent.mouseDown(openSpec);
    fireEvent.click(openSpec);
    expect(panelEl()).toBe(panel);

    // The REAL orchestration run modal, opened from the panel's own header. `<SpecRunButton>` OWNS
    // its open state and renders it in place, so dismissing the panel would unmount the modal under
    // the cursor. A stand-in `div` appended to the body would NOT prove this: it would only re-test
    // the geography ("outside the pane"), while what excludes the modal is the explicit
    // `data-portal-layer` marker `ui/dialog`'s `<Dialog.Portal>` stamps on the container it appends
    // to `document.body` — asserted here, since it is what the overlay's rule reads.
    fireEvent.click(screen.getByRole("button", { name: /open the orchestration run/i }));
    const modalBody = await screen.findByTestId("spec-run-modal-body");
    expect(modalBody.closest("[data-overlay-surface]")).toBeNull(); // truly outside the overlay
    expect(modalBody.closest("[data-portal-layer]")).not.toBeNull(); // and marked as a portalled layer
    fireEvent.mouseDown(modalBody);
    expect(panelEl()).toBe(panel);
  });
});
