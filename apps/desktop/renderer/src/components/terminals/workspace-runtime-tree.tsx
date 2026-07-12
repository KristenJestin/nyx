import { PlusIcon, ServerCogIcon, Settings2Icon, SquareTerminalIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { CommandControls } from "@/components/command/command-controls";
import { agentProviderFor } from "@/components/sidebar/agent-providers";
import type { AgentActivity } from "@/components/sidebar/use-agent-sessions";
import { RuntimeRow } from "./runtime-row";
import { RuntimeSection, RuntimeSectionList } from "./runtime-section";
import {
  activeAgentSession,
  notifiedRunState,
  serviceRunState,
  terminalLineState,
} from "./runtime-state";
import { LifecycleBadge } from "./lifecycle-badge";
import { WorkspaceRestorePanel } from "./workspace-restore-panel";
import { useRuntimeSelection } from "./runtime-selection";
import { useConsumedNotifications } from "./use-consumed-notifications";
import { useRestorePlan } from "./use-restore-plan";
import { useMarkReadOnActive } from "./use-mark-read";
import type {
  WorkspaceRuntimeService,
  WorkspaceRuntimeSnapshot,
  WorkspaceRuntimeTerminal,
} from "./use-workspace-runtime";

/**
 * `<WorkspaceRuntimeTree>` — a single workspace's runtime tree: its live terminals and its
 * managed-command services, grouped into always-open sections with the shared page selection.
 * Selecting a row focuses that element so the page's ONE pane host shows the matching surface.
 *
 * ## No cosmetic "Agents" category
 * Agent sessions are NOT a third top-level section. They live WHERE they run — on their host
 * terminal: a terminal that hosts a live agent session swaps its generic glyph for the
 * provider's brand logo, so an agent is legible in the tree without inventing a separate
 * cosmetic bucket.
 *
 * ## No business logic here
 * Run states are the pure projections from `runtime-state` (a total fold over the read model
 * the backend already computed); the service controls delegate to the shared
 * `<CommandControls>` (the same lifecycle `invoke`s the command UI uses). Every colour/size is
 * a design-system token.
 */
export function WorkspaceRuntimeTree({
  snapshot,
  onNewTerminal,
  onNewCommand,
  agentActivity,
}: {
  snapshot: WorkspaceRuntimeSnapshot;
  /** Open a NEW terminal in THIS workspace (the section "+"). Reused sidebar CRUD upstream. */
  onNewTerminal?: () => void;
  /** Open the project's command manager to CREATE a command (the section gear). */
  onNewCommand?: () => void;
  /**
   * The live per-terminal agent activity lookup (review T2) — `byActivity.get(terminalId)` from
   * the SAME `agent_activity_snapshot` read model the workspace/project rollups consume. Threaded
   * so a terminal LINE hosting an agent shows the agent's live dot (working/waiting/ready), not
   * just the rollup. `undefined` (or a lookup returning `undefined`) leaves a line in TERMINAL
   * mode (busy/exec_state, task T1) — so a caller with no activity source is unaffected.
   */
  agentActivity?: (terminalId: string) => AgentActivity | undefined;
}) {
  const { terminals, services } = snapshot;
  return (
    <div className="flex flex-col py-2">
      {/* Post-crash RESTORATION UI: lists this workspace's resumable interrupted sessions with a
          per-session Resume / Ignore choice (reads `workspace_restore_plan`). Renders nothing when
          there is nothing to restore, so a healthy workspace shows no panel. */}
      <WorkspaceRestorePanel workspaceId={snapshot.workspace.id} />

      <RuntimeSection
        title="Terminals"
        count={terminals.length}
        actions={
          onNewTerminal ? (
            <Tooltip label="New terminal">
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label="New terminal"
                onClick={onNewTerminal}
              >
                <PlusIcon />
              </Button>
            </Tooltip>
          ) : null
        }
      >
        <RuntimeSectionList
          empty="No open terminals."
          rows={terminals.map((terminal) => (
            <TerminalRow
              key={terminal.id}
              terminal={terminal}
              activity={agentActivity?.(terminal.id)}
            />
          ))}
        />
      </RuntimeSection>

      <RuntimeSection
        title="Commands"
        count={services.length}
        actions={
          onNewCommand ? (
            <Tooltip label="New command">
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label="New command"
                onClick={onNewCommand}
              >
                <Settings2Icon />
              </Button>
            </Tooltip>
          ) : null
        }
      >
        <RuntimeSectionList
          empty="No managed commands."
          rows={services.map((service) => (
            <ServiceRow key={service.id} service={service} />
          ))}
        />
      </RuntimeSection>
    </div>
  );
}

/**
 * A terminal row. Its LEAD glyph is the provider brand logo when the terminal hosts a live
 * agent session (a running/waiting session — the newest one wins), else the generic terminal
 * glyph. This is how the tree surfaces an agent WITHOUT a cosmetic Agents category: the glyph
 * alone carries the provider identity — the row stays name + dot, no trailing meta text.
 *
 * Its DOT is the TWO-MODE fold ({@link terminalLineState}, review T2): for an agent-hosting line
 * the live agent activity (working/waiting/ready) primes the dot; otherwise the OS busy/exec_state
 * terminal mode (task T1). `activity` is this terminal's live activity entry (`byActivity.get`).
 */
function TerminalRow({
  terminal,
  activity,
}: {
  terminal: WorkspaceRuntimeTerminal;
  activity: AgentActivity | undefined;
}) {
  const { isSelected } = useRuntimeSelection();
  const { isConsumed, consume } = useConsumedNotifications();
  const selection = { kind: "terminal", id: terminal.id } as const;
  const selected = isSelected(selection);
  const liveSession = activeAgentSession(terminal);
  // ICON RECONCILIATION (review T3): a real HOOK session (`sessions[]`, resumable) is the
  // authoritative provider; ONLY when there is none does the PASSIVELY-DETECTED foreground agent
  // (`detected_agent_kind`, no session) supply the icon — so a hand-launched agent is adopted
  // without the hook, the hook takes precedence once it fires (no double icon / flicker), and a
  // stopped agent (detected kind `null`) falls back to the generic terminal glyph. Both flow
  // through the SAME provider-agnostic `agentProviderFor` resolver — no `claude` hardcode.
  const providerKind = liveSession?.agent_kind ?? terminal.detected_agent_kind ?? undefined;
  const provider = agentProviderFor(providerKind);
  const Icon = provider?.icon ?? SquareTerminalIcon;
  const label = terminal.label ?? terminal.cwd;

  // AGENT MODE as soon as the line CARRIES an agent glyph — i.e. `providerKind` is defined (a live
  // hook session OR a passively-detected agent), the IDENTICAL predicate that chose the icon above.
  // "The row shows an agent icon" ⟺ "agent mode": the OS busy bit is ignored and the dot follows
  // the live agent activity (review T2). This is `terminalLineState`'s agent gate, kept in sync so
  // the mark-read seams match the dot's mode (no busy/exec_state notification on an agent line).
  const inAgentMode = providerKind !== undefined;

  // Terminal-mode notification: the settled success/error dot is shown only while unread AND this
  // terminal is NOT the viewed one. UNREAD only if the snapshot says so AND the user has not
  // consumed it this session (the mark-read does not re-pull the snapshot, so a local record keeps
  // it gone). Agent-mode uses its OWN unread (ready/error), so the terminal clear is gated off it.
  const terminalUnread = terminal.exec_state_unread && !isConsumed("terminal", terminal.id);
  useMarkReadOnActive("terminal", terminal.id, selected && !inAgentMode, terminalUnread, () =>
    consume("terminal", terminal.id),
  );
  // Agent-mode notification: a finished turn's "ready" (green) / turn-error (red) clears via
  // `agent_mark_ready_read` when this agent line becomes the viewed one — the SAME seam the
  // sidebar row uses, so a settled agent dot does not linger after the user looks at the terminal.
  // An agent line with NO live activity entry (idle at the prompt) has nothing settled to clear —
  // guarded with `activity?` since the line is now in agent mode even without an activity entry.
  const agentSettledUnread =
    inAgentMode && (activity?.errorUnread === true || activity?.readyUnread === true);
  useMarkReadOnActive("agent", terminal.id, selected && inAgentMode, agentSettledUnread);

  const dotState = terminalLineState(terminal, activity, { terminalUnread, selected });

  // A hosted agent session flipped to `unknown` by the crash reconcile (`active -> unknown`, its
  // liveness unconfirmed) badges the row so the doubt reads at a glance beside the run-state dot.
  const doubtful = liveSession?.state === "unknown";

  return (
    <RuntimeRow
      selection={selection}
      icon={<Icon className="size-4" />}
      label={label}
      state={dotState}
      badge={doubtful ? <LifecycleBadge state="unknown" /> : undefined}
    />
  );
}

/** A managed-command service row, with the shared start/stop/relaunch controls. */
function ServiceRow({ service }: { service: WorkspaceRuntimeService }) {
  const { isSelected } = useRuntimeSelection();
  const { isConsumed, consume } = useConsumedNotifications();
  const { isInterrupted } = useRestorePlan();
  const selection = { kind: "service", id: service.id } as const;
  const selected = isSelected(selection);
  const state = serviceRunState(service);

  // A command instance the crash reconcile flagged `interrupted` (persisted `running` but its
  // backing process died with the old sidecar — `running -> interrupted`, NOT auto-relaunched)
  // badges the row so the doubt reads beside the run-state dot. Sourced from the restore plan.
  const interrupted = isInterrupted(service.id);

  // Same Attention-Inbox semantics as terminals: the settled success/error dot shows only while
  // unread AND not viewed; selecting the service acknowledges it (`command_acknowledge`) + records
  // it locally. The CONTROLS keep the FACTUAL state (their own `showDot={false}`), so this only
  // governs the row's notification dot.
  const unread = service.unread && !isConsumed("service", service.id);
  useMarkReadOnActive("service", service.id, selected, unread, () =>
    consume("service", service.id),
  );
  const dotState = notifiedRunState(state, unread, selected);

  return (
    <RuntimeRow
      selection={selection}
      icon={<ServerCogIcon className="size-4" />}
      label={service.name}
      state={dotState}
      badge={interrupted ? <LifecycleBadge state="interrupted" /> : undefined}
      actions={
        <CommandControls
          instanceId={service.id}
          state={state}
          showDot={false}
          buttonSize="icon-xs"
          inRow
        />
      }
    />
  );
}
