import { useEffect, useRef } from "react";
import { mockIPC } from "@/bridge/test-harness";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { SettingsDialog, type SettingsDialogHandle } from "./settings-dialog";

/**
 * Unit coverage for the Settings modal (PRD-4 #3 + R-UI review + PRD-5 #44/#45/#46/#49).
 *
 * The modal is a left **section rail** (Integrations now, extensible) + a right
 * **detail pane**. The Integrations pane is a thin shell over three Tauri commands —
 * `integration_list` / `integration_install` / `integration_remove`. The nyx Claude
 * integration is now ONE bundled plugin (MCP server + session-capture hooks), so each
 * available provider has a SINGLE Install/Uninstall control (finding #45). We mock the
 * IPC layer and assert:
 *  - the section rail renders the Integrations entry and selecting it shows its detail
 *    pane (section navigation);
 *  - the four registry providers render; Claude Code's single Install/Uninstall button
 *    invokes the right command with the right `provider`; and the coming-soon providers
 *    (Codex/OpenCode/Custom) expose no action buttons;
 *  - clicking the single button shows the loading spinner + disabled state while the op is
 *    in flight, with the rest of the providers list still mounted (finding #49);
 *  - an install error (e.g. claude absent) is surfaced clearly (finding #49).
 *
 * The dialog loads the provider list ON THE OPEN EVENT (via its imperative `reload()`
 * handle), not from an effect watching `open` — so the test mirrors the real open path: a
 * tiny host opens the dialog and calls `reload()` exactly like `terminal-manager`'s
 * `onOpenSettings` does.
 */

/**
 * Mirror of `terminal-manager`'s open-event wiring: mount the dialog open and fire
 * `reload()` once, the way the gear-button handler does.
 */
function OpenSettings() {
  const ref = useRef<SettingsDialogHandle>(null);
  useEffect(() => {
    ref.current?.reload();
  }, []);
  return <SettingsDialog ref={ref} open onClose={() => {}} />;
}

interface IntegrationStatus {
  provider: string;
  label: string;
  installed: boolean;
  available: boolean;
}

/** The 4-provider list the Rust `integration_list` returns (bridge.rs). */
function defaultList(installed = false): IntegrationStatus[] {
  return [
    { provider: "claude_code", label: "Claude Code", installed, available: true },
    { provider: "codex", label: "Codex", installed: false, available: false },
    { provider: "opencode", label: "OpenCode", installed: false, available: false },
    { provider: "custom", label: "Custom", installed: false, available: false },
  ];
}

interface Backend {
  calls: { cmd: string; args: Record<string, unknown> }[];
  /** Mutable installed flag for claude_code so install/remove flip the list. */
  installed: boolean;
}

function claudeStatus(b: Backend): IntegrationStatus {
  return { provider: "claude_code", label: "Claude Code", installed: b.installed, available: true };
}

/** Install a mock IPC backend modelling the single-unit integration_* commands. */
function mockBackend(installed = false): Backend {
  const backend: Backend = { calls: [], installed };
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    backend.calls.push({ cmd, args: a });
    switch (cmd) {
      case "integration_list":
        return defaultList(backend.installed);
      case "integration_install":
      case "integration_remove": {
        if (a.provider !== "claude_code") {
          throw `provider '${String(a.provider)}' is not supported in v1`;
        }
        backend.installed = cmd === "integration_install";
        return claudeStatus(backend);
      }
      default:
        return null;
    }
  });
  return backend;
}

interface DeferredBackend extends Backend {
  /** Resolve the currently-pending install/remove op (lets the test hold it in flight). */
  resolveOp: () => void;
}

/**
 * A mock backend whose `integration_install` / `integration_remove` DO NOT resolve until
 * the test calls `resolveOp()`. This lets the test observe the in-flight state — the
 * spinner on the single button — deterministically, before the op settles.
 */
function mockDeferredBackend(installed = false): DeferredBackend {
  const backend = { calls: [], installed, resolveOp: () => {} } as DeferredBackend;
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    backend.calls.push({ cmd, args: a });
    switch (cmd) {
      case "integration_list":
        return defaultList(backend.installed);
      case "integration_install":
      case "integration_remove":
        return new Promise((resolve) => {
          backend.resolveOp = () => {
            backend.installed = cmd === "integration_install";
            resolve(claudeStatus(backend));
          };
        });
      default:
        return null;
    }
  });
  return backend;
}

/** A mock backend whose install/remove ALWAYS fail with a string error (claude absent). */
function mockFailingBackend(message: string): Backend {
  const backend: Backend = { calls: [], installed: false };
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    backend.calls.push({ cmd, args: a });
    switch (cmd) {
      case "integration_list":
        return defaultList(backend.installed);
      case "integration_install":
      case "integration_remove":
        throw message;
      default:
        return null;
    }
  });
  return backend;
}

