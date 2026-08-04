//! GATE e2e (v2.7 — Prototype : machinerie, Phase 5, task `01KY4DFR6MTNSH9RMY1W24BPMS`) — the
//! whole machinery built in phases 1-4, proven BOUT EN BOUT over the REAL nyx-core surface (no
//! mocks): a proto composed of NAMED components → rounds/élection → distill (freeze), the ROLLBACK
//! discipline, the TARGETED (bundle-scoped) re-synthesis, and the FIDELITY of the Handlebars render.
//!
//! It exercises the EXACT seams the sidecar wires, chained together:
//!   * Phase 1 (pivoted by review #40) — [`nyx_core::design`]: the deterministic BUNDLE DISCOVERY
//!     of the on-disk front ([`design::inventory_project`], the runtime trigger) and the
//!     INCREMENTAL synthesis loop (the `pending` work list + the hash record the volet drives
//!     through `design_refresh_component`, i.e. [`design::mark_synthesized_at`] with the agent's
//!     `input_hash` — NOT the hash-whatever-is-there `design::mark_synthesized`): a bundle is
//!     re-proposed for synthesis ONLY when its own source files moved.
//!   * Phase 2/4 — [`nyx_core::db`]: the prototype authority (proto / page / version / variants /
//!     rounds), the two-tier election, [`db::rebase_round`] (rollback), [`db::lock_design`] (the
//!     distill freeze) and [`db::elected_composition`] (the coder SEAM: arbre nommé = placement).
//!   * Phase 2/4 — [`nyx_core::mcp_tools_proto::dispatch_proto_tool`]: the REAL agent tool contract
//!     (`proto_present_round` / `proto_elect_variant` / `proto_feedback_*` / `proto_rebase` /
//!     `proto_distill` / `proto_composition`) — the SAME routing the MCP server dispatches.
//!   * Phase 1 (review #45) — [`nyx_core::mcp_tools_design::dispatch_design_tool`]: the DESIGN half
//!     of that same agent contract (`design_inventory` — list, per-bundle read, `pending_only` —
//!     and `design_refresh_component`), so the synthesis loop is proven THROUGH the MCP façade and
//!     not only against the library beneath it.
//!   * Phase 3 — [`nyx_core::render::render_composition`]: the LOGIC-LESS Handlebars render of the
//!     frozen composition into COHERENT markup (the fidelity check — structural, NOT pixel).
//!
//! The done-criterion volets, each proven below:
//!   1. `frozen_election_is_the_named_tree_and_consumable_placement` — proto (named components) →
//!      élection → distill produces the arbre de composants nommés + le placement consommable
//!      (`db::elected_composition` via the `proto_composition` MCP tool), the 3 natures
//!      `[DS]`/`[NEW]`/`[inline]` classified.
//!   2. `frozen_round_is_immutable_but_rebase_rolls_back` — the frozen election is IMMUTABLE
//!      (electing on the locked round is refused, the composition never drifts) yet the ROLLBACK
//!      (`proto_rebase`) re-opens a duplicated round where iteration resumes.
//!   3. `bundle_sync_re_synthesizes_only_the_bundles_whose_sources_changed` — the incremental loop,
//!      driven END TO END through the design MCP tools, proposes a re-synthesis ONLY for the
//!      bundles whose OWN sources changed; a logic-only `.ts` edit (not a bundle member) proposes
//!      NOTHING; and a source that moves DURING a synthesis is not swallowed (the `input_hash`
//!      guard leaves the bundle pending). It then walks a component's whole LIFE CYCLE on disk —
//!      ADDED (the catalogue grows and broadcasts), MOVED (the same row re-anchored by CONTENT, no
//!      duplicate, nothing re-proposed), DELETED (reported as an orphan and PRUNED, never
//!      re-pointed at a homonym) — and the EXPLICIT removal the agent drives
//!      (`design_delete_component`) for what the prune deliberately spares. It closes on the
//!      CSS/TOKENS half of a synthesis (review #42): the project's theme tokens INGESTED from the
//!      app's own stylesheet (`design_tokens`) — one project-wide layer that REFERENCES the source
//!      file instead of copying colours into components — plus a CVA component stored with its
//!      classes RESOLVED to a concrete string and its variant AXES declared as states, rendered
//!      through to markup that still carries those design-system classes.
//!   4. `render_of_the_frozen_composition_is_structurally_coherent` — the render produces markup
//!      COHERENT with the composition + design-system (named components REPLACED by their DS
//!      templates, placement/nesting respected, the 3 natures reflected) — fidelity, not pixels.
//!   5. `a_shared_sub_component_is_referenced_once_and_corrected_once` — a component template
//!      REFERENCES another catalogue component (`data-nyx-use`) instead of INLINING a copy of it
//!      (finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`): two hosts share ONE spinner, correcting it reaches
//!      both WITHOUT re-synthesizing either, its axes are the ones the reference site FIXED (never
//!      the host's), the expansion leaves NO `data-nyx-*` of its own for the shim to misread, and an
//!      unknown target / a CYCLE are refused by name at the render and warned about at the write.
//!   6. `the_agent_renders_what_it_synthesized_looks_at_it_and_corrects_it_alone` — the SYNTHESIZER
//!      GETS EYES (finding `01KYT0Z3RYGF3A881SDQN0X9D7`): every other diagnostic here judges the
//!      STRUCTURE, so a component that renders wrong is stored with `warnings: []`. `design_preview`
//!      resolves it into the markup the frame really DISPLAYS — the shim's applier run ahead of
//!      time, one step FURTHER than the feed `render_composition` produces — and the whole loop
//!      (synthesize → render → LOOK → correct → render again) runs with no human in it, over the
//!      defect actually reported: a spinner nothing gates. The answer always carries what it CANNOT
//!      show, so a preview is never mistaken for a visual validation.
//!   7. `the_agent_renders_the_whole_composition_it_is_about_to_present` — THE DELIVERABLE (finding
//!      `01KYT45C6RC7K84V1CA3561SPF`): volet 6 gave the agent eyes on ONE component, and what is
//!      shown to the human is a PAGE. The SAME tool renders a whole proto COMPOSITION from its
//!      `variant_id` — every node resolved, the catalogue derived from the variant's own Spec — and
//!      it ALWAYS COMPLETES: the `[NEW]` node the strict render refuses comes back as a placeholder
//!      that NAMES itself, beside the nodes that are right. Plus the three answers the finding
//!      demanded: the VIEWPORT reports which responsive classes are in force (nyx lays nothing out),
//!      the caps stay HONEST (the total is the run's, never the length of the head kept), and the
//!      answer still says it is not an image and not a visual validation.
//!
//! It runs ENTIRELY in-process against an on-disk temp `nyx.db` (integration tests build the lib
//! WITHOUT `cfg(test)`, so `Db::in_memory` is out of reach — same on-disk temp-DB pattern as the
//! sibling gates). It NEVER starts the sidecar MCP server, so the singleton port 19022 is never
//! touched, and it leaks no process. Deterministic: every assertion keys on ids/names captured
//! from the returns, never on a timestamp or a run-ordered uuid.

use std::path::{Path, PathBuf};

use nyx_core::db::{self, Db, VariantInput};
use nyx_core::design;
use nyx_core::mcp::{RpcCode, RpcError};
use nyx_core::mcp_tools_core::{ChangedTopic, ToolOutcome};
use nyx_core::mcp_tools_design::{
    dispatch_design_tool, DESIGN_DELETE_COMPONENT_TOOL, DESIGN_INVENTORY_TOOL, DESIGN_PREVIEW_TOOL,
    DESIGN_REFRESH_COMPONENT_TOOL, DESIGN_TOKENS_TOOL,
};
use nyx_core::mcp_tools_proto::{
    dispatch_proto_tool, PROTO_ADD_PAGE_TOOL, PROTO_ADD_VARIANT_TOOL, PROTO_ADD_VERSION_TOOL,
    PROTO_AUTHOR_COMPONENT_TOOL, PROTO_COMPOSITION_TOOL, PROTO_DISTILL_TOOL,
    PROTO_ELECT_VARIANT_TOOL, PROTO_FEEDBACK_CREATE_TOOL, PROTO_FEEDBACK_RESOLVE_TOOL,
    PROTO_PRESENT_ROUND_TOOL, PROTO_REBASE_TOOL,
};
use nyx_core::render::{render_composition, RenderComponent, RenderError};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Harness: an on-disk temp dir (RAII), a real Db, a real on-disk front tree.
// ---------------------------------------------------------------------------

