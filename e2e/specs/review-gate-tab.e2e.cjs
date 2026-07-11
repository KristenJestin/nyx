/* eslint-disable */
// v2.5 REVIEW GATE — PILOTAGE RÉEL (CDP) of the LOCAL multi-repo diff/review in its VALIDATED
// placement (PDR-0028): the Spec detail's Review TAB shows the COMMENTS + an "Open review" button,
// and the full diff/tree VIEWER lives on the Spec-scoped page `/work/$specId/review` (reached ONLY
// by that button — NO top-level `/review` route, NO `window.__nyxReview` seam). Diff/tree render
// with `@pierre/diffs` + `@pierre/trees`; the flanks use the closable `SidebarShell`. Task
// 01KX6NVYT9WZDRE7HXHE8HXK9B (PRD 01KX0EEP5H1MJW4T3QCZEWPHFK, PDR-0012/0015/0028).
//
// It drives the REAL Electron app through: the `window.__nyxWork` SEED seam (project + root
// workspace over a real multi-repo git tree) + the REAL MCP loopback (Spec authoring/anchoring +
// the agent's review tools + repo-id enumeration) + REAL navigation (rail → Work board → Spec card
// → Review TAB → "Open review" → the full-screen page) + the rendered review DOM. It asserts the
// full done-criterion end-to-end:
//   0. the Review TAB shows comments + the "Open review" button and NO diff viewer; the button opens
//      `/work/$specId/review`;
//   1. the review page aggregates ONLY the Spec-workspace's repos as tabs, WITHOUT mixing identities;
//   2. annotate GLOBAL; 3. annotate LINE; 4. annotate RANGE — the anchors show on the diff;
//   5. promote a comment into a task via the GRILL (human promote → agent structure via MCP → human
//      validate); 6. after a real commit, one anchor RE-MAPS (follows the code), another OUTDATES;
//   7. an AGENT reply via MCP appears in the thread; 8. the unread counter + views refresh in
//      REAL TIME on a repo change; 9. NO top-level `/review` route / nav item (static + DOM); the
//      right comments panel is CLOSABLE; the file tree is @pierre/trees. Plus: a Spec with NO
//      workspace (workspace_context null) shows a clean empty state in the tab; resolution is human-
//      only (backend guarantee); pure-local (no VS Code / no forge).
//
// ANTI-FALSE-GREEN: the before-hook FAILS HARD if the seam / MCP never appears, and every assertion
// has a finite timeout that THROWS on miss — a build that does not mount, a missing seam/tool, or an
// unreachable MCP FAILS the gate rather than passing silently.

const assert = require("assert");
const h = require("./review-tab-helpers.cjs");

