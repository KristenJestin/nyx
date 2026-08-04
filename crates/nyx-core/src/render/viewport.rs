//! WHAT A VIEWPORT WIDTH CAN HONESTLY MEAN to a render that produces MARKUP (v2.7, finding
//! `01KYT45C6RC7K84V1CA3561SPF` — "no way to render a proto COMPOSITION at a settable viewport").
//!
//! # The honest answer, first
//!
//! **The sidecar lays nothing out.** It has no browser, no CSS engine, no box model: the markup it
//! resolves is byte-for-byte the same at 390 px and at 1440 px. So a `viewport` parameter that
//! claimed to "render at mobile" would be DECORATIVE — the very thing the finding refuses.
//!
//! A width is nevertheless not meaningless here, for two REAL reasons:
//!
//!   1. **It is a value the CONSUMER applies.** The only thing that turns this markup into pixels is
//!      a frame, and a frame needs a width. The tool therefore ECHOES the width back as the width to
//!      apply — transport, and named as such.
//!   2. **It selects which classes are IN FORCE.** On a Tailwind front the responsive design IS the
//!      prefixed classes (`md:flex`, `lg:grid-cols-3`, `max-sm:hidden`), and which of them apply at a
//!      given width is a fact about the markup that can be computed EXACTLY, with no layout engine.
//!      That is what this module computes: the breakpoints in force at the width, the responsive
//!      variants the markup really carries, and which of those two lists intersect.
//!
//! And it makes the most common defect visible in one call: markup carrying NO responsive variant at
//! all renders IDENTICALLY at every width. That is either deliberate or the whole responsive
//! question left unanswered — and until now nothing said which.
//!
//! # Where the breakpoints come from
//!
//! From the PROJECT's own token layer when it declares them (Tailwind v4 writes `--breakpoint-md:
//! 48rem` in its `@theme`, and `design_tokens` ingests exactly that layer), else from Tailwind's
//! defaults. A `rem`/`em` length is converted at [`ASSUMED_ROOT_FONT_PX`] — an ASSUMPTION, so it is
//! reported as one rather than presented as a measurement.

use std::collections::BTreeMap;

/// Tailwind's default breakpoints, in px, ascending. Used when the project's token layer declares
/// none of its own.
pub const DEFAULT_BREAKPOINTS: [(&str, u32); 5] = [
    ("sm", 640),
    ("md", 768),
    ("lg", 1024),
    ("xl", 1280),
    ("2xl", 1536),
];

/// The three NAMED widths the human vocabulary uses (`mobile` / `tablet` / `desktop`) → a
/// REPRESENTATIVE width in CSS px. They are a convenience, not a measurement: the width they resolve
/// to is echoed back so nobody has to guess which device was meant.
pub const NAMED_VIEWPORTS: [(&str, u32); 3] = [("mobile", 390), ("tablet", 768), ("desktop", 1280)];

/// The root font size a `rem`/`em` breakpoint is converted at. The browser default; nyx cannot read
/// the real one, so the conversion is REPORTED as an assumption.
pub const ASSUMED_ROOT_FONT_PX: f64 = 16.0;

/// The token prefix Tailwind v4 declares a breakpoint under (`--breakpoint-md: 48rem`).
const BREAKPOINT_TOKEN_PREFIX: &str = "--breakpoint-";

/// ONE breakpoint, and whether it is in force at the width asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakpointState {
    /// The variant name (`md`), i.e. the class prefix (`md:`).
    pub name: String,
    /// Its `min-width`, in CSS px.
    pub min_width: u32,
    /// `width >= min_width` — the `md:` classes apply.
    pub active: bool,
    /// Did it come from the PROJECT's token layer (`--breakpoint-md`) rather than the defaults?
    pub from_project: bool,
}

/// ONE responsive variant the markup really carries, with how many class tokens use it and whether
/// it is in force at the width asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkupVariant {
    /// The variant as written in the class (`md`, `max-lg`, `min-[600px]`).
    pub prefix: String,
    /// How many class TOKENS carry it.
    pub occurrences: usize,
    /// Does it apply at this width?
    pub active: bool,
}

/// What a width really decides about ONE piece of resolved markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportReport {
    /// The width asked about, in CSS px — the width a FRAME must apply to display this markup.
    pub width: u32,
    /// The name it was asked by (`mobile`), when it was asked by name.
    pub named: Option<String>,
    /// The breakpoints in force (and not) at that width.
    pub breakpoints: Vec<BreakpointState>,
    /// The responsive variants the markup carries, most used first.
    pub variants: Vec<MarkupVariant>,
    /// How many class tokens carry a responsive variant at all.
    pub responsive_classes: usize,
    /// How many class tokens carry a CONTAINER-query variant (`@md:`) — those depend on an
    /// ancestor's width, not on the viewport, so no width can decide them here.
    pub container_classes: usize,
}

