/**
 * The nyx INTERACTION SHIM injected into the renderer iframe (v2.7 — Prototype : machinerie,
 * Phase 3, task `01KY4DFQ6NXKE9B7FC0KHRQH8T`). This is the ONLY JavaScript that runs inside the
 * frame besides the Tailwind engine — the agent ships ZERO application JS (PDR-0010). It is a
 * closed, declarative interaction model:
 *
 *   * **THE AXES APPLIER (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`)** — `window.__nyxAxes` declares
 *     the component's NAMED AXES (`variant`, `size`, a two-valued `loading`), each value carrying
 *     its CONCRETE classes, and `window.__nyxAxisValues` the ACTIVE COMBINATION. For each axis the
 *     shim puts the active value's classes on that axis's TARGETS — the elements carrying
 *     `data-nyx-axis="variant size"`, or the component's root elements when the template marks
 *     none. **One element in the template, orthogonal axes for free, no cartesian product.** The
 *     classes it added are remembered per element per axis, so flipping an axis removes exactly what
 *     it put there and never touches the template's own classes.
 *   * **THE STRUCTURE** — what classes cannot do: an element that only EXISTS for some values.
 *     `data-nyx-state-case="<value>"` (the FIRST axis, kept verbatim) and its generalization
 *     `data-nyx-if="loading=on"` / `data-nyx-if-not="variant=link"` — a condition on an AXIS
 *     VALUE (`|` = OR inside a clause, `,` = AND between clauses). An element is shown when every
 *     mechanism it declares agrees. An absent one is marked with the `hidden` ATTRIBUTE — what a
 *     stylesheet can select, and what works on an `<svg>` too (finding `01KYSCW89NRM1TSZ0TFXC8HD36`);
 *     the srcdoc turns that attribute into an unconditional `display:none`, whatever Preflight or a
 *     display utility claims.
 *   * **The chip-bar** — ONE ROW PER AXIS built from the declared axes; picking a chip re-runs the
 *     applier. `window.__nyxStates` / `__nyxActiveState` remain accepted as the LEGACY shape
 *     (a flat list of names = one anonymous `state` axis, class-less), so the components stored that
 *     way keep behaving exactly as before.
 *   * **Navigation (`data-nyx-page`, blue)** — clicking an element carrying `data-nyx-page`
 *     posts a `navigate` message to the parent (the ONLY cross-frame effect); the app decides how
 *     to switch page. No in-frame navigation, no history.
 *   * **Axis flip in place (`data-nyx-state`, green)** — clicking an element carrying
 *     `data-nyx-state` flips the FIRST axis to that value through the SAME applier as the
 *     chip-bar — "flip d'état sur place". `data-nyx-set="size=lg"` does it for ANY axis.
 *   * **DESIGNATING A NODE (finding `01KYW32N1BVDVTGS0WPMG2PJW2`)** — the render IS the composition:
 *     an element resolves to the `node_key` of the named tree it belongs to, read off the inert
 *     comment markers the host asked the sidecar to interleave ({@link markCompositionNodes}). In
 *     `select` mode a plain click posts `{ type: "select", nodeKey }` (the params gesture); in `pin`
 *     mode a full-cover capture layer swallows the click and posts `{ type: "pin", nodeKey, x, y }`
 *     (the comment anchor). Ported from depot's feedback shim, MINUS its `computeSelector`: nyx
 *     anchors on a NODE, never on a CSS selector — the tree is the contract.
 *
 * It also reports its content height to the parent (so a live preview can auto-size), and echoes the
 * whole active combination back (`{ type: "axes", values }`, plus `{ type: "state" }` for the first
 * axis) so the HOST — which owns the values — is never out of sync with an in-frame gesture.
 *
 * THE VOCABULARY IS `nyx`, and it is the ONE thing this file still reads under two spellings. The
 * machinery was ported from Depot and had kept that tool's name (`data-depot-*`, `window.__depot*`)
 * up to and including mechanisms INVENTED here; finding `01KYS14G4V1Q5ERRAHB0QVW362` renamed it.
 * Stored templates were rewritten by migration 37, nothing emits or teaches the old prefix any
 * more — and the LEGACY READ TOLERANCE below (one block, four helpers, nothing else) keeps a
 * template that arrives by another path rendering instead of coming back blank.
 *
 * The source is kept as a STRING so it can be inlined into the `srcdoc` with a CSP nonce; the same
 * string is `eval`-uated by the unit tests against a jsdom document to prove each behaviour.
 */
