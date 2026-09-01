import * as React from "react";
import { afterEach, describe, expect, it } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { Toaster, toast, toastManager } from "./toast";

/**
 * Unit coverage for the toast system (FEEDBACK #10), built on Base UI Toast.
 *
 * We assert the two contracts the rest of the app depends on:
 *  - `<Toaster>` renders the live toast stack from the GLOBAL `toastManager`, themed
 *    per variant — a `toast.success(...)` shows its message with the success accent,
 *    a `toast.error(...)` with the destructive accent, etc. (the renderer the
 *    bottom-right viewport mounts);
 *  - the imperative `toast.*` helper maps to the right manager payload (variant `type`,
 *    description, dedup by `id`, sticky-vs-auto timeout) — the surface every wired
 *    mutation calls.
 *
 * The manager is a singleton, so we dismiss everything between tests to isolate them.
 */

afterEach(() => {
  // Drop any toast still on the global manager so the next test starts clean.
  act(() => {
    toast.dismiss();
  });
});

describe("<Toaster> rendering", () => {
  it("renders a success toast with its message and success accent", async () => {
    render(<Toaster />);
    act(() => {
      toast.success("Command saved");
    });

    const alert = await screen.findByText("Command saved");
    // The toast card carries the variant marker the styling keys on.
    const root = alert.closest("[data-variant]");
    expect(root).not.toBeNull();
    expect(root).toHaveAttribute("data-variant", "success");
    // The status icon uses the success token color (never a hardcoded hex).
    expect((root as HTMLElement).querySelector(".text-success")).not.toBeNull();
  });

  it("renders an error toast with its message and destructive accent", async () => {
    render(<Toaster />);
    act(() => {
      toast.error("this command is running in at least one workspace");
    });

    // The viewport is PORTALED onto <body> (outside the render container), and errors
    // announce urgently (`priority: high`) so Base UI also mirrors the text into a
    // hidden live region → match on the VISIBLE toast card by its variant.
    await waitFor(() =>
      expect(document.body.querySelector('[data-variant="error"]')).not.toBeNull(),
    );
    const root = document.body.querySelector('[data-variant="error"]') as HTMLElement;
    expect(root).toHaveTextContent("this command is running in at least one workspace");
    expect(root.querySelector(".text-destructive")).not.toBeNull();
  });

  it("renders the info and warning variants with their tokens", async () => {
    render(<Toaster />);
    act(() => {
      toast.info("Heads up");
      toast.warning("Careful");
    });

    const info = (await screen.findByText("Heads up")).closest("[data-variant]");
    const warning = (await screen.findByText("Careful")).closest("[data-variant]");
    expect(info).toHaveAttribute("data-variant", "info");
    expect(warning).toHaveAttribute("data-variant", "warning");
    expect((info as HTMLElement).querySelector(".text-info")).not.toBeNull();
    expect((warning as HTMLElement).querySelector(".text-warning")).not.toBeNull();
  });

  it("renders a description line under the message when provided", async () => {
    render(<Toaster />);
    act(() => {
      toast.success("Saved", { description: "The command was updated." });
    });

    const root = (await screen.findByText("Saved")).closest("[data-variant]");
    expect(within(root as HTMLElement).getByText("The command was updated.")).toBeInTheDocument();
  });

  it("exposes a Dismiss control that closes the toast", async () => {
    render(<Toaster />);
    act(() => {
      toast.success("Dismiss me");
    });

    await screen.findByText("Dismiss me");
    // Base UI keeps the close button `aria-hidden` until the toast is hovered/focused
    // (it stays reachable for AT via the toast's own controls), so we target it by its
    // accessible label attribute rather than the accessibility ROLE. The viewport is
    // portaled onto <body>, outside the render container.
    const closeBtn = document.body.querySelector('[aria-label="Dismiss"]') as HTMLButtonElement;
    expect(closeBtn).not.toBeNull();
    act(() => {
      closeBtn.click();
    });
    await waitFor(() => {
      expect(screen.queryByText("Dismiss me")).not.toBeInTheDocument();
    });
  });
});

