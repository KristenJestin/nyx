import { GitBranchIcon } from "lucide-react";

import { LINE_FAMILY_ICON } from "@/components/ui/sidebar-line";
import { agentProviderFor } from "@/components/sidebar/agent-providers";
import { resolveDisplayName } from "@/components/sidebar/auto-label";
import { secondaryInfo, type AgentActivity } from "@/components/sidebar/use-agent-sessions";

import { HomeGlyph } from "./home-glyph";
import { formatAge } from "./home-format";
import { HOME_STATE_RUN_STATE } from "./home-state";
import type { HomeSession, HomeWorkspace } from "./types";

/**
 * The row's NAME — resolved by the SIDEBAR's own authority so the same session cannot be called
 * two things on two screens.
 *
 * `resolveDisplayName` is the single policy: a manual label wins, then the live auto label, then
 * the cwd basename, then `Terminal <n>`. The home has no live auto label to pass (that one is a
 * per-terminal `terminal_info` poll a row owns, and the home would need one per row), so it passes
 * `null` — the SAME static form the MOUNTED sidebar tree passes (`workspace-runtime-tree.tsx`'s
 * `TerminalRow`, which runs no poll either). That is where the parity actually holds, and where
 * `home-page.test.tsx` asserts it (review 01M15AWP3W1Y2PD3YHB406ERYT): the two surfaces resolve
 * the same string from the same two fields. The sidebar ITEM that does poll
 * (`sidebar/terminal-item.tsx`) only ever refines the name upwards, through this same function.
 *
 * The `index` only feeds the `Terminal <n>` last resort, so it is the row's position in its list.
 */
export function homeRowName(row: { label: string | null; cwd: string }, index: number): string {
  return resolveDisplayName(row, index, null);
}

/**
 * The checkout pill's value IN FULL — exactly what the pill reads when nothing is cut.
 *
 * The pill is capped, so on a real branch name (`codex/review-downloads-dev-2026-06-12`) what is
 * on screen is an ellipsis. A cut value with no way back to the whole is a value lost, so this is
 * what the pill hands to its `title`. The elected round never drew a branch longer than
 * `feat/home-recaps` and prescribes no treatment for one, so the treatment is the plain one: cut
 * cleanly, keep the whole of it one hover away.
 */
function checkout(workspace: HomeWorkspace): string {
  return workspace.branch ? `${workspace.name} ${workspace.branch}` : workspace.name;
}

/**
 * `<HomeSessionRow>` — ONE agent session, and THE row anatomy of the home.
 *
 * ## One shape for every state
 *
 * A `waiting` row, an `error` row, a `ready` row, a `working` row and an `idle` row are the SAME
 * element: same glyph, same name, same checkout pill, same info slot, same age. Only two things
 * change with the state, and both are the app's own:
 *
 *   * THE DOT on the glyph — and a QUIET state renders none at all (`isQuietState`), so an idle
 *     row carries no pastille;
 *   * THE RANK — where the row falls in the one sorted list, which the backend already applied.
 *
 * Nothing grows, tints, borders or gains a call-to-action because a session is blocked. A question
 * is legible because it is sorted to the TOP, not because it is drawn louder. There is no state
 * badge (it would double the dot) and no answer option (nyx does not hold them).
 *
 * ## …and no verb, here or on any other family
 *
 * The row used to reveal an action word on hover (`open ›`, `answer ›` on a blocked row). It is
 * gone (user feedback, review `01M1C8AHV63Y0D54BJCSN46737`): the WHOLE row is the control and the
 * hover already surfaces it (`hover:bg-muted/60`), so the word repeated what the surface said —
 * and it repeated it on ONE family out of three, since `<HomeLineRow>` and `<HomeSpecCard>` never
 * had one. The anatomy is the same on all three again, and it ends on the age.
 *
 * Its SLOT went with it rather than being hidden: it sat AFTER the age in the same group, and a
 * group that ends on the age is the one arrangement that puts every age of a panel on the same
 * right edge whatever it reads (`7d`, `32m`). The trailing slots that carry FACTS (`:5173`,
 * `exit 2`, `last used` on `<HomeLineRow>`) are untouched — those are not verbs.
 *
 * ## The checkout pill, and the length it is allowed to take
 *
 * Workspace then branch, in that order, in the one pill — the slot the elected round drew, with
 * nothing added to it and nothing taken away. What it lacked was a ceiling: a real branch name
 * (`codex/review-downloads-dev-2026-06-12`) let the pill grow to whatever it held, and the pill
 * ate the line it was meant to annotate (user feedback, review `01M1ENS135VBDJNDJ6R2S87ZEM`). It
 * is now capped, truncated as ONE run of text so only the branch can be cut, and its whole value
 * stays one hover away in `title` (see {@link checkout}). The branch carries the weight the
 * elected round gives it, which is what keeps the two values from reading as one string.
 *
 * ## The information slot, and its right to be empty
 *
 * Line 2 holds ONE text, resolved in ONE order: the question or permission when the row is
 * blocked, otherwise the tool in flight, otherwise NOTHING — the element is not emitted at all. A
 * row with nothing to say collapses to one line and stays level with its glyph; no dash, no
 * placeholder, no filler.
 *
 * That order is applied to the LIVE activity map when the row has an entry in it (`secondaryInfo`,
 * the renderer twin of `nyx_core::home::secondary_info`, moored to it by
 * `contract/home-secondary-info.json`), and falls back to the text the snapshot already resolved.
 * The live map is the fresher of the two authorities and it is the SAME rule on both sides, so
 * reading it can only make the line more current — never say something else.
 *
 * The row's STATE is NOT overlaid the same way, deliberately: projecting an `AgentActivity` onto
 * the five-state vocabulary is a state machine, and the PRD allows exactly one, in the backend.
 */
