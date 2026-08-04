//! A component template REFERENCING another catalogue component — `data-nyx-use` — resolved
//! ENTIRELY in the sidecar, at render time (v2.7, finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`).
//!
//! # The problem it removes
//!
//! A synthesized template was SELF-CONTAINED by decree, so a sub-component shared by several hosts
//! (a spinner, an icon, a badge) had to be INLINED into each of them and was then DUPLICATED: fixing
//! the spinner in `Button` fixed it nowhere else, and nothing said which hosts still carried the old
//! copy. Drift, guaranteed, by construction.
//!
//! # The mechanism, in one line of HTML
//!
//! ```html
//! <button class="inline-flex items-center rounded-md">
//!   <span data-nyx-if="loading=on" data-nyx-use="Spinner" data-nyx-use-axes="size=sm"
//!         data-nyx-param-label="{{label}}"></span>{{label}}</button>
//! ```
//!
//!   * `data-nyx-use="<Component>"` — the element is a PLACEHOLDER: its content is replaced by the
//!     referenced component's rendering. The element itself is the HOST's, and everything else it
//!     carries stays the host's — its classes, and its own `data-nyx-if` (which is how a `Button`
//!     shows its spinner only at `loading=on`, with the mechanism that already existed).
//!   * `data-nyx-param-<name>="…"` — ONE attribute per param handed to the sub-component. The host
//!     template is rendered by Handlebars BEFORE this pass runs, so `data-nyx-param-label="{{label}}"`
//!     FORWARDS the host's own param, with no new grammar and no logic.
//!   * `data-nyx-use-axes="size=sm, variant=ghost"` — the sub-component's OWN axes, FIXED here (the
//!     `data-nyx-if` grammar, reused verbatim). Omitted axes render at their `default`, else at their
//!     first value.
//!   * the placeholder's existing CONTENT, if any, is handed to the sub-component as `{{{children}}}`
//!     — the same slot semantics a composition node has.
//!
//! # What comes OUT, and why the Electron renderer learns NOTHING
//!
//! An expansion is FLATTENED and INERT: the sub-component's axes are resolved HERE into concrete
//! classes, its `data-nyx-if` / `data-nyx-state-case` are evaluated HERE (a hidden element gets the
//! `hidden` ATTRIBUTE — byte for byte what the shim's own `setHidden` does, and the srcdoc's
//! `[hidden]{display:none!important}` rule already turns that into an absence), and EVERY
//! `data-nyx-*` attribute is then STRIPPED from it. The markup that leaves this module therefore
//! carries the marks of the HOST and of nobody else — so the shim, which knows exactly ONE axis
//! vocabulary (the node's component's), can never meet a second one and mistake it for the first.
//! No shim change, no srcdoc change, no CSP change, no node-contract change.
//!
//! # Why the sub-component does NOT inherit the host's axes
//!
//! An axis NAME is local to a component: `Button.size ∈ {md, lg}` and `Spinner.size ∈ {sm, md}` are
//! two vocabularies, with different values and different classes. Inheriting by name would map one
//! onto the other silently — the very thing the node contract refuses to do with params ("nyx never
//! maps one name onto a neighbouring one"). And what makes an axis LIVE in the frame is the shim's
//! applier, which pilots ONE component's axes; a live sub-axis would mean a second vocabulary per
//! subtree and a second control over one notion. So the host's author FIXES the values at the
//! reference site — and when the sub-component really must follow the host, the host says so with
//! the mechanism it already has: two gated references
//! (`data-nyx-if="size=lg" … data-nyx-use-axes="size=md"`).
//!
//! # The refusals
//!
//! A reference to a component the catalogue does NOT have, a CYCLE (A → B → A, self-reference
//! included), a nesting deeper than [`MAX_REFERENCE_DEPTH`] or a fan-out beyond
//! [`MAX_REFERENCE_EXPANSIONS`] are EXPLICIT [`RenderError`]s naming the chain — never a silent hole,
//! never a truncated rendering, and never a stack that grows until it dies.

use std::collections::{BTreeMap, BTreeSet};

use handlebars::Handlebars;
use serde_json::{Map, Value};

use super::{RenderComponent, RenderError, CHILDREN_KEY};
use crate::design_axes::{
    active_combination, parse_axes, parse_condition_body, AxisSpec, TemplateCondition,
};

/// The attribute that makes an element a REFERENCE placeholder.
pub const USE_ATTR: &str = "data-nyx-use";
/// The attribute that FIXES the referenced component's own axes at the reference site.
pub const USE_AXES_ATTR: &str = "data-nyx-use-axes";
/// The prefix of the per-param attributes handed to the referenced component.
pub const PARAM_ATTR_PREFIX: &str = "data-nyx-param-";
/// The vocabulary prefix an expansion may not leak (see the module docs).
pub(super) const NYX_ATTR_PREFIX: &str = "data-nyx-";

/// The three attributes that decide whether an element EXISTS. Named once, because a pass that
/// resolves one of them owes the same answer to the other two ([`bake_node_axes`]).
const CONDITION_ATTRS: [&str; 3] = ["data-nyx-state-case", "data-nyx-if", "data-nyx-if-not"];

/// How deep a chain of references may go. A cycle is caught by NAME (below), so this only bounds the
/// acyclic chains — but it is what makes "the recursion terminates" a fact of this module rather than
/// a property of the catalogue.
pub const MAX_REFERENCE_DEPTH: usize = 8;

/// How many expansions ONE node's render may perform. Depth alone does not bound the WORK: a
/// template referencing the same component four times, six levels down, is acyclic and would still
/// expand thousands of nodes. The budget is what keeps a pathological catalogue from turning one
/// preview into a memory event.
pub const MAX_REFERENCE_EXPANSIONS: usize = 200;

// ---------------------------------------------------------------------------
// The reference SITES a template declares — read by the render AND by the write-path diagnostics.
// ---------------------------------------------------------------------------

/// ONE `data-nyx-use` site read out of a template: the component it names, the params it hands over
/// and the axis values it fixes. The write path checks these against the catalogue
/// ([`crate::mcp_tools_design::reference_warnings`]); the render RESOLVES them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceSite {
    /// The referenced catalogue component's NAME.
    pub component: String,
    /// The params handed over, in `data-nyx-param-<name>` order.
    pub params: Vec<(String, String)>,
    /// The sub-component axis values FIXED here, in `data-nyx-use-axes` order.
    pub axes: Vec<(String, String)>,
}

/// Every reference SITE a template declares, in document order. Never fails: markup this cannot
/// parse declares no reference, exactly as every other template scanner in this codebase behaves.
pub fn reference_sites(template: &str) -> Vec<ReferenceSite> {
    scan_tags(template).iter().filter_map(read_site).collect()
}

/// The site an open tag declares, if it declares one.
fn read_site(tag: &Tag) -> Option<ReferenceSite> {
    if tag.kind == TagKind::Close {
        return None;
    }
    let component = tag.attr(USE_ATTR)?.trim().to_string();
    if component.is_empty() {
        return None;
    }
    let params = tag
        .attrs
        .iter()
        .filter_map(|a| {
            let name = a.name.strip_prefix(PARAM_ATTR_PREFIX)?;
            (!name.is_empty()).then(|| (name.to_string(), a.value.clone().unwrap_or_default()))
        })
        .collect();
    let axes = tag
        .attr(USE_AXES_ATTR)
        .map(|body| {
            parse_condition_body(body, false)
                .into_iter()
                // A fixed value is ONE value: `size=sm|md` would be a condition, not a choice, so
                // the first named value is taken and the write path names the rest.
                .map(|c| (c.axis, c.values[0].clone()))
                .collect()
        })
        .unwrap_or_default();
    Some(ReferenceSite {
        component,
        params,
        axes,
    })
}

