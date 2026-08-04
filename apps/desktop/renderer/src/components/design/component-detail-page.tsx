import { getRouteApi, Link } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { ChevronDownIcon, ZapIcon } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { Badge } from "@/components/ui/badge";
import { Breadcrumb, BreadcrumbCurrent, breadcrumbLinkClass } from "@/components/ui/breadcrumb";
import { EmptyState } from "@/components/ui/empty-state";
import { Spinner } from "@/components/ui/spinner";
import { CollapsibleSection } from "@/components/sidebar/collapsible-section";
import { sectionTransition } from "@/components/sidebar/section-motion";
import { useActiveProject } from "@/components/terminals/active-project";
import { cn } from "@/lib/utils";

import {
  activeAxisValues,
  axisDefaults,
  conditionsOnUnknownAxes,
  inertAxisValues,
  parseAxes,
  primaryAxisStates,
  statesWithoutCase,
  templateConditions,
} from "./axes";
import { AxisField } from "./axis-field";
import { CompositionFrame } from "./composition-frame";
import { CssSlotCard, cssSlotOfComponent } from "./css-slot-card";
import { ParamField } from "./param-field";
import { previewState } from "./preview-state";
import { themeHasDark } from "./theme-css";
import {
  editableParams,
  inertBooleanParams,
  parseParams,
  paramsShadowingAxis,
  paramsWithDeprecatedStateType,
  previewParamValues,
  unsubstitutedParams,
} from "./params";
import { componentNature } from "./types";
import { useComponent } from "./use-component";
import { useDesign } from "./use-design";
import { useDesignRender, type RenderedCssSlot } from "./use-design-render";

/**
 * `ComponentDetailPage` (`/design/$componentId`) — the composant detail (v2.7 Phase 3, task #11).
 * It shows ONE component and nothing else: its REPRESENTATIVE preview (the `preview` values), its
 * axes of variance and its params, live. It is reached from a catalogue card, and it is a page one
 * opens to JUDGE a component — never to fix one of its placements.
 *
 * THE PAGE SPEAKS OF THE COMPONENT ALONE (finding `01KYT10VE8624MKQ18AFHSE0W0`). It used to carry a
 * « USED IN PROTOTYPES » list — every composition NODE instantiating the component, each selectable,
 * pre-parameterized and SAVEABLE back into its proto (`proto_variant_set_node_params`). Two notions
 * shared one page: the catalogue's vocabulary and the protos' placements, one of them persisted and
 * the other not, with a single form and a single frame standing for both.
 *
 * That capability is not gone — it MOVED, by the user's own arbitration, to the surface where one
 * already looks at the composition: the Spec's PROTOTYPE tab
 * ([`../specs/tabs/node-params`]). Everything followed it: selecting a node, its OWN params (the
 * node's `default` base — never this page's `preview` examples), the contract notices at the READ
 * and at the WRITE, the per-node Save, the statement of what is rendered, and the deep-link from the
 * elected composition (with its "usage introuvable" verdict, which still refuses to fall back
 * silently onto an aperçu). What is left here has ONE meaning, so the `Showing` cartouche has one
 * thing to name: the representative preview.
 *
 * ONE CONTROL PER AXIS, IN THE PANEL (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`). What makes a component
 * vary is always an AXIS — `variant` is one, `size` is one, and a boolean `loading` is one with two
 * values — so the panel renders an [`AxisField`] per declared axis, above the params. Picking
 * `size = lg` leaves `variant` alone and the frame re-renders the COMBINATION: orthogonal axes, no
 * cartesian product, no single "state" dropdown standing in for all of them.
 *
 * The axes' active values live in the SAME `values` map as the params, keyed by axis name. That is
 * not a shortcut: it is what keeps the whole existing chain intact — a template substituting
 * `{{state}}` still resolves, and the node editor in the prototype tab persists the very same map
 * onto a node, exactly as it persisted the `state` param.
 *
 * THE PANEL IS THE ONLY CONTROL (finding `01KYPJ1SCHGZ2STSXZ4VCR1K33`), and the frame is a VIEW:
 *
 *   * the PANEL wins, because it is the face the whole chain is seeded from and read through; a chip
 *     lives inside a sandboxed frame whose `srcdoc` is rebuilt on the next keystroke, so a flip made
 *     there would be silently discarded.
 *   * the FRAME renders what the panel says: `axes` + `axisValues` (and `activeState` for the first
 *     axis) is what finally reaches the shim, which applies each axis's classes and filters the
 *     structural cases.
 *   * the frame's own GESTURE (a `data-nyx-state` / `data-nyx-set` click — the green affordance
 *     the detail keeps, unlike the vignette) is not a second control: it is fed BACK into the values
 *     through `onAxisChange`, so the two faces can never diverge.
 *
 * AND THE EXCEPTION IS READABLE WHERE IT BELONGS (finding `01KYW342EE5DN6F70YB8VSTJR8`, point 7).
 * A component may carry ONE strict scoped CSS slot. It was visible NOWHERE from this page: the
 * rendering showed a dashed outline the reader could attach to nothing, and the only card that
 * stated the scope, the CSS and the lint lived beside a PLACEMENT, in the prototype. The slot is
 * carried by the COMPONENT — a node only points at it — so the reading moves here, onto the page of
 * the thing that owns it: a [`CssSlotSection`], CLOSED by default and animated open, holding the
 * very same [`CssSlotCard`] (no second card, no second vocabulary). A component carrying no slot
 * shows nothing at all.
 */

