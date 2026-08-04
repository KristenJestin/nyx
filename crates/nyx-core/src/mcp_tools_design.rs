//! Shell-agnostic MCP **DESIGN-SYSTEM tools** — the agent-facing half of the component catalogue:
//! the DISCOVERY trigger that keeps it populated, and the AUTHORING/refresh write that lets an
//! agent (re-)synthesize a component (v2.7 — Prototype : machinerie, reviews #23, #40, #45).
//!
//! # The two halves of the synthesis loop (review #40 — the BUNDLE pivot)
//!
//! A component is a multi-file BUNDLE (an Angular `.component.ts` + its `templateUrl` template +
//! its `.variants.ts` + styles), and no server-side regex can reconstruct it. So the work is split
//! and these two tools ARE the seam:
//!
//!   * [`design_inventory`] — nyx's DETERMINISTIC half. It walks the project's repos, LISTS the
//!     component bundles (files resolved, non-components filtered out), catalogues them (name /
//!     provenance / entry path — never a template), and reports which ones AWAIT a synthesis
//!     (their inputs moved, or they were never synthesized). Passing `component` switches it to a
//!     per-bundle READ that hands the agent the bundle's files WITH their content — the matter to
//!     synthesize, on demand, so a 100+-component front is never inlined wholesale.
//!   * [`design_refresh_component`] — the agent's WRITE half. It stores the synthesized
//!     self-contained Handlebars `template` + `params_json` + `states_json` onto an EXISTING
//!     catalogue component (the review-#45 gap: `proto_author_component` refuses a known name, so
//!     re-synthesizing or fixing a component was impossible without delete-then-recreate), and
//!     records the bundle's INPUT hash so the loop stays incremental.
//!   * [`design_delete_component`] — the REMOVAL the catalogue never had (review #44). The
//!     inventory now prunes the rows whose source really vanished; this closes the loop for
//!     everything it deliberately spares (a mistaken greenfield `[NEW]`, an orphan whose repo is
//!     momentarily unscannable). Its UI twin is the `/design` card's delete action.
//!   * [`design_preview`] — the agent's EYES on that write (finding `01KYT0Z3RYGF3A881SDQN0X9D7`).
//!     Everything above judges the STRUCTURE of a synthesis and nothing ever looked at its RESULT,
//!     so every visual defect of this feature was found by the human and by no tool. This resolves
//!     one component to the markup the frame really displays, at one axis combination and one param
//!     set, and closes the loop the manual now teaches: synthesize → render → LOOK → correct.
//!     ONE TOOL, TWO SUBJECTS (finding `01KYT45C6RC7K84V1CA3561SPF`): the same call renders a whole
//!     proto COMPOSITION (`variant_id`) — the thing actually shown to the human — because it is the
//!     same question, the same resolution and the same honesty; only the SUBJECT differs, and it is
//!     named by its own argument rather than by a mode flag.
//!   * [`design_tokens`] — the OTHER half of a component's look (review #42). A synthesized
//!     template carries Tailwind CLASSES; those classes are grey until the project's THEME TOKENS
//!     (`--primary`, `--background`, …) exist. This tool ingests them BY REFERENCE from the app's
//!     own stylesheet (and reads/edits them afterwards) into the project's single token layer,
//!     which the preview iframe turns into its `@theme` + `:root`.
//!
//! # Single authority (no double path)
//!
//! Both the MCP tool and the `design_inventory` IPC db-command delegate to
//! [`crate::design::inventory_project`] — the SHARED repo resolution (root workspace → repos,
//! PDR-0013) and discovery — so there is exactly ONE behaviour, whoever the caller is. Every write
//! goes through the `db` authority and returns [`ChangedTopic::Prototypes`] (the wire topic
//! `/design` listens on), so an open design surface re-pulls.
//!
//! No UI GESTURE calls it any more (review 01KYKX2A0JGWF8XNWWZ9YFF5MG, finding
//! 01KYKX7HBP4ZSSS6ZDM31T6A5E): `/design` used to carry an "Inventorier / Sync" button, which
//! duplicated step 0 of the agent's own manual and made the human the operator of a task that is
//! not theirs (PDR-0009 — le flux prototype est agent-driven). The button is gone; the TOOL and the
//! db-command are not. `/design` is a pure reader that repaints on `prototypes://changed`.

use diesel::connection::Connection;
use serde_json::{json, Value};

use crate::db::{self, Db};
use crate::design;
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{
    bounded_row, bounded_text, bounded_tool_result, internal_db, optional_bool, optional_str,
    optional_str_vec, optional_usize, require_str, under_response_budget, MAX_RESPONSE_BYTES,
    MAX_ROW_FIELD_BYTES,
};
use crate::mcp_tools_core::{ChangedTopic, ToolOutcome};

/// `design_inventory` — LIST a project's component BUNDLES (deterministic discovery) or READ one
/// bundle's content. The trigger that POPULATES the catalogue (advertised in `tools/list`).
pub const DESIGN_INVENTORY_TOOL: &str = "design_inventory";

/// `design_refresh_component` — STORE a synthesized template/params/states onto an EXISTING
/// catalogue component (review #45).
pub const DESIGN_REFRESH_COMPONENT_TOOL: &str = "design_refresh_component";

/// `design_delete_component` — REMOVE a component from the catalogue (review #44 — the catalogue
/// had no removal at all).
pub const DESIGN_DELETE_COMPONENT_TOOL: &str = "design_delete_component";

/// `design_tokens` — READ / INGEST / EDIT the project's THEME TOKEN layer (review #42 — the tokens
/// were never read from the app, so every preview rendered GREY).
pub const DESIGN_TOKENS_TOOL: &str = "design_tokens";

/// `design_recipe` — READ / WRITE the project's BUNDLE RECIPE: what constitutes a component in THIS
/// repo (finding 01KYKXPHW103C0V5RBXQ29BBXQ — the framework knowledge left nyx).
pub const DESIGN_RECIPE_TOOL: &str = "design_recipe";

/// `design_preview` — RESOLVE into the markup the frame really DISPLAYS either ONE catalogue
/// component at a given axis combination (`component`, finding `01KYT0Z3RYGF3A881SDQN0X9D7`) or a
/// WHOLE proto composition (`variant_id`, finding `01KYT45C6RC7K84V1CA3561SPF`): the agent's own
/// eyes on what it wrote, and on what it is about to present.
pub const DESIGN_PREVIEW_TOOL: &str = "design_preview";

/// The design tool names — advertised in `tools/list` and routed by [`dispatch_design_tool`]. A
/// closed slice so a new tool is added in exactly one place.
pub const DESIGN_TOOLS: [&str; 6] = [
    DESIGN_INVENTORY_TOOL,
    DESIGN_REFRESH_COMPONENT_TOOL,
    DESIGN_DELETE_COMPONENT_TOOL,
    DESIGN_TOKENS_TOOL,
    DESIGN_RECIPE_TOOL,
    DESIGN_PREVIEW_TOOL,
];

/// How many bundles [`design_inventory`] echoes by DEFAULT — a real front has 100+ components, so
/// the list is BOUNDED and the counters carry the whole picture (review #43: the tool used to
/// inline the WHOLE catalogue, 144 k chars on a real front — past the MCP response limit).
const DEFAULT_BUNDLE_LIMIT: usize = 25;

/// The hard ceiling on the echoed bundle list, whatever `limit` a caller passes (review #43). The
/// rest of the catalogue is reachable by PAGING with `offset`.
const MAX_BUNDLE_LIMIT: usize = 200;

/// How many bytes of EACH bundle file [`design_inventory`] returns in its per-bundle read — a real
/// template/class stays far below it; a pathological file can never blow the MCP response
/// (review #43). This bounds a RESPONSE, and nothing else: the CHANGE DETECTOR
/// ([`crate::design::bundle_input_hash`]) folds in every byte of every file (streamed), because a
/// bound on what is READ was silently a bound on what is DETECTED — a constant-size edit past this
/// cap used to be invisible to the incremental loop.
const MAX_BUNDLE_FILE_BYTES: usize = 32 * 1024;

// The RESPONSE-level byte budget (review #43) — `MAX_RESPONSE_BYTES` + `under_response_budget` —
// now lives in [`crate::mcp_runtime`], BESIDE the argument vocabulary every tool shares, because it
// was needed on the prototype reads too and a second copy would have been a second policy (review
// 01KYMF0KFZQG5JYZDGZCR85BAM). The semantics here are UNCHANGED: the per-file / per-list bounds cap
// each ITEM, and the budget caps the WHOLE payload.

/// `design_inventory` — three shapes, one tool:
///
/// * `{ project_id, globs?, limit?, pending_only? }` (no `offset`, or `offset: 0`) →
///   `{ mode: "bundles", repos_scanned, discovered, component_count, synthesized_count,
///   pending_count, moved, materialized, orphans, orphan_count, pruned_count, scan_error_count,
///   scan_errors, total, offset, returned, truncated, next_offset, bundles }` — the DETERMINISTIC
///   SCAN: resolve the project's repos (the SAME way the dev context does), scan them by each repo's
///   DETECTED-stack entry globs, group the files into component BUNDLES (`templateUrl` resolved,
///   `.service`/`.types`/`.variants`/spec filtered out), RECONCILE the catalogue (moves re-anchored
///   by content, orphans pruned or flagged — all reported), UPSERT each bundle's discovery, and
///   report which bundles AWAIT a synthesis. The `bundles` list is BOUNDED (`limit`, default 25,
///   max 200, never 0) — the counters, not the list, carry the scale (review #43). Reports a
///   `Prototypes` change ONLY when the catalogue actually moved.
/// * `{ project_id, offset > 0, limit?, pending_only? }` → `{ mode: "catalogue", … }` — a PAGE of
///   the PERSISTED catalogue. NON-MUTATING: no walk, no hash, no write, no notification (review —
///   every page used to re-inventory the whole project, and the offset shifted under a live disk).
///   Ask for a fresh scan by paging back to `offset: 0`.
/// * `{ project_id, component }` → `{ mode: "bundle", component, bundle, files, up_to_date,
///   truncated, read_error_count }` — a READ of ONE bundle with its files' CONTENT: the matter the
///   agent synthesizes into a self-contained logic-less Handlebars template (stored back via
///   `design_refresh_component`, quoting the `bundle.input_hash` this read returns). A file that
///   could NOT be read carries its failure in `files[].read_error` and is counted in
///   `read_error_count` — the `mode: "bundle"` twin of the listing's `scan_error_count`: an
///   unreadable file used to arrive as an EMPTY one, so the agent synthesized on partial matter and
///   the refresh then recorded that synthesis as up to date (review 01KYHV1C2M1QNQ6A4Z1D00SC3B).
///
/// # PAGING CONTRACT (review #43 — the traversal used to LOSE and DUPLICATE entries)
///
/// A traversal that starts at `offset: 0` and follows `next_offset` until it is `null` yields EVERY
/// item of the population EXACTLY ONCE, `pending_only` included, and `total` is the same on every
/// page. That holds because the SCAN page and the CATALOGUE pages now enumerate the SAME population
/// in the SAME order, out of the SAME persisted facts:
///
///   * POPULATION — "the bundles the LAST SCAN discovered". Page 0 lists what it just discovered;
///     a later page lists the rows that scan stamped with a `scanned_input_hash` (v35). A row the
///     scan did NOT see — an orphan it deliberately spared, a row of a repo it could not walk — is
///     in NEITHER (it used to be in the second only, which double-counted some entries and skipped
///     others).
///   * ORDER — the catalogue name, which is UNIQUE per project, so both sides sort identically.
///   * `pending_only` — the SAME predicate on both sides: an empty template, or a
///     `visual_hash` (the state at the last SYNTHESIS) that differs from the `scanned_input_hash`
///     (the state at the last SCAN). Page 0 used to include `source_changed` bundles that a later
///     page could not even see, so following `next_offset` silently dropped half an agent's work
///     list and reported `truncated: false` while doing it.
///
/// The one thing a traversal is NOT immune to is a CONCURRENT SCAN (another agent session, or the
/// `design_inventory` db-command) landing between two of its pages: pages are a view of the last
/// scan, and a new scan replaces it. Page back to `offset: 0` for a coherent restart.
///
/// ALL shapes are capped by [`MAX_RESPONSE_BYTES`] on top of their per-item bounds (review #43),
/// so no project size can produce a response past the MCP limit; `truncated` always says whether
/// something was left out, and `next_offset` — which can never equal `offset` — walks the rest.
///
/// The optional `globs` array PERSISTS a per-project glob OVERRIDE (the "surchargeable par projet"
/// seam) BEFORE scanning: a non-empty array replaces the detected-stack default for every repo; an
/// empty array CLEARS the override. Omitting `globs` leaves any stored override untouched.
/// GREENFIELD-safe: a project with no front inventories nothing WITHOUT error.
pub fn design_inventory(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    // --- READ shape: one bundle's content, on demand (never the whole catalogue inlined). ---
    if let Some(component) = optional_str(args, "component")? {
        let read = db
            .with_conn(|c| design::read_bundle(c, project_id, component, MAX_BUNDLE_FILE_BYTES))
            .map_err(internal_db)?
            .ok_or_else(|| {
                RpcError::new(
                    RpcCode::InvalidId,
                    format!(
                        "component '{component}' is not in the catalogue of project {project_id} \
                         (run design_inventory without `component` to list the discovered bundles)"
                    ),
                )
            })?;
        // A bundle can hold MANY files, each already capped at 32 KiB — the RESPONSE budget caps
        // their SUM (review #43). A file left out is still listed (path + role + real size) with an
        // empty `content` and `truncated: true`, so the agent knows it exists.
        let files: Vec<Value> = read.files.iter().map(|f| json!(f)).collect();
        let (mut kept, dropped) = under_response_budget(files, MAX_RESPONSE_BYTES);
        for skipped in read.files.iter().skip(kept.len()) {
            kept.push(json!({
                "path": skipped.path,
                "role": skipped.role,
                "bytes": skipped.bytes,
                "truncated": true,
                "content": "",
                // Carried even here: a file the budget dropped that was ALSO unreadable must not
                // lose its failure on the way out.
                "read_error": skipped.read_error,
            }));
        }
        let truncated = dropped || read.files.iter().any(|f| f.truncated);
        return Ok(ToolOutcome::read(json!({
            "mode": "bundle",
            "component": read.component,
            "bundle": read.bundle,
            "files": kept,
            "up_to_date": read.up_to_date,
            "truncated": truncated,
            // What could NOT be read — the `mode: "bundle"` twin of the listing's
            // `scan_error_count` (review 01KYHV1C2M1QNQ6A4Z1D00SC3B: an unreadable bundle file was
            // served to the agent as an EMPTY one, so it synthesized on partial matter and
            // `design_refresh_component` then marked that synthesis UP TO DATE). Non-zero ⇒ the
            // matter is PARTIAL: do not synthesize from it, re-read once the file is readable
            // again. Each affected file names its own failure in `files[].read_error`.
            "read_error_count": read.read_error_count,
        })));
    }

    // --- LIST shape: the deterministic discovery + the targeted work list. ---
    let globs_override = optional_str_vec(args, "globs")?;
    // BOUNDED ON BOTH SIDES (review — `limit: 0`): the upper bound caps the response, the lower
    // bound keeps the PAGING alive. `limit: 0` returned an empty page with `next_offset == offset`,
    // so an agent following `next_offset` while `truncated` — the protocol this tool's own
    // description teaches — looped forever, re-scanning the whole project at every turn.
    let limit = optional_usize(args, "limit")?
        .unwrap_or(DEFAULT_BUNDLE_LIMIT)
        .clamp(1, MAX_BUNDLE_LIMIT);
    let offset = optional_usize(args, "offset")?.unwrap_or(0);
    let pending_only = optional_bool(args, "pending_only")?.unwrap_or(false);

    // PAGE > 0 is a pure CATALOGUE READ (review — every page re-inventoried the whole project).
    // Only the FIRST page SCANS: walking every repo, hashing every bundle and re-upserting the
    // catalogue once per page turned "browsing a list" into N full scans + N waves of writes + N
    // change notifications, and made the offset unstable (a component appearing between two pages
    // shifted the list and SKIPPED an entry). Later pages therefore serve the catalogue the scan
    // just persisted — non-mutating, no walk, stable order.
    //
    // Supplying `globs` is a CONFIG WRITE, so it always scans, whatever the offset: dropping the
    // override because the caller happened to pass an offset would silently discard a persisted
    // setting.
    if offset > 0 && globs_override.is_none() {
        return catalogue_page(db, project_id, offset, limit, pending_only);
    }

    // `globs` is the ENTRY-GLOBS-ONLY shortcut; a project that has a full RECIPE has already said
    // more than that, and the recipe WINS (see `design::ProjectRecipe`). Accepting the argument
    // anyway would persist a setting that changes nothing — the silent config write this whole
    // review keeps finding. Say so instead, and name the field to edit.
    if globs_override.is_some() {
        let has_recipe = db
            .with_conn(|c| db::design_bundle_recipe_json(c, project_id))
            .map_err(internal_db)?
            .is_some();
        if has_recipe {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                "this project has a bundle RECIPE, which supersedes the `globs` shortcut — passing \
                 `globs` here would be stored and then ignored at every scan. Edit the recipe's \
                 `entries[].globs` with design_recipe(project_id, recipe: …), or \
                 design_recipe(project_id, reset: true) first if you really want the glob \
                 shortcut back.",
            ));
        }
    }

    let outcome = db
        .with_conn(|c| {
            if let Some(globs) = globs_override.as_ref() {
                db::set_design_inventory_globs(c, project_id, Some(globs.as_slice()))?;
            }
            design::inventory_project(c, project_id)
        })
        .map_err(internal_db)?;

    // The bundles WORTH echoing: the pending ones when asked, else every discovered bundle — each
    // flagged with whether it awaits a synthesis, then PAGED and BOUNDED.
    let pending_names: std::collections::BTreeSet<&str> =
        outcome.pending.iter().map(|p| p.name.as_str()).collect();
    let selected: Vec<&design::ComponentBundle> = outcome
        .bundles
        .iter()
        .filter(|b| !pending_only || pending_names.contains(b.name.as_str()))
        .collect();
    let page: Vec<Value> = selected
        .iter()
        .skip(offset)
        .take(limit)
        .map(|b| {
            json!({
                "name": b.name,
                "root_path": b.root_path,
                "entry_path": b.entry_path,
                "files": b.files,
                "input_hash": b.input_hash,
                "needs_synthesis": pending_names.contains(b.name.as_str()),
            })
        })
        .collect();
    let (listed, dropped) = under_response_budget(page, MAX_RESPONSE_BYTES);
    let next = offset + listed.len();
    let truncated = dropped || next < selected.len();
    let result = json!({
        "mode": "bundles",
        "repos_scanned": outcome.repos_scanned,
        "discovered": outcome.discovered,
        "component_count": outcome.component_count,
        "synthesized_count": outcome.synthesized_count,
        "pending_count": outcome.pending.len(),
        // The LIFE-CYCLE report (review — a row whose source vanished was invisible): what MOVED on
        // disk (re-anchored by content, identity preserved) and what went ORPHAN (pruned, or spared
        // with the reason). Bounded like everything else the tool echoes.
        "moved": outcome
            .moved
            .iter()
            .take(MAX_BUNDLE_LIMIT)
            .map(|(name, entry_path)| json!({ "name": name, "entry_path": entry_path }))
            .collect::<Vec<Value>>(),
        // The greenfield `[NEW]`s a discovered bundle MATERIALIZED this run (review #41). It is
        // what takes a node OUT of a composition's `new_components` — i.e. what stops the coder
        // being asked to build it — so it can never be the one life-cycle event nobody reports.
        "materialized": outcome
            .materialized
            .iter()
            .take(MAX_BUNDLE_LIMIT)
            .map(|(name, entry_path)| json!({ "name": name, "entry_path": entry_path }))
            .collect::<Vec<Value>>(),
        "orphans": outcome
            .orphans
            .iter()
            .take(MAX_BUNDLE_LIMIT)
            .collect::<Vec<&design::OrphanComponent>>(),
        "orphan_count": outcome.orphans.len(),
        "pruned_count": outcome.orphans.iter().filter(|o| o.pruned).count(),
        // What the WALK could not read (review — the CRITICAL data-loss path). Non-zero ⇒ the
        // discovery is PARTIAL, and NOTHING was pruned in the repos concerned: an absence measured
        // by a scan that failed to read is not an absence at all.
        "scan_error_count": outcome.scan_errors.count,
        "scan_errors": outcome.scan_errors.sample,
        "total": selected.len(),
        "offset": offset,
        "returned": listed.len(),
        "truncated": truncated,
        // The offset to ASK FOR NEXT — `offset + returned`, never `offset + limit`: the response
        // budget can cut a page short, and paging by `limit` would then skip what it dropped. It is
        // never equal to `offset` (`limit >= 1`), so following it always progresses.
        "next_offset": if next < selected.len() && next > offset { Some(next) } else { None },
        "bundles": listed,
    });
    // A scan that changed NOTHING notifies nothing: `/design` has no reason to re-pull, and the
    // tool stops reporting a mutation it did not make.
    Ok(if outcome.catalogue_changed {
        ToolOutcome::changed(result, ChangedTopic::Prototypes)
    } else {
        ToolOutcome::read(result)
    })
}

/// Does this catalogue row AWAIT a (re-)synthesis, judged on PERSISTED state alone?
///
/// The exact predicate the SCAN page applies to a freshly discovered bundle, expressed over what the
/// row stores: `visual_hash` is the bundle's input state AS OF THE LAST SYNTHESIS,
/// `scanned_input_hash` (v35) is its input state AS OF THE LAST SCAN. Never synthesized (no template
/// or no recorded synthesis) OR the two disagree (`source_changed`) ⇒ pending.
///
/// This is the whole fix for the paging divergence (review #43): the catalogue page could only ever
/// see `never_synthesized`, because `source_changed` was a DISK comparison it had no right to make.
/// Persisting what the scan observed turns it into a stored fact both pages read the same way.
fn needs_synthesis(component: &db::DesignComponent) -> bool {
    component.template.trim().is_empty()
        || component.visual_hash.is_none()
        || component.visual_hash != component.scanned_input_hash
}

/// The `offset > 0` half of [`design_inventory`]'s LIST shape: a PAGE of the PERSISTED catalogue,
/// in the catalogue's own stable `(name, id)` order — no walk, no hash, no write, no notification.
///
/// Its population is EXACTLY page 0's: the rows the LAST SCAN DISCOVERED, i.e. those carrying a
/// `source_path` AND the `scanned_input_hash` that scan stamped on them. A row the scan did not see
/// — an orphan it spared, a row of a repo it could not walk — is in neither page (it used to be in
/// this one only, which made a traversal DUPLICATE some entries and SKIP others). A greenfield
/// `[NEW]` is not a bundle and is likewise absent. `needs_synthesis` is [`needs_synthesis`], the same
/// predicate page 0 applies. See the PAGING CONTRACT on [`design_inventory`].
fn catalogue_page(
    db: &Db,
    project_id: &str,
    offset: usize,
    limit: usize,
    pending_only: bool,
) -> Result<ToolOutcome, RpcError> {
    let components = db
        .with_conn(|c| db::list_components(c, project_id))
        .map_err(internal_db)?;
    let selected: Vec<&db::DesignComponent> = components
        .iter()
        .filter(|c| c.source_path.is_some() && c.scanned_input_hash.is_some())
        .filter(|c| !pending_only || needs_synthesis(c))
        .collect();
    let page: Vec<Value> = selected
        .iter()
        .skip(offset)
        .take(limit)
        .map(|c| {
            json!({
                "name": c.name,
                "entry_path": c.source_path,
                // The BUNDLE's input hash as the last scan observed it — the same value page 0
                // echoes, and the one `design_refresh_component` expects back (it used to echo
                // `visual_hash`, the SYNTHESIS hash, under the same key).
                "input_hash": c.scanned_input_hash,
                "needs_synthesis": needs_synthesis(c),
            })
        })
        .collect();
    let (listed, dropped) = under_response_budget(page, MAX_RESPONSE_BYTES);
    let next = offset + listed.len();
    let truncated = dropped || next < selected.len();
    Ok(ToolOutcome::read(json!({
        "mode": "catalogue",
        "component_count": components.len(),
        "synthesized_count": components
            .iter()
            .filter(|c| !c.template.trim().is_empty())
            .count(),
        "total": selected.len(),
        "offset": offset,
        "returned": listed.len(),
        "truncated": truncated,
        // Never equal to `offset` (a page always returns at least one row while `truncated`), so a
        // caller following `next_offset` always progresses.
        "next_offset": if next < selected.len() && next > offset { Some(next) } else { None },
        "bundles": listed,
    })))
}

/// `design_delete_component` — `{ project_id, name, exclude_source? }` → `{ deleted, component,
/// ephemeral, verdict, source_path, repo, excluded, recipe_materialized, warnings }`. REMOVE a
/// component from the project's catalogue.
///
/// The catalogue had NO removal at all (review #44): a wrong glob left 28 phantom rows — email /
/// PDF templates, `.generated` mirrors — that no correction could ever take out, because the
/// inventory only ever UPSERTS. The automatic élagage
/// ([`crate::design::prune_orphans`]) now clears the rows whose source really vanished; THIS is the
/// explicit escape hatch for everything it deliberately spares: a greenfield `[NEW]` authored by
/// mistake, an orphan whose repo is momentarily unscannable, a duplicate.
///
/// `db::delete_component` already existed and was exposed nowhere; this tool and the `/design` card
/// action are its two callers — both through [`crate::design::remove_component`], so removal has ONE
/// authority like every other write.
///
/// # It no longer LIES about its effect (finding `01KYSCX4PNKTMX17YVG9XD8KBZ`)
///
/// A component's IDENTITY IS ITS SOURCE. Deleting the row of a component whose file still matches the
/// recipe deletes nothing durable: the next `design_inventory` re-discovers it, `pending`, with a new
/// id. The tool used to answer a bare `deleted: true` — so "keep only these N components" looked like
/// it had worked and came undone at the next scan, with nothing anywhere saying why. It now
/// * DECIDES whether the removal is EPHEMERAL (the discovery's own verdict on that path) and says it
///   in `warnings[]`, naming the two corrections (restrict the recipe / exclude the source);
/// * offers the exclusion itself — `exclude_source: true` appends the bundle's paths to the recipe's
///   `exclude` list, the exclusion the discovery has always honoured, so the removal LASTS.
///
/// An unknown name → `invalid_id` (never a silent no-op — the agent must know it deleted nothing). An
/// exclusion the recipe cannot hold → `invalid_argument`, and NOTHING is deleted (the two were one
/// gesture). Deleting is a CATALOGUE decision, not a prototype one: it never touches a composition,
/// which references components BY NAME and simply reclassifies the node `[NEW]` (to be built) at the
/// next read. Mutating → `Prototypes` changed.
pub fn design_delete_component(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let name = require_str(args, "name")?;
    let exclude_source = optional_bool(args, "exclude_source")?.unwrap_or(false);
    let result = db
        .with_conn(
            |c| -> Result<Result<Value, RpcError>, diesel::result::Error> {
                let Some(existing) = db::get_component_by_name(c, project_id, name)? else {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidId,
                        format!(
                            "component '{name}' is not in the catalogue of project {project_id} \
                             (run design_inventory to list what is)"
                        ),
                    )));
                };
                let removal = design::remove_component(c, project_id, &existing, exclude_source)?;
                if let Some(refused) = removal.refused {
                    return Ok(Err(RpcError::new(RpcCode::InvalidArgument, refused)));
                }
                // The echoed row carries the STORED `template` — matter the agent never sent and
                // whose size it cannot predict, so it is bounded like every other echoed row
                // (review 01KYMF0KFZQG5JYZDGZCR85BAM).
                Ok(Ok(json!({
                    "deleted": removal.deleted,
                    "component": bounded_row(&json!(existing)),
                    // Will the next inventory bring this source back? The answer the gesture used to
                    // keep to itself (finding 01KYSCX4PNKTMX17YVG9XD8KBZ).
                    "ephemeral": removal.effect.ephemeral,
                    "verdict": removal.effect.verdict,
                    "source_path": removal.effect.source_path,
                    "repo": removal.effect.repo,
                    "recipe_source": removal.effect.recipe_source,
                    "excluded": removal.excluded,
                    "recipe_materialized": removal.recipe_materialized,
                    "warnings": removal.warnings,
                })))
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(result, ChangedTopic::Prototypes))
}

/// `design_refresh_component` — `{ project_id, name, template, params_json?, states_json?,
/// input_hash? }` → `{ component, input_hash, synthesized, source_changed, current_input_hash }`.
/// STORE an agent's SYNTHESIS onto an EXISTING catalogue component: its self-contained Handlebars
/// `template` + `params_json` + `states_json` are REPLACED, its identity + provenance (`source_*`)
/// preserved, and the bundle's INPUT hash recorded ([`crate::design::mark_synthesized_at`]) so the
/// next inventory knows it is up to date.
///
/// OPTIMISTIC CONCURRENCY (`input_hash`): pass back the `bundle.input_hash` that
/// `design_inventory(component=…)` returned when you READ the bundle. The synthesis is then marked
/// up to date ONLY if the bundle's sources are STILL at that state; if they moved in between (a
/// human edit, a `git pull`, another agent), the template is still stored — no work is lost — but
/// the hash is NOT recorded, so the bundle stays on the `pending` work list and the response says
/// `synthesized: false, source_changed: true` with the `current_input_hash` to re-read from.
/// OMITTING `input_hash` keeps the historical behaviour: whatever is on disk at WRITE time is
/// recorded as synthesized (a change that landed mid-synthesis is then silently marked handled).
///
/// The EXPLICIT counterpart of `proto_author_component` (review #45): that tool CREATES a `[NEW]`
/// greenfield component and must keep REFUSING a name already catalogued (an accidental
/// duplicate); this one only ever UPDATES a component that already exists (an unknown name →
/// `invalid_id`). Two names, two intentions — no ambiguous upsert.
///
/// ATOMIC: the CONTENT write and the HASH record are the two halves of ONE act ("this template was
/// synthesized from THAT input state"), so they run inside a single `conn.transaction(...)` — the
/// repo's convention for a multi-statement write. `Db::with_conn` only hands out a pooled
/// connection; under SQLite's autocommit the two statements used to be two separate commits, and two
/// concurrent refreshes of the same component could interleave into A's template with B's hash.
///
/// GUARDS:
/// * the template must not be BLANK ([`ensure_not_blank`]) — this REPLACES the row's content, so a
///   whitespace-only synthesis is an ERASURE dressed as a write (see that function);
/// * the template must be LOGIC-LESS (PDR-0010): a Handlebars BLOCK helper (`{{#if}}`/`{{#each}}`/
///   `{{^…}}`) is REFUSED with `invalid_argument` (an LLM synthesis can contain anything, and one
///   poisoned template would fail the PROJECT-WIDE `/design` render);
/// * every `params_json` descriptor must declare a KNOWN `type`
///   ([`ensure_params_json_accepted`], finding `01KYPSG0HPG2D28CMJEXRKNEVK`) — an unknown word used to
///   become a `text` field in silence, dropping the `options` that made it a constrained choice;
/// * it never touches a prototype: the design LOCK (`prototype_design_lock`) freezes a ROUND's
///   ELECTION — the composition tree + placement — while PDR-0010 carries the design-system as a
///   REFERENCE, not injected content. Refreshing a catalogue component therefore mutates nothing
///   frozen, and the tool has no round to unfreeze — the same deliberate scoping already PINNED for
///   `proto_author_component` (review #34,
///   `mcp_tools_proto::tests::authoring_a_referenced_new_component_after_lock_keeps_it_new_until_it_is_materialized`).
///
/// DIAGNOSTICS (`warnings[]`, never a refusal — findings `01KYPSG1HM2GPJMNH2SP3DDADH` then
/// `01KYQ0R84KVF6QVQXQ9YVG9Z8T`): the whole VARIANCE contract is checked and reported, so a
/// synthesis whose axes cannot change a pixel — an inert value, a boolean param, a variant declared
/// both as a param and as an axis, a param still typed `state` — is stored WITH the component, the
/// faulty field and the correction named. See [`variance_warnings`] for why that one warns where the
/// param type refuses.
///
/// Mutating → `Prototypes` changed (an open `/design` catalogue surface re-pulls).
pub fn design_refresh_component(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let name = require_str(args, "name")?;
    let template = require_str(args, "template")?;
    let params_json = optional_str(args, "params_json")?;
    // The tolerated MAP form is CANONICALIZED before anything reads or stores it, so an axis ORDER
    // never depends on how a JSON object is iterated ([`canonical_states_json`]).
    let states_json = canonical_states_json(optional_str(args, "states_json")?);
    let states_json = states_json.as_deref();
    let expected_input_hash = optional_str(args, "input_hash")?;
    ensure_not_blank(template)?;
    ensure_template_accepted(template)?;
    // A param type the UI cannot render is REFUSED (it used to become a `text` field in silence) —
    // and so is a `params_json` / `states_json` whose SHAPE makes it declare nothing at all
    // (finding `01KZ3JS6WFVGYS6JKB9C4J50HR`).
    ensure_params_json_accepted(params_json)?;
    ensure_states_json_accepted(states_json)?;
    // The SCOPED CSS SLOT — the strict escape hatch for what utilities cannot express. Structural
    // validation + the scoping happen here (pure); the token-existence half runs in the transaction.
    // OMITTING the argument leaves the stored exception alone; `css_slot: ""` removes it.
    let css_slot = read_css_slot_arg(args, name)?;
    // The VARIANCE contract is DIAGNOSED, not enforced (see `variance_warnings`): the synthesis is
    // stored either way, and the agent is told what its axes will (not) do.
    let variance = variance_warnings(name, template, states_json, params_json);
    let result = db
        .with_conn(
            |conn| -> Result<Result<Value, RpcError>, diesel::result::Error> {
                // ONE transaction for the CONTENT + the HASH: they only mean anything together.
                conn.transaction(
            |c| -> Result<Result<Value, RpcError>, diesel::result::Error> {
                // The REFERENCE contract needs the CATALOGUE, so it is read here and not beside the
                // variance one: what `data-nyx-use` names has to exist, and the graph has to be
                // acyclic, for THIS project (finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`).
                let mut warnings = variance.clone();
                warnings.extend(reference_warnings(
                    name,
                    template,
                    &reference_catalogue(c, project_id)?,
                ));
                if let Err(refusal) = ensure_slot_tokens(c, project_id, &css_slot)? {
                    return Ok(Err(refusal));
                }
                let Some(existing) = db::get_component_by_name(c, project_id, name)? else {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidId,
                        format!(
                            "component '{name}' is not in the catalogue of project {project_id} — \
                             this tool REFRESHES an existing component; to create a greenfield \
                             [NEW] one, use proto_author_component"
                        ),
                    )));
                };
                let Some(_) = db::update_component_content(
                    c,
                    &existing.id,
                    template,
                    params_json,
                    states_json,
                )?
                else {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidId,
                        format!("component '{name}' vanished while being refreshed"),
                    )));
                };
                // The SLOT is written separately, and an OMITTED `css_slot` leaves it alone: a
                // re-synthesis of the template is not a decision about an exception a human already
                // reviewed. `css_slot: ""` removes it, explicitly.
                let Some(component) = db::set_component_css_slot(c, &existing.id, &css_slot.write())?
                else {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidId,
                        format!("component '{name}' vanished while being refreshed"),
                    )));
                };
                // Record the bundle's INPUT hash at synthesis time, so an unchanged bundle is never
                // re-synthesized — but ONLY if the bundle is still at the state the agent read
                // (`expected_input_hash`). A greenfield `[NEW]` (no on-disk source) simply has none.
                let record = design::mark_synthesized_at(c, project_id, name, expected_input_hash)?;
                let synthesized = !record.source_moved;
                let component = record.component.unwrap_or(component);
                let input_hash = component.visual_hash.clone();
                Ok(Ok(json!({
                    // Bounded like every echoed row: the row carries the whole stored `template`,
                    // and an agent that just sent a large one does not need it read back verbatim.
                    "component": bounded_row(&json!(component)),
                    "input_hash": input_hash,
                    // What the SYNTHESIS will not be able to do, said at the write (empty = nothing
                    // to report). Finding `01KYPSG1HM2GPJMNH2SP3DDADH`.
                    "warnings": &warnings,
                    // The EXCEPTION this component now carries, echoed with its scope, its compiled
                    // CSS and its reason — `null` when it carries none. Read off the STORED row, so
                    // omitting the argument still tells the agent the component has one (finding
                    // `01KYVMTWB71R0D0CT1SV88J8DX`: an exception is never something a reader has to
                    // go looking for).
                    "css_slot": stored_slot_echo(name, component.css_slot_json.as_deref()),
                    "synthesized": synthesized,
                    "source_changed": record.source_moved,
                    "current_input_hash": record.current_input_hash,
                    "note": if record.source_moved {
                        Some(format!(
                            "the bundle's sources CHANGED while you were synthesizing '{name}': \
                             your template was stored but NOT marked up to date — re-read the \
                             bundle (design_inventory with component='{name}') and refresh again \
                             quoting the new input_hash"
                        ))
                    } else {
                        None
                    },
                })))
            },
        )
            },
        )
        .map_err(internal_db)??;
    Ok(ToolOutcome::changed(result, ChangedTopic::Prototypes))
}

