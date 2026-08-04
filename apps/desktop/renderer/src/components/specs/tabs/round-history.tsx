import { ChevronRightIcon, HistoryIcon, LockIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Menu, MenuSeparator } from "@/components/ui/menu";
import { cn } from "@/lib/utils";

import type { ProtoTree, PrototypeRound } from "../proto-types";

/**
 * `created_at` → a short `dd/mm/yy` day. The proto timestamps cross the wire as epoch SECONDS in
 * some rows and MILLISECONDS in others (the same normalisation the comment thread applies), so the
 * unit is DETECTED rather than assumed — a round dated 1970 would be a lie about the history.
 */
function formatDay(ts: number): string {
  const d = new Date(ts < 1e12 ? ts * 1000 : ts);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getDate())}/${pad(d.getMonth() + 1)}/${pad(d.getFullYear() % 100)}`;
}

/** The round's summary, or `null` when it carries none (a blank string is no summary). */
function summaryOf(round: PrototypeRound): string | null {
  const summary = round.summary?.trim() ?? "";
  return summary.length > 0 ? summary : null;
}

/**
 * `<RoundHistory>` — the prototype's rounds, newest-first: the CURRENT (mutable) round on top, the
 * earlier ones below, a presented round marked frozen. Each one OPENS FOR CONSULTATION.
 *
 * # It used to offer "go back to this round", and that was a gesture the UI must not carry
 *
 * Clicking an earlier round ran `proto_round_rebase` — a ROLLBACK, which OPENS A NEW ROUND cloning
 * that round's manifest. That put the creation of a round in the user's hands, and a round is WORK:
 * it exists so the agent can apply a correction to what came before. The doctrine is settled — the
 * UI does not drive the work, it SHOWS and it DECIDES. Electing a variant is a decision and stays
 * on the workbench; opening a round is work and belongs to the agent, through its own `proto_*`
 * authority (PDR-0005).
 *
 * # …and then it offered NOTHING, which was the opposite mistake (finding `01KZ3AP34K1PKS6W0E75RP7DF6`)
 *
 * Withdrawing the rollback left a list of labels with a padlock and no detail: "je vois une liste
 * d'un truc dont je ne peux pas voir le détail". READ-ONLY WAS READ AS UNREADABLE. The two are not
 * the same thing: no MUTATION is the rule, non-navigable was an accident of it. So a row is a
 * button again — and what it does is the exact opposite of what it used to do: it CONSULTS.
 * `onConsult` re-sources the workbench from that round's manifest; the CURRENT row is the way back.
 *
 * This component still performs no write: it holds no actions object, calls no command, and the
 * only thing it hands its host is an id to look at.
 *
 * # …and a row said NOTHING about its round (finding `01KZ3JXCNK902DQXBD6QSYEE78`)
 *
 * Seven rows reading `v7 … v1`, a padlock on three of them with no explanation, no date, no
 * summary: nothing said which round was which, nor why one was opened. Yet `prototype_rounds.summary`
 * is FILLED — the agent writes it when it opens a round, and six of the seven rounds of the user's
 * own prototype carry one, sometimes a detailed one — and it was displayed NOWHERE in the app.
 *
 * So a row carries what the model already holds: the round's DATE and its SUMMARY (full text as the
 * row's `title`, since a summary is a sentence and a menu is not a document). It is the AGENT's
 * prose, in whatever language the user works in — it is CONTENT, and it is shown verbatim, never
 * translated nor paraphrased by this surface.
 *
 * # …and then it EXPLAINED ITSELF, which is the same mistake in prose (finding `01KZ46P0FTMJXRTT32H6G5TEEN`)
 *
 * A paragraph under the list read "Read-only: opening a round shows it, changes nothing. The agent
 * opens the next one to apply a correction. A padlock marks a round already presented, and frozen
 * with it." NOT ONE MUTATING CONTROL EXISTS IN THIS PANEL — there is no rollback, no "open a round",
 * nothing to write with — so read-only is what the panel already IS, and saying it is a caption on an
 * absence. The one thing that was not self-evident is the padlock, and a padlock is explained WHERE
 * IT IS: its own tooltip, on the icon, for the rows that carry one.
 */
export function RoundHistory({
  tree,
  consultedRoundId = null,
  onConsult,
}: {
  tree: ProtoTree;
  /** The round being CONSULTED right now — `null` = the current round (the live workbench). */
  consultedRoundId?: string | null;
  /**
   * CONSULT a round: `null` returns to the current one. Absent → the rows stay inert (a host with
   * nothing to re-source, e.g. a surface that does not render the workbench).
   */
  onConsult?: (roundId: string | null) => void;
}) {
  const rounds = tree.rounds;
  const currentId = tree.current_round?.id ?? null;

  // No history yet (0 or only the current round) → nothing to consult, and no control.
  if (rounds.length < 2) return null;

  // Newest-first (the current/most-recent round on top, older below).
  const ordered = [...rounds].reverse();

  return (
    <Menu
      side="bottom"
      align="end"
      tooltip="Rounds history"
      trigger={
        <Button
          size="icon-xs"
          variant="ghost"
          aria-label="Rounds history"
          data-testid="proto-rounds-trigger"
        >
          <HistoryIcon />
        </Button>
      }
    >
      <div className="px-2 py-1.5">
        <span className="font-medium text-2xs uppercase tracking-wide text-muted-foreground">
          Rounds history
        </span>
      </div>
      <MenuSeparator />

      {ordered.map((r) => {
        const isCurrent = r.id === currentId;
        const isPresented = r.presented_at != null;
        // The row being LOOKED AT — the current row is "consulted" when nothing else is.
        const isConsulted = isCurrent ? consultedRoundId === null : consultedRoundId === r.id;
        const summary = summaryOf(r);
        const inner = (
          <>
            <span className="flex w-full items-center gap-2">
              <span className="shrink-0 font-medium text-foreground">{r.label}</span>
              <span className="shrink-0 font-mono text-2xs text-muted-foreground/70">
                {formatDay(r.created_at)}
              </span>
              <span className="flex-1" />
              {isPresented ? (
                // THE PADLOCK EXPLAINS ITSELF, ON ITSELF. A native `title` on the WRAPPER, not on
                // the icon: an SVG ignores the `title` ATTRIBUTE, and this span also overrides the
                // row's own `title` (the summary) for exactly the padlock's few pixels.
                <span
                  data-testid={`proto-round-lock-${r.id}`}
                  title="Presented round · frozen with its presentation"
                  className="flex shrink-0 items-center"
                >
                  <LockIcon aria-label="Presented round (frozen)" className="size-3 opacity-70" />
                </span>
              ) : null}
              {isCurrent ? (
                <Badge variant="info" className="shrink-0">
                  Current
                </Badge>
              ) : null}
              {onConsult !== undefined && !isConsulted ? (
                <ChevronRightIcon aria-hidden className="size-3 shrink-0 text-muted-foreground" />
              ) : null}
            </span>
            {/* WHAT THE ROUND WAS FOR — the agent's own summary, verbatim (it is the user's content,
                in the user's language). Clamped to two lines here; the row's `title` carries it
                whole, because a menu is not the place to unfold a paragraph. */}
            {summary !== null ? (
              <span
                data-testid={`proto-round-summary-${r.id}`}
                className="line-clamp-2 w-full text-2xs leading-snug text-muted-foreground"
              >
                {summary}
              </span>
            ) : null}
          </>
        );

        // A FIXED row width, because the content is now a SENTENCE: left to size itself, the popup
        // would be as wide as the longest summary the agent ever wrote.
        const rowClass =
          "flex w-72 max-w-full flex-col items-start gap-0.5 rounded-md px-2 py-1.5 text-sm";

        if (onConsult === undefined) {
          return (
            <div
              key={r.id}
              data-testid={`proto-round-${r.id}`}
              title={summary ?? undefined}
              className={rowClass}
            >
              {inner}
            </div>
          );
        }

        return (
          <button
            key={r.id}
            type="button"
            data-testid={`proto-round-${r.id}`}
            aria-pressed={isConsulted}
            title={summary ?? undefined}
            // The CURRENT row is the way back, so it maps to `null` — the one state the workbench
            // is live in. Every other row is consulted by its own id.
            onClick={() => onConsult(isCurrent ? null : r.id)}
            className={cn(
              rowClass,
              "text-left transition-colors",
              isConsulted ? "bg-muted" : "hover:bg-accent/50",
            )}
          >
            {inner}
          </button>
        );
      })}
    </Menu>
  );
}
