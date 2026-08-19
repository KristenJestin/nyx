import { render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ChromeBar } from "./chrome-bar";

/**
 * Mock the IPC. `controlsEnv` is the value the `window_controls_visible`
 * command (the runtime `NYX_WINDOW_CONTROLS` resolution) returns; defaults to
 * `true` (visible) so the bar renders controls unless a test asks otherwise.
 */
function installIpc(controlsEnv = true): void {
  mockIPC((cmd) => {
    if (typeof cmd === "string" && cmd.includes("is_maximized")) return false;
    if (typeof cmd === "string" && cmd.includes("window_controls_visible")) {
      return controlsEnv;
    }
    return null;
  });
}

describe("<ChromeBar>", () => {
  beforeEach(() => {
    installIpc();
  });
  afterEach(() => {
    // clearMocks runs in vitest.setup.ts afterEach.
  });

  /**
   * The bar IS the drag strip, via the `drag-region` class → `-webkit-app-region: drag`
   * (globals.css), the only protocol that moves a frameless Electron window. It used to
   * claim `data-tauri-drag-region`, which nothing has interpreted since Tauri left
   * (PDR-0006) — a bar that looked draggable and moved nothing. jsdom applies no
   * stylesheet, so the class is what we can assert here; the attribute must be GONE.
   */
  it("marks the whole bar as an Electron drag region (and no Tauri leftover)", () => {
    const { container } = render(<ChromeBar />);
    const bar = container.querySelector(".drag-region");
    expect(bar).toBeInTheDocument();
    expect(container.querySelector("[data-tauri-drag-region]")).toBeNull();
  });

  /**
   * …and the controls cut themselves back OUT of it (`no-drag-region` →
   * `-webkit-app-region: no-drag`). Without that opt-out the drag region swallows the
   * pointer and the three buttons never fire — the window would look controlled and be
   * just as stuck as with no bar at all.
   */
  it("keeps the controls cluster out of the drag region", () => {
    const { container } = render(<ChromeBar controlsVisible />);
    const cluster = container.querySelector(".no-drag-region");
    expect(cluster).toBeInTheDocument();
    // The opt-out is the ancestor of the three buttons, not a stray empty box.
    expect(cluster!.querySelectorAll("button").length).toBe(3);
  });

  it("renders the window controls when enabled (default)", () => {
    render(<ChromeBar controlsVisible />);
    expect(screen.getByRole("button", { name: /close/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /minimize/i })).toBeInTheDocument();
  });

  it("hides the window controls when controlsVisible is false", () => {
    render(<ChromeBar controlsVisible={false} />);
    expect(screen.queryByRole("button", { name: /close/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /minimize/i })).toBeNull();
  });

  it("shows the active item title discreetly when provided", () => {
    render(<ChromeBar title="zsh — ~/work" controlsVisible />);
    expect(screen.getByText("zsh — ~/work")).toBeInTheDocument();
  });

  it("contains NO tabs in the chrome (sidebar owns navigation)", () => {
    render(<ChromeBar controlsVisible />);
    expect(screen.queryByRole("tab")).toBeNull();
    expect(screen.queryByRole("tablist")).toBeNull();
  });

  it("shows controls by default (no prop) when NYX_WINDOW_CONTROLS is unset/visible", async () => {
    installIpc(true);
    render(<ChromeBar />);
    await waitFor(() => expect(screen.getByRole("button", { name: /close/i })).toBeInTheDocument());
  });

  it("hides controls by default (no prop) when NYX_WINDOW_CONTROLS=0", async () => {
    installIpc(false);
    render(<ChromeBar />);
    await waitFor(() => expect(screen.queryByRole("button", { name: /close/i })).toBeNull());
  });
});