impl ViewportReport {
    /// Does this markup change AT ALL with the width? (No responsive variant anywhere ⇒ no.)
    pub fn is_responsive(&self) -> bool {
        self.responsive_classes > 0
    }
}

/// The width a NAMED viewport resolves to.
pub fn named_viewport(name: &str) -> Option<u32> {
    let lowered = name.trim().to_ascii_lowercase();
    NAMED_VIEWPORTS
        .iter()
        .find(|(n, _)| *n == lowered)
        .map(|(_, width)| *width)
}

/// The breakpoints in force for a project: the ones its TOKEN LAYER declares (`--breakpoint-*`),
/// else Tailwind's defaults. A token whose value is not a length nyx can read is ignored (the
/// default for that name survives), because a breakpoint nobody can compute is not a breakpoint.
pub fn breakpoints(tokens: &BTreeMap<String, String>) -> Vec<BreakpointState> {
    let declared: Vec<(String, u32)> = tokens
        .iter()
        .filter_map(|(name, value)| {
            let bp = name.strip_prefix(BREAKPOINT_TOKEN_PREFIX)?.trim();
            let px = parse_length_px(value)?;
            (!bp.is_empty()).then(|| (bp.to_string(), px))
        })
        .collect();
    let mut out: Vec<BreakpointState> = if declared.is_empty() {
        DEFAULT_BREAKPOINTS
            .iter()
            .map(|(name, px)| BreakpointState {
                name: (*name).to_string(),
                min_width: *px,
                active: false,
                from_project: false,
            })
            .collect()
    } else {
        declared
            .into_iter()
            .map(|(name, px)| BreakpointState {
                name,
                min_width: px,
                active: false,
                from_project: true,
            })
            .collect()
    };
    out.sort_by(|a, b| a.min_width.cmp(&b.min_width).then(a.name.cmp(&b.name)));
    out
}

/// A CSS length as CSS px — `768px`, `48rem`, `48em`, a bare `768`. Anything else (a `calc()`, a
/// percentage, a var reference) is `None`.
fn parse_length_px(raw: &str) -> Option<u32> {
    let value = raw.trim();
    let (number, factor) = if let Some(n) = value.strip_suffix("px") {
        (n, 1.0)
    } else if let Some(n) = value.strip_suffix("rem") {
        (n, ASSUMED_ROOT_FONT_PX)
    } else if let Some(n) = value.strip_suffix("em") {
        (n, ASSUMED_ROOT_FONT_PX)
    } else {
        (value, 1.0)
    };
    let parsed: f64 = number.trim().parse().ok()?;
    (parsed.is_finite() && parsed >= 0.0).then(|| (parsed * factor).round() as u32)
}

/// WHAT `width` DECIDES about `markup`: which breakpoints are in force, which responsive variants
/// the markup really carries, and which of them therefore apply. It reads the CLASS attributes and
/// nothing else — no layout is computed, and none could be.
pub fn report(
    markup: &str,
    width: u32,
    named: Option<String>,
    mut breakpoints: Vec<BreakpointState>,
) -> ViewportReport {
    for bp in &mut breakpoints {
        bp.active = width >= bp.min_width;
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut responsive_classes = 0usize;
    let mut container_classes = 0usize;
    for value in class_values(markup) {
        for token in value.split_whitespace() {
            let mut counted_responsive = false;
            let mut counted_container = false;
            for segment in variant_segments(token) {
                if segment.starts_with('@') {
                    if !counted_container {
                        container_classes += 1;
                        counted_container = true;
                    }
                    continue;
                }
                if classify(segment, &breakpoints).is_some() {
                    *counts.entry(segment.to_string()).or_default() += 1;
                    if !counted_responsive {
                        responsive_classes += 1;
                        counted_responsive = true;
                    }
                }
            }
        }
    }
    let mut variants: Vec<MarkupVariant> = counts
        .into_iter()
        .map(|(prefix, occurrences)| {
            let active = classify(&prefix, &breakpoints)
                .map(|(min, max)| width >= min && max.is_none_or(|max| width < max))
                .unwrap_or(false);
            MarkupVariant {
                prefix,
                occurrences,
                active,
            }
        })
        .collect();
    variants.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then(a.prefix.cmp(&b.prefix))
    });
    ViewportReport {
        width,
        named,
        breakpoints,
        variants,
        responsive_classes,
        container_classes,
    }
}

