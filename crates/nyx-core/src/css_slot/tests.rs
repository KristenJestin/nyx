//! Tests of the SCOPED CSS SLOT ([`super`], finding `01KYVMTWB71R0D0CT1SV88J8DX`).
//!
//! The four constraints are proven here, one section each: TOKENISED (a hard-coded value is
//! refused), SCOPED (the machinery REWRITES, so a global selector cannot escape whatever the agent
//! wrote), BOUNDED (a cap), SIGNALLED (a slot without its reason is refused, and a compiled slot
//! reports what it exercises). Everything is pure — no DB, no disk.

use super::*;

/// The scope id of the component every test below uses.
fn scope() -> String {
    component_scope_id("Spinner")
}

fn compiled(css: &str) -> ScopedCss {
    compile(&scope(), css).expect("the slot compiles")
}

fn refused(css: &str) -> SlotRefusal {
    compile(&scope(), css).expect_err("the slot is refused")
}

// ---------------------------------------------------------------------------
// SCOPED — the guarantee, not the request
// ---------------------------------------------------------------------------

#[test]
fn every_selector_is_anchored_on_the_component_scope() {
    let out = compiled(
        "@keyframes pulse { 0%, 100% { opacity: 1 } 50% { opacity: .55 } }\n\
         .spark i { animation: pulse 1.4s ease-in-out infinite }",
    );
    let anchor = scope_selector(&scope());
    assert!(
        out.css.contains(&format!("{anchor} .spark i")),
        "the rule is anchored on the scope: {}",
        out.css
    );
    assert_eq!(out.selector, anchor);
}

#[test]
fn a_global_selector_an_agent_writes_is_refused_and_never_neutered() {
    // The whole point of the finding: something that cannot work must SAY SO, never disappear.
    for css in [
        "body { background: var(--background) } .x::after { content: \"\" }",
        "html .x { display: block } .x::after { content: \"\" }",
        ":root { --a: 1 } .x::after { content: \"\" }",
    ] {
        let refusal = compile(&scope(), css).expect_err("a document-level selector is refused");
        assert_eq!(refusal.code(), "global_selector", "for {css}");
    }
}

#[test]
fn a_selector_that_would_escape_sideways_is_refused() {
    // `[scope] + .sibling` selects a SIBLING of the component — the one shape descendant-anchoring
    // does NOT contain, so it is refused by name.
    for selector in ["+ .sibling", "~ .later", "> .child"] {
        let refusal = refused(&format!(
            "{selector} {{ color: var(--x) }} .y::after {{ content: \"\" }}"
        ));
        assert_eq!(refusal.code(), "escaping_selector", "for {selector}");
    }
}

#[test]
fn an_unrecognised_selector_is_still_anchored_so_it_cannot_escape() {
    // The rewrite is UNCONDITIONAL: even a selector this scanner has no opinion about leaves here
    // prefixed by the scope. That is what makes the containment a property of the code rather than
    // of the list of refusals.
    let out = compiled(".card:has(> .badge) [data-role='row'] { color: var(--primary) }");
    let anchor = scope_selector(&scope());
    for line in out.css.lines().filter(|l| l.contains('{')) {
        assert!(
            line.trim_start().starts_with(&anchor) || line.trim_start().starts_with('@'),
            "every emitted rule starts at the scope: {line}"
        );
    }
}

#[test]
fn scope_and_ampersand_address_the_component_root_itself() {
    let anchor = scope_selector(&scope());
    let out = compiled(":scope::after { content: \"\"; color: var(--x) }");
    assert!(out.css.contains(&format!("{anchor}::after")), "{}", out.css);
    let out = compiled("&:has(.child) { color: var(--x) }");
    assert!(
        out.css.contains(&format!("{anchor}:has(.child)")),
        "{}",
        out.css
    );
    // …and only as the FIRST token.
    assert_eq!(
        refused(".a &::after { content: \"\" }").code(),
        "misplaced_scope"
    );
}

