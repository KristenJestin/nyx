//! THE GRAMMAR OF A SPEC REFERENCE — the one place a typed token becomes a Spec id.
//!
//! nyx **names** its Specs by their human key and used to **accept** only their canonical id. The
//! human never sees that id: the rendered context, the warnings and the UI all speak in `#3`, so
//! `#3` is the only form they can type — and it was the only form nothing accepted. A `/nspec 3`
//! died at the first link, the run opened unanchored, and the flow drifted all the way to ordering
//! a DUPLICATE of the very Spec that was meant.
//!
//! FOUR EQUIVALENT FORMS, ONE IMPLEMENTATION. A reference is the canonical id, the human key
//! `KEY-N`, `#N`, or the bare integer `N`. They resolve to the SAME Spec, and this module is the
//! only place in the repo that knows that — every Spec-reference argument reaches it through
//! [`crate::mcp_context`], and through nothing else: [`crate::mcp_context::resolve_spec_arg`] when
//! the project has to be derived from the call's own anchors, and
//! [`crate::mcp_context::resolve_spec_ref_in`] when the caller already holds one (`workspace_add`,
//! `idea_add`). A second implementation is how the hook's recogniser and the core drifted apart in
//! the first place, so nothing here is to be restated anywhere: the one truth to keep is this one.
//!
//! THE HOOK IS A RECOGNISER, NEVER A RESOLVER. `nyx-session-hook.cjs` opens the run BEFORE the
//! agent says anything, so a form IT cannot see is a form that never reaches this module on that
//! path — and its regex used to know the canonical id ALONE, the one shape a human never sees,
//! which is why a `/nspec 3` arrived at `run_start` with no Spec named at all. It recognises the
//! four SHAPES now (`SPEC_REF` / `specIdFromPrompt`), pinned to this grammar by the corpus both
//! sides replay ([`tests::the_shared_corpus_pins_the_hook_recogniser_to_this_grammar`](mod@tests)).
//! What that pinning does NOT do is make the two one implementation — the division of labour it
//! must respect is settled HERE: the hook matches a SHAPE and forwards the token RAW, byte for
//! byte as typed, while the TRANSLATION and the REFUSAL stay in Rust, in this module. That
//! asymmetry is deliberate and is NOT a gap to close: this module bounds a reference by VALUE, and
//! its two bounds do not even reach the same verdict. An integer too large for an `i64` never
//! becomes a reference at all — it reads as PROSE ([`SpecRefResolution::NotAReference`], see
//! [`parse`]) — while one that fits `i64` but overflows the 32-bit `specs.number` column IS a
//! reference, one that designates nothing ([`SpecRefResolution::Unresolved`], see [`lookup`]). Two
//! judgements on a VALUE, taken where the Spec is actually looked up, and a shape-only recogniser
//! has no business restating either. A hook that resolved would be a second truth to keep aligned
//! with this one, which is precisely how the two drifted apart in the first place. Its own
//! discipline stands either way: the FIRST token, and only when it is a whole reference.
//!
//! THE RECOGNITION ORDER IS PART OF THE CONTRACT: canonical id first, then `KEY-N`, then `#N` /
//! `N`. A full id can therefore never be re-read as something else, and a call that already passes
//! one gains NO new resolution path — its behaviour is strictly unchanged, which is what makes
//! widening the grammar safe for every `spec_id` field that inherits it, existing or yet to be
//! added: the property holds per FIELD, so there is no population to count here — and no count to
//! keep true. The advertised fields ARE counted, but only where a test asserts the number and
//! fails when it drifts (`mcp`'s `every_spec_id_argument_advertises_the_forms_it_accepts`).
//!
//! THE BARE INTEGER IS ADMITTED ONLY AS A WHOLE. Only a token that is ENTIRELY numeric counts, so
//! a word of the human's prose can never be mistaken for an anchor — the discipline the session
//! hook had already given itself for the id (`nyx-session-hook.cjs`), extended without being
//! relaxed. `3abc`, `v3` and `3.1` are prose.
//!
//! RESOLUTION IS PROJECT-RELATIVE, NEVER TRANSVERSE. `idx_specs_project_number` is UNIQUE on
//! `(project_id, number)`, so a reference designates exactly **0 or 1** Spec inside a project:
//! there is no N case, hence no ambiguity contract to invent (contrast the LOCATION-based
//! resolution in [`crate::mcp_context`], which must refuse when a workspace anchors several
//! Specs). The project is the caller's to supply — this module never looks past it.
//!
//! THE ONE EXCEPTION IS `KEY-N`, WHICH CARRIES ITS OWN PROJECT. `KEY-3` names the project keyed
//! `KEY`, so it is resolved THERE — never in the caller's — and the answer SAYS which project it
//! landed in ([`ResolvedSpecRef::project_id`]). A disagreement with the caller's own project is
//! not judged here: the caller confronts the RESOLVED id with the cross-project guard it already
//! owns ([`crate::mcp_context::resolve_run_target`]), exactly as it would a hand-typed id. ONE
//! guard, in one place — a short reference must not become the back door through which another
//! project's Spec enters, and a second verdict computed here would be the duplication this module
//! exists to refuse.

use diesel::prelude::*;
use diesel::QueryResult;

use crate::db;

/// The SHAPE a Spec reference token was recognised as — the output of [`parse`], before any
/// database is consulted. Recognition and resolution are separate on purpose: "this is not a
/// reference at all" (prose) and "this is a reference that designates nothing" (`#99`) are two
/// different answers, and the callers downstream treat them differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecRef {
    /// The canonical id nyx mints (a v7 UUID). It RESOLVES TO ITSELF — no lookup, no project, no
    /// other path. Recognised FIRST so a full id is never re-read as anything else.
    Uuid(String),
    /// The human key `KEY-N` (`KEY-3`): a project key and the Spec's per-project number. The key
    /// is matched case-insensitively — the human types what the render showed them.
    Key {
        /// The project key as typed (its case is not normalised here; the lookup folds it).
        key: String,
        /// The per-project sequence number.
        number: i64,
    },
    /// `#N` or the bare integer `N` — the form the renders actually display. It carries no
    /// project, so it resolves ONLY relative to the one the caller supplies.
    Number(i64),
}