// ---------------------------------------------------------------------------
// THE EXPANSION
// ---------------------------------------------------------------------------

/// EXPAND every `data-nyx-use` of `markup` (the already-Handlebars-rendered markup of ONE node or of
/// one referenced component), in place, depth-first.
///
/// `path` is the chain of components being expanded, HOST FIRST — it is what names a cycle and what
/// bounds the depth. `budget` is the remaining expansion allowance for this node's whole render.
pub(super) fn expand_references(
    hb: &Handlebars<'static>,
    catalogue: &BTreeMap<&str, &RenderComponent>,
    host: &str,
    markup: &str,
    path: &mut Vec<String>,
    budget: &mut usize,
) -> Result<String, RenderError> {
    if !markup.contains(USE_ATTR) {
        // The overwhelming majority of templates reference nothing: they are not re-scanned, not
        // re-emitted, and come back byte-identical.
        return Ok(markup.to_string());
    }
    let tags = scan_tags(markup);
    let mut out = String::with_capacity(markup.len());
    let mut cursor = 0usize;
    let mut idx = 0usize;
    while idx < tags.len() {
        let tag = &tags[idx];
        let Some(site) = read_site(tag) else {
            idx += 1;
            continue;
        };
        if is_void_element(&tag.name) {
            return Err(RenderError::BadReference {
                host: host.to_string(),
                detail: format!(
                    "<{}> carries {USE_ATTR}=\"{}\" but a VOID element can hold no content — put \
                     the reference on an element that can (a <span>, a <div>)",
                    tag.name, site.component
                ),
            });
        }
        // The placeholder's own extent: what it wraps is the sub-component's slot, and what follows
        // its close tag is where the scan resumes.
        let (inner, close_text, next_idx, next_cursor) = match tag.kind {
            TagKind::SelfClosing => ("", format!("</{}>", tag.name), idx + 1, tag.end),
            _ => {
                let Some(close_idx) = tag.close else {
                    return Err(RenderError::BadReference {
                        host: host.to_string(),
                        detail: format!(
                            "the <{}> carrying {USE_ATTR}=\"{}\" has NO matching </{}> — a \
                             reference placeholder must be a closed element, since its content is \
                             the sub-component's slot",
                            tag.name, site.component, tag.name
                        ),
                    });
                };
                let close = &tags[close_idx];
                (
                    &markup[tag.end..close.start],
                    markup[close.start..close.end].to_string(),
                    close_idx + 1,
                    close.end,
                )
            }
        };
        out.push_str(&markup[cursor..tag.start]);
        // The slot content is the HOST's markup: it is expanded in the HOST's chain, not the
        // sub-component's.
        let children = expand_references(hb, catalogue, host, inner, path, budget)?;
        let expansion = render_reference(hb, catalogue, host, &site, children, path, budget)?;
        out.push_str(&rebuild_tag(tag, &[], false, true));
        out.push_str(&expansion);
        out.push_str(&close_text);
        cursor = next_cursor;
        idx = next_idx;
    }
    out.push_str(&markup[cursor..]);
    Ok(out)
}

/// RESOLVE one reference site into flattened, inert markup: render the referenced component's
/// template with the site's params, expand ITS references in turn, then resolve ITS axes and strip
/// every `data-nyx-*` it carries.
fn render_reference(
    hb: &Handlebars<'static>,
    catalogue: &BTreeMap<&str, &RenderComponent>,
    host: &str,
    site: &ReferenceSite,
    children: String,
    path: &mut Vec<String>,
    budget: &mut usize,
) -> Result<String, RenderError> {
    if let Some(at) = path.iter().position(|p| *p == site.component) {
        let mut cycle: Vec<String> = path[at..].to_vec();
        cycle.push(site.component.clone());
        return Err(RenderError::ReferenceCycle { path: cycle });
    }
    if path.len() >= MAX_REFERENCE_DEPTH {
        let mut chain = path.clone();
        chain.push(site.component.clone());
        return Err(RenderError::ReferenceLimit {
            path: chain,
            detail: format!("references nest deeper than {MAX_REFERENCE_DEPTH} levels"),
        });
    }
    if *budget == 0 {
        let mut chain = path.clone();
        chain.push(site.component.clone());
        return Err(RenderError::ReferenceLimit {
            path: chain,
            detail: format!(
                "this node expanded more than {MAX_REFERENCE_EXPANSIONS} references (a fan-out, not \
                 a cycle: each level multiplies the one below it)"
            ),
        });
    }
    let Some(sub) = catalogue.get(site.component.as_str()) else {
        return Err(RenderError::UnknownReference {
            host: host.to_string(),
            reference: site.component.clone(),
        });
    };
    *budget -= 1;
    let mut ctx: Map<String, Value> = Map::new();
    for (name, value) in &site.params {
        ctx.insert(name.clone(), Value::String(value.clone()));
    }
    ctx.insert(CHILDREN_KEY.to_string(), Value::String(children));
    let rendered = hb
        .render_template(&sub.template, &Value::Object(ctx))
        .map_err(|e| RenderError::Template {
            component: sub.name.clone(),
            message: e.to_string(),
        })?;
    path.push(site.component.clone());
    let nested = expand_references(hb, catalogue, &sub.name, &rendered, path, budget);
    path.pop();
    // A REFERENCED sub-component gets NO ambient axes: an axis name is LOCAL to a component, so a
    // page-level `size` is never mapped onto a `Spinner.size` (see [`AmbientAxes`]).
    let resolved = resolve_axes(
        &nested?,
        &parse_axes(sub.states_json.as_deref()),
        &site.axes,
        AmbientAxes::default(),
    );
    // A REFERENCED sub-component keeps its own CSS slot: the marks are stamped here, AFTER the axis
    // pass strips the rest of the vocabulary, so a shared `Spinner`'s animation survives being used
    // rather than placed (finding `01KYVMTWB71R0D0CT1SV88J8DX` — the loader is exactly this case).
    Ok(super::stamp_component_scope(sub, resolved))
}

// ---------------------------------------------------------------------------
// AXES → CONCRETE CLASSES + `hidden`, then NOTHING left of the vocabulary
// ---------------------------------------------------------------------------