export function HomeSessionRow({
  session,
  activity,
  index,
  showProject,
  onOpen,
  now,
}: {
  session: HomeSession;
  /** The row's LIVE activity entry (`useAgentSessions().byActivity`), or `null` when it has none. */
  activity: AgentActivity | null;
  /** Position in the list — the `Terminal <n>` last resort of the name policy. */
  index: number;
  /** Hang the project mark on the glyph (the cross-project home). */
  showProject: boolean;
  /** Focus this session — select its terminal, entering its project first when needed. */
  onOpen: (session: HomeSession) => void;
  now: number;
}) {
  const name = homeRowName(session, index);
  const secondary = activity ? secondaryInfo(activity) : session.secondary;
  // The row TYPE glyph, resolved exactly as the sidebar resolves it — the SAME expression as
  // `workspace-runtime-tree.tsx`'s terminal row: the provider brand logo when the kind has one,
  // else the TERMINAL family's glyph read from `LINE_FAMILY_ICON`, the one declaration of what
  // each family looks like (human review 01M140VRZJG0RE5VMG91D8TH3T). Never the state.
  const Icon = agentProviderFor(session.agent_kind)?.icon ?? LINE_FAMILY_ICON.terminal;
  return (
    <button
      type="button"
      onClick={() => onOpen(session)}
      data-testid={`home-session-${session.session_id}`}
      data-state={session.state}
      className="flex w-full cursor-pointer items-start gap-3 rounded-lg px-2.5 py-2 text-left outline-none transition-colors hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
    >
      <HomeGlyph
        icon={Icon}
        state={HOME_STATE_RUN_STATE[session.state]}
        project={showProject ? session.project : null}
        variant="tile"
      />
      {/* The body is locked to the glyph's height and centred, so a row with no second line reads
          as CALM rather than as broken. */}
      <span className="flex min-h-8 min-w-0 flex-1 flex-col justify-center gap-0.5">
        <span className="flex min-w-0 items-center gap-2">
          <span
            data-testid="home-row-name"
            className="shrink-0 truncate font-mono text-xs text-foreground"
          >
            {name}
          </span>
          {session.workspace ? (
            <span
              title={checkout(session.workspace)}
              data-testid="home-row-checkout"
              className="inline-flex max-w-[12rem] min-w-0 items-center gap-1 rounded-full bg-muted/60 py-px pr-1.5 pl-1 font-mono text-2xs text-muted-foreground"
            >
              <GitBranchIcon className="size-2.5 shrink-0" />
              {/* ONE run of text, truncated ONCE. Left as two SIBLING boxes under the cap, both
                  would ellipse on their own and a flex line shrinks both — so `main` would lose
                  characters while the branch it stands next to still ran on. Nested, the branch —
                  the long, variable half — is the only part the ellipsis can reach. */}
              <span className="truncate">
                {session.workspace.name}
                {session.workspace.branch ? (
                  // The weight the elected round gives the branch (`.ws b`, 500): without it the
                  // two values carry the same stroke and read as one string.
                  <span className="ml-1 font-medium text-foreground/80">
                    {session.workspace.branch}
                  </span>
                ) : null}
              </span>
            </span>
          ) : null}
        </span>
        {/* THE INFO SLOT — emitted only when there is something to say. */}
        {secondary ? (
          <span
            title={secondary}
            data-testid="home-row-secondary"
            className="truncate font-mono text-2xs text-muted-foreground"
          >
            {secondary}
          </span>
        ) : null}
      </span>
      {/* The age, and NOTHING after it — the row ENDS on it, which is what puts every age of a
          panel on the same right edge. */}
      <span
        data-testid="home-row-age"
        className="flex min-h-8 shrink-0 items-center font-mono text-2xs text-muted-foreground"
      >
        {formatAge(session.started_at, now)}
      </span>
    </button>
  );
}
