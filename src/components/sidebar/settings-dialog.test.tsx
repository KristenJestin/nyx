import { useEffect, useRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";

import { SettingsDialog, type SettingsDialogHandle } from "./settings-dialog";

/**
 * Unit coverage for the Settings modal (PRD-4 #3 + R-UI review).
 *
 * The modal is a left **section rail** (Integrations now, extensible) + a right
 * **detail pane**. The Integrations pane is a thin shell over three Tauri
 * commands — `integration_list` / `integration_install` / `integration_remove`.
 * We mock the IPC layer and assert:
 *  - the section rail renders the Integrations entry and selecting it shows its
 *    detail pane (section navigation);
 *  - the four registry providers render, Claude Code's Install/Remove buttons
 *    invoke the right command with the right `provider` arg, and the coming-soon
 *    providers (Codex/OpenCode/Custom) expose no action buttons;
 *  - clicking Install/Remove shows the loading spinner ON THAT button while the
 *    op is in flight, with the rest of the providers list still mounted (no
 *    full-modal flash / reload).
 *
 * The dialog loads the provider list ON THE OPEN EVENT (via its imperative
 * `reload()` handle), not from an effect watching `open` — so the test mirrors
 * the real open path: a tiny host opens the dialog and calls `reload()` exactly
 * like `terminal-manager`'s `onOpenSettings` does.
 */

/**
 * Mirror of `terminal-manager`'s open-event wiring: mount the dialog open and
 * fire `reload()` once, the way the gear-button handler does. Using a mount
 * effect here drives the SAME imperative path the production event handler
 * uses; it is not the pattern under test (the component itself carries no
 * open-watching effect).
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
function defaultList(claudeInstalled = false): IntegrationStatus[] {
  return [
    { provider: "claude_code", label: "Claude Code", installed: claudeInstalled, available: true },
    { provider: "codex", label: "Codex", installed: false, available: false },
    { provider: "opencode", label: "OpenCode", installed: false, available: false },
    { provider: "custom", label: "Custom", installed: false, available: false },
  ];
}

interface Backend {
  calls: { cmd: string; args: Record<string, unknown> }[];
  /** Mutable installed flag for claude_code so install/remove can flip the list. */
  claudeInstalled: boolean;
}

