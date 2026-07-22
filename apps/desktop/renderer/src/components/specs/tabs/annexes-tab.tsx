import {
  FileTextIcon,
  LayoutTemplateIcon,
  LinkIcon,
  type LucideIcon,
  PackageIcon,
  PaperclipIcon,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Spinner } from "@/components/ui/spinner";

import { useAnnexes } from "../use-annexes";
import type { AnnexListItem, SpecDetail } from "../types";

/** Per-kind display: the icon + human label for an annexe's `kind` (`doc`/`artefact`/`url`/`prototype`). */
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
 * `<AnnexesTab>` — the Spec's "Annexes" block (PRD "bloc Annexes sur le détail Spec"): the artefacts
 * attached to the Spec (doc / artefact / url / prototype), each with its kind, title, blob size, and
 * the dedup content-hash — served by the `annexe_list` read model (`useAnnexes`). An empty Spec shows
 * a clean placeholder (never broken). Built on the shared `<Badge>`/`<Spinner>` primitives + tokens.
 */
export function AnnexesTab({ detail }: { detail: SpecDetail }) {
  const { annexes, loading } = useAnnexes(detail.spec.id);

  if (loading) {
    return (
      <div className="flex justify-center py-16 text-muted-foreground">
        <Spinner />
      </div>
    );
  }

  if (annexes.length === 0) {
    return (
      <div className="mx-auto flex max-w-xl flex-col items-center gap-3 py-16 text-center">
        <span className="grid size-12 place-items-center rounded-full bg-muted text-muted-foreground">
          <PaperclipIcon className="size-5" />
        </span>
        <h2 className="font-heading text-base font-semibold text-foreground">Annexes</h2>
        <p className="text-sm text-muted-foreground">
          No annexes attached yet. Attach a doc, artefact, URL, or prototype to this Spec and it
          appears here — stored internally and deduplicated by content.
        </p>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-2xl">
      <div className="mb-3 flex items-center gap-2">
        <PaperclipIcon className="size-4 text-muted-foreground" />
        <h2 className="font-heading text-base font-semibold text-foreground">Annexes</h2>
        <span className="rounded-full bg-muted px-1.5 font-mono text-2xs text-muted-foreground">
          {annexes.length}
        </span>
      </div>
      <ul className="flex flex-col gap-1.5">
        {annexes.map((annexe) => (
          <AnnexeRow key={annexe.id} annexe={annexe} />
        ))}
      </ul>
    </div>
  );
}
