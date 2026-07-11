/* eslint-disable */
// Shared helpers for the v2.5 REVIEW-IN-TAB gate e2e (task 01KX663WEJ62J1F6CVCRTWC7R7,
// PRD 01KX0EEP5H1MJW4T3QCZEWPHFK, PDR-0012/0015).
//
// This is the NET-NEW, TAB-BASED successor to the branch `v2.4-review` `review-helpers.cjs`. The
// review no longer lives behind a top-level `/review` route + the `window.__nyxReview` nav/data
// seam (both REMOVED by Finding 4): it now lives INSIDE the Spec detail's **Review tab**, scoped to
// the Spec's SINGLE dev workspace (`detail.workspace_context`, Finding 3). So this harness drives
// the review PURELY through real DOM + the two SHELL-AGNOSTIC seams the other v2 gates already use —
// ZERO review-specific production plumbing:
//   1. `window.__nyxWork` (base-layout.tsx, gated `VITE_NYX_E2E`) — ONLY to SEED the anchor project
//      + its root workspace over the multi-repo git tree (the SAME seam `spec-workflow.e2e.cjs`
//      uses). It exposes NO Spec/review authoring.
//   2. The REAL MCP loopback (`http://127.0.0.1:<NYX_MCP_PORT>/mcp`) — the phase-5 Spec tools
//      (spec_create / spec_update_section / task_add / spec_transition / spec_set_workspace) to
//      AUTHOR + ANCHOR the Spec exactly like an agent, AND the review tools (review_comment_post /
//      review_comment_list / review_task_list / review_task_structure / workspace_git) to make the
//      AGENT act + to enumerate the workspace's repository ids (the seam used to hand these back).
// Everything else is REAL navigation (rail → board → Spec card → Review tab) + the rendered review
// DOM (plain React, no WebGL): the SAME `data-testid` / `role` / `aria-label` selectors the ported
// review components emit (diff-scroll, comment-thread, Repositories tablist, Diff surface, Review
// variety, Base/Target rev, unread-badge, resolve/promote-comment, task-row, validate-task, …).
//
// The whole scenario is LOCAL: real git repos in a temp dir, the app's OWN isolated sidecar (on the
// dedicated e2e MCP port, ADR-0003 D2 `NYX_MCP_PORT` override → never the 8766 prod default), the
// loopback MCP. No forge, no VS Code.

const fs = require("fs");
const os = require("os");
const path = require("path");
const http = require("http");
const { execFileSync } = require("child_process");

// -------------------------------------------------------------------------------------------
// A REAL, multi-repo git tree on the host the app shares (spec + app run on the same machine).
// The review's repo TABS come from `workspace_git_summary` (UI) / `workspace_git` (MCP), which
// enumerate a workspace's NON-OPTIONAL repositories detected UNDER its root: a root dir with
// SEVERAL immediate child folders that are each a git repo yields ONE repo per folder — exactly
// the multi-repo aggregation the gate needs, WITHOUT mixing Git identities.
//   - `api` — a GROS change (many files/lines → virtualization) + an anchor file whose lines move
//             (re-map) / vanish (outdated) on a later commit.
//   - `web` — a smaller change on its own identity (proves multi-repo aggregation).
// A unique root per run keeps the scenario deterministic.
// -------------------------------------------------------------------------------------------

function reviewRootDir() {
  const root = fs.realpathSync(os.tmpdir());
  return path.join(root, "nyx-review-tab-e2e-" + process.pid);
}

// The anchoring file inside `api` — engineered so a later commit can (a) SHIFT a stable line down
// (re-map follows it) and (b) DELETE another line (outdated). Kept small + legible.
const ANCHOR_FILE = "src/anchor.rs";
const ANCHOR_KEEP_LINE = "    let result = compute_total(items);"; // moves down on the next commit
const ANCHOR_GONE_LINE = "    legacy_side_effect();"; // deleted by the next commit → outdated

function git(dir, ...args) {
  return execFileSync("git", ["-C", dir, ...args], { stdio: "pipe" }).toString();
}

function initRepo(dir) {
  fs.mkdirSync(dir, { recursive: true });
  git(dir, "init", "-q");
  git(dir, "config", "user.email", "e2e@nyx.local");
  git(dir, "config", "user.name", "nyx e2e");
  git(dir, "config", "commit.gpgsign", "false");
  git(dir, "config", "gc.auto", "0");
}

function writeFile(dir, rel, content) {
  const abs = path.join(dir, rel);
  fs.mkdirSync(path.dirname(abs), { recursive: true });
  fs.writeFileSync(abs, content);
}

// Build a GROS diff in `api`: a `base` branch, then a `feature` branch that rewrites every file
// (dozens of changed lines each → a virtualization-worthy diff). The anchor file carries the two
// engineered lines. Returns the resolved base + feature SHAs.
//
// SCROLLABILITY (Findings 52 + 66): the e2e window is only 800×600 (window.ts), so the `diff-scroll`
// viewport is a few hundred px tall. The sticky-header gate (52) and the content-visibility layout-
// virtualization gate (66) both REQUIRE a diff that is MANY viewports tall with real OFF-SCREEN file
// bodies — otherwise nothing scrolls, nothing pins, and no off-screen body is layout-skipped. A dozen
// short files sat right around one screenful (marginal → flaky), so we seed one DELIBERATELY HUGE file
// (`BIG_FILE`, ~`BIG_LINES` fully-rewritten lines). Its path sorts SECOND — right after the tiny
// `anchor.rs`, before every `mod_*` — so it sits at the TOP of `diff-scroll` (always on-screen) and
// @pierre paints its full (non-virtualized) body, making the scroll surface tens of thousands of px
// tall and pushing EVERY `mod_*` body far off-screen (reliably content-visibility-skipped). Its
// populated `file.hunks` also drives a large `contain-intrinsic-size` estimate, so the height holds
// even before paint. It is NOT the FIRST file on purpose: the Finding-55 perf test collapses the very
// first file section and that collapse PERSISTS across the later gate tests (DiffView keeps `collapsed`
// until the diff identity changes), so keeping `anchor.rs` first means only that harmless tiny file
// ever gets collapsed — the big scroll-anchor stays expanded and tall. Pure fixture size; no product
// behaviour changes.
const BIG_FILE = "src/big_change.rs"; // sorts 2nd (after anchor.rs, before mod_*) → top of the diff
const BIG_LINES = 400; // a single ~400-line rewrite ⇒ a diff far taller than the 600px e2e window
function seedApiRepo(dir) {
  initRepo(dir);
  const FILES = 12;
  const LINES = 60; // lines each → a diff well past one screenful of virtualized rows

  const bigBase = [];
  for (let l = 0; l < BIG_LINES; l++)
    bigBase.push(`fn huge_base_${l}() { let x = ${l}; return x * 3 + ${l % 7}; }`);
  writeFile(dir, BIG_FILE, bigBase.join("\n") + "\n");
  for (let f = 0; f < FILES; f++) {
    const body = [];
    for (let l = 0; l < LINES; l++) body.push(`fn base_${f}_${l}() { let x = ${l}; return x + ${f}; }`);
    writeFile(dir, `src/mod_${f}.rs`, body.join("\n") + "\n");
  }
  writeFile(dir, ANCHOR_FILE, "fn anchor_demo() {\n    old_stub();\n}\n");
  git(dir, "add", "-A");
  git(dir, "commit", "-q", "-m", "base");
  git(dir, "branch", "-M", "base");
  const baseSha = git(dir, "rev-parse", "HEAD").trim();

  git(dir, "checkout", "-q", "-b", "feature");
  const bigFeature = [];
  for (let l = 0; l < BIG_LINES; l++)
    bigFeature.push(`fn huge_feature_${l}() { let y = ${l} * 2; return y - ${l % 5}; }`);
  writeFile(dir, BIG_FILE, bigFeature.join("\n") + "\n");
  for (let f = 0; f < FILES; f++) {
    const body = [];
    for (let l = 0; l < LINES; l++)
      body.push(`fn feature_${f}_${l}() { let y = ${l} * 2; return y - ${f}; }`);
    writeFile(dir, `src/mod_${f}.rs`, body.join("\n") + "\n");
  }
  // The anchor file at FEATURE (HEAD): the KEEP line at a known depth, the GONE line just below it.
  writeFile(
    dir,
    ANCHOR_FILE,
    [
      "fn anchor_demo() {",
      "    setup();",
      "    prepare(items);",
      ANCHOR_KEEP_LINE,
      ANCHOR_GONE_LINE,
      "    finish();",
      "}",
    ].join("\n") + "\n",
  );
  git(dir, "add", "-A");
  git(dir, "commit", "-q", "-m", "feature: rewrite modules");
  const featureSha = git(dir, "rev-parse", "HEAD").trim();

  return { baseSha, featureSha };
}