/// Resolve `markup` (a sub-component's rendering) at the combination `fixed` names, and return
/// markup carrying NO `data-nyx-*` attribute at all.
///
/// It is the shim's applier, run once, ahead of time: the ACTIVE value of every axis puts its classes
/// on that axis's targets (the elements marked `data-nyx-axis`, else the markup's TOP-LEVEL elements
/// — the shim's `#depot-root` children), and every structural mechanism (`data-nyx-state-case`,
/// `data-nyx-if`, `data-nyx-if-not`) is evaluated, an absent element getting the `hidden` ATTRIBUTE.
/// A clause on an axis the sub-component does not declare is IGNORED, never falsified — the same rule
/// the shim applies, for the same reason (hiding on an unknown name is how a preview goes blank).
///
/// It is `pub(super)` because a SECOND caller needs the very same pass: [`super::resolve_component`],
/// the agent's own eyes on its synthesis (finding `01KYT0Z3RYGF3A881SDQN0X9D7`), resolves the
/// component the agent just wrote exactly as this resolves a referenced one. One applier, so what
/// the agent reads and what a host bakes in can never be two different renderings.
///
/// `ambient` carries the axes of the SUBJECT AROUND this markup — a proto VARIANT's own axes, which
/// pilot the whole page in the frame (finding `01KZ3JS7PNF3F8GFB3GYRK6CKH`). See [`AmbientAxes`].
pub(super) fn resolve_axes(
    markup: &str,
    axes: &[AxisSpec],
    fixed: &[(String, String)],
    ambient: AmbientAxes<'_>,
) -> String {
    let tags = scan_tags(markup);
    if tags.is_empty() {
        return markup.to_string();
    }
    // The ACTIVE combination: what the site fixed (when the axis really declares that value), else
    // the axis's own default — read ONCE, in `design_axes`, so the combination this pass RESOLVES is
    // the combination the preview tool REPORTS. The DECLARED axes come first, so the axis
    // `data-nyx-state-case` drives stays the markup's own first axis.
    let active = active_combination(axes, fixed);
    let ambient_active: Vec<(&str, &str)> = active_combination(ambient.axes, ambient.fixed)
        .into_iter()
        .filter(|(name, _)| !axes.iter().any(|a| a.name == *name))
        .collect();
    let value_of = |axis: &str| -> Option<&str> {
        active
            .iter()
            .chain(ambient_active.iter())
            .find(|(name, _)| *name == axis)
            .map(|(_, value)| *value)
    };
    // Which axes the markup TARGETS explicitly — the rest land on the top-level elements.
    let marked: BTreeSet<&str> = tags
        .iter()
        .filter_map(|t| t.attr("data-nyx-axis"))
        .flat_map(str::split_whitespace)
        .collect();
    let primary = active
        .first()
        .or(ambient_active.first())
        .map(|(_, value)| *value);

    let mut out = String::with_capacity(markup.len());
    let mut cursor = 0usize;
    for tag in &tags {
        out.push_str(&markup[cursor..tag.start]);
        cursor = tag.end;
        if tag.kind == TagKind::Close {
            out.push_str(&markup[tag.start..tag.end]);
            continue;
        }
        // THE STRUCTURE — every mechanism the element declares must agree, exactly as in the shim.
        let mut show = true;
        if let (Some(case), Some(primary)) = (tag.attr("data-nyx-state-case"), primary) {
            show = case == primary;
        }
        if show {
            if let Some(body) = tag.attr("data-nyx-if") {
                show = parse_condition_body(body, false)
                    .iter()
                    .all(|c| clause_matches(c, &value_of));
            }
        }
        if show {
            if let Some(body) = tag.attr("data-nyx-if-not") {
                show = !parse_condition_body(body, true)
                    .iter()
                    .any(|c| clause_matches(c, &value_of));
            }
        }
        // THE CLASSES — the active value of every axis whose target this element is.
        let element_axes: Vec<&str> = tag
            .attr("data-nyx-axis")
            .map(|v| v.split_whitespace().collect())
            .unwrap_or_default();
        let mut classes: Vec<&str> = Vec::new();
        for axis in axes {
            let targeted = if marked.contains(axis.name.as_str()) {
                element_axes.contains(&axis.name.as_str())
            } else {
                tag.depth == 0
            };
            if !targeted {
                continue;
            }
            let Some(value) = value_of(&axis.name).and_then(|v| axis.value(v)) else {
                continue;
            };
            classes.extend(value.classes.split_whitespace());
        }
        // THE AMBIENT AXES' classes land exactly where the shim puts them: on the elements that
        // OPT IN with `data-nyx-axis="<name>"`, else on the top-level elements of the DOCUMENT —
        // which, split node by node, is the ROOT node's markup and no other. Applying them at every
        // node's root would stamp a page-level class on forty elements the frame never gives it to.
        for axis in ambient.axes {
            if axes.iter().any(|a| a.name == axis.name) {
                continue; // the markup declares this name itself — its own axis wins, locally.
            }
            let targeted = if marked.contains(axis.name.as_str()) {
                element_axes.contains(&axis.name.as_str())
            } else {
                ambient.classes_on_root && tag.depth == 0
            };
            if !targeted {
                continue;
            }
            let Some(value) = value_of(&axis.name).and_then(|v| axis.value(v)) else {
                continue;
            };
            classes.extend(value.classes.split_whitespace());
        }
        out.push_str(&rebuild_tag(tag, &classes, !show, false));
    }
    out.push_str(&markup[cursor..]);
    out
}

