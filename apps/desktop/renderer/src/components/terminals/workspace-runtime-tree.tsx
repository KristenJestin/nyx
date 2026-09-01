import { useEffect, useMemo, useRef, useState } from "react";
import { PencilIcon, PlusIcon, Settings2Icon, XIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tooltip } from "@/components/ui/tooltip";
import { CommandControls } from "@/components/command/command-controls";
import { CommandGroupControls } from "@/components/command/command-group-controls";
import { commandGroupTally } from "@/components/command/use-command-group-run";
import { useCommandGroupsContext } from "@/components/command/command-groups-context";
import type { CommandGroup } from "@/components/command/use-command-groups";
import { agentProviderFor } from "@/components/sidebar/agent-providers";
import { resolveDisplayName } from "@/components/sidebar/auto-label";
import type { AgentActivity } from "@/components/sidebar/use-agent-sessions";
import { useRenameTerminal } from "@/components/sidebar/use-terminal-rename";
import {
  formatBytes,
  formatCpuPct,
  useTerminalStatsFor,
} from "@/components/sidebar/use-terminal-stats";
import { GroupBlock } from "@/components/panes/group-block";
import { groupLabel } from "@/components/panes/pane-groups";
import { RuntimeRow } from "./runtime-row";
import { useCloseTerminal } from "./use-terminal-close";
import { SortableTerminalRows } from "./sortable-terminal-rows";
import { RuntimeSection, RuntimeSectionList } from "./runtime-section";
import {
  activeAgentSession,
  notifiedRunState,
  serviceRollupState,
  serviceRunState,
  terminalLineState,
  terminalRollupState,
} from "./runtime-state";
import {
  LINE_FAMILY_ICON,
  LINE_GLYPH_SIZE,
  SidebarLineFamilyGlyph,
} from "@/components/ui/sidebar-line";
import { useGroupFolds } from "@/components/panes/use-group-folds";
import { aggregateState } from "@/components/ui/status-dot";
import { loadGroupOpen, saveGroupOpen } from "./ui-persistence";
import { useRuntimeSelection } from "./runtime-selection";
import { useConsumedNotifications } from "./use-consumed-notifications";
import { useMarkReadOnActive } from "./use-mark-read";
import type {
  WorkspaceRuntimeService,
  WorkspaceRuntimeSnapshot,
  WorkspaceRuntimeTerminal,
} from "@/components/runtime/use-workspace-runtime";

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
  formatCwd,
}: {
  snapshot: WorkspaceRuntimeSnapshot;
  /** Open a NEW terminal in THIS workspace (the section "+"). Reused sidebar CRUD upstream. */
  onNewTerminal?: () => void;
  /** Open the project's command manager to CREATE a command (the section gear). */
  onNewCommand?: () => void;
  /**
   * Re-render a terminal's CWD for display when the row falls back to it for a name (an unnamed
   * terminal shows its cwd). The SYSTÈME page passes the home abbreviation so a terminal opened at
   * the platform home reads `~` instead of `C:/Users/…` — a DISPLAY only: the record's cwd stays
   * the absolute path the backend wrote, and nothing here is ever sent back. Omitted ⇒ identity,
   * so every project surface is untouched.
   */
  formatCwd?: (cwd: string) => string;
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
  // A CLOSED FOLDER HIDES ITS MEMBERS' DOTS, so it has to carry their aggregate — and the aggregate
  // has to be the one the workspace accordion already computes over the very same terminals, or the
  // two dots on screen would disagree. `terminalRollupState` is that projection, exported for this
  // (human review 01M12MW1DV1V455EWBMV048TE6); the list aggregates it per block with the shared
  // `aggregateState`, so there is no second rollup rule anywhere.
  const { isConsumed } = useConsumedNotifications();
  // The rank of each terminal in the workspace's OWN list, resolved once. The row render-prop is
  // handed a terminal and not its index (the sortable list renumbers its members, which is exactly
  // what this rank must not follow), so without this map every row would scan the array again.
  const rankById = useMemo(
    () => new Map(terminals.map((terminal, index) => [terminal.id, index])),
    [terminals],
  );
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
            <TerminalRow
              terminal={terminal}
              // The row's rank in the workspace's own list — read from the source array rather
              // than from the sortable list, whose blocks/folders renumber their members. It
              // only ever feeds `resolveDisplayName`'s `Terminal <n>` last resort.
              index={rankById.get(terminal.id) ?? 0}
              activity={agentActivity?.(terminal.id)}
              formatCwd={formatCwd}
            />
          )}
          memberState={(terminal) =>
            terminalRollupState(terminal, agentActivity?.(terminal.id), isConsumed)
          }
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
        <CommandRows services={services} workspaceId={snapshot.workspace.id} />
      </RuntimeSection>
    </div>
  );
}

