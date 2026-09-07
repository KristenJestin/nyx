import { render, screen, waitFor, within } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import { createQueryWrapper } from "@/lib/test-query";
import { saveScopeKind } from "@/components/terminals/ui-persistence";

import type { SpecDetail } from "./types";

/**
 * THE SPEC DETAIL NAMES ITS PROJECT IN CLEAR, IN THE CROSS-PROJECT SCOPE.
 *
 * The board marks its cards, so a Spec opened from the transverse flow LOSES that answer the
 * moment the detail replaces the list: the breadcrumb's `BETA-1` is a key to decode, and a
 * detail — unlike a row — has no neighbour to read a colour against. The user's ruling
 * (« dans le detail d'une spec et d'une idea il faut aussi afficher le projet en clair quand on
 * est en "all" ») is that the NAME must be legible, not guessable.
 *
 * Three things this file pins:
 *
 *  1. in the cross-project scope the context rail names the project AS TEXT — the mark is welcome
 *     beside it but is not what satisfies the requirement, so the assertion is on the name;
 *  2. in the project scope nothing of the sort appears — the rail already says where you are;
 *  3. THE NAME SURVIVES AN UNANCHORED SPEC. This is the reason the read model had to be extended
 *     Rust-side: the only project the detail could reach was `workspace_context.project`, which is
 *     `null` on every Spec not yet posed on a dev workspace (and carries no accent/icon/image
 *     anyway). The identity now comes off the Spec's OWNER project, so a draft with no worktree
 *     still says whose it is.
 */

const BETA_PROJECT = {
  id: "p2",
  name: "Beta",
  accent: "#f97316",
  icon: null,
  image_path: null,
} as const;

/**
 * A Spec of `Beta` with NO workspace anchor — deliberately: it is the shape that used to carry no
 * project at all, and it is a perfectly normal Spec (a draft nobody has posed yet).
 */
function detail(overrides: Partial<SpecDetail> = {}): SpecDetail {
  return {
    spec: {
      id: "s2",
      project_id: "p2",
      workspace_id: null,
      title: "Stream the tokens",
      slug: null,
      number: 1,
      priority: "med",
      status: "draft",
      mode: "stepped",
      phases: "[]",
      current_revision_number: 1,
      position: 0,
      created_at: 0,
      updated_at: 0,
      completed_at: null,
      archived_at: null,
      // PDR-0017: uncategorized is the steady state, and the field is REQUIRED on the wire shape.
      category_id: null,
    },
    revision: {
      id: "r1",
      spec_id: "s2",
      revision_number: 1,
      revision_status: "active",
      title: "Stream the tokens",
      problem: "The problem statement.",
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
    project: BETA_PROJECT,
    // PDR-0017: the RESOLVED category of `spec.category_id` — `null` for an uncategorized Spec.
    category: null,
    workspace_context: null,
    ...overrides,
  };
}

function installBackend(spec: SpecDetail) {
  mockIPC((cmd) => {
    switch (cmd) {
      case "list_projects":
        return [];
      case "list_workspaces":
        return [];
      case "spec_detail":
        return spec;
      case "spec_activity":
        return { spec_id: "s2", entries: [] };
      case "engine_spec_runs":
        return [];
      default:
        return undefined;
    }
  });
}

function mount() {
  const router = createAppRouter(["/work/s2"]);
  // The Review tab's hooks are TanStack Query (PDR-0029) — render under a fresh query client.
  const { wrapper } = createQueryWrapper();
  return render(<RouterProvider router={router} />, { wrapper });
}

/** The context rail, once the detail has landed. */
async function rail(): Promise<HTMLElement> {
  return await screen.findByLabelText("Spec context");
}

describe("The Spec detail names its project in the cross-project scope", () => {
  beforeEach(() => {
    saveScopeKind("project");
  });

  it("writes the project's NAME in the context rail, with its mark beside it", async () => {
    saveScopeKind("all");
    installBackend(detail());
    mount();

    const panel = await rail();
    // THE NAME, as TEXT — this is the criterion. A mark alone would not satisfy it.
    await waitFor(() =>
      expect(screen.getByTestId("spec-detail-project-name").textContent).toBe("Beta"),
    );
    // It lives IN the existing metadata panel, as one more of its sections — not in a banner of
    // its own bolted on somewhere else on the page.
    expect(panel.contains(screen.getByTestId("spec-detail-project-name"))).toBe(true);
    expect(within(panel).getByText("Project")).toBeTruthy();
    // …and the mark accompanies the name (the home header's couple), without replacing it.
    expect(panel.contains(screen.getByTestId("spec-detail-project-mark"))).toBe(true);
  });

  /**
   * The point of the Rust extension. This Spec has `workspace_context: null` — the ONLY project
   * the detail could read before — and it still names its project, because the identity comes off
   * the Spec's own `project_id`.
   */
  it("names it even on a Spec with no dev workspace — where the derived context is null", async () => {
    saveScopeKind("all");
    installBackend(detail({ workspace_context: null }));
    mount();

    expect((await screen.findByTestId("spec-detail-project-name")).textContent).toBe("Beta");
  });

  it("says nothing about the project in the project scope — the rail already does", async () => {
    installBackend(detail());
    mount();

    // The page is up…
    await screen.findByLabelText("Spec context");
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: "Stream the tokens" })).toBeTruthy(),
    );
    // …and carries no project mention at all.
    expect(screen.queryByTestId("spec-detail-project-name")).toBeNull();
    expect(screen.queryByTestId("spec-detail-project-mark")).toBeNull();
  });

  it("names nothing for a Spec whose project row has vanished — no empty field, no filler", async () => {
    saveScopeKind("all");
    installBackend(detail({ project: null }));
    mount();

    const panel = await rail();
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: "Stream the tokens" })).toBeTruthy(),
    );
    expect(screen.queryByTestId("spec-detail-project-name")).toBeNull();
    expect(within(panel).queryByText("Project")).toBeNull();
  });
});