#[test]
fn keyframes_are_renamed_under_the_scope_and_the_animation_follows() {
    let out = compiled(
        "@keyframes spin { from { opacity: 0 } to { opacity: 1 } }\n\
         .ring { animation-name: spin }",
    );
    let renamed = format!("{}-spin", scope());
    assert!(
        out.css.contains(&format!("@keyframes {renamed}")),
        "the identifier is localised: {}",
        out.css
    );
    assert!(
        out.css.contains(&format!("animation-name: {renamed}")),
        "and every use follows it: {}",
        out.css
    );
    assert!(
        !out.css.contains("@keyframes spin "),
        "the global name is gone: {}",
        out.css
    );
}

#[test]
fn two_components_never_share_a_keyframes_identifier() {
    let a = compile(
        &component_scope_id("Spinner"),
        "@keyframes spin { to { opacity: 1 } } .r { animation: spin 1s linear infinite }",
    )
    .expect("ok");
    let b = compile(
        &component_scope_id("Loader"),
        "@keyframes spin { to { opacity: .5 } } .r { animation: spin 2s linear infinite }",
    )
    .expect("ok");
    assert_ne!(a.scope_id, b.scope_id);
    assert!(!a.css.contains(&b.scope_id) && !b.css.contains(&a.scope_id));
}

#[test]
fn a_conditional_at_rule_anchors_the_selectors_inside_it() {
    let out = compiled("@container (min-width: 400px) { .grid { gap: var(--gap) } }");
    let anchor = scope_selector(&scope());
    assert!(out.css.starts_with("@container"), "{}", out.css);
    assert!(out.css.contains(&format!("{anchor} .grid")), "{}", out.css);
    assert!(out.features.contains(&SlotFeature::Container));
}

#[test]
fn an_at_rule_with_nothing_to_anchor_is_refused() {
    for (css, keyword) in [
        ("@import url(\"x.css\");", "external_resource"),
        ("@font-face { font-family: X }", "at_rule"),
        ("@layer base { .x::after { content: \"\" } }", "at_rule"),
        ("@property --x { syntax: \"*\" }", "at_rule"),
        ("@page { margin: 0 }", "at_rule"),
        (".x { @apply p-2 } .y::after { content: \"\" }", "at_rule"),
    ] {
        assert_eq!(refused(css).code(), keyword, "for {css}");
    }
}

#[test]
fn markup_in_a_slot_can_never_close_the_style_element_it_travels_in() {
    assert_eq!(
        refused(".x::after { content: \"</style>\" }").code(),
        "markup"
    );
    // And the emitted element is well formed for a slot that IS admitted.
    let out = compiled(".x::after { content: \"·\"; color: var(--muted) }");
    let element = out.style_element();
    assert!(element.starts_with("<style data-nyx-css-slot=\""));
    assert!(element.ends_with("</style>"));
    assert_eq!(element.matches("</").count(), 1, "exactly the closing tag");
}

// ---------------------------------------------------------------------------
// TOKENISED
// ---------------------------------------------------------------------------

#[test]
fn a_hardcoded_colour_is_a_refusal_in_every_form() {
    for css in [
        ".x::after { content: \"\"; color: #ff0000 }",
        ".x::after { content: \"\"; background: rgb(255 0 0) }",
        ".x::after { content: \"\"; border-color: oklch(0.7 0.1 20) }",
        ".x::after { content: \"\"; color: red }",
        ".x::after { content: \"\"; box-shadow: 0 0 0 2px #0008 }",
    ] {
        assert_eq!(refused(css).code(), "hardcoded_colour", "for {css}");
    }
}

#[test]
fn a_colour_referenced_from_the_token_layer_is_accepted() {
    let out =
        compiled(".x::after { content: \"\"; color: var(--primary); background: transparent }");
    assert!(out.css.contains("var(--primary)"));
}

#[test]
fn a_raw_absolute_length_on_a_scaled_property_is_refused_but_a_motion_is_not() {
    assert_eq!(
        refused(".x::after { content: \"\"; padding: 12px }").code(),
        "hardcoded_length"
    );
    // A translate inside a keyframe is a MOTION, not a spacing token — left alone on purpose.
    let out =
        compiled("@keyframes bob { 50% { transform: translateY(-2px) } } .x { animation: bob 1s }");
    assert!(out.css.contains("translateY(-2px)"), "{}", out.css);
    // …and zero needs no token.
    assert!(compile(&scope(), ".x::after { content: \"\"; margin: 0 }").is_ok());
}