/// Resolve the axes ONE COMPOSITION NODE fixes in its params, INSIDE THE MARKUP THE FRAME RECEIVES —
/// and leave every other `data-nyx-*` mark to the shim (finding `01KZ41YK39SGNKF6XV87A35GPN`).
///
/// # The defect this closes
///
/// A node's params carry its axis values (`variant=outline`, `size=xl`, `loading=off`) — that is what
/// the params form writes and what `proto_variant_set_node_params` persists. [`resolve_axes`] applies
/// them for the AGENT ([`super::Vocabulary::Consume`]). The FRAME got [`super::Vocabulary::Preserve`],
/// which ran NO applier at all on the strength of one sentence: "the shim reads a node's axis values
/// off its params". IT DOES NOT. The shim is fed `window.__nyxAxes` / `__nyxAxisValues`, and those are
/// the SUBJECT's axes — a proto variant's `states_json`, applied over the WHOLE document. A node's own
/// axes reached nobody.
///
/// Measured in the running app: `design_render` returned BYTE-IDENTICAL markup for a `button` node at
/// `{variant: default, size: default, loading: off}` and at `{variant: outline, size: xl, loading:
/// on}`, and the frame's `__nyxAxes` was `[]`. Three live controls in the params form, one of which
/// (`buttonLabel`, a plain Handlebars substitution, which BOTH vocabularies perform) worked — which is
/// exactly how the defect read as "the form only half works".
///
/// # Why this cannot be left to the shim
///
/// [`super::Vocabulary::Preserve`] exists because the frame's shim pilots things LIVE: the chip-bar,
/// the in-place `data-nyx-state` flip, the `data-nyx-page` navigation. All three are DOCUMENT-level —
/// they move the SUBJECT's axes, the only ones the shim knows. Nothing in the frame pilots a NODE's
/// own axes: there is no chip for them, and `setAxis` refuses a name `__nyxAxes` does not declare. So
/// resolving them here removes no affordance; it is the only place they can act at all.
///
/// # What it does, and what it deliberately does not touch
///
/// * CLASSES — the active value's classes on that axis's targets: the elements that opt in with
///   `data-nyx-axis="<name>"`, else this node's TOP-LEVEL elements. [`resolve_axes`]'s rule, verbatim.
/// * STRUCTURE — a `data-nyx-state-case` / `data-nyx-if` / `data-nyx-if-not` clause naming one of THIS
///   node's axes is decided here, and the clause is then REMOVED. Removing it is not tidiness: the
///   shim re-evaluates every element still carrying one of those attributes, and a clause on an axis
///   it does not know is IGNORED (never falsified — hiding on an unknown name is how a preview goes
///   blank), so it would immediately un-hide what this pass just hid. That is the third dead control
///   in the report: `loading=off` on the node, and the loader visible anyway.
/// * WHAT STAYS — a clause naming an axis this node does NOT declare is left in place, at its own
///   attribute, for the shim to decide live: that is the subject's chip-bar, and it must keep moving.
///   `data-nyx-page`, `data-nyx-state`, `data-nyx-set`, `data-nyx-css` and everything else are
///   untouched.
/// * SHADOWING — this node's own axis NAMES are struck from `data-nyx-axis`, so a same-named DOCUMENT
///   axis does not also claim the element. An axis name is LOCAL to a component ([`AmbientAxes`] says
///   the same for the other direction); an unmarked document axis falls back to the document's root
///   children, which is not this node.
///
/// # THE SHIM'S OWN AXES ARE NOT BAKED — ONE OWNER PER AXIS
///
/// `shim_axes` is what the host will hand the frame as `window.__nyxAxes` — the SUBJECT's axes, the
/// ones the shim applies LIVE over the whole document. An axis of that set is left entirely alone
/// here, whatever this node also calls it. Two appliers on one axis is not a redundancy, it is a
/// conflict: the shim removes only the classes IT added, so a value baked here would survive every
/// flip and stack with the next one.
///
/// It matters on ONE surface, and it is the surface that made the rule visible: the component DETAIL
/// preview renders a composition of exactly ONE node — the component itself — and hands the shim that
/// same component's axes, because the panel's controls and the in-place `data-nyx-set` affordances
/// pilot them live. There, the node's axes ARE the subject's, so nothing is baked and the live
/// preview is untouched. On a prototype page the subject is the VARIANT, whose axes are a different
/// (usually empty) set — so a node's axes are baked, which is the whole finding.
pub(super) fn bake_node_axes(
    markup: &str,
    axes: &[AxisSpec],
    fixed: &[(String, String)],
    shim_axes: &[AxisSpec],
) -> String {
    if axes.is_empty() {
        return markup.to_string();
    }
    let tags = scan_tags(markup);
    if tags.is_empty() {
        return markup.to_string();
    }
    // OURS = declared by this node's component and NOT piloted live by the shim.
    let declares = |axis: &str| {
        axes.iter().any(|a| a.name == axis) && !shim_axes.iter().any(|a| a.name == axis)
    };
    // The combination this node really renders at — read ONCE, in `design_axes`, so the frame and the
    // agent resolve the SAME one (an axis the node leaves out renders at its own `default`).
    let active = active_combination(axes, fixed);
    let value_of = |axis: &str| -> Option<&str> {
        active
            .iter()
            .find(|(name, _)| *name == axis)
            .map(|(_, value)| *value)
    };
    let marked: BTreeSet<&str> = tags
        .iter()
        .filter_map(|t| t.attr("data-nyx-axis"))
        .flat_map(str::split_whitespace)
        .filter(|name| declares(name))
        .collect();
    // `data-nyx-state-case` names NO axis: it means the FIRST axis of the markup's own component. So
    // it is ours exactly when that first axis is ours — and it stays the shim's when the shim pilots
    // it, which is the detail preview's case.
    let primary = axes
        .first()
        .filter(|a| declares(&a.name))
        .and_then(|a| value_of(&a.name));

    let mut out = String::with_capacity(markup.len());
    let mut cursor = 0usize;
    for tag in &tags {
        out.push_str(&markup[cursor..tag.start]);
        cursor = tag.end;
        if tag.kind == TagKind::Close {
            out.push_str(&markup[tag.start..tag.end]);
            continue;
        }
        // THE STRUCTURE — decided here for this node's OWN axes, left alone for every other name.
        let mut show = true;
        let mut rewritten: Vec<(&str, Option<String>)> = Vec::new();
        if let (Some(case), Some(primary)) = (tag.attr("data-nyx-state-case"), primary) {
            show = case == primary;
            rewritten.push(("data-nyx-state-case", None));
        }
        for (attr_name, negated) in [("data-nyx-if", false), ("data-nyx-if-not", true)] {
            let Some(body) = tag.attr(attr_name) else {
                continue;
            };
            let clauses = parse_condition_body(body, negated);
            let (mine, theirs): (Vec<_>, Vec<_>) = clauses.iter().partition(|c| declares(&c.axis));
            if mine.is_empty() {
                continue; // nothing of ours in there — the shim keeps the whole attribute.
            }
            // `-if` needs EVERY clause, `-if-not` hides on ANY: either way OUR verdict, once false,
            // cannot be reversed by the clauses left to the shim.
            let ours = if negated {
                !mine.iter().any(|c| clause_matches(c, &value_of))
            } else {
                mine.iter().all(|c| clause_matches(c, &value_of))
            };
            show = show && ours;
            rewritten.push((
                attr_name,
                (ours && !theirs.is_empty()).then(|| condition_body(&theirs)),
            ));
        }
        // ONCE THIS PASS HAS DECIDED THE ELEMENT AWAY, NOTHING MAY BRING IT BACK. The verdict is a
        // CONJUNCTION over three attributes, and it was only ever enforced attribute by attribute: an
        // element hidden by its `data-nyx-state-case` (or by an `-if` of ours) while carrying an
        // `-if-not` naming only the shim's axes kept that attribute — and the shim's `applyStructure`
        // does `setHidden(el, !show)` from the clauses IT can see, so it REMOVED the `hidden` written
        // here. The whole conditional vocabulary therefore leaves with the verdict.
        if !show {
            rewritten.retain(|(name, _)| !CONDITION_ATTRS.contains(name));
            for name in CONDITION_ATTRS {
                rewritten.push((name, None));
            }
        }
        // THE CLASSES — the active value of every axis whose target this element is.
        let element_axes: Vec<&str> = tag
            .attr("data-nyx-axis")
            .map(|v| v.split_whitespace().collect())
            .unwrap_or_default();
        let mut classes: Vec<&str> = Vec::new();
        for axis in axes.iter().filter(|a| declares(&a.name)) {
            let targeted = if marked.contains(axis.name.as_str()) {
                element_axes.contains(&axis.name.as_str())
            } else {
                tag.depth == 0
            };
            if !targeted {
                continue;
            }
            let Some(value) = value_of(&axis.name).and_then(|v| axis.value(v)) else {
                continue;
            };
            classes.extend(value.classes.split_whitespace());
        }
        // …and the axis NAMES this pass owns struck from `data-nyx-axis`, so the shim's
        // document-level axes cannot claim an element it has already served.
        if !element_axes.is_empty() {
            let kept: Vec<&str> = element_axes
                .iter()
                .copied()
                .filter(|name| !declares(name))
                .collect();
            if kept.len() != element_axes.len() {
                rewritten.push(("data-nyx-axis", (!kept.is_empty()).then(|| kept.join(" "))));
            }
        }
        out.push_str(&rebuild_kept_tag(tag, &classes, !show, &rewritten));
    }
    out.push_str(&markup[cursor..]);
    out
}

