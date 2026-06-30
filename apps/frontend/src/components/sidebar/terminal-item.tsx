import { useEffect, useRef, useState } from "react";
import {
  MoreVerticalIcon,
  PencilIcon,
  SquareTerminalIcon,
  TriangleAlertIcon,
  XIcon,
} from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { nyxBridge } from "@/bridge";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Menu, MenuItem } from "@/components/ui/menu";
import { Tooltip } from "@/components/ui/tooltip";
import { isShellComm, resolveDisplayName, useAutoLabel, useShellSuffix } from "./auto-label";
import { itemTransition, itemVariants } from "./item-motion";
import { TerminalStateBadge, type BadgeState } from "./run-state";
import { useRunningDebounce } from "./use-running-debounce";
import { SidebarItemContent, sidebarRowClassName } from "./sidebar-item";
import { agentProviderFor } from "./agent-providers";
import { useTerminalAgentActivity, useTerminalAgentKind } from "./use-agent-sessions";
import { useRenameTerminal } from "./use-terminal-rename";
import { formatTerminalStats, useTerminalStatsFor } from "./use-terminal-stats";
import type { ExecState, TerminalRecord } from "./use-terminals";

/**
 * The human label for a terminal item, given a record + its list index, using
 * only the record (no live auto label). Kept as a thin wrapper over
 * `resolveDisplayName` for the static title (chrome bar) and existing tests:
 * manual `label` → cwd basename → `Terminal <n>`. The live, auto-named label
 * (cwd + foreground program) is resolved inside `<TerminalItemBody>` via
 * `useAutoLabel`; this static form is the no-auto fallback.
 */
export function displayName(record: TerminalRecord, index: number): string {
  return resolveDisplayName(record, index, null);
}

/**
 * Strip the redundant AGENT-program token from an auto label (FEEDBACK #29).
 *
 * The auto label is `<dir> · <foreground program>` (see `autoLabel`). For an agent
 * terminal that foreground program IS the agent (`claude`), so the trailing token merely
 * repeats the lead logo. Given the live `program` token (the shell/program suffix, only
 * passed when an agent provider icon is shown), drop a trailing ` · <program>` so the name
 * reads `nyx-v2` instead of `nyx-v2 · claude`. Anything else — no auto label, no program,
 * a name that does NOT end in that exact token (a real cwd basename, a manual rename) — is
 * returned untouched, so the user can always still tell terminals apart. Pure.
 */
export function dropAgentProgramToken(auto: string | null, program: string | null): string | null {
  if (!auto || !program) return auto;
  const tail = ` · ${program}`;
  return auto.endsWith(tail) ? auto.slice(0, -tail.length) : auto;
}

export interface TerminalItemProps {
  record: TerminalRecord;
  index: number;
  active: boolean;
  /**
   * Live PTY id for this terminal (or null if not spawned yet). Drives the auto
   * label (cwd + foreground program) read from `terminal_info`.
   */
  ptyId?: number | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
}

/**
 * The className for the VISUAL terminal row — the clickable inner element inside
 * `<TerminalItem>`'s height-collapsing `motion.li` wrapper. `dragging` is kept for
 * the legacy `<ReorderTerminalItem>` (no longer rendered live) and is a no-op for
 * the live row.
 *
 * `cursor-pointer`: the WHOLE row is one click target — a click ANYWHERE selects
 * it (finding 01KV3CND2GKZCG8MQCHF7W32Q5: no dead zone), since the row element
 * itself owns the click (no inner button whose bounds a click could miss). Drag
 * reorder was removed with Motion's Reorder (it could not coexist with a clean
 * height-collapse animation — see `item-motion.ts`).
 *
 * SELECTION CHANNEL: there is NO magenta BACKGROUND fill and no per-row magenta —
 * the magenta lives only in the SINGLE shared `<ActiveRail>` (a `layoutId`
 * element) rendered inside the active row, which Motion FLIPs between rows.
 * Selection is expressed on the row via the v6 DIMMED/ACTIVE model: inactive rows
 * sit at reduced opacity, the active row is full-opacity and its name goes
 * bold/white. `relative` anchors the rail. `overflow-hidden` is REQUIRED so the
 * enter/exit height collapse clips the row content.
 */
