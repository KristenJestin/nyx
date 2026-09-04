//! Unit + integration tests for the DESIGN TOKENS layer ([`super`], review #42).
//!
//! The PURE half (the hand-rolled CSS custom-property scanner + the layer's JSON round-trip) is
//! tested without disk/DB; the INGEST is exercised over a real scratch tree + an in-memory DB,
//! including the two safety properties that matter: a detection that finds nothing never erases a
//! hand-authored theme, and the layer stays a PROJECT SINGLETON (referenced, never copied).

use super::*;
use crate::db::{create_project, create_repository, get_design, open_in_memory, upsert_design};
use uuid::Uuid;

/// A unique scratch dir under the OS temp dir (no tempfile dev-dep — uniqueness from a v7 UUID).
fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nyx-tokens-{tag}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Write `content` to `dir/rel`, creating parent directories. `rel` uses `/` separators.
fn write_file(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir parent");
    }
    std::fs::write(path, content).expect("write file");
}

/// The light-theme value of `name` among extracted variables (last declaration wins, as in CSS).
fn light(vars: &[CssVariable], name: &str) -> Option<String> {
    vars.iter()
        .rfind(|v| v.name == name && v.scope == TokenScope::Root)
        .map(|v| v.value.clone())
}

/// A realistic shadcn/Tailwind-v4 `globals.css` — the exact shape the review's real front uses.
const SHADCN_GLOBALS: &str = r#"
@import "tailwindcss";

/* The brand palette — the single identity of `primary`. */
:root {
  --radius: 0.625rem;
  --background: oklch(1 0 0);
  --foreground: oklch(0.141 0.005 285.823);
  --primary: oklch(0.55 0.21 264.4);
  --primary-foreground: oklch(0.985 0 0);
  --font-sans: "Inter", ui-sans-serif, system-ui;
}

.dark {
  --background: oklch(0.141 0.005 285.823);
  --foreground: oklch(0.985 0 0);
  --primary: oklch(0.72 0.16 264.4);
}

@theme inline {
  --color-background: var(--background);
  --color-foreground: var(--foreground);
  --color-primary: var(--primary);
  --color-primary-foreground: var(--primary-foreground);
  --radius-sm: calc(var(--radius) - 4px);
}

/* NOT design tokens: a component-scoped custom property. */
.invoice-card {
  --card-gap: 12px;
}

@keyframes pulse {
  from { --fake-token: 1; }
}
"#;

// --- the scanner (pure) ----------------------------------------------------

#[test]
fn extracts_root_theme_and_dark_custom_properties_and_ignores_component_scopes() {
    let vars = extract_css_variables(SHADCN_GLOBALS);

    // `:root` declarations are tokens.
    assert_eq!(
        light(&vars, "--primary").as_deref(),
        Some("oklch(0.55 0.21 264.4)")
    );
    assert_eq!(light(&vars, "--radius").as_deref(), Some("0.625rem"));
    assert_eq!(
        light(&vars, "--font-sans").as_deref(),
        Some("\"Inter\", ui-sans-serif, system-ui"),
        "a value carrying a comma + quotes survives intact"
    );

    // A bare `@theme` block is the design system too (Tailwind v4's own shape), and a value with
    // nested parens is read whole.
    assert_eq!(
        light(&vars, "--color-primary").as_deref(),
        Some("var(--primary)")
    );
    assert_eq!(
        light(&vars, "--radius-sm").as_deref(),
        Some("calc(var(--radius) - 4px)")
    );

    // `.dark` lands in the DARK scope, not the light one.
    let dark: Vec<&CssVariable> = vars
        .iter()
        .filter(|v| v.scope == TokenScope::Dark)
        .collect();
    assert!(
        dark.iter()
            .any(|v| v.name == "--primary" && v.value == "oklch(0.72 0.16 264.4)"),
        "the dark override is captured separately: {dark:?}"
    );
    assert_eq!(
        light(&vars, "--primary").as_deref(),
        Some("oklch(0.55 0.21 264.4)"),
        "the dark block never overwrites the light identity"
    );

    // A component-scoped property is NOT a design token, and `@keyframes` is skipped outright.
    assert!(
        !vars.iter().any(|v| v.name == "--card-gap"),
        "a `.invoice-card` scoped property is not the design system"
    );
    assert!(!vars.iter().any(|v| v.name == "--fake-token"));
}

