import type { GlobalSearchHit } from "@/bridge/contract";
import { projectInitial } from "@/components/terminals/project-initial";

/**
 * `<SearchContextBlock>` — WHERE A LINE COMES FROM, drawn at the right edge of every line of the
 * palette, entity and action alike.
 *
 * ## Why it is a component and not a fragment of `<SearchRow>`
 *
 * It used to live inside the hit row, which was true as long as a hit row was the only species of
 * line. It is not: an ACTION line derives from a hit and must say the same "where" its entity says
 * (the user's « on a pas le projet, le workspace »). Two copies of this markup would be two truths
 * about what a context is — the same reason `sidebar/agent-providers` is one registry read from two
 * places. So the block moved here and both rows render it from the SAME hit.
 *
 * ## What it says, and why both halves are needed
 *
 * The search spans every project at once, so two Specs called "Auth" from two projects are
 * indistinguishable without the PROJECT — and activating one of them MOVES the entered project
 * durably. But an entity lives in a project AND, for three of the seven kinds (workspaces, command
 * instances, agent sessions), in a WORKSPACE: two instances of the same `build` command in two
 * workspaces of one project are the same row twice without it.
 *
 * Both names come from the HIT. They used to be rebuilt from the rail's project tree, which knows
 * only what the rail loaded and knows nothing of workspaces; joining them at the source is also what
 * makes them searchable, since the backend cannot match a string it never reads.
 *
 * ## THE ORDER: DETERMINANT, THEN PROJECT — the project CLOSES the line
 *
 * It reads `audio player · [M] Music Manager`, not `[M] Music Manager / audio player`. The user's
 * own framing: « projet tout à droite, ensuite le déterminant genre le workspace, ou l'id de spec
 * etc ? histoire qu'il ait tous la même gueule ». With the project FIRST, its name started at the
 * same x but ENDED wherever the workspace behind it began — so nothing lined up when the eye ran
 * down the right edge, and the project appeared to float from line to line depending on whether a
 * workspace followed it. Placed LAST it always occupies the same end-of-line position: the column
 * is stable, and the part that varies — the determinant — varies to its left, where it disturbs
 * nobody.
 *
 * The cost is accepted knowingly: the block no longer reads from the general to the particular.
 * This is a SCANNING choice — running down a stable column beats reading a sentence.
 *
 * THE DETERMINANT is whatever narrows the entity WITHIN its project: the workspace for a workspace,
 * a command instance or a session; the human KEY (`NYX-42`) for a Spec; nothing at all for the
 * kinds that have neither. The one separator is `·` in every case, so a line with a workspace and a
 * line with a key have the same shape.
 *
 * The LETTER TILE stays welded to the project's name, immediately in front of it — it is the
 * project's marker, not the block's, so it travels with what it marks.
 *
 * ## The type, and why each half sets its own
 *
 * The pair used to be `2xs`, MONO and flat `muted-foreground` — a size and a face that say
 * "technical identifier, skip me", so it was verifiable when looked FOR and invisible when the list
 * was merely scanned (the user's « c'est compliqué de voir le projet / le workspace sur lequel on
 * bosse »). It is now `xs` in the UI face, with the PROJECT at near-foreground and `medium` and the
 * WORKSPACE at full `muted-foreground` — three steps below the `sm` full-contrast title, which
 * stays the dominant element of the line. Only the human KEY keeps the mono face: it is an
 * identifier, mono is what says so everywhere else in the product, and leaving it there is what
 * keeps it from competing with the names beside it. Every value is a design-system token, so both
 * themes resolve it themselves and no literal colour ever enters this file.
 */