export function terminalRowClassName(active: boolean, dragging = false): string {
  // The row shape is now the SHARED `sidebarRowClassName` (the one gabarit used by
  // both terminal and command items — finding 01KV63TBV7…). `dragging` is the only
  // terminal-specific modifier left (the legacy drag-ghost opacity).
  return cn(sidebarRowClassName(active), dragging && "opacity-60");
}

/**
 * The INNER content of a terminal row — the lead glyph (with run-state badge),
 * the name (+ shell suffix), and the hover-revealed close (`x`) — WITHOUT the row
 * element itself. Wrapper-agnostic so it can live inside either a plain
 * `motion.li` (`<TerminalItem>`, used in isolation tests) or a `Reorder.Item`
 * (`<ReorderTerminalItem>`, used by the live sidebar — the WHOLE row is the drag
 * affordance, no separate grip).
 *
 * Selection is owned by the ROW (a click anywhere selects — see the wrappers);
 * this body renders the active rail and the controls only. The displayed name is
 * AUTO-COMPUTED (cwd basename + foreground program, live from `terminal_info`)
 * unless the user set a MANUAL label, which always wins.
 *
 * RENAME (FEEDBACK #30): the user can pin a manual name two ways — a "Rename" item
 * in the row's hover kebab, or a DOUBLE-CLICK on the name → an inline `<Input>`
 * (Enter/blur commit, Esc cancels). On commit we hand the TRIMMED value to the
 * rename callback (from context); an empty value clears the label back to
 * auto-naming. The manual label is never clobbered by the live `terminal_info`
 * poll: the auto name is only ever a DISPLAY-TIME fallback under a non-blank
 * `record.label` (see `resolveDisplayName`), never persisted into `terminals.label`.
 *
 * The close button stops propagation so clicking `x` closes without also selecting.
 */