/// How many bytes of the RESOLVED markup [`design_preview`] hands back. A real component resolves to
/// a few hundred bytes and a whole page-sized one to a few kilobytes, so this is only ever reached by
/// something pathological (a fan-out of references, a param carrying a document) — or by a WHOLE
/// COMPOSITION, which is the one subject that legitimately gets there. The overflow is CUT rather
/// than dropped — unlike a `*_json` payload, a PREFIX of markup is still readable, and the root
/// element with its resolved classes is the first thing in it — and `truncated` + `html_bytes` say
/// so, so a shortened preview is never read as the whole rendering.
const MAX_PREVIEW_HTML_BYTES: usize = 32 * 1024;

/// How many UNRENDERED nodes a composition answer NAMES individually. The COUNT is always the run's
/// real count ([`MAX_PREVIEW_HTML_BYTES`]'s discipline, applied to a list): a composition whose every
/// node is a `[NEW]` says how many there are and names the first ones, never the other way round.
const MAX_UNRENDERED_REPORTED: usize = 30;

/// WHAT THE RESOLVED MARKUP CANNOT TELL THE AGENT — returned on EVERY [`design_preview`] answer.
///
/// The whole point of the finding (`01KYT0Z3RYGF3A881SDQN0X9D7`) is that an agent shipped broken
/// components believing the tools had validated them. Handing it markup fixes half of that and could
/// easily create the other half: "I rendered it, therefore I checked it". So the tool says, in the
/// payload itself and at every call, exactly where its own sight stops. The agent sees a CLASS; it
/// does not see the pixel that class produces. The composition subject (finding
/// `01KYT45C6RC7K84V1CA3561SPF`) reopens NOTHING here: a whole page is more markup, not more sight,
/// and a width tells you which responsive classes are IN FORCE, never how the page reflows at it.
const PREVIEW_CANNOT_SHOW: &str = "This is RESOLVED MARKUP, not an image, and it is NOT a visual \
     validation. It proves what is THERE: which elements exist at this combination, which classes \
     are really applied, which params really substituted. It proves NOTHING about how it LOOKS — no \
     colour (you see `bg-primary`, not whether the token behind it resolves or contrasts), no \
     spacing, no alignment, no size, no overflow, no font, no z-order, no state on hover/focus. A \
     class you invented and a class the theme defines read identically here. A whole COMPOSITION is \
     no different — it is more markup, not more sight: nothing here says whether the page fits, \
     whether two blocks collide, or what it REFLOWS to at a width (a viewport tells you which \
     responsive classes are in force, never what they produce). nyx renders NO IMAGE: the sidecar \
     has no rendering engine, nothing in the product captures one, and an image would not fit a \
     response anyway — do not wait for one and do not pretend you have one. Say what you CHECKED \
     (structure) and what you did NOT (the rendering) — the human is still the only eye on the \
     visual.";

/// `design_preview` — RESOLVE what you wrote into the markup a frame really DISPLAYS. ONE tool, TWO
/// SUBJECTS, named by the argument that identifies WHAT is rendered (never by a `mode` flag):
///
/// * `{ project_id, component, axes?, params?, children?, viewport? }` → ONE catalogue COMPONENT at
///   ONE axis combination and ONE param set ([`preview_component`]);
/// * `{ variant_id, node_key?, viewport? }` → a whole proto COMPOSITION — the variant's tree, every
///   node resolved ([`preview_composition`], finding `01KYT45C6RC7K84V1CA3561SPF`).
///
/// # Why ONE tool and not two
///
/// It is the same question ("what does what I wrote look like?"), the same resolution (the shim's
/// applier run ahead of time), the same honesty (`cannot_show`) and the same bounding. Two tools
/// would have been two loops to teach, two caps to keep aligned and two places for the honesty
/// paragraph to drift. What makes ONE tool readable is that its subject is named by its OWN
/// argument: you pass `component` OR `variant_id`, and the answer echoes `subject`. Naming both, or
/// neither, is an `invalid_argument` that says which one to pick.
pub fn design_preview(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let component = optional_str(args, "component")?;
    let variant_id = optional_str(args, "variant_id")?;
    let viewport = viewport_arg(args)?;
    match (component, variant_id) {
        (Some(_), Some(_)) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            "name ONE subject: `component` renders a catalogue component, `variant_id` renders a \
             whole proto composition. Both were given, and they are two different renderings"
                .to_string(),
        )),
        (None, None) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            "name what to render: `component` (with `project_id`) for ONE catalogue component, or \
             `variant_id` for a whole proto COMPOSITION (the variant's tree — what you present to \
             the human). proto_add_variant returns that id when you compose; the one already \
             elected is `chosen_variant_id` in proto_get's round manifest, and `elected_variant_id` \
             in proto_composition"
                .to_string(),
        )),
        (Some(name), None) => preview_component(db, args, name, viewport),
        (None, Some(variant_id)) => preview_composition(db, args, variant_id, viewport),
    }
}

/// A VIEWPORT the caller asked about: the width in CSS px, and the name it was asked by.
struct ViewportRequest {
    width: u32,
    named: Option<String>,
}