/// Write a condition body back out in the `axis=a|b, axis2=c` grammar it was read in — the clauses
/// this pass did NOT own, handed on to the shim unchanged in meaning.
fn condition_body(clauses: &[&TemplateCondition]) -> String {
    clauses
        .iter()
        .map(|c| format!("{}={}", c.axis, c.values.join("|")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// RE-EMIT a tag KEEPING its whole `data-nyx-*` vocabulary except the attributes `rewritten` names —
/// `(name, None)` drops it, `(name, Some(value))` replaces its value — plus `extra_classes` merged
/// into `class` and `hidden` when the structure resolved it away.
///
/// It is [`rebuild_tag`]'s sibling for the FRAME: that one exists to STRIP the vocabulary (an agent
/// must not read back a mark it cannot run), this one to consume EXACTLY what it just decided and
/// leave the rest live. Merging them would have made one function whose contract is "strip, unless".
fn rebuild_kept_tag(
    tag: &Tag,
    extra_classes: &[&str],
    hidden: bool,
    rewritten: &[(&str, Option<String>)],
) -> String {
    let mut out = String::with_capacity(tag.end - tag.start + 32);
    out.push('<');
    out.push_str(&tag.name);
    let mut wrote_class = false;
    let mut has_hidden = false;
    for attr in &tag.attrs {
        let replacement = rewritten.iter().find(|(name, _)| *name == attr.name);
        let value = match replacement {
            Some((_, None)) => continue,
            Some((_, Some(value))) => Some(value.clone()),
            None => attr.value.clone(),
        };
        if attr.name.eq_ignore_ascii_case("hidden") {
            has_hidden = true;
        }
        out.push(' ');
        out.push_str(&attr.name);
        let value = match (value, attr.name.eq_ignore_ascii_case("class")) {
            (Some(value), true) if !extra_classes.is_empty() => {
                wrote_class = true;
                let mut merged = value;
                for class in extra_classes {
                    if !merged.split_whitespace().any(|c| c == *class) {
                        merged.push(' ');
                        merged.push_str(class);
                    }
                }
                Some(merged)
            }
            (Some(value), is_class) => {
                wrote_class |= is_class;
                Some(value)
            }
            (None, is_class) => {
                wrote_class |= is_class;
                None
            }
        };
        if let Some(value) = value {
            out.push_str("=\"");
            out.push_str(&encode_attribute(&value));
            out.push('"');
        }
    }
    if !wrote_class && !extra_classes.is_empty() {
        out.push_str(" class=\"");
        out.push_str(&encode_attribute(&extra_classes.join(" ")));
        out.push('"');
    }
    if hidden && !has_hidden {
        out.push_str(" hidden");
    }
    if tag.kind == TagKind::SelfClosing && tag.slash_closed {
        out.push_str("/>");
    } else {
        out.push('>');
    }
    out
}

/// The axes of the SUBJECT AROUND a markup — a proto VARIANT's own `states_json`, which pilots the
/// WHOLE composition in the frame (finding `01KZ3JS7PNF3F8GFB3GYRK6CKH`).
///
/// # Why they exist at all
///
/// `design_preview(variant_id)` resolves the tree NODE BY NODE, and each node was resolved against
/// its COMPONENT's axes alone. A variant's own axes were read by nobody, so an element gated on one
/// of them (`data-nyx-if="loading=on"` in an `[inline]` node, say) matched the "clause on an axis
/// this markup does not declare" rule and stayed VISIBLE at every value — while the frame, where the
/// shim applies the variant's axes over the whole document, hid it. Two renderings of one design.
///
/// # What they do, and where they stop
///
/// * CONDITIONS: an ambient axis decides `data-nyx-if` / `-if-not` / `data-nyx-state-case` wherever
///   the clause names it, at every node — a condition is the same condition wherever it sits.
/// * CLASSES: the shim's rule, exactly — the elements that opt in with `data-nyx-axis="<name>"`, and
///   otherwise the DOCUMENT's top-level elements, which is why `classes_on_root` is true for the
///   subject's root node and false for every other.
/// * SHADOWING: an ambient axis whose name the markup's own component DECLARES is ignored there. An
///   axis name is LOCAL to a component — the same rule `data-nyx-use` states — so a `Button.size`
///   is never overwritten by a page-level `size`.
/// * REFERENCES: a `data-nyx-use` sub-component gets NO ambient axes, for that same rule.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AmbientAxes<'a> {
    pub(super) axes: &'a [AxisSpec],
    pub(super) fixed: &'a [(String, String)],
    /// Is this markup the SUBJECT's root node? Only there do unmarked ambient axes put their classes
    /// on the top-level elements — the shim's `#depot-root` children, seen one node at a time.
    pub(super) classes_on_root: bool,
}

// ---------------------------------------------------------------------------
// THE CSS-SLOT SCOPE MARK (finding `01KYVMTWB71R0D0CT1SV88J8DX`)
// ---------------------------------------------------------------------------

/// STAMP the CSS-slot scope id onto the TOP-LEVEL elements of `markup` — the component's own root
/// element(s), the anchor every rule of its slot was rewritten against
/// ([`crate::css_slot::compile`]).
///
/// This is the half of the scope guarantee that lives in the MARKUP; the other half rewrites the
/// selectors. Neither is asked of the agent. It runs AFTER [`resolve_axes`] wherever both apply, so
/// the mark survives the pass that strips the rest of the `data-nyx-*` vocabulary — the mark is not
/// an instruction to the shim, it is the IDENTITY of a stylesheet.
///
/// An element already carrying a scope KEEPS it: the attribute is a space-separated list (matched
/// `~=`), so a root that sits in two scopes (a component whose template puts `{{{children}}}` at its
/// own top level) loses neither. Markup with no element at all comes back untouched.
pub(super) fn stamp_scope(markup: &str, scope_id: &str) -> String {
    let tags = scan_tags(markup);
    if tags.is_empty() {
        return markup.to_string();
    }
    let mut out = String::with_capacity(markup.len() + scope_id.len() + 24);
    let mut cursor = 0usize;
    for tag in &tags {
        if tag.depth != 0 || tag.kind == TagKind::Close {
            continue;
        }
        out.push_str(&markup[cursor..tag.start]);
        cursor = tag.end;
        match tag.attr(crate::css_slot::SCOPE_ATTR) {
            None => {
                // The path that runs: the attribute is INSERTED right after the tag name, so every
                // other byte of the tag is preserved verbatim.
                let at = tag.start + 1 + tag.name.len();
                out.push_str(&markup[tag.start..at]);
                out.push_str(&format!(" {}=\"{scope_id}\"", crate::css_slot::SCOPE_ATTR));
                out.push_str(&markup[at..tag.end]);
            }
            Some(existing) if existing.split_whitespace().any(|s| s == scope_id) => {
                out.push_str(&markup[tag.start..tag.end]);
            }
            Some(existing) => {
                // A second scope on the same root: MERGE, never replace.
                let merged = format!("{existing} {scope_id}");
                out.push_str(&rebuild_with_scope(tag, &merged));
            }
        }
    }
    out.push_str(&markup[cursor..]);
    out
}

/// Re-emit one open tag with its [`crate::css_slot::SCOPE_ATTR`] replaced by `value`, every other
/// attribute (vocabulary included) preserved.
fn rebuild_with_scope(tag: &Tag, value: &str) -> String {
    let mut out = String::with_capacity(tag.end - tag.start + value.len());
    out.push('<');
    out.push_str(&tag.name);
    for attr in &tag.attrs {
        out.push(' ');
        out.push_str(&attr.name);
        let emitted = if attr.name == crate::css_slot::SCOPE_ATTR {
            Some(value.to_string())
        } else {
            attr.value.clone()
        };
        if let Some(emitted) = emitted {
            out.push_str("=\"");
            out.push_str(&encode_attribute(&emitted));
            out.push('"');
        }
    }
    if tag.kind == TagKind::SelfClosing && tag.slash_closed {
        out.push_str("/>");
    } else {
        out.push('>');
    }
    out
}

// ---------------------------------------------------------------------------
// THE NAVIGATION MARK (finding `01KYW34347CZSNXCYS35KDG3EY`)
// ---------------------------------------------------------------------------

/// STAMP a node's NAVIGATION marks onto the TOP-LEVEL elements of its markup — the node's own root
/// element(s), which are what a user clicks.
///
/// Returns `None` when the markup carries NO element at all (a template that renders bare text, a
/// structural node with no children): there is nothing that can hold the link, and the caller NAMES
/// that rather than dropping the destination in silence.
///
/// `vocabulary` is the WHOLE navigation vocabulary, and every one of its attributes is REMOVED from
/// the root before the stated ones are written: when a node declares a destination, the navigation
/// marks on its root are the RENDER's and nobody else's.
///
/// # Why it clears the whole vocabulary and not just what it re-poses (finding `01KZ3JN69E32FYBF01NBE4FWJD`)
///
/// The pass used to drop only the attributes it was about to write back. That is enough for a link
/// that RESOLVES (the node's destination replaces a template's baked-in one — the link is a PLACEMENT
/// decision, and the composition is where placement lives). It was NOT enough for a link that
/// resolves to NOTHING: the render stamped [`super::PAGE_MISSING_ATTR`] and left a
/// [`super::PAGE_ATTR`] the template had baked in sitting right beside it, so the shim navigated to
/// a slug frozen in a component's markup while the answer said the link led nowhere. The marker
/// BADGED the failure and NEUTRALIZED nothing.
///
/// Clearing the vocabulary makes the guarantee structural rather than incidental: whatever the
/// template wrote, a root whose node states a destination ends up carrying exactly the marks this
/// render decided, and a dead link really is dead. (A node that states NO destination is not touched
/// at all — a template's own `data-nyx-page` is markup, and the shim honours it.)
pub(super) fn stamp_navigation(
    markup: &str,
    attrs: &[(&str, &str)],
    vocabulary: &[&str],
) -> Option<String> {
    let tags = scan_tags(markup);
    let roots: Vec<&Tag> = tags
        .iter()
        .filter(|t| t.depth == 0 && t.kind != TagKind::Close)
        .collect();
    if roots.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(markup.len() + attrs.len() * 48);
    let mut cursor = 0usize;
    for tag in roots {
        out.push_str(&markup[cursor..tag.start]);
        cursor = tag.end;
        if tag
            .attrs
            .iter()
            .any(|a| vocabulary.contains(&a.name.as_str()))
        {
            // A CONCURRENT navigation mark is already there: re-emit the tag without ANY of them,
            // ours appended. This is the branch the finding is about.
            out.push_str(&rebuild_with_navigation(tag, attrs, vocabulary));
            continue;
        }
        // The path that runs: the attributes are INSERTED right after the tag name, so every other
        // byte of the tag is preserved verbatim.
        let at = tag.start + 1 + tag.name.len();
        out.push_str(&markup[tag.start..at]);
        for (name, value) in attrs {
            out.push_str(&format!(" {name}=\"{}\"", encode_attribute(value)));
        }
        out.push_str(&markup[at..tag.end]);
    }
    out.push_str(&markup[cursor..]);
    Some(out)
}

/// Re-emit one open tag with every attribute of the navigation `vocabulary` DROPPED and the stated
/// ones appended — every other attribute (the rest of the `data-nyx-*` vocabulary included)
/// preserved.
fn rebuild_with_navigation(tag: &Tag, attrs: &[(&str, &str)], vocabulary: &[&str]) -> String {
    let mut out = String::with_capacity(tag.end - tag.start + attrs.len() * 48);
    out.push('<');
    out.push_str(&tag.name);
    for attr in &tag.attrs {
        if vocabulary.contains(&attr.name.as_str()) {
            continue;
        }
        out.push(' ');
        out.push_str(&attr.name);
        if let Some(value) = &attr.value {
            out.push_str("=\"");
            out.push_str(&encode_attribute(value));
            out.push('"');
        }
    }
    for (name, value) in attrs {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        out.push_str(&encode_attribute(value));
        out.push('"');
    }
    if tag.kind == TagKind::SelfClosing && tag.slash_closed {
        out.push_str("/>");
    } else {
        out.push('>');
    }
    out
}

// ---------------------------------------------------------------------------
// THE NODE MARK (finding `01KYW77Y65A9HYNDZ69M67S6B8`)
// ---------------------------------------------------------------------------

/// STAMP a node's own `node_key` onto the TOP-LEVEL elements of its markup — the ROOTS of the region
/// that node draws, and the only elements a designation gesture has to recognise.
///
/// This is what makes the RENDER designable: a click in the frame resolves to the `node_key` of the
/// named tree by walking up to the nearest element carrying [`super::NODE_ATTR`]. The name is written
/// HERE, beside the walk that knows it and right where [`stamp_scope`] writes the CSS scope — the
/// convention lives in ONE place, this one, instead of being re-derived by whoever reads the markup.
///
/// # Roots only, and the deepest owner wins
///
/// Only depth-0 tags are stamped, for the same reason [`stamp_navigation`] stamps only those: a
/// node's markup IS its root element(s), and marking every descendant would ring each `<rect>` of an
/// icon's `<svg>` separately instead of ringing the node. A tag that ALREADY carries the mark is left
/// verbatim — the walk is depth-first, so it can only have been claimed by a DEEPER node (a component
/// whose whole template is `{{{children}}}` puts a child's root at its own top level), and the
/// deepest node is the one an element belongs to.
///
/// Markup with no element at all (a template rendering bare text) comes back untouched: there is
/// nothing to name, which is exactly what the shim then resolves to — the enclosing node.
pub(super) fn stamp_node_key(markup: &str, node_key: &str) -> String {
    let tags = scan_tags(markup);
    if tags.is_empty() {
        return markup.to_string();
    }
    let value = encode_attribute(node_key);
    let mut out = String::with_capacity(markup.len() + value.len() + 24);
    let mut cursor = 0usize;
    for tag in &tags {
        let claimed = tag.attrs.iter().any(|a| a.name == super::NODE_ATTR);
        if tag.depth != 0 || tag.kind == TagKind::Close || claimed {
            continue;
        }
        // The attribute is INSERTED right after the tag name, so every other byte is preserved
        // verbatim — the same rewrite-free path the scope and the navigation marks take.
        let at = tag.start + 1 + tag.name.len();
        out.push_str(&markup[cursor..at]);
        out.push_str(&format!(" {}=\"{value}\"", super::NODE_ATTR));
        cursor = at;
    }
    out.push_str(&markup[cursor..]);
    out
}

/// Does ONE clause of a condition hold at the active combination? A clause on an axis the component
/// does NOT declare is IGNORED (true), never falsified — the shim's rule, kept identical.
fn clause_matches<'a>(
    clause: &TemplateCondition,
    value_of: &impl Fn(&str) -> Option<&'a str>,
) -> bool {
    match value_of(&clause.axis) {
        None => true,
        Some(active) => clause.values.iter().any(|v| v == active),
    }
}

// ---------------------------------------------------------------------------
// A MINIMAL TAG SCANNER — enough to know where an element starts, ends and what it carries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagKind {
    Open,
    Close,
    SelfClosing,
}