export const PROTO_SHIM_SOURCE = String.raw`(function () {
  "use strict";
  var doc = document;

  function isArray(v) { return Object.prototype.toString.call(v) === "[object Array]"; }
  function words(text) {
    var out = [];
    var parts = String(text == null ? "" : text).split(/\s+/);
    for (var i = 0; i < parts.length; i++) { if (parts[i]) { out.push(parts[i]); } }
    return out;
  }

  // ==== THE LEGACY READ TOLERANCE — THE ONLY PLACE THE OLD PREFIX IS SPELLED ==================
  // The interaction vocabulary is data-nyx-* / window.__nyx*. It used to be data-depot-* /
  // window.__depot* — the machinery was PORTED FROM DEPOT and kept the other tool's name, even on
  // mechanisms invented here (finding 01KYS14G4V1Q5ERRAHB0QVW362). Migration 37 rewrote every
  // stored template, the manual teaches only the new prefix, the warnings name only the new
  // prefix, and NOTHING in nyx emits the old one any more.
  //
  // This block is the belt-and-braces, and it is READ-ONLY: a template that reaches the frame by
  // ANOTHER path (a synthesis in flight from a session started before the rename, an import, an
  // older export) must not render a BLANK page — an unread data-depot-state-case shows every case
  // at once and an unread data-depot-if leaves a spinner permanently visible, which reads
  // as a broken renderer. Every other line below goes through these four helpers and spells the
  // NEW prefix only; the old prefix is tolerated, never written, never documented, never taught.
  var ATTR = "data-nyx-";
  var LEGACY_ATTR = "data-depot-"; // READ ONLY — never emitted
  // The value of attribute <suffix>: the new spelling, the old one only as a fallback.
  function attr(el, suffix) {
    var v = el.getAttribute(ATTR + suffix);
    return v != null ? v : el.getAttribute(LEGACY_ATTR + suffix);
  }
  // Does el carry <suffix> under EITHER spelling? (Guards non-element nodes.)
  function hasAttr(el, suffix) {
    if (!el || !el.hasAttribute) { return false; }
    return el.hasAttribute(ATTR + suffix) || el.hasAttribute(LEGACY_ATTR + suffix);
  }
  // A selector matching BOTH spellings of <suffix>; test is an optional predicate.
  function attrSelector(suffix, test) {
    var t = test == null ? "" : test;
    return "[" + ATTR + suffix + t + "],[" + LEGACY_ATTR + suffix + t + "]";
  }
  // The global window.__nyx<name>, falling back to the pre-rename window.__depot<name>.
  function globalOf(name) {
    var v = window["__nyx" + name];
    return v === undefined ? window["__depot" + name] : v;
  }
  // ==== END OF THE LEGACY READ TOLERANCE ======================================================

  // ---- THE MODEL: named axes, each value carrying its concrete classes ----------------------
  // Read from window.__nyxAxes; the LEGACY window.__nyxStates (a flat list of names) is read as
  // ONE anonymous axis called "state", class-less — exactly what a pre-axes component stores.
  var axes = [];
  var declaredAxes = globalOf("Axes");
  var rawAxes = isArray(declaredAxes) ? declaredAxes : [];
  for (var i = 0; i < rawAxes.length; i++) {
    var raw = rawAxes[i];
    if (!raw || raw.name == null || !isArray(raw.values)) { continue; }
    var values = [];
    for (var j = 0; j < raw.values.length; j++) {
      var v = raw.values[j];
      if (typeof v === "string") { values.push({ name: v, classes: "" }); }
      else if (v && v.name != null) {
        values.push({ name: String(v.name), classes: v.classes == null ? "" : String(v.classes) });
      }
    }
    if (values.length) {
      axes.push({
        name: String(raw.name),
        label: raw.label == null ? String(raw.name) : String(raw.label),
        values: values
      });
    }
  }
  var flatStates = globalOf("States");
  if (!axes.length && isArray(flatStates) && flatStates.length) {
    var legacy = [];
    for (var k = 0; k < flatStates.length; k++) {
      legacy.push({ name: String(flatStates[k]), classes: "" });
    }
    axes.push({ name: "state", label: "state", values: legacy });
  }

  function axisByName(name) {
    for (var n = 0; n < axes.length; n++) { if (axes[n].name === name) { return axes[n]; } }
    return null;
  }
  function hasValue(axis, name) {
    for (var n = 0; n < axis.values.length; n++) { if (axis.values[n].name === name) { return true; } }
    return false;
  }

  // The ACTIVE COMBINATION — one value per axis, always one the axis really declares.
  var rawValues = globalOf("AxisValues");
  var given = rawValues && typeof rawValues === "object" ? rawValues : {};
  var activeState = globalOf("ActiveState");
  var current = {};
  for (var a = 0; a < axes.length; a++) {
    var axis = axes[a];
    var want = given[axis.name] == null ? null : String(given[axis.name]);
    // The legacy single-value input still drives the FIRST axis when no map says otherwise.
    if (want == null && a === 0 && activeState != null) {
      want = String(activeState);
    }
    current[axis.name] = want != null && hasValue(axis, want) ? want : axis.values[0].name;
  }

  function post(message) {
    try {
      window.parent.postMessage(Object.assign({ source: "depot-frame" }, message), "*");
    } catch (e) { /* isolated frame — best effort */ }
  }

  function classesOf(axis) {
    var active = current[axis.name];
    for (var n = 0; n < axis.values.length; n++) {
      if (axis.values[n].name === active) { return words(axis.values[n].classes); }
    }
    return [];
  }

  // WHERE an axis puts its classes: the elements that opted in with data-nyx-axis="<name> …",
  // else the component's ROOT elements — which is what makes a ONE-ELEMENT template enough.
  function targetsFor(name) {
    var marked = doc.querySelectorAll(attrSelector("axis", '~="' + name + '"'));
    if (marked.length) { return marked; }
    var root = doc.getElementById("depot-root");
    return root ? root.children : [];
  }

  // Take back EXACTLY what this axis put there before (never a class the template itself carries —
  // we only ever remove what we recorded adding). It is a pass of its own, run for EVERY axis before
  // any of them adds anything back: two axes whose active values share a class used to have the
  // FIRST one record it and the second record nothing, so flipping the first took the class away
  // from the second too.
  function clearClasses(axis) {
    var targets = targetsFor(axis.name);
    for (var t = 0; t < targets.length; t++) {
      var el = targets[t];
      if (!el || !el.classList) { continue; }
      var applied = el.__nyxAxisClasses || (el.__nyxAxisClasses = {});
      var prev = applied[axis.name] || [];
      for (var p = 0; p < prev.length; p++) { el.classList.remove(prev[p]); }
      applied[axis.name] = [];
    }
  }

  // …then put the ACTIVE value's classes on, recording what was really added.
  function applyClasses(axis) {
    var wanted = classesOf(axis);
    var targets = targetsFor(axis.name);
    for (var t = 0; t < targets.length; t++) {
      var el = targets[t];
      if (!el || !el.classList) { continue; }
      var applied = el.__nyxAxisClasses || (el.__nyxAxisClasses = {});
      var added = [];
      for (var w = 0; w < wanted.length; w++) {
        if (el.classList.contains(wanted[w])) { continue; }
        el.classList.add(wanted[w]);
        added.push(wanted[w]);
      }
      applied[axis.name] = added;
    }
  }

  // ---- STRUCTURE: data-nyx-state-case (first axis) + the generalized data-nyx-if -------
  function parseClauses(body) {
    var out = [];
    var parts = String(body == null ? "" : body).split(/[,;]/);
    for (var p = 0; p < parts.length; p++) {
      var eq = parts[p].indexOf("=");
      if (eq < 0) { continue; }
      var name = parts[p].slice(0, eq).replace(/^\s+|\s+$/g, "");
      var raw = parts[p].slice(eq + 1).split("|");
      var vals = [];
      for (var q = 0; q < raw.length; q++) {
        var val = raw[q].replace(/^\s+|\s+$/g, "");
        if (val) { vals.push(val); }
      }
      if (name && vals.length) { out.push({ axis: name, values: vals }); }
    }
    return out;
  }
  // A clause on an axis the component does NOT declare is IGNORED, never falsified: hiding on an
  // unknown name is how a preview goes blank, and a blank aperçu reads as a broken renderer. The
  // write path warns about the dangling name instead.
  function clauseMatches(clause) {
    if (!axisByName(clause.axis)) { return true; }
    var active = current[clause.axis];
    for (var v = 0; v < clause.values.length; v++) {
      if (clause.values[v] === active) { return true; }
    }
    return false;
  }
  function allMatch(clauses) {
    for (var c = 0; c < clauses.length; c++) { if (!clauseMatches(clauses[c])) { return false; } }
    return true;
  }
  function anyMatch(clauses) {
    for (var c = 0; c < clauses.length; c++) { if (clauseMatches(clauses[c])) { return true; } }
    return false;
  }

  // HIDE FOR REAL, ON ANY KIND OF ELEMENT (finding 01KYSCW89NRM1TSZ0TFXC8HD36). This used to be
  // el.hidden = !show, and hidden is an IDL attribute of HTMLElement ONLY: on an <svg> (an
  // SVGElement) the assignment silently created a JS EXPANDO and reflected NO attribute, so a
  // spinner marked data-nyx-if="loading=on" stayed visible for ever — the reported symptom, and
  // exactly why wrapping the same svg in a <span> "fixed" it. Setting the CONTENT ATTRIBUTE is what
  // the stylesheet actually selects on ([hidden]) and it works on every element, HTML or not; where
  // the .hidden property exists it stays in sync with it, so nothing that reads it regresses.
  //
  // Removing the attribute (rather than pinning display:none inline) is also what keeps hiding
  // REVERSIBLE without bookkeeping: the element gets its own display back — Preflight's block for an
  // svg, its utility for an inline-flex — instead of the shim having to remember and restore a style
  // property it overwrote, and instead of a normal inline declaration that any author !important
  // rule would beat anyway. One mechanism, one owner: the attribute here, the rule in the srcdoc.
  function setHidden(el, hide) {
    if (hide) { el.setAttribute("hidden", ""); }
    else { el.removeAttribute("hidden"); }
  }

  function applyStructure() {
    var primary = axes.length ? current[axes[0].name] : null;
    var els = doc.querySelectorAll(
      attrSelector("state-case") + "," + attrSelector("if") + "," + attrSelector("if-not")
    );
    for (var e = 0; e < els.length; e++) {
      var el = els[e];
      var show = true;
      // The FIRST axis's per-case rendering — the mechanism kept from the previous model.
      var kase = attr(el, "state-case");
      if (kase != null && primary != null && kase !== primary) { show = false; }
      var cond = attr(el, "if");
      if (show && cond != null) { show = allMatch(parseClauses(cond)); }
      var notCond = attr(el, "if-not");
      if (show && notCond != null) { show = !anyMatch(parseClauses(notCond)); }
      setHidden(el, !show);
    }
  }

  // THE APPLIER: classes for every axis, then the structure, then reflect + report.
  function apply() {
    for (var a1 = 0; a1 < axes.length; a1++) { clearClasses(axes[a1]); }
    for (var a2 = 0; a2 < axes.length; a2++) { applyClasses(axes[a2]); }
    applyStructure();
    if (doc.body) {
      // The FIRST axis stays mirrored on data-nyx-state (legacy CSS hook + the flip affordance),
      // and EVERY axis gets its own data-nyx-axis-<name> so a stylesheet or a test can read the
      // whole combination off the body.
      if (axes.length) { doc.body.dataset.nyxState = current[axes[0].name]; }
      else { delete doc.body.dataset.nyxState; }
      for (var a3 = 0; a3 < axes.length; a3++) {
        var safe = axes[a3].name.toLowerCase().replace(/[^a-z0-9-]/g, "-");
        doc.body.setAttribute("data-nyx-axis-" + safe, current[axes[a3].name]);
      }
    }
    var chips = doc.querySelectorAll("#depot-chip-bar .depot-chip");
    for (var ch = 0; ch < chips.length; ch++) {
      var chipAxis = chips[ch].getAttribute("data-axis") || (axes.length ? axes[0].name : "");
      chips[ch].setAttribute(
        "aria-pressed",
        chips[ch].getAttribute("data-state") === current[chipAxis] ? "true" : "false"
      );
    }
    var echo = {};
    for (var a4 = 0; a4 < axes.length; a4++) { echo[axes[a4].name] = current[axes[a4].name]; }
    post({ type: "axes", values: echo });
    post({ type: "state", state: axes.length ? current[axes[0].name] : null });
  }

  // Set ONE axis (the others keep their value — that is the whole point of orthogonal axes).
  function setAxis(name, value) {
    var axis = axisByName(name);
    if (!axis || !hasValue(axis, value)) { return; }
    if (current[name] === value) { return; }
    current[name] = value;
    apply();
  }
  // The LEGACY entry point: flip the FIRST axis (window.__nyxActiveState, data-nyx-state).
  function applyState(name) {
    if (!axes.length) { return; }
    setAxis(axes[0].name, name == null ? null : String(name));
  }

  // Build the chip-bar: ONE ROW PER AXIS. A single anonymous "state" axis renders exactly the flat
  // bar it always did (no row label), so the legacy look is untouched.
  function buildChipBar() {
    var bar = doc.getElementById("depot-chip-bar");
    if (!bar) { return; }
    bar.textContent = "";
    for (var a5 = 0; a5 < axes.length; a5++) {
      var ax = axes[a5];
      var row = doc.createElement("div");
      row.className = "depot-chip-row";
      row.setAttribute("data-axis", ax.name);
      if (axes.length > 1 || ax.name !== "state") {
        var lbl = doc.createElement("span");
        lbl.className = "depot-chip-axis";
        lbl.textContent = ax.label;
        row.appendChild(lbl);
      }
      for (var v2 = 0; v2 < ax.values.length; v2++) {
        var name = ax.values[v2].name;
        var chip = doc.createElement("button");
        chip.type = "button";
        chip.className = "depot-chip";
        chip.setAttribute("data-axis", ax.name);
        chip.setAttribute("data-state", name);
        chip.setAttribute("aria-pressed", name === current[ax.name] ? "true" : "false");
        chip.textContent = name;
        (function (axisName, valueName) {
          chip.addEventListener("click", function () { setAxis(axisName, valueName); });
        })(ax.name, name);
        row.appendChild(chip);
      }
      bar.appendChild(row);
    }
  }

  // ==== THE NAMED TREE, READ BACK OFF THE MARKUP ==============================================
  // THE RENDER IS THE COMPOSITION (finding 01KYW32N1BVDVTGS0WPMG2PJW2). Clicking a component in the
  // page opens ITS params, and a pinned comment anchors on the element clicked — both need one
  // answer: WHICH NODE of the named tree is this element part of?
  //
  // Ported from depot's feedback shim (src/web/api/prototype-shim.ts) — the capture layer, the
  // hover ring, the elementFromPoint resolution, the click posted to the parent with its
  // coordinates. ONE THING IS DELIBERATELY NOT PORTED: depot's computeSelector, which walks up the
  // DOM building a CSS selector until it matches exactly one element. Here the TREE is the contract:
  // a comment is stored on node_key, RE-ANCHORED across rounds by node_key, and read by the coder
  // seam under the same Root/Header/Title paths. A selector would be a second identity for the same
  // node — one that dies the day a class changes, which is what a design round does for a living.
  //
  // The node names reach the markup as inert HTML COMMENTS bracketing each node's markup, asked for
  // by the host (see composition-markers.ts). They are read ONCE here into an element -> node_key
  // map held on an expando, so nothing is written into the document the page could see.
  var MARK_OPEN = "nyx-node:";
  var MARK_CLOSE = "/nyx-node:";
  var pickMode = "off";
  var byNode = {};
  var hoverEl = null;
  var pinnedKeys = [];
  var captureLayer = null;

  function decodeKey(raw) {
    try { return decodeURIComponent(raw); } catch (e) { return raw; }
  }

  // Walk one container's CHILD NODES: a marker comment opens (or closes) a node's region, and every
  // element met while a region is open belongs to it — the deepest one, since the walk descends with
  // the region it is in and any nested marker refines it.
  //
  // TWO ANSWERS ARE BUILT, and they are not the same question. __nyxNode is set on EVERY element so
  // a click anywhere resolves to a node. byNode keeps only the REGION ROOTS — the elements whose
  // key differs from their container's, i.e. the top of the node's own markup. That is what a
  // RING is drawn on: highlighting every descendant too outlines each rect of an icon's <svg>
  // separately (seen in the running app), which reads as a rendering fault rather than a selection.
  function indexInto(container, key) {
    var stack = [key];
    var child = container.firstChild;
    while (child) {
      if (child.nodeType === 8) {
        var body = String(child.nodeValue == null ? "" : child.nodeValue);
        if (body.indexOf(MARK_CLOSE) === 0) {
          if (stack.length > 1) { stack.pop(); }
        } else if (body.indexOf(MARK_OPEN) === 0) {
          stack.push(decodeKey(body.slice(MARK_OPEN.length)));
        }
      } else if (child.nodeType === 1) {
        var current = stack[stack.length - 1];
        child.__nyxNode = current;
        if (current != null && current !== key) {
          if (!byNode[current]) { byNode[current] = []; }
          byNode[current].push(child);
        }
        indexInto(child, current);
      }
      child = child.nextSibling;
    }
  }

  function indexNodes() {
    byNode = {};
    var root = doc.getElementById("depot-root");
    if (!root) { return; }
    // The ROOT node owns #depot-root itself: it has no parent to carry its bracket, so the host
    // names it directly, and that element IS its region root. An unmarked render (no host asked for
    // the markers) indexes to null keys and every gesture below simply resolves to nothing — the
    // page still renders and still navigates.
    var declaredRoot = globalOf("RootNode");
    var rootKey = declaredRoot == null ? null : String(declaredRoot);
    if (rootKey != null) {
      root.__nyxNode = rootKey;
      byNode[rootKey] = [root];
    }
    indexInto(root, rootKey);
  }

  // The node an element belongs to: itself, or the nearest ancestor that carries one.
  function nodeKeyOf(el) {
    var node = el;
    while (node && node !== doc.body) {
      if (node.__nyxNode != null) { return node.__nyxNode; }
      node = node.parentNode;
    }
    return null;
  }

  // Put className on exactly the elements of "keys", and on nothing else. Idempotent: the previous
  // set is cleared first, so a selection change never leaves a stale ring behind.
  function markClass(keys, className) {
    var stale = doc.querySelectorAll("." + className);
    for (var s = 0; s < stale.length; s++) { stale[s].classList.remove(className); }
    for (var k = 0; k < keys.length; k++) {
      var els = byNode[keys[k]] || [];
      for (var e2 = 0; e2 < els.length; e2++) { els[e2].classList.add(className); }
    }
  }

  // PIN MODE resolves its target through a full-cover transparent layer rather than the native click
  // target (depot PRD 0045/S1, kept verbatim in spirit): a disabled control never dispatches a click
  // and makes the event target climb to an enabled ancestor, while elementFromPoint reports what is
  // really under the cursor. The layer also SWALLOWS the click, so the prototype's own nav and flip
  // affordances never fire while one is pinning a comment.
  function elementUnder(x, y) {
    if (!captureLayer) { return doc.elementFromPoint(x, y); }
    var prev = captureLayer.style.pointerEvents;
    captureLayer.style.pointerEvents = "none";
    var el = doc.elementFromPoint(x, y);
    captureLayer.style.pointerEvents = prev || "auto";
    return el;
  }

  function setHover(el) {
    if (hoverEl === el) { return; }
    if (hoverEl && hoverEl.classList) { hoverEl.classList.remove("nyx-pick-hover"); }
    hoverEl = el;
    if (hoverEl && hoverEl.classList) { hoverEl.classList.add("nyx-pick-hover"); }
  }

  function mountCapture() {
    if (captureLayer) { return; }
    captureLayer = doc.createElement("div");
    captureLayer.className = "nyx-pick-capture";
    captureLayer.setAttribute("aria-hidden", "true");
    captureLayer.addEventListener("pointermove", function (ev) {
      setHover(elementUnder(ev.clientX, ev.clientY));
    });
    captureLayer.addEventListener("click", function (ev) {
      ev.preventDefault();
      ev.stopPropagation();
      var target = elementUnder(ev.clientX, ev.clientY);
      if (!target) { return; }
      // The coordinates travel with the node so the host can anchor its popup where the click
      // landed; they are FRAME-relative, which is what the host's own iframe box is measured in.
      post({ type: "pin", nodeKey: nodeKeyOf(target), x: ev.clientX, y: ev.clientY });
    }, true);
    doc.body.appendChild(captureLayer);
  }

  function unmountCapture() {
    if (captureLayer && captureLayer.parentNode) {
      captureLayer.parentNode.removeChild(captureLayer);
    }
    captureLayer = null;
    setHover(null);
  }

  function setPickMode(mode) {
    pickMode = mode === "pin" ? "pin" : (mode === "select" ? "select" : "off");
    if (pickMode === "pin") { mountCapture(); } else { unmountCapture(); }
    // The "already commented" marks are a PIN-MODE affordance (depot PRD 0045/S2): plain reading of
    // the page stays clean, and the rings only appear while one is placing a comment.
    markClass(pickMode === "pin" ? pinnedKeys : [], "nyx-pick-pinned");
  }

  // The HOST talks to the frame — the only inbound channel, and it carries nothing but a mode and a
  // set of node keys. Messages that do not name the host are ignored.
  window.addEventListener("message", function (ev) {
    var data = ev.data || {};
    if (!data || data.source !== "depot-host") { return; }
    if (data.type === "pick-mode") { setPickMode(data.mode); return; }
    if (data.type === "highlight-node") {
      markClass(data.nodeKey == null ? [] : [String(data.nodeKey)], "nyx-pick-selected");
      return;
    }
    if (data.type === "pinned-nodes") {
      pinnedKeys = isArray(data.nodeKeys) ? data.nodeKeys : [];
      markClass(pickMode === "pin" ? pinnedKeys : [], "nyx-pick-pinned");
      return;
    }
  });

  // Delegate clicks: navigation (data-nyx-page), first-axis flip (data-nyx-state) and the
  // any-axis flip (data-nyx-set="size=lg") — and, when nothing declares an interaction and the host
  // asked for SELECT mode, the node the click landed in (the params gesture).
  doc.addEventListener("click", function (ev) {
    var target = ev.target;
    while (target && target !== doc.body) {
      if (hasAttr(target, "page")) {
        ev.preventDefault();
        post({ type: "navigate", page: attr(target, "page") });
        return;
      }
      if (hasAttr(target, "set")) {
        ev.preventDefault();
        var clauses = parseClauses(attr(target, "set"));
        for (var c2 = 0; c2 < clauses.length; c2++) {
          setAxis(clauses[c2].axis, clauses[c2].values[0]);
        }
        return;
      }
      if (hasAttr(target, "state")) {
        ev.preventDefault();
        applyState(attr(target, "state"));
        return;
      }
      target = target.parentNode;
    }
    // The page declared nothing here — so the click DESIGNATES. The declared affordances keep
    // priority: a nav link navigates, it does not open a params form.
    if (pickMode === "select") {
      var picked = nodeKeyOf(ev.target);
      if (picked != null) { post({ type: "select", nodeKey: picked }); }
    }
  });

  // Report content height so a live-preview host can auto-size the frame.
  function reportHeight() {
    var h = Math.max(doc.documentElement.scrollHeight, doc.body ? doc.body.scrollHeight : 0);
    post({ type: "resize", height: h });
  }
  if (typeof ResizeObserver !== "undefined" && doc.body) {
    try { new ResizeObserver(reportHeight).observe(doc.body); } catch (e) { /* ignore */ }
  }

  buildChipBar();
  indexNodes();
  apply();
  reportHeight();
  post({ type: "ready" });
})();`;