export function SearchContextBlock({
  hit,
  testId,
  tileTestId,
}: {
  hit: GlobalSearchHit;
  /**
   * The block's own test id. It is a PROP because the block is drawn twice for one hit — once on
   * the entity, once on each action hanging off it — and two nodes may not answer to one id.
   */
  testId: string;
  /** Same reason, for the project's letter tile. */
  tileTestId: string;
}) {
  // `project_name` is joined at the source and is always there; the id is kept as a last-resort
  // fallback so a line can never render a blank context. `workspace_name` is present exactly on the
  // kinds that live in a workspace, and absent — not blank, absent — on the others.
  const projectLabel = hit.project_name.length > 0 ? hit.project_name : hit.project_id;
  const workspaceLabel =
    hit.workspace_name && hit.workspace_name.length > 0 ? hit.workspace_name : null;
  // The HUMAN KEY (`NYX-42`), on the kinds that have one — a Spec today. It rides its own field
  // rather than `detail` because `detail` is searchable backend-side: it is DRAWN, never matched.
  // It is a determinant like the workspace is one — what departs two homonymous Specs of one
  // project — so it sits in the same slot, left of the project, and never in front of the title:
  // a key is variable-width (`DEPOT-6` against `MUSICMAN-1`), so there it started every title at a
  // different abscissa and the eye zigzagged down the one column that carries the answer.
  const key = hit.human_key && hit.human_key.length > 0 ? hit.human_key : null;
  return (
    // Separate spans rather than one string, so a long project name cannot truncate the determinant
    // out of the line: the determinant is precisely what departs two otherwise identical lines.
    // The block itself carries NO type face and NO colour: each half sets its own, so the two NAMES
    // can be read at scanning strength while the KEY stays the quiet mono identifier it is
    // elsewhere in the product.
    <span data-testid={testId} className="flex shrink-0 items-baseline gap-1 text-xs">
      {key ? (
        <>
          {/* The key keeps `2xs` + MONO + `muted`: it is the technical identity of the entity, and
              raising it with the names would give the block two competing voices. */}
          <span className="shrink-0 font-mono text-2xs text-muted-foreground">{key}</span>
          <ContextSeparator />
        </>
      ) : null}
      {workspaceLabel ? (
        <>
          {/* THE WORKSPACE — full `muted-foreground`, no added weight, so the pair reads in an
              order instead of as two equal halves. */}
          <span className="max-w-[9rem] truncate text-muted-foreground">{workspaceLabel}</span>
          <ContextSeparator />
        </>
      ) : null}
      {/* THE PROJECT'S LETTER TILE — the very object the vertical rail draws at the far left
          (`terminals/base-layout.tsx`), at a line's scale: same rounded square, same `muted`
          surface, same centred uppercase initial, so it reads as the same thing seen smaller.
          It replaced a UNIFORM magenta dot, which was the wrong marker: identical on every line, it
          said "this is a project" and never told `depot` from `Music Manager` — and telling them
          apart at a glance is the whole of what was asked.
          The letter comes from `terminals/project-initial`, the leaf module the rail reads too,
          rather than being recomputed here: two derivations of one project's initial would be two
          truths about it.
          DECORATIVE (`aria-hidden`): the project's name follows it in words on this very line, so
          for assistive tech the tile would only be the same name read a second time.
          FIXME: replace with the project's real IMAGE icon, being developed in another PRD. This
          letter tile stands in until that icon exists — exactly as it already does in the rail,
          which is why it is the right placeholder where the uniform dot was not. */}
      <span
        aria-hidden
        data-testid={tileTestId}
        className="flex size-4.5 shrink-0 items-center justify-center self-center rounded bg-muted font-medium text-2xs text-foreground"
      >
        {projectInitial(projectLabel)}
      </span>
      {/* THE PROJECT CLOSES THE LINE — near-`foreground` and `medium`, the strongest thing on the
          line after the title itself, and now always at the same end-of-line position. The width
          is `9rem` (the same rem step `knowledge/idea-row` uses) so the type size buys legibility
          instead of spending it on truncation. */}
      <span className="max-w-[9rem] truncate font-medium text-foreground/85">{projectLabel}</span>
    </span>
  );
}

/**
 * The ONE separator of the block. A single glyph for every determinant — the key and the workspace
 * alike — is what makes two lines carrying different determinants read as the same shape; the `/`
 * the pair used to carry said "a path", which a key is not.
 *
 * It keeps the `/60` the pair separator had rather than the `/40` the key's dot had: now that the
 * determinant LEADS the block, the separator is what says "two different things", and at `/40` a
 * `·` that small measured as good as absent in a real capture.
 */
function ContextSeparator() {
  return (
    <span aria-hidden className="shrink-0 text-muted-foreground/60">
      ·
    </span>
  );
}