/// ONE attribute, its value already entity-DECODED (the value seen in the markup is escaped, and what
/// a param hands to Handlebars must be the text, not its escaping).
#[derive(Debug, Clone)]
struct Attr {
    name: String,
    value: Option<String>,
}

/// ONE tag of the markup: where it is, what it is, what it carries, how deep it sits, and — for an
/// open tag — the index of the tag that closes it.
#[derive(Debug, Clone)]
struct Tag {
    start: usize,
    end: usize,
    name: String,
    kind: TagKind,
    attrs: Vec<Attr>,
    /// Nesting depth of the ELEMENT (0 = top level of this markup).
    depth: usize,
    /// The index of the matching close tag, for an open one.
    close: Option<usize>,
    /// Was the tag written self-closed (`<span/>`)? Kept so a rewrite gives it back its own shape.
    slash_closed: bool,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.name == name)
            .and_then(|a| a.value.as_deref())
    }
}

/// The elements HTML gives no closing tag — a reference cannot live on one, and their depth must not
/// grow.
fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// SCAN the tags of a markup fragment. Deliberately minimal — it is not an HTML parser, it is what
/// this module needs: comments and doctypes skipped, quoted attribute values respected, a `<` that
/// opens no name treated as text, and mismatched close tags tolerated rather than fatal (the markup
/// comes from an LLM's template, and a scanner that gives up on it renders nothing at all).
fn scan_tags(html: &str) -> Vec<Tag> {
    let bytes = html.as_bytes();
    let mut out: Vec<Tag> = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if html[i..].starts_with("<!--") {
            i = html[i..].find("-->").map_or(bytes.len(), |p| i + p + 3);
            continue;
        }
        if i + 1 < bytes.len() && bytes[i + 1] == b'!' {
            i = html[i..].find('>').map_or(bytes.len(), |p| i + p + 1);
            continue;
        }
        let closing = i + 1 < bytes.len() && bytes[i + 1] == b'/';
        let name_at = if closing { i + 2 } else { i + 1 };
        if name_at >= bytes.len() || !bytes[name_at].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let mut j = name_at;
        while j < bytes.len()
            && (bytes[j].is_ascii_alphanumeric() || matches!(bytes[j], b'-' | b'_' | b':'))
        {
            j += 1;
        }
        let name = html[name_at..j].to_ascii_lowercase();
        let Some(gt) = tag_end(html, j) else {
            // An unterminated tag: everything from here is text as far as this scanner is concerned.
            break;
        };
        let body = &html[j..gt];
        let slash_closed = body.trim_end().ends_with('/');
        let attrs = if closing {
            Vec::new()
        } else {
            parse_attrs(body.trim_end().trim_end_matches('/'))
        };
        let void = is_void_element(&name);
        let kind = if closing {
            TagKind::Close
        } else if slash_closed || void {
            TagKind::SelfClosing
        } else {
            TagKind::Open
        };
        let index = out.len();
        let depth = match kind {
            TagKind::Close => {
                // Pop back to the matching open tag; an unmatched close changes nothing.
                if let Some(at) = stack.iter().rposition(|(_, n)| n == &name) {
                    let (open_index, _) = stack[at];
                    out[open_index].close = Some(index);
                    stack.truncate(at);
                }
                stack.len()
            }
            TagKind::SelfClosing => stack.len(),
            TagKind::Open => {
                let depth = stack.len();
                stack.push((index, name.clone()));
                depth
            }
        };
        out.push(Tag {
            start: i,
            end: gt + 1,
            name,
            kind,
            attrs,
            depth,
            close: None,
            slash_closed,
        });
        i = gt + 1;
    }
    out
}