/**
 * THE COMMANDS LIST, WITH ITS GROUPS — the same single-list treatment the terminals get.
 *
 * A command that belongs to a group is drawn inside a {@link GroupBlock}, at the position of the
 * group's first member; every other command stays a bare row. The membership is read from the
 * project's command groups (`useCommandGroupsContext`, backed by `managed_command_groups`), and
 * the block is the very component the terminals' list paints — same head, same colour, same name.
 *
 * THE SIDEBAR SHOWS A COMMAND GROUP; IT DOES NOT EDIT ONE (review finding
 * 01M11XMQA3M852JDD7R3MDG88X). Verbatim: « pour le groupement des commandes … le 3 petits points
 * pour les commandes c'est non. c'est ok pour les terminaux que je maîtrise, mais pour les
 * commandes, tout se passe dans le modal pas ici. » So this block passes NEITHER `handleRef` (no
 * grip: a block that cannot be moved must not draw a handle promising it) NOR `onOpenPanel` (no
 * `…`, and no `<GroupPanel>` mounted behind it). WHO is in a command group, WHERE each member sits,
 * and what the group is called or coloured are all settled in Project settings ▸ Commands
 * (`command-catalog.tsx`), which keeps the full head.
 *
 * ## …AND IT PILOTS ONE (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding 01M11XVEFEQ524XRT9ARPQFGSV)
 *
 * That finding chased COMPOSITION out of the sidebar, not the LIFECYCLE — and the lifecycle can
 * live nowhere else. A group holds command TEMPLATES (project-scoped) while starting one drives
 * `command_instances` (workspace-scoped): the settings modal knows no workspace, so it structurally
 * cannot start anything. The sidebar pilots, the modal composes. The head of a command group
 * therefore carries what the modal cannot offer — an aggregate dot, an `n/m` counter and
 * play / stop / restart over every member at once — and still offers no way to change the grouping.
 *
 * The TERMINAL blocks of `sortable-terminal-rows.tsx` are the other family, and the two heads are
 * each other's inverse ON PURPOSE: grip + `…` there (composition, which happens at the pointer in
 * this very sidebar), dot + `n/m` + controls here (piloting, which happens per workspace and only
 * here). Neither half is decoration — each is the operation that has nowhere else to go.
 */
function CommandRows({
  services,
  workspaceId,
}: {
  services: readonly WorkspaceRuntimeService[];
  /** The workspace these instances belong to — a group's lifecycle has no meaning without it. */
  workspaceId: string;
}) {
  const controller = useCommandGroupsContext();

  // The block partition, by the SAME rule as the terminals': a group takes the place of its first
  // member and gathers the others; a group with a single member here stays a bare row.
  const placed = new Set<string>();
  const blocks: Array<{ group: CommandGroup | null; items: WorkspaceRuntimeService[] }> = [];
  for (const service of services) {
    if (placed.has(service.id)) continue;
    const group = controller.groupOf(service.command_id);
    if (group == null) {
      placed.add(service.id);
      blocks.push({ group: null, items: [service] });
      continue;
    }
    const members = services.filter(
      (candidate) =>
        !placed.has(candidate.id) && controller.groupOf(candidate.command_id)?.id === group.id,
    );
    for (const member of members) placed.add(member.id);
    if (members.length < 2) {
      for (const member of members) blocks.push({ group: null, items: [member] });
      continue;
    }
    blocks.push({ group, items: members });
  }

  return (
    <>
      <RuntimeSectionList
        empty="No managed commands."
        rows={blocks.map((block) =>
          block.group == null ? (
            <ServiceRow key={block.items[0].id} service={block.items[0]} />
          ) : (
            <CommandGroupBlock
              key={block.group.id}
              group={block.group}
              index={controller.groups.findIndex((g) => g.id === block.group!.id)}
              members={block.items}
              workspaceId={workspaceId}
            />
          ),
        )}
      />
    </>
  );
}

