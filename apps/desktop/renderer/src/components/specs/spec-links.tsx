import { type FormEvent, useState } from "react";
import { ExternalLinkIcon, LinkIcon, Trash2Icon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";

import type { SpecLink } from "./types";
import { useAnnexeActions } from "./use-annexe-actions";
import { useSpecLinks } from "./use-spec-links";

/**
 * `<SpecLinks>` — the Spec's EXTERNAL LINKS block (PRD "Liens externes sur un Spec"): the tickets,
 * PRs and docs a Spec points at, listed with their label, clickable, and add/removable from here.
 *
 * # It is not a new model, and not a CLI
 *
 * A link IS a `url` annexe (`annexes` + `annex_blobs`, v27): the stored content is the URL, the
 * `title` is the label, and the attachment hangs off the Spec's LOGICAL id — which is why a fork
 * keeps its links with no copy. This block is the HUMAN surface for it. It replaces the `prd show`
 * idea the PRD started from: that command belongs to depot, and nyx-v2 has no CLI at all (one
 * `nyx-sidecar` binary, an Electron child speaking MessagePack — no clap, no argv).
 *
 * # One authority, two surfaces
 *
 * Add and remove go through `annexe_add` / `annexe_remove`, the db-commands that call the SAME
 * `nyx_core::db` annexe authority the MCP `annexe_*` tools call (PDR-0005). So a link added here and
 * a link added by the agent produce the identical row, and BOTH sides' rules are the authority's:
 * an absolute http(s) URL, at most one occurrence of a URL per Spec, and a blank label stored as the
 * URL itself. This component validates NOTHING of its own — it DISPLAYS the backend's refusal, in
 * the backend's words.
 *
 * # Clicking opens the real browser, never a view of the app
 *
 * Each link is a plain anchor with `target="_blank"`. The Electron shell's window policy
 * (`src/main/window.ts`) DENIES every programmatic window creation and hands an http/https target
 * to `shell.openExternal`; a same-frame navigation away from the app is likewise blocked and opened
 * externally. So the anchor is the whole mechanism — there is no in-app browser to escape from, and
 * no bespoke IPC needed to reach the OS one.
 */
export function SpecLinks({ specId }: { specId: string }) {
  const { links, loading } = useSpecLinks(specId);
  const { pending, adding, error, clearError, addLink, removeAnnexe } = useAnnexeActions();
  const [url, setUrl] = useState("");
  const [label, setLabel] = useState("");

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (url.trim().length === 0) return;
    const added = await addLink(specId, url, label);
    // Only clear the form when the link really landed: a refused URL stays in the field so the
    // user can fix it instead of retyping it.
    if (added) {
      setUrl("");
      setLabel("");
    }
  };

  // This block carries NO width of its own: `<AnnexesTab>` already centres and caps the column, and
  // the two blocks must line up on it. A `mx-auto max-w-2xl` here did NOT merely duplicate that cap
  // — auto margins on a flex child cancel the cross-axis `stretch`, so the section shrank to its
  // content width and sat visibly NARROWER than the "Documents" block below it.
  return (
    <section aria-labelledby="spec-links-heading">
      <div className="mb-3 flex items-center gap-2">
        <LinkIcon className="size-4 text-muted-foreground" />
        <h2
          id="spec-links-heading"
          className="font-heading text-base font-semibold text-foreground"
        >
          Links
        </h2>
        <span className="rounded-full bg-muted px-1.5 font-mono text-2xs text-muted-foreground">
          {links.length}
        </span>
      </div>

      <form className="mb-3 flex flex-wrap items-center gap-2" onSubmit={submit}>
        {/*
         * The URL field takes TWO shares against the label's ONE (`flex-[2]` vs `flex-1`). Both
         * were `flex-1` — same basis, same growth — so a 60-character URL was typed into half a
         * row while the card right below it laid the same URL out over the full width. The share
         * is proportional rather than a fixed width so it holds when the container changes; the
         * `flex-wrap` and both `min-w` stay, since they are what folds the form onto two lines in
         * a narrow window (the label keeps `min-w-40`, above its floor at the measured widths).
         */}
        <Input
          className="min-w-56 flex-[2]"
          type="text"
          value={url}
          placeholder="https://…"
          aria-label="Link URL"
          onChange={(e) => {
            setUrl(e.target.value);
            if (error) clearError();
          }}
        />
        <Input
          className="min-w-40 flex-1"
          type="text"
          value={label}
          placeholder="Label (optional)"
          aria-label="Link label"
          onChange={(e) => {
            setLabel(e.target.value);
            // Same as the URL field: editing the form is the user answering the refusal, so the
            // stale sentence goes. It was wired to ONE of the two fields, which left the alert
            // standing while the user retyped the label.
            if (error) clearError();
          }}
        />
        {/*
         * IN-FLIGHT STATE, SHOWN AND ANNOUNCED. `disabled` was the only signal, and a small button
         * that greys out is not noticed: on a real add the wait is SECONDS, during which nothing
         * moved on screen and the app read as frozen. So the button now carries the spinner
         * primitive and a distinct label while the write is in flight, plus `aria-busy` — the
         * greying alone says nothing to a screen reader. `min-w-24` is wider than BOTH labels, so
         * the two states render at one width and the form line does not jump when they swap.
         *
         * The `disabled` stays exactly as it was: it is what makes a double-submit impossible
         * (`useAnnexeActions` documents `pending` as the gate — ANY write, removals included), and
         * the empty-URL guard is untouched. What is SHOWN is `adding` and not `pending`: a removal
         * raises the gate too, and a button that spins "Adding…" while a link is being DETACHED
         * says something that is not happening.
         */}
        <Button
          type="submit"
          size="sm"
          className="min-w-24"
          aria-busy={adding}
          disabled={pending || url.trim().length === 0}
        >
          {adding ? (
            <>
              {/* Hidden from the a11y tree: the label already says "Adding…" and `aria-busy`
                  carries the state — the spinner's own `role="status"` would only add noise to
                  the button's accessible name. */}
              <Spinner aria-hidden />
              Adding…
            </>
          ) : (
            "Add link"
          )}
        </Button>
      </form>

      {/* The authority's own sentence — a refused URL and a duplicate link are both user-facing. */}
      {error !== null && (
        <p role="alert" className="mb-3 text-xs text-destructive">
          {error}
        </p>
      )}

      {loading ? (
        <div className="flex justify-center py-8 text-muted-foreground">
          <Spinner />
        </div>
      ) : links.length === 0 ? (
        <EmptyState>
          No external link on this Spec yet. Attach a ticket, a PR or a doc URL — it stays on the
          Spec, fork included.
        </EmptyState>
      ) : (
        <ul className="flex flex-col gap-1.5">
          {links.map((link) => (
            <SpecLinkRow
              key={link.id}
              link={link}
              disabled={pending}
              onRemove={() => void removeAnnexe(link.id)}
            />
          ))}
        </ul>
      )}
    </section>
  );
}

