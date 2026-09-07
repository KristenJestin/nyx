import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useHotkey } from "@tanstack/react-hotkeys";
import { ArrowDownIcon, ArrowUpIcon, CornerDownLeftIcon, SearchIcon } from "lucide-react";

import { nyxBridge } from "@/bridge";
import type { GlobalSearchHit } from "@/bridge/contract";
import { cn } from "@/lib/utils";
import { startNavTransition } from "@/lib/nav-transition";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { MicroLabel } from "@/components/ui/micro-label";
import { Spinner } from "@/components/ui/spinner";
import { SEARCH_PALETTE_CHORD } from "@/components/sidebar/use-terminal-shortcuts";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { useActiveProject } from "@/components/terminals/active-project";
import { usePendingTerminal } from "@/components/terminals/pending-terminal";
import { useRevealWorkspace } from "@/components/terminals/reveal-workspace";

import { SearchActionRow } from "./search-action-row";
import { searchActionsFor, type SearchAction, type SearchActionContext } from "./search-actions";
import { relativeSuggestionTime } from "./search-meta";
import { SearchRow } from "./search-row";
import { resolveSearchTarget, type SearchTarget } from "./search-target";
import { useGlobalSearch } from "./use-global-search";

/**
 * `<SearchPalette>` — the `Mod+K` GLOBAL SEARCH, mounted ONCE on the always-mounted shell.
 *
 * ## Why the chord is the hard part
 *
 * The focus in nyx belongs to a terminal by default: the focused pane's xterm keeps a textarea
 * focused at all times and STOPS PROPAGATION for every key it consumes. A palette bound with
 * `useHotkey` alone would therefore be unreachable from the one place the user's hands actually
 * are. The chord only works because `isTerminalNavChord`
 * (`components/sidebar/use-terminal-shortcuts.ts`) yields `k` — xterm lets the keystroke through
 * untouched, no PTY byte, no `stopPropagation` — and TanStack's manager, listening on `document`,
 * finally sees it. The binding constant lives in that same module so the yield list and the
 * bindings cannot drift apart.
 *
 * ## What it owns, and what it deliberately does not
 *
 * It owns the OPEN/CLOSE, the query, the keyboard walk and the activation. It owns NO ranking: the
 * order and the caps (8 per type, 40 overall) are the aggregator's answer (`nyx_core::search`), and
 * re-sorting them here would be a second, quieter product rule. It owns no route either beyond the
 * mapping in `./search-target`, and creates none.
 *
 * That "no ranking" is what the results state now looks like: ONE list, drawn in the order it
 * arrived, with no section per type. The order is the aggregator's `by_rank` — the certainty bucket
 * first (an exact label match, whatever its type, above a partial one), then the score, then
 * recency — and it could not be recomputed here anyway, since the bucket is not a field of the wire
 * hit. Grouping by type was the palette's one remaining opinion about relevance, and it was the
 * wrong one: it put a mediocre Spec above a command named exactly what was typed.
 *
 * It owns NO ACTION either — only the WALK over them. The palette is no longer only a way to GO
 * somewhere: a workspace that surfaces offers `Open a new terminal`, a project offers `Add a
 * workspace`. Which affordance rides with which kind is declared in `./search-actions`, a
 * table beside the results (the user's own shape: « une liste à côté sur toutes les actions
 * possibles et ça les remonte sur le projet / workspace correspondant »); adding a third is one
 * more entry there, and nothing here. What the entries CALL is likewise not written here: the
 * terminal comes from the Terminals page's own `newTerminalInWorkspace` through the one-shot
 * `terminals/pending-terminal` channel, the workspace from the rail's `useManualAdd` flow handed
 * down as `onAddWorkspace`. The palette holds neither creation path — it only names them.
 *
 * It owns NO CONTEXT either, and that is a change: the project name used to be handed down as a
 * `project_id → name` map built from the rail's tree, which knows only the projects the rail loaded
 * and knows nothing at all of workspaces. The hit now carries both names (`project_name`,
 * `workspace_name`), joined at the source — the same reads the backend matches on.
 *
 * ## The body never blinks: the wait is drawn in the field
 *
 * A search is re-issued at every settled keystroke, and the panel used to swap its whole content
 * for a centred spinner while one was out. With a 180 ms debounce restarted on each letter, typing
 * a three-word query collapsed and refilled the palette a dozen times — the user's « le loading en
 * gros au milieu c'est non ».
 *
 * So the body draws the LAST SETTLED ANSWER and is replaced only when the next one arrives
 * (stale-while-revalidate), while the request itself shows as a spinner IN THE QUERY FIELD, in the
 * search glyph's own place. There is deliberately no branch on `loading` below: making it a branch
 * is what made it exclusive of the four states that are actual answers. The one moment nothing is
 * kept is the very first load, and the answer there is to draw nothing at all rather than a
 * spinner.
 *
 * `useGlobalSearch` remains the sole authority on WHAT is displayed — its `cancelled` guard and its
 * sequence ref cover `loading` exactly as they cover the hits, so keeping the previous content
 * gives a late response no way back in.
 *
 * ## The two focus rules
 *
 * The query input takes the focus on open (`initialFocus`) and NEVER gives it up: `↑` `↓` `↵` are
 * handled on the input, and a row is a `role="option"` whose click is `preventDefault`ed on
 * mousedown, so one types and walks the list at once. On close — Escape, a scrim click or an
 * activation, the three going through the SAME path — the focus returns to whatever held it when
 * the palette opened (`finalFocus`), which is the terminal one was working in.
 *
 * THE ONE EXCEPTION IS AN ACTION, AND IT IS DELIBERATE. An action HANDS THE APP OVER to a surface
 * that wants the keyboard for itself: the add-workspace NAME DIALOG, or the brand-new terminal,
 * which focuses itself as soon as it becomes the active pane (`terminal/terminal.tsx`). Restoring
 * the focus to the terminal one came FROM would fight both. So an action closes the palette with
 * `finalFocus={false}` — Base UI then moves the focus NOWHERE and the incoming surface claims it
 * unopposed. Escape, the scrim and a plain hit activation are untouched: they restore as before.
 *
 * ## No preselection while there is no query
 *
 * In the `empty` state NOTHING is preselected, so a reflex `Mod+K ↵` does nothing. This is not
 * timidity: activating a hit ENTERS its project, and entering a project PERSISTS (it is remembered
 * across relaunches), so a stray `↵` would durably move the user somewhere they never asked to go.
 * The preselection of the first row comes back the moment there are results.
 *
 * The rule holds for the MOUSE too, and not only at t=0: hovering a suggestion does not arm it
 * either, because the palette opens under the cursor and a drifting mouse would otherwise hand `↵`
 * a target no keystroke chose. Clicking a suggestion, which names its row, works as always.
 */