// Advance `api` ON feature: a NEW commit that INSERTS lines ABOVE the KEEP line (so a comment
// anchored on it must FOLLOW the code down = re-map) and DELETES the GONE line (so a comment
// anchored on it can no longer be located = outdated). Returns the new HEAD sha.
function advanceApiRepo(dir) {
  writeFile(
    dir,
    ANCHOR_FILE,
    [
      "fn anchor_demo() {",
      "    setup();",
      "    validate(items);", // inserted
      "    normalize(items);", // inserted
      "    prepare(items);",
      ANCHOR_KEEP_LINE, // SAME text, now shifted down → re-map follows it
      // ANCHOR_GONE_LINE removed → outdated
      "    finish();",
      "}",
    ].join("\n") + "\n",
  );
  git(dir, "add", "-A");
  git(dir, "commit", "-q", "-m", "advance: shift anchor + drop legacy");
  return git(dir, "rev-parse", "HEAD").trim();
}

// A second, independent repo (`web`) so the review aggregates TWO Git identities in one UI.
function seedWebRepo(dir) {
  initRepo(dir);
  writeFile(dir, "index.js", ["const a = 1;", "const b = 2;", "console.log(a + b);"].join("\n") + "\n");
  git(dir, "add", "-A");
  git(dir, "commit", "-q", "-m", "base");
  git(dir, "branch", "-M", "base");
  const baseSha = git(dir, "rev-parse", "HEAD").trim();
  git(dir, "checkout", "-q", "-b", "feature");
  writeFile(
    dir,
    "index.js",
    ["const a = 10;", "const b = 20;", "const c = 30;", "console.log(a + b + c);"].join("\n") + "\n",
  );
  git(dir, "add", "-A");
  git(dir, "commit", "-q", "-m", "feature: web change");
  const featureSha = git(dir, "rev-parse", "HEAD").trim();
  return { baseSha, featureSha };
}

// Materialize the whole multi-repo tree under one project ROOT: two child repos (`api`, `web`),
// each on its `feature` branch, with a working-tree change in `api` so the LIVING working-tree diff
// is non-empty. Returns the root path + each repo's dir + SHAs. Idempotent per run (unique root).
function seedMultiRepoTree() {
  const root = reviewRootDir();
  fs.rmSync(root, { recursive: true, force: true });
  fs.mkdirSync(root, { recursive: true });
  const apiDir = path.join(root, "api");
  const webDir = path.join(root, "web");
  const api = seedApiRepo(apiDir);
  const web = seedWebRepo(webDir);
  writeFile(apiDir, "src/worktree_change.rs", "fn uncommitted() { return 42; }\n");
  return {
    root: fs.realpathSync(root),
    apiDir: fs.realpathSync(apiDir),
    webDir: fs.realpathSync(webDir),
    api,
    web,
  };
}

// A fresh, UNTRACKED file in a repo's work tree — a live "the repo changed" edit the git-watch
// picks up (drives the real-time refresh + the derived views). Returns the file's rel path.
function touchWorkTree(dir, marker) {
  const rel = "live_change_" + marker + ".txt";
  writeFile(dir, rel, "live " + marker + " " + Date.now() + "\n");
  return rel;
}

// -------------------------------------------------------------------------------------------
// SEED seam (`window.__nyxWork`) — create the anchor project + its root workspace. Published by
// the always-mounted <BaseLayout> shell behind `VITE_NYX_E2E`, inert in prod (like `window.__nyx`).
// This is the ONLY seam the seeding uses; the review itself is driven purely through real DOM.
// -------------------------------------------------------------------------------------------

async function waitForWorkSeam(timeoutMs) {
  await browser.waitUntil(
    async () =>
      browser.execute(
        () => !!(window.__nyxWork && typeof window.__nyxWork.createProject === "function"),
      ),
    {
      timeout: timeoutMs || 45000,
      timeoutMsg: "window.__nyxWork seed seam never appeared (app/seam did not start)",
    },
  );
}

// Create the multi-repo PROJECT through the seam (the SAME `create_project` the rail runs,
// auto-detecting the repos under the root). Returns `{ project, root }`.
const seedProject = (name, rootPath) =>
  browser.execute((n, r) => window.__nyxWork.createProject(n, r), name, rootPath);

// -------------------------------------------------------------------------------------------
// REAL navigation — rail → Work board → Spec card → PRD-detail tab (no seam, no hash route).
// -------------------------------------------------------------------------------------------

// Enter a project from the left rail (click its tile).
async function enterProject(projectName) {
  const label = "Project " + projectName;
  await browser.waitUntil(
    async () => browser.execute((l) => !!document.querySelector('button[aria-label="' + l + '"]'), label),
    { timeout: 15000, timeoutMsg: "project tile '" + projectName + "' never appeared in the rail" },
  );
  await browser.execute((l) => {
    document.querySelector('button[aria-label="' + l + '"]').click();
  }, label);
}

// Click the "Work" section-nav link → route to the Specs board. The Work link lives in the always-
// mounted shell rail, so it is reachable from any routed page. But the full-screen review PAGE
// (`/work/$specId/review`) is a distinct navigation state reached AFTER "Open review"; step back to the
// Spec shell first (its "Back" control → `/work/$specId`) so the rail nav is unambiguously in reach
// from this state before we look for the Work link (the failing empty-state + pure-local tests both
// enter `openBoard` straight from the review page).
async function openBoard() {
  const onReviewPage = await browser.execute(
    () => !!document.querySelector('[data-testid="review-page"]'),
  );
  if (onReviewPage) {
    await browser.execute(() => {
      const back = document.querySelector('[data-testid="review-back"]');
      if (back) back.click();
    });
    await browser.waitUntil(
      async () => browser.execute(() => !document.querySelector('[data-testid="review-page"]')),
      { timeout: 15000, timeoutMsg: "the review page did not close on Back before opening the board" },
    );
  }
  await browser.waitUntil(
    async () => browser.execute(() => !!document.querySelector('a[aria-label="Work"]')),
    { timeout: 15000, timeoutMsg: "the Work section-nav link never appeared" },
  );
  await browser.execute(() => document.querySelector('a[aria-label="Work"]').click());
  await browser.waitUntil(
    async () => browser.execute(() => /\bSpecs\b/.test(document.body.innerText || "")),
    { timeout: 15000, timeoutMsg: "the Specs board did not render" },
  );
}

// Wait for a Spec card (keyed by id via data-testid) to appear on the board, then click it.
async function openSpecCard(specId, timeoutMs) {
  const sel = '[data-testid="spec-card-' + specId + '"]';
  await browser.waitUntil(
    async () => browser.execute((s) => !!document.querySelector(s), sel),
    {
      timeout: timeoutMs || 20000,
      timeoutMsg: "the Spec card " + specId + " never appeared on the board (real-time board refresh)",
    },
  );
  await browser.execute((s) => document.querySelector(s).click(), sel);
}

// The labels of the PRD-detail tabs (role="tab"). Scoped away from the review's OWN repo tablist
// (aria-label="Repositories") so it only reads the detail-level tabs.
async function detailTabLabels() {
  return browser.execute(() =>
    Array.from(document.querySelectorAll('[role="tab"]'))
      .filter((el) => !el.closest('[role="tablist"][aria-label="Repositories"]'))
      .map((el) => (el.textContent || "").trim()),
  );
}

// Click the PRD-detail tab whose label contains `label` (never a repo tab).
async function clickDetailTab(label) {
  const ok = await browser.execute((lbl) => {
    const tab = Array.from(document.querySelectorAll('[role="tab"]'))
      .filter((el) => !el.closest('[role="tablist"][aria-label="Repositories"]'))
      .find((el) => (el.textContent || "").indexOf(lbl) !== -1);
    if (tab) {
      tab.click();
      return true;
    }
    return false;
  }, label);
  if (!ok) throw new Error("the '" + label + "' detail tab was not present/clickable");
}

// -------------------------------------------------------------------------------------------
// Review DOM helpers — the review screen is plain React DOM (no WebGL), so read it directly. These
// are the SAME selectors the ported review components emit (verified against v2 working tree).
// -------------------------------------------------------------------------------------------

const reviewText = () => browser.execute(() => document.body.innerText || "");
const hasElement = (selector) => browser.execute((s) => !!document.querySelector(s), selector);
const countElements = (selector) =>
  browser.execute((s) => document.querySelectorAll(s).length, selector);

// -------------------------------------------------------------------------------------------
// NEW placement (PDR-0028): the Review TAB shows COMMENTS + an "Open review" button; the full
// diff/tree VIEWER lives on the Spec-scoped page `/work/$specId/review`, reached ONLY by that
// button. These helpers drive the tab→page flow.
// -------------------------------------------------------------------------------------------

// Wait for the Review TAB's entry card ("Open review" button) to mount (the comments surface).
async function waitForReviewTab(timeoutMs) {
  await browser.waitUntil(
    async () => browser.execute(() => !!document.querySelector('[data-testid="open-review"]')),
    { timeout: timeoutMs || 20000, timeoutMsg: "the Review tab (Open review button) did not mount" },
  );
}

// Whether the diff VIEWER is absent from the current DOM (the tab must NOT embed it).
async function hasDiffViewer() {
  return browser.execute(
    () =>
      !!document.querySelector('[data-testid="diff-scroll"], [data-testid="review-diff"], [data-testid="review-page"]'),
  );
}

