//! The SCOPED CSS SLOT — a component's strict, tokenised, bounded and SIGNALLED escape hatch for
//! what Tailwind utilities cannot express (v2.7 — Prototype : machinerie, finding
//! `01KYVMTWB71R0D0CT1SV88J8DX`).
//!
//! # Why this module exists
//!
//! A synthesized component carries its look in TAILWIND CLASSES, and that is the framework of the
//! project: the look is made of utilities and of the project's tokens, first and by default. But
//! four things a real component needs cannot be written as a class — a custom `@keyframes`, the
//! CONTENT of a `::before` / `::after`, a relational selector (`:has`), a `@container` query. Until
//! this module, anything of that kind was SILENTLY LOST: an agent that had written a loader's
//! animation stored a template without it, every tool answered `ok`, and the component reached the
//! coder with nobody — not the agent, not the reviewer, not the developer — knowing something was
//! missing. That silence is the defect; the slot is its repair.
//!
//! # The rule, in this order
//!
//! Tailwind is THE channel. The slot is admitted ONLY for what utilities cannot express, and a slot
//! that exercises NONE of the four admitted constructs is REFUSED ([`SlotRefusal::NoEscapeHatch`]) —
//! that is what keeps it a relief valve and not a dumping ground, and it is decidable, so it is
//! decided by the machinery rather than asked of the agent.
//!
//! # The four constraints, none negotiable
//!
//! 1. **TOKENISED** — a hard-coded colour is a refusal, in every form (hex, `rgb()`, `oklch()`, a
//!    named colour on a colour-valued property). A raw absolute length on a property the design
//!    system SCALES (padding, gap, radius, font-size…) is a refusal too. Every `var(--x)` must name
//!    a token the project really declares ([`ensure_tokens_known`]) — a dangling `var()` renders to
//!    nothing, which is the silent loss again, one level down.
//! 2. **SCOPED** — [`compile`] REWRITES every selector so it is anchored on the component's scope
//!    attribute, and RENAMES every `@keyframes` identifier under the scope id. The CSS that leaves
//!    here CANNOT reach outside the component's subtree even when the agent wrote a global selector,
//!    because the selector the browser sees is not the one the agent wrote. See [`compile`].
//! 3. **BOUNDED** — [`MAX_CSS_SLOT_BYTES`] on the CSS, [`MAX_CSS_SLOT_REASON_BYTES`] on the reason.
//! 4. **SIGNALLED** — the slot is inseparable from its REASON (a slot without one is refused), the
//!    render reports every slot it emitted, and the coder seam carries the compiled CSS, its scope
//!    and its reason. The exception is traceable from the agent that wrote it to the developer who
//!    implements it.
//!
//! # It does NOT touch the frame's CSP
//!
//! The compiled CSS travels as a `<style>` element inside the markup the frame already displays, and
//! the frame's `Content-Security-Policy` already carries `style-src 'unsafe-inline'` for the theme
//! layer and the canvas. No directive is relaxed, no external resource is reachable: `@import` and
//! `url()` are REFUSED here (they are the only two forms that would fetch), so a slot can never turn
//! into a network request the CSP would have to allow.
//!
//! Everything here is PURE — no DB, no disk, no `handlebars`. The scanner is hand-rolled and
//! dependency-free, like [`crate::design_tokens::extract_css_variables`]: what it cannot understand
//! is REFUSED, never guessed.

use std::collections::BTreeSet;

/// The attribute the render STAMPS on a slotted component's root element(s), and the anchor every
/// compiled selector is rewritten against. Space-separated (`~=`), so an element can sit in more
/// than one scope without either losing its own.
pub const SCOPE_ATTR: &str = "data-nyx-css";

/// The attribute the emitted `<style>` element carries, naming the scope it belongs to — how a
/// display surface finds the block that goes with a badge, and how a reader tells a slot's stylesheet
/// from the frame's own.
pub const SLOT_STYLE_ATTR: &str = "data-nyx-css-slot";

/// How many bytes of CSS one slot may carry. A `@keyframes` plus the two or three rules that drive
/// it fit in a few hundred; this is the ceiling that stops the escape hatch from becoming a
/// stylesheet — the same discipline every other payload in this machinery is held to.
pub const MAX_CSS_SLOT_BYTES: usize = 2048;

/// How many bytes the slot's REASON may carry. A reason is one or two sentences naming what the
/// utilities could not do; it is read by a human in review and by the coder in the seam.
pub const MAX_CSS_SLOT_REASON_BYTES: usize = 240;

// ---------------------------------------------------------------------------
// WHAT A SLOT MAY BE FOR — the four constructs utilities cannot express
// ---------------------------------------------------------------------------

/// One of the FOUR things Tailwind utilities cannot express, and therefore the only reasons a slot
/// may exist at all. A compiled slot reports which ones it really exercises; a slot that exercises
/// NONE is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SlotFeature {
    /// A custom `@keyframes` — the animation a utility cannot describe (the loader).
    Keyframes,
    /// The CONTENT of a `::before` / `::after` — generated content has no utility.
    PseudoElement,
    /// A relational selector (`:has(…)`) — a parent styled from its children.
    Has,
    /// A `@container` query — a rule on the CONTAINER's size, not the viewport's.
    Container,
}

impl SlotFeature {
    /// The stable machine code — pinned by `contract/design-fixtures.json`.
    pub fn code(self) -> &'static str {
        match self {
            SlotFeature::Keyframes => "keyframes",
            SlotFeature::PseudoElement => "pseudo_element",
            SlotFeature::Has => "has",
            SlotFeature::Container => "container",
        }
    }
}

/// The four codes, in declaration order — what a descriptor and the manual enumerate.
pub const SLOT_FEATURES: [SlotFeature; 4] = [
    SlotFeature::Keyframes,
    SlotFeature::PseudoElement,
    SlotFeature::Has,
    SlotFeature::Container,
];