/// The `viewport` argument — a NUMBER of CSS px (`390`), or one of the three named widths
/// (`mobile` / `tablet` / `desktop`). Absent → `None` (no viewport report at all).
///
/// It is deliberately NOT a rendering parameter: see [`crate::render::viewport`] — the sidecar lays
/// nothing out, so this width is (a) what a frame must apply to display the markup and (b) what
/// decides which responsive CLASSES are in force. Anything else would be decoration.
fn viewport_arg(args: &Value) -> Result<Option<ViewportRequest>, RpcError> {
    let bad = |got: &Value| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "`viewport` must be a WIDTH in CSS px (e.g. 390) or one of the named widths {} — \
                 got {got}",
                crate::render::viewport::NAMED_VIEWPORTS
                    .iter()
                    .map(|(name, px)| format!("'{name}' ({px}px)"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    };
    match present_value(args, "viewport") {
        None => Ok(None),
        Some(Value::Number(n)) => {
            let width = n
                .as_u64()
                .filter(|w| *w > 0)
                .ok_or_else(|| bad(&json!(n)))?;
            Ok(Some(ViewportRequest {
                width: width.min(u32::MAX as u64) as u32,
                named: None,
            }))
        }
        Some(Value::String(s)) => {
            if let Some(width) = crate::render::viewport::named_viewport(s) {
                return Ok(Some(ViewportRequest {
                    width,
                    named: Some(s.trim().to_ascii_lowercase()),
                }));
            }
            let digits = s.trim().trim_end_matches("px").trim();
            let width: u32 = digits.parse().map_err(|_| bad(&json!(s)))?;
            if width == 0 {
                return Err(bad(&json!(s)));
            }
            Ok(Some(ViewportRequest { width, named: None }))
        }
        Some(other) => Err(bad(other)),
    }
}

/// The `viewport` block of an answer: the width, the breakpoints in force, the responsive variants
/// the markup really carries — and the sentence that says what a width does NOT do here.
fn viewport_answer(
    markup: &str,
    request: &ViewportRequest,
    tokens: &std::collections::BTreeMap<String, String>,
) -> (Value, Vec<String>) {
    let breakpoints = crate::render::viewport::breakpoints(tokens);
    let from_project = breakpoints.first().is_some_and(|b| b.from_project);
    let report =
        crate::render::viewport::report(markup, request.width, request.named.clone(), breakpoints);
    let mut warnings = Vec::new();
    if !report.is_responsive() {
        warnings.push(format!(
            "this markup carries NO responsive class (no `md:` / `lg:` / `max-*` variant anywhere), \
             so it renders IDENTICALLY at {}px and at every other width — nothing in it adapts. If \
             this design is meant to be responsive, that adaptation is not in what you wrote{}",
            report.width,
            if report.container_classes > 0 {
                format!(
                    " (it does carry {} CONTAINER-query class(es), which follow an ancestor's \
                     width, not the viewport's)",
                    report.container_classes
                )
            } else {
                String::new()
            }
        ));
    }
    let value = json!({
        "width": report.width,
        "named": report.named,
        "breakpoint_source": if from_project { "project_tokens" } else { "tailwind_defaults" },
        "breakpoints": report.breakpoints.iter().map(|b| json!({
            "name": b.name, "min_width": b.min_width, "active": b.active,
        })).collect::<Vec<_>>(),
        "variants": report.variants.iter().map(|v| json!({
            "prefix": v.prefix, "occurrences": v.occurrences, "active": v.active,
        })).collect::<Vec<_>>(),
        "responsive_classes": report.responsive_classes,
        "container_classes": report.container_classes,
        "note": "THE SIDECAR LAYS NOTHING OUT: this markup is byte for byte the same at every \
                 width, and no width here reflows anything. A viewport does exactly two real \
                 things — it is the width a FRAME must apply to display this markup, and it says \
                 which responsive CLASSES are in force (`breakpoints[].active`, \
                 `variants[].active`). What those classes then PRODUCE is a rendering, and this \
                 answer never sees one.",
    });
    (value, warnings)
}

/// `design_preview` with a COMPONENT subject — `{ project_id, component, axes?, params?, children?,
/// viewport? }` → `{ subject, component, html, html_bytes, truncated, axes, params, viewport,
/// warnings, cannot_show }`. RESOLVE one catalogue component into the markup a frame really
/// DISPLAYS, at ONE axis combination and ONE param set.
///
/// # The gap it closes (finding `01KYT0Z3RYGF3A881SDQN0X9D7`, MAJOR)
///
/// Every other diagnostic on this surface is STRUCTURAL — a missing case, an inert value, an unknown
/// key, a refused type, a `required` param nobody provides. None of them looks at a RENDERING, and
/// the measurable consequence on this very feature is that every visual defect (a spinner that never
/// hid, a loader, the colours) was found by the HUMAN and by no tool. An agent could therefore
/// synthesize a broken component, receive `ok`, and have no way to iterate on its own.
///
/// nyx already knew how to render — `render_composition` is what the `design_render` db-command
/// feeds the frame with, and it is the ONLY display path there is. What was missing was OFFERING
/// that to the agent that writes the component. This tool is that seam, and it goes one step
/// FURTHER than the db-command: it also runs the shim's applier ([`crate::render::resolve_component`]),
/// so the answer is what the frame ends up displaying, not what it receives before the shim touches
/// it. Concretely the agent can now see, without a browser: a spinner that is present with no
/// condition (it is still there at `loading=off`), a class that never lands (the axis resolved
/// elsewhere), a reference that did not resolve (an explicit refusal instead of a hole), a param
/// that substitutes to nothing.
///
/// # What it resolves, and by which rule
///
/// * AXES — `axes: { "variant": "ghost" }` fixes what you name; every axis you omit renders at its
///   own `default`, else its first value (the catalogue vignette's rule, read ONCE in
///   [`crate::design_axes::active_combination`]). The combination actually used comes BACK in the
///   answer, so a value the axis does not declare is visibly not the one you asked for.
/// * PARAMS — `params: { "label": "Payer" }` wins; a param you omit falls back to its `preview`,
///   then to its `default`, then to NOTHING — the exact `preview > default > rien` of the aperçu, so
///   this answer IS what the catalogue tile shows. A declared param that ends up empty is REPORTED:
///   that is what an empty box in the frame is made of.
/// * REFERENCES / SLOT — `data-nyx-use` is expanded and `children` fills `{{{children}}}`, both on
///   the render's own budgets, so a preview refuses exactly what the render refuses.
///
/// READ-ONLY: it stores nothing, changes nothing, notifies nobody. It can be called as often as the
/// loop needs.
fn preview_component(
    db: &Db,
    args: &Value,
    name: &str,
    viewport: Option<ViewportRequest>,
) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let requested_axes = object_arg(args, "axes")?;
    let requested_params = object_arg(args, "params")?;
    let children = optional_str(args, "children")?.unwrap_or_default();

    let (catalogue, tokens) = db
        .with_conn(|c| {
            Ok((
                reference_catalogue(c, project_id)?,
                crate::design_tokens::project_token_layer(c, project_id)?.variables,
            ))
        })
        .map_err(internal_db)?;
    let Some(component) = catalogue.iter().find(|c| c.name == name).cloned() else {
        return Err(RpcError::new(
            RpcCode::InvalidId,
            format!(
                "component '{name}' is not in the catalogue of project {project_id} — list the \
                 catalogue with design_inventory(project_id) and use the name it returns (two \
                 components sharing a base name get distinct path-qualified names)"
            ),
        ));
    };
    if component.template.trim().is_empty() {
        return Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "component '{name}' carries NO template yet — there is nothing to resolve. READ its \
                 bundle (design_inventory(project_id, component: \"{name}\")), SYNTHESIZE it and \
                 STORE it (design_refresh_component), then preview it"
            ),
        ));
    }

    let axes = parse_axes(component.states_json.as_deref());
    let declared = declared_param_specs(component.params_json.as_deref());
    let mut warnings: Vec<String> = Vec::new();

    // AN AXIS PASSED AS A PARAM is what a composition NODE does (a node's params carry its axis
    // values, and the shim reads them from there), so it is honoured rather than dropped — but it is
    // said, because `axes` is where this tool asks for them and silence here would make an agent
    // believe a param moved the rendering.
    let mut fixed: Vec<(String, String)> = Vec::new();
    let mut push_axis = |axis: &str, raw: &Value, warnings: &mut Vec<String>| {
        let Some(value) = raw.as_str() else {
            warnings.push(format!(
                "axes.{axis} must be a STRING naming one of the axis's values — {raw} was ignored"
            ));
            return;
        };
        match axes.iter().find(|a| a.name == axis) {
            None => warnings.push(format!(
                "component '{name}' declares NO axis '{axis}' — it was ignored. Its axes are: {}",
                named_axes(&axes)
            )),
            Some(spec) if spec.value(value).is_none() => warnings.push(format!(
                "axis '{axis}' of '{name}' does not declare the value '{value}' — it resolved at \
                 '{}' instead. Its values are: {}",
                spec.default_value(),
                spec.values
                    .iter()
                    .map(|v| format!("'{}'", v.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Some(_) => fixed.push((axis.to_string(), value.to_string())),
        }
    };
    for (axis, raw) in &requested_axes {
        push_axis(axis, raw, &mut warnings);
    }

    // PARAMS — the caller's value, else `preview`, else `default`, else nothing. The order is the
    // aperçu's, so this answer is what the catalogue tile really shows.
    let mut params = serde_json::Map::new();
    let mut resolved: Vec<Value> = Vec::new();
    let mut empty: Vec<&str> = Vec::new();
    for spec in &declared {
        let supplied = requested_params
            .iter()
            .find(|(key, value)| key == &spec.name && !value.is_null())
            .map(|(_, value)| value);
        let (value, source) = match supplied {
            Some(value) => (value.clone(), "requested"),
            None => match (&spec.preview, &spec.default) {
                (Some(preview), _) => (preview.clone(), "preview"),
                (None, Some(default)) => (default.clone(), "default"),
                (None, None) => (Value::String(String::new()), "none"),
            },
        };
        let renders_empty = match &value {
            Value::String(s) => s.is_empty(),
            Value::Null => true,
            _ => false,
        };
        // `renders_empty` stays REPORTED per param below whatever happens — the fact is the fact.
        // What is TRIAGED here is the WARNING: only an emptiness that leaves a hole a human would
        // SEE is worth an alarm (see [`empty_leaves_a_visible_hole`]).
        if renders_empty && empty_leaves_a_visible_hole(spec, &component.template) {
            empty.push(&spec.name);
        }
        resolved.push(json!({
            "name": spec.name,
            "source": source,
            "required": spec.required,
            "renders_empty": renders_empty,
            "value": bounded_text(&value_as_text(&value), MAX_ROW_FIELD_BYTES).0,
        }));
        params.insert(spec.name.clone(), value);
    }
    for (key, value) in &requested_params {
        if declared.iter().any(|p| &p.name == key) {
            continue;
        }
        if axes.iter().any(|a| &a.name == key) {
            // AN AXIS PASSED AS A PARAM is what a composition NODE does (a node's params carry its
            // axis values, and that is where the shim reads them), so it is HONOURED rather than
            // dropped — and said, because silence here would let an agent believe a param moved the
            // rendering.
            warnings.push(format!(
                "'{key}' is an AXIS of '{name}', not a param — pass it in `axes` (it was applied as \
                 an axis here, which is what a composition node's params mean, but the two are \
                 different notions: an axis APPLIES CLASSES, a param is SUBSTITUTED)"
            ));
            push_axis(key, value, &mut warnings);
            continue;
        }
        // A key the component declares NOWHERE is still substituted (a template can carry a mustache
        // its params_json forgot), and it is NAMED — a preview that silently drops the caller's own
        // value is exactly the silence this tool exists to break.
        warnings.push(format!(
            "component '{name}' declares NO param '{key}' (params_json declares: {}) — it was \
             substituted anyway, so a mustache in the template still picks it up, but the catalogue \
             contract does not know it. Declare it in params_json, or fix the name",
            named_params(&declared)
        ));
        params.insert(key.clone(), value.clone());
    }
    // THE ACTIVE AXIS VALUES ARE ALSO SUBSTITUTABLE, exactly as they are in the frame: a composition
    // node's params carry them, so `class="btn btn-{{state}}"` — the legitimate substitution escape
    // hatch the variance model documents — renders `btn btn-ghost` there. A preview that left them
    // out would show that whole family of components with an empty class and send the agent chasing
    // a defect nyx invented.
    let active = crate::design_axes::active_combination(&axes, &fixed);
    for (axis, value) in &active {
        params.insert((*axis).to_string(), Value::String((*value).to_string()));
    }
    if !empty.is_empty() {
        warnings.push(format!(
            "these declared param(s) resolved to NOTHING and leave a VISIBLE HOLE — the template \
             substitutes them into CONTENT (or marks them `required`), so empty means a missing \
             label, a missing text, an empty box: {} — that is what a blank tile in the frame is \
             made of. Pass a value in `params` to try one out, and give each one a `preview` in \
             params_json so the catalogue tile stops rendering blank ({} required, which have no \
             `default` by definition). A param whose emptiness changes NOTHING a human sees — a \
             passthrough substituted only inside an attribute, `class` being the usual one — is not \
             reported here; read `params[].renders_empty` for the full, untriaged truth",
            empty.join(", "),
            declared
                .iter()
                .filter(|p| p.required && empty.contains(&p.name.as_str()))
                .count()
        ));
    }

    let resolution = crate::render::resolve_component(&catalogue, name, &fixed, &params, children)
        .map_err(|e| RpcError::new(RpcCode::InvalidArgument, e.to_string()))?;
    let html_bytes = resolution.markup.len();
    let (html, truncated) = bounded_text(&resolution.markup, MAX_PREVIEW_HTML_BYTES);
    let viewport = viewport.map(|request| {
        let (value, notes) = viewport_answer(&resolution.markup, &request, &tokens);
        warnings.extend(notes);
        value
    });

    Ok(ToolOutcome::read(json!({
        "subject": "component",
        "component": name,
        "html": html,
        // The TOTAL of the run, never the length of the head that was kept — a cut answer must not
        // become an under-declared one.
        "html_bytes": html_bytes,
        "html_returned_bytes": html.len(),
        "truncated": truncated,
        "viewport": viewport,
        // The combination this markup was resolved AT, in declaration order — never an object, whose
        // key order is the serializer's opinion and not the component's (finding
        // `01KYSNPWWG9DA0AXP6D22GV5BK`: the FIRST axis is a verdict, not a presentation detail).
        "axes": resolution.axes.iter().map(|(axis, value)| json!({ "axis": axis, "value": value }))
            .collect::<Vec<_>>(),
        "params": resolved,
        // The SCOPED CSS SLOTS this resolution really applied — the component's own, plus any
        // referenced sub-component's. Each names the scope it is bounded to, the compiled CSS, the
        // constructs it uses and the REASON it exists. Empty for nearly every component; never
        // silent when it is not (finding `01KYVMTWB71R0D0CT1SV88J8DX`).
        "css_slots": css_slots_payload(&resolution.css_slots),
        "css_slot_count": resolution.css_slots.len(),
        "warnings": warnings,
        "cannot_show": PREVIEW_CANNOT_SHOW,
    })))
}

/// The payload form of the CSS slots a render emitted — the SAME shape everywhere it is reported
/// (the component subject, the composition subject, the `design_render` IPC command), so a display
/// surface reads one shape and only one.
pub(crate) fn css_slots_payload(slots: &[crate::render::RenderedCssSlot]) -> Vec<Value> {
    slots
        .iter()
        .map(|s| {
            json!({
                "scope_id": s.scope_id,
                "component": s.component,
                "scope": s.selector,
                "reason": s.reason,
                "features": s.features,
                "bytes": s.bytes,
                "css": s.css,
                "refused": s.refused,
                "detail": s.detail,
            })
        })
        .collect()
}

/// The identity of a variant, resolved up its whole chain — variant → page version → page →
/// prototype → Spec → PROJECT. The last one is what says which CATALOGUE the tree's nodes are
/// rendered against, and the agent never has to pass it.
struct VariantSubject {
    variant: db::PrototypeVariant,
    page_slug: String,
    page_title: String,
    prototype_id: String,
    spec_id: String,
    project_id: String,
    /// The prototype's PAGE DIRECTORY — what a node's `navigate_to` resolves against (finding
    /// `01KYW34347CZSNXCYS35KDG3EY`). Read at the RENDER, so a page renamed since the link was
    /// written comes back under its new name, and a page that is GONE comes back named as gone.
    pages: Vec<crate::render::RenderPage>,
}

/// Everything the composition preview reads in ONE connection: the variant's identity, the
/// CATALOGUE its nodes resolve against, and the project's TOKEN layer (which is where the
/// breakpoints come from when the project declares its own).
type CompositionRead = (
    VariantSubject,
    Vec<crate::render::RenderComponent>,
    std::collections::BTreeMap<String, String>,
);

/// `design_preview` with a COMPOSITION subject — `{ variant_id, node_key?, viewport? }` →
/// `{ subject, variant, node_key, html, html_bytes, html_returned_bytes, truncated, node_count,
/// unrendered, unrendered_count, viewport, warnings, cannot_show }`.
///
/// # The gap it closes (finding `01KYT45C6RC7K84V1CA3561SPF`, MAJOR)
///
/// A COMPOSITION is THE deliverable shown to the human — and nothing could render one for the agent
/// that composed it. `design_preview` gave it eyes on ONE component; `render_composition` fed the
/// FRAME (before the shim, and strictly). So an agent presented a round it had never seen, and every
/// defect of the page — not of a component: of the PAGE — was found by the human.
///
/// This resolves the WHOLE tree the way [`crate::render::resolve_composition`] does: every node
/// substituted, references flattened, each node's own axis combination applied (a node carries its
/// axis values in its params, which is where the shim reads them), nothing of `data-nyx-*` left.
///
/// # It ALWAYS COMPLETES, and names what it could not render
///
/// A composition under design legitimately holds `[NEW]` nodes nobody has synthesized. Refusing the
/// whole tree on the first of them would hide the forty nodes that ARE right — the very defect the
/// human reports on the UI side (a loader spinning where a placeholder belongs). So an unrenderable
/// node becomes a PLACEHOLDER naming itself in the markup, plus an entry in `unrendered`, and the
/// render goes on. The only refusals are: an unknown variant, a variant with no composition yet, a
/// tree that does not PARSE, and a `node_key` that names no node.
///
/// READ-ONLY: it stores nothing, changes nothing, notifies nobody.
fn preview_composition(
    db: &Db,
    args: &Value,
    variant_id: &str,
    viewport: Option<ViewportRequest>,
) -> Result<ToolOutcome, RpcError> {
    let node_key = optional_str(args, "node_key")?;

    let (subject, catalogue, tokens) = db
        .with_conn(
            |c| -> Result<Result<CompositionRead, RpcError>, diesel::result::Error> {
                let Some(variant) = db::get_variant(c, variant_id)? else {
                    return Ok(Err(RpcError::new(
                        RpcCode::InvalidId,
                        format!(
                            "unknown variant {variant_id} — proto_add_variant returns the id of \
                             the variant it creates, proto_get(spec_id)'s round manifest carries \
                             the elected one (`chosen_variant_id`), and proto_composition carries \
                             it as `elected_variant_id`"
                        ),
                    )));
                };
                // The chain up to the PROJECT: it is what says which catalogue this tree's nodes
                // are rendered against, so a broken link is a legible refusal, never a blank page.
                let orphan = |what: &str| {
                    Err(RpcError::new(
                        RpcCode::InvalidState,
                        format!(
                            "variant {variant_id} is not reachable from a project ({what} is \
                             missing), so nyx cannot tell which component catalogue to render it \
                             against"
                        ),
                    ))
                };
                let Some(version) = db::get_page_version(c, &variant.page_version_id)? else {
                    return Ok(orphan("its page version"));
                };
                let Some(page) = db::get_page(c, &version.page_id)? else {
                    return Ok(orphan("its page"));
                };
                let Some(prototype) = db::get_prototype(c, &page.prototype_id)? else {
                    return Ok(orphan("its prototype"));
                };
                let Some(spec) = db::get_spec(c, &prototype.spec_id)? else {
                    return Ok(orphan("its Spec"));
                };
                let catalogue = reference_catalogue(c, &spec.project_id)?;
                let tokens =
                    crate::design_tokens::project_token_layer(c, &spec.project_id)?.variables;
                let pages = db::render_pages(c, &prototype.id)?;
                Ok(Ok((
                    VariantSubject {
                        variant,
                        page_slug: page.slug,
                        page_title: page.title,
                        prototype_id: prototype.id,
                        spec_id: prototype.spec_id,
                        project_id: spec.project_id,
                        pages,
                    },
                    catalogue,
                    tokens,
                )))
            },
        )
        .map_err(internal_db)??;

    if subject.variant.composition_json.trim().is_empty() {
        return Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "variant '{}' ({variant_id}) carries NO composition tree yet — there is nothing to \
                 render. A variant's `composition_json` IS the design (a NAMED COMPONENT TREE, not \
                 an HTML blob): compose it with proto_add_variant(page_version_id, label, title, \
                 composition_json)",
                subject.variant.label
            ),
        ));
    }

    // THE VARIANT'S OWN AXES (finding `01KZ3JS7PNF3F8GFB3GYRK6CKH`). They pilot the WHOLE page in
    // the frame — one chip-bar row per axis — and this resolution used to read them nowhere: an
    // element gated on one of them stayed visible at every value, `axes` was absent from the answer
    // and asking for a value changed nothing. The requested combination is confronted with what the
    // variant DECLARES, exactly as the component subject does with its own (`push_axis` above), so a
    // name or a value the variant does not have is NAMED instead of quietly doing nothing.
    let declared_axes = parse_axes(subject.variant.states_json.as_deref());
    let mut warnings: Vec<String> = Vec::new();
    let mut fixed_axes: Vec<(String, String)> = Vec::new();
    for (axis, raw) in object_arg(args, "axes")? {
        let Some(value) = raw.as_str() else {
            warnings.push(format!(
                "axes.{axis} must be a STRING naming one of the axis's values — {raw} was ignored"
            ));
            continue;
        };
        match declared_axes.iter().find(|a| a.name == axis) {
            None if declared_axes.is_empty() => warnings.push(format!(
                "variant '{}' declares NO axis at all, so `axes.{axis}` changed nothing. A \
                 variant's axes are its own `states_json` (proto_add_variant(page_version_id, \
                 label, title, composition_json, states_json)) — they are what a `data-nyx-if` on \
                 this page can be conditioned on, and what the frame shows one chip-bar row per",
                subject.variant.label
            )),
            None => warnings.push(format!(
                "variant '{}' declares NO axis '{axis}' — it was ignored. Its axes are: {}",
                subject.variant.label,
                named_axes(&declared_axes)
            )),
            Some(spec) if spec.value(value).is_none() => warnings.push(format!(
                "axis '{axis}' of variant '{}' does not declare the value '{value}' — it resolved \
                 at '{}' instead. Its values are: {}",
                subject.variant.label,
                spec.default_value(),
                spec.values
                    .iter()
                    .map(|v| format!("'{}'", v.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Some(_) => fixed_axes.push((axis.clone(), value.to_string())),
        }
    }

    let resolution = crate::render::resolve_composition(
        &catalogue,
        &subject.pages,
        &subject.variant.composition_json,
        node_key,
        subject.variant.states_json.as_deref(),
        &fixed_axes,
    )
    .map_err(|e| match &e {
        crate::render::RenderError::UnknownNode { .. } => {
            RpcError::new(RpcCode::InvalidId, e.to_string())
        }
        _ => RpcError::new(RpcCode::InvalidArgument, e.to_string()),
    })?;

    let html_bytes = resolution.markup.len();
    let (html, truncated) = bounded_text(&resolution.markup, MAX_PREVIEW_HTML_BYTES);
    if truncated {
        warnings.push(format!(
            "the resolved markup is {html_bytes} bytes and only the first {} are returned — a whole \
             page is bigger than a component, and this cap is what keeps the answer readable. Read \
             it SUBTREE BY SUBTREE: design_preview(variant_id, node_key: \"<a node's key>\") \
             renders one branch whole (the node keys are the `/`-joined path of the tree's keys, the \
             same anchors proto_composition and proto_feedback use)",
            html.len()
        ));
    }
    if !resolution.unrendered.is_empty() {
        warnings.push(format!(
            "{} of this composition's {} nodes could NOT be rendered and appear as PLACEHOLDERS in \
             the markup ({}): the render COMPLETED anyway — naming them beats failing the whole \
             tree — but what the human would see there is a hole, not a design. Read `unrendered` \
             for each one's correction",
            resolution.unrendered.len(),
            resolution.node_count,
            resolution
                .unrendered
                .iter()
                .take(5)
                .map(|u| format!("{} [{}]", u.node_key, u.reason.code()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // THE PARCOURS (finding `01KYW34347CZSNXCYS35KDG3EY`). The agent's resolution CONSUMES the
    // `data-nyx-*` vocabulary, navigation marks included — so this list is the only place an agent
    // can read where its page leads. A link that could not be honoured is warned about by name: a
    // dead click is exactly the kind of defect the human finds one by one when nobody looked.
    let broken: Vec<&crate::render::NavigationLink> = resolution
        .navigation
        .iter()
        .filter(|link| link.status != crate::render::NavigationStatus::Linked)
        .collect();
    if !broken.is_empty() {
        warnings.push(format!(
            "{} of this composition's {} navigation links do NOT lead anywhere ({}): the node states \
             a destination and the click would do NOTHING. Read `navigation` for each one — a \
             parcours with a dead link is the defect a prototype exists to catch BEFORE the human \
             clicks it",
            broken.len(),
            resolution.navigation.len(),
            broken
                .iter()
                .take(5)
                .map(|l| format!("{} [{}]", l.node_key, l.status.code()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let viewport = viewport.map(|request| {
        let (value, notes) = viewport_answer(&resolution.markup, &request, &tokens);
        warnings.extend(notes);
        value
    });

    Ok(ToolOutcome::read(json!({
        "subject": "composition",
        "variant": {
            "variant_id": subject.variant.id,
            "label": subject.variant.label,
            "title": subject.variant.title,
            "page": subject.page_slug,
            "page_title": subject.page_title,
            "page_version_id": subject.variant.page_version_id,
            "prototype_id": subject.prototype_id,
            "spec_id": subject.spec_id,
            "project_id": subject.project_id,
        },
        // Which subtree was rendered — the whole tree when the caller named no node.
        "node_key": node_key,
        // The VARIANT's own axis combination this markup was resolved at, in the variant's
        // declaration order — an empty list means the variant declares no axis (finding
        // `01KZ3JS7PNF3F8GFB3GYRK6CKH`). Never an object: the FIRST axis is a verdict, and a JSON
        // object's key order is the serializer's opinion.
        "axes": resolution.axes.iter().map(|(axis, value)| json!({ "axis": axis, "value": value }))
            .collect::<Vec<_>>(),
        // What this variant CAN be piloted on, with the values each axis really declares — so an
        // agent that wants another combination knows what to ask for without re-reading the row.
        "declared_axes": declared_axes.iter().map(|a| json!({
            "axis": a.name,
            "default": a.default_value(),
            "values": a.values.iter().map(|v| v.name.clone()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "html": html,
        // The TOTAL of the run, never the length of the head that was kept. A composition is the
        // payload most likely to be cut here, so this pair is what makes the cut HONEST.
        "html_bytes": html_bytes,
        "html_returned_bytes": html.len(),
        "truncated": truncated,
        "node_count": resolution.node_count,
        "unrendered": resolution.unrendered.iter().take(MAX_UNRENDERED_REPORTED).map(|u| json!({
            "node_key": u.node_key,
            "component": u.component,
            "reason": u.reason.code(),
            "detail": u.detail,
        })).collect::<Vec<_>>(),
        // The run's own count, whatever the list above kept.
        "unrendered_count": resolution.unrendered.len(),
        "unrendered_truncated": resolution.unrendered.len() > MAX_UNRENDERED_REPORTED,
        // The SCOPED CSS SLOTS this page really reaches — the exceptions to the design system, in
        // document order, each with its scope, its compiled CSS and the REASON it exists. Read this
        // before presenting a round: an exception nobody named is an exception nobody reviewed
        // (finding `01KYVMTWB71R0D0CT1SV88J8DX`).
        "css_slots": css_slots_payload(&resolution.css_slots),
        "css_slot_count": resolution.css_slots.len(),
        // THE PARCOURS this page states: one entry per node that NAVIGATES, with the destination
        // resolved to what a human reads (`slug` + `page_title`) from the id the tree stores. The
        // resolution consumed the `data-nyx-*` marks, so this is where the navigation IS.
        "navigation": resolution.navigation.iter().map(|l| json!({
            "node_key": l.node_key,
            "page_id": l.page_id,
            "page": l.slug,
            "page_title": l.title,
            "status": l.status.code(),
            "detail": l.detail,
        })).collect::<Vec<_>>(),
        "navigation_count": resolution.navigation.len(),
        "navigation_broken_count": broken.len(),
        "viewport": viewport,
        "warnings": warnings,
        "cannot_show": PREVIEW_CANNOT_SHOW,
    })))
}

/// An OBJECT argument (`axes`, `params`), read as `(key, value)` pairs in DOCUMENT order — the same
/// reason `states_json`'s map form is read that way (`serde_json::Map` is a `BTreeMap`, and an
/// alphabetical reading is a verdict nobody wrote). Absent / `null` → empty; anything else is an
/// `invalid_argument` naming the shape.
fn object_arg(args: &Value, key: &str) -> Result<Vec<(String, Value)>, RpcError> {
    match present_value(args, key) {
        None => Ok(Vec::new()),
        Some(Value::Object(map)) => Ok(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
        Some(other) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("`{key}` must be an OBJECT (name → value), got {other}"),
        )),
    }
}

/// A JSON value as the TEXT a template substitutes — a string verbatim, anything else serialized.
fn value_as_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The component's axis names, quoted — for a diagnostic that names what DOES exist.
fn named_axes(axes: &[AxisSpec]) -> String {
    if axes.is_empty() {
        return "none (states_json declares no axis)".to_string();
    }
    axes.iter()
        .map(|a| format!("'{}'", a.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The component's declared param names, quoted — the twin of [`named_axes`].
fn named_params(params: &[DeclaredParam]) -> String {
    if params.is_empty() {
        return "none".to_string();
    }
    params
        .iter()
        .map(|p| format!("'{}'", p.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The NON-BLANK guard applied before ANY agent-written template is STORED — here and on the `[NEW]`
/// author path ([`crate::mcp_tools_proto::proto_author_component`]). REFUSES a template that is empty
/// once trimmed (`"   "`, `"\n"`, a non-breaking space…), naming what the agent must do instead.
///
/// # Why blank is not "just an empty write" (audit 01KYHYVD5V78FED8WRZ8NKZVF0)
///
/// The conditional `DELETE` protects a row that CARRIES content (`db::carries_synthesis`). This is
/// the path that can make a row STOP carrying any, without anyone deciding to: `require_str` only
/// ever refused the EMPTY string, so `template: "   "` was accepted, `db::update_component_content`
/// wrote the blank AND nulled `params_json` / `states_json` (the shape a `[NEW]` was authored with,
/// which no scan can re-derive), and the tool answered `synthesized: true` for a synthesis that
/// contains nothing. The row then carried nothing — so the very next inventory whose source had moved
/// PRUNED it, entirely within its rights. The guard of the CRITICAL was not crossed; it was emptied
/// upstream. Refusing blank closes that in the same line of code that already refuses `""`.
///
/// EMPTYING a component is a real intention — it just is not this tool's: it is
/// `design_delete_component`, an EXPLICIT gesture. Trimming is Unicode-wide (`str::trim` uses
/// `char::is_whitespace`), so a NBSP-only "template" is blank too.
pub(crate) fn ensure_not_blank(template: &str) -> Result<(), RpcError> {
    if !template.trim().is_empty() {
        return Ok(());
    }
    Err(RpcError::new(
        RpcCode::InvalidArgument,
        "the template is BLANK (empty once trimmed): there is nothing to store. This tool REPLACES \
         the component's content, so a blank template would ERASE the stored template AND its \
         params_json / states_json — work no re-scan can re-derive — and leave the row eligible for \
         the automatic pruning. Send the synthesized Handlebars; to remove a component on purpose, \
         use design_delete_component."
            .to_string(),
    ))
}

/// The SIX param types the design-system UI can render a control for — the whole vocabulary of a
/// `params_json` descriptor's `type`, in the order the manual lists them. `state` is DEPRECATED
/// (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`: a variance axis is never a param) but stays ACCEPTED —
/// rows carry it, and refusing it would make a stored synthesis unstorable; [`variance_warnings`]
/// names it instead, with the axis to declare in its place.
pub(crate) const PARAM_TYPES: [&str; 6] = ["text", "html", "state", "boolean", "number", "select"];

/// THE PARAM-TYPE GUARD (finding `01KYPSG0HPG2D28CMJEXRKNEVK`): a descriptor whose `type` is not one
/// of [`PARAM_TYPES`] is REFUSED, with the offending param and the accepted values named.
///
/// # Why a REFUSAL here, when the state-case defect only WARNS
///
/// A test agent declared `{ "name": "buttonVariant", "type": "enum", "options": [...] }` — a
/// reasonable guess at an undocumented contract. The UI's parser fell back to `"text"` for the
/// unknown type, silently: `options` was dropped, the constrained choice became a free-text field,
/// and the write answered `ok`. The agent had no way to learn it, and the user got a broken control.
///
/// The defect is CERTAIN here (there is no reading of `"enum"` the UI can honour — unlike a
/// case-less template, which may legitimately vary through a `{{state}}` substitution), and the fix
/// costs ONE call: the agent is still holding the whole synthesis it just composed. A `warnings[]`
/// entry would ship the broken control anyway — a warning nobody reads is the silent fallback with
/// extra steps. So: refuse, name the word, and let it re-send.
///
/// RETRO-COMPATIBILITY is preserved by scope, not by leniency: the guard applies to a NEW WRITE
/// only. Rows already stored keep their `params_json` untouched and the UI keeps parsing them
/// defensively (an unknown type still renders as a text control, now VISIBLY flagged as coerced —
/// `params.ts` / `param-field.tsx`), so no catalogue breaks the day this lands. An OMITTED `type` is
/// tolerated too: it has always meant `text`, and a synthesis written before this contract said it
/// that way.
///
/// The same reasoning covers the shape itself: `params_json` that is not valid JSON, or not an array
/// / object of descriptors, is dropped WHOLESALE by the UI parser (the component then declares no
/// param at all) — the loudest silent degradation of the lot, refused here for the same reason.
///
/// # The WHOLE shape, not only the `type` word (finding `01KZ3JS6WFVGYS6JKB9C4J50HR`)
///
/// The guard used to stop at the vocabulary of `type`, so three malformations went through with
/// `synthesized: true` and `warnings: []`: an OBJECT root (`{ "label": { … } }`) instead of the
/// array the manual documents, a descriptor with NO `name`, and a descriptor that is not an object
/// at all. Each of them makes the entry declare NOTHING — the very outcome the type refusal exists
/// to prevent, reached by another road. A document nyx will parse LATER anyway must be parsed HERE,
/// where the agent still holds what it wrote.
///
/// The OBJECT root is refused rather than tolerated: the manual states an ARRAY of descriptors, an
/// object has nowhere to put `name` (its key doubles as one), and a JSON object's key order is the
/// serializer's opinion. It stays READABLE — [`declared_param_specs`] still parses rows already
/// stored that way — but nothing NEW is written in it, and the refusal spells out the array.
pub(crate) fn ensure_params_json_accepted(params_json: Option<&str>) -> Result<(), RpcError> {
    let Some(raw) = params_json else {
        return Ok(());
    };
    if raw.trim().is_empty() {
        return Ok(());
    }
    let shape_error = |detail: String| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "params_json {detail}. It is an ARRAY of param descriptors — \
                 [{{ \"name\": \"label\", \"type\": \"text\", \"required\": true, \
                 \"preview\": \"Enregistrer\" }}] — and a blob nyx cannot parse declares NO param \
                 at all: the composant detail would show an empty form for a component that has \
                 inputs"
            ),
        )
    };
    let parsed: Value =
        serde_json::from_str(raw).map_err(|e| shape_error(format!("is not valid JSON ({e})")))?;
    let Value::Array(items) = &parsed else {
        return Err(shape_error(format!(
            "is {} at its root, not an ARRAY",
            match &parsed {
                Value::Object(_) =>
                    "a JSON OBJECT — rewrite it as an array with one entry per param, each \
                     carrying its own `name` key (an object's keys are ordered by the serializer \
                     and have nowhere to put the rest of a descriptor)",
                Value::String(_) => "a string",
                Value::Number(_) => "a number",
                Value::Bool(_) => "a boolean",
                _ => "null",
            }
        )));
    };
    let mut descriptors: Vec<(String, &Value)> = Vec::with_capacity(items.len());
    for (index, entry) in items.iter().enumerate() {
        if !entry.is_object() {
            return Err(shape_error(format!(
                "carries a NON-OBJECT entry at position {index} ({entry}) — every entry is a \
                 descriptor object"
            )));
        }
        // A DESCRIPTOR WITHOUT A NAME DECLARES NOTHING: the name IS the mustache the template
        // substitutes, so an entry without one has no subject and every reader drops it in silence.
        let Some(name) = entry
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.trim().is_empty())
        else {
            return Err(shape_error(format!(
                "carries an entry at position {index} with no usable `name` ({entry}) — `name` is \
                 the MUSTACHE the template substitutes, so that entry declares nothing and every \
                 reader drops it. Give it the name the template writes"
            )));
        };
        descriptors.push((name.to_string(), entry));
    }
    for (name, entry) in descriptors {
        let Some(raw_type) = entry.get("type") else {
            continue; // omitted → `text`, the historical meaning.
        };
        let known = raw_type.as_str().is_some_and(|t| PARAM_TYPES.contains(&t));
        if !known {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "param '{name}' declares an UNKNOWN type {raw_type}: the accepted types are {} \
                     — and NOTHING else. This is refused rather than silently turned into a `text` \
                     field (which is what used to happen: your `options` were dropped and the \
                     constrained choice became a free-text input, with nothing saying so). For a \
                     constrained VALUE use \"select\" with its `options`; for a VARIANCE AXIS \
                     (variant, size, or a boolean like loading) do not use a param at all — declare \
                     it in states_json as an axis whose values carry their classes \
                     ({{\"axes\":[{{\"name\":\"variant\",\"values\":[{{\"name\":\"…\",\"classes\":\"…\"}}]}}]}}), \
                     and nyx renders one control per axis",
                    PARAM_TYPES
                        .iter()
                        .map(|t| format!("\"{t}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
    Ok(())
}

/// THE `states_json` SHAPE GUARD (finding `01KZ3JS6WFVGYS6JKB9C4J50HR`) — the twin of
/// [`ensure_params_json_accepted`], on the OTHER half of the model.
///
/// [`parse_axes`] is a DIAGNOSTIC reader by design: "a blob that is absent or malformed declares
/// NOTHING". That is the right rule for READING a stored row — a row nobody can fix now must not
/// break a render — and exactly the wrong one for a WRITE, where the author is still holding the
/// text. So four malformations that used to store with `warnings: []` are refused here, each naming
/// its field:
///
/// * the blob is not valid JSON — every reader then sees a component with no axis at all;
/// * its root is neither an object nor an array — same outcome, one step earlier;
/// * `axes` is present but is not an ARRAY — the canonical form's ONE structural key, and the shape
///   the manual's every example writes;
/// * an entry of `axes` is not an object, has no `name`, or its `values` is not a NON-EMPTY array —
///   an axis with no value is an axis nothing can select, so it declares nothing either.
///
/// It accepts everything [`parse_axes`] really reads: the canonical `{ "axes": [...] }`, the bare
/// array of axis objects, the tolerated map form `{ "variant": [...] }`, and the LEGACY flat list
/// `["default", "hover"]`. This is a SHAPE guard, never a semantic one — whether an axis value
/// actually changes a pixel stays a WARNING ([`variance_warnings`]), because that one is a judgement
/// and this one is a fact.
pub(crate) fn ensure_states_json_accepted(states_json: Option<&str>) -> Result<(), RpcError> {
    let Some(raw) = states_json else {
        return Ok(());
    };
    if raw.trim().is_empty() {
        return Ok(());
    }
    let shape_error = |detail: String| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "states_json {detail}. It declares the VARIANCE AXES — \
                 {{ \"axes\": [{{ \"name\": \"variant\", \"default\": \"primary\", \"values\": \
                 [{{ \"name\": \"primary\", \"classes\": \"bg-primary\" }}] }}] }} — and a blob nyx \
                 cannot read declares NO axis at all: every control disappears, every \
                 `data-nyx-if` on those axes stops discriminating, and the write would still have \
                 answered ok"
            ),
        )
    };
    let parsed: Value =
        serde_json::from_str(raw).map_err(|e| shape_error(format!("is not valid JSON ({e})")))?;
    // The entries to check as AXIS OBJECTS: the canonical `axes` array, or the bare array when it
    // really carries axis objects. Every other accepted shape (the map form, the legacy flat list)
    // has no axis object to validate.
    let entries: &[Value] = match &parsed {
        Value::Object(map) => match map.get("axes") {
            None => return Ok(()), // the tolerated MAP form — no `axes` key to validate.
            Some(Value::Array(items)) => items.as_slice(),
            Some(other) => {
                return Err(shape_error(format!(
                    "declares `axes` as {other}, which is not an ARRAY — `axes` is the ONE \
                     structural key of the canonical form, and anything else there is read as the \
                     tolerated map form and dropped"
                )))
            }
        },
        Value::Array(items) => {
            // The LEGACY flat list (no entry carries a `values` array) is a valid shape of its own.
            if !items
                .iter()
                .any(|e| e.get("values").is_some_and(Value::is_array))
            {
                return Ok(());
            }
            items.as_slice()
        }
        other => {
            return Err(shape_error(format!(
                "is {other} at its root, which is neither the canonical object \
                 ({{ \"axes\": [...] }}) nor an array of axes"
            )))
        }
    };
    for (index, entry) in entries.iter().enumerate() {
        if !entry.is_object() {
            return Err(shape_error(format!(
                "carries a NON-OBJECT axis at position {index} ({entry})"
            )));
        }
        let Some(name) = entry
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.trim().is_empty())
        else {
            return Err(shape_error(format!(
                "carries an axis at position {index} with no usable `name` ({entry}) — the name is \
                 what `data-nyx-if` and every control refer to"
            )));
        };
        match entry.get("values") {
            Some(Value::Array(values)) if !values.is_empty() => {}
            Some(Value::Array(_)) => {
                return Err(shape_error(format!(
                    "declares axis '{name}' with an EMPTY `values` — an axis with no value is an \
                     axis nothing can select"
                )))
            }
            other => {
                return Err(shape_error(format!(
                    "declares axis '{name}' with `values` = {} instead of a non-empty ARRAY — the \
                     values are what carry the resolved classes",
                    other.unwrap_or(&Value::Null)
                )))
            }
        }
    }
    Ok(())
}

// --- THE VARIANCE-AXES MODEL lives in [`crate::design_axes`] -------------------------------
//
// It moved DOWN to a leaf module when the RENDERER needed it too (finding
// `01KYSCZ822K4ZFAAYTG6GT6YWZ`: resolving a REFERENCED sub-component means resolving ITS axes to
// concrete classes in the sidecar). Re-imported here under its old names so this module — and the
// suite below it — reads exactly as before: ONE reading of a stored `states_json`, whoever asks.
use crate::design_axes::parse_condition_body;
pub(crate) use crate::design_axes::{
    canonical_states_json, parse_axes, primary_axis_states, AxisSpec, TemplateCondition,
};

/// Every value of a given quoted attribute the template carries, in order. Both quotings are read —
/// the template is authored by an LLM, not by a formatter.
fn attribute_values(template: &str, attribute: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(at) = rest.find(attribute) {
        rest = &rest[at + attribute.len()..];
        let trimmed = rest.trim_start();
        // `data-nyx-if` would otherwise swallow `data-nyx-if-not`: only an `=` continues.
        let Some(after_eq) = trimmed.strip_prefix('=') else {
            continue;
        };
        let value = after_eq.trim_start();
        let quote = match value.chars().next() {
            Some(q @ ('"' | '\'')) => q,
            _ => continue,
        };
        let body = &value[quote.len_utf8()..];
        let Some(end) = body.find(quote) else {
            continue;
        };
        out.push(body[..end].to_string());
        rest = &body[end..];
    }
    out
}

/// Every STRUCTURAL condition the template declares (the generalized `data-nyx-if`).
fn template_conditions(template: &str) -> Vec<TemplateCondition> {
    let mut out = Vec::new();
    for body in attribute_values(template, "data-nyx-if-not") {
        out.extend(parse_condition_body(&body, true));
    }
    for body in attribute_values(template, "data-nyx-if") {
        out.extend(parse_condition_body(&body, false));
    }
    out
}

/// Is this VALUE shown by that condition — does an element carrying it exist when the axis sits on
/// this value?
fn condition_shows(condition: &TemplateCondition, value: &str) -> bool {
    let named = condition.values.iter().any(|v| v == value);
    if condition.negated {
        !named
    } else {
        named
    }
}

/// Does a `data-nyx-if` / `data-nyx-if-not` on THIS AXIS make THIS VALUE render differently from at
/// least one OTHER value of the same axis (finding `01KYSCY4J8JANZEJPYME86B1VH`)?
///
/// This is what CREDITS A BASELINE. The manual's canonical boolean is `loading: [{ "name": "off" },
/// { "name": "on", "classes": … }]` with ONE conditional element (`data-nyx-if="loading=on"`), and
/// `off` deliberately carries nothing: the baseline IS the absence of classes, and what makes it
/// visible is the COMPLEMENT — the spinner that appears at `on` and is gone at `off`. Asking whether
/// the condition NAMES the value (the previous reading) reported that `off` as inert, so the
/// validator reproached exactly the shape the manual teaches. Asking instead whether the condition
/// DISCRIMINATES the value keeps the diagnostic exact in both directions:
///
///   * `data-nyx-if="loading=on"` over `{off, on}` → shown at `on`, hidden at `off`: both values are
///     credited, because switching between them really does change the rendering;
///   * a value on an axis NOTHING conditions on stays inert — the real defect, still reported;
///   * so does a condition that cannot discriminate anything (`data-nyx-if="loading=off|on"` is
///     always true): the element is there whatever the axis does, which is the same inertia.
fn condition_discriminates(axis: &AxisSpec, value: &str, conditions: &[TemplateCondition]) -> bool {
    conditions.iter().filter(|c| c.axis == axis.name).any(|c| {
        let here = condition_shows(c, value);
        axis.values
            .iter()
            .any(|other| other.name != value && condition_shows(c, &other.name) != here)
    })
}

/// Does ANOTHER value of the same axis carry CLASSES, and so DISCRIMINATE this bare one (finding
/// `01KYT103MXTKFBZMKF968FGW9P`)?
///
/// The same rule as [`condition_discriminates`], applied to the OTHER mechanism. Observed on
/// `checkbox`: an axis with `enabled` bare beside `disabled` carrying `opacity-50` reported `enabled`
/// inert. It is not — choosing `enabled` rather than `disabled` really does change the rendering,
/// because it is the ABSENCE of `opacity-50` that defines it, exactly as it is the absence of classes
/// that defines a baseline `off`. The previous reading only credited a value through a
/// `data-nyx-if` / `-if-not`, so it reproached the very shape the manual teaches — and pushed the
/// author towards the one correction the manual explicitly forbids, INVENTING classes on a baseline
/// to silence a warning.
///
/// It stops at the AXIS, like every credit here: classes on one axis say nothing about another one.
/// The real inert case therefore survives intact — an axis where NO value carries classes, no
/// condition discriminates and no state-case exists is still reported, because selecting any of its
/// values really does change nothing.
fn classes_discriminate(axis: &AxisSpec, value: &str) -> bool {
    axis.values
        .iter()
        .any(|other| other.name != value && other.has_classes())
}

/// ONE axis value that can change NOTHING — the coordinates the warnings name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InertAxisValue {
    pub axis: String,
    pub value: String,
}

/// The axis values that are INERT — no `classes`, no sibling value of their axis carrying any, no
/// structural case, no substitution, and no condition on their axis that DISCRIMINATES them:
/// selecting one cannot change a single pixel.
///
/// The EXACT twin of the renderer's `inertAxisValues` (`axes.ts`), down to the ORDER of the pairs
/// (axes then values, both in declaration order). The two are pinned against the SAME shared fixture
/// (`contract/design-fixtures.json`, finding `01KYSNQWQD1324E0NA0172M66J`), so a rule that moves on
/// one side and not the other fails both suites instead of neither.
///
/// A template that is EMPTY reports nothing: a `[NEW]` component is legitimately authored from its
/// axes alone, before anyone writes its markup.
pub(crate) fn inert_axis_values(axes: &[AxisSpec], template: &str) -> Vec<InertAxisValue> {
    if template.trim().is_empty() {
        return Vec::new();
    }
    let cases = template_state_cases(template);
    let conditions = template_conditions(template);
    let mut out: Vec<InertAxisValue> = Vec::new();
    for (index, axis) in axes.iter().enumerate() {
        if template_substitutes(template, &axis.name) {
            continue;
        }
        for value in &axis.values {
            if value.has_classes() {
                continue;
            }
            // A SIBLING VALUE'S CLASSES discriminate this bare one just as a condition does — see
            // `classes_discriminate` (finding `01KYT103MXTKFBZMKF968FGW9P`).
            if classes_discriminate(axis, &value.name) {
                continue;
            }
            // `data-nyx-state-case` is the FIRST axis's mechanism, and only its.
            if index == 0 && cases.contains(&value.name) {
                continue;
            }
            // The COMPLEMENT counts (finding `01KYSCY4J8JANZEJPYME86B1VH`): a baseline carries
            // nothing ON PURPOSE, and it is the condition on its AXIS that makes it visible — see
            // `condition_discriminates`.
            if condition_discriminates(axis, &value.name, &conditions) {
                continue;
            }
            out.push(InertAxisValue {
                axis: axis.name.clone(),
                value: value.name.clone(),
            });
        }
    }
    out
}

/// Does the template SUBSTITUTE this axis as a value (`{{variant}}` / `{{{variant}}}`)? The rarer,
/// legitimate way to render a variant with no classes and no case — which is why an axis without
/// classes is WARNED about and never refused.
///
/// THE CLOSING PAIR IS REQUIRED, exactly as the renderer's regex requires it
/// (`\{\{\{?\s*name\s*\}\}\}?`, `axes.ts::templateSubstitutes`). This used to accept a SINGLE closing
/// brace, so `class="btn-{{state}"` — a typo Handlebars substitutes nothing for — was credited here
/// as a live axis and reported INERT by the panel: the same stored component, two verdicts
/// (finding `01KYSNPWWG9DA0AXP6D22GV5BK`).
fn template_substitutes(template: &str, name: &str) -> bool {
    let mut rest = template;
    while let Some(at) = rest.find("{{") {
        rest = &rest[at + 2..];
        // A third `{` is the triple-stash (unescaped) form `{{{name}}}` — one extra brace, never more.
        let body = rest.strip_prefix('{').unwrap_or(rest);
        if let Some(end) = body.find("}}") {
            if body[..end].trim() == name {
                return true;
            }
        }
    }
    false
}

/// Does this param's EMPTINESS leave a hole a HUMAN WOULD SEE — the criterion that decides whether
/// "resolves to nothing" is worth a WARNING (finding `01KYVMTWVF3TH1XPEKV9MEBZTB`).
///
/// # The defect: a warning that fires everywhere stops being read
///
/// The warning asked for a `preview` value for every param resolving to the empty string. Almost
/// every synthesized component declares a passthrough `class` param, which is EMPTY BY NATURE — it
/// exists so a CALLER can add classes, and its absence makes the component neither blank nor
/// incomplete. So the warning fired on essentially every component of every catalogue, which is the
/// same as not firing at all.
///
/// # The criterion, and why it is not the name `class`
///
/// Special-casing the NAME would only work until the next passthrough (`wrapperClass`, `testId`,
/// `ariaLabel`, `style`, `slotAttrs`…). What actually distinguishes the two families is not what a
/// param is CALLED but WHERE THE TEMPLATE PUTS IT:
///
///   * substituted in TEXT CONTENT (between tags) the param supplies something the human READS —
///     `<button>{{label}}</button>` empty is a button with no label, a blank tile, the very thing
///     the warning was written for. WARN.
///   * substituted only INSIDE A TAG (an attribute value — `class="… {{class}}"`,
///     `data-testid="{{testId}}"`, `aria-label="{{ariaLabel}}"`) the param MODIFIES an element that
///     renders identically without it. Empty is its normal resting state. SILENT.
///
/// `required` overrides the site analysis: the descriptor itself asserts the component carries no
/// value of its own, so an empty one is a defect the component DECLARED, wherever it lands. That is
/// also the safety net — if the site reading is ever wrong, the params that matter most still warn.
///
/// A param the template substitutes NOWHERE has no site at all and stays silent here: "declared but
/// never substituted" is a different defect, with a different correction, and the variance/node
/// contracts are where it is named.
///
/// This criterion is PINNED by `contract/design-fixtures.json` (`empty_param_cases`), because the
/// renderer carries the same judgement in `paramsWithoutPreviewValue`.
fn empty_leaves_a_visible_hole(spec: &DeclaredParam, template: &str) -> bool {
    spec.required || substitutes_in_content(template, &spec.name)
}

/// Does `template` substitute `name` in TEXT-CONTENT position — i.e. OUTSIDE any tag, where what it
/// yields is read by a human — as opposed to inside an attribute value?
///
/// The scan tracks the `<` … `>` depth as it walks, and reads `{{name}}` / `{{{name}}}` the same way
/// [`template_substitutes`] does (the closing PAIR is required: `{{name}` substitutes nothing —
/// finding `01KYSNPWWG9DA0AXP6D22GV5BK`). A `<` inside an attribute VALUE would fool it, but that is
/// markup no synthesis writes unescaped, and the failure mode is the safe one: the param is read as
/// content and keeps its warning.
fn substitutes_in_content(template: &str, name: &str) -> bool {
    let bytes = template.as_bytes();
    let mut in_tag = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => {
                in_tag = true;
                i += 1;
            }
            b'>' if in_tag => {
                in_tag = false;
                i += 1;
            }
            b'{' if i + 1 < bytes.len() && bytes[i + 1] == b'{' => {
                // The body starts after `{{` plus the optional third brace (the raw form `{{{p}}}`).
                let body_at = if template[i + 2..].starts_with('{') {
                    i + 3
                } else {
                    i + 2
                };
                let Some(end) = template[body_at..].find("}}") else {
                    return false;
                };
                if !in_tag && template[body_at..body_at + end].trim() == name {
                    return true;
                }
                i = body_at + end + 2;
            }
            _ => i += 1,
        }
    }
    false
}

/// ONE param a component DECLARES, in the diagnostic view the write paths need: its `name`, its
/// `ty` (the VARIANCE contract's business — [`variance_warnings`]) and whether the caller MUST
/// provide a value for it (the NODE contract's business — [`node_contract_warnings`]).
struct DeclaredParam {
    name: String,
    ty: Option<String>,
    /// `"required": true` on the descriptor — the component carries NO value of its own, so a node
    /// that omits it renders empty by construction (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`).
    required: bool,
    /// The EXAMPLE value the synthesis gave for an aperçu, and the component's OWN fallback — the
    /// `preview > default > nothing` precedence the catalogue vignette applies (and which
    /// [`design_preview`] applies too, so what the agent reads is what the tile shows).
    preview: Option<Value>,
    default: Option<Value>,
}

/// The params a `params_json` blob declares. Tolerates both the array and the object-map shapes,
/// and never fails: a malformed blob declares nothing rather than breaking a write.
fn declared_param_specs(params_json: Option<&str>) -> Vec<DeclaredParam> {
    let Some(raw) = params_json else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let read = |name: String, entry: &Value| DeclaredParam {
        name,
        ty: entry
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string),
        required: entry.get("required").and_then(Value::as_bool) == Some(true),
        preview: entry.get("preview").filter(|v| !v.is_null()).cloned(),
        default: entry.get("default").filter(|v| !v.is_null()).cloned(),
    };
    match &parsed {
        Value::Array(items) => items
            .iter()
            .filter_map(|entry| {
                let name = entry.get("name").and_then(Value::as_str)?;
                Some(read(name.to_string(), entry))
            })
            .collect(),
        Value::Object(map) => map.iter().map(|(k, v)| read(k.clone(), v)).collect(),
        _ => Vec::new(),
    }
}

/// The params a `params_json` blob declares, as `(name, type)` — the diagnostic view of the OTHER
/// half of the model. Tolerates both the array and the object-map shapes, and never fails.
fn declared_params(params_json: Option<&str>) -> Vec<(String, Option<String>)> {
    declared_param_specs(params_json)
        .into_iter()
        .map(|p| (p.name, p.ty))
        .collect()
}

/// Every `data-nyx-state-case="<name>"` a template carries, in order, deduplicated. Both quotings
/// are read: the template is authored by an LLM, not by a formatter.
fn template_state_cases(template: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(at) = rest.find("data-nyx-state-case") {
        rest = &rest[at + "data-nyx-state-case".len()..];
        let trimmed = rest.trim_start();
        let Some(after_eq) = trimmed.strip_prefix('=') else {
            continue;
        };
        let value = after_eq.trim_start();
        let quote = match value.chars().next() {
            Some(q @ ('"' | '\'')) => q,
            _ => continue,
        };
        let body = &value[quote.len_utf8()..];
        let Some(end) = body.find(quote) else {
            continue;
        };
        let name = &body[..end];
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        rest = &body[end..];
    }
    out
}

/// THE VARIANCE CONTRACT, checked at the WRITE (findings `01KYPSG1HM2GPJMNH2SP3DDADH`, then
/// `01KYQ0R84KVF6QVQXQ9YVG9Z8T`). It is the one place a synthesis can still be fixed cheaply — the
/// agent is holding everything it just composed — so every message NAMES THE COMPONENT, THE FAULTY
/// FIELD, AND THE CORRECTION. That style is not decoration: a test agent reported that the
/// `data-nyx-state-case` warning "m'a débloqué instantanément", which is why the whole model is
/// now guarded this way.
///
/// THE RULE it enforces: an AXIS OF VARIANCE goes in `states_json`, never in `params_json`; a
/// SUBSTITUTED VALUE goes in `params_json`, never in the axes. From it, six diagnostics:
///
///   1. an AXIS VALUE that can change nothing — no `classes`, no structural case, no substitution,
///      and nothing that DISCRIMINATES it from a sibling value of its own axis (this subsumes the
///      old "declared state with no `data-nyx-state-case`", and it is what finally gives a declared
///      BOOLEAN a verdict: two class-less values ARE a decorative toggle). A BASELINE is not inert,
///      and there are TWO ways it is credited, because there are two mechanisms: a
///      `data-nyx-if="loading=on"` element gone at `off` — the manual's canonical boolean, see
///      [`condition_discriminates`] (finding `01KYSCY4J8JANZEJPYME86B1VH`) — and the CLASSES a
///      sibling value carries, `enabled` bare beside `disabled` at `opacity-50`, see
///      [`classes_discriminate`] (finding `01KYT103MXTKFBZMKF968FGW9P`). Both were false positives
///      reproaching the very shape the manual teaches, and both pushed towards the one correction
///      the manual forbids: inventing classes on a baseline to silence a warning;
///   2. a first-axis value the template renders NO `data-nyx-state-case` for while it renders
///      others — worse than inert: selecting it hides every case, so the aperçu goes EMPTY;
///   3. a `data-nyx-state-case` no axis declares — nothing can ever select it (the typo trap);
///   4. a `data-nyx-if` naming an axis that does not exist — the shim ignores it, on purpose
///      (hiding on an unknown name is how a preview goes blank), so it is reported instead;
///   5. the SAME notion declared as a param AND as an axis — two controls over one thing;
///   6. a `boolean` param, and a param still typed `state` — a variance axis smuggled into the
///      params, which is the ambiguity this model removes.
///
/// WARNINGS, NOT A REFUSAL, and deliberately so: a template can legitimately vary WITHOUT classes
/// or cases by substituting the axis itself (`class="btn btn-{{state}}"` on a front whose variants
/// are plain class names), and a `[NEW]` component is often authored from its axes alone, before
/// anyone writes its markup. Refusing would throw away a whole correct synthesis over a heuristic.
/// The UNKNOWN PARAM TYPE ([`ensure_params_json_accepted`]) is the opposite case — there the defect is
/// certain — and the two are treated accordingly.
pub(crate) fn variance_warnings(
    component: &str,
    template: &str,
    states_json: Option<&str>,
    params_json: Option<&str>,
) -> Vec<String> {
    let axes = parse_axes(states_json);
    let params = declared_params(params_json);
    let mut warnings = Vec::new();
    let has_template = !template.trim().is_empty();
    let cases = if has_template {
        template_state_cases(template)
    } else {
        Vec::new()
    };
    let conditions = if has_template {
        template_conditions(template)
    } else {
        Vec::new()
    };
    let declared = primary_axis_states(&axes);

    // (2) + (3) — the CASE contract of the FIRST axis, kept verbatim from the previous model: it is
    // still how a structural per-value rendering is expressed, and its wording is what unblocked a
    // real agent.
    let missing: Vec<String> = if cases.is_empty() {
        Vec::new()
    } else {
        declared
            .iter()
            .filter(|n| !cases.contains(n))
            .cloned()
            .collect()
    };
    if !missing.is_empty() {
        warnings.push(format!(
            "component '{component}': these declared states have NO matching \
             `data-nyx-state-case` in the template: {} — selecting one of them hides every other \
             case, so the aperçu goes EMPTY. Render a case for each declared state, or stop \
             declaring the ones you did not render",
            missing.join(", ")
        ));
    }
    if !cases.is_empty() {
        let undeclared: Vec<&str> = cases
            .iter()
            .filter(|c| !declared.iter().any(|n| n == *c))
            .map(String::as_str)
            .collect();
        if !undeclared.is_empty() {
            warnings.push(format!(
                "component '{component}': the template carries `data-nyx-state-case` values that \
                 states_json does NOT declare: {} — nothing can ever select them (a name mismatch \
                 renders them never), so declare them in states_json or drop the cases",
                undeclared.join(", ")
            ));
        }
    }

    // (1) — INERT AXIS VALUES. The whole-component form keeps the historical wording (the message
    // that "unblocked instantly"); the per-axis form names the coordinates.
    if has_template && !axes.is_empty() {
        let nothing_anywhere = axes
            .iter()
            .all(|a| a.values.iter().all(|v| !v.has_classes()))
            && cases.is_empty()
            && conditions.is_empty()
            && !axes.iter().any(|a| template_substitutes(template, &a.name));
        if nothing_anywhere && axes.len() == 1 && axes[0].legacy {
            warnings.push(format!(
                "component '{component}': it declares {} state(s) ({}) but its template carries NO \
                 `data-nyx-state-case` element and no value carries `classes`: the renderer varies \
                 a component by APPLYING the active value's classes and by SHOWING the element whose \
                 data-nyx-state-case equals it, so as stored, selecting a state cannot change one \
                 pixel of the aperçu. Give each value its resolved classes in states_json \
                 ({{\"axes\":[{{\"name\":\"variant\",\"values\":[{{\"name\":\"{}\",\"classes\":\"…\"}}]}}]}}), \
                 or emit one element per value carrying data-nyx-state-case=\"<name>\"",
                declared.len(),
                declared.join(", "),
                declared.first().map(String::as_str).unwrap_or("default")
            ));
        } else {
            // ONE reading of inertia for the whole model ([`inert_axis_values`], the twin of the
            // renderer's); this loop only decides how to SAY it, axis by axis.
            let inert_values = inert_axis_values(&axes, template);
            for (index, axis) in axes.iter().enumerate() {
                let inert: Vec<&str> = inert_values
                    .iter()
                    .filter(|v| v.axis == axis.name)
                    // Already named, more precisely, by the missing-case warning above.
                    .filter(|v| !(index == 0 && missing.contains(&v.value)))
                    .map(|v| v.value.as_str())
                    .collect();
                if !inert.is_empty() {
                    warnings.push(format!(
                        "component '{component}': axis '{}' has value(s) with NEITHER `classes` NOR \
                         a structural case, on an axis where NO OTHER value carries classes either \
                         and NOTHING in the template conditions on '{}': {} — selecting them cannot \
                         change one pixel. Give each one its resolved classes in states_json \
                         (\"values\": [{{\"name\": \"{}\", \"classes\": \"…\"}}]), or render an \
                         element carrying data-nyx-if=\"{}={}\" for what only EXISTS at that value. \
                         A BASELINE value legitimately carries nothing — but only when something \
                         DISCRIMINATES it from a sibling: a data-nyx-if gating an element on ANOTHER \
                         value of the SAME axis, or the classes that other value carries. Here, \
                         neither exists",
                        axis.name,
                        axis.name,
                        inert.join(", "),
                        inert[0],
                        axis.name,
                        inert[0]
                    ));
                }
            }
        }
    }

    // (4) — a condition pointing at nothing.
    let unknown_axes: Vec<&str> = {
        let mut out: Vec<&str> = Vec::new();
        for c in &conditions {
            if !axes.iter().any(|a| a.name == c.axis) && !out.contains(&c.axis.as_str()) {
                out.push(&c.axis);
            }
        }
        out
    };
    if !unknown_axes.is_empty() {
        warnings.push(format!(
            "component '{component}': the template conditions on axis/axes states_json does NOT \
             declare: {} — the condition is IGNORED (hiding on an unknown name would blank the \
             aperçu). Declare the axis in states_json or drop the data-nyx-if",
            unknown_axes.join(", ")
        ));
    }

    // (5) — ONE notion, TWO declarations.
    let shadowed: Vec<&str> = params
        .iter()
        .filter(|(name, _)| axes.iter().any(|a| &a.name == name))
        .map(|(name, _)| name.as_str())
        .collect();
    if !shadowed.is_empty() {
        warnings.push(format!(
            "component '{component}': {} declared BOTH in params_json and as an axis in \
             states_json — one notion, two controls, and the param one is the decorative copy (a \
             param is substituted as a VALUE; an axis value is a whole SET of classes, which no \
             substitution produces). DROP {} from params_json and keep the axis",
            shadowed
                .iter()
                .map(|n| format!("'{n}'"))
                .collect::<Vec<_>>()
                .join(", "),
            if shadowed.len() > 1 {
                "those entries"
            } else {
                "that entry"
            }
        ));
    }

    // (6) — the two param shapes that are really an axis in disguise.
    for (name, ty) in &params {
        if shadowed.contains(&name.as_str()) {
            continue;
        }
        match ty.as_deref() {
            Some("state") => warnings.push(format!(
                "component '{component}': param '{name}' declares the DEPRECATED type \"state\". A \
                 variance axis is NEVER a param — that ambiguity is exactly what made a variant \
                 declarable twice. Declare it in states_json as an axis \
                 ({{\"axes\":[{{\"name\":\"{name}\",\"values\":[{{\"name\":\"…\",\"classes\":\"…\"}}]}}]}}) \
                 and remove this descriptor: nyx renders one control per axis on its own"
            )),
            Some("boolean")
                if !conditions.iter().any(|c| &c.axis == name)
                    && !axes.iter().any(|a| &a.name == name) =>
            {
                warnings.push(format!(
                    "component '{component}': param '{name}' is a `boolean`, and a boolean param can \
                     change NOTHING in the rendering — it is substituted as the text true/false, it \
                     carries no classes and it gates no element, so the toggle is decorative. A \
                     boolean IS AN AXIS WITH TWO VALUES: declare it in states_json \
                     ({{\"name\":\"{name}\",\"values\":[{{\"name\":\"off\"}},{{\"name\":\"on\",\"classes\":\"…\"}}]}}) \
                     and, for what only EXISTS in one of them, render it with data-nyx-if=\"{name}=on\""
                ));
            }
            _ => {}
        }
    }

    warnings
}

// --- THE NODE CONTRACT (finding `01KYQ0R9414Q5K63TBV9CCMC9J`) ---------------
//
// [`variance_warnings`] above checks a COMPONENT against ITSELF. This half checks a COMPOSITION
// NODE against the component it REFERENCES — the other end of the same seam, and the one that was
// checked against NOTHING: a node could carry `text: "Reset filters"` while its component declared
// `buttonLabel` + a `state` axis, and the only symptom was an EMPTY rendering. Silence, on invalid
// data. Same motif as every other trap of this round, same cure: SAY IT, name the correction, and
// never guess a mapping between two neighbouring names.

/// The keys a composition node may legitimately carry for a component: its declared PARAMS and its
/// declared AXES. `children` is excluded from the judgement — the renderer injects it itself.
const NODE_RESERVED_KEY: &str = "children";

/// How many keys ONE warning NAMES before it counts the rest. A component with 200 params must not
/// turn a single warning into the whole answer.
const MAX_NAMED_KEYS: usize = 8;

/// How many NODES of one composition are named before the report stops naming them (finding
/// `01KYQ0R9414Q5K63TBV9CCMC9J`: "un warning par nœud sur une grosse composition doit rester
/// borné"). This is a bound on the LIST; the response as a whole still leaves through
/// [`crate::mcp_runtime::bounded_tool_result`] on the router, which is the mechanism — this only
/// makes sure the answer is still USEFUL when it gets there, instead of being 400 lines of the same
/// sentence with the useful half cut off.
///
/// IT BOUNDS **NODES**, and it says so because it used not to (finding
/// `01KZ3JTVM23C3MBATJZ3RYVPY6`). The renderer's twin of this check
/// (`design/composition-contract.ts::MAX_NODE_CONTRACT_ISSUES`) carried the same 12 while bounding
/// NODES, and this side bounded MESSAGES: on a composition where every node has two defects, the
/// agent was told about six nodes and the human saw twelve — the two halves of ONE surface naming
/// two different sets of broken nodes. `contract/design-fixtures.json` (`node_contract`) now pins
/// which nodes are named, and both suites replay it.
pub const MAX_NODE_CONTRACT_WARNINGS: usize = 12;

/// The CONTRACT a catalogue component poses on any composition node that instantiates it: the keys
/// it DECLARES (`params_json` + the axes of `states_json`) and, among the params, the ones it
/// REQUIRES. Built from a `design_components` row by the caller, so this module never touches a DB.
pub struct ComponentContract {
    pub name: String,
    pub params_json: Option<String>,
    pub states_json: Option<String>,
}

/// Name at most [`MAX_NAMED_KEYS`] keys, then say how many more there are.
fn named_keys(keys: &[&str]) -> String {
    let shown: Vec<String> = keys
        .iter()
        .take(MAX_NAMED_KEYS)
        .map(|k| format!("'{k}'"))
        .collect();
    match keys.len().saturating_sub(MAX_NAMED_KEYS) {
        0 => shown.join(", "),
        rest => format!("{} (+{rest} more)", shown.join(", ")),
    }
}

/// Does the node PROVIDE a usable value for this key? Absent, `null` and `""` are all "not
/// provided": a required param resolved to the empty string renders exactly as an omitted one —
/// nothing — and that is the defect being named.
fn node_provides(provided: &serde_json::Map<String, Value>, name: &str) -> bool {
    match provided.get(name) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// CONFRONT one composition node's keys with the component it references — the check that did not
/// exist (finding `01KYQ0R9414Q5K63TBV9CCMC9J`). TWO DEFECTS, kept DISTINCT because their
/// corrections are opposite:
///
///   1. a key the node PROVIDES that the component declares NOWHERE (neither a param nor an axis) —
///      a typo or the wrong name. Nothing substitutes it, so the value is silently dropped.
///   2. a param the component marks `required` that the node does NOT provide — the component has no
///      value of its own, so the node renders EMPTY. Guaranteed, not probable.
///
/// It NEVER invents a mapping between neighbouring names (`text` → `buttonLabel`): that would be
/// guessing, and a wrong guess is worse than the silence it replaces. It NAMES the component, the
/// node, the faulty key and the correction — the wording style of the `data-nyx-state-case`
/// warning, which a test agent reported as the one that "unblocked it instantly".
///
/// A component that declares NOTHING (no params, no axes — a `[NEW]` authored from its name alone,
/// or a row awaiting its synthesis) has NO contract to confront: it reports nothing rather than
/// flagging every key of every node that uses it.
pub fn node_contract_warnings(
    component: &str,
    node_key: &str,
    provided: &serde_json::Map<String, Value>,
    params_json: Option<&str>,
    states_json: Option<&str>,
) -> Vec<String> {
    let params = declared_param_specs(params_json);
    let axes = parse_axes(states_json);
    if params.is_empty() && axes.is_empty() {
        return Vec::new();
    }
    let mut declared: Vec<&str> = Vec::with_capacity(params.len() + axes.len());
    for name in params
        .iter()
        .map(|p| p.name.as_str())
        .chain(axes.iter().map(|a| a.name.as_str()))
    {
        if !declared.contains(&name) {
            declared.push(name);
        }
    }

    let mut warnings = Vec::new();
    let unknown: Vec<&str> = provided
        .keys()
        .map(String::as_str)
        .filter(|k| *k != NODE_RESERVED_KEY && !declared.contains(k))
        .collect();
    if !unknown.is_empty() {
        warnings.push(format!(
            "node '{node_key}' carries key(s) component '{component}' declares NOWHERE: {} — \
             nothing substitutes them, so the value is DROPPED and the node renders without it. \
             '{component}' declares: {}. RENAME the key to the declared one you meant, or declare it \
             in the component's params_json — nyx never maps one name onto a neighbouring one \
             (guessing `text` means `label` is how a wrong value gets rendered confidently)",
            named_keys(&unknown),
            named_keys(&declared)
        ));
    }

    let missing: Vec<&str> = params
        .iter()
        .filter(|p| p.required && !node_provides(provided, &p.name))
        .map(|p| p.name.as_str())
        .collect();
    if !missing.is_empty() {
        warnings.push(format!(
            "node '{node_key}' does not provide REQUIRED param(s) of component '{component}': {} — \
             the component carries no value of its own for them, so as composed this node renders \
             EMPTY. Add {} to the node's params, or drop `required` from the descriptor if the \
             component really has its own fallback",
            named_keys(&missing),
            format_args!("\"{}\": \"…\"", missing[0])
        ));
    }
    warnings
}

/// CONFRONT a WHOLE `composition_json` with the catalogue — every node that references a known
/// component, checked by [`node_contract_warnings`], depth-first from the root so the report reads
/// in placement order.
///
/// BOUNDED BY CONSTRUCTION: at most [`MAX_NODE_CONTRACT_WARNINGS`] **NODES** are named — every
/// message a named node produces is kept, so a node is never reported half — then ONE line counting
/// the nodes it stopped naming. A 300-node composition where every node is wrong answers with at
/// most 25 lines, not 600, and the response still leaves through the router's
/// [`crate::mcp_runtime::bounded_tool_result`] on top of that.
///
/// The cap bounds NODES rather than messages so this answer and the renderer's
/// (`compositionContractReport`) name the SAME broken nodes; the fixture case
/// `node_contract.case` is what holds the two to it.
///
/// A node referencing a component ABSENT from the catalogue is NOT a mismatch: that is a `[NEW]`,
/// which the composition seam already reports as a build candidate. A malformed composition yields
/// an EMPTY list (never an error) — a diagnostic must not be able to refuse a write.
pub fn composition_contract_warnings(
    composition_json: &str,
    catalogue: &[ComponentContract],
) -> Vec<String> {
    let mut warnings: Vec<String> = Vec::new();
    let mut nodes_named = 0usize;
    let mut nodes_over_cap = 0usize;
    for node in crate::render::composition_nodes(composition_json) {
        let Some(name) = node.component.as_deref() else {
            continue;
        };
        let Some(contract) = catalogue.iter().find(|c| c.name == name) else {
            continue;
        };
        let node_warnings = node_contract_warnings(
            name,
            &node.node_key,
            &node.params,
            contract.params_json.as_deref(),
            contract.states_json.as_deref(),
        );
        if node_warnings.is_empty() {
            continue;
        }
        // The cap counts NODES, never messages: a node is named WHOLE (both of its defects) or not
        // at all, and the twelve named here are the twelve the renderer names too.
        if nodes_named >= MAX_NODE_CONTRACT_WARNINGS {
            nodes_over_cap += 1;
            continue;
        }
        nodes_named += 1;
        warnings.extend(node_warnings);
    }
    if nodes_over_cap > 0 {
        warnings.push(format!(
            "…and {nodes_over_cap} more composition node(s) mismatch the component they reference — \
             only the first {MAX_NODE_CONTRACT_WARNINGS} are named so one broken composition cannot \
             bury the rest of the answer. Fix these, write again, and the next ones are named"
        ));
    }
    warnings
}

// --- THE REFERENCE CONTRACT (finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`) -----------
//
// A template may REFERENCE another catalogue component (`data-nyx-use`) instead of inlining a copy
// of it. That reference is resolved at RENDER time, in the sidecar — and a reference the render will
// REFUSE (an unknown target, a cycle) must not be discovered through a blank preview three tools
// later. So it is checked at the WRITE, where the agent is still holding everything it needs to fix
// it, in exactly the style of the variance and node contracts above: the component, the faulty
// field, the correction.

/// The catalogue view [`reference_warnings`] needs: name + template + the two contract blobs. The
/// SAME view the renderer resolves a reference against ([`crate::render::RenderComponent`]), so a
/// diagnostic can never judge a shape the render does not see.
pub type ReferenceCatalogue = [crate::render::RenderComponent];

/// The reference SITES a template declares, checked against the catalogue. FIVE diagnostics, and the
/// first two are the ones the render turns into a hard refusal:
///
///   1. a reference to a component the catalogue does NOT have — the render refuses it, so as stored
///      this component cannot be previewed at all (this is also the "deleted after the fact" case);
///   2. a CYCLE (A → B → A, a self-reference included) — refused for the same reason, and named as a
///      chain so the author knows which link to break;
///   3. a `required` param of the referenced component the site does NOT provide — the sub-component
///      has no value of its own, so it renders empty THERE, exactly as an under-filled composition
///      node does;
///   4. a `data-nyx-param-<key>` the referenced component declares NOWHERE — nothing substitutes it;
///   5. a `data-nyx-use-axes` clause naming an axis, or a value, the referenced component does not
///      declare — the reference then renders at that axis's DEFAULT, which is not what was asked.
///
/// WARNINGS, not a refusal, and for the same reason as the variance contract: a component is
/// legitimately synthesized BEFORE the sub-component it references (the manual's own loop reads one
/// bundle at a time), and refusing would throw away a correct synthesis over an ORDERING. The render
/// is where an unresolved reference becomes an error; this is where it is said first.
///
/// BOUNDED by [`MAX_NODE_CONTRACT_WARNINGS`], like every other list an agent reads.
pub fn reference_warnings(
    component: &str,
    template: &str,
    catalogue: &ReferenceCatalogue,
) -> Vec<String> {
    let sites = crate::render::reference_sites(template);
    if sites.is_empty() {
        return Vec::new();
    }
    let mut warnings: Vec<String> = Vec::new();
    // (2) — the CYCLE, named ONCE however many sites lead into it.
    if let Some(cycle) = reference_cycle(component, template, catalogue) {
        warnings.push(format!(
            "component '{component}': its references form a CYCLE — {} — and a cycle has no \
             rendering, so the render REFUSES it rather than unrolling it. One of these components \
             must INLINE the other's markup instead of referencing it: a shared sub-component is a \
             LEAF of the reference graph, never a parent of its own host",
            cycle.join(" → ")
        ));
    }
    for site in &sites {
        if warnings.len() >= MAX_NODE_CONTRACT_WARNINGS {
            warnings.push(format!(
                "…and more reference defects in '{component}' — only the first \
                 {MAX_NODE_CONTRACT_WARNINGS} are named. Fix these, write again, and the next ones \
                 are named"
            ));
            break;
        }
        let Some(target) = catalogue.iter().find(|c| c.name == site.component) else {
            warnings.push(format!(
                "component '{component}': its template REFERENCES a component the catalogue does \
                 NOT have: '{name}' (data-nyx-use=\"{name}\") — the render REFUSES an unresolved \
                 reference, so as stored '{component}' cannot be previewed at all. Synthesize \
                 '{name}' first (design_inventory then design_refresh_component), author it \
                 greenfield (proto_author_component), or inline its markup here",
                name = site.component
            ));
            continue;
        };
        let declared_params = declared_param_specs(target.params_json.as_deref());
        let axes = parse_axes(target.states_json.as_deref());
        // (3) — a `required` param nobody provides at the site.
        let missing: Vec<&str> = declared_params
            .iter()
            .filter(|p| p.required)
            .filter(|p| {
                !site
                    .params
                    .iter()
                    .any(|(name, value)| name == &p.name && !value.is_empty())
            })
            .map(|p| p.name.as_str())
            .collect();
        if !missing.is_empty() {
            warnings.push(format!(
                "component '{component}': its reference to '{name}' does not provide REQUIRED \
                 param(s) of '{name}': {} — '{name}' carries no value of its own for them, so the \
                 sub-component renders EMPTY inside '{component}'. Add {attr}=\"…\" on the element \
                 carrying data-nyx-use=\"{name}\" ({attr}=\"{{{{{first}}}}}\" FORWARDS this \
                 component's own param of that name), or drop `required` from '{name}''s descriptor",
                named_keys(&missing),
                name = site.component,
                attr = format_args!("{}{}", crate::render::PARAM_ATTR_PREFIX, missing[0]),
                first = missing[0],
            ));
        }
        // (4) — a key the referenced component declares nowhere.
        let declared: Vec<&str> = declared_params
            .iter()
            .map(|p| p.name.as_str())
            .chain(axes.iter().map(|a| a.name.as_str()))
            .collect();
        if !declared.is_empty() {
            let unknown: Vec<&str> = site
                .params
                .iter()
                .map(|(name, _)| name.as_str())
                .filter(|name| !declared.contains(name))
                .collect();
            if !unknown.is_empty() {
                warnings.push(format!(
                    "component '{component}': its reference to '{name}' passes key(s) '{name}' \
                     declares NOWHERE: {} — nothing substitutes them, so the value is DROPPED. \
                     '{name}' declares: {}. RENAME the attribute onto the declared name you meant — \
                     nyx never maps one name onto a neighbouring one",
                    named_keys(&unknown),
                    named_keys(&declared),
                    name = site.component,
                ));
            }
        }
        // (5) — a fixed axis value that names nothing.
        let dangling: Vec<String> = site
            .axes
            .iter()
            .filter(|(axis, value)| {
                !axes
                    .iter()
                    .any(|a| &a.name == axis && a.value(value).is_some())
            })
            .map(|(axis, value)| format!("{axis}={value}"))
            .collect();
        if !dangling.is_empty() {
            warnings.push(format!(
                "component '{component}': {attr}=\"…\" on the reference to '{name}' fixes {} — an \
                 axis or a value '{name}' does NOT declare. Those clauses are IGNORED and the \
                 reference renders at '{name}''s DEFAULT combination instead, which is not what you \
                 asked for. Read '{name}''s states_json and name an axis it has, at a value it has",
                dangling.join(", "),
                attr = crate::render::USE_AXES_ATTR,
                name = site.component,
            ));
        }
    }
    warnings
}

/// The reference CYCLE `component` sits on, if any — the chain from the first repeated name back to
/// itself (`["Button", "Spinner", "Button"]`). `template` is the one being WRITTEN, which is not
/// necessarily the one the catalogue still stores for `component` (a greenfield `[NEW]` is not in it
/// at all): the walk therefore reads the NEW text for `component` and the stored one for everybody
/// else, so a cycle the write would CREATE is caught before it is stored.
fn reference_cycle(
    component: &str,
    template: &str,
    catalogue: &ReferenceCatalogue,
) -> Option<Vec<String>> {
    let references_of = |template: &str| -> Vec<String> {
        crate::render::reference_sites(template)
            .into_iter()
            .map(|s| s.component)
            .collect()
    };
    let template_of = |name: &str| -> Option<String> {
        if name == component {
            return Some(template.to_string());
        }
        catalogue
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.template.clone())
    };
    // Iterative DFS: `path` is the chain being explored, `settled` the names already proven
    // cycle-free (a diamond — two hosts referencing one leaf — is then walked once, not twice). The
    // stack is EXPLICIT, so the depth of the walk is the depth of the graph and never the depth of
    // the Rust stack.
    let mut settled: Vec<String> = Vec::new();
    let mut path: Vec<String> = vec![component.to_string()];
    let mut stack: Vec<(String, Vec<String>)> =
        vec![(component.to_string(), references_of(template))];
    while let Some((_, pending)) = stack.last_mut() {
        let Some(next) = pending.pop() else {
            let (done, _) = stack.pop().expect("the frame just inspected");
            settled.push(done);
            path.pop();
            continue;
        };
        if settled.contains(&next) {
            continue;
        }
        if let Some(at) = path.iter().position(|p| *p == next) {
            let mut cycle: Vec<String> = path[at..].to_vec();
            cycle.push(next);
            return Some(cycle);
        }
        let Some(child) = template_of(&next) else {
            // An unresolved reference is a defect of its own — named above, not here.
            settled.push(next);
            continue;
        };
        let children = references_of(&child);
        path.push(next.clone());
        stack.push((next, children));
    }
    None
}

/// The catalogue as the reference contract reads it, for a project — the ONE place both write paths
/// build it, so they judge the same catalogue the same way.
pub(crate) fn reference_catalogue(
    conn: &mut diesel::SqliteConnection,
    project_id: &str,
) -> diesel::QueryResult<Vec<crate::render::RenderComponent>> {
    Ok(db::list_components(conn, project_id)?
        .into_iter()
        .map(|c| crate::render::RenderComponent {
            name: c.name,
            template: c.template,
            states_json: c.states_json,
            params_json: c.params_json,
            css_slot: crate::css_slot::CssSlot::parse(c.css_slot_json.as_deref()),
        })
        .collect())
}

/// The TEMPLATE guard applied before ANY agent-written template is STORED — here and on the `[NEW]`
/// author path ([`crate::mcp_tools_proto::proto_author_component`]), since both store an LLM's
/// output into the SAME catalogue the project-wide `/design` render reads.
///
/// It reuses the renderer's OWN detector ([`crate::render::find_refused_construct`]) and its OWN
/// wording ([`crate::render::RefusedConstruct::correction`]), so what a write refuses is exactly
/// what a render refuses, said in exactly the same words. TWO constructs are refused:
///
///   * a Handlebars BLOCK (`{{#if}}` / `{{#each}}` / `{{^…}}`) — LOGIC (PDR-0010). Iteration and
///     conditionals are expressed by the composition TREE, never by the template.
///   * a PARTIAL include (`{{> Spinner}}`) — the accidental door around `data-nyx-use` (finding
///     `01KYSZNX4TAVSGEVBVJM7W2B21`). The message NAMES `data-nyx-use` as the form that works, with
///     the element to write. Teaching it in the manual was never going to be enough: `{{> Spinner}}`
///     is the natural spelling for anyone who knows Handlebars, and everywhere else this machinery
///     REFUSES what it does not admit rather than relying on the agent's discipline.
pub(crate) fn ensure_template_accepted(template: &str) -> Result<(), RpcError> {
    // `data-nyx-css` is nyx's OWN mark: the render stamps it on a slotted component's roots, and it
    // is what every rule of that component's CSS slot is anchored on. A template writing it by hand
    // would let one component pull ANOTHER's stylesheet onto its elements, so the mark is RESERVED —
    // refused at the write, exactly like the partial include, rather than trusted to discipline.
    if template.contains(crate::css_slot::SCOPE_ATTR) {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "the template writes `{}`, which is nyx's OWN mark: the render stamps it on a \
                 component's root element(s) to anchor that component's SCOPED CSS SLOT. Writing it \
                 by hand would borrow another component's stylesheet, so it is reserved. Declare the \
                 exception with the `css_slot` argument instead — nyx scopes it for you.",
                crate::css_slot::SCOPE_ATTR
            ),
        ));
    }
    match crate::render::find_refused_construct(template) {
        None => Ok(()),
        Some(refused) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            refused.correction(),
        )),
    }
}

/// THE COMPOSITION SHAPE GUARD, applied before a variant's tree is STORED (finding
/// `01KZ3JS6WFVGYS6JKB9C4J50HR`).
///
/// A `composition_json` is a [`crate::render::CompositionNode`] TREE — ONE root node object, its
/// children nested under it. Two malformations used to be accepted with `warnings: []`: a blob that
/// does not PARSE, and an ARRAY root (the natural mistake — a page "is" a list of blocks, but that
/// list is the root node's `children`). Both make the tree unreadable by every consumer: the render
/// fails, `composition_nodes` returns nothing, so the node-contract check runs on ZERO nodes and
/// comes back empty — the write then reports a clean bill of health on a document nothing can read.
///
/// The THIRD case is the `[inline]` node's markup. An inline node is rendered as a Handlebars
/// template exactly like a component's, and both renders already refuse a block helper or a partial
/// in it — the strict one by [`crate::render::RenderError`], the lenient one by a named placeholder.
/// Only the WRITE said nothing, while the very same construct in a COMPONENT template is refused
/// hard by [`ensure_template_accepted`]. One rule, one moment: it is refused here too, with the
/// renderer's own wording and the node's key.
pub(crate) fn ensure_composition_accepted(composition_json: &str) -> Result<(), RpcError> {
    if composition_json.trim().is_empty() {
        return Ok(());
    }
    let shape_error = |detail: String| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "composition_json {detail}. It is ONE composition NODE at its root — \
                 {{ \"component\": \"Page\", \"params\": {{ … }}, \"children\": [ … ] }} — a NAMED \
                 COMPONENT TREE and never an HTML blob. A tree nyx cannot read renders NOTHING, and \
                 the node-contract check that answers `warnings` would run on ZERO nodes and come \
                 back empty"
            ),
        )
    };
    let parsed: Value = serde_json::from_str(composition_json)
        .map_err(|e| shape_error(format!("is not valid JSON ({e})")))?;
    if !parsed.is_object() {
        return Err(shape_error(format!(
            "is {} at its root, not a node OBJECT{}",
            match &parsed {
                Value::Array(_) => "an ARRAY",
                Value::String(_) => "a string",
                Value::Number(_) => "a number",
                Value::Bool(_) => "a boolean",
                _ => "null",
            },
            if parsed.is_array() {
                " — several blocks are the CHILDREN of ONE root node: wrap them in \
                 { \"key\": \"root\", \"children\": [ … ] }"
            } else {
                ""
            }
        )));
    }
    // …and an `[inline]` node is held to the template contract, like every other rendered template.
    for (node_key, inline) in crate::render::composition_inline_templates(composition_json) {
        if let Some(refused) = crate::render::find_refused_construct(&inline) {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "node '{node_key}' is an [inline] one-off whose markup is REFUSED — it is a \
                     rendered template like any other, so it obeys the same logic-less contract as \
                     a component's: {}",
                    refused.correction()
                ),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// THE SCOPED CSS SLOT at the write (finding `01KYVMTWB71R0D0CT1SV88J8DX`)
// ---------------------------------------------------------------------------

/// What the `css_slot` / `css_slot_reason` arguments say about a component's exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlotArg {
    /// The argument was not sent — the stored slot is LEFT ALONE. Re-synthesizing a template is not
    /// a decision about an exception a human already reviewed.
    Absent,
    /// `css_slot: ""` — REMOVE the exception, explicitly.
    Clear,
    /// A slot to store, already VALIDATED (structure, scoping, tokenisation, bound) and carrying the
    /// reason it exists.
    Set {
        css: String,
        reason: String,
        compiled: crate::css_slot::ScopedCss,
    },
}

impl SlotArg {
    /// The DB write this argument means.
    pub(crate) fn write(&self) -> db::CssSlotWrite {
        match self {
            SlotArg::Absent => db::CssSlotWrite::Keep,
            SlotArg::Clear => db::CssSlotWrite::Clear,
            SlotArg::Set { css, reason, .. } => db::CssSlotWrite::Set(
                crate::css_slot::CssSlot {
                    css: css.clone(),
                    reason: reason.clone(),
                }
                .to_json(),
            ),
        }
    }
}

/// What a component write ECHOES back about the exception the component NOW CARRIES — read off the
/// STORED row, never off the argument, so an agent that sent no `css_slot` still learns that the
/// component has one (omitting the argument KEEPS it). `null` when there is none.
///
/// This is the SIGNAL half of the contract at the write: the scope the CSS is bounded to, the
/// compiled CSS that really applies, the constructs it exercises, and the reason it exists.
pub(crate) fn stored_slot_echo(name: &str, css_slot_json: Option<&str>) -> Value {
    let Some(slot) = crate::css_slot::CssSlot::parse(css_slot_json) else {
        return Value::Null;
    };
    let scope_id = crate::css_slot::component_scope_id(name);
    match crate::css_slot::compile(&scope_id, &slot.css) {
        Ok(compiled) => json!({
            "scope_id": compiled.scope_id,
            "scope": compiled.selector,
            "reason": slot.reason,
            "features": compiled.feature_codes(),
            "bytes": compiled.bytes,
            "css": compiled.css,
        }),
        // A stored slot that no longer compiles emits NOTHING at the render, and says so here rather
        // than reading as an absence.
        Err(refusal) => json!({
            "scope_id": scope_id,
            "scope": crate::css_slot::scope_selector(&scope_id),
            "reason": slot.reason,
            "features": Vec::<String>::new(),
            "bytes": slot.css.len(),
            "css": "",
            "refused": refusal.code(),
            "detail": refusal.message(),
        }),
    }
}

/// A CSS-slot refusal as an MCP error: `invalid_argument`, the full correction as the message, and
/// the machine CODE in `data.css_slot_refusal` so a surface can key off the reason without parsing
/// prose.
fn slot_refusal(refusal: crate::css_slot::SlotRefusal) -> RpcError {
    RpcError::new(RpcCode::InvalidArgument, refusal.message())
        .with_data(json!({ "css_slot_refusal": refusal.code() }))
}

/// READ + VALIDATE the `css_slot` / `css_slot_reason` arguments of a component write.
///
/// EVERY defect is a REFUSAL, never a `warnings[]` entry, and that is the finding's own conclusion:
/// a warning is how the loader's animation disappeared in the first place, the four constraints are
/// stated as non-negotiable, and the agent is still holding the CSS it just composed — the fix costs
/// it one call. An accepted-but-broken slot would be worse than none: it reaches the coder as the
/// exception NYX SANCTIONED, in place of the design system's own conventions.
///
/// The token-EXISTENCE half needs the project's layer and therefore the connection; it is applied by
/// [`ensure_slot_tokens`] inside the tool's transaction.
pub(crate) fn read_css_slot_arg(args: &Value, component: &str) -> Result<SlotArg, RpcError> {
    let Some(raw) = args.get("css_slot") else {
        return Ok(SlotArg::Absent);
    };
    if raw.is_null() {
        return Ok(SlotArg::Absent);
    }
    let css = raw.as_str().ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            "css_slot must be a STRING of CSS (send \"\" to remove the component's slot)",
        )
    })?;
    if css.trim().is_empty() {
        return Ok(SlotArg::Clear);
    }
    let reason = optional_str(args, "css_slot_reason")?.unwrap_or_default();
    crate::css_slot::ensure_reason(reason).map_err(slot_refusal)?;
    let compiled = crate::css_slot::compile(&crate::css_slot::component_scope_id(component), css)
        .map_err(slot_refusal)?;
    Ok(SlotArg::Set {
        css: css.to_string(),
        reason: reason.trim().to_string(),
        compiled,
    })
}

/// The TOKENISED constraint at its strongest: every `var(--x)` the slot references must name a token
/// THIS PROJECT declares. Run inside the tool's transaction, where the layer is readable.
pub(crate) fn ensure_slot_tokens(
    conn: &mut diesel::SqliteConnection,
    project_id: &str,
    slot: &SlotArg,
) -> diesel::QueryResult<Result<(), RpcError>> {
    let SlotArg::Set { css, .. } = slot else {
        return Ok(Ok(()));
    };
    let layer = crate::design_tokens::project_token_layer(conn, project_id)?;
    let known: std::collections::BTreeSet<String> = layer
        .variables
        .keys()
        .chain(layer.dark.keys())
        .cloned()
        .collect();
    Ok(crate::css_slot::ensure_tokens_known(css, &known).map_err(slot_refusal))
}

/// How many token names [`design_tokens`] echoes in one response. A design system runs to a few
/// dozen; the ceiling only stops a generated CSS bundle from blowing the MCP payload (review #43's
/// discipline, applied to the token layer).
const MAX_TOKENS_ECHOED: usize = 400;

/// Read a `{ "--name": "value" }` argument into a normalised map (`primary` and `--primary` are the
/// same token). A non-object → `invalid_argument`; a non-string value is coerced from a number and
/// otherwise refused, so a typo never lands silently in the theme.
fn token_map_arg(
    args: &Value,
    key: &str,
) -> Result<std::collections::BTreeMap<String, String>, RpcError> {
    let mut out = std::collections::BTreeMap::new();
    let Some(value) = args.get(key) else {
        return Ok(out);
    };
    if value.is_null() {
        return Ok(out);
    }
    let Some(map) = value.as_object() else {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("`{key}` must be an object of token name → value (e.g. {{\"--primary\": \"oklch(0.55 0.21 264)\"}})"),
        ));
    };
    for (name, raw) in map {
        let text = match raw {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!("token `{name}` must be a CSS value string, got {raw}"),
                ))
            }
        };
        out.insert(crate::design_tokens::normalize_token_name(name), text);
    }
    Ok(out)
}