// Click the tab's "Open review" button → navigate to the full-screen review page.
async function clickOpenReview() {
  const ok = await browser.execute(() => {
    const btn = document.querySelector('[data-testid="open-review"]');
    if (!btn) return false;
    btn.click();
    return true;
  });
  if (!ok) throw new Error("the tab's 'Open review' button was not present/clickable");
}

// Click the review page's Back control → return to the Spec detail (`/work/$specId`).
async function clickReviewBack() {
  return browser.execute(() => {
    const btn = document.querySelector('[data-testid="review-back"]');
    if (!btn) return false;
    btn.click();
    return true;
  });
}

// Wait for the full-screen review PAGE to mount: its container + the multi-repo tablist (which only
// renders once the workspace's repos have enumerated, so this doubles as "repos loaded"). Reached
// via `clickOpenReview()` — the tab itself never mounts the viewer.
async function waitForReviewScreen(timeoutMs) {
  await browser.waitUntil(
    async () =>
      browser.execute(
        () =>
          !!document.querySelector('[data-testid="review-page"]') &&
          !!document.querySelector('[role="tablist"][aria-label="Repositories"]'),
      ),
    { timeout: timeoutMs || 30000, timeoutMsg: "the full-screen review page (review-page + repo tablist) did not mount" },
  );
}

// The rendered repo TAB names (each an aggregated (workspace, repository) identity). Text is
// `<name><changedFileCount>`; strip the trailing count for the bare name.
async function reviewRepoTabNames() {
  return browser.execute(() =>
    Array.from(
      document.querySelectorAll('[role="tablist"][aria-label="Repositories"] [role="tab"]'),
    ).map((el) => (el.textContent || "").replace(/\d+\s*$/, "").trim()),
  );
}

// Wait until the review has enumerated >= minCount repo tabs.
async function waitForReviewRepoTabs(minCount, timeoutMs) {
  let last = [];
  await browser.waitUntil(
    async () => {
      last = await reviewRepoTabNames();
      return last.length >= (minCount || 1);
    },
    {
      timeout: timeoutMs || 30000,
      timeoutMsg:
        "review repo tabs never reached " + (minCount || 1) + " (got " + JSON.stringify(last) + ")",
    },
  );
  return last;
}

// Select the repo tab whose bare name equals `name` (click it). Throws if absent.
async function selectRepoTabByName(name) {
  const ok = await browser.execute((n) => {
    const tab = Array.from(
      document.querySelectorAll('[role="tablist"][aria-label="Repositories"] [role="tab"]'),
    ).find((el) => (el.textContent || "").replace(/\d+\s*$/, "").trim() === n);
    if (!tab) return false;
    tab.click();
    return true;
  }, name);
  if (!ok) throw new Error("repo tab '" + name + "' not found in the review tablist");
  await browser.pause(200);
}

// The diff scroll surface metrics with @pierre/diffs (PDR-0028): the per-file sections are LIGHT
// DOM (`[data-testid="diff-file"]`, our own collapsible chrome carrying the file path + status);
// the diff line rows live INSIDE @pierre's `<pierre-diff>` shadow DOM (virtualized by the lib), so
// we no longer count individual line rows here — the library owns virtualization. We report the
// file-section count + the scroll geometry (a tall scroll surface over a windowed viewport).
async function diffVirtualization() {
  return browser.execute(function () {
    const scroll = document.querySelector('[data-testid="diff-scroll"]');
    if (!scroll) return null;
    const fileSections = scroll.querySelectorAll('[data-testid="diff-file"]').length;
    return {
      fileSections,
      // Back-compat fields some assertions still read; with @pierre the line rows are in shadow DOM.
      domLineRows: fileSections,
      totalRows: fileSections,
      virtualHeight: scroll.scrollHeight,
      viewportHeight: scroll.clientHeight,
    };
  });
}

async function scrollDiff(dy) {
  await browser.execute(function (dy) {
    const scroll = document.querySelector('[data-testid="diff-scroll"]');
    if (scroll) {
      scroll.scrollTop = scroll.scrollTop + dy;
      scroll.dispatchEvent(new Event("scroll"));
    }
  }, dy);
  await browser.pause(250);
}

// Select the diff SURFACE (working_tree / staged / range / branches / commit) via the header
// `<select aria-label="Diff surface">`. Wait for the select first — switching the active repo
// re-pulls its diff and the header can briefly re-render around it — so a transient re-mount does
// not read as "gone".
async function selectDiffSurface(kind) {
  await browser.waitUntil(
    async () => browser.execute(() => !!document.querySelector('select[aria-label="Diff surface"]')),
    { timeout: 10000, timeoutMsg: "diff surface <select> not found" },
  );
  await browser.execute(function (kind) {
    const sel = document.querySelector('select[aria-label="Diff surface"]');
    sel.value = kind;
    sel.dispatchEvent(new Event("change", { bubbles: true }));
  }, kind);
  await browser.pause(200);
}

// Type a base/target rev-spec into the range/branches inputs. Sets them in TWO steps (base, then
// target) with a re-render pause between, so the second onChange sees the updated target object and
// neither field is clobbered by a stale closure.
async function setRangeRevs(base, target) {
  await browser.waitUntil(
    async () => browser.execute(() => !!document.querySelector('input[aria-label="Base rev"]')),
    { timeout: 8000, timeoutMsg: "range rev inputs never appeared (pick the branches/range surface first)" },
  );
  const setVal = function (label, v) {
    return browser.execute(
      function (label, v) {
        const el = document.querySelector('input[aria-label="' + label + '"]');
        if (!el) return false;
        const proto = Object.getPrototypeOf(el);
        const desc = Object.getOwnPropertyDescriptor(proto, "value");
        desc.set.call(el, v);
        el.dispatchEvent(new Event("input", { bubbles: true }));
        el.dispatchEvent(new Event("change", { bubbles: true }));
        return true;
      },
      label,
      v,
    );
  };
  await setVal("Base rev", base);
  await browser.pause(150);
  await setVal("Target rev", target);
  await browser.pause(200);
}

// CLEAR the range/branches rev-spec inputs (Base rev + Target rev) back to empty. The review keeps
// a PER-REPO target (review-page.tsx `targets` map) that SURVIVES across `it` blocks — a prior test
// that typed base/target on a repo (e.g. the big-diff test's `setRangeRevs("base","feature")` on
// `api`) leaves them populated, so a later `range`/`branches` switch on that SAME repo is already
// `targetReady` and loads the diff instead of showing the rev-spec prompt. Emptying the fields first
// restores the "no rev-spec typed yet" precondition Finding 43 asserts. Fires input+change like a
// real edit; no-op-safe when the fields are already empty.
async function clearRangeRevs() {
  await browser.waitUntil(
    async () => browser.execute(() => !!document.querySelector('input[aria-label="Base rev"]')),
    { timeout: 8000, timeoutMsg: "range rev inputs never appeared (pick the branches/range surface first)" },
  );
  await browser.execute(function () {
    const clear = function (label) {
      const el = document.querySelector('input[aria-label="' + label + '"]');
      if (!el) return;
      const proto = Object.getPrototypeOf(el);
      const desc = Object.getOwnPropertyDescriptor(proto, "value");
      desc.set.call(el, "");
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
    };
    clear("Base rev");
    clear("Target rev");
  });
  await browser.pause(200);
}

// Pick the LIVING or PINNED variety via the header's "Review variety" toggle group.
async function selectVariety(mode) {
  const ok = await browser.execute(function (mode) {
    const group = document.querySelector('[role="group"][aria-label="Review variety"]');
    if (!group) return false;
    const btn = Array.from(group.querySelectorAll("button")).find(
      (b) => (b.textContent || "").trim().toLowerCase() === mode,
    );
    if (!btn) return false;
    btn.click();
    return true;
  }, mode);
  if (!ok) throw new Error("variety toggle '" + mode + "' not found");
  await browser.pause(200);
}

// The effective mode/backend the header renders for the active diff (proves LIVING and that the
// diff engine actually produced the diff, e.g. gix / git_cli).
//
// Finding 48: the diff ENGINE (`gix`/`git_cli`) and the raw `mode` word are no longer user-facing
// TEXT — the engine was pure diagnostic jargon and the Living/Pinned choice already has its own
// labelled toggle. They survive ONLY as diagnostic `data-diff-backend` / `data-diff-mode` attributes
// on the sub-header (invisible), so the "a real engine produced the diff" check reads them there,
// while `subHeaderShowsGix` proves the jargon is GONE from the visible sub-header text.
async function reviewHeaderInfo() {
  return browser.execute(function () {
    const text = document.body.innerText || "";
    const sub = document.querySelector("[data-diff-backend]");
    const backend = sub ? sub.getAttribute("data-diff-backend") || "" : "";
    const subText = sub ? sub.innerText || "" : "";
    return {
      // `hasLiving`/`hasPinned` come from the intentional Living/Pinned TOGGLE (kept), not the
      // removed sub-header jargon.
      hasLiving: /\bliving\b/i.test(text),
      hasPinned: /\bpinned\b/i.test(text),
      // The engine, read from the diagnostic attribute (no longer visible text).
      hasGix: /\bgix\b/.test(backend),
      hasGitCli: /git_cli/.test(backend),
      hasBackendAttr: backend.length > 0,
      // Finding 48 — the sub-header's VISIBLE text must NOT expose the engine jargon.
      subHeaderShowsGix: /\bgix\b/i.test(subText),
    };
  });
}