describe("<SettingsDialog> — Integrations", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders all four registry providers when opened", async () => {
    mockBackend();
    render(<OpenSettings />);

    await waitFor(() => {
      expect(screen.getByText("Claude Code")).toBeInTheDocument();
    });
    expect(screen.getByText("Codex")).toBeInTheDocument();
    expect(screen.getByText("OpenCode")).toBeInTheDocument();
    expect(screen.getByText("Custom")).toBeInTheDocument();
  });

  it("shows a 'Coming soon' badge and no action button for Codex/OpenCode/Custom", async () => {
    mockBackend();
    render(<OpenSettings />);

    await waitFor(() => expect(screen.getByText("Codex")).toBeInTheDocument());

    // Three coming-soon badges (codex, opencode, custom).
    expect(screen.getAllByText("Coming soon")).toHaveLength(3);

    // Only Claude Code (available) exposes a single Install action.
    expect(screen.getAllByRole("button", { name: "Install" })).toHaveLength(1);
    expect(screen.queryByRole("button", { name: "Uninstall" })).not.toBeInTheDocument();
  });

  it("the single Install button invokes integration_install with the provider", async () => {
    const backend = mockBackend(false);
    render(<OpenSettings />);

    const installBtn = await screen.findByRole("button", { name: "Install" });
    installBtn.click();

    await waitFor(() => {
      expect(backend.calls.some((c) => c.cmd === "integration_install")).toBe(true);
    });
    const call = backend.calls.find((c) => c.cmd === "integration_install");
    expect(call?.args.provider).toBe("claude_code");
    // No `component` arg anymore — it is one unit.
    expect(call?.args.component).toBeUndefined();

    // After install the button flips to Uninstall and the Installed badge appears.
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Uninstall" })).toBeInTheDocument();
    });
    expect(screen.getByText("Installed")).toBeInTheDocument();
  });

  it("the single Uninstall button invokes integration_remove with the provider", async () => {
    const backend = mockBackend(true);
    render(<OpenSettings />);

    const uninstallBtn = await screen.findByRole("button", { name: "Uninstall" });
    // Installed badge present while installed.
    expect(screen.getByText("Installed")).toBeInTheDocument();
    uninstallBtn.click();

    await waitFor(() => {
      expect(backend.calls.some((c) => c.cmd === "integration_remove")).toBe(true);
    });
    const call = backend.calls.find((c) => c.cmd === "integration_remove");
    expect(call?.args.provider).toBe("claude_code");

    // After uninstall the button flips back to Install.
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Install" })).toBeInTheDocument();
    });
  });

  it("only Claude Code is available; the coming-soon rows carry no buttons", async () => {
    mockBackend();
    render(<OpenSettings />);

    await waitFor(() => expect(screen.getByText("Claude Code")).toBeInTheDocument());

    for (const label of ["Codex", "OpenCode", "Custom"]) {
      const card = screen.getByText(label).closest("li");
      expect(card).toBeTruthy();
      expect(within(card as HTMLElement).queryByRole("button")).not.toBeInTheDocument();
    }

    const claudeCard = screen.getByText("Claude Code").closest("li");
    expect(within(claudeCard as HTMLElement).getAllByRole("button").length).toBeGreaterThan(0);
  });

  it("surfaces an install error clearly (claude absent)", async () => {
    const message = "the 'claude' CLI was not found on PATH — install Claude Code";
    mockFailingBackend(message);
    render(<OpenSettings />);

    const installBtn = await screen.findByRole("button", { name: "Install" });
    fireEvent.click(installBtn);

    // The error message is rendered verbatim, and the button returns to Install (not stuck).
    await waitFor(() => expect(screen.getByText(message)).toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Install" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Install" })).not.toBeDisabled();
  });
});

describe("<SettingsDialog> — section rail navigation", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders the section rail with an Integrations entry that is selected by default", async () => {
    mockBackend();
    render(<OpenSettings />);

    const rail = screen.getByRole("navigation", { name: "Settings sections" });
    const integrationsTab = within(rail).getByRole("button", { name: "Integrations" });
    expect(integrationsTab).toHaveAttribute("aria-current", "page");

    await waitFor(() => expect(screen.getByText("Claude Code")).toBeInTheDocument());
  });

  it("keeps the Integrations section selected after re-clicking its rail entry", async () => {
    mockBackend();
    render(<OpenSettings />);

    const rail = screen.getByRole("navigation", { name: "Settings sections" });
    const integrationsTab = within(rail).getByRole("button", { name: "Integrations" });

    await waitFor(() => expect(screen.getByText("Claude Code")).toBeInTheDocument());

    fireEvent.click(integrationsTab);
    expect(integrationsTab).toHaveAttribute("aria-current", "page");
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
  });
});

describe("<SettingsDialog> — button loading", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("shows the spinner ON the Install button while the op is in flight, list stays mounted", async () => {
    const backend = mockDeferredBackend(false);
    render(<OpenSettings />);

    const installBtn = await screen.findByRole("button", { name: "Install" });
    fireEvent.click(installBtn);

    // The clicked button carries the Button's built-in loading markers and is disabled.
    await waitFor(() => {
      expect(installBtn).toHaveAttribute("data-loading", "");
    });
    expect(installBtn).toBeDisabled();
    expect(within(installBtn).getByRole("status", { name: "Loading" })).toBeInTheDocument();

    // The rest of the list is STILL MOUNTED during the op — no full-pane flash.
    expect(screen.getByText("Codex")).toBeInTheDocument();
    // Exactly ONE in-flight spinner (the single clicked button).
    expect(screen.getAllByRole("status", { name: "Loading" })).toHaveLength(1);

    // Resolve the op: the list re-pulls and the button flips to Uninstall.
    await act(async () => {
      backend.resolveOp();
    });
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Uninstall" })).toBeInTheDocument();
    });
  });

  it("shows the spinner ON the Uninstall button while the op is in flight", async () => {
    const backend = mockDeferredBackend(true);
    render(<OpenSettings />);

    const removeBtn = await screen.findByRole("button", { name: "Uninstall" });
    fireEvent.click(removeBtn);

    await waitFor(() => {
      expect(removeBtn).toHaveAttribute("data-loading", "");
    });
    expect(removeBtn).toBeDisabled();
    expect(screen.getByText("Codex")).toBeInTheDocument();
    expect(screen.getAllByRole("status", { name: "Loading" })).toHaveLength(1);

    await act(async () => {
      backend.resolveOp();
    });
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Install" })).toBeInTheDocument();
    });
  });
});
