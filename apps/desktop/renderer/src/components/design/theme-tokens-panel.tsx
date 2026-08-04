import { PaletteIcon } from "lucide-react";
import { useMemo, useState } from "react";

import { cn } from "@/lib/utils";

import { parseTokenLayer, resolveTokenColor, type TokenLayer } from "./theme-css";

/**
 * `ThemeTokensPanel` — the project's THEME TOKENS, READ-ONLY (v2.7, review #42).
 *
 * The catalogue showed COMPONENTS and nothing else, while the other half of a component's look —
 * the tokens its Tailwind classes resolve against — had no surface at all and was never even read
 * from the app. This panel is that surface: the stored layer, WHICH app file it is referenced from,
 * and which tokens carry a dark variant. The layer is a REFERENCE to that file, never a copy per
 * component.
 *
 * # Why there is NO editing here any more (finding `01KZ3JQXGSVZAC2MHHNZAS681Y`)
 *
 * Each row used to be an `<input>` that wrote through `design_tokens_set` on blur. The gesture LIED
 * three times over: nothing re-rendered (no preview, no vignette, no composition — they are compiled
 * by the sidecar from the STORED layer, and the frames on screen were never rebuilt), nothing said
 * the value had been written, nothing could take it back — and the write SURVIVED a restart, so the
 * project's data drifted away from the app's own stylesheet with no trace anywhere.
 *
 * The arbitration is the doctrine already established on this surface, applied one step further: the
 * token layer is INGESTED from the app's own stylesheet by the agent, exactly as the component
 * inventory is (PDR-0009, review `01KYKX2A0JGWF8XNWWZ9YFF5MG`, finding
 * `01KYKX7HBP4ZSSS6ZDM31T6A5E` — the panel already lost its "Tokens depuis l'app" trigger for that
 * reason). A token is not a judgement the UI arbitrates; it is a FACT about the project's stylesheet,
 * and the only honest way to change `--primary` is to change it in the app and let the ingest read it
 * again. An edit here could only ever produce a value that the next ingest silently overwrites.
 *
 * So the edit DISAPPEARS — it is not a disabled input, not a greyed field: a control that is present
 * and inert is the same lie in a quieter voice. The `design_tokens_set` db-command is untouched and
 * still tested; what is gone is the UI's claim to pilot it.
 *
 * A value ALREADY written by that gesture stays in `theme_tokens_json` until the next
 * `design_tokens` ingest re-derives the layer from the sheet and replaces it — the ingest rewrites
 * the whole layer, so nothing here has to migrate anything, and the panel now shows the stored value
 * as what it is: a reading.
 *
 * The CAPABILITY behind the layer is not the UI's either — ingestion emits `prototypes://changed`,
 * `useDesign` re-pulls on it, and the new layer arrives here through `themeTokensJson` with no poll
 * and no click.
 *
 * WHERE THE LAYER COMES FROM IS PART OF THE LAYER (finding `01KYSHSPGSPC98C49X68MWY60D`, after
 * `01KYSCX562SWDXZBBAPX397X8F`). A token layer is a claim: "this IS the project's design system".
 * The panel showed the best-ranked source sheet and nothing else, so a layer merged from a
 * `.generated` mirror and a docs theme looked exactly like a healthy one, and a developer whose own
 * stylesheet was skipped had no way to find out. The PROVENANCE block answers both questions with the
 * ingest's own report: the sheets RETAINED (with how many variables each contributed), the ones
 * DISCARDED with the backend's reason code, and the directories the walk never entered. Still a pure
 * READ — nothing here re-runs the ingest.
 */

export interface ThemeTokensPanelProps {
  /** The project's `design.theme_tokens_json` (the stored layer). */
  themeTokensJson: string | null;
  className?: string;
}

/**
 * A colour chip for a token whose value paints — the fastest way to see a theme is real.
 *
 * The colour is resolved in the PROJECT'S layer ([`resolveTokenColor`]), never handed to the browser
 * as-is: an inline `var(--secondary)` is resolved against NYX's stylesheet, which is how 36 rows came
 * to paint the app's own colours instead of the project's (finding `01KZ3JQY3FK9RRNS15H2BCCSN0`).
 * A value the layer cannot resolve renders NO chip: a missing swatch is a gap, a wrong one is a lie.
 */
