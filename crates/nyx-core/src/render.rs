//! LOGIC-LESS Handlebars rendering of a proto COMPOSITION into markup, ALL in the sidecar (v2.7
//! — Prototype : machinerie, Phase 3, task `01KY4DFPPM4KXFWWYCFQSMRKVX` — "Rendu Handlebars dans
//! le sidecar Rust (composition → markup)").
//!
//! # Why this module exists (PDR-0010)
//!
//! The proto is stored as a COMPOSITION — a tree of NAMED components + their params — NOT as an
//! HTML blob (`prototype_variants.composition_json`, Phase 2). To *show* it, the composition has
//! to become markup. PDR-0010 freezes exactly HOW:
//!
//!   * **Handlebars, logic-less** — the component templates carry no logic (no `{{#if}}` /
//!     `{{#each}}` / helpers). Iteration and conditionals are expressed by the SHAPE OF THE TREE,
//!     not by template logic.
//!   * **Registration data-driven from `components`** — every component's Handlebars `template`
//!     (the `design_components.template` column) is registered as a PARTIAL keyed by its name, in
//!     one pass. Nothing is hand-wired; the catalogue drives the registry.
//!   * **Compilation sidecar-only** — this runs in `nyx-core` (compiled INTO the sidecar). The
//!     Electron renderer compiles NOTHING; it only *displays* the markup this produces (task
//!     `01KY4DFQ6NXKE9B7FC0KHRQH8T`).
//!
//! # The composition schema (a tree of nodes)
//!
//! A composition is a single ROOT [`CompositionNode`]; each node is one of the THREE natures the
//! PRD names (`[DS]` / `[NEW]` / `[inline]`):
//!
//! ```json
//! {
//!   "component": "Card",          // [DS] existing / [NEW] proposed → a design_components.name
//!   "params": { "title": "Hi", "state": "loading" },  // the node's params (the state is a param)
//!   "children": [
//!     { "inline": "<p>bespoke {{note}}</p>", "params": { "note": "one-off" } }  // [inline] one-off
//!   ]
//! }
//! ```
//!
//!   * a `component` node renders that component's registered template with the node's `params`;
//!   * an `inline` node renders its own bespoke markup string (the `[inline]` one-off nature — a
//!     node that is NOT a catalogue component) as a template with the node's `params` — held to the
//!     SAME logic-less contract as a component template (block helpers and partial includes
//!     rejected, see [`find_refused_construct`]);
//!   * `children` are rendered FIRST (depth-first) and injected into the parent template as the
//!     raw `children` variable — a template composes its slot with `{{{children}}}` (triple-stache
//!     = unescaped). Text params interpolate with `{{p}}` (HTML-escaped); an html-typed param
//!     interpolates raw with `{{{p}}}` — the "param texte/html" of the detail view (task
//!     `01KY50ZPAYP0JN5G353JTZ6CGG`).
//!
//! The `node_key` PATH convention (`Root/Header/Title`, the anchor of `prototype_feedback`) is the
//! `/`-join of each node's [`CompositionNode::key`] (its explicit `key`, else its `component` name,
//! else `inline`) from the root — see [`node_key_path`]. That is how the detail view addresses a
//! node to persist its params PER NODE.
//!
//! # FOUR entry points, ONE resolution — they differ on PURPOSE, not on mechanism
//!
//!   * [`render_composition_lenient`] feeds the FRAME (`design_render`): it stops before the shim's
//!     applier — the renderer pilots the axes LIVE, which is the whole point of one element instead
//!     of a cartesian product — and it ALWAYS COMPLETES, an unrenderable node becoming a NAMED
//!     placeholder rather than a blanked page (finding `01KYT78699XG6Q6P3VZC64W8H8`).
//!   * [`render_composition`] is that same render, STRICT: one unknown component fails the tree. The
//!     all-or-nothing contract, kept for the callers that want it — and the reference the lenient
//!     one is proven byte-identical to whenever nothing is missing.
//!   * [`resolve_component`] answers an AGENT about ONE component: the shim's applier is run ahead
//!     of time, so what comes back is what is DISPLAYED at a combination. It refuses loudly — the
//!     component IS the subject, and a subject that cannot be rendered has nothing to show.
//!   * [`resolve_composition`] answers an AGENT about a WHOLE PAGE: the same resolution at every
//!     node, but it ALWAYS COMPLETES — a composition under design legitimately holds `[NEW]` nodes,
//!     so an unrenderable one becomes a NAMED placeholder instead of failing the forty that are
//!     right (finding `01KYT45C6RC7K84V1CA3561SPF`).
//!
//! The last two are ONE WALK ([`resolve_node`]) parameterized by [`Vocabulary`] — see
//! [`render_composition_lenient`] for why that is one function and not two.

mod reference;
pub mod viewport;

use std::collections::BTreeMap;

use handlebars::Handlebars;
use serde::Deserialize;
use serde_json::{Map, Value};

pub use reference::{
    reference_sites, ReferenceSite, MAX_REFERENCE_DEPTH, MAX_REFERENCE_EXPANSIONS,
    PARAM_ATTR_PREFIX, USE_ATTR, USE_AXES_ATTR,
};

/// A component available to the renderer: its NAME (the partial key), its logic-less Handlebars
/// `template`, and the two blobs a REFERENCE to it has to resolve against. Built from a
/// `design_components` row — the registration is data-driven from the catalogue, never hand-wired.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderComponent {
    pub name: String,
    pub template: String,
    /// The component's VARIANCE AXES (`design_components.states_json`). Only a component reached
    /// through a `data-nyx-use` REFERENCE needs them — that is where nyx, and not the shim, resolves
    /// an axis to concrete classes (see [`reference`]). `None` for a component with no variance.
    pub states_json: Option<String>,
    /// The component's declared params (`design_components.params_json`) — what a reference SITE is
    /// checked against by the write path. The render itself never needs them (a param nobody
    /// provides substitutes to the empty string, a prototype leniency).
    pub params_json: Option<String>,
    /// The component's SCOPED CSS SLOT (`design_components.css_slot_json`) — the strict escape hatch
    /// for what Tailwind utilities cannot express, `None` for the overwhelming majority of components
    /// (finding `01KYVMTWB71R0D0CT1SV88J8DX`). A component that carries one has its root element(s)
    /// STAMPED with the scope ([`crate::css_slot::SCOPE_ATTR`]) and its compiled stylesheet appended
    /// to the markup — see [`css_slot_styles`].
    pub css_slot: Option<crate::css_slot::CssSlot>,
}

impl RenderComponent {
    /// A component with a template and NO variance — the shape most call sites (and every test that
    /// predates references) want.
    pub fn new(name: impl Into<String>, template: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            template: template.into(),
            ..Self::default()
        }
    }
}

/// ONE page of the prototype, as a render needs to know it: the ID a node LINKS to
/// ([`CompositionNode::navigate_to`]) plus the two READABLE fields every surface displays. Built from
/// a `prototype_pages` row.
///
/// # Why the render is handed a page DIRECTORY at all (finding `01KYW34347CZSNXCYS35KDG3EY`)
///
/// A node states its destination as an ID, because an id is what does not move; the shim and every
/// human-facing surface need a SLUG and a TITLE, because an id is not something anybody reads. The
/// resolution id → (slug, title) therefore happens HERE, at every render — which is what makes a
/// rename FREE (the next render emits the new slug, and the stored link never moved) and a DELETION
/// VISIBLE (the id resolves to nothing, and the render says so instead of emitting a link to
/// nowhere).
///
/// A render given an EMPTY directory is a render that knows of no page: every link is then reported
/// UNKNOWN, by name, with the fact that no directory was supplied spelled out. That is the honest
/// answer for a caller that did not say which prototype it is rendering — never a reason to drop the
/// link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderPage {
    /// `prototype_pages.id` — the durable identity a node's `navigate_to` names.
    pub id: String,
    /// The page's slug — the value `data-nyx-page` carries, which the shim posts to its host.
    pub slug: String,
    /// The page's human title — what a surface DISPLAYS for this destination.
    pub title: String,
}

impl RenderPage {
    /// A page from its three fields, in the order the schema declares them.
    pub fn new(id: impl Into<String>, slug: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            slug: slug.into(),
            title: title.into(),
        }
    }
}

/// One node of a composition tree. Exactly one nature holds: `component` set → a `[DS]`/`[NEW]`
/// node rendering that catalogue component; `inline` set → a `[inline]` one-off node rendering its
/// own bespoke markup. `params` are the node's params (a state is just a param named `state`);
/// `children` are nested nodes composed into the parent via `{{{children}}}`.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct CompositionNode {
    /// The catalogue component this node instantiates (`[DS]`/`[NEW]`). `None` for an inline node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// Bespoke one-off markup (`[inline]`). `None` for a component node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<String>,
    /// An explicit stable path-segment label overriding the default (`component` / `inline`); lets
    /// the `node_key` path disambiguate same-named siblings. Purely addressing — never rendered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The node's params (includes `state`). Rendered into the template's context.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
    /// The prototype PAGE this node NAVIGATES TO when clicked — a `prototype_pages.id`, `None` for
    /// the overwhelming majority of nodes (finding `01KYW34347CZSNXCYS35KDG3EY`).
    ///
    /// # Why the link lives on the NODE and not on the component
    ///
    /// A destination is a PLACEMENT decision, not a property of a component. The same `Button` is a
    /// "Continue" on the pricing page and a "Back" on the checkout one; a `BackButton` authored with
    /// its destination baked in would need one component per destination, which is the copy-instead-
    /// of-reference drift `data-nyx-use` exists to end. Three further reasons agree:
    ///
    ///   * the composition tree IS the coder contract (`prototype_variants.composition_json`), and a
    ///     PARCOURS is placement information — it belongs where the placement lives, so it crosses
    ///     the seam with it;
    ///   * a `design_components` row is RE-SYNTHESIZED from the app's real sources
    ///     (`design_refresh_component`): a link authored into a template would be overwritten by the
    ///     next synthesis, i.e. it would not survive in the model at all;
    ///   * the catalogue is the DESIGN SYSTEM — shared across every Spec of the project — while
    ///     pages belong to ONE prototype. A component row pointing at a page id would be a
    ///     project-wide row holding a prototype-local pointer.
    ///
    /// A template may still carry a literal `data-nyx-page="<slug>"`: that is markup, and on a node
    /// that states NO destination the shim honours it. It is not the modelled link — it names a slug
    /// (which renames) rather than an id, nothing validates it, and it does not cross the coder seam.
    /// As soon as a node DOES state a destination, the render owns the navigation marks on its root:
    /// [`reference::stamp_navigation`] clears the whole [`NAVIGATION_ATTRS`] vocabulary before
    /// writing what it decided, so a baked-in link neither wins nor survives beside an invalidation
    /// marker (finding `01KZ3JN69E32FYBF01NBE4FWJD`).
    ///
    /// # Cycles are NOT a problem, and that is a decision, not an omission
    ///
    /// `A → B → A` is a legitimate parcours (any "Back" button makes one), and nothing here has to
    /// bound it: a navigation is a USER GESTURE at click time, never an expansion at render time.
    /// The render walks the composition TREE and stops there — it never follows a link, so a cycle
    /// costs exactly one stamped attribute per node, whatever its shape. That is the whole
    /// difference with `data-nyx-use`, whose cycle IS an infinite expansion and is refused
    /// ([`RenderError::ReferenceCycle`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigate_to: Option<String>,
    /// Nested child nodes, rendered depth-first and injected as the raw `children` variable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CompositionNode>,
}

impl CompositionNode {
    /// This node's PATH-SEGMENT label for the `node_key` convention: the explicit `key`, else the
    /// `component` name, else `"inline"`. Never used for rendering — only for [`node_key_path`].
    pub fn key(&self) -> &str {
        self.key
            .as_deref()
            .or(self.component.as_deref())
            .unwrap_or("inline")
    }
}

/// Why a render failed. Distinct, explicit variants (never a silent empty string) so the sidecar
/// maps each to a readable error — an unknown component or a template with logic is a BUG in the
/// composition/catalogue the caller must see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The composition JSON did not parse as a [`CompositionNode`] tree.
    BadComposition(String),
    /// A node referenced a `component` name that is not in the registered catalogue.
    UnknownComponent(String),
    /// A rendered template carries LOGIC (a `{{#…}}` / `{{^…}}` block helper) — rejected to keep
    /// templates logic-less (PDR-0010). This covers BOTH a component's template (checked at
    /// registration) AND an `[inline]` node's bespoke markup (checked at render, `component` =
    /// `"<inline>"`). Carries `(component, detail)`.
    LogicInTemplate { component: String, detail: String },
    /// A template INCLUDES a Handlebars PARTIAL (`{{> Spinner}}`) — the accidental door into
    /// composition that bypasses every guarantee `data-nyx-use` exists to give (finding
    /// `01KYSZNX4TAVSGEVBVJM7W2B21`). Carries `(component, detail)`.
    PartialInTemplate { component: String, detail: String },
    /// Handlebars failed to render a node (a malformed template). Carries `(component, message)`.
    Template { component: String, message: String },
    /// A template REFERENCED (`data-nyx-use`) a component that is not in the registered catalogue —
    /// it was never synthesized, or it was DELETED after the reference was written. Refused loudly:
    /// a reference that resolves to nothing would be an invisible hole in every host that carries it.
    UnknownReference { host: String, reference: String },
    /// A CYCLE of references (`A → B → A`, a self-reference included). Carries the chain, so the
    /// message names the two components to break apart rather than "recursion too deep".
    ReferenceCycle { path: Vec<String> },
    /// The reference expansion hit its BOUND — nested deeper than
    /// [`reference::MAX_REFERENCE_DEPTH`], or fanned out beyond
    /// [`reference::MAX_REFERENCE_EXPANSIONS`]. Acyclic but unbounded work is refused too, and named.
    ReferenceLimit { path: Vec<String>, detail: String },
    /// A reference placeholder that cannot hold an expansion (a void element, an element with no
    /// matching close tag). Carries `(host, detail)`.
    BadReference { host: String, detail: String },
    /// A `node_key` that resolves to NO node of the composition ([`resolve_composition`] asked for
    /// ONE subtree). Carries the key asked for and the keys the tree really holds, so the answer
    /// names what to ask for instead of saying "not found".
    UnknownNode {
        node_key: String,
        available: Vec<String>,
    },
    /// A node NAVIGATES to a page id the prototype does NOT have — it was never created, it was
    /// deleted after the link was written, or the id belongs to another prototype. Refused loudly by
    /// the STRICT render, NAMED by the lenient ones ([`NavigationStatus::UnknownPage`]): a link that
    /// resolves to nothing would be a dead click, which is the one thing a parcours may not be.
    UnknownPage {
        node_key: String,
        page_id: String,
        available: Vec<String>,
    },
    /// A node navigates somewhere but its markup carries NO element that could hold the link (a
    /// template that renders bare text, a structural node with no children). Named rather than
    /// dropped — the destination was stated, so its disappearance must be too.
    BadNavigation { node_key: String, page_id: String },
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::BadComposition(e) => write!(f, "bad composition json: {e}"),
            RenderError::UnknownComponent(name) => {
                write!(f, "composition references unknown component '{name}'")
            }
            RenderError::LogicInTemplate { component, detail } => {
                write!(
                    f,
                    "component '{component}' template is not logic-less: {detail}"
                )
            }
            RenderError::PartialInTemplate { component, detail } => {
                write!(
                    f,
                    "component '{component}' template includes a Handlebars PARTIAL: {detail}"
                )
            }
            RenderError::Template { component, message } => {
                write!(
                    f,
                    "component '{component}' template failed to render: {message}"
                )
            }
            RenderError::UnknownReference { host, reference } => {
                write!(
                    f,
                    "component '{host}' references (data-nyx-use=\"{reference}\") a component the \
                     catalogue does NOT have: '{reference}' — synthesize it, author it \
                     (proto_author_component), or inline its markup in '{host}'"
                )
            }
            RenderError::ReferenceCycle { path } => {
                write!(
                    f,
                    "reference CYCLE between components: {} — a cycle has no rendering, so it is \
                     refused rather than unrolled. One of them must inline the other's markup \
                     instead of referencing it",
                    path.join(" → ")
                )
            }
            RenderError::ReferenceLimit { path, detail } => {
                write!(f, "reference chain {} — {detail}", path.join(" → "))
            }
            RenderError::BadReference { host, detail } => {
                write!(f, "component '{host}': {detail}")
            }
            RenderError::UnknownNode {
                node_key,
                available,
            } => {
                write!(
                    f,
                    "this composition has no node at '{node_key}' — a node_key is the '/'-joined \
                     path of the node KEYS from the root (Root/Header/Title). Its nodes are: {}",
                    available.join(", ")
                )
            }
            RenderError::UnknownPage {
                node_key,
                page_id,
                available,
            } => {
                write!(
                    f,
                    "node '{node_key}' navigates to page '{page_id}', which this prototype does NOT \
                     have — {}. A node's `navigate_to` is a prototype_pages.id (proto_add_page \
                     returns it, proto_get lists them), never a slug",
                    available_pages_phrase(available)
                )
            }
            RenderError::BadNavigation { node_key, page_id } => {
                write!(
                    f,
                    "node '{node_key}' navigates to page '{page_id}' but renders NO element that \
                     could carry the link: a destination nothing can be clicked on is a link that \
                     does not exist. {NO_ANCHOR_CORRECTION}"
                )
            }
        }
    }
}

/// The tail of an [`RenderError::UnknownPage`] / [`NavigationStatus::UnknownPage`] message: the
/// pages that DO exist, or the fact that the caller supplied no directory at all. Written once
/// because a broken link is answered in three places (the strict refusal, the lenient report, the
/// write guard) and the three must say the same thing.
fn available_pages_phrase(available: &[String]) -> String {
    if available.is_empty() {
        "this render was given NO page directory at all, so no id could resolve (the caller must \
         say which prototype it is rendering)"
            .to_string()
    } else {
        format!("its pages are: {}", available.join(", "))
    }
}

impl std::error::Error for RenderError {}

/// The raw `children` variable name injected into a node's context — a template composes its slot
/// with `{{{children}}}` (triple-stache, unescaped).
const CHILDREN_KEY: &str = "children";

/// A Handlebars construct a component template may NOT carry. ONE detector
/// ([`find_refused_construct`]) for every path that accepts a template — the write paths
/// (`design_refresh_component`, `proto_author_component`), the strict render, the lenient one —
/// so what is refused at the write is exactly what is refused at the render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusedConstruct {
    /// A BLOCK opener (`{{#if}}` / `{{#each}}` / `{{#with}}`) or an inverse section (`{{^…}}`) —
    /// LOGIC, which PDR-0010 forbids ("logic-less par construction"). Carries the token (`#if`).
    Block(String),
    /// A PARTIAL include (`{{> Spinner}}`) — carries the name it includes, when it names one.
    ///
    /// # Why a partial is refused and not merely discouraged (finding `01KYSZNX4TAVSGEVBVJM7W2B21`)
    ///
    /// A partial expands INSIDE `render_template`, with the HOST's context. That gives, silently,
    /// the exact three failures [`reference`] was designed to prevent: the host's params flow into
    /// the sub-component by name collision, the sub-component's AXES are resolved NOWHERE (nyx never
    /// sees a boundary, so no class is applied and no `data-nyx-*` is consumed), and a CYCLE is
    /// bounded by nothing — [`MAX_REFERENCE_DEPTH`] and [`MAX_REFERENCE_EXPANSIONS`] bound the
    /// `data-nyx-use` expansion pass, which a partial never enters. And it was reachable BY
    /// ACCIDENT: `{{> Spinner}}` is the natural spelling for anyone who knows Handlebars.
    ///
    /// The manual teaches `data-nyx-use`, but teaching is not preventing: everywhere else this
    /// machinery REFUSES what it does not admit (an unknown param type, an unknown axis, an unknown
    /// reference) rather than relying on the agent's discipline. So the syntax is refused, by name,
    /// with the correct form named in the message.
    Partial(String),
}

impl RefusedConstruct {
    /// The offending token, as it appears in the template (`#if`, `> Spinner`).
    pub fn token(&self) -> &str {
        match self {
            RefusedConstruct::Block(token) | RefusedConstruct::Partial(token) => token,
        }
    }

    /// The reason a node carrying this construct is UNRENDERED — the machine code the frame and the
    /// agent both read (see [`UnrenderedReason`]).
    pub fn reason(&self) -> UnrenderedReason {
        match self {
            RefusedConstruct::Block(_) => UnrenderedReason::TemplateHasLogic,
            RefusedConstruct::Partial(_) => UnrenderedReason::TemplateHasPartial,
        }
    }

    /// The SHORT detail a [`RenderError`] carries — the offending token and the one-line rule.
    pub fn detail(&self) -> String {
        match self {
            RefusedConstruct::Block(token) => {
                format!("block helper `{{{{{token}}}}}` is not allowed")
            }
            RefusedConstruct::Partial(token) => format!(
                "`{{{{{token}}}}}` includes another template as a Handlebars PARTIAL — use \
                 data-nyx-use instead"
            ),
        }
    }

