import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { emit, mockIPC } from "@/bridge/test-harness";
import { afterEach, describe, expect, it } from "vitest";

import { Toaster, toast } from "@/components/ui/toast";
import { SystemApprovalGate } from "./system-approval-gate";
import { expiresInLabel, type SystemApproval } from "./use-system-approvals";

/** One pending request as `system_approval_list` renders it — token-free by construction. */
function approval(overrides: Partial<SystemApproval> = {}): SystemApproval {
  const merged = {
    request_id: "req-1",
    operation_kind: "stop_command",
    payload: { instance_id: "sys-inst", name: "palbank services" },
    state: "pending",
    requested_at: 1_000,
    expires_at: Date.now() + 120_000,
    summary: "stop_command — palbank services",
    requested_by: 'claude · terminal "claude"',
    ...overrides,
  };
  // The fingerprint follows the request unless a case pins it: it is the queue's dedup key AND the
  // toast's id, so two distinct requests must not silently share one.
  return { ...merged, fingerprint: overrides.fingerprint ?? `fp-${merged.request_id}` };
}

/**
 * The backend behind the decision surface. `calls` records every invocation so a test can assert
 * WHICH db-command a click sent — the point of the grant/deny cases.
 */
function installBackend(
  pending: SystemApproval[],
  calls: Array<{ cmd: string; args: Record<string, unknown> }> = [],
) {
  let queue = [...pending];
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args: a });
      switch (cmd) {
        case "system_approval_list":
          return queue;
        case "system_approval_grant":
        case "system_approval_deny":
          queue = queue.filter((r) => r.request_id !== a.requestId);
          setTimeout(() => void emit("system-approvals://changed"), 0);
          return { ...pending[0], state: cmd === "system_approval_grant" ? "granted" : "denied" };
        default:
          return undefined;
      }
    },
    { shouldMockEvents: true },
  );
}

/**
 * The gate renders NOTHING of its own since human review 01M1E7K6D81M057JG5MVV4J7QE (T2): it owns
 * the queue and drives ONE toast on the app-wide stack. So every case mounts the pair the shell
 * mounts — `<Toaster>` (the single toast host) and the gate — and looks for the card in the
 * portalled viewport, not in the gate's own subtree.
 */
function renderGate(extra?: React.ReactNode) {
  return render(
    <>
      {extra}
      <Toaster />
      <SystemApprovalGate />
    </>,
  );
}

/** The `<Toast.Root>` the approval body sits in — where the stack's own attributes live. */
function approvalRoot(): HTMLElement {
  const card = screen.getByTestId("system-approval-toast");
  const root = card.closest("[data-variant]");
  if (root == null) throw new Error("the approval card is not inside a toast root");
  return root as HTMLElement;
}

afterEach(() => {
  // The manager is a global singleton: drop whatever a case left on it.
  act(() => {
    toast.dismiss();
  });
});

describe("expiresInLabel", () => {
  it("counts down in seconds under a minute, in minutes above, and says so when it is over", () => {
    // A pure function of two numbers — the surface's ONLY time-dependent behaviour, asserted
    // without a clock and without a rendered component.
    expect(expiresInLabel(10_000, 10_000)).toBe("expired");
    expect(expiresInLabel(10_000, 20_000)).toBe("expired");
    expect(expiresInLabel(41_000, 0)).toBe("expires in 41 s");
    expect(expiresInLabel(59_000, 0)).toBe("expires in 59 s");
    expect(expiresInLabel(120_000, 0)).toBe("expires in 2 min");
    expect(expiresInLabel(300_000, 0)).toBe("expires in 5 min");
  });
});