/// A RAII guard over a test's on-disk temp dir: removed on drop, so the gate leaks no
/// `nyx-gate-2-7-*` dir under `$TMPDIR` (same pattern as `gate_v2_5_8_agent_harness_e2e.rs`).
/// The caller keeps it alive with a `_tmp`-style binding until the end of the test.
struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A unique temp dir for a test's on-disk `nyx.db` + front tree.
fn temp_dir(tag: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "nyx-gate-2-7-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// Write `content` to `root`-relative, `/`-separated `rel`, creating parent dirs.
fn write_file(root: &Path, rel: &str, content: &str) {
    let mut abs = root.to_path_buf();
    for seg in rel.split('/') {
        abs.push(seg);
    }
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(abs, content).unwrap();
}

/// Drive a PROTO tool through the REAL MCP routing ([`dispatch_proto_tool`]) — the same entry
/// point the sidecar dispatches. Panics on a mis-route or a tool error.
fn proto_tool(db: &Db, name: &str, args: Value) -> ToolOutcome {
    dispatch_proto_tool(db, name, &args)
        .unwrap_or_else(|| panic!("proto tool `{name}` was not routed"))
        .unwrap_or_else(|e| panic!("proto tool `{name}` failed: [{:?}] {}", e.code, e.message))
}

/// Drive a PROTO tool expecting a REFUSAL — returns the [`RpcError`] (panics if it was routed
/// away or unexpectedly succeeded).
fn proto_tool_err(db: &Db, name: &str, args: Value) -> RpcError {
    dispatch_proto_tool(db, name, &args)
        .unwrap_or_else(|| panic!("proto tool `{name}` was not routed"))
        .expect_err("expected the tool to be refused")
}

/// Drive a DESIGN tool through the REAL MCP routing ([`dispatch_design_tool`]) — the design twin of
/// [`proto_tool`], and the same entry point the sidecar dispatches. Panics on a mis-route or error.
fn design_tool(db: &Db, name: &str, args: Value) -> ToolOutcome {
    dispatch_design_tool(db, name, &args)
        .unwrap_or_else(|| panic!("design tool `{name}` was not routed"))
        .unwrap_or_else(|e| panic!("design tool `{name}` failed: [{:?}] {}", e.code, e.message))
}

// ---------------------------------------------------------------------------
// The composition under test — an ARBRE de composants NOMMÉS + PLACEMENT.
// ---------------------------------------------------------------------------
//
// A `Card` [DS] wrapping, in order: two `Button` [DS] (named, catalogue) children, a `PricingTable`
// [NEW] (referenced but NOT in the design-system catalogue — a candidate to build), and an [inline]
// one-off note. The tree SHAPE is the placement; the natures are intrinsic to it.

/// The elected variant's composition (variant A) — the rich named tree.
fn elected_composition_json() -> String {
    json!({
        "component": "Card",
        "key": "Root",
        "params": { "title": "Facturation" },
        "children": [
            { "component": "Button", "key": "Save", "params": { "label": "Enregistrer", "state": "idle" } },
            { "component": "Button", "key": "Cancel", "params": { "label": "Annuler", "state": "idle" } },
            { "component": "PricingTable", "params": { "plan": "pro" } },
            { "inline": "<p class=\"note\">{{note}}</p>", "params": { "note": "brouillon" } }
        ]
    })
    .to_string()
}

/// The alternative variant's composition (variant B) — a plainer tree, so the election is a REAL
/// arbitration between two variants (not a mono-variant "d'office").
fn alternative_composition_json() -> String {
    json!({
        "component": "Card",
        "key": "Root",
        "params": { "title": "Facturation (variante B)" },
        "children": [
            { "component": "Button", "key": "Save", "params": { "label": "OK", "state": "idle" } }
        ]
    })
    .to_string()
}

/// The Handlebars templates of the design-system [DS] components (`design_components.template`),
/// logic-less. `Card` is a SLOT component (`{{{children}}}`); `Button` a named leaf.
const CARD_TEMPLATE: &str = r#"<section class="ds-card"><header>{{title}}</header><div class="ds-card__body">{{{children}}}</div></section>"#;
const BUTTON_TEMPLATE: &str = r#"<button class="ds-btn" data-state="{{state}}">{{label}}</button>"#;
/// The proto's SKETCH template for the proposed [NEW] `PricingTable` — present so the whole tree
/// renders in the proto, though the component is NOT yet in the official DS catalogue.
const PRICING_TABLE_TEMPLATE: &str = r#"<table class="pricing" data-plan="{{plan}}"></table>"#;

/// The whole machinery driven up TO the distill (freeze): a live project with an on-disk React
/// front, its design-system catalogue (glob-inventoried + templated), a prototype whose current
/// round pins a page carrying TWO variants, a pre-dev comment raised + resolved, the user's RETENU
/// elected, the round PRESENTED and DISTILLED (locked). Every mutating step goes through the REAL
/// `proto_*` MCP tool. Returns the [`Fixture`] the volets assert against.
struct Fixture {
    _tmp: TempDir,
    db: Db,
    repo_root: PathBuf,
    project_id: String,
    spec_id: String,
    prototype_id: String,
    /// The DISTILLED (locked) round.
    round_id: String,
    page_id: String,
    /// Variant A — the elected (RETENU) rich named tree.
    variant_a: String,
    /// Variant B — the alternative, un-elected (used to prove the frozen round refuses re-election
    /// and the rebased round accepts it).
    variant_b: String,
    /// The `design_components` ids of the two [DS] components (name → id).
    card_id: String,
    button_id: String,
    /// The `design` row id — the DS reference the coder SEAM exposes.
    design_id: String,
}

fn distilled_fixture() -> Fixture {
    let tmp = temp_dir("proto");
    let repo_root = tmp.path().join("front");

    // --- The on-disk React front (the bundle discovery + the incremental loop feed on this). ---
    // Button = a MARKUP file + a sibling STYLE file (BOTH bundle members) + a sibling LOGIC `.ts`
    // (NOT a member — its edits must never trigger a re-synthesis). Card = a lone MARKUP file.
    write_file(
        &repo_root,
        "src/components/Button.tsx",
        "export const Button = () => <button/>;\n",
    );
    write_file(
        &repo_root,
        "src/components/Button.module.css",
        ".ds-btn { padding: 8px; }\n",
    );
    write_file(
        &repo_root,
        "src/components/Button.ts",
        "export const onClick = () => 1;\n",
    );
    write_file(
        &repo_root,
        "src/components/Card.tsx",
        "export const Card = () => <section/>;\n",
    );

    let db = Db::open(&tmp.path().join("nyx.db")).expect("open temp nyx.db");

    #[allow(clippy::type_complexity)]
    let (
        project_id,
        spec_id,
        prototype_id,
        round_id,
        page_id,
        variant_a,
        variant_b,
        card_id,
        button_id,
        design_id,
    ) = db.with_conn(|c| {
        // The project is rooted at the temp dir and declares its front as a REPO (`front/`), so the
        // runtime trigger resolves exactly `repo_root` through the root workspace (PDR-0013).
        let (project, _root) =
            db::create_project(c, "Cockpit", tmp.path().to_str().unwrap(), None).expect("project");
        db::create_repository(c, &project.id, "front", "front", false).expect("front repo");
        let (spec, _rev) =
            db::create_spec(c, &project.id, "Facturation", db::SpecSections::empty())
                .expect("spec");

        // A design-system row (so the coder SEAM's `design_ref.design_id` is a real reference).
        let design =
            db::upsert_design(c, &project.id, "# DESIGN\n\nBrand tokens…", None).expect("design");

        // Phase 1 (review #40) — the deterministic BUNDLE DISCOVERY of the on-disk front: groups
        // each component's files (Button = its `.tsx` + its `.module.css`; the sibling `.ts` logic
        // is NOT a member) and CATALOGUES them, without synthesizing any template.
        let inventoried = design::inventory_project(c, &project.id).expect("inventory");
        let inv_names: Vec<&str> = inventoried
            .bundles
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(
            inv_names,
            vec!["Button", "Card"],
            "the discovery finds exactly the two on-disk component BUNDLES"
        );
        assert_eq!(
            inventoried
                .bundles
                .iter()
                .find(|b| b.name == "Button")
                .map(|b| b.files.len()),
            Some(2),
            "the Button BUNDLE is its markup + its stylesheet (the `.ts` logic is not a member)"
        );
        assert_eq!(
            inventoried.pending.len(),
            2,
            "nothing is synthesized by the discovery — both bundles await an agent"
        );

        // Fill each catalogued component's Handlebars template — what the SYNTHESIS agent stores
        // (`design_refresh_component`); here written straight through the `db` authority so the
        // gate stays on the persistence seam. `update_component_content` is the very write that
        // tool performs, so the WHOLE provenance the discovery recorded (repo id AND path) survives
        // untouched and the incremental loop keeps resolving the right on-disk bundle.
        let mut synthesize = |name: &str, template: &str| {
            let row = db::get_component_by_name(c, &project.id, name)
                .expect("read the catalogued component")
                .unwrap_or_else(|| panic!("{name} was catalogued by the discovery"));
            assert!(
                row.source_repository_id.is_some(),
                "{name}: the discovery recorded its provenance repo"
            );
            db::update_component_content(c, &row.id, template, None, None)
                .expect("store the synthesis")
                .unwrap_or_else(|| panic!("{name} still exists"))
        };
        let card = synthesize("Card", CARD_TEMPLATE);
        let button = synthesize("Button", BUTTON_TEMPLATE);
        // …and the Button carries the ONE thing Tailwind utilities cannot express: a custom
        // `@keyframes` for its loading state (finding 01KYVMTWB71R0D0CT1SV88J8DX — the loader whose
        // animation used to be SILENTLY LOST). Stored AS AUTHORED, with the reason it exists; the
        // scoping is re-applied by the code at every render and at every crossing of the seam.
        db::set_component_css_slot(
            c,
            &button.id,
            &db::CssSlotWrite::Set(
                nyx_core::css_slot::CssSlot {
                    css: "@keyframes pulse { 50% { opacity: .55 } }\n\
                          .ds-spin { animation: pulse 1.4s ease-in-out infinite }"
                        .into(),
                    reason: "a custom pulse keyframe — no Tailwind utility expresses a keyframe"
                        .into(),
                }
                .to_json(),
            ),
        )
        .expect("store the exception")
        .expect("the Button still exists");
        // PricingTable is DELIBERATELY not upserted — it stays [NEW] (referenced, not catalogued).

        // Phase 2 — the prototype (create_prototype seeds the current round v1).
        let proto = db::create_prototype(c, &spec.id, None).expect("prototype");
        let round = db::current_round(c, &proto.id)
            .expect("round")
            .expect("seeded round");
        let page = db::create_page(c, &proto.id, "billing", "Billing", 0).expect("page");
        let version = db::create_page_version(c, &page.id, "v1", None).expect("version");
        let va = db::create_variant(
            c,
            &version.id,
            &VariantInput {
                label: "a".into(),
                title: "Named tree".into(),
                composition_json: elected_composition_json(),
                position: 0,
                ..Default::default()
            },
        )
        .expect("variant a");
        let vb = db::create_variant(
            c,
            &version.id,
            &VariantInput {
                label: "b".into(),
                title: "Alternative".into(),
                composition_json: alternative_composition_json(),
                position: 1,
                ..Default::default()
            },
        )
        .expect("variant b");
        // Pin the page's version into the current round's manifest (setup, not the action under test).
        db::set_round_page(c, &round.id, &page.id, &version.id, 0).expect("pin manifest");

        (
            project.id, spec.id, proto.id, round.id, page.id, va.id, vb.id, card.id, button.id,
            design.id,
        )
    });

    // --- The pre-dev REVIEW (raised + resolved BEFORE election) — via the REAL MCP tools. ---
    let created = proto_tool(
        &db,
        PROTO_FEEDBACK_CREATE_TOOL,
        json!({ "prototype_id": prototype_id, "body": "Le bouton devrait être plus large", "node_key": "Root/Save" }),
    );
    let feedback_id = created.result["feedback"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    proto_tool(
        &db,
        PROTO_FEEDBACK_RESOLVE_TOOL,
        json!({ "feedback_id": feedback_id, "resolution_note": "Élargi" }),
    );

    // --- PRESENT the round (handback), then the user ELECTS variant A (the RETENU). ---
    let presented = proto_tool(
        &db,
        PROTO_PRESENT_ROUND_TOOL,
        json!({ "round_id": round_id }),
    );
    assert_eq!(presented.effects, vec![ChangedTopic::Prototypes]);
    let elected = proto_tool(
        &db,
        PROTO_ELECT_VARIANT_TOOL,
        json!({ "round_id": round_id, "page_id": page_id, "variant_id": variant_a, "decided_by": "user:k" }),
    );
    assert_eq!(
        elected.result["round_page"]["chosen_variant_id"],
        json!(variant_a)
    );

    // --- DISTILL: freeze the elected round as THE design (the "figer l'élection" reframe). ---
    let distilled = proto_tool(
        &db,
        PROTO_DISTILL_TOOL,
        json!({ "prototype_id": prototype_id, "round_id": round_id, "decided_by": "user:k" }),
    );
    assert_eq!(distilled.result["lock"]["locked_round_id"], json!(round_id));

    Fixture {
        _tmp: tmp,
        db,
        repo_root,
        project_id,
        spec_id,
        prototype_id,
        round_id,
        page_id,
        variant_a,
        variant_b,
        card_id,
        button_id,
        design_id,
    }
}

// ---------------------------------------------------------------------------
// VOLET 1 — proto (named components) → élection → distill = ARBRE NOMMÉ + PLACEMENT.
// ---------------------------------------------------------------------------

/// The distill artefact IS the arbre de composants nommés + le placement consommable: the coder
/// SEAM (`proto_composition` → `db::elected_composition`), exposed ONLY once the design is LOCKED,
/// carries the frozen round, the elected variant's `composition_json` (the placement) and every
/// node classified by its NATURE (`[DS]` catalogue / `[NEW]` to-build / `[inline]` one-off), plus
/// the design-system as a REFERENCE and the resolved pre-dev review.
#[test]
fn frozen_election_is_the_named_tree_and_consumable_placement() {
    let f = distilled_fixture();

    let read = proto_tool(
        &f.db,
        PROTO_COMPOSITION_TOOL,
        json!({ "prototype_id": f.prototype_id }),
    );
    assert!(
        read.effects.is_empty(),
        "proto_composition is a READ (no effects)"
    );
    let comp = &read.result["composition"];

    // Proof this is a FROZEN election (read off the LOCK), for the right Spec.
    assert_eq!(comp["locked"]["locked_round_id"], json!(f.round_id));
    assert_eq!(comp["spec_id"], json!(f.spec_id));
    // The design-system is a REFERENCE (a pointer the harness resolves), not injected content.
    assert_eq!(comp["design_ref"]["project_id"], json!(f.project_id));
    assert_eq!(
        comp["design_ref"]["design_id"],
        json!(f.design_id),
        "the DS reference points at the real design row (a pointer, not injected content)"
    );

    // ONE elected page, whose composition_json IS the elected variant's named tree (the placement).
    let pages = comp["pages"].as_array().expect("pages array");
    assert_eq!(pages.len(), 1, "the frozen round's one pinned page");
    let page = &pages[0];
    assert_eq!(
        page["elected_variant_id"],
        json!(f.variant_a),
        "the RETENU won"
    );
    let placement = page["composition_json"]
        .as_str()
        .expect("composition_json string");
    assert_eq!(
        placement,
        elected_composition_json(),
        "the artefact is the elected variant's arbre nommé = le placement"
    );

    // The 3 NATURES, classified per node (the machine-readable contract).
    let nodes = page["nodes"].as_array().expect("nodes array");
    let by_key = |key: &str| -> &Value {
        nodes
            .iter()
            .find(|n| n["node_key"] == json!(key))
            .unwrap_or_else(|| panic!("node {key} present"))
    };
    // [DS] — the named catalogue components, carrying their design-system REFERENCE (component_ref).
    let root = by_key("Root");
    assert_eq!(root["nature"], json!("ds"));
    assert_eq!(root["component"], json!("Card"));
    assert_eq!(
        root["component_ref"],
        json!(f.card_id),
        "the [DS] node references the DS component id"
    );
    let save = by_key("Root/Save");
    assert_eq!(save["nature"], json!("ds"));
    assert_eq!(save["component"], json!("Button"));
    assert_eq!(save["component_ref"], json!(f.button_id));
    assert_eq!(
        save["params"]["label"],
        json!("Enregistrer"),
        "the node's params (placement) travel"
    );
    assert_eq!(by_key("Root/Cancel")["nature"], json!("ds"));
    // [NEW] — referenced but NOT in the catalogue → no ref, and surfaced as a to-build candidate.
    let pricing = by_key("Root/PricingTable");
    assert_eq!(pricing["nature"], json!("new"));
    assert_eq!(pricing["component"], json!("PricingTable"));
    assert!(
        pricing["component_ref"].is_null(),
        "a [NEW] node has no DS reference"
    );
    assert_eq!(
        page["new_components"],
        json!(["PricingTable"]),
        "the [NEW] candidates the coder must build"
    );
    // [inline] — bespoke one-off, no component.
    let inline = by_key("Root/inline");
    assert_eq!(inline["nature"], json!("inline"));
    assert!(inline["component"].is_null());

    // The pre-dev review RECORD: the resolved comment is carried as an ANCHOR (which node was
    // corrected), never as prose, and nothing is left open (review #47 — PDR-0009 made structural:
    // the seam a coder consumes has no comment body in it at all).
    assert_eq!(comp["resolved_feedback"].as_array().unwrap().len(), 1);
    assert_eq!(comp["resolved_feedback_count"], json!(1));
    assert_eq!(comp["open_feedback_count"], json!(0));
    assert!(
        comp["resolved_feedback"][0]["node_key"].is_string(),
        "the anchor (WHERE it was corrected) is what crosses the seam"
    );
    assert!(
        comp["resolved_feedback"][0].get("body").is_none()
            && comp["resolved_feedback"][0]
                .get("resolution_note")
                .is_none(),
        "the pre-dev review prose is NEVER transmitted to the coder (PDR-0009)"
    );

    // THE CSS EXCEPTION CROSSES THE SEAM, WHOLE AND WITH ITS REASON (finding
    // 01KYVMTWB71R0D0CT1SV88J8DX). Everything else here is a POINTER the harness resolves, because
    // it exists in the app; a scoped CSS slot exists NOWHERE else, so it travels entire — or the
    // coder rebuilds the component without it, which is exactly the disappearance this closes.
    let exceptions = page["css_exceptions"].as_array().expect("css_exceptions");
    assert_eq!(exceptions.len(), 1, "the one component that carries one");
    let exception = &exceptions[0];
    let scope = nyx_core::css_slot::component_scope_id("Button");
    assert_eq!(exception["component"], json!("Button"));
    assert_eq!(exception["scope_id"], json!(scope));
    assert_eq!(
        exception["scope"],
        json!(format!(r#"[data-nyx-css~="{scope}"]"#)),
        "the seam SAYS what the CSS is bounded to — the coder does not have to infer it"
    );
    assert_eq!(exception["features"], json!(["keyframes"]));
    assert_eq!(
        exception["reason"],
        json!("a custom pulse keyframe — no Tailwind utility expresses a keyframe"),
        "WITH ITS REASON: an exception the developer inherits, never one they discover"
    );
    assert_eq!(exception["reached_by"], json!("placement"));
    assert!(exception["refused"].is_null());
    let css = exception["css"].as_str().expect("the compiled css");
    assert!(
        css.contains(&format!(r#"[data-nyx-css~="{scope}"] .ds-spin"#))
            && css.contains(&format!("@keyframes {scope}-pulse")),
        "and it crosses ALREADY SCOPED — what the preview applied is what the coder implements: {css}"
    );
    // The NODE points at it; the record is not copied at every placement.
    assert_eq!(by_key("Root/Save")["css_scope"], json!(scope));
    assert!(
        by_key("Root/inline").get("css_scope").is_none(),
        "a node whose component carries no exception says nothing"
    );
}

// ---------------------------------------------------------------------------
// VOLET 2 — ROLLBACK OK: frozen round IMMUTABLE, rebase re-opens a duplicated round.
// ---------------------------------------------------------------------------

/// The frozen election is IMMUTABLE on the locked round (re-electing is REFUSED, the exposed
/// composition never drifts), yet the ROLLBACK stays possible: `proto_rebase` opens a NEW round
/// that DUPLICATES the frozen manifest, and iteration (electing another variant) resumes there.
#[test]
fn frozen_round_is_immutable_but_rebase_rolls_back() {
    let f = distilled_fixture();

    // Snapshot the frozen composition, to prove it does not drift under a refused mutation.
    let before = proto_tool(
        &f.db,
        PROTO_COMPOSITION_TOOL,
        json!({ "prototype_id": f.prototype_id }),
    );

    // Electing (variant B) on the FROZEN round is refused with invalid_state (not a silent no-op).
    let err = proto_tool_err(
        &f.db,
        PROTO_ELECT_VARIANT_TOOL,
        json!({ "round_id": f.round_id, "page_id": f.page_id, "variant_id": f.variant_b }),
    );
    assert_eq!(
        err.code,
        RpcCode::InvalidState,
        "élection figée: mutation on a locked round refused"
    );

    // The exposed composition is byte-for-byte unchanged — no drift under the harness.
    let after = proto_tool(
        &f.db,
        PROTO_COMPOSITION_TOOL,
        json!({ "prototype_id": f.prototype_id }),
    );
    assert_eq!(
        after.result, before.result,
        "the frozen composition never drifted"
    );

    // ROLLBACK: rebase onto the frozen round → a NEW current round, distinct id.
    let rebased = proto_tool(
        &f.db,
        PROTO_REBASE_TOOL,
        json!({ "prototype_id": f.prototype_id, "source_round_id": f.round_id, "label": "v2" }),
    );
    assert_eq!(rebased.effects, vec![ChangedTopic::Prototypes]);
    let new_round_id = rebased.result["round"]["id"].as_str().unwrap().to_string();
    assert_ne!(
        new_round_id, f.round_id,
        "rebase opens a fresh round, never mutating the frozen one"
    );

    // The manifest was DUPLICATED: the same page (same pinned version) is pinned in the new round.
    f.db.with_conn(|c| {
        let manifest = db::list_round_pages(c, &new_round_id)?;
        let src = db::get_round_page(c, &f.round_id, &f.page_id)?.expect("source manifest entry");
        assert_eq!(
            manifest.len(),
            1,
            "the rebased round copies the source manifest"
        );
        assert_eq!(manifest[0].page_id, f.page_id, "same page pinned");
        assert_eq!(
            manifest[0].page_version_id, src.page_version_id,
            "same version pinned (the design returned to that snapshot)"
        );
        Ok::<(), diesel::result::Error>(())
    })
    .expect("read the rebased manifest");

    // Iteration RESUMES on the non-locked rebased round: electing variant B now succeeds.
    let re_elected = proto_tool(
        &f.db,
        PROTO_ELECT_VARIANT_TOOL,
        json!({ "round_id": new_round_id, "page_id": f.page_id, "variant_id": f.variant_b, "decided_by": "user:k" }),
    );
    assert_eq!(
        re_elected.result["round_page"]["chosen_variant_id"],
        json!(f.variant_b),
        "the rollback re-opened iteration — a fresh election works again"
    );
}

// ---------------------------------------------------------------------------
// VOLET 3 — SYNTHÈSE CIBLÉE: only the bundles whose SOURCES changed are re-proposed.
// ---------------------------------------------------------------------------

/// The incremental loop (pivoted by review #40: the INPUTS are hashed, never the LLM output)
/// proposes a (re-)synthesis ONLY for the bundles that need one: every fresh bundle awaits a first
/// synthesis; once synthesized, an unchanged re-run proposes NOTHING (never a bulk regen); a
/// LOGIC-only (`.ts`) edit — not a bundle member — proposes NOTHING; a real source edit (the `.tsx`
/// markup, or the bundle's stylesheet) re-proposes EXACTLY that one component.
///
/// Driven ENTIRELY through the DESIGN MCP façade ([`dispatch_design_tool`]), like volets 1/2/4 do
/// for the proto tools — so the loop is proven on the REAL agent contract: `design_inventory`
/// (list + `pending_only` + the per-bundle READ) and `design_refresh_component` (review #45, the
/// ONLY e2e exercise of it), including the coupling the tool owns — content write + hash record in
/// ONE tool call, `ChangedTopic::Prototypes` echoed — and its `input_hash` staleness guard.
///
/// What this volet proves of that coupling is the RESULT (both writes landed, or neither is acted
/// on), not the isolation: the two statements now really do share one `conn.transaction(...)`
/// (review `01KYHQ7FAS4CY4Q842XZB6ZSPP` — `Db::with_conn` only hands out a pooled connection, so
/// under SQLite autocommit they used to be two separate commits), but atomicity under CONCURRENCY is
/// a property of that transaction, not something an end-to-end sequence can exhibit.
///
/// It also pins the LIFE CYCLE the review 01KYHPFDE21W0EMVX4J87RHGCV made conservative: a bundle
/// that MOVES is re-anchored, and a component whose source DISAPPEARS is reported and FLAGGED but
/// never destroyed while it carries a synthesis — the explicit `design_delete_component` is the only
/// thing that removes it.
#[test]
fn bundle_sync_re_synthesizes_only_the_bundles_whose_sources_changed() {
    let f = distilled_fixture();
    let root = &f.repo_root;

    // The TARGETED work list, read through the AGENT's own tool (`pending_only`) — the only path
    // that drives an inventory now that `/design` is a pure reader (finding
    // 01KYKX7HBP4ZSSS6ZDM31T6A5E removed its "Inventorier / Sync" gesture; the tool is untouched).
    let pending = |db: &Db| -> Vec<String> {
        let out = design_tool(
            db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": f.project_id, "pending_only": true, "limit": 200 }),
        );
        assert!(
            out.effects.is_empty(),
            "the fixture already catalogued this front: every run of this volet re-discovers the \
             SAME bundles at the SAME paths, so the CATALOGUE does not move and the tool reports no \
             change (a source EDIT moves the pending list, not the catalogue's identity). A scan \
             that really adds a component IS a mutation — asserted at the end of this volet."
        );
        assert!(
            out.result["orphans"]
                .as_array()
                .expect("orphans")
                .is_empty(),
            "no bundle went missing: nothing to prune, nothing to report"
        );
        let mut names: Vec<String> = out.result["bundles"]
            .as_array()
            .expect("bundles array")
            .iter()
            .map(|b| b["name"].as_str().expect("bundle name").to_string())
            .collect();
        assert_eq!(
            out.result["pending_count"].as_u64().unwrap() as usize,
            names.len(),
            "pending_count and the pending_only list agree"
        );
        names.sort();
        names
    };
    // READ a bundle the way the agent does, and hand back the input_hash it was given.
    let read_input_hash = |db: &Db, name: &str| -> String {
        let read = design_tool(
            db,
            DESIGN_INVENTORY_TOOL,
            json!({ "project_id": f.project_id, "component": name }),
        );
        assert!(read.effects.is_empty(), "a bundle read is a READ");
        read.result["bundle"]["input_hash"]
            .as_str()
            .expect("the read hands back the bundle's INPUT hash")
            .to_string()
    };
    // What an agent does after synthesizing: STORE the template through `design_refresh_component`,
    // quoting the hash it read — the write half of the loop, hash recorded in the same call.
    let synthesized = |db: &Db, name: &str, template: &str| {
        let input_hash = read_input_hash(db, name);
        let out = design_tool(
            db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": f.project_id,
                "name": name,
                "template": template,
                "input_hash": input_hash,
            }),
        );
        assert_eq!(
            out.effects,
            vec![ChangedTopic::Prototypes],
            "storing a synthesis is a catalogue mutation"
        );
        assert_eq!(out.result["synthesized"], json!(true));
        assert_eq!(
            out.result["input_hash"].as_str(),
            Some(input_hash.as_str()),
            "the bundle's INPUT hash is recorded in the SAME call that stored the template"
        );
        assert_eq!(
            out.result["component"]["template"].as_str(),
            Some(template),
            "the synthesized template really landed in the catalogue"
        );
    };

    // Fresh catalogue: neither bundle carries a synthesis hash → BOTH await the agent.
    assert_eq!(
        pending(&f.db),
        vec!["Button".to_string(), "Card".into()],
        "every fresh bundle awaits a first synthesis"
    );
    // The agent synthesizes both → nothing left to do (never a bulk regen on the next run).
    synthesized(&f.db, "Button", BUTTON_TEMPLATE);
    synthesized(&f.db, "Card", CARD_TEMPLATE);
    assert!(
        pending(&f.db).is_empty(),
        "an unchanged bundle is NEVER re-synthesized"
    );

    // A LOGIC-only edit (the sibling `.ts`, not a bundle member) proposes NOTHING.
    write_file(
        root,
        "src/components/Button.ts",
        "export const onClick = () => 42; // changed logic\n",
    );
    assert!(
        pending(&f.db).is_empty(),
        "a .ts logic edit is not a bundle input — no re-synthesis (the core incremental criterion)"
    );

    // A real SOURCE edit (the `.tsx` markup) re-proposes ONLY that bundle.
    write_file(
        root,
        "src/components/Button.tsx",
        "export const Button = () => <button className=\"ds-btn\"/>;\n",
    );
    assert_eq!(
        pending(&f.db),
        vec!["Button".to_string()],
        "only the changed bundle is re-proposed for synthesis"
    );

    // A source that moves DURING the synthesis is NOT swallowed: the agent reads the bundle, the
    // file changes under it, and its write (quoting the hash it read) stores the template but
    // leaves the bundle PENDING instead of marking the NEW source handled.
    let stale_hash = read_input_hash(&f.db, "Button");
    write_file(
        root,
        "src/components/Button.tsx",
        "export const Button = () => <button className=\"ds-btn wide\"/>;\n",
    );
    let stale = design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Button",
            "template": BUTTON_TEMPLATE,
            "input_hash": stale_hash,
        }),
    );
    assert_eq!(stale.result["synthesized"], json!(false));
    assert_eq!(stale.result["source_changed"], json!(true));
    assert_eq!(
        pending(&f.db),
        vec!["Button".to_string()],
        "a source that moved mid-synthesis stays on the work list"
    );

    // Re-reading and refreshing at the CURRENT hash closes it.
    synthesized(&f.db, "Button", BUTTON_TEMPLATE);
    assert!(pending(&f.db).is_empty());

    // Its sibling STYLE file IS a bundle member: editing it re-proposes Button too…
    write_file(
        root,
        "src/components/Button.module.css",
        ".ds-btn { padding: 12px; }\n",
    );
    assert_eq!(
        pending(&f.db),
        vec!["Button".to_string()],
        "a sibling style edit is a bundle-input change too"
    );
    // …while Card (untouched on disk) is NEVER re-proposed across all of the above.
    synthesized(&f.db, "Button", BUTTON_TEMPLATE);
    assert!(pending(&f.db).is_empty(), "the loop closes again");

    // --- The CATALOGUE tracks the app: a scan that really MOVES it says so, and only then. ---
    // A component ADDED on disk: the catalogue grows, the change is broadcast, and it is the only
    // bundle proposed.
    write_file(
        root,
        "src/components/Badge.tsx",
        "export const Badge = () => <span/>;\n",
    );
    let grown = design_tool(
        &f.db,
        DESIGN_INVENTORY_TOOL,
        json!({ "project_id": f.project_id, "limit": 200 }),
    );
    assert_eq!(
        grown.effects,
        vec![ChangedTopic::Prototypes],
        "a scan that ADDS a component IS a catalogue mutation (an open /design re-pulls)"
    );
    assert_eq!(grown.result["component_count"], json!(3));
    assert_eq!(pending(&f.db), vec!["Badge".to_string()]);
    let badge_id =
        f.db.with_conn(|c| db::get_component_by_name(c, &f.project_id, "Badge"))
            .expect("read Badge")
            .expect("catalogued")
            .id;

    // The component is MOVED on disk (after a synthesis, so there IS an identity worth keeping):
    // the SAME row is re-anchored by CONTENT, nothing is duplicated, nothing is re-proposed.
    synthesized(&f.db, "Badge", "<span class=\"ds-badge\">{{label}}</span>");
    std::fs::create_dir_all(root.join("src/components/badge")).expect("mkdir");
    std::fs::rename(
        root.join("src/components/Badge.tsx"),
        root.join("src/components/badge/Badge.tsx"),
    )
    .expect("move the component");
    let moved = design_tool(
        &f.db,
        DESIGN_INVENTORY_TOOL,
        json!({ "project_id": f.project_id, "limit": 200 }),
    );
    assert_eq!(
        moved.result["component_count"],
        json!(3),
        "no duplicate row"
    );
    assert_eq!(moved.result["moved"][0]["name"], json!("Badge"));
    assert_eq!(
        moved.result["moved"][0]["entry_path"],
        json!("src/components/badge/Badge.tsx")
    );
    assert!(
        moved.result["orphans"].as_array().unwrap().is_empty(),
        "a MOVE is never an orphan"
    );
    let after_move =
        f.db.with_conn(|c| db::get_component_by_name(c, &f.project_id, "Badge"))
            .expect("read Badge")
            .expect("still catalogued");
    assert_eq!(after_move.id, badge_id, "same row — the identity survived");
    assert_eq!(
        after_move.template,
        "<span class=\"ds-badge\">{{label}}</span>"
    );
    assert!(
        pending(&f.db).is_empty(),
        "a pure move changed no CONTENT — nothing to re-synthesize"
    );

    // The component is DELETED from the app: the row is REPORTED — never silently kept with a stale
    // synthesis, and never re-pointed at some homonym. It is NOT destroyed either: it carries an LLM
    // SYNTHESIS, and no scan is proof enough to throw reviewed work away (the walk can silently fail
    // to read a directory or a file — review 01KYHPFDE21W0EMVX4J87RHGCV). It is FLAGGED an orphan
    // and kept for an explicit decision.
    std::fs::remove_file(root.join("src/components/badge/Badge.tsx")).expect("delete");
    let orphaned = design_tool(
        &f.db,
        DESIGN_INVENTORY_TOOL,
        json!({ "project_id": f.project_id, "limit": 200 }),
    );
    assert_eq!(
        orphaned.effects,
        vec![ChangedTopic::Prototypes],
        "raising an orphan flag moves the catalogue, so it is broadcast"
    );
    assert_eq!(orphaned.result["orphans"][0]["name"], json!("Badge"));
    assert_eq!(orphaned.result["orphans"][0]["pruned"], json!(false));
    assert_eq!(
        orphaned.result["orphans"][0]["kept_because"],
        json!("synthesized")
    );
    assert_eq!(orphaned.result["orphans"][0]["flagged_orphan"], json!(true));
    assert_eq!(
        orphaned.result["pruned_count"],
        json!(0),
        "an automatic élagage NEVER destroys a synthesis"
    );
    assert_eq!(
        orphaned.result["scan_error_count"],
        json!(0),
        "this scan read everything it looked at — and STILL does not destroy the row"
    );
    assert_eq!(orphaned.result["component_count"], json!(3));
    let kept =
        f.db.with_conn(|c| db::get_component(c, &badge_id))
            .expect("read")
            .expect("the synthesized row survives the disappearance of its source");
    assert_eq!(
        kept.template, "<span class=\"ds-badge\">{{label}}</span>",
        "its synthesis is intact — it is the human's to discard, not a scan's"
    );
    assert!(kept.orphaned_at.is_some(), "…and it is FLAGGED for /design");

    // The EXPLICIT removal (review #44) is what reaches everything the élagage spares — the orphan
    // above, and Card (still on disk, so no prune would ever take it out).
    for name in ["Badge", "Card"] {
        let deleted = design_tool(
            &f.db,
            DESIGN_DELETE_COMPONENT_TOOL,
            json!({ "project_id": f.project_id, "name": name }),
        );
        assert_eq!(deleted.result["deleted"], json!(true));
        assert_eq!(deleted.effects, vec![ChangedTopic::Prototypes]);
    }
    assert_eq!(
        f.db.with_conn(|c| db::list_components(c, &f.project_id))
            .expect("catalogue")
            .len(),
        1,
        "the catalogue can finally be corrected"
    );

    // --- Review #42: the CSS/TOKENS half of a synthesis -----------------------------------
    //
    // A synthesis that stores only STRUCTURE loses the whole look. The style of a real front is
    // (a) the Tailwind CLASSES the `.variants.ts` CVA computes and (b) the TOKENS those classes
    // resolve against. nyx read NEITHER: `theme_tokens_json` was never written by anything, so
    // `bg-primary` resolved to nothing and the preview rendered GREY. This closes that chain on the
    // REAL agent contract: ingest the app's tokens, store a CVA resolved to CONCRETE classes with
    // its variant AXES declared as states, and render it.

    // The app's own token stylesheet — the authority the layer REFERENCES.
    write_file(
        root,
        "src/styles.css",
        ":root{--primary:oklch(0.55 0.21 264);--primary-foreground:oklch(0.985 0 0);\
         --background:oklch(1 0 0);--radius:0.625rem}\
         .dark{--primary:oklch(0.72 0.16 264)}\
         @theme inline{--color-primary:var(--primary)}",
    );

    let ingested = design_tool(
        &f.db,
        DESIGN_TOKENS_TOOL,
        json!({ "project_id": f.project_id, "ingest": true }),
    );
    assert_eq!(
        ingested.effects,
        vec![ChangedTopic::Prototypes],
        "a token layer that lands is a change every design surface must re-pull"
    );
    // REFERENCED, not copied: the layer names the app file `primary` lives in.
    assert_eq!(
        ingested.result["source"]["path"],
        json!("src/styles.css"),
        "the layer keeps the reference back to the app's own stylesheet"
    );
    assert_eq!(
        ingested.result["variables"]["--primary"],
        json!("oklch(0.55 0.21 264)")
    );
    assert_eq!(
        ingested.result["dark"]["--primary"],
        json!("oklch(0.72 0.16 264)"),
        "the dark override is a distinct map, not a clobber of the light identity"
    );

    // ONE layer for the WHOLE project — the identity of `primary` is never duplicated per component.
    let design_row =
        f.db.with_conn(|c| db::get_design(c, &f.project_id))
            .expect("read design")
            .expect("design row");
    assert!(design_row
        .theme_tokens_json
        .as_deref()
        .expect("the token layer is persisted on the project's single design row")
        .contains("--primary"));

    // The SYNTHESIS an agent stores for a CVA component: the `.variants.ts` axes RESOLVED into a
    // concrete class string inlined on the node (never a dangling `{{class}}` the logic-less
    // renderer would drop), and the axes themselves declared as STATES (the state is a param).
    let cva_template = "<button class=\"inline-flex h-9 items-center rounded-md bg-primary px-4 \
         text-primary-foreground\" data-state=\"{{state}}\">{{label}}</button>";
    let stored = design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Button",
            "template": cva_template,
            "params_json": json!([{ "name": "label", "type": "text" }]).to_string(),
            "states_json": json!(["default", "destructive", "outline", "disabled"]).to_string(),
        }),
    );
    assert_eq!(stored.effects, vec![ChangedTopic::Prototypes]);

    let button =
        f.db.with_conn(|c| db::get_component_by_name(c, &f.project_id, "Button"))
            .expect("read")
            .expect("Button");
    // The variant AXES are declared — that is what makes them pilotable from the composant detail
    // (`parseStates` turns `states_json` into the `state` param's options) and from the frame's
    // chip-bar.
    let axes: Vec<String> =
        serde_json::from_str(button.states_json.as_deref().expect("states_json")).expect("axes");
    assert_eq!(
        axes,
        vec!["default", "destructive", "outline", "disabled"],
        "each CVA axis value is a declared state"
    );
    // …and the classes are CONCRETE, resolved by the agent — not a CVA call nyx cannot execute.
    assert!(
        button.template.contains("bg-primary") && button.template.contains("rounded-md"),
        "the synthesis inlined the resolved class string: {}",
        button.template
    );
    assert!(
        !button.template.contains("{{class"),
        "no dangling CVA placeholder survives into the stored template"
    );

    // The render carries those concrete classes through to the markup the iframe displays — where
    // the ingested tokens above give `bg-primary` its real colour.
    let markup = render_composition(
        &[RenderComponent::new("Button", button.template.clone())],
        &[],
        &json!({ "component": "Button", "params": { "label": "Payer", "state": "destructive" } })
            .to_string(),
    )
    .expect("render the CVA component");
    assert!(
        markup.contains("bg-primary") && markup.contains("text-primary-foreground"),
        "the design-system classes reach the markup verbatim: {markup}"
    );
    assert!(
        markup.contains("data-state=\"destructive\"") && markup.contains("Payer"),
        "the state is just a param, and it drives the rendering: {markup}"
    );

    // --- Finding 01KYQ0R84KVF6QVQXQ9YVG9Z8T: the VARIANCE MODEL is NAMED AXES ----------------
    //
    // The flat list above is the DEGENERATE case — one anonymous axis — and it must keep working
    // (it just did). The model itself is AXES: `variant` × `size` × a two-valued `loading`, each
    // value carrying its own resolved classes, applied by the shim. A real front has several
    // orthogonal axes, and enumerating their product (12 complete renderings here) is what made the
    // previous model collapse. Here: ONE element in the template.
    let axes_template = "<button class=\"inline-flex items-center rounded-md font-medium\">\
         <i data-nyx-if=\"loading=on\" class=\"animate-spin\">◌</i>\
         <span data-nyx-if-not=\"loading=on\">{{label}}</span></button>";
    let axes_json = json!({ "axes": [
        { "name": "variant", "values": [
            { "name": "primary", "classes": "bg-primary px-4 text-primary-foreground" },
            { "name": "destructive", "classes": "bg-destructive px-4 text-destructive-foreground" } ] },
        { "name": "size", "values": [
            { "name": "md", "classes": "h-9 text-sm" },
            { "name": "lg", "classes": "h-11 text-base" } ] },
        { "name": "loading", "values": [
            { "name": "off" },
            { "name": "on", "classes": "pointer-events-none opacity-70" } ] } ] });
    let stored = design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Button",
            "template": axes_template,
            "params_json": json!([
                { "name": "label", "type": "text", "required": true, "preview": "Enregistrer" }
            ])
            .to_string(),
            "states_json": axes_json.to_string(),
        }),
    );
    assert_eq!(
        stored.result["warnings"],
        json!([]),
        "two axes + a structural boolean + one text param is a COMPLETE synthesis: {:?}",
        stored.result["warnings"]
    );

    let button =
        f.db.with_conn(|c| db::get_component_by_name(c, &f.project_id, "Button"))
            .expect("read")
            .expect("Button");
    let axes: serde_json::Value =
        serde_json::from_str(button.states_json.as_deref().expect("states_json")).expect("axes");
    let names: Vec<&str> = axes["axes"]
        .as_array()
        .expect("axes array")
        .iter()
        .map(|a| a["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        vec!["variant", "size", "loading"],
        "each CVA axis is its OWN named axis — the product is never enumerated"
    );
    assert_eq!(
        axes["axes"][0]["values"][1]["classes"],
        json!("bg-destructive px-4 text-destructive-foreground"),
        "…and every value carries the CONCRETE classes the shim applies for it"
    );
    assert_eq!(
        button.template.matches("<button").count(),
        1,
        "ONE element in the template for 2 x 2 x 2 renderings: {}",
        button.template
    );

    // The structural half — what classes cannot do — survives the render verbatim, so the shim can
    // show/hide the spinner on the `loading` axis.
    let markup = render_composition(
        &[RenderComponent::new("Button", button.template.clone())],
        &[],
        &json!({ "component": "Button",
                 "params": { "label": "Payer", "variant": "destructive", "size": "lg", "loading": "on" } })
        .to_string(),
    )
    .expect("render the multi-axis component");
    assert!(
        markup.contains("data-nyx-if=\"loading=on\"")
            && markup.contains("data-nyx-if-not=\"loading=on\""),
        "the generalized data-nyx-if reaches the frame: {markup}"
    );

    // …and THE WARNINGS are the guard-rail: a synthesis whose boolean is a decorative param, whose
    // variant is declared twice, or whose axis value carries nothing, is STORED with the component,
    // the faulty field and the correction NAMED.
    let broken = design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Button",
            "template": "<button class=\"rounded-md\">{{label}}</button>",
            "params_json": json!([
                { "name": "variant", "type": "select", "options": ["primary"] },
                { "name": "loading", "type": "boolean" }
            ])
            .to_string(),
            // A REALLY inert axis: NO value carries classes, nothing conditions on it, no case. A
            // bare value beside a sibling that DOES carry classes is a different thing and is
            // credited — it is the ABSENCE of those classes that defines it (finding
            // `01KYT103MXTKFBZMKF968FGW9P`).
            "states_json": json!({ "axes": [
                { "name": "variant", "values": [
                    { "name": "primary" }, { "name": "ghost" } ] } ] })
            .to_string(),
        }),
    );
    let warnings: Vec<String> = broken.result["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .map(|w| w.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("component 'Button'")
            && w.contains("axis 'variant'")
            && w.contains("primary, ghost")),
        "the INERT axis values are named: {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("param 'loading' is a `boolean`")
                && w.contains("AN AXIS WITH TWO VALUES")),
        "the INERT boolean param is named, with the correction: {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("declared BOTH in params_json and as an axis")),
        "the DOUBLE declaration is named: {warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// VOLET 4 — FIDÉLITÉ: the render is COHERENT with the named tree + DS (structural, not pixel).