/// A reference that DID designate a Spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpecRef {
    /// The canonical id of the designated Spec — what every caller hands downstream.
    pub spec_id: String,
    /// The project the designated Spec actually belongs to. For `#N` / `N` it is the project the
    /// caller supplied, by construction; for a `KEY-N` it is the project THE KEY named, which may
    /// be another one — the fact a caller's cross-project guard is applied to, on the resolved id.
    ///
    /// NO SECOND VERDICT IS COMPUTED HERE (finding `01M0ZGFRXN01DG3H3NE7DZP4X0`). A
    /// `cross_project: bool` used to ride alongside, saying "this key named another project" — and
    /// nothing outside this module's own tests ever read it, because the ONE entry point
    /// ([`crate::mcp_context::resolve_spec_arg`]) deliberately passes NO project for a `KEY-N` and
    /// lets [`crate::mcp_context::resolve_run_target`] confront the resolved id exactly as it does
    /// a hand-typed one. A flag that no caller reads is a claim nothing keeps honest; the project
    /// itself is the fact, and the guard that reads it lives where it always did.
    pub project_id: String,
}

/// What a token turned out to be, once the database has been asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecRefResolution {
    /// The token is NOT a Spec reference — prose, a slug, a partial id, an id in some other
    /// scheme. The caller keeps its previous behaviour for it (nyx passes it through verbatim),
    /// which is what makes widening the grammar a no-op for every existing call.
    NotAReference,
    /// A canonical id: it IS the answer. Returned WITHOUT a lookup, so a caller passing one is
    /// affected by nothing in this module.
    Canonical(String),
    /// A recognised reference that designates NO Spec (`#99` in a project whose numbers stop at
    /// 5, `ACME-3` when no project carries the key `ACME`). Distinct from [`Self::NotAReference`]
    /// because a caller may legitimately REFUSE this one while letting prose through.
    Unresolved(SpecRef),
    /// Exactly one Spec.
    Resolved(ResolvedSpecRef),
}

/// The accepted forms, in one sentence — named in every refusal so an agent learns the grammar
/// from the error instead of guessing. ONE string, so the message and the parser cannot drift.
///
/// THE EXAMPLE KEY IS A PLACEHOLDER, NOT A PROJECT (finding `01M0ZGBS0AX3WMWF88BBXH2ZMA`). This
/// sentence rides in the MCP description of every Spec-reference field whose call CAN anchor a
/// project — never in the ones that cannot, which announce [`ANCHORLESS_FORMS`] instead — and in
/// every `run_start` refusal. So it is read by an agent working on ANY project; spelling nyx's own
/// key there taught a human on `ACME` to look for `NYX`, and wired one project's identity into the
/// generic domain (nyx stays GENERIC — no project's, agent's or tool's name in the core domain).
/// `KEY-3` carries the same information and names no one.
pub const ACCEPTED_FORMS: &str =
    "the canonical id, the human key (e.g. KEY-3), '#3', or the bare number 3";

/// ONE WITNESS PER ENUMERATED FORM — `(token, how the sentence names it)` — declared NEXT TO
/// [`ACCEPTED_FORMS`] so that whoever teaches [`parse`] a form has the sentence and this table
/// under the same eye. PROXIMITY IS ALL IT IS, and proximity is not a mechanism: the table is BY
/// HAND, nothing derives it from `parse`, so it pins the forms it enumerates and claims nothing
/// about a form it does not (finding `01M1102H9JCX83JN85EVEE3K7T` — the bound is stated where it
/// is asserted, [`tests::every_enumerated_form_parses_and_is_named_by_the_sentence`](mod@tests)).
/// The sentence is prose, and prose alone binds to nothing — this table is the half of the lock
/// that faces the parser.
///
/// `#[cfg(test)]`, and `pub(crate)` under it: the table is a TEST INSTRUMENT, and a `pub` const in
/// a library escapes `dead_code` for good — nothing would ever report that its only readers are
/// three lines of `mod tests` (finding `01M0ZGBS00WKXZ43WFZ28MK3GX`). The repo's own convention
/// for such a constant is [`crate::mcp_tools_design::DESIGN_FIXTURES`]. Its argument is untouched:
/// it still sits at this exact spot in the file, next to the sentence it locks.
#[cfg(test)]
pub(crate) const ACCEPTED_FORM_WITNESSES: &[(&str, &str)] = &[
    // A canonical id — any 8-4-4-4-12 hex shape reads as one; this one is well-formed filler.
    ("decafbad-cafe-babe-f00d-0123456789ab", "the canonical id"),
    // The human key, as the sentence's own example spells it.
    ("KEY-3", "KEY-3"),
    // The sigil'd number, exactly as the sentence quotes it.
    ("#3", "'#3'"),
    // The bare number — the same reference without the sigil.
    ("3", "the bare number 3"),
];

/// The SELF-ANCHORED subset — the forms that carry their own project and therefore work on a call
/// that resolves none. Named wherever `#N` / `N` cannot be honoured, because a refusal that closes
/// by listing the form it has just turned away is not actionable: this is what to send INSTEAD.
///
/// Same placeholder discipline as [`ACCEPTED_FORMS`], and for the same reason: this sentence is
/// diffused to every agent, on every project.
pub const ANCHORLESS_FORMS: &str = "the canonical id or the human key (e.g. KEY-3)";