/**
 * ONE COMMAND GROUP IN THE SIDEBAR — the shared {@link GroupBlock} with a PILOTING head.
 *
 * NO `handleRef` and NO `onOpenPanel`: composing a command group is the modal's job (finding
 * 01M11XMQA3M852JDD7R3MDG88X). What the head gets instead is the half that only exists here — the
 * aggregate over THIS workspace's instances, and the three controls acting on all of them at once.
 *
 * The `n/m` is a CLIENT-SIDE JOIN and needs no route of its own: the runtime snapshot's services
 * each carry their `command_id` and their `run_state`, and the group carries its member template
 * ids, so the aggregate is `commandGroupTally` over the members actually materialised in this
 * workspace. `total` is therefore what is RUNNABLE here, which is exactly what the buttons act on —
 * a member with no instance in this workspace is not a member the play could start.
 *
 * ## THE COUNTER LEFT THE HEAD; THE TALLY DID NOT LEAVE THE CODE
 *
 * Human review 01M12MW1DV1V455EWBMV048TE6: « la tête porte un état, jamais un nombre ». So the
 * `n/m` badge is gone from the sidebar — and `commandGroupTally` is still called on every render,
 * because the three buttons read `canStart`/`canStop`/`canRelaunch` off it. Only its DISPLAY fell.
 *
 * What the counter was doing that a dot cannot — making a DIVERGENT group legible without inventing
 * a fifth `ExecState` — comes back as the folder's hover SUMMARY. It is the assumed trade: "2 of 3
 * running" moved from a glance to a hover, and it costs the head zero pixels.
 *
 * THE DOT ITSELF IS NOT `tally.state`. The rollup is `aggregateState` over the members' own
 * notification-aware row states — the SAME function and the SAME per-leaf projection the workspace
 * accordion and the terminal folders use. That matters here rather than being tidy: `tally.state`
 * lets anything running dominate, so a group with one member running and one FAILED would show blue
 * and the mockup's own promise for a closed folder (« le dot rouge dit qu'il y a un problème sans
 * qu'on l'ouvre ») would be false. Going through `aggregateState` also gives the folder
 * `isQuietState` for free, which is what makes an idle group draw no dot at all.
 */
