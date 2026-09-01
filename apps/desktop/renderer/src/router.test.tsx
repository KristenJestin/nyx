import { render, screen, waitFor } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { mockIPC } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it } from "vitest";

import { createAppRouter } from "./router";

/**
 * The app's OWN router (`createAppRouter`) mounted at each of the 3 routes, proving they are
 * really navigable (no dead link) and each renders its page in the shell's `<Outlet/>`. A
 * bare backend serves the project tree the shell rail reads.
 */
function installBackend() {
  mockIPC((cmd) => {
    switch (cmd) {
      case "list_projects":
        return [];
      case "list_workspaces":
        return [];
      case "spec_list":
        return [];
      default:
        return undefined;
    }
  });
}

function renderAt(path: string) {
  const router = createAppRouter([path]);
  return render(<RouterProvider router={router} />);
}

describe("app router", () => {
  beforeEach(() => {
    installBackend();
  });

  it("mounts the Terminals index route at `/`", async () => {
    renderAt("/");
    // No project entered (empty backend) → the Terminals placeholder shows.
    expect(await screen.findByText(/Enter a project/)).toBeTruthy();
    // The shell's section-nav is present.
    expect(screen.getByRole("link", { name: "Terminals" })).toBeTruthy();
  });

  it("mounts the Specs board at `/work`", async () => {
    renderAt("/work");
    // The board's own heading is "Specs"; the section-nav still carries a "Work" link.
    expect(await screen.findByRole("heading", { name: "Specs" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Work" })).toBeTruthy();
  });

  it("mounts the Knowledge route at `/knowledge`", async () => {
    renderAt("/knowledge");
    // The page is named `Ideas` everywhere in the UI — heading and rail entry alike; `knowledge`
    // stays the internal route name (`docs/product/README.md`).
    expect(await screen.findByRole("heading", { name: "Ideas" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Ideas" })).toBeTruthy();
  });

  it("accepts a `?terminal=<id>` deep-link on `/` (the Runtime-tab open-terminal seam)", async () => {
    // The validated `terminal` search param is the way to open a PRECISE terminal by id.
    const router = createAppRouter(["/?terminal=term-42"]);
    render(<RouterProvider router={router} />);
    // The index route still mounts (placeholder, empty backend) — the param did not break routing.
    expect(await screen.findByText(/Enter a project/)).toBeTruthy();
    // The param is parsed and narrowed onto the `/` route's search.
    expect(router.state.location.search).toMatchObject({ terminal: "term-42" });
  });

  it("accepts a `?command=<id>` deep-link on `/` (the Runtime-tab open-command seam)", async () => {
    // The validated `command` search param opens a PRECISE managed command's detail by id.
    const router = createAppRouter(["/?command=svc-42"]);
    render(<RouterProvider router={router} />);
    expect(await screen.findByText(/Enter a project/)).toBeTruthy();
    expect(router.state.location.search).toMatchObject({ command: "svc-42" });
  });

  it("keeps the shell rail + section-nav across every route", async () => {
    renderAt("/work");
    await waitFor(() => expect(screen.getByRole("link", { name: "Work" })).toBeTruthy());
    expect(screen.getByRole("link", { name: "Terminals" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Ideas" })).toBeTruthy();
    // The "Add project" affordance (never "Add workspace") is on the rail.
    expect(screen.getByRole("button", { name: "Add project" })).toBeTruthy();
  });
});