// ---------------------------------------------------------------------------

/// The Handlebars (logic-less) render of the FROZEN composition produces markup COHERENT with the
/// composition + design-system: the named `[DS]` components are REPLACED by their design-system
/// templates, the placement (nesting + order) is respected, and the `[NEW]`/`[inline]` natures are
/// reflected. Fidelity is STRUCTURAL (the named tree is found in the markup), not a pixel equality.
#[test]
fn render_of_the_frozen_composition_is_structurally_coherent() {
    let f = distilled_fixture();

    // Read the frozen composition back through the coder SEAM (the SAME artefact a harness gets).
    let read = proto_tool(
        &f.db,
        PROTO_COMPOSITION_TOOL,
        json!({ "prototype_id": f.prototype_id }),
    );
    let placement = read.result["composition"]["pages"][0]["composition_json"]
        .as_str()
        .expect("frozen composition_json")
        .to_string();

    // The render CATALOGUE is DATA-DRIVEN from the DB design-system (the [DS] templates) PLUS the
    // proto's sketch template for the proposed [NEW] component (so the whole proto tree renders).
    let mut catalogue: Vec<RenderComponent> =
        f.db.with_conn(|c| db::list_components(c, &f.project_id))
            .expect("catalogue")
            .into_iter()
            .map(|d| RenderComponent {
                name: d.name,
                template: d.template,
                states_json: d.states_json,
                params_json: d.params_json,
                css_slot: nyx_core::css_slot::CssSlot::parse(d.css_slot_json.as_deref()),
            })
            .collect();
    catalogue.push(RenderComponent::new("PricingTable", PRICING_TABLE_TEMPLATE));

    let markup =
        render_composition(&catalogue, &[], &placement).expect("render the frozen composition");

    // COHERENCE — the whole named tree, deterministically, with placement respected:
    //   * [DS] Card REPLACED by its DS template (`ds-card`), its `title` param interpolated;
    //   * [DS] Button REPLACED by its DS template (`ds-btn`), BOTH children present, IN ORDER,
    //     NESTED inside the Card's slot (the placement);
    //   * [NEW] PricingTable rendered from the proto sketch; [inline] note rendered bespoke;
    //   * the Button's SCOPED CSS SLOT: its roots STAMPED with the scope, its stylesheet appended
    //     ONCE at the end however many times the component is placed, its selectors already anchored
    //     and its `@keyframes` already renamed under that scope (finding 01KYVMTWB71R0D0CT1SV88J8DX);
    //   * EVERY NODE NAMES ITS OWN ROOT (`data-nyx-node`, finding 01KYW77Y65A9HYNDZ69M67S6B8) — the
    //     named tree is legible IN the markup, which is what makes a click in the frame resolve to a
    //     `node_key` without a second render and without a CSS selector. Note the two Buttons: same
    //     component, DIFFERENT keys, so `Root/Save` and `Root/Cancel` — the placement is what the
    //     mark carries, not the component name.
    let scope = nyx_core::css_slot::component_scope_id("Button");
    let expected = format!(
        concat!(
            r#"<section data-nyx-node="Root" class="ds-card"><header>Facturation</header>"#,
            r#"<div class="ds-card__body">"#,
            r#"<button data-nyx-node="Root/Save" data-nyx-css="{scope}" class="ds-btn" data-state="idle">Enregistrer</button>"#,
            r#"<button data-nyx-node="Root/Cancel" data-nyx-css="{scope}" class="ds-btn" data-state="idle">Annuler</button>"#,
            r#"<table data-nyx-node="Root/PricingTable" class="pricing" data-plan="pro"></table>"#,
            r#"<p data-nyx-node="Root/inline" class="note">brouillon</p>"#,
            r#"</div></section>"#,
            "<style data-nyx-css-slot=\"{scope}\">@keyframes {scope}-pulse {{\n",
            "  50% {{ opacity: .55 }}\n",
            "}}\n",
            r#"[data-nyx-css~="{scope}"] .ds-spin {{ animation: {scope}-pulse 1.4s ease-in-out infinite }}"#,
            "</style>",
        ),
        scope = scope,
    );
    assert_eq!(
        markup, expected,
        "the render is coherent with the named tree + design-system"
    );
    assert_eq!(
        markup.matches("<style").count(),
        1,
        "the exception's stylesheet is emitted ONCE, however many nodes place the component"
    );

    // Explicit structural checks (readable intent, beyond the exact-equality above):
    assert!(
        markup.contains(r#"<section data-nyx-node="Root" class="ds-card">"#),
        "the [DS] Card node was REPLACED by its design-system template (named, not a placeholder)"
    );
    assert!(
        markup.contains(&format!(
            r#"<div class="ds-card__body"><button data-nyx-node="Root/Save" data-nyx-css="{scope}" class="ds-btn" data-state="idle">Enregistrer</button>"#
        )),
        "placement respected: the first Button is NESTED inside the Card slot"
    );
    // THE EXCEPTION IS BOUNDED BY THE MACHINERY, not by the agent's discipline: every rule the
    // Button's slot compiles to is anchored on the scope the Button's roots carry, and the animation
    // NAME is local to it — two components may both call theirs `pulse`.
    assert!(
        markup.contains(&format!(r#"[data-nyx-css~="{scope}"] .ds-spin"#))
            && markup.contains(&format!("@keyframes {scope}-pulse")),
        "the CSS slot crosses ANCHORED and with its keyframes localised: {markup}"
    );
    assert!(
        !markup.contains("@keyframes pulse "),
        "the GLOBAL animation name never reaches the frame: {markup}"
    );
    assert!(
        markup.contains(
            r#"<table data-nyx-node="Root/PricingTable" class="pricing" data-plan="pro"></table>"#
        ),
        "the [NEW] node renders from the proto sketch"
    );
    assert!(
        markup.contains(r#"<p data-nyx-node="Root/inline" class="note">brouillon</p>"#),
        "the [inline] one-off renders its bespoke markup"
    );
}

// ---------------------------------------------------------------------------
// VOLET 5 — RÉFÉRENCE: a template REFERENCES a shared sub-component instead of copying it.
// ---------------------------------------------------------------------------

/// The catalogue as the RENDERER reads it (the same rows `design_render` hands to
/// `render_composition`, states and params included — a reference resolves against them).
fn render_catalogue(f: &Fixture) -> Vec<RenderComponent> {
    f.db.with_conn(|c| db::list_components(c, &f.project_id))
        .expect("catalogue")
        .into_iter()
        .map(|d| RenderComponent {
            name: d.name,
            template: d.template,
            states_json: d.states_json,
            params_json: d.params_json,
            css_slot: nyx_core::css_slot::CssSlot::parse(d.css_slot_json.as_deref()),
        })
        .collect()
}

/// The warnings of a design/proto tool result, as strings.
fn warnings_of(out: &ToolOutcome) -> Vec<String> {
    out.result["warnings"]
        .as_array()
        .expect("warnings is always an array")
        .iter()
        .map(|w| w.as_str().unwrap_or_default().to_string())
        .collect()
}

/// A SHARED sub-component is REFERENCED by its hosts (`data-nyx-use`), not inlined into each of
/// them — finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`. Proven END TO END over the real MCP surface: the
/// spinner is authored ONCE (`proto_author_component`), two hosts reference it
/// (`design_refresh_component`), and correcting the spinner ONCE reaches BOTH hosts without a single
/// re-synthesis of either — which is exactly what inlining could not do (drift, guaranteed).
///
/// It also pins the three decisions the capability rests on:
///   * the sub-component's AXES are the ones the REFERENCE SITE fixed, never the host's (an axis name
///     is local to a component);
///   * an expansion carries NO `data-nyx-*` of its own — the shim, which knows exactly ONE axis
///     vocabulary, can never meet a second one, so the Electron renderer learns NOTHING;
///   * an unresolved reference and a CYCLE are REFUSALS that name the chain, at the render, and
///     WARNINGS that name the correction, at the write.
#[test]
fn a_shared_sub_component_is_referenced_once_and_corrected_once() {
    let f = distilled_fixture();

    // --- The shared sub-component, authored ONCE as a greenfield [NEW] (the real MCP tool). ---
    let spinner = proto_tool(
        &f.db,
        PROTO_AUTHOR_COMPONENT_TOOL,
        json!({
            "prototype_id": f.prototype_id,
            "name": "Spinner",
            "template": "<i class=\"animate-spin\" aria-label=\"{{label}}\">◌</i>",
            "params_json": json!([
                { "name": "label", "type": "text", "required": true, "preview": "Chargement" }
            ]).to_string(),
            "states_json": json!({ "axes": [ { "name": "size", "default": "md", "values": [
                { "name": "sm", "classes": "h-3 w-3" },
                { "name": "md", "classes": "h-4 w-4" } ] } ] }).to_string(),
        }),
    );
    assert!(
        warnings_of(&spinner).is_empty(),
        "the sub-component itself is a complete synthesis: {:?}",
        warnings_of(&spinner)
    );

    // --- TWO hosts REFERENCE it. Neither inlines a copy. ---
    // `Button` gates its spinner on its OWN `loading` axis (the host's mechanism, on the host's
    // element) and fixes the spinner's OWN `size` axis at `sm` — the two axes never touch.
    let button_template = "<button class=\"ds-btn\">\
         <span data-nyx-if=\"loading=on\" data-nyx-use=\"Spinner\" data-nyx-use-axes=\"size=sm\" \
         data-nyx-param-label=\"{{label}}\"></span>{{label}}</button>";
    let stored = design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Button",
            "template": button_template,
            "params_json": json!([
                { "name": "label", "type": "text", "required": true, "preview": "Enregistrer" }
            ]).to_string(),
            "states_json": json!({ "axes": [
                { "name": "size", "values": [
                    { "name": "md", "classes": "h-9" }, { "name": "lg", "classes": "h-11" } ] },
                { "name": "loading", "values": [
                    { "name": "off" }, { "name": "on", "classes": "opacity-70" } ] } ] })
            .to_string(),
        }),
    );
    assert!(
        warnings_of(&stored).is_empty(),
        "a well-formed reference is not reproached: {:?}",
        warnings_of(&stored)
    );
    design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Card",
            "template": "<section class=\"ds-card\"><header>{{title}}</header>\
                 <span data-nyx-use=\"Spinner\" data-nyx-param-label=\"Chargement\"></span>\
                 <div class=\"ds-card__body\">{{{children}}}</div></section>",
        }),
    );

    // --- The RENDER flattens the reference, IN THE SIDECAR. ---
    let composition = json!({
        "component": "Card",
        "params": { "title": "Facturation" },
        "children": [ { "component": "Button", "params": { "label": "Payer", "size": "lg" } } ]
    })
    .to_string();
    let markup = render_composition(&render_catalogue(&f), &[], &composition).expect("render");

    // ONE definition, TWO renderings — the sub-component really is shared.
    assert_eq!(
        markup.matches("animate-spin").count(),
        2,
        "both hosts render the SAME spinner: {markup}"
    );
    // THE AXES ARE NOT INHERITED: the Button node is at `size=lg`, and its spinner is at the `sm`
    // the REFERENCE SITE fixed — `h-3 w-3`, never the host's `h-11`.
    assert!(
        markup.contains("class=\"animate-spin h-3 w-3\""),
        "the reference site's own axis value is what renders: {markup}"
    );
    // …and the `Card`, which fixes nothing, gets the sub-component's DECLARED DEFAULT (`md`).
    assert!(
        markup.contains("class=\"animate-spin h-4 w-4\""),
        "an omitted axis renders at the sub-component's default: {markup}"
    );
    // THE HOST'S OWN CONDITION GOVERNS THE PLACEHOLDER — and it is the NODE that decides it
    // (finding `01KZ41YK39SGNKF6XV87A35GPN`). The `Button` node leaves `loading` at its default
    // `off`, so its whole placeholder is HIDDEN; the `Card`'s placeholder carries no condition at
    // all and shows. Both are the host's element, both resolved from the host's own axes.
    assert!(
        markup.contains("<span hidden><i class=\"animate-spin h-3 w-3\" aria-label=\"Payer\">"),
        "the placeholder stays the HOST's element, and the HOST's condition decided it: {markup}"
    );
    assert!(
        markup.contains("<span><i class=\"animate-spin h-4 w-4\" aria-label=\"Chargement\">"),
        "…while a placeholder that gates on nothing shows: {markup}"
    );
    // …AND THE NODE'S VALUE IS WHAT DECIDES IT: the same composition with `loading=on` shows the
    // spinner and wears that value's class. Three axis controls of the node params form were dead
    // for exactly as long as this was not true.
    let loading = render_composition(
        &render_catalogue(&f),
        &[],
        &json!({
            "component": "Card",
            "params": { "title": "Facturation" },
            "children": [ { "component": "Button",
                            "params": { "label": "Payer", "size": "lg", "loading": "on" } } ]
        })
        .to_string(),
    )
    .expect("render");
    assert!(
        loading.contains("<span><i class=\"animate-spin h-3 w-3\" aria-label=\"Payer\">")
            && loading.contains("opacity-70"),
        "the node's own `loading=on` shows what it gates and wears its classes: {loading}"
    );
    // …AND THE EXPANSION LEAKS NOTHING: no second axis vocabulary can ever reach the shim, which is
    // why the Electron renderer needs to learn nothing at all about references.
    for expansion in markup.split("<i class=\"animate-spin").skip(1) {
        let inner = expansion.split("</i>").next().unwrap_or_default();
        assert!(
            !inner.contains("data-nyx-"),
            "the flattened sub-component is INERT: {inner}"
        );
    }
    // The param handed over at the site is the one that rendered (the host's own, forwarded).
    assert!(
        markup.contains("aria-label=\"Payer\"") && markup.contains("aria-label=\"Chargement\""),
        "each site's params reach the sub-component: {markup}"
    );

    // --- THE POINT: correcting the spinner ONCE corrects it EVERYWHERE. ---
    // No host is re-synthesized here; only the shared row moves.
    design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Spinner",
            "template": "<svg class=\"motion-safe:animate-spin\" role=\"img\" \
                 aria-label=\"{{label}}\"></svg>",
            "params_json": json!([
                { "name": "label", "type": "text", "required": true, "preview": "Chargement" }
            ]).to_string(),
            "states_json": json!({ "axes": [ { "name": "size", "default": "md", "values": [
                { "name": "sm", "classes": "h-3 w-3" },
                { "name": "md", "classes": "h-4 w-4" } ] } ] }).to_string(),
        }),
    );
    let button_after =
        f.db.with_conn(|c| db::get_component_by_name(c, &f.project_id, "Button"))
            .expect("read")
            .expect("Button");
    assert_eq!(
        button_after.template, button_template,
        "the HOSTS were not touched — that is the whole difference with inlining"
    );
    let fixed = render_composition(&render_catalogue(&f), &[], &composition).expect("render");
    assert_eq!(
        fixed.matches("motion-safe:animate-spin").count(),
        2,
        "ONE edit, BOTH hosts: {fixed}"
    );
    assert!(
        !fixed.contains(">◌<"),
        "and no copy of the old spinner survives anywhere: {fixed}"
    );

    // --- THE REFUSALS, named at the write AND at the render. ---
    // (a) an unresolved reference — which is also what a DELETE of the shared row produces.
    let dangling = design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Card",
            "template": "<section><span data-nyx-use=\"Ghost\"></span>{{{children}}}</section>",
        }),
    );
    assert!(
        warnings_of(&dangling)
            .iter()
            .any(|w| w.contains("catalogue does NOT have") && w.contains("'Ghost'")),
        "the write NAMES the missing target: {:?}",
        warnings_of(&dangling)
    );
    let err = render_composition(
        &render_catalogue(&f),
        &[],
        &json!({ "component": "Card" }).to_string(),
    )
    .expect_err("an unresolved reference is refused, never a silent hole");
    assert_eq!(
        err,
        RenderError::UnknownReference {
            host: "Card".into(),
            reference: "Ghost".into()
        }
    );

    // (b) a CYCLE — the spinner referencing its own host back.
    design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Card",
            "template": "<section><span data-nyx-use=\"Spinner\"></span>{{{children}}}</section>",
        }),
    );
    let cyclic = design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Spinner",
            "template": "<i class=\"animate-spin\"><b data-nyx-use=\"Card\"></b></i>",
        }),
    );
    assert!(
        warnings_of(&cyclic)
            .iter()
            .any(|w| w.contains("CYCLE") && w.contains("Spinner → Card → Spinner")),
        "the write NAMES the cycle as a chain: {:?}",
        warnings_of(&cyclic)
    );
    let err = render_composition(
        &render_catalogue(&f),
        &[],
        &json!({ "component": "Card" }).to_string(),
    )
    .expect_err("a cycle is refused, never unrolled");
    assert_eq!(
        err,
        RenderError::ReferenceCycle {
            path: vec!["Card".into(), "Spinner".into(), "Card".into()]
        }
    );
    assert!(
        err.to_string().contains("Card → Spinner → Card"),
        "…and the render says which link to break: {err}"
    );
}