    /// The FULL sentence an agent is answered with: what was found, why it cannot be run, and the
    /// form to write instead. This is the ONE wording — the write paths, the placeholder of an
    /// unrenderable node and the render error all read it from here.
    pub fn correction(&self) -> String {
        match self {
            RefusedConstruct::Block(token) => format!(
                "the template carries LOGIC (`{{{{{token}}}}}`): component templates are LOGIC-LESS \
                 (PDR-0010) — no block helper ({{{{#if}}}} / {{{{#each}}}} / {{{{^…}}}}). Express \
                 conditionals and repetition through the composition TREE, and keep the template to \
                 plain substitutions ({{{{param}}}}, {{{{{{children}}}}}})."
            ),
            RefusedConstruct::Partial(token) => {
                let name = self.partial_name();
                format!(
                    "the template includes a Handlebars PARTIAL (`{{{{{token}}}}}`), which nyx does \
                     NOT run: a partial expands inside this template with THIS component's context, \
                     so the host's params leak into it by name collision, its own axes are resolved \
                     NOWHERE (no class applied, no data-nyx-* consumed) and a cycle through it is \
                     bounded by nothing. REFERENCE the component instead — that is what \
                     data-nyx-use is: <span data-nyx-use=\"{name}\" \
                     data-nyx-param-<name>=\"{{{{<yourParam>}}}}\" \
                     data-nyx-use-axes=\"<axis>=<value>\"></span>. The reference is resolved in the \
                     sidecar: params are handed over EXPLICITLY, the sub-component's axes are \
                     applied, and an unknown or cyclic target is named."
                )
            }
        }
    }

    /// The component name a partial include names, for the correction's example (`<Component>` when
    /// the include is a dynamic or quoted form this does not read).
    fn partial_name(&self) -> &str {
        match self {
            RefusedConstruct::Block(_) => "<Component>",
            RefusedConstruct::Partial(token) => {
                let name = token.trim_start_matches('>').trim().trim_matches('"');
                if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    "<Component>"
                } else {
                    name
                }
            }
        }
    }

    /// The [`RenderError`] this construct is, for `component`.
    fn render_error(&self, component: &str) -> RenderError {
        match self {
            RefusedConstruct::Block(_) => RenderError::LogicInTemplate {
                component: component.to_string(),
                detail: self.detail(),
            },
            RefusedConstruct::Partial(_) => RenderError::PartialInTemplate {
                component: component.to_string(),
                detail: self.correction(),
            },
        }
    }
}

/// SCAN a template for the FIRST construct nyx refuses to run — a BLOCK opener (`{{#…}}` / `{{^…}}`,
/// the logic PDR-0010 forbids) or a PARTIAL include (`{{> …}}`, the door around `data-nyx-use`).
/// Returns `None` when the template is made of nothing but plain substitutions.
///
/// Whitespace-control (`{{~#`, `{{~>`) is handled, and so is the raw-block/triple-stache `{`. What
/// stays allowed is what a logic-less template is made of: `{{p}}`, `{{{p}}}`, `{{! comment }}`, and
/// the `data-nyx-*` vocabulary (which is HTML, not Handlebars).
pub fn find_refused_construct(template: &str) -> Option<RefusedConstruct> {
    let bytes = template.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // Skip the `{{`, then any raw-block `{` and whitespace-control `~`.
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j] == b'{' || bytes[j] == b'~') {
                j += 1;
            }
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            match bytes.get(j) {
                Some(b'#' | b'^') => {
                    // Grab the block name for a readable error.
                    let start = j;
                    let mut k = j + 1;
                    while k < bytes.len() && bytes[k] != b'}' && !bytes[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    return Some(RefusedConstruct::Block(
                        String::from_utf8_lossy(&bytes[start..k]).into_owned(),
                    ));
                }
                Some(b'>') => {
                    // Grab the whole include body (`> Spinner`) — a partial can be named, quoted or
                    // dynamic, and the message quotes back what was really written.
                    let start = j;
                    let mut k = j + 1;
                    while k < bytes.len() && bytes[k] != b'}' {
                        k += 1;
                    }
                    return Some(RefusedConstruct::Partial(
                        String::from_utf8_lossy(&bytes[start..k])
                            .trim_end()
                            .to_string(),
                    ));
                }
                _ => {}
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

/// Build the logic-less [`Handlebars`] every render uses, after VALIDATING every catalogue template
/// ([`accept_template`]): a refused construct or a template Handlebars cannot compile is an explicit
/// [`RenderError`], never something silently rendered. Missing variables render EMPTY (strict mode
/// OFF) so an unset param is not an error — a prototype leniency.
///
/// # Why the catalogue is NOT registered as partials any more (finding `01KYSZNX4TAVSGEVBVJM7W2B21`)
///
/// PDR-0010's "registration data-driven depuis components" was implemented as
/// `register_partial(name, template)` — and in handlebars-rust `register_partial` IS
/// `register_template_string`, so every catalogue component was reachable as `{{> Name}}` from every
/// other one. Nothing in this module ever USED that: each node renders its own template through
/// `render_template`, and a component-to-component composition goes through `data-nyx-use`
/// ([`reference`]), which is the only form that resolves axes, hands params over explicitly and
/// detects cycles. The registration was therefore pure attack surface, and it is gone: the refusal
/// above is the lexical guard, and an EMPTY partial table is the structural one — a spelling that
/// slipped past the scanner now resolves to NOTHING instead of smuggling a component in.
fn build_registry(components: &[RenderComponent]) -> Result<Handlebars<'static>, RenderError> {
    let mut hb = Handlebars::new();
    // No custom helpers and no partials registered → the only helpers that exist are the built-in
    // block ones, and those are rejected below. A template can therefore only SUBSTITUTE.
    hb.set_strict_mode(false);
    for c in components {
        accept_template(&c.name, &c.template)?;
    }
    Ok(hb)
}

/// VALIDATE one template the strict way: no refused construct, and it compiles. The one place the
/// strict paths ([`render_composition`], [`resolve_component`]) decide a template is acceptable —
/// the lenient paths ask [`template_refusal`], which answers the same question with a REASON instead
/// of an error.
fn accept_template(component: &str, template: &str) -> Result<(), RenderError> {
    if let Some(refused) = find_refused_construct(template) {
        return Err(refused.render_error(component));
    }
    handlebars::Template::compile(template).map_err(|e| RenderError::Template {
        component: component.to_string(),
        message: e.to_string(),
    })?;
    Ok(())
}

/// WHY this template cannot be run, as an unrenderable node's `(reason, detail)` — `None` when it
/// can. The LENIENT twin of [`accept_template`], and the ONE place a template-level defect is named:
/// both leniences ([`resolve_composition`] for the agent, [`render_composition_lenient`] for the
/// frame) read their reason and their wording from here, so the two can never drift.
fn template_refusal(component: &str, template: &str) -> Option<(UnrenderedReason, String)> {
    if let Some(refused) = find_refused_construct(template) {
        return Some((
            refused.reason(),
            format!(
                "component '{component}': {} Re-synthesize it (design_refresh_component).",
                refused.correction()
            ),
        ));
    }
    handlebars::Template::compile(template).err().map(|e| {
        (
            UnrenderedReason::RenderFailed,
            format!(
                "component '{component}' carries a template Handlebars cannot COMPILE: {e} — the \
                 node is not rendered, everything around it is. Re-synthesize it \
                 (design_refresh_component)"
            ),
        )
    })
}

// ---------------------------------------------------------------------------
// THE SCOPED CSS SLOT — the mark on the roots, the stylesheet at the end
// (finding `01KYVMTWB71R0D0CT1SV88J8DX`)
// ---------------------------------------------------------------------------

/// ONE CSS slot a render MET — what the review UI badges and what the coder seam carries. Reported
/// by every lenient path, so an exception is never something a reader has to go looking for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedCssSlot {
    /// The scope id stamped on the component's root element(s) and anchoring every rule.
    pub scope_id: String,
    /// The catalogue component the slot belongs to.
    pub component: String,
    /// The CSS selector the slot is bounded to, literally — what the review card displays as
    /// "scope" and what the coder reads to know where the exception applies.
    pub selector: String,
    /// WHY the exception exists, in the agent's words. Empty only for a row written before the
    /// reason became mandatory.
    pub reason: String,
    /// Which of the four infeasible-in-utilities constructs it exercises
    /// ([`crate::css_slot::SlotFeature`] codes).
    pub features: Vec<String>,
    /// The size of the AUTHORED CSS.
    pub bytes: usize,
    /// The compiled, scoped CSS — exactly what the frame applies and what the coder must implement.
    /// Empty when the slot was REFUSED at render time.
    pub css: String,
    /// The refusal CODE when the stored slot does not compile any more (a row written before a rule
    /// existed, a token that disappeared). `None` when the slot was emitted. A refused slot emits NO
    /// stylesheet — and SAYS so, rather than vanishing, which is the whole point of this finding.
    pub refused: Option<String>,
    /// The refusal's full wording, `None` when the slot was emitted.
    pub detail: Option<String>,
}

/// STAMP a component's CSS-slot scope onto the top-level elements of the markup it just produced —
/// the MARKUP half of the scope guarantee (the other half rewrites the selectors). A component with
/// no slot is returned untouched, so a catalogue that uses none renders byte for byte as before.
pub(crate) fn stamp_component_scope(component: &RenderComponent, markup: String) -> String {
    if component.css_slot.is_none() {
        return markup;
    }
    reference::stamp_scope(
        &markup,
        &crate::css_slot::component_scope_id(&component.name),
    )
}

/// The scope ids a piece of markup really carries, in DOCUMENT ORDER, deduped. The markup is the
/// ground truth: a slot's stylesheet is emitted because its component is really THERE, never because
/// the catalogue happens to hold one.
fn used_scope_ids(markup: &str) -> Vec<String> {
    let needle = format!("{}=\"", crate::css_slot::SCOPE_ATTR);
    let mut out: Vec<String> = Vec::new();
    let mut from = 0usize;
    while let Some(at) = markup[from..].find(&needle) {
        let start = from + at + needle.len();
        let Some(end) = markup[start..].find('"') else {
            break;
        };
        for id in markup[start..start + end].split_whitespace() {
            if !out.iter().any(|k| k == id) {
                out.push(id.to_string());
            }
        }
        from = start + end;
    }
    out
}

/// COMPILE the slots the markup really uses and return `(stylesheets, report)`.
///
/// The stylesheets are APPENDED to the markup by the caller — after the last element, never before
/// the first: every existing byte and every node position stays where it was, so the document ORDER
/// of the composition is untouched and a render with no slot is byte-identical to one from before
/// this existed. A `<style>` applies wherever it sits.
///
/// A stored slot that no longer compiles emits NOTHING and is REPORTED as refused. Dropping it
/// silently would be the exact defect this finding is about; the write path refuses such a slot in
/// the first place, so this is the defence in depth, not the normal road.
fn css_slot_styles(
    markup: &str,
    by_name: &BTreeMap<&str, &RenderComponent>,
) -> (String, Vec<RenderedCssSlot>) {
    let used = used_scope_ids(markup);
    if used.is_empty() {
        return (String::new(), Vec::new());
    }
    let by_scope: BTreeMap<String, &RenderComponent> = by_name
        .values()
        .filter(|c| c.css_slot.is_some())
        .map(|c| (crate::css_slot::component_scope_id(&c.name), *c))
        .collect();
    let mut styles = String::new();
    let mut report = Vec::new();
    for scope_id in used {
        let Some(component) = by_scope.get(&scope_id) else {
            continue;
        };
        let slot = component.css_slot.as_ref().expect("filtered on Some");
        match crate::css_slot::compile(&scope_id, &slot.css) {
            Ok(compiled) => {
                styles.push_str(&compiled.style_element());
                report.push(RenderedCssSlot {
                    scope_id,
                    component: component.name.clone(),
                    selector: compiled.selector.clone(),
                    reason: slot.reason.clone(),
                    features: compiled.feature_codes(),
                    bytes: compiled.bytes,
                    css: compiled.css,
                    refused: None,
                    detail: None,
                });
            }
            Err(refusal) => report.push(RenderedCssSlot {
                selector: crate::css_slot::scope_selector(&scope_id),
                scope_id,
                component: component.name.clone(),
                reason: slot.reason.clone(),
                features: Vec::new(),
                bytes: slot.css.len(),
                css: String::new(),
                refused: Some(refusal.code().to_string()),
                detail: Some(refusal.message()),
            }),
        }
    }
    (styles, report)
}

/// RENDER a `composition_json` (a [`CompositionNode`] tree) into markup against the `components`
/// catalogue and the prototype's `pages`: `(components, pages, composition_json) → markup`.
/// Compilation is ENTIRELY here (sidecar) — the renderer receives only the resulting markup.
///
/// STRICT: one node it cannot render fails the WHOLE tree. That is the right contract for a caller
/// that wants the render to be all-or-nothing (the gate reads it as the Phase-3 deliverable, and
/// [`render_composition_lenient`] is proven byte-identical to it whenever it succeeds) — but it is
/// NOT what a frame wants, because a composition under design legitimately holds `[NEW]` nodes. The
/// `design_render` seam therefore calls [`render_composition_lenient`]; anything user-facing should
/// too.
///
/// `pages` is the prototype's page DIRECTORY, which is what a node's `navigate_to` resolves against
/// (see [`RenderPage`]). Strict here too: a link to a page the directory does not hold is
/// [`RenderError::UnknownPage`], where the lenient renders NAME it in place and carry on.
pub fn render_composition(
    components: &[RenderComponent],
    pages: &[RenderPage],
    composition_json: &str,
) -> Result<String, RenderError> {
    let root: CompositionNode = serde_json::from_str(composition_json)
        .map_err(|e| RenderError::BadComposition(e.to_string()))?;
    let hb = build_registry(components)?;
    // Index the catalogue by name once so the walk resolves a node's template — and a REFERENCE's
    // target — in O(1).
    let by_name: BTreeMap<&str, &RenderComponent> =
        components.iter().map(|c| (c.name.as_str(), c)).collect();
    let root_key = root.key().to_string();
    let mut markup = render_node(&hb, &by_name, pages, &root, &root_key)?;
    // The slots the tree really reaches, compiled and appended — the SAME step the lenient render
    // runs, which is what keeps the two byte-identical.
    let (styles, _) = css_slot_styles(&markup, &by_name);
    markup.push_str(&styles);
    Ok(markup)
}

/// Render ONE node (depth-first): render its children, then render THIS node's template with a
/// context = the node's `params` PLUS the raw `children` markup. `component` nodes resolve their
/// template from `by_name` (unknown → [`RenderError::UnknownComponent`]); `inline` nodes render
/// their own bespoke markup string; an empty node (neither) renders just its children. A node that
/// NAVIGATES has its root element(s) stamped with the destination it resolves to.
fn render_node(
    hb: &Handlebars<'static>,
    by_name: &BTreeMap<&str, &RenderComponent>,
    pages: &[RenderPage],
    node: &CompositionNode,
    node_key: &str,
) -> Result<String, RenderError> {
    // Children first (depth-first), concatenated in order.
    let mut children_markup = String::new();
    for child in &node.children {
        let child_key = format!("{node_key}/{}", child.key());
        children_markup.push_str(&render_node(hb, by_name, pages, child, &child_key)?);
    }

    // Resolve the template for this node's nature.
    let (component_label, template): (String, String) = if let Some(name) = &node.component {
        let component = by_name
            .get(name.as_str())
            .ok_or_else(|| RenderError::UnknownComponent(name.clone()))?;
        (name.clone(), component.template.clone())
    } else if let Some(inline) = &node.inline {
        // An `[inline]` node is bespoke one-off markup, but it is STILL rendered as a Handlebars
        // template (its params interpolate). PDR-0010 forbids logic in ANY template ("zéro logique
        // dans les templates", "logic-less par construction"), so an inline node is held to the
        // SAME contract as a component template: a `{{#…}}` / `{{^…}}` block helper and a `{{> …}}`
        // partial include are rejected here, never executed. Plain substitutions stay allowed.
        accept_template(INLINE_LABEL, inline)?;
        (INLINE_LABEL.to_string(), inline.clone())
    } else {
        // A structural node with no template of its own: it is just its children.
        return strict_navigation(pages, node, node_key, children_markup);
    };

    // Context = the node params + the raw children markup. Params win over any reserved key EXCEPT
    // `children`, which we own (a param may not shadow the composed slot).
    let mut ctx = node.params.clone();
    ctx.insert(CHILDREN_KEY.to_string(), Value::String(children_markup));
    // THIS NODE'S OWN AXES — the values it fixes in its params, at the combination
    // `active_combination` resolves (an axis left out renders at its own `default`). Substitutable
    // like any param (`class="btn btn-{{variant}}"`), and baked as CLASSES + structure below. The
    // lenient walk does exactly the same, which is what keeps the two byte-identical.
    let axes = node
        .component
        .as_deref()
        .and_then(|n| by_name.get(n))
        .map(|c| crate::design_axes::parse_axes(c.states_json.as_deref()))
        .unwrap_or_default();
    let fixed: Vec<(String, String)> = node
        .params
        .iter()
        .filter(|(key, _)| axes.iter().any(|a| &a.name == *key))
        .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
        .collect();
    for (axis, value) in crate::design_axes::active_combination(&axes, &fixed) {
        ctx.insert(axis.to_string(), Value::String(value.to_string()));
    }

    let markup = hb
        .render_template(&template, &Value::Object(ctx))
        .map_err(|e| RenderError::Template {
            component: component_label.clone(),
            message: e.to_string(),
        })?;

    // THE REFERENCES (`data-nyx-use`), resolved HERE and only here (finding
    // `01KYSCZ822K4ZFAAYTG6GT6YWZ`): a sub-component shared by several hosts is REFERENCED, not
    // copied, and this pass flattens it into inert markup carrying only the HOST's `data-nyx-*`
    // marks. The chain starts at this node's own component, so a self-reference is already a cycle;
    // the budget is per node, so one node's fan-out cannot be paid for by the next.
    let mut path = vec![component_label.clone()];
    let mut budget = reference::MAX_REFERENCE_EXPANSIONS;
    let expanded = reference::expand_references(
        hb,
        by_name,
        &component_label,
        &markup,
        &mut path,
        &mut budget,
    )?;
    // THE NAVIGATION (`navigate_to`), stamped on this node's OWN root element(s) — before the CSS
    // scope, which is the same order the lenient walk uses (and what keeps the two byte-identical).
    let expanded = strict_navigation(pages, node, node_key, expanded)?;
    // THIS NODE'S OWN AXES, baked into the markup the frame receives — the classes of the active
    // value and the structural clauses that name one of them, everything else left to the shim
    // (finding `01KZ41YK39SGNKF6XV87A35GPN`). Same step, same place, as the lenient walk's
    // `Vocabulary::Preserve` arm.
    // The strict render has no SUBJECT around the tree, so no axis is the shim's: every one a node
    // declares is baked.
    let expanded = reference::bake_node_axes(&expanded, &axes, &fixed, &[]);
    // A node's component may carry a scoped CSS slot: its root element(s) are STAMPED here, so the
    // rules the slot compiles to have something to anchor on.
    Ok(
        match node.component.as_deref().and_then(|n| by_name.get(n)) {
            Some(component) => stamp_component_scope(component, expanded),
            None => expanded,
        },
    )
}

/// The STRICT half of the navigation: resolve this node's `navigate_to` against the page directory
/// and stamp it, or REFUSE. A node that does not navigate is returned untouched — which is why a
/// composition with no link renders byte for byte as it did before this existed.
fn strict_navigation(
    pages: &[RenderPage],
    node: &CompositionNode,
    node_key: &str,
    markup: String,
) -> Result<String, RenderError> {
    let Some(page_id) = node.navigate_to.as_deref() else {
        return Ok(markup);
    };
    let Some(page) = pages.iter().find(|p| p.id == page_id) else {
        return Err(RenderError::UnknownPage {
            node_key: node_key.to_string(),
            page_id: page_id.to_string(),
            available: pages.iter().map(|p| p.slug.clone()).collect(),
        });
    };
    let bad = || RenderError::BadNavigation {
        node_key: node_key.to_string(),
        page_id: page_id.to_string(),
    };
    if !carries_own_markup(node) {
        return Err(bad());
    }
    reference::stamp_navigation(
        &markup,
        &[(PAGE_ATTR, page.slug.as_str()), (PAGE_ID_ATTR, &page.id)],
        &NAVIGATION_ATTRS,
    )
    .ok_or_else(bad)
}

/// Does this node render markup OF ITS OWN — the condition for carrying a link?
///
/// A STRUCTURAL node (neither `component` nor `inline`) is nothing but its children: stamping a link
/// there would silently make every child clickable, which is a decision nobody wrote, and the node
/// itself does not even reach the coder seam (it carries none of the three natures). So a link on a
/// structural node is REFUSED / NAMED like a link with nothing to click — same rule, same wording,
/// one place.
pub fn carries_own_markup(node: &CompositionNode) -> bool {
    node.component.is_some() || node.inline.is_some()
}

/// The ONE sentence a link with nothing to click is answered with — read by the strict refusal, the
/// lenient report and the write guard, so the three cannot drift.
pub const NO_ANCHOR_CORRECTION: &str =
    "a link is carried by the node's OWN root element(s): a STRUCTURAL node (no component, no \
     inline) has no markup of its own — it is only its children — and a template rendering bare \
     text has no element at all. Move the `navigate_to` onto the node that really draws the \
     clickable thing";

