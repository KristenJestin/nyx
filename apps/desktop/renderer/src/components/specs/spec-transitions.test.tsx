import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

import { SpecTransitions } from "./spec-transitions";
import type { SpecDetail } from "./types";

/**
 * `<SpecTransitions>` — the Spec detail's LIFECYCLE controls (PDR-0031).
 *
 * What these tests hold, and what they deliberately do not: every RULE of the state machine lives
 * in `nyx_core::db::transition_spec_with_workspace` and is covered there — the legal edges, the
 * pre-ready and pre-done validations, the confirmation guard-rail. What is proven HERE is the
 * surface's own contract:
 *   * only the transitions LEGAL from the current status are offered (the edges transcribed from
 *     `SpecTransition::source_allowed`), and a terminal Spec offers none;
 *   * a SENSITIVE move (`ready` / `completed` / `cancelled`) sends NOTHING until its dialog is
 *     confirmed, and then sends `userConfirmed: true`;
 *   * an ordinary move (`in_progress` / `in_review`) fires straight off its button, and carries
 *     NO `userConfirmed` — the flag goes out with the dialog and nowhere else;
 *   * a refusal is shown in the BACKEND's own words, and the detail is not refreshed.
 */

/** A `spec_detail` fixture in `status`, optionally already anchored on a workspace. */
function detail(status: string, workspaceId: string | null = null): SpecDetail {
  return {
    spec: {
      id: "s1",
      project_id: "p1",
      workspace_id: workspaceId,
      title: "A spec",
      slug: null,
      number: 1,
      priority: "med",
      status,
      mode: "stepped",
      phases: "[]",
      category_id: null,
      current_revision_number: 1,
      position: 0,
      created_at: 0,
      updated_at: 0,
      completed_at: null,
      archived_at: null,
    },
    revision: {
      id: "r1",
      spec_id: "s1",
      revision_number: 1,
      revision_status: "active",
      title: "A spec",
      problem: "",
      solution: "",
      implementation_decisions: "",
      testing_decisions: "",
      user_stories: "",
      out_of_scope: "",
      change_reason: null,
      created_at: 0,
    },
    tasks: [],
    dependencies: [],
    workspace_context: null,
    category: null,
  };
}

/** Render the row over a scripted backend, capturing every command it sends. */
function mount(
  status: string,
  backend: (cmd: string, args?: Record<string, unknown>) => unknown = () => ({}),
  workspaceId: string | null = null,
) {
  const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
  const refreshes: number[] = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    return backend(cmd, args);
  });
  const { container } = render(
    <SpecTransitions
      detail={detail(status, workspaceId)}
      refresh={async () => {
        refreshes.push(1);
      }}
    />,
  );
  return { calls, refreshes, container };
}

/** The labels of the transition buttons currently offered (the row only — the dialog is portalled). */
function offered(container: HTMLElement): string[] {
  return Array.from(container.querySelectorAll("button")).map((b) => b.textContent?.trim() ?? "");
}

/** Every `spec_transition` the row actually sent. */
function transitions(calls: Array<{ cmd: string; args?: Record<string, unknown> }>) {
  return calls.filter((c) => c.cmd === "spec_transition");
}

/**
 * Press a button INSIDE the confirmation dialog. The dialog is portalled to the document root and
 * its confirm button deliberately repeats the row's label, so it is queried within the popup —
 * `screen` alone would see two buttons of that name.
 */
function pressInDialog(name: string) {
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name }));
}

describe("<SpecTransitions> — only the LEGAL edges are offered", () => {
  it("draft offers ready + cancel", () => {
    const { container } = mount("draft");
    expect(offered(container)).toEqual(["Mark ready", "Cancel Spec"]);
  });

  it("ready offers start + cancel", () => {
    const { container } = mount("ready");
    expect(offered(container)).toEqual(["Start work", "Cancel Spec"]);
  });

  it("in_progress offers review + cancel", () => {
    const { container } = mount("in_progress");
    expect(offered(container)).toEqual(["Send to review", "Cancel Spec"]);
  });

  it("in_review offers done + cancel", () => {
    const { container } = mount("in_review");
    expect(offered(container)).toEqual(["Mark done", "Cancel Spec"]);
  });

  // The guard's `Cancel` accepts draft / ready / in_progress / in_review as sources — and NOT
  // `blocked`, which is why a blocked Spec is offered nothing rather than a Cancel that would be
  // refused. The three terminal statuses are terminal.
  it.each(["blocked", "completed", "cancelled", "archived"])(
    "%s renders no row at all (no edge is legal from it)",
    (status) => {
      const { container } = mount(status);
      expect(container.querySelectorAll("button")).toHaveLength(0);
    },
  );

  it("an unknown wire status offers nothing rather than throwing", () => {
    const { container } = mount("some_status_from_the_future");
    expect(container.querySelectorAll("button")).toHaveLength(0);
  });
});