const routeApi = getRouteApi("/design/$componentId");

/** The panel's section titles, and the preview pane's caption — one typographic level for all. */
const SECTION_HEADING = "text-2xs font-semibold uppercase tracking-wide text-muted-foreground/70";

/**
 * `<CssSlotSection>` — the component's scoped CSS slot, FOLDED (finding
 * `01KYW342EE5DN6F70YB8VSTJR8`, point 7). Closed by default and animated, because it is an
 * EXCEPTION: it must be findable at a glance and must not shove the controls down the panel on
 * every visit. Only the head is permanent, and the head already NAMES the state — `exception` for
 * a slot that applies, `refused` for a stored one that no longer compiles — so a refusal is
 * announced before anything is unfolded and can never read as an absence.
 *
 * The body is the shared [`CssSlotCard`] on its `component` surface: same scope, same compiled CSS,
 * same reason, same lint, and NO edit control — a slot is authored in the catalogue by the agent
 * (`design_author_component`); this page reads it.
 */
function CssSlotSection({ slot }: { slot: RenderedCssSlot }) {
  const [open, setOpen] = useState(false);
  const reduced = useReducedMotion();
  const refused = slot.refused !== null;

  return (
    <section data-testid="component-css-slot">
      <button
        type="button"
        data-testid="component-css-slot-toggle"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className={cn(
          "flex w-full items-center gap-1.5 rounded-md border px-2 py-1.5 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50",
          refused
            ? "border-destructive/40 bg-destructive/[0.06] hover:bg-destructive/10"
            : "border-warning/40 bg-warning/[0.06] hover:bg-warning/10",
        )}
      >
        <motion.span
          aria-hidden
          animate={{ rotate: open ? 0 : -90 }}
          transition={sectionTransition(reduced)}
          className="flex items-center text-muted-foreground"
        >
          <ChevronDownIcon className="size-3.5" />
        </motion.span>
        <ZapIcon
          aria-hidden
          className={cn("size-3.5", refused ? "text-destructive" : "text-warning")}
        />
        <span
          className={cn(
            "flex-1 text-xs font-medium",
            refused ? "text-destructive" : "text-warning",
          )}
        >
          CSS slot
        </span>
        <Badge variant={refused ? "destructive" : "warning"} className="shrink-0">
          {refused ? "refused" : "exception"}
        </Badge>
      </button>
      {/* Animated reveal — the SHARED `<CollapsibleSection>` (height+opacity spring, honouring
          `prefers-reduced-motion`), so this fold behaves like every other fold in nyx. */}
      <CollapsibleSection open={open} className="pt-1.5">
        <CssSlotCard slot={slot} surface="component" />
      </CollapsibleSection>
    </section>
  );
}