/// ONE catalogue component RESOLVED at a given combination — the markup a frame really displays,
/// plus the combination it was displayed at. See [`resolve_component`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedComponent {
    /// The fully resolved markup: template expanded, `data-nyx-use` references flattened, axis
    /// classes baked onto their targets, `data-nyx-if` / `-if-not` / `-state-case` evaluated (an
    /// absent element carrying the `hidden` attribute) and every `data-nyx-*` consumed.
    pub markup: String,
    /// The combination the markup was resolved at — ONE value per declared axis, in declaration
    /// order: what the caller asked for when the axis declares it, else the axis's own default.
    pub axes: Vec<(String, String)>,
    /// The SCOPED CSS SLOTS this resolution emitted — the component's own, plus every referenced
    /// sub-component's. Empty for the overwhelming majority of components. An agent asking "what did
    /// I just store?" is told about its exception here, not left to spot a `<style>` in the markup.
    pub css_slots: Vec<RenderedCssSlot>,
}

/// RESOLVE ONE catalogue component into the markup a frame really DISPLAYS, at a given axis
/// combination and param set (finding `01KYT0Z3RYGF3A881SDQN0X9D7` — "the agent cannot SEE what it
/// synthesizes").
///
/// # Why this exists beside [`render_composition`]
///
/// `render_composition` stops where the frame begins: it substitutes the params, flattens the
/// references, and hands the node's OWN `data-nyx-*` marks over UNRESOLVED, because in the app it is
/// the renderer's shim that pilots them live (that is the whole point of one element instead of a
/// cartesian product). An agent has no shim and no browser, so the markup that tool returns tells it
/// nothing about what a given combination LOOKS like — a spinner with no condition and a spinner
/// correctly gated read identically there.
///
/// This runs the SECOND half too — [`reference::resolve_axes`], the shim's applier executed ahead of
/// time, the very same pass a REFERENCED sub-component already goes through. So the markup returned
/// here is what the frame ends up displaying at that combination: the classes really applied, the
/// elements really present, the params really substituted.
///
/// `children` fills the `{{{children}}}` slot (a composition's child nodes, in one string) — a slot
/// component resolved with nothing in it legitimately renders to almost nothing, and that would be a
/// misleading answer rather than a diagnostic one.
///
/// The failure modes are [`render_composition`]'s, verbatim: an unknown component, a template with
/// logic, an unknown/cyclic/too-deep reference. A preview that cannot be produced is an EXPLICIT
/// error naming what to fix, never an empty string.
pub fn resolve_component(
    components: &[RenderComponent],
    name: &str,
    fixed_axes: &[(String, String)],
    params: &Map<String, Value>,
    children: &str,
) -> Result<ResolvedComponent, RenderError> {
    let hb = build_registry(components)?;
    let by_name: BTreeMap<&str, &RenderComponent> =
        components.iter().map(|c| (c.name.as_str(), c)).collect();
    let component = *by_name
        .get(name)
        .ok_or_else(|| RenderError::UnknownComponent(name.to_string()))?;

    // Context = the caller's params + the slot, exactly as a composition node builds it (params may
    // not shadow `children`, which the renderer owns).
    let mut ctx = params.clone();
    ctx.insert(
        CHILDREN_KEY.to_string(),
        Value::String(children.to_string()),
    );
    let markup = hb
        .render_template(&component.template, &Value::Object(ctx))
        .map_err(|e| RenderError::Template {
            component: name.to_string(),
            message: e.to_string(),
        })?;

    // The references, on the SAME budget a composition node gets — a preview must refuse exactly
    // what the render refuses, or the agent would validate a component the frame cannot show.
    let mut path = vec![name.to_string()];
    let mut budget = reference::MAX_REFERENCE_EXPANSIONS;
    let expanded =
        reference::expand_references(&hb, &by_name, name, &markup, &mut path, &mut budget)?;

    let axes = crate::design_axes::parse_axes(component.states_json.as_deref());
    let active = crate::design_axes::active_combination(&axes, fixed_axes);
    // A catalogue component is rendered on its OWN axes and nothing else — there is no subject
    // around it here (contrast a composition node, which sits inside a variant).
    let mut markup = stamp_component_scope(
        component,
        reference::resolve_axes(
            &expanded,
            &axes,
            fixed_axes,
            reference::AmbientAxes::default(),
        ),
    );
    let (styles, css_slots) = css_slot_styles(&markup, &by_name);
    markup.push_str(&styles);
    Ok(ResolvedComponent {
        markup,
        axes: active
            .into_iter()
            .map(|(axis, value)| (axis.to_string(), value.to_string()))
            .collect(),
        css_slots,
    })
}

// ---------------------------------------------------------------------------
// THE WHOLE TREE, RESOLVED — a COMPOSITION preview that always COMPLETES
// (finding `01KYT45C6RC7K84V1CA3561SPF`)
// ---------------------------------------------------------------------------

/// Why ONE node of a composition could not be rendered. Every variant is a state a composition is
/// LEGITIMATELY in while it is being designed — a `[NEW]` component nobody has synthesized yet is
/// the ordinary case, not a failure — which is why it NAMES the node instead of aborting the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnrenderedReason {
    /// The node names a component the catalogue does not have — a `[NEW]` not authored yet, a
    /// component deleted after the fact, or a typo in the node's name.
    NotInCatalogue,
    /// The component IS catalogued but carries no template yet (discovered, never synthesized).
    NoTemplate,
    /// The template (a component's, or an `[inline]` node's own markup) carries a Handlebars BLOCK
    /// helper, which PDR-0010 forbids — it is refused, never executed.
    TemplateHasLogic,
    /// The template INCLUDES another one as a Handlebars PARTIAL (`{{> Spinner}}`) — refused, never
    /// executed, with `data-nyx-use` named as the form that works (finding
    /// `01KYSZNX4TAVSGEVBVJM7W2B21`). Distinct from [`UnrenderedReason::TemplateHasLogic`] because
    /// the CORRECTION is different: a block is rewritten into the tree, a partial into a reference.
    TemplateHasPartial,
    /// Handlebars or the reference expansion refused this node (a malformed template, an unknown /
    /// cyclic / too-deep `data-nyx-use`). The [`RenderError`] message is carried verbatim.
    RenderFailed,
}

impl UnrenderedReason {
    /// The stable machine code the tool answers with. PINNED by `contract/design-fixtures.json`,
    /// which is what stops this vocabulary from drifting away from the surfaces that read it.
    pub fn code(self) -> &'static str {
        match self {
            UnrenderedReason::NotInCatalogue => "not_in_catalogue",
            UnrenderedReason::NoTemplate => "no_template",
            UnrenderedReason::TemplateHasLogic => "template_has_logic",
            UnrenderedReason::TemplateHasPartial => "template_has_partial",
            UnrenderedReason::RenderFailed => "render_failed",
        }
    }

    /// The SHORT phrase the placeholder shows in the markup itself (the long form is `detail`).
    /// PINNED by `contract/design-fixtures.json` alongside [`UnrenderedReason::code`].
    pub fn short(self) -> &'static str {
        match self {
            UnrenderedReason::NotInCatalogue => "not in the catalogue",
            UnrenderedReason::NoTemplate => "no template yet",
            UnrenderedReason::TemplateHasLogic => "template carries logic",
            UnrenderedReason::TemplateHasPartial => "template includes a partial",
            UnrenderedReason::RenderFailed => "render failed",
        }
    }

    /// Every reason, in the order the fixture pins them — so a variant ADDED here without a fixture
    /// entry fails the arrimage test instead of shipping unnamed.
    pub const ALL: [UnrenderedReason; 5] = [
        UnrenderedReason::NotInCatalogue,
        UnrenderedReason::NoTemplate,
        UnrenderedReason::TemplateHasLogic,
        UnrenderedReason::TemplateHasPartial,
        UnrenderedReason::RenderFailed,
    ];
}

/// ONE node the resolution could not render, NAMED: where it sits in the tree, which component it
/// asked for, why it could not be rendered, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrenderedNode {
    /// The node's `node_key` PATH (`Root/Header/Title`) — the SAME anchor `proto_feedback` and the
    /// composition reads use, so naming it here is enough to go and fix it.
    pub node_key: String,
    /// The catalogue component the node named (`None` for an `[inline]` node).
    pub component: Option<String>,
    pub reason: UnrenderedReason,
    /// The full explanation, correction included.
    pub detail: String,
}

/// A whole COMPOSITION resolved into the markup a frame would display, plus what it could not
/// render. See [`resolve_composition`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedComposition {
    /// The resolved markup of the whole subject tree: every node's template substituted, references
    /// flattened, axis classes applied, conditions evaluated, no `data-nyx-*` left — and a
    /// PLACEHOLDER wherever a node could not be rendered.
    pub markup: String,
    /// How many nodes the walk visited (the rendered ones AND the placeholders).
    pub node_count: usize,
    /// The nodes that came back as placeholders, in the order the walk RECORDS them — depth-first,
    /// CHILDREN BEFORE THEIR PARENT (a hole nested under a hole is listed first). Pinned by
    /// `contract/design-fixtures.json`, since a display surface lists them in this order.
    pub unrendered: Vec<UnrenderedNode>,
    /// The SCOPED CSS SLOTS this composition really reaches, in the DOCUMENT ORDER of the elements
    /// they are stamped on — the exceptions to the design system, named. This is what the review
    /// surface badges on the node and counts in the chrome, and what makes the exception visible
    /// instead of buried in a `<style>` (finding `01KYVMTWB71R0D0CT1SV88J8DX`).
    pub css_slots: Vec<RenderedCssSlot>,
    /// The PARCOURS this composition states: one entry per node that NAVIGATES, in walk order,
    /// carried whether or not the link could be honoured (finding `01KYW34347CZSNXCYS35KDG3EY`).
    ///
    /// This is where the READABLE half of a link lives. The model stores an id (an id does not
    /// rename); the destination's `slug` and `title` are resolved here, at the render, so a screen
    /// and an agent both get "→ Pricing" without ever storing the words that move. A link that could
    /// NOT be honoured is in this list too, with the status that says why — the alternative would be
    /// a click that does nothing and a list that mentions nothing.
    pub navigation: Vec<NavigationLink>,
    /// The SUBJECT's axis combination this markup was resolved AT, in the subject's own declaration
    /// order — one entry per axis its `states_json` declares, EMPTY when it declares none (finding
    /// `01KZ3JS7PNF3F8GFB3GYRK6CKH`).
    ///
    /// A variant's axes pilot the WHOLE composition (the frame's chip-bar row per axis), so a
    /// resolution that applies them must SAY which values it applied — otherwise an agent reading
    /// the markup cannot tell whether the element it is looking for is absent because its condition
    /// is false, or because nobody honoured the condition at all. That was the defect.
    pub axes: Vec<(String, String)>,
}

/// The attribute the SHIM reads to navigate: the destination's SLUG (the shim posts its value to its
/// host). Emitted by a render only for a link that RESOLVED.
pub const PAGE_ATTR: &str = "data-nyx-page";
/// The attribute carrying the destination's durable IDENTITY beside the slug — what a host navigates
/// by when it would rather not round-trip through a name that can be renamed.
pub const PAGE_ID_ATTR: &str = "data-nyx-page-id";
/// The NAMED marker of a link that could not be resolved: it carries the id that resolved to
/// nothing, and — deliberately — NO [`PAGE_ATTR`], so the shim cannot navigate to a page that is not
/// there. It is the "placeholder nommé" of a broken link, in place.
///
/// THE NEUTRALISATION IS STRUCTURAL, and it was not (finding `01KZ3JN69E32FYBF01NBE4FWJD`): stamping
/// this marker also CLEARS the whole [`NAVIGATION_ATTRS`] vocabulary off the root, so a
/// [`PAGE_ATTR`] a component's template had baked in cannot survive beside it. Before that, the
/// rewrite only dropped the attributes it re-posed, and the shim happily navigated to the slug frozen
/// in the template while this answer said the link led nowhere.
///
/// NOTHING STYLES IT YET. No CSS rule in this crate or in the renderer's frame targets
/// `[data-nyx-page-missing]` (`frame-srcdoc.ts` styles `[data-nyx-page]` and only that), so a dead
/// link is currently INERT and UNMARKED to the eye. The mark is in the markup and in
/// [`ResolvedComposition::navigation`]; the badge is a renderer-side rule still to be written.
pub const PAGE_MISSING_ATTR: &str = "data-nyx-page-missing";

/// The WHOLE navigation vocabulary — every attribute a render may stamp on a linking node's root.
///
/// It exists as a list because a node that states a destination OWNS these marks: the stamp CLEARS
/// all of them before writing the ones it decided ([`reference::stamp_navigation`]), which is what
/// makes an invalidated link really dead instead of merely badged.
pub const NAVIGATION_ATTRS: [&str; 3] = [PAGE_ATTR, PAGE_ID_ATTR, PAGE_MISSING_ATTR];

/// What became of ONE node's `navigate_to`. Every outcome is NAMED — there is no variant meaning
/// "nothing happened".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationStatus {
    /// The page resolved and the link is carried by the node's root element(s).
    Linked,
    /// The page id resolves to NO page of this prototype (never created, deleted since, or the id of
    /// another prototype's page). The markup carries [`PAGE_MISSING_ATTR`] and NO navigation.
    UnknownPage,
    /// The page resolved, but the node's markup has no element that could hold the link.
    NoAnchor,
}

impl NavigationStatus {
    /// The stable machine code a display surface switches on. PINNED by
    /// `contract/design-fixtures.json` — the same arrimage the unrendered reasons get.
    pub fn code(self) -> &'static str {
        match self {
            NavigationStatus::Linked => "linked",
            NavigationStatus::UnknownPage => "unknown_page",
            NavigationStatus::NoAnchor => "no_anchor",
        }
    }

    /// The SHORT phrase a surface shows beside the node (the long form is `detail`).
    pub fn short(self) -> &'static str {
        match self {
            NavigationStatus::Linked => "navigates",
            NavigationStatus::UnknownPage => "page is gone",
            NavigationStatus::NoAnchor => "nothing to click",
        }
    }

    /// Every status, in the order the fixture pins them — a variant added here without a fixture
    /// entry fails the arrimage test instead of shipping unnamed.
    pub const ALL: [NavigationStatus; 3] = [
        NavigationStatus::Linked,
        NavigationStatus::UnknownPage,
        NavigationStatus::NoAnchor,
    ];
}

/// ONE navigation link of a composition, resolved: where it sits, where it goes, and whether it
/// really goes there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationLink {
    /// The linking node's `node_key` PATH — the same anchor `proto_feedback` and the composition
    /// reads use, so naming it is enough to go and fix it.
    pub node_key: String,
    /// The destination as it is STORED: a `prototype_pages.id`.
    pub page_id: String,
    /// The destination's slug, resolved at this render. `None` when the id resolved to nothing.
    pub slug: Option<String>,
    /// The destination's human TITLE, resolved at this render — what a screen displays. `None` when
    /// the id resolved to nothing.
    pub title: Option<String>,
    pub status: NavigationStatus,
    /// The full sentence: what happened and what to do about it. Empty for a link that resolved.
    pub detail: String,
}

/// The class the placeholder of an unrenderable node carries. Deliberately NOT a `data-nyx-*`
/// attribute: that vocabulary belongs to the shim, and resolved markup carries none of it.
///
/// IT IS ONE HALF OF A PAIR, and it did not use to be arrimé (finding
/// `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 8): the frame's `design/frame-srcdoc.ts` styles `.{this}`
/// UNCONDITIONALLY, and that rule is the ONLY thing that draws a hole AS a hole. Renaming either
/// side alone left every suite green while the placeholders lost their dashed box, their margin and
/// their type size and melted into the page as plain text. `contract/design-fixtures.json`
/// (`unrendered_nodes.placeholder_class`) is what joins them now.
const UNRENDERED_CLASS: &str = "nyx-unrendered";

/// The placeholder's `node_key` attribute — how a surface says WHICH node this hole stands for.
const UNRENDERED_NODE_ATTR: &str = "data-unrendered-node";
/// The placeholder's component attribute — absent on an `[inline]` node, which names no component.
const UNRENDERED_COMPONENT_ATTR: &str = "data-unrendered-component";
/// The placeholder's reason attribute, carrying an [`UnrenderedReason::code`].
const UNRENDERED_REASON_ATTR: &str = "data-unrendered-reason";

/// How many node keys [`RenderError::UnknownNode`] names — enough to find the one you meant, not a
/// whole tree in an error message.
const MAX_NAMED_NODE_KEYS: usize = 40;

/// The label an `[inline]` node's diagnostics use, matching [`RenderError`]'s own convention.
const INLINE_LABEL: &str = "<inline>";

/// RESOLVE a WHOLE COMPOSITION (or ONE of its subtrees) into the markup a frame would display —
/// the agent's eyes on the thing it actually presents to the human (finding
/// `01KYT45C6RC7K84V1CA3561SPF`).
///
/// # Why it exists beside [`render_composition`] and [`resolve_component`]
///
/// [`render_composition`] feeds the FRAME: it stops before the shim's applier, and it is STRICT —
/// one unknown component and the whole tree is an error. [`resolve_component`] answers about ONE
/// catalogue component. Neither answers the question an agent has to settle before presenting a
/// round: "what does the WHOLE page look like?". This does, with the two changes that question
/// implies:
///
///   * it runs the shim's applier at EVERY node ([`reference::resolve_axes`]), exactly as
///     `resolve_component` does for one — so the answer is what is DISPLAYED, not what the frame
///     receives. A composition node carries its axis values IN ITS PARAMS (that is where the shim
///     reads them), so that is where they are read here;
///   * it ALWAYS COMPLETES. A composition under design legitimately contains `[NEW]` nodes whose
///     component nobody has synthesized yet; failing the whole tree on the first of them would hide
///     the forty nodes that ARE right. Each unrenderable node becomes a PLACEHOLDER that names
///     itself in the markup and an entry in [`ResolvedComposition::unrendered`] — the "aboutir en
///     les nommant" rule. The only refusals left are a composition that does not PARSE and a
///     `node_key` that names no node.
///
/// `node_key` renders ONE subtree instead of the whole tree (`Root/Main/Pricing`) — the lever for a
/// composition whose markup does not fit one answer, and the same anchor convention as
/// [`node_key_path`].
///
/// The `data-nyx-*` vocabulary is CONSUMED here, the navigation marks included — an agent has no
/// shim, so a mark it cannot run is a mark it must not read back. The parcours is not lost for that:
/// it comes back in [`ResolvedComposition::navigation`], resolved to the destination's SLUG and
/// TITLE, which is the form an agent can actually reason about.
///
/// `subject_states_json` is the SUBJECT's own `states_json` — a proto VARIANT's axes, which pilot the
/// whole page in the frame — and `subject_axes` fixes the values to render them at (an axis left out
/// renders at its own `default`, else its first value). The combination really used comes back in
/// [`ResolvedComposition::axes`]. See [`reference::AmbientAxes`] for what they reach, and finding
/// `01KZ3JS7PNF3F8GFB3GYRK6CKH` for what happened while nobody read them.
pub fn resolve_composition(
    components: &[RenderComponent],
    pages: &[RenderPage],
    composition_json: &str,
    node_key: Option<&str>,
    subject_states_json: Option<&str>,
    subject_axes: &[(String, String)],
) -> Result<ResolvedComposition, RenderError> {
    resolve_composition_with(
        components,
        pages,
        composition_json,
        node_key,
        Vocabulary::Consume,
        subject_states_json,
        subject_axes,
    )
}

/// RENDER a whole composition FOR THE FRAME, and ALWAYS COMPLETE (finding
/// `01KYT78699XG6Q6P3VZC64W8H8`, MAJOR).
///
/// # The defect this closes
///
/// [`render_composition`] is ALL-OR-NOTHING: one node naming a component the catalogue does not
/// carry, and the whole page is an error; a component whose row exists with an EMPTY template
/// renders as the empty string with no signal at all. A composition under design ALWAYS holds
/// `[NEW]` nodes — the agent proposes a component before anybody builds it — so the normal state of
/// a page in flight is "mostly renderable, with holes", and a frame that refuses to draw until the
/// last hole is filled is a frame nobody can use.
///
/// [`resolve_composition`] already knew how to complete instead of failing, and it already NAMED
/// every hole — but it is the AGENT's resolution: it runs [`reference::resolve_axes`] at every node
/// and therefore CONSUMES the `data-nyx-*` vocabulary. Wiring the frame to it would have deleted the
/// chip-bar, the in-place state change and the `data-nyx-page` navigation, all of which the shim
/// drives off those very attributes. (Measured on the reference project: 4 of its 14
/// synthesized components carry `data-nyx-*`, 6 declare axes.) So the renderer grew its OWN notion
/// of an unrenderable node, in TypeScript, with its own reasons and its own wording — the exact
/// class of defect that has already bitten this feature twice (the inertia rule diverging between
/// Rust and TS, and the map form of `states_json` being read in two different orders; both times
/// invisible until somebody went looking).
///
/// # Why this is the SAME function and not a second one
///
/// The two resolutions differ in exactly ONE step: what happens to the `data-nyx-*` marks once a
/// node's template is rendered and its references are flattened. Everything else — the lenient
/// registry, the four ways a node can be unrenderable, the wording of each one, the placeholder
/// markup, the walk, the `node_key` convention, the reference budgets — is identical, and it is
/// precisely that identical part which must never exist twice. So there is ONE walk
/// ([`resolve_node`]) parameterized by [`Vocabulary`]: `Consume` answers the agent, `Preserve` feeds
/// the frame. A second path would have re-created, inside Rust, the divergence this finding is about.
///
/// The guarantee is proven, not asserted: `render_composition_lenient` and [`render_composition`]
/// return BYTE-IDENTICAL markup on a composition with no holes
/// (`the_lenient_frame_render_is_the_strict_one_when_nothing_is_missing`).
/// `subject_states_json` is what the host will hand the frame as `window.__nyxAxes` — the axes the
/// SHIM pilots live over the whole document. They are not RESOLVED here (that is exactly what the
/// shim is for); they are declared so that ONE OWNER applies each axis. Every other axis a node
/// declares is baked into its markup, because nothing in the frame can move it
/// ([`reference::bake_node_axes`], finding `01KZ41YK39SGNKF6XV87A35GPN`). `None` = the frame gets no
/// axes at all, so every node's own are baked — a page thumbnail, a variant with no `states_json`.
pub fn render_composition_lenient(
    components: &[RenderComponent],
    pages: &[RenderPage],
    composition_json: &str,
    subject_states_json: Option<&str>,
) -> Result<ResolvedComposition, RenderError> {
    resolve_composition_with(
        components,
        pages,
        composition_json,
        None,
        Vocabulary::Preserve,
        subject_states_json,
        &[],
    )
}

