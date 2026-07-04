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

  it("mounts the Work route at `/work`", async () => {
    renderAt("/work");
    expect(await screen.findByRole("heading", { name: "Work" })).toBeTruthy();
  });

  it("mounts the Knowledge route at `/knowledge`", async () => {
    renderAt("/knowledge");
    expect(await screen.findByRole("heading", { name: "Knowledge" })).toBeTruthy();
  });

  it("keeps the shell rail + section-nav across every route", async () => {
    renderAt("/work");
    await waitFor(() => expect(screen.getByRole("link", { name: "Work" })).toBeTruthy());
    expect(screen.getByRole("link", { name: "Terminals" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Knowledge" })).toBeTruthy();
    // The "Add project" affordance (never "Add workspace") is on the rail.
    expect(screen.getByRole("button", { name: "Add project" })).toBeTruthy();
  });
});