function Swatch({ layer, value }: { layer: TokenLayer; value: string }) {
  const color = resolveTokenColor(layer, value);
  if (color === null) return null;
  return (
    <span
      aria-hidden
      data-testid="token-swatch"
      className="size-4 shrink-0 rounded-sm border border-border/60"
      style={{ background: color }}
    />
  );
}

/**
 * What a DISCARD reason code means, in one clause. The backend's CODE stays visible next to it — it
 * is the word the `design_tokens` tool answers with, and the one to quote when asking why a sheet
 * was dropped; this map only makes it readable. An unknown code prints as itself.
 */
const SKIP_REASON_MEANING: Record<string, string> = {
  recipe_exclude: "the project recipe's `exclude` globs cover it",
  doc_tool_theme: "it lives in another tool's theme directory (docs)",
  foreign_namespace: "its variables are mostly a third-party namespace (--vp-*, …)",
  not_a_token_sheet: "too few GLOBAL custom properties to be a token sheet",
  unreadable: "unreadable — discarded out of caution, never fatal",
  peripheral_sheet: "a docs / example / demo sheet, while the project has an application one",
  outranked: "a real candidate, ranked below the ones retained",
  not_ranked: "beyond the number of candidates examined per repo — never opened",
};

/**
 * WHERE THE LAYER COMES FROM — the ingest's own report, read-only: the sheets RETAINED, the ones
 * DISCARDED with their reason, and the directories the walk refused to enter. This is what turns a
 * token into something one can trace back to a file, and a MISSING token into something one can
 * explain (finding `01KYSHSPGSPC98C49X68MWY60D`).
 */
