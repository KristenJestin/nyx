import { Fragment } from "react";

// THE FAMILY GLYPHS come from `LINE_FAMILY_ICON`, never straight from lucide: it is the one
// declaration of what a command and a terminal look like, so the same object cannot wear two signs
// depending on which screen you are on (human review 01M140VRZJG0RE5VMG91D8TH3T).
import { LINE_FAMILY_ICON } from "@/components/ui/sidebar-line";
import type { AgentActivity } from "@/components/sidebar/use-agent-sessions";

import { HomeEmpty, HomePanel } from "./home-panel";
import { HomeLineRow } from "./home-line-row";
import { HomeSessionRow, homeRowName } from "./home-session-row";
import { HomeSpecCard } from "./home-spec-card";
import { HomeStateGroup } from "./home-state-group";
import type { HomeService, HomeSession, HomeSpec, HomeTerminal } from "./types";

/**
 * The FOUR panels of the home, cabled to the read model.
 *
 * They are written once and rendered by BOTH pages: the cross-project home is the project home
 * with a wider scope and the project mark turned on, never a second implementation. That is why
 * every panel here takes a `showProject` flag and its data, and decides nothing about the scope.
 *
 * ## THE AFFORDANCE OF A CLICKABLE ROW — the same on all four families
 *
 * Every row of every panel is a control, so every row says so the same way: `cursor-pointer` under
 * the pointer, and the app's own `focus-visible:ring-2 ring-ring ring-inset` under the keyboard.
 * The four families are `<HomeSessionRow>`, `<HomeLineRow>` (services AND terminals) and
 * `<HomeSpecCard>`, and they carry it EXPLICITLY — including the Spec card, which is a `<Link>`
 * and would get the cursor from the browser's own anchor rule.
 *
 * That free anchor rule is exactly how the gap appeared (user feedback: « contrairement au recent
 * specs, j'ai pas le curseur sur agent ou sur terminals »): the Spec cards read as clickable
 * because they are anchors, the rows did not because they are `<button>`s and inherit nothing. The
 * inconsistency was never decided, it was inherited — so the affordance is now WRITTEN on all
 * four, and `home-cursor.test.tsx` sweeps the four rather than the two the user happened to name.
 *
 * The focus ring goes with the cursor, because it is the same affordance seen from the keyboard:
 * the rows were focusable already (native `<button>` / `<a>`) but their only focus signal was
 * `focus-visible:bg-muted/60`, i.e. the SAME tint as hover — indistinguishable from a passing
 * pointer. The ring is the app's existing one (`<IdeaRow>`'s), not a new treatment, and it shows
 * only under `:focus-visible`, so the elected v11 resting anatomy is untouched.
 */

/** What the two homes tell a panel about the scope it is drawn in. */
export interface HomeScopeProps {
  /** `true` on the cross-project home: hang each row's project mark on its glyph. */
  showProject: boolean;
  /** The clock the ages are read against — injected so a test is not a race. */
  now: number;
}

/**
 * AGENT SESSIONS — THE list, and its only hierarchy is the sort the backend already applied
 * (`waiting > error > ready > working > idle`, the app's own `STATE_PRIORITY`, oldest first
 * within a rank). The rows are ONE continuous flow: nothing is nested, nothing is grouped by
 * project, and a calm project never floats above a project that is waiting.
 *
 * A state's WORD is said ONCE, at the head of its run (`<HomeStateGroup>`) — the elected design's
 * legend over the flow. It is not a container and it is not repeated on the rows.
 */
export function HomeSessionsPanel({
  sessions,
  byActivity,
  onOpen,
  showProject,
  now,
}: HomeScopeProps & {
  sessions: readonly HomeSession[];
  /** The LIVE per-terminal activity map — see `<HomeSessionRow>`'s information slot. */
  byActivity: ReadonlyMap<string, AgentActivity>;
  onOpen: (session: HomeSession) => void;
}) {
  return (
    <HomePanel title="Agent sessions">
      {sessions.length === 0 ? (
        <HomeEmpty
          title="No agent session"
          hint={
            showProject
              ? "No agent is running, on any project."
              : "Start an agent from a terminal of this project."
          }
        />
      ) : (
        sessions.map((session, index) => {
          // The state word opens a RUN of rows: emitted on the row where the state changes, which
          // is exactly where the sorted flow moves from one rank to the next. The count is the
          // length of THAT RUN, walked forward from here — not the state's total over the whole
          // list. The two agree while the sort keeps a state contiguous, and only the run length
          // stays true if it ever does not; counting by run also drops the per-row full scan.
          const previous = index > 0 ? sessions[index - 1]!.state : null;
          let head: number | null = null;
          if (session.state !== previous) {
            let run = 1;
            while (sessions[index + run]?.state === session.state) run += 1;
            head = run;
          }
          return (
            <Fragment key={session.session_id}>
              {head !== null ? <HomeStateGroup state={session.state} count={head} /> : null}
              <HomeSessionRow
                session={session}
                activity={byActivity.get(session.terminal_id) ?? null}
                index={index}
                showProject={showProject}
                onOpen={onOpen}
                now={now}
              />
            </Fragment>
          );
        })
      )}
    </HomePanel>
  );
}

