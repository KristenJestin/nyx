import type { ExecState } from "@/components/sidebar/use-terminals";

import type { HomeSessionState } from "./home-state";

/**
 * The WIRE shape of `home_snapshot` — the renderer half of `nyx_core::home::HomeSnapshot`.
 *
 * Field names are the Rust struct's, verbatim (serde emits the declared snake_case), so this file
 * reads as a transcription and a drift shows up as a missing property rather than as a silent
 * `undefined`. Nothing is computed here: the backend already resolved each row's state, its
 * secondary text and the sort — see `crates/nyx-core/src/home.rs`.
 */

/** A project as a row wears it — the identity `<ProjectMark>` paints. */
export interface HomeProject {
  readonly id: string;
  readonly name: string;
  readonly accent: string | null;
  readonly icon: string | null;
  readonly image_path: string | null;
}

/** A workspace as a row wears it. `branch` is the workspace record's value (no Git call). */
export interface HomeWorkspace {
  readonly id: string;
  readonly name: string;
  readonly branch: string | null;
}

/** ONE agent session in the flow — a line of the home. */
export interface HomeSession {
  readonly session_id: string;
  /** The FOCUS target: selecting this terminal is what lands on the session. */
  readonly terminal_id: string;
  readonly agent_kind: string;
  /** The row's state — the shared vocabulary of {@link HomeSessionState}. */
  readonly state: HomeSessionState;
  /** What the agent asks / is doing, or `null` — and `null` renders NO text (no filler). */
  readonly secondary: string | null;
  readonly label: string | null;
  readonly cwd: string;
  readonly started_at: number;
  /** `null` on a session hosted by a LOOSE terminal — the row stays in the flow regardless. */
  readonly workspace: HomeWorkspace | null;
  readonly project: HomeProject | null;
}

/** ONE service in the flow — running, or failed and unacknowledged. */
export interface HomeService {
  /** `command_instances.id` — the `?command=<id>` deep-link target. */
  readonly instance_id: string;
  readonly name: string;
  /**
   * `running` | `error` — the FACTUAL state (live runner, else the persisted outcome), in the
   * app's own {@link ExecState} vocabulary rather than a bare `string`. `ExecState` is a strict
   * subset of the dot's `RunState`, so the row hands it to `<StatusDot>` with NO cast — the same
   * way `serviceRunState` does for the sidebar's services.
   */
  readonly run_state: ExecState;
  readonly exit_code: number | null;
  readonly unread: boolean;
  readonly workspace: HomeWorkspace;
  readonly project: HomeProject;
}

/** Where a Spec's work sits in its phase plan. `null` on a Spec with no plan. */
export interface HomePhase {
  readonly number: number;
  readonly total: number;
  readonly name: string;
}

/** ONE recently-touched Spec. */
export interface HomeSpec {
  readonly spec_id: string;
  /** The human key, `PROJECTKEY-number`. */
  readonly key: string;
  readonly title: string;
  readonly status: string;
  readonly updated_at: number;
  readonly tasks_done: number;
  readonly tasks_total: number;
  readonly phase: HomePhase | null;
  readonly project: HomeProject;
}

/** ONE bare terminal — a shell with no agent on it. */
export interface HomeTerminal {
  readonly terminal_id: string;
  readonly label: string | null;
  readonly cwd: string;
  /** The shell's settled exec state — the same {@link ExecState} the sidebar's terminals carry. */
  readonly exec_state: ExecState;
  readonly exec_state_unread: boolean;
  readonly workspace: HomeWorkspace | null;
  readonly project: HomeProject | null;
}

/** The four families, for one scope, from ONE call. */
export interface HomeSnapshot {
  readonly sessions: readonly HomeSession[];
  readonly services: readonly HomeService[];
  readonly specs: readonly HomeSpec[];
  readonly terminals: readonly HomeTerminal[];
}

/** The empty snapshot — what a page renders before its first read resolves. */
export const EMPTY_HOME_SNAPSHOT: HomeSnapshot = {
  sessions: [],
  services: [],
  specs: [],
  terminals: [],
};
