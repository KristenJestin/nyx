//! Unit + integration tests for the DESIGN-SYSTEM machinery ([`super`]).
//!
//! The PURE half (the glob matcher, the stack defaults + override precedence, the non-component
//! filter, the `templateUrl` resolution, the bundle base names) is tested without disk/DB; the
//! BUNDLE discovery + the incremental synthesis loop are exercised over a real scratch tree + an
//! in-memory DB, incl. the GREENFIELD (no front) graceful-empty case.

use super::*;
use crate::db::{
    create_project, create_repository, get_component_by_name, list_components, open_in_memory,
    set_design_inventory_globs, update_component_content,
};
use uuid::Uuid;

/// A unique scratch dir under the OS temp dir (mirrors the repository/db test helpers; no
/// tempfile dev-dep — uniqueness from a v7 UUID so parallel threads never collide).
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("nyx-design-{tag}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Write `content` to `dir/rel`, creating parent directories. `rel` uses `/` separators.
fn write_file(dir: &std::path::Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir parent");
    }
    std::fs::write(path, content).expect("write file");
}

/// The bundle named `name` among `bundles` (panics when absent — a readable test failure).
fn bundle<'a>(bundles: &'a [ComponentBundle], name: &str) -> &'a ComponentBundle {
    bundles
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("bundle {name} discovered"))
}

/// The paths of a bundle's files, path-ordered.
fn paths(b: &ComponentBundle) -> Vec<&str> {
    b.files.iter().map(|f| f.path.as_str()).collect()
}

// --- glob matcher (pure) ---------------------------------------------------

#[test]
fn glob_double_star_spans_directories_and_zero_dirs() {
    assert!(glob_match("**/*.tsx", "src/ui/Button.tsx"));
    assert!(glob_match("**/*.tsx", "src/Button.tsx"));
    // `**/` also matches ZERO directories → a root-level file matches.
    assert!(glob_match("**/*.tsx", "Button.tsx"));
    // A logic sibling is NOT a `.tsx`.
    assert!(!glob_match("**/*.tsx", "src/Button.ts"));
    assert!(!glob_match("**/*.tsx", "src/Button.css"));
}

/// REGRESSION (review — catastrophic backtracking): the matcher is a DP, not a backtracking
/// recursion. What this test pins is the COST, and only the cost: the measured pathological family
/// (`("**a" × n) + "zzz"` against `"a"×40 + "/b.tsx"`: 0,12 ms at n=3 → 8,1 s at n=9, ~×20 per extra
/// `**`) is now instant, so an agent-supplied, PERSISTED `globs` override can no longer freeze every
/// later inventory.
///
/// The DP rewrite was ISO-SEMANTIC — it corrected NO result (a differential of 496 860 pattern/path
/// pairs against the recursive matcher it replaced found ZERO divergence; the four nominal cases
/// below already passed on the old code). They are kept as NOMINAL cases, not as a regression: a
/// `**` yielding ground to a following segment-bounded `*` is the behaviour, whichever engine runs.
#[test]
fn glob_matching_is_bounded_and_never_backtracks_catastrophically() {
    // NOMINAL (not a fixed regression): a `**` yields to a following segment-bounded `*`.
    assert!(glob_match("**/*", "a/b/c"));
    assert!(glob_match("**/*.ts", "a/b/c.ts"));
    assert!(glob_match("src/**/*.tsx", "src/a/b/Card.tsx"));
    assert!(!glob_match("src/**/*.tsx", "lib/a/Card.tsx"));

    let path = format!("{}/b.tsx", "a".repeat(40));
    let started = std::time::Instant::now();
    for stars in 3..=12 {
        let pattern = format!("{}zzz", "**a".repeat(stars));
        assert!(!glob_match(&pattern, &path), "{stars} `**` must not match");
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "up to 12 `**` must stay instant (was 8.1 s at 9), took {elapsed:?}"
    );
}

/// DONE-CRITERION (review — non-standard, undocumented glob semantics): `**/` is ANCHORED to
/// segment boundaries and `?` consumes a whole CHARACTER, the semantics the doc-comment now states
/// and the one an agent writing a persisted `globs` override expects.
#[test]
fn glob_double_star_is_segment_anchored_and_question_mark_is_one_character() {
    // `**/` spans WHOLE directories — it can no longer eat a piece of a segment.
    assert!(!glob_match("**/Button.tsx", "MyButton.tsx"));
    assert!(glob_match("**/Button.tsx", "Button.tsx"));
    assert!(glob_match("**/Button.tsx", "src/ui/Button.tsx"));
    assert!(!glob_match("**/ui/**/*.tsx", "a/myui/x/b.tsx"));
    assert!(glob_match("**/ui/**/*.tsx", "a/ui/x/b.tsx"));
    assert!(!glob_match("**/components/**/*.ts", "src/xcomponents/a.ts"));
    assert!(glob_match("**/components/**/*.ts", "src/components/a.ts"));

    // A glob is only ever matched against a FILE path, so a trailing `**` requires the `/`: the
    // bare directory `src` is never a candidate.
    assert!(!glob_match("src/**", "src"));
    assert!(glob_match("src/**", "src/a.ts"));
    assert!(glob_match("src/**", "src/a/b/c.ts"));

    // `?` = exactly ONE code point (it used to be one BYTE, so `é` needed two `?`).
    assert!(glob_match("?", "é"));
    assert!(!glob_match("??", "é"));
    assert!(glob_match("Butt?n.tsx", "Button.tsx"));
    // Non-ASCII input never panics, whatever the pattern.
    assert!(!glob_match("?", "日本"));
    assert!(glob_match("??", "日本"));
    assert!(glob_match("*/?", "a/日"), "one `?` covers a 3-byte char");
    assert!(!glob_match("*/??", "a/日"));

    // The product's own defaults are unaffected by the anchoring.
    for stack in [
        ComponentStack::React,
        ComponentStack::Angular,
        ComponentStack::Vue,
        ComponentStack::Generic,
    ] {
        let globs = default_component_globs(stack);
        assert!(
            globs.iter().any(|g| glob_match(g, "src/app/nav/nav.ts"))
                || !matches!(stack, ComponentStack::Angular | ComponentStack::Generic),
            "the Angular net still takes a nested .ts"
        );
    }
    assert!(glob_match("**/*.tsx", "Button.tsx"));
    assert!(glob_match("**/*.tsx", "src/ui/Button.tsx"));
    assert!(glob_match(
        "**/*.component.html",
        "src/app/nav/nav.component.html"
    ));
}

#[test]
fn glob_single_star_does_not_cross_slash() {
    assert!(glob_match("src/*.tsx", "src/Button.tsx"));
    // `*` cannot cross a `/`, so a nested file does not match a single-segment star.
    assert!(!glob_match("src/*.tsx", "src/ui/Card.tsx"));
    // `?` is exactly one non-slash char.
    assert!(glob_match("Butto?.tsx", "Button.tsx"));
    assert!(!glob_match("Butto?.tsx", "Butto/.tsx"));
    // A trailing `*` matches within a segment but not across `/`.
    assert!(glob_match("src/B*", "src/Button.tsx"));
    assert!(!glob_match("src/B*", "src/ui/Button.tsx"));
}

// --- stack defaults + override precedence (pure) ---------------------------

#[test]
fn react_is_the_default_stack_and_globs() {
    assert_eq!(ComponentStack::default(), ComponentStack::React);
    let globs = default_component_globs(ComponentStack::React);
    assert!(globs.contains(&"**/*.tsx".to_string()));
    assert!(globs.contains(&"**/*.jsx".to_string()));
    assert_eq!(
        default_component_globs(ComponentStack::Vue),
        vec!["**/*.vue"]
    );
    // review #40: Angular's ENTRY is the CLASS, recognized by CONTENT (`@Component`) rather than by
    // name — the modern style guide names it `button.ts`, not `button.component.ts`, so the net
    // takes every `.ts` (a template-only legacy component still opens on its `.component.html`).
    assert_eq!(
        default_component_globs(ComponentStack::Angular),
        vec!["**/*.ts", "**/*.component.html"]
    );
}

#[test]
fn project_override_wins_but_empty_falls_back_to_stack_default() {
    // A non-empty per-project override REPLACES the stack default (surchargeable par projet).
    let over = vec!["src/components/*.tsx".to_string()];
    assert_eq!(
        resolve_component_globs(ComponentStack::React, Some(&over)),
        over
    );
    // No override, or an EMPTY override, falls back to the stack default (never inventory nothing).
    assert_eq!(
        resolve_component_globs(ComponentStack::React, None),
        default_component_globs(ComponentStack::React)
    );
    assert_eq!(
        resolve_component_globs(ComponentStack::React, Some(&[])),
        default_component_globs(ComponentStack::React)
    );
}

#[test]
fn component_name_strips_extension_and_family_qualifiers() {
    assert_eq!(
        component_name_from_file("Button.tsx").as_deref(),
        Some("Button")
    );
    assert_eq!(
        component_name_from_file("Button.module.css").as_deref(),
        Some("Button")
    );
    assert_eq!(
        component_name_from_file("Nav.component.html").as_deref(),
        Some("Nav")
    );
    // review #40: every family member of a BUNDLE resolves to the SAME base name, so the
    // `.variants.ts` / `.styles.ts` sidecars group WITH their component instead of beside it.
    assert_eq!(
        component_name_from_file("button.variants.ts").as_deref(),
        Some("button")
    );
    assert_eq!(
        component_name_from_file("button.component.scss").as_deref(),
        Some("button")
    );
    assert_eq!(component_name_from_file("").as_deref(), None);
}

/// DONE-CRITERION (review #40): `.service.ts`, `.types.ts`, `.variants.ts` and the spec/story
/// sidecars are NOT components — they never open a catalogue entry (they may still BELONG to a
/// bundle, see the discovery tests).
#[test]
fn non_components_never_open_a_catalogue_entry() {
    assert!(!is_component_entry("user.service.ts"));
    assert!(!is_component_entry("billing.types.ts"));
    assert!(!is_component_entry("button.variants.ts"));
    assert!(!is_component_entry("app.module.ts"));
    assert!(!is_component_entry("auth.guard.ts"));
    assert!(!is_component_entry("Button.test.tsx"));
    assert!(!is_component_entry("Button.spec.tsx"));
    assert!(!is_component_entry("Button.stories.tsx"));
    assert!(!is_component_entry("types.d.ts"));
    // The real entries pass.
    assert!(is_component_entry("Button.tsx"));
    assert!(is_component_entry("button.component.ts"));
    assert!(is_component_entry("button.component.html"));
    // The legacy test/story predicate still holds (it is one half of the filter).
    assert!(is_excluded_source_file("Button.test.tsx"));
    assert!(!is_excluded_source_file("Button.tsx"));
    assert_eq!(
        source_qualifier("button.variants.ts").as_deref(),
        Some("variants")
    );
    assert_eq!(source_qualifier("Button.tsx"), None);
}

/// DONE-CRITERION (review #40): the Angular `templateUrl` is READ from the class source (pure, no
/// TS parser) and RESOLVED against the entry's directory — including a `../` hop — and can never
/// escape the repo root.
#[test]
fn template_url_is_read_and_resolved_relative_to_the_entry() {
    assert_eq!(
        template_url_of("@Component({ selector: 'x', templateUrl: './x.component.html' })")
            .as_deref(),
        Some("./x.component.html")
    );
    assert_eq!(
        template_url_of("@Component({\n  templateUrl : \"../shared/tpl.html\",\n})").as_deref(),
        Some("../shared/tpl.html")
    );
    assert_eq!(template_url_of("@Component({ template: `<b></b>` })"), None);

    assert_eq!(
        join_relative("src/app/button", "./button.component.html").as_deref(),
        Some("src/app/button/button.component.html")
    );
    assert_eq!(
        join_relative("src/app/button", "../shared/tpl.html").as_deref(),
        Some("src/app/shared/tpl.html")
    );
    // Repo-root-relative, and the escape is refused (never reach outside the scanned repo).
    assert_eq!(
        join_relative("src/app", "/src/tpl.html").as_deref(),
        Some("src/tpl.html")
    );
    assert_eq!(join_relative("src", "../../outside.html"), None);
}

// --- stack detection (review #32) ------------------------------------------