function TokenProvenance({ layer }: { layer: TokenLayer }) {
  const { sources, skipped, skippedCount, prunedDirs, prunedDirsCount, provenanceReported } = layer;
  // NOTHING TO TRACE, nothing to say: a layer with neither a source nor a report was not derived
  // from anything (a theme typed by hand, or an empty one), so the whole provenance block is silence
  // rather than a row of zeroes. See the `tokens-provenance-unreported` branch below for the case
  // this deliberately excludes. The guard is NARROW, and stays so: an ingest that retained no sheet
  // now records its report all the same (finding `01KYSNVW31C0E8YQPET0505HW2`), so a layer with no
  // source but a report — the grey catalogue, where this block is the only diagnostic there is —
  // renders in full; what falls through here is a project no ingest ever ran on.
  if (sources.length === 0 && !provenanceReported) return null;
  return (
    <div
      data-testid="tokens-provenance"
      className="mb-2 space-y-1.5 border-b border-border/60 pb-2"
    >
      <p className="text-2xs font-medium text-foreground">Where the layer comes from</p>

      <div>
        <p className="text-2xs text-muted-foreground">Sheets retained ({sources.length})</p>
        {sources.length === 0 ? (
          <p className="text-2xs text-muted-foreground/80" data-testid="tokens-no-source">
            none — the layer was not derived from a sheet of the app (typed by hand, or nothing
            detected).
          </p>
        ) : (
          <ul>
            {/* The PATH, and nothing else. The sheet's own `variable_count` used to sit here: it
                counts what the ingest READ out of the file — light plus dark plus every `@theme
                inline` alias — while the list below shows the tokens the project HAS. So one panel
                stated 163, 103 and 48 for what a reader takes to be one notion, with nothing
                relating them (finding `01KZ3JYARFJ67T5N1NC22XK69C`). The count of tokens is stated
                ONCE, on the bar above; the provenance answers WHERE they come from. */}
            {sources.map((source) => (
              <li
                key={`${source.repo}/${source.path}`}
                data-testid="tokens-source-kept"
                className="truncate text-2xs text-foreground"
                title={`${source.repo}/${source.path}`}
              >
                <code className="font-mono">
                  {source.repo ? `${source.repo}/` : ""}
                  {source.path}
                </code>
              </li>
            ))}
          </ul>
        )}
      </div>

      {skipped.length > 0 ? (
        <div>
          <p className="text-2xs text-muted-foreground">Sheets discarded ({skippedCount})</p>
          <ul>
            {skipped.map((sheet) => (
              <li
                key={`${sheet.repo}/${sheet.path}`}
                data-testid="tokens-source-skipped"
                className="truncate text-2xs text-muted-foreground"
                title={SKIP_REASON_MEANING[sheet.reason] ?? sheet.reason}
              >
                <code className="font-mono">
                  {sheet.repo ? `${sheet.repo}/` : ""}
                  {sheet.path}
                </code>{" "}
                · <code className="font-mono">{sheet.reason}</code>
                {SKIP_REASON_MEANING[sheet.reason] ? ` — ${SKIP_REASON_MEANING[sheet.reason]}` : ""}
              </li>
            ))}
          </ul>
          {skippedCount > skipped.length ? (
            <p className="text-2xs text-muted-foreground/80" data-testid="tokens-skipped-more">
              … and {skippedCount - skipped.length} more, not detailed.
            </p>
          ) : null}
        </div>
      ) : null}

      {prunedDirs.length > 0 ? (
        <div>
          {/* The TOTAL, never the length of the head that fits: this list is capped exactly as the
              discarded sheets are, and showing the cap as the count is how a truncation becomes an
              under-declaration (finding `01KYSNRNY550NS778YCMC8CZGH`). */}
          <p className="text-2xs text-muted-foreground">
            Directories not descended into ({prunedDirsCount})
          </p>
          <p className="text-2xs text-muted-foreground" data-testid="tokens-pruned-dirs">
            {prunedDirs.map((dir) => (
              <code key={dir} className="mr-1 font-mono">
                {dir}
              </code>
            ))}
          </p>
          {prunedDirsCount > prunedDirs.length ? (
            <p className="text-2xs text-muted-foreground/80" data-testid="tokens-pruned-dirs-more">
              … and {prunedDirsCount - prunedDirs.length} more, not detailed.
            </p>
          ) : null}
        </div>
      ) : null}

      {!provenanceReported ? (
        /* HONEST GAP: "rien n'a été écarté" and "personne n'a noté ce qui a été écarté" are not the
           same statement, and presenting the second as the first is how a polluted layer passed for
           a healthy one. The report is produced by the agent's `design_tokens` ingest, which now
           STORES it in the layer (finding `01KYSK7GS9C3ZRJYW00ZA0H6GF`), so this is no longer what
           EVERY ingested layer looked like.
           WHAT REACHES THIS BRANCH is a layer that HAS sources and no report — a layer ingested
           before the report was persisted (the `LEGACY_INGESTED` case of the test); a run that
           retains NO sheet records its report too, so it does not land here either (finding
           `01KYSNVW31C0E8YQPET0505HW2`). A theme written
           BY HAND has neither sources nor report, so the guard at the top of this component returns
           null and this block is never rendered for it: a project that never ran an ingest is not
           told about a report it never asked for. The comment used to claim that case too
           (finding `01KYSNT38VZYJEAH4VEFJW8FHW`). */
        <p className="text-2xs text-muted-foreground/80" data-testid="tokens-provenance-unreported">
          Discarded sheets and pruned directories: not recorded by the last ingest (the report comes
          from the agent&apos;s <code className="font-mono">design_tokens</code> tool).
        </p>
      ) : null}
    </div>
  );
}