export function TerminalItemBody({
  record,
  index,
  active,
  ptyId = null,
  onClose,
}: TerminalItemProps) {
  // Live auto label (debounced via the backend cache + a fixed poll). Manual
  // label takes precedence inside resolveDisplayName below. Passing `recordId`
  // also makes this poll PERSIST the live cwd into the record on change (debounced,
  // FEEDBACK #32) — the one path that already holds both the live cwd AND the
  // record id — so a relaunch re-spawns the shell at the LAST dir, not the stale
  // spawn-time cwd (incl. a `cd` into a subdir of the same workspace).
  const auto = useAutoLabel(ptyId, { recordId: record.id });
  // Live shell/program suffix for the proto row (`web · zsh`). Same backend poll.
  const shell = useShellSuffix(ptyId);

  // Provider-aware lead glyph (finding #55): while this terminal hosts a LIVE agent
  // session, show the agent's brand logo (Claude) instead of the generic terminal icon,
  // reverting the moment the session ends. The active `agent_kind` comes from the shared
  // agent-sessions context (one subscription for all rows); an unknown/absent kind →
  // `undefined` provider → the terminal icon. Reactive: the context updates on
  // `agent-sessions://changed`, so the icon swaps live.
  const agentKind = useTerminalAgentKind(record.id);
  const provider = agentProviderFor(agentKind);
  const LeadIcon = provider?.icon ?? SquareTerminalIcon;

  // DECLUTTER (FEEDBACK #29): when the AGENT logo is shown the lead glyph ALREADY says
  // "Claude", so the agent program token is redundant text on the row. The foreground
  // program of an agent terminal IS the agent (`claude`), which leaks into BOTH the
  // auto-name (`nyx-v2 · claude`) AND the shell suffix (`· claude`) — the screenshot's
  // `nyx-v2 · claude · claude`. With a provider icon present we drop that redundant token:
  //  - strip a trailing ` · <shell>` from the auto-name (only when it matches the live
  //    foreground program, i.e. it IS the agent — a manual rename or a real cwd basename
  //    is never touched, the user must still tell terminals apart),
  //  - and hide the muted shell suffix entirely (it would just repeat the icon).
  const name = resolveDisplayName(
    record,
    index,
    dropAgentProgramToken(auto, provider ? shell : null),
  );
  // DECLUTTER (FEEDBACK #29, 2nd pass): the muted `· <program>` suffix only earns its
  // place — and the row width it eats — when it names a REAL foreground program (`vim`,
  // `htop`). For a plain LOGIN SHELL (`bash`, `zsh`, …) it is pure noise (`📁 pal… · bash`)
  // the user explicitly rejected AND it pre-truncated short names. So the suffix shows only
  // when `shell` is NOT a bare shell name (and never on an agent row, where the lead logo
  // already conveys the program — the #29 pass-1 gate, kept intact).
  const showShellSuffix = !provider && shell && !isShellComm(shell);

  // RENAME (FEEDBACK #30). `rename(id, label|null)` from context pins a MANUAL name
  // (always wins over the auto name) or, with `null`, clears it back to auto-naming.
  // `editing` toggles the inline `<Input>` (opened by the kebab "Rename" item or a
  // double-click on the name); `draft` is its controlled value, seeded from the
  // current displayed name so the user edits FROM what they see.
  const rename = useRenameTerminal();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  // Open the inline editor seeded with the current displayed name (so the user
  // edits the auto name they see, not an empty field), then select all for a quick
  // overwrite once the input mounts.
  const beginRename = () => {
    setDraft(record.label?.trim() ? record.label : name);
    setEditing(true);
  };

  // Commit the draft: a TRIMMED non-empty value pins a manual label; empty clears it
  // back to auto-naming (`null`). Skip the round-trip when the value is unchanged from
  // the persisted label. Always leaves edit mode.
  const commitRename = () => {
    setEditing(false);
    const trimmed = draft.trim();
    const next = trimmed ? trimmed : null;
    const current = record.label?.trim() ? record.label.trim() : null;
    if (next !== current) rename(record.id, next);
  };

  // Focus + select-all once the input is shown, so the user can immediately type
  // over the seeded name or tweak it.
  useEffect(() => {
    if (editing) inputRef.current?.select();
  }, [editing]);

  // RUN-STATE AUTHORITY — the dot.
  //
  // For an AGENT-hosting terminal (Claude session live) the dot reflects the AGENT'S
  // activity, NOT the PTY busy bit: Claude runs INSIDE one foreground process, so the OS
  // `busy` signal is on for the whole session and tells nothing about whether Claude is
  // thinking vs. idle between turns. The runtime activity (the per-turn hooks) is the real
  // signal:
  //   - `working` → the BLUE running dot (Claude is on a turn — a prompt is in flight or a
  //     tool/subagent is running; an in-flight tool keeps it blue with no timer).
  //   - `waiting` → the YELLOW dot (Claude is BLOCKED on the user: an `AskUserQuestion`
  //     tool in flight, or a permission/elicitation prompt). A LIVE state, like running.
  //   - a finished turn → the focus-aware "response ready" GREEN dot (success), shown only
  //     while UNREAD and the terminal is not active — IDENTICAL semantics to a settled
  //     `exec_state_unread` (the user only needs notifying when not looking at the session).
  //   - otherwise idle.
  //
  // For a NON-agent terminal the behaviour is unchanged (PRD task #1): the RUNNING dot is
  // the OS `busy` signal, the SETTLED success/error badge is `exec_state` + its persisted
  // `exec_state_unread`.
  const activity = useTerminalAgentActivity(record.id);
  const isAgent = agentKind !== null;

  // STALE-PLUGIN badge (#18b): when the nyx plugin THIS session loaded is older than the
  // version nyx bundles, the loaded hooks are out of date with NO other signal — restarting
  // the session reloads them. The runtime activity carries the per-session verdict (set once
  // at SessionStart); we surface a tiny MUTED ⚠ affordance on the row inviting a restart.
  // Light by design (cf. the sidebar redesign #6): hover-revealed, no persistent clutter.
  const pluginOutdated = activity?.pluginOutdated ?? false;

  // PER-TERMINAL CPU%/RAM (FEEDBACK #28). The live process-tree reading for THIS terminal
  // from the shared stats context (one subscription for all rows), or null before the
  // first poll tick. UI is a LIGHT PLACEHOLDER pending the sidebar redesign (#6): a
  // compact muted `1.2% · 340 MB` revealed only on row hover — no persistent clutter.
  const stats = useTerminalStatsFor(record.id);

  // The dot has channels — a LIVE signal (running/waiting) and a SETTLED notification —
  // chosen per terminal kind. For an AGENT terminal the agent activity drives them (the
  // always-on PTY `busy` bit is ignored); for a NON-agent terminal the OS `busy` + OSC-133
  // `exec_state` drive them (the unchanged PRD task #1 behaviour). `waiting` is agent-only.
  let running: boolean;
  let waiting: boolean;
  let settled: ExecState;
  let unread: boolean;
  if (isAgent) {
    // AGENT: `working` → running (blue), `waiting` → waiting (yellow); a pending "response
    // ready" → the settled SUCCESS channel (the green dot), gated by unread + not-active in
    // the badge — IDENTICAL semantics to `exec_state_unread`. A turn that ENDED ON AN API
    // ERROR (`StopFailure`, #35) → the settled ERROR channel (the RED dot), with PRIORITY
    // over the green — same focus-aware unread semantics, just red.
    running = activity?.activity === "working";
    waiting = activity?.activity === "waiting";
    settled = activity?.errorUnread ? "error" : activity?.readyUnread ? "success" : "idle";
    unread = (activity?.errorUnread || activity?.readyUnread) ?? false;
  } else {
    running = record.busy ?? false;
    waiting = false;
    settled =
      record.exec_state === "success" || record.exec_state === "error" ? record.exec_state : "idle";
    unread = record.exec_state_unread ?? false;
  }

  // Anti-flicker (finding #14): a sustained LIVE state (> threshold) reveals the dot; a fast
  // running episode never flashes.
  //  - NON-agent: keep the SINGLE-channel debounce (the unchanged behaviour) — an instant
  //    command's running→settled within the threshold shows NOTHING (neither dot nor result).
  //  - AGENT: debounce the LIVE channel (running OR waiting are both "busy/blocked") and
  //    overlay the settled "ready" DIRECTLY, so a freshly-pulled green dot is never swallowed
  //    by the transient PTY-`busy` "running" the row briefly shows before the agent context
  //    loads. Once the live state survives the debounce, the resolved kind is `waiting`
  //    (yellow) when the agent is blocked, else `running` (blue).
  const live = running || waiting;
  const nonAgentRaw: ExecState = running ? "running" : settled;
  const agentLive = useRunningDebounce(isAgent ? (live ? "running" : "idle") : "idle");
  const nonAgentState = useRunningDebounce(isAgent ? "idle" : nonAgentRaw);
  const state: BadgeState = isAgent
    ? agentLive === "running"
      ? waiting
        ? "waiting"
        : "running"
      : settled
    : nonAgentState;

  // ACTIVE-SETTLE for the AGENT "response ready" notification (the activity mirror of the
  // manager's exec-state active-settle): if a turn FINISHES (ready raised) while this row
  // is the one being VIEWED (active), acknowledge it at once so it never accumulates a
  // green dot on the very session the user is looking at — and stays cleared after
  // re-deselect. A row VIEWED after the turn finished is handled by the manager's
  // `selectTerminal` → `agent_mark_ready_read`; this covers the arrives-while-active case.
  // Best-effort, runtime-only (the backend nudges `agent-sessions://changed` to re-pull).
  useEffect(() => {
    if (active && (activity?.readyUnread || activity?.errorUnread)) {
      void nyxBridge.invoke("agent_mark_ready_read", { terminalId: record.id }).catch(() => {});
    }
  }, [active, activity?.readyUnread, activity?.errorUnread, record.id]);

  return (
    // The SHARED item layout (lead → name → actions). The magenta selection bar is
    // not per-row: a single MEASURED rail (see `useActiveRail` / `<SelectionRail>`)
    // tracks the active row by `[data-rail-row][aria-current]`.
    <SidebarItemContent
      // Lead glyph + run-state corner badge (the run-state channel, orthogonal to
      // selection). A settled badge shows only while the PERSISTED `exec_state_unread`
      // flag is set (PRD-2.1); `running` always pulses; `idle` shows nothing — see
      // <TerminalStateBadge>. The wrapper is NOT `aria-hidden` so the badge's
      // `role="status"` stays in the a11y tree; only the icon is hidden.
      lead={
        <span
          className="relative flex shrink-0 items-center"
          // Label the glyph when it is an agent logo so the swap is discoverable to AT
          // (the generic terminal icon stays decorative / aria-hidden).
          title={provider?.label}
        >
          <LeadIcon
            aria-hidden
            className={cn("size-3.5", active ? "text-sidebar-foreground" : "text-muted-foreground")}
          />
          <TerminalStateBadge state={state} unread={unread} active={active} />
        </span>
      }
      name={
        editing ? (
          // INLINE RENAME (#30): a controlled `<Input>` swapped in for the name.
          // Enter / blur commit, Esc cancels. `stopPropagation` on the pointer +
          // click keeps typing/selecting in the field from selecting/closing the
          // row (the row owns the select click).
          <Input
            ref={inputRef}
            aria-label={`Rename terminal ${name}`}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onClick={(e) => e.stopPropagation()}
            onPointerDown={(e) => e.stopPropagation()}
            onDoubleClick={(e) => e.stopPropagation()}
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
            className="h-6 w-full min-w-0 px-1.5 py-0 text-sm"
          />
        ) : (
          // Double-click the name to rename inline (mirrors the kebab "Rename").
          // `stopPropagation` so the dbl-click does not also fire a select.
          <span
            onDoubleClick={(e) => {
              e.stopPropagation();
              beginRename();
            }}
          >
            {name}
          </span>
        )
      }
      // Shell/program suffix ("· zsh") — muted, hidden while there's no room. Suppressed
      // for an agent row (FEEDBACK #29): the program there IS the agent, already conveyed
      // by the lead logo, so the suffix would only repeat it.
      suffix={
        showShellSuffix && <span className="shrink-0 text-xs text-muted-foreground">· {shell}</span>
      }
      actions={
        <>
          {/* FEEDBACK #18b — STALE-PLUGIN badge: a tiny MUTED ⚠ on a row whose Claude session
            loaded an OUT-OF-DATE nyx plugin (its hooks are stale until the session restarts).
            Always visible (it is actionable, unlike the hover-only stats) but muted/light so
            it never shouts; the tooltip explains the fix. `role="img"` + the tooltip label
            keep it discoverable to AT. */}
          {pluginOutdated && (
            <Tooltip label="Plugin nyx périmé — redémarre la session" side="top">
              <span
                role="img"
                aria-label="Plugin nyx périmé — redémarre la session"
                className="flex shrink-0 items-center text-muted-foreground/70"
              >
                <TriangleAlertIcon aria-hidden className="size-3" />
              </span>
            </Tooltip>
          )}
          {/* FEEDBACK #28 — LIGHT PLACEHOLDER (pending the sidebar redesign #6): a compact,
            MUTED process-tree CPU%/RAM readout revealed only on row hover (focus too),
            never persistent clutter. `tabular-nums` keeps the digits from jittering as the
            poll updates; it sits left of the close `x`. */}
          {stats && (
            <span
              className="shrink-0 text-[0.65rem] tabular-nums text-muted-foreground opacity-0 transition group-hover:opacity-100 group-focus-within:opacity-100"
              title="CPU · RAM (shell + descendants)"
            >
              {formatTerminalStats(stats)}
            </span>
          )}
          {/* FEEDBACK #30 — RENAME: a hover-revealed kebab carrying a "Rename" action
            (mirrors the workspace header kebab). Opens the inline editor seeded with
            the current name. `onPointerDown`/`onClick` stop propagation so opening the
            menu never also selects/drags the row. Hidden while the inline editor is
            already open. */}
          {!editing && (
            <span
              className="shrink-0 opacity-0 transition focus-within:opacity-100 group-hover:opacity-100 data-popup-open:opacity-100"
              onPointerDown={(e) => e.stopPropagation()}
              onClick={(e) => e.stopPropagation()}
            >
              <Menu
                tooltip="Terminal actions"
                trigger={
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    aria-label={`Terminal actions for ${name}`}
                    className="size-5"
                  >
                    <MoreVerticalIcon />
                  </Button>
                }
              >
                <MenuItem
                  icon={<PencilIcon className="size-4" />}
                  onClick={beginRename}
                  aria-label={`Rename terminal ${name}`}
                >
                  Rename
                </MenuItem>
              </Menu>
            </span>
          )}
          <Button
            variant="ghost-destructive"
            size="icon-xs"
            aria-label={`Close terminal ${name}`}
            // Stop propagation so closing never also selects the row (the row owns
            // the select click). `onPointerDown` stop keeps the drag controller from
            // treating an x-click as the start of a row drag.
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              onClose(record.id);
            }}
            className={cn(
              "size-5 shrink-0 rounded opacity-0 transition focus-visible:opacity-100 group-hover:opacity-100",
              active && "opacity-100",
            )}
          >
            <XIcon className="size-3.5" />
          </Button>
        </>
      }
    />
  );
}

