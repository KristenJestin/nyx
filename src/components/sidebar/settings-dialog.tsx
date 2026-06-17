import { forwardRef, useCallback, useImperativeHandle, useState, type ComponentType } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CheckCircle2Icon, PlugIcon, SettingsIcon, XCircleIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";

// ---------------------------------------------------------------------------
// Types (mirror the Tauri IntegrationStatus struct)
// ---------------------------------------------------------------------------

interface IntegrationStatus {
  provider: string;
  label: string;
  installed: boolean;
  /** `false` = coming soon, no actions available yet. */
  available: boolean;
}

// ---------------------------------------------------------------------------
// Provider card
// ---------------------------------------------------------------------------

interface ProviderCardProps {
  status: IntegrationStatus;
  /** Called after a successful install/remove so the list can be refreshed. */
  onRefresh: () => void;
}

/**
 * One integration row. The install/remove side effect is owned HERE (per card),
 * so its in-flight state stays local: the spinner renders ON THE CLICKED BUTTON
 * (the `Button`'s built-in `loading` prop) and the rest of the list — and the
 * whole modal — stays mounted. We never lift this `busy` flag up to a list-level
 * loading branch that would unmount the providers and flash the modal (review
 * finding 01KVAQAAW3D7TKPTVKMTBKDDPY).
 */