/// The `(min_width, exclusive_max_width)` a VARIANT segment stands for — `md` → `(768, None)`,
/// `max-md` → `(0, Some(768))`, `min-[600px]` → `(600, None)`. `None` when the segment is not a
/// width variant at all (a state variant like `hover`, an arbitrary selector).
fn classify(segment: &str, breakpoints: &[BreakpointState]) -> Option<(u32, Option<u32>)> {
    let named = |name: &str| {
        breakpoints
            .iter()
            .find(|b| b.name == name)
            .map(|b| b.min_width)
    };
    if let Some(px) = named(segment) {
        return Some((px, None));
    }
    if let Some(rest) = segment.strip_prefix("max-") {
        if let Some(px) = named(rest) {
            return Some((0, Some(px)));
        }
        if let Some(px) = arbitrary_length(rest) {
            return Some((0, Some(px)));
        }
        return None;
    }
    if let Some(rest) = segment.strip_prefix("min-") {
        return arbitrary_length(rest).map(|px| (px, None));
    }
    None
}

/// The length inside an ARBITRARY variant's brackets (`[600px]`).
fn arbitrary_length(raw: &str) -> Option<u32> {
    parse_length_px(raw.strip_prefix('[')?.strip_suffix(']')?)
}

/// The VARIANT segments of a class token — everything before the last `:` at bracket depth 0
/// (`md:hover:bg-primary` → `["md", "hover"]`). Brackets are respected so an arbitrary value
/// carrying a `:` (`lg:[&:hover]:block`) does not split into nonsense.
fn variant_segments(token: &str) -> Vec<&str> {
    let mut cuts: Vec<usize> = Vec::new();
    let mut depth = 0i32;
    for (index, c) in token.char_indices() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            ':' if depth <= 0 => cuts.push(index),
            _ => {}
        }
    }
    let mut out = Vec::with_capacity(cuts.len());
    let mut start = 0usize;
    for cut in cuts {
        out.push(&token[start..cut]);
        start = cut + 1;
    }
    out
}