describe("SystemApprovalGate", () => {
  it("renders NOTHING while no request is pending", async () => {
    installBackend([]);
    renderGate();
    await waitFor(() => {
      expect(screen.queryByTestId("system-approval-toast")).toBeNull();
    });
  });

  it("shows WHAT is being asked, WHEN it expires, and never a token", async () => {
    installBackend([approval()]);
    renderGate();

    // The human sentence for the verb, not the verb alone.
    expect(await screen.findByText("An agent wants to stop a system command")).toBeTruthy();
    // …and the operation itself, spelled out.
    expect(screen.getByText("stop_command")).toBeTruthy();
    expect(screen.getByText("palbank services")).toBeTruthy();
    expect(screen.getByText('claude · terminal "claude"')).toBeTruthy();
    expect(screen.getByText(/expires in/)).toBeTruthy();
    // The DECISION is the only way out: two buttons, no dismissal.
    expect(screen.getByRole("button", { name: "Approve once" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Deny" })).toBeTruthy();
    // NOTHING of the credential is on screen — the human decides on the OPERATION.
    const card = screen.getByTestId("system-approval-toast");
    expect(card.textContent ?? "").not.toContain("token");
  });

  it("approves ONE request through the human-only db-command, and drops the card", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend([approval()], calls);
    renderGate();
    fireEvent.click(await screen.findByRole("button", { name: "Approve once" }));

    await waitFor(() => {
      expect(calls.some((c) => c.cmd === "system_approval_grant")).toBe(true);
    });
    expect(calls.find((c) => c.cmd === "system_approval_grant")?.args).toEqual({
      requestId: "req-1",
    });
    // The card leaves — the decided request is no longer pending on the backend either.
    await waitFor(() => {
      expect(screen.queryByTestId("system-approval-toast")).toBeNull();
    });
  });

  it("denies through the OTHER db-command — a refusal is a decision, not a dismissal", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend([approval()], calls);
    renderGate();
    fireEvent.click(await screen.findByRole("button", { name: "Deny" }));

    await waitFor(() => {
      expect(calls.some((c) => c.cmd === "system_approval_deny")).toBe(true);
    });
    expect(calls.some((c) => c.cmd === "system_approval_grant")).toBe(false);
  });

  it("is a BOUNDED surface: one card at a time, oldest first, and the rest announced", async () => {
    installBackend([
      approval({
        request_id: "req-old",
        summary: "add_command — a",
        operation_kind: "add_command",
      }),
      approval({ request_id: "req-new", operation_kind: "relaunch_command" }),
      approval({ request_id: "req-newer", operation_kind: "remove_command" }),
    ]);
    renderGate();

    // The OLDEST is the one on screen (the backend orders by date; the surface takes the head).
    expect(await screen.findByText("An agent wants to add a system command")).toBeTruthy();
    expect(screen.queryByText("An agent wants to relaunch a system command")).toBeNull();
    // …and the queue behind it is announced, not stacked.
    expect(screen.getByText(/2 more waiting/)).toBeTruthy();
  });

  it("re-reads the queue on the broadcast, whichever side moved it", async () => {
    let queue: SystemApproval[] = [];
    mockIPC(
      (cmd) => {
        if (cmd === "system_approval_list") return queue;
        return undefined;
      },
      { shouldMockEvents: true },
    );
    renderGate();
    await waitFor(() => {
      expect(screen.queryByTestId("system-approval-toast")).toBeNull();
    });

    // An agent's protected call was just REFUSED on the MCP side — the one place a refusal is the
    // event. The surface must come up without any poll.
    queue = [approval({ operation_kind: "add_command" })];
    await emit("system-approvals://changed");
    expect(await screen.findByText("An agent wants to add a system command")).toBeTruthy();
  });

  it("shows the ENVIRONMENT a start_command.env request carries, values included", async () => {
    // The human is being asked to let a command line they DID approve run with a map they did not
    // — `NODE_OPTIONS=--require=…` executes someone else's module. Naming only the keys would be
    // asking for a decision while withholding its evidence.
    installBackend([
      approval({
        operation_kind: "start_command.env",
        payload: {
          instance_id: "sys-inst",
          env: { NODE_OPTIONS: "--require=/tmp/inject.cjs" },
        },
      }),
    ]);
    renderGate();

    expect(
      await screen.findByText("An agent wants to start a system command with its own environment"),
    ).toBeTruthy();
    expect(screen.getByText("NODE_OPTIONS=--require=/tmp/inject.cjs")).toBeTruthy();
  });

  it("retires a card whose TTL ran out, with NO gesture and NO decision sent", async () => {
    // The defect this pins: the backend expires LAZILY (on a read) and nothing reads on its own,
    // so an ignored request used to hold an undismissable modal over the whole app forever.
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    installBackend([approval({ request_id: "req-dead", expires_at: Date.now() - 1 })], calls);
    renderGate();

    await waitFor(() => {
      expect(screen.queryByTestId("system-approval-toast")).toBeNull();
    });
    // …and the exit was TIME, not a decision: neither button was clicked, and neither db-command
    // was sent. An expiry is not an answer.
    expect(calls.some((c) => c.cmd === "system_approval_grant")).toBe(false);
    expect(calls.some((c) => c.cmd === "system_approval_deny")).toBe(false);
    // The surface DID re-read, so the backend applies its own lazy expiry to the same row. Awaited
    // rather than read once: the reconcile is an effect, and the card can be gone before it lands.
    await waitFor(() => {
      expect(calls.filter((c) => c.cmd === "system_approval_list").length).toBeGreaterThan(1);
    });
  });

  it("lets the NEXT request take the place of an expired one instead of freezing the queue", async () => {
    installBackend([
      approval({
        request_id: "req-dead",
        operation_kind: "add_command",
        expires_at: Date.now() - 1,
      }),
      approval({ request_id: "req-live", operation_kind: "remove_command" }),
    ]);
    renderGate();

    expect(await screen.findByText("An agent wants to remove a system command")).toBeTruthy();
    expect(screen.queryByText("An agent wants to add a system command")).toBeNull();
    // The dead card is not counted as "waiting" either — a queue length must mean live requests.
    expect(screen.queryByText(/more waiting/)).toBeNull();
  });

  it("does NOT block the app: no backdrop, no modal, and what is behind stays clickable", async () => {
    // REVIEW FINDING 01M1C7DX1T7QZ89H3BCNRKYA0H. The information was right; the interruption was
    // not. A full-screen modal took the whole application hostage until the user answered. What
    // this pins is the difference between the two shells, in the terms that make it real: no
    // `aria-modal`, no backdrop element, and a corner surface that does not hold the app hostage.
    installBackend([approval()]);
    const clicked: string[] = [];
    renderGate(
      <button type="button" onClick={() => clicked.push("behind")}>
        Something else entirely
      </button>,
    );

    const card = await screen.findByTestId("system-approval-toast");
    expect(card.closest("[aria-modal='true']")).toBeNull();
    expect(document.querySelector("[data-backdrop]")).toBeNull();
    // Base UI's own root is `aria-modal="false"` — a dialog that claims nothing of the page.
    expect(approvalRoot().getAttribute("aria-modal")).toBe("false");
    // …and the rest of the app really answers while the request waits.
    fireEvent.click(screen.getByRole("button", { name: "Something else entirely" }));
    expect(clicked).toEqual(["behind"]);
    expect(screen.getByTestId("system-approval-toast")).toBeTruthy();
  });

  it("offers no dismissal: no close button, no swipe, no Escape — approve, deny, or expire", async () => {
    // Dismissing is not one of the three outcomes — a card swiped away would leave an agent
    // waiting on a request the user believes they answered. On the shared stack that is not a
    // property of the markup any more but a per-toast setting, so all three exits are pinned.
    installBackend([approval()]);
    renderGate();
    const card = await screen.findByTestId("system-approval-toast");
    const root = approvalRoot();

    // 1. THE CROSS. Every ordinary toast carries a `Toast.Close`; this root has none.
    const labels = [...root.querySelectorAll("button")].map((b) => b.textContent?.trim());
    expect(labels).toEqual(["Deny", "Approve once"]);
    expect(root.querySelector('[aria-label="Dismiss"]')).toBeNull();

    // 2. THE SWIPE. Base UI attaches its pointer handlers only when at least one swipe direction is
    // allowed, and this toast declares none; a full drag across the card records no movement and
    // leaves it on screen.
    fireEvent.pointerDown(root, { pointerId: 1, clientX: 0, clientY: 0, button: 0 });
    fireEvent.pointerMove(root, { pointerId: 1, clientX: 400, clientY: 400 });
    fireEvent.pointerUp(root, { pointerId: 1, clientX: 400, clientY: 400 });
    expect(root.style.getPropertyValue("--toast-swipe-movement-x")).toBe("0px");
    expect(root.getAttribute("data-swipe-direction")).toBeNull();
    expect(screen.getByTestId("system-approval-toast")).toBeTruthy();

    // 3. ESCAPE. Base UI closes a focused toast on Escape; this one refuses the key.
    act(() => {
      root.focus();
    });
    fireEvent.keyDown(root, { key: "Escape" });
    expect(screen.getByTestId("system-approval-toast")).toBeTruthy();
    expect(card).toBeTruthy();
  });

  it("is PINNED: an ordinary toast fired mid-decision neither covers it nor pushes it behind", async () => {
    // THE RISK THAT ONCE JUSTIFIED A SEPARATE SURFACE (review 01M1E7K6D81M057JG5MVV4J7QE / T2).
    // The stack orders by arrival — newest at rank 0, full size, in front — so a `toast.success`
    // landing while a request waits used to take the front slot and paint over the two buttons.
    // Pinning inverts the rule for actionable toasts only: rank 0 is theirs whatever arrives after.
    installBackend([approval()]);
    renderGate();
    await screen.findByTestId("system-approval-toast");
    expect(approvalRoot().style.getPropertyValue("--toast-index")).toBe("0");

    // THREE ordinary toasts: enough to push past the viewport's default `limit` of 3, which is the
    // second half of the risk — beyond the limit Base UI marks the OLDEST toasts `limited` and
    // INERTS them, and the decision is by definition the oldest of the four.
    act(() => {
      toast.success("A command was saved");
      toast.info("Heads up");
      toast.warning("Careful with that one");
    });
    const oldest = (await screen.findByText("A command was saved")).closest(
      "[data-variant]",
    ) as HTMLElement;
    const newest = (await screen.findByText("Careful with that one")).closest(
      "[data-variant]",
    ) as HTMLElement;

    // THE DECISION KEEPS THE FRONT SLOT: rank 0 (full scale, no offset), and the informative stack
    // starts one rank further back — newest of them at 1, oldest of them at 3.
    const decisionRoot = approvalRoot();
    expect(decisionRoot).toHaveAttribute("data-pinned");
    expect(decisionRoot.style.getPropertyValue("--toast-index")).toBe("0");
    expect(newest.style.getPropertyValue("--toast-index")).toBe("1");
    expect(oldest.style.getPropertyValue("--toast-index")).toBe("3");
    // …and it is not merely in front geometrically: paint order follows the stack rank, not the
    // manager's array, so nothing behind can still be drawn over the buttons.
    expect(Number(decisionRoot.style.zIndex)).toBeGreaterThan(Number(oldest.style.zIndex));
    expect(Number(decisionRoot.style.zIndex)).toBeGreaterThan(Number(newest.style.zIndex));
    // The buttons stay live. Base UI still counts the decision past the limit (it is the oldest of
    // the four, so it is flagged) — what a pinned toast is exempt from is the CONSEQUENCE: no
    // `inert`. A question on screen with dead buttons is the same defect wearing a smaller shape.
    expect(decisionRoot).toHaveAttribute("data-limited");
    expect(decisionRoot.hasAttribute("inert")).toBe(false);
    expect(oldest.hasAttribute("inert")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Approve once" }));
    await waitFor(() => {
      expect(screen.queryByTestId("system-approval-toast")).toBeNull();
    });
  });

  it("surfaces the backend's OWN reason when a decision is refused", async () => {
    mockIPC(
      (cmd) => {
        if (cmd === "system_approval_list") return [approval()];
        if (cmd === "system_approval_grant") {
          throw new Error("approval request req-1 is 'denied', not 'pending'");
        }
        return undefined;
      },
      { shouldMockEvents: true },
    );
    renderGate();
    fireEvent.click(await screen.findByRole("button", { name: "Approve once" }));
    // The real reason — a decision that raced another window's must say so, not vanish.
    expect(await screen.findByRole("alert")).toHaveTextContent(/not 'pending'/);
  });
});