describe("<SpecTransitions> — the sensitive moves ask first", () => {
  it("a `ready` sends NOTHING until the dialog is confirmed, then carries userConfirmed", async () => {
    const { calls, refreshes, container } = mount("draft");

    fireEvent.click(screen.getByRole("button", { name: "Mark ready" }));
    // The dialog is up and the backend has not been touched.
    expect(await screen.findByText("Mark this Spec ready?")).toBeTruthy();
    expect(transitions(calls)).toHaveLength(0);

    // Confirming is what sends it — the confirm button lives in the portalled popup.
    pressInDialog("Mark ready");
    await waitFor(() => expect(transitions(calls)).toHaveLength(1));
    expect(transitions(calls)[0].args).toEqual({ specId: "s1", to: "ready", userConfirmed: true });
    // On success the detail is re-pulled and the dialog is gone.
    await waitFor(() => expect(refreshes).toHaveLength(1));
    expect(screen.queryByText("Mark this Spec ready?")).toBeNull();
    expect(offered(container).length).toBeGreaterThan(0);
  });

  it("dismissing the dialog sends nothing at all", async () => {
    const { calls } = mount("in_review");

    fireEvent.click(screen.getByRole("button", { name: "Mark done" }));
    expect(await screen.findByText("Mark this Spec done?")).toBeTruthy();
    pressInDialog("Cancel");

    await waitFor(() => expect(screen.queryByText("Mark this Spec done?")).toBeNull());
    expect(transitions(calls)).toHaveLength(0);
  });

  it("a `cancelled` is confirmed through the destructive path", async () => {
    const { calls } = mount("in_progress");

    fireEvent.click(screen.getByRole("button", { name: "Cancel Spec" }));
    expect(await screen.findByText("Cancel this Spec?")).toBeTruthy();
    expect(transitions(calls)).toHaveLength(0);

    const confirm = within(screen.getByRole("dialog")).getByRole("button", {
      name: "Cancel the Spec",
    });
    expect(confirm.className).toContain("bg-destructive");
    fireEvent.click(confirm);

    await waitFor(() => expect(transitions(calls)).toHaveLength(1));
    expect(transitions(calls)[0].args).toEqual({
      specId: "s1",
      to: "cancelled",
      userConfirmed: true,
    });
  });
});

describe("<SpecTransitions> — the ordinary moves fire directly", () => {
  it("`in_review` sends on the click, with no dialog and no confirmation flag", async () => {
    const { calls, refreshes } = mount("in_progress");

    fireEvent.click(screen.getByRole("button", { name: "Send to review" }));

    await waitFor(() => expect(transitions(calls)).toHaveLength(1));
    // `review` is the ONE routine transition: the backend asks for no confirmation, so none is sent.
    expect(transitions(calls)[0].args).toEqual({ specId: "s1", to: "in_review" });
    expect(screen.queryByRole("dialog")).toBeNull();
    await waitFor(() => expect(refreshes).toHaveLength(1));
  });

  it("`in_progress` fires directly and carries the Spec's OWN anchor when it has one", async () => {
    const { calls } = mount("ready", () => ({}), "w1");

    fireEvent.click(screen.getByRole("button", { name: "Start work" }));

    await waitFor(() => expect(transitions(calls)).toHaveLength(1));
    // `start` POSES the anchor in the same write, but it is NOT sensitive
    // (`db::SpecTransition::is_sensitive` names `ready`/`completed`/`cancelled`/`draft`, and
    // `start` is not among them), so no confirmation flag is sent on a gesture nobody was asked
    // about.
    expect(transitions(calls)[0].args).toEqual({
      specId: "s1",
      to: "in_progress",
      workspaceId: "w1",
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("`in_progress` omits the anchor when the Spec carries none — the backend resolves it", async () => {
    const { calls } = mount("ready");

    fireEvent.click(screen.getByRole("button", { name: "Start work" }));

    await waitFor(() => expect(transitions(calls)).toHaveLength(1));
    expect(transitions(calls)[0].args).toEqual({ specId: "s1", to: "in_progress" });
  });
});

describe("<SpecTransitions> — a refusal is never swallowed", () => {
  it("shows the backend's own sentence, and does not refresh", async () => {
    // The pre-ready refusal, exactly as `map_spec_transition_err` words it.
    const refusal = "spec is not ready: the pre-ready validations failed (see data.failures)";
    const { calls, refreshes } = mount("draft", (cmd) => {
      if (cmd === "spec_transition") throw new Error(refusal);
      return undefined;
    });

    fireEvent.click(screen.getByRole("button", { name: "Mark ready" }));
    await screen.findByText("Mark this Spec ready?");
    pressInDialog("Mark ready");

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(refusal);
    expect(transitions(calls)).toHaveLength(1);
    expect(refreshes).toHaveLength(0);
  });

  it("shuts every button while a move is in flight, so a double-submit cannot race", async () => {
    let release: ((spec: unknown) => void) | null = null;
    const { calls, container } = mount("in_progress", (cmd) =>
      cmd === "spec_transition"
        ? new Promise((resolve) => {
            release = resolve;
          })
        : undefined,
    );

    fireEvent.click(screen.getByRole("button", { name: "Send to review" }));
    await waitFor(() => expect(transitions(calls)).toHaveLength(1));

    // Every button of the row is disabled while the call is out.
    const buttons = Array.from(container.querySelectorAll("button"));
    expect(buttons.every((b) => b.hasAttribute("disabled"))).toBe(true);
    // A second click on the pressed button sends nothing more. It is reached BY POSITION: the
    // `<Button>` swaps its label for a busy indicator while it is loading, so it no longer answers
    // to its accessible name.
    fireEvent.click(buttons[0]);
    expect(transitions(calls)).toHaveLength(1);

    await waitFor(() => expect(release).not.toBeNull());
    release!({});
    await waitFor(() =>
      expect(
        Array.from(container.querySelectorAll("button")).some((b) => b.hasAttribute("disabled")),
      ).toBe(false),
    );
  });
});