/// How many bytes of ONE token VALUE are echoed. A custom property is a short CSS value — but
/// NOTHING in CSS says so (a `url(data:…)` in a token is legal), so the COUNT ceiling above was not
/// a bound on the payload (review 01KYMF0KFZQG5JYZDGZCR85BAM).
const MAX_TOKEN_VALUE_BYTES: usize = 512;

/// The byte budget of ONE echoed token map. `design_tokens` echoes TWO (light + dark) beside its
/// counters, so a quarter of the response budget each leaves ample room for the envelope.
const MAX_TOKEN_MAP_BYTES: usize = MAX_RESPONSE_BYTES / 4;

/// Echo a token map bounded by [`MAX_TOKENS_ECHOED`], by [`MAX_TOKEN_VALUE_BYTES`] per value AND by
/// [`MAX_TOKEN_MAP_BYTES`] overall, plus whether anything was dropped. The three bounds are the
/// count, the item and the payload — the same trio every other agent-facing read applies.
fn echo_tokens(map: &std::collections::BTreeMap<String, String>) -> (Value, bool) {
    let mut truncated = map.len() > MAX_TOKENS_ECHOED;
    let mut object = serde_json::Map::new();
    let mut spent = 0usize;
    for (name, raw) in map.iter().take(MAX_TOKENS_ECHOED) {
        let (value, cut) = bounded_text(raw, MAX_TOKEN_VALUE_BYTES);
        truncated |= cut;
        let size = name.len() + value.len() + 6;
        if spent + size > MAX_TOKEN_MAP_BYTES {
            truncated = true;
            break;
        }
        spent += size;
        object.insert(name.clone(), Value::String(value.to_string()));
    }
    (Value::Object(object), truncated)
}

/// `design_tokens` — the project's THEME TOKEN layer, in three shapes:
///
/// * `{ project_id }` → READ `{ mode: "read", source, sources, provenance, variables, dark,
///   variable_count, dark_count, truncated }`. Pure. `provenance` is what the ingest that produced
///   the stored layer REFUSED (`{ skipped, skipped_count, pruned_dirs, pruned_dirs_count }`), or
///   `null` when the blob records none — the layer carries its own report, it is not re-derived by
///   reading. BOTH lists are bounded HEADS and both carry their own total, so a truncation can never
///   read as a short list (finding `01KYSNRNY550NS778YCMC8CZGH`).
/// * `{ project_id, ingest: true }` → INGEST from the app: walk the project's repos (the SAME
///   resolution the component inventory uses), rank the stylesheets that look like the design
///   system, and merge their global custom properties into the layer. Mutating when it changes
///   something. It applies the project's RECIPE — the same `ignored_dirs` (floor included) and the
///   same `exclude` globs as the component discovery — and reports both the sheets it took
///   (`sources[]`) and the ones it refused (`skipped[]` + `pruned_dirs[]`), see
///   [`crate::design_tokens::ingest_project_tokens`]. `changed` answers for the TOKENS,
///   `provenance_changed` for the report — a run that retains no sheet writes only the second, and
///   that is the run whose report is the only diagnostic there is.
/// * `{ project_id, set?, remove?, dark? }` → EDIT the layer in place (upsert / drop names, on the
///   light map or the `dark` one). Mutating.
///
/// # Why this tool exists (review #42 — "l'aperçu rend gris")
///
/// The style of a real front lives in its Tailwind CLASSES *and* in the TOKENS those classes
/// resolve against (`--primary`, `--background`, …). nyx read neither: `design.theme_tokens_json`
/// existed but NOTHING ever wrote it, so the preview iframe's `@theme` was empty and `bg-primary`
/// resolved to nothing — every component rendered grey.
///
/// # Referenced, never copied
///
/// The layer is a PROJECT SINGLETON. A component never carries a copy of `primary`: its template
/// carries the CLASS, and this one layer gives that class its colour — so `primary` has ONE
/// identity, and an edit here applies everywhere at once. The layer records WHERE it came from
/// (`sources[]`: repo + repo-relative path), which keeps the link back to the app's own stylesheet
/// explicit and re-syncable (`ingest` again after a theme change upstream).
pub fn design_tokens(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let ingest = optional_bool(args, "ingest")?.unwrap_or(false);
    let dark = optional_bool(args, "dark")?.unwrap_or(false);
    let set = token_map_arg(args, "set")?;
    let remove: Vec<String> = optional_str_vec(args, "remove")?
        .unwrap_or_default()
        .iter()
        .map(|n| crate::design_tokens::normalize_token_name(n))
        .collect();

    if ingest && (!set.is_empty() || !remove.is_empty()) {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "`ingest` re-derives the whole layer from the app's stylesheet, so it cannot be combined \
             with `set`/`remove` — run the ingest first, then edit what you want to diverge.",
        ));
    }

    if ingest {
        let run = db
            .with_conn(|c| crate::design_tokens::ingest_project_tokens(c, project_id))
            .map_err(internal_db)?;
        let layer = crate::design_tokens::TokenLayer::parse(Some(run.tokens_json.as_str()));
        let (variables, truncated) = echo_tokens(&layer.variables);
        let (dark_map, dark_truncated) = echo_tokens(&layer.dark);
        let result = json!({
            "mode": "ingest",
            "repos_scanned": run.repos_scanned,
            "candidates": run.candidates,
            "sources": run.sources,
            "source": run.sources.first(),
            // WHERE THE TOKENS DID NOT COME FROM (finding 01KYSCX562SWDXZBBAPX397X8F). The layer is
            // the design system's single reference, so the run reports the sheets it REFUSED and why
            // (a recipe exclusion, a docs-tool theme, a third-party namespace, a peripheral tree)
            // and the directory NAMES it never entered — `.generated` among them. A polluted layer
            // used to look exactly like a healthy one.
            "skipped": run.skipped,
            "skipped_count": run.skipped_count,
            "pruned_dirs": run.pruned_dirs,
            "pruned_dirs_count": run.pruned_dirs_count,
            "variables": variables,
            "dark": dark_map,
            "variable_count": run.variable_count,
            "dark_count": run.dark_count,
            "changed": run.changed,
            // The REPORT half of "did anything move" (finding `01KYSNVW31C0E8YQPET0505HW2`): a run
            // that retains no sheet writes no token — `changed` stays false, it is about the tokens
            // — but it does record WHY, and that is exactly the case where the report is the only
            // diagnostic there is. The tool is mutating, and the panel is woken, on EITHER.
            "provenance_changed": run.provenance_changed,
            "truncated": truncated || dark_truncated,
        });
        return Ok(if run.changed || run.provenance_changed {
            ToolOutcome::changed(result, ChangedTopic::Prototypes)
        } else {
            ToolOutcome::read(result)
        });
    }

    if !set.is_empty() || !remove.is_empty() {
        let layer = db
            .with_conn(|c| {
                crate::design_tokens::edit_project_tokens(c, project_id, &set, &remove, dark)
            })
            .map_err(internal_db)?;
        let (variables, truncated) = echo_tokens(&layer.variables);
        let (dark_map, dark_truncated) = echo_tokens(&layer.dark);
        return Ok(ToolOutcome::changed(
            json!({
                "mode": "edit",
                "scope": if dark { "dark" } else { "light" },
                "set": set.keys().collect::<Vec<_>>(),
                "removed": remove,
                "sources": layer.sources,
                "source": layer.sources.first(),
                "variables": variables,
                "dark": dark_map,
                "variable_count": layer.variables.len(),
                "dark_count": layer.dark.len(),
                "truncated": truncated || dark_truncated,
            }),
            ChangedTopic::Prototypes,
        ));
    }

    let layer = db
        .with_conn(|c| crate::design_tokens::project_token_layer(c, project_id))
        .map_err(internal_db)?;
    let (variables, truncated) = echo_tokens(&layer.variables);
    let (dark_map, dark_truncated) = echo_tokens(&layer.dark);
    Ok(ToolOutcome::read(json!({
        "mode": "read",
        "sources": layer.sources,
        "source": layer.sources.first(),
        // WHAT THE INGEST REFUSED, as the STORED layer records it (finding
        // 01KYSK7GS9C3ZRJYW00ZA0H6GF): that report used to exist only inside the `ingest` response,
        // so reading the layer back could not tell a clean layer from a polluted one. `null` when the
        // stored blob carries no report at all — "nobody recorded it" is not "nothing was
        // discarded". Bounded by construction (`TokenProvenance`).
        "provenance": layer.provenance,
        "variables": variables,
        "dark": dark_map,
        "variable_count": layer.variables.len(),
        "dark_count": layer.dark.len(),
        "truncated": truncated || dark_truncated,
    })))
}

