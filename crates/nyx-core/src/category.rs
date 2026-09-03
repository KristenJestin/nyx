//! The CATEGORY service — the single authority over the "genre of work" axis a Spec carries
//! (PDR-0017), sitting ABOVE the [`crate::db`] `categories` CRUD.
//!
//! ## Why this module exists rather than a handful of `db` calls
//!
//! Three invariants have no home in the schema, and each of them has a way of being forgotten by
//! whichever surface writes next (MCP tool, sidecar command, engine node, UI):
//!
//!   * **SAME-PROJECT.** `specs.category_id` is a plain FK: SQLite can check that the id names an
//!     existing category, never that the category belongs to the SAME project as the Spec. A
//!     cross-project assignment would give a Spec a chip its own project cannot draw and a slug its
//!     own project's vocabulary does not contain. PDR-0017 puts that refusal in the domain service;
//!     this is it, and [`assign_spec_category`] is the ONLY writer of the column.
//!   * **THE SLUG IS THE IDENTITY, AND IT IS IMMUTABLE.** The harness reads `bug` to decide which
//!     discipline a Spec is subject to. So a slug is minted ONCE, never rewritten ([`rename`]
//!     touches `name` alone), and a NEW slug may never land on one of the four
//!     [`crate::db::SYSTEM_CATEGORY_SLUGS`] — an agent-minted category that inherited `bug` would
//!     inherit `bug`'s execution constraint, imposing a red-before-green gate on work nobody
//!     classified as a bug.
//!   * **CREATION IS NEVER BLOCKING.** PDR-0017's governance is a grill, not a lock: the agent
//!     assigns from the existing set and, when nothing fits, MINTS one (`created_by_type = agent`).
//!     So no path here refuses because "the category does not exist" — an unknown label is a
//!     creation, not an error. Per the 2026-09-02 amendment of PDR-0017 (see the PDR's dated note),
//!     that mint is VALID AS IT STANDS: there is no proposed/confirmed axis any more, and the agent
//!     needs nobody's blessing. What a human curates is the VOCABULARY — renaming, creating and
//!     deleting entries from the project settings — not each individual mint.
//!
//! ## The two kinds of reference, deliberately not one type
//!
//! [`CategoryRef::Id`] is the EXACT row — what a UI selector and a sidecar command send, having
//! just read the list. [`CategoryRef::Label`] is the ERGONOMIC reference an agent types: a slug it
//! read from a workflow, or the human name it saw on a chip. They are resolved by different rules
//! and they fail in different ways (an unknown id is a mistake; an unknown label is a creation),
//! which is exactly why they are two variants and not one string the resolver has to guess about.
//!
//! Label resolution is DETERMINISTIC and ordered: **exact slug first, normalized name second**. The
//! slug is unique per project, so its match is always a single row; a name is NOT unique (a rename
//! may freely produce a homonym — PDR-0017), so two categories answering the same normalized name
//! is an [`CategoryError::AmbiguousName`] refusal, never a coin flip.

use diesel::prelude::*;
use diesel::result::QueryResult;
use diesel::SqliteConnection;

use crate::db::{self, Category, Spec};

/// The stub a name that carries no ASCII-slug-able character falls back to (`"🐛"`, `"!!!"`).
/// Collision-suffixed like any other candidate, so a project can hold several.
const SLUG_FALLBACK: &str = "category";

/// The maximum length of a minted slug, before any collision suffix. A slug is an identity read by
/// humans in prompts and logs; an unbounded one is a paragraph pretending to be a key.
const SLUG_MAX_LEN: usize = 48;