/// DONE-CRITERION (review #32): the stack is DETECTED from `package.json` deps — `@angular/core`
/// ⇒ Angular, `react` ⇒ React, `vue` ⇒ Vue — the pure core (no disk). An unrecognized / unparsable
/// manifest ⇒ `None` (the caller falls back to Generic).
#[test]
fn stack_from_package_json_reads_the_framework_dep() {
    assert_eq!(
        stack_from_package_json(r#"{"dependencies":{"@angular/core":"^17.0.0"}}"#),
        Some(ComponentStack::Angular)
    );
    assert_eq!(
        stack_from_package_json(r#"{"dependencies":{"react":"^18.0.0"}}"#),
        Some(ComponentStack::React)
    );
    // A dev-dependency counts too (Vue), and Angular WINS when both are present (priority order).
    assert_eq!(
        stack_from_package_json(r#"{"devDependencies":{"vue":"^3.0.0"}}"#),
        Some(ComponentStack::Vue)
    );
    assert_eq!(
        stack_from_package_json(r#"{"dependencies":{"@angular/core":"1","react":"1"}}"#),
        Some(ComponentStack::Angular),
        "Angular takes priority over a co-installed react"
    );
    // No recognized framework, or garbage → None (caller → Generic).
    assert_eq!(
        stack_from_package_json(r#"{"dependencies":{"lodash":"^4"}}"#),
        None
    );
    assert_eq!(stack_from_package_json("not json"), None);
}

/// DONE-CRITERION (review #32): `detect_stack` reads the on-disk repo — an `angular.json` at the
/// root is an Angular workspace (even before package.json is read); a React `package.json` ⇒ React;
/// a repo with NEITHER signal ⇒ Generic (a broad net, never nothing). GREENFIELD-safe (no manifest).
#[test]
fn detect_stack_reads_angular_json_then_package_json_then_generic() {
    let root = scratch_dir("detect");

    // angular.json alone ⇒ Angular (the strongest signal).
    write_file(&root, "angular.json", r#"{"version":1}"#);
    assert_eq!(detect_stack(&root), ComponentStack::Angular);
    let _ = std::fs::remove_dir_all(&root);

    // package.json with react ⇒ React.
    let react = scratch_dir("detect-react");
    write_file(
        &react,
        "package.json",
        r#"{"dependencies":{"react":"^18"}}"#,
    );
    assert_eq!(detect_stack(&react), ComponentStack::React);
    let _ = std::fs::remove_dir_all(&react);

    // No manifest at all (greenfield) ⇒ Generic, no error.
    let bare = scratch_dir("detect-bare");
    assert_eq!(detect_stack(&bare), ComponentStack::Generic);
    let _ = std::fs::remove_dir_all(&bare);
}

/// DONE-CRITERION (review #32): `.generated` (+ `.angular`) are pruned from the inventory walk, so
/// a generated mirror of the source never floods the catalogue.
#[test]
fn generated_and_angular_dirs_are_pruned() {
    assert!(IGNORED_DIRS.contains(&".generated"));
    assert!(IGNORED_DIRS.contains(&".angular"));

    let root = scratch_dir("pruned");
    write_file(&root, "src/Button.tsx", "<button/>");
    write_file(&root, ".generated/Copy.tsx", "<button/>"); // generated mirror → pruned
    write_file(&root, ".angular/cache/Cached.tsx", "<div/>"); // CLI cache → pruned

    let globs = default_component_globs(ComponentStack::React);
    let found = discover_bundles(&root, &globs);
    let names: Vec<&str> = found.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Button"],
        ".generated and .angular are never descended into"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// --- BUNDLE discovery (review #40) -----------------------------------------

/// DONE-CRITERION (review #40): on an ANGULAR tree the unit is the BUNDLE — the CLASS is the entry
/// (recognized by its `@Component` decorator, so BOTH the legacy `button.component.ts` and the
/// modern `chip.ts` layouts work), the template is RESOLVED from `templateUrl` (even when it does
/// not sit beside the class), and the sibling `.variants.ts` / styles JOIN the bundle. The
/// non-components (`.service.ts`, `.types.ts`, `.variants.ts`, spec, plain utils) create NO entry.
#[test]
fn angular_bundle_groups_class_template_variants_and_styles() {
    let root = scratch_dir("bundle-angular");
    write_file(&root, "angular.json", r#"{"version":1}"#);
    write_file(
        &root,
        "src/app/button/button.component.ts",
        r#"
        import { Component, input } from '@angular/core';
        @Component({ selector: 'pb-button', templateUrl: './button.component.html' })
        export class ButtonComponent { label = input<string>(''); }
        "#,
    );
    write_file(
        &root,
        "src/app/button/button.component.html",
        "<button class=\"btn\">{{ label() }}</button>",
    );
    write_file(&root, "src/app/button/button.component.scss", ".btn { }");
    write_file(
        &root,
        "src/app/button/button.variants.ts",
        "export const buttonVariants = cva('btn');",
    );
    write_file(
        &root,
        "src/app/button/button.spec.ts",
        "describe('x', () => {});",
    );
    // Non-components living in the same subsystem — catalogued by the OLD regex pass, dropped now.
    write_file(
        &root,
        "src/app/core/user.service.ts",
        "export class UserService {}",
    );
    write_file(&root, "src/app/core/billing.types.ts", "export type X = 1;");
    // A plain util `.ts` (no `@Component`) — matched by the broad Angular net, dropped by CONTENT.
    write_file(
        &root,
        "src/app/core/format.ts",
        "export const format = (n: number) => `${n}`;",
    );
    // A component whose template lives OUTSIDE its own folder (the `templateUrl` hop).
    write_file(
        &root,
        "src/app/nav/nav.component.ts",
        "@Component({ templateUrl: '../shared/nav.html' })\nexport class NavComponent {}",
    );
    write_file(&root, "src/app/shared/nav.html", "<nav></nav>");
    // The MODERN Angular layout (v20+ style guide — what the reference front uses): `chip.ts` + `chip.html`,
    // NO `.component` in the names. A name-only net would miss it entirely.
    write_file(
        &root,
        "src/app/chip/chip.ts",
        "@Component({ selector: 'pb-chip', templateUrl: './chip.html' })\nexport class ChipComponent {}",
    );
    write_file(
        &root,
        "src/app/chip/chip.html",
        "<span class=\"chip\"></span>",
    );
    write_file(
        &root,
        "src/app/chip/chip.variants.ts",
        "export const chipVariants = cva('chip');",
    );

    let globs = default_component_globs(ComponentStack::Angular);
    let bundles = discover_bundles(&root, &globs);
    let names: Vec<&str> = bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["button", "chip", "nav"],
        "one entry per COMPONENT — the service/types/variants/spec/util files open none"
    );

    // The MODERN layout forms the SAME bundle shape (entry + template + variants).
    let chip = bundle(&bundles, "chip");
    assert_eq!(chip.entry_path, "src/app/chip/chip.ts");
    assert_eq!(
        paths(chip),
        vec![
            "src/app/chip/chip.html",
            "src/app/chip/chip.ts",
            "src/app/chip/chip.variants.ts",
        ],
        "modern Angular naming (button.ts + button.html) bundles exactly like the legacy one"
    );

    let button = bundle(&bundles, "button");
    assert_eq!(button.entry_path, "src/app/button/button.component.ts");
    assert_eq!(button.root_path, "src/app/button");
    assert_eq!(
        paths(button),
        vec![
            "src/app/button/button.component.html",
            "src/app/button/button.component.scss",
            "src/app/button/button.component.ts",
            "src/app/button/button.variants.ts",
        ],
        "the BUNDLE is the class + the resolved template + the styles + the variants"
    );
    let role = |p: &str| {
        button
            .files
            .iter()
            .find(|f| f.path.ends_with(p))
            .map(|f| f.role)
            .unwrap()
    };
    assert_eq!(role("button.component.ts"), BundleRole::Entry);
    assert_eq!(role("button.component.html"), BundleRole::Template);
    assert_eq!(role("button.component.scss"), BundleRole::Style);
    assert_eq!(role("button.variants.ts"), BundleRole::Variants);

    // The `templateUrl` hop resolved a template OUTSIDE the component's own directory.
    let nav = bundle(&bundles, "nav");
    assert_eq!(
        paths(nav),
        vec!["src/app/nav/nav.component.ts", "src/app/shared/nav.html"],
        "templateUrl resolved across directories"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review #40): a REACT bundle is the `.tsx` + its css / `.variants.*` siblings; a
/// plain logic `.ts` sibling is NOT part of it (editing logic must not trigger a re-synthesis).
#[test]
fn react_bundle_groups_tsx_with_styles_and_variants_but_not_plain_logic() {
    let root = scratch_dir("bundle-react");
    write_file(&root, "package.json", r#"{"dependencies":{"react":"^18"}}"#);
    write_file(
        &root,
        "src/ui/Button.tsx",
        "export const Button = () => <button/>;",
    );
    write_file(&root, "src/ui/Button.module.css", ".btn {}");
    write_file(
        &root,
        "src/ui/Button.variants.ts",
        "export const v = cva('btn');",
    );
    write_file(&root, "src/ui/Button.ts", "export const onClick = () => 1;");
    write_file(&root, "src/ui/Button.test.tsx", "test('x', () => {});");
    write_file(
        &root,
        "src/ui/useButton.ts",
        "export const useButton = () => {};",
    );

    let globs = default_component_globs(ComponentStack::React);
    let bundles = discover_bundles(&root, &globs);
    let names: Vec<&str> = bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, vec!["Button"], "the test file opens no entry");

    let button = bundle(&bundles, "Button");
    assert_eq!(
        paths(button),
        vec![
            "src/ui/Button.module.css",
            "src/ui/Button.tsx",
            "src/ui/Button.variants.ts",
        ],
        "the plain logic `Button.ts` is NOT a bundle member (no rendering in it)"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review #40): the incremental key is the hash of the bundle's INPUTS — it is
/// STABLE when nothing changes, FLIPS when any bundle file changes (markup, style, variants, or
/// the class itself) or when a file is ADDED, and is INERT to a non-member logic edit.
#[test]
fn input_hash_covers_every_bundle_file_and_ignores_non_members() {
    let root = scratch_dir("bundle-hash");
    write_file(&root, "src/Button.tsx", "<button>{{label}}</button>");
    write_file(&root, "src/Button.module.css", ".btn { color: red; }");
    write_file(&root, "src/Button.ts", "export const onClick = () => {};");

    let hash =
        |root: &std::path::Path| bundle_for_entry(root, "src/Button.tsx").unwrap().input_hash;

    let h0 = hash(&root);
    assert_eq!(hash(&root), h0, "the hash is stable across runs");

    // A NON-member logic edit leaves the hash untouched → no re-synthesis for nothing.
    write_file(
        &root,
        "src/Button.ts",
        "export const onClick = () => elsewhere();",
    );
    assert_eq!(
        hash(&root),
        h0,
        "a plain `.ts` logic edit is not a bundle input"
    );

    // A STYLE edit flips it; a MARKUP edit flips it again.
    write_file(&root, "src/Button.module.css", ".btn { color: blue; }");
    let h1 = hash(&root);
    assert_ne!(h1, h0, "a style edit changes the bundle inputs");
    write_file(
        &root,
        "src/Button.tsx",
        "<button class=\"btn\">{{label}}</button>",
    );
    let h2 = hash(&root);
    assert_ne!(h2, h1, "a markup edit changes the bundle inputs");

    // ADDING a bundle file (a variants sidecar) flips it too (the names are folded in).
    write_file(
        &root,
        "src/Button.variants.ts",
        "export const v = cva('btn');",
    );
    assert_ne!(hash(&root), h2, "adding a bundle file changes the inputs");

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review — a CONSTANT-SIZE edit past the read bound was invisible): the input hash
/// folds in EVERY byte of every bundle file, so an edit at ANY offset — including one that keeps the
/// file's exact size, megabytes in — flips it and puts the bundle back on the work list.
///
/// This INVERTS what the previous test pinned. The hash used to read only the first 32 KiB of each
/// file plus its length, which made `class="a"` → `class="b"` in a big `.html`/`.css` template
/// undetectable: the component stayed `up_to_date` forever, silently breaking "a re-synthesis is
/// proposed when and ONLY when the bundle's source changed". The bound is now on the MEMORY (the
/// hash STREAMS), not on the content.
#[test]
fn bundle_input_hash_folds_every_byte_so_a_constant_size_deep_edit_is_detected() {
    let root = scratch_dir("hash-streaming");
    // Well past any read window (the old bound was 32 KiB; the streaming window is 64 KiB).
    let huge = "x".repeat(300 * 1024);
    let file = root.join("src").join("Huge.tsx");
    write_file(&root, "src/Huge.tsx", &huge);

    let hash = |root: &std::path::Path| bundle_for_entry(root, "src/Huge.tsx").unwrap().input_hash;
    let h0 = hash(&root);
    assert_eq!(hash(&root), h0, "the hash is stable across runs");

    // ONE byte, at the very END, with the file's size UNCHANGED — the exact blind spot.
    let mut edited = huge.clone().into_bytes();
    let last = edited.len() - 1;
    edited[last] = b'y';
    std::fs::write(&file, &edited).unwrap();
    let deep = hash(&root);
    assert_ne!(
        deep, h0,
        "a constant-size edit past the read window MUST flip the hash"
    );

    // A byte in the MIDDLE, size still unchanged, is seen too.
    edited[last] = b'x';
    edited[150 * 1024] = b'z';
    std::fs::write(&file, &edited).unwrap();
    assert_ne!(hash(&root), h0, "a constant-size edit mid-file is detected");
    assert_ne!(hash(&root), deep, "…and is distinct from the tail edit");

    // A resize flips it as well, and an edit inside the first window obviously does.
    edited.push(b'w');
    std::fs::write(&file, &edited).unwrap();
    let resized = hash(&root);
    assert_ne!(resized, h0);
    write_file(&root, "src/Huge.tsx", &format!("y{huge}"));
    assert_ne!(hash(&root), h0, "an edit at the head is detected");

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review — the bundle hash must recognize a MOVE): the input hash is
/// LOCATION-INDEPENDENT. Two byte-identical bundles in different directories hash the SAME, which is
/// what lets [`super::reconcile_moved_sources`] re-anchor a moved component by CONTENT instead of by
/// name — while ADDING / REMOVING / RENAMING a bundle FILE still flips it.
#[test]
fn bundle_input_hash_is_location_independent_but_file_name_sensitive() {
    let root = scratch_dir("hash-location");
    write_file(&root, "a/x/Card.tsx", "<div class=\"card\"/>");
    write_file(&root, "a/x/Card.module.css", ".card {}");
    write_file(&root, "b/y/z/Card.tsx", "<div class=\"card\"/>");
    write_file(&root, "b/y/z/Card.module.css", ".card {}");

    let hash = |entry: &str| bundle_for_entry(&root, entry).unwrap().input_hash;
    assert_eq!(
        hash("a/x/Card.tsx"),
        hash("b/y/z/Card.tsx"),
        "the same bundle at another path is the SAME content"
    );

    // A differing CONTENT still separates them…
    write_file(&root, "b/y/z/Card.module.css", ".card { color: red; }");
    assert_ne!(hash("a/x/Card.tsx"), hash("b/y/z/Card.tsx"));

    // …and so does a differing FILE SET (drop the style sibling).
    let before = hash("a/x/Card.tsx");
    std::fs::remove_file(root.join("a/x/Card.module.css")).unwrap();
    assert_ne!(
        hash("a/x/Card.tsx"),
        before,
        "removing a bundle file changes the inputs"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The streaming [`Md5`] agrees with the one-shot [`md5_hex`] whatever the CHUNKING — the property
/// the bundle hash relies on when it folds a file in 64 KiB windows.
#[test]
fn streaming_md5_matches_the_one_shot_digest_for_any_chunking() {
    let data: Vec<u8> = (0..(200 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let expected = md5_hex(&data);
    for chunk in [1usize, 7, 63, 64, 65, 1024, 64 * 1024] {
        let mut hasher = Md5::new();
        for part in data.chunks(chunk) {
            hasher.update(part);
        }
        assert_eq!(
            hasher.finish(),
            expected,
            "chunking by {chunk} must not change the digest"
        );
    }
    // The block-boundary lengths (55/56/57/63/64/65 bytes) exercise the padding branches.
    for len in [0usize, 55, 56, 57, 63, 64, 65, 119, 120] {
        let mut hasher = Md5::new();
        for part in data[..len].chunks(3) {
            hasher.update(part);
        }
        assert_eq!(hasher.finish(), md5_hex(&data[..len]), "length {len}");
    }
}

// --- CATALOGUE identity: one row per BUNDLE, one unambiguous name ----------

/// The pure NAME candidates: the bare base name first (readability), then increasingly many
/// directory segments — dropping the directory that merely REPEATS the component name — and the
/// repo label as the last resort.
#[test]
fn name_candidates_qualify_from_the_shortest_distinguishing_directory() {
    assert_eq!(
        name_candidates("", "src/app/catalog/list/data-table/data-table.ts", "data-table"),
        vec![
            "data-table",
            "list/data-table",
            "catalog/list/data-table",
            "app/catalog/list/data-table",
            "src/app/catalog/list/data-table",
        ],
        "the `data-table/` folder repeats the name and adds nothing"
    );
    // The repo LABEL opens one more level — two repos can expose the same relative path.
    assert_eq!(
        name_candidates("web", "src/Button.tsx", "Button"),
        vec!["Button", "src/Button", "web/src/Button"]
    );
    // A root-level entry has no directory to qualify with.
    assert_eq!(name_candidates("", "Button.tsx", "Button"), vec!["Button"]);
}

/// DONE-CRITERION (review — bundle name collision): two bundles sharing a BASE NAME in different
/// directories produce TWO DISTINCT catalogue entries — neither overwrites the other, each keeps
/// its OWN source and its OWN hash — and, once both are synthesized, a run WITHOUT any disk change
/// leaves `pending` EMPTY (the loser used to be re-proposed `source_changed` at every run, forever).
#[test]
fn homonymous_bundles_in_different_directories_are_two_components_and_stop_pending() {
    let root = scratch_dir("homonyms");
    write_file(&root, "angular.json", r#"{"version":1}"#);
    // A real-world collision: two distinct `data-table` components.
    write_file(
        &root,
        "src/app/billing/invoice-detail/data-table/data-table.ts",
        "@Component({ templateUrl: './data-table.html' })\nexport class BillingOtsTable {}",
    );
    write_file(
        &root,
        "src/app/billing/invoice-detail/data-table/data-table.html",
        "<table class=\"billing\"></table>",
    );
    write_file(
        &root,
        "src/app/catalog/list/data-table/data-table.ts",
        "@Component({ templateUrl: './data-table.html' })\nexport class ListOtsTable {}",
    );
    write_file(
        &root,
        "src/app/catalog/list/data-table/data-table.html",
        "<table class=\"list\"></table>",
    );

    let mut conn = open_in_memory();
    let (project, _ws) =
        create_project(&mut conn, "sandbox", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    let first = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(first.discovered, 2, "two bundles on disk");
    assert_eq!(
        first.component_count, first.discovered,
        "component_count no longer diverges from discovered (nothing was overwritten)"
    );
    let names: Vec<&str> = first.bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["list/data-table", "data-table"],
        "the second claimant is qualified by the shortest distinguishing directory"
    );
    // Each catalogue row points at its OWN source (neither clobbered the other).
    let mut entry_of = |name: &str| {
        get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap()
            .source_path
            .unwrap()
    };
    assert_eq!(
        entry_of("data-table"),
        "src/app/billing/invoice-detail/data-table/data-table.ts"
    );
    assert_eq!(
        entry_of("list/data-table"),
        "src/app/catalog/list/data-table/data-table.ts"
    );

    // The AGENT synthesizes BOTH…
    for name in ["data-table", "list/data-table"] {
        let c = get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap();
        update_component_content(&mut conn, &c.id, "<table>{{rows}}</table>", None, None).unwrap();
        super::mark_synthesized(&mut conn, &project.id, name)
            .unwrap()
            .expect("hash recorded");
    }
    // …and NOTHING is pending on a run without a single disk change — twice over, to prove the
    // perpetual `source_changed` loop is gone.
    for run in 0..2 {
        let again = super::inventory_project(&mut conn, &project.id).unwrap();
        assert_eq!(again.component_count, 2, "no duplicate on run {run}");
        assert!(
            again.pending.is_empty(),
            "run {run}: no bundle may stay pending without a source change, got {:?}",
            again.pending
        );
    }

    // A real edit on ONE of them re-proposes EXACTLY that one.
    write_file(
        &root,
        "src/app/catalog/list/data-table/data-table.html",
        "<table class=\"list compact\"></table>",
    );
    let changed = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        changed
            .pending
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["list/data-table"],
        "only the edited homonym is re-proposed"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review — bundle name collision, 2nd manifestation): a component resolves its
/// bundle under the repo its PROVENANCE records, not under whichever repo exposes the same relative
/// path first. Two repos both holding `src/Button.tsx` must read/hash their OWN file.
#[test]
fn a_component_resolves_its_bundle_under_its_own_provenance_repo() {
    let base = scratch_dir("two-repos");
    write_file(
        &base,
        "alpha/src/Button.tsx",
        "export const A = () => <b>ALPHA</b>;",
    );
    write_file(
        &base,
        "beta/src/Button.tsx",
        "export const B = () => <b>BETA</b>;",
    );

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Duo", base.to_str().unwrap(), None).unwrap();
    let alpha = create_repository(&mut conn, &project.id, "alpha", "alpha", false).unwrap();
    let beta = create_repository(&mut conn, &project.id, "beta", "beta", false).unwrap();

    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(out.discovered, 2);
    assert_eq!(out.component_count, 2, "one row per repo's component");
    let names: Vec<&str> = out.bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Button", "src/Button"],
        "the same relative path in two repos yields two distinct names"
    );
    // The provenance repo of each row is its OWN.
    let mut repo_of = |name: &str| {
        get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap()
            .source_repository_id
            .unwrap()
    };
    assert_eq!(repo_of("Button"), alpha.id);
    assert_eq!(repo_of("src/Button"), beta.id);

    // The READ follows the PROVENANCE: `src/Button` must hand back BETA's file, even though ALPHA
    // exposes the very same relative path and is scanned first.
    let read = super::read_bundle(&mut conn, &project.id, "src/Button", 64 * 1024)
        .unwrap()
        .expect("resolved");
    assert!(
        read.files[0].content.contains("BETA"),
        "the wrong repo's file was read: {}",
        read.files[0].content
    );
    let alpha_read = super::read_bundle(&mut conn, &project.id, "Button", 64 * 1024)
        .unwrap()
        .expect("resolved");
    assert!(alpha_read.files[0].content.contains("ALPHA"));

    // …and so does the HASH: synthesizing both, then editing only BETA's file, re-proposes only it.
    for name in ["Button", "src/Button"] {
        let c = get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap();
        update_component_content(&mut conn, &c.id, "<b>{{label}}</b>", None, None).unwrap();
        super::mark_synthesized(&mut conn, &project.id, name)
            .unwrap()
            .expect("hash recorded");
    }
    assert!(super::inventory_project(&mut conn, &project.id)
        .unwrap()
        .pending
        .is_empty());
    write_file(
        &base,
        "beta/src/Button.tsx",
        "export const B = () => <b>BETA v2</b>;",
    );
    let changed = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        changed
            .pending
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["src/Button"],
        "only the repo whose file moved is re-proposed"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A component already catalogued under its OWN source KEEPS its name when a homonym appears later:
/// the identity is the SOURCE, so an existing `proto_composition` reference (which resolves a
/// component BY NAME) and the recorded synthesis both survive. A GREENFIELD `[NEW]` row of the same
/// name is ADOPTED (provenance bound in place), never duplicated.
#[test]
fn an_existing_component_keeps_its_name_when_a_homonym_appears_and_greenfield_is_adopted() {
    let root = scratch_dir("stable-names");
    write_file(
        &root,
        "src/ui/Button.tsx",
        "export const Button = () => <button/>;",
    );

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    // A GREENFIELD `[NEW]` the proto authored, which the front is about to implement.
    crate::db::upsert_component(
        &mut conn,
        &project.id,
        &crate::db::ComponentInput {
            name: "Card".into(),
            template: "<section>{{title}}</section>".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let first = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        first
            .bundles
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Button"]
    );
    let button = get_component_by_name(&mut conn, &project.id, "Button")
        .unwrap()
        .unwrap();
    super::mark_synthesized(&mut conn, &project.id, "Button").unwrap();

    // The front now IMPLEMENTS the greenfield `Card` → the existing row is ADOPTED, not duplicated.
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <section/>;",
    );
    // …and a HOMONYM of Button lands in another folder.
    write_file(
        &root,
        "src/admin/Button.tsx",
        "export const Button = () => <button/>;",
    );

    let second = super::inventory_project(&mut conn, &project.id).unwrap();
    let names: Vec<&str> = second.bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Button", "Card", "admin/Button"],
        "the catalogued Button KEEPS its name; the newcomer is the one qualified"
    );
    let same = get_component_by_name(&mut conn, &project.id, "Button")
        .unwrap()
        .unwrap();
    assert_eq!(
        same.id, button.id,
        "the row was renamed nowhere — same identity"
    );
    assert!(
        same.visual_hash.is_some(),
        "its recorded synthesis survived the homonym"
    );
    let card = get_component_by_name(&mut conn, &project.id, "Card")
        .unwrap()
        .unwrap();
    assert_eq!(
        card.source_path.as_deref(),
        Some("src/ui/Card.tsx"),
        "the greenfield [NEW] was ADOPTED by the front that now implements it"
    );
    assert_eq!(
        card.template, "<section>{{title}}</section>",
        "adoption never clobbers the authored template"
    );
    assert_eq!(second.component_count, 3, "no duplicate row");

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review — adoption by NAME was a hijack): a catalogued + SYNTHESIZED component
/// whose file is DELETED is NOT re-pointed at an unrelated homonym that appears elsewhere.
///
/// The exact reproduced sequence: `src/ui/Card.tsx` catalogued and synthesized (a design-system
/// template), deleted, then an unrelated `src/admin/legacy/Card.tsx` added. The `Card` row used to
/// keep its id, its name and its DS template while its `source_path` silently became the admin file
/// — so the next synthesis overwrote the DS template, and a FROZEN composition (which resolves a
/// `[DS]` node by NAME) designated another component without a single signal.
///
/// Since the CRITICAL data-loss review the same sequence is ALSO non-destructive: the vanished row
/// carries a synthesis, so it is FLAGGED an orphan and KEPT instead of being pruned (it used to be
/// deleted, which freed its name for the unrelated newcomer). The hijack stays closed — the newcomer
/// is a distinct row with no inherited synthesis — and it is now closed even harder, because the
/// live name is still reserved and the newcomer takes a path-qualified one.
#[test]
fn a_sourced_component_is_never_re_pointed_at_an_unrelated_homonym() {
    let root = scratch_dir("no-hijack");
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    super::inventory_project(&mut conn, &project.id).unwrap();
    let card = get_component_by_name(&mut conn, &project.id, "Card")
        .unwrap()
        .unwrap();
    // The agent synthesizes the DESIGN-SYSTEM template for it.
    update_component_content(
        &mut conn,
        &card.id,
        "<div class=\"ds-card\">{{{children}}}</div>",
        None,
        None,
    )
    .unwrap();
    super::mark_synthesized(&mut conn, &project.id, "Card")
        .unwrap()
        .expect("hash recorded");

    // The DS component is DELETED, and an UNRELATED homonym lands elsewhere.
    std::fs::remove_file(root.join("src/ui/Card.tsx")).unwrap();
    write_file(
        &root,
        "src/admin/legacy/Card.tsx",
        "export const Card = () => <table>legacy admin</table>;",
    );

    let out = super::inventory_project(&mut conn, &project.id).unwrap();

    // The unrelated file is a DIFFERENT component: a NEW row, born empty, under a QUALIFIED name
    // (the live `Card` row still holds the bare one — a name a composition may reference).
    let newcomer = get_component_by_name(&mut conn, &project.id, "legacy/Card")
        .unwrap()
        .expect("the homonym is catalogued under a distinguishing name");
    assert_ne!(
        newcomer.id, card.id,
        "the admin file must NOT inherit the design-system component's row"
    );
    assert_eq!(
        newcomer.source_path.as_deref(),
        Some("src/admin/legacy/Card.tsx")
    );
    assert!(
        newcomer.template.is_empty() && newcomer.visual_hash.is_none(),
        "a brand-new component carries NO inherited synthesis: {newcomer:?}"
    );
    // The original row is neither re-pointed NOR destroyed: it is FLAGGED an orphan and kept, with
    // its design-system template intact (an LLM synthesis is not a scan's to throw away).
    let kept = crate::db::get_component(&mut conn, &card.id)
        .unwrap()
        .expect("a SYNTHESIZED row is never pruned automatically");
    assert_eq!(kept.name, "Card", "it keeps the name compositions resolve");
    assert_eq!(kept.template, "<div class=\"ds-card\">{{{children}}}</div>");
    assert_eq!(kept.source_path.as_deref(), Some("src/ui/Card.tsx"));
    assert!(
        kept.orphaned_at.is_some(),
        "…and it is FLAGGED, so /design can show it as an orphan"
    );
    assert_eq!(
        out.orphans
            .iter()
            .map(|o| (
                o.name.as_str(),
                o.source_path.as_str(),
                o.pruned,
                o.kept_because
            ))
            .collect::<Vec<_>>(),
        vec![(
            "Card",
            "src/ui/Card.tsx",
            false,
            Some(super::KEPT_SYNTHESIZED)
        )],
        "the vanished source is REPORTED and SPARED (it used to be silently destroyed)"
    );
    assert!(out.moved.is_empty(), "an unrelated homonym is not a move");
    assert_eq!(out.component_count, 2);

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review #58): a component MOVED on disk is re-anchored IN PLACE — its row, its
/// name, its synthesis and every frozen composition reference survive — whether its catalogue name
/// is BARE (`card`) or PATH-QUALIFIED (`y/card`). The qualified one used to be duplicated instead,
/// and its original row stayed forever pointing at a path that no longer existed, invisible.
#[test]
fn a_moved_bundle_is_re_anchored_by_content_for_bare_and_qualified_names() {
    let root = scratch_dir("moved");
    write_file(
        &root,
        "src/a/x/card.tsx",
        "export const A = () => <b>ALPHA</b>;",
    );
    write_file(
        &root,
        "src/b/y/card.tsx",
        "export const B = () => <b>BETA</b>;",
    );

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    let first = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        first
            .bundles
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        vec!["card", "y/card"],
        "one bare name, one QUALIFIED name — the population this lot creates"
    );
    // Both are synthesized (a move only has something to preserve once there IS a synthesis).
    let mut ids = std::collections::BTreeMap::new();
    for name in ["card", "y/card"] {
        let c = get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap();
        update_component_content(&mut conn, &c.id, &format!("<b>{name}</b>"), None, None).unwrap();
        super::mark_synthesized(&mut conn, &project.id, name)
            .unwrap()
            .expect("hash recorded");
        ids.insert(name, c.id);
    }
    assert!(super::inventory_project(&mut conn, &project.id)
        .unwrap()
        .pending
        .is_empty());

    // MOVE both: the bare-named one AND the qualified one.
    std::fs::create_dir_all(root.join("src/a/w")).unwrap();
    std::fs::create_dir_all(root.join("src/b/z")).unwrap();
    std::fs::rename(root.join("src/a/x/card.tsx"), root.join("src/a/w/card.tsx")).unwrap();
    std::fs::rename(root.join("src/b/y/card.tsx"), root.join("src/b/z/card.tsx")).unwrap();

    let moved = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        moved.component_count, 2,
        "a move DUPLICATES nothing (it used to: 3 rows for 2 bundles)"
    );
    assert!(moved.orphans.is_empty(), "a move is not an orphan");
    assert_eq!(
        moved.moved.len(),
        2,
        "both were re-anchored: {:?}",
        moved.moved
    );
    for (name, entry) in [("card", "src/a/w/card.tsx"), ("y/card", "src/b/z/card.tsx")] {
        let row = get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap_or_else(|| panic!("{name} kept its name"));
        assert_eq!(
            row.id, ids[name],
            "{name}: SAME row — the identity survived"
        );
        assert_eq!(row.source_path.as_deref(), Some(entry));
        assert_eq!(
            row.template,
            format!("<b>{name}</b>"),
            "{name}: synthesis kept"
        );
    }
    assert!(
        moved.pending.is_empty(),
        "a pure MOVE changed no content — nothing to re-synthesize: {:?}",
        moved.pending
    );
    // …and the loop is CLOSED: a second run without a disk change proposes nothing new.
    let again = super::inventory_project(&mut conn, &project.id).unwrap();
    assert!(again.pending.is_empty() && again.orphans.is_empty() && again.moved.is_empty());
    assert!(!again.catalogue_changed, "an idle run mutates nothing");

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review #44): a GLOB change ÉLAGUE the entries that no longer match — the 28
/// phantom rows a corrected glob left behind, which nothing could remove. A greenfield `[NEW]`
/// (`source_path = null`) SURVIVES the prune: it has no file by construction.
#[test]
fn changing_the_globs_prunes_what_no_longer_matches_but_never_a_greenfield() {
    let root = scratch_dir("prune-globs");
    write_file(
        &root,
        "src/ui/Button.tsx",
        "export const B = () => <button/>;",
    );
    write_file(
        &root,
        "src/emails/Welcome.tsx",
        "export const W = () => <div/>;",
    );

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    // A greenfield `[NEW]` the proto authored — it must survive every prune.
    crate::db::upsert_component(
        &mut conn,
        &project.id,
        &crate::db::ComponentInput {
            name: "PricingTable".into(),
            template: "<section>{{title}}</section>".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let first = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(first.discovered, 2, "the wrong glob caught the email too");
    assert_eq!(first.component_count, 3, "…plus the greenfield row");
    assert!(first.orphans.is_empty(), "nothing is orphaned yet");

    // The glob is CORRECTED to exclude the email subsystem.
    set_design_inventory_globs(
        &mut conn,
        &project.id,
        Some(&["src/ui/**/*.tsx".to_string()]),
    )
    .unwrap();
    let pruned = super::inventory_project(&mut conn, &project.id).unwrap();

    assert_eq!(
        pruned
            .orphans
            .iter()
            .map(|o| (o.name.as_str(), o.pruned))
            .collect::<Vec<_>>(),
        vec![("Welcome", true)],
        "the entry the new glob no longer matches is REMOVED"
    );
    let names: Vec<String> = list_components(&mut conn, &project.id)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(
        names,
        vec!["Button".to_string(), "PricingTable".to_string()],
        "the greenfield [NEW] survived; the phantom entry is gone"
    );
    // Idempotent: a second run with the same globs prunes nothing more and reports no change.
    let again = super::inventory_project(&mut conn, &project.id).unwrap();
    assert!(again.orphans.is_empty() && !again.catalogue_changed);

    let _ = std::fs::remove_dir_all(&root);
}

/// The prune's GUARDS: it never empties a catalogue on the strength of a scan it could not make.
/// A repo whose root is MISSING (a half-checked-out worktree, a branch switch) spares every row it
/// owns with `repo_not_scanned`; a repo that is there but discovered NOTHING spares them with
/// `empty_scan`. Both are REPORTED, so the rows are never invisible either.
#[test]
fn the_prune_spares_what_the_run_could_not_judge_and_says_why() {
    let base = scratch_dir("prune-guards");
    write_file(
        &base,
        "web/src/Button.tsx",
        "export const B = () => <button/>;",
    );
    write_file(&base, "web/src/Card.tsx", "export const C = () => <div/>;");

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", base.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", "web", false).unwrap();
    assert_eq!(
        super::inventory_project(&mut conn, &project.id)
            .unwrap()
            .component_count,
        2
    );

    // The repo's whole tree is EMPTIED (the files are gone, the root still exists).
    std::fs::remove_file(base.join("web/src/Button.tsx")).unwrap();
    std::fs::remove_file(base.join("web/src/Card.tsx")).unwrap();
    let empty = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(empty.discovered, 0);
    assert!(
        empty
            .orphans
            .iter()
            .all(|o| !o.pruned && o.kept_because == Some("empty_scan")),
        "an EMPTY scan is a mis-configuration signal, never an order to purge: {:?}",
        empty.orphans
    );
    assert_eq!(
        empty.component_count, 2,
        "the catalogue survives an empty scan intact"
    );

    // The repo ROOT itself disappears → the run cannot judge at all.
    std::fs::remove_dir_all(base.join("web")).unwrap();
    let gone = super::inventory_project(&mut conn, &project.id).unwrap();
    assert!(
        gone.orphans
            .iter()
            .all(|o| !o.pruned && o.kept_because == Some("repo_not_scanned")),
        "a repo that was not walked can never justify a prune: {:?}",
        gone.orphans
    );
    assert_eq!(gone.component_count, 2);
    assert!(
        !gone.catalogue_changed,
        "sparing everything mutates nothing"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The two naming INVARIANTS the life-cycle work must not regress, asserted together because they
/// pull against each other: names are DETERMINISTIC (the same tree yields the same names on a fresh
/// catalogue and on any re-run), and a component ALREADY catalogued is NEVER renamed (a composition
/// resolves it BY NAME, so a rename would silently re-aim the reference).
#[test]
fn catalogue_names_are_deterministic_and_an_existing_component_is_never_renamed() {
    let build = |root: &std::path::Path| {
        write_file(root, "src/ui/card.tsx", "export const A = () => <b>UI</b>;");
        write_file(
            root,
            "src/admin/card.tsx",
            "export const B = () => <b>ADMIN</b>;",
        );
        write_file(
            root,
            "src/ui/button.tsx",
            "export const C = () => <button/>;",
        );
    };
    let names_of = |conn: &mut diesel::SqliteConnection, project: &str| -> Vec<(String, String)> {
        list_components(conn, project)
            .unwrap()
            .into_iter()
            .map(|c| (c.name, c.source_path.unwrap_or_default()))
            .collect()
    };

    // Catalogue A: a FRESH catalogue over the tree.
    let root_a = scratch_dir("determinism-a");
    build(&root_a);
    let mut conn = open_in_memory();
    let (a, _ws) = create_project(&mut conn, "A", root_a.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &a.id, "web", ".", false).unwrap();
    super::inventory_project(&mut conn, &a.id).unwrap();
    let first = names_of(&mut conn, &a.id);
    assert_eq!(
        first,
        vec![
            ("button".to_string(), "src/ui/button.tsx".to_string()),
            // The bare name goes to the first claimant in the (repo, entry path) order — `admin`
            // before `ui` — and the loser is qualified by its shortest distinguishing directory.
            ("card".to_string(), "src/admin/card.tsx".to_string()),
            ("ui/card".to_string(), "src/ui/card.tsx".to_string()),
        ],
    );

    // Re-running over the SAME tree changes nothing at all.
    for run in 0..3 {
        super::inventory_project(&mut conn, &a.id).unwrap();
        assert_eq!(names_of(&mut conn, &a.id), first, "run {run} is not stable");
    }

    // Catalogue B: a SECOND, independent fresh catalogue over an identical tree — same names.
    let root_b = scratch_dir("determinism-b");
    build(&root_b);
    let (b, _ws) = create_project(&mut conn, "B", root_b.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &b.id, "web", ".", false).unwrap();
    super::inventory_project(&mut conn, &b.id).unwrap();
    assert_eq!(
        names_of(&mut conn, &b.id),
        first,
        "a fresh catalogue over the same tree yields the SAME names"
    );

    // A NEWCOMER never renames what is already catalogued — it is the one that gets qualified.
    let ids: std::collections::BTreeMap<String, String> = list_components(&mut conn, &a.id)
        .unwrap()
        .into_iter()
        .map(|c| (c.name, c.id))
        .collect();
    write_file(
        &root_a,
        "src/legacy/card.tsx",
        "export const D = () => <b>OLD</b>;",
    );
    super::inventory_project(&mut conn, &a.id).unwrap();
    for (name, id) in &ids {
        let row = get_component_by_name(&mut conn, &a.id, name)
            .unwrap()
            .unwrap_or_else(|| panic!("{name} kept its name"));
        assert_eq!(&row.id, id, "{name} kept its identity under its own name");
    }
    assert!(
        get_component_by_name(&mut conn, &a.id, "legacy/card")
            .unwrap()
            .is_some(),
        "the NEWCOMER is the one that takes a qualified name"
    );

    let _ = std::fs::remove_dir_all(&root_a);
    let _ = std::fs::remove_dir_all(&root_b);
}

// --- inventory over a real tree + DB ---------------------------------------

/// DONE-CRITERION (review #40): the inventory CATALOGUES the discovered bundles (their name,
/// provenance and entry path) WITHOUT synthesizing anything — the template stays EMPTY until an
/// agent fills it, and a re-run reconciles in place (never duplicates).
#[test]
fn inventory_catalogues_bundles_without_synthesizing() {
    let root = scratch_dir("inv-default");
    write_file(
        &root,
        "src/Button.tsx",
        "export const Button = () => <button/>;",
    );
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );
    write_file(&root, "src/helpers.ts", "export const x = 1;"); // logic → no entry glob
    write_file(&root, "src/Button.test.tsx", "test('x');"); // test → excluded
    write_file(&root, "node_modules/dep/Ignored.tsx", "export const I = 1;"); // dep → pruned

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    let repo = create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    let globs = resolve_component_globs(ComponentStack::React, None);
    let out = inventory_components(&mut conn, &project.id, Some(&repo.id), &root, &globs).unwrap();

    let names: Vec<&str> = out.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Button", "Card"],
        "only real component bundles (deps/tests/logic excluded)"
    );
    let button = out.iter().find(|c| c.name == "Button").unwrap();
    assert_eq!(
        button.source_repository_id.as_deref(),
        Some(repo.id.as_str())
    );
    assert_eq!(button.source_path.as_deref(), Some("src/Button.tsx"));
    assert!(
        button.template.is_empty(),
        "the discovery never synthesizes — the agent does"
    );
    assert!(
        button.visual_hash.is_none(),
        "the hash is written at SYNTHESIS time, not at discovery"
    );

    // A synthesis lands… and a RE-INVENTORY must not wipe it (the whole point of the split).
    update_component_content(
        &mut conn,
        &button.id,
        "<button class=\"ds\">{{label}}</button>",
        Some("[]"),
        None,
    )
    .unwrap();
    let again =
        inventory_components(&mut conn, &project.id, Some(&repo.id), &root, &globs).unwrap();
    assert_eq!(again.len(), 2, "re-inventory does not duplicate");
    assert_eq!(
        again.iter().find(|c| c.name == "Button").unwrap().template,
        "<button class=\"ds\">{{label}}</button>",
        "a re-inventory NEVER clobbers a synthesized template"
    );
    assert_eq!(list_components(&mut conn, &project.id).unwrap().len(), 2);

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (task 2): the glob is SURCHARGEABLE PAR PROJET — a project override changes the
/// discovered set (here, restricting to top-level `src/*.tsx` drops the nested `Card`).
#[test]
fn inventory_honours_a_project_glob_override() {
    let root = scratch_dir("inv-override");
    write_file(
        &root,
        "src/Button.tsx",
        "export const Button = () => <button/>;",
    );
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();

    let over = vec!["src/*.tsx".to_string()];
    let globs = resolve_component_globs(ComponentStack::React, Some(&over));
    let out = inventory_components(&mut conn, &project.id, None, &root, &globs).unwrap();

    let names: Vec<&str> = out.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Button"],
        "the override restricts to top-level src/*.tsx (nested Card dropped)"
    );
    // A loose scan (no source repo) leaves the provenance repo NULL but still records the path.
    assert!(out[0].source_repository_id.is_none());
    assert_eq!(out[0].source_path.as_deref(), Some("src/Button.tsx"));

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (task 2): GREENFIELD — an app with no front (a missing root, or an empty tree)
/// inventories NOTHING WITHOUT error (the lib is created by the agent later).
#[test]
fn greenfield_inventory_is_graceful_and_errorless() {
    let mut conn = open_in_memory();
    let base = scratch_dir("greenfield");
    let (project, _ws) = create_project(&mut conn, "Neuf", base.to_str().unwrap(), None).unwrap();
    let globs = resolve_component_globs(ComponentStack::default(), None);

    // A root that does NOT exist → empty, no error, no panic.
    let missing = base.join("does-not-exist");
    assert!(!missing.exists());
    let out = inventory_components(&mut conn, &project.id, None, &missing, &globs).unwrap();
    assert!(out.is_empty(), "missing front → no components");

    // An EXISTING but empty tree → likewise empty.
    let empty = base.join("empty-front");
    std::fs::create_dir_all(&empty).unwrap();
    let out = inventory_components(&mut conn, &project.id, None, &empty, &globs).unwrap();
    assert!(out.is_empty(), "empty front → no components");
    assert_eq!(
        list_components(&mut conn, &project.id).unwrap().len(),
        0,
        "greenfield leaves the catalogue empty"
    );

    let _ = std::fs::remove_dir_all(&base);
}

// =============================================================================
// The MD5 (the incremental key's primitive)
// =============================================================================

/// The self-contained MD5 matches the RFC 1321 known-answer vectors — pins the hash constants
/// so a wrong K/S table (or a padding bug) fails loudly.
#[test]
fn md5_matches_known_answer_vectors() {
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"a"), "0cc175b9c0f1b6a831c399e269772661");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(
        md5_hex(b"message digest"),
        "f96b697d7cb7938d525a2f31aaf161d0"
    );
    assert_eq!(
        md5_hex(b"The quick brown fox jumps over the lazy dog"),
        "9e107d9d372bb6826bd81d3542a419d6"
    );
    // The RFC 1321 A.5 62-byte vector (> 56 bytes) exercises multi-block padding.
    assert_eq!(
        md5_hex(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"),
        "d174ab98d277d9f5a5611c2c9f419d9f"
    );
}

#[test]
fn extension_classification_partitions_templates_and_styles() {
    assert!(is_template_extension("html") && is_template_extension("htm"));
    assert!(is_style_extension("css") && is_style_extension("scss"));
    // A template extension is never a style one, and vice-versa.
    assert!(!is_style_extension("html") && !is_template_extension("css"));
    assert_eq!(
        file_extension_lower("Button.module.CSS").as_deref(),
        Some("css")
    );
    assert_eq!(file_extension_lower("Makefile"), None);
}

// =============================================================================
// RUNTIME TRIGGER — inventory_project + the INCREMENTAL synthesis loop
// =============================================================================

/// DONE-CRITERION (review #23 + #40): the RUNTIME trigger resolves the project's repos and
/// CATALOGUES the discovered bundles; the PENDING list is the targeted work an agent must
/// synthesize — a synthesized bundle drops out of it, a logic-only edit never brings it back, and
/// a real source change puts EXACTLY that bundle back in.
#[test]
fn inventory_project_catalogues_and_tracks_pending_synthesis_incrementally() {
    let root = scratch_dir("inv-project");
    write_file(&root, "src/Button.tsx", "<button>{{label}}</button>");
    write_file(&root, "src/Button.ts", "export const onClick = () => {};"); // logic → not a member
    write_file(&root, "src/ui/Card.tsx", "<div class=\"card\"/>");
    write_file(&root, "src/Button.test.tsx", "test('x');"); // test → excluded
    write_file(&root, "node_modules/dep/Dep.tsx", "export const D = 1;"); // dep → pruned

    let mut conn = open_in_memory();
    // create_project seeds the ROOT workspace at `root`; the repo derives at `root_path + "."`.
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    let repo = create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    // First trigger: the catalogue is POPULATED with the real bundles (deps/tests/logic out).
    let first = super::inventory_project(&mut conn, &project.id).unwrap();
    let names: Vec<&str> = first.bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Button", "Card"],
        "the trigger peuples design_components with the real component bundles"
    );
    assert_eq!(first.repos_scanned, 1, "the project's one repo was scanned");
    assert_eq!(first.discovered, 2, "two bundles discovered on disk");
    assert_eq!(first.component_count, 2);
    assert_eq!(first.synthesized_count, 0, "nothing synthesized yet");
    let pending: Vec<&str> = first.pending.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        pending,
        vec!["Button", "Card"],
        "every fresh bundle awaits its LLM synthesis"
    );
    assert_eq!(first.pending[0].reason, "never_synthesized");
    // Provenance: a configured repo is recorded on each component.
    let button = get_component_by_name(&mut conn, &project.id, "Button")
        .unwrap()
        .unwrap();
    assert_eq!(
        button.source_repository_id.as_deref(),
        Some(repo.id.as_str())
    );
    assert_eq!(button.source_path.as_deref(), Some("src/Button.tsx"));

    // The AGENT synthesizes both bundles (template stored + the input hash recorded).
    for name in ["Button", "Card"] {
        let c = get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap();
        update_component_content(&mut conn, &c.id, "<div>{{label}}</div>", None, None).unwrap();
        super::mark_synthesized(&mut conn, &project.id, name)
            .unwrap()
            .expect("hash recorded");
    }

    // A NO-OP re-run: idempotent (no duplicates) AND nothing to re-synthesize (no bulk regen).
    let again = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(again.component_count, 2, "re-inventory does not duplicate");
    assert_eq!(again.synthesized_count, 2);
    assert!(
        again.pending.is_empty(),
        "an unchanged bundle is NEVER re-synthesized"
    );

    // A logic-only `.ts` edit (not a bundle member) → still nothing pending.
    write_file(
        &root,
        "src/Button.ts",
        "export const onClick = () => other();",
    );
    assert!(
        super::inventory_project(&mut conn, &project.id)
            .unwrap()
            .pending
            .is_empty(),
        "a non-member logic edit triggers NO re-synthesis"
    );

    // A real SOURCE change → EXACTLY that bundle is pending again (targeted, never en bloc).
    write_file(
        &root,
        "src/Button.tsx",
        "<button class=\"b\">{{label}}</button>",
    );
    let changed = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        changed
            .pending
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Button"],
        "only the changed bundle awaits a re-synthesis"
    );
    assert_eq!(changed.pending[0].reason, "source_changed");
    // …and the stale run did NOT touch the stored hash, so an un-acted change stays pending.
    assert_eq!(
        super::inventory_project(&mut conn, &project.id)
            .unwrap()
            .pending
            .len(),
        1,
        "observing a change never marks it handled"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review #40): `read_bundle` hands the AGENT the matter to synthesize — the
/// bundle's files WITH their content, on demand, per component (never the whole catalogue).
#[test]
fn read_bundle_exposes_the_files_and_their_content_on_demand() {
    let root = scratch_dir("read-bundle");
    write_file(&root, "angular.json", r#"{"version":1}"#);
    write_file(
        &root,
        "src/app/chip/chip.component.ts",
        "@Component({ templateUrl: './chip.component.html' })\nexport class ChipComponent { label = input(''); }",
    );
    write_file(
        &root,
        "src/app/chip/chip.component.html",
        "<span class=\"chip\">{{ label() }}</span>",
    );

    let mut conn = open_in_memory();
    let (project, _ws) =
        create_project(&mut conn, "sandbox", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    super::inventory_project(&mut conn, &project.id).unwrap();

    let read = super::read_bundle(&mut conn, &project.id, "chip", 64 * 1024)
        .unwrap()
        .expect("the bundle is readable");
    assert_eq!(read.component.name, "chip");
    assert!(!read.up_to_date, "not synthesized yet");
    let bundle = read.bundle.as_ref().expect("resolved bundle");
    assert_eq!(bundle.entry_path, "src/app/chip/chip.component.ts");
    assert_eq!(read.files.len(), 2, "class + resolved template");
    let html = read
        .files
        .iter()
        .find(|f| f.path.ends_with(".html"))
        .expect("the template file");
    assert_eq!(html.role, BundleRole::Template);
    assert!(
        html.content.contains("<span class=\"chip\">"),
        "the agent receives the REAL markup: {}",
        html.content
    );
    assert!(!html.truncated);

    // Truncation is bounded (a pathological file can never blow the response).
    let tiny = super::read_bundle(&mut conn, &project.id, "chip", 8)
        .unwrap()
        .unwrap();
    assert!(tiny.files.iter().all(|f| f.content.len() <= 8));
    assert!(tiny.files.iter().any(|f| f.truncated));

    // An unknown component is `None` (the caller maps it to a structured refusal).
    assert!(super::read_bundle(&mut conn, &project.id, "nope", 1024)
        .unwrap()
        .is_none());

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review #32 + #40): on an ANGULAR project the inventory DETECTS the stack and
/// catalogues the component BUNDLES — not the `.tsx` of a sibling React subsystem (the real-world
/// bug), not the `.generated` mirror, and not the `.service.ts` / `.types.ts` non-components.
#[test]
fn inventory_project_detects_angular_stack_and_excludes_generated_tsx_and_non_components() {
    let root = scratch_dir("inv-angular");
    // Angular workspace markers → detect_stack ⇒ Angular ⇒ the component-bundle globs.
    write_file(&root, "angular.json", r#"{"version":1}"#);
    write_file(
        &root,
        "package.json",
        r#"{"dependencies":{"@angular/core":"^17.0.0"}}"#,
    );
    // The REAL Angular components (button has a class + template; nav is template-only).
    write_file(
        &root,
        "src/app/button/button.component.html",
        "<button></button>",
    );
    write_file(
        &root,
        "src/app/button/button.component.ts",
        "@Component({ templateUrl: './button.component.html' })\nexport class B {}",
    );
    write_file(&root, "src/app/nav/nav.component.html", "<nav></nav>");
    // Non-components — never catalogued (the regex pass used to).
    write_file(&root, "src/app/core/api.service.ts", "export class Api {}");
    write_file(&root, "src/app/core/dto.types.ts", "export type D = 1;");
    // A sibling React subsystem (email/PDF templating) — must NOT be captured on an Angular scan.
    write_file(
        &root,
        "src/email/Welcome.tsx",
        "export const W = () => <div/>;",
    );
    // A generated mirror — must be pruned.
    write_file(&root, ".generated/copy.component.html", "<button></button>");

    let mut conn = open_in_memory();
    let (project, _ws) =
        create_project(&mut conn, "sandbox", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    let mut names: Vec<&str> = out.bundles.iter().map(|b| b.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["button", "nav"],
        "Angular bundles captured; the React .tsx, the .generated copy and the service/types excluded"
    );
    // The `button` BUNDLE carries BOTH files under ONE catalogue entry (its class is the entry).
    let button = bundle(&out.bundles, "button");
    assert_eq!(button.entry_path, "src/app/button/button.component.ts");
    assert_eq!(button.files.len(), 2);

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (review #32): the inventory is SURCHARGEABLE PAR PROJET — a PERSISTED glob
/// override ([`set_design_inventory_globs`]) REPLACES the detected-stack default for the whole
/// project. Here an Angular-detected project is overridden to scan `.tsx`, so the React file is
/// captured instead of the `.component.html`.
#[test]
fn inventory_project_honours_persisted_glob_override() {
    let root = scratch_dir("inv-override-project");
    write_file(&root, "angular.json", r#"{"version":1}"#); // would detect Angular…
    write_file(&root, "src/app/nav/nav.component.html", "<nav></nav>");
    write_file(
        &root,
        "src/email/Welcome.tsx",
        "export const W = () => <div/>;",
    );

    let mut conn = open_in_memory();
    let (project, _ws) =
        create_project(&mut conn, "sandbox", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    // Persist a per-project override: scan `.tsx` instead of the Angular default.
    set_design_inventory_globs(&mut conn, &project.id, Some(&["**/*.tsx".to_string()])).unwrap();

    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    let names: Vec<&str> = out.bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Welcome"],
        "the persisted override wins over the detected Angular default (nav.component.html skipped)"
    );

    let _ = std::fs::remove_dir_all(&root);

    // A FRESH project with the override CLEARED (None) falls back to the detected Angular default —
    // proving the clear path (an accumulated catalogue would mask this, hence a clean tree here).
    let root2 = scratch_dir("inv-override-cleared");
    write_file(&root2, "angular.json", r#"{"version":1}"#);
    write_file(&root2, "src/app/nav/nav.component.html", "<nav></nav>");
    write_file(
        &root2,
        "src/email/Welcome.tsx",
        "export const W = () => <div/>;",
    );
    let (project2, _ws2) =
        create_project(&mut conn, "sandbox2", root2.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project2.id, "web", ".", false).unwrap();
    // Set then clear the override → back to the detected stack default.
    set_design_inventory_globs(&mut conn, &project2.id, Some(&["**/*.tsx".to_string()])).unwrap();
    set_design_inventory_globs(&mut conn, &project2.id, None).unwrap();
    let out = super::inventory_project(&mut conn, &project2.id).unwrap();
    let names: Vec<&str> = out.bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["nav"],
        "cleared override → back to the detected Angular default"
    );

    let _ = std::fs::remove_dir_all(&root2);
}

/// DONE-CRITERION (review #23): GREENFIELD — a project whose front is empty (the root workspace
/// exists but the folder holds no component) inventories NOTHING WITHOUT error; the fallback
/// current-folder member is still scanned (repos_scanned = 1) but discovers nothing.
#[test]
fn inventory_project_greenfield_is_graceful_and_errorless() {
    let root = scratch_dir("inv-project-greenfield");
    let mut conn = open_in_memory();
    // No configured repo → the current-folder fallback member; the folder holds no component.
    let (project, _ws) = create_project(&mut conn, "Neuf", root.to_str().unwrap(), None).unwrap();

    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(out.discovered, 0, "greenfield discovers nothing");
    assert_eq!(out.component_count, 0, "the catalogue stays empty");
    assert!(out.pending.is_empty(), "nothing to synthesize");
    assert_eq!(
        out.repos_scanned, 1,
        "the fallback current-folder repo is still walked (no error)"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ===========================================================================
// FAIL-CLOSED ÉLAGAGE — a scan that could not read everything destroys NOTHING
// (review 01KYHPFDE21W0EMVX4J87RHGCV, finding 01KYHQ8P2QJAG2G9EC5ZYPHDEN, CRITICAL)
// ===========================================================================

/// Make `path` UNREADABLE (mode 000), returning whether it REALLY became unreadable. A test process
/// running as ROOT (some CI containers do) bypasses the permission bits entirely, so the sequence
/// these tests reproduce simply cannot happen there — the caller then skips instead of asserting a
/// behaviour the OS is refusing to produce.
#[cfg(unix)]
fn make_unreadable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).expect("stat").permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(path, perms).expect("chmod 000");
    if path.is_dir() {
        std::fs::read_dir(path).is_err()
    } else {
        std::fs::File::open(path).is_err()
    }
}

/// Give `path` its permissions back, so the scratch tree can be removed.
#[cfg(unix)]
fn make_readable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() { 0o755 } else { 0o644 };
    let mut perms = std::fs::metadata(path).expect("stat").permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms).expect("chmod restore");
}

/// DONE-CRITERION (CRITICAL — sequence (1) of the audit): a scan that could not read ONE SUBTREE,
/// while the REST of the repo scans perfectly, prunes NOTHING and says why.
///
/// The reproduced sequence: `src/a/Alpha.tsx` + `src/b/{Beta,Gamma}.tsx`, all three catalogued AND
/// synthesized; `chmod 000` on `src/b`. The walk swallowed the `read_dir` failure, the repo still
/// "discovered something" (Alpha), so Beta and Gamma looked like rows whose source had vanished:
/// both were DELETED with `kept_because: None`, and their two LLM syntheses were gone for good —
/// restoring the permissions brought them back `template: ""`, `never_synthesized`.
///
/// The trigger needs no `chmod` in real life: a concurrent `git checkout`, a network FS returning
/// EIO, an EMFILE. AN ABSENCE IS NEVER A PROOF.
#[cfg(unix)]
#[test]
fn a_partially_unreadable_repo_prunes_nothing_and_reports_the_failure() {
    let root = scratch_dir("scan-partial");
    write_file(&root, "src/a/Alpha.tsx", "export const A = () => <b>A</b>;");
    write_file(&root, "src/b/Beta.tsx", "export const B = () => <b>B</b>;");
    write_file(&root, "src/b/Gamma.tsx", "export const G = () => <b>G</b>;");

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    assert_eq!(
        super::inventory_project(&mut conn, &project.id)
            .unwrap()
            .discovered,
        3
    );
    // All three carry an LLM synthesis — that is what the bug destroyed.
    for name in ["Alpha", "Beta", "Gamma"] {
        let c = get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap();
        update_component_content(
            &mut conn,
            &c.id,
            &format!("<b data-c=\"{name}\"/>"),
            None,
            None,
        )
        .unwrap();
        super::mark_synthesized(&mut conn, &project.id, name)
            .unwrap()
            .expect("hash recorded");
    }

    // ONE subtree becomes unreadable. The rest of the repo scans normally.
    let hidden = root.join("src/b");
    if !make_unreadable(&hidden) {
        let _ = std::fs::remove_dir_all(&root);
        return; // running as root: the OS refuses to produce the failure this test needs
    }

    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    make_readable(&hidden);

    assert_eq!(out.discovered, 1, "only Alpha could be walked");
    assert!(
        out.scan_errors.count >= 1,
        "the walk REPORTS what it could not read (it used to swallow it): {:?}",
        out.scan_errors
    );
    assert!(
        out.scan_errors
            .sample
            .iter()
            .any(|e| e.kind == "read_dir" && e.path == "src/b"),
        "the failure is NAMED (path + nature): {:?}",
        out.scan_errors.sample
    );
    let mut spared: Vec<(&str, bool, Option<&'static str>)> = out
        .orphans
        .iter()
        .map(|o| (o.name.as_str(), o.pruned, o.kept_because))
        .collect();
    spared.sort();
    assert_eq!(
        spared,
        vec![
            ("Beta", false, Some(super::KEPT_SCAN_INCOMPLETE)),
            ("Gamma", false, Some(super::KEPT_SCAN_INCOMPLETE)),
        ],
        "a repo whose scan is INCOMPLETE prunes nothing at all"
    );
    assert_eq!(out.component_count, 3, "the catalogue is intact");
    for name in ["Alpha", "Beta", "Gamma"] {
        let c = get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap_or_else(|| panic!("{name} survived"));
        assert_eq!(
            c.template,
            format!("<b data-c=\"{name}\"/>"),
            "{name}: its synthesis survived a scan that could not read it"
        );
        assert!(
            c.orphaned_at.is_none(),
            "{name}: a run that could not judge flags NOTHING"
        );
    }

    // Permissions restored → the next scan is clean and the loop closes with nothing pending.
    let back = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(back.discovered, 3);
    assert!(back.scan_errors.is_clean() && back.orphans.is_empty());
    assert!(
        back.pending.is_empty(),
        "the syntheses are still recorded — nothing to redo: {:?}",
        back.pending
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (CRITICAL — sequence (2) of the audit): on ANGULAR, a SINGLE
/// unreadable `.ts` used to cost that component its row and its template.
///
/// Angular's net takes every `.ts` and tells components from utils by the `@Component` decorator's
/// presence — a CONTENT probe. A read failure returned `false`, i.e. "not a component", i.e. "its
/// source is gone", i.e. a DELETE. One `open()` away from destroying a synthesis.
#[cfg(unix)]
#[test]
fn a_single_unreadable_angular_entry_never_costs_a_component_its_template() {
    let root = scratch_dir("scan-angular");
    write_file(&root, "angular.json", r#"{"version":1}"#);
    write_file(
        &root,
        "src/app/button.ts",
        "@Component({ template: '<button></button>' })\nexport class Button {}",
    );
    write_file(
        &root,
        "src/app/card.ts",
        "@Component({ template: '<div class=\"card\"></div>' })\nexport class Card {}",
    );

    let mut conn = open_in_memory();
    let (project, _ws) =
        create_project(&mut conn, "sandbox", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    assert_eq!(
        super::inventory_project(&mut conn, &project.id)
            .unwrap()
            .discovered,
        2
    );
    for name in ["button", "card"] {
        let c = get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap();
        update_component_content(&mut conn, &c.id, &format!("<llm-{name}/>"), None, None).unwrap();
        super::mark_synthesized(&mut conn, &project.id, name)
            .unwrap()
            .expect("hash recorded");
    }

    let hidden = root.join("src/app/card.ts");
    if !make_unreadable(&hidden) {
        let _ = std::fs::remove_dir_all(&root);
        return; // running as root
    }

    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    make_readable(&hidden);

    assert_eq!(out.discovered, 1, "card.ts could not be probed");
    assert!(
        out.scan_errors
            .sample
            .iter()
            .any(|e| e.kind == "read_file" && e.path == "src/app/card.ts"),
        "the unreadable ENTRY is named: {:?}",
        out.scan_errors.sample
    );
    assert_eq!(
        out.orphans
            .iter()
            .map(|o| (o.name.as_str(), o.pruned, o.kept_because))
            .collect::<Vec<_>>(),
        vec![("card", false, Some(super::KEPT_SCAN_INCOMPLETE))],
        "ONE failed read can no longer justify a deletion"
    );
    let card = get_component_by_name(&mut conn, &project.id, "card")
        .unwrap()
        .expect("the card row survived");
    assert_eq!(card.template, "<llm-card/>", "its template is untouched");
    assert_eq!(out.component_count, 2);

    let _ = std::fs::remove_dir_all(&root);
}

/// The line the automatic élagage never crosses, both sides of it, in ONE clean scan (no I/O
/// failure at all — the run IS in a position to judge):
///   * a row that was never SYNTHESIZED and whose source is gone is PRUNED — lossless, a re-scan
///     would rebuild it for free;
///   * a row CARRYING a synthesis is SPARED, FLAGGED (`orphaned_at`) and reported. Its removal is an
///     explicit gesture (`design_delete_component` / the `/design` card), never a scan's side effect.
#[test]
fn a_clean_scan_prunes_only_what_was_never_synthesized_and_flags_the_rest() {
    let root = scratch_dir("prune-line");
    write_file(&root, "src/Stay.tsx", "export const S = () => <b>S</b>;");
    write_file(&root, "src/Kept.tsx", "export const K = () => <b>K</b>;");
    write_file(&root, "src/Gone.tsx", "export const G = () => <b>G</b>;");

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    super::inventory_project(&mut conn, &project.id).unwrap();
    // Only `Kept` is synthesized; `Gone` stays a bare discovery.
    let kept = get_component_by_name(&mut conn, &project.id, "Kept")
        .unwrap()
        .unwrap();
    update_component_content(
        &mut conn,
        &kept.id,
        "<b class=\"ds\">{{label}}</b>",
        None,
        None,
    )
    .unwrap();
    super::mark_synthesized(&mut conn, &project.id, "Kept")
        .unwrap()
        .expect("hash recorded");

    std::fs::remove_file(root.join("src/Kept.tsx")).unwrap();
    std::fs::remove_file(root.join("src/Gone.tsx")).unwrap();
    let out = super::inventory_project(&mut conn, &project.id).unwrap();

    assert!(out.scan_errors.is_clean(), "this scan read everything");
    let mut reported: Vec<(&str, bool, Option<&'static str>, bool)> = out
        .orphans
        .iter()
        .map(|o| (o.name.as_str(), o.pruned, o.kept_because, o.flagged_orphan))
        .collect();
    reported.sort();
    assert_eq!(
        reported,
        vec![
            ("Gone", true, None, false),
            ("Kept", false, Some(super::KEPT_SYNTHESIZED), true),
        ]
    );
    let names: Vec<String> = list_components(&mut conn, &project.id)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, vec!["Kept".to_string(), "Stay".to_string()]);
    let survivor = get_component_by_name(&mut conn, &project.id, "Kept")
        .unwrap()
        .unwrap();
    assert_eq!(survivor.template, "<b class=\"ds\">{{label}}</b>");
    assert!(survivor.orphaned_at.is_some(), "flagged for the human");
    assert!(
        out.catalogue_changed,
        "raising an orphan flag IS a catalogue move — /design must re-pull"
    );

    // The flag is STABLE: a second identical run neither re-flags nor re-notifies.
    let again = super::inventory_project(&mut conn, &project.id).unwrap();
    assert!(
        !again.catalogue_changed,
        "an idle run mutates nothing (the orphan is already flagged)"
    );
    assert_eq!(again.orphans.len(), 1);

    // The EXPLICIT removal is the way out — and it still works.
    assert!(crate::db::delete_component(&mut conn, &survivor.id).unwrap());
    let after = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(after.component_count, 1, "only Stay remains");

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (#58 — a move made in TWO steps): write the new source, let an inventory run, THEN
/// remove the old one. The synthesis used to be DESTROYED: the new file already owned an (empty) row
/// of its own, so it was not a "free" bundle any more, the re-anchoring had no target, and the row
/// carrying the template became an ordinary orphan the prune removed. The name was lost twice over —
/// the survivor kept `design/Card`, so a FROZEN composition referencing `Card` resolved nothing and
/// its node went back to `[NEW]`: the coder was told to build a component that exists.
#[test]
fn a_move_made_in_two_steps_keeps_one_row_its_template_and_its_name() {
    let root = scratch_dir("two-step-move");
    let source = "export const Card = () => <div class=\"card\"/>;";
    write_file(&root, "src/ui/Card.tsx", source);

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    super::inventory_project(&mut conn, &project.id).unwrap();
    let card = get_component_by_name(&mut conn, &project.id, "Card")
        .unwrap()
        .unwrap();
    update_component_content(&mut conn, &card.id, "<PRECIOUS-LLM-TEMPLATE/>", None, None).unwrap();
    super::mark_synthesized(&mut conn, &project.id, "Card")
        .unwrap()
        .expect("hash recorded");

    // STEP 1 — the new source appears; an inventory runs in between (another agent session, or the
    // `/design` button). The newcomer gets its own, EMPTY row.
    write_file(&root, "src/design/Card.tsx", source);
    let between = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(between.component_count, 2);
    assert!(between.moved.is_empty(), "nothing has moved yet");

    // STEP 2 — the old source is removed. THIS is where the template used to die.
    std::fs::remove_file(root.join("src/ui/Card.tsx")).unwrap();
    let out = super::inventory_project(&mut conn, &project.id).unwrap();

    assert_eq!(
        out.moved,
        vec![("Card".to_string(), "src/design/Card.tsx".to_string())],
        "the two-step move IS a move"
    );
    assert!(out.orphans.is_empty(), "…so it is not an orphan either");
    assert_eq!(out.component_count, 1, "ONE row, not two");
    let survivor = get_component_by_name(&mut conn, &project.id, "Card")
        .unwrap()
        .expect("the NAME a frozen composition references survived");
    assert_eq!(survivor.id, card.id, "the same row — identity preserved");
    assert_eq!(survivor.template, "<PRECIOUS-LLM-TEMPLATE/>");
    assert_eq!(survivor.source_path.as_deref(), Some("src/design/Card.tsx"));
    assert!(survivor.orphaned_at.is_none());
    assert!(
        out.pending.is_empty(),
        "a pure move changed no content: nothing to re-synthesize"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (#41): the MATERIALIZATION of a greenfield `[NEW]` — a discovered bundle adopting
/// its name and binding its provenance — is REPORTED. It is the event that takes a node OUT of a
/// composition's `new_components` (so the coder stops being asked to build it), and it was the only
/// life-cycle event the inventory did not surface: `moved` and `orphans` were listed scrupulously,
/// this one changed a `[NEW]` into a `[DS]` in silence.
#[test]
fn materializing_a_greenfield_new_is_reported_by_the_inventory() {
    let root = scratch_dir("materialize");
    write_file(
        &root,
        "src/emails/PricingTable.tsx",
        "export const P = () => <td/>;",
    );

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    crate::db::upsert_component(
        &mut conn,
        &project.id,
        &crate::db::ComponentInput {
            name: "PricingTable".into(),
            template: "<section class=\"proto\"/>".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        out.materialized,
        vec![(
            "PricingTable".to_string(),
            "src/emails/PricingTable.tsx".to_string()
        )],
        "an adoption is an EVENT, and events are reported"
    );
    let row = get_component_by_name(&mut conn, &project.id, "PricingTable")
        .unwrap()
        .unwrap();
    assert_eq!(
        row.source_path.as_deref(),
        Some("src/emails/PricingTable.tsx")
    );
    assert_eq!(
        row.template, "<section class=\"proto\"/>",
        "adoption never clobbers the authored maquette"
    );
    // Idempotent: a second run adopts nothing more (the row now HAS a source).
    let again = super::inventory_project(&mut conn, &project.id).unwrap();
    assert!(again.materialized.is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

// ===========================================================================
// TOCTOU — the élagage judges a SNAPSHOT, so the DELETE re-judges itself
// (review 01KYHT7RN7E3557ZARNWGCY3R5, finding 01KYHV8DJ7XZG9F5TCRG2Q17Q1, CRITICAL)
// ===========================================================================

/// A FILE-BACKED `Db` — a real r2d2 pool (8 connections, WAL), the only shape in which two threads
/// can be inside the database at once: `Db::in_memory()` is capped at ONE connection, so it could
/// never reproduce the race. Returns the db and the scratch dir to remove (the repo tree lives in
/// `dir/front`, beside the `nyx.db` file, so the scan never walks the database itself).
fn file_backed_db(tag: &str) -> (crate::db::Db, std::path::PathBuf) {
    let dir = scratch_dir(tag);
    let db = crate::db::Db::open(&dir.join("nyx.db")).expect("open file-backed db");
    (db, dir)
}

/// DONE-CRITERION (CRITICAL 01KYHV8DJ7XZG9F5TCRG2Q17Q1) — THE RACE, run for real with the two REAL
/// MCP tools on a REAL pooled `Db`, at the delays the audit measured (+120 µs, +400 µs).
///
/// The sequence: 300 components catalogued, every source removed (a concurrent `git checkout`) but
/// one keeper so the `empty_scan` guard never fires; thread 1 runs `design_inventory`; thread 2
/// lands in the middle of its prune loop and runs `design_refresh_component` on `Z299` with an LLM
/// template. It used to return OK — the agent believing its synthesis stored — while the inventory,
/// judging a snapshot taken before that commit, reported `pruned_count = 300` and the `Z299` row no
/// longer existed.
///
/// What is pinned is the INVARIANT, whoever wins the race (that is why it is not flaky): a tool that
/// ANSWERED OK is never contradicted afterwards, and `pruned_count` equals the number of rows that
/// really left the catalogue — never the number the run intended to remove.
#[test]
fn a_refresh_that_returned_ok_is_never_destroyed_by_a_concurrent_inventory() {
    use serde_json::json;
    let (db, dir) = file_backed_db("toctou-race");
    let root = dir.join("front");
    let project_id = {
        write_file(
            &root,
            "src/ui/Keeper.tsx",
            "export const K = () => <b>K</b>;",
        );
        db.with_conn(|c| {
            let (project, _ws) = create_project(c, "Nyx", root.to_str().unwrap(), None).unwrap();
            create_repository(c, &project.id, "web", ".", false).unwrap();
            project.id
        })
    };

    for delay_us in [120u64, 400] {
        // (Re-)populate the front and catalogue it: 300 rows, none synthesized. The prune loop does
        // 1–2 DB round trips per row, so the window between the snapshot and each DELETE is wide.
        for i in 0..300 {
            write_file(
                &root,
                &format!("src/ui/Z{i}.tsx"),
                &format!("export const Z{i} = () => <b>{i}</b>;"),
            );
        }
        crate::mcp_tools_design::design_inventory(&db, &json!({ "project_id": project_id }))
            .expect("catalogue the front");
        // The concurrent `git checkout`: every source but the keeper is gone.
        for i in 0..300 {
            std::fs::remove_file(root.join(format!("src/ui/Z{i}.tsx"))).unwrap();
        }
        let before = db
            .with_conn(|c| list_components(c, &project_id))
            .unwrap()
            .len();

        let (inventory, refresh) = std::thread::scope(|s| {
            let inventory = s.spawn(|| {
                crate::mcp_tools_design::design_inventory(&db, &json!({"project_id": project_id}))
            });
            let refresh = s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_micros(delay_us));
                crate::mcp_tools_design::design_refresh_component(
                    &db,
                    &json!({
                        "project_id": project_id,
                        "name": "Z299",
                        "template": "<PRECIOUS-LLM/>",
                    }),
                )
            });
            (
                inventory.join().expect("inventory thread"),
                refresh.join().expect("refresh thread"),
            )
        });

        let inventory = inventory.expect("the inventory itself never fails");
        let after = db.with_conn(|c| list_components(c, &project_id)).unwrap();
        let z299 = after.iter().find(|c| c.name == "Z299");

        // (1) A tool that ANSWERED OK is never contradicted: the row and its template are there.
        if refresh.is_ok() {
            let row = z299.unwrap_or_else(|| {
                panic!("+{delay_us} µs: design_refresh_component returned OK — the row must exist")
            });
            assert_eq!(
                row.template, "<PRECIOUS-LLM/>",
                "+{delay_us} µs: the acknowledged template survives the concurrent élagage"
            );
        }
        // (2) `pruned_count` counts the DELETEs that really happened, not the ones intended.
        let pruned = inventory.result["pruned_count"].as_u64().unwrap() as usize;
        assert_eq!(
            pruned,
            before - after.len(),
            "+{delay_us} µs: pruned_count must equal the rows that really left the catalogue"
        );
        assert!(
            pruned >= 250,
            "+{delay_us} µs: the prune really ran ({pruned})"
        );
        // (3) A spared row is never reported as pruned.
        assert!(
            inventory.result["orphans"]
                .as_array()
                .unwrap()
                .iter()
                .all(|o| o["name"] != json!("Z299")
                    || o["pruned"] == json!(false)
                    || z299.is_none()),
            "+{delay_us} µs: a row still in the catalogue is never reported pruned"
        );
        // Clean slate for the next delay.
        db.with_conn(|c| {
            for row in list_components(c, &project_id).unwrap() {
                if row.name != "Keeper" {
                    crate::db::delete_component(c, &row.id).unwrap();
                }
            }
        });
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// DONE-CRITERION (CRITICAL 01KYHV8DJ7XZG9F5TCRG2Q17Q1) — the DETERMINISTIC pin of the same race:
/// the content is committed EXACTLY between the snapshot the guards judge and the DELETE that acts
/// on it. No timing, no threads — the interleaving the race can only produce by luck.
///
/// The conditional DELETE spares the row (`kept_because = "content_appeared"`), flags it orphan
/// (the source really is gone and the row really does hold something now), and does not count it as
/// pruned. `Gone`, which grew nothing, is still removed — the élagage a corrected glob must produce
/// is untouched.
#[test]
fn content_committed_between_the_snapshot_and_the_delete_is_spared_not_destroyed() {
    let root = scratch_dir("toctou-interleave");
    write_file(&root, "src/Stay.tsx", "export const S = () => <b>S</b>;");
    write_file(&root, "src/Raced.tsx", "export const R = () => <b>R</b>;");
    write_file(&root, "src/Gone.tsx", "export const G = () => <b>G</b>;");

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    super::inventory_project(&mut conn, &project.id).unwrap();

    // The two sources vanish (a `git checkout` in another session).
    std::fs::remove_file(root.join("src/Raced.tsx")).unwrap();
    std::fs::remove_file(root.join("src/Gone.tsx")).unwrap();

    // --- The inventory's pass 3, step by step ------------------------------
    // (a) the SNAPSHOT the guards judge: both rows are empty in it;
    let snapshot = list_components(&mut conn, &project.id).unwrap();
    assert!(
        snapshot
            .iter()
            .all(|c| c.template.is_empty() && c.visual_hash.is_none()),
        "the snapshot sees an empty catalogue — this is what used to authorise the DELETE"
    );
    // (b) the DISCOVERY (what pass 1 walks);
    let (roots, discovered) = discover_for_tests(&mut conn, &project.id);
    // (c) THE CONCURRENT COMMIT — another session's `design_refresh_component`, which has already
    //     answered OK to its agent, lands here;
    let raced = snapshot.iter().find(|c| c.name == "Raced").unwrap();
    update_component_content(&mut conn, &raced.id, "<PRECIOUS-LLM/>", None, None).unwrap();
    // (d) …and only THEN the prune runs, on the stale snapshot.
    let orphans = super::prune_orphans(
        &mut conn,
        &roots,
        &snapshot,
        &discovered,
        &std::collections::BTreeSet::new(),
    )
    .unwrap();

    let mut reported: Vec<(&str, bool, Option<&'static str>, bool)> = orphans
        .iter()
        .map(|o| (o.name.as_str(), o.pruned, o.kept_because, o.flagged_orphan))
        .collect();
    reported.sort();
    assert_eq!(
        reported,
        vec![
            ("Gone", true, None, false),
            ("Raced", false, Some(super::KEPT_CONTENT_APPEARED), true),
        ],
        "the DELETE itself refuses a row that grew content; the empty one is still pruned"
    );
    let survivor = get_component_by_name(&mut conn, &project.id, "Raced")
        .unwrap()
        .expect("the raced row survived");
    assert_eq!(survivor.template, "<PRECIOUS-LLM/>");
    assert!(survivor.orphaned_at.is_some(), "flagged for the human");
    assert!(
        get_component_by_name(&mut conn, &project.id, "Gone")
            .unwrap()
            .is_none(),
        "the élagage a glob change must produce is intact"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// DONE-CRITERION (audit 01KYHYW25GKJBRTZVTSXQAMP9F): an EXPLICIT delete landing between the
/// inventory's SNAPSHOT and its per-row WRITES makes the run SKIP that row — never abort.
///
/// The CRITICAL's fix gave the conditional DELETE a third outcome, `AlreadyGone`, precisely so a
/// concurrent `design_delete_component` (the `/design` card, another session's MCP call) would not
/// break the inventory: `prune_orphans` does `continue`. The other two per-row writes of the same
/// walk had no such treatment — `bind_component_source` used `get_result` without `.optional()`, so
/// the vanished row raised `NotFound`, the `?` carried it up to `inventory_project`, and a run whose
/// passes 2/3 had ALREADY committed failed in block. Nothing was destroyed; the robustness was just
/// asymmetric inside one family of race.
///
/// Driven the way `content_committed_between_the_snapshot_and_the_delete_is_spared_not_destroyed`
/// drives the prune: the passes are run explicitly so the delete can be injected exactly where a
/// concurrent one would land — between the snapshot and the write.
#[test]
fn a_row_deleted_under_the_inventory_is_skipped_like_already_gone() {
    let root = scratch_dir("toctou-rebind");
    write_file(
        &root,
        "src/old/Moving.tsx",
        "export const M = () => <b>M</b>;",
    );
    write_file(&root, "src/Stay.tsx", "export const S = () => <b>S</b>;");

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    super::inventory_project(&mut conn, &project.id).unwrap();
    // Both synthesized — a MOVE only has something to re-anchor once there IS a synthesis.
    for name in ["Moving", "Stay"] {
        let c = get_component_by_name(&mut conn, &project.id, name)
            .unwrap()
            .unwrap();
        update_component_content(&mut conn, &c.id, &format!("<b>{name}</b>"), None, None).unwrap();
        super::mark_synthesized(&mut conn, &project.id, name)
            .unwrap()
            .expect("hash recorded");
    }

    // The bundle MOVES on disk — the very case pass 2 re-anchors by CONTENT.
    std::fs::create_dir_all(root.join("src/new")).unwrap();
    std::fs::rename(
        root.join("src/old/Moving.tsx"),
        root.join("src/new/Moving.tsx"),
    )
    .unwrap();

    // --- The inventory's passes 1-2, step by step -------------------------
    // (a) the SNAPSHOT pass 2 judges + (b) the DISCOVERY pass 1 walks;
    let snapshot = list_components(&mut conn, &project.id).unwrap();
    let (roots, discovered) = discover_for_tests(&mut conn, &project.id);
    // (c) THE CONCURRENT COMMIT — the human hits "supprimer" on the /design card of the very row
    //     pass 2 is about to re-anchor (or another session's design_delete_component lands);
    let moving = snapshot.iter().find(|c| c.name == "Moving").unwrap();
    assert!(crate::db::delete_component(&mut conn, &moving.id).unwrap());
    // (d) …and only THEN the re-anchoring runs, on the stale snapshot.
    let moved = super::reconcile_moved_sources(&mut conn, &roots, &snapshot, &discovered)
        .expect("a row that vanished under the pass is not an error");
    assert!(
        moved.is_empty(),
        "nothing was re-anchored — the row it targeted is gone: {moved:?}",
    );

    // The primitive itself says it plainly: a vanished row is `None`, the twin of `AlreadyGone`.
    assert!(
        crate::db::bind_component_source(&mut conn, &moving.id, None, "src/new/Moving.tsx")
            .expect("no error on a vanished row")
            .is_none(),
    );

    // …and the FULL run completes and reports the rest: `Stay` keeps its identity and its synthesis,
    // the deleted one is simply re-discovered as a fresh (unsynthesized) row.
    let out = super::inventory_project(&mut conn, &project.id).expect("the inventory completes");
    let stay = get_component_by_name(&mut conn, &project.id, "Stay")
        .unwrap()
        .expect("the untouched component is still reported");
    assert_eq!(stay.template, "<b>Stay</b>", "its synthesis survived");
    assert!(out.orphans.iter().all(|o| o.name != "Stay"));
    let reborn = get_component_by_name(&mut conn, &project.id, "Moving")
        .unwrap()
        .expect("the moved bundle is catalogued again");
    assert_ne!(reborn.id, moving.id, "a NEW row — the old one was deleted");
    assert_eq!(reborn.source_path.as_deref(), Some("src/new/Moving.tsx"));

    let _ = std::fs::remove_dir_all(&root);
}

/// The DISCOVERY half of [`super::inventory_project`]'s pass 1, reproduced so a test can drive
/// [`super::prune_orphans`] directly and control WHAT HAPPENS BETWEEN the snapshot and the prune.
fn discover_for_tests(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> (Vec<super::RepoRoot>, Vec<super::DiscoveredBundle>) {
    let roots = super::project_repo_roots(conn, project_id).unwrap();
    let mut discovered = Vec::new();
    for repo in &roots {
        let globs = resolve_component_globs(detect_stack(&repo.root), None);
        for bundle in scan_bundles(&repo.root, &globs).bundles {
            discovered.push(super::DiscoveredBundle {
                repo_id: repo.repo_id.clone(),
                repo_label: repo.label.clone(),
                bundle,
            });
        }
    }
    (roots, discovered)
}

/// The SQL condition of [`crate::db::delete_component_if_unauthored`] and the Rust
/// [`super::carries_synthesis`] are ONE predicate expressed twice — a divergence between them is
/// exactly how a row with something to lose would slip through the conditional DELETE. Pinned over
/// every shape of the four authored fields, including the blank-but-present ones.
#[test]
fn the_sql_delete_condition_matches_carries_synthesis_exactly() {
    let root = scratch_dir("delete-condition");
    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();

    /// `(label, template, visual_hash, params_json, states_json)` — one row shape to pin.
    type Shape = (
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
    );
    let shapes: [Shape; 10] = [
        ("empty", "", None, None, None),
        ("blank_template", "  \n\t ", None, None, None),
        ("template", "<b/>", None, None, None),
        ("hash_only", "", Some("d41d8cd9"), None, None),
        ("params", "", None, Some("{\"label\":\"string\"}"), None),
        ("blank_params", "", None, Some("   "), None),
        ("empty_params", "", None, Some(""), None),
        ("states", "", None, None, Some("[\"hover\"]")),
        ("blank_states", "", None, None, Some(" ")),
        ("params_and_states", "", None, Some("{}"), Some("[]")),
    ];
    for (label, template, visual_hash, params_json, states_json) in shapes {
        let row = crate::db::upsert_component(
            &mut conn,
            &project.id,
            &crate::db::ComponentInput {
                name: label.into(),
                template: template.into(),
                visual_hash: visual_hash.map(str::to_string),
                params_json: params_json.map(str::to_string),
                states_json: states_json.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap();
        let authored = super::carries_synthesis(&row);
        let outcome = crate::db::delete_component_if_unauthored(&mut conn, &row.id).unwrap();
        assert_eq!(
            outcome == crate::db::ConditionalDelete::Deleted,
            !authored,
            "`{label}`: the SQL DELETE condition and carries_synthesis must agree"
        );
        if authored {
            assert_eq!(outcome, crate::db::ConditionalDelete::KeptContentAppeared);
            assert!(
                crate::db::get_component(&mut conn, &row.id)
                    .unwrap()
                    .is_some(),
                "`{label}`: a refused DELETE leaves the row exactly where it was"
            );
        }
    }
    // Where the two CANNOT agree (SQLite's `trim` knows no Unicode whitespace), they diverge in the
    // SAFE direction only: SQL spares what Rust calls blank, never the reverse.
    let nbsp = crate::db::upsert_component(
        &mut conn,
        &project.id,
        &crate::db::ComponentInput {
            name: "nbsp".into(),
            template: "\u{00a0}".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!super::carries_synthesis(&nbsp), "Rust calls a NBSP blank");
    assert_eq!(
        crate::db::delete_component_if_unauthored(&mut conn, &nbsp.id).unwrap(),
        crate::db::ConditionalDelete::KeptContentAppeared,
        "…and the DELETE spares it: the divergence only ever ADDS caution"
    );

    // A row that is ALREADY GONE is neither a deletion nor a sparing — the caller must be able to
    // tell, so it never reports a phantom row as pruned.
    let ghost = crate::db::upsert_component(
        &mut conn,
        &project.id,
        &crate::db::ComponentInput {
            name: "ghost".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(crate::db::delete_component(&mut conn, &ghost.id).unwrap());
    assert_eq!(
        crate::db::delete_component_if_unauthored(&mut conn, &ghost.id).unwrap(),
        crate::db::ConditionalDelete::AlreadyGone
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ===========================================================================
// AUTHORED CONTENT ≠ TEMPLATE — a `[NEW]` may carry only params/states
// (finding 01KYHV9F1JXCCKYMYYZR6Y1JYD, major)
// ===========================================================================

/// DONE-CRITERION (01KYHV9F1JXCCKYMYYZR6Y1JYD): `proto_author_component` makes the `template`
/// OPTIONAL, so a `[NEW]` can legitimately be born with ONLY a `params_json` / `states_json` — the
/// param shape and the declared states the prototype proposes. Greenfield it was safe (the prune
/// never considers a row with no source); the MATERIALIZATION the round trip is FOR
/// (`bind_component_source` gives it a source, never a template) then made it look empty, and the
/// disappearance of that file — the coder renaming the component he has just built — DESTROYED the
/// params and the states, after which `classify_elected_nodes` re-listed the frozen node as `[NEW]`.
#[test]
fn a_new_authored_with_only_params_and_states_survives_the_elagage() {
    let root = scratch_dir("params-only-new");
    write_file(
        &root,
        "src/ui/Widget.tsx",
        "export const W = () => <b>W</b>;",
    );
    // A second source, so the `empty_scan` guard never masks the verdict under test.
    write_file(
        &root,
        "src/ui/Keeper.tsx",
        "export const K = () => <b>K</b>;",
    );

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    // The `[NEW]`: authored WITHOUT a template, which the MCP schema explicitly allows.
    crate::db::upsert_component(
        &mut conn,
        &project.id,
        &crate::db::ComponentInput {
            name: "Widget".into(),
            template: String::new(),
            params_json: Some("{\"label\":\"string\"}".into()),
            states_json: Some("[\"idle\",\"hover\"]".into()),
            ..Default::default()
        },
    )
    .unwrap();

    // MATERIALIZED by the discovered bundle — a source, still no template.
    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        out.materialized,
        vec![("Widget".to_string(), "src/ui/Widget.tsx".to_string())]
    );
    let row = get_component_by_name(&mut conn, &project.id, "Widget")
        .unwrap()
        .unwrap();
    assert!(row.template.is_empty() && row.visual_hash.is_none());

    // The coder renames the component he just built: the source vanishes.
    std::fs::remove_file(root.join("src/ui/Widget.tsx")).unwrap();
    let out = super::inventory_project(&mut conn, &project.id).unwrap();

    assert_eq!(
        out.orphans
            .iter()
            .map(|o| (o.name.as_str(), o.pruned, o.kept_because, o.flagged_orphan))
            .collect::<Vec<_>>(),
        vec![("Widget", false, Some(super::KEPT_SYNTHESIZED), true)],
        "authored params + states are content no scan can re-derive"
    );
    let survivor = get_component_by_name(&mut conn, &project.id, "Widget")
        .unwrap()
        .expect("the row survived");
    assert_eq!(
        survivor.params_json.as_deref(),
        Some("{\"label\":\"string\"}"),
        "its params are intact"
    );
    assert_eq!(
        survivor.states_json.as_deref(),
        Some("[\"idle\",\"hover\"]"),
        "its states are intact"
    );
    assert!(survivor.orphaned_at.is_some(), "flagged for the human");

    let _ = std::fs::remove_dir_all(&root);
}

// ===========================================================================
// `orphaned_at` — only a run IN A POSITION TO JUDGE may lower it
// (finding 01KYHVAB5ZQN6K56SSSPV55EJY, minor)
// ===========================================================================

/// DONE-CRITERION (01KYHVAB5ZQN6K56SSSPV55EJY), the audit's three runs: a run that spares a row
/// because it COULD NOT TELL (`scan_incomplete` / `repo_not_scanned` / `empty_scan`) used to write
/// `orphaned_at = NULL`, i.e. erase the verdict of a run that COULD. In `/design`,
/// `isOrphan = orphaned_at != null`, so the card's badge, the "Orphelines" filter and
/// `flagged_orphan_count` all dropped to zero on a row that IS orphaned — and each flip re-raised
/// `catalogue_changed`, so a repo that blinks notified at every run.
#[cfg(unix)]
#[test]
fn a_run_that_cannot_judge_leaves_the_orphan_flag_exactly_as_it_found_it() {
    let root = scratch_dir("orphan-flag-persist");
    write_file(&root, "src/Kept.tsx", "export const K = () => <b>K</b>;");
    write_file(
        &root,
        "src/keep/Other.tsx",
        "export const O = () => <b>O</b>;",
    );
    write_file(
        &root,
        "src/hidden/Ghost.tsx",
        "export const G = () => <b>G</b>;",
    );

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    super::inventory_project(&mut conn, &project.id).unwrap();
    let kept = get_component_by_name(&mut conn, &project.id, "Kept")
        .unwrap()
        .unwrap();
    update_component_content(&mut conn, &kept.id, "<PRECIOUS-LLM/>", None, None).unwrap();
    super::mark_synthesized(&mut conn, &project.id, "Kept")
        .unwrap()
        .expect("hash recorded");

    // RUN 1 — clean scan, the source is gone: the verdict is pronounced and DATED.
    std::fs::remove_file(root.join("src/Kept.tsx")).unwrap();
    let run1 = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        run1.orphans
            .iter()
            .map(|o| (o.name.as_str(), o.kept_because, o.flagged_orphan))
            .collect::<Vec<_>>(),
        vec![("Kept", Some(super::KEPT_SYNTHESIZED), true)]
    );
    let flagged_at = get_component_by_name(&mut conn, &project.id, "Kept")
        .unwrap()
        .unwrap()
        .orphaned_at
        .expect("the date it was first found gone");

    // RUN 2 — ONE subtree ELSEWHERE in the repo becomes unreadable: this run cannot tell.
    let hidden = root.join("src/hidden");
    if !make_unreadable(&hidden) {
        let _ = std::fs::remove_dir_all(&root);
        return; // running as root: the OS refuses to produce the failure this test needs
    }
    let run2 = super::inventory_project(&mut conn, &project.id).unwrap();
    make_readable(&hidden);
    assert_eq!(
        run2.orphans
            .iter()
            .find(|o| o.name == "Kept")
            .map(|o| (o.kept_because, o.flagged_orphan)),
        Some((Some(super::KEPT_SCAN_INCOMPLETE), true)),
        "a run that cannot judge still REPORTS the flag a competent run raised"
    );
    assert_eq!(
        get_component_by_name(&mut conn, &project.id, "Kept")
            .unwrap()
            .unwrap()
            .orphaned_at,
        Some(flagged_at),
        "…and leaves `orphaned_at` at the DATE the competent run wrote"
    );
    assert!(
        !run2.catalogue_changed,
        "the flag does not flip, so `/design` is not notified for nothing"
    );

    // RUN 3 — readable again, the source still gone: same verdict, same date, no rewriting.
    let run3 = super::inventory_project(&mut conn, &project.id).unwrap();
    assert!(run3.scan_errors.is_clean());
    assert_eq!(
        get_component_by_name(&mut conn, &project.id, "Kept")
            .unwrap()
            .unwrap()
            .orphaned_at,
        Some(flagged_at),
        "an already-flagged orphan keeps its original date"
    );
    // And the ONE event that legitimately LOWERS it: the source comes back.
    write_file(&root, "src/Kept.tsx", "export const K = () => <b>K</b>;");
    super::inventory_project(&mut conn, &project.id).unwrap();
    assert!(
        get_component_by_name(&mut conn, &project.id, "Kept")
            .unwrap()
            .unwrap()
            .orphaned_at
            .is_none(),
        "a REDISCOVERY — and only a rediscovery — clears the flag"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ===========================================================================
// `read_bundle` — an unreadable file is not an empty one
// (finding 01KYHV1C2M1QNQ6A4Z1D00SC3B, minor)
// ===========================================================================

/// DONE-CRITERION (01KYHV1C2M1QNQ6A4Z1D00SC3B): the bundle READ used to `unwrap_or_default()` its
/// I/O, handing an unreadable file to the synthesizing agent as `bytes: 0, truncated: false,
/// content: ""` — indistinguishable from a legitimately empty one. The agent then synthesized on
/// PARTIAL matter without knowing it, and `design_refresh_component` marked that synthesis UP TO
/// DATE (the input hash folds the same "nothing" for the same file, so both hashes agreed).
///
/// The bundle carries a readable EMPTY sibling next to the unreadable one, so what is pinned is the
/// DISTINCTION, not merely the presence of a signal.
#[cfg(unix)]
#[test]
fn an_unreadable_bundle_file_is_signalled_not_served_as_empty() {
    use serde_json::json;
    let root = scratch_dir("bundle-read-error");
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );
    write_file(&root, "src/ui/Card.css", ".card { color: red }");
    write_file(&root, "src/ui/Card.scss", ""); // legitimately EMPTY, and readable

    let db = crate::db::Db::in_memory();
    let project_id = db.with_conn(|c| {
        let (project, _ws) = create_project(c, "Nyx", root.to_str().unwrap(), None).unwrap();
        create_repository(c, &project.id, "web", ".", false).unwrap();
        super::inventory_project(c, &project.id).unwrap();
        project.id
    });

    let unreadable = root.join("src/ui/Card.css");
    if !make_unreadable(&unreadable) {
        let _ = std::fs::remove_dir_all(&root);
        return; // running as root: the OS refuses to produce the failure this test needs
    }
    let read = db
        .with_conn(|c| super::read_bundle(c, &project_id, "Card", 32 * 1024))
        .unwrap()
        .expect("the bundle resolves");
    let file = |rel: &str| {
        read.files
            .iter()
            .find(|f| f.path == rel)
            .unwrap_or_else(|| panic!("{rel} listed"))
    };
    assert!(
        file("src/ui/Card.css").read_error.is_some(),
        "the FAILURE is carried to the agent: {:?}",
        file("src/ui/Card.css")
    );
    assert_eq!(
        (
            file("src/ui/Card.scss").read_error.as_deref(),
            file("src/ui/Card.scss").bytes,
            file("src/ui/Card.scss").content.as_str(),
        ),
        (None, 0, ""),
        "a genuinely EMPTY file is still just empty — the two are distinguishable"
    );
    assert_eq!(read.read_error_count, 1);
    assert!(file("src/ui/Card.tsx").read_error.is_none());

    // The MCP `mode: "bundle"` response COUNTS it, like the listing's `scan_error_count`, and each
    // failing file NAMES its failure — so the agent knows not to synthesize on this matter.
    let out = crate::mcp_tools_design::design_inventory(
        &db,
        &json!({ "project_id": project_id, "component": "Card" }),
    )
    .expect("the read shape never fails because of an unreadable file");
    make_readable(&unreadable);
    assert_eq!(out.result["mode"], json!("bundle"));
    assert_eq!(out.result["read_error_count"], json!(1));
    assert!(
        out.result["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == json!("src/ui/Card.css") && f["read_error"].is_string()),
        "the failing file names its failure: {}",
        out.result["files"]
    );

    let _ = std::fs::remove_dir_all(&root);
}

// =============================================================================
// The BUNDLE RECIPE (finding 01KYKXPHW103C0V5RBXQ29BBXQ) — the framework knowledge left nyx
// =============================================================================

/// NON-REGRESSION, at the level the finding demands: for EVERY detected stack, the DEFAULT recipe
/// discovers EXACTLY what the pre-recipe, hard-coded ladder discovered — same bundles, same entries,
/// same files, same ROLES and the same INPUT HASHES (so the incremental loop cannot even flinch).
///
/// The two sides are genuinely different code paths: the right-hand one goes through
/// [`recipe_from_legacy_globs`] (the extension ladder that reproduces the old `entry_priority`),
/// which is also what every legacy `discover_bundles(root, globs)` caller now uses.
///
/// The tree deliberately mixes the shapes that used to be decided by hard-coded knowledge: an
/// Angular class + `templateUrl`, an Angular v20 `x.ts` + `x.html`, a template-only
/// `.component.html`, a React `.tsx` with css/variants siblings, a Vue SFC, a Svelte SFC, a
/// non-component `.service.ts`, a spec and a `.d.ts`.
#[test]
fn the_default_recipe_reproduces_the_hard_coded_discovery_exactly() {
    let root = scratch_dir("recipe-iso");
    write_file(
        &root,
        "src/app/button/button.component.ts",
        "@Component({ selector: 'app-button', templateUrl: './button.component.html' })\nexport class ButtonComponent {}",
    );
    write_file(
        &root,
        "src/app/button/button.component.html",
        "<button></button>",
    );
    write_file(&root, "src/app/button/button.component.scss", ".b{}");
    write_file(
        &root,
        "src/app/button/button.variants.ts",
        "export const v = {};",
    );
    write_file(
        &root,
        "src/app/button/button.service.ts",
        "export class S {}",
    );
    // Angular v20 layout — the shape that once cost the reference front 177 of its 179 components.
    write_file(
        &root,
        "src/app/chip/chip.ts",
        "@Component({ selector: 'app-chip', templateUrl: './chip.html' })\nexport class Chip {}",
    );
    write_file(&root, "src/app/chip/chip.html", "<span></span>");
    write_file(&root, "src/app/orphan/legacy.component.html", "<div></div>");
    write_file(&root, "src/app/store/counter.ts", "export const store = 1;");
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );
    write_file(&root, "src/ui/Card.css", ".card{}");
    write_file(&root, "src/ui/Card.variants.ts", "export const cv = {};");
    write_file(&root, "src/ui/Card.test.tsx", "it('x', () => {});");
    write_file(&root, "src/ui/types.d.ts", "export type T = 1;");
    write_file(&root, "src/ui/Panel.vue", "<template><div/></template>");
    write_file(&root, "src/ui/Drawer.svelte", "<div/>");

    for stack in [
        ComponentStack::React,
        ComponentStack::Vue,
        ComponentStack::Svelte,
        ComponentStack::Angular,
        ComponentStack::Generic,
    ] {
        let legacy = scan_bundles(&root, &default_component_globs(stack));
        let by_recipe = scan_bundles_with(&root, &default_recipe(stack));
        assert_eq!(
            by_recipe, legacy,
            "{stack:?}: the default recipe must discover EXACTLY what the hard-coded ladder did"
        );
        assert!(
            legacy.errors.is_clean(),
            "{stack:?}: the fixture reads cleanly"
        );
    }

    // And the shapes that mattered are really there (the assertion above would also pass on two
    // empty results).
    let angular = scan_bundles_with(&root, &default_recipe(ComponentStack::Angular)).bundles;
    let chip = bundle(&angular, "chip");
    assert_eq!(
        paths(chip),
        vec!["src/app/chip/chip.html", "src/app/chip/chip.ts"],
        "the v20 `x.ts` + `x.html` bundle, template resolved through `templateUrl`"
    );
    assert!(
        angular
            .iter()
            .all(|b| b.name != "button.service" && b.name != "counter"),
        "the `.service.ts` and the undecorated store are still not components: {:?}",
        angular.iter().map(|b| b.name.as_str()).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// THE PROOF the framework knowledge really left nyx (done-criterion #2): a convention NO stack in
/// this codebase knows about becomes fully inventoriable by writing ONLY A RECIPE — not one line of
/// Rust is aware of `.widget.js`, of `defineWidget(`, of a `view:` attribute or of a `.skin.css`.
///
/// The invented convention: the entry is a `*.widget.js` that CALLS `defineWidget(`; its template is
/// named by a `view:` attribute of that call; its styles are a `.skin.css` neighbour; `*.spec.js` is
/// excluded. A `.widget.js` that does NOT call the factory is not a component, and neither is a
/// plain `.js` util.
#[test]
fn an_unwired_framework_becomes_inventoriable_by_writing_only_a_recipe() {
    let root = scratch_dir("recipe-invented");
    write_file(
        &root,
        "src/widgets/counter.widget.js",
        "import { defineWidget } from '@house/core';\n\
         export default defineWidget({\n  view: './counter.view.html',\n  props: ['value'],\n});",
    );
    write_file(&root, "src/widgets/counter.view.html", "<b>{{value}}</b>");
    write_file(&root, "src/widgets/counter.skin.css", ".counter{color:red}");
    write_file(
        &root,
        "src/widgets/gauge.widget.js",
        "import { defineWidget } from '@house/core';\nexport default defineWidget({ props: [] });",
    );
    // NOT components: a helper that never calls the factory, and a spec.
    write_file(
        &root,
        "src/widgets/helpers.js",
        "export const clamp = () => {};",
    );
    write_file(
        &root,
        "src/widgets/notawidget.widget.js",
        "export const notAWidget = 1;",
    );
    write_file(&root, "src/widgets/counter.spec.js", "test('x', () => {});");

    // 1. nyx knows NOTHING about this stack: with the default recipe it discovers nothing at all.
    let stack = detect_stack(&root);
    assert_eq!(
        stack,
        ComponentStack::Generic,
        "no manifest → unknown stack"
    );
    assert!(
        scan_bundles_with(&root, &default_recipe(stack))
            .bundles
            .is_empty(),
        "the built-in knowledge cannot see a `.widget.js` front — that is the whole point"
    );

    // 2. The AGENT writes the convention it read in the repo. This is DATA, not code.
    let recipe = BundleRecipe::from_json(
        r#"{
            "version": 1,
            "entries": [
                {
                    "globs": ["**/*.widget.js"],
                    "requires_content": "defineWidget(",
                    "template_attribute": "view"
                }
            ],
            "siblings": [
                { "extensions": ["html"], "role": "template" },
                { "extensions": ["css"], "role": "style" }
            ],
            "family_qualifiers": ["widget", "view", "skin"],
            "exclude": ["**/*.spec.*"]
        }"#,
    )
    .expect("the recipe is valid");

    // 3. nyx applies it deterministically.
    let scan = scan_bundles_with(&root, &recipe);
    assert!(scan.errors.is_clean(), "clean walk: {:?}", scan.errors);
    let names: Vec<&str> = scan.bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["counter", "gauge"],
        "exactly the two widgets that CALL the factory — the helper, the undecorated `.widget.js` \
         and the spec are out"
    );
    let counter = bundle(&scan.bundles, "counter");
    assert_eq!(counter.entry_path, "src/widgets/counter.widget.js");
    assert_eq!(
        counter
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.role))
            .collect::<Vec<_>>(),
        vec![
            ("src/widgets/counter.skin.css", BundleRole::Style),
            ("src/widgets/counter.view.html", BundleRole::Template),
            ("src/widgets/counter.widget.js", BundleRole::Entry),
        ],
        "the `view:` attribute resolved the template and the `.skin.css` took the style role"
    );

    // 4. The INCREMENTAL loop still works on it: an edit to the recipe-resolved TEMPLATE moves the
    //    bundle's input hash (it is a real member of the bundle, not decoration).
    let before = counter.input_hash.clone();
    write_file(&root, "src/widgets/counter.view.html", "<b>{{value}}!</b>");
    let after = bundle(&scan_bundles_with(&root, &recipe).bundles, "counter")
        .input_hash
        .clone();
    assert_ne!(before, after, "the template is folded into the input hash");

    let _ = std::fs::remove_dir_all(&root);
}

/// THE ESCAPE HATCH (done-criterion #3): a component NO rule can express — its entry carries no
/// marker, its template lives in another directory under another base name, its style under a third
/// — is PINNED explicitly and inventoried verbatim, hash and all.
///
/// The pin also WINS over the rules: the file it claims as its template never also stands as a
/// component of its own.
#[test]
fn an_explicitly_pinned_component_is_inventoried_verbatim() {
    let root = scratch_dir("recipe-pin");
    write_file(
        &root,
        "src/legacy/gauge.js",
        "window.Gauge = function () {};",
    );
    write_file(
        &root,
        "src/shared/markup/dial.html",
        "<div class='dial'></div>",
    );
    write_file(&root, "src/shared/styles/dial-theme.css", ".dial{}");
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );

    // The rules alone: the React component is found, the legacy widget is invisible, and the
    // stray markup would open a bundle of its own if a rule ever claimed `.html`.
    let mut recipe = default_recipe(ComponentStack::React);
    recipe.entries.push(
        BundleRecipe::from_json(r#"{"version":1,"entries":[{"globs":["**/*.html"]}]}"#)
            .unwrap()
            .entries
            .remove(0),
    );
    let before: Vec<String> = scan_bundles_with(&root, &recipe)
        .bundles
        .iter()
        .map(|b| b.name.clone())
        .collect();
    assert_eq!(
        before,
        vec!["Card".to_string(), "dial".to_string()],
        "without the pin, the markup stands alone and the legacy widget does not exist"
    );

    // The AGENT pins it: THESE are the files of THAT component.
    recipe.pins = serde_json::from_str(
        r#"[{
            "entry": "src/legacy/gauge.js",
            "name": "legacy-gauge",
            "files": [
                { "path": "src/shared/markup/dial.html", "role": "template" },
                { "path": "src/shared/styles/dial-theme.css", "role": "style" }
            ]
        }]"#,
    )
    .expect("the pins parse");
    recipe.validate().expect("the pinned recipe is valid");

    let scan = scan_bundles_with(&root, &recipe);
    assert!(scan.errors.is_clean(), "clean walk: {:?}", scan.errors);
    let names: Vec<&str> = scan.bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Card", "legacy-gauge"],
        "the pin ADDED its component and TOOK the markup out of the rule-driven set"
    );
    let pinned = bundle(&scan.bundles, "legacy-gauge");
    assert_eq!(pinned.entry_path, "src/legacy/gauge.js");
    assert_eq!(
        pinned
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.role))
            .collect::<Vec<_>>(),
        vec![
            ("src/legacy/gauge.js", BundleRole::Entry),
            ("src/shared/markup/dial.html", BundleRole::Template),
            ("src/shared/styles/dial-theme.css", BundleRole::Style),
        ],
        "the declared files, with the declared roles, whatever the rules say"
    );

    // The incremental loop covers a pinned bundle exactly like any other: a change to a file that
    // NO rule would ever have associated with this component still moves its hash.
    let before_hash = pinned.input_hash.clone();
    write_file(
        &root,
        "src/shared/styles/dial-theme.css",
        ".dial{color:red}",
    );
    assert_ne!(
        bundle(&scan_bundles_with(&root, &recipe).bundles, "legacy-gauge").input_hash,
        before_hash,
        "a pinned file is a real bundle member"
    );

    // A pin naming a file that is NOT there is FAIL-CLOSED: the scan reports the failure (so the
    // élagage prunes nothing) rather than silently shipping a truncated bundle.
    std::fs::remove_file(root.join("src/shared/markup/dial.html")).unwrap();
    let scan = scan_bundles_with(&root, &recipe);
    assert!(
        !scan.errors.is_clean(),
        "a missing pinned file is visible, never silent"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// CONFINEMENT (audit 01KYM2RKGHY1WE9HTWEAJ2N2SZ — a pin could make nyx READ outside the repo).
///
/// `normalized_repo_path` only ever filtered `..`, while `fs::metadata` / `read_bounded` FOLLOW
/// symlinks — so a symlink already present inside the repo let a pin pull a file from anywhere on
/// the disk into `bundle.files`, into the input hash, and (through `read_bundle`) into the agent's
/// context. The walk itself never did that (`entry.file_type()` is an `lstat`), so the pins held a
/// capability the discovery does not.
///
/// The two cases the audit demonstrated — a FILE symlink (`src/leak.ts -> <outside>/passwd`) and a
/// DIRECTORY symlink (`src/outside -> <outside>/`) — each as a pin's `files[]` AND as a pin's
/// `entry`.
#[cfg(unix)]
#[test]
fn a_pin_never_reads_through_a_symlink_out_of_the_repo() {
    use std::os::unix::fs::symlink;

    let root = scratch_dir("recipe-pin-symlink");
    let outside = scratch_dir("recipe-pin-outside");
    write_file(&outside, "passwd", "root:x:0:0:TOP SECRET");
    write_file(&root, "src/button.ts", "export class Button {}");
    std::fs::create_dir_all(root.join("src")).unwrap();
    symlink(outside.join("passwd"), root.join("src/leak.ts")).expect("file symlink");
    symlink(&outside, root.join("src/outside")).expect("directory symlink");

    let mut recipe = BundleRecipe::from_json(r#"{"version":1,"entries":[{"globs":["**/*.ts"]}]}"#)
        .expect("a bare recipe");

    // 1. The WALK already ignores both symlinks — only the real file is discovered. That is the
    //    reference behaviour the pins must not exceed.
    let scan = scan_bundles_with(&root, &recipe);
    assert_eq!(
        scan.bundles
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        vec!["button"],
        "the walk never follows a symlink, so `leak.ts` is not a candidate entry"
    );
    assert!(scan.errors.is_clean(), "{:?}", scan.errors);

    // 2. A pin naming BOTH symlinked paths in its `files[]`: they are REFUSED, not read.
    recipe.pins = serde_json::from_str(
        r#"[{
            "entry": "src/button.ts",
            "files": [
                { "path": "src/outside/passwd", "role": "style" },
                { "path": "src/leak.ts", "role": "variants" }
            ]
        }]"#,
    )
    .expect("the pins parse");
    recipe.validate().expect("lexically, these paths are fine");

    let scan = scan_bundles_with(&root, &recipe);
    let pinned = bundle(&scan.bundles, "button");
    assert_eq!(
        paths(pinned),
        vec!["src/button.ts"],
        "NOTHING outside the repo enters the bundle — so `read_bundle` cannot hand its content out"
    );
    assert_eq!(
        scan.errors.count, 2,
        "one refusal per escaping path, fail-closed: {:?}",
        scan.errors
    );
    assert!(
        scan.errors
            .sample
            .iter()
            .all(|e| e.kind == "outside_repo" && e.message.contains("SYMLINK")),
        "the refusal NAMES what it refused: {:?}",
        scan.errors.sample
    );

    // 3. Editing ONLY the file OUTSIDE the repo does not move the component's input hash any more
    //    (the audit's decisive measurement: a89a707d… → 61eb58df… used to follow such an edit).
    let hash_before = pinned.input_hash.clone();
    write_file(&outside, "passwd", "root:x:0:0:TOP SECRET (edited)");
    assert_eq!(
        bundle(&scan_bundles_with(&root, &recipe).bundles, "button").input_hash,
        hash_before,
        "an edit outside the repo is invisible to the incremental loop, as it always claimed to be"
    );

    // 4. The same refusal when the symlink is the pin's ENTRY — through the file symlink…
    recipe.pins =
        serde_json::from_str(r#"[{ "entry": "src/leak.ts", "name": "leak" }]"#).expect("parses");
    let scan = scan_bundles_with(&root, &recipe);
    assert!(
        scan.bundles.iter().all(|b| b.name != "leak"),
        "a pinned entry that is a symlink is not a component"
    );
    assert_eq!(scan.errors.count, 1, "{:?}", scan.errors);
    assert_eq!(scan.errors.sample[0].kind, "outside_repo");

    // … and through the DIRECTORY symlink (the case a tail-only check would miss).
    recipe.pins = serde_json::from_str(r#"[{ "entry": "src/outside/passwd", "name": "leak" }]"#)
        .expect("parses");
    let scan = scan_bundles_with(&root, &recipe);
    assert!(scan.bundles.iter().all(|b| b.name != "leak"));
    assert_eq!(scan.errors.count, 1, "{:?}", scan.errors);
    assert_eq!(scan.errors.sample[0].kind, "outside_repo");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

/// CONFINEMENT UNDER CONCURRENCY (audit 01KYM9KVQ0M1PA3AMA7MYCSE93 — the check→open TOCTOU).
///
/// The first fix CHECKED the path and handed back a path the callers RE-OPENED. Between the two, a
/// concurrent writer could swap a segment for a symlink out of the repo — and nyx is MULTI-SESSION,
/// so an agent writing a repo while an inventory walks it is the normal case, not an attack. The
/// audit measured, with a writer alternating `src/race.ts` between a real file and a symlink to a
/// file outside the repo:
///   * 2062 of 30000 reads (~7 %) made with `read_bundle`'s own expression returned OUT-OF-REPO
///     BYTES — i.e. the secret reached the agent's context;
///   * 222 of 3000 scans produced EXACTLY the input hash of the out-of-repo secret folded in whole,
///     while `bundle_input_hash` asserted that an edit outside the repo could never move a hash.
///
/// This replays that bench against [`open_confined`] (`openat` + `O_NOFOLLOW` per segment, the read
/// done on the verified DESCRIPTOR) and demands ZERO of both, over >= 10 000 attempts. It has teeth
/// twice over: a DETERMINISTIC control first (the symlink in place, no concurrency at all — the
/// read is refused and the hash is the "unreadable" one, never the secret's), then the race itself,
/// which must actually HAPPEN (the refusal counter is asserted non-zero).
///
/// `NYX_CONFINEMENT_RACE_ITERS` raises the iteration count for a longer soak.
#[cfg(unix)]
#[test]
fn a_concurrent_symlink_swap_never_leaks_a_byte_or_a_hash_from_outside_the_repo() {
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const SECRET: &str = "root:x:0:0:TOP SECRET — this file lives OUTSIDE the scanned repo";
    const REAL: &str = "export class Race {}";
    const RACED: &str = "src/race.ts";

    let iterations: usize = std::env::var("NYX_CONFINEMENT_RACE_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);

    let root = scratch_dir("confinement-race");
    let outside = scratch_dir("confinement-race-outside");
    write_file(&outside, "secret.ts", SECRET);
    write_file(&root, RACED, REAL);

    let files = vec![BundleFile {
        path: RACED.to_string(),
        role: BundleRole::Entry,
    }];
    let hash_in = |dir: &std::path::Path| {
        let mut errors = ScanErrors::default();
        bundle_input_hash(dir, "src", &files, &mut errors)
    };

    // The THREE reference digests, each computed off the race, in a tree of its own:
    //   * LEGIT — the real repo file;
    //   * UNREADABLE — the file is not there (an open that fails folds in the NAME only);
    //   * LEAKED — the out-of-repo secret folded in whole. This is the hash the audit SAW.
    let legit_tree = scratch_dir("confinement-race-legit");
    write_file(&legit_tree, RACED, REAL);
    let legit_hash = hash_in(&legit_tree);
    let unreadable_tree = scratch_dir("confinement-race-unreadable");
    std::fs::create_dir_all(unreadable_tree.join("src")).expect("mkdir");
    let unreadable_hash = hash_in(&unreadable_tree);
    let leaked_tree = scratch_dir("confinement-race-leaked");
    write_file(&leaked_tree, RACED, SECRET);
    let leaked_hash = hash_in(&leaked_tree);
    assert_ne!(legit_hash, leaked_hash);
    assert_ne!(unreadable_hash, leaked_hash);

    // The exact expression `read_bundle` uses to turn a bundle file into bytes for the agent.
    let read_like_read_bundle = |root: &std::path::Path| {
        open_confined(root, RACED)
            .and_then(|mut file| read_bounded(&mut file, 64 * 1024).map_err(ConfinedOpenError::Io))
    };

    // --- CONTROL, no concurrency: the symlink simply IS the file. -------------------------------
    std::fs::remove_file(root.join(RACED)).expect("drop the real file");
    symlink(outside.join("secret.ts"), root.join(RACED)).expect("symlink in place");
    match read_like_read_bundle(&root) {
        Err(ConfinedOpenError::OutsideRepo(message)) => {
            assert!(message.contains("SYMLINK"), "{message}")
        }
        other => panic!("a symlinked bundle file must be REFUSED, got {other:?}"),
    }
    assert_eq!(
        hash_in(&root),
        unreadable_hash,
        "a refused file folds in its NAME only — never the secret's bytes"
    );
    std::fs::remove_file(root.join(RACED)).expect("drop the symlink");
    write_file(&root, RACED, REAL);

    // --- THE RACE: a writer swaps the two states as fast as it can, atomically. ------------------
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (root, outside, stop) = (root.clone(), outside.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let target = root.join(RACED);
            let staged_link = root.join("src/.race.staged-link");
            let staged_file = root.join("src/.race.staged-file");
            let mut swaps = 0usize;
            while !stop.load(Ordering::Relaxed) {
                let _ = std::fs::remove_file(&staged_link);
                symlink(outside.join("secret.ts"), &staged_link).expect("stage the symlink");
                std::fs::rename(&staged_link, &target).expect("swap in the symlink");
                std::fs::write(&staged_file, REAL).expect("stage the real file");
                std::fs::rename(&staged_file, &target).expect("swap in the real file");
                swaps += 1;
            }
            swaps
        })
    };

    let mut leaked_reads = 0usize;
    let mut refused_reads = 0usize;
    let mut seen_hashes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for _ in 0..iterations {
        match read_like_read_bundle(&root) {
            Ok((head, _len)) => {
                if String::from_utf8_lossy(&head).contains("TOP SECRET") {
                    leaked_reads += 1;
                }
            }
            Err(_) => refused_reads += 1,
        }
        seen_hashes.insert(hash_in(&root));
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = writer.join().expect("the writer thread finished");

    eprintln!(
        "confinement race: {iterations} reads + {iterations} hashes against {swaps} swaps — \
         {leaked_reads} leaked byte-reads, {refused_reads} refusals, hashes seen: {}",
        seen_hashes.len()
    );
    assert_eq!(
        leaked_reads, 0,
        "{leaked_reads}/{iterations} reads returned bytes from OUTSIDE the repo"
    );
    assert!(
        !seen_hashes.contains(&leaked_hash),
        "an input hash folded in the out-of-repo secret"
    );
    for hash in &seen_hashes {
        assert!(
            *hash == legit_hash || *hash == unreadable_hash,
            "a scan produced an input hash that is neither the repo file's nor the unreadable \
             one: {hash}"
        );
    }
    assert!(
        refused_reads > 0,
        "the bench never caught the symlink state ({swaps} swaps): it proves nothing — re-run, or \
         raise NYX_CONFINEMENT_RACE_ITERS"
    );

    // --- THE SAME RACE ON AN INTERMEDIATE DIRECTORY SEGMENT -------------------------------------
    //
    // `src` itself becomes a symlink to a directory outside the repo (which holds its own
    // `race.ts`, full of the secret). This is the vector a TAIL-ONLY check misses, and the one the
    // `O_DIRECTORY | O_NOFOLLOW` on every intermediate segment answers. The swap cannot be atomic
    // (`rename` will not exchange a directory for a symlink), so the reader also sees a moment
    // where `src` is simply absent — an I/O failure, which is fail-closed like any other.
    let outside_dir = scratch_dir("confinement-race-outside-dir");
    write_file(&outside_dir, "race.ts", SECRET);
    for staged in [".race.staged-link", ".race.staged-file"] {
        let _ = std::fs::remove_file(root.join("src").join(staged));
    }
    write_file(&root, RACED, REAL);

    let stop = Arc::new(AtomicBool::new(false));
    let dir_writer = {
        let (root, outside_dir, stop) = (root.clone(), outside_dir.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let src = root.join("src");
            let stash = root.join(".race-src-stash");
            let mut swaps = 0usize;
            while !stop.load(Ordering::Relaxed) {
                std::fs::rename(&src, &stash).expect("stash the real directory");
                symlink(&outside_dir, &src).expect("swap in the directory symlink");
                std::fs::remove_file(&src).expect("drop the directory symlink");
                std::fs::rename(&stash, &src).expect("restore the real directory");
                swaps += 1;
            }
            swaps
        })
    };

    let mut dir_leaked_reads = 0usize;
    let mut dir_refused_reads = 0usize;
    let mut dir_hashes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for _ in 0..iterations {
        match read_like_read_bundle(&root) {
            Ok((head, _len)) => {
                if String::from_utf8_lossy(&head).contains("TOP SECRET") {
                    dir_leaked_reads += 1;
                }
            }
            Err(_) => dir_refused_reads += 1,
        }
        dir_hashes.insert(hash_in(&root));
    }
    stop.store(true, Ordering::Relaxed);
    let dir_swaps = dir_writer.join().expect("the writer thread finished");

    eprintln!(
        "confinement race (DIRECTORY segment): {iterations} reads + {iterations} hashes against \
         {dir_swaps} swaps — {dir_leaked_reads} leaked byte-reads, {dir_refused_reads} refusals, \
         hashes seen: {}",
        dir_hashes.len()
    );
    assert_eq!(
        dir_leaked_reads, 0,
        "{dir_leaked_reads}/{iterations} reads returned bytes from OUTSIDE the repo through a \
         symlinked DIRECTORY segment"
    );
    assert!(!dir_hashes.contains(&leaked_hash));
    for hash in &dir_hashes {
        assert!(
            *hash == legit_hash || *hash == unreadable_hash,
            "a scan produced an input hash that is neither the repo file's nor the unreadable \
             one: {hash}"
        );
    }
    assert!(
        dir_refused_reads > 0,
        "the bench never caught the symlinked-directory state ({dir_swaps} swaps): it proves \
         nothing — re-run, or raise NYX_CONFINEMENT_RACE_ITERS"
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
    let _ = std::fs::remove_dir_all(&outside_dir);
    let _ = std::fs::remove_dir_all(&legit_tree);
    let _ = std::fs::remove_dir_all(&unreadable_tree);
    let _ = std::fs::remove_dir_all(&leaked_tree);
}

/// THE NAME OF A PIN COMES FROM THE RECIPE (audit 01KYM2W7KH89X33091AQJ2PN5H).
///
/// A pin without a `name` used to be named through nyx's HARD-CODED `DEFAULT_FAMILY_QUALIFIERS`
/// (`legacy_recipe_for_any_entry()`), not through the project's. Two measured consequences, both
/// pinned here:
///   * the SAME file was named `card` when a rule found it and `card.ui` when a pin did, on a
///     project whose recipe declares `family_qualifiers: ["ui"]`;
///   * a pinned `payment.service.js` was renamed `payment` by nyx's Angular list and COLLIDED with
///     the `payment` the rules had discovered — two homonymous components out of a convention the
///     recipe never expressed.
#[test]
fn a_pin_is_named_by_the_projects_recipe_never_by_nyxs_defaults() {
    let root = scratch_dir("recipe-pin-name");
    write_file(&root, "src/kite/card.ui.mjs", "export const Card = 1;");
    write_file(&root, "src/pay/payment.js", "export const Payment = 1;");
    write_file(&root, "src/pay/payment.service.js", "export const Api = 1;");

    // The project's OWN convention: `.ui` is the family qualifier, and nothing else is. `service`,
    // `component`, `spec`… are nyx's idea of a convention, not this project's.
    let mut recipe = BundleRecipe::from_json(
        r#"{"version":1,
            "entries":[{"globs":["**/*.mjs","**/*.js"]}],
            "family_qualifiers":["ui"]}"#,
    )
    .expect("a recipe with the project's own qualifiers");

    // 1. BY RULE: `card.ui.mjs` → `card` (the recipe's `ui` is stripped),
    //    `payment.service.js` → `payment.service` (the recipe does NOT declare `service`).
    let by_rule: Vec<String> = scan_bundles_with(&root, &recipe)
        .bundles
        .iter()
        .map(|b| b.name.clone())
        .collect();
    assert_eq!(by_rule, vec!["card", "payment", "payment.service"]);

    // 2. BY PIN, with no `name`: the SAME names, because the SAME recipe derives them.
    recipe.pins = serde_json::from_str(
        r#"[{ "entry": "src/kite/card.ui.mjs" },
            { "entry": "src/pay/payment.service.js" }]"#,
    )
    .expect("the pins parse");
    recipe.validate().expect("valid");
    let scan = scan_bundles_with(&root, &recipe);
    assert!(scan.errors.is_clean(), "{:?}", scan.errors);
    let by_pin: Vec<String> = scan.bundles.iter().map(|b| b.name.clone()).collect();
    assert_eq!(
        by_pin, by_rule,
        "a pin overrides the FILES of a component, never the naming rule — and the naming rule is \
         the PROJECT's (nyx's own list would have said `card.ui` and `payment`)"
    );
    assert_eq!(
        bundle(&scan.bundles, "card").entry_path,
        "src/kite/card.ui.mjs"
    );
    assert_eq!(
        scan.bundles.iter().filter(|b| b.name == "payment").count(),
        1,
        "no homonym: nyx's hard-coded `service` qualifier is NOT applied to a recipe that never \
         declared it"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// EVERY ANNOUNCED BOUND IS APPLIED, AT THE VALUE IT ANNOUNCES, AND NAMES ITS FIELD
/// (audit 01KYM2SSV6CGGERA9RGNQ3JCRW — seven bounds, zero tests, and one of them contradicted by
/// the code that applied it: `MAX_QUALIFIERS` said 64 while `check_patterns` capped at 32, so 33
/// qualifiers were refused with "max 32" and 65 with "capped at 64").
///
/// Each bound is exercised at LIMIT (accepted) and LIMIT+1 (refused, message naming the field).
#[test]
fn every_announced_recipe_bound_is_applied_and_names_its_field() {
    // A list of `count` distinct SHORT patterns — short so a COUNT bound is never masked by the
    // TOTAL byte budget, distinct so nothing can be deduplicated away.
    let list = |count: usize| -> String {
        (0..count)
            .map(|i| format!("\"p{i}\""))
            .collect::<Vec<_>>()
            .join(",")
    };
    let ok = |json: String| BundleRecipe::from_json(&json);
    // `field` must be named by the refusal, and the recipe one notch below must be ACCEPTED.
    let bound = |field: &str, at_limit: String, over: String| {
        ok(at_limit).unwrap_or_else(|e| panic!("`{field}` at its limit must be accepted: {e}"));
        let err = ok(over).expect_err(&format!("`{field}` past its limit must be refused"));
        assert!(
            err.contains(field),
            "the refusal must NAME the field `{field}`: {err}"
        );
    };

    // MAX_ENTRY_RULES = 32
    let rules = |n: usize| {
        let body = (0..n)
            .map(|i| format!("{{\"globs\":[\"g{i}\"]}}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"version\":1,\"entries\":[{body}]}}")
    };
    bound("entries", rules(32), rules(33));

    // MAX_SIBLING_RULES = 32
    let siblings = |n: usize| {
        let body = (0..n)
            .map(|_| "{\"role\":\"style\"}".to_string())
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"version\":1,\"entries\":[{{\"globs\":[\"a\"]}}],\"siblings\":[{body}]}}")
    };
    bound("siblings", siblings(32), siblings(33));

    // MAX_GLOBS_PER_RULE = 32 — on every list it guards.
    let per_rule = |field: &str, n: usize| {
        match field {
        "entries[0].globs" => format!("{{\"version\":1,\"entries\":[{{\"globs\":[{}]}}]}}", list(n)),
        "entries[0].extensions" => format!(
            "{{\"version\":1,\"entries\":[{{\"globs\":[\"a\"],\"extensions\":[{}]}}]}}",
            list(n)
        ),
        "entries[0].qualifiers" => format!(
            "{{\"version\":1,\"entries\":[{{\"globs\":[\"a\"],\"qualifiers\":[{}]}}]}}",
            list(n)
        ),
        "siblings[0].extensions" => format!(
            "{{\"version\":1,\"entries\":[{{\"globs\":[\"a\"]}}],\"siblings\":[{{\"role\":\"style\",\"extensions\":[{}]}}]}}",
            list(n)
        ),
        "siblings[0].qualifiers" => format!(
            "{{\"version\":1,\"entries\":[{{\"globs\":[\"a\"]}}],\"siblings\":[{{\"role\":\"style\",\"qualifiers\":[{}]}}]}}",
            list(n)
        ),
        _ => format!(
            "{{\"version\":1,\"entries\":[{{\"globs\":[\"a\"]}}],\"{field}\":[{}]}}",
            list(n)
        ),
    }
    };
    for field in [
        "entries[0].globs",
        "entries[0].extensions",
        "entries[0].qualifiers",
        "siblings[0].extensions",
        "siblings[0].qualifiers",
        "exclude",
    ] {
        bound(field, per_rule(field, 32), per_rule(field, 33));
    }

    // MAX_QUALIFIERS = 64 — the bound the code CONTRADICTED. 33 and 64 are ACCEPTED (they used to
    // be refused with "max 32"), 65 is refused.
    for field in ["family_qualifiers", "non_component_qualifiers"] {
        ok(per_rule(field, 33))
            .unwrap_or_else(|e| panic!("33 qualifiers are within the announced 64: {e}"));
        bound(field, per_rule(field, 64), per_rule(field, 65));
    }
    // MAX_IGNORED_DIRS = 64
    bound(
        "ignored_dirs",
        per_rule("ignored_dirs", 64),
        per_rule("ignored_dirs", 65),
    );

    // MAX_PATTERN_BYTES = 512 — per individual string, on a glob and on the two free-text fields.
    let big = |n: usize| "z".repeat(n);
    bound(
        "entries[0].globs",
        format!(
            "{{\"version\":1,\"entries\":[{{\"globs\":[\"{}\"]}}]}}",
            big(512)
        ),
        format!(
            "{{\"version\":1,\"entries\":[{{\"globs\":[\"{}\"]}}]}}",
            big(513)
        ),
    );
    for field in ["requires_content", "template_attribute"] {
        let doc = |n: usize| {
            format!(
                "{{\"version\":1,\"entries\":[{{\"globs\":[\"a\"],\"{field}\":\"{}\"}}]}}",
                big(n)
            )
        };
        bound(field, doc(512), doc(513));
    }

    // MAX_PINS = 512 / MAX_PIN_FILES = 64
    let pins = |n: usize| {
        let body = (0..n)
            .map(|i| format!("{{\"entry\":\"src/p{i}.ts\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"version\":1,\"entries\":[{{\"globs\":[\"a\"]}}],\"pins\":[{body}]}}")
    };
    bound("pins", pins(512), pins(513));
    let pin_files = |n: usize| {
        let body = (0..n)
            .map(|i| format!("{{\"path\":\"src/f{i}.css\",\"role\":\"style\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"version\":1,\"entries\":[{{\"globs\":[\"a\"]}}],\
              \"pins\":[{{\"entry\":\"src/a.ts\",\"files\":[{body}]}}]}}"
        )
    };
    bound("pins[0]", pin_files(64), pin_files(65));
}

/// A PIN PATH IS TAKEN LITERALLY OR REFUSED, NEVER RE-INTERPRETED
/// (audit 01KYM2TWYT43Z9DP91QWQVKA6Y): `/etc/passwd` used to be ACCEPTED, PERSISTED verbatim, and
/// only re-read at scan time as `etc/passwd` under the repo root — a different file from the one
/// the agent named, chosen in silence, while `validate()`'s own message promised to refuse anything
/// that is not "a usable repo-relative path".
#[test]
fn an_absolute_or_non_canonical_pin_path_is_refused_by_name() {
    let pin = |entry: &str| {
        format!(
            "{{\"version\":1,\"entries\":[{{\"globs\":[\"**/*.ts\"]}}],\
              \"pins\":[{{\"entry\":\"{}\"}}]}}",
            entry.replace('\\', "\\\\")
        )
    };
    let file = |path: &str| {
        format!(
            "{{\"version\":1,\"entries\":[{{\"globs\":[\"**/*.ts\"]}}],\
              \"pins\":[{{\"entry\":\"src/a.ts\",\
                          \"files\":[{{\"path\":\"{}\",\"role\":\"style\"}}]}}]}}",
            path.replace('\\', "\\\\")
        )
    };

    // ABSOLUTE, in `entry` AND in `files[].path` — the two places a pin names a file.
    for path in [
        "/etc/passwd",
        "//etc//passwd",
        "~/.ssh/id_rsa",
        "C:\\Windows\\win.ini",
    ] {
        let err = BundleRecipe::from_json(&pin(path)).expect_err("refused as an entry");
        assert!(
            err.contains("ABSOLUTE") && err.contains("pins[0].entry"),
            "the refusal names the field and says WHY: {err}"
        );
        let err = BundleRecipe::from_json(&file(path)).expect_err("refused as a file");
        assert!(
            err.contains("ABSOLUTE") && err.contains("pins[0].files[0].path"),
            "{err}"
        );
    }

    // NON-CANONICAL: accepted before, then resolved to something else at scan time. Refused now,
    // with the form to write instead — so the persisted recipe says what the scan does.
    for (written, resolved) in [
        ("....//....//etc/passwd", "..../..../etc/passwd"),
        ("./src/a.ts", "src/a.ts"),
        ("src//a.ts", "src/a.ts"),
    ] {
        let err = BundleRecipe::from_json(&pin(written)).expect_err("refused");
        assert!(
            err.contains(resolved) && err.contains("not written the way the scan resolves it"),
            "the refusal shows the resolved form '{resolved}': {err}"
        );
    }

    // The LEXICAL escape is unchanged, and a plain repo-relative path is still fine.
    assert!(BundleRecipe::from_json(&pin("../../etc/passwd"))
        .expect_err("refused")
        .contains("escapes the repo root"));
    BundleRecipe::from_json(&pin("src/legacy/gauge.js")).expect("a real pin still works");
}

/// THE COST OF A SCAN IS BOUNDED — BOTH FACTORS (audits 01KYM2TCTZNRF1WAZRYJ4550V6 then
/// 01KYM9MN6RF9KTA5M4ZW75HAXP).
///
/// The first audit bounded the MATCHING work per file (the per-item caps allowed ~524 000 bytes of
/// glob against every file; the TOTAL byte budget is what actually bounds it). The second audit
/// showed that was only ONE factor of a product, and not the dominant one: on a real repo, the same
/// budget with `"ignored_dirs": []` took 60,2 s (×2030 the default scan) and catalogued 14 229
/// components out of `node_modules` — and the test that claimed to pin the worst case ran on 200
/// files, a tree where `node_modules` does not exist, so it could not see it.
///
/// So this test now runs on a REPRESENTATIVE tree (thousands of files, a fat `node_modules`, a
/// `.git`), and pins both halves of the guarantee:
///   1. the worst recipe `validate()` accepts, `"ignored_dirs": []` included, stays within a
///      ceiling of the same ORDER as the default recipe's scan (a small multiple, not ×2030);
///   2. the walk's floor holds: nothing under `node_modules` / `.git` is catalogued whatever the
///      recipe says, while a directory the DEFAULT list prunes (`build`) is reachable again by a
///      recipe that drops it — finding J is not regressed.
#[test]
fn the_most_expensive_accepted_recipe_still_scans_a_real_tree_quickly() {
    let root = scratch_dir("recipe-cost");
    // A repo-sized tree: 2 000 real source files, plus a dependency tree an order of magnitude
    // bigger — the shape a real Angular monorepo has (4 796 files outside `node_modules`, 73 505 with it).
    for i in 0..2_000 {
        write_file(
            &root,
            &format!("src/feature{}/widget{i}.tsx", i % 50),
            "export const W = () => <div/>;",
        );
    }
    for i in 0..8_000 {
        write_file(
            &root,
            &format!("node_modules/pkg{}/dist/mod{i}.tsx", i % 200),
            "export const Dep = () => <div/>;",
        );
    }
    for i in 0..200 {
        write_file(&root, &format!(".git/objects/ab/{i}"), "not source");
    }
    write_file(
        &root,
        "build/Generated.tsx",
        "export const G = () => <div/>;",
    );

    // The worst shape the budget still admits: every byte spent on `?` (which the DP can never
    // short-circuit), spread over as many rules as fit. `ignored_dirs: []` frees its own bytes for
    // the globs AND asks for the whole tree, so this really is the ceiling.
    let worst = |rules: usize, bytes: usize| {
        let body = (0..rules)
            .map(|_| format!("{{\"globs\":[\"{}\"]}}", "?".repeat(bytes)))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"version\":1,\"entries\":[{body}],\"ignored_dirs\":[]}}")
    };
    assert!(
        BundleRecipe::from_json(&worst(17, 512)).is_err(),
        "one glob past the budget is REFUSED — otherwise there is no ceiling to measure"
    );
    let recipe = BundleRecipe::from_json(&worst(16, 512)).expect("exactly at the budget");
    assert_eq!(recipe.total_pattern_bytes(), 8 * 1024);

    // The REFERENCE: the default recipe on the same tree. The guarantee is a RATIO, so it is the
    // number the ceiling below is expressed against.
    let started = std::time::Instant::now();
    let default_scan = scan_bundles_with(&root, &default_recipe(ComponentStack::React));
    let default_case = started.elapsed();
    assert!(default_scan.errors.is_clean(), "{:?}", default_scan.errors);
    assert_eq!(
        default_scan.bundles.len(),
        2_000,
        "the default recipe catalogues the repo's own front and nothing else"
    );

    let started = std::time::Instant::now();
    let scan = scan_bundles_with(&root, &recipe);
    let worst_case = started.elapsed();
    assert!(scan.errors.is_clean(), "{:?}", scan.errors);
    eprintln!(
        "recipe cost on a repo-sized tree (2 000 source + 8 000 node_modules files): default \
         {default_case:?} / worst accepted {worst_case:?}"
    );

    // THE ANNOUNCED NUMBER (the BOUNDS block of `recipe.rs`, RELEASE): 24,2 ms under the
    // default recipe against 4,29 s under this one — where the SAME configuration measured 61,7 s
    // before the walk had a floor. Here the profile is UNOPTIMIZED and CI machines vary, so the
    // ceiling is 20 s against a measured ~7,6 s; what it pins is the ORDER, over a tree the size of
    // a real repo whose `node_modules` is four times its front — the term the 200-file tree this
    // test used to run on could not exhibit at all.
    assert!(
        worst_case < std::time::Duration::from_secs(20),
        "the worst recipe validate() accepts scanned a repo-sized tree in {worst_case:?} \
         (default: {default_case:?}) — the budget plus the walk's floor are supposed to keep that \
         a small multiple of the default, not a ×2030"
    );

    // THE FLOOR HAS TEETH: `ignored_dirs: []` cannot pull the dependency tree in. Measured with a
    // BANAL glob, since the `?`-globs above match nothing by construction.
    let greedy = BundleRecipe::from_json(
        r#"{"version":1,"entries":[{"globs":["**/*.tsx"]}],"ignored_dirs":[]}"#,
    )
    .expect("a recipe may still ask for everything");
    let greedy_scan = scan_bundles_with(&root, &greedy);
    assert!(
        greedy_scan
            .bundles
            .iter()
            .all(|b| !b.entry_path.starts_with("node_modules/")
                && !b.entry_path.starts_with(".git/")),
        "the walk descended into the dependency tree or the VCS internals"
    );
    assert_eq!(
        greedy_scan.bundles.len(),
        2_001,
        "`ignored_dirs: []` reaches the repo's own front INCLUDING `build/` (finding J), and \
         nothing from `node_modules`"
    );
    assert!(
        greedy_scan.bundles.iter().any(|b| b.name == "Generated"),
        "a front under `build/` is still reachable by a recipe that drops `build` — finding J"
    );
    assert_eq!(
        greedy.unlisted_mandatory_ignored_dirs(),
        vec!["node_modules", ".git"],
        "and the recipe READ says so, instead of letting the agent believe `[]` walked everything"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// THE PRUNED DIRECTORIES ARE THE RECIPE'S (audit 01KYM2W9H0QCV9EE3X9T96SMK8): `IGNORED_DIRS` was
/// hard-coded and inexpressible, so a project could neither add `vendor/` nor keep a front that
/// legitimately lives under a directory named `build`. The default is UNCHANGED (retro-compatible),
/// including for a recipe stored before the field existed.
#[test]
fn the_walk_prunes_the_directories_the_recipe_names() {
    let root = scratch_dir("recipe-ignored-dirs");
    write_file(&root, "src/Card.tsx", "export const Card = () => <div/>;");
    write_file(
        &root,
        "build/Panel.tsx",
        "export const Panel = () => <div/>;",
    );
    write_file(&root, "vendor/Vendor.tsx", "export const V = () => <div/>;");

    let names = |recipe: &BundleRecipe| -> Vec<String> {
        scan_bundles_with(&root, recipe)
            .bundles
            .iter()
            .map(|b| b.name.clone())
            .collect()
    };

    // DEFAULT: `build` is pruned, `vendor` is not — nyx's historical list, unchanged.
    let default = default_recipe(ComponentStack::React);
    assert_eq!(default.ignored_dirs, IGNORED_DIRS.to_vec());
    assert_eq!(names(&default), vec!["Card", "Vendor"]);

    // RETRO-COMPATIBLE: a recipe stored BEFORE the field existed carries no `ignored_dirs`, and
    // must still prune exactly what it always pruned.
    let legacy = BundleRecipe::from_json(
        r#"{"version":1,"entries":[{"globs":["**/*.tsx"]}],"exclude":["**/*.spec.*"]}"#,
    )
    .expect("an older recipe document still parses");
    assert_eq!(
        legacy.ignored_dirs,
        IGNORED_DIRS.to_vec(),
        "the field DEFAULTS to nyx's list, so nothing changes for a recipe that never mentioned it"
    );
    assert_eq!(names(&legacy), vec!["Card", "Vendor"]);

    // THE PROJECT'S OWN: keep `build` (its components are real), drop `vendor`.
    let mut own = default_recipe(ComponentStack::React);
    own.ignored_dirs = vec!["vendor".to_string(), "node_modules".to_string()];
    own.validate().expect("valid");
    assert_eq!(
        names(&own),
        vec!["Card", "Panel"],
        "a front under `build/` is reachable, and `vendor/` is now noise"
    );

    // `ignored_dirs` holds NAMES, not paths — a path is refused by name.
    assert!(BundleRecipe::from_json(
        r#"{"version":1,"entries":[{"globs":["a"]}],"ignored_dirs":["src/vendor"]}"#
    )
    .expect_err("a path is refused")
    .contains("ignored_dirs[0]"));

    // THE FLOOR (audit 01KYM9MN6RF9KTA5M4ZW75HAXP): a recipe built IN MEMORY (not through
    // `from_json`) that drops `node_modules` does not get to walk it either — the union happens on
    // the walk itself, so there is no door around it.
    write_file(&root, "node_modules/dep/Dep.tsx", "export const D = 1;");
    let mut greedy = default_recipe(ComponentStack::React);
    greedy.ignored_dirs = Vec::new();
    assert_eq!(
        names(&greedy),
        vec!["Card", "Panel", "Vendor"],
        "everything the project owns is reachable — the dependency tree is not"
    );

    // …and the READ says so, so the echoed recipe is not a lie: the warning names what the walk
    // adds, tells the agent the rest of the list is still theirs, and points at the escape hatch.
    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Floor", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    set_project_recipe(&mut conn, &project.id, Some(&greedy)).unwrap();
    let view = project_recipe_view(&mut conn, &project.id).unwrap();
    assert_eq!(
        view.recipe.ignored_dirs,
        Vec::<String>::new(),
        "the document stays the agent's — the floor is not smuggled into it (it would eat the \
         `ignored_dirs` count and the pattern budget)"
    );
    let warning = view
        .warnings
        .iter()
        .find(|w| w.contains("ignored_dirs"))
        .expect("the read WARNS about what the walk prunes anyway");
    assert!(
        warning.contains("`node_modules`") && warning.contains("`.git`") && warning.contains("PIN"),
        "{warning}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// ONE ASSEMBLY PER SCAN (audit 01KYM2W82XBPJXW38GMXZDRJDP): a pin whose entry is ALSO elected by
/// the rules used to be assembled TWICE — once by the rule-driven pass (which short-circuits to
/// `pinned_bundle`) and once by the pin loop, the first bundle being thrown away afterwards. The
/// I/O was doubled and, worse, `scan_errors` COUNTED DOUBLE: one missing pinned file reported two
/// failures, on the very counter a human reads to judge whether a scan may prune.
#[test]
fn a_pinned_entry_the_rules_also_elect_is_assembled_exactly_once() {
    let root = scratch_dir("recipe-pin-once");
    write_file(&root, "package.json", r#"{"dependencies":{"react":"19"}}"#);
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );

    let mut recipe = default_recipe(ComponentStack::React);
    // The pin names the file the React rule ALREADY elects, plus a file that is not there.
    recipe.pins = serde_json::from_str(
        r#"[{
            "entry": "src/ui/Card.tsx",
            "files": [{ "path": "src/markup/does-not-exist.hbs", "role": "template" }]
        }]"#,
    )
    .expect("the pins parse");
    recipe.validate().expect("valid");

    let scan = scan_bundles_with(&root, &recipe);
    assert_eq!(
        scan.bundles.len(),
        1,
        "one component, not two: {:?}",
        scan.bundles.iter().map(|b| &b.name).collect::<Vec<_>>()
    );
    let card = bundle(&scan.bundles, "Card");
    assert_eq!(
        paths(card),
        vec!["src/markup/does-not-exist.hbs", "src/ui/Card.tsx"],
        "the pin is still catalogued VERBATIM — the declared files, whatever the rules say"
    );
    assert_eq!(
        scan.errors.count, 1,
        "ONE missing file, ONE scan error: {:?}",
        scan.errors
    );
    assert_eq!(scan.errors.sample[0].kind, "read_input");
    assert_eq!(scan.errors.sample[0].path, "src/markup/does-not-exist.hbs");

    // …and the fail-closed rule still holds end to end: with a scan error, NOTHING is pruned.
    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Once", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    super::set_project_recipe(&mut conn, &project.id, Some(&recipe)).unwrap();
    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(out.discovered, 1);
    assert_eq!(
        out.scan_errors.count, 1,
        "the counter reaching the human is EXACT"
    );
    assert!(
        out.orphans.iter().all(|o| !o.pruned),
        "a scan that failed a read prunes nothing: {:?}",
        out.orphans
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The recipe SCHEMA is a contract: it round-trips through JSON, and every way of getting it wrong
/// is REFUSED with a message that names the field — an agent-written value that is persisted and
/// applied at every scan may never be half-understood.
#[test]
fn the_recipe_schema_round_trips_and_refuses_what_it_cannot_apply() {
    let recipe = default_recipe(ComponentStack::Angular);
    let round_tripped = BundleRecipe::from_json(&recipe.to_json()).expect("round-trips");
    assert_eq!(round_tripped, recipe);

    let err = |json: &str| BundleRecipe::from_json(json).expect_err("must be refused");
    assert!(
        err(r#"{"version":1,"entries":[]}"#).contains("inventory NOTHING"),
        "a recipe with no entry rule is refused (it would empty the catalogue)"
    );
    assert!(
        err(r#"{"version":1,"entries":[{"glob":["**/*.ts"]}]}"#).contains("recipe schema"),
        "an UNKNOWN field (here `glob` for `globs`) is refused, never ignored"
    );
    assert!(
        err(r#"{"version":99,"entries":[{"globs":["**/*.ts"]}]}"#).contains("unsupported"),
        "an unsupported schema version is refused"
    );
    assert!(
        err(r#"{"version":1,"entries":[{"globs":[]}]}"#).contains("no `globs`"),
        "an entry rule that claims no file is refused"
    );
    assert!(
        err(r#"{"version":1,"entries":[{"globs":["**/*.ts"],"requires_content":"  "}]}"#)
            .contains("blank"),
        "a blank content marker is refused (it would match everything)"
    );
    assert!(
        err(r#"{"version":1,"entries":[{"globs":["**/*.ts"]}],
                "pins":[{"entry":"../../etc/passwd"}]}"#)
        .contains("escapes the repo root"),
        "a LEXICAL escape (`..`) is refused by validate(); the DISK escape (an internal symlink) is \
         refused at RESOLUTION time — see `a_pin_never_reads_through_a_symlink_out_of_the_repo`"
    );
    assert!(
        err(r#"{"version":1,"entries":[{"globs":["**/*.ts"]}],
                "pins":[{"entry":"a.ts","files":[{"path":"b.html","role":"markup"}]}]}"#)
        .contains("recipe schema"),
        "an unknown ROLE is refused"
    );
}

/// END TO END through the project: the persisted RECIPE drives `inventory_project`, the catalogue
/// row lands with the recipe's name and entry, and `read_bundle` hands the agent the SAME input hash
/// the scan computed (or the incremental loop would re-propose a component that never changed).
///
/// It also pins the PRECEDENCE: recipe > v33 glob override > detected-stack default, and clearing
/// the recipe falls back to the override that is still there.
#[test]
fn inventory_project_honours_the_persisted_recipe_and_its_precedence() {
    let root = scratch_dir("recipe-project");
    write_file(&root, "angular.json", r#"{"version":1}"#); // detected: Angular
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
    write_file(
        &root,
        "src/app/nav/nav.ts",
        "@Component({ templateUrl: './nav.html' })\nexport class Nav {}",
    );
    write_file(&root, "src/app/nav/nav.html", "<nav></nav>");

    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "House", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    // 0. No recipe, no override → the DETECTED stack default (Angular).
    let view = project_recipe_view(&mut conn, &project.id).unwrap();
    assert_eq!(view.source, RECIPE_SOURCE_STACK_DEFAULT);
    assert_eq!(view.repos.len(), 1);
    assert_eq!(view.repos[0].stack, "angular");
    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        out.bundles
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        vec!["nav"],
        "the Angular default sees only the Angular component"
    );

    // 1. A v33 GLOB OVERRIDE still works, and still wins over the stack default.
    set_design_inventory_globs(&mut conn, &project.id, Some(&["**/*.tsx".to_string()])).unwrap();
    assert_eq!(
        project_recipe_view(&mut conn, &project.id).unwrap().source,
        RECIPE_SOURCE_GLOBS_OVERRIDE
    );

    // 2. The AGENT's RECIPE wins over both.
    let recipe = BundleRecipe::from_json(
        r#"{
            "version": 1,
            "entries": [
                { "globs": ["**/*.widget.js"], "template_attribute": "view" }
            ],
            "siblings": [ { "extensions": ["html"], "role": "template" } ],
            "family_qualifiers": ["widget", "view"]
        }"#,
    )
    .unwrap();
    set_project_recipe(&mut conn, &project.id, Some(&recipe)).unwrap();
    let view = project_recipe_view(&mut conn, &project.id).unwrap();
    assert_eq!(view.source, RECIPE_SOURCE_CUSTOM);
    assert_eq!(view.recipe, recipe, "what was written is what is read back");

    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    let discovered = bundle(&out.bundles, "counter");
    assert_eq!(discovered.entry_path, "src/widgets/counter.widget.js");
    assert_eq!(
        paths(discovered),
        vec![
            "src/widgets/counter.view.html",
            "src/widgets/counter.widget.js"
        ],
    );
    let scan_hash = discovered.input_hash.clone();

    // The per-component READ resolves the bundle through the SAME recipe — same files, same hash.
    let read = super::read_bundle(&mut conn, &project.id, "counter", 4096)
        .unwrap()
        .expect("the catalogue holds it");
    assert_eq!(
        read.bundle.as_ref().map(|b| b.input_hash.as_str()),
        Some(scan_hash.as_str()),
        "the read hash IS the scan hash — the incremental loop cannot drift"
    );
    assert_eq!(read.files.len(), 2, "entry + recipe-resolved template");

    // 3. Clearing the recipe falls BACK to the override that was still persisted underneath.
    set_project_recipe(&mut conn, &project.id, None).unwrap();
    let view = project_recipe_view(&mut conn, &project.id).unwrap();
    assert_eq!(view.source, RECIPE_SOURCE_GLOBS_OVERRIDE);

    let _ = std::fs::remove_dir_all(&root);
}

/// FAIL-CLOSED: a stored recipe that cannot be PARSED (a hand-edited DB — the tool validates on
/// write) makes the inventory REFUSE to scan, rather than silently scanning under the default rules
/// and letting the élagage act on a population the project never asked for.
#[test]
fn an_unparsable_stored_recipe_refuses_the_scan_instead_of_guessing() {
    let root = scratch_dir("recipe-corrupt");
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );
    let mut conn = open_in_memory();
    let (project, _ws) =
        create_project(&mut conn, "Corrupt", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    crate::db::set_design_bundle_recipe(&mut conn, &project.id, Some("{ not json")).unwrap();
    let err = super::inventory_project(&mut conn, &project.id)
        .expect_err("a corrupt recipe is not a silent fall-back");
    assert!(
        format!("{err}").contains("REFUSES to scan"),
        "the failure says WHY and how to fix it: {err}"
    );

    // Cleared → the scan works again.
    crate::db::set_design_bundle_recipe(&mut conn, &project.id, None).unwrap();
    let out = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(out.discovered, 1);

    let _ = std::fs::remove_dir_all(&root);
}

// --- the EXPLICIT removal, and what it really achieves (finding 01KYSCX4PNKTMX17YVG9XD8KBZ) -----

/// THE FINDING: deleting the row of a component whose file is STILL THERE and still matches the
/// recipe is EPHEMERAL — the next inventory re-discovers it, `pending`, under a new id. So "keep
/// only these N components" cannot be had that way, and the gesture used to answer a bare
/// `deleted: true`. What this pins: the removal now SAYS it (naming the problem AND the two
/// corrections), and `exclude_source` makes it LAST — through the recipe's own `exclude` list, the
/// one the discovery has always honoured.
#[test]
fn deleting_a_component_whose_source_still_matches_warns_and_is_undone_by_the_next_scan() {
    let root = scratch_dir("delete-ephemeral");
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );
    write_file(
        &root,
        "src/mail/Welcome.tsx",
        "export const Welcome = () => <div/>;",
    );
    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    super::inventory_project(&mut conn, &project.id).unwrap();

    let welcome = get_component_by_name(&mut conn, &project.id, "Welcome")
        .unwrap()
        .expect("catalogued");

    // 1. The EFFECT is computed BEFORE anything is destroyed, and it is honest.
    let effect = super::removal_effect(&mut conn, &project.id, &welcome).unwrap();
    assert!(effect.ephemeral, "its source still matches the recipe");
    assert_eq!(effect.verdict, "elected");
    assert_eq!(effect.source_path.as_deref(), Some("src/mail/Welcome.tsx"));
    assert_eq!(effect.repo.as_deref(), Some("web"));

    // 2. The plain removal deletes the row AND says the gesture will not last — problem + correction.
    let removal = super::remove_component(&mut conn, &project.id, &welcome, false).unwrap();
    assert!(removal.deleted && removal.refused.is_none());
    assert!(removal.excluded.is_empty(), "nothing was asked for");
    let warning = removal.warnings.join(" | ");
    for expected in [
        "EPHEMERAL",
        "src/mail/Welcome.tsx",
        "design_inventory DISCOVERS IT AGAIN",
        "RESTRICT the recipe",
        "exclude_source: true",
    ] {
        assert!(
            warning.contains(expected),
            "the warning must carry {expected:?}: {warning}"
        );
    }
    assert!(
        get_component_by_name(&mut conn, &project.id, "Welcome")
            .unwrap()
            .is_none(),
        "the row IS gone — the warning is about what happens NEXT"
    );

    // 3. …and the warning told the truth: a rescan brings it straight back, pending.
    super::inventory_project(&mut conn, &project.id).unwrap();
    let back = get_component_by_name(&mut conn, &project.id, "Welcome")
        .unwrap()
        .expect("the source still matches, so the discovery re-creates the row");
    assert!(back.template.is_empty(), "it comes back as pending");

    // 4. WITH the exclusion, the removal LASTS — and only for that source.
    let removal = super::remove_component(&mut conn, &project.id, &back, true).unwrap();
    assert!(removal.deleted && removal.refused.is_none());
    assert_eq!(
        removal.excluded,
        vec!["src/mail/Welcome.tsx".to_string()],
        "the pattern is the bundle's own path, in the recipe's `exclude`"
    );
    assert!(
        removal.recipe_materialized,
        "the project ran on the stack default, which is materialized to hold the exclusion"
    );
    let stored = project_recipe_view(&mut conn, &project.id).unwrap();
    assert_eq!(stored.source, RECIPE_SOURCE_CUSTOM);
    assert!(stored
        .recipe
        .exclude
        .contains(&"src/mail/Welcome.tsx".to_string()));

    let after = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        list_components(&mut conn, &project.id)
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect::<Vec<_>>(),
        vec!["Card".to_string()],
        "the excluded source stays out; every other component is untouched"
    );
    assert_eq!(after.discovered, 1);
    // The exclusion is a scan rule, never a destruction: nothing was pruned to obtain it.
    assert!(after.orphans.is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

/// The cases where the warning must NOT cry wolf, and where the exclusion must be REFUSED rather
/// than written for nothing: a greenfield `[NEW]` (no source at all — the review-#44 case the
/// removal exists for), a source the recipe already ignores, and a PINNED entry (a pin bypasses
/// every rule, `exclude` included, so only removing the pin can work).
#[test]
fn a_removal_that_is_already_definitive_says_nothing_and_writes_nothing() {
    let root = scratch_dir("delete-definitive");
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );
    write_file(&root, "src/legacy/gauge.js", "// no marker, no rule");
    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();

    // A greenfield `[NEW]` authored by the proto: no source ⇒ nothing can re-discover it.
    crate::db::upsert_component(
        &mut conn,
        &project.id,
        &crate::db::ComponentInput {
            name: "PricingTable".into(),
            template: "<section>{{title}}</section>".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let new_one = get_component_by_name(&mut conn, &project.id, "PricingTable")
        .unwrap()
        .unwrap();
    let effect = super::removal_effect(&mut conn, &project.id, &new_one).unwrap();
    assert!(!effect.ephemeral && effect.verdict == "greenfield");
    // Asking to exclude it is refused — with the delete, and with the reason.
    let refused = super::remove_component(&mut conn, &project.id, &new_one, true).unwrap();
    assert!(!refused.deleted, "the two were ONE gesture");
    assert!(refused
        .refused
        .as_deref()
        .is_some_and(|m| m.contains("NO on-disk source")));
    let removal = super::remove_component(&mut conn, &project.id, &new_one, false).unwrap();
    assert!(
        removal.deleted && removal.warnings.is_empty(),
        "no wolf cried"
    );

    // A row the recipe does NOT take (a `.js` with no marker): the delete is already definitive, so
    // `exclude_source` writes NOTHING into the recipe and says so.
    super::inventory_project(&mut conn, &project.id).unwrap();
    let ghost = crate::db::upsert_component_discovery(
        &mut conn,
        &project.id,
        "gauge",
        None,
        "src/legacy/gauge.js",
    )
    .unwrap()
    .expect("a fresh row");
    let effect = super::removal_effect(&mut conn, &project.id, &ghost).unwrap();
    assert!(!effect.ephemeral, "no entry rule elects it: {effect:?}");
    let removal = super::remove_component(&mut conn, &project.id, &ghost, true).unwrap();
    assert!(removal.deleted && removal.excluded.is_empty());
    assert!(
        removal
            .warnings
            .join(" ")
            .contains("no exclusion was written"),
        "{:?}",
        removal.warnings
    );
    assert_eq!(
        project_recipe_view(&mut conn, &project.id).unwrap().source,
        RECIPE_SOURCE_STACK_DEFAULT,
        "a pointless exclusion never materializes the recipe"
    );

    // A PINNED entry: `exclude` cannot hide it, so the exclusion is refused and names the pin.
    let mut recipe = default_recipe(ComponentStack::React);
    recipe.pins.push(PinnedBundle {
        entry: "src/legacy/gauge.js".into(),
        files: Vec::new(),
        name: None,
    });
    set_project_recipe(&mut conn, &project.id, Some(&recipe)).unwrap();
    super::inventory_project(&mut conn, &project.id).unwrap();
    let pinned = get_component_by_name(&mut conn, &project.id, "gauge")
        .unwrap()
        .expect("the pin catalogues it");
    let effect = super::removal_effect(&mut conn, &project.id, &pinned).unwrap();
    assert!(effect.ephemeral && effect.verdict == "pinned");
    let refused = super::remove_component(&mut conn, &project.id, &pinned, true).unwrap();
    assert!(!refused.deleted);
    assert!(refused
        .refused
        .as_deref()
        .is_some_and(|m| m.contains("PINNED")));
    // Without the exclusion it deletes, and the warning names the pin as what to remove.
    let removal = super::remove_component(&mut conn, &project.id, &pinned, false).unwrap();
    assert!(removal.deleted);
    assert!(removal.warnings.join(" ").contains("remove the pin"));

    let _ = std::fs::remove_dir_all(&root);
}

/// THE GUARD THIS MUST NOT TOUCH (two CRITICALs were closed on this path): a SYNTHESIS is never
/// destroyed by anything automatic. Excluding a source is a SCAN rule, so the row whose source the
/// exclusion hides becomes an ORPHAN — and an orphan carrying a synthesis is FLAGGED and KEPT, never
/// pruned. The explicit removal stays the only thing that can take it out.
#[test]
fn excluding_a_source_never_destroys_a_synthesis_the_prune_still_spares_it() {
    let root = scratch_dir("delete-exclude-guard");
    write_file(
        &root,
        "src/ui/Card.tsx",
        "export const Card = () => <div/>;",
    );
    write_file(
        &root,
        "src/ui/Badge.tsx",
        "export const Badge = () => <span/>;",
    );
    // A third component nothing here excludes — so the scan is never EMPTY and the élagage is always
    // in a position to judge (its `empty_scan` guard would otherwise spare the row for another reason
    // than the one under test).
    write_file(&root, "src/ui/Chip.tsx", "export const Chip = () => <b/>;");
    let mut conn = open_in_memory();
    let (project, _ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
    create_repository(&mut conn, &project.id, "web", ".", false).unwrap();
    super::inventory_project(&mut conn, &project.id).unwrap();

    // Badge is SYNTHESIZED (human-reviewed LLM work), Card is the one being taken out.
    let badge = get_component_by_name(&mut conn, &project.id, "Badge")
        .unwrap()
        .unwrap();
    update_component_content(
        &mut conn,
        &badge.id,
        "<span class=\"ds-badge\">{{label}}</span>",
        None,
        None,
    )
    .unwrap();
    super::mark_synthesized(&mut conn, &project.id, "Badge").unwrap();

    let card = get_component_by_name(&mut conn, &project.id, "Card")
        .unwrap()
        .unwrap();
    super::remove_component(&mut conn, &project.id, &card, true).unwrap();

    // The exclusion took Card's SOURCE out of the discovery — and touched nothing else.
    let after = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        after.discovered, 2,
        "Card's source is out; Badge and Chip remain"
    );
    let kept = get_component_by_name(&mut conn, &project.id, "Badge")
        .unwrap()
        .expect("the synthesized row is intact");
    assert_eq!(kept.template, "<span class=\"ds-badge\">{{label}}</span>");
    assert!(kept.orphaned_at.is_none(), "its source is still discovered");
    assert!(
        root.join("src/ui/Card.tsx").exists(),
        "NOTHING on disk is touched"
    );

    // Now EXCLUDE the synthesized one's source too, and prove the automatic élagage still refuses to
    // destroy it: it is flagged as an orphan and KEPT.
    let removal = super::remove_component(&mut conn, &project.id, &kept, true).unwrap();
    assert!(removal.deleted);
    // Re-create the row (as a synthesis someone restores by hand) and rescan: the source is excluded,
    // so the row is an orphan carrying content — the prune must SPARE it.
    let restored = crate::db::upsert_component(
        &mut conn,
        &project.id,
        &crate::db::ComponentInput {
            name: "Badge".into(),
            source_repository_id: kept.source_repository_id.clone(),
            source_path: kept.source_path.clone(),
            template: "<span class=\"ds-badge\">{{label}}</span>".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let run = super::inventory_project(&mut conn, &project.id).unwrap();
    assert_eq!(
        run.orphans
            .iter()
            .map(|o| (o.name.as_str(), o.pruned, o.kept_because))
            .collect::<Vec<_>>(),
        vec![("Badge", false, Some(super::KEPT_SYNTHESIZED))],
        "an orphan carrying a synthesis is FLAGGED, never destroyed"
    );
    assert!(crate::db::get_component(&mut conn, &restored.id)
        .unwrap()
        .is_some_and(|c| !c.template.is_empty()));

    let _ = std::fs::remove_dir_all(&root);
}