/** Install a mock IPC backend modelling the integration_* commands. */
function mockBackend(claudeInstalled = false): Backend {
  const backend: Backend = { calls: [], claudeInstalled };
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    backend.calls.push({ cmd, args: a });
    switch (cmd) {
      case "integration_list":
        return defaultList(backend.claudeInstalled);
      case "integration_install":
        if (a.provider === "claude_code") {
          backend.claudeInstalled = true;
          return {
            provider: "claude_code",
            label: "Claude Code",
            installed: true,
            available: true,
          };
        }
        throw `provider '${String(a.provider)}' is not supported in v1`;
      case "integration_remove":
        if (a.provider === "claude_code") {
          backend.claudeInstalled = false;
          return {
            provider: "claude_code",
            label: "Claude Code",
            installed: false,
            available: true,
          };
        }
        throw `provider '${String(a.provider)}' is not supported in v1`;
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
 * A mock backend whose `integration_install` / `integration_remove` DO NOT
 * resolve until the test calls `resolveOp()`. This lets the test observe the
 * in-flight state — the spinner on the clicked button — deterministically,
 * before the op settles and the list re-pulls.
 */
function mockDeferredBackend(claudeInstalled = false): DeferredBackend {
  const backend = { calls: [], claudeInstalled, resolveOp: () => {} } as DeferredBackend;
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    backend.calls.push({ cmd, args: a });
    switch (cmd) {
      case "integration_list":
        return defaultList(backend.claudeInstalled);
      case "integration_install":
      case "integration_remove":
        return new Promise((resolve) => {
          backend.resolveOp = () => {
            backend.claudeInstalled = cmd === "integration_install";
            resolve({
              provider: "claude_code",
              label: "Claude Code",
              installed: backend.claudeInstalled,
              available: true,
            });
          };
        });
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

    // The list loads asynchronously via integration_list.
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

    // None of the disabled providers expose Install/Remove. Only Claude Code
    // (available) does → exactly one Install button overall.
    expect(screen.getAllByRole("button", { name: "Install" })).toHaveLength(1);
    expect(screen.queryByRole("button", { name: "Remove" })).not.toBeInTheDocument();
  });

  it("Install for Claude Code invokes integration_install with the right provider", async () => {
    const backend = mockBackend(false);
    render(<OpenSettings />);

    const installBtn = await screen.findByRole("button", { name: "Install" });
    installBtn.click();

    await waitFor(() => {
      expect(backend.calls.some((c) => c.cmd === "integration_install")).toBe(true);
    });
    const call = backend.calls.find((c) => c.cmd === "integration_install");
    expect(call?.args.provider).toBe("claude_code");

    // After a successful install the list re-pulls and the button flips to Remove.
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Remove" })).toBeInTheDocument();
    });
  });

  it("Remove for an installed Claude Code invokes integration_remove with the right provider", async () => {
    const backend = mockBackend(true);
    render(<OpenSettings />);

    const removeBtn = await screen.findByRole("button", { name: "Remove" });
    // An "Installed" success badge accompanies the installed state.
    expect(screen.getByText("Installed")).toBeInTheDocument();
    removeBtn.click();

    await waitFor(() => {
      expect(backend.calls.some((c) => c.cmd === "integration_remove")).toBe(true);
    });
    const call = backend.calls.find((c) => c.cmd === "integration_remove");
    expect(call?.args.provider).toBe("claude_code");

    // After a successful remove the list re-pulls and the button flips to Install.
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Install" })).toBeInTheDocument();
    });
  });

  it("only Claude Code is available; the coming-soon rows carry no buttons", async () => {
    mockBackend();
    render(<OpenSettings />);

    await waitFor(() => expect(screen.getByText("Claude Code")).toBeInTheDocument());

    // Locate each coming-soon row by its label, walk up to the provider card
    // (the <li>), and assert the card carries no action button.
    for (const label of ["Codex", "OpenCode", "Custom"]) {
      const card = screen.getByText(label).closest("li");
      expect(card).toBeTruthy();
      expect(within(card as HTMLElement).queryByRole("button")).not.toBeInTheDocument();
    }

    // And the available Claude Code card DOES carry an action button (sanity:
    // the helper above would also pass if every card lacked buttons).
    const claudeCard = screen.getByText("Claude Code").closest("li");
    expect(within(claudeCard as HTMLElement).getByRole("button")).toBeInTheDocument();
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

    // The rail is a labelled <nav> region; Integrations is its (only, for now)
    // entry, and it is current/selected on open.
    const rail = screen.getByRole("navigation", { name: "Settings sections" });
    const integrationsTab = within(rail).getByRole("button", { name: "Integrations" });
    expect(integrationsTab).toHaveAttribute("aria-current", "page");

    // Selecting the section shows the Integrations detail pane (its providers).
    await waitFor(() => expect(screen.getByText("Claude Code")).toBeInTheDocument());
  });

  it("keeps the Integrations section selected after re-clicking its rail entry", async () => {
    mockBackend();
    render(<OpenSettings />);

    const rail = screen.getByRole("navigation", { name: "Settings sections" });
    const integrationsTab = within(rail).getByRole("button", { name: "Integrations" });

    await waitFor(() => expect(screen.getByText("Claude Code")).toBeInTheDocument());

    // Clicking the already-selected section keeps the pane mounted (navigation
    // is idempotent — re-selecting does not blank the detail pane).
    fireEvent.click(integrationsTab);
    expect(integrationsTab).toHaveAttribute("aria-current", "page");
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
  });
});

describe("<SettingsDialog> — per-button loading", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("shows the spinner ON the Install button while in flight, list stays mounted", async () => {
    const backend = mockDeferredBackend(false);
    render(<OpenSettings />);

    const installBtn = await screen.findByRole("button", { name: "Install" });

    // Start the install — it hangs (deferred backend) so we can observe the
    // in-flight state.
    fireEvent.click(installBtn);

    // The CLICKED button carries the Button's built-in loading markers
    // (`data-loading` + its own spinner indicator), and is disabled while busy.
    await waitFor(() => {
      expect(installBtn).toHaveAttribute("data-loading", "");
    });
    expect(installBtn).toBeDisabled();
    expect(within(installBtn).getByRole("status", { name: "Loading" })).toBeInTheDocument();

    // The rest of the providers list is STILL MOUNTED during the op — no
    // full-modal/full-pane flash that would unmount the other rows.
    expect(screen.getByText("Codex")).toBeInTheDocument();
    expect(screen.getByText("OpenCode")).toBeInTheDocument();
    expect(screen.getByText("Custom")).toBeInTheDocument();
    // There is exactly ONE in-flight button spinner (only the clicked button),
    // not a pane-level loader replacing the list.
    expect(screen.getAllByRole("status", { name: "Loading" })).toHaveLength(1);

    // Resolve the op: the list re-pulls and the button flips to Remove.
    await act(async () => {
      backend.resolveOp();
    });
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Remove" })).toBeInTheDocument();
    });
  });

  it("shows the spinner ON the Remove button while in flight, list stays mounted", async () => {
    const backend = mockDeferredBackend(true);
    render(<OpenSettings />);

    const removeBtn = await screen.findByRole("button", { name: "Remove" });
    fireEvent.click(removeBtn);

    await waitFor(() => {
      expect(removeBtn).toHaveAttribute("data-loading", "");
    });
    expect(removeBtn).toBeDisabled();

    // The other (coming-soon) rows stay mounted while the remove is in flight.
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