/// How a caller names a category.
///
/// See the module docs: the two variants exist because they resolve by different rules and fail in
/// different ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryRef<'a> {
    /// An exact `categories.id` — what a UI selector or a sidecar command sends.
    Id(&'a str),
    /// A slug or a human name — what an agent types. Resolved slug-first, then by normalized name.
    Label(&'a str),
}

/// Why a category operation was refused (or the DB failed). Mirrors the shape of
/// [`crate::db::SetSpecWorkspaceError`] so a UI/MCP caller can react precisely instead of parsing a
/// string.
#[derive(Debug)]
pub enum CategoryError {
    /// No Spec resolves to the given id.
    SpecNotFound,
    /// No category resolves to the given reference. Raised for an unknown ID (a mistake) — an
    /// unknown LABEL is a creation, not this (see [`resolve_or_create`]).
    NotFound,
    /// THE SAME-PROJECT GUARD: the category exists but belongs to ANOTHER project than the Spec.
    /// Carries both sides so the refusal can name them.
    ProjectMismatch {
        spec_project_id: String,
        category_project_id: String,
    },
    /// A [`CategoryRef::Label`] matched the NAME of two or more categories of the project. Names
    /// are free to collide (PDR-0017 allows a rename into a homonym), so the resolver refuses
    /// rather than picking one. Carries the slugs of everything that matched, in list order — the
    /// caller re-asks with one of them, which is unique by construction.
    AmbiguousName { name: String, matches: Vec<String> },
    /// A blank (or whitespace-only) name was given to a creation or a rename. Not a slug problem —
    /// a name that slugifies to nothing is fine and falls back to [`SLUG_FALLBACK`] — but a chip
    /// with no label at all, which is the one thing the UI cannot draw.
    BlankName,
    /// A DELETION was asked on one of the four [`crate::db::SYSTEM_CATEGORY_SLUGS`]. Those are
    /// nyx's OWN vocabulary and the slug is what carries the discipline (`bug` decides the
    /// red-before-green gate), so removing one would silently disarm a rule for every Spec that
    /// carried it. Carries the slug so the refusal can name it.
    SystemCategory { slug: String },
    /// A DELETION was asked on a category `count` Specs still carry. Deleting it would RELEASE
    /// them (`ON DELETE SET NULL`) — a silent mass-declassification — so the service refuses and
    /// names the number: the human re-classifies (or un-classifies) them first, deliberately.
    CategoryInUse { name: String, count: i64 },
    /// A DB error surfaced while resolving or writing.
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for CategoryError {
    fn from(e: diesel::result::Error) -> Self {
        CategoryError::Db(e)
    }
}

impl std::fmt::Display for CategoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CategoryError::SpecNotFound => write!(f, "no such Spec"),
            CategoryError::NotFound => write!(f, "no such category"),
            CategoryError::ProjectMismatch {
                spec_project_id,
                category_project_id,
            } => write!(
                f,
                "category belongs to project {category_project_id}, the Spec to project \
                 {spec_project_id} — a Spec may only carry a category of its OWN project"
            ),
            CategoryError::AmbiguousName { name, matches } => write!(
                f,
                "'{name}' names {} categories of this project ({}) — re-ask by slug, which is \
                 unique",
                matches.len(),
                matches.join(", ")
            ),
            CategoryError::BlankName => write!(f, "a category name cannot be blank"),
            CategoryError::SystemCategory { slug } => write!(
                f,
                "'{slug}' is one of nyx's system categories — it may be renamed, never deleted: \
                 the slug is what the agent harness reads to decide a Spec's discipline"
            ),
            CategoryError::CategoryInUse { name, count } => write!(
                f,
                "'{name}' is still carried by {count} Spec{} — re-classify {} before deleting the \
                 category",
                if *count > 1 { "s" } else { "" },
                if *count > 1 { "them" } else { "it" }
            ),
            CategoryError::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for CategoryError {}

// --- Slugs ---------------------------------------------------------------

/// The pure part of slug minting: `name` folded to ASCII lowercase with hyphen separators, or
/// `None` when nothing survives (`"🐛"`, `"!!!"`, `"   "`).
///
/// Latin-1/Latin-Extended-A letters are TRANSLITERATED rather than dropped (`Réfacto` → `refacto`,
/// not `r facto`): a French or German name is the normal case in this codebase, and dropping its
/// accented letters would mint slugs nobody can read back. Everything else non-alphanumeric — an
/// emoji, a CJK glyph, punctuation — becomes a separator; runs collapse, and the ends are trimmed.
/// The result is capped at [`SLUG_MAX_LEN`] on a separator boundary so the cap never splits a word
/// in half.
///
/// PURE by design: no connection, no project — the collision and reservation rules live in
/// [`slugify_for`], which is the one that needs a database.
pub fn slugify(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if let Some(folded) = fold_latin(ch) {
            out.push_str(folded);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    let capped = if trimmed.len() <= SLUG_MAX_LEN {
        trimmed
    } else {
        let cut = trimmed[..SLUG_MAX_LEN]
            .rfind('-')
            .filter(|at| *at > 0)
            .unwrap_or(SLUG_MAX_LEN);
        trimmed[..cut].trim_matches('-')
    };
    if capped.is_empty() {
        None
    } else {
        Some(capped.to_string())
    }
}

/// Transliterate ONE accented Latin letter to its ASCII skeleton, or `None` when the character has
/// no ASCII reading (an emoji, a CJK glyph, punctuation). Covers Latin-1 Supplement and the common
/// Latin Extended-A letters — enough for the French / German / Spanish names this product is
/// written and used in, and deliberately not a full Unicode normalizer (a dependency to fold
/// `é` would be a poor trade).
fn fold_latin(ch: char) -> Option<&'static str> {
    Some(match ch.to_lowercase().next().unwrap_or(ch) {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ğ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' => "i",
        'ĺ' | 'ľ' | 'ł' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ř' => "r",
        'ś' | 'š' | 'ş' => "s",
        'ß' => "ss",
        'ť' | 'ţ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// Mint the slug a NEW category of `project_id` will carry, from its human `name`.
///
/// Three rules, in order:
///
/// 1. the ASCII skeleton of the name ([`slugify`]), or [`SLUG_FALLBACK`] when the name carries no
///    slug-able character — creation is never blocking (PDR-0017), so an emoji-only name gets a
///    slug rather than an error;
/// 2. a candidate equal to one of the four [`crate::db::SYSTEM_CATEGORY_SLUGS`] is REFUSED
///    OUTRIGHT, whether or not that system category currently exists in the project. This is the
///    load-bearing rule: the discipline is read off the slug, so `bug` must be reachable only
///    through the seeded row. A project whose `bug` was removed must NOT let the next proposal
///    inherit the name;
/// 3. a candidate already taken inside the project is suffixed `-2`, `-3`, … until it is free.
///
/// Rules 2 and 3 share the suffixing, so `Bug` in a project that still has `bug` gives `bug-2`, and
/// so does `Bug` in a project where `bug` was deleted.
pub fn slugify_for(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
) -> QueryResult<String> {
    let stub = slugify(name).unwrap_or_else(|| SLUG_FALLBACK.to_string());
    let reserved = db::SYSTEM_CATEGORY_SLUGS.contains(&stub.as_str());
    if !reserved && db::find_category_by_slug(conn, project_id, &stub)?.is_none() {
        return Ok(stub);
    }
    let mut n = 2;
    loop {
        let candidate = format!("{stub}-{n}");
        // A suffixed candidate can never BE a system slug (none of the four ends in `-<digits>`),
        // so only the taken-in-project test remains.
        if db::find_category_by_slug(conn, project_id, &candidate)?.is_none() {
            return Ok(candidate);
        }
        n += 1;
    }
}

/// Fold a human NAME to its comparison form: trimmed, lowercased, inner whitespace collapsed. The
/// key label resolution compares on — so `"  Tech   Debt "` finds `"Tech Debt"` — and nothing else:
/// it is NOT a slug (it keeps accents and spaces), because two names that differ only by an accent
/// are two different labels a human chose.
fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

// --- Resolution ----------------------------------------------------------

/// Resolve a [`CategoryRef`] inside `project_id`, or [`CategoryError::NotFound`].
///
/// An [`CategoryRef::Id`] that resolves to a category of ANOTHER project is [`CategoryError::
/// NotFound`] here and not a mismatch: the caller asked "which category of THIS project is this",
/// and the answer is none. The mismatch refusal belongs to [`assign_spec_category`], which is where
/// two projects are actually being confronted.
///
/// A [`CategoryRef::Label`] is matched EXACT SLUG FIRST, then normalized name — see the module
/// docs for why the order is fixed and why a name tie is refused rather than broken.
pub fn resolve(
    conn: &mut SqliteConnection,
    project_id: &str,
    reference: CategoryRef<'_>,
) -> Result<Category, CategoryError> {
    match reference {
        CategoryRef::Id(id) => match db::get_category(conn, id)? {
            Some(category) if category.project_id == project_id => Ok(category),
            _ => Err(CategoryError::NotFound),
        },
        CategoryRef::Label(label) => {
            let label = label.trim();
            if label.is_empty() {
                return Err(CategoryError::NotFound);
            }
            if let Some(category) = db::find_category_by_slug(conn, project_id, label)? {
                return Ok(category);
            }
            let wanted = normalize_name(label);
            let matches: Vec<Category> = db::list_categories(conn, project_id)?
                .into_iter()
                .filter(|c| normalize_name(&c.name) == wanted)
                .collect();
            match matches.len() {
                0 => Err(CategoryError::NotFound),
                1 => Ok(matches.into_iter().next().expect("len checked")),
                _ => Err(CategoryError::AmbiguousName {
                    name: label.to_string(),
                    matches: matches.into_iter().map(|c| c.slug).collect(),
                }),
            }
        }
    }
}

/// The slugs of the OTHER categories of `project_id` whose NAME is the same name as `name` —
/// [`normalize_name`]'s notion of "the same", which is the only one there is. `except_id` is the row
/// being asked ABOUT (a rename asks about itself) and is never part of the answer; pass `None` to
/// ask about a name no row holds yet.
///
/// # Why this is not [`resolve`]
///
/// [`resolve`] answers "which category does this label designate", and its first rule is EXACT SLUG
/// FIRST — deliberately, so `bug` always means the seeded `bug` row whatever anybody renamed. That
/// makes it the wrong instrument for the homonymy question, and wrong in a way that only shows on
/// the case that matters: renaming a category to a string that is another row's SLUG (`chore`,
/// lower-case) short-circuits on the slug lookup, returns `Ok` of that other row, and reports NO
/// conflict — while two rows now normalise to the same name, the renamed one has become unreachable
/// by its own name, and every later `resolve` of it silently lands on the system row. Spelled
/// `Chore` the same gesture reported the conflict correctly, which is the tell.
///
/// So the homonymy probe compares NAMES and nothing else. Empty answer = the name is unique.
///
/// `pub` because the SIDECAR's `category_rename` is what surfaces the conflict to the human, and
/// [`normalize_name`] is private to this module ON PURPOSE — "when are two names the same name"
/// (case, accents, inner whitespace) is a rule that must not be re-implemented in TypeScript.
pub fn homonym_slugs(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    except_id: Option<&str>,
) -> Result<Vec<String>, CategoryError> {
    let wanted = normalize_name(name);
    Ok(db::list_categories(conn, project_id)?
        .into_iter()
        .filter(|c| Some(c.id.as_str()) != except_id && normalize_name(&c.name) == wanted)
        .map(|c| c.slug)
        .collect())
}

/// Resolve `label` inside `project_id` and, when nothing answers it, MINT a category for it.
/// Returns the category and whether it was created.
///
/// This is PDR-0017's "creation is a non-blocking fallback" in one call: the agent names what it
/// wants, gets the existing row when there is one, and gets a new `created_by_type = agent` row
/// when there is not — never a refusal for "unknown category". The two refusals that remain are
/// real ones: a BLANK name (nothing to label the chip with) and an AMBIGUOUS one (two homonyms —
/// creating a third would make it worse).
pub fn resolve_or_create(
    conn: &mut SqliteConnection,
    project_id: &str,
    label: &str,
) -> Result<(Category, bool), CategoryError> {
    if label.trim().is_empty() {
        return Err(CategoryError::BlankName);
    }
    match resolve(conn, project_id, CategoryRef::Label(label)) {
        Ok(category) => Ok((category, false)),
        Err(CategoryError::NotFound) => create_by_agent(conn, project_id, label).map(|c| (c, true)),
        Err(other) => Err(other),
    }
}

/// Mint a category FOR AN AGENT (`created_by_type = agent`) — the on-the-fly creation PDR-0017
/// grants it when the project's vocabulary holds nothing that fits.
///
/// **The mint is VALID AS IT STANDS** (PDR-0017, amended 2026-09-02). There is no proposed /
/// confirmed axis: the agent has full rights over the classification, and `spec_set_category` IS
/// the validation. `confirmed_at` is therefore stamped at the insert like every other creation
/// path — see [`create_by_human`] for the other one.
///
/// The slug comes from [`slugify_for`] (collision-safe, and never a system slug); the position is
/// appended after the project's current list and the colour is derived deterministically from the
/// slug ([`crate::db::category_color_token`]), so nothing here asks the caller to choose a
/// presentation detail it has no opinion about.
///
/// What separates this constructor from [`create_by_human`] is the PROVENANCE, and only that:
/// `created_by_type = agent` on the row, [`db::DomainEventSource::Mcp`] on the event. The two
/// facts stay two facts in `domain_events` — which is the trace PDR-0017's governance reads, now
/// that the curation state no longer exists.
pub fn create_by_agent(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
) -> Result<Category, CategoryError> {
    create(
        conn,
        project_id,
        name,
        db::CATEGORY_CREATED_BY_AGENT,
        db::DomainEventSource::Mcp,
    )
}

/// Mint a category FOR A HUMAN (`created_by_type = human`) — the "manage the vocabulary" gesture
/// the project settings expose (PDR-0017 amended 2026-09-02: the human curates the SET of
/// categories, the agent classifies inside it).
///
/// Same rules as [`create_by_agent`] — the slug is minted by [`slugify_for`], so it is
/// collision-safe and can never land on one of the four [`db::SYSTEM_CATEGORY_SLUGS`], whoever
/// asks. Only the provenance differs: `created_by_type = human` and a `category.created` event
/// filed [`db::DomainEventSource::Ui`].
pub fn create_by_human(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
) -> Result<Category, CategoryError> {
    create(
        conn,
        project_id,
        name,
        db::CATEGORY_CREATED_BY_HUMAN,
        db::DomainEventSource::Ui,
    )
}

/// The ONE creation body the two public constructors share, so the slug rule, the blank refusal,
/// the `confirmed_at` stamp and the event are written once and cannot diverge by provenance.
///
/// Private on purpose: `created_by_type` and the event `source` are NOT free parameters — they
/// travel in pairs (`agent`/`mcp`, `human`/`ui`), and a public function taking both would let a
/// caller file a human's creation as the agent's, which is exactly the trace PDR-0017 depends on.
fn create(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    created_by_type: &str,
    source: db::DomainEventSource,
) -> Result<Category, CategoryError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CategoryError::BlankName);
    }
    conn.transaction(|conn| {
        let slug = slugify_for(conn, project_id, name)?;
        // `confirmed_at` IS ALWAYS SET, on every path, since the 2026-09-02 amendment retired the
        // proposed/confirmed axis. The column is kept rather than migrated away — dropping it
        // would cost a table rebuild for a value nothing reads any more. FUTURE CLEANUP: drop
        // `categories.confirmed_at` (and its `db::Category` field) the next time this table is
        // migrated for another reason.
        let category = db::insert_category(
            conn,
            project_id,
            &slug,
            name,
            created_by_type,
            Some(db::now_millis()),
        )?;
        db::record_category_event(
            conn,
            "category.created",
            &category,
            source,
            serde_json::json!({ "created_by_type": created_by_type }),
        )?;
        Ok(category)
    })
}

// --- Assignment ----------------------------------------------------------

/// Assign `reference` to the Spec, or REMOVE its category when `reference` is `None`. Returns the
/// Spec as it now stands.
///
/// THE SAME-PROJECT GUARD LIVES HERE, and this is the only writer of `specs.category_id`: the
/// category is resolved inside the SPEC'S OWN project, so a category of another project is
/// [`CategoryError::ProjectMismatch`] — named on both sides — and never a silent write. The whole
/// thing runs in ONE transaction with its outbox event, so a refusal leaves nothing behind and a
/// success is never observed without its `spec.category_set`.
///
/// The category is NOT frozen by the Spec's lifecycle: unlike a prose section, a classification is
/// metadata and stays correctable at any status.
///
/// `source` is the PROVENANCE of the gesture, and it is the caller's to state — the one place in
/// this module where it is not decided here. Being the only writer of the column, this function is
/// reached by BOTH halves of PDR-0017: the agent classifying through the MCP surface
/// ([`db::DomainEventSource::Mcp`]) and the human assigning or clearing the chip from the Spec
/// detail screen ([`db::DomainEventSource::Ui`]). A constant would make the second
/// indistinguishable from the first in `domain_events`, which is the exact trace the "the human
/// curates after" half of that PDR depends on. (The human-only gestures — [`create_by_human`],
/// [`rename`], [`delete`] — pin their provenance at the call site instead; see the section comment
/// below.)
pub fn assign_spec_category(
    conn: &mut SqliteConnection,
    spec_id: &str,
    reference: Option<CategoryRef<'_>>,
    source: db::DomainEventSource,
) -> Result<Spec, CategoryError> {
    conn.transaction(|conn| {
        let spec = db::get_spec(conn, spec_id)?.ok_or(CategoryError::SpecNotFound)?;
        let category = match reference {
            None => None,
            Some(CategoryRef::Id(id)) => {
                // Resolved GLOBALLY on purpose: an id that exists in another project must be
                // refused as a MISMATCH (naming both projects), not reported as "no such
                // category" — the caller needs to learn which of the two facts is wrong.
                let category = db::get_category(conn, id)?.ok_or(CategoryError::NotFound)?;
                if category.project_id != spec.project_id {
                    return Err(CategoryError::ProjectMismatch {
                        spec_project_id: spec.project_id.clone(),
                        category_project_id: category.project_id,
                    });
                }
                Some(category)
            }
            Some(label @ CategoryRef::Label(_)) => {
                // A label is resolved INSIDE the Spec's project, so a cross-project assignment is
                // not merely refused here — it is unreachable by construction.
                Some(resolve(conn, &spec.project_id, label)?)
            }
        };
        db::set_spec_category_id(conn, spec_id, category.as_ref().map(|c| c.id.as_str()))?;
        db::record_spec_category_event(conn, &spec.project_id, spec_id, category.as_ref(), source)?;
        db::get_spec(conn, spec_id)?.ok_or(CategoryError::SpecNotFound)
    })
}

/// Assign the category named by `label` to the Spec, MINTING it when the project has nothing that
/// answers (PDR-0017's non-blocking fallback). Returns the Spec, the category, and whether the
/// category was created.
///
/// The ergonomic entry point for the agent surfaces (`spec_create(category=…)`, `spec_set_category`
/// and the engine's `set_category` effect): they know a word, not a row. Cross-project is
/// impossible here by construction — the label is resolved, and any creation happens, INSIDE the
/// Spec's own project.
///
/// `source` is carried through to the `spec.category_set` event of [`assign_spec_category`] — see
/// there for why the assignment's provenance belongs to the caller. Note that a category MINTED
/// along the way keeps its own `category.created` provenance ([`create_by_agent`], always `Mcp`):
/// minting is the agent's gesture whatever surface asked for the assignment.
pub fn set_spec_category_by_label(
    conn: &mut SqliteConnection,
    spec_id: &str,
    label: &str,
    source: db::DomainEventSource,
) -> Result<(Spec, Category, bool), CategoryError> {
    conn.transaction(|conn| {
        let spec = db::get_spec(conn, spec_id)?.ok_or(CategoryError::SpecNotFound)?;
        let (category, created) = resolve_or_create(conn, &spec.project_id, label)?;
        let spec =
            assign_spec_category(conn, spec_id, Some(CategoryRef::Id(&category.id)), source)?;
        Ok((spec, category, created))
    })
}

/// CREATE a Spec and CLASSIFY it in ONE transaction — the authoring surface's
/// `spec_create(category=…)`. Returns the Spec, its first revision, and the category it carries
/// (`None` when `category` was not given).
///
/// **Why this composition lives here and not in the MCP adapter.** `category` is optional on
/// `spec_create`, so the naive adapter is "create the Spec, then assign" — two commits, and the
/// window between them is a real state: a failure in the second leaves a Spec that was ASKED to be
/// a `bug` sitting UNCATEGORIZED, and a category MINTED for it with nothing pointing at it. Under
/// this PRD that half-write is not cosmetic — an uncategorized Spec is exactly what escapes the
/// red-before-green discipline, and the minted category is an entry left in the project's
/// vocabulary for a Spec that never took it. Wrapping both in the caller's transaction makes the pair
/// ATOMIC: an error anywhere — the mint, the assignment, or anything the caller does afterwards
/// inside the same transaction — rolls back the category AND the Spec together.
///
/// The label follows [`set_spec_category_by_label`]: existing slug or name first, and a MINT when
/// the project has nothing that answers (PDR-0017's non-blocking creation).
///
/// `None` — the argument OMITTED — is the one way to say "create the Spec unclassified". A
/// `Some(label)` that is BLANK once trimmed is [`CategoryError::BlankName`] and takes the Spec down
/// with it: the caller asked for a classification and did not get one, so it does not get the Spec
/// either. That guard used to be unreachable for the EMPTY-string spelling, because the MCP adapter
/// folded `""` into `None` before this was ever called — so `category: ""` created an unclassified
/// Spec in silence while `category: " "` destroyed the whole call. The adapter now refuses both
/// identically ([`crate::mcp_tools_spec::spec_create`], through the same `anchor_str` its sibling
/// verb uses), so ABSENCE is the only spelling of "unclassified" that reaches here.
pub fn create_spec_with_category(
    conn: &mut SqliteConnection,
    project_id: &str,
    title: &str,
    sections: db::SpecSections,
    category: Option<&str>,
    source: db::DomainEventSource,
) -> Result<(Spec, db::SpecRevision, Option<Category>), CategoryError> {
    conn.transaction(|conn| {
        let (spec, revision) = db::create_spec(conn, project_id, title, sections)?;
        let Some(label) = category else {
            return Ok((spec, revision, None));
        };
        let (spec, category, _created) = set_spec_category_by_label(conn, &spec.id, label, source)?;
        Ok((spec, revision, Some(category)))
    })
}

// --- Human curation ------------------------------------------------------
//
// The gestures PDR-0017 reserves for a human, and the reason their events are filed with
// [`db::DomainEventSource::Ui`] rather than the `Mcp` of [`create_by_agent`]: `created_by_type`
// records who minted the ROW, and nothing else in `domain_events` would say who took the DECISION.
//
// **THE 2026-09-02 AMENDMENT.** `confirm` used to live here — the human's "yes, keep this one" on
// an agent's proposal. It is gone, with the whole proposed/confirmed axis: the agent has full
// rights, and `spec_set_category` IS the validation. What a human curates is the VOCABULARY
// itself — create, rename, delete, from the project settings — not each mint one by one.
//
// **Why these FREEZE the provenance while [`assign_spec_category`] takes it as a parameter.**
// Renaming (and creating or deleting from the settings) is human BY DEFINITION — the agent surface
// has no rename and no delete, in this PRD's phase 2 (`spec_create(category)`,
// `spec_set_category`, `spec_category`) or anywhere after it — so `Ui` is a property of the
// gesture, not of the caller, and hard-coding it here means the sidecar command cannot get it
// wrong. ASSIGNING is the opposite: the same call is reached by an agent (phase 2, MCP) and by a
// human (phase 8, the Spec detail screen's assign / unassign), so its provenance belongs to the
// caller and must travel as an argument.

/// RENAME a category — the human curation gesture on a label. Returns the category as it now
/// stands.
///
/// **The slug is NOT touched, and that is the point.** A rename changes the label a human reads; it
/// must never change the identity a workflow reads, or renaming "Bug" to "Défaut" would silently
/// take the Spec out of the red-before-green discipline. `db::update_category_name` does not even
/// accept a slug parameter, so there is no path through which a rename could rewrite one.
///
/// A rename INTO a homonym of another category is ALLOWED (PDR-0017 — names are labels, and merge
/// is a separate gesture). The cost is that the resulting pair can no longer be told apart by name,
/// which [`resolve`] reports as [`CategoryError::AmbiguousName`] rather than guessing.
pub fn rename(
    conn: &mut SqliteConnection,
    project_id: &str,
    reference: CategoryRef<'_>,
    name: &str,
) -> Result<Category, CategoryError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CategoryError::BlankName);
    }
    conn.transaction(|conn| {
        let category = resolve(conn, project_id, reference)?;
        let previous = category.name.clone();
        db::update_category_name(conn, &category.id, name)?;
        let category = db::get_category(conn, &category.id)?.ok_or(CategoryError::NotFound)?;
        db::record_category_event(
            conn,
            "category.renamed",
            &category,
            db::DomainEventSource::Ui,
            serde_json::json!({ "previous_name": previous }),
        )?;
        Ok(category)
    })
}

