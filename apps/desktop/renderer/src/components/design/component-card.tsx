import { Trash2Icon } from "lucide-react";
import { useEffect, useMemo, useState } from "react";

import { nyxBridge } from "@/bridge";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import { axisDefaults, parseAxes, primaryAxisStates } from "./axes";
import { CompositionFrame } from "./composition-frame";
import {
  parseParams,
  paramsWithoutPreviewValue,
  representativeComposition,
  requiredParamsWithoutPreview,
} from "./params";
import { previewState } from "./preview-state";
import { themeHasDark } from "./theme-css";
import { componentNature, isOrphan, type DesignComponentRow } from "./types";
import { useDesignRender } from "./use-design-render";

/**
 * `ComponentCard` — one catalogue tile (v2.7 Phase 3, task #10). The card shows ONE REPRESENTATIVE
 * INSTANCE of the component rendered in the local-Tailwind iframe (PDR-0010: "carte = 1 instance
 * représentative, pas la liste de params"), plus its name and provenance badges (`[DS]` existing /
 * `[NEW]` proposed, and a "liés-page" marker). Editing lives in the composant detail (task #11), so
 * the card is display-only here; `onOpen` (wired by #11) opens the detail.
 *
 * "ONE instance" is taken literally (finding `01KYPD8K75EC2AAY1KVAS2XZNX`): the preview runs
 * WITHOUT the renderer's states chip-bar. Enumerating a component's variant axes is a full-page
 * affordance — it belongs to the composant detail and to the proto-viewer, which keep it; on a
 * 160 px tile the chips left ~40 px for the component itself.
 *
 * And a vignette is a PICTURE, not an interaction surface (finding `01KYPEG16E95WADT2VMCYBPCK5`):
 * the preview runs with NO chrome at all (`showChrome={false}` — the chip-bar and the dashed
 * `data-nyx-*` affordance outlines), CENTRED in the tile (`center`) and on the project's OWN
 * background token instead of a hard `bg-white`. The tile reads at a glance: one element, in the
 * middle, on a surface that matches the design system it comes from.
 *
 * It carries ONE catalogue write: DELETE (review #44 — an entry a wrong glob had catalogued could be
 * removed from nowhere). The inventory prunes on its own the rows whose source really vanished; this
 * covers what it deliberately spares (a mistaken `[NEW]`, an orphan whose repo is momentarily
 * unscannable). It asks for a CONFIRMATION first, so a mis-click never destroys a synthesis.
 *
 * AND THE CONFIRMATION SAYS WHAT THE REMOVAL REALLY ACHIEVES (finding `01KYSHSPGSPC98C49X68MWY60D`).
 * A component's identity IS its source: for a `[DS]` row whose file is still on disk, deleting the row
 * is UNDONE by the next inventory, which re-discovers the same file under a new id. The gesture used
 * to promise a removal that did not last, silently. So the armed panel NAMES the ephemeral character
 * and offers the DURABLE form of the same decision — the `excludeSource` exclusion the backend writes
 * into the project's own recipe. That choice must travel WITH the delete (they are one atomic act in
 * `nyx_core::design::remove_component`, and once the row is gone there is no id left to exclude
 * anything for), which is why it is a checkbox here rather than an after-the-fact offer. What the
 * BACKEND then reports — its verbatim `warnings[]`, or a refusal that deleted nothing — is rendered
 * by the page (`CataloguePage`), since a deleted card cannot carry its own epilogue.
 *
 * WHAT IT COSTS COMES FIRST (finding `01KZ3JW5A63TJWX83V8ZKX62CN`). The panel opened on three
 * internal tool names and a description of what the backend would answer, and never said the one
 * thing that decides the gesture: HOW MANY PAGES use this component. It does now, read on arm, and
 * the machinery's names have left the human-facing text — they belong to the agent's manual.
 *
 * The exclusion is offered ONLY where it can work: never for a greenfield `[NEW]` (no source to
 * exclude — the backend refuses it and deletes nothing) and never for an ORPHAN (the last inventory
 * found the source gone, so the removal is already definitive). Offering a control whose refusal is
 * certain is just another lie.
 */