// ---------------------------------------------------------------------------
// VOLET 6 — VOIR: the agent RENDERS what it synthesized, LOOKS at it, and CORRECTS it alone.
// ---------------------------------------------------------------------------

/// Drive a DESIGN tool expecting a REFUSAL — the design twin of [`proto_tool_err`].
fn design_tool_err(db: &Db, name: &str, args: Value) -> RpcError {
    dispatch_design_tool(db, name, &args)
        .unwrap_or_else(|| panic!("design tool `{name}` was not routed"))
        .expect_err("expected the tool to be refused")
}

/// THE SYNTHESIZER GETS EYES (finding `01KYT0Z3RYGF3A881SDQN0X9D7`, MAJOR) — proven END TO END over
/// the real MCP surface, on the defect that was actually reported.
///
/// Every other diagnostic of this machinery judges the STRUCTURE of a synthesis (a missing case, an
/// inert value, an unknown key, a refused type) and NOT ONE of them ever looks at the RESULT. The
/// measurable consequence on this very feature: every visual defect — a spinner that never hid, a
/// loader, the colours — was found by the HUMAN and by no tool, so an agent shipped broken
/// components without knowing it and could not iterate on its own.
///
/// This walks the loop the manual now teaches, with no human in it:
///   SYNTHESIZE (`design_refresh_component`, which answers `warnings: []` — the write is happy)
///   → RENDER (`design_preview`) → LOOK (the spinner is STILL THERE at `loading=off`)
///   → CORRECT (gate it) → RENDER again (it is gone at `off`, present at `on`).
/// It also pins the two things that make the tool honest: the answer goes ONE STEP FURTHER than the
/// display path the frame is fed (the shim's applier is run here, so what comes back is what is
/// DISPLAYED, not what is received), and it always carries what it CANNOT show — a preview must
/// never be mistaken for a visual validation.
#[test]
fn the_agent_renders_what_it_synthesized_looks_at_it_and_corrects_it_alone() {
    let f = distilled_fixture();

    // The shared sub-component the host will REFERENCE — so the preview is exercised on the whole
    // chain (template + reference + axes + params), not on a lone element.
    proto_tool(
        &f.db,
        PROTO_AUTHOR_COMPONENT_TOOL,
        json!({
            "prototype_id": f.prototype_id,
            "name": "Spinner",
            "template": "<i class=\"animate-spin\" aria-label=\"{{label}}\">◌</i>",
            "params_json": json!([
                { "name": "label", "type": "text", "required": true, "preview": "Chargement" }
            ]).to_string(),
            "states_json": json!({ "axes": [ { "name": "size", "default": "md", "values": [
                { "name": "sm", "classes": "h-3 w-3" },
                { "name": "md", "classes": "h-4 w-4" } ] } ] }).to_string(),
        }),
    );

    let variant_axis = json!({ "name": "variant", "values": [
        { "name": "primary", "classes": "bg-primary text-primary-foreground" },
        { "name": "ghost", "classes": "bg-transparent" } ] });
    let params = json!([
        { "name": "label", "type": "text", "required": true, "preview": "Enregistrer" }
    ])
    .to_string();
    let store = |template: &str, states: Value| {
        design_tool(
            &f.db,
            DESIGN_REFRESH_COMPONENT_TOOL,
            json!({
                "project_id": f.project_id,
                "name": "Button",
                "template": template,
                "params_json": params,
                "states_json": states.to_string(),
            }),
        )
    };
    let preview = |args: Value| design_tool(&f.db, DESIGN_PREVIEW_TOOL, args).result;

    // --- SYNTHESIZE: the defect as reported — the spinner is there, and NOTHING gates it. ---
    let ungated = "<button class=\"ds-btn rounded-md\">\
         <span data-nyx-use=\"Spinner\" data-nyx-use-axes=\"size=sm\" \
         data-nyx-param-label=\"{{label}}\"></span>{{label}}</button>";
    let stored = store(ungated, json!({ "axes": [variant_axis] }));
    assert!(
        warnings_of(&stored).is_empty(),
        "THE STARTING POINT: the write path is entirely HAPPY with a component that renders wrong — \
         it judges the STRUCTURE (axes, cases, keys, types) and never the RESULT, which is why every \
         visual defect of this feature was found by the human: {:?}",
        warnings_of(&stored)
    );

    // --- RENDER + LOOK: the agent finally sees the defect the human used to catch. ---
    let off = preview(json!({ "project_id": f.project_id, "component": "Button" }));
    let html = off["html"].as_str().expect("html");
    assert!(
        html.contains("animate-spin") && !html.contains("hidden"),
        "the spinner is glued on: it is THERE on a button that is not loading, and the agent can \
         now SEE it: {html}"
    );
    // …and everything else about that rendering is visible too, in the same answer.
    assert!(
        html.contains("class=\"ds-btn rounded-md bg-primary text-primary-foreground\""),
        "the ACTIVE value's classes are applied to the root, as the shim applies them: {html}"
    );
    assert!(
        html.contains("class=\"animate-spin h-3 w-3\"")
            && html.contains("aria-label=\"Enregistrer\""),
        "the REFERENCE is expanded, at the axis the site fixed, with the host's param forwarded: {html}"
    );
    assert!(
        html.contains("Enregistrer") && !html.contains("{{"),
        "the params are SUBSTITUTED — the `preview` value, where the caller gave none: {html}"
    );
    assert_eq!(
        off["axes"],
        json!([{ "axis": "variant", "value": "primary" }]),
        "the combination it was resolved AT comes back, in DECLARATION order"
    );

    // THE ANSWER IS THE DISPLAY, NOT THE FEED. `render_composition` is what the frame receives — the
    // node's own vocabulary still in it, because the shim resolves it live. The preview runs that
    // last step too, which is the whole reason it can answer "what does it look like at THIS value".
    // The one `data-nyx-*` a resolved answer KEEPS is `data-nyx-css`, and it is not part of that
    // vocabulary at all: it is the IDENTITY of the component's SCOPED CSS SLOT — what every rule of
    // the slot is anchored on — not an instruction anyone interprets. Removing it would unanchor the
    // stylesheet that travels with the markup, which is the disappearance finding
    // 01KYVMTWB71R0D0CT1SV88J8DX exists to end. Everything else is consumed, as before.
    assert!(
        !html
            .replace("data-nyx-css-slot=", "")
            .replace("data-nyx-css=", "")
            .replace("[data-nyx-css~=", "")
            .contains("data-nyx-"),
        "the PREVIEW is RESOLVED: nothing of the INTERACTION vocabulary is left for anyone to \
         interpret: {html}"
    );
    // …and the exception the component carries is REPORTED, never left to be spotted in the markup.
    assert_eq!(off["css_slot_count"], json!(1));
    assert_eq!(off["css_slots"][0]["component"], json!("Button"));
    assert_eq!(off["css_slots"][0]["features"], json!(["keyframes"]));
    assert_eq!(
        off["css_slots"][0]["reason"],
        json!("a custom pulse keyframe — no Tailwind utility expresses a keyframe"),
        "with the REASON it exists — that is what makes it reviewable and actionable"
    );

    // NEVER A VISUAL VALIDATION — said in the payload itself, at every call.
    let cannot_show = off["cannot_show"]
        .as_str()
        .expect("every answer carries it");
    assert!(
        cannot_show.contains("NOT a visual validation")
            && cannot_show.contains("no colour")
            && cannot_show.contains("the human is still the only eye on the visual"),
        "the tool states where its own sight stops: {cannot_show}"
    );
    assert!(
        off["html_bytes"].is_number() && off["truncated"] == json!(false),
        "the markup is BOUNDED like every other agent-facing payload: {off}"
    );

    // --- CORRECT + RENDER AGAIN: the same call proves the fix, with no human in the loop. ---
    // The correction is the one the manual teaches: DECLARE the axis the element depends on, and
    // GATE the element on it. The baseline `off` stays bare — it is the complement that makes it
    // visible, and inventing classes for it would be the very thing the manual forbids.
    let gated = json!({ "axes": [
        variant_axis,
        { "name": "loading", "values": [
            { "name": "off" },
            { "name": "on", "classes": "pointer-events-none opacity-70" } ] } ] });
    store(
        "<button class=\"ds-btn rounded-md\">\
         <span data-nyx-if=\"loading=on\" data-nyx-use=\"Spinner\" data-nyx-use-axes=\"size=sm\" \
         data-nyx-param-label=\"{{label}}\"></span>{{label}}</button>",
        gated,
    );
    let fixed_off = preview(json!({ "project_id": f.project_id, "component": "Button" }));
    let html = fixed_off["html"].as_str().unwrap();
    assert!(
        html.contains("hidden") && html.contains("animate-spin"),
        "at loading=off the spinner is now HIDDEN — byte for byte what the frame does: {html}"
    );
    let on = preview(json!({
        "project_id": f.project_id,
        "component": "Button",
        "axes": { "loading": "on", "variant": "ghost" },
        "params": { "label": "Payer" },
    }));
    let html = on["html"].as_str().unwrap();
    assert!(
        !html.contains("hidden") && html.contains("animate-spin"),
        "…and at loading=on it is THERE: {html}"
    );
    assert!(
        html.contains("bg-transparent") && html.contains("opacity-70") && html.contains("Payer"),
        "the WHOLE combination is applied, and the caller's own param wins: {html}"
    );

    // --- THE REFUSALS: a preview that cannot be produced NAMES what to fix, never a blank. ---
    let unknown = design_tool_err(
        &f.db,
        DESIGN_PREVIEW_TOOL,
        json!({ "project_id": f.project_id, "component": "Ghost" }),
    );
    assert_eq!(unknown.code, RpcCode::InvalidId);
    // A bundle the scan just discovered carries NO template yet: there is nothing to resolve, and
    // saying so beats handing back an empty string an agent would read as "it renders to nothing".
    write_file(
        &f.repo_root,
        "src/components/Badge.tsx",
        "export const Badge = () => <span/>;\n",
    );
    design_tool(
        &f.db,
        DESIGN_INVENTORY_TOOL,
        json!({ "project_id": f.project_id }),
    );
    let pending = design_tool_err(
        &f.db,
        DESIGN_PREVIEW_TOOL,
        json!({ "project_id": f.project_id, "component": "Badge" }),
    );
    assert_eq!(
        pending.code,
        RpcCode::InvalidState,
        "a component still awaiting its synthesis has nothing to resolve, and says so: {pending:?}"
    );
    assert!(
        pending.message.contains("design_refresh_component"),
        "{pending:?}"
    );
}