/// What a resolution does with the `data-nyx-*` vocabulary once a node's template is rendered and
/// its `data-nyx-use` references are flattened — the ONE axis on which the agent's resolution and
/// the frame's render differ. See [`render_composition_lenient`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vocabulary {
    /// CONSUME it — run the shim's applier ahead of time ([`reference::resolve_axes`]): axis classes
    /// baked on, `data-nyx-if` / `-if-not` / `-state-case` evaluated, every `data-nyx-*` stripped.
    /// What an AGENT must read, because it has no shim and no browser.
    Consume,
    /// PRESERVE it — hand the node's own marks over untouched, for the renderer's shim to pilot
    /// LIVE. What the FRAME must receive: the chip-bar, the in-place state change and the
    /// `data-nyx-page` navigation are all driven off those attributes.
    Preserve,
}

/// The ONE walk both leniences run. See [`render_composition_lenient`] for why it is one.
#[allow(clippy::too_many_arguments)]
fn resolve_composition_with(
    components: &[RenderComponent],
    pages: &[RenderPage],
    composition_json: &str,
    node_key: Option<&str>,
    vocabulary: Vocabulary,
    subject_states_json: Option<&str>,
    subject_fixed: &[(String, String)],
) -> Result<ResolvedComposition, RenderError> {
    let root: CompositionNode = serde_json::from_str(composition_json)
        .map_err(|e| RenderError::BadComposition(e.to_string()))?;
    let (subject, subject_key) = match node_key {
        None => (&root, root.key().to_string()),
        Some(key) => (
            find_node(&root, key).ok_or_else(|| RenderError::UnknownNode {
                node_key: key.to_string(),
                available: composition_nodes(composition_json)
                    .into_iter()
                    .map(|n| n.node_key)
                    .take(MAX_NAMED_NODE_KEYS)
                    .collect(),
            })?,
            key.to_string(),
        ),
    };
    let (hb, rejected) = lenient_registry(components);
    let by_name: BTreeMap<&str, &RenderComponent> =
        components.iter().map(|c| (c.name.as_str(), c)).collect();
    let mut resolved = ResolvedComposition::default();
    // The SUBJECT's axes, read exactly like a component's — one reading of a stored `states_json`,
    // whoever asks (finding `01KZ3JS7PNF3F8GFB3GYRK6CKH`).
    let subject_axes = crate::design_axes::parse_axes(subject_states_json);
    let walk = LenientWalk {
        hb: &hb,
        by_name: &by_name,
        rejected: &rejected,
        pages,
        vocabulary,
        subject_axes: &subject_axes,
        subject_fixed,
        subject_key: &subject_key,
    };
    let markup = resolve_node(&walk, subject, &subject_key, &mut resolved);
    let (styles, css_slots) = css_slot_styles(&markup, &by_name);
    resolved.markup = markup;
    resolved.markup.push_str(&styles);
    resolved.css_slots = css_slots;
    // The combination this markup was really resolved AT — never left for the caller to re-derive.
    resolved.axes = crate::design_axes::active_combination(&subject_axes, subject_fixed)
        .into_iter()
        .map(|(axis, value)| (axis.to_string(), value.to_string()))
        .collect();
    Ok(resolved)
}

/// The node at `node_key` (a `/`-joined [`node_key_path`]), matching child `key()`s segment by
/// segment — the READ twin of [`set_node_params`]'s walk. `None` when the path leaves the tree.
fn find_node<'a>(root: &'a CompositionNode, node_key: &str) -> Option<&'a CompositionNode> {
    let mut segments = node_key.split('/');
    if segments.next()? != root.key() {
        return None;
    }
    let mut cur = root;
    for segment in segments {
        cur = cur.children.iter().find(|c| c.key() == segment)?;
    }
    Some(cur)
}

/// [`build_registry`], but a template it cannot accept DISQUALIFIES ITS COMPONENT instead of
/// failing the whole render: the names it refused come back WITH THEIR REASON, and a node naming
/// one of them becomes a placeholder carrying it. One logic-carrying template in a 200-component
/// catalogue must not blank a page.
fn lenient_registry(
    components: &[RenderComponent],
) -> (
    Handlebars<'static>,
    BTreeMap<String, (UnrenderedReason, String)>,
) {
    let mut hb = Handlebars::new();
    hb.set_strict_mode(false);
    let mut rejected = BTreeMap::new();
    for c in components {
        if let Some(refusal) = template_refusal(&c.name, &c.template) {
            rejected.insert(c.name.clone(), refusal);
        }
    }
    (hb, rejected)
}

/// Everything a lenient walk holds CONSTANT from the root to the last leaf: the registry, the
/// catalogue, the templates it had to disqualify, the prototype's page directory and what it does
/// with the `data-nyx-*` vocabulary. Grouped so [`resolve_node`] takes the four things that really
/// VARY down the tree (the node, its key, the accumulator) instead of threading eight.
struct LenientWalk<'a> {
    hb: &'a Handlebars<'static>,
    by_name: &'a BTreeMap<&'a str, &'a RenderComponent>,
    rejected: &'a BTreeMap<String, (UnrenderedReason, String)>,
    pages: &'a [RenderPage],
    vocabulary: Vocabulary,
    /// The SUBJECT's own axes — a proto VARIANT's `states_json`, which pilots the whole composition
    /// in the frame (finding `01KZ3JS7PNF3F8GFB3GYRK6CKH`). Empty for a render that has no subject
    /// around the tree.
    ///
    /// ONE OWNER PER AXIS, and the vocabulary decides which: [`Vocabulary::Consume`] APPLIES them
    /// (the agent has no shim), while [`Vocabulary::Preserve`] leaves them to the frame's shim — and
    /// therefore refuses to bake a NODE axis of the same name over it
    /// ([`reference::bake_node_axes`]).
    subject_axes: &'a [crate::design_axes::AxisSpec],
    subject_fixed: &'a [(String, String)],
    /// The key of the node the walk STARTED at — the one whose top-level elements are the document's
    /// (see [`reference::AmbientAxes::classes_on_root`]).
    subject_key: &'a str,
}

impl LenientWalk<'_> {
    /// The subject's axes as they apply to the node at `node_key`.
    fn ambient(&self, node_key: &str) -> reference::AmbientAxes<'_> {
        reference::AmbientAxes {
            axes: self.subject_axes,
            fixed: self.subject_fixed,
            classes_on_root: node_key == self.subject_key,
        }
    }
}

/// Resolve ONE node of the tree (depth-first, children first), returning its markup and RECORDING
/// every node it could not render into `acc`. Never fails: an unrenderable node becomes a
/// placeholder that keeps its children.
fn resolve_node(
    walk: &LenientWalk<'_>,
    node: &CompositionNode,
    node_key: &str,
    acc: &mut ResolvedComposition,
) -> String {
    let LenientWalk {
        hb,
        by_name,
        rejected,
        pages,
        vocabulary,
        ..
    } = *walk;
    acc.node_count += 1;
    let mut children_markup = String::new();
    for child in &node.children {
        let child_key = format!("{node_key}/{}", child.key());
        children_markup.push_str(&resolve_node(walk, child, &child_key, acc));
    }

    let Some(name) = node.component.as_deref() else {
        let Some(inline) = node.inline.as_deref() else {
            // A structural node with no template of its own: it is just its children.
            return lenient_navigation(acc, pages, node, node_key, children_markup);
        };
        // An `[inline]` one-off is a rendered template too, held to the same contract as a
        // component's: no block helper, no partial include.
        if let Some(refused) = find_refused_construct(inline) {
            return placeholder(
                acc,
                node_key,
                None,
                refused.reason(),
                format!(
                    "this [inline] node's markup is refused: {}",
                    refused.correction()
                ),
                &children_markup,
            );
        }
        let mut ctx = node.params.clone();
        ctx.insert(
            CHILDREN_KEY.to_string(),
            Value::String(children_markup.clone()),
        );
        return match render_and_expand(hb, by_name, INLINE_LABEL, inline, &Value::Object(ctx)) {
            // An inline node declares no axes of ITS OWN — but it sits inside a SUBJECT, and a
            // variant's axes pilot the whole page in the frame (finding
            // `01KZ3JS7PNF3F8GFB3GYRK6CKH`). So the applier runs on the subject's axes here, which
            // is exactly where the reported defect lived: a `data-nyx-if="loading=on"` written in an
            // inline node used to match the "clause on an axis this markup does not declare" rule
            // and stay visible at every value. Beyond that, all `Consume` does is STRIP the
            // vocabulary the agent must not read back — the navigation marks INCLUDED, which is why
            // they are stamped BEFORE this switch.
            Ok(markup) => {
                let markup = lenient_navigation(acc, pages, node, node_key, markup);
                match vocabulary {
                    Vocabulary::Consume => {
                        reference::resolve_axes(&markup, &[], &[], walk.ambient(node_key))
                    }
                    Vocabulary::Preserve => markup,
                }
            }
            Err(e) => placeholder(
                acc,
                node_key,
                None,
                UnrenderedReason::RenderFailed,
                e.to_string(),
                &children_markup,
            ),
        };
    };

    let Some(component) = by_name.get(name).copied() else {
        return placeholder(
            acc,
            node_key,
            Some(name),
            UnrenderedReason::NotInCatalogue,
            format!(
                "the project's catalogue holds no component named '{name}': it is a [NEW] nobody \
                 has synthesized yet (author it with proto_author_component, then synthesize it \
                 with design_refresh_component), it was deleted after this composition was \
                 written, or the node's name is a typo. The node is NOT rendered — everything \
                 around it is"
            ),
            &children_markup,
        );
    };
    if let Some((reason, detail)) = rejected.get(name) {
        return placeholder(
            acc,
            node_key,
            Some(name),
            *reason,
            detail.clone(),
            &children_markup,
        );
    }
    if component.template.trim().is_empty() {
        return placeholder(
            acc,
            node_key,
            Some(name),
            UnrenderedReason::NoTemplate,
            format!(
                "component '{name}' is catalogued but carries NO template yet — the inventory \
                 discovered it and nobody synthesized it. READ its bundle (design_inventory with \
                 component='{name}') and STORE a synthesis (design_refresh_component)"
            ),
            &children_markup,
        );
    }

    // A composition NODE carries its AXIS values in its PARAMS — that is where the shim reads them
    // in the frame, so that is where they are read here (the same rule `design_preview` applies to
    // an axis passed as a param).
    let axes = crate::design_axes::parse_axes(component.states_json.as_deref());
    let fixed: Vec<(String, String)> = node
        .params
        .iter()
        .filter(|(key, _)| axes.iter().any(|a| &a.name == *key))
        .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
        .collect();
    let mut ctx = node.params.clone();
    ctx.insert(
        CHILDREN_KEY.to_string(),
        Value::String(children_markup.clone()),
    );
    // The ACTIVE axis values are substitutable too (`class="btn btn-{{variant}}"`), and an axis the
    // node left out renders at its own default. BOTH vocabularies do it, and that is the point: the
    // frame used to be left out on the strength of "the shim reads a node's axis values off its
    // params" — it does not (finding `01KZ41YK39SGNKF6XV87A35GPN`, and see
    // [`reference::bake_node_axes`]). Skipping it here rendered `btn-{{variant}}` as `btn-` for the
    // human and `btn-outline` for the agent: two renderings of one composition.
    for (axis, value) in crate::design_axes::active_combination(&axes, &fixed) {
        ctx.insert(axis.to_string(), Value::String(value.to_string()));
    }
    match render_and_expand(hb, by_name, name, &component.template, &Value::Object(ctx)) {
        Ok(markup) => {
            // THE NAVIGATION, stamped on this node's own root element(s) BEFORE the vocabulary
            // switch: the frame keeps the marks and the shim navigates, the agent's resolution
            // strips them like every other `data-nyx-*` and reads the parcours off `navigation[]`.
            let markup = lenient_navigation(acc, pages, node, node_key, markup);
            stamp_component_scope(
                component,
                match vocabulary {
                    Vocabulary::Consume => reference::resolve_axes(
                        &markup,
                        &axes,
                        &fixed,
                        // The SUBJECT's axes reach this node too, for every clause the component
                        // does not declare itself — the shim applies them over the whole document.
                        walk.ambient(node_key),
                    ),
                    // The frame's shim pilots the DOCUMENT's marks LIVE — the chip-bar, the in-place
                    // state change and the data-nyx-page navigation are all driven off them, and
                    // preserving those is the whole reason this render exists beside the agent's
                    // one. It pilots NOTHING of a node's own axes, though (there is no chip for
                    // them, and `setAxis` refuses a name `__nyxAxes` does not declare), so those are
                    // resolved here or nowhere — finding `01KZ41YK39SGNKF6XV87A35GPN`.
                    Vocabulary::Preserve => {
                        reference::bake_node_axes(&markup, &axes, &fixed, walk.subject_axes)
                    }
                },
            )
        }
        Err(e) => placeholder(
            acc,
            node_key,
            Some(name),
            UnrenderedReason::RenderFailed,
            e.to_string(),
            &children_markup,
        ),
    }
}

/// The LENIENT half of the navigation: resolve this node's `navigate_to`, stamp what can be stamped,
/// and RECORD the outcome — every outcome, honoured or not — into `acc.navigation`. A node that does
/// not navigate is returned untouched and records nothing, which is what keeps a composition with no
/// link byte for byte what it was.
///
/// It never fails, exactly like the rest of this walk: a link that cannot be honoured leaves a NAMED
/// marker in place ([`PAGE_MISSING_ATTR`]) and an entry that says what to do — the "aboutir en les
/// nommant" rule applied to a parcours. The strict render REFUSES the same two cases
/// ([`strict_navigation`]); the two never disagree on WHAT is wrong, only on what to do about it.
fn lenient_navigation(
    acc: &mut ResolvedComposition,
    pages: &[RenderPage],
    node: &CompositionNode,
    node_key: &str,
    markup: String,
) -> String {
    let Some(page_id) = node.navigate_to.as_deref() else {
        return markup;
    };
    let Some(page) = pages.iter().find(|p| p.id == page_id) else {
        let available: Vec<String> = pages.iter().map(|p| p.slug.clone()).collect();
        acc.navigation.push(NavigationLink {
            node_key: node_key.to_string(),
            page_id: page_id.to_string(),
            slug: None,
            title: None,
            status: NavigationStatus::UnknownPage,
            detail: format!(
                "this node navigates to page '{page_id}', which this prototype does NOT have — {}. \
                 The link is NOT carried into the markup (a click would go nowhere): the element is \
                 marked `{PAGE_MISSING_ATTR}` instead. Re-point the node at a real page id \
                 (proto_add_page returns it, proto_get lists them) or drop its `navigate_to`",
                available_pages_phrase(&available)
            ),
        });
        // The NAMED marker, in place — and the whole navigation vocabulary CLEARED off the root
        // with it, so a `data-nyx-page` a template baked in cannot survive beside the marker and
        // send the shim to a slug frozen in a component's markup. The click is dead BY
        // CONSTRUCTION, which is what this comment used to claim while the code only badged it
        // (finding `01KZ3JN69E32FYBF01NBE4FWJD`).
        return reference::stamp_navigation(
            &markup,
            &[(PAGE_MISSING_ATTR, page_id)],
            &NAVIGATION_ATTRS,
        )
        .unwrap_or(markup);
    };
    let stamped = if carries_own_markup(node) {
        reference::stamp_navigation(
            &markup,
            &[(PAGE_ATTR, page.slug.as_str()), (PAGE_ID_ATTR, &page.id)],
            &NAVIGATION_ATTRS,
        )
    } else {
        None
    };
    match stamped {
        Some(stamped) => {
            acc.navigation.push(NavigationLink {
                node_key: node_key.to_string(),
                page_id: page.id.clone(),
                slug: Some(page.slug.clone()),
                title: Some(page.title.clone()),
                status: NavigationStatus::Linked,
                detail: String::new(),
            });
            stamped
        }
        None => {
            acc.navigation.push(NavigationLink {
                node_key: node_key.to_string(),
                page_id: page.id.clone(),
                slug: Some(page.slug.clone()),
                title: Some(page.title.clone()),
                status: NavigationStatus::NoAnchor,
                detail: format!(
                    "this node navigates to '{}' but renders NO element that could carry the link — \
                     there is nothing to click. {NO_ANCHOR_CORRECTION}",
                    page.slug
                ),
            });
            markup
        }
    }
}

/// Render ONE template with `ctx` and flatten its `data-nyx-use` references, on the SAME budgets a
/// composition node gets from [`render_composition`] — a preview refuses exactly what the render
/// refuses.
fn render_and_expand(
    hb: &Handlebars<'static>,
    by_name: &BTreeMap<&str, &RenderComponent>,
    label: &str,
    template: &str,
    ctx: &Value,
) -> Result<String, RenderError> {
    let markup = hb
        .render_template(template, ctx)
        .map_err(|e| RenderError::Template {
            component: label.to_string(),
            message: e.to_string(),
        })?;
    let mut path = vec![label.to_string()];
    let mut budget = reference::MAX_REFERENCE_EXPANSIONS;
    reference::expand_references(hb, by_name, label, &markup, &mut path, &mut budget)
}

/// The markup an unrenderable node leaves BEHIND — and the record of it in `acc`.
///
/// It is a plain, inert element that NAMES the node in the markup itself (so the defect is visible
/// where it sits, not only in a list beside it) and KEEPS the node's children (a node nyx cannot
/// render must not swallow the subtree under it). It carries no `data-nyx-*`: that vocabulary is
/// the shim's, and resolved markup leaves none of it.
fn placeholder(
    acc: &mut ResolvedComposition,
    node_key: &str,
    component: Option<&str>,
    reason: UnrenderedReason,
    detail: String,
    children: &str,
) -> String {
    let label = component.unwrap_or(INLINE_LABEL);
    let mut markup = format!(
        "<div class=\"{UNRENDERED_CLASS}\" {UNRENDERED_NODE_ATTR}=\"{}\"",
        escape_html(node_key)
    );
    if let Some(component) = component {
        markup.push_str(&format!(
            " {UNRENDERED_COMPONENT_ATTR}=\"{}\"",
            escape_html(component)
        ));
    }
    markup.push_str(&format!(
        " {UNRENDERED_REASON_ATTR}=\"{}\">[UNRENDERED {} — {}]{children}</div>",
        reason.code(),
        escape_html(label),
        reason.short()
    ));
    acc.unrendered.push(UnrenderedNode {
        node_key: node_key.to_string(),
        component: component.map(str::to_string),
        reason,
        detail,
    });
    markup
}

/// The five characters a placeholder's own text and attributes must not inject verbatim (a
/// component NAME comes from the catalogue, i.e. from an agent's write).
fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// The `node_key` PATH of a node reached by `indices` (a path of child positions from the root):
/// the `/`-join of each node's [`CompositionNode::key`] along the way — the same anchor convention
/// as `prototype_feedback.node_key` (`Root/Header/Title`). Returns `None` when the path leaves the
/// tree. This is how the detail view maps a CLICKED node to the params it persists PER NODE.
pub fn node_key_path(root: &CompositionNode, indices: &[usize]) -> Option<String> {
    let mut segments = vec![root.key().to_string()];
    let mut cur = root;
    for &idx in indices {
        cur = cur.children.get(idx)?;
        segments.push(cur.key().to_string());
    }
    Some(segments.join("/"))
}

/// The DISTINCT component names a `composition_json` TREE names (every node's `component`, inline
/// nodes ignored), in first-seen order. A malformed composition yields an EMPTY list (never an
/// error — a filter must not break a listing).
///
/// This is the tree half of "which components does this page use". The OTHER half — a component
/// reached only from another component's TEMPLATE, through `data-nyx-use` — is
/// [`ReferenceIndex::extend_with_referenced`], and every caller that means "used" rather than
/// "placed in the tree" runs both (finding `01KYSZPG9Y8QGGVED208SG5BTB`).
pub fn referenced_component_names(composition_json: &str) -> Vec<String> {
    let Ok(root) = serde_json::from_str::<CompositionNode>(composition_json) else {
        return Vec::new();
    };
    let mut seen = Vec::new();
    collect_component_names(&root, &mut seen);
    seen
}