export function ThemeTokensPanel({ themeTokensJson, className }: ThemeTokensPanelProps) {
  const layer = useMemo(() => parseTokenLayer(themeTokensJson), [themeTokensJson]);
  const [open, setOpen] = useState(false);

  /**
   * THE TOKENS, ONE ROW EACH. An alias of another row of the same layer is dropped: it is the same
   * token under a second name, and 36 of the 103 rows were that (finding
   * `01KZ3JW5A63TJWX83V8ZKX62CN`). The layer still carries them — they are what defines the
   * `bg-secondary` utility — they are simply not a second thing to read.
   */
  const rows = useMemo(() => layer.variables.filter(([name]) => !layer.aliasOf.has(name)), [layer]);
  /**
   * How many of THOSE rows carry a dark variant. Counted over what is on screen, never over the raw
   * `dark` map: a count the reader cannot check against the list is exactly the certainty that was
   * false (finding `01KZ3JYARFJ67T5N1NC22XK69C`).
   */
  const darkCount = useMemo(() => {
    const dark = new Set(layer.dark.map(([name]) => name));
    return rows.filter(([name]) => dark.has(name)).length;
  }, [layer, rows]);

  const source = layer.sources[0] ?? null;
  const count = rows.length;

  return (
    <section
      className={cn("shrink-0 rounded-md border border-border/60 bg-card", className)}
      data-testid="theme-tokens-panel"
    >
      <div className="flex flex-wrap items-center gap-2 px-3 py-2">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
        >
          <PaletteIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
          <span className="text-xs font-medium text-foreground">Tokens</span>
          {/* THE SUMMARY — and only while the panel is CLOSED. Open, the very same sheet path and
              the very same discarded count are stated by the provenance block a few pixels below,
              word for word (finding `01KZ3JW5A63TJWX83V8ZKX62CN`). The bar keeps the ONE number
              that has no other home: how many tokens the project declares. */}
          <span className="truncate text-2xs text-muted-foreground">
            {count === 0 ? (
              /* EMPTY-STATE, visible WITHOUT opening the disclosure (finding
                 01KYKX7HBP4ZSSS6ZDM31T6A5E): the "Tokens depuis l'app" button is gone, so the
                 collapsed line itself has to name who fills the layer. */
              <span data-testid="tokens-empty-agent-driven">
                no token — the design-system classes will render grey; the agent ingests them from
                the app&apos;s own stylesheet
              </span>
            ) : (
              <>
                {count} tokens
                {darkCount > 0 ? ` · ${darkCount} with a dark variant` : ""}
                {!open && source ? ` · from ${source.repo}/${source.path}` : ""}
              </>
            )}
            {/* The POLLUTION SIGNAL, without opening anything (finding
                `01KYSHSPGSPC98C49X68MWY60D`): a layer merged next to a dozen discarded sheets is not
                the same object as one merged from a single clean stylesheet — and on an EMPTY layer
                it is the only diagnostic there is. */}
            {!open && layer.skippedCount > 0 ? (
              <span data-testid="tokens-skipped-summary">
                {" "}
                · {layer.skippedCount} sheet(s) discarded
              </span>
            ) : null}
          </span>
        </button>
      </div>

      {open ? (
        <div className="max-h-64 overflow-y-auto border-t border-border/60 px-3 py-2">
          {/* WHERE the layer comes from — before the tokens themselves: a value one cannot trace is
              a value one cannot trust, and an ABSENT token is explained here or nowhere. Rendered
              even for an empty layer: that is precisely the case where "which sheets were discarded,
              and which directories were never entered" is the answer. */}
          <TokenProvenance layer={layer} />
          {count === 0 ? (
            /* The long form of the same empty-state (the collapsed line above carries the short
               one): who fills the layer, and why an empty one shows grey previews. */
            <p className="text-2xs text-muted-foreground" data-testid="tokens-empty-detail">
              No token: without <code>--primary</code> &amp; co., <code>bg-primary</code> resolves
              to nothing and every preview stays grey. The agent ingests them from the app&apos;s
              own stylesheet — they show up here on their own.
            </p>
          ) : (
            /* READ-ONLY (finding `01KZ3JQXGSVZAC2MHHNZAS681Y`) — see the module doc. The value is
               shown, never offered for editing: the layer is a reading of the app's stylesheet. */
            <ul className="space-y-1">
              {rows.map(([name, value]) => (
                <li key={name} className="flex items-center gap-2" data-testid="token-row">
                  <Swatch layer={layer} value={value} />
                  <code className="w-48 shrink-0 truncate font-mono text-2xs text-muted-foreground">
                    {name}
                  </code>
                  <code
                    aria-label={name}
                    className="min-w-0 flex-1 truncate font-mono text-2xs text-foreground"
                    title={value}
                  >
                    {value}
                  </code>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}
    </section>
  );
}