// ---------------------------------------------------------------------------
// VOLET 7 — THE DELIVERABLE: the agent RENDERS the whole COMPOSITION it presents.
// ---------------------------------------------------------------------------

/// THE COMPOSITION IS WHAT THE HUMAN IS SHOWN (finding `01KYT45C6RC7K84V1CA3561SPF`, MAJOR) — and
/// nothing could render one for the agent that composed it.
///
/// Volet 6 gave the synthesizer eyes on ONE component. What is presented, elected and frozen is a
/// PAGE: a tree of nodes, their params, their nesting. The only render that existed for a tree fed
/// the FRAME (`render_composition`, before the shim's applier) and was STRICT — a single `[NEW]`
/// node nobody had synthesized failed the whole thing, which is exactly the state a composition is
/// in while it is being designed. So an agent presented a page it had never seen.
///
/// This walks the answer end to end, over the real MCP surface:
///   * the SAME tool (`design_preview`), a second SUBJECT named by its own argument (`variant_id`),
///     the catalogue derived from the variant's own Spec — the agent passes no `project_id`;
///   * the tree RESOLVED node by node (each at its own combination, no `data-nyx-*` left);
///   * a node the strict render REFUSES comes back as a placeholder that NAMES itself, beside the
///     nodes that are right — the render ABOUTIT;
///   * `node_key` renders ONE subtree; the caps stay HONEST (the total is the run's, never the head
///     that was kept) and point at that lever;
///   * the VIEWPORT answers what it can (which responsive classes are in force) and says what it
///     cannot (nyx lays nothing out) — and it names markup that carries no responsive class at all;
///   * the answer still carries what it CANNOT show, image included: this subject reopens nothing.
#[test]
fn the_agent_renders_the_whole_composition_it_is_about_to_present() {
    let f = distilled_fixture();
    let preview = |args: Value| design_tool(&f.db, DESIGN_PREVIEW_TOOL, args).result;

    // --- THE WHOLE PAGE, from its variant id alone. ---
    let out = preview(json!({ "variant_id": f.variant_a }));
    assert_eq!(out["subject"], json!("composition"));
    assert_eq!(
        out["variant"]["project_id"],
        json!(f.project_id),
        "the catalogue is DERIVED (variant → version → page → prototype → Spec → project): the \
         agent renders a page without knowing which project's components it is made of"
    );
    assert_eq!(out["variant"]["page"], json!("billing"));
    let html = out["html"].as_str().expect("html");
    let button_scope = nyx_core::css_slot::component_scope_id("Button");
    assert!(
        html.contains(r#"<section class="ds-card"><header>Facturation</header>"#)
            && html.contains(&format!(
                r#"<button data-nyx-css="{button_scope}" class="ds-btn" data-state="idle">Enregistrer</button><button data-nyx-css="{button_scope}" class="ds-btn" data-state="idle">Annuler</button>"#
            ))
            && html.contains(r#"<p class="note">brouillon</p>"#),
        "the whole named tree is resolved, in placement order: {html}"
    );
    // EXACTLY the component preview's allowance, and for the same reason (see the assertion in
    // `the_agent_sees_what_it_just_synthesized`): the ONE `data-nyx-*` a resolved answer keeps is
    // `data-nyx-css`, the IDENTITY of the SCOPED CSS SLOT the appended stylesheet is anchored on —
    // never an instruction anyone interprets. A NESTED slotted component used to lose it here: its
    // PARENT's resolution pass stripped the whole prefix off the children it had just composed, so
    // the `Button`'s animation reached neither this markup nor `css_slots[]` — the very
    // disappearance finding 01KYVMTWB71R0D0CT1SV88J8DX exists to end, one level down.
    assert!(
        !html
            .replace("data-nyx-css-slot=", "")
            .replace("data-nyx-css=", "")
            .replace("[data-nyx-css~=", "")
            .contains("data-nyx-"),
        "the answer is RESOLVED, exactly like a component preview: {html}"
    );
    assert!(
        html.contains(&format!(r#"<style data-nyx-css-slot="{button_scope}">"#))
            && html.contains(&format!(r#"[data-nyx-css~="{button_scope}"] .ds-spin"#)),
        "…and the NESTED component's scoped stylesheet travels with it: {html}"
    );
    assert_eq!(
        out["css_slot_count"],
        json!(1),
        "one entry per COMPONENT, whatever how many placements reach it"
    );
    assert_eq!(out["css_slots"][0]["component"], json!("Button"));

    // --- THE [NEW] NODE: named, not fatal. ---
    // The same tree, through the STRICT render the frame is fed, is an error — the whole page for
    // one node. That is the state every composition under design is in.
    let mut catalogue = render_catalogue(&f);
    catalogue.retain(|c| c.name != "PricingTable");
    assert_eq!(
        render_composition(&catalogue, &[], &elected_composition_json()).unwrap_err(),
        RenderError::UnknownComponent("PricingTable".into()),
        "the strict render refuses the WHOLE tree for one unbuilt node"
    );
    assert_eq!(out["unrendered_count"], json!(1));
    let missing = &out["unrendered"][0];
    assert_eq!(missing["node_key"], json!("Root/PricingTable"));
    assert_eq!(missing["component"], json!("PricingTable"));
    assert_eq!(missing["reason"], json!("not_in_catalogue"));
    assert!(
        missing["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("proto_author_component"),
        "…with the correction, not just the diagnosis: {missing}"
    );
    assert!(
        html.contains(
            r#"<div class="nyx-unrendered" data-unrendered-node="Root/PricingTable" data-unrendered-component="PricingTable" data-unrendered-reason="not_in_catalogue">[UNRENDERED PricingTable — not in the catalogue]</div>"#
        ),
        "the hole NAMES ITSELF where it sits — a placeholder, never a spinner and never nothing: \
         {html}"
    );
    assert!(
        warnings_of(&design_tool(
            &f.db,
            DESIGN_PREVIEW_TOOL,
            json!({ "variant_id": f.variant_a })
        ))
        .iter()
        .any(|w| w.contains("could NOT be rendered") && w.contains("Root/PricingTable")),
        "…and the warning says it out loud, so a hole is never read as a design"
    );

    // --- THE VIEWPORT, answered for what it is. ---
    let mobile = preview(json!({ "variant_id": f.variant_a, "viewport": "mobile" }));
    assert_eq!(mobile["viewport"]["width"], json!(390));
    assert_eq!(mobile["viewport"]["named"], json!("mobile"));
    assert_eq!(
        mobile["viewport"]["responsive_classes"],
        json!(0),
        "this design carries NO responsive class at all"
    );
    assert!(
        mobile["viewport"]["note"]
            .as_str()
            .unwrap_or_default()
            .contains("LAYS NOTHING OUT"),
        "…and the answer says a width renders nothing differently: {}",
        mobile["viewport"]
    );
    assert!(
        mobile["warnings"].as_array().unwrap().iter().any(|w| w
            .as_str()
            .unwrap_or_default()
            .contains("renders IDENTICALLY at 390px")),
        "…which is itself the verdict on `is this responsive?`: {}",
        mobile["warnings"]
    );

    // The agent makes the page responsive (the real gesture: re-synthesize the component), and the
    // SAME call now says which classes are in force at which width.
    design_tool(
        &f.db,
        DESIGN_REFRESH_COMPONENT_TOOL,
        json!({
            "project_id": f.project_id,
            "name": "Card",
            "template": r#"<section class="ds-card grid grid-cols-1 md:grid-cols-2"><header>{{title}}</header><div class="ds-card__body">{{{children}}}</div></section>"#,
        }),
    );
    let at = |width: Value| -> Value {
        preview(json!({ "variant_id": f.variant_a, "viewport": width }))["viewport"].clone()
    };
    let small = at(json!("mobile"));
    assert_eq!(small["variants"][0]["prefix"], json!("md"));
    assert_eq!(
        small["variants"][0]["active"],
        json!(false),
        "at 390 the `md:` classes are NOT in force — the one thing a width really decides here: {small}"
    );
    let large = at(json!(1280));
    assert_eq!(large["variants"][0]["active"], json!(true));
    assert!(
        large["breakpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["name"] == json!("md") && b["active"] == json!(true)),
        "…and the breakpoints in force are named: {large}"
    );

    // --- ONE SUBTREE, and the caps that point at it. ---
    let branch = preview(json!({ "variant_id": f.variant_a, "node_key": "Root/Save" }));
    let scope = nyx_core::css_slot::component_scope_id("Button");
    assert_eq!(
        branch["html"],
        json!(format!(
            "<button data-nyx-css=\"{scope}\" class=\"ds-btn\" data-state=\"idle\">Enregistrer</button>\
             <style data-nyx-css-slot=\"{scope}\">@keyframes {scope}-pulse {{\n  50% {{ opacity: .55 }}\n}}\n\
             [data-nyx-css~=\"{scope}\"] .ds-spin {{ animation: {scope}-pulse 1.4s ease-in-out infinite }}</style>"
        )),
        "a SUBTREE carries the exception of the component it holds, scoped and named"
    );
    assert_eq!(branch["css_slot_count"], json!(1));
    assert_eq!(branch["node_count"], json!(1));
    let unknown_node = design_tool_err(
        &f.db,
        DESIGN_PREVIEW_TOOL,
        json!({ "variant_id": f.variant_a, "node_key": "Root/Nope" }),
    );
    assert_eq!(unknown_node.code, RpcCode::InvalidId);
    assert!(
        unknown_node.message.contains("Root/Save"),
        "an unknown node_key names the keys that DO exist: {unknown_node:?}"
    );

    // A page big enough to be CUT: the bornage must never turn into an under-declaration — the
    // total stays the total of the RUN, the head says how much of it came back, and the answer
    // names the lever (node_key) instead of leaving the agent with a silent prefix.
    let page = proto_tool(
        &f.db,
        PROTO_ADD_PAGE_TOOL,
        json!({ "prototype_id": f.prototype_id, "slug": "long", "title": "Long" }),
    );
    let version = proto_tool(
        &f.db,
        PROTO_ADD_VERSION_TOOL,
        json!({ "page_id": page.result["page"]["id"], "label": "v1" }),
    );
    // A component whose RENDERING is far bigger than the node that instantiates it — so the tree
    // stays a small `composition_json` and the markup is what overflows, which is precisely the
    // case the cap has to state honestly.
    proto_tool(
        &f.db,
        PROTO_AUTHOR_COMPONENT_TOOL,
        json!({
            "prototype_id": f.prototype_id,
            "name": "Filler",
            "template": format!("<p class=\"filler\">{}</p>", "x".repeat(1900)),
        }),
    );
    let children: Vec<Value> = (0..20)
        .map(|i| json!({ "component": "Filler", "key": format!("F{i}") }))
        .collect();
    let long = proto_tool(
        &f.db,
        PROTO_ADD_VARIANT_TOOL,
        json!({
            "page_version_id": version.result["version"]["id"],
            "label": "a",
            "title": "A very long page",
            "composition_json": json!({ "component": "Card", "key": "Root",
                                        "params": { "title": "Long" }, "children": children })
                .to_string(),
        }),
    );
    let cut = preview(json!({ "variant_id": long.result["variant"]["id"] }));
    let returned = cut["html"].as_str().unwrap().len() as u64;
    assert_eq!(cut["truncated"], json!(true));
    assert_eq!(cut["html_returned_bytes"], json!(returned));
    assert!(
        cut["html_bytes"].as_u64().unwrap() > returned,
        "the TOTAL is the run's, never the length of the head that was kept: {} vs {returned}",
        cut["html_bytes"]
    );
    assert_eq!(
        cut["node_count"],
        json!(21),
        "…and the counters describe the WHOLE tree, not the part that fitted"
    );
    assert!(
        cut["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("node_key")),
        "…and the answer names the way to read the rest: {}",
        cut["warnings"]
    );

    // --- THE HONESTY, unchanged by the new subject. ---
    let cannot_show = out["cannot_show"]
        .as_str()
        .expect("every answer carries it");
    assert!(
        cannot_show.contains("NOT a visual validation")
            && cannot_show.contains("A whole COMPOSITION is no different")
            && cannot_show.contains("nyx renders NO IMAGE")
            && cannot_show.contains("the human is still the only eye on the visual"),
        "a whole page is more markup, not more sight — and the image door stays shut: {cannot_show}"
    );

    // --- THE PARCOURS: a node opens ANOTHER PAGE (finding `01KYW34347CZSNXCYS35KDG3EY`). ---
    // A prototype exists to show a JOURNEY. The link is on the NODE (a destination is a PLACEMENT
    // decision), it names the page BY ID (a slug renames), and the agent reads it back RESOLVED —
    // slug + title — because an id is not something anybody reasons about.
    let pricing_page = proto_tool(
        &f.db,
        PROTO_ADD_PAGE_TOOL,
        json!({ "prototype_id": f.prototype_id, "slug": "pricing", "title": "Tarifs" }),
    );
    let pricing_page_id = pricing_page.result["page"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let pricing_version = proto_tool(
        &f.db,
        PROTO_ADD_VERSION_TOOL,
        json!({ "page_id": pricing_page_id, "label": "v1" }),
    );
    let with_parcours = proto_tool(
        &f.db,
        PROTO_ADD_VARIANT_TOOL,
        json!({
            "page_version_id": pricing_version.result["version"]["id"],
            "label": "a",
            "title": "Tarifs",
            // A CYCLE, on purpose: this page links back to the page the fixture already holds. A
            // navigation is a USER GESTURE, never an expansion — so it renders instead of refusing.
            "composition_json": json!({
                "component": "Card", "key": "Root", "params": { "title": "Tarifs" },
                "children": [
                    { "component": "Button", "key": "Back",
                      "params": { "label": "Retour", "state": "idle" },
                      "navigate_to": f.page_id }
                ]
            })
            .to_string(),
        }),
    );
    let parcours = preview(json!({ "variant_id": with_parcours.result["variant"]["id"] }));
    assert_eq!(parcours["navigation_count"], json!(1));
    assert_eq!(parcours["navigation_broken_count"], json!(0));
    assert_eq!(parcours["navigation"][0]["node_key"], json!("Root/Back"));
    assert_eq!(parcours["navigation"][0]["page_id"], json!(f.page_id));
    assert_eq!(parcours["navigation"][0]["page"], json!("billing"));
    assert_eq!(
        parcours["navigation"][0]["page_title"],
        json!("Billing"),
        "the destination comes back under the name a HUMAN reads, resolved from the id"
    );
    assert_eq!(parcours["navigation"][0]["status"], json!("linked"));
    assert!(
        !parcours["html"]
            .as_str()
            .unwrap()
            .replace("data-nyx-css-slot=", "")
            .replace("data-nyx-css=", "")
            .replace("[data-nyx-css~=", "")
            .contains("data-nyx-"),
        "…and the agent's resolution still leaves NO vocabulary behind: the parcours is DATA here, \
         not a mark it cannot run (the scope mark is an IDENTITY, not vocabulary — see above)"
    );
    // A link to a page the prototype does not have never reaches the model: it is refused at the
    // WRITE, naming the pages that DO exist — a dead click is not a state a design may be in.
    let dead = proto_tool_err(
        &f.db,
        PROTO_ADD_VARIANT_TOOL,
        json!({
            "page_version_id": pricing_version.result["version"]["id"],
            "label": "dead",
            "title": "Dead",
            "composition_json": json!({ "component": "Button", "navigate_to": "pg-nope" })
                .to_string(),
        }),
    );
    assert_eq!(dead.code, RpcCode::InvalidId);
    assert!(
        dead.message.contains("billing") && dead.message.contains("pricing"),
        "the refusal names the real pages: {dead:?}"
    );

    // --- THE VARIANT'S OWN AXES ARE HONOURED (finding `01KZ3JS7PNF3F8GFB3GYRK6CKH`). ---
    // A variant declares axes exactly as a component does, and in the frame they pilot the WHOLE
    // page (one chip-bar row each). This resolution used to read them NOWHERE: the element gated on
    // `loading=on` was present at `off`, the `on` classes never landed, `axes` was absent from the
    // answer and asking for a value changed nothing — the "spinner that never hid" this very tool
    // exists to catch, one level up.
    let axed_page = proto_tool(
        &f.db,
        PROTO_ADD_PAGE_TOOL,
        json!({ "prototype_id": f.prototype_id, "slug": "loading", "title": "Chargement" }),
    );
    let axed_version = proto_tool(
        &f.db,
        PROTO_ADD_VERSION_TOOL,
        json!({ "page_id": axed_page.result["page"]["id"], "label": "v1" }),
    );
    let axed = proto_tool(
        &f.db,
        PROTO_ADD_VARIANT_TOOL,
        json!({
            "page_version_id": axed_version.result["version"]["id"],
            "label": "a",
            "title": "With a page-level loading axis",
            "composition_json": json!({
                "key": "Root",
                "inline": "<section><i data-nyx-if=\"loading=on\" class=\"spin\">◌</i>\
                           <b data-nyx-if-not=\"loading=on\">Prêt</b></section>",
            })
            .to_string(),
            "states_json": json!({ "axes": [{ "name": "loading", "default": "off", "values": [
                { "name": "off" },
                { "name": "on", "classes": "pointer-events-none opacity-70" }
            ] }] })
            .to_string(),
        }),
    );
    let axed_id = axed.result["variant"]["id"].clone();

    let at_default = preview(json!({ "variant_id": axed_id }));
    assert_eq!(
        at_default["axes"],
        json!([{ "axis": "loading", "value": "off" }]),
        "the combination the markup was resolved at comes BACK — an axis left out renders at its \
         own default"
    );
    assert_eq!(
        at_default["declared_axes"],
        json!([{ "axis": "loading", "default": "off", "values": ["off", "on"] }]),
        "…and the answer says what this variant can be piloted on"
    );
    let off = at_default["html"].as_str().unwrap();
    assert!(
        off.contains(r#"<i class="spin" hidden>"#) && !off.contains("opacity-70"),
        "at `loading=off` the conditioned element is ABSENT and the `on` classes are not applied: \
         {off}"
    );

    let on = preview(json!({ "variant_id": axed_id, "axes": { "loading": "on" } }));
    assert_eq!(on["axes"], json!([{ "axis": "loading", "value": "on" }]));
    let on_html = on["html"].as_str().unwrap();
    assert!(
        on_html.contains(r#"<i class="spin">"#)
            && on_html.contains("pointer-events-none opacity-70")
            && on_html.contains("<b hidden>Prêt</b>"),
        "ASKING FOR A VALUE HAS AN EFFECT: the element appears, the value's classes land on the \
         page's root, and the complement goes: {on_html}"
    );
    assert!(
        !on_html.contains("data-nyx-"),
        "…and the vocabulary is still consumed: {on_html}"
    );

    // An axis, or a value, the variant does NOT declare is NAMED — never silently ignored.
    let unknown_axis = design_tool(
        &f.db,
        DESIGN_PREVIEW_TOOL,
        json!({ "variant_id": axed_id, "axes": { "size": "lg" } }),
    );
    assert!(
        warnings_of(&unknown_axis)
            .iter()
            .any(|w| w.contains("NO axis 'size'") && w.contains("loading")),
        "the refusal to guess names the axes the variant really has: {:?}",
        warnings_of(&unknown_axis)
    );
    let unknown_value = design_tool(
        &f.db,
        DESIGN_PREVIEW_TOOL,
        json!({ "variant_id": axed_id, "axes": { "loading": "maybe" } }),
    );
    assert!(
        warnings_of(&unknown_value)
            .iter()
            .any(|w| w.contains("does not declare the value 'maybe'") && w.contains("'off'")),
        "…and so does an unknown VALUE, with the one it fell back to: {:?}",
        warnings_of(&unknown_value)
    );
    // A variant that declares NO axis says so rather than swallowing the argument.
    let axeless = design_tool(
        &f.db,
        DESIGN_PREVIEW_TOOL,
        json!({ "variant_id": f.variant_a, "axes": { "loading": "on" } }),
    );
    assert_eq!(axeless.result["axes"], json!([]));
    assert!(
        warnings_of(&axeless)
            .iter()
            .any(|w| w.contains("declares NO axis at all") && w.contains("states_json")),
        "…and it names the write that would give it one: {:?}",
        warnings_of(&axeless)
    );

    // --- THE SUBJECT IS NAMED, never guessed. ---
    let both = design_tool_err(
        &f.db,
        DESIGN_PREVIEW_TOOL,
        json!({ "project_id": f.project_id, "component": "Card", "variant_id": f.variant_a }),
    );
    assert_eq!(both.code, RpcCode::InvalidArgument);
    let neither = design_tool_err(
        &f.db,
        DESIGN_PREVIEW_TOOL,
        json!({ "project_id": f.project_id }),
    );
    assert!(
        neither.message.contains("variant_id") && neither.message.contains("component"),
        "the refusal names both subjects: {neither:?}"
    );
    let ghost = design_tool_err(
        &f.db,
        DESIGN_PREVIEW_TOOL,
        json!({ "variant_id": "01JZZZZZZZZZZZZZZZZZZZZZZZ" }),
    );
    assert_eq!(ghost.code, RpcCode::InvalidId);
}
