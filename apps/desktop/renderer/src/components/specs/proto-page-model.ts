import type { PagesPanelPage } from "./tabs/pages-panel";
import {
  FEEDBACK_STATUS_OPEN,
  type ProtoTree,
  type ProtoTreePage,
  type ProtoTreeVersion,
  type PrototypeVariant,
  type RoundManifestPage,
} from "./proto-types";

/**
 * The pure RESOLUTION rules of the prototype tree — which version of a page is shown, which variant
 * it defaults to, and the per-page summary both prototype surfaces render (the dedicated page's rail
 * and the Spec tab's contact sheet).
 *
 * They live apart from the components because BOTH surfaces need the same answers, and because they
 * are the part worth pinning by test on their own: a wrong default variant silently shows the user
 * something other than what they retained.
 */

/**
 * The page-VERSION to SHOW: the pinned version (the current round's manifest entry), else the newest
 * live version, else the newest. `null` when the page has no version at all.
 */
export function displayVersion(page: ProtoTreePage): ProtoTreeVersion | null {
  if (page.versions.length === 0) return null;
  const pinnedId = page.manifest_entry?.page_version_id ?? null;
  return (
    (pinnedId != null ? page.versions.find((v) => v.version.id === pinnedId) : undefined) ??
    page.versions.find((v) => v.version.archived_at == null) ??
    page.versions[0] ??
    null
  );
}

/**
 * The variant a page DEFAULTS to: within the shown version, the EFFECTIVE retained variant
 * (`effective_variant_id` — the user's RETAINED, or the mono-variant retained by default), else the
 * agent's `is_main` reco, else the first. `null` when the page has no variant.
 *
 * It is a DEFAULT, not the shown variant: the user picks what the surface displays (that is the
 * point of the strip), and the pick is what the params editor, the comment anchors and the election
 * then act on.
 */
export function defaultVariant(page: ProtoTreePage): PrototypeVariant | null {
  const version = displayVersion(page);
  if (version === null || version.variants.length === 0) return null;
  const effectiveId = page.effective_variant_id;
  return (
    (effectiveId != null ? version.variants.find((v) => v.id === effectiveId) : undefined) ??
    version.variants.find((v) => v.is_main) ??
    version.variants[0] ??
    null
  );
}

/**
 * The tree AS ONE PAST ROUND SAW IT (finding `01KZ3AP34K1PKS6W0E75RP7DF6`).
 *
 * The rounds panel listed `v7 … v1` and nothing in it could be opened — a list of a thing whose
 * detail was unreachable. Consulting a round is NOT a second screen and NOT a second set of panels:
 * it is the SAME workbench, re-sourced. `proto_tree` already carries every version and variant a
 * page ever had (archived included) — the only thing bound to the current round is the ELECTION
 * (`manifest_entry` / `effective_variant_id`). So swapping that one pair for the consulted round's
 * manifest makes the rail, the stage and the variant strip show that round with no branch of their
 * own: the pinned VERSION is what {@link displayVersion} then resolves, the RETENU is what
 * {@link defaultVariant} then shows.
 *
 * The pages are the MANIFEST's, in ITS order: a round pins the pages it pins, and a page created
 * after it simply was not part of it — showing it would be inventing history. A manifest entry
 * naming a page the tree does not carry is dropped for the same reason.
 *
 * PURE — it derives a value and writes nothing.
 */
export function treeAtRound(tree: ProtoTree, manifest: readonly RoundManifestPage[]): ProtoTree {
  const byPageId = new Map(tree.pages.map((p) => [p.page.id, p]));
  const pages = manifest.flatMap<ProtoTreePage>((m) => {
    const page = byPageId.get(m.entry.page_id);
    if (page === undefined) return [];
    return [{ ...page, manifest_entry: m.entry, effective_variant_id: m.effective_variant_id }];
  });
  return { ...tree, pages };
}

/**
 * The per-page SUMMARY both surfaces show: the composition that would be displayed, how many
 * variants it competes with, whether a design is retained for it, and the OPEN comments anchored in
 * it — a comment names the variant it was raised on, so a page's count is that of every variant it
 * ever offered.
 */
export function panelPagesOf(tree: ProtoTree): PagesPanelPage[] {
  return tree.pages.map((p) => {
    const shown = defaultVariant(p);
    const version = displayVersion(p);
    const variantIds = new Set(p.versions.flatMap((v) => v.variants.map((c) => c.id)));
    return {
      id: p.page.id,
      title: p.page.title,
      slug: p.page.slug,
      compositionJson: shown?.composition_json ?? null,
      statesJson: shown?.states_json ?? null,
      retained: p.effective_variant_id !== null,
      variantCount: version?.variants.length ?? 0,
      openCommentCount: tree.open_feedback.filter(
        (f) =>
          f.status === FEEDBACK_STATUS_OPEN &&
          f.origin_variant_id !== null &&
          variantIds.has(f.origin_variant_id),
      ).length,
    };
  });
}