describe("v2.5 review-in-tab gate — local multi-repo diff/review, pilotage réel (CDP), no /review route", function () {
  let seed;
  let projectName;
  let projectId;
  let workspaceId;
  let specId; // the ANCHORED Spec (has a dev workspace → repos)
  let scopeSpecId; // a SECOND Spec on the SAME workspace/repos → comment-scoping check (Finding 41)
  let emptySpecId; // a Spec with NO workspace → empty-state review
  let apiRepositoryId;
  let webRepositoryId;

  // Marker bodies so the spec can fish its own comments out of the feed deterministically.
  const GLOBAL_BODY = "REVIEW_GLOBAL_" + Date.now();
  const RESOLVE_BODY = "REVIEW_RESOLVE_" + Date.now();
  const PROMOTE_BODY = "REVIEW_PROMOTE_" + Date.now();
  const LINE_BODY = "REVIEW_LINE_" + Date.now();
  const RANGE_BODY = "REVIEW_RANGE_" + Date.now();
  // Finding 51/63 — a RANGE comment anchored by a real UI DRAG selection (not an MCP post).
  const RANGE_DRAG_BODY = "REVIEW_RANGE_DRAG_" + Date.now();
  // Finding 65a — a RANGE comment whose SUBMIT must then CLEAR the frozen plage + "+".
  const RANGE_CLEAR_BODY = "REVIEW_RANGE_CLEAR_" + Date.now();
  // Finding 62 — a GLOBAL comment resolved to PROVE a non-code mutation does not reflow the diff.
  const NOREFLOW_BODY = "REVIEW_NOREFLOW_" + Date.now();
  const KEEP_BODY = "REVIEW_KEEP_ANCHOR_" + Date.now();
  const GONE_BODY = "REVIEW_GONE_ANCHOR_" + Date.now();
  const AGENT_REPLY = "REVIEW_AGENT_REPLY_" + Date.now();
  // Finding 41 — two markers scoped to DIFFERENT Specs sharing the same (workspace, repos): each
  // must appear ONLY on its own Spec's review, never on the other's.
  const SCOPE_A_BODY = "REVIEW_SCOPE_A_" + Date.now();
  const SCOPE_B_BODY = "REVIEW_SCOPE_B_" + Date.now();

  before(async function () {
    // Anti-false-green gates: the shell seed seam AND the MCP loopback must both come up.
    await h.waitForWorkSeam(60000);
    await h.waitForMcp(30000);
    // Seed the GROS multi-repo change on disk (two real local git repos under one root).
    seed = h.seedMultiRepoTree();
    projectName = "review-tab-gate";
  });

  // ── SEED: project + anchored Spec (repos), plus a second unanchored Spec (empty-state) ──────
  it("seeds a project + a Spec anchored on the multi-repo workspace (repos derived), all via seam+MCP", async function () {
    // Project + its ROOT workspace: `create_project` auto-detects the two repos under the root.
    const created = await h.seedProject(projectName, seed.root);
    assert(created && created.project && created.root, "seedProject returns project + root");
    projectId = created.project.id;
    workspaceId = created.root.id;
    assert(projectId && workspaceId, "project + primary(root) workspace ids present");

    // The ANCHORED Spec — authored purely through the real MCP tools (like an agent).
    const createOut = await h.tool("spec_create", {
      project_id: projectId,
      title: "Review the widget change",
      problem: "The widget change needs local review.",
      solution: "Review its multi-repo diff in the Spec's Review tab.",
    });
    assert(createOut && createOut.spec && createOut.spec.id, "spec_create returned a spec");
    specId = createOut.spec.id;
    await h.tool("spec_update_section", {
      spec_id: specId,
      section: "implementation_decisions",
      value: "Diff engine gix; anchors re-map on living HEAD.",
    });
    // A task so the pre-ready validations pass (mirrors the proven spec-workflow flow).
    await h.tool("task_add", { project_id: projectId, spec_id: specId, title: "Land the widget diff", kind: "slice", phase: 1 });
    // draft → ready (sensitive: user_confirmed) → in_progress, so its board card is interactable.
    const ready = await h.tool("spec_transition", { spec_id: specId, transition: "ready", user_confirmed: true });
    assert.strictEqual(ready.spec.status, "ready", "the anchored Spec reached ready");
    await h.tool("spec_transition", { spec_id: specId, transition: "start" });
    // ANCHOR on the workspace: the repos + project are DERIVED through `specs.workspace_id`.
    const anchored = await h.tool("spec_set_workspace", { spec_id: specId, workspace_id: workspaceId });
    assert.strictEqual(anchored.spec.workspace_id, workspaceId, "the Spec is anchored on the dev workspace");

    // A SECOND Spec anchored on the SAME dev workspace (same repos) — the control for the
    // comment-scoping check (Finding 41): a comment on `specId` must NOT surface on this one.
    const scopeOut = await h.tool("spec_create", {
      project_id: projectId,
      title: "Second spec on the same workspace",
      problem: "Shares the multi-repo workspace with the first spec.",
      solution: "Its review must show only ITS OWN comments (no cross-spec leak).",
    });
    scopeSpecId = scopeOut.spec.id;
    await h.tool("spec_update_section", {
      spec_id: scopeSpecId,
      section: "implementation_decisions",
      value: "Shares repos with the first spec; comments must stay spec-scoped.",
    });
    await h.tool("task_add", { project_id: projectId, spec_id: scopeSpecId, title: "Second slice", kind: "slice", phase: 1 });
    await h.tool("spec_transition", { spec_id: scopeSpecId, transition: "ready", user_confirmed: true });
    await h.tool("spec_transition", { spec_id: scopeSpecId, transition: "start" });
    const scopeAnchored = await h.tool("spec_set_workspace", { spec_id: scopeSpecId, workspace_id: workspaceId });
    assert.strictEqual(scopeAnchored.spec.workspace_id, workspaceId, "the second Spec shares the dev workspace");

    // A SECOND Spec with NO workspace anchor → its Review tab must show the clean empty state.
    const emptyOut = await h.tool("spec_create", {
      project_id: projectId,
      title: "Unanchored spec (no workspace)",
      problem: "A Spec with no dev workspace yet.",
      solution: "Its Review tab shows a clean empty state.",
    });
    emptySpecId = emptyOut.spec.id;
    await h.tool("task_add", { project_id: projectId, spec_id: emptySpecId, title: "placeholder", kind: "slice", phase: 1 });
    await h.tool("spec_transition", { spec_id: emptySpecId, transition: "ready", user_confirmed: true });
    await h.tool("spec_transition", { spec_id: emptySpecId, transition: "start" });

    // Enumerate the workspace's repository ids (the seam's old job) via the MCP `workspace_git`.
    const repos = await h.workspaceGitRepos(workspaceId);
    const byName = Object.fromEntries(repos.map((r) => [r.name, r]));
    assert(byName.api && byName.web, "workspace_git enumerates BOTH `api` and `web`");
    apiRepositoryId = byName.api.repository_id;
    webRepositoryId = byName.web.repository_id;
    assert.notStrictEqual(apiRepositoryId, webRepositoryId, "distinct repository identities (no mixing, PDR-0015)");
  });

  // ── 0: the Review TAB shows comments + an "Open review" button and NO diff viewer ──────────────
  it("shows the Review tab as comments + an Open review button (no diff viewer in the tab)", async function () {
    await h.enterProject(projectName);
    await h.openBoard();
    await h.openSpecCard(specId, 20000);
    await browser.waitUntil(
      async () => browser.execute((t) => (document.body.innerText || "").indexOf(t) !== -1, "Review the widget change"),
      { timeout: 15000, timeoutMsg: "the PRD detail did not render the Spec title" },
    );

    // The Review tab is present in the detail; open it — it must show the COMMENTS surface + the
    // "Open review" button, and MUST NOT embed the diff viewer (that lives on the dedicated page).
    const labels = await h.detailTabLabels();
    assert(labels.some((l) => l.indexOf("Review") !== -1), "the Review tab is present (got " + JSON.stringify(labels) + ")");
    await h.clickDetailTab("Review");
    await h.waitForReviewTab(20000);
    assert(!(await h.hasDiffViewer()), "the Review TAB embeds NO diff viewer (comments-only, PDR-0028)");
    assert(!(await h.hasElement('[role="tablist"][aria-label="Repositories"]')), "no repo tablist (viewer chrome) in the tab");
  });

  // ── 1: "Open review" → the full-screen page aggregates the workspace's repos as tabs ───────────
  it("opens the full-screen review page and aggregates ONLY the Spec-workspace's repos, WITHOUT mixing identities", async function () {
    // Click the tab's "Open review" → navigate to `/work/$specId/review`.
    await h.clickOpenReview();
    await h.waitForReviewScreen(30000);
    assert(await h.hasElement('[data-testid="review-page"]'), "the full-screen review page mounted");

    // The review aggregates BOTH workspace repos as tabs (each its own identity).
    const names = await h.waitForReviewRepoTabs(2, 30000);
    assert(names.includes("api") && names.includes("web"), "both `api` and `web` repos are aggregated as tabs (got " + JSON.stringify(names) + ")");
    const tabCount = await h.countElements('[role="tablist"][aria-label="Repositories"] [role="tab"]');
    assert(tabCount >= 2, "the header renders >=2 repo tabs (got " + tabCount + ")");
    // Workspace-scoping (Finding 3): the tabs match EXACTLY the workspace's enumerated repos, no more.
    assert.strictEqual(names.filter((n) => n === "api" || n === "web").length, tabCount, "no foreign-workspace repos leak into the tabs");
    // The @pierre/trees file tree is present on the page's left flank.
    assert(await h.hasElement('[data-testid="review-file-tree"]'), "the @pierre/trees changed-files tree mounts on the page");
  });

  // ── 3: parcourir the big diff on the LIVING branches review (base…feature), @pierre/diffs ──────
  it("renders the big diff (@pierre/diffs) on the LIVING branches review and follows the scroll", async function () {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    const info = await h.reviewHeaderInfo();
    assert(info.hasLiving, "the header shows the LIVING variety");
    assert(info.hasGix || info.hasGitCli, "a real diff-engine backend produced the diff (gix/git_cli)");
    // Finding 48 — the engine is a DIAGNOSTIC attribute now, NOT user-facing jargon: the sub-header
    // must expose the backend attribute but show NO visible "gix" text (and no raw "mode" word).
    assert(info.hasBackendAttr, "the diff backend survives as a diagnostic data-attribute (Finding 48)");
    assert(!info.subHeaderShowsGix, "the sub-header shows NO user-facing 'gix' jargon (Finding 48)");

    // With @pierre/diffs the library owns line-level virtualization inside its shadow DOM; the
    // per-file collapsible sections are LIGHT DOM. Assert the big multi-file diff rendered its
    // sections (the 14 changed files of `api`: the big scroll-anchor file + anchor.rs + 12 modules)
    // and that the scroll surface is tall + scrollable.
    const v0 = await h.diffVirtualization();
    assert(v0, "the diff scroll surface is present");
    assert(v0.fileSections >= 10, "the big diff rendered its many file sections (got " + v0.fileSections + ")");
    assert(v0.virtualHeight > v0.viewportHeight, "the diff scroll surface is taller than the viewport: " + JSON.stringify(v0));

    // Finding 42 — the LEFT changed-files tree must show the N files (NOT an empty tree), coherent
    // with the diff's file count. The @pierre/trees model is keyed on the diff identity now, so it
    // rebuilds when the diff resolves instead of staying stuck at its initial empty `paths`.
    try {
      await browser.waitUntil(
        async () => {
          const info = await h.reviewFileTreeInfo();
          return info && info.count === v0.fileSections && info.count > 0 && !info.empty;
        },
        { timeout: 15000, timeoutMsg: "the changed-files tree stayed empty / out of sync with the diff" },
      );
    } catch (err) {
      // The tree blanked behind its error boundary (Finding 55). wdio does not pipe the renderer
      // console, so surface the REAL caught stack (message + component stack) that error-boundary.tsx
      // mirrored onto `window.__nyxLastTreeError` — the diagnostic the CDP gate could not show before.
      // eslint-disable-next-line no-console
      console.log("FILE_TREE_CAUGHT_ERROR=" + JSON.stringify(await h.lastTreeError(), null, 2));
      throw err;
    }
    const treeInfo = await h.reviewFileTreeInfo();
    assert(!treeInfo.empty, "the changed-files tree is NOT empty when the diff has files (Finding 42)");
    assert.strictEqual(treeInfo.count, v0.fileSections, "the tree count matches the diff's file count");
    assert(treeInfo.nodes > 0, "the @pierre/trees tree actually rendered file rows (not blank)");

    const top0 = await browser.execute(() => {
      const s = document.querySelector('[data-testid="diff-scroll"]');
      return s ? s.scrollTop : -1;
    });
    await h.scrollDiff(4000);
    const top1 = await browser.execute(() => {
      const s = document.querySelector('[data-testid="diff-scroll"]');
      return s ? s.scrollTop : -1;
    });
    assert(top1 > top0, "the diff scroll surface scrolled (top moved from " + top0 + " to " + top1 + ")");

    // Parcourir the working-tree surface — `api`'s work tree is dirty (the uncommitted file shows in
    // the light-DOM file-section header path).
    await h.selectDiffSurface("working_tree");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);
    await browser.waitUntil(
      async () => (await h.diffFilePaths()).some((p) => p.indexOf("worktree_change.rs") !== -1),
      { timeout: 15000, timeoutMsg: "the working-tree diff never listed the uncommitted file" },
    );
  });

  // ── v2.5 round-2/3 POLISH: repo-tablist scroll (45) · LEFT files rail always open, no toggle (59)
  //    · RIGHT comments rail still closable · gutter "+" z-index (49). Light DOM/CSS asserts; the
  //    fully-visual feel is confirmed by the CDP gate. ──
  it("polishes the review chrome: no 2D tab scroll (45), files rail always open + no toggle (59), comments rail closable, gutter '+' above the numbers (49)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.waitForDiffLoaded(20000);

    // Finding 45 — the repo tablist must NOT raise a parasitic VERTICAL scrollbar (the 2D look).
    assert(
      (await h.repoTablistOverflowsY()) === false,
      "the repo tablist does not overflow vertically (no parasitic scrollbar, Finding 45)",
    );

    // Finding 46 — the RIGHT comments rail has NO redundant internal "Close panel" arrow; the two
    // header buttons are the SINGLE control for each flank, and BOTH flanks toggle symmetrically.
    assert(
      !(await h.hasInternalPanelCloseArrow()),
      "no internal 'Close panel' arrow survives on either rail (Finding 46 — header buttons are the single control)",
    );
    assert(await h.railOpen("Changed files"), "the left files rail starts open");
    assert(await h.railOpen("Comments"), "the right comments rail starts open");

    // Finding 59 — the LEFT files rail is ALWAYS open: its collapse toggle is REMOVED. Neither the
    // "Hide files panel" nor the "Show files panel" header button exists anymore, and the rail stays
    // open (there is no way to close it).
    assert(
      !(await h.hasElement('button[aria-label="Hide files panel"]')) &&
        !(await h.hasElement('button[aria-label="Show files panel"]')),
      "no LEFT files-panel toggle button survives (Finding 59 — files rail is always open)",
    );
    assert(await h.railOpen("Changed files"), "the left files rail stays open (no way to close it)");

    // The RIGHT comments rail is STILL closable + reopens from its header button (Finding 59 keeps it).
    await h.clickHeaderToggle("Hide comments panel");
    await browser.waitUntil(async () => (await h.railOpen("Comments")) === false, {
      timeout: 6000,
      timeoutMsg: "the right comments rail did not close from its header button",
    });
    await h.clickHeaderToggle("Show comments panel");
    await browser.waitUntil(async () => (await h.railOpen("Comments")) === true, {
      timeout: 6000,
      timeoutMsg: "the right comments rail did not reopen from its header button",
    });

    // Finding 49 — the gutter "+" is a POSITIONED (`relative z-10`) light-DOM button that stacks
    // ABOVE the line-number column. Assert its stacking styles when present (the button is slotted
    // into @pierre's gutter shadow DOM; the pixel-level overlap is confirmed visually by the CDP gate).
    const g = await h.gutterAddButtonInfo();
    if (g) {
      assert(g.position !== "static", "the gutter '+' is positioned so its z-index applies (Finding 49)");
      assert(
        /\bz-10\b/.test(g.className) || (g.zIndex !== "auto" && Number(g.zIndex) >= 1),
        "the gutter '+' carries a stacking z-index above the line numbers (Finding 49): " + JSON.stringify(g),
      );
    }
  });

  // ── Finding 55: PERF — a review action must re-render ONLY the file it touches (not all 77). The
  //    DiffView exposes `window.__nyxFileSectionRenders` = { total, byPath }: each FileSection commit
  //    bumps its path's counter, and a React.memo bail-out runs no effect, so a NON-re-rendered file
  //    does not count. Toggle ONE file and assert ONLY that file re-rendered. ─────────────────────
  it("re-renders ONLY the touched file on an action, not the whole big diff (Finding 55 — perf)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    const v = await h.diffVirtualization();
    assert(v && v.fileSections >= 10, "the big diff rendered many file sections (got " + (v && v.fileSections) + ")");

    // Let the initial mounts settle, then snapshot the per-file render tally.
    await browser.pause(700);
    const before = await h.fileSectionRendersByPath();
    const totalBefore = await h.fileSectionRenderTotal();
    assert(
      totalBefore != null && Object.keys(before).length >= 10,
      "the render tally is exposed and populated (window.__nyxFileSectionRenders): " + JSON.stringify({ totalBefore, files: Object.keys(before).length }),
    );

    // Toggle ONE file section (click its sticky header) — a pure single-file action: only THAT file's
    // `collapsed` prop changes, so only THAT FileSection re-renders (the other 12+ are React.memo'd).
    const toggledPath = await browser.execute(function () {
      const header = document.querySelector('[data-testid="diff-file"] [data-row="file-header"]');
      if (!header) return null;
      const file = header.closest('[data-testid="diff-file"]');
      header.click();
      return file ? file.getAttribute("data-path") : null;
    });
    assert(toggledPath, "clicked a file section header to toggle it");
    await browser.pause(500);

    const after = await h.fileSectionRendersByPath();
    const totalAfter = await h.fileSectionRenderTotal();
    const grown = Object.keys(after).filter((p) => (after[p] || 0) > (before[p] || 0));
    const delta = totalAfter - totalBefore;

    assert(
      grown.length >= 1 && grown.length <= 2,
      "the action re-rendered ONLY the touched file (files re-rendered: " + JSON.stringify(grown) + ")",
    );
    assert(grown.indexOf(toggledPath) !== -1, "the re-rendered file is the one toggled (" + toggledPath + ")");
    assert(
      delta < v.fileSections,
      "the action did NOT re-render all files (delta " + delta + " of " + v.fileSections + " sections — was a global re-render before)",
    );
  });

  // ── Finding 62 (TanStack Query): a mutation that does NOT touch the code must NOT reflow the diff.
  //    BEFORE: every mutation broadcast `repos://changed` → `use-repo-diff` re-pulled → a NEW `RepoDiff`
  //    identity → `patchByFile` rebuilt → all `FileSection`s re-rendered + @pierre reconstructed the
  //    diff DOM (the "depop/pop" flash). AFTER: React Query STRUCTURAL SHARING keeps the SAME `diff`
  //    reference when the re-pulled content is identical → `patchByFile` stable → the diff does NOT
  //    globally re-render. Assert via `window.__nyxFileSectionRenders`: a resolve delta stays FAR below
  //    a full reflow, and the diff surface is not remounted. Also: the resolve is OPTIMISTIC (instant). ─
  it("does NOT reflow the diff on a non-code mutation (resolve), and the resolve is optimistic (Finding 62)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    const v = await h.diffVirtualization();
    assert(v && v.fileSections >= 10, "the big diff rendered many file sections (got " + (v && v.fileSections) + ")");

    // A fresh GLOBAL comment (no anchor → touches NEITHER the code NOR the anchored threads on the
    // diff), scoped to the Spec so it surfaces in the panel. Posted via the agent MCP façade.
    await h.mcpCall("tools/call", {
      name: "review_comment_post",
      arguments: { workspace_id: workspaceId, repository_id: apiRepositoryId, spec_id: specId, body: NOREFLOW_BODY },
    });
    await h.waitForThreadWithBody(NOREFLOW_BODY, 20000);

    // Let the post's re-pull settle, THEN snapshot the render tally — so we measure ONLY the resolve.
    await browser.pause(800);
    const totalBefore = await h.fileSectionRenderTotal();
    const before = await h.fileSectionRendersByPath();
    assert(totalBefore != null, "the render tally is exposed (window.__nyxFileSectionRenders)");

    // Resolve the GLOBAL comment through the UI (human-only), timing the UI flip.
    const t0 = Date.now();
    assert(await h.clickThreadControl(NOREFLOW_BODY, "resolve-comment"), "clicked Resolve on the global comment");

    // OPTIMISTIC — the thread flips to resolved FAST (the old lag was 2-3 s of round-trip + re-pull;
    // the optimistic `setQueryData` shows it near-instantly). The resolved card drops its Resolve
    // control (it offers Reopen instead).
    await browser.waitUntil(async () => h.threadResolvedInUi(NOREFLOW_BODY), {
      timeout: 2500,
      timeoutMsg: "the resolve did not reflect in the UI quickly (optimistic update missing?)",
    });
    const flipMs = Date.now() - t0;
    assert(flipMs < 2500, "the resolve reflected optimistically (" + flipMs + "ms, not a 2-3s round-trip)");

    // The backend confirms the resolve round-tripped (human resolution) — the optimism reconciled.
    await browser.waitUntil(
      async () => {
        const c = (await h.listComments(workspaceId, apiRepositoryId)).find((x) => x.body === NOREFLOW_BODY);
        return c && c.status === "resolved" && c.resolved_by_type === "human";
      },
      { timeout: 15000, timeoutMsg: "the resolve never reconciled on the backend" },
    );

    // NO GLOBAL REFLOW — let any invalidation re-pull settle, then measure the FileSection re-renders
    // the resolve caused. Structural sharing keeps `diff` stable, so the diff did NOT globally reflow.
    await browser.pause(1200);
    const totalAfter = await h.fileSectionRenderTotal();
    const after = await h.fileSectionRendersByPath();
    const delta = totalAfter - totalBefore;
    const grown = Object.keys(after).filter((p) => (after[p] || 0) > (before[p] || 0));

    assert(
      delta < v.fileSections,
      "resolving a global comment did NOT globally reflow the diff (delta " + delta + " of " + v.fileSections + " — a full reflow was the pre-#62 bug)",
    );
    assert(
      grown.length < v.fileSections,
      "not every file re-rendered on the resolve (re-rendered: " + grown.length + " of " + v.fileSections + ")",
    );
    // The diff surface was NOT remounted (its file-section count is unchanged — no depop/pop).
    const v2 = await h.diffVirtualization();
    assert(v2 && v2.fileSections === v.fileSections, "the diff was not remounted (file count stable: " + (v2 && v2.fileSections) + ")");
  });

  // ── Finding 63 (was 51): multi-line selection RETURNS TO THE @pierre NATIVE mechanism. A press-drag
  //    on the line NUMBERS paints the plage in REAL TIME and VISIBLY (non-controlled → `renderSelection`
  //    in a rAF → `[data-selected-line]`, franc via `unsafeCSS`), and PERSISTS after release; then the
  //    NATIVE gutter "+" (`onGutterUtilityClick`) opens a range composer over the whole plage. ────────
  it("paints a VISIBLE, persistent multi-line selection natively on a drag over the numbers (Finding 63)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    // A raw drag over a few changed line-NUMBER cells — no comment yet. The library must PAINT the
    // plage (real-time) and KEEP it after the pointer is released (the "sélection qu'on voit").
    const dragged = await h.dragSelectLineNumbers(3);
    assert(dragged, "found changed line-number cells to drag a range over");
    let painted = 0;
    await browser.waitUntil(
      async () => {
        painted = await h.selectedLineCount();
        return painted > 0;
      },
      { timeout: 10000, timeoutMsg: "the drag over the numbers never painted a visible selection (data-selected-line)" },
    );
    assert(painted > 0, "the native drag PAINTED a franc, visible selection highlight (Finding 63)");
    // It PERSISTS after release (non-controlled) — still painted a beat later, no round-trip needed.
    await browser.pause(300);
    assert((await h.selectedLineCount()) > 0, "the selection PERSISTS after the drag is released (Finding 63)");
  });

  // ── Finding 63: anchor a comment over the drag-selected plage via the NATIVE gutter "+"; the anchor
  //    stores a REAL multi-line range (line_end STRICTLY > line_start). ──────────────────────────────
  it("anchors a RANGE comment from the native drag + gutter '+' (line_end > line_start, Finding 63)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    // Drag the plage (asserts the highlight internally), press the native "+", anchor a comment.
    await h.addRangeComment(RANGE_DRAG_BODY);

    // The persisted comment carries a REAL multi-line anchor (line_end STRICTLY > line_start), proving
    // the native range propagated through onGutterUtilityClick → onAddAnchored → postAnchored.
    let range;
    await browser.waitUntil(
      async () => {
        range = (await h.listComments(workspaceId, apiRepositoryId)).find((c) => c.body === RANGE_DRAG_BODY);
        return range && range.line_start != null && range.line_end != null && range.line_end > range.line_start;
      },
      {
        timeout: 20000,
        timeoutMsg: "the drag-selected RANGE comment never persisted with line_end > line_start: " + JSON.stringify(range || null),
      },
    );
    assert(range.line_end > range.line_start, "the anchor stores the real lineStart..lineEnd (Finding 63)");
  });

  // ── Finding 63: a plain CLICK on a line number no longer opens a parasitic single-line composer
  //    (`onLineNumberClick` was removed) — it just selects that line; the "+" opens the composer. ─────
  it("does NOT open a parasitic single-line composer on a plain line-number click (Finding 63)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    // Click a single changed line-number cell (a plain click, no drag).
    await browser.execute(function () {
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
      const cell = (changed[0] || cells[0]);
      if (cell) cell.click();
    });
    await browser.pause(400);
    // No inline composer opened from the bare click (the parasite is gone).
    const composers = await browser.execute(function () {
      let n = 0;
      (function walk(node) {
        if (!node || !node.querySelectorAll) return;
        n += node.querySelectorAll('[data-testid="line-composer"]').length;
        for (const el of node.querySelectorAll("*")) if (el.shadowRoot) walk(el.shadowRoot);
      })(document);
      return n;
    });
    assert.strictEqual(composers, 0, "a bare line-number click opens NO single-line composer (Finding 63)");
  });

  // ── Finding 65b: a plain line-number CLICK no longer leaves a stuck 1-line selection — @pierre
  //    paints it on the click, and our `onLineSelected` clear empties it on release (before the fix a
  //    click left a bande + "+" bloquée indéfiniment). ──────────────────────────────────────────────
  it("clears a stuck selection on a plain line-number click (no persistent highlight, Finding 65b)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    // A plain click (pointerdown + immediate pointerup, no drag) paints a 1-line selection that
    // @pierre would keep forever; the fix clears it on release.
    assert(await h.clickLineNumberOnce(), "clicked a changed line-number cell");
    await browser.waitUntil(async () => (await h.selectedLineCount()) === 0, {
      timeout: 8000,
      timeoutMsg: "a plain line-number click left a persistent selection (Finding 65b not cleared)",
    });
    assert.strictEqual(
      await h.selectedLineCount(),
      0,
      "no selection/'+' persists after a plain line-number click (Finding 65b)",
    );
  });

  // ── Finding 65a: after SUBMITTING the range composer, the frozen plage + the "+" are CLEARED (they
  //    used to stay stuck). Cancel shares the same `closeComposer` → `requestClear` path. ────────────
  it("clears the selection + '+' after the range composer is submitted (Finding 65a)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    // The proven #63 flow: drag a plage → native "+" → type → submit (the anchor persists as a comment).
    await h.addRangeComment(RANGE_CLEAR_BODY);
    // After submit the painted plage + "+" must be GONE (the null pulse cleared @pierre's selection).
    await browser.waitUntil(async () => (await h.selectedLineCount()) === 0, {
      timeout: 8000,
      timeoutMsg: "the selection stayed painted after submitting the range composer (Finding 65a)",
    });
    assert.strictEqual(
      await h.selectedLineCount(),
      0,
      "no selection persists after submitting the range composer (Finding 65a)",
    );
    // The RANGE anchor still persisted (the clear did not swallow the submit).
    await browser.waitUntil(
      async () => (await h.listComments(workspaceId, apiRepositoryId)).some((c) => c.body === RANGE_CLEAR_BODY),
      { timeout: 15000, timeoutMsg: "the submitted range comment never persisted (clear must not swallow the submit)" },
    );
  });

  // ── Finding 64: the selection highlight + the native gutter "+" are painted with our ROSE `--primary`,
  //    NOT @pierre's default BLUE accent (`--diffs-modified-base`). ──────────────────────────────────
  it("paints the selection + gutter '+' in the ROSE primary, not @pierre's blue accent (Finding 64)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    // Drag a plage so @pierre paints the selection AND auto-places the native "+" on it to colour-check.
    assert(await h.dragSelectLineNumbers(3), "found changed line-number cells to drag a range over");
    await browser.waitUntil(async () => (await h.selectedLineCount()) > 0, {
      timeout: 10000,
      timeoutMsg: "the drag never painted a selection to colour-check",
    });
    let colors;
    await browser.waitUntil(
      async () => {
        colors = await h.selectionAccentColors();
        return colors && colors.hasButton && colors.utilityBg && colors.primary;
      },
      { timeout: 8000, timeoutMsg: "the gutter '+' never appeared for the colour check" },
    );
    // The "+" is `background-color: var(--primary)` (unsafeCSS) → it serializes IDENTICALLY to a fresh
    // probe of var(--primary). Before the fix it was @pierre's blue, so this equality would FAIL.
    assert.strictEqual(
      colors.utilityBg,
      colors.primary,
      "the gutter '+' is painted with var(--primary) (rose), not the @pierre blue accent: " + JSON.stringify(colors),
    );
  });

  // ── Finding 66: PERF/scale — the diff layout-virtualizes ACROSS files with the browser's NATIVE
  //    content-visibility, so OFF-SCREEN file bodies are NOT laid out (their huge line DOM is skipped).
  //    This shrinks the reflow the comments-sidebar toggle triggers and scales to many more files.
  //    (Full @pierre `<Virtualizer>` was deferred — see the task done_criterion note.) ────────────────
  it("layout-virtualizes the diff across files (content-visibility), not all files laid out at once (Finding 66)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    // The diff must be tall enough that files sit far off-screen (same precondition as the sticky test).
    // @pierre grows the file BODIES asynchronously (its Shiki worker tokenises off the main thread), so
    // give the big scroll-anchor file time to paint before deciding it is not scrollable.
    await browser.waitUntil(
      async () => {
        const v = await h.diffVirtualization();
        return v && v.fileSections >= 10 && v.virtualHeight > v.viewportHeight + 200;
      },
      { timeout: 30000, timeoutMsg: "the diff never grew tall enough to virtualize across files" },
    );
    // Scroll to the top so the LAST files are the ones off-screen.
    await browser.execute(() => {
      const s = document.querySelector('[data-testid="diff-scroll"]');
      if (s) s.scrollTop = 0;
    });
    await browser.pause(300);

    const cv = await h.diffContentVisibility();
    assert(cv && cv.bodies >= 10, "the big diff has many file bodies (got " + JSON.stringify(cv) + ")");
    assert.strictEqual(
      cv.withCv,
      cv.bodies,
      "EVERY file body carries content-visibility:auto (Finding 66): " + JSON.stringify(cv),
    );

    // If the runtime exposes checkVisibility({contentVisibilityAuto}), PROVE the off-screen bodies are
    // actually layout-SKIPPED (the DOM-bounded assertion: not every file is laid out at once).
    if (cv.measurable > 0) {
      await browser.waitUntil(
        async () => {
          const s = await h.diffContentVisibility();
          return s && s.skipped >= 1;
        },
        { timeout: 8000, timeoutMsg: "no off-screen file body was content-visibility-skipped (Finding 66)" },
      );
      const s2 = await h.diffContentVisibility();
      assert(
        s2.skipped >= 1 && s2.skipped < s2.bodies,
        "some (not all) file bodies are layout-skipped off-screen (Finding 66): " + JSON.stringify(s2),
      );
    }
  });

  // ── Finding 43: Commit/Range/Branches show a "fill the rev-specs" PROMPT, not a raw throw ──────
  it("switching to Commit / Range / Branches shows a rev-spec prompt, not a raw error (Finding 43)", async () => {
    await h.selectRepoTabByName("api");

    // COMMIT with no rev: the guard shows the prompt and NEVER fires the throwing `repo_diff`.
    await h.selectDiffSurface("commit");
    await h.waitForAwaitingInput(8000);
    assert(!(await h.diffHasRawRevError()), "Commit surface shows a prompt, not a raw rev-spec error");

    // RANGE with no base/target. The per-repo target survives across tests (review-page keeps a
    // `targets` map), and the earlier big-diff test typed base/target on THIS repo (`api`), so
    // clear the fields first to restore the "no rev-spec typed yet" precondition this asserts.
    await h.selectDiffSurface("range");
    await h.clearRangeRevs();
    await h.waitForAwaitingInput(8000);
    assert(!(await h.diffHasRawRevError()), "Range surface shows a prompt, not a raw rev-spec error");

    // BRANCHES with no base/target (same per-repo rev-spec fields as range — clear them too).
    await h.selectDiffSurface("branches");
    await h.clearRangeRevs();
    await h.waitForAwaitingInput(8000);
    assert(!(await h.diffHasRawRevError()), "Branches surface shows a prompt, not a raw rev-spec error");

    // Filling the rev-specs lifts the guard and loads the diff — still no raw error.
    await h.setRangeRevs("base", "feature");
    await h.waitForDiffLoaded(20000);
    assert(!(await h.diffHasRawRevError()), "the branches diff loaded once the rev-specs were entered");
  });

  // ── Finding 52: the current file header stays PINNED to the top of the diff, VISIBLE on scroll ──
  it("keeps the current file header pinned to the top of the diff and visible while scrolling (Finding 52)", async () => {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    // @pierre grows the file BODIES asynchronously (its Shiki worker tokenises off the main thread),
    // so the diff is short for a beat AFTER the sections mount — scrolling then leaves scrollTop clamped
    // at 0. Wait until the diff is actually tall enough to scroll before we scroll into it.
    await browser.waitUntil(
      async () => {
        const v = await h.diffVirtualization();
        return v && v.virtualHeight > v.viewportHeight + 200;
      },
      { timeout: 30000, timeoutMsg: "the diff never grew tall enough to scroll" },
    );

    // Scroll into the tall diff so a file's header is pinned while its body is still in view. If the
    // scroll happens to rest in the thin gap between two cards, nudge it a little into a file body.
    await h.scrollDiff(1200);
    await browser.waitUntil(
      async () => {
        const s = await h.reviewStickyHeaderInfo();
        if (s && s.pinned && s.pinned.position === "sticky") return true;
        await h.scrollDiff(40);
        return false;
      },
      { timeout: 12000, timeoutMsg: "no file header pinned (sticky) at the top of the scrolling diff" },
    );

    const s = await h.reviewStickyHeaderInfo();
    assert(s && s.pinned, "a file header is pinned at the top of the scrolling diff");
    assert.strictEqual(s.pinned.position, "sticky", "the pinned file header uses position:sticky (not clipped away)");
    assert(
      Math.abs(s.pinned.top - s.viewportTop) <= 8,
      "the file header is stuck to the top of the diff area (top " + s.pinned.top + " vs viewport " + s.viewportTop + ")",
    );
    assert(
      s.pinned.top >= s.pageHeaderBottom - 2,
      "the sticky file header is NOT hidden behind the ReviewHeader (top " + s.pinned.top + " >= page header bottom " + s.pageHeaderBottom + ")",
    );
    assert(s.pinned.height > 0, "the sticky file header is visible (non-zero height)");
  });

  // ── 2 + 3 + 4: ANNOTER — global / line / range — the anchors show on the diff ───────────────
  it("annotates the diff with a GLOBAL, a LINE and a RANGE comment; the anchors show on the diff", async function () {
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    // A GLOBAL (unanchored) comment via the RIGHT panel's "New comment".
    await h.addGlobalComment(GLOBAL_BODY);

    // Two more GLOBAL comments the later steps act on independently (one RESOLVED, one PROMOTED).
    // The agent scopes them to the Spec under review (Finding 41 — `spec_id`) so they surface in the
    // Spec-scoped panel (an unscoped post would be orphaned off this Spec's feed).
    for (const body of [RESOLVE_BODY, PROMOTE_BODY]) {
      await h.mcpCall("tools/call", { name: "review_comment_post", arguments: { workspace_id: workspaceId, repository_id: apiRepositoryId, spec_id: specId, body } });
    }

    // A LINE comment on the FIRST anchorable diff line.
    await h.addLineComment(LINE_BODY);

    // The UI comments landed in the feed (via the MCP read façade — proves persistence).
    await browser.waitUntil(
      async () => {
        const bodies = (await h.listComments(workspaceId, apiRepositoryId)).map((c) => c.body);
        return bodies.includes(GLOBAL_BODY) && bodies.includes(LINE_BODY);
      },
      { timeout: 15000, timeoutMsg: "the global + line comments never landed in the feed" },
    );

    // A RANGE comment: a multi-line anchor over the LINE comment's file (feature/new side).
    const lineComment = (await h.listComments(workspaceId, apiRepositoryId)).find((c) => c.body === LINE_BODY);
    assert(lineComment && lineComment.file_path, "the LINE comment is anchored to a file");
    assert(lineComment.line_start != null, "the LINE comment carries an anchor line");
    await h.mcpCall("tools/call", {
      name: "review_comment_post",
      arguments: {
        workspace_id: workspaceId,
        repository_id: apiRepositoryId,
        spec_id: specId,
        body: RANGE_BODY,
        file_path: lineComment.file_path,
        side: lineComment.side || "new",
        line_start: lineComment.line_start,
        line_end: (lineComment.line_start || 1) + 2,
      },
    });
    await browser.waitUntil(
      async () => {
        const range = (await h.listComments(workspaceId, apiRepositoryId)).find((c) => c.body === RANGE_BODY);
        return range && range.line_end != null && range.line_end > range.line_start;
      },
      { timeout: 15000, timeoutMsg: "the RANGE comment never persisted with a multi-line anchor" },
    );

    // The ANCHORS SHOW on the diff (a marker dot) and the panel shows the GLOBAL comment.
    await browser.waitUntil(async () => (await h.anchorMarkerCount()) >= 1, { timeout: 15000, timeoutMsg: "no anchor marker dot appeared on the diff" });
    assert((await h.reviewText()).indexOf(GLOBAL_BODY) !== -1, "the GLOBAL comment shows in the comments panel");

    // Finding 44 — the ANCHORED threads in the right panel now offer "Promote to task" too (it used
    // to be only on the inline diff cards + the global threads).
    await browser.waitUntil(
      async () => h.hasElement('[data-testid="panel-thread-row"] [data-testid="promote-comment"]'),
      { timeout: 15000, timeoutMsg: "Promote to task never appeared on an anchored panel thread row" },
    );
  });

  // ── 7: AGENT reply via MCP appears in the thread ────────────────────────────────────────────
  it("shows an AGENT reply posted via MCP in the comment thread (réponse agent via MCP)", async function () {
    const root = (await h.listComments(workspaceId, apiRepositoryId)).find((c) => c.body === GLOBAL_BODY);
    assert(root, "the global comment root is present");
    const reply = await h.agentReply(workspaceId, apiRepositoryId, root.id, AGENT_REPLY);
    assert.strictEqual(reply.author_type, "agent", "the MCP reply is authored by the AGENT");
    assert.strictEqual(reply.parent_id, root.id, "the reply threads under the global comment root");

    await browser.waitUntil(async () => (await h.reviewText()).indexOf(AGENT_REPLY) !== -1, {
      timeout: 20000,
      timeoutMsg: "the agent's MCP reply never appeared in the review UI",
    });
    assert(await h.hasElement('[data-testid="comment-thread"]'), "a comment thread card is rendered showing the agent reply");
  });

  // ── resolution HUMAN-only (backend guarantee) — bonus over the 9 points ──────────────────────
  it("lets the HUMAN resolve a comment while the agent façade exposes NO resolve tool", async function () {
    const tools = await h.mcpCall("tools/list", {});
    const names = (tools.tools || []).map((t) => t.name);
    assert(!names.includes("review_comment_resolve") && !names.includes("review_comment_reopen"), "the MCP agent façade exposes NO comment-resolve tool (human-only)");

    await h.waitForThreadWithBody(RESOLVE_BODY, 15000);
    assert(await h.clickThreadControl(RESOLVE_BODY, "resolve-comment"), "the human clicked Resolve on the dedicated global thread");
    let resolved;
    await browser.waitUntil(
      async () => {
        resolved = (await h.listComments(workspaceId, apiRepositoryId)).find((c) => c.body === RESOLVE_BODY);
        return resolved && resolved.status === "resolved" && resolved.resolved_by_type === "human";
      },
      { timeout: 15000, timeoutMsg: "the resolved comment never reached resolved/human status" },
    );
    assert.strictEqual(resolved.resolved_by_type, "human", "resolution is stamped HUMAN (never agent)");
  });

  // ── 5: PROMOTION en task (finding = task) via the GRILL (human → agent → human) ──────────────
  it("promotes a comment into a task via the GRILL: human promote → agent structure (MCP) → human validate", async function () {
    await h.waitForThreadWithBody(PROMOTE_BODY, 15000);
    assert(await h.clickThreadControl(PROMOTE_BODY, "promote-comment"), "the human clicked Promote to task (grill step 1 — lâche brut)");

    // Finding 44 — promoting gives VISIBLE feedback (a confirmation banner), no longer "nothing
    // happens": the promoted task lands and the panel confirms it.
    await browser.waitUntil(
      async () => h.hasElement('[data-testid="promote-feedback"]'),
      { timeout: 10000, timeoutMsg: "no promote feedback appeared after promoting (silent promote)" },
    );

    const srcComment = (await h.listComments(workspaceId, apiRepositoryId)).find((c) => c.body === PROMOTE_BODY);
    assert(srcComment, "the promoted comment is present");

    let task;
    await browser.waitUntil(
      async () => {
        task = (await h.listTasks(workspaceId, apiRepositoryId)).find((t) => t.from_comment_id === srcComment.id && t.grill_state === "proposed");
        return !!task;
      },
      { timeout: 15000, timeoutMsg: "no `proposed` task appeared for the promoted comment" },
    );

    const structured = await h.agentStructureTask(task.id, "Fix: " + task.title.slice(0, 40), "Structured by the agent for the coder.", "medium");
    assert.strictEqual(structured.grill_state, "structured", "the agent structured the task via MCP");

    // The UI shows the structured task with a HUMAN Validate control (grill step 3 pending).
    await browser.waitUntil(
      async () => browser.execute(() => !!document.querySelector('[data-testid="task-row"][data-grill-state="structured"]')),
      { timeout: 15000, timeoutMsg: "the structured task never surfaced in the UI with a validate control" },
    );
    const validated = await browser.execute(() => {
      const btn = document.querySelector('[data-testid="validate-task"]');
      if (!btn) return false;
      btn.click();
      return true;
    });
    assert(validated, "the human clicked Validate on the structured task");
    await browser.waitUntil(
      async () => (await h.listTasks(workspaceId, apiRepositoryId)).some((t) => t.id === task.id && t.grill_state === "validated"),
      { timeout: 15000, timeoutMsg: "the task never reached `validated` after the human validated it" },
    );
  });

  // ── 6: RE-MAPPING + OUTDATED after a real commit (one follows, one outdates), driven by the
  //       LIVING-HEAD effect — NO seam trigger: the git-watch push re-pulls the diff, the fresh
  //       target_sha fires the one-shot re-map (review-page.tsx living-HEAD effect, Finding 5). ──
  it("re-maps one anchor and outdates another after a real commit moves/deletes the anchored code", async function () {
    // Keep `api` active on the LIVING branches (base…feature) diff so its target_sha tracks feature HEAD.
    await h.selectRepoTabByName("api");
    await h.selectDiffSurface("branches");
    await h.setRangeRevs("base", "feature");
    await h.selectVariety("living");
    await h.waitForDiffLoaded(20000);

    // Anchor TWO comments on the anchor file's feature (HEAD) side, capturing the CURRENT HEAD blob.
    const gitOut = (...args) => require("child_process").execFileSync("git", ["-C", seed.apiDir, ...args], { stdio: "pipe" }).toString();
    const headBlob = gitOut("rev-parse", "HEAD:" + h.ANCHOR_FILE).trim();
    const content = gitOut("show", "HEAD:" + h.ANCHOR_FILE);
    const lines = content.split("\n");
    const keepLine = lines.indexOf(h.ANCHOR_KEEP_LINE) + 1; // 1-based
    const goneLine = lines.indexOf(h.ANCHOR_GONE_LINE) + 1;
    assert(keepLine > 0 && goneLine > 0, "both anchor lines are present in the feature HEAD content");

    const postAnchor = (body, line, snippet) =>
      h.mcpCall("tools/call", {
        name: "review_comment_post",
        arguments: {
          workspace_id: workspaceId,
          repository_id: apiRepositoryId,
          spec_id: specId,
          body,
          file_path: h.ANCHOR_FILE,
          side: "new",
          line_start: line,
          blob: headBlob,
          snippet,
          context: lines.slice(Math.max(0, line - 4), line + 3).join("\n"),
        },
      });
    await postAnchor(KEEP_BODY, keepLine, h.ANCHOR_KEEP_LINE);
    await postAnchor(GONE_BODY, goneLine, h.ANCHOR_GONE_LINE);
    await browser.waitUntil(
      async () => {
        const bodies = (await h.listComments(workspaceId, apiRepositoryId)).map((c) => c.body);
        return bodies.includes(KEEP_BODY) && bodies.includes(GONE_BODY);
      },
      { timeout: 15000, timeoutMsg: "the two re-map anchors never landed" },
    );

    // A REAL NEW COMMIT on feature (HEAD advances): shifts the KEEP line DOWN, DELETES the GONE line.
    // The git-watch pushes `repos://changed`; the living review re-pulls; the fresh target_sha fires
    // the one-shot living-HEAD re-map (no manual trigger). Assert the OUTCOME from the feed.
    h.advanceApiRepo(seed.apiDir);
    let keep, gone;
    await browser.waitUntil(
      async () => {
        const comments = await h.listComments(workspaceId, apiRepositoryId);
        keep = comments.find((c) => c.body === KEEP_BODY);
        gone = comments.find((c) => c.body === GONE_BODY);
        return keep && gone && keep.outdated === false && keep.line_start > keepLine && gone.outdated === true;
      },
      {
        timeout: 30000,
        timeoutMsg: "re-mapping did not follow the KEEP anchor / outdate the GONE anchor: " + JSON.stringify({ keep, gone, keepLine }),
      },
    );
    assert.strictEqual(keep.outdated, false, "the KEEP anchor followed the code (re-mapped, not outdated)");
    assert(keep.line_start > keepLine, "the KEEP anchor line moved DOWN after the insert");
    assert.strictEqual(gone.outdated, true, "the GONE anchor flipped OUTDATED (its line was deleted)");

    // The OUTDATED state is VISIBLE in the UI (an outdated badge on the anchored root / panel row).
    await browser.waitUntil(
      async () => /outdated/i.test(await h.reviewText()) || (await h.hasElement('[data-testid="comment-outdated"]')),
      { timeout: 15000, timeoutMsg: "the OUTDATED state never became visible in the review UI" },
    );
  });

  // ── 8: TEMPS RÉEL — a repo edit refreshes the views + the unread counter updates live ────────
  it("refreshes the review live on a repo change and updates the unread counter (temps réel)", async function () {
    // Switch to the `web` repo tab and open its LIVING working-tree review.
    await h.selectRepoTabByName("web");
    await h.selectDiffSurface("working_tree");
    await h.selectVariety("living");
    await browser.pause(500);

    // A fresh, untracked file in `web`'s work tree — the git-watch pushes `repos://changed`; the
    // living review re-pulls and the new file appears WITHOUT a manual reload.
    const marker = "live" + Date.now();
    h.touchWorkTree(seed.webDir, marker);
    const changedRel = "live_change_" + marker + ".txt";
    await browser.waitUntil(async () => (await h.reviewText()).indexOf(changedRel) !== -1, {
      timeout: 25000,
      timeoutMsg: "the live repo edit never refreshed the review view (no real-time push)",
    });

    // UNREAD counter (non-lus) updates LIVE. Close the comments panel first (an open panel marks the
    // feed read on sight); then post AGENT comments on `web` via MCP → the unread badge RISES on the
    // `repos://changed` push, no reload. Closing also proves the RIGHT panel is CLOSABLE (SidebarShell
    // side="right", collapsible — PDR-0028).
    await h.closeCommentsPanel();
    await browser.pause(400);
    assert(
      await browser.execute(() => {
        const aside = document.querySelector('aside[aria-label="Comments"]');
        return !!aside && (aside.getAttribute("data-open") === "false" || aside.getAttribute("aria-hidden") === "true");
      }),
      "the right comments panel is CLOSABLE (collapsed after the toggle)",
    );
    await h.mcpCall("tools/call", { name: "review_comment_post", arguments: { workspace_id: workspaceId, repository_id: webRepositoryId, spec_id: specId, body: "UNREAD_A_" + marker } });
    await h.mcpCall("tools/call", { name: "review_comment_post", arguments: { workspace_id: workspaceId, repository_id: webRepositoryId, spec_id: specId, body: "UNREAD_B_" + marker } });
    await browser.waitUntil(async () => (await h.unreadBadgeCount()) >= 1, {
      timeout: 20000,
      timeoutMsg: "the unread badge never rose after live agent comments on the repo (panel closed)",
    });
  });

  // ── Finding 41: review comments are SPEC-SCOPED — a comment on Spec A never leaks onto Spec B ──
  it("scopes review comments to their Spec: a Spec A comment never appears on Spec B (Finding 41)", async function () {
    // Two GLOBAL markers on the SAME (workspace, api repo), each bound to a DIFFERENT Spec.
    await h.mcpCall("tools/call", { name: "review_comment_post", arguments: { workspace_id: workspaceId, repository_id: apiRepositoryId, spec_id: specId, body: SCOPE_A_BODY } });
    await h.mcpCall("tools/call", { name: "review_comment_post", arguments: { workspace_id: workspaceId, repository_id: apiRepositoryId, spec_id: scopeSpecId, body: SCOPE_B_BODY } });
    // Both persist (unscoped MCP read sees the whole repo feed).
    await browser.waitUntil(
      async () => {
        const bodies = (await h.listComments(workspaceId, apiRepositoryId)).map((c) => c.body);
        return bodies.includes(SCOPE_A_BODY) && bodies.includes(SCOPE_B_BODY);
      },
      { timeout: 15000, timeoutMsg: "the two spec-scoped markers never persisted" },
    );

    // The SECOND Spec's review shows ONLY its own comment — NOT Spec A's markers.
    await h.openReviewForSpec(scopeSpecId);
    await h.selectRepoTabByName("api");
    await browser.waitUntil(async () => (await h.reviewText()).indexOf(SCOPE_B_BODY) !== -1, {
      timeout: 20000,
      timeoutMsg: "Spec B's own comment never rendered on its review",
    });
    const bText = await h.reviewText();
    assert(bText.indexOf(SCOPE_A_BODY) === -1, "Spec A's comment must NOT leak onto Spec B's review (Finding 41)");
    assert(bText.indexOf(GLOBAL_BODY) === -1, "none of Spec A's earlier comments leak onto Spec B");

    // Finding 53 — the TASKS are Spec-scoped too. The task promoted+validated on Spec A (test 5,
    // seeded from PROMOTE_BODY = "REVIEW_PROMOTE_…") must NOT appear on Spec B's Tasks section.
    const bTasks = await h.reviewTaskTitles();
    assert(
      !bTasks.some((t) => t.indexOf("REVIEW_PROMOTE") !== -1),
      "a task promoted on Spec A must NOT leak onto Spec B's review (Finding 53): " + JSON.stringify(bTasks),
    );

    // The FIRST Spec's review shows Spec A's marker and NOT Spec B's.
    await h.openReviewForSpec(specId);
    await h.selectRepoTabByName("api");
    await browser.waitUntil(async () => (await h.reviewText()).indexOf(SCOPE_A_BODY) !== -1, {
      timeout: 20000,
      timeoutMsg: "Spec A's own comment never rendered on its review",
    });
    assert((await h.reviewText()).indexOf(SCOPE_B_BODY) === -1, "Spec B's comment must NOT leak onto Spec A's review (Finding 41)");

    // Finding 53 — Spec A's OWN promoted task DOES appear on Spec A's review (the scoping keeps a
    // Spec's own tasks; it only blocks the cross-Spec leak).
    await browser.waitUntil(
      async () => (await h.reviewTaskTitles()).some((t) => t.indexOf("REVIEW_PROMOTE") !== -1),
      { timeout: 20000, timeoutMsg: "Spec A's own promoted task never rendered on Spec A's review (Finding 53)" },
    );
  });

  // ── Finding 47: the shared breadcrumb harmonises the 3 Work pages and is NAVIGABLE ───────────
  it("renders a shared, navigable breadcrumb on review / Spec detail / board (Finding 47)", async function () {
    // On the review page the breadcrumb is `Work / <specKey> / Review`.
    await h.openReviewForSpec(specId);
    await h.selectRepoTabByName("api");
    const crumbs = await h.breadcrumbCrumbs();
    assert(crumbs && crumbs.length === 3, "the review breadcrumb has 3 crumbs (got " + JSON.stringify(crumbs) + ")");
    assert(crumbs[0] === "Work", "review breadcrumb starts with Work");
    assert(crumbs[2] === "Review", "review breadcrumb ends with Review");
    const specKeyLabel = crumbs[1];
    assert(specKeyLabel && specKeyLabel.length > 0, "the middle crumb is the Spec key (got '" + specKeyLabel + "')");

    // Click the <specKey> crumb → the Spec detail (`/work/$specId`); it LEAVES the review page and
    // its breadcrumb becomes `Work / <specKey>` (no Review crumb) — the SAME shared chrome.
    await h.clickBreadcrumbCrumb(specKeyLabel);
    await browser.waitUntil(
      async () => browser.execute(() => !document.querySelector('[data-testid="review-page"]')),
      { timeout: 10000, timeoutMsg: "the <specKey> crumb did not leave the review page" },
    );
    await browser.waitUntil(
      async () => {
        const c = await h.breadcrumbCrumbs();
        return c && c.length === 2 && c[0] === "Work" && c[1] === specKeyLabel;
      },
      { timeout: 10000, timeoutMsg: "the Spec-detail breadcrumb is not `Work / <specKey>`" },
    );

    // Click the Work crumb → the board (`/work`); its breadcrumb is a single `Work` crumb.
    await h.clickBreadcrumbCrumb("Work");
    await browser.waitUntil(
      async () => browser.execute(() => /\bSpecs\b/.test(document.body.innerText || "")),
      { timeout: 10000, timeoutMsg: "the Work crumb did not reach the board" },
    );
    const boardCrumbs = await h.breadcrumbCrumbs();
    assert(
      boardCrumbs && boardCrumbs.length === 1 && boardCrumbs[0] === "Work",
      "the board breadcrumb is a single `Work` crumb (got " + JSON.stringify(boardCrumbs) + ")",
    );
  });

  // ── empty-state — a Spec with NO workspace (workspace_context null) → clean empty copy ────────
  it("shows a clean empty state on a Spec with no dev workspace (workspace_context null)", async function () {
    await h.openBoard();
    await h.openSpecCard(emptySpecId, 20000);
    await browser.waitUntil(
      async () => browser.execute((t) => (document.body.innerText || "").indexOf(t) !== -1, "Unanchored spec (no workspace)"),
      { timeout: 15000, timeoutMsg: "the unanchored PRD detail did not render" },
    );
    await h.clickDetailTab("Review");
    // The review VIEWER (repo tablist) must NOT mount; instead the null empty-state copy renders.
    await browser.waitUntil(
      async () => browser.execute(() => /this Spec has no dev workspace yet/i.test(document.body.innerText || "")),
      { timeout: 15000, timeoutMsg: "the null-workspace empty state did not render on the Review tab" },
    );
    assert(!(await h.hasElement('[role="tablist"][aria-label="Repositories"]')), "no review viewer chrome renders for a Spec with no workspace");
    assert(!(await h.hasElement('[data-testid="open-review"]')), "no 'Open review' entry card for a Spec with no workspace (nothing to review)");
  });

  // ── 9: NO /review route / nav item exists anymore (static source + DOM) ──────────────────────
  it("has NO /review route, NO top-level nav item, NO __nyxReview seam (review lives ONLY in the tab)", async function () {
    // Static (the "assert statique" of the done-criterion): read the renderer source directly.
    h.assertNoReviewRoute(assert);
    // DOM: no top-level nav element routes to /review (the shell nav has no Review entry).
    const hasReviewNav = await browser.execute(
      () => !!document.querySelector('a[href="/review"], a[aria-label="Review"], [data-to="/review"]'),
    );
    assert(!hasReviewNav, "no top-level nav item points at /review (the review is reachable only via the Spec's Review tab)");
  });

  // ── NON-REGRESSION — pure-local, no VS Code / no forge ───────────────────────────────────────
  it("stays pure-local: no VS Code, no remote forge surfaced anywhere in the review", async function () {
    // Re-open the anchored Spec's Review tab, then the full-screen page, for the final assertions.
    await h.openBoard();
    await h.openSpecCard(specId, 20000);
    await h.clickDetailTab("Review");
    await h.waitForReviewTab(20000);
    await h.clickOpenReview();
    await h.waitForReviewScreen(30000);
    const txt = await h.reviewText();
    assert(!/vs ?code/i.test(txt), "no VS Code surface in the review");
    assert(!/github\.com|gitlab\.com|bitbucket|pull request|\bPR #/i.test(txt), "no remote-forge surface in the review (local only)");
    assert(await h.hasElement('[role="tablist"][aria-label="Repositories"]'), "the review page is still mounted and functional at the end of the gate");
    // Back from the page returns to the Spec detail (`/work/$specId`) — the tab, not a top-level route.
    await h.clickReviewBack();
    await browser.waitUntil(
      async () => browser.execute(() => !document.querySelector('[data-testid="review-page"]')),
      { timeout: 10000, timeoutMsg: "the Back control did not leave the review page" },
    );
  });
});
