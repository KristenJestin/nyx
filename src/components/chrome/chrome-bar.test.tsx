import { render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
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

  it("renders a drag region (data-tauri-drag-region)", () => {
    const { container } = render(<ChromeBar />);
    expect(container.querySelector("[data-tauri-drag-region]")).toBeInTheDocument();
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