// ---------------------------------------------------------------------------
// THE STORED SLOT
// ---------------------------------------------------------------------------

/// A component's CSS slot AS AUTHORED: the CSS the agent wrote, and the REASON it wrote it. The two
/// are ONE value — a slot with no reason is refused at the write, because "signalled with its
/// reason" is a constraint of the slot, not a nicety of the UI.
///
/// Persisted in `design_components.css_slot_json` as `{ "css": …, "reason": … }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CssSlot {
    /// The CSS AS THE AGENT WROTE IT — never the compiled form. The scope id is derived from the
    /// component's NAME, so storing the compiled CSS would freeze a scope a rename invalidates;
    /// [`compile`] runs again at every render, and the guarantee travels with the code, not with the
    /// row.
    pub css: String,
    /// WHY this exception exists — what the utilities could not do. Carried into the review UI and
    /// into the coder seam verbatim.
    pub reason: String,
}

impl CssSlot {
    /// Read a stored `css_slot_json` back. TOLERANT like every other stored blob here: a blank,
    /// malformed or shapeless value yields `None` (no slot) rather than an error — a catalogue read
    /// must never break on one bad row.
    pub fn parse(stored: Option<&str>) -> Option<Self> {
        let raw = stored.map(str::trim).filter(|s| !s.is_empty())?;
        let value: serde_json::Value = serde_json::from_str(raw).ok()?;
        let object = value.as_object()?;
        let css = object.get("css")?.as_str()?.to_string();
        if css.trim().is_empty() {
            return None;
        }
        let reason = object
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Some(CssSlot { css, reason })
    }

    /// The stored form.
    pub fn to_json(&self) -> String {
        serde_json::json!({ "css": self.css, "reason": self.reason }).to_string()
    }
}

// ---------------------------------------------------------------------------
// THE REFUSALS
// ---------------------------------------------------------------------------

/// Why a CSS slot is NOT admitted. Every variant is a REFUSAL at the write, never a warning: the
/// four constraints are not negotiable, a warning is how the loader disappeared in the first place,
/// and the agent is still holding the CSS it just composed — the fix costs it one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotRefusal {
    /// The CSS is blank once trimmed — there is nothing to store.
    Blank,
    /// No reason was given. A slot is an EXCEPTION; an exception nobody can explain is not one.
    NoReason,
    /// The reason is past [`MAX_CSS_SLOT_REASON_BYTES`].
    ReasonTooLong(usize),
    /// The CSS is past [`MAX_CSS_SLOT_BYTES`].
    TooLarge(usize),
    /// The CSS carries `</` — markup, in a stylesheet. Refused at the source rather than escaped on
    /// the way out, so nothing this module emits can ever close the `<style>` element it travels in.
    Markup,
    /// `!important` — an escalation over the utilities, which is the opposite of what the slot is
    /// for.
    Important,
    /// `url(…)` — an external resource. The frame's CSP grants no `img-src`/`font-src` beyond
    /// `data:`, and a slot that needed one would be a slot that relaxes the CSP.
    ExternalResource,
    /// An at-rule with no selector to anchor (`@import`, `@font-face`, `@layer`, `@property`,
    /// `@page`, `@charset`, `@apply`…). Carries the keyword.
    AtRule(String),
    /// A nested rule inside a declaration block — this scanner scopes SELECTORS, and a nested one
    /// would inherit a prefix it never sees. Carries the rule's prelude.
    NestedRule(String),
    /// A block or a string the scanner could not close. Carries what it was reading.
    Unterminated(String),
    /// A declaration that is not `property: value`. Carries it.
    BadDeclaration(String),
    /// A selector that, once anchored on the scope, could match NOTHING — `html`, `body`, `:root`,
    /// `:host`. Refused rather than silently neutered: a rule that quietly does nothing is exactly
    /// the silence this slot exists to end. Carries the selector.
    GlobalSelector(String),
    /// A selector that starts with a combinator (`> .x`, `+ .y`, `~ .z`) — anchored on the scope it
    /// would select a SIBLING or a CHILD OF THE PARENT, i.e. outside the component. Carries it.
    EscapingSelector(String),
    /// `:scope` / `&` used anywhere but as the FIRST token of a selector. Carries the selector.
    MisplacedScope(String),
    /// An empty selector (a stray comma).
    EmptySelector,
    /// A colour written in the CSS instead of referenced from the token layer. Carries
    /// `(property, value)`.
    HardcodedColour { property: String, value: String },
    /// An absolute length (`px`, `pt`, `cm`…) on a property the design system SCALES. Carries
    /// `(property, value)`.
    HardcodedLength { property: String, value: String },
    /// A `var(--x)` naming a custom property the project's token layer does not declare and the slot
    /// does not declare either. Carries the name.
    UnknownToken(String),
    /// The slot exercises NONE of the four constructs utilities cannot express — so it is not an
    /// escape hatch, it is CSS written instead of the design system.
    NoEscapeHatch,
}