/** How many suggestions the empty state shows — the aggregator's per-type cap, and what fits. */
export const SUGGESTION_CAP = 8;

/**
 * ONE line of the palette — an ENTITY the backend found, or an ACTION derived from one.
 *
 * The two are a single sequence rather than two lists, because the keyboard walks ONE list: `↑` `↓`
 * step through hits and actions alike, `↵` activates whichever is under the cursor, and the active
 * index means the same thing in both cases. The actions of a hit are expanded immediately AFTER it
 * (`rows` below), so one reads the entity and then what can be done with it.
 */
type PaletteRow =
  | { readonly type: "hit"; readonly hit: GlobalSearchHit }
  | { readonly type: "action"; readonly hit: GlobalSearchHit; readonly action: SearchAction };

export function SearchPalette({
  onAddWorkspace,
  systemProjectId = null,
}: {
  /**
   * Add a workspace to a project: the rail's OWN flow (`useManualAdd`'s picker → name dialog →
   * `create_workspace`), handed down by `<BaseLayout>` because that is where it is already mounted.
   * The palette writes no creation of its own. Absent ⇒ the `project` action is inert, which is
   * what a palette mounted outside the shell (a test, the catalogue) sees.
   */
  onAddWorkspace?: (projectId: string) => void;
  /**
   * The SYSTÈME singleton's project id, or `null` when the database has none — handed down by
   * `<BaseLayout>` for the same reason as the flow above: the shell already holds that read
   * (`system_scope_detail`, for the rail's tile), and a second one here would be a second
   * subscription onto one fact. It is what tells a HOST hit from a project's — see
   * {@link SearchTargetContext.systemProjectId}. Absent ⇒ every hit resolves as a project's, which
   * is exactly right for a database with no singleton.
   */
  systemProjectId?: string | null;
} = {}) {
  const navigate = useNavigate();
  const { setActiveProjectId, setScopeKind } = useActiveProject();
  const { revealWorkspace } = useRevealWorkspace();
  const { requestTerminal } = usePendingTerminal();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  // `-1` = NOTHING is active (see the header: the empty state has no preselection).
  const [activeIndex, setActiveIndex] = useState(-1);
  const inputRef = useRef<HTMLInputElement>(null);
  // ONE activation at a time. `activate` awaits a liveness read before it closes anything on the
  // `session` kind, and that await is a window: a second `↵` — or an `↵` landing on a click already
  // in flight — used to start a second activation that entered a project, raised a second reveal
  // and navigated again, the two racing to decide where the user ends up. A ref rather than state
  // because it gates the very call that would re-render, and it is cleared on close so a palette
  // reopened after a failed activation is not left inert.
  const activatingRef = useRef(false);
  // The element that held the keyboard when the palette opened — where the focus goes back on
  // close. It is the focused pane's xterm textarea in the case this whole feature is built for.
  const restoreRef = useRef<HTMLElement | null>(null);
  // THE HANDOVER: this close gives the keyboard to a surface the palette itself opened (the
  // add-workspace dialog, the terminal an action just created), so it must restore the focus to
  // NOBODY. Base UI reads `finalFocus` at close time through a ref onto the latest render, and
  // `false` means "move focus nowhere" — see the header's two focus rules. Reset on every open.
  const [focusHandoff, setFocusHandoff] = useState(false);
  const listId = useId();

  const { state, hits, knowledgeEmpty, loading, answered } = useGlobalSearch(query, open);

  // `Mod+K` — Cmd on macOS, Ctrl on Windows/Linux, resolved by TanStack Hotkeys (the platform
  // mapping is the library's, never re-implemented here). Ctrl/Meta chords do not skip inputs by
  // default, so it also fires from the query input itself and from any other field.
  useHotkey(SEARCH_PALETTE_CHORD, () => {
    if (open) {
      inputRef.current?.focus();
      return;
    }
    const held = document.activeElement;
    restoreRef.current = held instanceof HTMLElement ? held : null;
    // A palette always opens BLANK: the previous search is not a state one returns to.
    setQuery("");
    setActiveIndex(-1);
    setFocusHandoff(false);
    // A fresh opening is a fresh intent: whatever the last activation did — landed, or threw
    // before it could — this palette accepts one again.
    activatingRef.current = false;
    setOpen(true);
  });

  // The rows, in the order they are DRAWN — which is the order the keyboard walks. BOTH states are
  // one flat list now: the results in the aggregator's cross-type rank order, the suggestions in
  // its recency order, capped.
  const suggestions = useMemo(
    () => (state === "empty" ? hits.slice(0, SUGGESTION_CAP) : []),
    [state, hits],
  );
  // THE ACTIONS RIDE ALONG WITH THEIR ENTITY — expanded HERE, on the answer the backend already
  // settled, and never asked for.
  //
  // THAT IS WHAT KEEPS THE CAPS INTACT. The 8-per-type and 40-overall caps belong to
  // `nyx_core::search` and apply to `hits`; the actions are added to the DRAWN sequence afterwards,
  // so a workspace that surfaces still costs one workspace slot and its `Open a new terminal` line
  // costs none. An affordance derived from a result can never evict a result.
  //
  // AND ONLY IN THE `results` STATE. The suggestions answer "what was touched recently", not a
  // question the user asked: no entity was SEARCHED there, so nothing is offered to act upon — a
  // reflex `↵` on a suggestion must not be able to create anything (the same reasoning as the
  // no-preselection rule above, one step further).
  const rows = useMemo<readonly PaletteRow[]>(() => {
    if (state !== "results") return suggestions.map((hit) => ({ type: "hit", hit }) as const);
    return hits.flatMap((hit) => [
      { type: "hit", hit } as const,
      ...searchActionsFor(hit).map((action) => ({ type: "action", hit, action }) as const),
    ]);
  }, [state, hits, suggestions]);

  // An INSTALL with no Spec, no task and no idea: the suggestions would be a list of the containers
  // one just created, which answers nothing. It says so instead — the sixth case, which none of the
  // five states covered (it is neither "no query" nor "the query found nothing").
  //
  // The verdict is the BACKEND'S (`knowledge_empty`), never the composition of the list above. That
  // list is capped at eight, so "no Spec among the suggestions" is not "no Spec in the database" —
  // and on a machine where agents run, sessions and commands saturate those eight while the Specs
  // sit still. Inferred here, the empty state fired on installs holding dozens of Specs.
  const emptyInstall = state === "empty" && knowledgeEmpty;

  // Every settled answer re-seeds the selection: the FIRST row when there are results (typing then
  // pressing `↵` must open the best match), NOTHING otherwise. Keyed on the response identity, so
  // a `↓` pressed in the empty state is not undone by the next render.
  useEffect(() => {
    setActiveIndex(state === "results" && hits.length > 0 ? 0 : -1);
  }, [state, hits]);

  // Keep the active row visible when the keyboard walks past the fold. `scrollIntoView` is absent
  // in jsdom, so it is feature-detected rather than assumed.
  useEffect(() => {
    if (activeIndex < 0) return;
    const el = document.getElementById(`${listId}-row-${activeIndex}`);
    if (el && typeof el.scrollIntoView === "function") el.scrollIntoView({ block: "nearest" });
  }, [activeIndex, listId]);

  const close = useCallback(() => setOpen(false), []);

  /**
   * ACTIVATE a hit: enter its project, reveal its workspace, then navigate to the nearest existing
   * surface.
   *
   * The project is activated BEFORE the navigation because every page reads through the entered
   * project — and that activation is PERSISTED, so jumping into another project's Spec durably
   * moves where one is. That is the intended behaviour, and the reason the empty state preselects
   * nothing.
   *
   * THE WORKSPACE IS REVEALED IN THAT SAME BREATH, and for a strictly weaker reason. Landing on the
   * right page is not landing on the entity: a workspace band opens by RANK on the Terminals page
   * (`defaultOpen={index === 0}`), so a jump into the second, third or tenth workspace of a project
   * arrived on a closed band with the target inside it — the user's « le workspace est fermé donc
   * c'est bizarre ». The signal is raised HERE, before `navigate`, so the band is open in the very
   * first paint that follows and the target never appears in a second beat.
   *
   * Unlike the project activation, that signal PERSISTS NOTHING — no `set_workspace_collapsed`, no
   * backend event, nothing carried to the next relaunch. Arriving somewhere is not a preference
   * about how the sidebar should fold; `terminals/reveal-workspace` carries the whole rationale.
   *
   * The SESSION case is the only one that needs to ask the backend anything: a session is reached
   * through its terminal, so a terminal that is no longer alive must not be deep-linked (the page
   * would wait forever for a target that will never appear, with its last-terminal restore
   * suppressed — an empty deck). The liveness read happens HERE, on activation, never per
   * keystroke; a failed read is treated as "not alive", which costs a deep-link and never an error.
   */
  const activate = useCallback(
    async (hit: GlobalSearchHit, { handOverFocus = false }: { handOverFocus?: boolean } = {}) => {
      if (activatingRef.current) return;
      activatingRef.current = true;
      let terminalAlive = false;
      if (hit.kind === "session" && hit.terminal_id != null && hit.terminal_id !== "") {
        try {
          const rowsAlive = await nyxBridge.invoke<TerminalRecord[]>("list_terminals");
          terminalAlive =
            Array.isArray(rowsAlive) &&
            rowsAlive.some((r) => r.id === hit.terminal_id && r.status === "alive");
        } catch {
          terminalAlive = false;
        }
      }
      const target = resolveSearchTarget(hit, { terminalAlive, systemProjectId });
      // Batched with the close below, so the popup is already rendered with `finalFocus={false}`
      // when Base UI decides where the focus goes.
      if (handOverFocus) setFocusHandoff(true);
      close();
      // ACTIVATING A HIT ENTERS ITS SCOPE — the same ruling the cross-project home's rows took
      // (review `01M1ECXAXVGHJWPWA019QAER6G`: clicking a row IS entering what holds it). A HOST hit
      // enters the Système scope and touches the entered project not at all — the singleton has no
      // id to put there, and the project is where the rail puts the user back.
      if (target.to === "/system") {
        setScopeKind("system");
      } else {
        setActiveProjectId(target.projectId);
        setScopeKind("project");
      }
      // Keyed on `workspace_id` rather than on a list of kinds: the field IS the question ("does
      // this entity live in a band?"), and the backend sets it on exactly the kinds that do — a
      // command instance, an agent session, the terminal reached through it, and a workspace hit
      // itself, whose band is the target outright. The six kinds that live nowhere narrower than
      // their project carry `null` here and raise no signal at all.
      revealWorkspace(hit.workspace_id ?? null);
      startNavTransition(() => {
        void navigateToTarget(navigate, target);
      });
    },
    [close, navigate, revealWorkspace, setActiveProjectId, setScopeKind, systemProjectId],
  );

  /**
   * The capabilities the {@link SEARCH_ACTIONS} registry calls back into — the app's EXISTING
   * creation paths, and no second one.
   *
   * `newTerminalInWorkspace` raises the one-shot `terminals/pending-terminal` signal rather than
   * creating anything: the path that creates a terminal AND ENTERS IT is the Terminals page's
   * (`useTerminalActions`, whose "enter" is a selection in the page's own runtime), and the page is
   * where it must therefore happen. The signal is raised BEFORE the landing below so the page finds
   * it already standing in the first render that follows.
   *
   * `addWorkspaceToProject` is the rail's `useManualAdd` flow, handed down by `<BaseLayout>`:
   * native folder picker, then the name dialog, then `create_workspace`. Cancelling the picker
   * returns from it having created NOTHING — no dialog, no record, no state to unwind here either,
   * which is what makes "cancel leaves the app coherent" a property of the existing flow rather
   * than a new rule.
   */
  const actionContext = useMemo<SearchActionContext>(
    () => ({
      newTerminalInWorkspace: (workspaceId, projectId) => requestTerminal(workspaceId, projectId),
      addWorkspaceToProject: (projectId) => onAddWorkspace?.(projectId),
    }),
    [onAddWorkspace, requestTerminal],
  );

  /**
   * ACTIVATE an action row.
   *
   * It LANDS EXACTLY LIKE ITS ENTITY WOULD, and then does its extra thing. `Add a workspace` under
   * `depot` first does what activating `depot` does — enter the project, go to its Terminals page —
   * so the workspace being created appears where the user is looking, and a CANCELLED picker
   * leaves them on the project they just named rather than half-way to nowhere. Nothing is created
   * by the landing itself, so a cancel creates nothing, which is the whole requirement.
   *
   * The action runs FIRST so its signal (the pending terminal) is set in the same commit as the
   * navigation; and the landing hands the FOCUS over rather than restoring it — see the header.
   */
  const activateAction = useCallback(
    (hit: GlobalSearchHit, action: SearchAction) => {
      // The same one-at-a-time gate as `activate`, read HERE because `run` fires BEFORE it and
      // would otherwise escape the guard entirely: two quick `↵` on `Add a workspace` opened two
      // native folder pickers. The check and the set that follows inside `activate` are separated
      // by no `await`, so nothing can slip between them.
      if (activatingRef.current) return;
      action.run(hit, actionContext);
      void activate(hit, { handOverFocus: true });
    },
    [actionContext, activate],
  );

  /** Activate whichever species of row is under the cursor — the ONE path `↵` and a click share. */
  const activateRow = useCallback(
    (row: PaletteRow) => {
      if (row.type === "action") activateAction(row.hit, row.action);
      else void activate(row.hit);
    },
    [activate, activateAction],
  );

  const move = useCallback(
    (delta: 1 | -1) => {
      const count = rows.length;
      if (count === 0) return;
      setActiveIndex((current) => {
        if (current < 0) return delta > 0 ? 0 : count - 1;
        return (current + delta + count) % count;
      });
    },
    [rows.length],
  );

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        move(1);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        move(-1);
        return;
      }
      if (e.key === "Enter") {
        // NOTHING active ⇒ nothing happens. That is the empty state's whole rule.
        const row = activeIndex >= 0 ? rows[activeIndex] : undefined;
        if (!row) return;
        e.preventDefault();
        activateRow(row);
      }
    },
    [activateRow, activeIndex, move, rows],
  );

  // THE MOUSE ARMS A ROW ONLY WHERE THE KEYBOARD ALREADY MAY. In the `results` state hovering
  // preselects, as one expects of a list. In the EMPTY state it must not: the palette opens at
  // `top-[12vh]`, centred — right under the cursor of someone who just clicked in a pane — so a
  // one-pixel drift would arm a suggestion, and the reflex `↵` that follows would ENTER another
  // project durably, by a path `↓` never walked. That is the exact outcome the no-preselection rule
  // exists to prevent, and the rule is about what `↵` may do, not only about what t=0 looks like.
  // A CLICK is untouched in both states: it names its target.
  const hoverArms = state === "results";

  // ONE renderer for both species, so a row's id, its active state and its hover rule are written
  // once and cannot drift between an entity and the affordance hanging under it.
  const rowFor = (row: PaletteRow, index: number, time?: string) => {
    const id = `${listId}-row-${index}`;
    const active = index === activeIndex;
    const onHover = hoverArms ? () => setActiveIndex(index) : undefined;
    return row.type === "action" ? (
      <SearchActionRow
        key={`${row.hit.kind}:${row.hit.id}:${row.action.id}`}
        id={id}
        hit={row.hit}
        action={row.action}
        active={active}
        onActivate={() => activateAction(row.hit, row.action)}
        onHover={onHover}
      />
    ) : (
      <SearchRow
        key={`${row.hit.kind}:${row.hit.id}`}
        id={id}
        hit={row.hit}
        active={active}
        time={time}
        onActivate={() => void activate(row.hit)}
        onHover={onHover}
      />
    );
  };

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        // ESCAPE AND THE SCRIM COME THROUGH HERE — the same single close path as an activation, so
        // the focus restoration below can never depend on HOW the palette was dismissed.
        if (!next) close();
      }}
    >
      <Dialog.Portal>
        <DialogBackdrop />
        <DialogPopup
          aria-label="Global search"
          initialFocus={inputRef}
          // `false` = MOVE FOCUS NOWHERE — the handover an action needs (see the header). Otherwise
          // the focus goes back to whatever held it when the palette opened.
          finalFocus={focusHandoff ? false : restoreRef}
          className={cn(
            // A palette is not a modal card: it hangs from the top of the window, is wider, and
            // its body scrolls. The rest (border, popover surface, shadow) is the dialog's own.
            "top-[12vh] w-[min(40rem,calc(100vw-3rem))] -translate-y-0 overflow-hidden p-0",
          )}
        >
          <Dialog.Title className="sr-only">Global search</Dialog.Title>
          <div className="flex items-center gap-2.5 border-b border-border/60 px-3.5 py-2.5">
            {/* THE SEARCH GLYPH IS THE LOADING SLOT. The spinner takes the icon's place rather
                than sitting beside it, so the header never reflows and the indication lands where
                the eye already is — on the field being typed in. Both are decorative: the fact is
                announced by the live region below, not by an animation. */}
            {loading ? (
              <Spinner aria-hidden className="size-4 shrink-0 text-muted-foreground" />
            ) : (
              <SearchIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
            )}
            <input
              ref={inputRef}
              type="text"
              role="combobox"
              aria-expanded
              aria-controls={listId}
              aria-autocomplete="list"
              aria-activedescendant={activeIndex >= 0 ? `${listId}-row-${activeIndex}` : undefined}
              aria-label="Search everything"
              aria-busy={loading}
              placeholder="Search Specs, tasks, ideas, workspaces, commands, sessions…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={onKeyDown}
              className="w-full border-0 bg-transparent p-0 text-sm text-foreground outline-none placeholder:text-muted-foreground/60"
            />
            {/* A SPINNING GLYPH SAYS NOTHING TO A SCREEN READER. The wait is announced here, in a
                polite live region that is part of the header — the same place, told the other way. */}
            <span role="status" aria-live="polite" className="sr-only">
              {loading ? "Searching…" : ""}
            </span>
          </div>

          <div
            id={listId}
            role="listbox"
            aria-label="Search results"
            className="max-h-[52vh] overflow-y-auto p-1.5"
          >
            {/* NO LOADING BRANCH HERE, and that is the point: the body draws the LAST SETTLED
                answer and is replaced only when the next one lands. A branch on "a request is in
                flight" would be exclusive of the four below, which is what emptied the panel at
                every keystroke — with a 180 ms debounce restarted on each letter, a three-word
                query blinked three times a word. The wait is drawn in the field above instead. */}
            {state === "error" ? (
              <div className="px-2 py-8 text-center text-xs text-destructive">
                Search failed. Try again.
              </div>
            ) : null}

            {/* `answered`, NOT the live input: the verdict names the query it was pronounced on.
                Reading the field would claim "no results" for the words still being typed.
                And a query BELOW THE FLOOR never reaches this branch at all — it was not asked, so
                it found nothing to report; the hook leaves the previous answer standing (the
                suggestions, on the first letter) rather than settling a dead end over it. */}
            {state === "no-results" ? (
              <div className="px-2 py-8 text-center text-xs text-muted-foreground">
                No results for “{answered}”.
              </div>
            ) : null}

            {/* THE SUGGESTIONS — the empty state, and ONLY the empty state. "No results" is a dry
                dead end on purpose: falling back to a list of unrelated entities would answer a
                question the user did not ask.
                The `length` guard is the FIRST-EVER load, the one moment there is nothing to keep:
                the panel stays bare under the field rather than drawing a section heading over an
                empty list — and never a big spinner in its middle. */}
            {state === "empty" && !emptyInstall && suggestions.length > 0 ? (
              <>
                <div className="flex items-baseline justify-between gap-2 px-2 pt-1 pb-1.5">
                  <MicroLabel>Suggestions</MicroLabel>
                  {/* HONEST COPY: this is what was last TOUCHED, and an agent touches far more
                      than the user does. It is not a history of what one opened — there is none. */}
                  <span className="text-2xs text-muted-foreground/60">recently updated</span>
                </div>
                {/* `rows` IS the suggestions in this state — walked from the same array the
                    keyboard walks, so an index can never mean two different lines. It holds no
                    action here, by construction. */}
                {rows.map((row, index) =>
                  rowFor(row, index, relativeSuggestionTime(row.hit.updated_at)),
                )}
              </>
            ) : null}

            {emptyInstall ? (
              <EmptyState className="m-1">
                Nothing to search yet — no Spec, task or idea exists in this install.
              </EmptyState>
            ) : null}

            {/* THE RESULTS — ONE list, no section headings. What the user typed is answered by
                relevance across the seven types (the aggregator's buckets, then its score), so a
                command named exactly what was typed sits above a Spec that merely mentions it.
                A fixed sequence of sections said the opposite before the query was read; the type
                stays legible on the row itself, through its glyph.
                THE ACTIONS ARE IN THIS SAME SEQUENCE, each one right under the entity it derives
                from — read the thing, then what you can do with it. */}
            {state === "results" ? rows.map((row, index) => rowFor(row, index)) : null}
          </div>

          {/* THE KEY HINTS — the keys are ICONS from the app's own library, not Unicode arrows
              typed into the string (the user's « t'es icons, c'est du ascii alors qu'on a une lib
              d'icon ? »). A raw `↑` follows neither the weight, the size nor the optical alignment
              of the glyphs the rest of the palette draws, and what it looks like depends on
              whichever font happens to have it — precisely what an icon library exists to avoid.
              They are `lucide-react`, the same source as the row glyphs and the search icon above,
              at the same `muted-foreground`. The size is the footer's OWN line-height (`3.5` =
              `0.875rem`, the `2xs` step): an icon is then never the tallest thing in the bar, so
              the bar's height is exactly what it was.
              DECORATIVE: the word beside each one carries the meaning, so a screen reader reads
              "navigate", "open", "close" and never spells an icon's name.
              ESCAPE KEEPS ITS WORD. There is no settled glyph for it — inventing one would say
              less than the four letters everyone already reads on the key itself. */}
          <div className="flex items-center gap-3 border-t border-border/60 px-3.5 py-1.5 font-mono text-2xs text-muted-foreground/70">
            <span className="flex items-center gap-1">
              <ArrowUpIcon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
              <ArrowDownIcon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
              navigate
            </span>
            <span className="flex items-center gap-1">
              <CornerDownLeftIcon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
              open
            </span>
            <span>Esc close</span>
          </div>
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

/**
 * Drive the router to a resolved {@link SearchTarget}. Split out of the component so the union is
 * narrowed in one place — TanStack types `params` and `search` per route, so the three shapes
 * cannot be passed through a single call.
 */
function navigateToTarget(navigate: ReturnType<typeof useNavigate>, target: SearchTarget) {
  switch (target.to) {
    case "/work/$specId":
      return navigate({ to: "/work/$specId", params: { specId: target.params.specId } });
    case "/knowledge":
      return navigate({ to: "/knowledge" });
    case "/system":
      // The Système scope's screen, with its OWN `?terminal=` / `?command=` — a different route
      // from `/`, so an id of one is never carried into the other.
      return navigate({ to: "/system", search: { ...target.search } });
    default:
      return navigate({ to: "/", search: { ...target.search } });
  }
}