/// RECOGNISE `token` as a Spec reference — pure, no database, no project.
///
/// `None` means "not a reference": the token is prose, a slug, a partial id, or an id in another
/// scheme, and nyx must keep treating it exactly as it did before this grammar existed.
///
/// A number too large to hold in an `i64` reads as prose: `specs.number` is a 32-bit column, so
/// such a token could not designate a Spec under any reading, and the caller's refusal is the same
/// one branch earlier.
pub fn parse(token: &str) -> Option<SpecRef> {
    if token.is_empty() {
        return None;
    }
    // 1. THE CANONICAL ID, FIRST AND UNCONDITIONALLY (see the module note on the order).
    if is_canonical_uuid(token) {
        return Some(SpecRef::Uuid(token.to_string()));
    }
    // 2. `KEY-N`. A single hyphen separating a project key from an all-digit number; anything else
    //    carrying a hyphen is not a reference (a multi-hyphen token fails `all_digits` on the
    //    remainder, so `a-b-c` falls out here rather than being read as key `a`).
    if let Some((key, digits)) = token.split_once('-') {
        if is_project_key(key) && all_digits(digits) {
            return digits.parse::<i64>().ok().map(|number| SpecRef::Key {
                key: key.to_string(),
                number,
            });
        }
        return None;
    }
    // 3. `#N` and the bare `N` — the SAME form, one with the sigil the renders print. The digits
    //    must be the WHOLE token: `3abc` and `3.1` are prose, deliberately.
    let digits = token.strip_prefix('#').unwrap_or(token);
    if all_digits(digits) {
        return digits.parse::<i64>().ok().map(SpecRef::Number);
    }
    None
}

/// RESOLVE `token` to a Spec id, relative to `project_id` (the project the caller's own anchors
/// determined — this module never resolves one itself, and never looks past the one it is given).
///
/// `project_id` may be `None`: a caller with no anchor at all can still resolve a canonical id or
/// a `KEY-N` (which carries its project), while `#N` / `N` designate nothing — there is no project
/// to count in, and guessing one across the whole database is exactly the cross-project leak this
/// grammar refuses to open.
pub fn resolve(
    conn: &mut SqliteConnection,
    project_id: Option<&str>,
    token: &str,
) -> QueryResult<SpecRefResolution> {
    let Some(reference) = parse(token) else {
        return Ok(SpecRefResolution::NotAReference);
    };
    match &reference {
        SpecRef::Uuid(id) => Ok(SpecRefResolution::Canonical(id.clone())),
        SpecRef::Key { key, number } => {
            // ABSENT AND AMBIGUOUS RESOLVE THE SAME NOTHING here, deliberately: a key several
            // projects carry once case is folded designates no ONE project, and this module has no
            // ambiguity contract to state ([`db::ProjectByKey`] carries the distinction for a
            // caller that ever needs to phrase it differently — none does today).
            let db::ProjectByKey::Found(project) = db::project_by_key(conn, key)? else {
                return Ok(SpecRefResolution::Unresolved(reference));
            };
            match lookup(conn, &project.id, *number)? {
                Some(spec_id) => Ok(SpecRefResolution::Resolved(ResolvedSpecRef {
                    spec_id,
                    project_id: project.id,
                })),
                None => Ok(SpecRefResolution::Unresolved(reference)),
            }
        }
        SpecRef::Number(number) => {
            let Some(project_id) = project_id else {
                return Ok(SpecRefResolution::Unresolved(reference));
            };
            match lookup(conn, project_id, *number)? {
                Some(spec_id) => Ok(SpecRefResolution::Resolved(ResolvedSpecRef {
                    spec_id,
                    project_id: project_id.to_string(),
                })),
                None => Ok(SpecRefResolution::Unresolved(reference)),
            }
        }
    }
}

/// The Spec of `project_id` a FAILED reference most probably meant — the "did you mean" half of an
/// actionable refusal. The LIVE Spec whose number is nearest `wanted` (the lower one on a tie, so
/// the answer is deterministic).
///
/// `Ok(None)` MEANS ONE THING AND ONLY ONE: the project holds NO live Spec, a fact the caller may
/// state as such. It used to mean that OR "this reference carries no number to be near" (a
/// canonical id), and a caller that read the two as one told a human their project held no Spec
/// while it held five. The NUMBER is therefore the caller's to supply: a reference that has none
/// cannot reach this function, so the ambiguity has nowhere to come back from.
///
/// A SUGGESTION, never a resolution: nothing in nyx acts on it, it only goes into a sentence. That
/// is why "nearest" is an acceptable heuristic here and would not be anywhere in [`resolve`].
pub fn likely_candidate(
    conn: &mut SqliteConnection,
    project_id: &str,
    wanted: i64,
) -> QueryResult<Option<db::Spec>> {
    // The NEAREST, taken as a minimum rather than read off a sort: one row is wanted, so ordering
    // the other n-1 buys nothing. The LOWER number breaks a tie, so the sentence a human reads is
    // stable — `min_by_key` keeps the FIRST minimum and `list_specs` orders by `(position, number)`,
    // which is why the tie-break rides in the key itself and not in the traversal order.
    //
    // SATURATING, because `wanted` is UNBOUNDED: it comes from a token a human typed, and [`parse`]
    // admits anything that fits an `i64` (`9223372036854775807` rides in the shared corpus for that
    // very reason). `n - wanted` on a negative `number` would then overflow — a debug panic, a wrap
    // in release — and `i64::MIN.abs()` panics either way. Saturating keeps a "how far off is this?"
    // heuristic total: at that distance the ordering it produces is the same one.
    Ok(db::list_specs(conn, project_id)?
        .into_iter()
        .min_by_key(|s| {
            let n = i64::from(s.number);
            (n.saturating_sub(wanted).saturating_abs(), n)
        }))
}

/// The `(project, number)` read, kept in one spot so both reference forms hit the same UNIQUE
/// index and can never disagree about what `#3` means.
fn lookup(
    conn: &mut SqliteConnection,
    project_id: &str,
    number: i64,
) -> QueryResult<Option<String>> {
    // `specs.number` is a 32-bit column: a reference outside that range designates nothing, and
    // saying so here keeps the cast total instead of panicking on a wrap.
    let Ok(number) = i32::try_from(number) else {
        return Ok(None);
    };
    Ok(db::spec_by_project_number(conn, project_id, number)?.map(|s| s.id))
}

