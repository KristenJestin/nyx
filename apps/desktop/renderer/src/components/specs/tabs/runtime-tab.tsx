import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { ServerCogIcon, SquareTerminalIcon } from "lucide-react";

import { nyxBridge } from "@/bridge";
import { CommandControls } from "@/components/command/command-controls";
import type { WorkspaceRuntimeSnapshot } from "@/components/terminals/use-workspace-runtime";

import { Dot } from "../spec-primitives";
import type { SpecDetail } from "../types";

/** A titled section panel with a count badge. */
function SectionPanel({
  title,
  count,
  children,
}: {
  title: string;
  count: number;
  children: React.ReactNode;
}) {
  return (
    <div className="overflow-hidden rounded-xl border border-border/60 bg-card">
      <div className="flex items-center gap-2 border-b border-border/50 bg-muted/40 px-3 py-2">
        <span className="text-sm font-medium text-foreground">{title}</span>
        <span className="font-mono text-2xs text-muted-foreground/70">{count}</span>
      </div>
      {count > 0 ? (
        <div className="divide-y divide-border/40">{children}</div>
      ) : (
        <p className="px-3 py-3 text-sm italic text-muted-foreground/60">None.</p>
      )}
    </div>
  );
}

/** The exec-state → dot-tone mapping (design-system token classes). */
function execTone(state: string): string {
  switch (state) {
    case "running":
      return "bg-warning";
    case "success":
      return "bg-success";
    case "error":
      return "bg-destructive";
    default:
      return "bg-muted-foreground/40";
  }
}

/**
 * A clickable TERMINAL row — the icon + coloured status dot + label. Clicking NAVIGATES to the
 * Terminals page with that terminal opened (`/?terminal=<id>`), after entering the terminal's
 * project so it renders. The whole row is a button (keyboard-reachable) — the exec_state word
 * stays out (icon + dot convey it; a prior finding removed the literal "idle").
 */
function TerminalRow({
  terminal,
  onOpen,
}: {
  terminal: WorkspaceRuntimeSnapshot["terminals"][number];
  onOpen: (terminalId: string) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => onOpen(terminal.id)}
      className="flex w-full items-center gap-2.5 px-3 py-2 text-left transition-colors hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none"
    >
      <span className="relative inline-flex shrink-0 text-muted-foreground">
        <SquareTerminalIcon className="size-4" />
        <Dot
          tone={execTone(terminal.exec_state)}
          size="xs"
          className="absolute -bottom-0.5 -right-0.5 rounded-full ring-2 ring-background"
        />
      </span>
      <span className="min-w-0 flex-1 truncate text-sm text-foreground">
        {terminal.label ?? terminal.cwd}
      </span>
    </button>
  );
}

/**
 * A MANAGED-COMMAND row — the command name + its live start/stop/relaunch controls (the SAME
 * `<CommandControls>` the Terminals cockpit uses, keyed on the `command_instances.id`). The
 * lead status dot is shown by the controls, so this row does not repeat one.
 *
 * The whole row is CLICKABLE → it opens the command's DETAIL on the Terminals page (its live
 * output pane, selected by id). It is a `div role="button"` (NOT a `<button>`) so the lifecycle
 * control buttons can nest inside it — a `<button>` may not contain buttons; this mirrors the
 * sidebar's command row. The controls carry `inRow`, so a Start/Stop/Relaunch stops propagation
 * and never triggers the row navigation. Enter/Space activate the row.
 */
function CommandRow({
  service,
  onOpen,
}: {
  service: WorkspaceRuntimeSnapshot["services"][number];
  onOpen: (serviceId: string) => void;
}) {
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={() => onOpen(service.id)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen(service.id);
        }
      }}
      className="flex w-full cursor-pointer items-center gap-2.5 px-3 py-2 text-left transition-colors hover:bg-muted/50 focus-visible:bg-muted/50 focus-visible:outline-none"
    >
      <ServerCogIcon className="size-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0 flex-1">
        <span className="block truncate text-sm text-foreground">{service.name}</span>
        <span className="block truncate font-mono text-2xs text-muted-foreground/60">
          {service.command}
        </span>
      </div>
      <CommandControls
        instanceId={service.id}
        state={service.run_state}
        showDot
        buttonSize="icon-xs"
        inRow
        className="shrink-0"
      />
    </div>
  );
}