/// The `class="…"` values of a markup fragment, in document order. A deliberately minimal scan —
/// the markup this reads was re-emitted by the resolver, which always writes double-quoted
/// attributes, and a single-quoted one is handled anyway.
fn class_values(markup: &str) -> Vec<&str> {
    let bytes = markup.as_bytes();
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(found) = markup[from..].find("class") {
        let at = from + found;
        from = at + 5;
        // It must be a whole attribute name: preceded by whitespace, and followed by `=`.
        if at == 0 || !bytes[at - 1].is_ascii_whitespace() {
            continue;
        }
        let mut i = from;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || (bytes[i] != b'"' && bytes[i] != b'\'') {
            continue;
        }
        let quote = bytes[i];
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        out.push(&markup[start..i.min(bytes.len())]);
        from = (i + 1).min(bytes.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Vec<BreakpointState> {
        breakpoints(&BTreeMap::new())
    }

    #[test]
    fn the_project_tokens_win_over_the_defaults_and_a_length_is_converted() {
        assert_eq!(
            defaults()
                .iter()
                .map(|b| (b.name.as_str(), b.min_width))
                .collect::<Vec<_>>(),
            vec![
                ("sm", 640),
                ("md", 768),
                ("lg", 1024),
                ("xl", 1280),
                ("2xl", 1536)
            ]
        );
        let tokens: BTreeMap<String, String> = [
            ("--breakpoint-md".to_string(), "48rem".to_string()),
            ("--breakpoint-lg".to_string(), "900px".to_string()),
            // Not a length nyx can compute: ignored rather than guessed.
            (
                "--breakpoint-xl".to_string(),
                "calc(100% - 2rem)".to_string(),
            ),
            ("--primary".to_string(), "oklch(0.5 0.2 264)".to_string()),
        ]
        .into_iter()
        .collect();
        let project = breakpoints(&tokens);
        assert_eq!(
            project
                .iter()
                .map(|b| (b.name.as_str(), b.min_width, b.from_project))
                .collect::<Vec<_>>(),
            vec![("md", 768, true), ("lg", 900, true)],
            "the project's own breakpoints, ascending — `48rem` converted at the assumed root font"
        );
    }

    #[test]
    fn a_width_says_which_variants_apply_and_which_do_not() {
        let markup = r#"<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3">
             <span class="hidden md:inline max-sm:block hover:underline">x</span></div>"#;
        let mobile = report(markup, 390, Some("mobile".into()), defaults());
        assert!(mobile.is_responsive());
        assert_eq!(mobile.responsive_classes, 4, "md × 2, lg, max-sm");
        let by_prefix = |r: &ViewportReport, p: &str| {
            r.variants
                .iter()
                .find(|v| v.prefix == p)
                .cloned()
                .unwrap_or_else(|| panic!("{p} was not seen"))
        };
        assert_eq!(by_prefix(&mobile, "md").occurrences, 2);
        assert!(!by_prefix(&mobile, "md").active, "390 < 768");
        assert!(
            by_prefix(&mobile, "max-sm").active,
            "a max-* variant is in force BELOW its breakpoint"
        );
        assert!(
            !mobile.breakpoints.iter().any(|b| b.active),
            "no breakpoint is crossed at 390"
        );
        let desktop = report(markup, 1280, None, defaults());
        assert!(by_prefix(&desktop, "md").active && by_prefix(&desktop, "lg").active);
        assert!(!by_prefix(&desktop, "max-sm").active);
        assert_eq!(
            desktop
                .breakpoints
                .iter()
                .filter(|b| b.active)
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            vec!["sm", "md", "lg", "xl"]
        );
    }

    #[test]
    fn markup_with_no_responsive_class_is_said_to_be_width_independent() {
        let flat = report(
            r#"<section class="flex items-center gap-2"><b class="font-bold">x</b></section>"#,
            390,
            None,
            defaults(),
        );
        assert!(
            !flat.is_responsive() && flat.variants.is_empty(),
            "nothing here changes with the width, and THAT is the answer: {flat:?}"
        );
    }

    #[test]
    fn stacked_arbitrary_and_container_variants_are_read_for_what_they_are() {
        let markup = r#"<div class="md:hover:bg-primary lg:[&amp;:focus]:ring min-[600px]:flex max-[480px]:hidden @md:grid">x</div>"#;
        let r = report(markup, 640, None, defaults());
        let prefixes: Vec<&str> = r.variants.iter().map(|v| v.prefix.as_str()).collect();
        assert_eq!(
            prefixes,
            vec!["lg", "max-[480px]", "md", "min-[600px]"],
            "a stacked variant is read at its width segment, an arbitrary one at its own length: \
             {prefixes:?}"
        );
        assert!(r
            .variants
            .iter()
            .any(|v| v.prefix == "min-[600px]" && v.active));
        assert!(r
            .variants
            .iter()
            .any(|v| v.prefix == "max-[480px]" && !v.active));
        assert_eq!(
            r.container_classes, 1,
            "a container query depends on an ANCESTOR's width, never on the viewport"
        );
    }

    #[test]
    fn the_class_scan_ignores_what_is_not_a_class_attribute() {
        let values =
            class_values(r#"<i data-class="md:x" class='sm:flex' title="class=lg:no"></i>"#);
        assert_eq!(
            values,
            vec!["sm:flex"],
            "a `data-class` is not a class, a single-quoted one is"
        );
    }

    #[test]
    fn a_named_viewport_resolves_to_its_documented_width() {
        assert_eq!(named_viewport("mobile"), Some(390));
        assert_eq!(named_viewport(" Desktop "), Some(1280));
        assert_eq!(named_viewport("phablet"), None);
    }

    /// THE ARRIMAGE of the NAMED WIDTHS (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 7). These
    /// three are served to the AGENT here and offered to the HUMAN as the stage presets of the
    /// prototype page (`design/viewports.ts`), and the two lists were joined by nothing: move
    /// `tablet` to 834 here and the agent reasons at 834 while the human validates at 768, with no
    /// test anywhere disagreeing. Both directions, and in order — the order the presets are offered.
    #[test]
    fn the_shared_fixture_pins_the_named_viewport_widths() {
        let section = crate::mcp_tools_design::design_fixture_section("viewports");
        let pinned: Vec<(String, u32)> = section["named"]
            .as_array()
            .expect("the named viewports")
            .iter()
            .map(|v| {
                (
                    v["name"].as_str().expect("a name").to_string(),
                    v["width"].as_u64().expect("a width") as u32,
                )
            })
            .collect();
        assert_eq!(
            pinned,
            NAMED_VIEWPORTS
                .iter()
                .map(|(n, w)| ((*n).to_string(), *w))
                .collect::<Vec<_>>(),
            "the fixture names exactly the widths this module resolves, in the same order"
        );
        for (name, width) in &pinned {
            assert_eq!(
                named_viewport(name),
                Some(*width),
                "`{name}` resolves to the pinned width"
            );
        }
    }
}
