import {
  FileTextIcon,
  LayoutTemplateIcon,
  LinkIcon,
  type LucideIcon,
  PackageIcon,
  PaperclipIcon,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/ui/empty-state";
import { Spinner } from "@/components/ui/spinner";

import { SpecLinks } from "../spec-links";
import { useAnnexes } from "../use-annexes";
import { ANNEX_KIND_URL, type AnnexListItem, type SpecDetail } from "../types";

/**
 * Per-kind display: the icon + human label for an annexe's `kind` (`doc`/`artefact`/`url`/`prototype`).
 *
 * The `url` entry is KEPT although this block no longer renders url rows (they belong to
 * `<SpecLinks>`, see below): `KIND_META` covers the whole `annexes.kind` vocabulary of the backend,
 * not the subset one surface happens to draw today. Dropping it would make a url row — should any
 * future read stop filtering — fall silently onto the generic paperclip fallback instead of reading
 * as a link.
 */
const KIND_META: Record<string, { icon: LucideIcon; label: string }> = {
  doc: { icon: FileTextIcon, label: "Doc" },
  artefact: { icon: PackageIcon, label: "Artefact" },
  url: { icon: LinkIcon, label: "URL" },
  prototype: { icon: LayoutTemplateIcon, label: "Prototype" },
};

/** Human-readable byte size (a size hint next to each annexe). */
function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function AnnexeRow({ annexe }: { annexe: AnnexListItem }) {
  const meta = KIND_META[annexe.kind] ?? { icon: PaperclipIcon, label: annexe.kind };
  const Icon = meta.icon;
  return (
    <li className="flex items-center gap-3 rounded-lg border border-border/60 bg-card px-3 py-2.5">
      <span className="grid size-8 shrink-0 place-items-center rounded-md bg-muted text-muted-foreground">
        <Icon className="size-4" />
      </span>
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm text-foreground">{annexe.title}</p>
        <p className="mt-0.5 flex items-center gap-2 text-2xs text-muted-foreground">
          <span>{humanBytes(annexe.byte_len)}</span>
          <span aria-hidden>·</span>
          {/* The content_hash is the DEDUP key (the same bytes attach once) — shown short + mono. */}
          <span className="font-mono" title={`content hash ${annexe.content_hash}`}>
            {annexe.content_hash.slice(0, 10)}
          </span>
        </p>
      </div>
      <Badge variant="muted" className="shrink-0">
        {meta.label}
      </Badge>
    </li>
  );
}

/**
 * `<AnnexesTab>` — the Spec's attachments, in TWO blocks: the LINKS block first (`<SpecLinks>` —
 * the `url` annexes, clickable and add/removable, PRD "Liens externes sur un Spec"), then the
 * "Documents" block for everything else attached to the Spec (doc / artefact / prototype), each
 * with its kind, title, blob size, and the dedup content-hash — served by the `annexe_list` read
 * model (`useAnnexes`). Built on the shared `<Badge>`/`<Spinner>` primitives + tokens.
 *
 * The `url` rows are EXCLUDED from the lower list on purpose: they are the same rows the Links
 * block above already renders — as links, with their URL and their actions — and listing them twice
 * on one tab would say the same thing in two voices. Nothing is hidden: what is filtered here is
 * displayed there. Each block carries its OWN empty state, so a Spec with links but no document
 * (or the reverse) never shows a phantom section.
 *
 * WHY THE LOWER BLOCK IS CALLED "Documents" AND NOT "Annexes" (second human review, finding F8).
 * It used to be titled "Annexes", which made its counter LIE: a Spec carrying three links and no
 * document read "Annexes 0 — No annexe attached yet", while three annexes were in fact attached
 * (a link IS an annexe: `kind='url'`, same table). The count was never wrong about what the block
 * SHOWS — it is the word that claimed the whole domain while showing one part of it. Naming the
 * block after what it draws makes the zero true again and stops the two blocks' vocabularies from
 * overlapping: "Links" on one side, "Documents" on the other, each counting what it lists. The TAB
 * keeps the name "Annexes" — it holds both blocks, and links really are annexes — and so does
 * everything behind it (`useAnnexes`, the `annexe_list` db-command, the Rust authority).
 *
 * The exclusion itself CHANGES a pre-existing surface (this block used to list url rows), so it is
 * a DECLARED decision, not a silent one: it is written in the PRD revision's
 * `implementation-decisions-addenda` annex (section "SURFACE HUMAINE") and pinned by
 * `annexes-tab.test.tsx` — inverting the predicate, or renaming `ANNEX_KIND_URL` on the Rust side,
 * fails a test instead of quietly showing every link twice.
 */
export function AnnexesTab({ detail }: { detail: SpecDetail }) {
  const { annexes, loading } = useAnnexes(detail.spec.id);
  const attachments = annexes.filter((annexe) => annexe.kind !== ANNEX_KIND_URL);

  return (
    // `max-w-3xl`, like Spec / Tasks / Review / Runtime / Prototype: the column width is set per
    // tab in this app, and this tab was one of the two left at `2xl` — so the content jumped ~96px
    // wide when you moved between tabs. Aligned on the majority rather than kept as an exception.
    <div className="mx-auto flex max-w-3xl flex-col gap-8">
      <SpecLinks specId={detail.spec.id} />

      <section aria-labelledby="spec-documents-heading">
        <div className="mb-3 flex items-center gap-2">
          <PaperclipIcon className="size-4 text-muted-foreground" />
          <h2
            id="spec-documents-heading"
            className="font-heading text-base font-semibold text-foreground"
          >
            Documents
          </h2>
          <span className="rounded-full bg-muted px-1.5 font-mono text-2xs text-muted-foreground">
            {attachments.length}
          </span>
        </div>
        {loading ? (
          <div className="flex justify-center py-8 text-muted-foreground">
            <Spinner />
          </div>
        ) : attachments.length === 0 ? (
          // The SHARED primitive, like the Links block above — the hand-rolled `<p>` that stood
          // here spelled out the very classes `<EmptyState variant="dashed">` already carries, so
          // the two blocks' placeholders could drift apart on one edit.
          <EmptyState>
            No document attached yet. Attach a doc, artefact or prototype to this Spec and it
            appears here — stored internally and deduplicated by content.
          </EmptyState>
        ) : (
          <ul className="flex flex-col gap-1.5">
            {attachments.map((annexe) => (
              <AnnexeRow key={annexe.id} annexe={annexe} />
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
