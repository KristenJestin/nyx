import { PlusIcon, ServerCogIcon, Settings2Icon, SquareTerminalIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { CommandControls } from "@/components/command/command-controls";
import { agentProviderFor } from "@/components/sidebar/agent-providers";
import { RuntimeRow } from "./runtime-row";
import { RuntimeSection, RuntimeSectionList } from "./runtime-section";
import {
  activeAgentSession,
  notifiedRunState,
  serviceRunState,
  terminalRunState,
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
}: {
  snapshot: WorkspaceRuntimeSnapshot;
  /** Open a NEW terminal in THIS workspace (the section "+"). Reused sidebar CRUD upstream. */
  onNewTerminal?: () => void;
  /** Open the project's command manager to CREATE a command (the section gear). */
  onNewCommand?: () => void;
}) {
  const { terminals, services } = snapshot;
  return (
    <div className="flex flex-col py-2">
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
            <TerminalRow key={terminal.id} terminal={terminal} />
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
 */
function TerminalRow({ terminal }: { terminal: WorkspaceRuntimeTerminal }) {
  const { isSelected } = useRuntimeSelection();
  const { isConsumed, consume } = useConsumedNotifications();
  const selection = { kind: "terminal", id: terminal.id } as const;
  const selected = isSelected(selection);
  const liveSession = activeAgentSession(terminal);
  const provider = agentProviderFor(liveSession?.agent_kind);
  const Icon = provider?.icon ?? SquareTerminalIcon;
  const label = terminal.label ?? terminal.cwd;

  // The settled success/error dot is a NOTIFICATION: shown only while unread AND this terminal is
  // NOT the viewed one. It is UNREAD only if the snapshot says so AND the user has not consumed it
  // this session (the mark-read does not re-pull the snapshot, so a local record keeps it gone).
  // Selecting it CONSUMES the notification (records locally + persists via mark-read). `running`
  // is unaffected (always shown).
  const unread = terminal.exec_state_unread && !isConsumed("terminal", terminal.id);
  useMarkReadOnActive("terminal", terminal.id, selected, unread, () =>
    consume("terminal", terminal.id),
  );
  const dotState = notifiedRunState(terminalRunState(terminal), unread, selected);

  return (
    <RuntimeRow
      selection={selection}
      icon={<Icon className="size-4" />}
      label={label}
      state={dotState}
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