function CommandGroupBlock({
  group,
  index,
  members,
  workspaceId,
}: {
  group: CommandGroup;
  /** The group's rank in the project's list — how an UNNAMED group is designated. */
  index: number;
  members: readonly WorkspaceRuntimeService[];
  workspaceId: string;
}) {
  const { isConsumed } = useConsumedNotifications();
  const { select, isSelected } = useRuntimeSelection();
  const tally = commandGroupTally(members.map((service) => serviceRunState(service)));
  const label = groupLabel(group, Math.max(index, 0));
  // CLOSED BY DEFAULT, and only the first time (`useGroupFolds` persists the group's own choice from
  // its first fold on): a group of commands is a launcher you press, not a workspace you watch —
  // four commands cost ONE line folded where they cost five unfolded.
  const folds = useGroupFolds({ defaultOpen: false, load: loadGroupOpen, save: saveGroupOpen });
  const rollup = aggregateState(members.map((service) => serviceRollupState(service, isConsumed)));
  /**
   * THE FOLDER IS THE SELECTION (human review 01M14112THEB4094B2G614SXR5) — « pour les commandes y a
   * pas de notion de focus, c'est trop chelou que je doive cliquer que un des 3 pour avoir le 3 ».
   * The group is selected AS A GROUP, so `<RuntimePaneHost>` shows the whole disposition without any
   * member having to be the viewed one. And because folding closes nothing, a folder can be CLOSED
   * and ACTIVE at once: it then wears its selected fill and its rollup dot together.
   */
  const selection = { kind: "command-group", id: group.id, workspaceId } as const;
  const groupViewed = isSelected(selection);
  return (
    <GroupBlock
      groupId={group.id}
      color={group.color}
      name={group.name}
      open={folds.isOpen(group.id)}
      onToggle={() => folds.toggle(group.id)}
      onSelect={() => select(selection)}
      selected={groupViewed}
      rollup={rollup}
      summary={`${tally.running} of ${tally.total} running in ${label}`}
      actions={
        <CommandGroupControls
          groupId={group.id}
          workspaceId={workspaceId}
          groupLabel={label}
          tally={tally}
        />
      }
    >
      {members.map((service) => (
        // A MEMBER IS VIEWED WHEN ITS GROUP IS. The panes of the whole group are on screen, so the
        // Attention Inbox has been answered for each of them — without this a command could be
        // watched running and keep its unread dot forever, since the folder is now a way to look at
        // it that never selects it.
        <ServiceRow key={service.id} service={service} viewedByGroup={groupViewed} />
      ))}
    </GroupBlock>
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
  index = 0,
  activity,
  formatCwd,
}: {
  terminal: WorkspaceRuntimeTerminal;
  /**
   * The row's position in its list — it feeds ONLY {@link resolveDisplayName}'s `Terminal <n>`
   * last resort (a terminal with neither a manual label nor a usable cwd). Defaults to `0` so a
   * caller that has no list position still renders a name.
   */
  index?: number;
  activity: AgentActivity | undefined;
  /**
   * Re-render the CWD FALLBACK name for display (see `<WorkspaceRuntimeTree>`). It touches the
   * fallback ONLY: a terminal carrying a manual `label` shows that label untouched, and the
   * record's cwd is never rewritten — this is a rendering, not a value.
   */
  formatCwd?: (cwd: string) => string;
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
  const Icon = provider?.icon ?? LINE_FAMILY_ICON.terminal;
  // THE NAME — resolved by `resolveDisplayName`, the app's ONE naming authority (a manual label,
  // then the live auto label, then the cwd BASENAME, then `Terminal <n>`), so one terminal cannot
  // be called two things on two screens (review 01M15AWP3W1Y2PD3YHB406ERYT). This line used to
  // read the raw `terminal.label ?? terminal.cwd`: an unlabelled terminal — the DEFAULT case —
  // was named by its FULL path here (`c:\users\kris\appdata\local\temp\beta-ld4bfi`) while the
  // home named the same session by its basename (`beta-ld4bfi`).
  //
  // `auto` is normally `null`: this tree runs no per-row `terminal_info` poll, which is exactly
  // the static form the home passes too (`homeRowName`) — so the two surfaces resolve the SAME
  // string from the SAME two fields. The sidebar item that DOES poll (`terminal-item.tsx`) only
  // ever refines it upwards, through this same function.
  //
  // `formatCwd`, when a caller supplies one, feeds THAT slot rather than bypassing the authority:
  // the Système page renders its cwds home-relative (`~/logs`, `systemCwdLabel`) and the auto slot
  // is precisely "a computed name, under the manual label and above the bare basename". A manual
  // rename therefore still wins on the Système page, and every caller WITHOUT a `formatCwd` keeps
  // the plain basename → `Terminal <n>` ladder untouched.
  const label = resolveDisplayName(terminal, index, formatCwd ? formatCwd(terminal.cwd) : null);

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
    setDraft(terminal.label?.trim() ? terminal.label : label);
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

  // `resolveDisplayName` always yields a non-empty string (its last resort is `Terminal <n>`),
  // so the row's accessible names read the SAME name the user sees.
  const nameForLabels = label;

  return (
    <RuntimeRow
      selection={selection}
      icon={<Icon className={LINE_GLYPH_SIZE} />}
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

/**
 * A managed-command service row, with the shared start/stop/relaunch controls.
 *
 * ## NO `Clear output` HERE (human review `01M1EC7Y2625Q07C5386GKHZFS`, finding U2)
 *
 * The row briefly carried a fourth control — an eraser calling `command_clear_output` — mounted
 * only where a caller threaded it, which meant the Système page and nowhere else. The user's
 * verdict on the real app was that the verb's INCOHERENCE was the problem: the same command row
 * exists on every project surface and only one of them offered it. Given the choice between moving
 * it everywhere and dropping it, they dropped it. So the row's controls are the lifecycle trio and
 * nothing else, on every surface — one row, one set of verbs.
 *
 * The IPC itself stays (`command_clear_output`, `bridge/contract.ts`): it is the human twin of the
 * MCP `clear_command_output`, it is covered by the Système guard matrix, and removing it would be a
 * backend change the user did not ask for.
 */
function ServiceRow({
  service,
  viewedByGroup = false,
}: {
  service: WorkspaceRuntimeService;
  /**
   * Whether this row's GROUP is the selected one — its pane is then on screen even though the row
   * itself is not the selection (human review 01M14112THEB4094B2G614SXR5). It answers the Attention
   * Inbox, and nothing else: the row's own selected fill still belongs to the row's own selection.
   */
  viewedByGroup?: boolean;
}) {
  const { isSelected } = useRuntimeSelection();
  const { isConsumed, consume } = useConsumedNotifications();
  const selection = { kind: "service", id: service.id } as const;
  const selected = isSelected(selection);
  const state = serviceRunState(service);

  // Same Attention-Inbox semantics as terminals: the settled success/error dot shows only while
  // unread AND not viewed; selecting the service acknowledges it (`command_acknowledge`) + records
  // it locally. The CONTROLS keep the FACTUAL state (their own `showDot={false}`), so this only
  // governs the row's notification dot.
  const viewed = selected || viewedByGroup;
  const unread = service.unread && !isConsumed("service", service.id);
  useMarkReadOnActive("service", service.id, viewed, unread, () => consume("service", service.id));
  const dotState = notifiedRunState(state, unread, viewed);

  return (
    <RuntimeRow
      selection={selection}
      icon={<SidebarLineFamilyGlyph family="command" />}
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