/// The index of the `>` that ends a tag whose attribute region starts at `from`, quoted values
/// respected (`title="a > b"` does NOT end it).
fn tag_end(html: &str, from: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut quote: Option<u8> = None;
    for (offset, byte) in bytes.iter().enumerate().skip(from) {
        match quote {
            Some(q) if *byte == q => quote = None,
            Some(_) => {}
            None => match *byte {
                b'"' | b'\'' => quote = Some(*byte),
                b'>' => return Some(offset),
                _ => {}
            },
        }
    }
    None
}

/// The attributes of a tag's attribute region. A bare attribute (`hidden`) carries `None`; a quoted
/// or unquoted value is entity-decoded.
fn parse_attrs(region: &str) -> Vec<Attr> {
    let bytes = region.as_bytes();
    let mut out: Vec<Attr> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let name_at = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        if i == name_at {
            i += 1;
            continue;
        }
        let name = region[name_at..i].to_string();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            out.push(Attr { name, value: None });
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
            let quote = bytes[i];
            i += 1;
            let value_at = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            let raw = &region[value_at..i];
            i = (i + 1).min(bytes.len());
            raw
        } else {
            let value_at = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            &region[value_at..i]
        };
        out.push(Attr {
            name,
            value: Some(decode_entities(value)),
        });
    }
    out
}

/// RE-EMIT a tag: its attributes minus everything `data-nyx-*` (when `strip_vocabulary`), plus
/// `extra_classes` merged into `class`, plus `hidden` when the structure resolved it away.
///
/// `keep_own_vocabulary` re-emits the HOST's own marks (that is the placeholder's rewrite: only the
/// three consumed attributes go); an expansion's tags are re-emitted with the vocabulary GONE.
fn rebuild_tag(
    tag: &Tag,
    extra_classes: &[&str],
    hidden: bool,
    keep_own_vocabulary: bool,
) -> String {
    let consumed = |name: &str| -> bool {
        if keep_own_vocabulary {
            name == USE_ATTR || name == USE_AXES_ATTR || name.starts_with(PARAM_ATTR_PREFIX)
        } else {
            // THE SCOPE MARK IS NOT VOCABULARY (finding `01KYVMTWB71R0D0CT1SV88J8DX`, the half that
            // was left out). `data-nyx-css` shares the prefix but is not an instruction to the shim:
            // it is the IDENTITY of a stylesheet, and `css_slot_styles` reads it off the MARKUP to
            // decide which slots to emit. A REFERENCED sub-component is stamped inside
            // [`render_reference`], so the OUTER `resolve_axes` of the agent's resolution
            // (`resolve_component`, `Vocabulary::Consume`) used to strip it right back off — and a
            // shared `Spinner`'s animation then reached neither the markup nor `css_slots`, which is
            // exactly the silent loss this slot exists to end.
            name != crate::css_slot::SCOPE_ATTR && name.starts_with(NYX_ATTR_PREFIX)
        }
    };
    let mut out = String::with_capacity(tag.end - tag.start + 32);
    out.push('<');
    out.push_str(&tag.name);
    let mut wrote_class = false;
    let mut has_hidden = false;
    for attr in &tag.attrs {
        if consumed(&attr.name) {
            continue;
        }
        if attr.name.eq_ignore_ascii_case("hidden") {
            has_hidden = true;
        }
        out.push(' ');
        out.push_str(&attr.name);
        let value = match (&attr.value, attr.name.eq_ignore_ascii_case("class")) {
            (Some(value), true) if !extra_classes.is_empty() => {
                wrote_class = true;
                let mut merged = value.clone();
                for class in extra_classes {
                    if !merged.split_whitespace().any(|c| c == *class) {
                        merged.push(' ');
                        merged.push_str(class);
                    }
                }
                Some(merged)
            }
            (Some(value), is_class) => {
                wrote_class |= is_class;
                Some(value.clone())
            }
            (None, is_class) => {
                wrote_class |= is_class;
                None
            }
        };
        if let Some(value) = value {
            out.push_str("=\"");
            out.push_str(&encode_attribute(&value));
            out.push('"');
        }
    }
    if !wrote_class && !extra_classes.is_empty() {
        out.push_str(" class=\"");
        out.push_str(&encode_attribute(&extra_classes.join(" ")));
        out.push('"');
    }
    if hidden && !has_hidden {
        out.push_str(" hidden");
    }
    if tag.kind == TagKind::SelfClosing && tag.slash_closed {
        out.push_str("/>");
    } else {
        out.push('>');
    }
    out
}