/// The reference GRAPH of a catalogue — each component name → the components its template
/// REFERENCES (`data-nyx-use`), built in ONE pass and then queried without parsing anything again.
///
/// # The gap it closes (finding `01KYSZPG9Y8QGGVED208SG5BTB`)
///
/// [`referenced_component_names`] reads the COMPOSITION TREE, and since `data-nyx-use` exists a
/// component has a SECOND way of being used: from another component's TEMPLATE. A `Spinner` used by
/// three components — exactly the case the capability was asked for — appeared in no page's node
/// list, so the catalogue showed it with no "linked" badge (a legitimate-looking deletion candidate,
/// while it is loaded on every page) and a `[NEW]` reached only that way never surfaced in the coder
/// seam (so nobody was ever asked to build it).
///
/// # Transitivity: YES, and here is why
///
/// If A's tree uses B, and B's template references C, then C IS LINKED. Three reasons, and they
/// agree:
///
///   * the RENDER is transitive — [`reference::expand_references`] recurses down to
///     [`MAX_REFERENCE_DEPTH`], so C is really expanded into the page's markup. "Linked" has to mean
///     "would be rendered", or the badge is about something other than what happens;
///   * DELETION breaks the page identically — remove C and B stops rendering, so A's page stops
///     rendering. A badge that said "unused" there would be pointing at a load-bearing row;
///   * for the SEAM, C must be BUILT for B to be buildable at all.
///
/// The walk terminates on a CYCLE (which the render refuses separately) because a name already
/// reached is never expanded again — which is also what bounds the cost.
///
/// # The cost, bounded (the reference catalogue can be large — 179 bundles on the reference project)
///
/// [`ReferenceIndex::build`] scans each catalogue template EXACTLY ONCE — O(Σ template bytes) — and
/// [`ReferenceIndex::extend_with_referenced`] parses NOTHING: it walks the edges already built, and
/// expands each name at most once, so it is O(edges) whatever the shape of the graph. One index per
/// READ (`design_component_list`, `elected_composition`), never one per card and never one per
/// render: the surfaces that need it already load the whole catalogue for other reasons, so the
/// added work is one linear pass over templates they are already holding in memory.
#[derive(Debug, Clone, Default)]
pub struct ReferenceIndex {
    /// name → the DISTINCT components its template references, in first-seen order. A component
    /// whose template references nothing carries no entry at all.
    edges: BTreeMap<String, Vec<String>>,
}

impl ReferenceIndex {
    /// Build the graph from `(name, template)` pairs — ONE [`reference_sites`] scan per template.
    pub fn build<'a>(components: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, template) in components {
            let mut targets: Vec<String> = Vec::new();
            for site in reference_sites(template) {
                if !targets.contains(&site.component) {
                    targets.push(site.component);
                }
            }
            if !targets.is_empty() {
                edges.insert(name.to_string(), targets);
            }
        }
        Self { edges }
    }

    /// EXTEND `names` with every component reachable from it through `data-nyx-use`, transitively,
    /// in first-seen order and without duplicating anything already in the list. `names` is both the
    /// seed set and the visited set, which is what makes a cycle terminate and the cost O(edges).
    pub fn extend_with_referenced(&self, names: &mut Vec<String>) {
        let mut at = 0;
        while at < names.len() {
            if let Some(targets) = self.edges.get(&names[at]) {
                for target in targets {
                    if !names.iter().any(|n| n == target) {
                        names.push(target.clone());
                    }
                }
            }
            at += 1;
        }
    }

    /// Whether the graph carries no edge at all — a catalogue where no template references another
    /// component, i.e. the state every project was in before `data-nyx-use` existed.
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

/// Depth-first collect of each node's `component` into `out` (deduped, first-seen order).
fn collect_component_names(node: &CompositionNode, out: &mut Vec<String>) {
    if let Some(name) = &node.component {
        if !out.iter().any(|n| n == name) {
            out.push(name.clone());
        }
    }
    for child in &node.children {
        collect_component_names(child, out);
    }
}

/// A flattened reference to ONE node of a composition: its `node_key` PATH (the `Root/Header/…`
/// anchor), its nature (`component` / `inline`), and its `params`. The read the composant detail
/// uses to list the composition nodes that instantiate a component ("clic sur un nœud → détail
/// pré-paramétré") — `Serialize` for the sidecar boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CompositionNodeRef {
    /// The `/`-joined path key (the [`node_key_path`] convention).
    pub node_key: String,
    /// The catalogue component this node instantiates (`None` for an inline node).
    pub component: Option<String>,
    /// Whether the node is an `[inline]` one-off.
    pub inline: bool,
    /// The node's params (the pre-parameterization the detail opens with).
    pub params: Map<String, Value>,
    /// The PAGE this node navigates to (`prototype_pages.id`), `None` for a node that does not.
    /// Flattened here so a caller can read a composition's whole PARCOURS without rendering it —
    /// which is what the write guard and the display surfaces do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_to: Option<String>,
}