#[test]
fn a_var_naming_a_token_the_project_does_not_declare_is_refused() {
    let known: BTreeSet<String> = ["--primary".to_string(), "--muted".to_string()]
        .into_iter()
        .collect();
    assert!(ensure_tokens_known(".x { color: var(--primary) }", &known).is_ok());
    let refusal = ensure_tokens_known(".x { color: var(--brand-500) }", &known)
        .expect_err("a dangling var is refused");
    assert_eq!(refusal.code(), "unknown_token");
    assert!(refusal.message().contains("--brand-500"));
    // A slot-local variable is legitimate.
    assert!(ensure_tokens_known(".x { --local: 2; opacity: var(--local) }", &known).is_ok());
    // A greenfield project (no token layer at all) does not have every var refused.
    assert!(ensure_tokens_known(".x { color: var(--anything) }", &BTreeSet::new()).is_ok());
}

#[test]
fn important_is_refused_because_the_slot_never_outranks_the_utilities() {
    assert_eq!(
        refused(".x::after { content: \"\"; opacity: 1 !important }").code(),
        "important"
    );
}

#[test]
fn an_external_resource_is_refused_rather_than_relaxing_the_frame_csp() {
    let refusal = refused(".x::after { content: \"\"; background-image: url(\"/logo.png\") }");
    assert_eq!(refusal.code(), "external_resource");
    assert!(
        refusal.message().contains("CSP"),
        "the refusal says WHY: {}",
        refusal.message()
    );
}

// ---------------------------------------------------------------------------
// BOUNDED + SIGNALLED
// ---------------------------------------------------------------------------

#[test]
fn the_slot_is_capped_and_so_is_its_reason() {
    let big = format!(
        ".x::after {{ content: \"\"; color: var(--a) }} /* {} */",
        "x".repeat(MAX_CSS_SLOT_BYTES)
    );
    assert_eq!(refused(&big).code(), "too_large");
    assert_eq!(ensure_reason("").unwrap_err().code(), "no_reason");
    assert_eq!(
        ensure_reason(&"r".repeat(MAX_CSS_SLOT_REASON_BYTES + 1))
            .unwrap_err()
            .code(),
        "reason_too_long"
    );
    assert!(ensure_reason("a custom pulse keyframe; no utility expresses it").is_ok());
}

#[test]
fn a_slot_that_utilities_could_express_is_refused_it_is_not_an_escape_hatch() {
    let refusal = refused(".card { display: flex; gap: var(--gap); color: var(--fg) }");
    assert_eq!(refusal.code(), "no_escape_hatch");
    assert!(
        refusal.message().contains("Tailwind"),
        "the refusal names the channel that IS the framework: {}",
        refusal.message()
    );
}

#[test]
fn a_compiled_slot_reports_which_infeasible_constructs_it_really_uses() {
    let out = compiled(
        "@keyframes pulse { 50% { opacity: .5 } }\n\
         .a::before { content: \"\" }\n\
         .b:has(.c) { opacity: 1 }\n\
         @container (min-width: 20rem) { .d { opacity: 1 } }\n\
         .e { animation: pulse 1s linear infinite }",
    );
    assert_eq!(
        out.feature_codes(),
        vec!["keyframes", "pseudo_element", "has", "container"]
    );
    assert!(out.bytes > 0);
}

#[test]
fn a_stored_slot_round_trips_and_a_junk_row_reads_as_no_slot() {
    let slot = CssSlot {
        css: ".x::after { content: \"·\" }".to_string(),
        reason: "generated content has no utility".to_string(),
    };
    assert_eq!(CssSlot::parse(Some(&slot.to_json())), Some(slot));
    assert_eq!(CssSlot::parse(None), None);
    assert_eq!(CssSlot::parse(Some("   ")), None);
    assert_eq!(CssSlot::parse(Some("{ not json")), None);
    assert_eq!(CssSlot::parse(Some("{\"css\":\"  \"}")), None);
}

#[test]
fn the_scope_id_is_deterministic_readable_and_collision_free() {
    assert_eq!(component_scope_id("Spinner"), component_scope_id("Spinner"));
    assert!(component_scope_id("Card/Header").starts_with("c-card-header-"));
    assert_ne!(
        component_scope_id("Card/Header"),
        component_scope_id("Card-Header"),
        "two names that slug alike still get different scopes"
    );
    assert_eq!(
        scope_selector("c-x-000000"),
        "[data-nyx-css~=\"c-x-000000\"]"
    );
}

