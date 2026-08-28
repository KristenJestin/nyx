import { render, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { aggregateState, isQuietState, type RunState, StatusDot } from "@/components/ui/status-dot";

describe("StatusDot", () => {
  it("reflects the state on the host via data-state", () => {
    const { container } = render(<StatusDot state="running" />);
    const host = container.querySelector("[data-state]");
    expect(host).not.toBeNull();
    expect(host!.getAttribute("data-state")).toBe("running");
  });

  it("renders NOTHING for a quiet (grey/empty) state — no dot on idle/closed", () => {
    for (const quiet of ["idle", "closed"] as const) {
      const { container } = render(<StatusDot state={quiet} />);
      // A quiet state paints no dot at all: no host element, empty output.
      expect(container.querySelector("[data-state]")).toBeNull();
      expect(container.innerHTML).toBe("");
    }
  });

  it("renders a dot for every significant state", () => {
    for (const state of ["running", "waiting", "success", "error"] as const) {
      const { container } = render(<StatusDot state={state} />);
      expect(container.querySelector("[data-state]")).not.toBeNull();
    }
  });

  it("pulses the core only for live states (running / waiting)", () => {
    for (const live of ["running", "waiting"] as const) {
      const { container } = render(<StatusDot state={live} />);
      // The core carries the pulse; the ping halo is visible (not hidden) for live states.
      expect(container.innerHTML).toContain("animate-pulse");
      expect(container.innerHTML).toContain("animate-ping");
    }
  });

  it("does not pulse settled states (success / error)", () => {
    for (const state of ["success", "error"] as const) {
      const { container } = render(<StatusDot state={state} />);
      expect(container.innerHTML).not.toContain("animate-pulse");
      // The ping layer exists but is `hidden` for non-live states.
      expect(container.innerHTML).toContain("hidden");
    }
  });

  it("carries a colour transition so a state change glides (no snap)", () => {
    // The core fill animates via `transition-colors` (token-backed duration/easing),
    // so switching state does not hard-cut the colour.
    const { container } = render(<StatusDot state="running" />);
    expect(container.innerHTML).toContain("transition-colors");
  });

  it("animates the dot IN on appear — the host carries the presence scale/opacity target", () => {
    // The dot mounts as a Motion element (its inline style shows the appear scale/opacity),
    // so a state leaving quiet POPS in (fade + slight scale) rather than snapping.
    const { container } = render(<StatusDot state="success" />);
    const host = container.querySelector("[data-state]") as HTMLElement | null;
    expect(host).not.toBeNull();
    // Motion writes the animated transform/opacity inline while it settles from the
    // `DOT_PRESENCE.initial` (scale 0 / opacity 0) target — the enter animation ran.
    expect(host!.getAttribute("style") ?? "").toMatch(/opacity|transform|scale/);
  });

  it("ANIMATES the dot OUT when the state goes quiet (exit, not an instant cut)", async () => {
    // A significant dot is shown; when the state becomes quiet (idle) the node LEAVES via an
    // exit animation — it is not immediately gone, it animates away first, then unmounts.
    const { container, rerender } = render(<StatusDot state="success" />);
    expect(container.querySelector("[data-state]")).not.toBeNull();
    rerender(<StatusDot state="idle" />);
    await waitFor(() => expect(container.querySelector("[data-state]")).toBeNull());
  });

  it("keeps ONE dot on a visible→visible transition — the old one does NOT linger in exit", async () => {
    // The reported bug: a running→success swap keyed the presence on `state`, so the OLD
    // `running` dot stayed mounted fading in EXIT while the new `success` popped IN — two
    // dots stacked. A STABLE key keeps the SAME node, so the fill glides in place
    // (transition-colors) and there is ALWAYS exactly one dot.
    const { container, rerender } = render(<StatusDot state="running" />);
    expect(container.querySelectorAll("[data-state]")).toHaveLength(1);
    rerender(<StatusDot state="success" />);
    // Flush AnimatePresence's exit-injection tick. Under the buggy remount THIS is where a
    // fading OLD `running` node would appear alongside the new one (two mounted dots).
    await new Promise((r) => setTimeout(r, 0));
    const dots = container.querySelectorAll("[data-state]");
    expect(dots).toHaveLength(1);
    expect(dots[0]!.getAttribute("data-state")).toBe("success");
    // No stale `running` dot fading on top of the settled one.
    expect(container.querySelector('[data-state="running"]')).toBeNull();
  });

  it("stays one dot across successive visible→visible swaps (running→waiting→error)", async () => {
    const { container, rerender } = render(<StatusDot state="running" />);
    for (const next of ["waiting", "error"] as const) {
      rerender(<StatusDot state={next} />);
      await new Promise((r) => setTimeout(r, 0));
      const dots = container.querySelectorAll("[data-state]");
      expect(dots).toHaveLength(1);
      expect(dots[0]!.getAttribute("data-state")).toBe(next);
    }
  });

  it("still MOUNTS a dot when a state leaves quiet (idle→running enter preserved)", () => {
    // The presence pop on the quiet↔visible boundary must survive the stable-key fix: a
    // quiet state renders nothing, so leaving quiet is a genuine mount (the enter animation).
    const { container, rerender } = render(<StatusDot state="idle" />);
    expect(container.querySelector("[data-state]")).toBeNull();
    rerender(<StatusDot state="running" />);
    expect(container.querySelector("[data-state]")).not.toBeNull();
  });
});

describe("isQuietState", () => {
  it("treats idle/closed as quiet (no dot) and every other state as significant", () => {
    expect(isQuietState("idle")).toBe(true);
    expect(isQuietState("closed")).toBe(true);
    for (const state of ["running", "waiting", "success", "error"] as const) {
      expect(isQuietState(state)).toBe(false);
    }
  });
});

describe("aggregateState", () => {
  it("returns idle for an empty bag", () => {
    expect(aggregateState([])).toBe("idle");
  });

  it("picks the most attention-worthy state present (priority order)", () => {
    // Priority: waiting > error > success > running > idle/closed.
    // success now OUTRANKS running (a settled result wins over a busy one).
    expect(aggregateState(["idle", "success", "running"])).toBe("success");
    expect(aggregateState(["success", "error"])).toBe("error");
    // waiting (agent blocked on the user) leads — it outranks even error.
    expect(aggregateState(["idle", "waiting", "running"])).toBe("waiting");
    expect(aggregateState(["error", "waiting"])).toBe("waiting");
    expect(aggregateState(["running"])).toBe("running");
    expect(aggregateState(["closed", "idle"])).toBe("idle");
    expect(aggregateState(["closed"])).toBe("closed");
  });

  it("is total over every RunState", () => {
    const all: RunState[] = ["idle", "running", "success", "error", "waiting", "closed"];
    // waiting is the highest priority now, so any bag containing it rolls up to waiting.
    expect(aggregateState(all)).toBe("waiting");
  });
});