/// `design_recipe` — the project's BUNDLE RECIPE: WHAT CONSTITUTES A COMPONENT in this repo.
///
/// * `{ project_id }` → `{ mode: "recipe", source, recipe, pin_count, repos, warnings }` — READ the
///   effective recipe (the agent's own when it wrote one, else the one derived from the v33 glob
///   override, else the DETECTED stack's default — nyx's former hard-coded behaviour), plus each
///   repo's detected stack AND the recipe that repo is really scanned with.
/// * `{ project_id, recipe: {...} }` → WRITE it (validated, then persisted). A section the document
///   does NOT MENTION is INHERITED from the current effective recipe (see below); a section written
///   as `[]` is really cleared.
/// * `{ project_id, pins: [...] }` → WRITE only the PINS onto the current effective recipe (which is
///   MATERIALIZED as the project's own if it was still a default — pinning is a statement about this
///   project). An empty array clears them.
/// * `{ project_id, reset: true }` → CLEAR the recipe, back to the default.
///
/// # An OMITTED section is INHERITED, and the echo is the EFFECTIVE recipe
///
/// (audit 01KYM2W8J1FGWT2EHCENKPAN8S.) A write of `{ entries: [...] }` used to be taken literally:
/// `siblings`, `family_qualifiers`, `non_component_qualifiers`, `exclude` and `ignored_dirs` all
/// fell to EMPTY, and since empty vectors were not serialized the response echoed back exactly the
/// document that was sent — no signal at all. Measured on a real Angular front: 205 bundles instead
/// of 179 (specs / services / types became components again) and 28 `.variants.ts` sidecars left
/// their bundles, moving 28 input hashes and triggering a wave of re-syntheses. The recipe is
/// PERSISTED and replayed at every scan, so that mistake is permanent until someone notices it.
///
/// Two changes make it impossible to make silently:
///   1. INHERITANCE — a section the write does not mention KEEPS its current effective value.
///      Clearing one is still possible and still explicit: write it as `[]`. What the write
///      inherited is reported in `inherited`.
///   2. THE ECHO IS EFFECTIVE — the response re-reads the STORED recipe and serializes every
///      section, empty ones included, so what will actually be applied is on screen.
///
/// # Why this tool exists (finding 01KYKXPHW103C0V5RBXQ29BBXQ)
///
/// "What constitutes a component" was FRAMEWORK KNOWLEDGE hard-coded in nyx, and it was always going
/// to be late: it already cost the reference front 177 of its 179 components once (nyx assumed Angular =
/// `**/*.component.html`; that front follows the v20 `x.ts` + `x.html` convention). The agent EXPLORES
/// the repo ONCE, understands the convention and writes the recipe HERE; nyx then applies it
/// deterministically at every scan, with the hash and the incremental loop. One agent pass PER
/// PROJECT, not one per scan.
///
/// It is a CONFIG write, not a catalogue one: it notifies nothing (`/design` shows the catalogue,
/// which has not moved). Run `design_inventory` afterwards to apply it.
pub fn design_recipe(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let reset = optional_bool(args, "reset")?.unwrap_or(false);
    let recipe_arg = present_value(args, "recipe");
    let pins_arg = present_value(args, "pins");
    if reset && (recipe_arg.is_some() || pins_arg.is_some()) {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "`reset` clears the whole recipe, so it cannot be combined with `recipe`/`pins` — reset \
             first (to see the default nyx derives from the detected stack), then write.",
        ));
    }

    // The recipe the write STARTS from: the one the agent supplies, else the project's current
    // EFFECTIVE one (so a pins-only write materializes the default instead of silently discarding
    // it, and so a `recipe` write inherits the pins it did not mention).
    let view = db
        .with_conn(|c| design::project_recipe_view(c, project_id))
        .map_err(internal_db)?;

    let written = reset || recipe_arg.is_some() || pins_arg.is_some();
    let mut inherited: Vec<&'static str> = Vec::new();
    let mut recipe = match recipe_arg {
        Some(value) => {
            let mut parsed = parse_recipe_arg(value)?;
            // A section the document does not MENTION keeps its current effective value — the
            // rules, the sibling roles, the naming qualifiers, the exclusions, the pruned
            // directories and the escape hatch are separate decisions, and rewriting one must not
            // silently blank the others.
            inherit_omitted_sections(&mut parsed, &view.recipe, value, &mut inherited);
            parsed
        }
        None => view.recipe.clone(),
    };
    if let Some(value) = pins_arg {
        recipe.pins = parse_pins_arg(value)?;
    }
    recipe
        .validate()
        .map_err(|message| RpcError::new(RpcCode::InvalidArgument, message))?;

    if written {
        let to_store = if reset { None } else { Some(recipe.clone()) };
        db.with_conn(|c| design::set_project_recipe(c, project_id, to_store.as_ref()))
            .map_err(internal_db)?;
    }
    // Re-read, so the response describes the STORED state (and the source it now resolves from),
    // never the argument the caller sent. Every section is serialized, empty ones included.
    let view = db
        .with_conn(|c| design::project_recipe_view(c, project_id))
        .map_err(internal_db)?;
    let mut warnings = view.warnings.clone();
    if !inherited.is_empty() {
        warnings.push(format!(
            "your `recipe` did not mention {}, so {} KEPT from the recipe that was already \
             effective (a section written as `[]` is cleared instead). The `recipe` echoed back is \
             what the next scan will really apply — check it.",
            inherited.join(", "),
            if inherited.len() == 1 {
                "it was"
            } else {
                "they were"
            }
        ));
    }
    Ok(ToolOutcome::read(json!({
        "mode": "recipe",
        "source": view.source,
        "written": written,
        "recipe": view.recipe,
        "pin_count": view.recipe.pins.len(),
        "inherited": inherited,
        "warnings": warnings,
        "repos": view.repos,
        "next": "run design_inventory(project_id) to apply it — the recipe only changes what the \
                 NEXT scan discovers",
    })))
}

/// Fill the sections the agent's `recipe` document does NOT MENTION from the currently effective
/// `base`, recording their names in `inherited` (audit 01KYM2W8J1FGWT2EHCENKPAN8S).
///
/// Keyed on the presence of the JSON KEY, not on emptiness: `"exclude": []` is a deliberate "no
/// exclusions" and is honoured, while an absent `exclude` is "I am not talking about exclusions"
/// and keeps what was there. `entries` is deliberately NOT inheritable — it is the one section a
/// recipe cannot be without, and `validate()` already refuses an empty one.
fn inherit_omitted_sections(
    parsed: &mut design::BundleRecipe,
    base: &design::BundleRecipe,
    document: &Value,
    inherited: &mut Vec<&'static str>,
) {
    let mentions = |key: &str| document.get(key).is_some();
    if !mentions("siblings") {
        parsed.siblings = base.siblings.clone();
        inherited.push("siblings");
    }
    if !mentions("family_qualifiers") {
        parsed.family_qualifiers = base.family_qualifiers.clone();
        inherited.push("family_qualifiers");
    }
    if !mentions("non_component_qualifiers") {
        parsed.non_component_qualifiers = base.non_component_qualifiers.clone();
        inherited.push("non_component_qualifiers");
    }
    if !mentions("exclude") {
        parsed.exclude = base.exclude.clone();
        inherited.push("exclude");
    }
    if !mentions("ignored_dirs") {
        parsed.ignored_dirs = base.ignored_dirs.clone();
        inherited.push("ignored_dirs");
    }
    if !mentions("pins") {
        parsed.pins = base.pins.clone();
        inherited.push("pins");
    }
}

/// The raw value of `key` when the caller SUPPLIED it (a JSON `null` counts as "not supplied", so
/// omitting a field and nulling it mean the same thing — the convention every other tool here uses).
fn present_value<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|value| !value.is_null())
}

/// PARSE the `recipe` argument into a [`design::BundleRecipe`], turning every schema error into an
/// `invalid_argument` that NAMES the offending field — an agent-written recipe is persisted and
/// applied at every scan, so a typo must be refused loudly, never half-understood.
fn parse_recipe_arg(value: &Value) -> Result<design::BundleRecipe, RpcError> {
    if !value.is_object() {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "`recipe` must be an OBJECT (the recipe document) — read the current one first with \
             design_recipe(project_id) and edit that shape.",
        ));
    }
    design::BundleRecipe::from_json(&value.to_string())
        .map_err(|message| RpcError::new(RpcCode::InvalidArgument, message))
}

/// PARSE the `pins` argument — the escape hatch's payload.
fn parse_pins_arg(value: &Value) -> Result<Vec<design::PinnedBundle>, RpcError> {
    if !value.is_array() {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "`pins` must be an ARRAY of { entry, files: [{ path, role }], name? } — pass [] to \
             clear the pins.",
        ));
    }
    serde_json::from_value::<Vec<design::PinnedBundle>>(value.clone()).map_err(|err| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "`pins` does not match the pin schema ({{ entry, files: [{{ path, role }}], \
                 name? }} — role is one of entry/template/style/variants): {err}"
            ),
        )
    })
}

/// Route a DESIGN tool call, or `None` when `name` is not one of ours. Called from
/// [`crate::mcp_tools_core::dispatch_extension_tool`] so the design tools share the SAME dispatch
/// entry point (one dispatcher, one pool) as every other extension tool.
///
/// EVERY answer leaves through [`bounded_tool_result`], the same net the proto router applies
/// (review 01KYMF0KFZQG5JYZDGZCR85BAM): `design_inventory` and `design_tokens` cap themselves
/// gracefully, and the router catches whatever a `design_recipe` an agent grew — or a tool added
/// later — could still produce.
pub fn dispatch_design_tool(
    db: &Db,
    name: &str,
    args: &Value,
) -> Option<Result<ToolOutcome, RpcError>> {
    let outcome = route_design_tool(db, name, args)?;
    Some(outcome.map(|mut outcome| {
        outcome.result = bounded_tool_result(name, outcome.result);
        outcome
    }))
}

/// The routing table itself — [`dispatch_design_tool`] minus the response cap.
fn route_design_tool(db: &Db, name: &str, args: &Value) -> Option<Result<ToolOutcome, RpcError>> {
    match name {
        DESIGN_INVENTORY_TOOL => Some(design_inventory(db, args)),
        DESIGN_REFRESH_COMPONENT_TOOL => Some(design_refresh_component(db, args)),
        DESIGN_DELETE_COMPONENT_TOOL => Some(design_delete_component(db, args)),
        DESIGN_TOKENS_TOOL => Some(design_tokens(db, args)),
        DESIGN_RECIPE_TOOL => Some(design_recipe(db, args)),
        DESIGN_PREVIEW_TOOL => Some(design_preview(db, args)),
        _ => None,
    }
}

/// THE SHARED FIXTURE — `contract/design-fixtures.json`, read by THIS suite and by the
/// renderer's `axes.test.ts` (finding `01KYSNQWQD1324E0NA0172M66J`).
///
/// The variance rule exists TWICE, once here and once in `axes.ts`, and only the Rust half was
/// moored to anything: `mcp_context`'s guard extracts the manual's canonical example and runs it
/// through this validator, while the TS suite RE-TYPED that example by hand. A hand copy cannot
/// fail — it drifts, silently, which is exactly how the two twins ended up reading the map form
/// in two different orders.
///
/// The fix is DATA, not a build dependency: the cases live in a checked-in JSON file that both
/// suites read at test time (nothing generates it, and neither side builds the other). A case
/// added to it fails BOTH suites until BOTH implementations agree — and the manual stays the
/// source, because `the_manuals_canonical_component_passes_the_checks_the_write_path_applies`
/// asserts the `from_manual` case IS what the rendered manual carries.
#[cfg(test)]
pub(crate) const DESIGN_FIXTURES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contract/design-fixtures.json"
));

#[cfg(test)]
fn design_fixtures() -> Value {
    serde_json::from_str(DESIGN_FIXTURES).expect("contract/design-fixtures.json is valid JSON")
}

/// The `variance_cases` of the shared fixture, as `(id, case)`.
#[cfg(test)]
pub(crate) fn variance_fixture_cases() -> Vec<(String, Value)> {
    design_fixtures()["variance_cases"]
        .as_array()
        .expect("the fixture carries variance_cases")
        .iter()
        .map(|case| {
            (
                case["id"].as_str().unwrap_or_default().to_string(),
                case.clone(),
            )
        })
        .collect()
}

