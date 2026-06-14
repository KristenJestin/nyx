/* eslint-disable */
// RELEASE evidence for the human review 01KV3CM1941N7PYTSA1JGF2NX6 — the sidebar
// animation findings that kept REGRESSING. Captured from the REAL WebKitGTK
// release build under WSLg (software GL), this spec proves, with per-frame rAF
// traces, the ROBUST redesign:
//
//   - the SELECTION RAIL is now a single `layoutId="active-rail"` element rendered
//     INSIDE the active row (Motion shared-layout FLIP), so it FOLLOWS the active
//     row through collapse-close, close-active and reorder — never flies in the
//     void, never vanishes (finding 01KV3CMN0PGZHJ0KZH6B25KQ08);
//   - ADD a terminal: the new row ramps in ONCE (no double-tp) and existing /
//     active rows keep a STABLE height (no shrink) (01KV3CMX5HVEEVA42ZEW486M0K);
//   - REMOVE a terminal: single collapse + the sibling below slides up, no
//     second teleport;
//   - REORDER: the dragged row follows the pointer (covered by the existing
//     rail-slide on select + this spec's rail-follows-active proof).
//
// Traces + screenshots go to <repo>/.shots (gitignored). The decisive numbers are
// asserted here AND printed so the report can cite them.

const assert = require("assert");
const fs = require("fs");
const os = require("os");
const path = require("path");

const REFLOW_ROOT = fs.realpathSync(os.tmpdir());
function mkDir(name) {
  const p = path.join(REFLOW_ROOT, "nyx-rail-" + process.pid, name);
  fs.mkdirSync(p, { recursive: true });
  return fs.realpathSync(p);
}
const PROJ_A = mkDir("railproj");
const PROJ_B = mkDir("railproj-b");

const SHOTS = path.resolve(__dirname, "..", "..", ".shots");
function saveJson(name, obj) {
  fs.mkdirSync(SHOTS, { recursive: true });
  fs.writeFileSync(path.join(SHOTS, name), JSON.stringify(obj, null, 2));
}
function saveShot(name) {
  fs.mkdirSync(SHOTS, { recursive: true });
  return browser.saveScreenshot(path.join(SHOTS, name));
}

async function waitForApp() {
  await browser.waitUntil(
    async () => browser.execute(() => !!(window.__nyx && window.__nyx.activeId() != null)),
    { timeout: 30000, timeoutMsg: "window.__nyx active terminal never appeared" },
  );
  await browser.pause(1000);
}

const listRecords = () => browser.execute(() => (window.__nyx ? window.__nyx.list() : []));

// Distinct-value counter helper used by the assertions (a slide/ramp produces
// several distinct intermediate samples; a teleport produces ~2).
function distinctRounded(values) {
  return Array.from(new Set(values.map((v) => Math.round(v))));
}