/// The CANONICAL shape nyx mints its ids in (`8-4-4-4-12` hex), anchored so a token is either the
/// whole id or not an id at all — the same rule, and the same case-insensitivity, the session
/// hook's `SPEC_REF.uuid` regex applies (`nyx-session-hook.cjs`, read by `specIdFromPrompt`), and
/// which it tries FIRST for the same reason this branch does: a whole id must never be re-read as
/// a key.
fn is_canonical_uuid(token: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let mut parts = token.split('-');
    for len in GROUPS {
        let Some(part) = parts.next() else {
            return false;
        };
        if part.len() != len || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
    }
    parts.next().is_none()
}

/// Whether `s` could be a `projects.key`: a leading ASCII letter then ASCII alphanumerics — the
/// shape nyx derives (`derive_project_key_stub` uppercases letters, `unique_project_key` may append
/// a digit) and the shape a curated Linear prefix takes. Whether such a project EXISTS is a
/// question for the database, not for the grammar.
fn is_project_key(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric())
}

/// Whether `s` is non-empty and ENTIRELY ASCII digits — the `^\d+$` discipline that keeps a word
/// of prose from being read as an anchor.
fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SpecSections;

    /// A project named `name` (its key is derived from it, uppercased) plus `count` Specs numbered
    /// 1..=count. Returns `(project_id, key, spec ids in number order)`.
    fn project_with_specs(
        conn: &mut SqliteConnection,
        name: &str,
        root: &str,
        count: usize,
    ) -> (String, String, Vec<String>) {
        let (project, _) = db::create_project(conn, name, root, None).expect("create project");
        let ids = (1..=count)
            .map(|n| {
                db::create_spec(
                    conn,
                    &project.id,
                    &format!("Spec {n}"),
                    SpecSections::empty(),
                )
                .expect("create spec")
                .0
                .id
            })
            .collect();
        (project.id, project.key, ids)
    }

    /// THE HALF OF THE LOCK THE SENTENCE ALONE COULD NOT PROVIDE: [`ACCEPTED_FORMS`] is prose, and
    /// prose drifts silently. Every enumerated witness is fed to `parse` AND its mention looked for
    /// in the sentence, so the advertisement and the parser fail TOGETHER — a witness `parse` has
    /// stopped accepting, or a mention the sentence has stopped carrying, goes red here.
    ///
    /// WHAT IT DOES NOT HOLD, AND ONCE CLAIMED TO (finding `01M1102H9JCX83JN85EVEE3K7T`): the
    /// other direction. This test was called `the_enumerated_forms_are_exactly_what_parse_accepts`
    /// and the note beside the corpus read "a form added to `parse` forces a witness into the
    /// table" — neither was true. [`ACCEPTED_FORM_WITNESSES`] is kept BY HAND, and NO test can
    /// enumerate the token set a hand-written parser accepts: only a `parse` rewritten as a table
    /// this test reads would make it derivable, and that does not remove the drift, it moves it
    /// into the table. So a SPELLING added to `parse` alone — an `@3` routed to
    /// [`SpecRef::Number`] — turns nothing red anywhere, and the name no longer says otherwise.
    ///
    /// WHAT **IS** DERIVED FROM `parse`, because the type reaches that far: its BRANCHES. Each
    /// witness's variant is named by an EXHAUSTIVE match, so a variant added to [`SpecRef`] — the
    /// way `parse` grows a new KIND of reference, as opposed to a second spelling of an old one —
    /// stops this file from compiling until an arm is written HERE, in the test that then names the
    /// table the branch owes a witness to. The whole mapping is asserted at once, which is also
    /// where the four enumerated forms show as THREE branches: `#3` and `3` are one reference under
    /// two spellings.
    #[test]
    fn every_enumerated_form_parses_and_is_named_by_the_sentence() {
        // EXHAUSTIVE ON PURPOSE — this arm list is the tripwire described above, not a formatter.
        fn branch_of(reference: &SpecRef) -> &'static str {
            match reference {
                SpecRef::Uuid(_) => "SpecRef::Uuid",
                SpecRef::Key { .. } => "SpecRef::Key",
                SpecRef::Number(_) => "SpecRef::Number",
            }
        }

        let mut branches = Vec::new();
        for (witness, mention) in ACCEPTED_FORM_WITNESSES {
            let Some(reference) = parse(witness) else {
                panic!(
                    "`parse` rejects '{witness}', which ACCEPTED_FORMS enumerates as '{mention}'"
                )
            };
            assert!(
                ACCEPTED_FORMS.contains(mention),
                "ACCEPTED_FORMS no longer names '{mention}' — the sentence and the parser \
                 have drifted"
            );
            branches.push(branch_of(&reference));
        }
        assert_eq!(
            branches,
            [
                "SpecRef::Uuid",
                "SpecRef::Key",
                "SpecRef::Number",
                "SpecRef::Number"
            ],
            "the enumerated forms are the branches of `parse`, in table order — `#3` and `3` are \
             one reference under two spellings, and any other mapping is a form that changed \
             meaning"
        );
    }

    /// The three SECTION HEADERS of the corpus — the verdict each token under them is pinned to.
    /// They are matched as WRITTEN in the file, so renaming one there without renaming it here is
    /// a compile-time-visible break rather than a section that silently stops being checked.
    ///
    /// TWO WAYS TO BE REFUSED, AND THEY ARE NOT THE SAME FACT. [`CORPUS_REFUSED_HEADER`] holds what
    /// NO form recognises — the two readers of this file must agree on it. [`CORPUS_SHAPE_ONLY_HEADER`]
    /// holds what IS one of the four shapes and dies on this module's VALUE bound: the hook
    /// forwards those, [`parse`] refuses them, and that disagreement is the division of labour
    /// itself (see the module note), not a drift to close.
    const CORPUS_ACCEPTED_HEADER: &str = "// --- accepted by spec_ref::parse";
    const CORPUS_REFUSED_HEADER: &str = "// --- refused by spec_ref::parse";
    const CORPUS_SHAPE_ONLY_HEADER: &str = "// --- a recognised SHAPE";

    /// THE SHARED CORPUS IS THE OTHER HALF OF THE LOCK. `contract/spec-ref-corpus.txt` holds the
    /// token set this grammar is pinned on, one whole token per line, under the section headers
    /// above that state its verdict — the three [`CORPUS_ACCEPTED_HEADER`] /
    /// [`CORPUS_REFUSED_HEADER`] / [`CORPUS_SHAPE_ONLY_HEADER`] this reader branches on, so the
    /// headers are named once and counted nowhere. A witness forces its way into that file, or
    /// [`tests::the_witnesses_never_leave_the_shared_corpus`](mod@tests) goes red — and every line
    /// of the file is then held to its section by
    /// [`tests::every_corpus_token_answers_as_its_section_says`](mod@tests).
    ///
    /// THE CHAIN STARTS AT THE TABLE, NOT AT `parse` (finding `01M1102H9JCX83JN85EVEE3K7T`): this
    /// note used to open with "a form added to [`parse`] forces a witness into the table above",
    /// which nothing holds — [`ACCEPTED_FORM_WITNESSES`] is hand-kept, and what follows from a
    /// witness is locked here while what precedes it is not. This file pins the forms that ARE
    /// enumerated; a form `parse` learns quietly is caught by neither half.
    ///
    /// IT LIVES IN `contract/` — the repo's established home for a fixture READ FROM BOTH SIDES of
    /// the Rust↔JS line, as `contract/README.md` states it in its own first sentence ("Where a rule
    /// or a wire format exists twice, once in Rust and once in TypeScript, this directory holds the
    /// shared cases that make the two halves fail together"), with the exact precedent of
    /// `contract/design-fixtures.json` and the same `include_str!(CARGO_MANIFEST_DIR)` idiom
    /// ([`crate::mcp_tools_design::DESIGN_FIXTURES`]). It sat under `crates/nyx-core/tests/` first,
    /// justified as "the shared half" — but that directory is cargo's INTEGRATION-TEST folder, which
    /// nothing outside the crate reads, so the justification described a property the location did
    /// not have (finding `01M0ZGBRZNAJTD5XQCW43STXZA`). The session hook's shape-only recogniser IS
    /// that second reader now — [`tests::the_shared_corpus_pins_the_hook_recogniser_to_this_grammar`](mod@tests)
    /// replays this very file against it, under `node` — and it finds the corpus where every other
    /// shared fixture lives.
    ///
    /// Returns `(section header, token)` so a caller can tell the verdicts apart — including the
    /// two ways of being refused, which the hook answers differently and this reader keeps distinct.
    fn shared_corpus() -> Vec<(&'static str, &'static str)> {
        const TEXT: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contract/spec-ref-corpus.txt"
        ));
        let mut section = "";
        let mut tokens = Vec::new();
        for line in TEXT.lines().map(str::trim) {
            if line.starts_with(CORPUS_ACCEPTED_HEADER) {
                section = CORPUS_ACCEPTED_HEADER;
            } else if line.starts_with(CORPUS_REFUSED_HEADER) {
                section = CORPUS_REFUSED_HEADER;
            } else if line.starts_with(CORPUS_SHAPE_ONLY_HEADER) {
                section = CORPUS_SHAPE_ONLY_HEADER;
            }
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            tokens.push((section, line));
        }
        tokens
    }

    #[test]
    fn the_witnesses_never_leave_the_shared_corpus() {
        let corpus = shared_corpus();
        assert!(
            corpus.len() >= ACCEPTED_FORM_WITNESSES.len(),
            "the corpus carries at least one token per enumerated form, got {corpus:?}"
        );
        for (witness, mention) in ACCEPTED_FORM_WITNESSES {
            assert!(
                corpus.iter().any(|(_, t)| t == witness),
                "'{witness}' ({mention}) must ride in contract/spec-ref-corpus.txt — that file is \
                 the token set the session hook replays against `parse`"
            );
        }
        // Every line IS a whole token: no blanks, no embedded whitespace, no comment leakage.
        for (_, token) in &corpus {
            assert_eq!(
                token.trim(),
                *token,
                "the corpus reader trims; lines must not rely on it: '{token}'"
            );
            assert!(
                !token.chars().any(char::is_whitespace),
                "one WHOLE token per line — whitespace is input tokenisation, not grammar: '{token}'"
            );
        }
    }

    /// THE HEADERS ARE A CLASSIFICATION, SO THEY ARE REPLAYED. The corpus does not merely hold
    /// tokens: it sorts them into "accepted by `spec_ref::parse`" and "refused by it", and until
    /// now nothing ran that claim — a second statement of the grammar living outside the parser,
    /// free to drift, which is the exact failure mode this module was written to end (findings
    /// `01M0ZGBRZ9HV90Z8V7VJAV50Y6` / `01M0ZGEFQN84739W8N4N4SBED3`).
    ///
    /// Now every line is fed to `parse` and must answer as its section says. Moving one token
    /// across a header without touching `parse` turns this red, and so does a change to `parse`
    /// that reclassifies a token without moving it — the lock closes in both directions. Every
    /// token must ALSO sit under one of the three headers: a line above them all would be checked
    /// by nothing, so its section is asserted before its verdict.
    ///
    /// The two REFUSED sections answer the same thing to `parse` and are checked together here;
    /// what separates them is the HOOK's answer, which is the other test's business.
    #[test]
    fn every_corpus_token_answers_as_its_section_says() {
        let corpus = shared_corpus();
        let mut accepted = 0usize;
        let mut refused = 0usize;
        let mut shape_only = 0usize;
        for (section, token) in &corpus {
            match *section {
                CORPUS_ACCEPTED_HEADER => {
                    assert!(
                        parse(token).is_some(),
                        "'{token}' rides under \"{CORPUS_ACCEPTED_HEADER}\" but `parse` refuses it"
                    );
                    accepted += 1;
                }
                CORPUS_REFUSED_HEADER | CORPUS_SHAPE_ONLY_HEADER => {
                    assert!(
                        parse(token).is_none(),
                        "'{token}' rides under \"{section}\" but `parse` reads it as {:?}",
                        parse(token)
                    );
                    if *section == CORPUS_REFUSED_HEADER {
                        refused += 1;
                    } else {
                        shape_only += 1;
                    }
                }
                _ => panic!("'{token}' sits under NO section header — no verdict is pinned on it"),
            }
        }
        assert!(
            accepted >= ACCEPTED_FORM_WITNESSES.len() && refused > 0 && shape_only > 0,
            "all three sections must actually be exercised, saw {accepted} accepted / {refused} \
             refused / {shape_only} shape-only"
        );
    }

    /// THE SHADOW RECOGNISER IS PINNED TO THIS GRAMMAR — the corpus's second reader, and the reason
    /// it lives in `contract/`.
    ///
    /// The session hook (`nyx-session-hook.cjs`) opens the run BEFORE the agent says anything, so
    /// its `specIdFromPrompt` decides which forms reach this module ON THAT PATH at all: a shape it
    /// cannot see never gets here, which is exactly how a `/nspec 3` opened an unanchored run while
    /// this module would have resolved it. Two grammars in two languages, with nothing making them
    /// fail together, is the drift the whole module was written to end — so every token of the
    /// corpus is replayed against the REAL hook, under `node`, through the REAL function (the hook
    /// exports it; no copy of the regex exists in this test).
    ///
    /// WHAT AGREEMENT MEANS HERE, precisely: the hook forwards a token iff its SHAPE is one of the
    /// four. So it must forward everything under the accepted header AND everything under the
    /// shape-only header — the tokens `parse` refuses on its `i64` bound — and forward NOTHING under
    /// the refused header. That third section is not a hole in the lock, it IS the lock's statement
    /// of the division of labour: a recogniser that mirrored a value bound it cannot act on would be
    /// the second truth this contract exists to forbid.
    ///
    /// The token is forwarded RAW: the assertion is byte equality with what was typed, `#` and
    /// leading zeros included, because the translation is this module's and no one else's.
    ///
    /// SKIPS when `node` is absent — ON A DEV MACHINE ONLY, and never where it would matter. The
    /// hook's own e2e set that precedent, but it covers an e2e; this test is the ONLY thing holding
    /// two implementations of one grammar together, and a lock that can switch itself off in silence
    /// is not one: libtest CAPTURES the output of a test that PASSES, so the skip line was never
    /// printed and the suite went green with the lock disarmed (finding
    /// `01M0ZWAHA48A214M0M3107YH28`).
    ///
    /// SO THE SKIP IS BOUNDED BY WHERE IT HAPPENS: under `CI` (every GitHub Actions job sets it) or
    /// under an explicit `NYX_REQUIRE_NODE`, a missing `node` FAILS instead of skipping. That is the
    /// place where the absence would otherwise be invisible AND consequential — a machine without
    /// node is a machine where the hook cannot run at all, so demanding it there costs nothing and
    /// buys the guarantee that the green tick means the two grammars were actually compared.
    #[test]
    fn the_shared_corpus_pins_the_hook_recogniser_to_this_grammar() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let node_ok = Command::new("node")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !node_ok {
            assert!(
                std::env::var_os("CI").is_none() && std::env::var_os("NYX_REQUIRE_NODE").is_none(),
                "node is unavailable, so the ONLY lock between the hook's recogniser and this \
                 module's grammar did not run — and CI (or NYX_REQUIRE_NODE) is exactly where that \
                 must be a failure rather than a silent skip. Install node, or unset the variable \
                 to accept an unverified grammar"
            );
            eprintln!("node unavailable: skipped (node-agnostic by contract, off CI only)");
            return;
        }
        let hook = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/claude-plugin/hooks/nyx-session-hook.cjs");
        let corpus = shared_corpus();
        let tokens: Vec<&str> = corpus.iter().map(|(_, t)| *t).collect();

        // The driver REQUIRES the hook (which is why the hook guards its own `main` on
        // `require.main`) and calls its recogniser on a real invocation string — the token in FIRST
        // position, followed by the human's prose, which is the only position the hook ever reads.
        // Tokens travel on stdin as JSON so no shell or argv encoding can touch them.
        const DRIVER: &str = r#"