/// The named/numeric entities an attribute value can carry, decoded — the value handed to a param is
/// TEXT, and Handlebars escapes it again on the way back into the markup.
fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = [
            ("&amp;", "&"),
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&quot;", "\""),
            ("&#x27;", "'"),
            ("&#39;", "'"),
            ("&#x60;", "`"),
            ("&#96;", "`"),
            ("&#61;", "="),
            ("&nbsp;", "\u{a0}"),
        ]
        .into_iter()
        .find(|(entity, _)| rest.starts_with(entity));
        match decoded {
            Some((entity, text)) => {
                out.push_str(text);
                rest = &rest[entity.len()..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The double-quoted-attribute escaping of a value this module re-emits.
fn encode_attribute(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_site_with_its_params_and_its_fixed_axes() {
        let sites = reference_sites(
            r#"<span data-nyx-if="loading=on" data-nyx-use="Spinner" data-nyx-use-axes="size=sm, tone=muted" data-nyx-param-label="Chargement &amp; co"></span>"#,
        );
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].component, "Spinner");
        assert_eq!(
            sites[0].params,
            vec![("label".to_string(), "Chargement & co".to_string())],
            "the attribute value is entity-DECODED — a param carries text, not its escaping"
        );
        assert_eq!(
            sites[0].axes,
            vec![
                ("size".to_string(), "sm".to_string()),
                ("tone".to_string(), "muted".to_string())
            ]
        );
    }

    #[test]
    fn scanner_respects_quotes_comments_and_void_elements() {
        let tags = scan_tags(r#"<!-- <b> --><div title="a > b"><br/><img src="x"><i>y</i></div>"#);
        let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["div", "br", "img", "i", "i", "div"]);
        assert_eq!(
            tags[0].attr("title"),
            Some("a > b"),
            "a `>` inside a quoted value does not end the tag"
        );
        assert_eq!(tags[1].kind, TagKind::SelfClosing);
        assert_eq!(
            tags[2].kind,
            TagKind::SelfClosing,
            "a void element closes itself"
        );
        assert_eq!(tags[3].depth, 1);
        assert_eq!(tags[0].close, Some(5));
    }

    #[test]
    fn resolve_axes_bakes_the_classes_and_hides_the_absent_element() {
        let axes = parse_axes(Some(
            r#"{"axes":[{"name":"size","default":"md","values":[
                 {"name":"sm","classes":"h-3 w-3"},{"name":"md","classes":"h-4 w-4"}]},
                {"name":"tone","values":[{"name":"muted","classes":"opacity-70"},{"name":"strong"}]}]}"#,
        ));
        let markup = r#"<i class="animate-spin">◌</i><b data-nyx-if="tone=strong">!</b>"#;
        // Nothing is marked `data-nyx-axis`, so both axes land on the TOP-LEVEL elements.
        let out = resolve_axes(
            markup,
            &axes,
            &[("size".into(), "sm".into())],
            AmbientAxes::default(),
        );
        assert_eq!(
            out,
            r#"<i class="animate-spin h-3 w-3 opacity-70">◌</i><b class="h-3 w-3 opacity-70" hidden>!</b>"#
        );
        // With nothing fixed, the axis renders at its DEFAULT (`md`), else its first value.
        let out = resolve_axes(markup, &axes, &[], AmbientAxes::default());
        assert!(out.contains("h-4 w-4"), "the declared default wins: {out}");
    }

    #[test]
    fn resolve_axes_leaves_no_vocabulary_behind() {
        let axes = parse_axes(Some(
            r#"{"axes":[{"name":"state","values":[{"name":"on"},{"name":"off"}]}]}"#,
        ));
        let out = resolve_axes(
            r#"<div data-nyx-axis="state" data-nyx-state-case="on" data-nyx-page="home" data-nyx-set="state=off">x</div>"#,
            &axes,
            &[("state".into(), "on".into())],
            AmbientAxes::default(),
        );
        assert_eq!(
            out, "<div>x</div>",
            "every data-nyx-* is consumed here: {out}"
        );
    }

    /// [`bake_node_axes`] SPLITS a condition rather than swallowing it whole (finding
    /// `01KZ41YK39SGNKF6XV87A35GPN`). One attribute can name an axis this node owns AND one the shim
    /// pilots; the pass decides its own clauses and hands the rest on, in the grammar they came in.
    /// Consuming the whole attribute would freeze the chip-bar's half; leaving the whole attribute
    /// would let the shim ignore ours (a clause on an axis it does not declare is IGNORED, never
    /// falsified) and un-hide what we hid.
    #[test]
    fn baking_a_node_splits_a_condition_between_its_owner_and_the_shim() {
        let axes = parse_axes(Some(
            r#"{"axes":[{"name":"size","default":"md","values":[
                 {"name":"sm","classes":"h-3"},{"name":"md","classes":"h-4"}]}]}"#,
        ));
        let shim = parse_axes(Some(
            r#"{"axes":[{"name":"theme","values":[{"name":"day"},{"name":"night"}]}]}"#,
        ));
        // OUR clause holds at the default `md`, so only the shim's half is left on the attribute.
        let out = bake_node_axes(
            r#"<i data-nyx-if="size=md, theme=night" class="ic">◌</i>"#,
            &axes,
            &[],
            &shim,
        );
        assert_eq!(out, r#"<i data-nyx-if="theme=night" class="ic h-4">◌</i>"#);

        // OUR clause fails: the element is hidden for good, and NOTHING is left for the shim to
        // re-decide — no clause of theirs can bring back an `-if` that already lost one.
        let out = bake_node_axes(
            r#"<i data-nyx-if="size=sm, theme=night" class="ic">◌</i>"#,
            &axes,
            &[],
            &shim,
        );
        assert_eq!(out, r#"<i class="ic h-4" hidden>◌</i>"#);

        // An attribute naming ONLY the shim's axes is not ours to touch.
        let out = bake_node_axes(r#"<i data-nyx-if="theme=night">◌</i>"#, &axes, &[], &shim);
        assert_eq!(out, r#"<i data-nyx-if="theme=night" class="h-4">◌</i>"#);

        // …and an axis the SHIM declares is not baked at all, however this node also spells it.
        let both = parse_axes(Some(
            r#"{"axes":[{"name":"size","default":"md","values":[
                 {"name":"sm","classes":"h-3"},{"name":"md","classes":"h-4"}]}]}"#,
        ));
        let out = bake_node_axes(
            r#"<i data-nyx-axis="size" data-nyx-if="size=md">◌</i>"#,
            &axes,
            &[],
            &both,
        );
        assert_eq!(
            out, r#"<i data-nyx-axis="size" data-nyx-if="size=md">◌</i>"#,
            "one owner per axis — the shim's, untouched: {out}"
        );
    }

    /// A HIDE THIS PASS DECIDES TAKES THE WHOLE CONDITIONAL VOCABULARY WITH IT. The verdict is a
    /// CONJUNCTION over three attributes, and enforcing it attribute by attribute left the shim a
    /// clause to re-decide on: `applyStructure` does `setHidden(el, !show)` from what IT can see, so
    /// an `-if-not` naming only its own axes REMOVED the `hidden` written here.
    #[test]
    fn a_hidden_element_keeps_no_clause_the_shim_could_un_hide_it_with() {
        let axes = parse_axes(Some(
            r#"{"axes":[{"name":"size","default":"md","values":[{"name":"sm"},{"name":"md"}]}]}"#,
        ));
        let shim = parse_axes(Some(
            r#"{"axes":[{"name":"theme","values":[{"name":"day"},{"name":"night"}]}]}"#,
        ));
        // OUR `-if` fails; the `-if-not` beside it names only the SHIM's axis.
        let out = bake_node_axes(
            r#"<i data-nyx-if="size=sm" data-nyx-if-not="theme=day">◌</i>"#,
            &axes,
            &[],
            &shim,
        );
        assert_eq!(out, "<i hidden>◌</i>", "nothing left to re-decide: {out}");

        // Same through the OTHER mechanism: the state-case hides, the `-if-not` is the shim's.
        let out = bake_node_axes(
            r#"<i data-nyx-state-case="sm" data-nyx-if-not="theme=day">◌</i>"#,
            &axes,
            &[],
            &shim,
        );
        assert_eq!(out, "<i hidden>◌</i>", "nothing left to re-decide: {out}");
    }
}