#[test]
fn the_same_slot_always_compiles_to_the_same_bytes() {
    let css =
        "@keyframes pulse {\n  50% { opacity: .5 }\n}\n.a { animation: pulse 1s linear infinite }";
    let spaced = "@keyframes   pulse{50%{opacity:.5}}   .a{animation:pulse 1s linear infinite}";
    assert_eq!(compiled(css).css, compiled(spaced).css);
}

/// THE CROSS-LANGUAGE VOCABULARY. Rust OWNS the validation and the scoping — the renderer never
/// compiles a slot, it displays what the sidecar already scoped — so what crosses the boundary is a
/// VOCABULARY: the two attributes, the four feature codes a review card ticks, the refusal codes an
/// error may carry, and the caps. Pinned in the shared fixture so a display surface that keys off one
/// of them fails the day it changes, instead of quietly ceasing to react.
#[test]
fn the_shared_fixture_pins_the_slot_vocabulary() {
    let section = crate::mcp_tools_design::design_fixture_section("css_slot");
    assert_eq!(section["scope_attribute"], SCOPE_ATTR);
    assert_eq!(section["style_attribute"], SLOT_STYLE_ATTR);
    assert_eq!(section["max_css_bytes"], MAX_CSS_SLOT_BYTES);
    assert_eq!(section["max_reason_bytes"], MAX_CSS_SLOT_REASON_BYTES);

    let features: Vec<&str> = section["features"]
        .as_array()
        .expect("the features")
        .iter()
        .map(|f| f["code"].as_str().expect("a code"))
        .collect();
    assert_eq!(
        features,
        SLOT_FEATURES.iter().map(|f| f.code()).collect::<Vec<_>>(),
        "the fixture names the four admitted constructs, in order — and only those"
    );

    // Every refusal the module can answer with is pinned, and every pinned code is one this module
    // really produces. Both directions, so neither list can grow alone.
    let pinned: Vec<&str> = section["refusals"]
        .as_array()
        .expect("the refusals")
        .iter()
        .map(|r| r["code"].as_str().expect("a code"))
        .collect();
    let produced = [
        SlotRefusal::Blank,
        SlotRefusal::NoReason,
        SlotRefusal::ReasonTooLong(0),
        SlotRefusal::TooLarge(0),
        SlotRefusal::Markup,
        SlotRefusal::Important,
        SlotRefusal::ExternalResource,
        SlotRefusal::AtRule(String::new()),
        SlotRefusal::NestedRule(String::new()),
        SlotRefusal::Unterminated(String::new()),
        SlotRefusal::BadDeclaration(String::new()),
        SlotRefusal::GlobalSelector(String::new()),
        SlotRefusal::EscapingSelector(String::new()),
        SlotRefusal::MisplacedScope(String::new()),
        SlotRefusal::EmptySelector,
        SlotRefusal::HardcodedColour {
            property: String::new(),
            value: String::new(),
        },
        SlotRefusal::HardcodedLength {
            property: String::new(),
            value: String::new(),
        },
        SlotRefusal::UnknownToken(String::new()),
        SlotRefusal::NoEscapeHatch,
    ];
    assert_eq!(
        pinned,
        produced.iter().map(SlotRefusal::code).collect::<Vec<_>>(),
        "the fixture names every refusal the slot can answer with — and only those"
    );
    // Every refusal carries a CORRECTION, not just a verdict: a wall an agent cannot act on is how a
    // constraint turns into an agent inventing its own way round it.
    for refusal in &produced {
        assert!(
            refusal.message().len() > 60,
            "[{}] the refusal explains what to do instead",
            refusal.code()
        );
    }
}

#[test]
fn a_slot_this_scanner_cannot_read_whole_is_refused_never_guessed() {
    assert_eq!(refused(".a::after { content: \"x\"").code(), "unterminated");
    assert_eq!(refused(".a::after { content: \"x }").code(), "unterminated");
    assert_eq!(
        refused("/* open .a::after { content: \"\" }").code(),
        "unterminated"
    );
    assert_eq!(
        refused(".a { .b::after { content: \"\" } }").code(),
        "nested_rule"
    );
}