/** RECENT SPECS — what moved lately, newest first. Cards carry no project mark (the key names it). */
export function HomeSpecsPanel({
  specs,
  showProject,
  now,
}: HomeScopeProps & { specs: readonly HomeSpec[] }) {
  return (
    <HomePanel title="Recent specs" note="by last activity">
      {specs.length === 0 ? (
        <HomeEmpty
          title={showProject ? "No active spec" : "No spec yet"}
          hint={
            showProject
              ? "Nothing has moved on any project."
              : "The specs of this project show up here as soon as they move."
          }
        />
      ) : (
        specs.map((spec) => <HomeSpecCard key={spec.spec_id} spec={spec} now={now} />)
      )}
    </HomePanel>
  );
}

/**
 * SERVICES — the managed commands that are UP, plus the ones that failed and have not been
 * acknowledged. A stopped service is not news and the backend already dropped it.
 *
 * The trailing token carries what the dot cannot: the exit CODE of a failure. It is a fact, not a
 * state word — the dot says the state, alone.
 */
export function HomeServicesPanel({
  services,
  onOpen,
  showProject,
}: Omit<HomeScopeProps, "now"> & {
  services: readonly HomeService[];
  onOpen: (service: HomeService) => void;
}) {
  return (
    <HomePanel title="Services" note="live runtime">
      {services.length === 0 ? (
        <HomeEmpty
          title="No service running"
          hint={showProject ? "Nothing is up, on any project." : "Nothing is up in this project."}
        />
      ) : (
        services.map((service) => (
          <HomeLineRow
            key={service.instance_id}
            testId={`home-service-${service.instance_id}`}
            icon={LINE_FAMILY_ICON.command}
            // `run_state` is the FACTUAL state the backend resolved (`running` | `error`), in the
            // same vocabulary the dot paints — read 1:1, exactly as `serviceRunState` does.
            state={service.run_state}
            project={showProject ? service.project : null}
            name={service.name}
            detail={service.workspace.name}
            trailing={service.exit_code != null ? `exit ${service.exit_code}` : undefined}
            onOpen={() => onOpen(service)}
          />
        ))
      )}
    </HomePanel>
  );
}

/**
 * TERMINALS — SHELLS ONLY. A terminal hosting an agent is already one row up in the sessions
 * panel; the backend excludes it here so the page never counts the same terminal twice under a
 * name that says nothing about the agent on it.
 */
export function HomeTerminalsPanel({
  terminals,
  onOpen,
  showProject,
}: Omit<HomeScopeProps, "now"> & {
  terminals: readonly HomeTerminal[];
  onOpen: (terminal: HomeTerminal) => void;
}) {
  return (
    <HomePanel title="Terminals" note="shells only">
      {terminals.length === 0 ? (
        <HomeEmpty
          title="No terminal open"
          hint={showProject ? "Enter a project in the rail to open one." : "Ctrl+T opens one here."}
        />
      ) : (
        terminals.map((terminal, index) => (
          <HomeLineRow
            key={terminal.terminal_id}
            testId={`home-terminal-${terminal.terminal_id}`}
            icon={LINE_FAMILY_ICON.terminal}
            // A shell's exec state, in the dot's own vocabulary — and shown only while UNREAD, the
            // notification semantics every other row in the app already follows. A read (or idle)
            // shell is quiet and `<StatusDot>` draws nothing.
            state={terminal.exec_state_unread ? terminal.exec_state : "idle"}
            project={showProject ? terminal.project : null}
            name={homeRowName(terminal, index)}
            detail={terminal.cwd}
            onOpen={() => onOpen(terminal)}
          />
        ))
      )}
    </HomePanel>
  );
}