/// DELETE a category from the project's vocabulary — the last of the human's curation gestures
/// (PDR-0017 amended 2026-09-02: the human owns the SET, the agent classifies inside it). Returns
/// the deleted category, for the message the caller shows.
///
/// **THE TWO REFUSALS, AND THEY ARE PRONOUNCED HERE AND ONLY HERE.** They are invariants of the
/// domain, not of a screen — an app that hides a button is a courtesy, never a guarantee, and the
/// sidecar command is reachable without that screen:
///
///   1. A SYSTEM CATEGORY may never be deleted ([`CategoryError::SystemCategory`]). The four
///      seeded slugs are nyx's own vocabulary and the slug is what carries the discipline: the
///      harness reads `bug` to decide whether a Spec owes a reproduction. Deleting `bug` would
///      release every Spec that carried it (`ON DELETE SET NULL`) AND make the discipline
///      unreachable for the project — with the seed's idempotence quietly recreating the row on
///      the next boot, so even the removal would not stick. Renaming stays allowed: the LABEL is
///      the human's, the identity is not.
///   2. A CATEGORY A SPEC STILL CARRIES may not be deleted ([`CategoryError::CategoryInUse`], which
///      NAMES THE COUNT). The FK is `ON DELETE SET NULL`, so a deletion would not fail — it would
///      silently un-classify N Specs, which is precisely the kind of mass edit nobody asked for.
///      The count is read INSIDE the transaction, so it cannot be stale by the time it refuses.
///
/// The whole thing runs in ONE transaction with its `category.deleted` event, filed
/// [`db::DomainEventSource::Ui`] — deleting is a human gesture by definition, and the agent
/// surface has none.
pub fn delete(
    conn: &mut SqliteConnection,
    project_id: &str,
    reference: CategoryRef<'_>,
) -> Result<Category, CategoryError> {
    conn.transaction(|conn| {
        let category = resolve(conn, project_id, reference)?;
        if db::SYSTEM_CATEGORY_SLUGS.contains(&category.slug.as_str()) {
            return Err(CategoryError::SystemCategory {
                slug: category.slug.clone(),
            });
        }
        let count = db::count_specs_with_category(conn, &category.id)?;
        if count > 0 {
            return Err(CategoryError::CategoryInUse {
                name: category.name.clone(),
                count,
            });
        }
        // The event is recorded BEFORE the row goes: `record_category_event` reads the category to
        // build its payload, and a deleted row has nothing left to read.
        db::record_category_event(
            conn,
            "category.deleted",
            &category,
            db::DomainEventSource::Ui,
            serde_json::json!({ "created_by_type": category.created_by_type }),
        )?;
        db::delete_category(conn, &category.id)?;
        Ok(category)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        open_in_memory, DomainEventSource, CATEGORY_CREATED_BY_AGENT, CATEGORY_CREATED_BY_HUMAN,
        CATEGORY_CREATED_BY_SYSTEM, SYSTEM_CATEGORY_SLUGS,
    };

    /// A project plus its seeded four categories (the state `create_project` leaves).
    fn project(conn: &mut SqliteConnection, name: &str) -> String {
        let (project, _) = db::create_project(conn, name, &format!("/tmp/{name}"), None)
            .expect("create project");
        project.id
    }

    fn spec(conn: &mut SqliteConnection, project_id: &str, title: &str) -> Spec {
        let (spec, _) = db::create_spec(conn, project_id, title, db::SpecSections::default())
            .expect("create spec");
        spec
    }

    #[test]
    fn slugify_folds_accents_hyphenates_and_gives_up_on_a_name_with_no_ascii_reading() {
        assert_eq!(slugify("Bug").as_deref(), Some("bug"));
        assert_eq!(slugify("Tech Debt").as_deref(), Some("tech-debt"));
        // Accents are TRANSLITERATED, not dropped — a French name stays readable.
        assert_eq!(slugify("Réfacto Général").as_deref(), Some("refacto-general"));
        assert_eq!(slugify("Größe").as_deref(), Some("grosse"));
        // Runs of separators collapse and the ends are trimmed.
        assert_eq!(slugify("  --Perf //  tuning-- ").as_deref(), Some("perf-tuning"));
        // Nothing ASCII-readable survives: the pure function says so, and the minting rule
        // (`slugify_for`) is what turns that into the fallback stub.
        assert_eq!(slugify("🐛"), None);
        assert_eq!(slugify("!!!"), None);
        assert_eq!(slugify("   "), None);
        // The cap lands on a separator, so it never splits a word in half.
        let long = slugify(&"segment ".repeat(20)).expect("a slug");
        assert!(long.len() <= SLUG_MAX_LEN, "got {long}");
        assert!(long.ends_with("segment"), "got {long}");
    }

    #[test]
    fn a_minted_slug_is_collision_suffixed_and_never_lands_on_a_system_slug() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");

        // The seeded four are present, so `Bug` cannot take `bug`…
        assert_eq!(
            slugify_for(&mut conn, &project_id, "Bug").expect("mint"),
            "bug-2"
        );

        // …and it STILL cannot once the seeded `bug` is gone. This is the load-bearing half: the
        // discipline is read off the slug, so `bug` must be reachable only through the seeded row.
        let seeded_bug = db::find_category_by_slug(&mut conn, &project_id, "bug")
            .expect("read")
            .expect("seeded");
        diesel::sql_query("DELETE FROM categories WHERE id = ?")
            .bind::<diesel::sql_types::Text, _>(&seeded_bug.id)
            .execute(&mut conn)
            .expect("delete the seeded bug");
        assert_eq!(
            slugify_for(&mut conn, &project_id, "bug").expect("mint"),
            "bug-2",
            "a system slug is reserved whether or not its category currently exists"
        );
        for reserved in SYSTEM_CATEGORY_SLUGS {
            assert_ne!(
                slugify_for(&mut conn, &project_id, reserved).expect("mint"),
                reserved
            );
        }

        // A non-system collision suffixes and keeps counting.
        create_by_agent(&mut conn, &project_id, "Perf").expect("first");
        create_by_agent(&mut conn, &project_id, "Perf").expect("second");
        let third = create_by_agent(&mut conn, &project_id, "perf").expect("third");
        assert_eq!(third.slug, "perf-3");

        // An emoji-only name is NEVER a refusal (PDR-0017): it falls back to the stub.
        let emoji = create_by_agent(&mut conn, &project_id, "🐛🐛").expect("emoji name");
        assert_eq!(emoji.slug, "category");
        assert_eq!(emoji.name, "🐛🐛", "the human label keeps the emoji");
    }

    /// A minted category appends its position, derives its colour — and IS VALID AS IT STANDS
    /// (PDR-0017 amended 2026-09-02): there is no proposed state left, whoever minted it. The two
    /// constructors differ by PROVENANCE alone, which is what the last two assertions hold.
    #[test]
    fn a_minted_category_appends_its_position_derives_its_colour_and_is_born_valid() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");

        let first = create_by_agent(&mut conn, &project_id, "Perf").expect("mint");
        let second = create_by_agent(&mut conn, &project_id, "Docs").expect("mint");
        assert_eq!(first.position, 4, "appended after the seeded four (0..3)");
        assert_eq!(second.position, 5);
        assert_eq!(first.created_by_type, CATEGORY_CREATED_BY_AGENT);
        assert!(
            first.confirmed_at.is_some(),
            "an agent's category needs nobody's blessing — it is born VALID"
        );
        assert_eq!(
            first.color_token.as_deref(),
            Some(db::category_color_token("perf")),
            "the colour is DERIVED from the slug, deterministically"
        );

        // THE HUMAN CONSTRUCTOR: the same rules, another provenance — and the row is equally
        // valid, which is now the only possible state.
        let human = create_by_human(&mut conn, &project_id, "Dette technique").expect("mint");
        assert_eq!(human.created_by_type, CATEGORY_CREATED_BY_HUMAN);
        assert!(human.confirmed_at.is_some());
        assert_eq!(human.slug, "dette-technique");

        // The provenance travels to `domain_events`, and it is the ONLY thing telling the two
        // creations apart now that the curation state is gone.
        let sources: Vec<(String, String)> = db::list_domain_events(&mut conn)
            .expect("events")
            .into_iter()
            .filter(|e| e.event_type == "category.created")
            .map(|e| (e.payload.clone(), e.source))
            .collect();
        assert!(
            sources
                .iter()
                .any(|(p, s)| p.contains(CATEGORY_CREATED_BY_AGENT) && s == DomainEventSource::Mcp.as_str()),
            "an agent's mint is the MCP half: {sources:?}"
        );
        assert!(
            sources
                .iter()
                .any(|(p, s)| p.contains(CATEGORY_CREATED_BY_HUMAN) && s == DomainEventSource::Ui.as_str()),
            "a human's creation is the UI half: {sources:?}"
        );

        assert!(matches!(
            create_by_agent(&mut conn, &project_id, "   "),
            Err(CategoryError::BlankName)
        ));
        assert!(matches!(
            create_by_human(&mut conn, &project_id, "   "),
            Err(CategoryError::BlankName)
        ));
    }

    #[test]
    fn resolution_takes_the_exact_slug_first_then_the_normalized_name() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");

        // Exact slug.
        let by_slug = resolve(&mut conn, &project_id, CategoryRef::Label("bug")).expect("resolve");
        assert_eq!(by_slug.slug, "bug");
        // Normalized name — case and inner whitespace fold.
        let by_name =
            resolve(&mut conn, &project_id, CategoryRef::Label("  rEfActor ")).expect("resolve");
        assert_eq!(by_name.slug, "refactor");
        // Id.
        let by_id =
            resolve(&mut conn, &project_id, CategoryRef::Id(&by_name.id)).expect("resolve");
        assert_eq!(by_id.id, by_name.id);

        assert!(matches!(
            resolve(&mut conn, &project_id, CategoryRef::Label("nothing")),
            Err(CategoryError::NotFound)
        ));

        // THE SLUG WINS over a name that would have matched another row: renaming `chore` to
        // "Bug" leaves `bug` resolvable to the bug category, not to the homonym.
        rename(
            &mut conn,
            &project_id,
            CategoryRef::Label("chore"),
            "Bug",
        )
        .expect("rename");
        let still_bug =
            resolve(&mut conn, &project_id, CategoryRef::Label("bug")).expect("resolve");
        assert_eq!(still_bug.slug, "bug");

        // …but asking by the now-ambiguous NAME is refused rather than guessed. The refusal names
        // the slugs, which are unique, so the caller can re-ask unambiguously.
        match resolve(&mut conn, &project_id, CategoryRef::Label("BUG ")) {
            Err(CategoryError::AmbiguousName { matches, .. }) => {
                assert_eq!(matches, vec!["bug".to_string(), "chore".to_string()]);
            }
            other => panic!("expected AmbiguousName, got {other:?}"),
        }
    }

    #[test]
    fn resolve_or_create_never_blocks_on_an_unknown_label() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");

        let (existing, created) =
            resolve_or_create(&mut conn, &project_id, "Bug").expect("resolve");
        assert!(!created, "an existing NAME is reused, never duplicated");
        assert_eq!(existing.slug, "bug");

        let (fresh, created) =
            resolve_or_create(&mut conn, &project_id, "Tech Debt").expect("create");
        assert!(created);
        assert_eq!(fresh.slug, "tech-debt");
        assert_eq!(fresh.created_by_type, CATEGORY_CREATED_BY_AGENT);
        assert!(fresh.confirmed_at.is_some(), "born valid — no curation state");

        assert!(matches!(
            resolve_or_create(&mut conn, &project_id, "  "),
            Err(CategoryError::BlankName)
        ));
    }

    #[test]
    fn a_spec_never_carries_a_category_of_another_project() {
        let mut conn = open_in_memory();
        let alpha = project(&mut conn, "alpha");
        let beta = project(&mut conn, "beta");
        let spec = spec(&mut conn, &alpha, "A Spec of alpha");

        let beta_bug = db::find_category_by_slug(&mut conn, &beta, "bug")
            .expect("read")
            .expect("seeded");

        // BY ID: refused, and the refusal names BOTH projects.
        match assign_spec_category(
            &mut conn,
            &spec.id,
            Some(CategoryRef::Id(&beta_bug.id)),
            DomainEventSource::Mcp,
        ) {
            Err(CategoryError::ProjectMismatch {
                spec_project_id,
                category_project_id,
            }) => {
                assert_eq!(spec_project_id, alpha);
                assert_eq!(category_project_id, beta);
            }
            other => panic!("expected ProjectMismatch, got {other:?}"),
        }
        // …and nothing was written: the refusal rolled back before the update.
        let after = db::get_spec(&mut conn, &spec.id)
            .expect("read")
            .expect("spec");
        assert_eq!(after.category_id, None);

        // BY LABEL, the cross-project assignment is not even reachable: the label resolves inside
        // the SPEC's project, so `bug` lands on alpha's own bug category.
        let assigned = assign_spec_category(
            &mut conn,
            &spec.id,
            Some(CategoryRef::Label("bug")),
            DomainEventSource::Mcp,
        )
        .expect("assign");
        let alpha_bug = db::find_category_by_slug(&mut conn, &alpha, "bug")
            .expect("read")
            .expect("seeded");
        assert_eq!(assigned.category_id.as_deref(), Some(alpha_bug.id.as_str()));

        // An unknown SPEC is refused too.
        assert!(matches!(
            assign_spec_category(
                &mut conn,
                "nope",
                Some(CategoryRef::Label("bug")),
                DomainEventSource::Mcp
            ),
            Err(CategoryError::SpecNotFound)
        ));
    }

    #[test]
    fn assignment_removal_and_label_creation_all_write_their_event() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");
        let spec = spec(&mut conn, &project_id, "A Spec");

        let (spec_after, category, created) =
            set_spec_category_by_label(&mut conn, &spec.id, "Tech Debt", DomainEventSource::Mcp)
                .expect("assign");
        assert!(created, "an unknown label MINTS rather than refusing");
        assert_eq!(spec_after.category_id.as_deref(), Some(category.id.as_str()));

        // Removal is `None`, and it is a first-class write with its own event.
        let cleared =
            assign_spec_category(&mut conn, &spec.id, None, DomainEventSource::Mcp)
                .expect("unassign");
        assert_eq!(cleared.category_id, None);

        let events = db::list_domain_events(&mut conn).expect("events");
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(types.contains(&"category.created"), "got {types:?}");
        assert_eq!(
            types
                .iter()
                .filter(|t| **t == "spec.category_set")
                .count(),
            2,
            "one for the assignment, one for the removal — got {types:?}"
        );
        // The Spec's own Activity feed sees them (it matches on `$.spec_id`).
        let feed = db::recent_domain_events_for_spec(&mut conn, &spec.id, 20).expect("feed");
        assert!(feed
            .iter()
            .any(|e| e.event_type == "spec.category_set"));
    }

    /// THERE IS NO CONFIRMATION LEFT (PDR-0017 amended 2026-09-02). What the agent mints stands;
    /// the human gesture that remains on an existing row is the RENAME, and it never touches the
    /// slug.
    #[test]
    fn renaming_is_persisted_evented_and_never_touches_the_slug() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");

        let minted = create_by_agent(&mut conn, &project_id, "Tech Debt").expect("mint");
        assert!(
            minted.confirmed_at.is_some(),
            "no proposed state to leave — the mint is valid as it stands"
        );

        // A rename rewrites the LABEL and leaves the IDENTITY alone — the whole reason the two
        // columns exist.
        let renamed = rename(
            &mut conn,
            &project_id,
            CategoryRef::Id(&minted.id),
            "Dette technique",
        )
        .expect("rename");
        assert_eq!(renamed.name, "Dette technique");
        assert_eq!(renamed.slug, "tech-debt", "the slug is IMMUTABLE");
        // …and the harness can still find it by that slug.
        assert_eq!(
            resolve(&mut conn, &project_id, CategoryRef::Label("tech-debt"))
                .expect("resolve")
                .id,
            minted.id
        );

        assert!(matches!(
            rename(&mut conn, &project_id, CategoryRef::Id(&minted.id), "  "),
            Err(CategoryError::BlankName)
        ));

        let events = db::list_domain_events(&mut conn).expect("events");
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            types.iter().filter(|t| **t == "category.confirmed").count(),
            0,
            "`category.confirmed` was RETIRED with the axis — got {types:?}"
        );
        assert_eq!(
            types.iter().filter(|t| **t == "category.renamed").count(),
            1
        );

        // THE TWO HALVES OF PDR-0017 ARE TOLD APART IN THE TRACE: the agent minted the category,
        // a human renamed it, and `source` is the only column that says so.
        let source_of = |event_type: &str| -> String {
            events
                .iter()
                .find(|e| e.event_type == event_type)
                .unwrap_or_else(|| panic!("{event_type} was recorded"))
                .source
                .clone()
        };
        assert_eq!(
            source_of("category.created"),
            DomainEventSource::Mcp.as_str(),
            "an agent's on-the-fly creation is the MCP half"
        );
        assert_eq!(
            source_of("category.renamed"),
            DomainEventSource::Ui.as_str(),
            "renaming is a HUMAN gesture — it may not be filed as the agent's"
        );
    }

    #[test]
    fn an_assignment_files_the_provenance_its_caller_states() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");
        let agent_spec = spec(&mut conn, &project_id, "Classified by an agent");
        let human_spec = spec(&mut conn, &project_id, "Classified by a human");

        // The agent half (phase 2 — the MCP surface).
        set_spec_category_by_label(&mut conn, &agent_spec.id, "Bug", DomainEventSource::Mcp)
            .expect("assign");
        // The human half (phase 8 — the Spec detail screen), including the CLEAR, which is a
        // decision too and must not be filed as the agent's either.
        assign_spec_category(
            &mut conn,
            &human_spec.id,
            Some(CategoryRef::Label("refactor")),
            DomainEventSource::Ui,
        )
        .expect("assign");
        assign_spec_category(&mut conn, &human_spec.id, None, DomainEventSource::Ui)
            .expect("unassign");

        // Unlike `category.*`, `spec.category_set` is reached by BOTH halves through the same
        // function — so the ONLY thing that tells a human's classification from an agent's is the
        // `source` its caller stated. Read the events back per Spec and assert exactly that.
        let sources_for = |conn: &mut SqliteConnection, spec_id: &str| -> Vec<String> {
            db::recent_domain_events_for_spec(conn, spec_id, 20)
                .expect("feed")
                .into_iter()
                .filter(|e| e.event_type == "spec.category_set")
                .map(|e| e.source)
                .collect()
        };
        assert_eq!(
            sources_for(&mut conn, &agent_spec.id),
            vec![DomainEventSource::Mcp.as_str().to_string()],
            "an agent's classification is the MCP half"
        );
        assert_eq!(
            sources_for(&mut conn, &human_spec.id),
            vec![
                DomainEventSource::Ui.as_str().to_string(),
                DomainEventSource::Ui.as_str().to_string()
            ],
            "a human's assign AND unassign are the UI half — never the agent's"
        );
    }

    /// The Spec and its classification are ONE write: the category rides the creation's own
    /// transaction, so it exists exactly when the Spec does.
    #[test]
    fn creating_a_spec_with_a_category_classifies_it_in_the_same_write() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");

        // A SEEDED slug — the nominal `/nspec` gesture.
        let (spec, revision, category) = create_spec_with_category(
            &mut conn,
            &project_id,
            "A bug",
            db::SpecSections::default(),
            Some("bug"),
            DomainEventSource::Mcp,
        )
        .expect("create + classify");
        let category = category.expect("a category was asked for");
        assert_eq!(category.slug, "bug");
        assert_eq!(category.created_by_type, CATEGORY_CREATED_BY_SYSTEM);
        assert_eq!(revision.spec_id, spec.id);
        assert_eq!(spec.category_id.as_deref(), Some(category.id.as_str()));

        // An UNKNOWN label MINTS rather than refusing (PDR-0017) — creation is never blocking.
        let (_, _, minted) = create_spec_with_category(
            &mut conn,
            &project_id,
            "Some debt",
            db::SpecSections::default(),
            Some("Tech Debt"),
            DomainEventSource::Mcp,
        )
        .expect("create + mint");
        let minted = minted.expect("a category was asked for");
        assert_eq!(minted.slug, "tech-debt");
        assert_eq!(minted.created_by_type, CATEGORY_CREATED_BY_AGENT);
        assert!(minted.confirmed_at.is_some());

        // NO category asked for — an unclassified Spec is a legitimate state, not a refusal.
        let (plain, _, none) = create_spec_with_category(
            &mut conn,
            &project_id,
            "Unclassified",
            db::SpecSections::default(),
            None,
            DomainEventSource::Mcp,
        )
        .expect("create");
        assert!(none.is_none());
        assert_eq!(plain.category_id, None);
    }

    /// THE ATOMICITY THIS COMPOSITION EXISTS FOR: an error INJECTED after the pair was written —
    /// anywhere in the caller's transaction — takes BOTH the minted category and the Spec down.
    ///
    /// The failure it guards against is not hypothetical bookkeeping: a Spec created but left
    /// UNCATEGORIZED is exactly what escapes the category's discipline, and a category minted for a
    /// Spec that no longer exists is a chip a human is asked to curate for nothing. The two writes
    /// are visible from INSIDE the transaction and neither survives its rollback — which is what
    /// "the same transaction" means, and the only way to tell it from two commits in a row.
    #[test]
    fn an_injected_error_rolls_back_the_minted_category_together_with_its_spec() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");
        let seeded = db::list_categories(&mut conn, &project_id).expect("read").len();

        let outcome: Result<(), CategoryError> = conn.transaction(|conn| {
            let (spec, _revision, category) = create_spec_with_category(
                conn,
                &project_id,
                "Doomed",
                db::SpecSections::default(),
                Some("Flaky Infra"),
                DomainEventSource::Mcp,
            )?;
            // BOTH are there, seen from inside the transaction…
            assert!(db::get_spec(conn, &spec.id)?.is_some());
            assert_eq!(category.expect("minted").slug, "flaky-infra");
            // …and now something downstream fails.
            Err(CategoryError::Db(
                diesel::result::Error::RollbackTransaction,
            ))
        });
        assert!(outcome.is_err(), "the injected error surfaces");

        // NEITHER survives: no half-created Spec, no orphan proposal.
        assert!(
            db::list_specs(&mut conn, Some(&project_id))
                .expect("read")
                .is_empty(),
            "the Spec was rolled back with the error"
        );
        assert!(
            db::find_category_by_slug(&mut conn, &project_id, "flaky-infra")
                .expect("read")
                .is_none(),
            "the category minted for that Spec was rolled back with it"
        );
        assert_eq!(
            db::list_categories(&mut conn, &project_id).expect("read").len(),
            seeded,
            "the project's vocabulary is exactly what it was"
        );
    }

    /// A category the project cannot name is refused BEFORE the Spec exists — the refusal is
    /// atomic too, in the other direction.
    #[test]
    fn a_refused_category_leaves_no_spec_behind() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");
        // Two categories answering the same NAME: `bug` and a `chore` renamed onto it.
        rename(&mut conn, &project_id, CategoryRef::Label("chore"), "Bug").expect("rename");

        let refused = create_spec_with_category(
            &mut conn,
            &project_id,
            "Ambiguously classified",
            db::SpecSections::default(),
            Some("Bug"),
            DomainEventSource::Mcp,
        );
        assert!(matches!(refused, Err(CategoryError::AmbiguousName { .. })));
        assert!(
            db::list_specs(&mut conn, Some(&project_id))
                .expect("read")
                .is_empty(),
            "a Spec whose classification was refused is not created half-way"
        );

        // A BLANK label is the same story: the caller asked for a classification and got none, so
        // it does not get the Spec either.
        assert!(matches!(
            create_spec_with_category(
                &mut conn,
                &project_id,
                "Blank",
                db::SpecSections::default(),
                Some("   "),
                DomainEventSource::Mcp,
            ),
            Err(CategoryError::BlankName)
        ));
        assert!(db::list_specs(&mut conn, Some(&project_id))
            .expect("read")
            .is_empty());
    }

    #[test]
    fn renaming_a_system_category_leaves_the_slug_the_harness_reads_intact() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");
        let spec = spec(&mut conn, &project_id, "A bug");

        assign_spec_category(
            &mut conn,
            &spec.id,
            Some(CategoryRef::Label("bug")),
            DomainEventSource::Mcp,
        )
        .expect("assign");
        rename(&mut conn, &project_id, CategoryRef::Label("bug"), "Défaut").expect("rename");

        let category = db::find_category_by_slug(&mut conn, &project_id, "bug")
            .expect("read")
            .expect("still there");
        assert_eq!(category.name, "Défaut");
        assert_eq!(category.slug, "bug");
        assert_eq!(
            category.created_by_type, CATEGORY_CREATED_BY_SYSTEM,
            "a rename does not re-attribute provenance"
        );
        let spec_now = db::get_spec(&mut conn, &spec.id)
            .expect("read")
            .expect("spec");
        assert_eq!(spec_now.category_id.as_deref(), Some(category.id.as_str()));
    }

    /// A HUMAN's creation obeys the SAME slug rules as the agent's — collision-safe, and never one
    /// of the four reserved slugs. The gesture is new (the settings' Categories section); the rule
    /// it is subject to is not, because both constructors go through `slugify_for`.
    #[test]
    fn a_humans_creation_is_slugified_by_the_same_rules_as_an_agents() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");

        // A RESERVED slug is unreachable, whoever asks — the discipline is read off the slug.
        for reserved in SYSTEM_CATEGORY_SLUGS {
            let made = create_by_human(&mut conn, &project_id, reserved).expect("create");
            assert_ne!(
                made.slug, reserved,
                "a human cannot mint a second '{reserved}' either"
            );
            assert_eq!(made.created_by_type, CATEGORY_CREATED_BY_HUMAN);
        }

        // Collisions suffix, exactly as they do for the agent — and the two constructors share the
        // same namespace, so a human's "Perf" after an agent's takes `perf-2`.
        let agent = create_by_agent(&mut conn, &project_id, "Perf").expect("mint");
        assert_eq!(agent.slug, "perf");
        let human = create_by_human(&mut conn, &project_id, "Perf").expect("create");
        assert_eq!(human.slug, "perf-2");
        assert_eq!(human.name, "Perf", "the LABEL is free to be a homonym");
    }

    /// THE TWO REFUSALS OF A DELETION, and the one deletion that goes through.
    ///
    /// Both are the DOMAIN's, not a screen's: a hidden button is a courtesy, and the sidecar
    /// command is reachable without the screen that hides it.
    #[test]
    fn deleting_refuses_a_system_category_and_one_a_spec_still_carries() {
        let mut conn = open_in_memory();
        let project_id = project(&mut conn, "alpha");
        let spec = spec(&mut conn, &project_id, "A Spec");

        // (1) THE FOUR SYSTEM CATEGORIES ARE NOT DELETABLE — the slug carries the discipline.
        for reserved in SYSTEM_CATEGORY_SLUGS {
            match delete(&mut conn, &project_id, CategoryRef::Label(reserved)) {
                Err(CategoryError::SystemCategory { slug }) => assert_eq!(slug, reserved),
                other => panic!("expected SystemCategory for '{reserved}', got {other:?}"),
            }
            assert!(
                db::find_category_by_slug(&mut conn, &project_id, reserved)
                    .expect("read")
                    .is_some(),
                "'{reserved}' is still there"
            );
        }

        // (2) A CATEGORY A SPEC STILL CARRIES IS REFUSED, and the refusal NAMES THE COUNT — the FK
        // is `ON DELETE SET NULL`, so the alternative is a silent mass-declassification.
        let debt = create_by_human(&mut conn, &project_id, "Dette technique").expect("create");
        assign_spec_category(
            &mut conn,
            &spec.id,
            Some(CategoryRef::Id(&debt.id)),
            DomainEventSource::Ui,
        )
        .expect("assign");
        match delete(&mut conn, &project_id, CategoryRef::Id(&debt.id)) {
            Err(CategoryError::CategoryInUse { name, count }) => {
                assert_eq!(name, "Dette technique");
                assert_eq!(count, 1);
            }
            other => panic!("expected CategoryInUse, got {other:?}"),
        }
        assert!(
            db::get_category(&mut conn, &debt.id).expect("read").is_some(),
            "a refused deletion leaves the row alone"
        );

        // …and the Spec was NOT released on the way: the refusal happened before any write.
        assert_eq!(
            db::get_spec(&mut conn, &spec.id)
                .expect("read")
                .expect("spec")
                .category_id
                .as_deref(),
            Some(debt.id.as_str())
        );

        // (3) FREED, IT GOES — with its own `category.deleted` event, filed as the human gesture it
        // is.
        assign_spec_category(&mut conn, &spec.id, None, DomainEventSource::Ui).expect("unassign");
        let removed = delete(&mut conn, &project_id, CategoryRef::Id(&debt.id)).expect("delete");
        assert_eq!(removed.slug, "dette-technique");
        assert!(db::get_category(&mut conn, &debt.id).expect("read").is_none());

        let deleted_events: Vec<String> = db::list_domain_events(&mut conn)
            .expect("events")
            .into_iter()
            .filter(|e| e.event_type == "category.deleted")
            .map(|e| e.source)
            .collect();
        assert_eq!(
            deleted_events,
            vec![DomainEventSource::Ui.as_str().to_string()],
            "exactly one deletion, filed as a human's"
        );

        // A category of ANOTHER project is not this project's to delete — `resolve` says so.
        let beta = project(&mut conn, "beta");
        let elsewhere = create_by_human(&mut conn, &beta, "Ailleurs").expect("create");
        assert!(matches!(
            delete(&mut conn, &project_id, CategoryRef::Id(&elsewhere.id)),
            Err(CategoryError::NotFound)
        ));
    }
}
