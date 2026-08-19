import { render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it } from "vitest";

import App from "./app";

/**
 * THE APP ITSELF, MOUNTED — the coverage hole that let the window chrome go missing.
 *
 * `<ChromeBar>` and `<WindowControls>` each had a full unit suite, and both stayed green
 * while NOTHING in the app ever mounted them: the window was created `frame: false`
 * (src/main/window.ts) with no title bar of its own, and under Windows — where the OS
 * gives an undecorated window no controls at all — it could not be moved, minimized,
 * maximized or closed by the mouse (review finding `01M04Z5GCYA3S0T0K6XE125KZS`).
 *
 * A test on the component could not have caught that, because the component was never
 * the problem. So this one renders the REAL `<App>` — the same root `main.tsx` renders,
 * with its own router, its own route tree and its own shell — and asserts the chrome is
 * THERE. It fails the moment the bar is unmounted from the tree again, whatever the
 * component's own suite says.
 */

/**
 * A bare backend: enough for the shell to render (empty project tree) plus the
 * `window_controls_visible` resolution the chrome bar performs on mount. `controls`
 * is what the main process answers from the OS env `NYX_WINDOW_CONTROLS`
 * (`=0` → false; unset/anything else → true, see src/main/env.ts).
 */
function installBackend(controls = true) {
  mockIPC((cmd) => {
    switch (cmd) {
      case "window_controls_visible":
        return controls;
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

describe("<App> window chrome", () => {
  beforeEach(() => {
    installBackend();
  });

  it("mounts the window chrome bar with its three controls", async () => {
    const { container } = render(<App />);

    // The drag strip is in the app's own tree (`drag-region` → `-webkit-app-region: drag`).
    await waitFor(() => expect(container.querySelector(".drag-region")).toBeInTheDocument());
    // …and it carries the three controls the frameless window has no other source for.
    expect(await screen.findByRole("button", { name: "Minimize window" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Maximize or restore window" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close window" })).toBeInTheDocument();
  });

  it("puts the chrome bar at the TOP of the window, above the project rail", async () => {
    const { container } = render(<App />);

    const bar = await waitFor(() => {
      const el = container.querySelector(".drag-region");
      expect(el).toBeInTheDocument();
      return el!;
    });
    // The rail is the shell's left column; "Add project" is its always-present affordance.
    const rail = await screen.findByRole("button", { name: "Add project" });
    // DOCUMENT_POSITION_FOLLOWING (4): the rail comes AFTER the bar — i.e. the bar is the
    // top strip and the rail hangs below it, not beside or under some page.
    expect(bar.compareDocumentPosition(rail) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("honours NYX_WINDOW_CONTROLS=0: the app renders the bar without the controls", async () => {
    // The main process resolved the env to "hidden"; the bar itself must survive (the
    // window still needs its drag strip), only the cluster goes.
    installBackend(false);
    const { container } = render(<App />);

    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Close window" })).not.toBeInTheDocument(),
    );
    expect(container.querySelector(".drag-region")).toBeInTheDocument();
  });
});