export interface ComponentCardProps {
  /**
   * The project the render is scoped to. NULLABLE on purpose: the active project is resolved
   * asynchronously and `useDesignRender` short-circuits on `null`, so the tile must be able to SAY
   * "cannot render" instead of falling off the end of a ladder into a blank rectangle.
   */
  projectId: string | null;
  component: DesignComponentRow;
  /** The project's theme tokens, shared across all cards (one design authority). */
  themeTokensJson: string | null | undefined;
  /** Whether the component is referenced by a proto page (the liés-page marker). */
  linked: boolean;
  /** Open the composant detail (task #11). */
  onOpen?: (component: DesignComponentRow) => void;
  /**
   * REMOVE the component from the catalogue (review #44). Omitted → no delete affordance.
   *
   * `excludeSource` is the human's DURABLE variant: the source is appended to the project's recipe
   * `exclude` list in the same transaction as the delete, so the next inventory does not bring the
   * row back (finding `01KYSHSPGSPC98C49X68MWY60D`).
   */
  onDelete?: (
    component: DesignComponentRow,
    options: { excludeSource: boolean },
  ) => void | Promise<void>;
}

export function ComponentCard({
  projectId,
  component,
  themeTokensJson,
  linked,
  onOpen,
  onDelete,
}: ComponentCardProps) {
  const nature = componentNature(component);
  const orphan = isOrphan(component);
  // THE DEFAULT COMBINATION is what a vignette shows (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`): one
  // value per AXIS, never an enumeration of the axes — the tile is one picture of the component.
  const axes = useMemo(() => parseAxes(component.states_json), [component.states_json]);
  const states = useMemo(() => primaryAxisStates(axes), [axes]);
  const combination = useMemo(() => axisDefaults(axes), [axes]);
  const synthesized = component.template.trim() !== "";
  // A single representative instance seeded from the component's APERÇU params (`preview >
  // default > rien` — finding `01KYPJ1RC6WJX1Z9CPPV23FG27`: a required param has no default by
  // definition, so only an example value can make it render) — or NOTHING for a component
  // DISCOVERED but not yet SYNTHESIZED (review #40): there is no template to render, so we
  // short-circuit the render hook (it treats `null` as "disabled") instead of firing one
  // `design_render` IPC per card whose result is thrown away.
  const descriptors = useMemo(
    () => parseParams(component.params_json, axes),
    [component.params_json, axes],
  );
  const composition = useMemo(
    () => (synthesized ? representativeComposition(component.name, descriptors, axes) : null),
    [component.name, descriptors, axes, synthesized],
  );
  // What the aperçu could not render, in TWO degrees (findings `01KYPD8K75EC2AAY1KVAS2XZNX` then
  // `01KYPJ1RC6WJX1Z9CPPV23FG27`):
  //   * a REQUIRED param with neither preview value nor default — a definite defect: the component
  //     DEMANDS that value, so no aperçu of it can be right;
  //   * any other param whose emptiness leaves a hole a human would SEE — the blank is milder, but
  //     still a blank.
  // That second degree is TRIAGED by the template (finding `01KYVMTWVF3TH1XPEKV9MEBZTB`), which is
  // why `component.template` is handed to it: a passthrough substituted only inside a tag —
  // `class`, `wrapperClass`, `testId` — is empty BY NATURE, and a caveat that appeared on every tile
  // of the catalogue was one nobody read any more.
  // The card does NOT fabricate content in their place: a representative label is a fact about the
  // real component, which only the synthesis (the agent reading the source) knows — the Prototype
  // manual now asks for a preview value exactly there. All the card does is NAME the gap, as a
  // tooltip: zero visual weight on a 160 px tile, but the blank stops being a mystery.
  const missingRequired = useMemo(() => requiredParamsWithoutPreview(descriptors), [descriptors]);
  const missingPreview = useMemo(
    () => paramsWithoutPreviewValue(descriptors, component.template),
    [descriptors, component.template],
  );
  // Does the project's design system define a DARK variant (finding `01KYSVEZMJP1Z8V7SYJ8YC5H9W`)?
  // The frame follows nyx's appearance by selecting the project's OWN `.dark` overrides; a
  // light-only layer has nothing to select, so the tile shows the light variant — and says it. A
  // 160 px tile has no room for a badge, so it goes where the tile's other caveats already live:
  // the tooltip. Zero visual weight, and the contrast with a dark app stops being a mystery.
  const layerHasDark = useMemo(() => themeHasDark(themeTokensJson), [themeTokensJson]);
  // `states_json` twice over, and deliberately: the frame hands it to the SHIM (`axes` below),
  // the render is TOLD about it so it bakes no node axis the shim already pilots — one owner per
  // axis (finding `01KZ41YK39SGNKF6XV87A35GPN`).
  const { markup, loading, error } = useDesignRender(
    projectId,
    composition,
    null,
    component.states_json,
  );
  /**
   * WHAT THE TILE SHOWS — decided by the TOTAL function, never by a ladder of its own (finding
   * `01KZ3JPW0RFM7AZ0AG2YASKBNR`'s family). `useDesignRender` SHORT-CIRCUITS on a `null` project
   * (`markup: null, loading: false, error: null`), and this tile's hand-written ladder fell off its
   * end into `null` — a tile with a name, a delete button and a blank rectangle above them, saying
   * nothing at all. [`previewState`] answers every combination, so `loading` is only ever a real
   * in-flight read and every other outcome is a sentence.
   */
  const preview = previewState({
    // "Is there anything to preview" — for a CATALOGUE tile that is the synthesis, not a variant.
    hasVariant: synthesized,
    projectId,
    compositionJson: composition,
    renderLoading: loading,
    renderError: error,
    markup,
  });
  // The delete is a TWO-STEP: the first click ARMS it (it opens the confirmation panel), the button
  // inside removes. No modal, no dependency — a destructive catalogue write is never one stray click
  // away, and moving the focus out of the panel disarms it again.
  const [confirming, setConfirming] = useState(false);
  // The DURABLE variant of the same decision (finding `01KYSHSPGSPC98C49X68MWY60D`): also write the
  // exclusion into the project's recipe, so the next inventory does not re-discover the source.
  const [excludeSource, setExcludeSource] = useState(false);
  // Can an exclusion do anything here? It needs a source the scan could still take: a greenfield
  // `[NEW]` has none, and an ORPHAN's is already gone — the backend REFUSES both (and then deletes
  // nothing), so the control is not offered rather than offered and refused.
  const canExclude = component.source_path !== null && !orphan;

  /**
   * HOW MANY PAGES USE THIS COMPONENT — the one thing the confirmation owes and did not say
   * (finding `01KZ3JW5A63TJWX83V8ZKX62CN`). The panel cited three internal tool names and explained
   * what the backend would answer, while the card just above showed a `linked` badge whose whole
   * content — "somewhere, once at least" — was the question the reader actually had.
   *
   * Read ON ARM, never per card: eighty tiles must not each cost a walk of the project's protos.
   * `null` = not answered (in flight, or the read failed) and then NOTHING is claimed — a count one
   * could not obtain is not a zero.
   */
  const [pagesUsing, setPagesUsing] = useState<number | null>(null);
  useEffect(() => {
    if (!confirming) return;
    let cancelled = false;
    setPagesUsing(null);
    void nyxBridge
      .invoke<{ nodes?: readonly { page_title?: string }[] }>("design_component_nodes", {
        projectId,
        componentName: component.name,
      })
      .then((answer) => {
        if (cancelled) return;
        const pages = new Set((answer.nodes ?? []).map((n) => n.page_title ?? ""));
        setPagesUsing(pages.size);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [confirming, projectId, component.name]);

  const clickable = typeof onOpen === "function";
  return (
    <div
      data-testid={`component-card-${component.id}`}
      className={cn(
        "group flex flex-col overflow-hidden rounded-xl border border-border/60 bg-card",
        clickable && "cursor-pointer transition-colors hover:border-border",
      )}
      onClick={clickable ? () => onOpen?.(component) : undefined}
      role={clickable ? "button" : undefined}
      tabIndex={clickable ? 0 : undefined}
      onKeyDown={
        clickable
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onOpen?.(component);
              }
            }
          : undefined
      }
    >
      {/* Preview: the representative instance in the local-Tailwind iframe. A component DISCOVERED
          but not yet SYNTHESIZED (review #40: nyx catalogues the bundle, an agent writes the
          template) has nothing to render — say so instead of showing a blank card. */}
      <div
        // No `bg-white` (finding `01KYPEG16E95WADT2VMCYBPCK5`): the tile's surface is the PROJECT'S
        // — the frame paints its own canvas from the `--background` token — and this container is
        // only what shows through when a project has no token for it: an app-themed neutral, not a
        // raw white rectangle in a dark app.
        className="relative h-40 overflow-hidden border-b border-border/60 bg-muted/30"
        title={
          !synthesized
            ? undefined
            : [
                missingRequired.length > 0
                  ? `Rendered with the preview params. These params are REQUIRED but have no preview value: ${missingRequired.join(", ")} — the component cannot be previewed as it really is until the synthesis gives them one.`
                  : missingPreview.length > 0
                    ? `Rendered with the preview params. No preview value nor default was synthesized for: ${missingPreview.join(", ")} — they render empty.`
                    : null,
                layerHasDark
                  ? null
                  : "LIGHT variant: this project's design system declares no dark override (`dark`) in its token layer. The preview therefore renders the single variant that exists, rather than inventing a dark one.",
              ]
                .filter(Boolean)
                .join("\n\n") || undefined
        }
      >
        {preview.kind === "no-variant" ? (
          <div className="flex h-full items-center justify-center p-3 text-center text-2xs text-muted-foreground">
            to synthesize
          </div>
        ) : preview.kind === "loading" ? (
          <div className="flex h-full items-center justify-center">
            <Spinner className="text-muted-foreground/50" />
          </div>
        ) : preview.kind !== "ready" ? (
          /* A 160 px tile has room for a few words, never for the sidecar's sentence — the detail
             says it in full. What matters here is that each dead end reads as a DEAD END rather
             than as a blank tile or an eternal wait. */
          <div
            data-testid={`component-card-preview-${preview.kind}`}
            title={
              preview.kind === "error" || preview.kind === "unavailable"
                ? preview.message
                : undefined
            }
            className={cn(
              "flex h-full items-center justify-center p-3 text-center text-2xs",
              preview.kind === "error" ? "text-destructive/80" : "text-muted-foreground/70",
            )}
          >
            {preview.kind === "error"
              ? "render failed"
              : preview.kind === "unavailable"
                ? "cannot render"
                : "empty"}
          </div>
        ) : (
          <CompositionFrame
            themeTokensJson={themeTokensJson}
            states={states}
            // The AXES reach the shim (it applies the DEFAULT value's classes of each one and
            // filters the structural cases) — only the CONTROLS are dropped below.
            axes={axes}
            axisValues={combination}
            // NO CHROME on a vignette: neither the chip-bar (finding `01KYPD8K75EC2AAY1KVAS2XZNX`:
            // the tile shows ONE instance — the default one — not the list of the component's
            // variant axes, a `badge` has 13 and their chips ate 3-4 rows of the 160 px) nor the
            // dashed `data-nyx-*` outlines (finding `01KYPEG16E95WADT2VMCYBPCK5`: an affordance
            // for a surface nobody clicks — the tile is `pointer-events-none` — that drew debug
            // dashes around the preview). The states are STILL handed to the shim so the frame
            // renders the ACTIVE state and hides the other state cases; only the chrome goes. Both
            // affordances stay where a full page affords them: the detail and the proto-viewer.
            showChrome={false}
            // …and the instance sits in the MIDDLE of the tile, clipped at its edge if it is wider.
            center
            className="pointer-events-none h-full"
            title={`${component.name} preview`}
            markup={preview.markup}
          />
        )}
      </div>

      {/* Meta: name + provenance / liés-page badges + the catalogue DELETE (review #44). */}
      <div className="flex items-center justify-between gap-2 px-3 py-2">
        <span className="truncate text-sm font-medium text-foreground" title={component.name}>
          {component.name}
        </span>
        <div className="flex shrink-0 items-center gap-1">
          {/* ONLY THE EXCEPTION IS BADGED (finding `01KZ3JW5A63TJWX83V8ZKX62CN`). A card carries `DS`
              or `NEW` by construction, so `DS` was on 76 of 79 tiles: a mark every card wears marks
              nothing, it just repaints the grid. `NEW` is the exception — the component the agent
              PROPOSES, which nobody has built yet — and that is the one worth seeing from across the
              grid. What was lost with the `DS` chip is nowhere else on the tile, and it was not
              needed there: the nature is a filter, with its own count, one row above. */}
          {nature === "new" ? <Badge variant="warning">NEW</Badge> : null}
          {/* ORPHELINE: the last inventory found this component's source file GONE while it carried
              a synthesis. It is deliberately KEPT (a scan never destroys reviewed LLM work — it can
              silently fail to read a directory), so the catalogue has to say which rows no longer
              answer to a real file. The delete button beside it is the way out. */}
          {orphan ? (
            <Badge
              variant="destructive"
              title={`No source file was found for ${component.name} at the last inventory — its synthesis is kept until you decide`}
            >
              orphan
            </Badge>
          ) : null}
          {linked ? <Badge variant="success">linked</Badge> : null}
          {onDelete ? (
            <button
              type="button"
              // The card itself opens the detail — a click on the delete must never navigate.
              onClick={(e) => {
                e.stopPropagation();
                setConfirming((armed) => !armed);
                setExcludeSource(false);
              }}
              // THE KEYBOARD REACHES IT TOO (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point B). The
              // card opens the detail on Enter/Space; a `keydown` on this button BUBBLED up to it,
              // so tabbing to the trash and pressing Enter NAVIGATED — the confirmation never
              // opened and deleting a component was impossible without a mouse. The native button
              // still turns Enter/Space into the `click` above; only the bubbling stops here.
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") e.stopPropagation();
              }}
              aria-expanded={confirming}
              aria-label={
                confirming
                  ? `Cancel removing ${component.name} from the catalogue`
                  : `Remove ${component.name} from the catalogue`
              }
              title={
                confirming ? "Abandon the removal" : "Remove this component from the catalogue"
              }
              className={cn(
                "rounded p-1 text-muted-foreground transition-colors hover:text-destructive",
                confirming && "bg-destructive/10 text-destructive",
              )}
            >
              <Trash2Icon aria-hidden className="size-3.5" />
            </button>
          ) : null}
        </div>
      </div>

      {/* THE CONFIRMATION, which says what the removal really achieves (finding
          `01KYSHSPGSPC98C49X68MWY60D`). It names the EPHEMERAL character of a row whose source is
          still on disk, names the correction, and carries the DURABLE variant of the decision —
          `excludeSource`, which must travel WITH the delete (one atomic act in the backend). The
          backend's own verdict + `warnings[]` land on the page after the call: only it knows the
          verdict and the recipe, so the surface never re-writes its sentences. */}
      {onDelete && confirming ? (
        <div
          data-testid={`component-card-confirm-${component.id}`}
          className="border-t border-destructive/30 bg-destructive/5 px-3 py-2"
          // The card opens the detail on click AND on Enter/Space — neither must fire from inside
          // the panel (Space on the checkbox would otherwise navigate away mid-decision).
          onClick={(e) => e.stopPropagation()}
          onKeyDown={(e) => e.stopPropagation()}
          // Moving the focus OUT of the panel disarms it, the way blurring the button used to.
          onBlur={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setConfirming(false);
          }}
        >
          <p className="text-2xs font-medium text-foreground">
            Remove &quot;{component.name}&quot; from the catalogue?
          </p>
          {/* WHAT IT COSTS, first: the pages that place this component keep their node, and it
              renders as a named hole from then on. */}
          {pagesUsing !== null ? (
            <p className="mt-1 text-2xs text-muted-foreground" data-testid="delete-usage">
              {pagesUsing > 0 ? (
                <>
                  Used on{" "}
                  <strong className="font-semibold text-foreground">
                    {pagesUsing} page{pagesUsing > 1 ? "s" : ""}
                  </strong>{" "}
                  of this project: those placements stay, and render as a hole.
                </>
              ) : linked ? (
                "No page places it directly — another component's template pulls it in."
              ) : (
                "No page uses it."
              )}
            </p>
          ) : null}
          {canExclude ? (
            <>
              <p
                className="mt-1 text-2xs text-muted-foreground"
                data-testid="delete-ephemeral-note"
              >
                <strong className="font-semibold text-foreground">Ephemeral</strong>: a component IS
                its source file. While <code className="font-mono">{component.source_path}</code> is
                still scanned, the next inventory brings it back.
              </p>
              <label className="mt-1.5 flex cursor-pointer items-start gap-2 text-2xs text-foreground">
                <Checkbox
                  checked={excludeSource}
                  onCheckedChange={(next) => setExcludeSource(next === true)}
                  aria-label={`Exclude ${component.name}'s source from the project's recipe`}
                  className="mt-px"
                />
                <span>
                  Make it <strong className="font-semibold">durable</strong> — stop scanning that
                  file. A persisted decision, and a reversible one.
                </span>
              </label>
            </>
          ) : (
            <p className="mt-1 text-2xs text-muted-foreground" data-testid="delete-definitive-note">
              {orphan
                ? "Definitive: the last inventory no longer found its source, so nothing can bring it back."
                : "Definitive: this component has no source file, so nothing can bring it back."}
            </p>
          )}
          <div className="mt-2 flex items-center gap-2">
            <button
              type="button"
              onClick={() => {
                setConfirming(false);
                void onDelete(component, { excludeSource: canExclude && excludeSource });
              }}
              aria-label={`Confirm removing ${component.name} from the catalogue`}
              className="rounded border border-destructive/40 bg-destructive/10 px-2 py-1 text-2xs font-medium text-destructive transition-colors hover:bg-destructive/20"
            >
              {canExclude && excludeSource ? "Delete and exclude" : "Delete"}
            </button>
            <button
              type="button"
              onClick={() => {
                setConfirming(false);
                setExcludeSource(false);
              }}
              className="rounded px-2 py-1 text-2xs text-muted-foreground transition-colors hover:text-foreground"
            >
              Cancel
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