export function ComponentDetailPage() {
  const { componentId } = routeApi.useParams();
  const { activeProjectId } = useActiveProject();
  const { component, loading, error } = useComponent(componentId);
  const { design } = useDesign(activeProjectId);
  const themeTokensJson = design?.theme_tokens_json ?? null;
  /**
   * The preview follows nyx's appearance by selecting the DESIGN SYSTEM'S dark variant — when the
   * design system has one (finding `01KYSVEZMJP1Z8V7SYJ8YC5H9W`). When it does NOT, the frame
   * renders the only variant that exists, the light one, and this page SAYS it: a preview that
   * contrasts with the app is a fact about the project's tokens, not a rendering defect, and the
   * user is owed the difference. Silence here is what let "l'aperçu est blanc" be reported three
   * times.
   */
  const layerHasDark = useMemo(() => themeHasDark(themeTokensJson), [themeTokensJson]);

  const axes = useMemo(() => parseAxes(component?.states_json), [component?.states_json]);
  const states = useMemo(() => primaryAxisStates(axes), [axes]);
  const descriptors = useMemo(
    () => parseParams(component?.params_json, axes),
    [component?.params_json, axes],
  );
  // The form offers a control only for what the TEMPLATE actually reads (finding
  // `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point A) — the rest is named once, below, as the synthesis defect
  // it is.
  const fields = useMemo(
    () => editableParams(descriptors, component?.template),
    [descriptors, component?.template],
  );
  const deadParams = useMemo(
    () => unsubstitutedParams(descriptors, component?.template),
    [descriptors, component?.template],
  );

  /**
   * WHAT THE VARIANCE CONTROLS CAN ACTUALLY DO HERE (findings `01KYPSG1HM2GPJMNH2SP3DDADH`, then
   * `01KYQ0R84KVF6QVQXQ9YVG9Z8T`). An axis value changes the rendering in exactly two ways: its
   * CLASSES (the shim puts them on the target) or a STRUCTURAL case (`data-nyx-state-case` for the
   * first axis, `data-nyx-if` for any). Everything the synthesis can get wrong is named here
   * rather than left to be clicked into the void:
   *   * INERT VALUES — neither classes nor case: selecting them cannot change one pixel. That is the
   *     original "les dropdowns de variante sont cosmétiques", and it now also covers the DECLARED
   *     BOOLEAN that has no home (a two-valued axis with nothing on either value).
   *   * PARTIAL CASES — the first axis renders SOME `data-nyx-state-case` and not others. Worse
   *     than inert: selecting a case-less value hides every other case, so the aperçu goes EMPTY.
   *   * DOUBLE DECLARATION — the same notion as a param AND as an axis, or a param still typed
   *     `state`: two controls over one thing, one of which lies.
   *   * INERT BOOLEAN PARAM — a `boolean` param is substituted as the text `true`/`false`: it can
   *     never carry classes nor gate an element. It has to become an axis.
   *   * DANGLING CONDITION — a `data-nyx-if` naming an axis the component does not declare; the
   *     shim ignores it (a hidden element would blank the preview), so it is reported instead.
   * nyx never invents the missing classes: only the agent that read the source knows them. The write
   * path warns about the same things at the moment the synthesis is stored.
   */
  const missingCases = useMemo(
    () => statesWithoutCase(component?.template, states),
    [component?.template, states],
  );
  // The two lists are DISJOINT on purpose: a first-axis value the template renders no case for is
  // reported ONCE, by `missingCases` — which says the sharper thing (the aperçu goes EMPTY, not just
  // "nothing changes"). Everything else that can change nothing lands here.
  const inertValues = useMemo(
    () =>
      inertAxisValues(axes, component?.template).filter(
        (iv) => !(iv.axis === axes[0]?.name && missingCases.includes(iv.value)),
      ),
    [axes, component?.template, missingCases],
  );
  const shadowedParams = useMemo(() => paramsShadowingAxis(descriptors), [descriptors]);
  const deprecatedStateParams = useMemo(
    () => paramsWithDeprecatedStateType(descriptors),
    [descriptors],
  );
  const inertBooleans = useMemo(
    () =>
      inertBooleanParams(
        descriptors,
        axes,
        templateConditions(component?.template).map((c) => c.axis),
      ),
    [descriptors, axes, component?.template],
  );
  const unknownConditionAxes = useMemo(
    () => conditionsOnUnknownAxes(axes, component?.template),
    [axes, component?.template],
  );

  /**
   * The current values of the REPRESENTATIVE preview — the component's `preview > default > rien`
   * base (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`), so a `button` whose label is mandatory shows an
   * example label instead of an empty pill. Nothing here is persisted: an example value must never be
   * able to reach a composition, and the surface that writes to one (the prototype tab's node editor)
   * seeds itself from the node's DEFAULTS instead, precisely for that reason.
   */
  const [values, setValues] = useState<Record<string, unknown>>({});

  // (Re)seed the preview when the component loads. The AXES' default combination is part of that map
  // (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`): the aperçu opens on one value per axis, and an axis
  // always wins over a param of the same name.
  useEffect(() => {
    if (!component) return;
    setValues({ ...previewParamValues(descriptors), ...axisDefaults(axes) });
    // Re-seed only when the component identity or its descriptor/axis set changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [component?.id, descriptors, axes]);

  // IDEMPOTENT: an unchanged value keeps the SAME `values` object. The frame echoes its whole
  // combination back on every apply (`onAxisChange`), and a new object there would rebuild the
  // composition, the srcdoc and the nonce on a loop.
  const setParam = useCallback((name: string, value: unknown) => {
    setValues((prev) => (Object.is(prev[name], value) ? prev : { ...prev, [name]: value }));
  }, []);

  // The ACTIVE COMBINATION — one value per axis, coerced to something the axis really declares. It
  // is what the frame renders.
  const combination = useMemo(() => activeAxisValues(axes, values), [axes, values]);
  // …and its FIRST axis, which the legacy half of the chain still speaks (`data-nyx-state-case`,
  // `window.__nyxActiveState`).
  const activeState = axes.length > 0 ? (combination[axes[0]!.name] ?? null) : null;

  // The frame's own gestures, folded back INTO the values — never a second source of truth. Only a
  // DECLARED axis value is accepted: the frame is sandboxed, its messages are input.
  const onFrameAxisChange = useCallback(
    (next: Record<string, string>) => {
      setValues((prev) => {
        let changed = false;
        const merged = { ...prev };
        for (const axis of axes) {
          const value = next[axis.name];
          if (typeof value !== "string") continue;
          if (!axis.values.some((v) => v.name === value)) continue;
          if (Object.is(prev[axis.name], value)) continue;
          merged[axis.name] = value;
          changed = true;
        }
        return changed ? merged : prev;
      });
    },
    [axes],
  );

  /**
   * IS THERE ANYTHING TO PREVIEW AT ALL (finding `01KYSCW8RY8JKAR50C42QQ0B62`)? A component
   * DISCOVERED by the inventory but not yet SYNTHESIZED has an EMPTY template (review #40: nyx
   * catalogues the bundles, the agent writes the template), and rendering nothing is not a preview —
   * it is an empty document whose canvas is painted with the project's `--background`, i.e. the large
   * white rectangle a light design system produces, indistinguishable from a broken render.
   *
   * The catalogue VIGNETTE has always short-circuited that case; the detail did not. It does now, in
   * the same way and for the same two reasons: nothing to show, and no `design_render` to spend on a
   * result that would be thrown away.
   */
  const synthesized = (component?.template ?? "").trim() !== "";

  // The live composition = a single instance of the component with the current values — and NOTHING
  // for an unsynthesized one (`null` disables the render hook: no `design_render` is ever issued).
  const composition = useMemo(
    () =>
      component && synthesized
        ? JSON.stringify({ component: component.name, params: values })
        : null,
    [component, synthesized, values],
  );
  const {
    markup,
    loading: renderLoading,
    error: renderError,
    cssSlots,
    // THE AXES THE PANEL PILOTS, named to the render as the SHIM's (finding
    // `01KZ41YK39SGNKF6XV87A35GPN`). Here the composition is ONE node — this very component — so
    // its axes and the shim's are the same set, and the sidecar must bake none of them: this
    // preview is LIVE, and a baked value survives every flip the shim performs.
  } = useDesignRender(activeProjectId, composition, null, component?.states_json ?? null);

  /**
   * WHAT THE PREVIEW PANE SHOWS — the TOTAL function, not a ladder of this page's own (finding
   * `01KZ3JPW0RFM7AZ0AG2YASKBNR`'s family). `activeProjectId` comes from the active-project store
   * and CAN be null; `useDesignRender` then short-circuits to `markup: null, loading: false,
   * error: null`, which the old ladder — `renderError ? … : markup != null ? frame : <Spinner/>` —
   * read as "still loading". The pane spun for ever, exactly as the prototype vignettes did.
   *
   * `hasVariant` is the SYNTHESIS here: a component with no template has nothing to preview, and
   * that case must keep mounting NO frame at all (finding `01KYSCW8RY8JKAR50C42QQ0B62`) — which is
   * precisely `previewState`'s `no-variant`, checked before anything else.
   */
  const preview = previewState({
    hasVariant: synthesized,
    projectId: activeProjectId,
    compositionJson: composition,
    renderLoading,
    renderError,
    markup,
  });

  /**
   * THE COMPONENT'S OWN SCOPED CSS SLOT, if it carries one (finding
   * `01KYW342EE5DN6F70YB8VSTJR8`, point 7). It arrives WITH the render — the sidecar compiles and
   * scopes it, the renderer only displays it — and it is keyed on the COMPONENT name, the thing a
   * slot belongs to (`cssSlotOfComponent`, the same resolution the placement surface uses).
   *
   * A composition of ONE instance can meet at most this component's slot, so the list is 0 or 1
   * entry; a refused one is IN it, with `css` empty, and stays visible as a named refusal.
   */
  const cssSlot = useMemo(
    () => cssSlotOfComponent(cssSlots, component?.name),
    [cssSlots, component?.name],
  );

  if (loading) {
    return (
      <div className="flex h-svh items-center justify-center bg-background">
        <Spinner className="text-muted-foreground/50" />
      </div>
    );
  }
  if (error || !component) {
    return (
      <div className="flex h-svh flex-col bg-background px-6 pb-6 pt-10">
        <Breadcrumb className="mb-2">
          <Link to="/design" className={breadcrumbLinkClass}>
            Catalogue
          </Link>
          <BreadcrumbCurrent>Component</BreadcrumbCurrent>
        </Breadcrumb>
        <EmptyState>This component could not be found.</EmptyState>
      </div>
    );
  }

  const nature = componentNature(component);
  return (
    <div className="flex h-svh flex-col bg-background px-6 pb-6 pt-10">
      {/* THE NAME, ONCE (finding `01KZ3JW5A63TJWX83V8ZKX62CN`). It used to sit in the breadcrumb,
          in the title and in the preview's caption — three times inside 200 px, on a page whose
          whole subject is that one component. The TITLE keeps it; the breadcrumb is reduced to what
          it is actually for here, the way back to the catalogue; the caption names what it shows
          without re-naming the page. And only the EXCEPTION is badged: `DS` is the norm. */}
      <header className="mb-4 shrink-0">
        <Breadcrumb className="mb-2">
          <Link to="/design" className={breadcrumbLinkClass}>
            Catalogue
          </Link>
        </Breadcrumb>
        <div className="flex items-center gap-2">
          <h1 className="font-heading text-xl font-semibold tracking-tight text-foreground">
            {component.name}
          </h1>
          {nature === "new" ? <Badge variant="warning">NEW</Badge> : null}
        </div>
      </header>

      <div className="grid min-h-0 flex-1 grid-cols-[minmax(18rem,22rem)_1fr] gap-4">
        {/* Left: the component's OWN controls — its axes, then its params. Nothing about a
            composition lives here any more (finding `01KYT10VE8624MKQ18AFHSE0W0`): the usages list,
            its per-node form and its Save moved to the Spec's prototype tab, where the composition
            they belong to is on screen. */}
        <div className="flex min-h-0 flex-col gap-4 overflow-y-auto pr-1">
          {/* THE EXCEPTION FIRST, FOLDED (finding `01KYW342EE5DN6F70YB8VSTJR8`, point 7). A scoped
              CSS slot is what this component does OUTSIDE the utilities, and it exists for almost
              none of them — so it costs nothing when absent, and when present it is the first thing
              the panel names rather than something the reader must scroll for. Closed by default:
              the controls below stay where they are, and the head alone already says whether the
              slot applies or was refused. */}
          {cssSlot !== null ? <CssSlotSection slot={cssSlot} /> : null}

          {/* ONE CONTROL PER AXIS (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`) — above the params,
              because an axis is what the component IS, and a param is what it says. */}
          {axes.length > 0 ? (
            <section>
              <h2 className={cn(SECTION_HEADING, "mb-1.5")}>Axes</h2>
              {/* A control is only as real as what the synthesis put behind each value — say so
                  rather than let the user flip a picker that changes nothing. One line, no chrome,
                  and it never widens the panel. */}
              {inertValues.length > 0 ? (
                <p
                  data-testid="state-cases-missing"
                  className="mb-2 rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-1.5 text-2xs leading-relaxed text-destructive/90"
                >
                  {inertValues.length} axis value
                  {inertValues.length > 1 ? "s carry" : " carries"} neither{" "}
                  <code className="font-mono">classes</code> nor a structural case (
                  <code className="font-mono">data-nyx-if</code> /{" "}
                  <code className="font-mono">data-nyx-state-case</code>):{" "}
                  {inertValues
                    .map((v) => (axes.length > 1 ? `${v.axis}=${v.value}` : v.value))
                    .join(", ")}{" "}
                  — selecting them cannot change the rendering. The synthesis must give each value
                  its resolved classes.
                </p>
              ) : null}
              {missingCases.length > 0 ? (
                <p
                  data-testid="axis-cases-missing"
                  className="mb-2 rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-1.5 text-2xs leading-relaxed text-destructive/90"
                >
                  No <code className="font-mono">data-nyx-state-case</code> was synthesized for:{" "}
                  {missingCases.join(", ")} — selecting one of them renders nothing.
                </p>
              ) : null}
              {unknownConditionAxes.length > 0 ? (
                <p
                  data-testid="axis-unknown-condition"
                  className="mb-2 rounded-md border border-warning/30 bg-warning/5 px-2.5 py-1.5 text-2xs leading-relaxed text-warning"
                >
                  The template conditions on {unknownConditionAxes.map((a) => `'${a}'`).join(", ")},
                  which is not a declared axis — the condition is ignored. Declare the axis in{" "}
                  <code className="font-mono">states_json</code> or drop the condition.
                </p>
              ) : null}
              <div className="space-y-3">
                {axes.map((axis) => (
                  <AxisField
                    key={axis.name}
                    axis={axis}
                    value={combination[axis.name] ?? axis.defaultValue}
                    onChange={(v) => setParam(axis.name, v)}
                    inertValues={inertValues
                      .filter((iv) => iv.axis === axis.name)
                      .map((iv) => iv.value)}
                  />
                ))}
              </div>
            </section>
          ) : null}

          <section>
            <h2 className={cn(SECTION_HEADING, "mb-1.5")}>Params</h2>
            {/* THE RULE, enforced where it is broken: an axis of variance goes in the axes, a
                substituted value goes in the params. Each of these names the field and the fix. */}
            {shadowedParams.length > 0 ? (
              <p
                data-testid="param-shadows-axis"
                className="mb-2 rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-1.5 text-2xs leading-relaxed text-destructive/90"
              >
                Declared BOTH as a param and as an axis: {shadowedParams.join(", ")} — one notion,
                two controls. The axis is the real one (a param is substituted as a value; an axis
                value is a whole set of classes); drop{" "}
                {shadowedParams.length > 1 ? "these entries" : "this entry"} from{" "}
                <code className="font-mono">params_json</code>.
              </p>
            ) : null}
            {deprecatedStateParams.length > 0 ? (
              <p
                data-testid="param-state-type"
                className="mb-2 rounded-md border border-warning/30 bg-warning/5 px-2.5 py-1.5 text-2xs leading-relaxed text-warning"
              >
                {deprecatedStateParams.join(", ")} still declares the deprecated type{" "}
                <code className="font-mono">state</code> — a variance axis is never a param. Declare
                it in <code className="font-mono">states_json</code> as an axis; nyx renders one
                control per axis on its own.
              </p>
            ) : null}
            {inertBooleans.length > 0 ? (
              <p
                data-testid="param-boolean-inert"
                className="mb-2 rounded-md border border-warning/30 bg-warning/5 px-2.5 py-1.5 text-2xs leading-relaxed text-warning"
              >
                {inertBooleans.join(", ")}: a <code className="font-mono">boolean</code> param is
                substituted as the text true/false — it carries no classes and gates no element, so
                the toggle is decorative. Declare it as a TWO-VALUE AXIS in{" "}
                <code className="font-mono">states_json</code> instead.
              </p>
            ) : null}
            {/* DECLARED, AND WIRED NOWHERE (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point A). The
                template neither substitutes the name nor gates an element on it, so a control for
                it could not change one pixel — `class` was declared by 9 components of a real
                catalogue and read by 1. No control is rendered for these; the declaration is named
                here, where the correction is (the synthesis, i.e. the agent). */}
            {deadParams.length > 0 ? (
              <p
                data-testid="param-unsubstituted"
                className="mb-2 rounded-md border border-warning/30 bg-warning/5 px-2.5 py-1.5 text-2xs leading-relaxed text-warning"
              >
                Declared but read nowhere by the template: {deadParams.join(", ")} — no value of{" "}
                {deadParams.length > 1 ? "these" : "this one"} could change the rendering, so no
                field is offered. The synthesis must substitute{" "}
                {deadParams.length > 1 ? "them" : "it"} or stop declaring{" "}
                {deadParams.length > 1 ? "them" : "it"}.
              </p>
            ) : null}
            {fields.length === 0 && deadParams.length === 0 ? (
              <EmptyState variant="plain">This component declares no params.</EmptyState>
            ) : fields.length === 0 ? null : (
              <div className="space-y-3">
                {fields.map((d) => (
                  <ParamField
                    key={d.name}
                    descriptor={d}
                    value={values[d.name]}
                    onChange={(v) => setParam(d.name, v)}
                  />
                ))}
              </div>
            )}
          </section>
        </div>

        {/* Right: the live preview (re-renders on every param edit), CAPTIONED with what it renders.
            The caption used to arbitrate between two things — the component alone, or one of its
            placements; now that the placements have their own surface (finding
            `01KYT10VE8624MKQ18AFHSE0W0`) it has ONE thing to name, and it still names it: an empty
            frame here is a fact about the COMPONENT, never about somebody's composition. The caption
            is OUTSIDE the pane on purpose: the frame still fills the pane edge to edge.

            THE VIGNETTE'S PRESENTATION, finally extended here (finding
            `01KYSA1N8SDT012R6E9XFXFJB0` — the vignette fix `01KYPEG16E95WADT2VMCYBPCK5` had put the
            detail explicitly OUT of scope, so this pane kept the two defects the card lost: a hard
            `bg-white` plate inside a dark app, and no layout at all, which glued the instance to
            the top-left corner of a full-height pane).

            The surface is now an app-themed neutral — it is only what shows THROUGH the frame when
            the project declares no `--background` token, since the frame paints its own canvas from
            the design system's own token — and the instance is CENTRED inside it. */}
        {/* `min-w-0` — the guard WITHOUT WHICH THE PREVIEW LEAVES THE WINDOW (finding
            `01KYSVFKJTYF7NY5BNGMCJX4JE`). This is the `1fr` grid cell: with `min-width:auto` its
            automatic minimum size is its MIN-CONTENT, and the caption below is `truncate`
            (`white-space:nowrap`), so a long caption becomes the FLOOR of the track. Measured in
            Chromium at a 1560 px viewport: the track blew from 1084 px to 1534.8 px, the pane
            ran 387 px PAST the window's right edge, and the instance — correctly centred in a frame
            wider than what is visible — landed 193 px right of the visible middle while staying
            vertically centred. Exactly the reported picture.
            And nothing said so: `globals.css` clips the document (`html,body,#root{overflow-x:clip}`),
            which suppresses the scrollbar AND clamps `scrollWidth`, so the overflow left no trace at
            all. Same guard, same reason, as the shell's own outlet cell (`base-layout.tsx`): the
            cell follows the TRACK, not its content. The caption names the COMPONENT, and a component
            name is as unbounded as the `node_key` that used to be here. */}
        <div className="flex min-h-0 min-w-0 flex-col gap-1.5">
          <div
            data-testid="preview-scope"
            className="flex shrink-0 items-baseline gap-2 overflow-hidden"
          >
            <h2 className={cn(SECTION_HEADING, "shrink-0")}>Showing</h2>
            {/* `min-w-0` so the caption ELLIPSES instead of pushing itself out of the column — the
                pane's width is the preview's, and nothing here may steal from it. The component's
                name is NOT repeated here: the page's title, two rows up, is that name. */}
            <p className="min-w-0 truncate text-2xs text-muted-foreground">
              the component alone, on its preview values
            </p>
            {/* WHAT VARIANT OF THE DESIGN SYSTEM IS ON SCREEN (finding
                `01KYSVEZMJP1Z8V7SYJ8YC5H9W`). The preview follows the app: nyx is dark, so the frame
                selects the project's OWN `.dark` overrides. A project whose layer has none has only
                a light variant to render — that is a fact about its tokens, and rendering it in
                silence is what made a light aperçu inside a dark app read as a bug three times over.
                So the caption names it, and names the cure, exactly where the rendering is claimed.
                It is a MARKER, not a control: nothing here switches anything. `shrink-0` keeps it out
                of the caption's ellipsis. */}
            {!layerHasDark && synthesized ? (
              <span
                data-testid="theme-variant-notice"
                title="This project's design system declares the LIGHT theme only: its token layer carries no dark override (`dark`). The preview therefore renders the single variant that exists — the light one — rather than a dark one nyx would have invented. For the preview to follow nyx's appearance, the project must declare its dark tokens (they are re-ingested by design_tokens)."
                className="shrink-0 rounded border border-warning/30 bg-warning/5 px-1 py-px text-2xs font-medium uppercase tracking-wide text-warning"
              >
                light variant
              </span>
            ) : null}
          </div>
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border border-border/60 bg-muted/30">
            {preview.kind === "no-variant" ? (
              /* NOT SYNTHESIZED → NO PREVIEW AT ALL (finding `01KYSCW8RY8JKAR50C42QQ0B62`).
                 Mounting the frame on an empty template produced a big EMPTY document, painted with
                 the project's own `--background` — a white rectangle for a light design system,
                 which reads as a broken render. The honest surface is an empty-state that says what
                 is missing and WHO produces it: the synthesis is the agent's work, not a gesture
                 the user is missing here (same spirit as the catalogue's own empty-state, finding
                 `01KYKX7HBP4ZSSS6ZDM31T6A5E`). The vignette already did exactly this. */
              <div className="flex h-full items-center justify-center p-6">
                <EmptyState className="max-w-md">
                  {/* SHORT, and in the reader's words (finding `01KZ3JW5A63TJWX83V8ZKX62CN`): four
                      lines naming an internal tool, an empty template and a stored row, to conclude
                      that there was nothing for the reader to do. Two facts survive — nothing to
                      show, and it is not yours to fix. */}
                  <div data-testid="detail-not-synthesized" className="space-y-1.5">
                    <p className="text-sm font-medium text-foreground">Not described yet</p>
                    <p className="leading-relaxed">
                      This component was found in the project, but nobody has described how it
                      renders — so there is nothing to preview. The agent does that; the preview
                      appears here on its own.
                    </p>
                  </div>
                </EmptyState>
              </div>
            ) : preview.kind === "error" || preview.kind === "unavailable" ? (
              /* A DEAD END, IN FULL. This pane has room for the sentence the vignette can only put
                 in a tooltip — and `unavailable` is the one the hand-written ladder could not even
                 reach: `useDesignRender` short-circuits on a null project, so its final `else` was
                 an eternal spinner (finding `01KZ3JPW0RFM7AZ0AG2YASKBNR`'s family). */
              <div
                data-testid={`detail-preview-${preview.kind}`}
                className={cn(
                  "flex h-full items-center justify-center p-4 text-center text-xs",
                  preview.kind === "error" ? "text-destructive/80" : "text-muted-foreground",
                )}
              >
                {preview.message}
              </div>
            ) : preview.kind === "empty" ? (
              <div
                data-testid="detail-preview-empty"
                className="flex h-full items-center justify-center p-4 text-center text-xs text-muted-foreground"
              >
                This component rendered nothing at all.
              </div>
            ) : preview.kind === "ready" ? (
              <div className="min-h-0 flex-1 overflow-hidden">
                <CompositionFrame
                  markup={preview.markup}
                  themeTokensJson={themeTokensJson}
                  states={states}
                  // THE chaining that makes the variance controls work: the panel's AXIS values →
                  // here → `buildFrameSrcdoc` → `window.__nyxAxes` / `__nyxAxisValues` → the
                  // shim's applier (classes per axis + the structural cases). `activeState` carries
                  // the FIRST axis for the legacy half of the chain.
                  axes={axes}
                  axisValues={combination}
                  activeState={activeState}
                  onAxisChange={onFrameAxisChange}
                  // …and NO chip-bar: the param panel is the one control (see the module doc). The
                  // rest of the chrome — the dashed `data-nyx-*` affordances — stays, this is a
                  // live interactive preview.
                  showChipBar={false}
                  // CENTRED, with the very prop the vignette uses (`center` — nothing new): the
                  // body lays out with `safe center`, so a button sits in the MIDDLE of the pane
                  // instead of in its top-left corner, while a FULL-PAGE component (a `dialog`
                  // taller/wider than the pane) is start-aligned rather than cropped on both
                  // sides — it still reads from its top-left, as a page does.
                  //
                  // Centring needs a DEFINITE box, so the frame FILLS the pane (`h-full`) instead
                  // of auto-sizing to its content: `autoHeight` made the box exactly as tall as the
                  // instance, which is precisely what left it glued to the top of the empty pane.
                  center
                  className="h-full"
                  title={`${component.name} live preview`}
                />
              </div>
            ) : (
              <div className="flex h-full items-center justify-center">
                <Spinner className="text-muted-foreground/50" />
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