/** One link row: the LABEL as the external anchor, the URL underneath, and the detach action. */
function SpecLinkRow({
  link,
  disabled,
  onRemove,
}: {
  link: SpecLink;
  disabled: boolean;
  onRemove: () => void;
}) {
  // A link with no label was stored with the URL as its title — showing both lines would then say
  // the same thing twice.
  const labelled = link.title !== link.url;
  return (
    <li className="flex items-center gap-3 rounded-lg border border-border/60 bg-card px-3 py-2.5">
      <span className="grid size-8 shrink-0 place-items-center rounded-md bg-muted text-muted-foreground">
        <LinkIcon className="size-4" />
      </span>
      <div className="min-w-0 flex-1">
        <a
          href={link.url}
          target="_blank"
          rel="noreferrer noopener"
          className="inline-flex max-w-full items-center gap-1.5 truncate text-sm text-foreground underline-offset-4 hover:underline"
        >
          <span className="truncate">{link.title}</span>
          <ExternalLinkIcon className="size-3 shrink-0 opacity-70" aria-hidden />
        </a>
        {labelled && (
          <p className="mt-0.5 truncate text-2xs text-muted-foreground" title={link.url}>
            {link.url}
          </p>
        )}
      </div>
      <Button
        variant="ghost-destructive"
        size="icon-xs"
        disabled={disabled}
        aria-label={`Remove link ${link.title}`}
        onClick={onRemove}
      >
        <Trash2Icon />
      </Button>
    </li>
  );
}
