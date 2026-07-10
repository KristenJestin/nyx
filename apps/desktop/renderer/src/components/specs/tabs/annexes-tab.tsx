import { PaperclipIcon } from "lucide-react";

import type { SpecDetail } from "../types";

/**
 * `<AnnexesTab>` — the Spec's annexes (agent-authored artifacts: files, images, docs). The
 * annexes DOMAIN (a backing model) is explicitly OUT of v2.4 scope, so with no annexes model in
 * phases 1-3 this tab reserves the slot and renders a DEFERRED-FEATURE placeholder (not an empty
 * state) so it reads as
 * "coming later", never as broken. It mirrors the Review tab's "grafts in a later iteration"
 * copy pattern. Placement reproduced from the prototype's `annexes-tab` (a grouped artifact
 * list) collapsed — the list grows when the annexes model lands.
 */
export function AnnexesTab(_props: { detail: SpecDetail }) {
  return (
    <div className="mx-auto flex max-w-xl flex-col items-center gap-3 py-16 text-center">
      <span className="grid size-12 place-items-center rounded-full bg-muted text-muted-foreground">
        <PaperclipIcon className="size-5" />
      </span>
      <h2 className="font-heading text-base font-semibold text-foreground">Annexes</h2>
      <p className="text-sm text-muted-foreground">
        Annexes — agent-authored artifacts (files, images, docs) attached to this Spec — arrive in a
        later version. This tab reserves their home; nothing is missing or broken yet.
      </p>
    </div>
  );
}