#[test]
fn scanner_ignores_comments_selectors_and_var_usages() {
    let css = r#"
/* :root { --commented-out: red; } */
.btn--primary { color: red; }
:root {
  --brand: #ff0055;
  --on-brand: var(--brand);
  --url: url("a;b}c.png");
}
@media (prefers-color-scheme: dark) {
  :root { --brand: #220011; }
}
"#;
    let vars = extract_css_variables(css);
    assert!(
        !vars.iter().any(|v| v.name == "--commented-out"),
        "a commented-out block declares nothing"
    );
    assert_eq!(light(&vars, "--brand").as_deref(), Some("#ff0055"));
    assert_eq!(light(&vars, "--on-brand").as_deref(), Some("var(--brand)"));
    assert_eq!(
        light(&vars, "--url").as_deref(),
        Some("url(\"a;b}c.png\")"),
        "a `;` or closing brace inside a string or parens does not end the value"
    );
    assert!(
        vars.iter()
            .any(|v| v.scope == TokenScope::Dark && v.name == "--brand"),
        "`prefers-color-scheme: dark` is the dark theme"
    );
    // The `.btn--primary` SELECTOR is not a declaration.
    assert!(!vars.iter().any(|v| v.name.contains("primary")));
}

#[test]
fn the_dark_marker_is_the_theme_root_not_any_selector_spelling_dark() {
    // The REAL conventions a front uses to declare its dark theme — every one of them still lands
    // in the dark map (review #42 findings 01KYJ72WE64ACMKNTSYBAPJ4HX / 01KYJ7APQ23621HQG6K6T07QNW).
    let css = r#"
.dark { --a: 1 }
html.dark { --b: 2 }
:root[data-theme="dark"] { --c: 3 }
[data-theme='dark'] { --d: 4 }
html.dark body { --e: 5 }
@media (prefers-color-scheme: dark) { :root { --f: 6 } }
"#;
    let vars = extract_css_variables(css);
    for name in ["--a", "--b", "--c", "--d", "--e", "--f"] {
        assert!(
            vars.iter()
                .any(|v| v.name == name && v.scope == TokenScope::Dark),
            "{name} is declared on the dark theme ROOT: {vars:?}"
        );
    }

    // The FALSE POSITIVES `contains("dark")` used to swallow: a component selector that merely
    // SPELLS «dark» is not the design system — it contributes NOTHING, light or dark.
    let css = r#"
.btn-darken { --shadow: 1px }
.dark-panel { --panel-gap: 4px }
.dark-mode-card { --card-gap: 12px }
.sidebar--darker { --sidebar-bg: #111 }
[data-theme="darkroom"] { --room: 1 }
.dark .invoice-card { --invoice-gap: 8px }
@media not all and (prefers-color-scheme: dark) { :root { --light-only: 7 } }
"#;
    let vars = extract_css_variables(css);
    for name in [
        "--shadow",
        "--panel-gap",
        "--card-gap",
        "--sidebar-bg",
        "--room",
        "--invoice-gap",
    ] {
        assert!(
            !vars.iter().any(|v| v.name == name),
            "{name} sits on a COMPONENT selector — not a token, and above all not a dark token: {vars:?}"
        );
    }
    // A NEGATED colour-scheme query is the LIGHT branch, not the dark one.
    assert_eq!(light(&vars, "--light-only").as_deref(), Some("7"));

    // The MIXED prelude: the list really does declare on `:root`, so the LIGHT identity is kept
    // (it used to be hijacked into the dark map by the `.dark-panel` member).
    let vars = extract_css_variables(":root, .dark-panel { --primary: #ff0055 }");
    assert_eq!(light(&vars, "--primary").as_deref(), Some("#ff0055"));
    assert!(
        !vars.iter().any(|v| v.scope == TokenScope::Dark),
        "a component member of the list never turns the block into the dark theme: {vars:?}"
    );
}

#[test]
fn scanner_never_panics_on_malformed_or_non_ascii_css() {
    for css in [
        "",
        "{{{",
        ":root { --a",
        ":root { --é-token: rouge; /* accentué */ }",
        "@theme { --color-x: oklch(0.5 0.1 200",
        "}}}:root{--b:1}",
    ] {
        let _ = extract_css_variables(css);
    }
    let vars = extract_css_variables(":root { --é: 1px; --after: 2px; }");
    assert_eq!(
        light(&vars, "--after").as_deref(),
        Some("2px"),
        "a multi-byte name does not desynchronise the scan"
    );
}

// --- the persisted layer (pure) --------------------------------------------

#[test]
fn token_layer_json_round_trips_and_tolerates_legacy_blobs() {
    let mut layer = TokenLayer::default();
    layer.variables.insert("--primary".into(), "#123456".into());
    layer.dark.insert("--primary".into(), "#abcdef".into());
    layer.sources.push(TokenSource {
        repository_id: Some("repo-1".into()),
        repo: "web".into(),
        path: "src/styles.css".into(),
        variable_count: 1,
    });

    let json = layer.to_json();
    let back = TokenLayer::parse(Some(&json));
    assert_eq!(back, layer, "the layer round-trips through its JSON shape");
    // The PRIMARY reference is echoed for readers that only want "where does `primary` live?".
    let value: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["source"]["path"], "src/styles.css");

    // A layer with NO report emits no report key at all: the panel must be able to tell "nobody
    // recorded what was discarded" from "nothing was discarded".
    assert!(layer.provenance.is_none());
    for key in [
        "skipped",
        "skipped_count",
        "pruned_dirs",
        "pruned_dirs_count",
    ] {
        assert!(
            value.get(key).is_none(),
            "{key} is absent from a layer that carries no report: {json}"
        );
    }

    // The Phase-1 untyped blob (and any junk) degrades to an empty layer, never an error.
    for legacy in [
        None,
        Some(""),
        Some("   "),
        Some("not json"),
        Some("[]"),
        Some(r##"{"colors":{"primary":"#fff"}}"##),
    ] {
        let parsed = TokenLayer::parse(legacy);
        assert!(parsed.is_empty(), "legacy blob {legacy:?}");
        assert!(
            parsed.provenance.is_none(),
            "a legacy blob RECORDED nothing — which is not 'it discarded nothing': {legacy:?}"
        );
    }
}

/// THE FINDING (01KYSK7GS9C3ZRJYW00ZA0H6GF): the provenance of the DISCARDED sheets was computed at
/// ingest and thrown away at persistence — `to_json` emitted `{ source, sources, variables, dark }`
/// and `parse` read nothing else, so the report lasted exactly one tool call. The `/design` panel may
/// not trigger an ingest (the agent is the only pilot of that step), so the stored blob is the ONLY
/// place it can read the report from: it showed "non enregistrés" over a layer that may well have
/// been polluted. The report has to survive the round-trip, BOUNDED, under the keys the renderer
/// reserves.
#[test]
fn the_persisted_layer_carries_the_provenance_of_what_was_discarded() {
    let mut layer = TokenLayer::default();
    layer.variables.insert("--primary".into(), "#123456".into());
    layer.sources.push(TokenSource {
        repository_id: None,
        repo: "web".into(),
        path: "src/styles.css".into(),
        variable_count: 1,
    });
    // MORE discards than the cap: a token blob is a reference, not a journal.
    let discarded: Vec<SkippedSheet> = (0..MAX_SKIPPED_REPORTED + 9)
        .map(|i| SkippedSheet {
            repo: "web".into(),
            path: format!("docs/theme-{i:02}.css"),
            reason: "doc_tool_theme",
        })
        .collect();
    let total = discarded.len();
    // …and MORE pruned directory names than THEIR cap: `ignored_dirs` is an agent-written field
    // capped at 64 entries, so 64 + the floor + the tokens-only names really does overflow this one
    // (finding `01KYSNRNY550NS778YCMC8CZGH`).
    let mut pruned: Vec<String> = vec![".generated".into(), "node_modules".into()];
    pruned.extend((0..MAX_PRUNED_DIRS_REPORTED).map(|i| format!("vendor-{i:03}")));
    let pruned_total = pruned.len();
    layer.provenance = Some(TokenProvenance::bounded(
        discarded,
        total,
        pruned,
        pruned_total,
    ));

    let json = layer.to_json();
    let back = TokenLayer::parse(Some(&json));
    assert_eq!(back, layer, "the report round-trips with the layer");

    let report = back.provenance.expect("the layer carries its report");
    // BOUNDED head, HONEST total — truncating never reads as "only the head was discarded".
    assert_eq!(report.skipped.len(), MAX_SKIPPED_REPORTED);
    assert_eq!(report.skipped_count, total);
    assert_eq!(report.skipped[0].reason, "doc_tool_theme");
    // The SAME treatment for the pruned directories: a bounded head that carries its own total, so
    // the panel can say "… et N autre(s)" instead of presenting the cap as the count.
    assert_eq!(report.pruned_dirs.len(), MAX_PRUNED_DIRS_REPORTED);
    assert_eq!(report.pruned_dirs_count, pruned_total);
    assert!(
        pruned_total > MAX_PRUNED_DIRS_REPORTED,
        "the cap really bites"
    );
    assert_eq!(report.pruned_dirs[0], ".generated");

    // The KEYS are the ones `theme-css.ts` reserves — the renderer flattens every unknown top-level
    // key into a CSS custom property, so a fourth spelling would come out as `--pruned-dirs-0`.
    let value: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["skipped_count"], json!(total));
    assert_eq!(value["skipped"][0]["reason"], "doc_tool_theme");
    assert_eq!(value["pruned_dirs"][0], ".generated");
    assert_eq!(value["pruned_dirs_count"], json!(pruned_total));

    // A blob whose report is JUNK still says a report was recorded (the key is the claim), but not
    // one entry of it is trusted: no path, or a reason code that is not ours, is dropped.
    let junk = TokenLayer::parse(Some(
        r##"{"variables":{"--primary":"#fff"},"skipped":[{"repo":"web"},{"path":"a.css","reason":"made_up"},"nope",7],"skipped_count":"lots","pruned_dirs":["dist","",3]}"##,
    ));
    let report = junk
        .provenance
        .expect("the keys are there — a run recorded this");
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(report.skipped_count, 0);
    assert_eq!(report.pruned_dirs, vec!["dist"]);
    // No usable total in the blob → the length IS the total, never a smaller number.
    assert_eq!(report.pruned_dirs_count, 1);
    assert_eq!(
        junk.variables.get("--primary").map(String::as_str),
        Some("#fff"),
        "a junk report never costs the layer its tokens"
    );

    // A blob WITH a report but nothing in it is a run that discarded nothing — still not `None`.
    let clean = TokenLayer::parse(Some(
        r##"{"variables":{"--primary":"#fff"},"skipped":[],"skipped_count":0,"pruned_dirs":[]}"##,
    ));
    assert_eq!(clean.provenance, Some(TokenProvenance::default()));
}

#[test]
fn token_names_are_normalized_to_one_spelling() {
    assert_eq!(normalize_token_name("primary"), "--primary");
    assert_eq!(normalize_token_name("--primary"), "--primary");
    assert_eq!(normalize_token_name("  --primary  "), "--primary");
    assert_eq!(normalize_token_name("-primary"), "--primary");
}

// --- ingest / edit (disk + DB) ---------------------------------------------

#[test]
fn a_pathological_stylesheet_is_read_bounded_never_slurped_whole() {
    let root = scratch_dir("capped");
    // A generated bundle several times the cap: the tokens are at the TOP, the marker at the very
    // END — past the cap, so a bounded read can never see it.
    let padding = "/* generated bundle padding */\n".repeat(4 * MAX_STYLESHEET_BYTES / 30);
    let huge = format!(":root {{ --a: 1; --b: 2; --c: 3; }}\n{padding}\n:root {{ --tail: 9; }}\n");
    assert!(
        huge.len() > 3 * MAX_STYLESHEET_BYTES,
        "the file dwarfs the cap"
    );
    write_file(&root, "src/styles.css", &huge);

    let read = read_capped(&root.join("src/styles.css")).expect("a readable sheet");
    assert_eq!(
        read.len(),
        MAX_STYLESHEET_BYTES,
        "the read stops AT the cap (ASCII content ⇒ bytes == chars)"
    );
    assert!(
        !read.contains("--tail"),
        "what lies past the cap is never read"
    );

    // …and the scan of that head still yields the tokens it did understand.
    let vars = extract_css_variables(&read);
    assert_eq!(light(&vars, "--a").as_deref(), Some("1"));
    assert!(!vars.iter().any(|v| v.name == "--tail"));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_candidate_carries_the_variables_its_scoring_pass_already_extracted() {
    // The ingest reads and parses each candidate ONCE: the scoring walk keeps what it extracted,
    // and the merge folds THAT (it used to re-open and re-scan every retained sheet).
    let root = scratch_dir("read-once");
    write_file(&root, "src/styles.css", SHADCN_GLOBALS);

    let sweep = sweep_repo(
        &root,
        &crate::design::default_recipe(crate::design::ComponentStack::Generic),
    );
    let candidates = sweep.candidates;
    assert_eq!(candidates.len(), 1, "candidates: {candidates:?}");
    assert_eq!(candidates[0].rel_path, "src/styles.css");
    assert_eq!(
        candidates[0].variables,
        extract_css_variables(SHADCN_GLOBALS),
        "the candidate carries exactly the scan the merge would otherwise redo"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn ingests_the_app_theme_stylesheet_by_reference_and_feeds_bg_primary() {
    let root = scratch_dir("ingest");
    write_file(&root, "src/styles.css", SHADCN_GLOBALS);
    // Decoys: a build artefact (skipped dir) and a page stylesheet (too few global tokens).
    write_file(
        &root,
        "node_modules/pkg/theme.css",
        ":root{--primary:red;--a:1;--b:2}",
    );
    write_file(
        &root,
        "src/app/invoice/invoice.css",
        ".invoice { color: red; }",
    );

    let mut conn = open_in_memory();
    let (project, _ws) =
        create_project(&mut conn, "sandbox", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    let run = ingest_project_tokens(&mut conn, &project.id).unwrap();
    assert_eq!(run.repos_scanned, 1);
    assert!(run.changed, "a first ingest writes the layer");

    // REFERENCED, not copied: the layer names the app stylesheet it was derived from.
    assert_eq!(run.sources.len(), 1, "sources: {:?}", run.sources);
    assert_eq!(run.sources[0].path, "src/styles.css");
    assert_eq!(run.sources[0].repo, "web");
    assert!(
        run.sources[0].repository_id.is_some(),
        "the reference carries the provenance repo id"
    );

    // The tokens `bg-primary` needs are there — the direct cause of the GREY preview.
    let layer = project_token_layer(&mut conn, &project.id).unwrap();
    assert_eq!(
        layer.variables.get("--primary").map(String::as_str),
        Some("oklch(0.55 0.21 264.4)")
    );
    assert_eq!(
        layer.variables.get("--color-primary").map(String::as_str),
        Some("var(--primary)"),
        "the app's own @theme alias is kept — one identity, referenced twice"
    );
    assert_eq!(
        layer.dark.get("--primary").map(String::as_str),
        Some("oklch(0.72 0.16 264.4)")
    );
    assert!(!layer.variables.contains_key("--card-gap"));

    // The layer is the project's SINGLE row (never per component), and re-ingesting is a no-op.
    let stored = get_design(&mut conn, &project.id)
        .unwrap()
        .expect("design row");
    assert_eq!(
        stored.theme_tokens_json.as_deref(),
        Some(run.tokens_json.as_str())
    );
    let again = ingest_project_tokens(&mut conn, &project.id).unwrap();
    assert!(!again.changed, "an unchanged app re-ingests to nothing");
    assert_eq!(again.tokens_json, run.tokens_json);

    std::fs::remove_dir_all(&root).ok();
}

/// A8 — A TOKEN NEVER COMES OUT OF THE INGEST REFERENCING ITSELF.
///
/// ORIGIN, MEASURED: the INGEST, not the source. `depot/src/web/styles/globals.css` declares the
/// real value once in `:root` (`--radius-md: calc(var(--radius) - 2px)`, line 14) and bridges it
/// once in `@theme inline` (`--radius-md: var(--radius-md)`, line 182) — which is Tailwind v4's
/// documented way to expose a `:root` property into the theme namespace, resolved at build time.
/// Both blocks are GLOBAL scope, so nyx flattened them into one map under the plain-CSS last-wins
/// rule and the bridge overwrote the value. What the `/design` layer then handed every consumer was
/// `--radius-md: var(--radius-md)`: a variable that resolves to empty while looking defined.
///
/// The sheet below carries the same shape — a value in `:root`, a bridge in `@theme inline`, in
/// both orders, plus a bridge with NO backing value at all.
#[test]
fn a_theme_bridge_never_overwrites_the_value_it_bridges() {
    let root = scratch_dir("selfref");
    write_file(
        &root,
        "src/styles.css",
        r#"
:root {
  --radius: 0.625rem;
  --radius-md: calc(var(--radius) - 2px);
  --background: oklch(1 0 0);
  --primary: oklch(0.55 0.21 264.4);
  --spacing: 4px;
}
@theme inline {
  --radius-md: var(--radius-md);
  --color-background: var(--background);
  --orphan-token: var(--orphan-token);
  --spacing-with-fallback: var(--spacing-with-fallback, 8px);
}
@theme inline {
  --primary: var(--primary);
}
"#,
    );

    let mut conn = open_in_memory();
    let (project, _ws) =
        create_project(&mut conn, "sandbox", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    ingest_project_tokens(&mut conn, &project.id).unwrap();
    let layer = project_token_layer(&mut conn, &project.id).unwrap();

    // THE PROPERTY: not one rendered token references itself, in either scope.
    for (scope, map) in [("light", &layer.variables), ("dark", &layer.dark)] {
        for (name, value) in map.iter() {
            assert_ne!(
                value.trim(),
                format!("var({name})"),
                "{scope} token {name} references itself"
            );
        }
    }

    // …and the value the bridge was hiding is the one that survived — bridge AFTER the declaration…
    assert_eq!(
        layer.variables.get("--radius-md").map(String::as_str),
        Some("calc(var(--radius) - 2px)")
    );
    // …and bridge in a LATER block, over a value declared earlier.
    assert_eq!(
        layer.variables.get("--primary").map(String::as_str),
        Some("oklch(0.55 0.21 264.4)")
    );
    // A bridge to ANOTHER name is not a self-reference and is kept, as it always was: it is the
    // alias that gives one identity two names.
    assert_eq!(
        layer.variables.get("--color-background").map(String::as_str),
        Some("var(--background)")
    );
    // A bridge with no backing value is not rendered at all — an absent token is honest, a
    // self-referencing one is not.
    assert!(!layer.variables.contains_key("--orphan-token"));
    // A self-reference WITH a fallback carries a real value, so it is left alone.
    assert_eq!(
        layer
            .variables
            .get("--spacing-with-fallback")
            .map(String::as_str),
        Some("var(--spacing-with-fallback, 8px)")
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn an_ingest_that_detects_nothing_never_erases_an_authored_theme() {
    let root = scratch_dir("greenfield");
    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Green", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    let mut authored = TokenLayer::default();
    authored
        .variables
        .insert("--primary".into(), "#ff0055".into());
    upsert_design(
        &mut conn,
        &project.id,
        "# DESIGN",
        Some(&authored.to_json()),
    )
    .unwrap();

    let run = ingest_project_tokens(&mut conn, &project.id).unwrap();
    assert!(!run.changed, "nothing detected ⇒ no TOKEN written");
    assert_eq!(run.variable_count, 1, "the authored layer is reported back");
    let layer = project_token_layer(&mut conn, &project.id).unwrap();
    assert_eq!(
        layer.variables.get("--primary").map(String::as_str),
        Some("#ff0055"),
        "a failed detection is never collateral damage on a hand-authored theme"
    );
    // What it DID write is its report — an empty one here, since this tree holds nothing at all to
    // refuse. `Some(empty)` is a statement ("a run looked, and discarded nothing"); `None` is the
    // absence of one, and the panel renders the two differently.
    assert_eq!(
        layer.provenance,
        Some(TokenProvenance::default()),
        "the run recorded that it refused nothing — which is not 'nobody recorded anything'"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// A tree whose EVERY stylesheet is refused, each for a different reason, plus two directories the
/// walk never descends into — the shape of a front where the ingest retains nothing at all (a SCSS
/// variables front, a docs-tooled repo, a `--vp-*` theme). Returns the scratch root.
fn tree_where_every_sheet_is_refused(tag: &str) -> PathBuf {
    let root = scratch_dir(tag);
    // Another tool's theme directory: never the app's design system, whatever the file is named.
    write_file(
        &root,
        "docs/.vitepress/theme/custom.css",
        ":root { --vp-c-brand-1: #3451b2; --vp-c-bg: #fff; --vp-custom-block-tip-bg: #e6f6e6 }",
    );
    // A sheet whose variables ARE a third-party namespace, sitting in the app's own tree.
    write_file(
        &root,
        "src/brand.css",
        ":root { --vp-c-brand-1: #3451b2; --vp-c-bg: #fff; --vp-c-text-1: #111 }",
    );
    // A page stylesheet: its only custom property is component-scoped, so it is not a token sheet.
    write_file(
        &root,
        "src/page.css",
        ".invoice-card { --card-gap: 12px; color: red }",
    );
    // …and the two directories the walk refuses to enter — one of them the GENERATED mirror of a
    // stylesheet that WOULD have been ingested, which is exactly what the report has to say.
    write_file(&root, ".generated/styles.css", SHADCN_GLOBALS);
    write_file(
        &root,
        "node_modules/pkg/theme.css",
        ":root{--primary:red;--a:1;--b:2}",
    );
    root
}

/// THE FINDING (`01KYSNVW31C0E8YQPET0505HW2`): the ingest bailed out BEFORE any write as soon as the
/// derived layer was empty, so this run's report died with the tool call — the defect that was fixed
/// for a non-empty layer (`01KYSK7GS9C3ZRJYW00ZA0H6GF`) coming back out through the worst door there
/// is. Zero token stored means every design-system class renders grey, and the panel (which may not
/// run an ingest of its own) then had BOTH "aucun token" and "non enregistrés" to show: an unusable
/// layer with no signal whatsoever, which is the original defect verbatim.
///
/// The report is now recorded — AND NOTHING ELSE IS. The reasoning the early return protected is
/// intact: a failed detection is not collateral damage on a theme someone typed by hand.
#[test]
fn a_run_that_retains_no_sheet_records_why_and_leaves_the_authored_theme_alone() {
    let root = tree_where_every_sheet_is_refused("retains-nothing");
    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Grey", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    // A theme TYPED BY HAND: variables, a dark override, a source reference — and a `css` escape
    // hatch, a shape `TokenLayer` does not even model (`parse` degrades it to an empty layer). If
    // the run rewrote the layer it PARSED, that key would be gone.
    let authored = r##"{"sources":[{"repository_id":null,"repo":"web","path":"hand/written.css","variable_count":1}],"variables":{"--primary":"#ff0055"},"dark":{"--primary":"#220011"},"css":"--color-brand: var(--primary);"}"##;
    upsert_design(&mut conn, &project.id, "# DESIGN", Some(authored)).unwrap();

    let run = ingest_project_tokens(&mut conn, &project.id).unwrap();
    assert_eq!(run.variable_count, 1, "the stored theme is reported back");
    assert!(!run.changed, "no sheet retained ⇒ no TOKEN written");
    assert!(run.provenance_changed, "…but the run DID record WHY");

    // RE-READ FROM THE DATABASE — the panel's own path, and the one the report used to never reach.
    let layer = project_token_layer(&mut conn, &project.id).unwrap();
    assert_eq!(
        layer.variables.get("--primary").map(String::as_str),
        Some("#ff0055"),
        "the hand-authored light identity is untouched"
    );
    assert_eq!(
        layer.dark.get("--primary").map(String::as_str),
        Some("#220011"),
        "…and its dark override"
    );
    assert_eq!(
        layer.sources.first().map(|s| s.path.as_str()),
        Some("hand/written.css"),
        "…and the reference it carried"
    );
    let report = layer
        .provenance
        .expect("the stored layer carries the run's report");
    let reason_of = |path: &str| {
        report
            .skipped
            .iter()
            .find(|s| s.path == path)
            .map(|s| s.reason)
            .unwrap_or_else(|| panic!("{path} in the STORED report: {:?}", report.skipped))
    };
    assert_eq!(
        reason_of("docs/.vitepress/theme/custom.css"),
        "doc_tool_theme"
    );
    assert_eq!(reason_of("src/brand.css"), "foreign_namespace");
    assert_eq!(reason_of("src/page.css"), "not_a_token_sheet");
    assert_eq!(report.skipped_count, 3);
    // …and where the walk did not go: `.generated` here names the mirror of a sheet that would
    // otherwise have BEEN the layer.
    assert_eq!(report.pruned_dirs, vec![".generated", "node_modules"]);
    assert_eq!(report.pruned_dirs_count, 2);
    assert_eq!(report.skipped, run.skipped, "the blob echoes the run");

    // ONLY the report keys were rewritten: a shape the layer type does not model survives the write.
    let stored: Value = serde_json::from_str(
        &get_design(&mut conn, &project.id)
            .unwrap()
            .and_then(|d| d.theme_tokens_json)
            .expect("a stored layer"),
    )
    .unwrap();
    assert_eq!(
        stored["css"], "--color-brand: var(--primary);",
        "the run touched the four report keys and nothing else: {stored}"
    );

    // Re-running over the same tree is a no-op — the report is not a journal that grows per run.
    let again = ingest_project_tokens(&mut conn, &project.id).unwrap();
    assert!(!again.changed && !again.provenance_changed, "{again:?}");
    assert_eq!(again.tokens_json, run.tokens_json);

    // THE BOUNDING STAYS HONEST ON THIS PATH TOO (finding `01KYSNRNY550NS778YCMC8CZGH`): the stored
    // head is what fits, the stored count is what really happened.
    for i in 0..MAX_SKIPPED_REPORTED {
        write_file(
            &root,
            &format!("docs/.vitepress/theme/extra-{i:02}.css"),
            ":root { --vp-c-bg: #fff }",
        );
    }
    let run = ingest_project_tokens(&mut conn, &project.id).unwrap();
    assert!(!run.changed && run.provenance_changed, "{run:?}");
    let layer = project_token_layer(&mut conn, &project.id).unwrap();
    let report = layer.provenance.expect("still reported");
    assert_eq!(report.skipped.len(), MAX_SKIPPED_REPORTED, "the head fits");
    assert_eq!(
        report.skipped_count,
        MAX_SKIPPED_REPORTED + 3,
        "the total is the run's, never the length of the head"
    );
    assert_eq!(
        layer.variables.get("--primary").map(String::as_str),
        Some("#ff0055"),
        "and the hand-authored theme is STILL not collateral damage"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// THE SHAPE THE PANEL IS ACTUALLY GIVEN, pinned across the two languages through
/// `contract/design-fixtures.json` (the mechanism of finding `01KYSNQWQD1324E0NA0172M66J`).
///
/// The renderer's test for "the walk discarded everything" — its own comment calls it the most
/// valuable case — built its blob BY HAND, and built one no backend write could produce. A front
/// ready for a shape the backend never sends is not a covered case, it is a fixture. So the fixture
/// is now what a REAL run stores: this half asserts the run writes exactly it,
/// `theme-tokens-panel.test.tsx` renders exactly it. Note what a run that retained nothing over a
/// project with nothing stored does NOT write: no `variables`, no `dark`, no `sources` — it has no
/// business claiming any.
#[test]
fn the_blob_a_run_that_retains_nothing_writes_is_the_shared_fixture() {
    let root = tree_where_every_sheet_is_refused("empty-run-blob");
    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Grey", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    let run = ingest_project_tokens(&mut conn, &project.id).unwrap();
    assert!(!run.changed && run.provenance_changed, "{run:?}");
    let stored = get_design(&mut conn, &project.id)
        .unwrap()
        .and_then(|d| d.theme_tokens_json)
        .expect("a run that retained nothing still WROTE its report");
    assert_eq!(stored, run.tokens_json, "the run answers what it stored");

    let fixture: Value = serde_json::from_str(crate::mcp_tools_design::DESIGN_FIXTURES)
        .expect("contract/design-fixtures.json is valid JSON");
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap(),
        fixture["empty_run_blob"]["blob"],
        "contract/design-fixtures.json must carry the blob this run really writes — the renderer \
         renders that one: {stored}"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn editing_a_token_keeps_the_source_reference_and_the_design_md() {
    let root = scratch_dir("edit");
    write_file(&root, "src/styles.css", SHADCN_GLOBALS);
    let mut conn = open_in_memory();
    let (project, _ws) =
        create_project(&mut conn, "sandbox", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    upsert_design(&mut conn, &project.id, "# DESIGN.md stays", None).unwrap();
    ingest_project_tokens(&mut conn, &project.id).unwrap();

    let mut set = BTreeMap::new();
    // Accepts the bare spelling too — one storage spelling either way.
    set.insert("primary".to_string(), "#ff0055".to_string());
    let layer = edit_project_tokens(
        &mut conn,
        &project.id,
        &set,
        &["--font-sans".to_string()],
        false,
    )
    .unwrap();

    assert_eq!(
        layer.variables.get("--primary").map(String::as_str),
        Some("#ff0055")
    );
    assert!(!layer.variables.contains_key("--font-sans"), "removed");
    assert_eq!(
        layer.sources.first().map(|s| s.path.as_str()),
        Some("src/styles.css"),
        "an edit never severs the reference back to the app stylesheet"
    );
    assert!(
        layer.provenance.is_some(),
        "nor does it erase what the ingest refused — the report describes the RUN, not the value \
         a human then diverged"
    );
    // The DESIGN.md content is orthogonal to the token layer and survives the write.
    let stored = get_design(&mut conn, &project.id)
        .unwrap()
        .expect("design row");
    assert_eq!(stored.content, "# DESIGN.md stays");

    // A DARK edit targets the dark map only.
    let mut dark_set = BTreeMap::new();
    dark_set.insert("--primary".to_string(), "#220011".to_string());
    let layer = edit_project_tokens(&mut conn, &project.id, &dark_set, &[], true).unwrap();
    assert_eq!(
        layer.dark.get("--primary").map(String::as_str),
        Some("#220011")
    );
    assert_eq!(
        layer.variables.get("--primary").map(String::as_str),
        Some("#ff0055"),
        "the light identity is untouched"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// THE FINDING (01KYSCX562SWDXZBBAPX397X8F): the ingest swept the SAME arborescence as the component
/// discovery under DIFFERENT rules — it knew nothing of the recipe's `ignored_dirs` nor of its
/// `exclude`, so a real front got a `.generated` MIRROR of its stylesheet and a VitePress docs theme
/// (`--vp-*`, `--vp-custom-block-*`) merged into the layer that is supposed to be THE reference of
/// its design system. And the response reported only what it took, so the pollution was invisible.
///
/// The tree here is that shape: the app stylesheet, a generated mirror, a docs site (VitePress theme
/// + an ordinary docs sheet) and a path the project's own recipe excludes.
#[test]
fn the_ingest_honours_the_recipe_exclusions_and_reports_what_it_refused() {
    let root = scratch_dir("exclusions");
    // The APPLICATION's own token sheet — the only thing that may enter the layer.
    write_file(&root, "src/styles.css", SHADCN_GLOBALS);
    // A GENERATED mirror of it, under a directory the recipe prunes (`.generated`). Its file name
    // scores exactly like the real one and it sits SHALLOWER, so nothing but the pruning stops it.
    write_file(
        &root,
        ".generated/styles.css",
        ":root { --primary: #dead00; --background: #111; --foreground: #eee; --generated-only: 1 }",
    );
    // The VitePress docs THEME: its variables are its own namespace, nothing the app ever uses.
    write_file(
        &root,
        "docs/.vitepress/theme/custom.css",
        ":root { --vp-c-brand-1: #3451b2; --vp-c-bg: #fff; --vp-custom-block-tip-bg: #e6f6e6 }",
    );
    // An ORDINARY docs stylesheet: a legitimate sheet, just not the app's — it must not enter the
    // layer while the application has one of its own.
    write_file(
        &root,
        "docs/theme.css",
        ":root { --primary: #cc0000; --background: #fafafa; --docs-gap: 8px }",
    );
    // …and a sheet the PROJECT's recipe excludes by hand.
    write_file(
        &root,
        "src/legacy/tokens.css",
        ":root { --primary: #00ff00; --background: #000; --legacy: 1 }",
    );

    let mut conn = open_in_memory();
    let (project, _ws) =
        create_project(&mut conn, "sandbox", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    // The project states its own convention — INCLUDING what is excluded. The token ingest must obey
    // that document, exactly as the component discovery does.
    let mut recipe = crate::design::default_recipe(crate::design::ComponentStack::React);
    recipe.exclude.push("src/legacy/**".to_string());
    crate::design::set_project_recipe(&mut conn, &project.id, Some(&recipe)).unwrap();

    let run = ingest_project_tokens(&mut conn, &project.id).unwrap();

    // ONE source: the application's stylesheet.
    assert_eq!(
        run.sources
            .iter()
            .map(|s| s.path.as_str())
            .collect::<Vec<_>>(),
        vec!["src/styles.css"],
        "sources: {:?}",
        run.sources
    );
    let layer = project_token_layer(&mut conn, &project.id).unwrap();
    assert_eq!(
        layer.variables.get("--primary").map(String::as_str),
        Some("oklch(0.55 0.21 264.4)"),
        "the app's own identity, not the generated mirror's nor the docs site's"
    );
    for polluted in [
        "--vp-c-brand-1",
        "--vp-custom-block-tip-bg",
        "--generated-only",
        "--docs-gap",
        "--legacy",
    ] {
        assert!(
            !layer.variables.contains_key(polluted),
            "{polluted} is not the design system: {:?}",
            layer.variables.keys().collect::<Vec<_>>()
        );
    }

    // WHAT IT REFUSED, and why — the other half of the provenance.
    let reason_of = |path: &str| {
        run.skipped
            .iter()
            .find(|s| s.path == path)
            .map(|s| s.reason)
            .unwrap_or_else(|| panic!("{path} reported in skipped: {:?}", run.skipped))
    };
    assert_eq!(reason_of("src/legacy/tokens.css"), "recipe_exclude");
    assert_eq!(
        reason_of("docs/.vitepress/theme/custom.css"),
        "doc_tool_theme"
    );
    assert_eq!(reason_of("docs/theme.css"), "peripheral_sheet");
    assert_eq!(run.skipped_count, run.skipped.len(), "nothing hidden here");
    // The GENERATED mirror was never even reached: the walk did not descend, and it says which name.
    assert!(
        run.pruned_dirs.contains(&".generated".to_string()),
        "pruned_dirs: {:?}",
        run.pruned_dirs
    );
    assert!(
        !run.skipped
            .iter()
            .any(|s| s.path.starts_with(".generated/")),
        "a pruned directory is not walked at all"
    );

    // …AND THE REPORT IS IN THE BLOB (finding 01KYSK7GS9C3ZRJYW00ZA0H6GF). The `/design` panel may
    // not trigger an ingest, so anything that lives only in the RESPONSE is invisible to it: what
    // was refused has to be re-readable from the stored layer, identical to what the run answered.
    let report = layer
        .provenance
        .as_ref()
        .expect("the stored layer carries the run's report");
    assert_eq!(report.skipped, run.skipped, "the blob echoes the run");
    assert_eq!(report.skipped_count, run.skipped_count);
    assert_eq!(report.pruned_dirs, run.pruned_dirs);
    assert!(report.pruned_dirs.contains(&".generated".to_string()));
    // Read through the ONE key the renderer reads, on the ONE blob it is given.
    let stored = get_design(&mut conn, &project.id)
        .unwrap()
        .and_then(|d| d.theme_tokens_json)
        .expect("a stored layer");
    let blob: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(
        blob["skipped"]
            .as_array()
            .map(|entries| entries.len())
            .unwrap_or(0),
        run.skipped.len()
    );
    assert!(blob["pruned_dirs"]
        .as_array()
        .expect("pruned_dirs in the blob")
        .contains(&Value::String(".generated".to_string())));

    // The recipe is the AUTHORITY, not a hard-coded list: a project that removes `.generated` from
    // its own `ignored_dirs` (because that is where its front really lives) is obeyed — and the sheet
    // then enters the ranking like any other.
    let mut recipe = crate::design::default_recipe(crate::design::ComponentStack::React);
    recipe.ignored_dirs.retain(|dir| dir != ".generated");
    crate::design::set_project_recipe(&mut conn, &project.id, Some(&recipe)).unwrap();
    let run = ingest_project_tokens(&mut conn, &project.id).unwrap();
    assert!(
        !run.pruned_dirs.contains(&".generated".to_string()),
        "the project said so: {:?}",
        run.pruned_dirs
    );
    assert!(
        run.sources
            .iter()
            .any(|s| s.path == ".generated/styles.css"),
        "sources: {:?}",
        run.sources
    );

    std::fs::remove_dir_all(&root).ok();
}

/// A DOCS-ONLY repo still gets a layer: the tier is a RANKING (an application sheet always wins), not
/// a blacklist — a grey preview would be worse than tokens that at least render. The report then says
/// where they came from.
#[test]
fn a_docs_only_project_still_ingests_its_sheets_and_says_so() {
    let root = scratch_dir("docs-only");
    write_file(
        &root,
        "docs/styles/tokens.css",
        ":root { --primary: #cc0000; --background: #fafafa; --foreground: #111 }",
    );
    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Docs", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    let run = ingest_project_tokens(&mut conn, &project.id).unwrap();
    assert_eq!(
        run.sources
            .iter()
            .map(|s| s.path.as_str())
            .collect::<Vec<_>>(),
        vec!["docs/styles/tokens.css"],
        "nothing else exists, so the peripheral sheet is the layer"
    );
    assert!(run.changed && run.variable_count == 3);

    std::fs::remove_dir_all(&root).ok();
}

/// The FOREIGN-NAMESPACE test is on CONTENT, not on the path: the tester's VitePress theme sat in a
/// perfectly ordinary file name, and a docs tool's theme can be dropped anywhere. A sheet that merely
/// OVERRIDES a couple of `--vp-*` alongside its own tokens stays the design system.
#[test]
fn a_sheet_that_is_a_third_party_theme_is_refused_wherever_it_sits() {
    let vitepress = extract_css_variables(
        ":root { --vp-c-brand-1: #1; --vp-c-bg: #2; --vp-layout-max-width: 3; --radius: 4px }",
    );
    assert_eq!(
        foreign_namespace(&vitepress),
        Some("--vp-"),
        "three quarters of it is VitePress' own namespace"
    );
    let app = extract_css_variables(
        ":root { --primary: #1; --background: #2; --foreground: #3; --vp-c-brand-1: #4 }",
    );
    assert_eq!(
        foreign_namespace(&app),
        None,
        "one embed override does not make an app stylesheet somebody else's theme"
    );
    assert_eq!(foreign_namespace(&[]), None);

    // The DIRECTORY tests are exact segment matches, never substrings.
    assert_eq!(
        has_dir_segment("docs/.vitepress/theme/x.css", DOC_TOOL_DIRS).as_deref(),
        Some(".vitepress")
    );
    assert_eq!(has_dir_segment("src/ui/x.css", DOC_TOOL_DIRS), None);
    assert_eq!(
        has_dir_segment("src/documentation-viewer/x.css", PERIPHERAL_DIRS),
        None,
        "a component directory that merely SPELLS «documentation» is the app's"
    );
    assert_eq!(
        has_dir_segment("Docs/theme.css", PERIPHERAL_DIRS).as_deref(),
        Some("Docs"),
        "the match is case-insensitive"
    );
}

/// DONE-CRITERION (finding `01KYSNQWQD1324E0NA0172M66J`): THE KEYS `to_json` EMITS AND THE KEYS THE
/// RENDERER RESERVES ARE ONE LIST, AND THAT LIST IS DATA.
///
/// The correspondence used to be verified BY HAND in two unrelated tests — this file's round-trip on
/// one side, `theme-css.test.ts` on the other — so nothing broke the day a key appeared in `to_json`
/// and not in `RESERVED_KEYS`. The renderer FLATTENS every unreserved top-level key into a CSS
/// custom property, so the symptom would have been a `--pruned-dirs-count` inside `@theme`: no
/// error, just a nonsense variable in the preview's theme.
///
/// The list now lives in `contract/design-fixtures.json`, which BOTH suites read. This half asserts
/// `to_json` emits EXACTLY those keys; the TS half asserts every one of them is reserved. A key
/// added here fails this test until the fixture follows, and the fixture then fails the TS test
/// until `RESERVED_KEYS` follows.
#[test]
fn to_json_emits_exactly_the_keys_the_renderer_reserves() {
    let fixture: Value = serde_json::from_str(crate::mcp_tools_design::DESIGN_FIXTURES)
        .expect("contract/design-fixtures.json is valid JSON");
    let expected: Vec<String> = fixture["token_layer_keys"]["keys"]
        .as_array()
        .expect("the fixture lists the token-layer keys")
        .iter()
        .map(|k| k.as_str().unwrap_or_default().to_string())
        .collect();

    // A layer carrying EVERYTHING the shape can carry — the report included, since its keys are
    // omitted (never emitted empty) when there is none.
    let mut layer = TokenLayer {
        sources: vec![TokenSource {
            repository_id: None,
            repo: "web".into(),
            path: "src/styles.css".into(),
            variable_count: 3,
        }],
        ..TokenLayer::default()
    };
    layer.variables.insert("--primary".into(), "#0f172a".into());
    layer.dark.insert("--primary".into(), "#e2e8f0".into());
    layer.provenance = Some(TokenProvenance::bounded(
        vec![SkippedSheet {
            repo: "web".into(),
            path: "docs/theme.css".into(),
            reason: "doc_tool_theme",
        }],
        1,
        vec![".generated".into()],
        1,
    ));

    let value: Value = serde_json::from_str(&layer.to_json()).expect("to_json is valid JSON");
    let mut emitted: Vec<String> = value
        .as_object()
        .expect("the layer is a JSON object")
        .keys()
        .cloned()
        .collect();
    emitted.sort();
    let mut expected_sorted = expected.clone();
    expected_sorted.sort();
    assert_eq!(
        emitted, expected_sorted,
        "every key `to_json` emits must be listed in contract/design-fixtures.json (and reserved by \
         theme-css.ts), and nothing else"
    );
}

/// A self-reference is one whatever CSS spelling carries it: the function name is case-insensitive,
/// `!important` is not part of the value, and an EMPTY fallback (`var(--x,)`, Tailwind's "empty
/// unless set") still resolves to nothing. Only a REAL fallback makes it a value.
#[test]
fn a_self_reference_is_caught_in_every_css_spelling() {
    let is = |value: &str| super::is_self_reference("--radius-md", value);
    assert!(is("var(--radius-md)"), "canonical");
    assert!(is("var( --radius-md )"), "inner whitespace");
    assert!(is("VAR(--radius-md)"), "function names are case-insensitive in CSS");
    assert!(is("var(--radius-md,)"), "an empty fallback still resolves to nothing");
    assert!(is("var(--radius-md, )"), "an empty fallback, spaced");
    assert!(is("var(--radius-md) !important"), "!important is not part of the value");
    assert!(!is("var(--radius-md, 8px)"), "a REAL fallback is a value");
    assert!(!is("var(--radius)"), "another name is a real alias");
    assert!(!is("calc(var(--radius-md) - 2px)"), "a use inside an expression is a value");
    assert!(!is("var"), "too short to be a var() at all");
}