/// A SECTION of the shared fixture, by key — the loader `crate::render`'s own suite uses to read the
/// `unrendered_nodes` cases without re-opening the file.
#[cfg(test)]
pub(crate) fn design_fixture_section(key: &str) -> Value {
    design_fixtures()
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("contract/design-fixtures.json carries a `{key}` section"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, list_components};
    use crate::mcp::RpcCode;
    use uuid::Uuid;

    fn db_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A unique scratch dir under the OS temp dir (no tempfile dev-dep — uniqueness from a v7 UUID).
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nyx-mcp-design-{tag}-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn write_file(dir: &std::path::Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        std::fs::write(path, content).expect("write file");
    }

    /// Route a design tool, panicking on a mis-route (the tool's own error is returned).
    fn call(db: &Db, name: &str, args: Value) -> Result<ToolOutcome, RpcError> {
        dispatch_design_tool(db, name, &args).expect("routed")
    }

    /// The names of the bundles echoed by a LIST run.
    fn bundle_names(out: &ToolOutcome) -> Vec<&str> {
        out.result["bundles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["name"].as_str().unwrap())
            .collect()
    }

    /// A project rooted at `root` with one configured repo. Returns its id.
    fn seeded_project(db: &Db, name: &str, root: &std::path::Path) -> String {
        db.with_conn(|c| {
            let (project, _ws) = db::create_project(c, name, root.to_str().unwrap(), None).unwrap();
            db::create_repository(c, &project.id, "web", ".", false).unwrap();
            Ok::<_, diesel::result::Error>(project.id)
        })
        .unwrap()
    }

    /// The `globs` shortcut is refused once a RECIPE exists — and it names the field to edit.
    fn assert_eq_recipe_shortcut(err: &RpcError) {
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains("supersedes the `globs` shortcut")
                && err.message.contains("design_recipe"),
            "{}",
            err.message
        );
    }

    /// `design_recipe` — the agent's lever on WHAT CONSTITUTES A COMPONENT (finding
    /// 01KYKXPHW103C0V5RBXQ29BBXQ). It READS the default nyx derives from the detected stack,
    /// WRITES the convention the agent found in the repo (which the very next inventory applies),
    /// and RESETS back to the default. The knowledge is DATA: not one line of Rust here knows what
    /// a `.widget.js` is.
    #[test]
    fn design_recipe_tool_reads_the_default_writes_the_convention_and_resets() {
        let _g = db_guard();
        let root = scratch_dir("recipe");
        write_file(&root, "package.json", r#"{"dependencies":{"react":"19"}}"#);
        write_file(
            &root,
            "src/widgets/counter.widget.js",
            "export default defineWidget({ view: './counter.view.html' });",
        );
        write_file(&root, "src/widgets/counter.view.html", "<b></b>");
        write_file(
            &root,
            "src/ui/Card.tsx",
            "export const Card = () => <div/>;",
        );
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "House", &root);

        // READ: the default derived from the DETECTED stack — the recipe an agent edits.
        let out = call(&db, DESIGN_RECIPE_TOOL, json!({ "project_id": project_id })).unwrap();
        assert_eq!(out.result["mode"], json!("recipe"));
        assert_eq!(
            out.result["source"],
            json!(design::RECIPE_SOURCE_STACK_DEFAULT)
        );
        assert_eq!(out.result["written"], json!(false));
        assert_eq!(out.result["repos"][0]["stack"], json!("react"));
        assert!(
            !out.result["recipe"]["entries"]
                .as_array()
                .unwrap()
                .is_empty(),
            "the default is a COMPLETE recipe, ready to be edited: {}",
            out.result["recipe"]
        );
        assert!(out.effects.is_empty(), "a config READ notifies nothing");

        // With that default, the `.widget.js` front does not exist.
        let listed = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .unwrap();
        assert_eq!(bundle_names(&listed), vec!["Card"]);

        // WRITE the convention the agent read in the repo.
        let recipe = json!({
            "version": 1,
            "entries": [
                { "globs": ["**/*.widget.js"], "template_attribute": "view" }
            ],
            "siblings": [ { "extensions": ["html"], "role": "template" } ],
            "family_qualifiers": ["widget", "view"],
        });
        let out = call(
            &db,
            DESIGN_RECIPE_TOOL,
            json!({ "project_id": project_id, "recipe": recipe }),
        )
        .unwrap();
        assert_eq!(out.result["source"], json!(design::RECIPE_SOURCE_CUSTOM));
        assert_eq!(out.result["written"], json!(true));
        assert_eq!(
            out.result["recipe"]["entries"][0]["globs"][0],
            json!("**/*.widget.js")
        );

        // The NEXT inventory applies it — no Rust changed, only data.
        let listed = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .unwrap();
        assert_eq!(bundle_names(&listed), vec!["counter"]);
        assert_eq!(
            listed.result["bundles"][0]["files"]
                .as_array()
                .unwrap()
                .len(),
            2,
            "entry + the template its `view:` attribute names: {}",
            listed.result["bundles"][0]
        );

        // RESET → back to the stack default, and the inventory follows.
        let out = call(
            &db,
            DESIGN_RECIPE_TOOL,
            json!({ "project_id": project_id, "reset": true }),
        )
        .unwrap();
        assert_eq!(
            out.result["source"],
            json!(design::RECIPE_SOURCE_STACK_DEFAULT)
        );
        let listed = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .unwrap();
        assert_eq!(bundle_names(&listed), vec!["Card"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The tool's PINS shape (the escape hatch) and its GUARDS. Pinning alone leaves the rules
    /// untouched — and materializes the default as the project's own, so a later rule edit does not
    /// silently drop the pin. Every malformed recipe is refused with `invalid_argument`, naming the
    /// field: an agent-written value that is persisted and applied at EVERY scan may never be
    /// half-understood.
    #[test]
    fn design_recipe_tool_pins_a_component_and_refuses_what_it_cannot_apply() {
        let _g = db_guard();
        let root = scratch_dir("recipe-pins");
        write_file(&root, "package.json", r#"{"dependencies":{"react":"19"}}"#);
        write_file(
            &root,
            "src/ui/Card.tsx",
            "export const Card = () => <div/>;",
        );
        write_file(
            &root,
            "src/legacy/gauge.js",
            "window.Gauge = function () {};",
        );
        write_file(&root, "src/shared/dial.html", "<div class='dial'></div>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "House", &root);

        let out = call(
            &db,
            DESIGN_RECIPE_TOOL,
            json!({
                "project_id": project_id,
                "pins": [{
                    "entry": "src/legacy/gauge.js",
                    "name": "legacy-gauge",
                    "files": [{ "path": "src/shared/dial.html", "role": "template" }],
                }],
            }),
        )
        .unwrap();
        assert_eq!(out.result["written"], json!(true));
        assert_eq!(out.result["pin_count"], json!(1));
        assert_eq!(
            out.result["source"],
            json!(design::RECIPE_SOURCE_CUSTOM),
            "pinning MATERIALIZES the default as the project's own recipe"
        );
        assert_eq!(
            out.result["recipe"]["entries"][0]["globs"][0],
            json!("**/*.tsx"),
            "the rules it did not touch are the stack default's: {}",
            out.result["recipe"]
        );

        // The inventory catalogues the pinned component alongside the rule-discovered one.
        let listed = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .unwrap();
        assert_eq!(bundle_names(&listed), vec!["Card", "legacy-gauge"]);

        // The GUARDS. Each names what is wrong.
        let refused = |args: Value| -> RpcError {
            call(&db, DESIGN_RECIPE_TOOL, args).expect_err("must be refused")
        };
        let err =
            refused(json!({ "project_id": project_id, "recipe": { "version": 1, "entries": [] } }));
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(err.message.contains("inventory NOTHING"), "{}", err.message);
        let err = refused(json!({
            "project_id": project_id,
            "recipe": { "version": 1, "entries": [{ "glob": ["**/*.ts"] }] },
        }));
        assert!(err.message.contains("recipe schema"), "{}", err.message);
        let err = refused(json!({ "project_id": project_id, "recipe": ["**/*.ts"] }));
        assert!(err.message.contains("must be an OBJECT"), "{}", err.message);
        let err = refused(json!({ "project_id": project_id, "pins": { "entry": "a.ts" } }));
        assert!(err.message.contains("must be an ARRAY"), "{}", err.message);
        let err = refused(json!({
            "project_id": project_id,
            "reset": true,
            "recipe": { "version": 1, "entries": [{ "globs": ["**/*.ts"] }] },
        }));
        assert!(
            err.message.contains("cannot be combined"),
            "{}",
            err.message
        );

        // A refusal WROTE NOTHING: the pin is still there.
        let out = call(&db, DESIGN_RECIPE_TOOL, json!({ "project_id": project_id })).unwrap();
        assert_eq!(out.result["pin_count"], json!(1));

        // `globs` is the entry-globs-only SHORTCUT: once a recipe exists it would be stored and
        // then ignored at every scan, so it is REFUSED rather than silently swallowed.
        let err = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "globs": ["**/*.ts"] }),
        )
        .expect_err("the shortcut is refused once a recipe exists");
        assert_eq_recipe_shortcut(&err);

        // Clearing the pins is an explicit [] — the rules survive it.
        let out = call(
            &db,
            DESIGN_RECIPE_TOOL,
            json!({ "project_id": project_id, "pins": [] }),
        )
        .unwrap();
        assert_eq!(out.result["pin_count"], json!(0));
        let listed = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .unwrap();
        assert!(
            bundle_names(&listed).contains(&"Card"),
            "the rules still apply after the pins are cleared"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// AN OMITTED SECTION IS INHERITED, NOT EMPTIED (audit 01KYM2W8J1FGWT2EHCENKPAN8S).
    ///
    /// Writing `{ entries: [...] }` used to blank `siblings` / `family_qualifiers` /
    /// `non_component_qualifiers` / `exclude` in silence — and since empty vectors were not
    /// serialized, the response echoed back the exact document the agent had sent, so nothing could
    /// reveal it. Measured on a real Angular front: 205 bundles instead of 179 and 28 input hashes
    /// moved. The same shape, small: a component, its `.variants.ts` sidecar and its `.spec.ts`.
    #[test]
    fn design_recipe_write_inherits_the_sections_it_does_not_mention_and_echoes_the_effective_one()
    {
        let _g = db_guard();
        let root = scratch_dir("recipe-inherit");
        write_file(&root, "angular.json", r#"{"version":1}"#);
        write_file(
            &root,
            "src/ui/button.ts",
            "@Component({ templateUrl: './button.html' })\nexport class Button {}",
        );
        write_file(&root, "src/ui/button.html", "<button></button>");
        write_file(&root, "src/ui/button.variants.ts", "export const v = {};");
        write_file(
            &root,
            "src/ui/button.spec.ts",
            "@Component({})\nit('x', () => {});",
        );
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Front", &root);

        // The DEFAULT: one component, the sidecar folded in, the spec excluded.
        let listed = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .unwrap();
        assert_eq!(bundle_names(&listed), vec!["button"]);
        let baseline_hash = listed.result["bundles"][0]["input_hash"].clone();

        // The agent writes ONLY `entries` — exactly what the audit did.
        let out = call(
            &db,
            DESIGN_RECIPE_TOOL,
            json!({
                "project_id": project_id,
                "recipe": { "version": 1, "entries": [
                    { "globs": ["**/*.ts"], "requires_content": "@Component",
                      "template_attribute": "templateUrl" },
                    { "globs": ["**/*.component.html"] },
                ]},
            }),
        )
        .unwrap();
        let inherited: Vec<&str> = out.result["inherited"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            inherited,
            vec![
                "siblings",
                "family_qualifiers",
                "non_component_qualifiers",
                "exclude",
                "ignored_dirs",
                "pins",
            ],
            "what the write did NOT say is named back to it"
        );
        assert!(
            out.result["warnings"][0]
                .as_str()
                .unwrap()
                .contains("KEPT from the recipe that was already effective"),
            "{}",
            out.result["warnings"]
        );
        assert!(
            !out.result["recipe"]["exclude"]
                .as_array()
                .unwrap()
                .is_empty()
                && !out.result["recipe"]["siblings"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
            "the ECHO is the EFFECTIVE recipe, not the document that was sent: {}",
            out.result["recipe"]
        );

        // …and the consequence the audit measured is gone: same population, same hash.
        let listed = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .unwrap();
        assert_eq!(
            bundle_names(&listed),
            vec!["button"],
            "the spec did not become a component and the sidecar did not leave the bundle"
        );
        assert_eq!(
            listed.result["bundles"][0]["input_hash"], baseline_hash,
            "no input hash moved, so no re-synthesis wave"
        );

        // CLEARING is still possible, and still explicit: `[]` means `[]`, and the echo SHOWS it.
        let out = call(
            &db,
            DESIGN_RECIPE_TOOL,
            json!({
                "project_id": project_id,
                "recipe": { "version": 1,
                            "entries": [{ "globs": ["**/*.ts"], "requires_content": "@Component" }],
                            "exclude": [] },
            }),
        )
        .unwrap();
        assert_eq!(
            out.result["recipe"]["exclude"],
            json!([]),
            "an EMPTY section is serialized, so what will be applied is on screen"
        );
        assert!(
            !out.result["inherited"]
                .as_array()
                .unwrap()
                .contains(&json!("exclude")),
            "a section the write MENTIONED is not inherited"
        );
        let listed = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .unwrap();
        let files: Vec<&str> = listed.result["bundles"][0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect();
        assert!(
            files.contains(&"src/ui/button.spec.ts"),
            "clearing `exclude` really cleared it — the spec is a bundle member again: {files:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A MIXED-STACK MONOREPO KEEPS EVERY REPO (audit 01KYM2W91TKCTEAK20ZDZJ75Z4).
    ///
    /// The read echoed ONE recipe — the FIRST repo's — while a write applies to ALL of them, so an
    /// agent doing exactly what step 0-bis says ("take that default as your template") wrote the
    /// Angular default over a project whose second repo is React and dropped it from the discovery
    /// (`discovered` 2 → 1). The read now shows EVERY repo with its own effective recipe, warns
    /// about the mixed stacks, and the project-wide document it offers is the UNION — so writing
    /// back what was read cannot lose a repo.
    #[test]
    fn design_recipe_read_covers_every_repo_of_a_mixed_stack_monorepo() {
        let _g = db_guard();
        let root = scratch_dir("recipe-monorepo");
        write_file(&root, "ng/angular.json", r#"{"version":1}"#);
        write_file(
            &root,
            "ng/src/nav.ts",
            "@Component({ templateUrl: './nav.html' })\nexport class Nav {}",
        );
        write_file(&root, "ng/src/nav.html", "<nav></nav>");
        write_file(
            &root,
            "rx/package.json",
            r#"{"dependencies":{"react":"19"}}"#,
        );
        write_file(
            &root,
            "rx/src/Card.tsx",
            "export const Card = () => <div/>;",
        );

        let db = Db::in_memory();
        let project_id = db
            .with_conn(|c| {
                let (project, _ws) =
                    db::create_project(c, "Mono", root.to_str().unwrap(), None).unwrap();
                db::create_repository(c, &project.id, "ng", "ng", false).unwrap();
                db::create_repository(c, &project.id, "rx", "rx", false).unwrap();
                Ok::<_, diesel::result::Error>(project.id)
            })
            .unwrap();

        // Both repos are discovered under their OWN default.
        let listed = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .unwrap();
        assert_eq!(listed.result["discovered"], json!(2));

        // The READ shows both repos, each with its own stack AND its own effective recipe, and it
        // SAYS that one recipe governs them all.
        let out = call(&db, DESIGN_RECIPE_TOOL, json!({ "project_id": project_id })).unwrap();
        let stacks: Vec<&str> = out.result["repos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["stack"].as_str().unwrap())
            .collect();
        assert_eq!(stacks, vec!["angular", "react"]);
        for repo in out.result["repos"].as_array().unwrap() {
            assert!(
                !repo["recipe"]["entries"].as_array().unwrap().is_empty(),
                "every repo shows the recipe it is really scanned with: {repo}"
            );
        }
        assert!(
            out.result["warnings"][0]
                .as_str()
                .unwrap()
                .contains("DIFFERENT stacks"),
            "{}",
            out.result["warnings"]
        );

        // STEP 0-BIS TO THE LETTER: write back the document that was just read.
        let echoed = out.result["recipe"].clone();
        call(
            &db,
            DESIGN_RECIPE_TOOL,
            json!({ "project_id": project_id, "recipe": echoed }),
        )
        .unwrap();
        let listed = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .unwrap();
        assert_eq!(
            listed.result["discovered"],
            json!(2),
            "NO component leaves the discovery: {:?}",
            bundle_names(&listed)
        );
        assert_eq!(bundle_names(&listed), vec!["Card", "nav"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The `design_inventory` tool LISTS the discovered BUNDLES (review #40) and reports a
    /// `Prototypes` change (so `/design` re-pulls). It catalogues WITHOUT synthesizing (every
    /// bundle `needs_synthesis`), and a re-run is idempotent.
    #[test]
    fn design_inventory_tool_lists_bundles_and_reports_change() {
        let _g = db_guard();
        let root = scratch_dir("populate");
        write_file(&root, "src/Button.tsx", "<button>{{label}}</button>");
        write_file(&root, "src/Button.module.css", ".btn {}");
        write_file(&root, "src/ui/Card.tsx", "<div class=\"card\"/>");

        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);

        let out = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(out.effects, vec![ChangedTopic::Prototypes]);
        assert_eq!(out.result["mode"], json!("bundles"));
        assert_eq!(out.result["component_count"], json!(2));
        assert_eq!(out.result["discovered"], json!(2));
        assert_eq!(out.result["repos_scanned"], json!(1));
        assert_eq!(out.result["synthesized_count"], json!(0));
        assert_eq!(
            out.result["pending_count"],
            json!(2),
            "nothing synthesized yet — both bundles await the agent"
        );
        assert_eq!(bundle_names(&out), vec!["Button", "Card"]);
        // The BUNDLE (not the file) is the unit: Button carries its sibling stylesheet.
        let button = &out.result["bundles"][0];
        assert_eq!(button["entry_path"], json!("src/Button.tsx"));
        assert_eq!(button["needs_synthesis"], json!(true));
        let files: Vec<&str> = button["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect();
        assert_eq!(files, vec!["src/Button.module.css", "src/Button.tsx"]);

        // The catalogue is really populated in the DB (what `/design` lists), template still empty.
        let stored = db.with_conn(|c| list_components(c, &project_id)).unwrap();
        assert_eq!(stored.len(), 2);
        assert!(stored.iter().all(|c| c.template.is_empty()));

        // A re-run does not duplicate.
        let again = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(again.result["component_count"], json!(2));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// review #40 — the response is BOUNDED: `limit` caps the echoed list (the counters carry the
    /// real scale, so a 100+-component front never floods the agent), and `pending_only` narrows it
    /// to the work left to do.
    #[test]
    fn design_inventory_tool_bounds_its_response_and_filters_pending() {
        let _g = db_guard();
        let root = scratch_dir("bounded");
        for i in 0..6 {
            write_file(&root, &format!("src/C{i}.tsx"), "<div/>");
        }
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Big", &root);

        let out = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "limit": 2 }),
        )
        .expect("ok");
        assert_eq!(out.result["discovered"], json!(6), "every bundle counted…");
        assert_eq!(out.result["returned"], json!(2), "…but only 2 echoed");
        assert_eq!(out.result["truncated"], json!(true));
        assert_eq!(bundle_names(&out).len(), 2);

        // Synthesize one → it drops out of the `pending_only` list.
        call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({ "project_id": project_id, "name": "C0", "template": "<div/>" }),
        )
        .expect("refresh ok");
        let pending = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "pending_only": true, "limit": 50 }),
        )
        .expect("ok");
        assert_eq!(pending.result["pending_count"], json!(5));
        assert_eq!(pending.result["synthesized_count"], json!(1));
        assert!(
            !bundle_names(&pending).contains(&"C0"),
            "a synthesized bundle is not pending work"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// review #40 — `component` switches the tool to the per-bundle READ: the agent gets the
    /// bundle's files WITH their content (the matter to synthesize), and an unknown name is a
    /// structured `invalid_id`.
    #[test]
    fn design_inventory_tool_reads_one_bundle_on_demand() {
        let _g = db_guard();
        let root = scratch_dir("read");
        write_file(&root, "angular.json", r#"{"version":1}"#);
        write_file(
            &root,
            "src/app/chip/chip.component.ts",
            "@Component({ templateUrl: './chip.component.html' })\nexport class ChipComponent {}",
        );
        write_file(
            &root,
            "src/app/chip/chip.component.html",
            "<span class=\"chip\">{{ label() }}</span>",
        );
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "sandbox", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        let read = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "component": "chip" }),
        )
        .expect("ok");
        assert!(read.effects.is_empty(), "a bundle read is a READ");
        assert_eq!(read.result["mode"], json!("bundle"));
        assert_eq!(read.result["up_to_date"], json!(false));
        assert_eq!(
            read.result["bundle"]["entry_path"],
            json!("src/app/chip/chip.component.ts")
        );
        let html = read.result["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"].as_str().unwrap().ends_with(".html"))
            .expect("the resolved template file");
        assert_eq!(html["role"], json!("template"));
        assert!(html["content"]
            .as_str()
            .unwrap()
            .contains("<span class=\"chip\">"));

        let err = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "component": "ghost" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// review #32 — the tool's `globs` param PERSISTS a per-project override before scanning: an
    /// Angular-detected project overridden to `.tsx` captures the React file, and the override
    /// STICKS (a subsequent call without `globs` still uses it).
    #[test]
    fn design_inventory_tool_persists_glob_override() {
        let _g = db_guard();
        let root = scratch_dir("override");
        write_file(&root, "angular.json", "{\"version\":1}"); // would detect Angular…
        write_file(&root, "src/app/nav/nav.component.html", "<nav></nav>");
        write_file(
            &root,
            "src/email/Welcome.tsx",
            "export const W = () => <div/>;",
        );

        let db = Db::in_memory();
        let project_id = seeded_project(&db, "sandbox", &root);

        // Override to scan `.tsx` → captures the React file, not the Angular component.
        let out = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "globs": ["**/*.tsx"] }),
        )
        .expect("ok");
        assert_eq!(
            bundle_names(&out),
            vec!["Welcome"],
            "override captured the .tsx"
        );

        // The override PERSISTED: a re-run WITHOUT `globs` still honours it (Welcome, not nav).
        let again = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(
            bundle_names(&again),
            vec!["Welcome"],
            "the persisted override sticks"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// GREENFIELD: a project with no front inventories nothing WITHOUT error (repos_scanned reflects
    /// the current-folder fallback member, discovered = 0). A missing `project_id` is invalid_argument.
    #[test]
    fn design_inventory_tool_greenfield_and_arg_validation() {
        let _g = db_guard();
        let root = scratch_dir("greenfield");
        let db = Db::in_memory();
        let project_id = db
            .with_conn(|c| {
                let (project, _ws) =
                    db::create_project(c, "Neuf", root.to_str().unwrap(), None).unwrap();
                Ok::<_, diesel::result::Error>(project.id)
            })
            .unwrap();

        let out = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(out.result["component_count"], json!(0));
        assert_eq!(out.result["discovered"], json!(0));
        assert_eq!(out.result["pending_count"], json!(0));

        // A missing required arg is a structured invalid_argument (never a panic).
        let err = call(&db, DESIGN_INVENTORY_TOOL, json!({})).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);

        // A foreign tool is not ours.
        assert!(dispatch_design_tool(&db, "list_projects", &json!({})).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (review #45): an agent can RE-SYNTHESIZE / correct a component already in the
    /// catalogue — its template/params/states are updated in place (no delete-then-recreate), its
    /// provenance survives, the bundle's INPUT hash is recorded (so the next inventory sees it up to
    /// date), and a later source change puts it back on the work list.
    #[test]
    fn design_refresh_component_updates_an_existing_component_and_records_the_input_hash() {
        let _g = db_guard();
        let root = scratch_dir("refresh");
        write_file(
            &root,
            "src/Button.tsx",
            "export const Button = () => <button/>;",
        );
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        // The agent synthesizes the bundle → stored, with the params/states it derived.
        let refreshed = call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": project_id,
                "name": "Button",
                "template": "<button class=\"ds-btn\" data-state=\"{{state}}\">{{label}}</button>",
                "params_json": "[{\"name\":\"label\"}]",
                "states_json": "[\"idle\",\"loading\"]",
            }),
        )
        .expect("ok");
        assert_eq!(refreshed.effects, vec![ChangedTopic::Prototypes]);
        let component = &refreshed.result["component"];
        assert_eq!(component["name"], json!("Button"));
        assert!(component["template"].as_str().unwrap().contains("ds-btn"));
        assert_eq!(component["params_json"], json!("[{\"name\":\"label\"}]"));
        assert_eq!(
            component["source_path"],
            json!("src/Button.tsx"),
            "a refresh preserves the provenance the discovery recorded"
        );
        assert!(
            refreshed.result["input_hash"].as_str().is_some(),
            "the bundle's INPUT hash is recorded at synthesis time"
        );

        // The loop is INCREMENTAL: nothing pending until the SOURCE moves.
        let after = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(after.result["pending_count"], json!(0));
        assert_eq!(after.result["synthesized_count"], json!(1));
        write_file(
            &root,
            "src/Button.tsx",
            "export const Button = () => <button className=\"x\"/>;",
        );
        let stale = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(
            stale.result["pending_count"],
            json!(1),
            "a changed bundle is re-proposed for synthesis"
        );

        // A SECOND refresh of the same component works (the review-#45 gap: no invalid_state).
        let corrected = call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({ "project_id": project_id, "name": "Button", "template": "<button>{{label}}</button>" }),
        )
        .expect("a re-synthesis is allowed");
        assert_eq!(
            corrected.result["component"]["template"],
            json!("<button>{{label}}</button>")
        );
        // The catalogue holds ONE Button (updated in place, never duplicated).
        let stored = db.with_conn(|c| list_components(c, &project_id)).unwrap();
        assert_eq!(stored.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (review — the hash is taken at WRITE time): a source edited BETWEEN the
    /// agent's READ of the bundle and the WRITE of its synthesis must NOT be recorded as handled.
    /// Quoting the `input_hash` the read returned makes the write OPTIMISTIC: the template is still
    /// stored (no work lost) but the bundle stays PENDING, and the response says so.
    #[test]
    fn design_refresh_component_leaves_a_bundle_pending_when_its_sources_moved_mid_synthesis() {
        let _g = db_guard();
        let root = scratch_dir("stale-hash");
        write_file(
            &root,
            "src/Button.tsx",
            "export const Button = () => <button/>;",
        );
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        // The agent READS the bundle and keeps the input hash it was handed.
        let read = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "component": "Button" }),
        )
        .expect("ok");
        let read_hash = read.result["bundle"]["input_hash"]
            .as_str()
            .expect("the read hands back the bundle's input hash")
            .to_string();

        // …a human edits the source WHILE the LLM synthesizes…
        write_file(
            &root,
            "src/Button.tsx",
            "export const Button = () => <button className=\"new\"/>;",
        );

        // …and the agent stores its (now stale) synthesis, quoting the hash it read.
        let stale = call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": project_id,
                "name": "Button",
                "template": "<button>{{label}}</button>",
                "input_hash": read_hash,
            }),
        )
        .expect("the write is accepted — the work is not thrown away");
        assert_eq!(stale.result["synthesized"], json!(false));
        assert_eq!(stale.result["source_changed"], json!(true));
        assert_eq!(
            stale.result["input_hash"],
            Value::Null,
            "NOTHING was recorded — the component is not up to date"
        );
        assert!(stale.result["current_input_hash"].as_str().is_some());
        assert_eq!(
            stale.result["component"]["template"],
            json!("<button>{{label}}</button>"),
            "the synthesized template IS stored (no work lost)"
        );

        // The change is therefore STILL proposed — never silently swallowed.
        let after = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(
            after.result["pending_count"],
            json!(1),
            "a source that moved mid-synthesis is re-proposed"
        );

        // Re-reading and refreshing with the CURRENT hash closes the loop.
        let fresh = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "component": "Button" }),
        )
        .expect("ok");
        let fresh_hash = fresh.result["bundle"]["input_hash"].as_str().unwrap();
        let ok = call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": project_id,
                "name": "Button",
                "template": "<button class=\"new\">{{label}}</button>",
                "input_hash": fresh_hash,
            }),
        )
        .expect("ok");
        assert_eq!(ok.result["synthesized"], json!(true));
        assert_eq!(ok.result["source_changed"], json!(false));
        assert_eq!(
            call(
                &db,
                DESIGN_INVENTORY_TOOL,
                json!({ "project_id": project_id })
            )
            .expect("ok")
            .result["pending_count"],
            json!(0)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (review #43 — "réponse hors limite MCP"): the response is bounded at the
    /// RESPONSE level, not only per item. The LIST shape PAGES (`limit` + `offset`, `total` carrying
    /// the real scale), and a bundle READ whose files add up past the budget is truncated with every
    /// file still LISTED (path / role / real size) so nothing disappears silently.
    #[test]
    fn design_inventory_pages_its_list_and_caps_the_whole_response() {
        let _g = db_guard();
        let root = scratch_dir("paged");
        for i in 0..6 {
            write_file(&root, &format!("src/C{i}.tsx"), "<div/>");
        }
        // A bundle whose files ALONE blow the response budget: a class + its template + styles +
        // variants, each already at the 32 KiB per-file cap.
        let filler = "y".repeat(MAX_BUNDLE_FILE_BYTES);
        write_file(
            &root,
            "src/heavy/Heavy.tsx",
            &format!("export const Heavy = () => <div/>; // {filler}"),
        );
        write_file(
            &root,
            "src/heavy/Heavy.module.css",
            &format!(".h {{}} /* {filler} */"),
        );
        write_file(
            &root,
            "src/heavy/Heavy.variants.ts",
            &format!("// {filler}"),
        );

        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Big", &root);

        // --- LIST: paged, and `total` says how much there really is. ---
        let page = |offset: usize| {
            call(
                &db,
                DESIGN_INVENTORY_TOOL,
                json!({ "project_id": project_id, "limit": 3, "offset": offset }),
            )
            .expect("ok")
        };
        let first = page(0);
        assert_eq!(first.result["total"], json!(7), "6 + the heavy bundle");
        assert_eq!(first.result["returned"], json!(3));
        assert_eq!(first.result["truncated"], json!(true));
        // Walk the WHOLE list by following `next_offset` — the paged read the human finding asked
        // for ("la liste complète reste accessible via une lecture paginée").
        let mut walked: Vec<String> = bundle_names(&first)
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut out = first;
        while let Some(offset) = out.result["next_offset"].as_u64() {
            out = page(offset as usize);
            assert_eq!(out.result["offset"], json!(offset));
            walked.extend(bundle_names(&out).into_iter().map(str::to_string));
        }
        assert_eq!(walked.len(), 7, "paging walks the WHOLE list: {walked:?}");
        assert_eq!(
            out.result["truncated"],
            json!(false),
            "the last page is not truncated"
        );

        // --- READ: the whole response stays under the byte budget. ---
        let read = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "component": "Heavy" }),
        )
        .expect("ok");
        let files = read.result["files"].as_array().expect("files");
        assert_eq!(files.len(), 3, "every bundle file is still LISTED");
        let echoed: usize = files
            .iter()
            .map(|f| f["content"].as_str().unwrap_or_default().len())
            .sum();
        assert!(
            echoed <= MAX_RESPONSE_BYTES,
            "the RESPONSE is budgeted, not just each file: {echoed} bytes"
        );
        assert_eq!(read.result["truncated"], json!(true));
        assert!(
            files
                .iter()
                .all(|f| f["bytes"].as_u64().unwrap_or(0) > MAX_BUNDLE_FILE_BYTES as u64),
            "each file still reports its REAL size"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (review — `limit: 0` and the per-page re-inventory): the paging surface can
    /// neither STALL nor cost a full scan.
    ///   * `limit` is clamped to at least 1, so a page always advances: `next_offset` can never come
    ///     back equal to `offset` (an agent following it while `truncated` used to loop forever on
    ///     `limit: 0`, relaunching a whole project inventory at every turn);
    ///   * only `offset: 0` SCANS: a later page is a NON-MUTATING catalogue read (no
    ///     `ChangedTopic`, no walk), and walking the whole list still yields every component.
    #[test]
    fn design_inventory_paging_always_progresses_and_only_the_first_page_scans() {
        let _g = db_guard();
        let root = scratch_dir("paging");
        for i in 0..6 {
            write_file(&root, &format!("src/C{i}.tsx"), "<div/>");
        }
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Big", &root);

        // `limit: 0` is clamped to 1 — the page is non-empty and `next_offset` MOVES.
        let stalled = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "limit": 0 }),
        )
        .expect("ok");
        assert_eq!(
            stalled.result["returned"],
            json!(1),
            "limit 0 → clamped to 1"
        );
        assert_eq!(stalled.result["truncated"], json!(true));
        assert_eq!(
            stalled.result["next_offset"],
            json!(1),
            "next_offset must never equal offset — the pagination has to progress"
        );

        // Walk EVERY page with limit 0 and prove the walk terminates AND is complete.
        let mut names: Vec<String> = bundle_names(&stalled)
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut out = stalled;
        let mut turns = 0;
        while let Some(offset) = out.result["next_offset"].as_u64() {
            turns += 1;
            assert!(turns < 50, "the paging walk must terminate");
            out = call(
                &db,
                DESIGN_INVENTORY_TOOL,
                json!({ "project_id": project_id, "limit": 0, "offset": offset }),
            )
            .expect("ok");
            assert_eq!(out.result["offset"], json!(offset));
            assert_ne!(
                out.result["next_offset"],
                json!(offset),
                "a page never hands back its own offset"
            );
            assert_eq!(
                out.result["mode"],
                json!("catalogue"),
                "a page past the first is a CATALOGUE read, not a re-scan"
            );
            assert!(
                out.effects.is_empty(),
                "a page mutates nothing: no walk, no upsert, no change notification"
            );
            names.extend(bundle_names(&out).into_iter().map(str::to_string));
        }
        names.sort();
        assert_eq!(
            names,
            vec!["C0", "C1", "C2", "C3", "C4", "C5"],
            "paging still walks the WHOLE list"
        );

        // A SCAN that changes nothing reports no change either (it used to broadcast per page).
        let idle = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert!(
            idle.effects.is_empty(),
            "an inventory that moved nothing notifies nothing"
        );

        // Supplying `globs` is a CONFIG WRITE — it SCANS whatever the offset, so an override is
        // never silently dropped because the caller also happened to page.
        write_file(&root, "src/late/Late.jsx", "<div/>");
        let overridden = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "globs": ["**/*.jsx"], "offset": 0 }),
        )
        .expect("ok");
        assert_eq!(overridden.result["mode"], json!("bundles"), "it SCANNED");
        assert_eq!(bundle_names(&overridden), vec!["Late"]);
        let paged_with_globs = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id, "globs": ["**/*.tsx"], "offset": 2, "limit": 2 }),
        )
        .expect("ok");
        assert_eq!(
            paged_with_globs.result["mode"],
            json!("bundles"),
            "an offset does not make the tool drop a persisted glob override"
        );
        assert_eq!(paged_with_globs.result["offset"], json!(2));
        assert_eq!(
            paged_with_globs.result["discovered"],
            json!(6),
            "the new override really took effect (back to the .tsx set)"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (review #44): a component can be REMOVED from the catalogue by the agent —
    /// `db::delete_component` existed but was reachable from nowhere. Unknown name → `invalid_id`
    /// (never a silent no-op), and the removal is a `Prototypes` change.
    #[test]
    fn design_delete_component_removes_an_entry_and_refuses_an_unknown_one() {
        let _g = db_guard();
        let root = scratch_dir("delete");
        write_file(&root, "src/Button.tsx", "<button/>");
        write_file(&root, "src/emails/Welcome.tsx", "<div/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(
            db.with_conn(|c| list_components(c, &project_id))
                .unwrap()
                .len(),
            2
        );

        // An entry that does not belong in the catalogue is REMOVED.
        let out = call(
            &db,
            DESIGN_DELETE_COMPONENT_TOOL,
            json!({ "project_id": project_id, "name": "Welcome" }),
        )
        .expect("ok");
        assert_eq!(out.effects, vec![ChangedTopic::Prototypes]);
        assert_eq!(out.result["deleted"], json!(true));
        assert_eq!(out.result["component"]["name"], json!("Welcome"));
        // …and the tool no longer LIES about what that achieves (finding
        // 01KYSCX4PNKTMX17YVG9XD8KBZ): the file is still there and still matches the recipe, so the
        // removal is EPHEMERAL and the response says it, with the two corrections.
        assert_eq!(out.result["ephemeral"], json!(true));
        assert_eq!(out.result["verdict"], json!("elected"));
        assert_eq!(out.result["source_path"], json!("src/emails/Welcome.tsx"));
        let warnings = out.result["warnings"].to_string();
        for expected in ["EPHEMERAL", "RESTRICT the recipe", "exclude_source: true"] {
            assert!(warnings.contains(expected), "{expected:?} in {warnings}");
        }
        let left = db.with_conn(|c| list_components(c, &project_id)).unwrap();
        assert_eq!(
            left.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["Button"]
        );

        // A rescan proves the warning right — and `exclude_source` is what makes it last.
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(
            db.with_conn(|c| list_components(c, &project_id))
                .unwrap()
                .len(),
            2,
            "the deleted row came straight back, pending"
        );
        let out = call(
            &db,
            DESIGN_DELETE_COMPONENT_TOOL,
            json!({ "project_id": project_id, "name": "Welcome", "exclude_source": true }),
        )
        .expect("ok");
        assert_eq!(out.result["deleted"], json!(true));
        assert_eq!(
            out.result["excluded"],
            json!(["src/emails/Welcome.tsx"]),
            "the exclusion is the recipe's own `exclude` list — the discovery already honours it"
        );
        assert_eq!(out.result["recipe_materialized"], json!(true));
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(
            db.with_conn(|c| list_components(c, &project_id))
                .unwrap()
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>(),
            vec!["Button".to_string()],
            "THIS removal lasts: the source is out of the discovery, nothing else changed"
        );

        // An unknown name is a structured refusal, so the agent knows nothing was removed.
        let err = call(
            &db,
            DESIGN_DELETE_COMPONENT_TOOL,
            json!({ "project_id": project_id, "name": "Welcome" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
        // A missing arg is invalid_argument, never a panic.
        assert_eq!(
            call(
                &db,
                DESIGN_DELETE_COMPONENT_TOOL,
                json!({ "project_id": project_id })
            )
            .unwrap_err()
            .code,
            RpcCode::InvalidArgument
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`): deleting a component OTHER templates
    /// still REFERENCE names the hosts. A reference resolves at render time, so the row's removal
    /// breaks every host that carries one — a readable error, but one that would otherwise surface
    /// in a preview, later, with nothing linking it back to this gesture.
    #[test]
    fn deleting_a_referenced_component_names_the_hosts_that_still_use_it() {
        let _g = db_guard();
        let root = scratch_dir("referenced-delete");
        write_file(&root, "src/Button.tsx", "<button/>");
        write_file(&root, "src/Spinner.tsx", "<i/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        // `Button` REFERENCES the shared `Spinner` instead of inlining a copy of it.
        call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": project_id,
                "name": "Button",
                "template": "<button><span data-nyx-use=\"Spinner\"></span>{{label}}</button>",
            }),
        )
        .expect("ok");

        let out = call(
            &db,
            DESIGN_DELETE_COMPONENT_TOOL,
            json!({ "project_id": project_id, "name": "Spinner" }),
        )
        .expect("ok");
        let warnings = out.result["warnings"].to_string();
        for expected in ["still REFERENCE 'Spinner'", "'Button'", "FAILS to render"] {
            assert!(warnings.contains(expected), "{expected:?} in {warnings}");
        }
        // …and the render really does refuse it now, naming the same thing.
        let catalogue: Vec<crate::render::RenderComponent> = db
            .with_conn(|c| reference_catalogue(c, &project_id))
            .expect("catalogue");
        let err = crate::render::render_composition(
            &catalogue,
            &[],
            &json!({ "component": "Button", "params": { "label": "x" } }).to_string(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            crate::render::RenderError::UnknownReference {
                host: "Button".into(),
                reference: "Spinner".into()
            }
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (review #45): the refresh is DISTINCT from the `[NEW]` creation — it REFUSES
    /// an unknown component (`invalid_id`, pointing at proto_author_component) — and the stored
    /// template must be LOGIC-LESS: an LLM synthesis carrying a block helper is REFUSED
    /// (`invalid_argument`), never stored.
    #[test]
    fn design_refresh_component_refuses_unknown_names_and_logic_templates() {
        let _g = db_guard();
        let root = scratch_dir("refresh-guards");
        write_file(&root, "src/Button.tsx", "<button/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        // Unknown component → invalid_id (creation stays proto_author_component's job).
        let err = call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({ "project_id": project_id, "name": "Ghost", "template": "<div/>" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
        assert!(err.message.contains("proto_author_component"));

        // A LOGIC-carrying synthesis is refused and NOTHING is stored.
        let err = call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": project_id,
                "name": "Button",
                "template": "{{#if open}}<div/>{{/if}}",
            }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(err.message.contains("LOGIC-LESS"));
        let stored = db
            .with_conn(|c| db::get_component_by_name(c, &project_id, "Button"))
            .unwrap()
            .unwrap();
        assert!(
            stored.template.is_empty(),
            "a refused template is never stored"
        );

        // A missing `template` is a structured invalid_argument.
        let err = call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({ "project_id": project_id, "name": "Button" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (finding `01KYSZNX4TAVSGEVBVJM7W2B21`): a PARTIAL include is refused at the
    /// write, by name, with `data-nyx-use` named as the form that works — and nothing is stored.
    ///
    /// `{{> Spinner}}` is the natural spelling for anyone who knows Handlebars, and until now it
    /// WORKED: every catalogue component was registered as a partial, so the include expanded with
    /// the HOST's context — the host's params leaking in by name collision, the sub-component's axes
    /// resolved nowhere, and a cycle through it bounded by nothing (`MAX_REFERENCE_DEPTH` and
    /// `MAX_REFERENCE_EXPANSIONS` only bound the `data-nyx-use` pass). No warning, no error, no
    /// signal of any kind. The manual taught the right form; teaching is not preventing.
    #[test]
    fn design_refresh_component_refuses_a_partial_and_names_data_nyx_use() {
        let _g = db_guard();
        let root = scratch_dir("refresh-partial");
        write_file(&root, "src/Button.tsx", "<button/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        for template in [
            "<button>{{> Spinner}}{{label}}</button>",
            // whitespace-control and the quoted form are the same door.
            "<button>{{~> Spinner}}</button>",
            "<button>{{> \"Spinner\"}}</button>",
        ] {
            let err = call(
                &db,
                DESIGN_REFRESH_COMPONENT_TOOL,
                json!({ "project_id": project_id, "name": "Button", "template": template }),
            )
            .unwrap_err();
            assert_eq!(err.code, RpcCode::InvalidArgument, "{template}");
            assert!(
                err.message.contains("PARTIAL") && err.message.contains("data-nyx-use=\"Spinner\""),
                "the refusal NAMES the form that works, on the component asked for: {}",
                err.message
            );
            let stored = db
                .with_conn(|c| db::get_component_by_name(c, &project_id, "Button"))
                .unwrap()
                .unwrap();
            assert!(
                stored.template.is_empty(),
                "a refused template is never stored"
            );
        }

        // …and the form the message names is ACCEPTED — a refusal that leaves no way through is a
        // wall, not a guard.
        call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": project_id,
                "name": "Button",
                "template": "<button><i data-nyx-use=\"Spinner\"></i>{{label}}</button>",
            }),
        )
        .expect("data-nyx-use is the way through");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (finding 01KYPSG0HPG2D28CMJEXRKNEVK): a param `type` the UI cannot render is
    /// REFUSED, and NOTHING is stored — it is no longer silently turned into a free-text field.
    ///
    /// The report: `{ "name": "buttonVariant", "type": "enum", "options": [...] }`. The UI's parser
    /// fell back to `"text"` for the unknown word, dropped the `options`, and the write answered
    /// `ok` — "j'ai bien fourni options, l'UI les a ignorées". The word was wrong; the CONTRACT was
    /// nowhere. Now the write names it, the manual lists the six types, and a correct `select`
    /// (or the six-strong vocabulary generally) goes through untouched.
    #[test]
    fn design_refresh_component_refuses_an_unknown_param_type() {
        let _g = db_guard();
        let root = scratch_dir("param-types");
        write_file(&root, "src/Button.tsx", "<button/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        let refresh = |params: &str| {
            call(
                &db,
                DESIGN_REFRESH_COMPONENT_TOOL,
                json!({
                    "project_id": project_id,
                    "name": "Button",
                    "template": "<button>{{label}}</button>",
                    "params_json": params,
                }),
            )
        };

        // THE reported shape: a guessed `enum`. Refused, naming the param and the vocabulary.
        let err =
            refresh("[{\"name\":\"buttonVariant\",\"type\":\"enum\",\"options\":[\"a\",\"b\"]}]")
                .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains("'buttonVariant'")
                && err.message.contains("UNKNOWN type")
                && err.message.contains("\"select\"")
                && err.message.contains("\"boolean\""),
            "the refusal names the param and the accepted types: {}",
            err.message
        );
        assert!(
            err.message.contains("states_json"),
            "…and points a VARIANT axis at its single home"
        );
        let stored = db
            .with_conn(|c| db::get_component_by_name(c, &project_id, "Button"))
            .unwrap()
            .unwrap();
        assert!(
            stored.template.is_empty() && stored.params_json.is_none(),
            "a refused write stores NOTHING (the guard runs before the transaction)"
        );

        // The SIX are accepted, in the ARRAY form the manual documents.
        refresh(
            "[{\"name\":\"label\",\"type\":\"text\"},{\"name\":\"body\",\"type\":\"html\"},\
             {\"name\":\"state\",\"type\":\"state\"},{\"name\":\"on\",\"type\":\"boolean\"},\
             {\"name\":\"n\",\"type\":\"number\"},\
             {\"name\":\"align\",\"type\":\"select\",\"options\":[\"left\",\"right\"]}]",
        )
        .expect("the six documented types go through");
        // RETRO-COMPATIBLE: an OMITTED type has always meant `text` — a synthesis written before
        // this contract must not become unstorable.
        refresh("[{\"name\":\"label\"}]").expect("an omitted type stays legal");

        // …and the SHAPE itself (finding `01KZ3JS6WFVGYS6JKB9C4J50HR`): every blob that would
        // declare NO param is refused, naming what is wrong with it.
        let err = refresh("not json").unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(err.message.contains("is not valid JSON"));
        let err = refresh("\"label\"").unwrap_err();
        assert!(err.message.contains("not an ARRAY"), "{}", err.message);
        // The OBJECT-map root: readable on rows already stored, never written anew — the manual
        // documents an array, and an object has nowhere to put `name`.
        let err = refresh("{\"label\":{\"type\":\"text\"}}").unwrap_err();
        assert!(
            err.message.contains("JSON OBJECT") && err.message.contains("`name`"),
            "the object root is refused with the array to write instead: {}",
            err.message
        );
        let err = refresh("[{\"type\":\"text\"}]").unwrap_err();
        assert!(
            err.message.contains("no usable `name`"),
            "a descriptor with no name declares nothing: {}",
            err.message
        );
        let err = refresh("[\"label\"]").unwrap_err();
        assert!(err.message.contains("NON-OBJECT entry"), "{}", err.message);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (finding `01KZ3JS6WFVGYS6JKB9C4J50HR`): A `states_json` NYX CANNOT READ IS
    /// REFUSED AT THE WRITE, never stored with `synthesized: true` and `warnings: []`.
    ///
    /// [`parse_axes`] answers "declares nothing" to anything it cannot read — the right rule for
    /// READING a stored row, and exactly the wrong one at a WRITE, where the author still holds the
    /// text. So the four shapes that make a component silently axis-LESS are named here, while every
    /// shape the reader really honours keeps going through untouched.
    #[test]
    fn design_refresh_component_refuses_a_states_json_that_declares_nothing() {
        let _g = db_guard();
        let root = scratch_dir("states-shape");
        write_file(&root, "src/Button.tsx", "<button/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        let refresh = |states: &str| {
            call(
                &db,
                DESIGN_REFRESH_COMPONENT_TOOL,
                json!({
                    "project_id": project_id,
                    "name": "Button",
                    "template": "<button>{{label}}</button>",
                    "states_json": states,
                }),
            )
        };

        // THE FOUR REFUSALS, each naming the field.
        for (blob, expected) in [
            ("{\"axes\": [", "is not valid JSON"),
            ("{\"axes\": \"variant\"}", "which is not an ARRAY"),
            ("42", "neither the canonical object"),
            (
                "{\"axes\":[{\"name\":\"variant\"}]}",
                "with `values` = null",
            ),
            (
                "{\"axes\":[{\"values\":[{\"name\":\"a\"}]}]}",
                "no usable `name`",
            ),
            (
                "{\"axes\":[{\"name\":\"variant\",\"values\":[]}]}",
                "EMPTY `values`",
            ),
        ] {
            let err = refresh(blob).unwrap_err();
            assert_eq!(err.code, RpcCode::InvalidArgument);
            assert!(
                err.message.contains(expected),
                "`{blob}` must be refused with '{expected}': {}",
                err.message
            );
        }
        // NOTHING was stored by any of them.
        let stored = db
            .with_conn(|c| db::get_component_by_name(c, &project_id, "Button"))
            .unwrap()
            .unwrap();
        assert!(
            stored.states_json.is_none(),
            "a refused write stores NOTHING (the guard runs before the transaction)"
        );

        // …AND EVERY SHAPE THE READER REALLY HONOURS STILL GOES THROUGH.
        for blob in [
            r#"{"axes":[{"name":"variant","default":"a","values":[{"name":"a","classes":"bg-primary"},{"name":"b"}]}]}"#,
            r#"[{"name":"variant","values":[{"name":"a","classes":"bg-primary"}]}]"#,
            r#"{"variant":["a","b"]}"#,
            r#"["default","hover"]"#,
        ] {
            refresh(blob).unwrap_or_else(|e| panic!("`{blob}` is an accepted shape: {e:?}"));
            assert!(
                !parse_axes(Some(blob)).is_empty(),
                "`{blob}` really declares an axis — the guard and the reader agree"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (finding 01KYPSG1HM2GPJMNH2SP3DDADH): a synthesis that DECLARES states its
    /// template renders no `data-nyx-state-case` for is SIGNALLED at the write — rather than
    /// letting the user click a dropdown that cannot change one pixel.
    ///
    /// The three shapes, in one test: the degenerate one (states, no case at all), the PARTIAL one
    /// (a declared name with no case — selecting it hides every case, so the aperçu goes empty), and
    /// the correct one (one case per declared state → no warning). The synthesis is stored either
    /// way: this is a DIAGNOSTIC, not a guard (see `variance_warnings` for why refusing would be
    /// wrong here — `{{state}}` substitution is a legitimate second way to render a variant).
    #[test]
    fn design_refresh_component_warns_when_declared_states_have_no_state_case() {
        let _g = db_guard();
        let root = scratch_dir("state-cases");
        write_file(&root, "src/Button.tsx", "<button/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        let refresh = |template: &str, states: &str| {
            call(
                &db,
                DESIGN_REFRESH_COMPONENT_TOOL,
                json!({
                    "project_id": project_id,
                    "name": "Button",
                    "template": template,
                    "states_json": states,
                }),
            )
            .expect("ok")
        };
        let warnings = |out: &ToolOutcome| -> Vec<String> {
            out.result["warnings"]
                .as_array()
                .expect("warnings is always an array")
                .iter()
                .map(|w| w.as_str().unwrap_or_default().to_string())
                .collect()
        };

        // 1. THE REPORTED CASE: three variants declared, one hard-coded rendering. The template is
        //    stored (it is not wrong, it is INCOMPLETE) and the agent is told what it will not do.
        let degenerate = refresh(
            "<button class=\"bg-primary px-4\">{{label}}</button>",
            "[\"default\",\"destructive\",\"outline\"]",
        );
        let w = warnings(&degenerate);
        assert_eq!(w.len(), 1, "one warning, naming the whole defect");
        assert!(
            w[0].contains("NO `data-nyx-state-case`")
                && w[0].contains("default, destructive, outline")
                && w[0].contains("cannot change one pixel"),
            "the warning names the mechanism, the declared states and the consequence: {}",
            w[0]
        );
        assert!(
            !degenerate.result["component"]["template"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "the synthesis is STORED — the write warns, it does not refuse"
        );

        // 2. PARTIAL: `outline` is declared but has no case → selecting it empties the aperçu.
        let partial = refresh(
            "<button data-nyx-state-case=\"default\" class=\"bg-primary\">{{label}}</button>\
             <button data-nyx-state-case=\"destructive\" class=\"bg-destructive\">{{label}}</button>",
            "[\"default\",\"destructive\",\"outline\"]",
        );
        let w = warnings(&partial);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("NO matching `data-nyx-state-case`")
                && w[0].contains("outline")
                && !w[0].contains("default,"),
            "only the state without a case is named: {}",
            w[0]
        );

        // 3. The CONTRACT honoured: one case per declared state → nothing to report.
        let complete = refresh(
            "<button data-nyx-state-case=\"default\" class=\"bg-primary\">{{label}}</button>\
             <button data-nyx-state-case=\"destructive\" class=\"bg-destructive\">{{label}}</button>",
            "[\"default\",\"destructive\"]",
        );
        assert!(
            warnings(&complete).is_empty(),
            "a complete synthesis is warned about nothing"
        );

        // 4. The OTHER honest way to vary with the state — substituting the param itself — is not
        //    a defect either (this is why the check WARNS instead of refusing).
        let substituted = refresh(
            "<button class=\"btn btn-{{state}}\">{{label}}</button>",
            "[\"default\",\"destructive\"]",
        );
        assert!(warnings(&substituted).is_empty());

        // 5. A case the states never declare can never be SELECTED — the name-typo trap.
        let stray = refresh(
            "<button data-nyx-state-case=\"destructif\">{{label}}</button>",
            "[\"destructive\"]",
        );
        let w = warnings(&stray);
        assert_eq!(w.len(), 2, "both directions of the contract are checked");
        assert!(
            w.iter()
                .any(|m| m.contains("states_json does NOT declare: destructif")),
            "the stray case is named: {w:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (finding 01KYQ0R84KVF6QVQXQ9YVG9Z8T): the REFONTE of the variance model —
    /// NAMED AXES whose values carry their own classes — and the PEDAGOGICAL WARNINGS that guard it.
    ///
    /// The reported defect was one flou with four faces: booleans inert, a single pilotable axis, a
    /// variant declarable as a param OR as a state, and a template that had to enumerate every case.
    /// The write path is where a synthesis can still be fixed for free, so each warning must NAME
    /// THE COMPONENT, THE FAULTY FIELD AND THE CORRECTION — the style a test agent said "m'a
    /// débloqué instantanément".
    #[test]
    fn design_refresh_component_warns_on_every_way_the_variance_model_is_broken() {
        let _g = db_guard();
        let root = scratch_dir("variance-axes");
        write_file(&root, "src/Button.tsx", "<button/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        let refresh = |template: &str, states: Value, params: Value| {
            call(
                &db,
                DESIGN_REFRESH_COMPONENT_TOOL,
                json!({
                    "project_id": project_id,
                    "name": "Button",
                    "template": template,
                    "states_json": states.to_string(),
                    "params_json": params.to_string(),
                }),
            )
            .expect("ok")
        };
        let warnings = |out: &ToolOutcome| -> Vec<String> {
            out.result["warnings"]
                .as_array()
                .expect("warnings is always an array")
                .iter()
                .map(|w| w.as_str().unwrap_or_default().to_string())
                .collect()
        };

        // THE TARGET SHAPE: ONE element in the template, TWO orthogonal axes + a structural boolean,
        // and ONE text param. Nothing to warn about — this is what the manual now asks for.
        let good = refresh(
            "<button class=\"inline-flex rounded-md\">\
             <i data-nyx-if=\"loading=on\">…</i>\
             <span data-nyx-if-not=\"loading=on\">{{label}}</span></button>",
            json!({ "axes": [
                { "name": "variant", "values": [
                    { "name": "primary", "classes": "bg-primary text-primary-foreground" },
                    { "name": "destructive", "classes": "bg-destructive" } ] },
                { "name": "size", "values": [
                    { "name": "md", "classes": "h-9 px-4" },
                    { "name": "lg", "classes": "h-11 px-6" } ] },
                { "name": "loading", "values": [
                    { "name": "off" }, { "name": "on", "classes": "opacity-70" } ] }
            ]}),
            json!([{ "name": "label", "type": "text", "required": true, "preview": "Enregistrer" }]),
        );
        assert!(
            warnings(&good).is_empty(),
            "a component with two axes + a structural boolean + one param is complete: {:?}",
            warnings(&good)
        );

        // 1. AN INERT AXIS — NO value carries classes, no case, no condition. Named axis BY value,
        //    with the correction. (A BARE value beside a sibling that DOES carry classes is a
        //    different thing entirely and is credited — see
        //    `variance_warnings_credit_a_bare_value_a_sibling_s_classes_discriminate`.)
        let inert = refresh(
            "<button class=\"rounded-md\">{{label}}</button>",
            json!({ "axes": [
                { "name": "variant", "values": [ { "name": "primary" }, { "name": "ghost" } ] }
            ]}),
            json!([{ "name": "label", "type": "text" }]),
        );
        let w = warnings(&inert);
        assert_eq!(w.len(), 1, "one warning, on the one broken axis: {w:?}");
        assert!(
            w[0].contains("component 'Button'")
                && w[0].contains("axis 'variant'")
                && w[0].contains("primary, ghost")
                && w[0].contains("NEITHER `classes` NOR")
                && w[0].contains("data-nyx-if=\"variant=primary\""),
            "the warning names the component, the axis, the values and the correction: {}",
            w[0]
        );
        assert!(
            !inert.result["component"]["template"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "the synthesis is STORED — the write warns, it does not refuse"
        );

        // 2. AN INERT BOOLEAN PARAM — symptom #1 of the finding, and the correction is the axis.
        let boolean = refresh(
            "<button class=\"rounded-md\">{{label}}</button>",
            json!({ "axes": [
                { "name": "variant", "values": [{ "name": "a", "classes": "bg-primary" }] } ]}),
            json!([{ "name": "label", "type": "text" }, { "name": "loading", "type": "boolean" }]),
        );
        let w = warnings(&boolean);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains("component 'Button'")
                && w[0].contains("param 'loading' is a `boolean`")
                && w[0].contains("change NOTHING")
                && w[0].contains("AN AXIS WITH TWO VALUES")
                && w[0].contains("data-nyx-if=\"loading=on\""),
            "the boolean warning names the component, the param and the correction: {}",
            w[0]
        );

        // 3. THE SAME NOTION DECLARED TWICE — a param AND an axis.
        let both = refresh(
            "<button class=\"rounded-md\">{{label}}</button>",
            json!({ "axes": [
                { "name": "variant", "values": [{ "name": "a", "classes": "bg-primary" }] } ]}),
            json!([{ "name": "variant", "type": "select", "options": ["a"] }]),
        );
        let w = warnings(&both);
        assert!(
            w.iter().any(|m| {
                m.contains("component 'Button'")
                    && m.contains("'variant' declared BOTH in params_json and as an axis")
                    && m.contains("DROP that entry from params_json")
            }),
            "the double declaration is named on both sides, with the fix: {w:?}"
        );

        // 4. A PARAM STILL TYPED `state` — the deprecated shape, with what to do instead.
        let state_param = refresh(
            "<button class=\"rounded-md\">{{label}}</button>",
            json!({ "axes": [
                { "name": "variant", "values": [{ "name": "a", "classes": "bg-primary" }] } ]}),
            json!([{ "name": "mode", "type": "state" }]),
        );
        let w = warnings(&state_param);
        assert!(
            w.iter().any(|m| {
                m.contains("component 'Button'")
                    && m.contains("param 'mode' declares the DEPRECATED type \"state\"")
                    && m.contains("NEVER a param")
                    && m.contains("states_json")
            }),
            "the deprecated type is named with its replacement: {w:?}"
        );

        // 5. A CONDITION ON AN AXIS NOBODY DECLARED — ignored by the shim (a blank aperçu is worse),
        //    so the write is the place that says it.
        let dangling = refresh(
            "<button class=\"rounded-md\"><i data-nyx-if=\"busy=yes\">…</i></button>",
            json!({ "axes": [
                { "name": "variant", "values": [{ "name": "a", "classes": "bg-primary" }] } ]}),
            json!([]),
        );
        let w = warnings(&dangling);
        assert!(
            w.iter()
                .any(|m| m.contains("does NOT declare: busy") && m.contains("IGNORED")),
            "a dangling condition is reported rather than blanking the preview: {w:?}"
        );

        // RETRO-COMPATIBILITY, at the write: the FLAT format keeps its previous verdicts exactly —
        // a legacy row re-stored as-is behaves as it always did.
        let legacy_ok = refresh(
            "<button data-nyx-state-case=\"default\" class=\"bg-primary\">{{label}}</button>\
             <button data-nyx-state-case=\"ghost\" class=\"bg-transparent\">{{label}}</button>",
            json!(["default", "ghost"]),
            json!([{ "name": "label", "type": "text" }]),
        );
        assert!(
            warnings(&legacy_ok).is_empty(),
            "a flat states_json honouring the case contract is still complete: {:?}",
            warnings(&legacy_ok)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (finding `01KYSCY4J8JANZEJPYME86B1VH`): THE BASELINE OF AN AXIS, WRITTEN BARE,
    /// IS NOT A DEFECT — and the validator must stop reproaching the shape the manual teaches.
    ///
    /// The reported contradiction: the manual's canonical boolean is `{ "name": "off" }` (no
    /// classes) beside `{ "name": "on", "classes": … }`, with ONE conditional element
    /// (`data-nyx-if="loading=on"`); the write answered "selecting them cannot change one pixel".
    /// Both were right about their own half — the baseline IS the absence of classes, and the
    /// rendering DOES change, through the complement. The check now asks whether a condition on the
    /// axis DISCRIMINATES the value rather than whether it NAMES it, which keeps the warning exact
    /// for the real inert case: no classes, no case, and nothing conditioning on that axis.
    #[test]
    fn variance_warnings_credit_the_baseline_a_complement_makes_visible() {
        // THE MANUAL'S CANONICAL SHAPE, verbatim: three axes, ONE element, ONE param — and a
        // `loading` axis whose baseline carries nothing while the spinner is gated on `on`.
        let canonical_template =
            "<button class=\"inline-flex items-center justify-center rounded-md font-medium\">\
             <i data-nyx-if=\"loading=on\" class=\"mr-2 animate-spin\">◌</i>{{label}}</button>";
        let canonical_states = r#"{ "axes": [
            { "name": "variant", "values": [
                { "name": "primary", "classes": "bg-primary px-4" },
                { "name": "outline", "classes": "border border-input px-4" } ] },
            { "name": "size", "values": [
                { "name": "md", "classes": "h-9 text-sm" },
                { "name": "lg", "classes": "h-11 text-base" } ] },
            { "name": "loading", "values": [
                { "name": "off" }, { "name": "on", "classes": "pointer-events-none opacity-70" } ] } ] }"#;
        let params =
            r#"[{ "name": "label", "type": "text", "required": true, "preview": "Enregistrer" }]"#;
        assert!(
            variance_warnings(
                "Button",
                canonical_template,
                Some(canonical_states),
                Some(params)
            )
            .is_empty(),
            "the manual's canonical example must pass the validator the manual teaches: {:?}",
            variance_warnings(
                "Button",
                canonical_template,
                Some(canonical_states),
                Some(params)
            )
        );

        // The COMPLEMENT written the other way round (`data-nyx-if-not`) was already credited, and
        // still is — the two spellings of one condition must not diverge.
        let complement_only = "<button class=\"rounded-md\">\
             <span data-nyx-if-not=\"loading=on\">{{label}}</span></button>";
        assert!(
            variance_warnings(
                "Button",
                complement_only,
                Some(canonical_states),
                Some(params)
            )
            .is_empty(),
            "data-nyx-if-not credits both values of the axis it conditions on"
        );

        // THE REAL INERT CASE, untouched: NO value of the axis carries classes, no case, and NOTHING
        // conditioning on it. `canonical_states` no longer qualifies — its `on` carries classes,
        // which DISCRIMINATES the bare `off` (finding `01KYT103MXTKFBZMKF968FGW9P`) — so the real
        // case is written with a boolean bare on BOTH sides.
        let bare_boolean = r#"{ "axes": [
            { "name": "variant", "values": [
                { "name": "primary", "classes": "bg-primary px-4" },
                { "name": "outline", "classes": "border border-input px-4" } ] },
            { "name": "loading", "values": [ { "name": "off" }, { "name": "on" } ] } ] }"#;
        let no_condition = "<button class=\"rounded-md\">{{label}}</button>";
        let w = variance_warnings("Button", no_condition, Some(bare_boolean), Some(params));
        assert_eq!(w.len(), 1, "the true inert axis is still reported: {w:?}");
        assert!(
            w[0].contains("axis 'loading'")
                && w[0].contains("NEITHER `classes` NOR")
                && w[0].contains("off, on")
                && w[0].contains("data-nyx-if=\"loading=off\""),
            "the warning still names the component, the axis, the values and the correction: {}",
            w[0]
        );

        // A condition true at EVERY value of its axis conditions on NOTHING: the element is there
        // whatever the axis does, so the values stay inert and are still reported. This is what
        // separates "the complement is credited" from a blunt "the axis is mentioned somewhere".
        let always_true = "<button class=\"rounded-md\">\
             <i data-nyx-if=\"loading=off|on\">◌</i>{{label}}</button>";
        let w = variance_warnings("Button", always_true, Some(bare_boolean), Some(params));
        assert_eq!(
            w.len(),
            1,
            "an always-true condition credits nothing: {w:?}"
        );
        assert!(
            w[0].contains("axis 'loading'") && w[0].contains("off"),
            "{}",
            w[0]
        );

        // BOTH CREDITS ARE AXIS-LOCAL: the `loading` axis is credited twice over (a condition names
        // it, and one of its values carries classes) and NEITHER reaches `variant`, whose two bare
        // values nothing discriminates.
        let other_axis = r#"{ "axes": [
            { "name": "variant", "values": [{ "name": "primary" }, { "name": "ghost" }] },
            { "name": "loading", "values": [{ "name": "off" }, { "name": "on", "classes": "opacity-70" }] } ] }"#;
        let w = variance_warnings("Button", canonical_template, Some(other_axis), Some(params));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains("axis 'variant'") && w[0].contains("primary, ghost"),
            "what credits `loading` credits nothing on `variant`: {}",
            w[0]
        );
    }

    /// DONE-CRITERION (finding `01KYT103MXTKFBZMKF968FGW9P`): A BARE VALUE IS NOT INERT WHEN A
    /// SIBLING VALUE OF THE SAME AXIS CARRIES CLASSES.
    ///
    /// Observed on `checkbox`: an axis with `enabled` bare beside `disabled` carrying `opacity-50`
    /// reported `enabled` inert. It is not — choosing `enabled` rather than `disabled` really does
    /// change the rendering, because it is the ABSENCE of `opacity-50` that defines it, exactly as it
    /// is the absence of classes that defines the baseline `off`. Finding
    /// `01KYSCY4J8JANZEJPYME86B1VH` introduced the right notion — a value is not inert when something
    /// DISCRIMINATES it from another value of the same axis — but implemented it for `data-nyx-if` /
    /// `-if-not` only. This is the same rule, applied to the other mechanism.
    ///
    /// It matters because the manual teaches, in so many words, that one NEVER invents classes on a
    /// baseline to silence a warning — and this false positive pushed towards exactly that.
    #[test]
    fn variance_warnings_credit_a_bare_value_a_sibling_s_classes_discriminate() {
        // THE REPORTED SHAPE, verbatim: no condition anywhere, one value bare, its sibling styled.
        let checkbox = "<label class=\"inline-flex items-center gap-2\">\
             <input type=\"checkbox\"/>{{label}}</label>";
        let two_valued = r#"{"axes":[{"name":"disabled","values":[
            {"name":"enabled"},
            {"name":"disabled","classes":"opacity-50 pointer-events-none"}]}]}"#;
        let params =
            r#"[{ "name": "label", "type": "text", "required": true, "preview": "J'accepte" }]"#;
        assert!(
            variance_warnings("Checkbox", checkbox, Some(two_valued), Some(params)).is_empty(),
            "the bare value is credited by its sibling's classes: {:?}",
            variance_warnings("Checkbox", checkbox, Some(two_valued), Some(params))
        );

        // …AND THE WARNING STAYS USEFUL on the real inert case: NO value of the axis carries classes,
        // nothing conditions on it, no state-case — selecting any of them really changes nothing.
        let inert = r#"{"axes":[{"name":"disabled","values":[
            {"name":"enabled"},{"name":"disabled"}]}]}"#;
        let w = variance_warnings("Checkbox", checkbox, Some(inert), Some(params));
        assert_eq!(w.len(), 1, "the true inert axis is still named: {w:?}");
        assert!(
            w[0].contains("axis 'disabled'") && w[0].contains("enabled, disabled"),
            "{}",
            w[0]
        );

        // The credit is a fact about the AXIS, not about the component: classes on one axis say
        // nothing about a second one whose values are all bare.
        let two_axes = r#"{"axes":[
            {"name":"variant","values":[{"name":"primary","classes":"bg-primary"},{"name":"ghost"}]},
            {"name":"loading","values":[{"name":"off"},{"name":"on"}]}]}"#;
        let w = variance_warnings("Button", "<button>{{label}}</button>", Some(two_axes), None);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains("axis 'loading'") && !w[0].contains("ghost"),
            "the bare `ghost` is credited by `primary`; the wholly-bare `loading` is not: {}",
            w[0]
        );
    }

    /// The AXES parser reads a stored `states_json` in every shape an agent writes it — and, above
    /// all, keeps reading the FLAT one the whole existing catalogue is written in.
    #[test]
    fn parse_axes_reads_every_shape_including_the_legacy_flat_list() {
        // The canonical object form.
        let axes = parse_axes(Some(
            r#"{"axes":[{"name":"variant","values":[{"name":"a","classes":"bg-primary"},"b"]},
                        {"name":"size","values":["md","lg"]}]}"#,
        ));
        assert_eq!(
            axes.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["variant", "size"]
        );
        assert!(axes[0].values[0].has_classes() && !axes[0].values[1].has_classes());

        // The bare array of axis objects (an entry with `values` IS an axis) — `avatar`'s 3 axes.
        let axes = parse_axes(Some(
            r#"[{"name":"type","values":["image","initials"]},
                {"name":"shape","values":["circle","square"]},
                {"name":"size","values":["sm","lg"]}]"#,
        ));
        assert_eq!(axes.len(), 3, "three orthogonal axes, not 8 combinations");

        // The tolerated map form.
        assert_eq!(
            parse_axes(Some(r#"{"variant":["a","b"]}"#))
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            vec!["variant"]
        );

        // THE LEGACY FLAT LIST — one anonymous, class-less axis. Both spellings.
        for blob in [
            r#"["default","ghost"]"#,
            r#"[{"name":"default"},{"name":"ghost"}]"#,
        ] {
            let axes = parse_axes(Some(blob));
            assert_eq!(axes.len(), 1);
            assert_eq!(axes[0].name, "state");
            assert!(axes[0].legacy);
            assert_eq!(primary_axis_states(&axes), vec!["default", "ghost"]);
            assert!(axes[0].values.iter().all(|v| !v.has_classes()));
        }

        // A blob nyx cannot read declares NOTHING, and never panics.
        for blob in [None, Some("nope"), Some("42"), Some(r#"{"axes":[]}"#)] {
            assert!(parse_axes(blob).is_empty());
        }
    }

    /// DONE-CRITERION (audit 01KYHYVD5V78FED8WRZ8NKZVF0): a BLANK template is REFUSED, and the row is
    /// left EXACTLY as it was.
    ///
    /// `require_str` only ever refused the EMPTY string, so `template: "   "` sailed through: the
    /// blank was written, `params_json` / `states_json` were nulled (content another actor —
    /// `proto_author_component` — authored, and that no scan can re-derive), the tool answered
    /// `synthesized: true`, and the row stopped carrying anything. The conditional `DELETE` of the
    /// CRITICAL could then prune it, entirely within its rights: the guard was not crossed, it was
    /// EMPTIED upstream. This pins both halves — the refusal, and the row's integrity after it.
    #[test]
    fn design_refresh_component_refuses_a_blank_template_and_leaves_the_row_intact() {
        let _g = db_guard();
        let root = scratch_dir("refresh-blank");
        write_file(
            &root,
            "src/Pricing.tsx",
            "export const Pricing = () => <div/>;",
        );
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        // A real synthesis first: template + the params/states shape.
        call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": project_id,
                "name": "Pricing",
                "template": "<section class=\"ds-pricing\">{{title}}</section>",
                "params_json": "[{\"name\":\"title\"}]",
                "states_json": "[\"default\",\"compact\"]",
            }),
        )
        .expect("ok");
        let before = db
            .with_conn(|c| db::get_component_by_name(c, &project_id, "Pricing"))
            .unwrap()
            .unwrap();
        assert!(before.visual_hash.is_some(), "it was synthesized");

        // Every flavour of BLANK is refused — whitespace, a newline, a non-breaking space.
        for blank in ["   ", "\n", "\t ", "\u{00a0}"] {
            let err = call(
                &db,
                DESIGN_REFRESH_COMPONENT_TOOL,
                json!({ "project_id": project_id, "name": "Pricing", "template": blank }),
            )
            .unwrap_err();
            assert_eq!(
                err.code,
                RpcCode::InvalidArgument,
                "blank {blank:?} refused"
            );
            assert!(
                err.message.contains("BLANK") && err.message.contains("design_delete_component"),
                "the refusal says the synthesis is empty and names the explicit removal path",
            );
        }

        // …and the row is EXACTLY as it was: template, params, states, hash — nothing touched.
        let after = db
            .with_conn(|c| db::get_component_by_name(c, &project_id, "Pricing"))
            .unwrap()
            .unwrap();
        assert_eq!(after.template, before.template);
        assert_eq!(after.params_json, before.params_json);
        assert_eq!(after.states_json, before.states_json);
        assert_eq!(after.visual_hash, before.visual_hash);

        // So the row still CARRIES content, and the automatic élagage still cannot touch it (the
        // conditional DELETE re-tests exactly that).
        assert_eq!(
            db.with_conn(|c| db::delete_component_if_unauthored(c, &after.id))
                .unwrap(),
            db::ConditionalDelete::KeptContentAppeared,
            "a row whose blank refresh was refused is NOT back on the pruning menu",
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (#43, sequence (A) of the audit): a `pending_only` traversal that follows
    /// `next_offset` returns EVERY pending bundle, exactly once.
    ///
    /// Reproduced: 6 components, all SYNTHESIZED then EDITED — so all 6 are pending for the reason
    /// `source_changed`. Page 0 (the scan) knew that reason; the later pages did NOT (they judged on
    /// the stored state alone, which only ever yields `never_synthesized`). With `limit: 3` the walk
    /// used to be `total: 6, returned: 3, next_offset: 3` then `total: 0, returned: 0,
    /// truncated: false` — HALF the work list unreachable, and not one signal saying so.
    #[test]
    fn a_pending_only_walk_returns_every_pending_bundle_exactly_once() {
        let _g = db_guard();
        let root = scratch_dir("pending-walk");
        for i in 0..6 {
            write_file(&root, &format!("src/C{i:02}.tsx"), "<div/>");
        }
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Big", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        // Synthesize all 6, then EDIT all 6 → 6 pending, every one of them `source_changed`.
        for i in 0..6 {
            call(
                &db,
                DESIGN_REFRESH_COMPONENT_TOOL,
                json!({ "project_id": project_id, "name": format!("C{i:02}"), "template": "<div/>" }),
            )
            .expect("refresh ok");
        }
        for i in 0..6 {
            write_file(
                &root,
                &format!("src/C{i:02}.tsx"),
                "<div class=\"edited\"/>",
            );
        }

        let page = |offset: usize| {
            call(
                &db,
                DESIGN_INVENTORY_TOOL,
                json!({
                    "project_id": project_id,
                    "limit": 3,
                    "offset": offset,
                    "pending_only": true,
                }),
            )
            .expect("ok")
        };
        let first = page(0);
        assert_eq!(first.result["pending_count"], json!(6));
        assert_eq!(first.result["total"], json!(6));
        let mut walked: Vec<String> = bundle_names(&first)
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut out = first;
        let mut turns = 0;
        while let Some(offset) = out.result["next_offset"].as_u64() {
            turns += 1;
            assert!(turns < 20, "the walk must terminate");
            out = page(offset as usize);
            assert_eq!(
                out.result["total"],
                json!(6),
                "`total` is the same on every page — a consumer can rely on it"
            );
            walked.extend(bundle_names(&out).into_iter().map(str::to_string));
        }
        assert_eq!(
            out.result["truncated"],
            json!(false),
            "the last page says the walk is complete"
        );
        walked.sort();
        assert_eq!(
            walked,
            vec!["C00", "C01", "C02", "C03", "C04", "C05"],
            "every pending bundle, EXACTLY once: {walked:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (#43, sequence (B) of the audit): a full traversal in the presence of SPARED
    /// ORPHANS neither duplicates nor omits an entry, and `total` does not move.
    ///
    /// Reproduced: two repos — `legacy/` (AAA, AAB) and `front/` (Z00..Z05), 8 catalogue rows. The
    /// `legacy/` root disappears, so its two rows are SPARED (`repo_not_scanned`) but stay in the
    /// catalogue. Page 0 listed the 6 DISCOVERED bundles (`total: 6`); the later pages listed the 8
    /// rows carrying a source (`total: 8`), and the shared offset then walked
    /// `[Z00,Z01,Z02,Z01,Z02,Z03,Z04,Z05]`: two entries returned twice, two never returned, and the
    /// total changing mid-walk. The population of every page is now the same one — what the LAST
    /// SCAN discovered.
    #[test]
    fn a_walk_with_spared_orphans_never_duplicates_nor_omits_an_entry() {
        let _g = db_guard();
        let base = scratch_dir("orphan-walk");
        write_file(&base, "legacy/src/AAA.tsx", "<div/>");
        write_file(&base, "legacy/src/AAB.tsx", "<div/>");
        for i in 0..6 {
            write_file(&base, &format!("front/src/Z{i:02}.tsx"), "<div/>");
        }
        let db = Db::in_memory();
        let project_id = db
            .with_conn(|c| {
                let (project, _ws) =
                    db::create_project(c, "Two", base.to_str().unwrap(), None).unwrap();
                db::create_repository(c, &project.id, "legacy", "legacy", false).unwrap();
                db::create_repository(c, &project.id, "front", "front", false).unwrap();
                Ok::<_, diesel::result::Error>(project.id)
            })
            .unwrap();
        let seeded = call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        assert_eq!(seeded.result["component_count"], json!(8));

        // The legacy repo's ROOT disappears: its two rows are spared, and stay in the catalogue.
        std::fs::remove_dir_all(base.join("legacy")).unwrap();

        let page = |offset: usize| {
            call(
                &db,
                DESIGN_INVENTORY_TOOL,
                json!({ "project_id": project_id, "limit": 3, "offset": offset }),
            )
            .expect("ok")
        };
        let first = page(0);
        assert_eq!(
            first.result["component_count"],
            json!(8),
            "8 rows are stored…"
        );
        assert_eq!(
            first.result["total"],
            json!(6),
            "…6 are in the paged population"
        );
        assert_eq!(
            first.result["orphans"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|o| o["pruned"] == json!(false))
                .count(),
            2,
            "AAA/AAB are SPARED, and reported as such"
        );
        let mut walked: Vec<String> = bundle_names(&first)
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut out = first;
        let mut turns = 0;
        while let Some(offset) = out.result["next_offset"].as_u64() {
            turns += 1;
            assert!(turns < 20, "the walk must terminate");
            out = page(offset as usize);
            assert_eq!(
                out.result["total"],
                json!(6),
                "`total` NEVER changes between pages (it used to go 6 → 8 mid-walk)"
            );
            walked.extend(bundle_names(&out).into_iter().map(str::to_string));
        }
        let unique: std::collections::BTreeSet<&String> = walked.iter().collect();
        assert_eq!(
            unique.len(),
            walked.len(),
            "no entry is returned twice: {walked:?}"
        );
        let mut sorted = walked.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec!["Z00", "Z01", "Z02", "Z03", "Z04", "Z05"],
            "the whole discovered population, and nothing else: {walked:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// DONE-CRITERION (review #42): the THEME TOKENS are a first-class layer the agent can INGEST
    /// from the app, READ and EDIT — and they are REFERENCED (the layer names the app stylesheet it
    /// came from, and lives once per project) rather than copied into each component.
    #[test]
    fn design_tokens_ingests_from_the_app_then_reads_and_edits_the_layer() {
        let _g = db_guard();
        let root = scratch_dir("tokens");
        write_file(&root, "src/Button.tsx", "<button class=\"bg-primary\"/>");
        write_file(
            &root,
            "src/styles.css",
            ":root{--primary:oklch(0.55 0.21 264);--background:oklch(1 0 0);\
             --foreground:oklch(0.14 0 0);--radius:0.625rem}\
             .dark{--primary:oklch(0.72 0.16 264)}\
             @theme inline{--color-primary:var(--primary)}",
        );
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "sandbox", &root);

        // INGEST — derived from the app, and it MOVED something, so `/design` must re-pull.
        let ingested = call(
            &db,
            DESIGN_TOKENS_TOOL,
            json!({ "project_id": project_id, "ingest": true }),
        )
        .expect("ok");
        assert_eq!(ingested.effects, vec![ChangedTopic::Prototypes]);
        assert_eq!(ingested.result["mode"], json!("ingest"));
        assert_eq!(ingested.result["changed"], json!(true));
        assert_eq!(ingested.result["repos_scanned"], json!(1));
        // REFERENCED: the layer says exactly WHERE in the app `primary` lives.
        assert_eq!(ingested.result["source"]["path"], json!("src/styles.css"));
        assert_eq!(ingested.result["source"]["repo"], json!("web"));
        assert_eq!(
            ingested.result["variables"]["--primary"],
            json!("oklch(0.55 0.21 264)")
        );
        assert_eq!(
            ingested.result["dark"]["--primary"],
            json!("oklch(0.72 0.16 264)")
        );

        // A re-ingest of an unchanged app is a pure READ (no notification storm).
        let again = call(
            &db,
            DESIGN_TOKENS_TOOL,
            json!({ "project_id": project_id, "ingest": true }),
        )
        .expect("ok");
        assert!(
            again.effects.is_empty(),
            "an idle re-ingest changes nothing"
        );
        assert_eq!(again.result["changed"], json!(false));

        // READ — the same layer, no mutation.
        let read = call(&db, DESIGN_TOKENS_TOOL, json!({ "project_id": project_id })).expect("ok");
        assert!(read.effects.is_empty());
        assert_eq!(read.result["mode"], json!("read"));
        assert_eq!(read.result["variable_count"], json!(5));
        assert_eq!(read.result["truncated"], json!(false));
        // …and the READ carries the report of the run that produced the layer (finding
        // 01KYSK7GS9C3ZRJYW00ZA0H6GF): it used to exist only inside the `ingest` response, so
        // nothing that could not re-run an ingest could ever see it. Here the sweep refused
        // nothing — which is a STATEMENT, not the absence of one (`provenance` is not null).
        assert_eq!(read.result["provenance"]["skipped_count"], json!(0));
        assert_eq!(read.result["provenance"]["skipped"], json!([]));

        // EDIT — one identity, edited once (the bare `primary` spelling is accepted).
        let edited = call(
            &db,
            DESIGN_TOKENS_TOOL,
            json!({
                "project_id": project_id,
                "set": { "primary": "#ff0055" },
                "remove": ["--radius"],
            }),
        )
        .expect("ok");
        assert_eq!(edited.effects, vec![ChangedTopic::Prototypes]);
        assert_eq!(edited.result["variables"]["--primary"], json!("#ff0055"));
        assert!(edited.result["variables"].get("--radius").is_none());
        assert_eq!(
            edited.result["source"]["path"],
            json!("src/styles.css"),
            "an edit never severs the reference back to the app stylesheet"
        );
        // The layer is stored ONCE for the project — no per-component copy.
        let stored = db
            .with_conn(|c| db::get_design(c, &project_id))
            .unwrap()
            .expect("design row");
        assert!(stored
            .theme_tokens_json
            .as_deref()
            .unwrap_or_default()
            .contains("#ff0055"));
        for component in db.with_conn(|c| list_components(c, &project_id)).unwrap() {
            assert!(
                !component.template.contains("#ff0055"),
                "a component never carries a COPY of a token"
            );
        }

        // Guard-rails: an ingest cannot be smuggled together with an edit, and a bad `set` shape is
        // refused rather than silently dropped.
        let clash = call(
            &db,
            DESIGN_TOKENS_TOOL,
            json!({ "project_id": project_id, "ingest": true, "set": { "--primary": "#000" } }),
        )
        .expect_err("refused");
        assert_eq!(clash.code, RpcCode::InvalidArgument);
        let bad = call(
            &db,
            DESIGN_TOKENS_TOOL,
            json!({ "project_id": project_id, "set": ["--primary"] }),
        )
        .expect_err("refused");
        assert_eq!(bad.code, RpcCode::InvalidArgument);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (finding `01KYQ0R9414Q5K63TBV9CCMC9J`): a composition NODE's keys are
    /// CONFRONTED with the params AND axes the component it references declares, and the two
    /// defects are reported DISTINCTLY — a key the component declares nowhere, and a `required`
    /// param the node does not provide. Each message names the COMPONENT, the NODE, the KEY and
    /// the CORRECTION, and NO mapping is ever guessed between two neighbouring names.
    #[test]
    fn a_nodes_keys_are_confronted_with_what_the_component_declares() {
        // THE REPORTED SHAPE: a Button declaring `buttonLabel` (required) + a `variant` AXIS, and a
        // node carrying `text` instead. It rendered EMPTY, and nothing said so.
        let params = r#"[{"name":"buttonLabel","type":"text","required":true}]"#;
        let states = r#"{"axes":[{"name":"variant","values":[{"name":"ghost","classes":"x"}]}]}"#;
        let mut provided = serde_json::Map::new();
        provided.insert("text".into(), json!("Reset filters"));
        let warnings = node_contract_warnings(
            "Button",
            "Root/Filters/Reset",
            &provided,
            Some(params),
            Some(states),
        );
        assert_eq!(warnings.len(), 2, "TWO distinct defects: {warnings:?}");
        // (1) the UNKNOWN key — named with the component, the node, and what IS declared.
        assert!(
            warnings[0].contains("Root/Filters/Reset")
                && warnings[0].contains("'Button'")
                && warnings[0].contains("'text'")
                && warnings[0].contains("declares NOWHERE")
                && warnings[0].contains("'buttonLabel'")
                && warnings[0].contains("'variant'")
                && warnings[0].contains("RENAME"),
            "the unknown key names component/node/key + the correction: {}",
            warnings[0]
        );
        assert!(
            !warnings[0].contains("text` -> ") && !warnings[0].contains("did you mean"),
            "no mapping is ever GUESSED between neighbouring names: {}",
            warnings[0]
        );
        // (2) the REQUIRED param NOT provided — a DIFFERENT defect with the opposite correction.
        assert!(
            warnings[1].contains("Root/Filters/Reset")
                && warnings[1].contains("REQUIRED")
                && warnings[1].contains("'buttonLabel'")
                && warnings[1].contains("renders EMPTY")
                && warnings[1].contains("\"buttonLabel\": \"…\""),
            "the missing required names the fix to APPLY: {}",
            warnings[1]
        );

        // An AXIS is a legitimate node key (the model that just landed), and so is `children`.
        let mut honoured = serde_json::Map::new();
        honoured.insert("buttonLabel".into(), json!("Reset filters"));
        honoured.insert("variant".into(), json!("ghost"));
        honoured.insert("children".into(), json!("<i/>"));
        assert!(
            node_contract_warnings("Button", "Root", &honoured, Some(params), Some(states))
                .is_empty(),
            "a node honouring the contract is silent"
        );

        // A required param provided as the EMPTY STRING renders exactly like an omitted one.
        let mut blank = serde_json::Map::new();
        blank.insert("buttonLabel".into(), json!(""));
        let blank_warnings =
            node_contract_warnings("Button", "Root", &blank, Some(params), Some(states));
        assert_eq!(blank_warnings.len(), 1);
        assert!(blank_warnings[0].contains("REQUIRED"));

        // A component that declares NOTHING has no contract to confront: a `[NEW]` authored from its
        // name alone must not flag every key of every node that uses it.
        assert!(
            node_contract_warnings("Fresh", "Root", &provided, None, None).is_empty(),
            "no declaration, no verdict"
        );
    }

    /// DONE-CRITERION (finding `01KYQ0R9414Q5K63TBV9CCMC9J`): the whole-composition report is
    /// BOUNDED — a big composition where every node is wrong names the first
    /// [`MAX_NODE_CONTRACT_WARNINGS`] and COUNTS the rest, instead of answering with one warning per
    /// node. A node referencing a component absent from the catalogue is not a mismatch (it is a
    /// `[NEW]`), and a malformed composition reports nothing rather than failing.
    #[test]
    fn the_composition_report_is_bounded_on_a_big_composition() {
        let catalogue = vec![ComponentContract {
            name: "Button".into(),
            params_json: Some(r#"[{"name":"buttonLabel","type":"text","required":true}]"#.into()),
            states_json: None,
        }];
        let children: Vec<Value> = (0..200)
            .map(|i| json!({ "component": "Button", "key": format!("b{i}"), "params": { "text": "x" } }))
            .collect();
        let composition = json!({ "key": "Root", "children": children }).to_string();
        let warnings = composition_contract_warnings(&composition, &catalogue);
        // The cap bounds NODES, and each of these nodes has two defects: at most two messages per
        // named node, plus the counting line.
        assert!(
            warnings.len() <= 2 * MAX_NODE_CONTRACT_WARNINGS + 1,
            "200 broken nodes answer with a BOUNDED list, got {}",
            warnings.len()
        );
        assert_eq!(
            warnings
                .iter()
                .filter(|w| w.contains("carries key(s) component"))
                .count(),
            MAX_NODE_CONTRACT_WARNINGS,
            "exactly {MAX_NODE_CONTRACT_WARNINGS} NODES are named — the cap counts nodes"
        );
        let last = warnings.last().expect("a report");
        assert!(
            last.contains("more composition node(s) mismatch"),
            "…and what it stopped naming is COUNTED: {last}"
        );

        // A node whose component is NOT catalogued is a `[NEW]`, not a mismatch; a malformed
        // composition is silent (a diagnostic never blocks a write).
        let unknown = json!({ "component": "NotInCatalogue", "params": { "whatever": 1 } });
        assert!(composition_contract_warnings(&unknown.to_string(), &catalogue).is_empty());
        assert!(composition_contract_warnings("not json", &catalogue).is_empty());
    }

    /// DONE-CRITERION (finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`): a template that REFERENCES another
    /// catalogue component is checked at the WRITE, where the agent can still fix it for free. The
    /// two defects the RENDER refuses — an unknown target and a cycle — are named here first, and so
    /// are the three that only degrade the rendering.
    #[test]
    fn the_reference_contract_names_the_unknown_target_the_cycle_and_the_missing_required() {
        use crate::render::RenderComponent;
        let catalogue = vec![RenderComponent {
            name: "Spinner".into(),
            template: "<i class=\"animate-spin\">{{label}}</i>".into(),
            states_json: Some(
                r#"{"axes":[{"name":"size","values":[{"name":"sm","classes":"h-3"}]}]}"#.into(),
            ),
            params_json: Some(r#"[{"name":"label","type":"text","required":true}]"#.into()),
            css_slot: None,
        }];
        let sentence = |warnings: &[String], needle: &str| -> String {
            warnings
                .iter()
                .find(|w| w.contains(needle))
                .unwrap_or_else(|| panic!("expected a warning about {needle:?}: {warnings:?}"))
                .clone()
        };

        // A template referencing NOTHING is not judged at all.
        assert!(reference_warnings("Button", "<button>{{label}}</button>", &catalogue).is_empty());

        // (1) an unresolved reference — the render refuses it, so the write says so.
        let unknown = reference_warnings(
            "Button",
            "<button><span data-nyx-use=\"Ghost\"></span></button>",
            &catalogue,
        );
        let w = sentence(&unknown, "catalogue does NOT have");
        assert!(
            w.contains("component 'Button'")
                && w.contains("'Ghost'")
                && w.contains("proto_author_component"),
            "the component, the missing target and the corrections are named: {w}"
        );

        // (2) the CYCLE — named as a chain, ONCE.
        let mut cyclic = catalogue.clone();
        cyclic.push(RenderComponent::new(
            "Spinner2",
            "<i><b data-nyx-use=\"Button\"></b></i>",
        ));
        let cycle = reference_warnings(
            "Button",
            "<button><span data-nyx-use=\"Spinner2\"></span></button>",
            &cyclic,
        );
        let w = sentence(&cycle, "CYCLE");
        assert!(
            w.contains("Button → Spinner2 → Button"),
            "the chain is named so the author knows which link to break: {w}"
        );
        assert_eq!(
            cycle.iter().filter(|w| w.contains("CYCLE")).count(),
            1,
            "ONE cycle sentence however many sites lead into it: {cycle:?}"
        );

        // (3)+(4)+(5) — a required param nobody provides, a key the target declares nowhere, and a
        // fixed axis value that names nothing.
        let sloppy = reference_warnings(
            "Button",
            "<button><span data-nyx-use=\"Spinner\" data-nyx-param-text=\"x\" \
             data-nyx-use-axes=\"size=xl\"></span></button>",
            &catalogue,
        );
        let w = sentence(&sloppy, "REQUIRED");
        assert!(
            w.contains("'label'") && w.contains("data-nyx-param-label"),
            "the correction is the exact attribute to write: {w}"
        );
        let w = sentence(&sloppy, "declares NOWHERE");
        assert!(w.contains("'text'"), "the dropped key is named: {w}");
        let w = sentence(&sloppy, "DEFAULT combination");
        assert!(
            w.contains("size=xl"),
            "the dangling axis value is named: {w}"
        );

        // …and a reference that satisfies the contract says nothing at all.
        assert!(
            reference_warnings(
                "Button",
                "<button><span data-nyx-use=\"Spinner\" data-nyx-param-label=\"{{label}}\" \
                 data-nyx-use-axes=\"size=sm\"></span></button>",
                &catalogue,
            )
            .is_empty(),
            "a well-formed reference is not reproached"
        );
    }

    /// DONE-CRITERION (finding `01KYT0Z3RYGF3A881SDQN0X9D7`, MAJOR): THE AGENT CAN SEE WHAT IT
    /// SYNTHESIZED.
    ///
    /// Every other diagnostic here judges the STRUCTURE, so on this very feature every visual defect
    /// — a spinner that never hid, a loader, the colours — was found by the human and by no tool.
    /// This walks the loop the manual now teaches (synthesize → render → LOOK → correct → render
    /// again) over the defect that was actually reported: a spinner present with NO condition. The
    /// preview SHOWS it at `loading=off`, and shows it gone once the condition is written.
    #[test]
    fn design_preview_resolves_the_component_the_way_the_frame_displays_it() {
        let _g = db_guard();
        let root = scratch_dir("preview");
        write_file(&root, "src/Button.tsx", "<button/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");

        let states = json!({ "axes": [
            { "name": "variant", "values": [
                { "name": "primary", "classes": "bg-primary text-primary-foreground" },
                { "name": "ghost", "classes": "bg-transparent" } ] },
            { "name": "loading", "values": [
                { "name": "off" },
                { "name": "on", "classes": "pointer-events-none opacity-70" } ] } ] })
        .to_string();
        let params = json!([
            { "name": "label", "type": "text", "required": true, "preview": "Enregistrer" },
            { "name": "hint", "type": "text" }
        ])
        .to_string();
        let store = |template: &str| {
            call(
                &db,
                DESIGN_REFRESH_COMPONENT_TOOL,
                json!({
                    "project_id": project_id,
                    "name": "Button",
                    "template": template,
                    "states_json": states,
                    "params_json": params,
                }),
            )
            .expect("stored")
        };
        let preview = |args: Value| {
            call(&db, DESIGN_PREVIEW_TOOL, args)
                .expect("previewed")
                .result
        };

        // THE DEFECT AS REPORTED: the spinner is in the template but nothing gates it.
        store(
            "<button class=\"inline-flex rounded-md\">\
             <i class=\"mr-2 animate-spin\">◌</i>{{label}}<em>{{hint}}</em></button>",
        );
        let out = preview(json!({ "project_id": project_id, "component": "Button" }));
        let html = out["html"].as_str().expect("html");
        assert!(
            html.contains("animate-spin") && !html.contains("hidden"),
            "the spinner is STILL THERE at loading=off, and the agent can now see it: {html}"
        );
        assert!(
            html.contains("class=\"inline-flex rounded-md bg-primary text-primary-foreground\""),
            "the ACTIVE value's classes are applied to the root, as the shim applies them: {html}"
        );
        assert!(
            html.contains("Enregistrer") && !html.contains("{{"),
            "the params are SUBSTITUTED — a preview value where the caller gave none: {html}"
        );
        assert!(
            !html.contains("data-nyx-"),
            "nothing of the vocabulary survives: it is resolved, not described: {html}"
        );
        assert_eq!(
            out["axes"],
            json!([{ "axis": "variant", "value": "primary" },
                   { "axis": "loading", "value": "off" }]),
            "the combination it was resolved AT comes back, in DECLARATION order"
        );
        let sources: Vec<&str> = out["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["source"].as_str().unwrap())
            .collect();
        assert_eq!(sources, vec!["preview", "none"], "preview > default > rien");
        assert!(
            out["warnings"].as_array().unwrap().iter().any(|w| w
                .as_str()
                .unwrap()
                .contains("resolved to NOTHING and leave a VISIBLE HOLE")
                && w.as_str().unwrap().contains("hint")),
            "a param resolving to NOTHING is named — that is what an empty box is made of: {:?}",
            out["warnings"]
        );
        assert!(
            out["cannot_show"]
                .as_str()
                .expect("every answer says where its sight stops")
                .contains("NOT a visual validation"),
            "the tool never lets the agent believe it validated the RENDERING"
        );

        // THE CORRECTION, and the SAME call proves it: gate the spinner on `loading=on`.
        store(
            "<button class=\"inline-flex rounded-md\">\
             <i data-nyx-if=\"loading=on\" class=\"mr-2 animate-spin\">◌</i>{{label}}</button>",
        );
        let off = preview(json!({ "project_id": project_id, "component": "Button" }));
        assert!(
            off["html"]
                .as_str()
                .unwrap()
                .contains("<i class=\"mr-2 animate-spin\" hidden>"),
            "at loading=off the spinner is HIDDEN — byte for byte what the frame does: {}",
            off["html"]
        );
        let on = preview(json!({
            "project_id": project_id,
            "component": "Button",
            "axes": { "loading": "on", "variant": "ghost" },
            "params": { "label": "Payer" },
        }));
        let html = on["html"].as_str().unwrap();
        assert!(
            html.contains("<i class=\"mr-2 animate-spin\">") && html.contains("Payer"),
            "at loading=on it is THERE, and the caller's own param wins: {html}"
        );
        assert!(
            html.contains("bg-transparent") && html.contains("opacity-70"),
            "BOTH axes' classes are applied — the combination, never one axis at a time: {html}"
        );

        // A value the axis does not declare falls back to the default AND is named — a preview that
        // silently answered about another combination would be worse than no preview.
        let stray = preview(json!({
            "project_id": project_id,
            "component": "Button",
            "axes": { "loading": "maybe", "tone": "muted" },
        }));
        assert_eq!(stray["axes"][1]["value"], json!("off"));
        let w: Vec<String> = stray["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(
            w.iter()
                .any(|m| m.contains("does not declare the value 'maybe'"))
                && w.iter().any(|m| m.contains("declares NO axis 'tone'")),
            "{w:?}"
        );

        // THE VIEWPORT — on a COMPONENT too, since it is one tool. It renders NOTHING differently
        // (same markup at both widths, below); what it answers is which responsive classes are in
        // force, and it reads the PROJECT's own breakpoints when the token layer declares them.
        store("<button class=\"inline-flex rounded-md px-2 md:px-4\">{{label}}</button>");
        let narrow = preview(json!({
            "project_id": project_id, "component": "Button", "viewport": "mobile",
        }));
        assert_eq!(narrow["viewport"]["width"], json!(390));
        assert_eq!(
            narrow["viewport"]["breakpoint_source"],
            json!("tailwind_defaults")
        );
        assert_eq!(
            narrow["viewport"]["variants"],
            json!([{ "prefix": "md", "occurrences": 1, "active": false }]),
            "at 390 the `md:` class is NOT in force: {}",
            narrow["viewport"]
        );
        call(
            &db,
            DESIGN_TOKENS_TOOL,
            json!({ "project_id": project_id, "set": { "--breakpoint-md": "20rem" } }),
        )
        .expect("tokens");
        let themed = preview(json!({
            "project_id": project_id, "component": "Button", "viewport": 390,
        }));
        assert_eq!(
            themed["viewport"]["breakpoint_source"],
            json!("project_tokens")
        );
        assert_eq!(
            themed["viewport"]["variants"][0]["active"],
            json!(true),
            "the PROJECT's own breakpoint decides (its `md` is 320px), never Tailwind's default: {}",
            themed["viewport"]
        );
        assert_eq!(
            narrow["html"], themed["html"],
            "…and the MARKUP is the same at every width, which is the whole honesty of this \
             parameter: nyx lays nothing out"
        );

        // THE REFUSALS are explicit, and each names its correction.
        let unknown = call(
            &db,
            DESIGN_PREVIEW_TOOL,
            json!({ "project_id": project_id, "component": "Nope" }),
        )
        .expect_err("unknown component");
        assert_eq!(unknown.code, RpcCode::InvalidId);
        assert!(unknown.message.contains("design_inventory"), "{unknown:?}");

        // A template that REFERENCES a component the catalogue does not have cannot be previewed —
        // the render's own refusal, surfaced verbatim rather than as an empty frame.
        store("<button><span data-nyx-use=\"Spinner\"></span>{{label}}</button>");
        let dangling = call(
            &db,
            DESIGN_PREVIEW_TOOL,
            json!({ "project_id": project_id, "component": "Button" }),
        )
        .expect_err("dangling reference");
        assert_eq!(dangling.code, RpcCode::InvalidArgument);
        assert!(
            dangling.message.contains("catalogue does NOT have"),
            "{dangling:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A component still awaiting its synthesis has NOTHING to resolve, and the tool says so instead
    /// of handing back an empty string an agent would read as "it renders to nothing".
    #[test]
    fn design_preview_refuses_a_component_that_carries_no_template_yet() {
        let _g = db_guard();
        let root = scratch_dir("preview-pending");
        write_file(&root, "src/Card.tsx", "<div/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        let err = call(
            &db,
            DESIGN_PREVIEW_TOOL,
            json!({ "project_id": project_id, "component": "Card" }),
        )
        .expect_err("nothing synthesized yet");
        assert_eq!(err.code, RpcCode::InvalidState);
        assert!(
            err.message.contains("design_refresh_component") && err.message.contains("NO template"),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The `{{state}}` SUBSTITUTION escape hatch — a whole axis rendered by substituting its name, on
    /// a front whose variants are plain class names — renders in the preview exactly as it renders in
    /// the frame, because a composition node's params carry the axis values too. Leaving them out
    /// would have shown that entire family of components with an empty class and sent the agent
    /// chasing a defect nyx had invented.
    #[test]
    fn design_preview_substitutes_the_active_axis_value_like_a_composition_node_does() {
        let _g = db_guard();
        let root = scratch_dir("preview-legacy");
        write_file(&root, "src/Button.tsx", "<button/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": project_id,
                "name": "Button",
                "template": "<button class=\"btn btn-{{state}}\">{{label}}</button>",
                "states_json": "[\"default\",\"ghost\"]",
            }),
        )
        .expect("stored");
        let out = call(
            &db,
            DESIGN_PREVIEW_TOOL,
            json!({ "project_id": project_id, "component": "Button", "axes": { "state": "ghost" } }),
        )
        .expect("previewed")
        .result;
        assert!(
            out["html"].as_str().unwrap().contains("btn btn-ghost"),
            "the ACTIVE value substitutes: {}",
            out["html"]
        );
        // …and the same value handed over as a PARAM is honoured as the axis it is, out loud.
        let as_param = call(
            &db,
            DESIGN_PREVIEW_TOOL,
            json!({ "project_id": project_id, "component": "Button", "params": { "state": "ghost" } }),
        )
        .expect("previewed")
        .result;
        assert!(as_param["html"].as_str().unwrap().contains("btn btn-ghost"));
        assert!(
            as_param["warnings"].as_array().unwrap().iter().any(|w| w
                .as_str()
                .unwrap()
                .contains("is an AXIS of 'Button', not a param")),
            "{:?}",
            as_param["warnings"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// THE ARRIMAGE of the SIX PARAM TYPES (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 1). The
    /// list lived at three places joined by nothing: this constant (which REFUSES a write outside
    /// it), the renderer's own (which COERCES an unknown type towards `text`), and the manual's
    /// prose (which TEACHES them). A seventh type accepted here alone would be stored and then
    /// rendered as a free-text field with its `options` dropped — the regression finding
    /// `01KYPSG0HPG2D28CMJEXRKNEVK` already paid for once, and nothing would have failed.
    ///
    /// BOTH DIRECTIONS, and in ORDER: a type added to the constant with no fixture line fails here,
    /// and a fixture line with no constant entry fails here too. The renderer's half is
    /// `design/params.test.ts`; the manual's is
    /// `mcp_context::tests::the_manual_teaches_exactly_the_six_param_types_the_write_path_accepts`.
    #[test]
    fn the_shared_fixture_pins_the_six_param_types() {
        let section = design_fixture_section("param_types");
        let pinned: Vec<&str> = section["types"]
            .as_array()
            .expect("the types")
            .iter()
            .map(|t| t.as_str().expect("a type"))
            .collect();
        assert_eq!(
            pinned,
            PARAM_TYPES.to_vec(),
            "the fixture names exactly the types the write path accepts, in the order it names them"
        );
        assert!(
            pinned.contains(&section["deprecated"].as_str().expect("the deprecated type")),
            "the deprecated type is one of the six — it is still PARSED, only never taught"
        );
        // …and the refusal really is a refusal: an unknown type never degrades into `text` here.
        let refused = ensure_params_json_accepted(Some(r#"[{"name":"align","type":"enum"}]"#))
            .expect_err("an unknown type is REFUSED at the write, not repaired");
        let message = format!("{refused:?}");
        for accepted in &pinned {
            assert!(
                message.contains(accepted),
                "the refusal names the accepted values — `{accepted}` is missing from: {message}"
            );
        }
    }

    /// THE ARRIMAGE of WHAT THE NODE-CONTRACT CAP BOUNDS (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`,
    /// family 3). Both halves of this check carried a cap of 12 and bounded two different things —
    /// MESSAGES here, NODES in the renderer — so on a composition where every node has two defects
    /// the agent was told about six nodes and the human saw twelve. The fixture pins WHICH nodes are
    /// named; `design/composition-contract.test.ts` replays the same case.
    #[test]
    fn the_shared_fixture_pins_what_the_node_contract_cap_bounds() {
        let section = design_fixture_section("node_contract");
        let max = section["max_nodes"].as_u64().expect("the cap") as usize;
        assert_eq!(
            max, MAX_NODE_CONTRACT_WARNINGS,
            "the cap is the fixture's, on both sides"
        );
        let case = &section["case"];
        let component = &case["component"];
        let catalogue = vec![ComponentContract {
            name: component["name"].as_str().expect("a name").to_string(),
            params_json: component["params_json"].as_str().map(str::to_string),
            states_json: component["states_json"].as_str().map(str::to_string),
        }];
        let node_params = &case["node_params"];
        let children: Vec<Value> = case["nodes"]
            .as_array()
            .expect("the nodes")
            .iter()
            .map(|key| {
                json!({
                    "component": component["name"],
                    "key": key,
                    "params": node_params,
                })
            })
            .collect();
        let composition = json!({ "key": case["root_key"], "children": children }).to_string();

        let warnings = composition_contract_warnings(&composition, &catalogue);
        let named: Vec<String> = case["expected_named_nodes"]
            .as_array()
            .expect("the named nodes")
            .iter()
            .map(|k| k.as_str().expect("a node key").to_string())
            .collect();
        for node_key in &named {
            assert!(
                warnings
                    .iter()
                    .any(|w| w.contains(&format!("node '{node_key}' carries key(s)")))
                    && warnings.iter().any(
                        |w| w.contains(&format!("node '{node_key}' does not provide REQUIRED"))
                    ),
                "a NAMED node is named WHOLE — both defects of `{node_key}`: {warnings:?}"
            );
        }
        for node_key in case["nodes"]
            .as_array()
            .expect("the nodes")
            .iter()
            .map(|k| {
                format!(
                    "{}/{}",
                    case["root_key"].as_str().unwrap(),
                    k.as_str().unwrap()
                )
            })
            .filter(|k| !named.contains(k))
        {
            assert!(
                !warnings
                    .iter()
                    .any(|w| w.contains(&format!("node '{node_key}'"))),
                "a node past the cap is COUNTED, never named: {node_key}"
            );
        }
        let over = case["expected_total"].as_u64().expect("the total") as usize - named.len();
        let last = warnings.last().expect("the counting line");
        assert!(
            last.contains(&format!("…and {over} more composition node(s)")),
            "the report counts what it stopped naming: {last}"
        );
        assert_eq!(
            case["expected_truncated"],
            Value::Bool(true),
            "this case exists to exercise the cap"
        );
    }

    /// THE ARRIMAGE of finding `01KYVMTWVF3TH1XPEKV9MEBZTB`: WHEN "this param resolves empty" is
    /// worth a warning. The cases live in the shared fixture because the renderer carries the same
    /// judgement (`paramsWithoutPreviewValue`), and because the criterion is the kind that rots — a
    /// rule keyed on the NAME `class` would pass every test written today and go on shouting about
    /// the first passthrough somebody calls something else.
    #[test]
    fn the_shared_fixture_pins_when_an_empty_param_is_worth_a_warning() {
        let cases = design_fixture_section("empty_param_cases")["cases"]
            .as_array()
            .expect("the cases")
            .clone();
        assert!(cases.len() >= 5, "the fixture must carry the cases");
        for case in &cases {
            let id = case["id"].as_str().unwrap_or_default();
            let template = case["template"].as_str().expect("a template");
            let spec = DeclaredParam {
                name: case["param"]["name"]
                    .as_str()
                    .expect("a param name")
                    .to_string(),
                ty: None,
                required: case["param"]["required"].as_bool() == Some(true),
                preview: None,
                default: None,
            };
            assert_eq!(
                empty_leaves_a_visible_hole(&spec, template),
                case["expect_warned"].as_bool().expect("a verdict"),
                "[{id}] {}",
                case["why"].as_str().unwrap_or_default()
            );
        }
    }

    /// …and the same rule THROUGH `design_preview`, on a real stored component: the passthrough is
    /// silent, the label still bites, and `renders_empty` keeps telling the truth about BOTH.
    #[test]
    fn a_passthrough_param_no_longer_warns_but_a_missing_label_still_does() {
        let _g = db_guard();
        let root = scratch_dir("empty-param-triage");
        write_file(&root, "src/Button.tsx", "<button/>");
        let db = Db::in_memory();
        let project_id = seeded_project(&db, "Nyx", &root);
        call(
            &db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("ok");
        call(
            &db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": project_id,
                "name": "Button",
                // `class` is a PASSTHROUGH (an attribute site, empty by nature); `label` is CONTENT
                // (empty leaves a coloured pill with no text).
                "template": "<button class=\"btn {{class}}\">{{label}}</button>",
                "params_json": "[{\"name\":\"class\",\"type\":\"text\"},{\"name\":\"label\",\"type\":\"text\"}]",
            }),
        )
        .expect("stored");

        let answer = call(
            &db,
            DESIGN_PREVIEW_TOOL,
            json!({ "project_id": project_id, "component": "Button" }),
        )
        .expect("previewed")
        .result;
        let warnings: Vec<&str> = answer["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap_or_default())
            .collect();
        let empty_warning = warnings
            .iter()
            .find(|w| w.contains("resolved to NOTHING"))
            .unwrap_or_else(|| panic!("the label's hole is still named: {warnings:?}"));
        assert!(
            empty_warning.contains("label"),
            "the REAL case still bites: {empty_warning}"
        );
        assert!(
            !empty_warning.contains("class,") && !empty_warning.contains(" class"),
            "the passthrough is not what the alarm is about any more: {empty_warning}"
        );
        // The FACT is untouched — only the alarm was triaged. Both params still report their
        // emptiness in the machine-readable list.
        let empties: Vec<&str> = answer["params"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["renders_empty"] == json!(true))
            .map(|p| p["name"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            empties,
            vec!["class", "label"],
            "`renders_empty` stays the untriaged truth"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_shared_fixture_pins_both_implementations_of_the_variance_rule() {
        let cases = variance_fixture_cases();
        assert!(
            cases.len() >= 9,
            "the fixture must actually carry the cases, got {}",
            cases.len()
        );
        let names = |values: &Value| -> Vec<String> {
            values
                .as_array()
                .expect("an expectation is a list")
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect()
        };
        for (id, case) in &cases {
            let template = case["template"].as_str().expect("a template");
            let states = case["states_json"].as_str().expect("a states_json");
            let expect = &case["expect"];
            let axes = parse_axes(Some(states));

            assert_eq!(
                axes.iter().map(|a| a.name.clone()).collect::<Vec<_>>(),
                names(&expect["axes"]),
                "[{id}] the AXES, in order — the first one is the one data-nyx-state-case drives"
            );
            assert_eq!(
                primary_axis_states(&axes),
                names(&expect["primary_axis_values"]),
                "[{id}] the PRIMARY axis's values"
            );
            assert_eq!(
                template_state_cases(template),
                names(&expect["state_cases"]),
                "[{id}] the data-nyx-state-case values the template renders"
            );
            let substituted: Vec<String> = axes
                .iter()
                .filter(|a| template_substitutes(template, &a.name))
                .map(|a| a.name.clone())
                .collect();
            assert_eq!(
                substituted,
                names(&expect["substituted_axes"]),
                "[{id}] the axes the template SUBSTITUTES ({{{{name}}}})"
            );
            let conditions = template_conditions(template);
            let mut unknown: Vec<String> = Vec::new();
            for c in &conditions {
                if !axes.iter().any(|a| a.name == c.axis) && !unknown.contains(&c.axis) {
                    unknown.push(c.axis.clone());
                }
            }
            assert_eq!(
                unknown,
                names(&expect["unknown_condition_axes"]),
                "[{id}] the conditions naming an axis nobody declares"
            );
            let inert: Vec<Value> = inert_axis_values(&axes, template)
                .into_iter()
                .map(|v| json!({ "axis": v.axis, "value": v.value }))
                .collect();
            assert_eq!(
                Value::Array(inert),
                expect["inert"],
                "[{id}] the INERT axis values — the discrimination rule"
            );
        }
    }

    /// DONE-CRITERION (finding `01KYSNPWWG9DA0AXP6D22GV5BK`): THE MAP FORM IS READ IN THE ORDER THE
    /// DOCUMENT DECLARES IT, whatever an alphabet says.
    ///
    /// `serde_json::Map` is a `BTreeMap` here, so `map.iter()` used to hand `{ "variant": …,
    /// "loading": … }` back as `[loading, variant]` while the renderer's `Object.entries` handed it
    /// back as `[variant, loading]`. The FIRST axis is the one `data-nyx-state-case` drives on both
    /// sides: on one stored row, this validator judged one axis while the panel piloted the other.
    #[test]
    fn the_map_form_is_read_in_declaration_order_not_alphabetically() {
        // Declaration order is the REVERSE of the alphabetical one, in both directions, so neither
        // reading can pass by luck.
        for (blob, expected) in [
            (
                r#"{"variant":["primary","ghost"],"loading":["off","on"]}"#,
                vec!["variant", "loading"],
            ),
            (
                r#"{"loading":["off","on"],"variant":["primary","ghost"]}"#,
                vec!["loading", "variant"],
            ),
            (
                r#"{"zeta":["a"],"alpha":["b"],"mu":["c"]}"#,
                vec!["zeta", "alpha", "mu"],
            ),
        ] {
            let axes = parse_axes(Some(blob));
            assert_eq!(
                axes.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
                expected,
                "the map form declares its axes in THIS order: {blob}"
            );
        }
        // A key repeated in the blob resolves as a JS object literal resolves it — LAST value, FIRST
        // position — so even that pathology reads the same on both sides.
        let axes = parse_axes(Some(
            r#"{"variant":["a"],"size":["md"],"variant":["b","c"]}"#,
        ));
        assert_eq!(
            axes.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["variant", "size"]
        );
        assert_eq!(
            axes[0]
                .values
                .iter()
                .map(|v| v.name.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c"]
        );

        // The OTHER shapes keep their behaviour byte for byte: they were never sorted.
        assert_eq!(
            parse_axes(Some(
                r#"{"axes":[{"name":"variant","values":["a"]},{"name":"loading","values":["off"]}]}"#
            ))
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
            vec!["variant", "loading"]
        );
        assert_eq!(
            parse_axes(Some(r#"["zeta","alpha"]"#))
                .iter()
                .flat_map(|a| a.values.iter().map(|v| v.name.as_str()))
                .collect::<Vec<_>>(),
            vec!["zeta", "alpha"],
            "the legacy flat list is untouched"
        );
    }

    /// …and the WRITE canonicalizes that form, so no new row ever depends on how an object is
    /// iterated ([`canonical_states_json`]). Every other shape is stored VERBATIM.
    #[test]
    fn the_write_canonicalizes_the_map_form_and_leaves_every_other_shape_alone() {
        let canonical = canonical_states_json(Some(
            r#"{"variant":["primary",{"name":"ghost","classes":"x"}]}"#,
        ))
        .expect("a blob in, a blob out");
        assert_eq!(
            canonical,
            r#"{"axes":[{"name":"variant","values":["primary",{"classes":"x","name":"ghost"}]}]}"#,
            "the map form becomes the canonical ARRAY, values (classes included) verbatim"
        );
        // …and the canonicalized blob declares exactly what the map form declared.
        assert_eq!(
            parse_axes(Some(&canonical))
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            vec!["variant"]
        );

        // The order survives the rewrite — that is the whole point.
        let two = canonical_states_json(Some(
            r#"{"variant":["primary","ghost"],"loading":["off","on"]}"#,
        ))
        .expect("a blob in, a blob out");
        assert_eq!(
            parse_axes(Some(&two))
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            vec!["variant", "loading"]
        );

        // VERBATIM for everything else, including what nyx cannot read.
        for untouched in [
            r#"{"axes":[{"name":"variant","values":["a"]}]}"#,
            r#"["default","ghost"]"#,
            r#"[{"name":"type","values":["image"]}]"#,
            r#"{"variant":"not-an-array"}"#,
            "not json",
            "",
        ] {
            assert_eq!(
                canonical_states_json(Some(untouched)).as_deref(),
                Some(untouched),
                "{untouched} is not the map form and must be stored as sent"
            );
        }
        assert_eq!(canonical_states_json(None), None);
    }

    /// DONE-CRITERION (same finding, the smaller half): `{{state}` — ONE closing brace — substitutes
    /// NOTHING, here as in the renderer's regex. This used to credit the axis as alive on the write
    /// path while the panel reported it inert.
    #[test]
    fn a_substitution_needs_its_closing_pair_exactly_as_the_renderer_requires_it() {
        for alive in [
            "<b class=\"btn-{{state}}\"></b>",
            "<b class=\"btn-{{{state}}}\"></b>",
            "<b class=\"btn-{{ state }}\"></b>",
        ] {
            assert!(template_substitutes(alive, "state"), "{alive}");
        }
        for dead in [
            "<b class=\"btn-{{state}\"></b>",
            "<b class=\"btn-{state}}\"></b>",
            "<b class=\"btn-{{state\"></b>",
            "<b class=\"btn-{{states}}\"></b>",
        ] {
            assert!(!template_substitutes(dead, "state"), "{dead}");
        }
    }

    // -----------------------------------------------------------------------
    // THE SCOPED CSS SLOT at the MCP surface (finding `01KYVMTWB71R0D0CT1SV88J8DX`)
    // -----------------------------------------------------------------------

    /// A project whose front carries ONE component and ONE token, inventoried and synthesized.
    /// Returns `(project_id, root)`.
    fn slot_project(db: &Db, tag: &str) -> (String, std::path::PathBuf) {
        let root = scratch_dir(tag);
        write_file(
            &root,
            "src/Spinner.tsx",
            "export const Spinner = () => <i/>;",
        );
        write_file(
            &root,
            "src/styles.css",
            ":root { --primary: oklch(0.7 0.1 250); --muted: #888888; --gap: 0.5rem; \
             --radius: 0.5rem }",
        );
        let project_id = seeded_project(db, "Nyx", &root);
        call(
            db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": project_id }),
        )
        .expect("inventory");
        db.with_conn(|c| crate::design_tokens::ingest_project_tokens(c, &project_id))
            .expect("tokens");
        (project_id, root)
    }

    fn refresh(db: &Db, project_id: &str, args: Value) -> Result<ToolOutcome, RpcError> {
        let mut base = json!({
            "project_id": project_id,
            "name": "Spinner",
            "template": "<i class=\"spark\"><b></b></i>",
        });
        let object = base.as_object_mut().unwrap();
        for (k, v) in args.as_object().unwrap() {
            object.insert(k.clone(), v.clone());
        }
        call(db, DESIGN_REFRESH_COMPONENT_TOOL, base)
    }

    /// THE HAPPY PATH — a slot is accepted, SCOPED by the machinery, stored with its reason, and
    /// echoed back with everything a review card and a coder need.
    #[test]
    fn a_css_slot_is_accepted_scoped_and_echoed_with_its_reason() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project_id, _root) = slot_project(&db, "slot-ok");

        let out = refresh(
            &db,
            &project_id,
            json!({
                "css_slot": "@keyframes pulse { 50% { opacity: .55 } }\n\
                             .spark b { animation: pulse 1.4s ease-in-out infinite; \
                             background: var(--primary) }",
                "css_slot_reason": "a custom pulse keyframe; no utility expresses a keyframe",
            }),
        )
        .expect("the slot is admitted");

        let slot = &out.result["css_slot"];
        let scope_id = crate::css_slot::component_scope_id("Spinner");
        assert_eq!(slot["scope_id"], scope_id);
        assert_eq!(slot["scope"], crate::css_slot::scope_selector(&scope_id));
        assert_eq!(
            slot["reason"],
            "a custom pulse keyframe; no utility expresses a keyframe"
        );
        assert_eq!(slot["features"], json!(["keyframes"]));
        let css = slot["css"].as_str().expect("the compiled css");
        assert!(
            css.contains(&format!(
                "{} .spark b",
                crate::css_slot::scope_selector(&scope_id)
            )),
            "the rule is ANCHORED, whatever the agent wrote: {css}"
        );
        assert!(
            css.contains(&format!("@keyframes {scope_id}-pulse")),
            "the keyframes identifier is localised: {css}"
        );

        // The STORED row carries the AUTHORED css + the reason — never the compiled form.
        let stored = db
            .with_conn(|c| db::get_component_by_name(c, &project_id, "Spinner"))
            .unwrap()
            .unwrap();
        let parsed = crate::css_slot::CssSlot::parse(stored.css_slot_json.as_deref())
            .expect("the row carries the slot");
        assert!(parsed.css.contains("@keyframes pulse"));
        assert!(!parsed.css.contains(&scope_id));
    }

    /// EVERY defect is a REFUSAL at the write, with the machine code in `error.data` — not a
    /// `warnings[]` entry. A warning is how the loader's animation disappeared.
    #[test]
    fn every_slot_defect_is_refused_at_the_write_with_its_code() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project_id, _root) = slot_project(&db, "slot-refused");
        let reason = "a custom pulse keyframe";

        let cases: Vec<(&str, &str)> = vec![
            // TOKENISED
            (
                ".spark b::after { content: \"\"; color: #ff0000 }",
                "hardcoded_colour",
            ),
            (
                ".spark b::after { content: \"\"; color: red }",
                "hardcoded_colour",
            ),
            (
                ".spark b::after { content: \"\"; padding: 12px }",
                "hardcoded_length",
            ),
            (
                ".spark b::after { content: \"\"; color: var(--nope) }",
                "unknown_token",
            ),
            // SCOPED
            (
                "body { color: var(--primary) } .x::after { content: \"\" }",
                "global_selector",
            ),
            (
                "+ .sibling { color: var(--primary) } .x::after { content: \"\" }",
                "escaping_selector",
            ),
            ("@import \"x.css\"; .x::after { content: \"\" }", "at_rule"),
            ("@layer a { .x::after { content: \"\" } }", "at_rule"),
            (
                ".x::after { content: \"\"; background-image: url(\"/a.png\") }",
                "external_resource",
            ),
            // NOT AN ESCAPE HATCH
            (
                ".spark { display: flex; gap: var(--primary) }",
                "no_escape_hatch",
            ),
            // OUTRANKING
            (
                ".x::after { content: \"\"; opacity: 1 !important }",
                "important",
            ),
        ];
        for (css, code) in cases {
            let err = refresh(
                &db,
                &project_id,
                json!({ "css_slot": css, "css_slot_reason": reason }),
            )
            .expect_err("refused");
            assert_eq!(err.code, RpcCode::InvalidArgument, "for {css}");
            assert_eq!(
                err.data
                    .as_ref()
                    .and_then(|d| d["css_slot_refusal"].as_str()),
                Some(code),
                "for {css} — the machine code travels in error.data: {}",
                err.message
            );
        }

        // BOUNDED + SIGNALLED.
        let big = format!(
            ".x::after {{ content: \"\"; color: var(--primary) }} /* {} */",
            "x".repeat(crate::css_slot::MAX_CSS_SLOT_BYTES)
        );
        let err = refresh(
            &db,
            &project_id,
            json!({ "css_slot": big, "css_slot_reason": reason }),
        )
        .expect_err("refused");
        assert_eq!(
            err.data
                .as_ref()
                .and_then(|d| d["css_slot_refusal"].as_str()),
            Some("too_large")
        );
        let err = refresh(
            &db,
            &project_id,
            json!({ "css_slot": ".x::after { content: \"\" }" }),
        )
        .expect_err("a slot with no reason is refused");
        assert_eq!(
            err.data
                .as_ref()
                .and_then(|d| d["css_slot_refusal"].as_str()),
            Some("no_reason")
        );

        // NOTHING was stored by any of the refusals.
        let stored = db
            .with_conn(|c| db::get_component_by_name(c, &project_id, "Spinner"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.css_slot_json, None);
    }

    /// A re-synthesis KEEPS the exception (an omitted argument is not a decision about it), and
    /// `css_slot: ""` removes it — deliberately.
    #[test]
    fn re_synthesizing_keeps_the_exception_and_only_an_explicit_empty_removes_it() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project_id, _root) = slot_project(&db, "slot-keep");
        let css = ".spark b::after { content: \"\"; background: var(--muted) }";

        refresh(
            &db,
            &project_id,
            json!({ "css_slot": css, "css_slot_reason": "generated content has no utility" }),
        )
        .expect("stored");

        // A refresh that says NOTHING about the slot leaves it alone — and SAYS so in its answer.
        let out = refresh(&db, &project_id, json!({})).expect("refreshed");
        assert_eq!(
            out.result["css_slot"]["reason"], "generated content has no utility",
            "the answer names the exception the component still carries"
        );

        // `""` removes it.
        let out = refresh(&db, &project_id, json!({ "css_slot": "" })).expect("refreshed");
        assert!(out.result["css_slot"].is_null());
        let stored = db
            .with_conn(|c| db::get_component_by_name(c, &project_id, "Spinner"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.css_slot_json, None);
    }

    /// The reserved MARK: a template may not write `data-nyx-css` itself — that is how one component
    /// would borrow another's stylesheet.
    #[test]
    fn a_template_may_not_write_the_scope_mark_itself() {
        let err = ensure_template_accepted("<i data-nyx-css=\"c-other-abc123\"></i>")
            .expect_err("reserved");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(err.message.contains("css_slot"), "{}", err.message);
    }

    /// design_preview REPORTS the slot it applied — the agent's own eyes on its exception.
    #[test]
    fn design_preview_reports_the_slot_it_applied() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project_id, _root) = slot_project(&db, "slot-preview");
        refresh(
            &db,
            &project_id,
            json!({
                "css_slot": "@keyframes pulse { 50% { opacity: .5 } } \
                             .spark b { animation: pulse 1s linear infinite }",
                "css_slot_reason": "a custom pulse keyframe",
            }),
        )
        .expect("stored");

        let out = call(
            &db,
            DESIGN_PREVIEW_TOOL,
            json!({ "project_id": project_id, "component": "Spinner" }),
        )
        .expect("preview");
        assert_eq!(out.result["css_slot_count"], 1);
        let slot = &out.result["css_slots"][0];
        assert_eq!(slot["component"], "Spinner");
        assert_eq!(slot["reason"], "a custom pulse keyframe");
        assert!(slot["refused"].is_null());
        let html = out.result["html"].as_str().unwrap();
        let scope_id = crate::css_slot::component_scope_id("Spinner");
        assert!(
            html.contains(&format!("data-nyx-css=\"{scope_id}\"")),
            "the ROOT carries the scope mark: {html}"
        );
        assert!(
            html.contains("<style data-nyx-css-slot="),
            "and the stylesheet travels with the markup: {html}"
        );
    }
}