const hook = require(process.argv[1]);
let buf = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (d) => { buf += d; });
process.stdin.on("end", () => {
  const tokens = JSON.parse(buf);
  const answers = tokens.map((t) => hook.specIdFromPrompt("/ndev " + t + " ajoute le retry"));
  process.stdout.write(JSON.stringify(answers));
});
"#;
        let mut child = Command::new("node")
            .arg("-e")
            .arg(DRIVER)
            .arg(&hook)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn node");
        let payload = serde_json::to_string(&tokens).expect("tokens as json");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("write tokens");
        let out = child.wait_with_output().expect("node ran");
        assert!(
            out.status.success(),
            "the driver ran: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let answers: Vec<String> = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "the driver answered JSON: {e} — stdout {:?} stderr {:?}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert_eq!(answers.len(), corpus.len(), "one answer per corpus token");

        for ((section, token), answer) in corpus.iter().zip(&answers) {
            let forwarded = match *section {
                // A shape the grammar names — the hook must SEE it, or it never reaches `parse`.
                CORPUS_ACCEPTED_HEADER | CORPUS_SHAPE_ONLY_HEADER => true,
                // No form recognises it: the human's prose is never mistaken for an anchor.
                CORPUS_REFUSED_HEADER => false,
                _ => panic!("'{token}' sits under NO section header"),
            };
            if forwarded {
                assert_eq!(
                    answer, token,
                    "the hook must forward '{token}' RAW (section \"{section}\"), got '{answer}'"
                );
            } else {
                assert_eq!(
                    answer, "",
                    "'{token}' rides under \"{CORPUS_REFUSED_HEADER}\", so the hook must name no \
                     reference at all — it forwarded '{answer}'"
                );
            }
        }
    }

    /// THE CENTRAL EQUIVALENCE: the four forms are one reference. Whatever the human types — the
    /// id they never see, the key the render prints, the `#3` the list shows, or the bare `3` they
    /// actually type — the SAME Spec comes back.
    #[test]
    fn the_four_forms_resolve_the_same_spec() {
        let mut conn = db::open_in_memory();
        let (project_id, key, ids) = project_with_specs(&mut conn, "nyx", "/tmp/nyx", 3);
        let third = ids[2].clone();

        for token in [
            third.clone(),
            format!("{key}-3"),
            "#3".to_string(),
            "3".to_string(),
        ] {
            let resolved = resolve(&mut conn, Some(&project_id), &token).expect("resolve");
            let id = match resolved {
                SpecRefResolution::Canonical(id) => id,
                SpecRefResolution::Resolved(r) => r.spec_id,
                other => panic!("{token} did not resolve: {other:?}"),
            };
            assert_eq!(id, third, "form '{token}' resolved the wrong Spec");
        }
    }

    /// A canonical id is RECOGNISED as one and takes NO other path: [`resolve`] answers
    /// `Canonical` without a lookup, so an unknown-but-well-formed id comes back untouched
    /// (its refusal stays where it always was, downstream) instead of being re-read as a key or a
    /// number.
    #[test]
    fn a_canonical_id_is_recognised_and_takes_no_other_path() {
        let mut conn = db::open_in_memory();
        let (project_id, _, ids) = project_with_specs(&mut conn, "nyx", "/tmp/nyx", 1);

        assert!(matches!(parse(&ids[0]), Some(SpecRef::Uuid(_))));
        // Uppercase, exactly as the hook's regex tolerates.
        assert!(matches!(
            parse("019FF517-0000-7000-8000-0000000000AB"),
            Some(SpecRef::Uuid(_))
        ));
        // An id naming NO Spec is still an id — never a key, never a number.
        assert_eq!(
            resolve(
                &mut conn,
                Some(&project_id),
                "019ff517-0000-7000-8000-0000000000ab"
            )
            .expect("resolve"),
            SpecRefResolution::Canonical("019ff517-0000-7000-8000-0000000000ab".to_string())
        );
    }

    /// PROSE IS NOT A REFERENCE. A partially numeric token, a version-looking one, a decimal and a
    /// plain word are all left alone — the discipline that lets an agent forward the human's first
    /// word without risking a false anchor.
    #[test]
    fn a_partially_numeric_token_is_not_a_reference() {
        for token in [
            "3abc", "v3", "3.1", "abc", "", "#", "#3a", "-3", "3-", "nyx-", "nyx-3a", "#nyx-3",
            "3 4",
        ] {
            assert_eq!(
                parse(token),
                None,
                "'{token}' must not parse as a reference"
            );
        }
    }

    /// "NOT A REFERENCE" AND "A REFERENCE THAT DESIGNATES NOTHING" ARE TWO ANSWERS. `#99` in a
    /// project whose Specs stop at 3 is the second one — the distinction `run_start` needs to
    /// refuse a designated-but-empty reference while still letting prose through unchanged.
    #[test]
    fn a_reference_that_designates_nothing_is_not_prose() {
        let mut conn = db::open_in_memory();
        let (project_id, key, _) = project_with_specs(&mut conn, "nyx", "/tmp/nyx", 3);

        assert_eq!(
            resolve(&mut conn, Some(&project_id), "#99").expect("resolve"),
            SpecRefResolution::Unresolved(SpecRef::Number(99))
        );
        assert_eq!(
            resolve(&mut conn, Some(&project_id), &format!("{key}-99")).expect("resolve"),
            SpecRefResolution::Unresolved(SpecRef::Key {
                key: key.clone(),
                number: 99
            })
        );
        // A key no project carries: still a reference, still designating nothing.
        assert_eq!(
            resolve(&mut conn, Some(&project_id), "ACME-1").expect("resolve"),
            SpecRefResolution::Unresolved(SpecRef::Key {
                key: "ACME".to_string(),
                number: 1
            })
        );
        assert_eq!(
            resolve(&mut conn, Some(&project_id), "abc").expect("resolve"),
            SpecRefResolution::NotAReference
        );
    }

    /// THE KEY'S CASE IS THE HUMAN'S BUSINESS, not nyx's: `nyx-3` and `NYX-3` are one reference,
    /// exactly as an id in either case is one id.
    #[test]
    fn the_key_case_is_tolerated() {
        let mut conn = db::open_in_memory();
        let (project_id, key, ids) = project_with_specs(&mut conn, "nyx", "/tmp/nyx", 3);
        assert_eq!(key, "NYX");

        for token in ["nyx-3", "NYX-3", "Nyx-3"] {
            let resolved = resolve(&mut conn, Some(&project_id), token).expect("resolve");
            assert_eq!(
                resolved,
                SpecRefResolution::Resolved(ResolvedSpecRef {
                    spec_id: ids[2].clone(),
                    project_id: project_id.clone(),
                }),
                "'{token}' must resolve like the stored-case key"
            );
        }
    }

    /// PROJECT ISOLATION. Two projects each own a Spec #3; the reference resolves the one of the
    /// project it was GIVEN, never the other — the property that lets `#3` be typed at all without
    /// opening a cross-project door.
    #[test]
    fn the_same_number_in_two_projects_resolves_the_one_asked_for() {
        let mut conn = db::open_in_memory();
        let (alpha, _, alpha_specs) = project_with_specs(&mut conn, "alpha", "/tmp/alpha", 3);
        let (beta, _, beta_specs) = project_with_specs(&mut conn, "beta", "/tmp/beta", 3);
        assert_ne!(alpha_specs[2], beta_specs[2]);

        for (project_id, expected) in [(&alpha, &alpha_specs[2]), (&beta, &beta_specs[2])] {
            for token in ["3", "#3"] {
                assert_eq!(
                    resolve(&mut conn, Some(project_id), token).expect("resolve"),
                    SpecRefResolution::Resolved(ResolvedSpecRef {
                        spec_id: expected.clone(),
                        project_id: project_id.clone(),
                    })
                );
            }
        }
    }

    /// A `KEY-N` NAMING ANOTHER PROJECT RESOLVES IN **ITS** PROJECT — that is what the key means —
    /// and the answer SAYS SO: `project_id` comes back as the key's project, never as the one the
    /// caller asked in. That is the whole fact a caller needs; the DISAGREEMENT is then judged by
    /// the cross-project guard the caller already owns, on the resolved id, exactly as for a
    /// hand-typed id (`mcp_engine::tests::run_start_refuses_a_short_reference_from_a_project_that_
    /// disagrees_with_the_anchor` is where that verdict is locked).
    ///
    /// A `cross_project: bool` used to be computed here as a SECOND statement of the same
    /// disagreement, and no production caller ever read it (finding
    /// `01M0ZGFRXN01DG3H3NE7DZP4X0`) — it could not: the one entry point passes NO project for a
    /// `KEY-N`, so the flag was structurally `false` outside these tests. It is gone; the project
    /// the reference landed in is the fact, and it is asserted below.
    #[test]
    fn a_key_of_another_project_resolves_in_the_project_the_key_names() {
        let mut conn = db::open_in_memory();
        let (alpha, _, alpha_specs) = project_with_specs(&mut conn, "alpha", "/tmp/alpha", 3);
        let (beta, beta_key, beta_specs) = project_with_specs(&mut conn, "beta", "/tmp/beta", 3);
        assert_ne!(alpha_specs[2], beta_specs[2]);

        // Asked in ALPHA, `BETA-3` still names beta's Spec — and reports beta as its project, so
        // the caller's guard has the disagreement in hand.
        assert_eq!(
            resolve(&mut conn, Some(&alpha), &format!("{beta_key}-3")).expect("resolve"),
            SpecRefResolution::Resolved(ResolvedSpecRef {
                spec_id: beta_specs[2].clone(),
                project_id: beta.clone(),
            })
        );
        // Asked IN its own project, the very same key answers identically — there is nothing to
        // disagree about, and the resolution never depended on which project was asked.
        assert_eq!(
            resolve(&mut conn, Some(&beta), &format!("{beta_key}-3")).expect("resolve"),
            SpecRefResolution::Resolved(ResolvedSpecRef {
                spec_id: beta_specs[2].clone(),
                project_id: beta,
            })
        );
    }

    /// WITHOUT A PROJECT, only the two self-describing forms resolve. `#N` / `N` designate nothing
    /// rather than being hunted across the database — the cross-project leak this grammar exists
    /// to keep shut.
    #[test]
    fn without_a_project_only_the_self_describing_forms_resolve() {
        let mut conn = db::open_in_memory();
        let (project_id, key, ids) = project_with_specs(&mut conn, "nyx", "/tmp/nyx", 3);

        // `KEY-N` carries its own project: it resolves with no anchor, and nothing disagrees.
        assert_eq!(
            resolve(&mut conn, None, &format!("{key}-3")).expect("resolve"),
            SpecRefResolution::Resolved(ResolvedSpecRef {
                spec_id: ids[2].clone(),
                project_id: project_id.clone(),
            })
        );
        // A canonical id needs no project either.
        assert_eq!(
            resolve(&mut conn, None, &ids[0]).expect("resolve"),
            SpecRefResolution::Canonical(ids[0].clone())
        );
        // `#3` / `3` have no project to count in — a reference designating nothing, NOT prose.
        for token in ["3", "#3"] {
            assert_eq!(
                resolve(&mut conn, None, token).expect("resolve"),
                SpecRefResolution::Unresolved(SpecRef::Number(3))
            );
        }
    }

    /// THE SUGGESTION a refusal carries: the nearest LIVE Spec of the project, deterministic on a
    /// tie, and `None` for ONE reason only — the project holds none.
    ///
    /// THAT SINGLE MEANING IS THE POINT (finding `01M0D3SCY0MVFYS6KVD8AC40TD`): the callers turn
    /// `None` into the sentence "it holds no Spec yet", so a second way of producing `None` is a
    /// way of producing a LIE. A populated project answers `Some` for every number asked, however
    /// far off — asserted here, so no future short-circuit can slip back in.
    #[test]
    fn the_likely_candidate_is_the_nearest_live_spec() {
        let mut conn = db::open_in_memory();
        let (project_id, _, ids) = project_with_specs(&mut conn, "nyx", "/tmp/nyx", 3);

        let near = likely_candidate(&mut conn, &project_id, 99)
            .expect("candidate")
            .expect("a project with Specs has one");
        assert_eq!(near.id, ids[2]);
        for wanted in [-1, 0, 1, 2, 3, 4, i64::from(i32::MAX) + 1] {
            assert!(
                likely_candidate(&mut conn, &project_id, wanted)
                    .expect("candidate")
                    .is_some(),
                "a project holding Specs never answers 'no Spec' — asked for {wanted}"
            );
        }

        let (empty, _, _) = project_with_specs(&mut conn, "empty", "/tmp/empty", 0);
        assert!(likely_candidate(&mut conn, &empty, 1)
            .expect("candidate")
            .is_none());
    }
}