// Wait for the diff viewer to actually render its file sections (the diff loaded). With @pierre the
// per-file collapsible sections (light DOM) appear once the `repo_diff` resolves; the code lines
// stream in inside the shadow DOM afterward.
async function waitForDiffLoaded(timeoutMs) {
  await browser.waitUntil(
    async () => {
      const v = await diffVirtualization();
      return v && v.fileSections > 0;
    },
    { timeout: timeoutMs || 20000, timeoutMsg: "diff viewer never rendered any file sections" },
  );
}

// The rendered changed-file PATHS on the diff (from the light-DOM file-section headers). Lets a test
// assert a specific file is in the diff without reaching into @pierre's shadow DOM.
async function diffFilePaths() {
  return browser.execute(() =>
    Array.from(document.querySelectorAll('[data-testid="diff-file"]')).map((el) =>
      (el.getAttribute("data-path") || "").trim(),
    ),
  );
}

// Anchor a SINGLE-LINE comment on the diff via the NEW native flow (Finding 63). `onLineNumberClick`
// was REMOVED — a bare number click no longer opens a composer (that parasite is gone), so we drive
// @pierre's own single-line affordance: HOVER a changed line NUMBER cell (reveals the native gutter
// "+") AND select that one line (a press+release on its number, no drag → a single-line selection),
// then PRESS the native "+" (`[data-utility-button]`). @pierre fires `onGutterUtilityClick({start,end})`
// with `start === end` for the one line → `diff-view.tsx` opens a MONO-line composer
// (`[data-testid="line-composer"]`, no range label). Everything lives in the open shadow DOM, so the
// browser-side helpers pierce open shadow roots. This is the same native mechanism `addRangeComment`
// uses, just over ONE line instead of a drag-selected plage — proving single-line stays discoverable.
async function addLineComment(body) {
  // 1) Reveal + press the "+" over ONE changed line: hover the number cell (native "+" appears) and
  //    select that single line, so pressing the "+" anchors exactly that line (start === end).
  //
  //    ROBUSTNESS: this runs right after two MCP `review_comment_post`s + a global-comment post in the
  //    GLOBAL/LINE/RANGE gate, each of which re-pulls the diff and can transiently re-paint @pierre's
  //    shadow DOM (the `[data-column-number]` number cells briefly vanish). The old single-shot search
  //    threw "no anchorable diff line-number cell…" if it happened to fire mid-repaint. Now we WAIT for
  //    the number cells to be (re)painted and RETRY the find+gesture inside `waitUntil` — the same
  //    "lines are actually rendered" precondition the working `addRangeComment`/`dragSelectLineNumbers`
  //    path relies on, using the identical shadow-piercing `[data-column-number]` selector.
  let targeted = null;
  await browser.waitUntil(
    async () => {
      targeted = await browser.execute(function () {
        function deep(sel) {
          var out = [];
          (function w(n) {
            if (!n || !n.querySelectorAll) return;
            out.push.apply(out, n.querySelectorAll(sel));
            for (var i = 0, es = n.querySelectorAll("*"); i < es.length; i++)
              if (es[i].shadowRoot) w(es[i].shadowRoot);
          })(document);
          return out;
        }
        var cells = deep("[data-column-number]").filter(function (c) {
          return /^\s*\d+\s*$/.test(c.textContent || "");
        });
        var changed = cells.filter(function (c) {
          return (c.getAttribute("data-line-type") || "") !== "context";
        });
        var cell = changed[0] || cells[0];
        if (!cell) return null;
        var r = cell.getBoundingClientRect();
        var x = Math.round(r.left + r.width / 2);
        var yTop = Math.round(r.top + r.height * 0.3);
        var yBot = Math.round(r.top + r.height * 0.7);
        var opts = function (yy, extra) {
          return Object.assign(
            { bubbles: true, cancelable: true, composed: true, pointerId: 3, pointerType: "mouse", button: 0, buttons: 1, clientX: x, clientY: yy, view: window },
            extra || {},
          );
        };
        // Hover to reveal the native gutter "+", then press + micro-drag WITHIN the one number cell (a
        // real pressed move, not a bare click) so @pierre registers a SINGLE-line selection (start === end)
        // and auto-places its "+" on it — the exact user gesture for a quick one-line comment.
        cell.dispatchEvent(new PointerEvent("pointerover", opts(yTop, { buttons: 0 })));
        cell.dispatchEvent(new PointerEvent("pointermove", opts(yTop, { buttons: 0 })));
        cell.dispatchEvent(new PointerEvent("pointerdown", opts(yTop)));
        document.dispatchEvent(new PointerEvent("pointermove", opts(yBot)));
        document.dispatchEvent(new PointerEvent("pointerup", opts(yBot, { buttons: 0 })));
        return (cell.textContent || "").trim();
      });
      return !!targeted;
    },
    { timeout: 15000, timeoutMsg: "no anchorable diff line-number cell to select a single line" },
  );

  // 2) Wait for the native "+" and press+release it (a real pointer gesture, not `.click()`), which
  //    fires `onGutterUtilityClick({start:line,end:line})` → the mono-line composer opens.
  await browser.waitUntil(
    async () =>
      browser.execute(function () {
        function deep(sel) {
          var out = [];
          (function w(n) {
            if (!n || !n.querySelectorAll) return;
            out.push.apply(out, n.querySelectorAll(sel));
            for (var i = 0, es = n.querySelectorAll("*"); i < es.length; i++)
              if (es[i].shadowRoot) w(es[i].shadowRoot);
          })(document);
          return out;
        }
        // Already opened by a prior press? then we're done.
        if (deep('[data-testid="line-composer"] textarea').length > 0) return true;
        var btn = deep("[data-utility-button]")[0];
        if (!btn) return false;
        var r = btn.getBoundingClientRect();
        var cx = Math.round(r.left + r.width / 2);
        var cy = Math.round(r.top + r.height / 2);
        var opts = function (extra) {
          return Object.assign(
            { bubbles: true, cancelable: true, composed: true, pointerId: 4, pointerType: "mouse", button: 0, buttons: 1, clientX: cx, clientY: cy, view: window },
            extra || {},
          );
        };
        btn.dispatchEvent(new PointerEvent("pointerdown", opts()));
        document.dispatchEvent(new PointerEvent("pointerup", opts({ buttons: 0 })));
        return true;
      }),
    { timeout: 10000, timeoutMsg: "no native gutter '+' opened a single-line composer" },
  );
  await browser.pause(200);

  // 3) Type + submit in the opened mono-line composer (shadow-pierced).
  const typed = await browser.execute(function (body) {
    function deep(sel) {
      var out = [];
      (function w(n) {
        if (!n || !n.querySelectorAll) return;
        out.push.apply(out, n.querySelectorAll(sel));
        for (var i = 0, es = n.querySelectorAll("*"); i < es.length; i++)
          if (es[i].shadowRoot) w(es[i].shadowRoot);
      })(document);
      return out;
    }
    var ta = deep('[data-testid="line-composer"] textarea')[0];
    if (!ta) return false;
    var proto = Object.getPrototypeOf(ta);
    var desc = Object.getOwnPropertyDescriptor(proto, "value");
    desc.set.call(ta, body);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    var scope = ta.closest('[data-testid="line-composer"]') || ta.parentElement;
    var btn = Array.prototype.slice.call((scope || document).querySelectorAll("button")).find(function (b) {
      return /comment/i.test((b.textContent || "").trim());
    });
    if (btn) btn.click();
    return true;
  }, body);
  if (!typed) throw new Error("the single-line composer textarea never opened");
  await browser.pause(300);
  return true;
}

// Open the RIGHT comments panel's "New comment" composer, type `body`, submit → a GLOBAL comment.
async function addGlobalComment(body) {
  const opened = await browser.execute(function () {
    const panel = document.querySelector('aside[aria-label="Comments"]');
    if (!panel) return false;
    const btn = Array.from(panel.querySelectorAll("button")).find(
      (b) => (b.textContent || "").trim() === "New comment",
    );
    if (!btn) return false;
    btn.click();
    return true;
  });
  if (!opened) throw new Error("comments panel 'New comment' button not found");
  await browser.pause(150);
  await typeIntoComposer('aside[aria-label="Comments"] textarea', body);
}

// Type `body` into the composer textarea matched by `selector` and click its Comment/Reply button.
async function typeIntoComposer(selector, body) {
  await browser.waitUntil(
    async () => browser.execute((s) => !!document.querySelector(s), selector),
    { timeout: 8000, timeoutMsg: "composer textarea not found: " + selector },
  );
  await browser.execute(
    function (selector, body) {
      const ta = document.querySelector(selector);
      const proto = Object.getPrototypeOf(ta);
      const desc = Object.getOwnPropertyDescriptor(proto, "value");
      desc.set.call(ta, body);
      ta.dispatchEvent(new Event("input", { bubbles: true }));
      const container = ta.closest("div");
      const scope = container ? container.parentElement || container : document;
      const btn = Array.from(scope.querySelectorAll("button")).find((b) =>
        /comment|reply/i.test((b.textContent || "").trim()),
      );
      if (btn) btn.click();
    },
    selector,
    body,
  );
  await browser.pause(300);
}

