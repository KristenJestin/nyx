import { useEffect, useRef } from "react";
import { mockIPC } from "@/bridge/test-harness";
import type { DetectedShell, IntegrationStatus, ShellChoice } from "@/bridge";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { SettingsDialog, type SettingsDialogHandle } from "./settings-dialog";

/**
 * Unit coverage for the Settings modal (PRD-4 #3 + R-UI review + PRD-5 #44/#45/#46/#49).
 *
 * The modal is a left **section rail** (Integrations now, extensible) + a right
 * **detail pane**. The Integrations pane is a thin shell over three sidecar commands —
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

/** The 4-provider list the Rust `integration_list` returns (`nyx_core::integrations`). */
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

  /**
   * A REFUSED LIST IS NOT AN EMPTY ONE (review finding `01M08B0T3BZMVW44KZAHPMRKZ2`).
   *
   * `integration_list` used to be caught into a silent block, so a sidecar that refused the call
   * rendered as "No integrations available." — a machine reported without ever being looked at,
   * the same confusion the Terminal section had to unlearn. The refusal is now a sentence.
   */
  it("surfaces an integration_list failure instead of reporting an empty machine", async () => {
    const message = "sidecar exited before answering";
    mockIPC((cmd) => {
      if (cmd === "integration_list") throw { kind: "command", message, source: "integration_list" };
      return null;
    });
    render(<OpenSettings />);

    await waitFor(() => expect(screen.getByText(message)).toBeInTheDocument());
    // The vacuity phrase belongs to a list that ANSWERED — and a BridgeError is an object, so the
    // one path that reads it is what keeps `[object Object]` off the screen.
    expect(screen.queryByText("No integrations available.")).not.toBeInTheDocument();
    expect(screen.queryByText(/\[object Object\]/)).not.toBeInTheDocument();
  });

  /**
   * A RE-READ THAT FAILS DROPS ITS LIST, exactly like `shell_list` (review finding
   * `01M08E0XSZG9MKV228ZA60YY70`).
   *
   * `integration_list` is re-fired on every modal open and after every install/remove. When that
   * re-read is refused, the cards already on screen are state the backend has just declined to
   * confirm — and they carry LIVE Install/Uninstall buttons, under a red line. The Terminal section
   * answers the same event by purging (`setShells(null)`); the two sections now answer it the same
   * way, which is what their shared doc claims.
   */
  it("drops the integration list when a RE-read fails, instead of leaving stale cards actionable", async () => {
    let reads = 0;
    const message = "sidecar exited before answering";
    mockIPC((cmd) => {
      switch (cmd) {
        case "integration_list":
          reads += 1;
          // The FIRST read answers (the cards mount); the re-read fired by `onRefresh` refuses.
          if (reads > 1) throw { kind: "command", message, source: "integration_list" };
          return defaultList(false);
        case "integration_install":
          return {
            provider: "claude_code",
            label: "Claude Code",
            installed: true,
            available: true,
          };
        default:
          return null;
      }
    });
    render(<OpenSettings />);

    const installBtn = await screen.findByRole("button", { name: "Install" });
    fireEvent.click(installBtn);

    await waitFor(() => expect(screen.getByText(message)).toBeInTheDocument());
    // Nothing of the unconfirmed list survives — no card, and therefore no button to act on it.
    expect(screen.queryByText("Claude Code")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Uninstall" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Install" })).not.toBeInTheDocument();
    // And the vacuity phrase stays reserved for a list that ANSWERED.
    expect(screen.queryByText("No integrations available.")).not.toBeInTheDocument();
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

  // --- the port diagnostic (task 01M1F8XMR4PRRCYMKYPVX7A0WG) -----------------------------
  //
  // The agent plugin is GLOBAL to the machine while nyx installs and worktrees are not, so an
  // installed integration can point at another nyx. Unsaid, that reaches the agent as a bare
  // `ConnectionRefused` naming nothing. This card is where it becomes visible BEFORE that.

  it("shows the backend diagnostic on an installed card, while keeping it installed", async () => {
    const diagnostic =
      "The installed Claude Code plugin points at port 8787, but this nyx is listening on 8766.";
    mockIPC((cmd) => {
      if (cmd !== "integration_list") return null;
      return [
        { provider: "claude_code", label: "Claude Code", installed: true, available: true, diagnostic },
        { provider: "codex", label: "Codex", installed: false, available: false },
        { provider: "opencode", label: "OpenCode", installed: false, available: false },
        { provider: "custom", label: "Custom", installed: false, available: false },
      ] satisfies IntegrationStatus[];
    });
    render(<OpenSettings />);

    // Said verbatim — the two ports are the whole point, so nothing may reword it.
    await waitFor(() => expect(screen.getByText(diagnostic)).toBeInTheDocument());
    // Still INSTALLED: the integration is not broken, it is aimed elsewhere.
    expect(screen.getByText("Installed")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Uninstall" })).toBeInTheDocument();
  });

  it("says nothing when the backend reports no diagnostic", async () => {
    mockBackend(true);
    render(<OpenSettings />);

    await waitFor(() => expect(screen.getByText("Installed")).toBeInTheDocument());
    expect(screen.queryByText(/listening on/)).not.toBeInTheDocument();
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

// ---------------------------------------------------------------------------
// Terminal section — the default-shell preference
// ---------------------------------------------------------------------------

/**
 * The shells a Windows host would report (`shell_list`), including a WSL distribution — the ONE
 * entry that carries `args`, and therefore the one that proves the picker hands the backend the
 * WHOLE choice rather than a path.
 */
const DETECTED_SHELLS: DetectedShell[] = [
  {
    id: "windows-powershell",
    label: "Windows PowerShell",
    path: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
    args: [],
  },
  { id: "cmd", label: "Command Prompt", path: "C:\\Windows\\System32\\cmd.exe", args: [] },
  {
    id: "wsl:Ubuntu",
    label: "Ubuntu (WSL)",
    path: "C:\\Windows\\System32\\wsl.exe",
    args: ["-d", "Ubuntu"],
  },
];

interface ShellBackend {
  calls: { cmd: string; args: Record<string, unknown> }[];
  /** The PERSISTED preference the backend hands back — mutated by `set_default_shell`, so a
   *  successful write followed by the modal's re-pull reads the stored truth, exactly like the
   *  real `app_settings` round-trip. */
  defaultShell: ShellChoice | null;
}

/**
 * A mock backend for the three Terminal-section commands, modelling the REAL persistence: a
 * `set_default_shell` write is what the following `app_settings_get` reads back (`path: null`
 * clears it). `integration_list` is stubbed so the modal's other section loads too.
 */
function mockShellBackend(
  initial: ShellChoice | null = null,
  shells: DetectedShell[] = DETECTED_SHELLS,
): ShellBackend {
  const backend: ShellBackend = { calls: [], defaultShell: initial };
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    backend.calls.push({ cmd, args: a });
    switch (cmd) {
      case "integration_list":
        return defaultList(false);
      case "shell_list":
        return shells;
      case "app_settings_get":
        return { defaultShell: backend.defaultShell };
      case "set_default_shell": {
        const path = a.path as string | null;
        backend.defaultShell = path ? { path, args: (a.args as string[] | undefined) ?? [] } : null;
        return null;
      }
      default:
        return null;
    }
  });
  return backend;
}

/** Open the modal and switch to the Terminal section, returning its `<select>`. */
async function openTerminalSection(): Promise<HTMLSelectElement> {
  render(<OpenSettings />);
  const rail = screen.getByRole("navigation", { name: "Settings sections" });
  fireEvent.click(within(rail).getByRole("button", { name: "Terminal" }));
  return (await screen.findByRole("combobox", { name: "Default shell" })) as HTMLSelectElement;
}

describe("<SettingsDialog> — Terminal section", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("exposes a Terminal entry in the rail, next to Integrations", async () => {
    mockShellBackend();
    render(<OpenSettings />);

    const rail = screen.getByRole("navigation", { name: "Settings sections" });
    expect(within(rail).getByRole("button", { name: "Integrations" })).toBeInTheDocument();
    expect(within(rail).getByRole("button", { name: "Terminal" })).toBeInTheDocument();

    // Integrations stays the default selection — the new section is added, not substituted.
    expect(within(rail).getByRole("button", { name: "Integrations" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    await waitFor(() => expect(screen.getByText("Claude Code")).toBeInTheDocument());
  });

  it("lists the DETECTED shells plus the Auto option, and nothing else", async () => {
    mockShellBackend();
    const select = await openTerminalSection();

    const labels = Array.from(select.options).map((o) => o.textContent);
    expect(labels).toEqual([
      "Auto (system default)",
      "Windows PowerShell",
      "Command Prompt",
      "Ubuntu (WSL)",
    ]);
    // Nothing chosen yet → Auto is what is selected.
    expect(select.value).toBe("");
  });

  it("persists the pick through set_default_shell, carrying its path AND its args", async () => {
    const backend = mockShellBackend();
    const select = await openTerminalSection();

    // The WSL entry: the one whose identity is a program PLUS arguments.
    fireEvent.change(select, { target: { value: "wsl:Ubuntu" } });

    await waitFor(() => {
      expect(backend.calls.some((c) => c.cmd === "set_default_shell")).toBe(true);
    });
    const call = backend.calls.find((c) => c.cmd === "set_default_shell");
    expect(call?.args.path).toBe("C:\\Windows\\System32\\wsl.exe");
    expect(call?.args.args).toEqual(["-d", "Ubuntu"]);

    // The modal re-pulls after the write, so the select now shows the PERSISTED choice.
    await waitFor(() => expect(select.value).toBe("wsl:Ubuntu"));
  });

  it("re-reads the persisted preference on open (it survives a remount)", async () => {
    mockShellBackend({ path: "C:\\Windows\\System32\\cmd.exe", args: [] });
    const select = await openTerminalSection();

    // No local state carried this: the value comes from `app_settings_get`, which is what a
    // restarted app reads too.
    expect(select.value).toBe("cmd");
  });

  it("picking Auto CLEARS the preference (path: null), back to the system default", async () => {
    const backend = mockShellBackend({ path: "C:\\Windows\\System32\\cmd.exe", args: [] });
    const select = await openTerminalSection();
    expect(select.value).toBe("cmd");

    fireEvent.change(select, { target: { value: "" } });

    await waitFor(() => {
      expect(backend.calls.some((c) => c.cmd === "set_default_shell")).toBe(true);
    });
    expect(backend.calls.find((c) => c.cmd === "set_default_shell")?.args.path).toBeNull();
    await waitFor(() => expect(select.value).toBe(""));
    expect(backend.defaultShell).toBeNull();
  });

  it("flags a stored preference the detected list no longer holds, instead of reading as Auto", async () => {
    // Persisted months ago, uninstalled since: it matches NO detected shell.
    const gone = "C:\\Program Files\\Git\\bin\\bash.exe";
    mockShellBackend({ path: gone, args: [] });
    const select = await openTerminalSection();

    // NOT "" — the user must be able to tell "my choice is gone" from "I never chose".
    expect(select.value).not.toBe("");
    expect(screen.getByText(/no longer in the list detected on this machine/i)).toBeInTheDocument();
    // The orphaned choice is SHOWN (so it can be recognised) but not selectable.
    const orphan = Array.from(select.options).find(
      (o) => o.textContent === `${gone} (no longer detected)`,
    );
    expect(orphan).toBeTruthy();
    expect(orphan?.disabled).toBe(true);
  });

  /**
   * THE ORPHANED OPTION NAMES THE WHOLE CHOICE, AND CLAIMS NOTHING IT CANNOT (review findings
   * `01M08E04Z0CDZ3J9VKDZ4XFR1Q` / `01M08DSZ4Z6ABMDJV15E897QN4`).
   *
   * The stored preference is a WSL distribution that was REMOVED — `wsl.exe -d Debian` — while the
   * machine still lists another one at the very same path (`Ubuntu (WSL)`). It is the only case
   * where the path does not name the shell, i.e. the only case where the gesture these options
   * exist for ("show the user what they had picked") can fail, and it used to: the option read
   * `C:\Windows\System32\wsl.exe`, indistinguishable from the live entry two lines above.
   *
   * Nor may the screen conclude anything about the DISK from `shell_list`: `wsl.exe` is still
   * installed, so "no longer installed" is simply false — the binary is right there, it is the
   * DISTRIBUTION that is gone.
   *
   * And the screen still may not narrate the SPAWN either, though for a different reason than it
   * used to (review `01M0BT2SST6ZMG84SW1CW9BSWH`). `shells.rs`'s `resolve` no longer tests
   * `program_exists(path)` alone — it validates the pair, so this preference does now lose to the
   * historical chain instead of opening a terminal that dies. But that is the SIDECAR's verdict,
   * re-taken against the machine at the next spawn; this list is a snapshot the renderer holds,
   * and what it establishes is membership. The screen says the fact it read, and leaves the
   * spawn's outcome to the component that decides it.
   */
  it("names a removed WSL distribution in full, and claims nothing about the binary or the spawn", async () => {
    const removed = { path: "C:\\Windows\\System32\\wsl.exe", args: ["-d", "Debian"] };
    // DETECTED_SHELLS still holds `wsl:Ubuntu` at that same path.
    mockShellBackend(removed);
    const select = await openTerminalSection();

    const labels = Array.from(select.options).map((o) => o.textContent ?? "");
    expect(labels).toContain("Ubuntu (WSL)");
    const orphan = select.options[select.selectedIndex];
    expect(orphan?.textContent).toBe(
      "C:\\Windows\\System32\\wsl.exe -d Debian (no longer detected)",
    );
    expect(orphan?.disabled).toBe(true);

    // What is said is bounded by what the list established: membership, and nothing else.
    expect(screen.getByText(/no longer in the list detected on this machine/i)).toBeInTheDocument();
    expect(screen.queryByText(/no longer installed/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/not found/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/new terminals open on the system default/i)).not.toBeInTheDocument();
  });

  /**
   * THE REGRESSION, at the level it happened (review finding `01M083NBZW4ZXKNSGDTS7F14Y8`).
   *
   * Measured on a real machine: `shell_list` answered with five real shells while
   * `app_settings_get` rejected with `db error: no such table: app_settings`. The two reads were
   * awaited under ONE `Promise.all`, so the rejection threw away the list that had already
   * arrived, and the picker offered "Auto" alone — the exact symptom, with the backend blameless.
   *
   * The two reads answer two independent questions and must fail independently.
   */
  it("still lists the detected shells when the preference read fails", async () => {
    mockIPC((cmd) => {
      switch (cmd) {
        case "integration_list":
          return defaultList(false);
        case "shell_list":
          return DETECTED_SHELLS;
        case "app_settings_get":
          throw "db error: no such table: app_settings";
        default:
          return null;
      }
    });
    const select = await openTerminalSection();

    const labels = Array.from(select.options).map((o) => o.textContent);
    expect(labels).toEqual([
      "Auto (system default)",
      "Windows PowerShell",
      "Command Prompt",
      "Ubuntu (WSL)",
      // The list that ANSWERED is offered whole; the read that failed adds the option below, which
      // states that the pick is unknown rather than letting "Auto" answer for it.
      "Unknown (the saved default shell could not be read)",
    ]);
    expect(select.value).not.toBe("");
  });

  /**
   * The failure is on screen, read through `errorText` (review finding
   * `01M083P95W52AC7RWMNFV5GTG9`). The previous `catch {}` left no trace at all: the section
   * claimed to list the installed shells above a menu holding only "Auto".
   */
  it("surfaces a preference-read failure inline instead of swallowing it", async () => {
    const message = "db error: no such table: app_settings";
    mockIPC((cmd) => {
      switch (cmd) {
        case "integration_list":
          return defaultList(false);
        case "shell_list":
          return DETECTED_SHELLS;
        case "app_settings_get":
          throw message;
        default:
          return null;
      }
    });
    await openTerminalSection();

    await waitFor(() => expect(screen.getByText(new RegExp(message))).toBeInTheDocument());
    expect(screen.queryByText(/\[object Object\]/)).not.toBeInTheDocument();
  });

  /** The other half: a `shell_list` that fails says so too, instead of rendering as "this machine
   *  has no shell". A BridgeError is an OBJECT — `errorText` is what keeps it readable. */
  it("surfaces a shell_list failure inline, never as [object Object]", async () => {
    const message = "sidecar exited before answering";
    mockIPC((cmd) => {
      switch (cmd) {
        case "integration_list":
          return defaultList(false);
        case "shell_list":
          throw { kind: "command", message, source: "shell_list" };
        case "app_settings_get":
          return { defaultShell: null };
        default:
          return null;
      }
    });
    await openTerminalSection();

    await waitFor(() => expect(screen.getByText(new RegExp(message))).toBeInTheDocument());
    expect(screen.queryByText(/\[object Object\]/)).not.toBeInTheDocument();
    // A failed read is NOT "no shell detected" — the two must never be confused on screen.
    expect(screen.queryByText(/No shell was detected/i)).not.toBeInTheDocument();
  });

  /**
   * BOTH READS DOWN — the most common failure of all, since a missing sidecar or an unready bridge
   * rejects the two of them (review findings `01M08B7P4832P19RY8RAGCKGAE` / `01M08AZ7PK173VZTR8H4ZR6VX1`).
   *
   * Two unrelated failures are two statements, and the screen must read as two. Joined by a space
   * they became "sidecar exited before answering db error: no such table: app_settings" — a run-on
   * that quotes neither backend cleanly, at the moment the user has nothing else to go on.
   *
   * And the CONTROL itself is pinned here (review finding `01M08DYTE5CR2RG3B7GFYB2EVT`): with both
   * reads down, the select used to sit on "Auto (system default)" — the screen telling the user
   * they had picked nothing, on the one path where nobody managed to read what they picked. It is
   * the same unverified claim `UNVERIFIED_SHELL_VALUE` was introduced to forbid on the other half.
   */
  it("renders two failed reads as two separate messages, never glued into one sentence", async () => {
    const shellsMessage = "sidecar exited before answering";
    const settingsMessage = "db error: no such table: app_settings";
    mockIPC((cmd) => {
      switch (cmd) {
        case "integration_list":
          return defaultList(false);
        case "shell_list":
          throw { kind: "command", message: shellsMessage, source: "shell_list" };
        case "app_settings_get":
          throw settingsMessage;
        default:
          return null;
      }
    });
    const select = await openTerminalSection();

    // Each message is the WHOLE text of its own element — an exact `getByText` match fails the
    // moment the two are concatenated into one node.
    await waitFor(() => expect(screen.getByText(shellsMessage)).toBeInTheDocument());
    expect(screen.getByText(settingsMessage)).toBeInTheDocument();
    expect(screen.getByText(shellsMessage)).not.toBe(screen.getByText(settingsMessage));
    // And the glued form is nowhere on screen.
    expect(
      screen.queryByText(new RegExp(`${shellsMessage}\\s+${settingsMessage}`)),
    ).not.toBeInTheDocument();

    // THE SELECT, on the same rule: "Auto (system default)" is the statement "nothing was ever
    // picked", and this pass of the two reads established nothing at all. So the control names the
    // preference as unknown — and, being unknown, refuses to be re-submitted.
    expect(select.value).not.toBe("");
    const current = select.options[select.selectedIndex];
    expect(current?.textContent).toBe("Unknown (the saved default shell could not be read)");
    expect(current?.disabled).toBe(true);
    // The Auto option is still THERE (clearing the preference stays possible) — it is simply not
    // what the screen reports the user to have chosen.
    expect(Array.from(select.options).map((o) => o.textContent)).toContain("Auto (system default)");
  });

  /**
   * THE HALF THAT ANSWERED IS NOT THE OTHER HALF (review finding `01M08B6WV54AYQ0NJDNYFFWZZB`).
   *
   * `shell_list` fails while `app_settings_get` hands back a REAL stored preference. Nothing has
   * looked at the disk, so the section may not say the binary is gone: the sidecar resolves that
   * preference against `program_exists` at the next spawn (`shells.rs`), not against a probe that
   * never answered — the shell will simply open. Saying "no longer installed" here would be the
   * screen inventing a fact, which is the defect this whole surface was reworked to stop.
   */
  it("does not call a stored shell uninstalled when the shell list is the read that failed", async () => {
    const message = "sidecar exited before answering";
    const stored = "C:\\Windows\\System32\\cmd.exe";
    mockIPC((cmd) => {
      switch (cmd) {
        case "integration_list":
          return defaultList(false);
        case "shell_list":
          throw { kind: "command", message, source: "shell_list" };
        case "app_settings_get":
          return { defaultShell: { path: stored, args: [] } };
        default:
          return null;
      }
    });
    const select = await openTerminalSection();

    // The unread list IS said, once.
    await waitFor(() => expect(screen.getByText(message)).toBeInTheDocument());
    // …and nothing else is: no verdict on the binary, no "(not found)" option, and no claim that
    // the machine has no shell (the probe that would know is the one that failed).
    expect(screen.queryByText(/no longer installed/i)).not.toBeInTheDocument();
    const labels = Array.from(select.options).map((o) => o.textContent ?? "");
    expect(labels.some((label) => label.includes("(not found)"))).toBe(false);
    expect(screen.queryByText(/No shell was detected/i)).not.toBeInTheDocument();
    // The preference itself is still shown as what it is — an unread list is no reason to read back
    // as "Auto", which would say nyx forgot the pick.
    expect(select.value).not.toBe("");
    expect(labels).toContain(stored);
  });

  /**
   * A SENTINEL IS NOT A CHOICE (review finding `01M08DZF1AQMFTEANQG7JJ8JDG`).
   *
   * `__unverified__` / `__missing__` / `__unread__` are DISPLAY values: they name a state of the two
   * reads, and no detected shell carries them. Submitting one used to fall through `shells.find` as
   * `undefined`, i.e. exactly as "Auto" — a `set_default_shell` with `path: null` that ERASES the
   * preference the option was rendered to show, announced by a success toast. The `disabled`
   * attribute made that unreachable through a browser; nothing made it unreachable through the
   * handler, and nothing tested the attribute on this branch.
   *
   * The stored preference here is a WSL distribution, so the option is also the proof that the
   * unverified branch names the WHOLE choice — program AND args.
   */
  it("refuses a sentinel submission instead of erasing the preference it displays", async () => {
    const calls: string[] = [];
    const stored = { path: "C:\\Windows\\System32\\wsl.exe", args: ["-d", "Ubuntu"] };
    mockIPC((cmd) => {
      calls.push(cmd);
      switch (cmd) {
        case "integration_list":
          return defaultList(false);
        case "shell_list":
          throw {
            kind: "command",
            message: "sidecar exited before answering",
            source: "shell_list",
          };
        case "app_settings_get":
          return { defaultShell: stored };
        default:
          return null;
      }
    });
    const select = await openTerminalSection();

    // The preference is on screen, WHOLE — `wsl.exe` alone would name any distribution — and the
    // option is not selectable.
    const current = select.options[select.selectedIndex];
    expect(current?.textContent).toBe("C:\\Windows\\System32\\wsl.exe -d Ubuntu");
    expect(current?.disabled).toBe(true);
    expect(select.value).toBe("__unverified__");

    // Now submit it anyway — the gesture a missing `disabled` would let through.
    fireEvent.change(select, { target: { value: "__unverified__" } });

    await waitFor(() => expect(screen.getByText(/sidecar exited/i)).toBeInTheDocument());
    expect(calls).not.toContain("set_default_shell");
    // …and the display is untouched: still the stored choice, never snapped back to "Auto".
    expect(select.value).toBe("__unverified__");
  });

  /**
   * The mirror case of the same finding: `app_settings_get` fails while `shell_list` answers an
   * EMPTY list. The emptiness was measured, so it is still named — the failure of the other read
   * does not turn a legitimate "this machine has nothing else" into an unexplained empty menu.
   */
  it("still names an empty probe when the preference read is the one that failed", async () => {
    const message = "db error: no such table: app_settings";
    mockIPC((cmd) => {
      switch (cmd) {
        case "integration_list":
          return defaultList(false);
        case "shell_list":
          return [];
        case "app_settings_get":
          throw message;
        default:
          return null;
      }
    });
    const select = await openTerminalSection();

    expect(Array.from(select.options).map((o) => o.textContent)).toEqual([
      "Auto (system default)",
      // …and the preference read that failed is named as unknown, not as "Auto".
      "Unknown (the saved default shell could not be read)",
    ]);
    // Both facts on screen, side by side: the read that failed, and the probe that answered empty.
    await waitFor(() => expect(screen.getByText(new RegExp(message))).toBeInTheDocument());
    expect(screen.getByText(/No shell was detected/i)).toBeInTheDocument();
    // …and the empty list stops there: with the pick UNREAD, nothing establishes what the next
    // spawn resolves to, so the fallback half of that sentence is not said.
    expect(screen.queryByText(/new terminals open on the system default/i)).not.toBeInTheDocument();
  });

  /** And the legitimate empty case stays legible as itself: both reads answered, the machine
   *  simply offers nothing else. No error, but no silent empty menu either — and here the fallback
   *  IS established: the preference read answered "nothing picked", which is exactly the state in
   *  which `resolve` has no preference to prefer. */
  it("names an empty-but-successful probe as such, not as a failure", async () => {
    mockShellBackend(null, []);
    const select = await openTerminalSection();

    expect(Array.from(select.options).map((o) => o.textContent)).toEqual(["Auto (system default)"]);
    expect(
      screen.getByText(
        "No shell was detected on this machine — new terminals open on the system default.",
      ),
    ).toBeInTheDocument();
  });

  /**
   * AN EMPTY LIST DECIDES THE MENU, NOT THE NEXT SPAWN (review finding
   * `01M08GZHK8SHNN8C1NYRCPFY95`).
   *
   * The sibling of the sentence the previous pass already had to disarm, carrying the same half of
   * the same claim. Both paragraphs are JSX siblings on INDEPENDENT guards, so they co-occur: a
   * stored `/usr/bin/fish` absent from a list that answered EMPTY renders the orphaned option, the
   * warning above it AND "new terminals open on the system default" — which is false exactly there.
   * `resolve` (`shells.rs`) tests `program_exists("/usr/bin/fish")` alone, so the preference still
   * WINS and the next terminal is fish, not the system default.
   *
   * Reachable, not hypothetical: on Unix `detect_candidates` answers `Vec::new()` when `/etc/shells`
   * cannot be read (`shells.rs`), which the renderer reads as a MEASURED emptiness — `shells !== null`
   * with nothing in it — while the machine's shells and the stored pick are all still on disk.
   */
  it("does not promise the system default when an orphaned preference could still win the spawn", async () => {
    mockShellBackend({ path: "/usr/bin/fish", args: [] }, []);
    const select = await openTerminalSection();

    // The emptiness was MEASURED, so it is still named…
    expect(screen.getByText(/No shell was detected on this machine/i)).toBeInTheDocument();
    // …and nothing beyond it is: the spawn is `resolve`'s call, and the stored pick may still win.
    expect(screen.queryByText(/new terminals open on the system default/i)).not.toBeInTheDocument();
    // Nor is advice given by a menu that offers nothing to take it — "Auto" and the disabled orphan
    // are the whole list.
    expect(screen.queryByText(/pick another one/i)).not.toBeInTheDocument();
    expect(screen.getByText(/no longer in the list detected on this machine/i)).toBeInTheDocument();
    // The pick itself is still shown as itself, whole and unselectable.
    const orphan = select.options[select.selectedIndex];
    expect(orphan?.textContent).toBe("/usr/bin/fish (no longer detected)");
    expect(orphan?.disabled).toBe(true);
  });

  it("surfaces a write failure inline instead of swallowing it", async () => {
    const message = "db error: database is locked";
    mockIPC((cmd) => {
      switch (cmd) {
        case "integration_list":
          return defaultList(false);
        case "shell_list":
          return DETECTED_SHELLS;
        case "app_settings_get":
          return { defaultShell: null };
        case "set_default_shell":
          throw message;
        default:
          return null;
      }
    });
    const select = await openTerminalSection();

    fireEvent.change(select, { target: { value: "cmd" } });

    await waitFor(() => expect(screen.getByText(message)).toBeInTheDocument());
    // The control is usable again (never stuck disabled on an error).
    expect(select).not.toBeDisabled();
  });
});