describe("toast.decision — the actionable toast (human review 01M1E7K6D81M057JG5MVV4J7QE / T2)", () => {
  /** A body with its OWN state: what the free content slot exists for (a live countdown). */
  function Body() {
    const [ticks, setTicks] = React.useState(0);
    return (
      <div data-testid="decision-body">
        <p>Approve this?</p>
        <button type="button" onClick={() => setTicks((n) => n + 1)}>
          Yes ({ticks})
        </button>
      </div>
    );
  }

  it("renders its own content — not a title/description pair — and that content stays live", async () => {
    render(<Toaster />);
    act(() => {
      toast.decision({ id: "decision-1", body: Body });
    });

    const body = await screen.findByTestId("decision-body");
    const root = body.closest("[data-variant]") as HTMLElement;
    // The shell is the stack's own card (same variant marker, same animation classes); what is
    // inside it is the caller's.
    expect(root).toHaveAttribute("data-variant", "warning");
    expect(root).toHaveAttribute("data-pinned");
    // The body owns its state: it re-renders without the manager being updated once.
    act(() => {
      screen.getByRole("button", { name: /^Yes/ }).click();
    });
    expect(screen.getByRole("button", { name: "Yes (1)" })).toBeTruthy();
  });

  it("cannot be dismissed — no close control, and only the owner takes it back", async () => {
    render(<Toaster />);
    act(() => {
      toast.decision({ id: "decision-2", body: Body });
    });
    const root = (await screen.findByTestId("decision-body")).closest(
      "[data-variant]",
    ) as HTMLElement;

    expect(root.querySelector('[aria-label="Dismiss"]')).toBeNull();
    act(() => {
      root.focus();
    });
    fireEvent.keyDown(root, { key: "Escape" });
    expect(screen.getByTestId("decision-body")).toBeTruthy();

    // …while an ordinary toast beside it keeps every one of those exits.
    act(() => {
      toast.info("Just so you know");
    });
    const ordinary = (await screen.findByText("Just so you know")).closest(
      "[data-variant]",
    ) as HTMLElement;
    expect(ordinary.querySelector('[aria-label="Dismiss"]')).not.toBeNull();

    // The owner is the only way out.
    act(() => {
      toast.dismiss("decision-2");
    });
    await waitFor(() => expect(screen.queryByTestId("decision-body")).not.toBeInTheDocument());
  });

  it("is PINNED: it keeps stack rank 0 whatever lands after it", async () => {
    render(<Toaster />);
    act(() => {
      toast.decision({ id: "decision-3", body: Body });
      toast.success("Saved");
    });
    const decision = (await screen.findByTestId("decision-body")).closest(
      "[data-variant]",
    ) as HTMLElement;
    const ordinary = (await screen.findByText("Saved")).closest("[data-variant]") as HTMLElement;

    // Arrival order would put the newest — `Saved` — at rank 0, in front of the decision.
    expect(decision.style.getPropertyValue("--toast-index")).toBe("0");
    expect(ordinary.style.getPropertyValue("--toast-index")).toBe("1");
    expect(Number(decision.style.zIndex)).toBeGreaterThan(Number(ordinary.style.zIndex));
  });
});

describe("toast helper → manager payload", () => {
  it("adds the right `type` per variant", () => {
    const seen: { type?: string; title?: unknown }[] = [];
    const unsub = toastManager[" subscribe"]((ev) => {
      if (ev.action === "add") seen.push({ type: ev.options.type, title: ev.options.title });
    });

    toast.success("ok");
    toast.error("boom");
    toast.info("fyi");
    toast.warning("watch out");
    unsub();

    expect(seen.map((s) => s.type)).toEqual(["success", "error", "info", "warning"]);
    expect(seen.map((s) => s.title)).toEqual(["ok", "boom", "fyi", "watch out"]);
  });

  it("dedups by `id` — re-using an id updates in place instead of stacking", async () => {
    render(<Toaster />);
    act(() => {
      toast.info("first", { id: "dup" });
    });
    await screen.findByText("first");

    act(() => {
      toast.info("second", { id: "dup" });
    });
    // The updated copy shows; the old text is gone — a single toast, not two.
    await screen.findByText("second");
    expect(screen.queryByText("first")).not.toBeInTheDocument();
  });

  it("makes errors sticky (timeout 0) and successes auto-dismiss (timeout > 0)", () => {
    const byVariant: Record<string, number | undefined> = {};
    const unsub = toastManager[" subscribe"]((ev) => {
      if (ev.action === "add") byVariant[ev.options.type as string] = ev.options.timeout;
    });

    toast.success("s");
    toast.error("e");
    unsub();

    expect(byVariant.error).toBe(0);
    expect(byVariant.success).toBeGreaterThan(0);
  });
});