/**
 * `<RuntimeTab>` — the Spec's ACTIONABLE runtime, recentred (review finding): the associated
 * TERMINALS (clickable → open that terminal on the Terminals page) and the MANAGED COMMANDS
 * (with start/stop/relaunch controls). It reads the SAME `workspace_runtime_snapshot` read model
 * the Terminals cockpit + the MCP `workspace_runtime` tool serve (PDR-0005), scoped to the Spec's
 * dev workspace (via `detail.workspace_context`). "Associated" = the terminals/commands of the
 * Spec's workspace (a real terminal↔spec association model is deferred). The whole-workspace dump
 * (workspace header + agent sessions) is intentionally dropped — this tab is about what the user
 * wants to act on, not a full workspace mirror.
 */
export function RuntimeTab({ detail }: { detail: SpecDetail }) {
  const ws = detail.workspace_context?.workspace ?? null;
  const workspaceId = ws?.id ?? null;
  const navigate = useNavigate();
  const [snapshot, setSnapshot] = useState<WorkspaceRuntimeSnapshot | null>(null);
  const [loading, setLoading] = useState<boolean>(workspaceId !== null);

  useEffect(() => {
    let cancelled = false;
    if (workspaceId === null) {
      setSnapshot(null);
      setLoading(false);
      return () => {
        cancelled = true;
      };
    }
    setLoading(true);
    void nyxBridge
      .invoke<WorkspaceRuntimeSnapshot>("workspace_runtime_snapshot", { workspaceId })
      .then((s) => {
        if (!cancelled) setSnapshot(s);
      })
      .catch(() => {
        if (!cancelled) setSnapshot(null);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId]);

  /**
   * Open a specific terminal on the Terminals page — a plain deep-link, NO project selection here.
   *
   * These two handlers used to enter the workspace's project themselves. That is now redundant AND a
   * second definition of "which project a Spec puts you in": the Spec page hosting this tab is
   * already synced to the Spec's owner project by `useSpecProjectSync`, so by the time a row can be
   * clicked the shell is in the right project and `/` resolves to it. Entering the project stays the
   * ONE responsibility of that shared hook (PRD fix 2).
   */
  function openTerminal(terminalId: string) {
    void navigate({ to: "/", search: { terminal: terminalId } });
  }

  /** Open a managed command's DETAIL on the Terminals page — mirrors `openTerminal` (its live
   *  output pane is selected by id), likewise relying on the page-level project sync. */
  function openCommand(serviceId: string) {
    void navigate({ to: "/", search: { command: serviceId } });
  }

  if (workspaceId === null) {
    return (
      <div className="mx-auto max-w-3xl">
        <p className="text-sm text-muted-foreground">
          No runtime — this Spec has no dev workspace yet.
        </p>
      </div>
    );
  }

  if (loading || snapshot === null) {
    return (
      <div className="mx-auto max-w-3xl">
        <p className="text-sm text-muted-foreground">
          {loading ? "Loading runtime…" : "No runtime available."}
        </p>
      </div>
    );
  }

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-4">
      <SectionPanel title="Terminals" count={snapshot.terminals.length}>
        {snapshot.terminals.map((t) => (
          <TerminalRow key={t.id} terminal={t} onOpen={openTerminal} />
        ))}
      </SectionPanel>

      <SectionPanel title="Managed commands" count={snapshot.services.length}>
        {snapshot.services.map((c) => (
          <CommandRow key={c.id} service={c} onOpen={openCommand} />
        ))}
      </SectionPanel>
    </div>
  );
}
