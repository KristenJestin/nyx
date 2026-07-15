import { useEffect, useRef, useState } from "react";
import {
  PencilIcon,
  PlusIcon,
  ServerCogIcon,
  Settings2Icon,
  SquareTerminalIcon,
  XIcon,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tooltip } from "@/components/ui/tooltip";
import { CommandControls } from "@/components/command/command-controls";
import { agentProviderFor } from "@/components/sidebar/agent-providers";
import type { AgentActivity } from "@/components/sidebar/use-agent-sessions";
import { useRenameTerminal } from "@/components/sidebar/use-terminal-rename";
import {
  formatBytes,
  formatCpuPct,
  useTerminalStatsFor,
} from "@/components/sidebar/use-terminal-stats";
import { RuntimeRow } from "./runtime-row";
import { useCloseTerminal } from "./use-terminal-close";
import { SortableTerminalRows } from "./sortable-terminal-rows";
import { RuntimeSection, RuntimeSectionList } from "./runtime-section";
import {
  activeAgentSession,
  notifiedRunState,
  serviceRunState,
  terminalLineState,
} from "./runtime-state";
import { useRuntimeSelection } from "./runtime-selection";
import { useConsumedNotifications } from "./use-consumed-notifications";
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
      {/* (v2.5.8: the post-crash "Interrupted — Resume/Ignore" restore panel was reverted. A crash
          is now treated as a clean shutdown → the session resumes SILENTLY via `use-lazy-resume`,
          gated by the project resume toggle. No card, no per-workspace restore surface.) */}
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
        {/* DRAG-REORDER (v2.5.6): the workspace's terminals are drag-sortable within this group
            (never across workspaces). A drop persists via the reorder context (backend `reorder`
            → each terminal's `order_index`); the 5px pointer activation keeps the row's click /
            rename / close intact. */}
        <SortableTerminalRows
          terminals={terminals}
          group={snapshot.workspace.id}
          empty="No open terminals."
          renderRow={(terminal) => (
            <TerminalRow terminal={terminal} activity={agentActivity?.(terminal.id)} />
          )}
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
 *
 * ## v2.5.6 cockpit affordances
 *  - CPU%/RAM (FEEDBACK #28) show ONLY as a hover TOOLTIP (`RuntimeRow`'s `tooltip`) — NON-inline,
 *    so the reading never masks/truncates the terminal name (the firm constraint). The stats come
 *    from the shared `terminal://stats` map (one subscription for all rows).
 *  - RENAME inline (`use-terminal-rename`): a hover pencil or a double-click on the name swaps it
 *    for an `<Input>` (Enter/blur commit, Esc cancels), pinning a MANUAL label via the context
 *    rename callback.
 *  - CLOSE per row (`use-terminal-close`): a hover "×" closes THIS terminal by id (previously only
 *    Ctrl+W, which targets the selected one).
 *
 * Exported so the Terminals page's LOOSE-terminals band (`workspace_id == null`) reuses the exact
 * same row — those terminals get the identical icon/dot/stats/rename/close behaviour.
 */
export function TerminalRow({
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

  // PER-TERMINAL CPU%/RAM (FEEDBACK #28) — NON-INLINE: the live process-tree reading for THIS
  // terminal from the shared `terminal://stats` map (one subscription for all rows), shown ONLY as
  // a hover TOOLTIP so it never masks/truncates the terminal name. `null` before the first tick →
  // no tooltip (the row is unchanged until a reading arrives).
  const stats = useTerminalStatsFor(terminal.id);
  const statsTooltip = stats
    ? `CPU ${formatCpuPct(stats.cpuPct)} · RAM ${formatBytes(stats.memBytes)}`
    : undefined;

  // RENAME inline (v2.5.6): the context rename callback pins a MANUAL label (wins over the auto
  // name) or clears it back to auto-naming with `null`. `editing` toggles the inline `<Input>`
  // (opened by the hover pencil or a double-click on the name); `draft` is its controlled value,
  // seeded from the current displayed name so the user edits FROM what they see.
  const rename = useRenameTerminal();
  const close = useCloseTerminal();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  const beginRename = () => {
    setDraft(terminal.label?.trim() ? terminal.label : typeof label === "string" ? label : "");
    setEditing(true);
  };

  // Commit the draft: a TRIMMED non-empty value pins a manual label; empty clears it back to
  // auto-naming (`null`). Skip the round-trip when unchanged from the persisted label.
  const commitRename = () => {
    setEditing(false);
    const trimmed = draft.trim();
    const next = trimmed ? trimmed : null;
    const current = terminal.label?.trim() ? terminal.label.trim() : null;
    if (next !== current) rename(terminal.id, next);
  };

  // Focus + select-all once the input shows, so the user can immediately overwrite or tweak.
  useEffect(() => {
    if (editing) inputRef.current?.select();
  }, [editing]);

  const nameForLabels = typeof label === "string" ? label : terminal.id;

  return (
    <RuntimeRow
      selection={selection}
      icon={<Icon className="size-4" />}
      label={label}
      state={dotState}
      tooltip={statsTooltip}
      editing={editing}
      onLabelDoubleClick={beginRename}
      editor={
        <Input
          ref={inputRef}
          aria-label={`Rename terminal ${nameForLabels}`}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onClick={(e) => e.stopPropagation()}
          onPointerDown={(e) => e.stopPropagation()}
          onBlur={commitRename}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              commitRename();
            } else if (e.key === "Escape") {
              e.preventDefault();
              setEditing(false);
            }
          }}
          // h-5 (20px) matches the label's line-box (text-sm ⇒ line-height 20px) so entering /
          // leaving rename does NOT change the row height (v2.5.6 finding #25). The shared
          // `py-1.5` wrapper (runtime-row) then yields the SAME 32px row in both states; a taller
          // h-6 (24px) grew the row by +4px on edit.
          className="h-5 w-full min-w-0 px-1.5 py-0 text-sm"
        />
      }
      actions={
        !editing ? (
          <>
            {/* RENAME (v2.5.6): a hover pencil opening the inline editor — the discoverable twin of
                the double-click on the name. `stopPropagation` so it never also selects the row. */}
            <Tooltip label="Rename terminal">
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label={`Rename terminal ${nameForLabels}`}
                // stopPropagation on BOTH pointerdown AND click: with the reorder sensor's
                // `preventActivation: () => false` (reorder-dnd.ts:18-29), a pointerdown+drag that
                // reached this button would otherwise START a row drag. Swallowing pointerdown here
                // keeps the drag off this control (the click still fires → rename opens).
                onPointerDown={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  beginRename();
                }}
              >
                <PencilIcon />
              </Button>
            </Tooltip>
            {/* CLOSE (v2.5.6): a per-row "×" that closes THIS terminal (by id), so the user is no
                longer limited to Ctrl+W on the selected one. `stopPropagation` so closing never
                also selects the row. */}
            <Tooltip label="Close terminal">
              <Button
                variant="ghost-destructive"
                size="icon-xs"
                aria-label={`Close terminal ${nameForLabels}`}
                // stopPropagation on pointerdown too (same reason as the pencil above): the reorder
                // sensor opts out of preventActivation, so without this a pointerdown+drag on the ×
                // could start a row drag. The click still fires → the terminal closes.
                onPointerDown={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  close(terminal.id);
                }}
              >
                <XIcon />
              </Button>
            </Tooltip>
          </>
        ) : undefined
      }
    />
  );
}

/** A managed-command service row, with the shared start/stop/relaunch controls. */
function ServiceRow({ service }: { service: WorkspaceRuntimeService }) {
  const { isSelected } = useRuntimeSelection();
  const { isConsumed, consume } = useConsumedNotifications();
  const selection = { kind: "service", id: service.id } as const;
  const selected = isSelected(selection);
  const state = serviceRunState(service);

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