/// FLATTEN a `composition_json` tree into every node with its `node_key` PATH + params (depth-first,
/// root first). A malformed composition yields an EMPTY list (never an error). The sidecar filters
/// this by component name to list where a catalogue component is used across a proto composition.
pub fn composition_nodes(composition_json: &str) -> Vec<CompositionNodeRef> {
    let Ok(root) = serde_json::from_str::<CompositionNode>(composition_json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let root_key = root.key().to_string();
    collect_nodes(&root, &root_key, &mut out);
    out
}

/// Every `[inline]` node's MARKUP with its `node_key`, depth-first (root first). A malformed
/// composition yields an EMPTY list, exactly like [`composition_nodes`].
///
/// An inline node is a rendered Handlebars template like any other, so it obeys the same logic-less
/// contract — and this is what lets the WRITE hold it to that contract instead of leaving it to a
/// render to discover (finding `01KZ3JS6WFVGYS6JKB9C4J50HR`). Kept beside the flattener rather than
/// added as a field on [`CompositionNodeRef`]: an inline node's markup can be kilobytes, and that
/// struct crosses the IPC boundary on every composition read.
pub fn composition_inline_templates(composition_json: &str) -> Vec<(String, String)> {
    fn walk(node: &CompositionNode, prefix: &str, out: &mut Vec<(String, String)>) {
        if let Some(inline) = &node.inline {
            out.push((prefix.to_string(), inline.clone()));
        }
        for child in &node.children {
            walk(child, &format!("{prefix}/{}", child.key()), out);
        }
    }
    let Ok(root) = serde_json::from_str::<CompositionNode>(composition_json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(&root, root.key(), &mut out);
    out
}

/// Depth-first walk building each node's `/`-joined path key, pushing a [`CompositionNodeRef`].
fn collect_nodes(node: &CompositionNode, prefix: &str, out: &mut Vec<CompositionNodeRef>) {
    out.push(CompositionNodeRef {
        node_key: prefix.to_string(),
        component: node.component.clone(),
        inline: node.inline.is_some(),
        params: node.params.clone(),
        navigate_to: node.navigate_to.clone(),
    });
    for child in &node.children {
        let child_key = format!("{prefix}/{}", child.key());
        collect_nodes(child, &child_key, out);
    }
}

/// Find the node at `node_key` (a `/`-joined [`node_key_path`]) and REPLACE its `params`, returning
/// the mutated tree. The persistence primitive of the detail view: params ENREGISTRÉS PAR NŒUD.
/// Matching walks child `key()`s segment by segment; the FIRST child matching a segment wins (the
/// `key` override disambiguates same-named siblings). `None` when the path does not resolve.
pub fn set_node_params(
    root: &CompositionNode,
    node_key: &str,
    params: Map<String, Value>,
) -> Option<CompositionNode> {
    let segments: Vec<&str> = node_key.split('/').collect();
    let (first, rest) = segments.split_first()?;
    if root.key() != *first {
        return None;
    }
    let mut updated = root.clone();
    if apply_params(&mut updated, rest, params) {
        Some(updated)
    } else {
        None
    }
}

/// Recursive helper of [`set_node_params`]: descend `rest` from `node`, replacing `params` on the
/// terminal node. Returns whether the path resolved.
fn apply_params(node: &mut CompositionNode, rest: &[&str], params: Map<String, Value>) -> bool {
    let Some((next, tail)) = rest.split_first() else {
        node.params = params;
        return true;
    };
    for child in &mut node.children {
        if child.key() == *next {
            return apply_params(child, tail, params);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn components() -> Vec<RenderComponent> {
        vec![
            // A slot component: header text (escaped) + a raw html param + the composed slot.
            RenderComponent::new(
                "Card",
                "<section class=\"card\"><h2>{{title}}</h2>{{{body}}}<div class=\"slot\">{{{children}}}</div></section>",
            ),
            RenderComponent::new("Button", "<button data-state=\"{{state}}\">{{label}}</button>"),
        ]
    }

    #[test]
    fn renders_named_component_with_params() {
        let out = render_composition(
            &components(),
            &[],
            &json!({ "component": "Button", "params": { "label": "OK", "state": "idle" } })
                .to_string(),
        )
        .unwrap();
        assert_eq!(out, "<button data-state=\"idle\">OK</button>");
    }

    #[test]
    fn composes_children_into_parent_slot() {
        // A Card wrapping two Buttons — iteration by TREE SHAPE, not template logic.
        let composition = json!({
            "component": "Card",
            "params": { "title": "Actions", "body": "<p>intro</p>" },
            "children": [
                { "component": "Button", "params": { "label": "Yes", "state": "idle" } },
                { "component": "Button", "params": { "label": "No", "state": "idle" } }
            ]
        });
        let out = render_composition(&components(), &[], &composition.to_string()).unwrap();
        assert_eq!(
            out,
            "<section class=\"card\"><h2>Actions</h2><p>intro</p><div class=\"slot\">\
             <button data-state=\"idle\">Yes</button><button data-state=\"idle\">No</button>\
             </div></section>"
        );
    }

    #[test]
    fn text_param_is_escaped_html_param_is_raw() {
        // `{{title}}` escapes; `{{{body}}}` injects raw — the "param texte/html" distinction.
        let out = render_composition(
            &components(),
            &[],
            &json!({
                "component": "Card",
                "params": { "title": "<b>x</b>", "body": "<b>raw</b>" }
            })
            .to_string(),
        )
        .unwrap();
        assert!(
            out.contains("<h2>&lt;b&gt;x&lt;/b&gt;</h2>"),
            "text escaped: {out}"
        );
        assert!(out.contains("<b>raw</b>"), "html raw: {out}");
    }

    #[test]
    fn inline_node_renders_bespoke_markup() {
        let out = render_composition(
            &components(),
            &[],
            &json!({ "inline": "<hr class=\"{{variant}}\"/>", "params": { "variant": "thin" } })
                .to_string(),
        )
        .unwrap();
        assert_eq!(out, "<hr class=\"thin\"/>");
    }

    #[test]
    fn unknown_component_is_an_explicit_error() {
        let err = render_composition(
            &components(),
            &[],
            &json!({ "component": "Nope" }).to_string(),
        )
        .unwrap_err();
        assert_eq!(err, RenderError::UnknownComponent("Nope".into()));
    }

    #[test]
    fn template_with_block_logic_is_rejected() {
        let comps = vec![RenderComponent::new(
            "Loop",
            "{{#each items}}<li>{{this}}</li>{{/each}}",
        )];
        let err = render_composition(&comps, &[], &json!({ "component": "Loop" }).to_string())
            .unwrap_err();
        match err {
            RenderError::LogicInTemplate { component, .. } => assert_eq!(component, "Loop"),
            other => panic!("expected LogicInTemplate, got {other:?}"),
        }
    }

    #[test]
    fn inline_node_with_block_logic_is_rejected() {
        // An `[inline]` one-off is a rendered template too: block logic in it is REJECTED (never
        // executed), the same logic-less contract as a component template (PDR-0010, "zéro logique
        // dans les templates"). The error labels the offending node `<inline>`.
        let err = render_composition(
            &components(),
            &[],
            &json!({ "inline": "{{#each items}}<li>{{this}}</li>{{/each}}" }).to_string(),
        )
        .unwrap_err();
        match err {
            RenderError::LogicInTemplate { component, .. } => assert_eq!(component, "<inline>"),
            other => panic!("expected LogicInTemplate, got {other:?}"),
        }
    }

    /// THE ARRIMAGE of finding `01KYT78699XG6Q6P3VZC64W8H8`: the REASONS a node is unrendered, their
    /// LABELS, and the one difference between the two resolutions, all read out of
    /// `contract/design-fixtures.json`.
    ///
    /// The notion "which nodes cannot be rendered, and why" existed TWICE — here for the agent, and
    /// in the renderer's `composition-holes.ts` for the frame — with nothing making the two fail
    /// together. That is the class of defect that has already bitten this feature twice (the inertia
    /// rule, the map form of `states_json`), and both times it was invisible until somebody looked.
    /// So the reasons are now DATA in the shared fixture: a case added there fails whichever
    /// implementation drifts, and the codes/labels a display surface may depend on are stated once.
    #[test]
    fn the_shared_fixture_pins_the_unrendered_reasons_and_both_resolutions() {
        let section = crate::mcp_tools_design::design_fixture_section("unrendered_nodes");

        // (1) THE VOCABULARY — every reason, its stable code and the phrase the placeholder prints.
        // Both directions: an unlisted variant fails, and a listed one nobody implements fails too.
        let reasons = section["reasons"].as_array().expect("the reasons").clone();
        assert_eq!(
            reasons.len(),
            UnrenderedReason::ALL.len(),
            "the fixture names every reason the render can answer with — and only those"
        );
        for (reason, pinned) in UnrenderedReason::ALL.iter().zip(reasons.iter()) {
            assert_eq!(
                reason.code(),
                pinned["code"].as_str().expect("a code"),
                "the machine code a display surface switches on"
            );
            assert_eq!(
                reason.short(),
                pinned["short"].as_str().expect("a short label"),
                "[{}] the phrase the placeholder prints IN the markup",
                reason.code()
            );
        }

        // (1b) THE PLACEHOLDER'S OWN MARKUP — the class and the three attributes a surface keys off
        // (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 8). The class is written HERE and read by
        // `design/frame-srcdoc.ts`, whose unconditional rule is the only thing that draws a hole AS
        // a hole; nothing joined the two, so renaming either left both suites green while every
        // placeholder lost its box, its margin and its type size.
        assert_eq!(
            section["placeholder_class"].as_str().expect("the class"),
            UNRENDERED_CLASS,
            "the class the frame styles is the class the render writes"
        );
        let attrs = &section["placeholder_attributes"];
        for (key, attr) in [
            ("node", UNRENDERED_NODE_ATTR),
            ("component", UNRENDERED_COMPONENT_ATTR),
            ("reason", UNRENDERED_REASON_ATTR),
        ] {
            assert_eq!(
                attrs[key].as_str().expect("an attribute name"),
                attr,
                "the `{key}` attribute a surface reads off a placeholder"
            );
        }

        // (2) THE CASES — run through BOTH resolutions. The holes must be identical; only the
        // `data-nyx-*` vocabulary may differ.
        let cases = section["cases"].as_array().expect("the cases");
        assert!(cases.len() >= 3, "the fixture must carry the cases");
        for case in cases {
            let id = case["id"].as_str().unwrap_or_default();
            let catalogue: Vec<RenderComponent> = case["catalogue"]
                .as_array()
                .expect("a catalogue")
                .iter()
                .map(|c| RenderComponent {
                    name: c["name"].as_str().unwrap_or_default().to_string(),
                    template: c["template"].as_str().unwrap_or_default().to_string(),
                    states_json: c["states_json"].as_str().map(str::to_string),
                    params_json: c["params_json"].as_str().map(str::to_string),
                    css_slot: None,
                })
                .collect();
            let composition = case["composition"].to_string();
            let expect = &case["expect"];
            // THE SUBJECT's axes — what the host hands the frame as `window.__nyxAxes`. The SAME
            // value reaches both resolutions and they do OPPOSITE things with it: the frame leaves
            // those axes to the shim (and bakes no NODE axis of that name), the agent resolves them
            // itself. Absent = the frame gets no axes at all, so every node's own are baked.
            let subject = case["subject_states_json"].as_str();

            let frame = render_composition_lenient(&catalogue, &[], &composition, subject)
                .unwrap_or_else(|e| panic!("[{id}] the FRAME render always completes, got {e}"));
            let agent = resolve_composition(&catalogue, &[], &composition, None, subject, &[])
                .unwrap_or_else(|e| {
                    panic!("[{id}] the AGENT resolution always completes, got {e}")
                });

            for (mode, resolved) in [("frame", &frame), ("agent", &agent)] {
                assert_eq!(
                    resolved.node_count,
                    expect["node_count"].as_u64().expect("a node_count") as usize,
                    "[{id}/{mode}] every node the walk visited, placeholders included"
                );
                let named: Vec<(String, Option<String>, &str)> = resolved
                    .unrendered
                    .iter()
                    .map(|u| (u.node_key.clone(), u.component.clone(), u.reason.code()))
                    .collect();
                let pinned: Vec<(String, Option<String>, &str)> = expect["unrendered"]
                    .as_array()
                    .expect("the unrendered list")
                    .iter()
                    .map(|u| {
                        (
                            u["node_key"].as_str().unwrap_or_default().to_string(),
                            u["component"].as_str().map(str::to_string),
                            u["reason"].as_str().unwrap_or_default(),
                        )
                    })
                    .collect();
                assert_eq!(
                    named, pinned,
                    "[{id}/{mode}] the holes, in document order — IDENTICAL in both resolutions"
                );
                for needle in expect["markup_contains"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    let needle = needle.as_str().expect("a string");
                    assert!(
                        resolved.markup.contains(needle),
                        "[{id}/{mode}] the markup names its hole in place: {needle} missing from {}",
                        resolved.markup
                    );
                }
                // WHAT NEITHER RESOLUTION MAY LEAVE BEHIND — a mark BOTH of them consumed. A node's
                // own axis is decided in both, so the `data-nyx-if` that decided it must survive in
                // neither: the shim re-evaluates every element still carrying one, and a clause on
                // an axis IT does not declare is IGNORED, so a surviving mark would silently un-hide
                // what the render just hid (finding `01KZ41YK39SGNKF6XV87A35GPN`).
                for needle in expect["markup_absent"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    let needle = needle.as_str().expect("a string");
                    assert!(
                        !resolved.markup.contains(needle),
                        "[{id}/{mode}] the mark was consumed, not left live: {needle} still in {}",
                        resolved.markup
                    );
                }
                for needle in expect["detail_contains"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    let needle = needle.as_str().expect("a string");
                    assert!(
                        resolved
                            .unrendered
                            .iter()
                            .any(|u| u.detail.contains(needle)),
                        "[{id}/{mode}] a detail carries the correction: {needle}"
                    );
                }
            }

            // (3) THE ONE DIFFERENCE. The frame KEEPS what the shim pilots; the agent's answer holds
            // no `data-nyx-*` at all — it has no shim to read them with.
            for needle in expect["frame_only_contains"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let needle = needle.as_str().expect("a string");
                assert!(
                    frame.markup.contains(needle),
                    "[{id}] the FRAME preserves what the shim pilots: {needle} missing from {}",
                    frame.markup
                );
                assert!(
                    !agent.markup.contains(needle),
                    "[{id}] the AGENT's resolution consumed it: {needle} still in {}",
                    agent.markup
                );
            }
            for needle in expect["agent_only_contains"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let needle = needle.as_str().expect("a string");
                assert!(
                    agent.markup.contains(needle),
                    "[{id}] the AGENT's resolution applied the axis classes: {needle} missing from {}",
                    agent.markup
                );
            }
            assert!(
                !agent.markup.contains(reference::NYX_ATTR_PREFIX),
                "[{id}] the agent's answer leaves NO data-nyx-* behind: {}",
                agent.markup
            );
        }
    }

    // -----------------------------------------------------------------------
    // THE SCOPED CSS SLOT (finding `01KYVMTWB71R0D0CT1SV88J8DX`)
    // -----------------------------------------------------------------------

    fn slotted(name: &str, template: &str, css: &str) -> RenderComponent {
        RenderComponent {
            css_slot: Some(crate::css_slot::CssSlot {
                css: css.into(),
                reason: "a custom keyframe; no utility expresses one".into(),
            }),
            ..RenderComponent::new(name, template)
        }
    }

    /// THE LOADER, END TO END. A `Spinner` carries a `@keyframes` slot; a `Button` REFERENCES it
    /// (`data-nyx-use`) and a page places the `Button`. The animation must reach the frame — that is
    /// the whole finding: the sub-component is USED without ever being PLACED, which is exactly the
    /// case that used to lose it.
    #[test]
    fn a_referenced_sub_components_slot_reaches_the_frame_with_its_scope() {
        let catalogue = vec![
            RenderComponent::new(
                "Button",
                "<button class=\"btn\"><i data-nyx-use=\"Spinner\"></i>{{label}}</button>",
            ),
            slotted(
                "Spinner",
                "<span class=\"spin\"><b></b></span>",
                "@keyframes spin { to { opacity: 1 } } .spin b { animation: spin 1s linear infinite }",
            ),
        ];
        let composition = json!({ "component": "Button", "params": { "label": "Go" } }).to_string();
        let resolved =
            render_composition_lenient(&catalogue, &[], &composition, None).expect("renders");
        let scope = crate::css_slot::component_scope_id("Spinner");

        assert!(
            resolved
                .markup
                .contains(&format!("<span data-nyx-css=\"{scope}\" class=\"spin\">")),
            "the SUB-component's own root carries the scope: {}",
            resolved.markup
        );
        assert!(
            resolved
                .markup
                .contains(&format!("@keyframes {scope}-spin")),
            "and its stylesheet travels with the markup: {}",
            resolved.markup
        );
        assert_eq!(resolved.css_slots.len(), 1);
        assert_eq!(resolved.css_slots[0].component, "Spinner");
        assert!(resolved.css_slots[0].refused.is_none());
        // The BUTTON's own root is untouched — a component with no slot is not marked.
        assert!(resolved.markup.starts_with("<button class=\"btn\">"));
    }

    /// …AND IT REACHES THE AGENT TOO. `resolve_component` / `Vocabulary::Consume` run
    /// [`reference::resolve_axes`] over the ALREADY-EXPANDED markup, and that pass strips the whole
    /// `data-nyx-*` vocabulary — which used to take the sub-component's `data-nyx-css` stamp with it,
    /// so `css_slot_styles` found no scope and the shared `Spinner`'s animation reached neither the
    /// markup nor `css_slots`. The scope mark is an IDENTITY, not an instruction to the shim.
    #[test]
    fn a_referenced_sub_components_slot_survives_the_agents_own_resolution() {
        let catalogue = vec![
            RenderComponent::new(
                "Button",
                "<button class=\"btn\"><i data-nyx-use=\"Spinner\"></i>{{label}}</button>",
            ),
            slotted(
                "Spinner",
                "<span class=\"spin\"><b></b></span>",
                "@keyframes spin { to { opacity: 1 } } .spin b { animation: spin 1s linear infinite }",
            ),
        ];
        let scope = crate::css_slot::component_scope_id("Spinner");

        // The agent resolving the component it just synthesized.
        let resolved = resolve_component(&catalogue, "Button", &[], &Map::new(), "").expect("ok");
        assert!(
            resolved
                .markup
                .contains(&format!("data-nyx-css=\"{scope}\"")),
            "the sub-component's scope survives the vocabulary strip: {}",
            resolved.markup
        );
        assert!(
            resolved
                .markup
                .contains(&format!("@keyframes {scope}-spin")),
            "so its stylesheet is emitted: {}",
            resolved.markup
        );
        assert_eq!(resolved.css_slots.len(), 1, "and it is REPORTED");
        assert_eq!(resolved.css_slots[0].component, "Spinner");

        // …and the agent resolving a whole composition that places the host.
        let composition = json!({ "component": "Button", "params": { "label": "Go" } }).to_string();
        let out = resolve_composition(&catalogue, &[], &composition, None, None, &[]).expect("ok");
        assert_eq!(out.css_slots.len(), 1, "same answer through the walk");
        assert!(out.markup.contains(&format!("@keyframes {scope}-spin")));
    }

    /// THE SCOPE IS A PROPERTY OF THE MACHINERY. A stored slot whose selectors are global (a row
    /// written by hand, or before the write path refused them) still cannot reach outside the
    /// component: the render REFUSES to compile it and REPORTS it, rather than emitting it or
    /// dropping it in silence.
    #[test]
    fn a_stored_slot_that_no_longer_compiles_emits_nothing_and_says_so() {
        let catalogue = vec![slotted(
            "Card",
            "<section class=\"card\">{{{children}}}</section>",
            "body { background: red }",
        )];
        let composition = json!({ "component": "Card" }).to_string();
        let resolved =
            render_composition_lenient(&catalogue, &[], &composition, None).expect("renders");
        assert!(
            !resolved.markup.contains("<style"),
            "nothing was emitted: {}",
            resolved.markup
        );
        assert_eq!(resolved.css_slots.len(), 1, "and it is NAMED, not dropped");
        assert_eq!(
            resolved.css_slots[0].refused.as_deref(),
            Some("global_selector")
        );
        assert_eq!(resolved.css_slots[0].css, "");
    }

    /// The slot changes NOTHING about the byte-for-byte equality of the two renders: the stylesheet
    /// is appended by the SAME step on both paths, and a composition that carries none is identical
    /// to what it was before this existed.
    #[test]
    fn the_slot_is_appended_identically_by_both_renders_and_only_when_used() {
        let catalogue = vec![
            slotted(
                "Spinner",
                "<span class=\"spin\"><b></b></span>",
                "@keyframes spin { to { opacity: 1 } } .spin b { animation: spin 1s linear infinite }",
            ),
            RenderComponent::new("Card", "<section>{{{children}}}</section>"),
        ];
        let with_slot =
            json!({ "component": "Card", "children": [{ "component": "Spinner" }] }).to_string();
        let strict = render_composition(&catalogue, &[], &with_slot).expect("strict");
        let lenient =
            render_composition_lenient(&catalogue, &[], &with_slot, None).expect("lenient");
        assert_eq!(lenient.markup, strict, "byte for byte, slot included");
        assert!(
            strict.ends_with("</style>"),
            "the stylesheet is LAST: {strict}"
        );

        // A composition that reaches NO slotted component gets nothing appended at all.
        let without = json!({ "component": "Card" }).to_string();
        let plain = render_composition(&catalogue, &[], &without).expect("strict");
        assert_eq!(plain, "<section></section>");
        assert!(render_composition_lenient(&catalogue, &[], &without, None)
            .expect("lenient")
            .css_slots
            .is_empty());
    }

    /// The FRAME's leniency changes NOTHING about a composition that has no holes — proven, not
    /// asserted. This is what makes `render_composition_lenient` safe to wire under `design_render`
    /// in place of `render_composition`: same markup, byte for byte, whenever the strict one
    /// succeeds. The chip-bar, the in-place state change and the `data-nyx-page` navigation are the
    /// three features that would have died if the frame had been wired to the agent's resolution.
    #[test]
    fn the_lenient_frame_render_is_the_strict_one_when_nothing_is_missing() {
        let catalogue = vec![
            RenderComponent {
                name: "Button".into(),
                template: "<button class=\"btn\" data-nyx-page=\"pricing\">\
                           <i data-nyx-if=\"loading=on\" data-nyx-use=\"Spinner\"></i>\
                           <b data-nyx-state-case=\"on\">!</b>{{label}}</button>"
                    .into(),
                states_json: Some(
                    "{\"axes\":[{\"name\":\"loading\",\"values\":[{\"name\":\"off\"},\
                     {\"name\":\"on\",\"classes\":\"opacity-70\"}]}]}"
                        .into(),
                ),
                params_json: None,
                css_slot: None,
            },
            RenderComponent::new("Spinner", "<span class=\"spin\">◌</span>"),
            RenderComponent::new("Card", "<section>{{{children}}}</section>"),
        ];
        // A NAVIGATING node is in the tree too: the link is stamped by ONE code path, so the two
        // renders agree on it exactly as they agree on everything else (finding
        // `01KYW34347CZSNXCYS35KDG3EY`).
        let pages = vec![RenderPage::new("pg-home", "home", "Accueil")];
        let composition = json!({
            "component": "Card",
            "children": [
                { "component": "Button", "params": { "label": "Go" } },
                { "component": "Button", "params": { "label": "Stop", "loading": "on" } },
                { "key": "Home", "component": "Button", "params": { "label": "Accueil" },
                  "navigate_to": "pg-home" },
                { "inline": "<hr class=\"{{variant}}\"/>", "params": { "variant": "thin" } }
            ]
        })
        .to_string();

        let strict =
            render_composition(&catalogue, &pages, &composition).expect("the strict render");
        let lenient = render_composition_lenient(&catalogue, &pages, &composition, None)
            .expect("the lenient one");
        assert_eq!(
            lenient.markup, strict,
            "the leniency only changes what happens when a node CANNOT render"
        );
        assert!(lenient.unrendered.is_empty(), "nothing was missing");
        assert_eq!(lenient.node_count, 5);
        assert_eq!(
            lenient.navigation.len(),
            1,
            "the parcours is reported beside the markup, not instead of it"
        );
        // …and the vocabulary the shim needs really is still there. The NAVIGATION is what that
        // vocabulary is on a page: the shim is the only thing that can honour a click. A node's OWN
        // axes are NOT in this list any more (finding `01KZ41YK39SGNKF6XV87A35GPN`) — nothing in the
        // frame pilots them, so they are resolved here and their marks consumed with them; the
        // assertions just below pin that half.
        for mark in [
            "data-nyx-page=\"pricing\"",
            "data-nyx-page=\"home\"",
            "data-nyx-page-id=\"pg-home\"",
        ] {
            assert!(
                lenient.markup.contains(mark),
                "{mark} in {}",
                lenient.markup
            );
        }
        // …AND A NODE'S OWN AXES REACHED THE FRAME (finding `01KZ41YK39SGNKF6XV87A35GPN`). No
        // subject declares `loading` here, so nothing in the frame can move it: the classes of the
        // value each node FIXED are on its button, the `data-nyx-if` and `data-nyx-state-case` that
        // decided its structure are CONSUMED — leaving them would let the shim, which ignores a
        // clause on an axis it does not declare, un-hide everything this pass just hid.
        assert!(
            lenient.markup.contains("class=\"btn opacity-70\""),
            "the node that fixed loading=on wears that value's classes: {}",
            lenient.markup
        );
        assert_eq!(
            lenient.markup.matches("<b hidden>!</b>").count(),
            2,
            "the two nodes left at the `off` default hide the case: {}",
            lenient.markup
        );
        assert_eq!(
            lenient.markup.matches("<b>!</b>").count(),
            1,
            "…and the one at `on` shows it: {}",
            lenient.markup
        );
        for consumed in ["data-nyx-if", "data-nyx-state-case"] {
            assert!(
                !lenient.markup.contains(consumed),
                "{consumed} was decided here, so nothing may re-decide it: {}",
                lenient.markup
            );
        }
    }

    #[test]
    fn the_detector_flags_blocks_and_partials_but_allows_substitutions() {
        assert_eq!(
            find_refused_construct("{{#if x}}a{{/if}}"),
            Some(RefusedConstruct::Block("#if".into()))
        );
        assert_eq!(
            find_refused_construct("{{^unless}}a{{/unless}}"),
            Some(RefusedConstruct::Block("^unless".into()))
        );
        assert_eq!(
            find_refused_construct("{{~#each y}}"),
            Some(RefusedConstruct::Block("#each".into()))
        );
        // THE PARTIAL (finding `01KYSZNX4TAVSGEVBVJM7W2B21`) — every spelling, whitespace-control
        // and quoted forms included, because "the agent wrote it the other way" is not a defence.
        assert_eq!(
            find_refused_construct("<b>{{x}}</b> {{> Spinner}}"),
            Some(RefusedConstruct::Partial("> Spinner".into()))
        );
        assert_eq!(
            find_refused_construct("{{~> Spinner ~}}"),
            Some(RefusedConstruct::Partial("> Spinner ~".into()))
        );
        assert!(matches!(
            find_refused_construct("{{> \"Spinner\"}}"),
            Some(RefusedConstruct::Partial(_))
        ));
        // …and what a logic-less template IS stays allowed.
        assert_eq!(
            find_refused_construct("<b>{{x}}</b> {{{raw}}} {{! comment }}"),
            None
        );
        // The correction NAMES the form that works, with the component it was trying to include.
        let refused = find_refused_construct("{{> Spinner}}").unwrap();
        assert_eq!(refused.reason(), UnrenderedReason::TemplateHasPartial);
        assert!(
            refused.correction().contains("data-nyx-use=\"Spinner\""),
            "the correction names the reference form, on the component asked for: {}",
            refused.correction()
        );
    }

    #[test]
    fn a_partial_is_refused_at_the_render_and_resolves_to_nothing_if_it_ever_got_through() {
        // A template STORED before the guard existed carries `{{> Spinner}}`. The strict render
        // REFUSES it by name instead of expanding it with the host's context.
        let comps = vec![
            RenderComponent::new("Spinner", "<i class=\"spin\">◌</i>"),
            RenderComponent::new("Button", "<button>{{> Spinner}}{{label}}</button>"),
        ];
        let err = render_composition(
            &comps,
            &[],
            &json!({ "component": "Button", "params": { "label": "OK" } }).to_string(),
        )
        .unwrap_err();
        match &err {
            RenderError::PartialInTemplate { component, detail } => {
                assert_eq!(component, "Button");
                assert!(detail.contains("data-nyx-use"), "{detail}");
            }
            other => panic!("expected PartialInTemplate, got {other:?}"),
        }
        // …and the door is shut STRUCTURALLY too: the catalogue is no longer registered as partials,
        // so a spelling the scanner might ever miss resolves to NOTHING rather than smuggling a
        // component in with the host's params. (Here the scanner is bypassed on purpose.)
        let hb = build_registry(&[RenderComponent::new("Spinner", "<i>◌</i>")]).unwrap();
        let out = hb
            .render_template("<button>{{> Spinner}}</button>", &json!({}))
            .unwrap_or_default();
        assert!(
            !out.contains("◌"),
            "no partial table means no smuggled component: {out}"
        );
    }

    #[test]
    fn a_partial_in_an_inline_node_is_refused_too() {
        let err = render_composition(
            &components(),
            &[],
            &json!({ "inline": "<div>{{> Button}}</div>" }).to_string(),
        )
        .unwrap_err();
        match err {
            RenderError::PartialInTemplate { component, .. } => assert_eq!(component, "<inline>"),
            other => panic!("expected PartialInTemplate, got {other:?}"),
        }
    }

    /// Finding `01KYSZPG9Y8QGGVED208SG5BTB`: a component reached ONLY through `data-nyx-use` is
    /// linked — transitively — and the closure costs one template scan, not one per query.
    #[test]
    fn a_component_reached_only_by_reference_is_linked_and_the_reach_is_transitive() {
        // Card places nothing but Button in the tree; Button references Spinner; Spinner references
        // Icon. Under the old reading (the tree alone) all three of Spinner, Icon and — worse — a
        // shared leaf used by twenty hosts came back UNLINKED, i.e. a legitimate deletion candidate.
        let index = ReferenceIndex::build([
            ("Card", "<section>{{{children}}}</section>"),
            (
                "Button",
                "<button><i data-nyx-use=\"Spinner\"></i>{{label}}</button>",
            ),
            ("Spinner", "<span data-nyx-use=\"Icon\"></span>"),
            ("Icon", "<svg/>"),
            ("Unused", "<hr/>"),
        ]);
        let mut linked = referenced_component_names(
            &json!({ "component": "Card", "children": [ { "component": "Button" } ] }).to_string(),
        );
        assert_eq!(linked, vec!["Card", "Button"], "the TREE half");
        index.extend_with_referenced(&mut linked);
        assert_eq!(
            linked,
            vec!["Card", "Button", "Spinner", "Icon"],
            "…plus everything the templates reach, TRANSITIVELY — the render expands Icon into the \
             page, so deleting it breaks the page exactly as deleting Spinner does. `Unused` is \
             reached by nothing and stays out: the badge still means something."
        );

        // A CYCLE terminates (the render refuses one separately; the closure must not hang on it),
        // and each name is expanded AT MOST ONCE — which is what bounds the cost on a 179-component
        // catalogue: O(edges) after ONE scan per template, and nothing re-parsed per query.
        let cyclic = ReferenceIndex::build([
            ("A", "<div data-nyx-use=\"B\"></div>"),
            ("B", "<div data-nyx-use=\"A\"></div>"),
        ]);
        let mut names = vec!["A".to_string()];
        cyclic.extend_with_referenced(&mut names);
        assert_eq!(names, vec!["A", "B"]);

        // A DIAMOND (two hosts, one leaf) yields the leaf ONCE, in first-seen order.
        let diamond = ReferenceIndex::build([
            ("Left", "<div data-nyx-use=\"Leaf\"></div>"),
            ("Right", "<div data-nyx-use=\"Leaf\"></div>"),
            ("Leaf", "<hr/>"),
        ]);
        let mut names = vec!["Left".to_string(), "Right".to_string()];
        diamond.extend_with_referenced(&mut names);
        assert_eq!(names, vec!["Left", "Right", "Leaf"]);

        // A catalogue where nothing references anything carries no edge at all — the state every
        // project was in before `data-nyx-use`, and the closure is then a no-op.
        assert!(ReferenceIndex::build([("A", "<hr/>")]).is_empty());
    }

    #[test]
    fn node_key_path_follows_the_feedback_anchor_convention() {
        let root: CompositionNode = serde_json::from_value(json!({
            "component": "Root",
            "children": [
                { "component": "Header", "children": [ { "component": "Title" } ] },
                { "component": "Footer" }
            ]
        }))
        .unwrap();
        assert_eq!(node_key_path(&root, &[]).as_deref(), Some("Root"));
        assert_eq!(
            node_key_path(&root, &[0, 0]).as_deref(),
            Some("Root/Header/Title")
        );
        assert_eq!(node_key_path(&root, &[1]).as_deref(), Some("Root/Footer"));
        assert_eq!(node_key_path(&root, &[2]), None);
    }

    #[test]
    fn set_node_params_persists_by_node() {
        let root: CompositionNode = serde_json::from_value(json!({
            "component": "Root",
            "children": [ { "component": "Header", "children": [ { "component": "Title", "params": { "text": "old" } } ] } ]
        }))
        .unwrap();
        let mut new_params = Map::new();
        new_params.insert("text".into(), json!("new"));
        new_params.insert("state".into(), json!("active"));
        let updated = set_node_params(&root, "Root/Header/Title", new_params).unwrap();
        let title = &updated.children[0].children[0];
        assert_eq!(title.params.get("text"), Some(&json!("new")));
        assert_eq!(title.params.get("state"), Some(&json!("active")));
        // A path off the tree does not resolve.
        assert!(set_node_params(&root, "Root/Nope", Map::new()).is_none());
    }

    #[test]
    fn referenced_component_names_collects_distinct_components() {
        let composition = json!({
            "component": "Card",
            "children": [
                { "component": "Button" },
                { "inline": "<hr/>" },
                { "component": "Button" },
                { "component": "Icon" }
            ]
        });
        assert_eq!(
            referenced_component_names(&composition.to_string()),
            vec!["Card".to_string(), "Button".into(), "Icon".into()]
        );
        // A malformed composition yields an empty list (a filter must never break a listing).
        assert!(referenced_component_names("not json").is_empty());
    }

    #[test]
    fn composition_nodes_flattens_with_path_keys_and_params() {
        let composition = json!({
            "component": "Root",
            "children": [
                { "component": "Header", "params": { "title": "Hi" },
                  "children": [ { "component": "Title" } ] },
                { "inline": "<hr/>" }
            ]
        });
        let nodes = composition_nodes(&composition.to_string());
        let keys: Vec<&str> = nodes.iter().map(|n| n.node_key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["Root", "Root/Header", "Root/Header/Title", "Root/inline"]
        );
        // The Header node carries its params (the detail's pre-parameterization).
        let header = nodes.iter().find(|n| n.node_key == "Root/Header").unwrap();
        assert_eq!(header.params.get("title"), Some(&json!("Hi")));
        assert!(nodes.last().unwrap().inline);
    }

    #[test]
    fn bad_composition_json_is_an_explicit_error() {
        let err = render_composition(&components(), &[], "not json").unwrap_err();
        matches!(err, RenderError::BadComposition(_));
    }

    // --- REFERENCES (`data-nyx-use`, finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`) -------------------
    //
    // A shared sub-component is REFERENCED, not copied. The whole resolution happens here, in the
    // sidecar: the Electron renderer receives markup that is already flat and already inert.

    /// A catalogue where `Button` REFERENCES the shared `Spinner` instead of inlining it.
    fn referencing_components() -> Vec<RenderComponent> {
        vec![
            RenderComponent {
                name: "Button".into(),
                template: "<button class=\"btn\">\
                     <span data-nyx-if=\"loading=on\" data-nyx-use=\"Spinner\" \
                     data-nyx-use-axes=\"size=sm\" data-nyx-param-label=\"{{label}}\"></span>\
                     {{label}}</button>"
                    .into(),
                states_json: Some(
                    r#"{"axes":[{"name":"loading","values":[{"name":"off"},{"name":"on","classes":"opacity-70"}]}]}"#
                        .into(),
                ),
                params_json: None,
                css_slot: None,
            },
            RenderComponent {
                name: "Spinner".into(),
                template: "<i class=\"animate-spin\" aria-label=\"{{label}}\">◌</i>".into(),
                states_json: Some(
                    r#"{"axes":[{"name":"size","default":"md","values":[
                         {"name":"sm","classes":"h-3 w-3"},{"name":"md","classes":"h-4 w-4"}]}]}"#
                        .into(),
                ),
                params_json: None,
                css_slot: None,
            },
        ]
    }

    #[test]
    fn a_reference_is_flattened_with_its_params_and_its_own_axes_resolved() {
        let out = render_composition(
            &referencing_components(),
            &[],
            &json!({ "component": "Button", "params": { "label": "Payer" } }).to_string(),
        )
        .unwrap();
        assert_eq!(
            out,
            "<button class=\"btn\">\
             <span hidden><i class=\"animate-spin h-3 w-3\" aria-label=\"Payer\">◌</i></span>\
             Payer</button>",
            "the sub-component is INLINED at render time, its `sm` classes baked in and its param \
             forwarded from the host's own — and the HOST's own `loading` axis, left at its default \
             by this node, hides the placeholder"
        );
        // …and the node that FIXES the axis gets the other rendering, from the same one pass
        // (finding `01KZ41YK39SGNKF6XV87A35GPN`). This is what "the axes act" means for a frame:
        // the value's classes on the host's root, the condition it decides resolved in place.
        let loading = render_composition(
            &referencing_components(),
            &[],
            &json!({ "component": "Button", "params": { "label": "Payer", "loading": "on" } })
                .to_string(),
        )
        .unwrap();
        assert_eq!(
            loading,
            "<button class=\"btn opacity-70\">\
             <span><i class=\"animate-spin h-3 w-3\" aria-label=\"Payer\">◌</i></span>\
             Payer</button>",
            "the node's own `loading=on` puts its classes on and shows what it gates"
        );
    }

    #[test]
    fn an_expansion_never_leaks_the_vocabulary_the_shim_would_read() {
        // THE INVARIANT that lets the Electron renderer learn nothing: the flattened sub-component
        // carries NO `data-nyx-*` of its own, so the shim — which knows exactly ONE axis vocabulary,
        // the DOCUMENT's — can never meet a second one.
        let mut components = referencing_components();
        components[1].template = "<i data-nyx-axis=\"size\" data-nyx-if=\"size=sm\" \
             data-nyx-state=\"md\" class=\"animate-spin\">◌</i>\
             <b data-nyx-if=\"size=md\">!</b>"
            .into();
        let out = render_composition(
            &components,
            &[],
            &json!({ "component": "Button", "params": { "label": "Payer", "loading": "on" } })
                .to_string(),
        )
        .unwrap();
        let expansion = out
            .split_once("<span>")
            .and_then(|(_, rest)| rest.split_once("</span>"))
            .map(|(inner, _)| inner.to_string())
            .expect("the placeholder wraps the expansion");
        assert!(
            !expansion.contains("data-nyx-"),
            "the expansion is INERT: {expansion}"
        );
        assert!(
            expansion.contains("class=\"animate-spin h-3 w-3\""),
            "the marked element got its axis's classes: {expansion}"
        );
        assert!(
            expansion.contains("<b hidden>!</b>"),
            "an element absent at the fixed value is HIDDEN exactly as the shim hides it: {expansion}"
        );
        // …and the HOST's own condition, on the host's own `loading` axis, is decided by the NODE
        // that fixed it rather than left live (finding `01KZ41YK39SGNKF6XV87A35GPN`). Nothing in the
        // frame pilots a node's axes, so a surviving mark would only ever be re-read by a shim that
        // ignores it — which is how a `loading=off` node kept showing its spinner.
        assert!(
            !out.contains("data-nyx-"),
            "the whole node is inert once its own axes are resolved: {out}"
        );
    }

    #[test]
    fn a_reference_hands_its_placeholder_content_over_as_the_slot() {
        let components = vec![
            RenderComponent::new(
                "Host",
                "<div><section data-nyx-use=\"Frame\"><p>{{body}}</p></section></div>",
            ),
            RenderComponent::new("Frame", "<article>{{{children}}}</article>"),
        ];
        let out = render_composition(
            &components,
            &[],
            &json!({ "component": "Host", "params": { "body": "hello" } }).to_string(),
        )
        .unwrap();
        assert_eq!(
            out,
            "<div><section><article><p>hello</p></article></section></div>"
        );
    }

    #[test]
    fn a_reference_to_an_unknown_component_is_an_explicit_refusal() {
        // The `[DELETED AFTER THE FACT]` case too: the host stays, its reference does not resolve,
        // and the render says which component is missing — never a silent hole in the markup.
        let components = vec![RenderComponent::new(
            "Button",
            "<button><span data-nyx-use=\"Spinner\"></span></button>",
        )];
        let err = render_composition(
            &components,
            &[],
            &json!({ "component": "Button" }).to_string(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            RenderError::UnknownReference {
                host: "Button".into(),
                reference: "Spinner".into()
            }
        );
        assert!(
            err.to_string().contains("data-nyx-use=\"Spinner\"")
                && err.to_string().contains("catalogue does NOT have"),
            "the message names the reference and the correction: {err}"
        );
    }

    #[test]
    fn a_reference_cycle_is_refused_by_name_not_by_a_stack_overflow() {
        let components = vec![
            RenderComponent::new("A", "<div data-nyx-use=\"B\"></div>"),
            RenderComponent::new("B", "<div data-nyx-use=\"A\"></div>"),
        ];
        let err = render_composition(&components, &[], &json!({ "component": "A" }).to_string())
            .unwrap_err();
        assert_eq!(
            err,
            RenderError::ReferenceCycle {
                path: vec!["A".into(), "B".into(), "A".into()]
            }
        );
        assert!(
            err.to_string().contains("A → B → A"),
            "the chain is named, so the author knows which link to break: {err}"
        );
        // A SELF-reference is the degenerate cycle, caught by the very same rule.
        let selfish = vec![RenderComponent::new("A", "<div data-nyx-use=\"A\"></div>")];
        assert_eq!(
            render_composition(&selfish, &[], &json!({ "component": "A" }).to_string())
                .unwrap_err(),
            RenderError::ReferenceCycle {
                path: vec!["A".into(), "A".into()]
            }
        );
    }

    #[test]
    fn an_acyclic_but_unbounded_chain_is_refused_too() {
        // No cycle here — a strictly descending chain of DISTINCT components — so only the DEPTH
        // bound stops it. `MAX_REFERENCE_DEPTH` levels are rendered; the next one is refused.
        let deep: Vec<RenderComponent> = (0..MAX_REFERENCE_DEPTH + 3)
            .map(|i| {
                RenderComponent::new(
                    format!("C{i}"),
                    format!("<div data-nyx-use=\"C{}\"></div>", i + 1),
                )
            })
            .chain(std::iter::once(RenderComponent::new(
                format!("C{}", MAX_REFERENCE_DEPTH + 3),
                "<i>leaf</i>",
            )))
            .collect();
        let err =
            render_composition(&deep, &[], &json!({ "component": "C0" }).to_string()).unwrap_err();
        match &err {
            RenderError::ReferenceLimit { path, detail } => {
                assert_eq!(
                    path.len(),
                    MAX_REFERENCE_DEPTH + 1,
                    "the whole chain is named"
                );
                assert!(detail.contains("deeper than"), "{detail}");
            }
            other => panic!("expected ReferenceLimit, got {other:?}"),
        }

        // …and DEPTH alone does not bound the WORK: a FAN-OUT is acyclic and shallow enough to pass
        // the depth check, yet each level multiplies the one below it (2^7 leaves here). The
        // expansion BUDGET is the second bound, and it refuses just as explicitly.
        let wide: Vec<RenderComponent> = (0..MAX_REFERENCE_DEPTH - 1)
            .map(|i| {
                RenderComponent::new(
                    format!("W{i}"),
                    format!(
                        "<div data-nyx-use=\"W{next}\"></div><div data-nyx-use=\"W{next}\"></div>",
                        next = i + 1
                    ),
                )
            })
            .chain(std::iter::once(RenderComponent::new(
                format!("W{}", MAX_REFERENCE_DEPTH - 1),
                "<i>leaf</i>",
            )))
            .collect();
        match render_composition(&wide, &[], &json!({ "component": "W0" }).to_string()).unwrap_err()
        {
            RenderError::ReferenceLimit { detail, .. } => {
                assert!(detail.contains("fan-out"), "{detail}");
            }
            other => panic!("expected ReferenceLimit, got {other:?}"),
        }
    }

    #[test]
    fn a_shared_sub_component_is_fixed_once_for_every_host() {
        // THE POINT OF THE WHOLE MECHANISM: correcting the spinner corrects it in every host, with
        // no re-synthesis of the hosts — which is exactly what inlining could not do.
        let mut components = referencing_components();
        components.push(RenderComponent::new(
            "Toast",
            "<aside><span data-nyx-use=\"Spinner\"></span></aside>",
        ));
        let composition = json!({
            "component": "Card",
            "children": [
                { "component": "Button", "params": { "label": "Payer" } },
                { "component": "Toast" }
            ]
        });
        components.push(RenderComponent::new(
            "Card",
            "<section>{{{children}}}</section>",
        ));
        let before = render_composition(&components, &[], &composition.to_string()).unwrap();
        assert_eq!(before.matches("animate-spin").count(), 2);
        // ONE edit, to ONE row, and BOTH hosts follow.
        let spinner = components.iter_mut().find(|c| c.name == "Spinner").unwrap();
        spinner.template = "<svg class=\"motion-safe:animate-spin\"></svg>".into();
        let after = render_composition(&components, &[], &composition.to_string()).unwrap();
        assert_eq!(after.matches("motion-safe:animate-spin").count(), 2);
        assert!(
            !after.contains(">◌<"),
            "no copy of the old spinner survives: {after}"
        );
    }

    // --- THE WHOLE TREE, RESOLVED (finding `01KYT45C6RC7K84V1CA3561SPF`) -----------------------

    /// The catalogue a composition preview resolves against: one component with AXES (so the
    /// resolution really runs the shim's applier), one slot component, and one catalogued but
    /// UNSYNTHESIZED row (a bundle the inventory discovered and nobody wrote a template for).
    fn composition_catalogue() -> Vec<RenderComponent> {
        vec![
            RenderComponent::new("Card", "<section class=\"card\">{{{children}}}</section>"),
            RenderComponent {
                name: "Button".into(),
                template: "<button class=\"btn\"><i data-nyx-if=\"loading=on\">◌</i>{{label}}</button>"
                    .into(),
                states_json: Some(
                    r#"{"axes":[{"name":"loading","values":[{"name":"off"},{"name":"on","classes":"opacity-70"}]}]}"#
                        .into(),
                ),
                params_json: None,
                css_slot: None,
            },
            RenderComponent::new("Badge", ""),
        ]
    }

    #[test]
    fn a_composition_resolves_whole_with_the_shim_applier_run_at_every_node() {
        let composition = json!({
            "component": "Card",
            "key": "Root",
            "children": [
                { "component": "Button", "key": "Idle", "params": { "label": "Payer" } },
                { "component": "Button", "key": "Busy",
                  "params": { "label": "Patientez", "loading": "on" } }
            ]
        });
        let out = resolve_composition(
            &composition_catalogue(),
            &[],
            &composition.to_string(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(out.node_count, 3);
        assert!(out.unrendered.is_empty(), "{:?}", out.unrendered);
        assert_eq!(
            out.markup,
            "<section class=\"card\">\
             <button class=\"btn\"><i hidden>◌</i>Payer</button>\
             <button class=\"btn opacity-70\"><i>◌</i>Patientez</button>\
             </section>",
            "each node is resolved AT ITS OWN combination — the axis value a node carries IN ITS \
             PARAMS is where the shim reads it — and nothing of the vocabulary is left"
        );
        assert!(
            !out.markup.contains("data-nyx-"),
            "the answer is RESOLVED, like a component preview: {}",
            out.markup
        );
    }

    #[test]
    fn an_unrenderable_node_is_named_and_the_rest_of_the_tree_still_renders() {
        // THE CASE OF A COMPOSITION UNDER DESIGN: a `[NEW]` the catalogue does not have, and a
        // component catalogued but never synthesized. Failing the whole tree on either would hide
        // everything that IS right — the defect the UI shows as a loader that never resolves.
        let composition = json!({
            "component": "Card",
            "key": "Root",
            "children": [
                { "component": "PricingTable", "params": { "plan": "pro" },
                  "children": [ { "inline": "<em>{{n}}</em>", "params": { "n": "kept" } } ] },
                { "component": "Badge" },
                { "component": "Button", "key": "Ok", "params": { "label": "OK" } }
            ]
        });
        let out = resolve_composition(
            &composition_catalogue(),
            &[],
            &composition.to_string(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(
            out.unrendered
                .iter()
                .map(|u| (u.node_key.as_str(), u.component.as_deref(), u.reason))
                .collect::<Vec<_>>(),
            vec![
                (
                    "Root/PricingTable",
                    Some("PricingTable"),
                    UnrenderedReason::NotInCatalogue
                ),
                ("Root/Badge", Some("Badge"), UnrenderedReason::NoTemplate),
            ],
            "each unrenderable node is NAMED by its node_key, its component and why"
        );
        assert!(
            out.unrendered[0].detail.contains("proto_author_component")
                && out.unrendered[1]
                    .detail
                    .contains("design_refresh_component"),
            "…with the correction: {:?}",
            out.unrendered
        );
        assert!(
            out.markup.contains(
                "<div class=\"nyx-unrendered\" data-unrendered-node=\"Root/PricingTable\" \
                 data-unrendered-component=\"PricingTable\" \
                 data-unrendered-reason=\"not_in_catalogue\">[UNRENDERED PricingTable — not in \
                 the catalogue]<em>kept</em></div>"
            ),
            "the placeholder names the node IN the markup and KEEPS its children: {}",
            out.markup
        );
        assert!(
            out.markup
                .contains("<button class=\"btn\"><i hidden>◌</i>OK</button>"),
            "…and the sibling that is fine renders as it always did: {}",
            out.markup
        );
        assert_eq!(out.node_count, 5);
    }

    #[test]
    fn one_logic_carrying_template_does_not_blank_the_whole_page() {
        let mut catalogue = composition_catalogue();
        catalogue.push(RenderComponent::new(
            "Loop",
            "{{#each items}}<li>{{this}}</li>{{/each}}",
        ));
        let composition = json!({
            "component": "Card", "key": "Root",
            "children": [
                { "component": "Loop" },
                { "component": "Button", "key": "Ok", "params": { "label": "OK" } }
            ]
        });
        let out = resolve_composition(&catalogue, &[], &composition.to_string(), None, None, &[])
            .unwrap();
        assert_eq!(out.unrendered.len(), 1);
        assert_eq!(out.unrendered[0].reason, UnrenderedReason::TemplateHasLogic);
        assert!(
            out.markup.contains("OK"),
            "the rest of the page survives a catalogue entry nyx refuses to run: {}",
            out.markup
        );
    }

    /// The reported case, verbatim (finding `01KZ41YK39SGNKF6XV87A35GPN`, CRITICAL): a `button`
    /// declaring `variant` / `size` / `loading`, placed in a composition, its axis values set on the
    /// NODE — exactly what the params form writes and what the backend persists. Changing any one of
    /// them must CHANGE THE FRAME'S MARKUP, like a text param does.
    ///
    /// It bites on the defect itself: before this, `design_render` answered BYTE-IDENTICAL markup for
    /// the two combinations below (measured in the running app, 3486 bytes both times) — the frame
    /// ran no applier on a node's axes, and the shim only ever applies the SUBJECT's.
    #[test]
    fn a_nodes_own_axes_act_on_the_frame_unless_the_shim_owns_them() {
        let catalogue = vec![
            RenderComponent {
                name: "button".into(),
                template: "<button class=\"base\">\
                           <i data-nyx-if=\"loading=on\" class=\"spin\">◌</i>{{buttonLabel}}\
                           </button>"
                    .into(),
                states_json: Some(
                    r#"{"axes":[
                         {"name":"variant","default":"default","values":[
                           {"name":"default","classes":"bg-primary"},
                           {"name":"outline","classes":"border bg-background"}]},
                         {"name":"size","default":"default","values":[
                           {"name":"default","classes":"h-9 px-4"},{"name":"xl","classes":"h-12 px-8"}]},
                         {"name":"loading","default":"off","values":[
                           {"name":"off"},{"name":"on","classes":"opacity-70"}]}]}"#
                        .into(),
                ),
                ..RenderComponent::default()
            },
            RenderComponent::new("card", "<section>{{{children}}}</section>"),
        ];
        let composed = |variant: &str, size: &str, loading: &str| {
            json!({
                "component": "card", "key": "root",
                "children": [{ "component": "button", "key": "reset", "params": {
                    "buttonLabel": "Reset filters",
                    "variant": variant, "size": size, "loading": loading } }]
            })
            .to_string()
        };

        let at_default = render_composition_lenient(
            &catalogue,
            &[],
            &composed("default", "default", "off"),
            None,
        )
        .expect("renders")
        .markup;
        let changed =
            render_composition_lenient(&catalogue, &[], &composed("outline", "xl", "on"), None)
                .expect("renders")
                .markup;

        assert_ne!(
            at_default, changed,
            "three axis changes on a node must not compile to the same page"
        );
        assert!(
            at_default.contains("class=\"base bg-primary h-9 px-4\"")
                && at_default.contains("<i class=\"spin\" hidden>"),
            "at the defaults: the default classes, and what `loading=on` gates is hidden — {at_default}"
        );
        assert!(
            changed.contains("class=\"base border bg-background h-12 px-8 opacity-70\"")
                && changed.contains("<i class=\"spin\">"),
            "outline + xl + loading=on: the three values act — {changed}"
        );
        assert!(
            !changed.contains("data-nyx-"),
            "…and the marks that decided it are CONSUMED, or the shim would ignore the clause (an \
             axis it does not declare) and un-hide it again — {changed}"
        );

        // THE OTHER HALF OF THE RULE: an axis the SHIM pilots is left entirely alone. This is the
        // component DETAIL preview's case — one node, and the panel drives that same component's
        // axes live — and baking under a live applier is a conflict, not a redundancy: the shim
        // removes only the classes IT added, so a baked value survives every flip.
        let shim_owns = "{\"axes\":[{\"name\":\"loading\",\"values\":[{\"name\":\"off\"},\
                         {\"name\":\"on\",\"classes\":\"opacity-70\"}]}]}";
        let live = render_composition_lenient(
            &catalogue,
            &[],
            &composed("outline", "xl", "on"),
            Some(shim_owns),
        )
        .expect("renders")
        .markup;
        assert!(
            live.contains("data-nyx-if=\"loading=on\""),
            "the shim's own axis keeps its mark, live — {live}"
        );
        assert!(
            live.contains("class=\"base border bg-background h-12 px-8\"")
                && !live.contains("opacity-70"),
            "…and nothing of that axis is baked, while the two axes it does NOT pilot still act — \
             {live}"
        );
    }

    /// THE SUBJECT'S AXES REACH THE WHOLE TREE (finding `01KZ3JS7PNF3F8GFB3GYRK6CKH`), and stop
    /// exactly where the shim's do. Three rules pinned here, because each one is a decision:
    ///   * a CONDITION on a subject axis is decided at EVERY node — an `[inline]` node included,
    ///     which is where the reported defect lived (the clause named an axis the node's own markup
    ///     did not declare, so it was ignored and the element stayed visible at every value);
    ///   * the CLASSES of an unmarked subject axis land on the SUBJECT's root elements only — the
    ///     shim puts them on `#depot-root`'s children, which is this node and no other;
    ///   * a component that DECLARES an axis of the same name keeps its own: an axis name is LOCAL,
    ///     the rule `data-nyx-use` already states.
    #[test]
    fn the_subjects_axes_decide_every_condition_and_land_only_on_its_root() {
        let catalogue = vec![
            RenderComponent {
                name: "Page".into(),
                template: "<section>{{{children}}}</section>".into(),
                ..RenderComponent::default()
            },
            RenderComponent {
                name: "Chip".into(),
                template: r#"<span data-nyx-if="loading=on">busy</span>"#.into(),
                ..RenderComponent::default()
            },
            // Declares `loading` ITSELF, with its own values — the subject must not overwrite it.
            RenderComponent {
                name: "Local".into(),
                template: r#"<b data-nyx-if="loading=idle">local</b>"#.into(),
                states_json: Some(
                    r#"{"axes":[{"name":"loading","values":[{"name":"idle"},{"name":"busy"}]}]}"#
                        .into(),
                ),
                ..RenderComponent::default()
            },
        ];
        let composition = json!({
            "component": "Page", "key": "Root",
            "children": [
                { "component": "Chip", "key": "Chip" },
                { "component": "Local", "key": "Local" },
                { "key": "Inline", "inline": r#"<i data-nyx-if="loading=on">spin</i>"# }
            ]
        })
        .to_string();
        let states = r#"{"axes":[{"name":"loading","default":"off","values":[
             {"name":"off"},{"name":"on","classes":"opacity-70"}]}]}"#;

        let off =
            resolve_composition(&catalogue, &[], &composition, None, Some(states), &[]).unwrap();
        assert_eq!(
            off.axes,
            vec![("loading".to_string(), "off".to_string())],
            "an axis nobody fixed renders at its declared default, and the run SAYS which"
        );
        assert!(
            off.markup.contains("<span hidden>busy</span>")
                && off.markup.contains("<i hidden>spin</i>"),
            "the subject's axis hides BOTH the component node and the inline one: {}",
            off.markup
        );
        assert!(
            !off.markup.contains("opacity-70"),
            "the `on` classes are nowhere at `off`: {}",
            off.markup
        );
        assert!(
            off.markup.contains("<b>local</b>"),
            "a component declaring `loading` itself keeps ITS vocabulary — `idle` is its first \
             value, so its element shows: {}",
            off.markup
        );

        let on = resolve_composition(
            &catalogue,
            &[],
            &composition,
            None,
            Some(states),
            &[("loading".to_string(), "on".to_string())],
        )
        .unwrap();
        assert!(
            on.markup.contains("<span>busy</span>") && on.markup.contains("<i>spin</i>"),
            "fixing the value has an EFFECT at every node: {}",
            on.markup
        );
        assert_eq!(
            on.markup.matches("opacity-70").count(),
            1,
            "the unmarked subject axis puts its classes on the SUBJECT's root and nowhere else — \
             not once per node: {}",
            on.markup
        );
        assert!(
            on.markup.starts_with(r#"<section class="opacity-70">"#),
            "…and that root is the composition's own: {}",
            on.markup
        );
        assert!(
            on.markup.contains("<b>local</b>"),
            "the local `loading` axis is still the component's own — `on` is not one of its values \
             and never reached it: {}",
            on.markup
        );
        assert!(!on.markup.contains("data-nyx-"), "{}", on.markup);
    }

    #[test]
    fn a_node_key_renders_one_subtree_and_an_unknown_one_names_the_tree() {
        let composition = json!({
            "component": "Card", "key": "Root",
            "children": [
                { "component": "Card", "key": "Aside",
                  "children": [ { "component": "Button", "key": "Ok", "params": { "label": "OK" } } ] }
            ]
        });
        let out = resolve_composition(
            &composition_catalogue(),
            &[],
            &composition.to_string(),
            Some("Root/Aside"),
            None,
            &[],
        )
        .unwrap();
        assert_eq!(
            out.markup,
            "<section class=\"card\"><button class=\"btn\"><i hidden>◌</i>OK</button></section>"
        );
        assert_eq!(out.node_count, 2, "only the subtree is walked");
        match resolve_composition(
            &composition_catalogue(),
            &[],
            &composition.to_string(),
            Some("Root/Nope"),
            None,
            &[],
        )
        .unwrap_err()
        {
            RenderError::UnknownNode { available, .. } => assert_eq!(
                available,
                vec![
                    "Root".to_string(),
                    "Root/Aside".into(),
                    "Root/Aside/Ok".into()
                ],
                "the refusal names the keys that DO exist"
            ),
            other => panic!("expected UnknownNode, got {other:?}"),
        }
    }

    #[test]
    fn a_composition_that_does_not_parse_is_still_the_one_refusal() {
        // The tree is the SUBJECT here: a subject that does not exist is the only thing left to
        // refuse (everything INSIDE it is named instead).
        assert!(matches!(
            resolve_composition(&composition_catalogue(), &[], "not json", None, None, &[])
                .unwrap_err(),
            RenderError::BadComposition(_)
        ));
    }

    // -----------------------------------------------------------------------
    // INTER-PAGE NAVIGATION (finding `01KYW34347CZSNXCYS35KDG3EY`)
    // -----------------------------------------------------------------------

    /// A two-page prototype: the ids are what a node links to, the slugs are what the shim reads.
    fn two_pages() -> Vec<RenderPage> {
        vec![
            RenderPage::new("pg-home", "home", "Accueil"),
            RenderPage::new("pg-pricing", "pricing", "Tarifs"),
        ]
    }

    fn nav_catalogue() -> Vec<RenderComponent> {
        vec![
            RenderComponent::new("Button", "<button class=\"btn\">{{label}}</button>"),
            RenderComponent::new("Card", "<section>{{{children}}}</section>"),
            // A component whose template renders BARE TEXT — nothing a link can hang on.
            RenderComponent::new("Caption", "{{label}}"),
        ]
    }

    #[test]
    fn a_node_links_to_a_page_by_id_and_the_render_carries_the_readable_slug() {
        let composition = json!({
            "component": "Card",
            "children": [
                { "component": "Button", "params": { "label": "Voir les tarifs" },
                  "navigate_to": "pg-pricing" }
            ]
        })
        .to_string();

        let frame = render_composition_lenient(&nav_catalogue(), &two_pages(), &composition, None)
            .expect("the frame render");

        // The SHIM's attribute carries the SLUG (nothing about the shim changes), and the durable
        // id travels beside it.
        assert!(
            frame
                .markup
                .contains("<button data-nyx-page=\"pricing\" data-nyx-page-id=\"pg-pricing\""),
            "{}",
            frame.markup
        );
        // …and the READABLE half is in the report, resolved from the id at THIS render.
        assert_eq!(frame.navigation.len(), 1);
        let link = &frame.navigation[0];
        assert_eq!(link.node_key, "Card/Button");
        assert_eq!(link.page_id, "pg-pricing");
        assert_eq!(link.slug.as_deref(), Some("pricing"));
        assert_eq!(link.title.as_deref(), Some("Tarifs"), "what a screen shows");
        assert_eq!(link.status, NavigationStatus::Linked);
    }

    #[test]
    fn renaming_a_page_moves_the_link_because_the_link_is_an_id() {
        // THE reason the model stores an id: the page is renamed (slug AND title), the composition
        // is NOT rewritten, and the next render simply emits the new name.
        let composition =
            json!({ "component": "Button", "params": { "label": "Go" }, "navigate_to": "pg-pricing" })
                .to_string();
        let renamed = vec![RenderPage::new("pg-pricing", "plans", "Nos formules")];

        let after = render_composition_lenient(&nav_catalogue(), &renamed, &composition, None)
            .expect("the frame render");
        assert!(
            after.markup.contains("data-nyx-page=\"plans\""),
            "the render follows the rename: {}",
            after.markup
        );
        assert_eq!(after.navigation[0].title.as_deref(), Some("Nos formules"));
    }

    #[test]
    fn a_link_to_a_page_that_is_gone_is_named_in_place_and_never_navigates() {
        let composition =
            json!({ "component": "Button", "params": { "label": "Go" }, "navigate_to": "pg-deleted" })
                .to_string();

        // THE FRAME: it completes, and the defect is where it sits.
        let frame = render_composition_lenient(&nav_catalogue(), &two_pages(), &composition, None)
            .expect("the frame render");
        assert!(
            frame
                .markup
                .contains("data-nyx-page-missing=\"pg-deleted\""),
            "the NAMED marker sits on the node: {}",
            frame.markup
        );
        assert!(
            !frame.markup.contains("data-nyx-page=\""),
            "and nothing can navigate to a page that is not there: {}",
            frame.markup
        );
        let link = &frame.navigation[0];
        assert_eq!(link.status, NavigationStatus::UnknownPage);
        assert_eq!(link.slug, None);
        assert!(
            link.detail.contains("home") && link.detail.contains("pricing"),
            "the report names the pages that DO exist: {}",
            link.detail
        );

        // THE STRICT render REFUSES the same thing — the two never disagree on what is wrong.
        match render_composition(&nav_catalogue(), &two_pages(), &composition).unwrap_err() {
            RenderError::UnknownPage {
                node_key, page_id, ..
            } => {
                assert_eq!(
                    (node_key.as_str(), page_id.as_str()),
                    ("Button", "pg-deleted")
                );
            }
            other => panic!("expected UnknownPage, got {other:?}"),
        }

        // A render given NO directory at all says SO, rather than pretending the page is gone
        // without explanation — the case a caller that never named its prototype produces.
        let blind = render_composition_lenient(&nav_catalogue(), &[], &composition, None).unwrap();
        assert!(
            blind.navigation[0].detail.contains("NO page directory"),
            "{}",
            blind.navigation[0].detail
        );
    }

    #[test]
    fn a_link_with_nothing_to_click_is_named_too() {
        // (a) a template that renders bare text: no element, nothing to stamp.
        let bare = json!({ "component": "Caption", "params": { "label": "hi" },
                           "navigate_to": "pg-home" })
        .to_string();
        let out = render_composition_lenient(&nav_catalogue(), &two_pages(), &bare, None).unwrap();
        assert_eq!(out.markup, "hi", "the node still renders");
        assert_eq!(out.navigation[0].status, NavigationStatus::NoAnchor);
        assert!(matches!(
            render_composition(&nav_catalogue(), &two_pages(), &bare).unwrap_err(),
            RenderError::BadNavigation { .. }
        ));

        // (b) a STRUCTURAL node — it is only its children, so stamping would make each of THEM
        // clickable, a decision nobody wrote. Named instead, and the children render untouched.
        let structural = json!({
            "key": "Nav",
            "navigate_to": "pg-home",
            "children": [ { "component": "Button", "params": { "label": "A" } } ]
        })
        .to_string();
        let out =
            render_composition_lenient(&nav_catalogue(), &two_pages(), &structural, None).unwrap();
        assert_eq!(out.markup, "<button class=\"btn\">A</button>");
        assert_eq!(out.navigation[0].status, NavigationStatus::NoAnchor);
        assert!(
            out.navigation[0].detail.contains("STRUCTURAL"),
            "{}",
            out.navigation[0].detail
        );
    }

    #[test]
    fn a_cycle_of_navigation_renders_because_a_click_is_not_an_expansion() {
        // A → B and B → A on the same page tree: the "Back" button every parcours has. A cycle of
        // `data-nyx-use` is refused because the RENDER would never end; a cycle of navigation costs
        // one attribute per node, because the render never follows a link. Both links are carried.
        let composition = json!({
            "component": "Card",
            "children": [
                { "key": "Go", "component": "Button", "params": { "label": "Tarifs" },
                  "navigate_to": "pg-pricing" },
                { "key": "Back", "component": "Button", "params": { "label": "Retour" },
                  "navigate_to": "pg-home" }
            ]
        })
        .to_string();
        let out = render_composition_lenient(&nav_catalogue(), &two_pages(), &composition, None)
            .expect("a cycle is a parcours, not a refusal");
        assert_eq!(
            out.navigation
                .iter()
                .map(|l| (l.node_key.as_str(), l.slug.as_deref().unwrap_or("")))
                .collect::<Vec<_>>(),
            vec![("Card/Go", "pricing"), ("Card/Back", "home")]
        );
        assert!(out
            .navigation
            .iter()
            .all(|l| l.status == NavigationStatus::Linked));
        // The SAME "Back" component placed twice keeps ONE destination per PLACEMENT — which is the
        // whole reason the link is not on the component.
        assert_eq!(out.markup.matches("data-nyx-page=").count(), 2);
    }

    #[test]
    fn the_nodes_destination_beats_one_a_template_baked_in() {
        // A template may still carry a literal `data-nyx-page="…"` — it is markup, and the shim
        // honours it. When the NODE states a destination too, the node wins and there is exactly ONE
        // attribute: two of them on a tag is not a merge, it is a browser silently picking the first.
        let catalogue = vec![RenderComponent::new(
            "Button",
            "<button data-nyx-page=\"stale\" class=\"btn\">{{label}}</button>",
        )];
        let composition =
            json!({ "component": "Button", "params": { "label": "Go" }, "navigate_to": "pg-home" })
                .to_string();
        let out = render_composition_lenient(&catalogue, &two_pages(), &composition, None).unwrap();
        assert_eq!(
            out.markup.matches("data-nyx-page=").count(),
            1,
            "{}",
            out.markup
        );
        assert!(
            out.markup.contains("data-nyx-page=\"home\""),
            "{}",
            out.markup
        );
        assert!(
            out.markup.contains("class=\"btn\""),
            "every other attribute survives: {}",
            out.markup
        );
    }

    #[test]
    fn the_agents_resolution_consumes_the_marks_and_answers_with_the_parcours() {
        // An agent has no shim: a mark it cannot run is a mark it must not read back. The parcours
        // is not lost for that — it comes back RESOLVED, which is the form an agent can reason about.
        let composition =
            json!({ "component": "Button", "params": { "label": "Go" }, "navigate_to": "pg-pricing" })
                .to_string();
        let agent = resolve_composition(
            &nav_catalogue(),
            &two_pages(),
            &composition,
            None,
            None,
            &[],
        )
        .unwrap();
        assert!(
            !agent.markup.contains("data-nyx-"),
            "the agent's resolution leaves NO vocabulary: {}",
            agent.markup
        );
        assert_eq!(agent.navigation[0].title.as_deref(), Some("Tarifs"));
    }

    #[test]
    fn a_link_survives_a_per_node_params_edit() {
        // The model half of this finding: the link must SURVIVE. `set_node_params` re-serializes the
        // whole tree, so a field the struct did not know about would be dropped on the first param
        // edit — the link would vanish the moment the user touched the node it sits on.
        let root: CompositionNode = serde_json::from_str(
            &json!({
                "component": "Card",
                "children": [ { "component": "Button", "params": { "label": "a" },
                                "navigate_to": "pg-home" } ]
            })
            .to_string(),
        )
        .unwrap();
        let mut params = Map::new();
        params.insert("label".into(), Value::String("b".into()));
        let updated = set_node_params(&root, "Card/Button", params).expect("the node resolves");
        let round_tripped = serde_json::to_string(&updated).unwrap();
        assert!(
            round_tripped.contains("\"navigate_to\":\"pg-home\""),
            "the link survived the edit: {round_tripped}"
        );
        // …and it is READABLE off the flattened tree, which is what the write guard and the display
        // surfaces walk.
        let nodes = composition_nodes(&round_tripped);
        assert_eq!(nodes[1].navigate_to.as_deref(), Some("pg-home"));
        assert_eq!(
            nodes[0].navigate_to, None,
            "a node without a link states none"
        );
    }

    /// THE ARRIMAGE of the `node_key` CONVENTION (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 2).
    ///
    /// ONE RULE, and it was written four times. A `node_key` is the `/`-join of every node's SEGMENT
    /// LABEL from the root down — the explicit `key`, else the `component` name, else `"inline"`.
    /// It is what a pinned comment is STORED on, what a params write ADDRESSES, and what the coder
    /// seam hands over: change a segment on one side alone and the stored comments stop re-anchoring
    /// while the writes land on the wrong node, silently, both.
    ///
    /// The cases live in `contract/design-fixtures.json` and the renderer replays the same list in
    /// `specs/composition-nodes.test.ts`. `same-named siblings collide` is deliberately pinned as
    /// the CONVENTION's own consequence: an implementation that "improved" it into an index suffix
    /// on one side alone would move every anchor stored by the other.
    #[test]
    fn the_shared_fixture_pins_the_node_key_convention() {
        let section = crate::mcp_tools_design::design_fixture_section("node_key");
        assert_eq!(
            section["separator"].as_str().expect("the separator"),
            "/",
            "the join character of a node_key path"
        );
        assert_eq!(
            section["inline_label"].as_str().expect("the inline label"),
            CompositionNode::default().key(),
            "the label a node with neither `key` nor `component` is addressed by"
        );
        let cases = section["cases"].as_array().expect("the cases");
        assert!(cases.len() >= 3, "the fixture must carry the cases");
        for case in cases {
            let id = case["id"].as_str().unwrap_or_default();
            let composition = case["composition"].to_string();
            let got: Vec<String> = composition_nodes(&composition)
                .into_iter()
                .map(|n| n.node_key)
                .collect();
            let pinned: Vec<String> = case["node_keys"]
                .as_array()
                .expect("the node keys")
                .iter()
                .map(|k| k.as_str().expect("a node key").to_string())
                .collect();
            assert_eq!(
                got, pinned,
                "[{id}] the node_key path of every node, in order"
            );

            // …and the SAME paths come out of `node_key_path`, the index-driven half of the
            // convention. Two derivations of one rule inside one language is how it drifts.
            let root: CompositionNode =
                serde_json::from_str(&composition).expect("the fixture composition parses");
            let mut by_indices: Vec<String> = Vec::new();
            fn walk(
                node: &CompositionNode,
                root: &CompositionNode,
                indices: &mut Vec<usize>,
                out: &mut Vec<String>,
            ) {
                out.push(node_key_path(root, indices).expect("a path inside the tree"));
                for (i, child) in node.children.iter().enumerate() {
                    indices.push(i);
                    walk(child, root, indices, out);
                    indices.pop();
                }
            }
            walk(&root, &root, &mut Vec::new(), &mut by_indices);
            assert_eq!(
                by_indices, pinned,
                "[{id}] node_key_path walks the SAME convention as composition_nodes"
            );
        }
    }

    /// The navigation VOCABULARY is DATA, like the unrendered reasons: the attribute names the
    /// markup carries and the status codes a display surface switches on live in
    /// `contract/design-fixtures.json`, so the renderer that will read them cannot drift from the
    /// render that writes them.
    #[test]
    fn the_shared_fixture_pins_the_navigation_vocabulary() {
        let section = crate::mcp_tools_design::design_fixture_section("navigation");

        let attributes = &section["attributes"];
        assert_eq!(attributes["page"].as_str(), Some(PAGE_ATTR));
        assert_eq!(attributes["page_id"].as_str(), Some(PAGE_ID_ATTR));
        assert_eq!(attributes["missing"].as_str(), Some(PAGE_MISSING_ATTR));

        let statuses = section["statuses"]
            .as_array()
            .expect("the statuses")
            .clone();
        assert_eq!(
            statuses.len(),
            NavigationStatus::ALL.len(),
            "the fixture names every status the render can answer with — and only those"
        );
        for (status, pinned) in NavigationStatus::ALL.iter().zip(statuses.iter()) {
            assert_eq!(status.code(), pinned["code"].as_str().expect("a code"));
            assert_eq!(status.short(), pinned["short"].as_str().expect("a label"));
            assert!(
                pinned["why"].as_str().is_some_and(|w| w.len() > 40),
                "[{}] a status nobody explained is a status nobody can implement",
                status.code()
            );
        }

        // The SEAM's own three, pinned the same way (they are a different question — "is this
        // destination part of the frozen design?" — and they are answered in `proto_composition`).
        let seam: Vec<&str> = section["seam_statuses"]
            .as_array()
            .expect("the seam statuses")
            .iter()
            .map(|s| s["code"].as_str().expect("a code"))
            .collect();
        assert_eq!(
            seam,
            vec![
                crate::db::NAVIGATION_PINNED,
                crate::db::NAVIGATION_UNPINNED,
                crate::db::NAVIGATION_UNKNOWN
            ]
        );
    }

    /// THE INVALIDATION MARKER NEUTRALIZES, IT DOES NOT ONLY BADGE (finding
    /// `01KZ3JN69E32FYBF01NBE4FWJD`, volet B).
    ///
    /// The stamp used to drop only the attributes it was about to re-pose, so a `data-nyx-page` a
    /// component's TEMPLATE had baked in survived right beside `data-nyx-page-missing` — and the
    /// shim, which reads exactly that attribute, navigated to the slug frozen in the template while
    /// this very render answered `unknown_page`. The code's comment and the fixture's note both said
    /// the click was dead by construction; it was not.
    ///
    /// The case is PINNED in `contract/design-fixtures.json` (`navigation.neutralisation_case`), so
    /// the markup asserted here is the markup the shared contract states.
    #[test]
    fn an_invalidated_link_leaves_no_concurrent_link_behind() {
        let case = crate::mcp_tools_design::design_fixture_section("navigation")
            ["neutralisation_case"]
            .clone();
        let template = case["template"].as_str().expect("the fixture's template");
        let catalogue = vec![RenderComponent::new("Linky", template)];
        let composition = json!({
            "component": "Linky", "key": "Root",
            "navigate_to": case["navigate_to"].as_str().expect("a dead destination"),
        })
        .to_string();

        // No page directory at all — the id resolves to nothing, which is the render-time shape of
        // a page that DISAPPEARED (the write refuses a link that was never valid).
        let out = resolve_composition(&catalogue, &[], &composition, None, None, &[]).unwrap();
        assert_eq!(out.navigation.len(), 1);
        assert_eq!(
            out.navigation[0].status.code(),
            case["expected_status"].as_str().expect("the status")
        );

        // The FRAME's render is the one that matters here: it PRESERVES the vocabulary, so it is
        // where a surviving `data-nyx-page` would still be clickable.
        let frame = render_composition_lenient(&catalogue, &[], &composition, None).unwrap();
        assert_eq!(
            frame.markup,
            case["expected_markup"].as_str().expect("the markup"),
            "the marker is the ONLY navigation mark left on the root"
        );
        for absent in case["absent_attributes"]
            .as_array()
            .expect("the attributes that must be gone")
        {
            let attr = absent.as_str().expect("an attribute name");
            // `="` on purpose: `data-nyx-page` is a PREFIX of `data-nyx-page-missing`, and the
            // marker is precisely what must still be there.
            assert!(
                !frame.markup.contains(&format!("{attr}=\"")),
                "`{attr}` baked into the template must NOT survive the invalidation marker — the \
                 shim reads it and would navigate: {}",
                frame.markup
            );
        }

        // …and a link that RESOLVES still wins over the template's own, as it always did.
        let live = render_composition_lenient(
            &catalogue,
            &[RenderPage::new("pg-gone", "checkout", "Checkout")],
            &composition,
            None,
        )
        .unwrap();
        assert!(
            live.markup.contains(r#"data-nyx-page="checkout""#)
                && !live.markup.contains(r#"data-nyx-page="pricing""#)
                && !live.markup.contains("data-nyx-page-missing"),
            "the modelled destination replaces the baked-in one, and nothing of the marker remains: \
             {}",
            live.markup
        );
    }
}