function ProviderCard({ status, onRefresh }: ProviderCardProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleInstall = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      await invoke("integration_install", { provider: status.provider });
      onRefresh();
    } catch (e) {
      setError(typeof e === "string" ? e : "Install failed");
    } finally {
      setBusy(false);
    }
  }, [status.provider, onRefresh]);

  const handleRemove = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      await invoke("integration_remove", { provider: status.provider });
      onRefresh();
    } catch (e) {
      setError(typeof e === "string" ? e : "Remove failed");
    } finally {
      setBusy(false);
    }
  }, [status.provider, onRefresh]);

  return (
    <div
      className={cn(
        "flex items-center justify-between rounded-lg border px-4 py-3",
        status.available ? "border-border bg-card" : "border-border/50 bg-card/50 opacity-60",
      )}
    >
      {/* Left: icon + label + status badge */}
      <div className="flex min-w-0 flex-col gap-0.5">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-card-foreground">{status.label}</span>
          {status.available && status.installed && (
            <span className="inline-flex items-center gap-1 rounded-full bg-success/12 px-2 py-0.5 text-[11px] font-medium text-success">
              <CheckCircle2Icon className="size-3" />
              Installed
            </span>
          )}
          {!status.available && (
            <span className="inline-flex items-center rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
              Coming soon
            </span>
          )}
        </div>
        {error && (
          <p className="flex items-center gap-1 text-xs text-destructive">
            <XCircleIcon className="size-3 shrink-0" />
            {error}
          </p>
        )}
      </div>

      {/* Right: action button (only for available providers). The spinner shows
          ON the clicked button via the Button's built-in `loading` state — the
          button stays in place, the row never unmounts, no full-modal flash. */}
      {status.available && (
        <div className="ml-4 flex shrink-0 items-center gap-2">
          {status.installed ? (
            <Button
              variant="destructive-outline"
              size="xs"
              loading={busy}
              disabled={busy}
              onClick={() => void handleRemove()}
            >
              Remove
            </Button>
          ) : (
            <Button
              variant="default"
              size="xs"
              loading={busy}
              disabled={busy}
              onClick={() => void handleInstall()}
            >
              Install
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Integrations section
// ---------------------------------------------------------------------------

interface IntegrationsSectionProps {
  integrations: IntegrationStatus[];
  loading: boolean;
  onRefresh: () => void;
}

/**
 * The **Integrations** detail pane. Lists all supported MCP provider
 * integrations and threads each card the `onRefresh` callback so a successful
 * install/remove re-pulls the list.
 *
 * The list itself stays MOUNTED across an install/remove op — the per-card
 * spinner (see `ProviderCard`) covers the in-flight state. The list-level
 * spinner is reserved for the FIRST load (when there is nothing to show yet).
 */
function IntegrationsSection({ integrations, loading, onRefresh }: IntegrationsSectionProps) {
  return (
    <section>
      <h3 className="text-base font-semibold text-foreground">Integrations</h3>
      <p className="mt-1 mb-4 text-sm text-muted-foreground">
        Connect nyx to AI coding tools. Installed integrations are kept up to date automatically on
        every launch.
      </p>

      {/* Only show the list-level spinner on the FIRST load (empty list). Once
          providers are on screen we keep them mounted and let the per-button
          loading state cover install/remove — no full-pane flash. */}
      {loading && integrations.length === 0 ? (
        <div className="flex items-center justify-center py-6">
          <Spinner className="size-5 text-muted-foreground" />
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {integrations.map((s) => (
            <li key={s.provider}>
              <ProviderCard status={s} onRefresh={onRefresh} />
            </li>
          ))}
          {integrations.length === 0 && (
            <li className="py-4 text-center text-sm text-muted-foreground">
              No integrations available.
            </li>
          )}
        </ul>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Section registry (the left rail)
// ---------------------------------------------------------------------------

type SectionId = "integrations";

interface SectionDef {
  id: SectionId;
  label: string;
  icon: ComponentType<{ className?: string }>;
}

/**
 * The Settings sections, in rail order. Adding a future section (e.g. Appearance,
 * General) is a matter of appending a `SectionDef` here and rendering its pane in
 * the detail switch below — the rail, selection state and navigation are generic.
 */
const SECTIONS: readonly SectionDef[] = [
  { id: "integrations", label: "Integrations", icon: PlugIcon },
];

// ---------------------------------------------------------------------------
// Settings dialog
// ---------------------------------------------------------------------------

export interface SettingsDialogProps {
  open: boolean;
  onClose: () => void;
}

/**
 * Imperative handle exposed by `<SettingsDialog>` so the *event* that opens the
 * modal (the gear-button handler) can pull a fresh provider list. We deliberately
 * do NOT reload from a `useEffect` watching `open`: that fakes an event handler
 * with an effect (an extra render that runs late, flagged by react-doctor's
 * `no-event-handler`). The open path owns the side effect instead — see
 * `terminal-manager`'s `onOpenSettings`.
 */
export interface SettingsDialogHandle {
  /** Re-pull the integration list from the backend. */
  reload: () => void;
}

/**
 * `<SettingsDialog>` — the global Settings modal (triggered from the gear icon
 * in the sidebar head).
 *
 * LAYOUT: a **left section rail** (the `SECTIONS` registry — Integrations now,
 * extensible) + a **right detail pane** that renders the selected section. This
 * mirrors the Commands-modal navigation pattern (a rail of selectable entries +
 * a detail surface), rendered with nyx's design tokens.
 *
 * The **Integrations** section lists all supported MCP provider integrations:
 * - **Claude Code** — functional in v1: Install writes nyx's entry to
 *   `~/.claude.json` and marks `installed = true` in `integrations.json` so
 *   future boots only reconcile (never silently re-create). Remove inverts both.
 * - **Codex** / **OpenCode** / **Custom** — coming soon (UI disabled, no actions).
 *
 * Install state is persisted by the Rust backend; this component only reads and
 * triggers mutations via `integration_list` / `integration_install` /
 * `integration_remove` Tauri commands.
 *
 * The dialog stays mounted across open/close (its exit is Motion-animated, so an
 * immediate unmount would cut the animation short). The provider list is loaded
 * on the OPEN EVENT via the imperative `reload()` handle, not via an effect that
 * watches the `open` prop.
 */
export const SettingsDialog = forwardRef<SettingsDialogHandle, SettingsDialogProps>(
  function SettingsDialog({ open, onClose }, ref) {
    const [integrations, setIntegrations] = useState<IntegrationStatus[]>([]);
    const [loading, setLoading] = useState(false);
    const [activeSection, setActiveSection] = useState<SectionId>("integrations");

    const loadIntegrations = useCallback(async () => {
      setLoading(true);
      try {
        const list = await invoke<IntegrationStatus[]>("integration_list");
        setIntegrations(list);
      } catch {
        // Silently ignore — the UI will show an empty list.
      } finally {
        setLoading(false);
      }
    }, []);

    // Expose the reload to the open-event handler so opening the modal pulls a
    // fresh list (every open), driven by the event rather than an effect.
    useImperativeHandle(ref, () => ({ reload: () => void loadIntegrations() }), [loadIntegrations]);

    return (
      <Dialog.Root
        open={open}
        onOpenChange={(next) => {
          if (!next) onClose();
        }}
      >
        <Dialog.Portal>
          <DialogBackdrop />
          <DialogPopup className="flex max-h-[calc(100vh-4rem)] w-[min(44rem,calc(100vw-2rem))] flex-col overflow-hidden p-0">
            {/* Header */}
            <div className="flex items-center gap-2.5 border-b border-border px-5 py-4">
              <SettingsIcon className="size-4 shrink-0 text-muted-foreground" />
              <Dialog.Title className="text-base font-semibold">Settings</Dialog.Title>
            </div>

            {/* Body: left section rail + right detail pane. */}
            <div className="flex min-h-0 flex-1">
              {/* Left rail of sections (Integrations now, extensible). */}
              <nav
                aria-label="Settings sections"
                className="w-44 shrink-0 border-r border-border bg-muted/30 p-2"
              >
                <ul className="flex flex-col gap-0.5">
                  {SECTIONS.map((section) => {
                    const Icon = section.icon;
                    const selected = activeSection === section.id;
                    return (
                      <li key={section.id}>
                        <button
                          type="button"
                          aria-current={selected ? "page" : undefined}
                          onClick={() => setActiveSection(section.id)}
                          className={cn(
                            "flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-sm font-medium outline-none transition-colors",
                            "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
                            selected
                              ? "bg-secondary text-secondary-foreground"
                              : "text-muted-foreground hover:bg-muted hover:text-foreground",
                          )}
                        >
                          <Icon className="size-4 shrink-0 opacity-80" />
                          {section.label}
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </nav>

              {/* Right detail pane: the selected section. */}
              <div className="min-h-0 flex-1 overflow-y-auto px-5 py-5">
                {activeSection === "integrations" && (
                  <IntegrationsSection
                    integrations={integrations}
                    loading={loading}
                    onRefresh={() => void loadIntegrations()}
                  />
                )}
              </div>
            </div>

            {/* Footer */}
            <div className="flex justify-end border-t border-border px-5 py-3.5">
              <Dialog.Close
                render={
                  <Button variant="outline" size="sm">
                    Close
                  </Button>
                }
                onClick={onClose}
              />
            </div>
          </DialogPopup>
        </Dialog.Portal>
      </Dialog.Root>
    );
  },
);