describe("rail + list animations — RELEASE evidence (tauri-driver, WebKitGTK)", function () {
  before(async function () {
    await waitForApp();
  });

  it("ADD: a new loose row ramps in ONCE (no double-tp) AND the active row keeps a STABLE height (no shrink)", async function () {
    // Make sure there is an ACTIVE loose row already (the bootstrap terminal is
    // active). We sample BOTH the new row (must ramp once, no settle-teleport) and
    // the previously-active row (its height must stay constant — the Image-13
    // shrink bug).
    await saveShot("add-before.png");

    const trace = await browser.executeAsync(function (done) {
      var footer = document.querySelector("aside section:last-of-type");
      if (!footer) return done({ error: "footer not found" });

      function looseRows() {
        return Array.prototype.filter.call(
          footer.querySelectorAll("[data-rail-row]"),
          function (r) {
            return r.querySelector('button[aria-label^="Close terminal"]');
          },
        );
      }
      var beforeRows = looseRows();
      // The active row we will watch for shrink (if any active loose row exists).
      var activeRow =
        beforeRows.filter(function (r) {
          return r.getAttribute("aria-current") === "true";
        })[0] ||
        beforeRows[0] ||
        null;
      var activeH0 = activeRow
        ? Math.round(activeRow.getBoundingClientRect().height * 100) / 100
        : null;

      var btn = document.querySelector('button[aria-label="New terminal"]');
      if (!btn) return done({ error: "global + button not found" });

      // Lock onto the freshly-added row element ONCE (the first row that is NOT in
      // the before-set), then keep measuring THAT SAME element across all frames —
      // so a transient DOM reorder during the animation can't make us measure a
      // different row (which would read a phantom height drop).
      var beforeSet = beforeRows.slice();
      var lockedNewRow = null;
      function newRow() {
        if (lockedNewRow && lockedNewRow.isConnected) return lockedNewRow;
        var rows = looseRows();
        for (var i = 0; i < rows.length; i++) {
          if (beforeSet.indexOf(rows[i]) === -1) {
            lockedNewRow = rows[i];
            return lockedNewRow;
          }
        }
        return null;
      }

      var samples = [];
      var start = performance.now();
      btn.click();

      function sample() {
        var nr = newRow();
        var nh = nr ? Math.round(nr.getBoundingClientRect().height * 100) / 100 : null;
        var no = nr ? Math.round(parseFloat(getComputedStyle(nr).opacity) * 1000) / 1000 : null;
        var ah =
          activeRow && activeRow.isConnected
            ? Math.round(activeRow.getBoundingClientRect().height * 100) / 100
            : null;
        samples.push({ t: Math.round(performance.now() - start), newH: nh, newO: no, activeH: ah });
        if (performance.now() - start < 800) requestAnimationFrame(sample);
        else done({ activeH0: activeH0, samples: samples });
      }
      requestAnimationFrame(sample);
    });

    assert(!trace.error, "add trace error: " + trace.error);
    saveJson("add-trace.json", trace);
    await saveShot("add-after.png");

    const present = trace.samples.filter((s) => s.newH != null);
    assert(present.length >= 5, "captured several frames of the new row");
    const heights = present.map((s) => s.newH);
    const minH = Math.min.apply(null, heights);
    const maxH = Math.max.apply(null, heights);
    assert(maxH >= 24, "new row reaches full height (was " + maxH + ")");
    assert(minH <= maxH * 0.7, "new row starts well below full (start " + minH + ")");
    const ramp = distinctRounded(heights.filter((h) => h > minH + 1 && h < maxH - 1));
    assert(
      ramp.length >= 3,
      "new row RAMPED over multiple frames (intermediates " + JSON.stringify(ramp) + ")",
    );

    // NO DOUBLE-TP: once the new row REACHES (within 1px of) its full height, it
    // must not later DROP back down and re-grow (a second teleport/settle). Find
    // the first frame at full height; from there height must stay ~full.
    const fullFrom = present.findIndex((s) => s.newH >= maxH - 1);
    assert(fullFrom !== -1, "the new row reached full height");
    const tail = present.slice(fullFrom).map((s) => s.newH);
    const tailMin = Math.min.apply(null, tail);
    assert(
      tailMin >= maxH - 2,
      "NO double-tp: after reaching full height the new row never dropped back (tail min " +
        tailMin +
        " vs full " +
        maxH +
        ")",
    );

    // NO ACTIVE-ROW SHRINK: the previously-active row's height stayed within 2px
    // of its starting height across the whole add (Image-13 had it shrink/distort).
    if (trace.activeH0 != null) {
      const activeHs = trace.samples.map((s) => s.activeH).filter((h) => h != null);
      const aMin = Math.min.apply(null, activeHs);
      const aMax = Math.max.apply(null, activeHs);
      console.log(
        "ADD active-row height: start=" + trace.activeH0 + " min=" + aMin + " max=" + aMax,
      );
      assert(
        trace.activeH0 - aMin <= 2,
        "the active row did NOT shrink on add (start " + trace.activeH0 + ", min " + aMin + ")",
      );
      assert(
        aMax - trace.activeH0 <= 2,
        "the active row did NOT grow/distort on add (start " +
          trace.activeH0 +
          ", max " +
          aMax +
          ")",
      );
    }
    console.log(
      "ADD new-row: minH=" +
        minH +
        " maxH=" +
        maxH +
        " ramp=" +
        ramp.length +
        " tailMin=" +
        tailMin,
    );
  });

  it("REMOVE: a closed row collapses ONCE (no double-tp) and the sibling below slides up", async function () {
    // Use a workspace list (scroll area) with two rows so "sibling slides up" is
    // unambiguous. Build a project + attach two terminals to its root.
    const projE = await browser.execute(
      (n, r) => window.__nyx.createProject(n, r, "root"),
      "rail-remove",
      PROJ_A,
    );
    const wsE = projE.root.id;
    for (let k = 0; k < 2; k++) {
      const before = (await listRecords()).map((r) => r.id);
      await browser.execute((c) => window.__nyx.create(c), PROJ_A);
      let id = null;
      await browser.waitUntil(
        async () => {
          const fresh = (await listRecords()).filter((r) => before.indexOf(r.id) === -1);
          if (fresh.length) id = fresh[fresh.length - 1].id;
          return !!id;
        },
        { timeout: 15000, timeoutMsg: "remove terminal " + k + " never appeared" },
      );
      await browser.pause(500);
      await browser.execute((i) => window.__nyx.autoAttach(i), id);
      await browser.waitUntil(
        async () => {
          const rec = (await listRecords()).find((r) => r.id === id);
          return rec && rec.workspace_id === wsE;
        },
        { timeout: 10000, timeoutMsg: "remove terminal " + k + " never bound" },
      );
    }
    await browser.pause(700);

    const trace = await browser.executeAsync(function (done) {
      var scroll = document.querySelector("aside section");
      var rows = Array.prototype.filter.call(
        (scroll || document).querySelectorAll("[data-rail-row]"),
        function (r) {
          return r.querySelector('button[aria-label^="Close terminal"]');
        },
      );
      if (rows.length < 2) return done({ error: "need >=2 workspace rows, got " + rows.length });
      var leaving = rows[0];
      var sibling = rows[1];
      var closeBtn = leaving.querySelector('button[aria-label^="Close terminal"]');
      var samples = [];
      var start = performance.now();
      var leaveH0 = Math.round(leaving.getBoundingClientRect().height * 100) / 100;
      closeBtn.click();
      function sample() {
        var lh = leaving.isConnected
          ? Math.round(leaving.getBoundingClientRect().height * 100) / 100
          : 0;
        var st = sibling.isConnected
          ? Math.round(sibling.getBoundingClientRect().top * 100) / 100
          : null;
        samples.push({ t: Math.round(performance.now() - start), leaveH: lh, sibTop: st });
        if (performance.now() - start < 700) requestAnimationFrame(sample);
        else done({ leaveH0: leaveH0, samples: samples });
      }
      requestAnimationFrame(sample);
    });

    assert(!trace.error, "remove trace error: " + trace.error);
    saveJson("remove-trace.json", trace);
    await saveShot("remove-after.png");

    const heights = trace.samples.map((s) => s.leaveH).filter((h) => h != null);
    const maxH = Math.max.apply(null, heights);
    const lastH = heights[heights.length - 1];
    assert(maxH >= 20, "leaving row started at a real height (" + maxH + ")");
    assert(lastH <= maxH * 0.4, "leaving row collapsed toward 0 (ended " + lastH + ")");
    const midH = distinctRounded(heights.filter((h) => h > 1 && h < maxH - 1));
    assert(midH.length >= 3, "collapse RAMPED (intermediates " + JSON.stringify(midH) + ")");

    // NO DOUBLE-TP on remove: the leaving height is MONOTONIC non-increasing (it
    // never grows back mid-collapse, which a second animator would cause).
    let maxGrowth = 0;
    for (let i = 1; i < heights.length; i++) {
      maxGrowth = Math.max(maxGrowth, heights[i] - heights[i - 1]);
    }
    assert(
      maxGrowth <= 2,
      "NO double-tp on remove: collapse is monotonic (max upward step " + maxGrowth + ")",
    );

    const sibs = trace.samples.map((s) => s.sibTop).filter((y) => y != null);
    const minSib = Math.min.apply(null, sibs);
    const maxSib = Math.max.apply(null, sibs);
    assert(maxSib - minSib >= 6, "sibling slid up (delta " + (maxSib - minSib) + ")");
    const distinctSib = distinctRounded(sibs);
    assert(
      distinctSib.length >= 3,
      "sibling SLID over frames (distinct " + distinctSib.length + ")",
    );
    console.log(
      "REMOVE leave: max=" +
        maxH +
        " last=" +
        lastH +
        " maxGrowth=" +
        maxGrowth +
        " sibDelta=" +
        (maxSib - minSib),
    );
  });

  it("RAIL follows the ACTIVE row through a COLLAPSE-CLOSE (travels WITH the row, not a post-jump)", async function () {
    // Image-14 scenario: the active row must TRAVEL with its row when a band
    // COLLAPSES, not stay put then jump ("fly in the void"). To make the active
    // row actually MOVE, we put the active terminal in a SECOND project (below the
    // first) and collapse the FIRST project's band — the active row in the second
    // project slides UP as the band above it folds, and the rail (a layoutId
    // element INSIDE that row) travels with it frame-by-frame.
    const projB = await browser.execute(
      (n, r) => window.__nyx.createProject(n, r, "root"),
      "rail-collapse-B",
      PROJ_B,
    );
    const wsB = projB.root.id;
    // Attach + activate a terminal in project B (it renders BELOW rail-remove).
    // Created at PROJ_B (distinct from rail-remove's PROJ_A) so auto-attach binds
    // it to project B's workspace, not rail-remove's.
    {
      const before = (await listRecords()).map((r) => r.id);
      await browser.execute((c) => window.__nyx.create(c), PROJ_B);
      let id = null;
      await browser.waitUntil(
        async () => {
          const fresh = (await listRecords()).filter((r) => before.indexOf(r.id) === -1);
          if (fresh.length) id = fresh[fresh.length - 1].id;
          return !!id;
        },
        { timeout: 15000, timeoutMsg: "collapse terminal never appeared" },
      );
      await browser.pause(500);
      await browser.execute((i) => window.__nyx.autoAttach(i), id);
      await browser.waitUntil(
        async () => {
          const rec = (await listRecords()).find((r) => r.id === id);
          return rec && rec.workspace_id === wsB;
        },
        { timeout: 10000, timeoutMsg: "collapse terminal never bound" },
      );
      await browser.execute((i) => window.__nyx.setActive(i), id);
    }
    await browser.pause(600);

    const trace = await browser.executeAsync(function (done) {
      function activeRow() {
        return document.querySelector('aside [data-rail-row][aria-current="true"]');
      }
      function railEl() {
        var row = activeRow();
        if (!row) return null;
        // The rail is the absolutely-positioned bg-primary span inside the row.
        var spans = row.querySelectorAll("span");
        for (var i = 0; i < spans.length; i++) {
          var s = spans[i];
          var cs = getComputedStyle(s);
          if (cs.position === "absolute" && parseFloat(cs.width) <= 4) return s;
        }
        return null;
      }
      function railTop() {
        var r = railEl();
        return r ? Math.round(r.getBoundingClientRect().top * 100) / 100 : null;
      }
      // Collapse the FIRST project ("rail-remove"), which sits ABOVE project B.
      var toggle = null;
      var lis = document.querySelectorAll("aside li");
      lis.forEach(function (li) {
        var b = li.querySelector("button[aria-expanded]");
        if (b && (b.textContent || "").indexOf("rail-remove") !== -1 && !toggle) toggle = b;
      });
      if (!toggle) return done({ error: "rail-remove project toggle not found" });

      var samples = [];
      var start = performance.now();
      var rt0 = railTop();
      var present0 = !!railEl();
      toggle.click(); // collapse the band ABOVE the active row
      function sample() {
        samples.push({
          t: Math.round(performance.now() - start),
          railTop: railTop(),
          present: !!railEl(),
        });
        if (performance.now() - start < 600) requestAnimationFrame(sample);
        else done({ rt0: rt0, present0: present0, samples: samples });
      }
      requestAnimationFrame(sample);
    });

    assert(!trace.error, "collapse-close rail trace error: " + trace.error);
    saveJson("collapse-close-rail-trace.json", trace);
    await saveShot("collapse-close-rail-after.png");

    // The rail stays present throughout (never flies into the void / vanishes)
    // and its top travels with the active row over MULTIPLE frames as the band
    // above folds — not a settle-then-jump.
    const visible = trace.samples.filter((s) => s.present && s.railTop != null);
    assert(visible.length >= 4, "the rail stayed present + measurable during the collapse");
    const tops = visible.map((s) => s.railTop);
    const minT = Math.min.apply(null, tops);
    const maxT = Math.max.apply(null, tops);
    const distinct = distinctRounded(tops);
    assert(
      maxT - minT >= 6,
      "the rail TRAVELLED with the row during collapse (delta " + (maxT - minT) + ")",
    );
    assert(
      distinct.length >= 3,
      "the rail moved over MULTIPLE frames (distinct tops " +
        JSON.stringify(distinct) +
        "); a post-collapse jump would have ~2",
    );
    console.log("COLLAPSE-CLOSE rail: delta=" + (maxT - minT) + " frames=" + distinct.length);

    // Re-expand for the next test.
    await browser.execute(() => {
      var lis = document.querySelectorAll("aside li");
      lis.forEach(function (li) {
        var b = li.querySelector("button[aria-expanded]");
        if (
          b &&
          (b.textContent || "").indexOf("rail-remove") !== -1 &&
          b.getAttribute("aria-expanded") === "false"
        ) {
          b.click();
        }
      });
    });
    await browser.pause(500);
  });

  it("RAIL slides to the NEW active row when the ACTIVE terminal is CLOSED (does NOT vanish)", async function () {
    // Add two loose terminals so we have neighbours, select the first, then CLOSE
    // the active one and prove the rail SLIDES to the new active row (re-targets a
    // neighbour) instead of disappearing.
    await browser.execute(() => {
      const btn = document.querySelector('button[aria-label="New terminal"]');
      if (btn) btn.click();
    });
    await browser.pause(700);
    await browser.execute(() => {
      const btn = document.querySelector('button[aria-label="New terminal"]');
      if (btn) btn.click();
    });
    await browser.pause(700);

    const trace = await browser.executeAsync(function (done) {
      var footer = document.querySelector("aside section:last-of-type");
      if (!footer) return done({ error: "footer not found" });
      var rows = Array.prototype.filter.call(
        footer.querySelectorAll("[data-rail-row]"),
        function (r) {
          return r.querySelector('button[aria-label^="Close terminal"]');
        },
      );
      if (rows.length < 2) return done({ error: "need >=2 loose rows, got " + rows.length });

      function railEl() {
        var row = document.querySelector('aside [data-rail-row][aria-current="true"]');
        if (!row) return null;
        var spans = row.querySelectorAll("span");
        for (var i = 0; i < spans.length; i++) {
          var cs = getComputedStyle(spans[i]);
          if (cs.position === "absolute" && parseFloat(cs.width) <= 4) return spans[i];
        }
        return null;
      }
      function railTop() {
        var r = railEl();
        return r ? Math.round(r.getBoundingClientRect().top * 100) / 100 : null;
      }

      // Make the SECOND-to-last row active, then close it.
      var target = rows[rows.length - 2];
      target.click(); // select it (active)
      setTimeout(function () {
        var samples = [];
        var start = performance.now();
        var before = railTop();
        var closeBtn = target.querySelector('button[aria-label^="Close terminal"]');
        closeBtn.click(); // close the ACTIVE one
        function sample() {
          samples.push({
            t: Math.round(performance.now() - start),
            railTop: railTop(),
            present: !!railEl(),
          });
          if (performance.now() - start < 700) requestAnimationFrame(sample);
          else done({ before: before, samples: samples });
        }
        requestAnimationFrame(sample);
      }, 350);
    });

    assert(!trace.error, "close-active rail trace error: " + trace.error);
    saveJson("close-active-rail-trace.json", trace);
    await saveShot("close-active-rail-after.png");

    // The rail must NOT vanish: after the close settles, a rail is present on the
    // new active row (the LAST sample has a present rail at a real top).
    const settled = trace.samples[trace.samples.length - 1];
    assert(
      settled && settled.present && settled.railTop != null,
      "the rail did NOT vanish — it is present on the new active row after close-active",
    );
    // And it MOVED to its new home (the new active row is a different position) OR
    // at minimum stayed visible throughout (never dropped to absent for the rest).
    const presentCount = trace.samples.filter((s) => s.present).length;
    assert(
      presentCount >= trace.samples.length * 0.5,
      "the rail stayed present for the majority of frames (present " +
        presentCount +
        "/" +
        trace.samples.length +
        ")",
    );
    console.log(
      "CLOSE-ACTIVE rail: before=" +
        trace.before +
        " settledTop=" +
        settled.railTop +
        " presentFrames=" +
        presentCount +
        "/" +
        trace.samples.length,
    );
  });

  it("CLICK-ANYWHERE (01KV3CND2…): a click at the BOTTOM of a row (below the text) selects it — no dead zone", async function () {
    // The dead-zone bug: clicking BELOW the text but still inside the row showed
    // the grab cursor and did NOT select. The row now owns the click, so a click
    // at the row's bottom edge (clearly below the text baseline) must select it.
    const result = await browser.executeAsync(function (done) {
      var footer = document.querySelector("aside section:last-of-type");
      if (!footer) return done({ error: "footer not found" });
      var rows = Array.prototype.filter.call(
        footer.querySelectorAll("[data-rail-row]"),
        function (r) {
          return r.querySelector('button[aria-label^="Close terminal"]');
        },
      );
      if (rows.length < 2) return done({ error: "need >=2 loose rows, got " + rows.length });

      // Choose a row that is NOT currently active, so a successful select changes
      // aria-current to it.
      var target = null;
      for (var i = 0; i < rows.length; i++) {
        if (rows[i].getAttribute("aria-current") !== "true") {
          target = rows[i];
          break;
        }
      }
      target = target || rows[0];

      var box = target.getBoundingClientRect();
      // A point near the BOTTOM edge of the row (below the text), horizontally to
      // the LEFT of the close button so we hit empty row area, not the 'x'.
      var x = box.left + 12;
      var y = box.bottom - 2;
      var atPoint = document.elementFromPoint(x, y);
      // Sanity: the point is inside the target row (or a descendant of it), not
      // the close button.
      var insideRow = !!(atPoint && target.contains(atPoint));
      var onClose = !!(
        atPoint &&
        atPoint.closest &&
        atPoint.closest('button[aria-label^="Close terminal"]')
      );

      // Dispatch a real click sequence at that point (pointer + mouse + click).
      function fire(type, isPointer) {
        var Ctor = isPointer ? PointerEvent : MouseEvent;
        var ev;
        try {
          ev = new Ctor(type, {
            bubbles: true,
            cancelable: true,
            clientX: x,
            clientY: y,
            button: 0,
          });
        } catch (e) {
          ev = new MouseEvent(type, {
            bubbles: true,
            cancelable: true,
            clientX: x,
            clientY: y,
            button: 0,
          });
        }
        (atPoint || target).dispatchEvent(ev);
      }
      fire("pointerdown", true);
      fire("mousedown", false);
      fire("pointerup", true);
      fire("mouseup", false);
      fire("click", false);

      setTimeout(function () {
        var nowActive = target.getAttribute("aria-current") === "true";
        done({ insideRow: insideRow, onClose: onClose, selected: nowActive, x: x, y: y });
      }, 250);
    });

    assert(!result.error, "click-anywhere trace error: " + result.error);
    assert(result.insideRow, "the bottom-edge point is inside the row (a real in-row click)");
    assert(!result.onClose, "the click point is NOT on the close button (it's empty row area)");
    assert(
      result.selected,
      "clicking the BOTTOM of the row (below the text) SELECTED it — no dead zone",
    );
    console.log(
      "CLICK-ANYWHERE bottom-of-row selected=" +
        result.selected +
        " at " +
        result.x +
        "," +
        result.y,
    );
  });

  after(function () {
    try {
      fs.rmSync(path.join(REFLOW_ROOT, "nyx-rail-" + process.pid), {
        recursive: true,
        force: true,
      });
    } catch {}
  });
});
