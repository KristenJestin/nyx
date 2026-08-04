import { Link } from "@tanstack/react-router";
import { LayoutTemplateIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";

import { useDesign } from "../../design/use-design";
import { panelPagesOf } from "../proto-page-model";
import type { SpecDetail } from "../types";
import { useProtoActions } from "../use-proto-actions";
import { useProtoTree } from "../use-proto-tree";
import { NodeComments } from "./node-comments";
import { PagesSheet } from "./pages-panel";
import { RoundHistory } from "./round-history";

/**
 * `<PrototypeTab>` — the Spec's Prototype tab, which is an ENTRY POINT and no longer the workbench
 * (finding `01KYVMSMMBVSZNBBSBE18PRZAK`, the elected v8 `full-page` design).
 *
 * # What it stopped being
 *
 * It used to be the whole prototype surface: the pages panel, the stage, the variant strip, the
 * holes, the node params, the comments and the elected composition, stacked into a tab of a Spec
 * detail that already spends its width on a rail. Measured in the shipped app, that gave the page
 * under review an iframe of **486 × 489** — a vignette. A prototype stands for a REAL WEB PAGE; a
 * tab is not the size of one, and every card stacked under the surface took more of the little
 * height it had. The workbench moved to its own route, `/work/$specId/prototype`
 * ({@link PrototypePage}), exactly as the code review already had.
 *
 * # What it is
 *
 * The SAME chassis as the Review tab's entry card, because it is the same gesture:
 *
 *  - the ENTRY CARD — what there is to look at (pages, arbitrations still owed) and the button that
 *    opens the page. The open COMMENTS are not in it: the thread below counts itself, and one screen
 *    states one fact once (finding `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`);
 *  - the CONTACT SHEET — a live miniature per page, each one a link INTO that page. The tab says
 *    WHAT exists; it never pretends to be a stage;
 *  - the pre-dev COMMENT thread, read-only here: anchoring a remark needs the composition on
 *    screen, and the composition is on the page.
 *
 * The rounds history stays in the header (a kebab, as before) — it is a property of the prototype,
 * not of the stage. It is READ-ONLY: only the agent opens a round.
 */
export function PrototypeTab({ detail }: { detail: SpecDetail }) {
  const specId = detail.spec.id;
  /** The SPEC's project — the one owning the catalogue + tokens the miniatures compile against. */
  const projectId = detail.spec.project_id;
  const { tree, loading, error } = useProtoTree(specId);
  const { design } = useDesign(projectId);
  const themeTokensJson = design?.theme_tokens_json ?? null;
  // The comment writes the thread below needs. The election + pin writes live on the page, with what
  // they act on; the rounds are read-only here and everywhere (finding `01KYW32N1BVDVTGS0WPMG2PJW2`).
  const actions = useProtoActions();

  if (loading) {
    return (
      <div className="flex justify-center py-16 text-muted-foreground">
        <Spinner />
      </div>
    );
  }

  // No prototype (or a read error) → the clean empty cue. `proto_tree` returns null for a Spec with
  // no prototype; an error degrades to the same placeholder rather than a broken card.
  if (error || tree === null || tree.pages.length === 0) {
    return (
      <div className="mx-auto flex max-w-xl flex-col items-center gap-3 py-16 text-center">
        <span className="grid size-12 place-items-center rounded-full bg-muted text-muted-foreground">
          <LayoutTemplateIcon className="size-5" />
        </span>
        <h2 className="font-heading text-base font-semibold text-foreground">Prototype</h2>
        <p className="text-sm text-muted-foreground">
          {error
            ? "The prototype could not be loaded."
            : "This Spec has no prototype yet. The agent proposes a prototype as an annexe of the Spec; once it does, its pages preview here."}
        </p>
      </div>
    );
  }

  const roundLabel = tree.current_round?.label ?? null;
  const roundId = tree.current_round?.id ?? null;
  const currentLocked =
    tree.design_lock != null && roundId != null && tree.design_lock.locked_round_id === roundId;

  const pages = panelPagesOf(tree);
  const toElect = pages.filter((p) => !p.retained).length;
  // `prototypeId` is what lets each miniature resolve its own page links (finding
  // `01KZ3JN69E32FYBF01NBE4FWJD`) — without it the render answers `unknown_page` for every one of
  // them and stamps the missing marker into markup the contact sheet then draws.
  const thumbnailCtx = { projectId, themeTokensJson, prototypeId: tree.prototype.id };

  /**
   * WHAT THERE IS TO LOOK AT — and only what nothing else on this tab already says (finding
   * `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`). It used to end with `N open comments`, five centimetres above a
   * thread whose own header badges `N open`: the same count, written twice, on one screen.
   */
  const summary = [
    `${pages.length} page${pages.length === 1 ? "" : "s"}`,
    toElect > 0 ? `${toElect} to elect` : null,
  ]
    .filter((s) => s !== null)
    .join(" · ");

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-4">
      <div className="flex items-center gap-2">
        <LayoutTemplateIcon className="size-4 text-muted-foreground" />
        <h2 className="font-heading text-base font-semibold text-foreground">Prototype</h2>
        {roundLabel ? (
          <Badge variant="muted" className="shrink-0">
            {roundLabel}
          </Badge>
        ) : null}
        {currentLocked ? (
          <Badge variant="info" className="shrink-0" data-testid="proto-round-frozen">
            Election frozen
          </Badge>
        ) : null}
        <div className="ml-auto shrink-0">
          <RoundHistory tree={tree} />
        </div>
      </div>

      {/* THE ENTRY CARD → the dedicated prototype page. Same chassis as the Review tab's own.
          IT DESCRIBES NOTHING (finding `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`): it carried two sentences
          explaining that the stage lives on its own page, next to a button reading `Open prototype`
          and above a contact sheet of that page's own pages. The button is the explanation. */}
      <div className="flex items-center gap-3 rounded-xl border border-border/70 bg-muted/40 px-4 py-3.5">
        <LayoutTemplateIcon className="size-4 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium text-foreground">{summary}</div>
        </div>
        <Button
          size="sm"
          className="shrink-0"
          data-testid="open-prototype"
          render={<Link to="/work/$specId/prototype" params={{ specId }} />}
        >
          <LayoutTemplateIcon />
          Open prototype
        </Button>
      </div>

      {/* THE CONTACT SHEET — each card opens the page ON that page (`?page=<slug>`). */}
      <PagesSheet
        pages={pages}
        ctx={thumbnailCtx}
        renderLink={(page, children) => (
          <Link
            to="/work/$specId/prototype"
            params={{ specId }}
            search={{ page: page.slug }}
            data-testid={`proto-sheet-${page.slug}`}
            className="block overflow-hidden rounded-xl border border-border/60 transition-colors hover:border-border hover:bg-accent/30"
          >
            {children}
          </Link>
        )}
      />

      {/* The pre-dev review thread, READ-ONLY here: a comment is anchored to a node of the
          composition on screen, and the composition is on the page. */}
      <NodeComments tree={tree} actions={actions} composer={false} />
    </div>
  );
}