impl SlotRefusal {
    /// The stable machine code — pinned by `contract/design-fixtures.json`, so a display surface can
    /// key off the reason without parsing prose.
    pub fn code(&self) -> &'static str {
        match self {
            SlotRefusal::Blank => "blank",
            SlotRefusal::NoReason => "no_reason",
            SlotRefusal::ReasonTooLong(_) => "reason_too_long",
            SlotRefusal::TooLarge(_) => "too_large",
            SlotRefusal::Markup => "markup",
            SlotRefusal::Important => "important",
            SlotRefusal::ExternalResource => "external_resource",
            SlotRefusal::AtRule(_) => "at_rule",
            SlotRefusal::NestedRule(_) => "nested_rule",
            SlotRefusal::Unterminated(_) => "unterminated",
            SlotRefusal::BadDeclaration(_) => "bad_declaration",
            SlotRefusal::GlobalSelector(_) => "global_selector",
            SlotRefusal::EscapingSelector(_) => "escaping_selector",
            SlotRefusal::MisplacedScope(_) => "misplaced_scope",
            SlotRefusal::EmptySelector => "empty_selector",
            SlotRefusal::HardcodedColour { .. } => "hardcoded_colour",
            SlotRefusal::HardcodedLength { .. } => "hardcoded_length",
            SlotRefusal::UnknownToken(_) => "unknown_token",
            SlotRefusal::NoEscapeHatch => "no_escape_hatch",
        }
    }

    /// The whole refusal, with the CORRECTION — the wording an agent reads at the write and a human
    /// reads in review. Every message names what was refused AND the form that works, because a
    /// refusal an agent cannot act on is a wall.
    pub fn message(&self) -> String {
        match self {
            SlotRefusal::Blank => "the css_slot is BLANK (empty once trimmed): there is nothing to \
                 store. Send the CSS, or leave the argument out to carry no slot at all."
                .to_string(),
            SlotRefusal::NoReason => format!(
                "a css_slot without a REASON is refused. The slot is an EXCEPTION to the design \
                 system, and it travels — into the review UI and into the coder's seam — WITH the \
                 reason it exists. Send css_slot_reason naming what the utilities could not do (\"a \
                 custom pulse animation; Tailwind has no utility for a keyframe\"), at most \
                 {MAX_CSS_SLOT_REASON_BYTES} bytes"
            ),
            SlotRefusal::ReasonTooLong(bytes) => format!(
                "css_slot_reason is {bytes} bytes, past the {MAX_CSS_SLOT_REASON_BYTES}-byte cap. \
                 One or two sentences naming what the utilities could not do — the design decision \
                 belongs in the component's params and in the round's presentation, not here"
            ),
            SlotRefusal::TooLarge(bytes) => format!(
                "the css_slot is {bytes} bytes, past the {MAX_CSS_SLOT_BYTES}-byte cap. A slot is a \
                 relief valve for what utilities cannot express — a @keyframes and the two or three \
                 rules that drive it. If it does not fit, what is in it is a STYLESHEET: put the \
                 look back in Tailwind classes and in the project's tokens, and keep here only what \
                 no utility can say"
            ),
            SlotRefusal::Markup => "the css_slot carries `</` — that is MARKUP, not CSS, and it is \
                 refused at the source rather than escaped on the way out (the compiled slot travels \
                 inside a <style> element). Send CSS only."
                .to_string(),
            SlotRefusal::Important => "the css_slot carries `!important`. The slot exists for what \
                 utilities CANNOT express, never to OUTRANK them: a declaration that has to beat a \
                 Tailwind class is a class that should not have been there, or a class that should \
                 have been. Remove it and fix the markup instead."
                .to_string(),
            SlotRefusal::ExternalResource => "the css_slot carries `url(…)` — an external resource. \
                 The preview frame's CSP grants no network at all (`default-src 'none'`, \
                 `connect-src 'none'`), so the fetch would simply fail, and RELAXING the CSP for a \
                 CSS slot is a design failure, not an option. Inline the asset as an SVG in the \
                 template, or use a `data:` URI in the markup — never a fetch from a stylesheet."
                .to_string(),
            SlotRefusal::AtRule(keyword) => format!(
                "the css_slot carries `@{keyword}`, an at-rule with NO selector to anchor: it is \
                 GLOBAL by nature, so nyx cannot bound it to this component's subtree and will not \
                 pretend to. Only `@keyframes`, `@media`, `@supports` and `@container` are admitted \
                 here. `@import` in particular is refused twice over — it is also a network fetch \
                 the frame's CSP forbids"
            ),
            SlotRefusal::NestedRule(prelude) => format!(
                "the css_slot nests a rule inside a declaration block (`{prelude}`). This scoper \
                 anchors SELECTORS, and a nested one would never see the anchor. Write the full \
                 selector instead — nesting saves you nothing here, since every selector is already \
                 rewritten under the component's scope"
            ),
            SlotRefusal::Unterminated(what) => format!(
                "the css_slot does not close `{what}` — the CSS is truncated or malformed. nyx \
                 refuses what it cannot read rather than guessing at it: a slot half-understood is a \
                 rule that silently does something else"
            ),
            SlotRefusal::BadDeclaration(text) => format!(
                "the css_slot carries `{text}`, which is not a `property: value` declaration"
            ),
            SlotRefusal::GlobalSelector(selector) => format!(
                "the css_slot selector `{selector}` targets the DOCUMENT, not this component. \
                 Anchored on the component's scope it could never match anything — and a rule that \
                 silently does nothing is exactly the disappearance this slot exists to end, so it \
                 is refused instead of neutered. Style the component's own elements; a page-level \
                 rule belongs to the app's stylesheet, not to a component"
            ),
            SlotRefusal::EscapingSelector(selector) => format!(
                "the css_slot selector `{selector}` starts with a combinator: anchored on the \
                 component's scope it would select a SIBLING (or a child of the parent), i.e. \
                 outside the component. Write a selector that starts inside the component (`.spark \
                 i`), or `:scope` for the component's own root element"
            ),
            SlotRefusal::MisplacedScope(selector) => format!(
                "the css_slot selector `{selector}` uses `:scope` / `&` somewhere other than as its \
                 FIRST token. Here they mean exactly one thing — the component's own root element — \
                 so they are only meaningful at the front: `:scope::after {{ … }}`, `:scope:has(.x) \
                 .y {{ … }}`"
            ),
            SlotRefusal::EmptySelector => "the css_slot carries an EMPTY selector — a stray comma \
                 in a selector list, or a rule whose prelude is blank. Name what the rule applies \
                 to, or drop the rule."
                .to_string(),
            SlotRefusal::HardcodedColour { property, value } => format!(
                "the css_slot writes a COLOUR in `{property}: {value}` instead of referencing the \
                 project's token layer. A hard-coded colour is a refusal, with no exception: it \
                 freezes one theme into a component the whole design system re-themes at once, and \
                 it is invisible to every later change of the palette. Write `var(--<token>)` \
                 (`design_tokens` lists what the project declares), or `currentColor` / \
                 `transparent` when the colour is meant to be inherited"
            ),
            SlotRefusal::HardcodedLength { property, value } => format!(
                "the css_slot writes a raw absolute length in `{property}: {value}`, on a property \
                 the design system SCALES. Reference the scale — `var(--<token>)` — or use a \
                 relative unit (`rem`, `em`, `%`) so the component follows the theme instead of \
                 pinning one pixel value. (A length in a `transform`, a duration or a plain number \
                 is not a themed value and is left alone.)"
            ),
            SlotRefusal::UnknownToken(name) => format!(
                "the css_slot references `var({name})`, a custom property this project's token layer \
                 does NOT declare. It would resolve to NOTHING — the same silent disappearance the \
                 slot exists to end, one level down. Call `design_tokens` to read the names the \
                 project really declares, and reference one of those (or declare `{name}` inside the \
                 slot itself if it is a slot-local variable)"
            ),
            SlotRefusal::NoEscapeHatch => format!(
                "this css_slot uses NONE of the four things Tailwind utilities cannot express — a \
                 custom `@keyframes`, the content of a `::before` / `::after`, a relational \
                 selector (`:has`), a `@container` query. So it is not an escape hatch: it is CSS \
                 written INSTEAD of the design system, and the coder would receive it in place of \
                 the project's conventions. Express this look in Tailwind utility classes on the \
                 template (and in the axes' `classes`), which is the framework of this project, and \
                 keep the slot for what really cannot be said that way. The four admitted \
                 constructs, as codes: {}",
                SLOT_FEATURES
                    .iter()
                    .map(|f| f.code())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::fmt::Display for SlotRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for SlotRefusal {}

// ---------------------------------------------------------------------------
// THE SCOPE IDENTITY
// ---------------------------------------------------------------------------

/// The scope id of a catalogue COMPONENT: a readable slug of its name plus a short digest of the
/// FULL name, so two components whose names slug identically (`Card/Header` and `Card-Header`) never
/// share a scope. Deterministic — the same name always yields the same id, which is what lets the
/// render, the seam and the UI name the same scope without passing it around.
pub fn component_scope_id(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_dash = true;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.extend(c.to_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    let slug: String = slug.chars().take(24).collect();
    let slug = slug.trim_end_matches('-').to_string();
    let digest = crate::design::md5_hex(name.as_bytes());
    if slug.is_empty() {
        format!("c-{}", &digest[..8])
    } else {
        format!("c-{slug}-{}", &digest[..6])
    }
}

/// The CSS selector a scope id resolves to — an attribute selector on [`SCOPE_ATTR`], matched with
/// `~=` so an element that sits in two scopes keeps both.
pub fn scope_selector(scope_id: &str) -> String {
    format!("[{SCOPE_ATTR}~=\"{scope_id}\"]")
}

// ---------------------------------------------------------------------------
// THE COMPILED SLOT
// ---------------------------------------------------------------------------

/// A slot's CSS, VALIDATED and SCOPED — what actually reaches a frame and a coder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedCss {
    /// The scope id the CSS was anchored on.
    pub scope_id: String,
    /// The selector every rule was rewritten under, literally — what the review UI displays as
    /// "scope", and what the coder reads to know where the exception applies.
    pub selector: String,
    /// The rewritten CSS. Every selector anchored, every `@keyframes` renamed under the scope.
    pub css: String,
    /// Which of the four admitted constructs the slot really exercises (never empty — a slot that
    /// exercises none is refused).
    pub features: Vec<SlotFeature>,
    /// The size of the AUTHORED CSS, in bytes.
    pub bytes: usize,
}

impl ScopedCss {
    /// The `<style>` element the render appends to the markup. Deterministic, and safe by
    /// construction: `</` is refused at the write and re-checked at compile, so nothing here can
    /// close the element it lives in.
    pub fn style_element(&self) -> String {
        format!(
            "<style {SLOT_STYLE_ATTR}=\"{}\">{}</style>",
            self.scope_id, self.css
        )
    }

    /// The feature codes, for a payload.
    pub fn feature_codes(&self) -> Vec<String> {
        self.features.iter().map(|f| f.code().to_string()).collect()
    }
}

/// VALIDATE and SCOPE a slot's CSS against `scope_id`.
///
/// # How the scope is GUARANTEED, and not merely asked for
///
/// Three mechanisms, in this order, and the third is the one that makes it a guarantee:
///
/// 1. **Refusal of what cannot be anchored** — an at-rule with no selector (`@import`, `@font-face`,
///    `@layer`, `@property`…) is refused, because there is no honest way to bound it.
/// 2. **Refusal of what would escape or would die** — a selector starting with a combinator (`+ .x`)
///    would, once anchored, select a SIBLING of the component; a selector on `html` / `body` /
///    `:root` / `:host` could, once anchored, match NOTHING. Both are refused BY NAME rather than
///    silently rewritten into something else, so the agent learns what it did.
/// 3. **Unconditional REWRITING of every selector that remains** — whatever the agent wrote,
///    `.foo` becomes `[data-nyx-css~="<id>"] .foo`, and `:scope` / `&` become the anchor itself.
///    The browser never sees the selector the agent typed. Descendant-anchoring is closed under
///    every combinator (a sibling of a descendant is a descendant), so no rule this function emits
///    can match an element outside the component's subtree — INCLUDING a selector this scanner did
///    not recognise, which is why the rewrite is unconditional rather than conditional on step 2.
///
/// `@keyframes` names are RENAMED under the scope id for the same reason: an animation name is a
/// GLOBAL identifier, so two components each defining `spin` would silently redefine each other's.
/// Every `animation` / `animation-name` value in the slot is rewritten to the renamed identifier.
///
/// Blocked at-rules with a body (`@media`, `@supports`, `@container`) are recursed into, so the
/// selectors inside them are anchored exactly like the ones outside.
pub fn compile(scope_id: &str, css: &str) -> Result<ScopedCss, SlotRefusal> {
    let bytes = css.len();
    if css.trim().is_empty() {
        return Err(SlotRefusal::Blank);
    }
    if bytes > MAX_CSS_SLOT_BYTES {
        return Err(SlotRefusal::TooLarge(bytes));
    }
    if css.contains("</") {
        return Err(SlotRefusal::Markup);
    }
    let source = strip_comments(css)?;
    let lower = source.to_ascii_lowercase();
    if lower.contains("!important") {
        return Err(SlotRefusal::Important);
    }
    if lower.contains("url(") {
        return Err(SlotRefusal::ExternalResource);
    }

    let mut scoper = Scoper {
        scope_id: scope_id.to_string(),
        selector: scope_selector(scope_id),
        keyframes: collect_keyframe_names(&source),
        features: BTreeSet::new(),
    };
    let mut out = String::with_capacity(source.len() + 64);
    scoper.rewrite(&source, 0, &mut out)?;
    if scoper.features.is_empty() {
        return Err(SlotRefusal::NoEscapeHatch);
    }
    let css = out.trim_end().to_string();
    // The emitted CSS travels inside a `<style>` element. `</` was refused on the source; this is the
    // same check on the OUTPUT, so no rewriting step can ever reintroduce it.
    if css.contains("</") {
        return Err(SlotRefusal::Markup);
    }
    Ok(ScopedCss {
        scope_id: scope_id.to_string(),
        selector: scoper.selector,
        css,
        features: scoper.features.into_iter().collect(),
        bytes,
    })
}

/// The REASON guard — a slot travels with the reason it exists, or it does not travel.
pub fn ensure_reason(reason: &str) -> Result<(), SlotRefusal> {
    if reason.trim().is_empty() {
        return Err(SlotRefusal::NoReason);
    }
    if reason.len() > MAX_CSS_SLOT_REASON_BYTES {
        return Err(SlotRefusal::ReasonTooLong(reason.len()));
    }
    Ok(())
}

/// Every `var(--x)` the slot references must name a token the PROJECT declares — the "tokenisé"
/// constraint at its strongest: not "it looks like a token", but "it IS one of this project's".
///
/// `known` is the project's token layer (`--name` keys, light + dark). A custom property the slot
/// DECLARES itself counts as known (a slot-local variable is legitimate). An EMPTY layer disables the
/// check entirely: a greenfield project has no tokens yet, and refusing every `var()` there would
/// make the slot unusable exactly where the design system is being invented.
pub fn ensure_tokens_known(css: &str, known: &BTreeSet<String>) -> Result<(), SlotRefusal> {
    if known.is_empty() {
        return Ok(());
    }
    let local = declared_custom_properties(css);
    for name in referenced_custom_properties(css) {
        if !known.contains(&name) && !local.contains(&name) {
            return Err(SlotRefusal::UnknownToken(name));
        }
    }
    Ok(())
}

/// The `--name`s a stylesheet DECLARES (`--x: value`).
fn declared_custom_properties(css: &str) -> BTreeSet<String> {
    let bytes = css.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'-'
            && bytes[i + 1] == b'-'
            && !is_ident_byte(i.checked_sub(1).map(|p| bytes[p]))
        {
            let mut j = i + 2;
            while j < bytes.len() && is_ident_byte(Some(bytes[j])) {
                j += 1;
            }
            // A DECLARATION is `--name` followed (after spaces) by a colon.
            let mut k = j;
            while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            if k < bytes.len() && bytes[k] == b':' {
                out.insert(css[i..j].to_string());
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// The `--name`s a stylesheet REFERENCES (`var(--x)` / `var(--x, fallback)`), in source order.
fn referenced_custom_properties(css: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = css.to_ascii_lowercase();
    let bytes = css.as_bytes();
    let mut from = 0usize;
    while let Some(at) = lower[from..].find("var(") {
        let mut i = from + at + 4;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && is_ident_byte(Some(bytes[i])) {
            i += 1;
        }
        if i > start && css[start..].starts_with("--") {
            out.push(css[start..i].to_string());
        }
        from = start.max(from + at + 4);
    }
    out
}

fn is_ident_byte(b: Option<u8>) -> bool {
    matches!(b, Some(c) if c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

// ---------------------------------------------------------------------------
// THE SCANNER
// ---------------------------------------------------------------------------

/// Strip `/* … */` comments, respecting strings. An unterminated comment or string is a REFUSAL —
/// a slot this scanner cannot read whole is a slot whose scoping cannot be guaranteed.
fn strip_comments(css: &str) -> Result<String, SlotRefusal> {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                let end = css[i + 2..]
                    .find("*/")
                    .ok_or_else(|| SlotRefusal::Unterminated("a /* comment".to_string()))?;
                out.push(' ');
                i = i + 2 + end + 2;
            }
            quote @ (b'"' | b'\'') => {
                let start = i;
                i += 1;
                loop {
                    if i >= bytes.len() {
                        return Err(SlotRefusal::Unterminated(format!(
                            "a {} string",
                            quote as char
                        )));
                    }
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                out.push_str(&css[start..i.min(css.len())]);
            }
            _ => {
                let ch = css[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    Ok(out)
}

/// The `@keyframes` identifiers a slot declares — read in a FIRST pass, because a rule may use an
/// animation the sheet declares further down.
fn collect_keyframe_names(css: &str) -> BTreeSet<String> {
    let lower = css.to_ascii_lowercase();
    let mut out = BTreeSet::new();
    let mut from = 0usize;
    while let Some(at) = lower[from..].find("keyframes") {
        let mut i = from + at + "keyframes".len();
        while i < css.len() && css.as_bytes()[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < css.len() && is_ident_byte(Some(css.as_bytes()[i])) {
            i += 1;
        }
        if i > start {
            out.insert(css[start..i].to_string());
        }
        from = (from + at + "keyframes".len()).min(css.len());
    }
    out
}

struct Scoper {
    scope_id: String,
    selector: String,
    keyframes: BTreeSet<String>,
    features: BTreeSet<SlotFeature>,
}

impl Scoper {
    /// The renamed identifier of a `@keyframes` this slot declares — GLOBAL names made local.
    fn keyframe_name(&self, name: &str) -> String {
        format!("{}-{name}", self.scope_id)
    }

    /// Rewrite one LEVEL of the sheet (the whole sheet at `depth` 0, a conditional at-rule's body
    /// below), appending to `out`.
    fn rewrite(&mut self, src: &str, depth: usize, out: &mut String) -> Result<(), SlotRefusal> {
        let indent = "  ".repeat(depth);
        let mut rest = src;
        loop {
            rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ';');
            if rest.is_empty() {
                return Ok(());
            }
            if rest.starts_with('@') {
                rest = self.rewrite_at_rule(rest, depth, &indent, out)?;
                continue;
            }
            let (prelude, body, tail) = split_rule(rest)?;
            self.emit_rule(prelude, body, &indent, out)?;
            rest = tail;
        }
    }

    /// One at-rule. Returns what is LEFT of the input.
    fn rewrite_at_rule<'a>(
        &mut self,
        rest: &'a str,
        depth: usize,
        indent: &str,
        out: &mut String,
    ) -> Result<&'a str, SlotRefusal> {
        let keyword: String = rest[1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let lower = keyword.trim_start_matches('-').to_ascii_lowercase();
        let lower = lower
            .strip_prefix("webkit-")
            .or_else(|| lower.strip_prefix("moz-"))
            .map(str::to_string)
            .unwrap_or(lower);
        match lower.as_str() {
            "media" | "supports" | "container" => {
                if lower == "container" {
                    self.features.insert(SlotFeature::Container);
                }
                let (prelude, body, tail) = split_rule(rest)?;
                out.push_str(indent);
                out.push_str(normalize_space(prelude).trim());
                out.push_str(" {\n");
                self.rewrite(body, depth + 1, out)?;
                out.push_str(indent);
                out.push_str("}\n");
                Ok(tail)
            }
            "keyframes" => {
                self.features.insert(SlotFeature::Keyframes);
                let (prelude, body, tail) = split_rule(rest)?;
                let name = prelude
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                if name.is_empty() {
                    return Err(SlotRefusal::BadDeclaration(prelude.trim().to_string()));
                }
                out.push_str(indent);
                out.push_str("@keyframes ");
                out.push_str(&self.keyframe_name(&name));
                out.push_str(" {\n");
                self.rewrite_keyframe_body(body, depth + 1, out)?;
                out.push_str(indent);
                out.push_str("}\n");
                Ok(tail)
            }
            _ => Err(SlotRefusal::AtRule(keyword)),
        }
    }

    /// A `@keyframes` body: `<from|to|percentage list> { declarations }`, repeated. The selectors are
    /// KEYFRAME positions, not element selectors, so they are copied — there is nothing to anchor;
    /// the anchoring of the animation is its RENAMED identifier.
    fn rewrite_keyframe_body(
        &mut self,
        src: &str,
        depth: usize,
        out: &mut String,
    ) -> Result<(), SlotRefusal> {
        let indent = "  ".repeat(depth);
        let mut rest = src;
        loop {
            rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ';');
            if rest.is_empty() {
                return Ok(());
            }
            let (prelude, body, tail) = split_rule(rest)?;
            let stop = normalize_space(prelude);
            for part in stop.split(',') {
                let part = part.trim();
                let ok = part.eq_ignore_ascii_case("from")
                    || part.eq_ignore_ascii_case("to")
                    || (part.ends_with('%')
                        && part[..part.len() - 1].trim().parse::<f64>().is_ok());
                if !ok {
                    return Err(SlotRefusal::BadDeclaration(part.to_string()));
                }
            }
            out.push_str(&indent);
            out.push_str(&stop);
            out.push_str(" { ");
            out.push_str(&self.declarations(body)?);
            out.push_str(" }\n");
            rest = tail;
        }
    }

    /// A qualified rule: its selector list ANCHORED, its declarations checked.
    fn emit_rule(
        &mut self,
        prelude: &str,
        body: &str,
        indent: &str,
        out: &mut String,
    ) -> Result<(), SlotRefusal> {
        if body.contains('{') {
            return Err(SlotRefusal::NestedRule(normalize_space(prelude)));
        }
        let mut selectors = Vec::new();
        for raw in split_top_level(prelude, ',') {
            selectors.push(self.rewrite_selector(&raw)?);
        }
        if selectors.is_empty() {
            return Err(SlotRefusal::EmptySelector);
        }
        let declarations = self.declarations(body)?;
        out.push_str(indent);
        out.push_str(&selectors.join(", "));
        out.push_str(" { ");
        out.push_str(&declarations);
        out.push_str(" }\n");
        Ok(())
    }

    /// ANCHOR one selector on the scope — the guarantee, see [`compile`].
    fn rewrite_selector(&mut self, raw: &str) -> Result<String, SlotRefusal> {
        let selector = normalize_space(raw);
        if selector.is_empty() {
            return Err(SlotRefusal::EmptySelector);
        }
        let lower = selector.to_ascii_lowercase();
        if lower.contains("::before")
            || lower.contains("::after")
            || lower.contains(":before")
            || lower.contains(":after")
        {
            self.features.insert(SlotFeature::PseudoElement);
        }
        if lower.contains(":has(") {
            self.features.insert(SlotFeature::Has);
        }
        if selector.starts_with(['>', '+', '~']) {
            return Err(SlotRefusal::EscapingSelector(selector));
        }
        // `:scope` / `&` — the component's OWN root element, and only as the FIRST token.
        let anchored = if let Some(tail) = lower
            .strip_prefix(":scope")
            .map(|_| &selector[":scope".len()..])
            .or_else(|| selector.strip_prefix('&'))
        {
            if tail.to_ascii_lowercase().contains(":scope") || tail.contains('&') {
                return Err(SlotRefusal::MisplacedScope(selector));
            }
            format!("{}{tail}", self.selector)
        } else {
            if lower.contains(":scope") || selector.contains('&') {
                return Err(SlotRefusal::MisplacedScope(selector));
            }
            // A selector that could never match once anchored is REFUSED, not neutered.
            let head: String = selector
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>()
                .to_ascii_lowercase();
            if matches!(head.as_str(), "html" | "body")
                || lower.starts_with(":root")
                || lower.starts_with(":host")
            {
                return Err(SlotRefusal::GlobalSelector(selector));
            }
            format!("{} {selector}", self.selector)
        };
        Ok(anchored)
    }

    /// The declarations of one block: each checked against the TOKENISED constraint, and each
    /// `animation` / `animation-name` value rewritten onto the slot's renamed keyframes.
    fn declarations(&self, body: &str) -> Result<String, SlotRefusal> {
        let mut out: Vec<String> = Vec::new();
        for raw in split_top_level(body, ';') {
            let declaration = normalize_space(&raw);
            if declaration.is_empty() {
                continue;
            }
            // An at-rule INSIDE a declaration block — `@apply p-2` above all. It is named for what
            // it is: the Tailwind directive belongs on the template's `class`, not in the slot (and
            // the `<style>` a slot travels in is not processed by the frame's Tailwind runtime, so
            // it would silently do nothing — the disappearance this slot exists to end).
            if let Some(keyword) = declaration.strip_prefix('@') {
                return Err(SlotRefusal::AtRule(
                    keyword
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .collect(),
                ));
            }
            let Some((property, value)) = declaration.split_once(':') else {
                return Err(SlotRefusal::BadDeclaration(declaration));
            };
            let property = property.trim().to_string();
            let value = value.trim().to_string();
            if property.is_empty() || value.is_empty() {
                return Err(SlotRefusal::BadDeclaration(declaration));
            }
            let lower_property = property.to_ascii_lowercase();
            // A custom property DECLARED by the slot is a slot-local variable: its value is checked
            // like any other (a hard-coded colour hidden behind `--x` is still hard-coded).
            check_value(&lower_property, &value)?;
            let value = if lower_property == "animation" || lower_property == "animation-name" {
                self.rename_animations(&value)
            } else {
                value
            };
            out.push(format!("{property}: {value}"));
        }
        Ok(out.join("; "))
    }

    /// Rewrite the slot's own `@keyframes` identifiers inside an `animation` value — whole word only,
    /// so a duration or an easing keyword is never touched.
    fn rename_animations(&self, value: &str) -> String {
        let mut out = value.to_string();
        for name in &self.keyframes {
            out = replace_word(&out, name, &self.keyframe_name(name));
        }
        out
    }
}

/// Replace whole-word occurrences of `word` (CSS identifier boundaries) in `text`.
fn replace_word(text: &str, word: &str, with: &str) -> String {
    if word.is_empty() {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < text.len() {
        if text[i..].starts_with(word) {
            let before_ok = i == 0 || !is_ident_byte(Some(bytes[i - 1]));
            let after = i + word.len();
            let after_ok = after >= bytes.len() || !is_ident_byte(Some(bytes[after]));
            if before_ok && after_ok {
                out.push_str(with);
                i = after;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Split a rule off the front of `src`: `(prelude, body, rest)`. The prelude runs up to the `{` that
/// opens the block at nesting level 0 (parens / brackets / strings respected); the body is what the
/// matching `}` closes.
fn split_rule(src: &str) -> Result<(&str, &str, &str), SlotRefusal> {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut nesting = 0usize;
    let mut open: Option<usize> = None;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'"' | b'\'') => {
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' | b'[' => nesting += 1,
            b')' | b']' => nesting = nesting.saturating_sub(1),
            b'{' if nesting == 0 => {
                open = Some(i);
                break;
            }
            b';' if nesting == 0 => {
                // A statement at-rule (`@import …;`) never reaches here — those are refused by
                // keyword — so a `;` before any `{` is a declaration adrift outside a rule.
                return Err(SlotRefusal::BadDeclaration(src[..i].trim().to_string()));
            }
            _ => {}
        }
        i += 1;
    }
    let Some(open) = open else {
        return Err(SlotRefusal::Unterminated(format!(
            "the block of `{}`",
            normalize_space(src).chars().take(60).collect::<String>()
        )));
    };
    let mut depth = 1usize;
    let mut j = open + 1;
    while j < bytes.len() {
        match bytes[j] {
            quote @ (b'"' | b'\'') => {
                j += 1;
                while j < bytes.len() && bytes[j] != quote {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&src[..open], &src[open + 1..j], &src[j + 1..]));
                }
            }
            _ => {}
        }
        j += 1;
    }
    Err(SlotRefusal::Unterminated(format!(
        "the block of `{}`",
        normalize_space(&src[..open])
            .chars()
            .take(60)
            .collect::<String>()
    )))
}

/// Split on `sep` at nesting level 0 (parens / brackets / strings respected) — a selector list's
/// commas, a block's semicolons.
fn split_top_level(src: &str, sep: char) -> Vec<String> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut nesting = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'"' | b'\'') => {
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' | b'[' => nesting += 1,
            b')' | b']' => nesting = nesting.saturating_sub(1),
            c if c == sep as u8 && nesting == 0 => {
                out.push(src[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(src[start..].to_string());
    out.retain(|s| !s.trim().is_empty());
    out
}

/// Collapse runs of whitespace to one space and trim — the emitted CSS is normalized, so the same
/// slot always compiles to the same bytes.
fn normalize_space(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut space = false;
    for c in src.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// THE TOKENISED CONSTRAINT
// ---------------------------------------------------------------------------

/// The properties whose value IS a colour — a value here with no `var(--…)` and no inherit-style
/// keyword is a hard-coded colour even when it is spelled `red`.
const COLOUR_PROPERTIES: &[&str] = &[
    "color",
    "background",
    "background-color",
    "background-image",
    "border",
    "border-color",
    "border-top",
    "border-right",
    "border-bottom",
    "border-left",
    "border-top-color",
    "border-right-color",
    "border-bottom-color",
    "border-left-color",
    "border-block-color",
    "border-inline-color",
    "outline",
    "outline-color",
    "fill",
    "stroke",
    "caret-color",
    "accent-color",
    "column-rule-color",
    "text-decoration-color",
    "text-emphasis-color",
    "box-shadow",
    "text-shadow",
];

/// The value keywords a colour-valued property may carry WITHOUT naming a token: they express
/// "inherit the colour" or "no colour at all", which is the design system's own answer.
const COLOUR_KEYWORDS: &[&str] = &[
    "currentcolor",
    "transparent",
    "inherit",
    "initial",
    "unset",
    "revert",
    "none",
];

/// The properties the design system SCALES — where a raw absolute length is a value written beside
/// the token layer instead of out of it. Geometry that is NOT themed (`transform`, `filter`, a
/// duration, a plain number) is deliberately absent: a `translateY(-2px)` in a keyframe is a motion,
/// not a spacing token.
const SCALE_PROPERTIES: &[&str] = &[
    "width",
    "height",
    "min-width",
    "min-height",
    "max-width",
    "max-height",
    "padding",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "padding-block",
    "padding-inline",
    "margin",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "margin-block",
    "margin-inline",
    "gap",
    "row-gap",
    "column-gap",
    "top",
    "right",
    "bottom",
    "left",
    "inset",
    "border-radius",
    "border-width",
    "font-size",
    "letter-spacing",
    "word-spacing",
    "outline-width",
    "outline-offset",
    "text-indent",
];

/// The tokenised check on ONE declaration.
fn check_value(property: &str, value: &str) -> Result<(), SlotRefusal> {
    if let Some(literal) = colour_literal(value) {
        return Err(SlotRefusal::HardcodedColour {
            property: property.to_string(),
            value: literal,
        });
    }
    if COLOUR_PROPERTIES.contains(&property) {
        let lower = value.to_ascii_lowercase();
        let references_token = lower.contains("var(--");
        let keyword_only = lower
            .split_whitespace()
            .all(|word| COLOUR_KEYWORDS.contains(&word.trim_end_matches(',')));
        if !references_token && !keyword_only {
            return Err(SlotRefusal::HardcodedColour {
                property: property.to_string(),
                value: value.to_string(),
            });
        }
    }
    if SCALE_PROPERTIES.contains(&property) {
        if let Some(length) = absolute_length(value) {
            return Err(SlotRefusal::HardcodedLength {
                property: property.to_string(),
                value: length,
            });
        }
    }
    Ok(())
}

/// The COLOUR literal a value carries, if any — a hex triplet/quadruplet or a colour function, in
/// any of the forms CSS accepts.
fn colour_literal(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'#' {
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
            j += 1;
        }
        if matches!(j - i - 1, 3 | 4 | 6 | 8) {
            return Some(value[i..j].to_string());
        }
    }
    let lower = value.to_ascii_lowercase();
    for function in [
        "rgb(",
        "rgba(",
        "hsl(",
        "hsla(",
        "hwb(",
        "lab(",
        "lch(",
        "oklab(",
        "oklch(",
        "color(",
        "color-mix(",
    ] {
        if let Some(at) = lower.find(function) {
            // `color(` must not match the tail of another identifier (`--brand-color(`… never valid,
            // but be exact anyway).
            if at > 0 && is_ident_byte(Some(lower.as_bytes()[at - 1])) {
                continue;
            }
            return Some(function.trim_end_matches('(').to_string());
        }
    }
    None
}

/// The absolute LENGTH a value carries, if any — a non-zero number followed by an absolute unit.
fn absolute_length(value: &str) -> Option<String> {
    const UNITS: [&str; 7] = ["px", "pt", "pc", "in", "cm", "mm", "q"];
    let bytes = value.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let starts_number = (bytes[i].is_ascii_digit() || bytes[i] == b'.')
            && (i == 0 || !is_ident_byte(Some(bytes[i - 1])));
        if !starts_number {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        let number = &value[start..i];
        let unit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let unit = value[unit_start..i].to_ascii_lowercase();
        if UNITS.contains(&unit.as_str()) && number.parse::<f64>().unwrap_or(0.0) != 0.0 {
            return Some(value[start..i].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests;