// The count of anchor MARKERS on the diff (an inline anchored thread on a line) — proves anchors
// show. @pierre may render annotations inside the `<pierre-diff>` shadow DOM, so pierce shadow roots.
const anchorMarkerCount = () =>
  browser.execute(function () {
    function deepCount(selector) {
      let n = 0;
      (function walk(node) {
        if (!node || !node.querySelectorAll) return;
        n += node.querySelectorAll(selector).length;
        for (const el of node.querySelectorAll("*")) if (el.shadowRoot) walk(el.shadowRoot);
      })(document);
      return n;
    }
    return deepCount('[data-testid="anchor-marker"]');
  });
// The count of rendered comment thread cards (panel + inline).
const threadCount = () => countElements('[data-testid="comment-thread"]');

// Click a control (by its `data-testid`) INSIDE the thread card whose text contains `bodyText`.
async function clickThreadControl(bodyText, testid) {
  return browser.execute(
    function (bodyText, testid) {
      const cards = Array.from(document.querySelectorAll('[data-testid="comment-thread"]'));
      const card = cards.find((c) => (c.textContent || "").indexOf(bodyText) !== -1);
      if (!card) return false;
      const btn = card.querySelector('[data-testid="' + testid + '"]');
      if (!btn) return false;
      btn.click();
      return true;
    },
    bodyText,
    testid,
  );
}

// Wait for a thread card containing `bodyText` to render.
async function waitForThreadWithBody(bodyText, timeoutMs) {
  await browser.waitUntil(
    async () =>
      browser.execute(function (bodyText) {
        return Array.from(document.querySelectorAll('[data-testid="comment-thread"]')).some(
          (c) => (c.textContent || "").indexOf(bodyText) !== -1,
        );
      }, bodyText),
    { timeout: timeoutMs || 15000, timeoutMsg: "thread card with body '" + bodyText + "' never rendered" },
  );
}

// Finding 62 — is the thread carrying `bodyText` shown RESOLVED in the UI? A resolved thread card drops
// its `[data-testid="resolve-comment"]` control (it renders a Reopen button instead), so the absence of
// that control on a card that still shows the body is the resolved signal. Used to time the optimistic
// resolve flip (the item changes before the backend round-trip).
async function threadResolvedInUi(bodyText) {
  return browser.execute(function (bodyText) {
    const card = Array.from(document.querySelectorAll('[data-testid="comment-thread"]')).find(
      (c) => (c.textContent || "").indexOf(bodyText) !== -1,
    );
    if (!card) return false;
    return !card.querySelector('[data-testid="resolve-comment"]');
  }, bodyText);
}

// Close the RIGHT comments panel via the header toggle (so the review does NOT auto-mark-read the
// feed — the unread counter then accumulates). Idempotent. Returns whether the toggle was actuated.
async function closeCommentsPanel() {
  return browser.execute(function () {
    const btn = document.querySelector('button[aria-label="Hide comments panel"]');
    if (!btn) return false; // already closed (the button reads "Show comments panel")
    btn.click();
    return true;
  });
}

// The current unread-badge count on the header comments toggle (0 when the badge is absent).
async function unreadBadgeCount() {
  return browser.execute(function () {
    const b = document.querySelector('[data-testid="unread-badge"]');
    return b ? parseInt((b.textContent || "0").replace(/\D/g, ""), 10) || 0 : 0;
  });
}

// -------------------------------------------------------------------------------------------
// Finding 42 — the LEFT changed-files tree (@pierre/trees) must show the N changed files (not an
// empty tree) once the diff is loaded. Reads the header count, the empty-state flag, and the number
// of rendered tree nodes (a secondary "the tree actually painted rows" signal).
// -------------------------------------------------------------------------------------------
// The LAST render error any `<ErrorBoundary>` caught, mirrored onto `window.__nyxLastTreeError` by
// error-boundary.tsx (message + stack + React component stack). wdio does NOT pipe the renderer
// console into its log, so the boundary's `console.error` is invisible here; this seam lets a spec
// read the REAL stack of a swallowed crash (e.g. the @pierre/trees file-tree throw) from the outside.
// Returns null when nothing was caught. Diagnostic — never gates a pass.
async function lastTreeError() {
  return browser.execute(() => window.__nyxLastTreeError || null);
}

async function reviewFileTreeInfo() {
  return browser.execute(function () {
    const panel = document.querySelector('[data-testid="review-file-tree"]');
    if (!panel) return null;
    const countEl = panel.querySelector('[data-testid="file-tree-count"]');
    const count = countEl ? parseInt((countEl.textContent || "").trim(), 10) : -1;
    const empty = /No changed files\./i.test(panel.innerText || "");
    // @pierre/trees paints its file/dir ROWS INSIDE the `<file-tree-container>` element's OPEN
    // shadow root (`render/FileTree.js` → `host.attachShadow({ mode: "open" })`), each row carrying
    // `role="treeitem"` + `data-item-path` (`render/rowAttributes.js`) — the folder rows included.
    // A plain `panel.querySelectorAll(...)` does NOT pierce that boundary (it would always return 0
    // even on a fully-painted tree), so — like `addLineComment`/`anchorMarkerCount` do for the
    // @pierre/diffs shadow DOM — walk the light DOM + every open shadowRoot under the panel and
    // count the real @pierre/trees rows.
    function deepCount(root, selector) {
      var n = 0;
      (function walk(node) {
        if (!node || !node.querySelectorAll) return;
        n += node.querySelectorAll(selector).length;
        for (const el of node.querySelectorAll("*")) if (el.shadowRoot) walk(el.shadowRoot);
      })(root);
      return n;
    }
    const nodes = deepCount(panel, '[role="treeitem"], [data-item-path]');
    return { count: count, empty: empty, nodes: nodes };
  });
}

// -------------------------------------------------------------------------------------------
// Finding 43 — selecting a Commit / Range / Branches surface must NOT throw a raw backend error
// before the rev-spec(s) are typed; the viewer shows a clear "fill in the fields" prompt instead.
// -------------------------------------------------------------------------------------------
async function waitForAwaitingInput(timeoutMs) {
  await browser.waitUntil(
    async () => browser.execute(() => !!document.querySelector('[data-testid="diff-awaiting-input"]')),
    { timeout: timeoutMs || 8000, timeoutMsg: "the rev-spec prompt (diff-awaiting-input) never appeared" },
  );
}

// Whether the diff area currently shows a RAW backend rev-spec error (the pre-fix symptom). Matches
// the BACKEND phrasing only (`git_diff::target_from_params`), never the friendly prompt copy (which
// itself contains the words "rev-spec").
async function diffHasRawRevError() {
  return browser.execute(() =>
    /diff kind '|requires a 'rev'|requires 'base'/i.test(document.body.innerText || ""),
  );
}

// -------------------------------------------------------------------------------------------
// Finding 52 — the per-file header must PIN to the top of the scrolling diff area and stay VISIBLE
// (not clipped, not behind the ReviewHeader) while scrolling. Reports the geometry of the header
// currently pinned at the top of `diff-scroll`, plus the ReviewHeader's bottom edge.
// -------------------------------------------------------------------------------------------
async function reviewStickyHeaderInfo() {
  return browser.execute(function () {
    const scroll = document.querySelector('[data-testid="diff-scroll"]');
    if (!scroll) return null;
    const sr = scroll.getBoundingClientRect();
    const tablist = document.querySelector('[role="tablist"][aria-label="Repositories"]');
    const pageHeader = tablist ? tablist.closest("header") : null;
    const pageHeaderBottom = pageHeader ? pageHeader.getBoundingClientRect().bottom : 0;
    const headers = Array.prototype.slice.call(scroll.querySelectorAll('[data-row="file-header"]'));
    let pinned = null;
    for (const el of headers) {
      const r = el.getBoundingClientRect();
      const cs = getComputedStyle(el);
      // A header pinned at the top of the scroll viewport (its top ~= the viewport top, still on
      // screen). Among several, the LOWEST such top is the currently-active sticky one.
      if (r.top <= sr.top + 6 && r.bottom > sr.top + 2) {
        if (!pinned || r.top > pinned.top) {
          pinned = { top: r.top, bottom: r.bottom, height: r.height, position: cs.position, zIndex: cs.zIndex };
        }
      }
    }
    return { viewportTop: sr.top, pageHeaderBottom: pageHeaderBottom, headerCount: headers.length, pinned: pinned };
  });
}

// Navigate to a Spec's full-screen review page from anywhere: board → its card → Review tab →
// "Open review". Used to compare TWO Specs sharing the same dev-workspace repos (Finding 41).
async function openReviewForSpec(specId) {
  await openBoard();
  await openSpecCard(specId, 20000);
  await clickDetailTab("Review");
  await waitForReviewTab(20000);
  await clickOpenReview();
  await waitForReviewScreen(30000);
}