/**
 * The LIVE sidebar terminal row: a height-collapsing `motion.li` wrapper hosting
 * the clickable visual row + `<TerminalItemBody>`. This is THE row the sidebar
 * renders everywhere now (the old `Reorder.Item`-based `<ReorderTerminalItem>` was
 * dropped — Motion's Reorder could not coexist with a clean height collapse).
 *
 * TWO LEVELS on purpose:
 *  - the OUTER `motion.li` is the SINGLE animator (height 0↔auto, opacity, a tiny
 *    translate via `itemVariants`) and carries NO `layout` prop, so the animated
 *    height reflows neighbours + parent bands in normal flow with nothing to fight
 *    (the double-tp fix). `overflow-hidden` clips the content as it collapses;
 *  - the INNER `div` is the visual, clickable row (its `py` padding lives INSIDE
 *    the clip, so the collapse reaches a true 0 — no padding residual / leftover
 *    pop). A click anywhere on it selects the terminal.
 *
 * Animation is chrome-only and never touches the xterm viewport (a hard rule).
 */
export function TerminalItem(props: TerminalItemProps) {
  const reduced = useReducedMotion();
  return (
    <motion.li
      variants={itemVariants}
      initial="initial"
      animate="animate"
      exit="exit"
      transition={itemTransition(reduced)}
      onClick={() => props.onSelect(props.record.id)}
      aria-current={props.active ? "true" : undefined}
      data-rail-row
      className="overflow-hidden"
      style={{ listStyle: "none" }}
    >
      <div className={terminalRowClassName(props.active)}>
        <TerminalItemBody {...props} />
      </div>
    </motion.li>
  );
}

export default TerminalItem;
