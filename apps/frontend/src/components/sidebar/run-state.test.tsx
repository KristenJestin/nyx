import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { StatusDot, TerminalStateBadge, type BadgeState } from "./run-state";
import type { ExecState } from "./use-terminals";

const ALL_STATES: ExecState[] = ["idle", "running", "success", "error"];

/**
 * The COLOUR token now lives on the `<CrossfadeFill>` layer (an `aria-hidden`
 * absolute child), NOT on the `role="status"` host — the host owns the shape,
 * a11y, `data-state` and the pulse, the layer owns the cross-fading colour. Read
 * the fill classes off that layer.
 */
function fillClasses(host: HTMLElement): string {
  // During a colour cross-fade two stacked layers briefly coexist; aggregate them so a
  // token assertion sees the layer it cares about regardless of DOM order.
  return Array.from(host.querySelectorAll("[aria-hidden]"))
    .map((el) => el.className)
    .join(" ");
}

describe("<StatusDot> (command run-state, all 4 states)", () => {
  it.each(ALL_STATES)("renders the %s state with the right token color", (state) => {
    render(<StatusDot state={state} />);
    const dot = screen.getByRole("status", { name: new RegExp(`status: ${state}`, "i") });
    expect(dot).toHaveAttribute("data-state", state);
  });

  it("colors each state from a design-system token (no raw colors)", () => {
    const tokens: Record<ExecState, string> = {
      idle: "bg-muted-foreground/50",
      running: "bg-info",
      success: "bg-success",
      error: "bg-destructive",
    };
    for (const state of ALL_STATES) {
      const { unmount } = render(<StatusDot state={state} />);
      // The colour lives on the cross-fade layer, not the role="status" host.
      expect(fillClasses(screen.getByRole("status"))).toContain(tokens[state]);
      unmount();
    }
  });

  it("pulses ONLY in the running state", () => {
    const { rerender } = render(<StatusDot state="running" />);
    expect(screen.getByRole("status").className).toContain("animate-pulse");
    rerender(<StatusDot state="success" />);
    expect(screen.getByRole("status").className).not.toContain("animate-pulse");
  });
});

describe("<TerminalStateBadge> (terminal run-state, persisted-unread model)", () => {
  it("renders NO badge for idle (nothing to notify)", () => {
    render(<TerminalStateBadge state="idle" unread />);
    // Idle is never shown: AnimatePresence's child is `false` from the first render,
    // so no `role="status"` badge node exists at all.
    expect(screen.queryByRole("status")).toBeNull();
  });

  it.each(["success", "error"] as ExecState[])(
    "renders the %s badge while UNREAD (the persisted flag drives visibility)",
    (state) => {
      render(<TerminalStateBadge state={state} unread />);
      const badge = screen.getByRole("status", {
        name: new RegExp(`terminal status: ${state}`, "i"),
      });
      expect(badge).toHaveAttribute("data-state", state);
    },
  );

  it("HIDES the settled badge once READ — even when the terminal is INACTIVE again (user story #3)", () => {
    for (const state of ["success", "error"] as ExecState[]) {
      // unread=false (the user already viewed it) + active=false (re-deselected):
      // the badge must NOT appear — visibility is driven by `unread`, not `active`.
      // It was never shown, so no exit animation runs: the node is simply absent.
      const { unmount } = render(
        <TerminalStateBadge state={state} unread={false} active={false} />,
      );
      expect(screen.queryByRole("status")).toBeNull();
      unmount();
    }
  });

  it("KEEPS the running (blue, pulsing) badge regardless of unread/active (live state)", () => {
    // Running is a live state, never gated by the unread flag — shows even active+read.
    render(<TerminalStateBadge state="running" unread={false} active />);
    const badge = screen.getByRole("status", { name: /terminal status: running/i });
    // The blue token is on the cross-fade layer; the pulse stays on the host.
    expect(fillClasses(badge)).toContain("bg-info");
    expect(badge.className).toContain("animate-pulse");
  });

  it("KEEPS the WAITING (yellow, pulsing) badge regardless of unread/active (live state)", () => {
    // `waiting` is the agent-only "blocked on the user" LIVE state — yellow (--warning),
    // pulsing, and never gated by unread/active (it shows even on the active terminal).
    render(<TerminalStateBadge state="waiting" unread={false} active />);
    const badge = screen.getByRole("status", { name: /terminal status: waiting/i });
    expect(badge).toHaveAttribute("data-state", "waiting");
    expect(fillClasses(badge)).toContain("bg-warning");
    expect(badge.className).toContain("animate-pulse");
  });

  it("badge colors come from tokens (the appear pop is Motion-driven)", () => {
    const tokens: Record<string, string> = {
      running: "bg-info",
      waiting: "bg-warning",
      success: "bg-success",
      error: "bg-destructive",
    };
    // success/error need `unread` to show; running/waiting are live and show regardless.
    for (const state of ["running", "waiting", "success", "error"] as BadgeState[]) {
      const { unmount } = render(<TerminalStateBadge state={state} unread />);
      // Token lives on the cross-fade layer (the appear pop + colour are Motion-driven).
      expect(fillClasses(screen.getByRole("status"))).toContain(tokens[state]);
      unmount();
    }
  });

  it("ANIMATES the settled badge OUT when it becomes read (exit, not an instant cut)", async () => {
    // An unread success badge is shown; once acknowledged (unread=false) the node leaves
    // via an exit animation — it is no longer immediately gone, it animates away first.
    const { rerender } = render(<TerminalStateBadge state="success" unread active={false} />);
    expect(screen.getByRole("status", { name: /terminal status: success/i })).toBeInTheDocument();
    rerender(<TerminalStateBadge state="success" unread={false} active={false} />);
    await waitFor(() =>
      expect(
        screen.queryByRole("status", { name: /terminal status: success/i }),
      ).not.toBeInTheDocument(),
    );
  });
});