// -------------------------------------------------------------------------------------------
// Point 9 (static) — the review page is REACHABLE ONLY through the Spec's Review tab. Assert at the
// SOURCE (PDR-0028): the review route is Spec-SCOPED (`/work/$specId/review`), NOT a top-level
// `/review`, and there is NO `/review` nav item and NO `window.__nyxReview` seam. (The spec runs
// under Node, so it reads the repo source directly — the "assert statique" the done-criterion calls
// for.)
// -------------------------------------------------------------------------------------------

function repoRoot() {
  return path.resolve(__dirname, "..", "..");
}

// Strip JS/TS comments (block + line) so a `/review` mention in PROSE — e.g. a route docstring
// explaining WHY there is no bare `/review` route — never trips the source-literal checks below. Only
// real CODE literals should count. The `[^:]` guard keeps `://` (URLs) intact.
function stripComments(src) {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|[^:])\/\/[^\n]*/g, "$1");
}

function assertNoReviewRoute(assert) {
  const rendererSrc = path.join(repoRoot(), "apps/desktop/renderer/src");
  const router = stripComments(fs.readFileSync(path.join(rendererSrc, "router.tsx"), "utf8"));
  // NO TOP-LEVEL `/review` route path. The bare-literal regex only matches a `/review` bounded by
  // quotes on BOTH sides, so the legitimate Spec-scoped `/work/$specId/review` route (whose `/review`
  // is preceded by `…specId`, not a quote) is NOT flagged — but a prose `` `/review` `` in a comment
  // WOULD be, which is why comments are stripped first.
  assert(!/path:\s*["'`]\/review["'`]/.test(router), "router.tsx declares NO top-level '/review' route path");
  assert(
    !/["'`]\/review["'`]/.test(router),
    "router.tsx has NO bare '/review' path literal (the Spec-scoped '/work/$specId/review' route is allowed)",
  );
  // The review route that DOES exist is Spec-scoped under the detail (`/work/$specId/review`).
  assert(
    /["'`]\/work\/\$specId\/review["'`]/.test(router),
    "router.tsx carries the Spec-scoped '/work/$specId/review' route",
  );

  const baseLayout = stripComments(
    fs.readFileSync(path.join(rendererSrc, "components/terminals/base-layout.tsx"), "utf8"),
  );
  assert(
    !/to:\s*["'`]\/review["'`]/.test(baseLayout),
    "base-layout.tsx has NO { to: '/review' } top-level nav item",
  );

  // No `window.__nyxReview` seam survives anywhere in the renderer (removed by Finding 4).
  const offenders = [];
  (function walk(dir) {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(p);
      else if (/\.(ts|tsx)$/.test(entry.name) && fs.readFileSync(p, "utf8").indexOf("__nyxReview") !== -1)
        offenders.push(p);
    }
  })(rendererSrc);
  assert.strictEqual(offenders.length, 0, "NO __nyxReview seam remains in the renderer: " + offenders.join(", "));
}

// -------------------------------------------------------------------------------------------
// The MCP loopback — Spec authoring + the AGENT acting (reply + structure a task) + enumerate the
// workspace's repository ids, exactly as a real agent does.
// -------------------------------------------------------------------------------------------

function mcpPort() {
  return process.env.NYX_E2E_MCP_PORT || process.env.NYX_MCP_PORT || "8779";
}

function mcpCall(method, params) {
  const body = JSON.stringify({ jsonrpc: "2.0", id: Date.now(), method, params });
  return new Promise((resolve, reject) => {
    const req = http.request(
      {
        host: "127.0.0.1",
        port: Number(mcpPort()),
        path: "/mcp",
        method: "POST",
        headers: { "content-type": "application/json", "content-length": Buffer.byteLength(body) },
      },
      (res) => {
        let data = "";
        res.on("data", (c) => (data += c));
        res.on("end", () => {
          let parsed;
          try {
            parsed = JSON.parse(data);
          } catch (e) {
            return reject(new Error("MCP response not JSON (status " + res.statusCode + "): " + data));
          }
          if (parsed.error) return reject(new Error("MCP " + method + " error: " + JSON.stringify(parsed.error)));
          resolve(parsed.result);
        });
      },
    );
    req.on("error", reject);
    req.write(body);
    req.end();
  });
}

async function waitForMcp(timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 30000);
  let lastErr = null;
  while (Date.now() < deadline) {
    try {
      await new Promise((resolve, reject) => {
        const req = http.request(
          { host: "127.0.0.1", port: Number(mcpPort()), path: "/health", method: "GET" },
          (res) => {
            res.resume();
            res.statusCode === 200 ? resolve() : reject(new Error("health " + res.statusCode));
          },
        );
        req.on("error", reject);
        req.end();
      });
      return;
    } catch (e) {
      lastErr = e;
      await browser.pause(300);
    }
  }
  throw new Error("MCP never healthy on 127.0.0.1:" + mcpPort() + " (" + (lastErr && lastErr.message) + ")");
}

// The MCP tool payload: prefer structuredContent, fall back to the mirrored text block.
function mcpStructured(result) {
  if (result && result.structuredContent) return result.structuredContent;
  if (result && Array.isArray(result.content)) {
    const text = result.content.find((c) => c.type === "text");
    if (text) {
      try {
        return JSON.parse(text.text);
      } catch {}
    }
  }
  return result;
}

// Call a tool by name; return the parsed tool payload.
async function tool(name, args) {
  return mcpStructured(await mcpCall("tools/call", { name, arguments: args }));
}

// Enumerate the workspace's repos via the MCP `workspace_git` tool → the (name → repository_id) the
// old `window.__nyxReview.repos()` seam used to hand back. Returns `[{ repository_id, name, ... }]`.
async function workspaceGitRepos(workspaceId) {
  const out = await tool("workspace_git", { workspace_id: workspaceId });
  const git = (out && out.git) || out;
  return (git && git.repos) || [];
}

// The AGENT replies into a thread (root `parentId`) via `review_comment_post` — the exact tool a
// real agent uses (always author_type=agent). Returns the created comment.
async function agentReply(workspaceId, repositoryId, parentId, body) {
  const res = await tool("review_comment_post", {
    workspace_id: workspaceId,
    repository_id: repositoryId,
    parent_id: parentId,
    body,
  });
  return (res && res.comment) || res;
}

// The AGENT STRUCTURES a promoted (`proposed`) task via `review_task_structure` (grill step 2).
async function agentStructureTask(taskId, title, details, severity) {
  const res = await tool("review_task_structure", { id: taskId, title, details, severity });
  return (res && res.task) || res;
}

async function listComments(workspaceId, repositoryId) {
  const res = await tool("review_comment_list", { workspace_id: workspaceId, repository_id: repositoryId });
  return (res && res.comments) || [];
}

async function listTasks(workspaceId, repositoryId) {
  const res = await tool("review_task_list", { workspace_id: workspaceId, repository_id: repositoryId });
  return (res && res.tasks) || [];
}

// -------------------------------------------------------------------------------------------
// Finding 47 — the SHARED breadcrumb (`Work` / `<specKey>` / `Review`) replaces the board's bare
// `<h1>`, the Spec detail's hand-rolled nav and the review's `←` button. Read + navigate it.
// -------------------------------------------------------------------------------------------

// The breadcrumb's crumb labels, in order (from `[data-testid="breadcrumb"]`). `null` when absent.
async function breadcrumbCrumbs() {
  return browser.execute(function () {
    const nav = document.querySelector('[data-testid="breadcrumb"]');
    if (!nav) return null;
    return Array.from(nav.children)
      .map((el) => (el.textContent || "").trim())
      .filter((t) => t.length > 0 && t !== "/");
  });
}

// Click the breadcrumb crumb whose text CONTAINS `label` (a link crumb). Throws if not a link.
async function clickBreadcrumbCrumb(label) {
  const ok = await browser.execute(function (label) {
    const nav = document.querySelector('[data-testid="breadcrumb"]');
    if (!nav) return false;
    const crumb = Array.from(nav.querySelectorAll("a")).find(
      (a) => (a.textContent || "").indexOf(label) !== -1,
    );
    if (!crumb) return false;
    crumb.click();
    return true;
  }, label);
  if (!ok) throw new Error("breadcrumb crumb '" + label + "' (link) not found");
}

// -------------------------------------------------------------------------------------------
// Finding 46 — the sidebar toggles are uniform: the RIGHT comments rail has NO internal "Close
// panel" arrow (the header comment button is the single control), and BOTH flanks are driven by
// their header buttons. Read the state + drive the header toggles.
// -------------------------------------------------------------------------------------------

// Whether either rail carries the removed internal "Close panel" arrow (must be FALSE now).
async function hasInternalPanelCloseArrow() {
  return browser.execute(
    () => !!document.querySelector('aside button[aria-label="Close panel"]'),
  );
}

// Whether a rail (`aria-label`) is currently OPEN (the collapsible SidebarShell exposes `data-open`).
async function railOpen(ariaLabel) {
  return browser.execute(function (label) {
    const aside = document.querySelector('aside[aria-label="' + label + '"]');
    if (!aside) return null;
    return aside.getAttribute("data-open") !== "false" && aside.getAttribute("aria-hidden") !== "true";
  }, ariaLabel);
}

// Click a header panel toggle by its aria-label (e.g. "Hide files panel" / "Show files panel").
async function clickHeaderToggle(ariaLabelPrefix) {
  return browser.execute(function (prefix) {
    const btn = Array.from(document.querySelectorAll("button[aria-label]")).find((b) =>
      (b.getAttribute("aria-label") || "").indexOf(prefix) === 0,
    );
    if (!btn) return false;
    btn.click();
    return true;
  }, ariaLabelPrefix);
}

// -------------------------------------------------------------------------------------------
// Finding 45 — the repo tablist must not raise a parasitic VERTICAL scrollbar (the 2D-scroll look).
// Report whether the `Tabs.List` overflows vertically (scrollHeight > clientHeight).
// -------------------------------------------------------------------------------------------
async function repoTablistOverflowsY() {
  return browser.execute(function () {
    const list = document.querySelector('[role="tablist"][aria-label="Repositories"]');
    if (!list) return null;
    return list.scrollHeight > list.clientHeight + 1;
  });
}

// -------------------------------------------------------------------------------------------
// Finding 63 — the review uses @pierre's NATIVE gutter utility now (`onGutterUtilityClick`), so the
// "+" is the library's own `[data-utility-button]` (blue, `z-index:4`, positioned) — NOT our old
// custom `[data-testid="diff-add-comment"]` (removed: pairing a custom `renderGutterUtility` with
// `onGutterUtilityClick` throws, regression 61). It is placed lazily on the hovered/selected line, so
// this returns null until a hover/selection reveals it. Reports its stacking styles (finding 49's
// intent — a positioned, stacked "+" — is met by construction by the native button).
// -------------------------------------------------------------------------------------------
async function gutterAddButtonInfo() {
  return browser.execute(function () {
    function deep(selector) {
      const out = [];
      (function walk(node) {
        if (!node || !node.querySelectorAll) return;
        out.push.apply(out, node.querySelectorAll(selector));
        for (const el of node.querySelectorAll("*")) if (el.shadowRoot) walk(el.shadowRoot);
      })(document);
      return out;
    }
    const btn = deep("[data-utility-button]")[0];
    if (!btn) return null;
    const cs = getComputedStyle(btn);
    return { className: btn.className || "", position: cs.position, zIndex: cs.zIndex };
  });
}

// -------------------------------------------------------------------------------------------
// Finding 63 — the FRANC selection highlight: a NON-CONTROLLED drag over the line NUMBERS makes
// @pierre paint `[data-selected-line]` on the rows in real time (and keep them after release). Count
// them (shadow-pierced) to PROVE the drag produced a visible, persistent highlight.
// -------------------------------------------------------------------------------------------
async function selectedLineCount() {
  return browser.execute(function () {
    let n = 0;
    (function walk(node) {
      if (!node || !node.querySelectorAll) return;
      n += node.querySelectorAll("[data-selected-line]").length;
      for (const el of node.querySelectorAll("*")) if (el.shadowRoot) walk(el.shadowRoot);
    })(document);
    return n;
  });
}

// -------------------------------------------------------------------------------------------
// Finding 64 — the selection accent is our ROSE `--primary`, not @pierre's default BLUE
// (`--diffs-modified-base`). Resolve `var(--primary)` to a concrete rgb via a light-DOM probe, and
// read the NATIVE gutter "+" (`[data-utility-button]`, shadow-pierced) computed backgroundColor — it
// is recolored to `var(--primary)` in unsafeCSS, so the two must serialize identically (and must NOT
// be the blue accent #009fff/#69b1ff). `selectedBg` is the painted highlight (a color-mix, diagnostic).
// -------------------------------------------------------------------------------------------
async function selectionAccentColors() {
  return browser.execute(function () {
    function deep(selector) {
      const out = [];
      (function walk(node) {
        if (!node || !node.querySelectorAll) return;
        out.push.apply(out, node.querySelectorAll(selector));
        for (const el of node.querySelectorAll("*")) if (el.shadowRoot) walk(el.shadowRoot);
      })(document);
      return out;
    }
    const probe = document.createElement("span");
    probe.style.color = "var(--primary)";
    probe.style.display = "none";
    document.body.appendChild(probe);
    const primary = getComputedStyle(probe).color;
    probe.remove();
    const btn = deep("[data-utility-button]")[0];
    const utilityBg = btn ? getComputedStyle(btn).backgroundColor : null;
    const sel = deep("[data-selected-line]")[0];
    const selectedBg = sel ? getComputedStyle(sel).backgroundColor : null;
    return { primary: primary, utilityBg: utilityBg, selectedBg: selectedBg, hasButton: !!btn };
  });
}

// -------------------------------------------------------------------------------------------
// Finding 65b — a PLAIN (no-drag) click on a single changed line-NUMBER cell (shadow-pierced). Used to
// prove it leaves NO persistent selection: @pierre paints a 1-line highlight on the click, and our
// `onLineSelected` clear empties it on release (before the fix it stayed stuck with a "+").
// -------------------------------------------------------------------------------------------
async function clickLineNumberOnce() {
  return browser.execute(function () {
    function deep(selector) {
      const out = [];
      (function walk(node) {
        if (!node || !node.querySelectorAll) return;
        out.push.apply(out, node.querySelectorAll(selector));
        for (const el of node.querySelectorAll("*")) if (el.shadowRoot) walk(el.shadowRoot);
      })(document);
      return out;
    }
    const cells = deep("[data-column-number]").filter((c) => /^\s*\d+\s*$/.test(c.textContent || ""));
    const changed = cells.filter((c) => (c.getAttribute("data-line-type") || "") !== "context");
    const cell = changed[0] || cells[0];
    if (!cell) return false;
    const r = cell.getBoundingClientRect();
    const x = Math.round(r.left + r.width / 2);
    const y = Math.round(r.top + r.height / 2);
    const opts = (extra) =>
      Object.assign(
        { bubbles: true, cancelable: true, composed: true, pointerId: 5, pointerType: "mouse", button: 0, buttons: 1, clientX: x, clientY: y, view: window },
        extra || {},
      );
    // pointerdown + immediate pointerup at the SAME spot (no move) = a plain click, not a drag.
    cell.dispatchEvent(new PointerEvent("pointerdown", opts()));
    document.dispatchEvent(new PointerEvent("pointerup", opts({ buttons: 0 })));
    return true;
  });
}

// -------------------------------------------------------------------------------------------
// Finding 66 — the diff is layout-virtualized ACROSS files with the browser's NATIVE content-visibility:
// every file BODY carries `content-visibility: auto`, so an OFF-SCREEN body is not laid out. Count the
// file bodies, how many carry the applied CSS, and how many are currently SKIPPED (`checkVisibility({
// contentVisibilityAuto:true})` returns false when content-visibility skips the subtree). `measurable`
// is 0 when the runtime lacks that `checkVisibility` option, so the caller can gate the skip assertion.
// -------------------------------------------------------------------------------------------
async function diffContentVisibility() {
  return browser.execute(function () {
    const scroll = document.querySelector('[data-testid="diff-scroll"]');
    if (!scroll) return null;
    const cards = Array.prototype.slice.call(scroll.querySelectorAll('[data-testid="diff-file"]'));
    let bodies = 0;
    let withCv = 0;
    let skipped = 0;
    let measurable = 0;
    for (const card of cards) {
      const body = card.querySelector('[style*="content-visibility"]');
      if (!body) continue;
      bodies++;
      if (getComputedStyle(body).contentVisibility === "auto") withCv++;
      if (typeof body.checkVisibility === "function") {
        measurable++;
        const visible = body.checkVisibility({ contentVisibilityAuto: true });
        if (!visible) skipped++;
      }
    }
    return { cards: cards.length, bodies: bodies, withCv: withCv, skipped: skipped, measurable: measurable };
  });
}

// -------------------------------------------------------------------------------------------
// Finding 53 — the promoted TASKS are Spec-scoped (like the comments, Finding 41). Read the task
// titles the UI renders in the right panel's Tasks section (`[data-testid="task-row"]`).
// -------------------------------------------------------------------------------------------
async function reviewTaskTitles() {
  return browser.execute(function () {
    return Array.from(document.querySelectorAll('[data-testid="task-row"]')).map((el) =>
      (el.textContent || "").trim(),
    );
  });
}

// -------------------------------------------------------------------------------------------
// Finding 55 — PERF: a review action must re-render ONLY the file it touches (not all 77). The
// `DiffView` exposes `window.__nyxFileSectionRenders` = { total, byPath } — each `FileSection`
// commit bumps its path's counter (a `React.memo` bail-out runs no effect, so a NON-re-rendered file
// does not count). Read the total render count to measure the delta an action causes.
// -------------------------------------------------------------------------------------------
async function fileSectionRenderTotal() {
  return browser.execute(function () {
    const t = window.__nyxFileSectionRenders;
    return t && typeof t.total === "number" ? t.total : null;
  });
}

// The set of file PATHS that re-rendered since `total` was `prevTotal` is not tracked directly, so
// snapshot `byPath` and diff it. Returns the paths whose per-file render count grew.
async function fileSectionRendersByPath() {
  return browser.execute(function () {
    const t = window.__nyxFileSectionRenders;
    return t && t.byPath ? Object.assign({}, t.byPath) : {};
  });
}

// -------------------------------------------------------------------------------------------
// Finding 63 (rewritten for the NATIVE @pierre mechanism) — DRAG-select a RANGE of diff lines, then
// anchor a comment over the whole plage. This drives @pierre's OWN Diffhub selection: a NON-CONTROLLED
// press-drag on the line-NUMBER column (`[data-column-number]`, `requireNumberColumn:true`) paints the
// plage LIVE (the library schedules `renderSelection` in a rAF → `[data-selected-line]`) and KEEPS it
// after release; @pierre then auto-places its native gutter "+" (`[data-utility-button]`) on the
// selection's bottom line, and a press+release on it fires `onGutterUtilityClick({start,end})` →
// `diff-view.tsx` opens the composer over the WHOLE plage. Everything lives in the open shadow DOM, so
// we shadow-pierce. Returns the two dragged line numbers (diagnostic).
// -------------------------------------------------------------------------------------------
// Finding 63 — DRAG over `span+1` changed line-NUMBER cells (a real non-controlled selection). Returns
// the two dragged line numbers, or null if the diff had too few changed cells. The CALLER waits for
// the rAF-painted highlight (`selectedLineCount`). Shared by `addRangeComment` and the #63 gate test.
async function dragSelectLineNumbers(span) {
  return browser.execute(function (span) {
    function deep(selector) {
      const out = [];
      (function walk(node) {
        if (!node || !node.querySelectorAll) return;
        out.push.apply(out, node.querySelectorAll(selector));
        for (const el of node.querySelectorAll("*")) if (el.shadowRoot) walk(el.shadowRoot);
      })(document);
      return out;
    }
    const scroll = document.querySelector('[data-testid="diff-scroll"]');
    if (!scroll) return null;
    const cells = deep("[data-column-number]").filter((c) => /^\s*\d+\s*$/.test(c.textContent || ""));
    const changed = cells.filter((c) => (c.getAttribute("data-line-type") || "") !== "context");
    const pool = changed.length >= span + 1 ? changed : cells;
    if (pool.length < span + 1) return null;
    const a = pool[0];
    const b = pool[span];
    const ra = a.getBoundingClientRect();
    const rb = b.getBoundingClientRect();
    const pt = (r) => ({ x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2) });
    const pa = pt(ra);
    const pb = pt(rb);
    const opts = (x, y, extra) =>
      Object.assign(
        { bubbles: true, cancelable: true, composed: true, pointerId: 1, pointerType: "mouse", button: 0, buttons: 1, clientX: x, clientY: y, view: window },
        extra || {},
      );
    // Press on the FIRST number cell (event-path resolves the number column) then drag DOWN over the
    // rows (coordinates-first hit-testing) so the non-controlled selection paints across the plage.
    a.dispatchEvent(new PointerEvent("pointerdown", opts(pa.x, pa.y)));
    const midY = Math.round((pa.y + pb.y) / 2);
    document.dispatchEvent(new PointerEvent("pointermove", opts(pb.x, midY)));
    document.dispatchEvent(new PointerEvent("pointermove", opts(pb.x, pb.y)));
    document.dispatchEvent(new PointerEvent("pointerup", opts(pb.x, pb.y, { buttons: 0 })));
    return { aLine: (a.textContent || "").trim(), bLine: (b.textContent || "").trim() };
  }, span);
}

async function addRangeComment(body) {
  // 1) Drag over the line NUMBERS to paint a non-controlled selection (spanning a few changed rows).
  const dragged = await dragSelectLineNumbers(3);
  if (!dragged) throw new Error("could not find two changed line-number cells to drag a range over");
  // Let the rAF-scheduled `renderSelection` paint the plage, then confirm the FRANC highlight appeared
  // (this is the visible, persistent selection the finding requires — not the old ~15% invisible tint).
  await browser.waitUntil(async () => (await selectedLineCount()) > 0, {
    timeout: 8000,
    timeoutMsg: "the drag over the line numbers never painted a selection highlight (data-selected-line)",
  });

  // 2) Press @pierre's NATIVE gutter "+" (`[data-utility-button]`, auto-placed on the selection) and
  //    RELEASE (a real pointerdown → document pointerup, NOT a `.click()`): its
  //    `startGutterSelectionFromPointerDown` reuses the live selection endpoints and fires
  //    `onGutterUtilityClick({start,end})` at pointerup → `diff-view.tsx` opens the range composer.
  await browser.waitUntil(
    async () =>
      browser.execute(function () {
        function deep(selector) {
          const out = [];
          (function walk(node) {
            if (!node || !node.querySelectorAll) return;
            out.push.apply(out, node.querySelectorAll(selector));
            for (const el of node.querySelectorAll("*")) if (el.shadowRoot) walk(el.shadowRoot);
          })(document);
          return out;
        }
        const btn = deep("[data-utility-button]")[0];
        if (!btn) return false;
        const r = btn.getBoundingClientRect();
        const cx = Math.round(r.left + r.width / 2);
        const cy = Math.round(r.top + r.height / 2);
        const opts = (x, y, extra) =>
          Object.assign(
            { bubbles: true, cancelable: true, composed: true, pointerId: 2, pointerType: "mouse", button: 0, buttons: 1, clientX: x, clientY: y, view: window },
            extra || {},
          );
        btn.dispatchEvent(new PointerEvent("pointerdown", opts(cx, cy)));
        document.dispatchEvent(new PointerEvent("pointerup", opts(cx, cy, { buttons: 0 })));
        return true;
      }),
    { timeout: 8000, timeoutMsg: "the native gutter '+' ([data-utility-button]) never appeared on the selection" },
  );
  await browser.pause(200);

  // 3) Type + submit in the opened line composer (shadow-pierced).
  const typed = await browser.execute(function (body) {
    function deep(selector) {
      const out = [];
      (function walk(node) {
        if (!node || !node.querySelectorAll) return;
        out.push.apply(out, node.querySelectorAll(selector));
        for (const el of node.querySelectorAll("*")) if (el.shadowRoot) walk(el.shadowRoot);
      })(document);
      return out;
    }
    const ta = deep('[data-testid="line-composer"] textarea')[0];
    if (!ta) return false;
    const proto = Object.getPrototypeOf(ta);
    const desc = Object.getOwnPropertyDescriptor(proto, "value");
    desc.set.call(ta, body);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    const scope = ta.closest('[data-testid="line-composer"]') || ta.parentElement;
    const btn = Array.from((scope || document).querySelectorAll("button")).find((b) =>
      /comment/i.test((b.textContent || "").trim()),
    );
    if (btn) btn.click();
    return true;
  }, body);
  if (!typed) throw new Error("the range composer textarea never opened after the drag + native '+'");
  await browser.pause(300);
  return dragged;
}

module.exports = {
  // git seed
  reviewRootDir,
  ANCHOR_FILE,
  ANCHOR_KEEP_LINE,
  ANCHOR_GONE_LINE,
  seedMultiRepoTree,
  advanceApiRepo,
  touchWorkTree,
  // seed seam + navigation
  waitForWorkSeam,
  seedProject,
  enterProject,
  openBoard,
  openSpecCard,
  detailTabLabels,
  clickDetailTab,
  // review tab → page flow (PDR-0028)
  waitForReviewTab,
  hasDiffViewer,
  clickOpenReview,
  clickReviewBack,
  // review DOM
  reviewText,
  hasElement,
  countElements,
  waitForReviewScreen,
  reviewRepoTabNames,
  waitForReviewRepoTabs,
  selectRepoTabByName,
  diffVirtualization,
  diffFilePaths,
  scrollDiff,
  selectDiffSurface,
  setRangeRevs,
  clearRangeRevs,
  selectVariety,
  reviewHeaderInfo,
  waitForDiffLoaded,
  addLineComment,
  addGlobalComment,
  typeIntoComposer,
  anchorMarkerCount,
  threadCount,
  clickThreadControl,
  waitForThreadWithBody,
  threadResolvedInUi,
  closeCommentsPanel,
  unreadBadgeCount,
  // findings 41/42/43/52/55 review-fix helpers
  reviewFileTreeInfo,
  lastTreeError,
  waitForAwaitingInput,
  diffHasRawRevError,
  reviewStickyHeaderInfo,
  openReviewForSpec,
  // v2.5 round-2 polish/feature helpers (findings 45/46/47/49/51/53)
  breadcrumbCrumbs,
  clickBreadcrumbCrumb,
  hasInternalPanelCloseArrow,
  railOpen,
  clickHeaderToggle,
  repoTablistOverflowsY,
  gutterAddButtonInfo,
  selectedLineCount,
  dragSelectLineNumbers,
  reviewTaskTitles,
  addRangeComment,
  // v2.5 round-4 review polish helpers (findings 64/65/66)
  selectionAccentColors,
  clickLineNumberOnce,
  diffContentVisibility,
  // v2.5 round-3 perf helpers (finding 55)
  fileSectionRenderTotal,
  fileSectionRendersByPath,
  // point 9 (static)
  assertNoReviewRoute,
  // mcp
  mcpPort,
  mcpCall,
  waitForMcp,
  mcpStructured,
  tool,
  workspaceGitRepos,
  agentReply,
  agentStructureTask,
  listComments,
  listTasks,
};
