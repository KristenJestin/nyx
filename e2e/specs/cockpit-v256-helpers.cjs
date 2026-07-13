/* eslint-disable */
// Shared helpers for the v2.5.6 COCKPIT-REBRANCH gate e2e (PRD 01KXBNX508EYHSQ1CW7RWYKV95,
// task 01KXBNX51EPQ69MVKYG4W960HE). These specs drive the REAL Electron app through the
// v2.5.6 shell (NOT the removed v2.2 cockpit screen) and assert every rebranched cockpit
// feature: the close-warning chrome, the project/workspace kebab CRUD, per-terminal stats,
// inline rename + per-row close, the loose ("libres") terminals band, and drag-reorder of
// terminals AND projects with persistence across a real relaunch — plus the v2.2 / v2.5.5
// non-regression.
//
// WHY NEW SPECS (not a port of cockpit-0*): PDR-0025 replaced the v2.2 cockpit screen with the
// multi-page shell (Terminals / Work / Knowledge) and UNMOUNTED `<TerminalManager>`, so the old
// `window.__nyx` control seam it published is GONE in v2.5.6. The live seams on the current shell
// are:
//   - `window.__nyxWork`      (base-layout.tsx, gated VITE_NYX_E2E=1) — { createProject, listProjects }
//   - `window.__nyxDeck[id]()` / `window.__nyxDeckInput[id](s)` (terminal-deck.tsx) — per-terminal
//     read/input, keyed by record id, published by the SAME <TerminalDeck> the Terminals page reuses
//   - the DOM of the real shell: rail project tiles, the Terminals page sidebar tree, deck panes
//     (`[data-terminal-id]`) and sortable rows (`[data-terminal-row]`).
// Everything else is driven through those seams + real aria-labelled controls, exactly like the
// other specs. The MCP loopback + git-repo + handoff primitives are REUSED verbatim from
// cockpit-helpers.cjs (they never touched `__nyx`, so they are shell-agnostic).

const fs = require("fs");
const os = require("os");
const path = require("path");

// Reuse the shell-agnostic primitives from the v2.2 cockpit helpers (MCP loopback, git repo,
// agent-session seeding) and restore-helpers (handoff). These do NOT depend on `window.__nyx`.
const cockpit = require("./cockpit-helpers.cjs");
const restore = require("./restore-helpers.cjs");

// The window IPC channel the main process sends the renderer when the OS asks to close (mirrors
// WINDOW_CHANNELS.closeRequested in apps/desktop/src/shared/ipc.ts). Sending it directly from the
// main process is the EXACT payload the frameless window's `close` withhold emits, so the
// renderer-side close-guard chain (onCloseRequested → agent_close_warnings → dialog) runs verbatim.
const CLOSE_REQUESTED_CHANNEL = "nyx:window/close-requested";

// -------------------------------------------------------------------------------------------
// App / seam readiness
// -------------------------------------------------------------------------------------------

// Wait for the v2.5.6 navigation shell to mount AND publish the `window.__nyxWork` seam (gated
// behind VITE_NYX_E2E=1). Anti-false-green: a build that never mounts the shell / never publishes
// the seam FAILS here rather than silently passing.
async function waitForWorkShell() {
  await browser.waitUntil(
    async () =>
      browser.execute(function () {
        var w = window.__nyxWork;
        return !!(
          w &&
          typeof w.createProject === "function" &&
          typeof w.listProjects === "function"
        );
      }),
    {
      timeout: 30000,
      timeoutMsg: "window.__nyxWork seam never appeared (v2.5.6 shell / e2e build did not start)",
    },
  );
  // Let the rail + first paint settle.
  await browser.pause(400);
}

// -------------------------------------------------------------------------------------------
// Filesystem fixtures (real, existing folders the app can root a project/workspace at)
// -------------------------------------------------------------------------------------------

// Create a real, empty directory under the OS temp root and return its realpath (the backend
// stores normalized/realpath paths, so we resolve up front to compare/assert against the shell).
function makeTempDir(prefix) {
  const dir = fs.mkdtempSync(path.join(fs.realpathSync(os.tmpdir()), (prefix || "nyx-v256-") + ""));
  return fs.realpathSync(dir);
}

// -------------------------------------------------------------------------------------------
// `window.__nyxWork` seam — seed projects without the native folder picker
// -------------------------------------------------------------------------------------------

// Create a project (+ its root workspace) through the inert seam — the SAME `create_project` the
// rail's "+ Add project" flow drives, minus the OS folder dialog. Returns the created
// `{ project, root }` (ProjectWithRoot).
async function seedProject(name, rootPath, rootName) {
  return browser.execute(
    function (n, r, rn) {
      return window.__nyxWork.createProject(n, r, rn || undefined);
    },
    name,
    rootPath,
    rootName || null,
  );
}

// The live project trees the shell renders (rail order + each project's workspaces).
async function listProjectTrees() {
  return browser.execute(function () {
    return window.__nyxWork ? window.__nyxWork.listProjects() : [];
  });
}

// -------------------------------------------------------------------------------------------
// aria-label / DOM driving primitives
// -------------------------------------------------------------------------------------------

function ariaSel(label) {
  return '[aria-label="' + label + '"]';
}

async function elByAria(label) {
  return $(ariaSel(label));
}

async function waitAria(label, timeoutMs) {
  const el = await elByAria(label);
  await el.waitForExist({ timeout: timeoutMs || 15000, timeoutMsg: "missing " + ariaSel(label) });
  return el;
}

async function clickAria(label, timeoutMs) {
  const el = await waitAria(label, timeoutMs);
  // Opacity-0 hover actions are still hit-testable (no pointer-events:none), so WebDriver's
  // element-click works without simulating :hover; it also scrolls the element into view.
  await el.click();
  return el;
}

async function existsAria(label) {
  return browser.execute(function (sel) {
    return !!document.querySelector(sel);
  }, ariaSel(label));
}

// Wait until an aria-labelled element is GONE (e.g. a deleted project tile / closed terminal row).
async function waitAriaGone(label, timeoutMs) {
  await browser.waitUntil(async () => !(await existsAria(label)), {
    timeout: timeoutMs || 15000,
    timeoutMsg: ariaSel(label) + " never disappeared",
  });
}

// Click a button by its visible text (dialog confirm buttons: "Save", "Delete", "Add workspace",
// "Remove workspace", "Keep open", "Close anyway"). Scoped to the topmost dialog when present.
async function clickButtonByText(text, timeoutMs) {
  await browser.waitUntil(
    async () =>
      browser.execute(function (t) {
        var btns = Array.from(document.querySelectorAll("button"));
        var btn = btns.find(function (b) {
          return (b.textContent || "").trim() === t && b.offsetParent !== null;
        });
        if (btn) {
          btn.click();
          return true;
        }
        return false;
      }, text),
    { timeout: timeoutMs || 15000, timeoutMsg: 'button "' + text + '" never became clickable' },
  );
}

// True when a visible element's text contains `needle` anywhere on screen (dialog copy, toasts…).
async function bodyIncludes(needle) {
  return browser.execute(function (n) {
    return (document.body.innerText || "").indexOf(n) !== -1;
  }, needle);
}

async function waitForText(needle, timeoutMs) {
  await browser.waitUntil(async () => bodyIncludes(needle), {
    timeout: timeoutMs || 15000,
    timeoutMsg: 'text "' + needle + '" never appeared',
  });
}

// True when the "Loose terminals" band SECTION is rendered. The band's micro-label is CSS
// `text-transform: uppercase` (design-system `<MicroLabel>`), so `document.body.innerText` reads
// "LOOSE TERMINALS" and a case-sensitive `bodyIncludes("Loose terminals")` would miss it even
// though the band IS present. We match on a LEAF node's raw `textContent` (which is NOT rewritten
// by `text-transform`), case-insensitively — an honest "is the band on screen" check that still
// fails if the section is genuinely missing.
async function looseBandRendered() {
  return browser.execute(function () {
    return Array.from(document.querySelectorAll("*")).some(function (el) {
      return el.children.length === 0 && /loose terminals/i.test((el.textContent || "").trim());
    });
  });
}

// -------------------------------------------------------------------------------------------
// Rail (projects) — enter, kebab / context menu, order
// -------------------------------------------------------------------------------------------

// Enter a project (left-click its rail tile). The Terminals page then renders that project's
// workspaces + runtime.
async function enterProject(name) {
  await clickAria("Project " + name);
  await browser.pause(300);
}

// Open the per-project actions menu by RIGHT-CLICKING the tile (v2.5.6 finding #24: the ⋮ kebab
// trigger was removed, so right-click is the SOLE entry point to `ProjectRailMenuItems`).
async function openProjectContextMenu(name) {
  const tile = await waitAria("Project " + name);
  await tile.click({ button: "right" });
  await browser.pause(200);
}

// The rail's project tiles, in DOM (rail) order, as their display names. SCOPED to the base-layout
// RAIL (the FIRST `<aside>` in the grid) so it never picks up the Terminals page's SidebarShell
// (also an `<aside>`) whose header carries a `Project settings` gear (aria-label "Project settings")
// — that gear would otherwise surface as a spurious "settings" tile. Excludes the per-tile kebab
// ("actions for") and the settings modal ("settings for").
async function railProjectOrder() {
  return browser.execute(function () {
    var rail = document.querySelector("aside");
    if (!rail) return [];
    var tiles = Array.from(rail.querySelectorAll('[aria-label^="Project "]')).filter(function (el) {
      var l = el.getAttribute("aria-label") || "";
      return l.indexOf("actions for") === -1 && l.indexOf("settings for") === -1;
    });
    return tiles.map(function (el) {
      return (el.getAttribute("aria-label") || "").replace(/^Project /, "");
    });
  });
}

// Drive the REAL project reorder (the SAME `reorderProjects` the rail drag commits → `projects_reorder`
// → persisted `order_index`) through the inert `__nyxWork` seam, since the WebDriver Actions pointer
// API is unusable in this Electron harness so a simulated drag never fires the dnd-kit sensor. The
// caller passes the FULL new project-id order and asserts the rail DOM order actually changed +
// survives a relaunch — honest persistence, not a no-op.
async function reorderProjectsSeam(ids) {
  return browser.execute(function (idOrder) {
    if (window.__nyxWork && typeof window.__nyxWork.reorderProjects === "function") {
      window.__nyxWork.reorderProjects(idOrder);
      return true;
    }
    return false;
  }, ids);
}

// Drive the REAL within-group terminal reorder (the SAME `reorderTerminals` the row drag commits →
// `reorder` IPC → persisted `order_index`) through the `__nyxReorderTerminals` seam (published by
// the Terminals page under VITE_NYX_E2E). `group` is the workspace id (or null for the loose band);
// `ids` is that group's terminal record ids in the NEW order.
async function reorderTerminalsSeam(group, ids) {
  return browser.execute(
    function (g, idOrder) {
      if (typeof window.__nyxReorderTerminals === "function") {
        window.__nyxReorderTerminals(g, idOrder);
        return true;
      }
      return false;
    },
    group,
    ids,
  );
}

// -------------------------------------------------------------------------------------------
// Terminals page — workspaces, terminals, deck panes
// -------------------------------------------------------------------------------------------

// The record ids of every terminal pane the deck currently mounts (`[data-terminal-id]`), which is
// every terminal in the entered project + the loose band. Used to detect a newly-created terminal.
async function deckTerminalIds() {
  return browser.execute(function () {
    return Array.from(document.querySelectorAll("[data-terminal-id]")).map(function (el) {
      return el.getAttribute("data-terminal-id");
    });
  });
}

// Wait until a NEW deck pane id appears relative to `beforeIds`, returning it. The deck mounts a
// pane per terminal, so a created terminal (workspace "+" or loose "+") surfaces here once the
// runtime snapshot re-pulls.
async function waitForNewTerminalId(beforeIds, timeoutMs) {
  let fresh = null;
  await browser.waitUntil(
    async () => {
      const now = await deckTerminalIds();
      fresh = now.find((id) => beforeIds.indexOf(id) === -1) || null;
      return fresh != null;
    },
    { timeout: timeoutMs || 15000, timeoutMsg: "a new terminal pane never appeared in the deck" },
  );
  // Let the PTY spawn + print its first prompt.
  await browser.pause(700);
  return fresh;
}

// Open a NEW terminal in the entered project's FIRST workspace (the section "+"), returning its
// record id. Requires a workspace accordion to be present.
async function newWorkspaceTerminal() {
  const before = await deckTerminalIds();
  await clickAria("New terminal");
  return waitForNewTerminalId(before);
}

// Read a terminal's live xterm buffer through the deck read seam (works for hidden panes too).
async function readDeck(id) {
  return browser.execute(function (i) {
    var deck = window.__nyxDeck || {};
    return typeof deck[i] === "function" ? deck[i]() : null;
  }, id);
}

// Type into a terminal's PTY through the deck input seam (keystrokes → the shell echoes/executes).
async function typeDeck(id, data) {
  return browser.execute(
    function (i, d) {
      var seam = window.__nyxDeckInput || {};
      if (typeof seam[i] === "function") {
        seam[i](d);
        return true;
      }
      return false;
    },
    id,
    data,
  );
}

// Poll a terminal's buffer until `needle` appears (a typed marker echoed back), or throw.
async function waitForDeck(id, needle, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 15000);
  let last = null;
  while (Date.now() < deadline) {
    last = await readDeck(id);
    if (last != null && last.indexOf(needle) !== -1) return last;
    await browser.pause(200);
  }
  throw new Error("timed out waiting for '" + needle + "' in terminal " + id + ". Buffer:\n" + last);
}

// The display names of the terminal ROWS currently in the tree/loose band, read from each row's
// "Rename terminal <name>" control (present whether the row is hovered or not — opacity only).
async function terminalRowNames() {
  return browser.execute(function () {
    return Array.from(document.querySelectorAll('[aria-label^="Rename terminal "]'))
      .map(function (el) {
        return (el.getAttribute("aria-label") || "").replace(/^Rename terminal /, "");
      })
      // De-dupe (the editor input shares the button's aria-label while editing).
      .filter(function (v, i, a) {
        return a.indexOf(v) === i;
      });
  });
}

// Inline-rename the terminal row whose current display name is `currentName` to `newName`:
// click the row's pencil (opens the inline <Input>, same aria-label), overwrite, commit with Enter.
//
// The editor is a CONTROLLED Base UI <Input> (value bound to React state, pre-seeded with the
// current auto/label name). WDIO's `setValue` calls the WebDriver Clear command first, which sets
// the native `.value` — but React's controlled value-tracker reverts that write and no `input`
// event reaches onChange, so the pre-seeded draft survives and the rename commits the OLD name
// (observed: the row kept its auto path). So we drive the field with REAL key events instead:
// focus it, select-all (Ctrl+A) over the pre-seeded text, type the replacement (each keystroke
// fires onChange), then Enter to commit. This exercises the exact product path the unit test covers
// (pencil → type → Enter → rename callback), just through genuine keystrokes.
async function renameTerminalRow(currentName, newName) {
  await clickAria("Rename terminal " + currentName);
  await browser.pause(250);
  // While editing, the actions cluster is hidden and the <input> carries the SAME aria-label, so
  // scope to the input tag to resolve the field (never a stale pencil button).
  const sel = 'input[aria-label="Rename terminal ' + currentName + '"]';
  const input = await $(sel);
  await input.waitForExist({ timeout: 5000, timeoutMsg: "inline rename input never appeared" });
  // Focus the editor DIRECTLY (WDIO's element.click can leave this Base UI controlled input
  // unfocused — it stopPropagation's pointerdown/click — so the following keystrokes would go
  // nowhere and the rename would never commit). Then drive it with REAL keystrokes: select-all over
  // the pre-seeded auto-name, type the replacement (a real `input` per key → onChange updates the
  // draft — the native/synthetic value-set path does NOT fire React onChange here), then Enter
  // (onKeyDown → commitRename → the rename IPC). This is the exact user gesture; the wait below
  // fails honestly if the rename does not persist AND repaint the row.
  await browser.execute(
    function (s) {
      var el = document.querySelector(s);
      if (el) el.focus();
    },
    sel,
  );
  await browser.pause(80);
  await browser.keys(["Control", "a"]); // select the pre-seeded draft
  await browser.keys(newName); // real keystrokes → onChange per key → draft = newName
  await browser.pause(120);
  await browser.keys("Enter"); // onKeyDown Enter → commitRename → rename IPC → terminals://changed
  // The tree row renders its name from the workspace runtime snapshot, which re-pulls on the
  // `terminals://changed` the rename now broadcasts — wait for the row to actually repaint to the
  // manual label (honest: times out if the rename did not take effect).
  await browser.waitUntil(async () => (await terminalRowNames()).indexOf(newName) !== -1, {
    timeout: 8000,
    timeoutMsg: "the inline rename never repainted the row to '" + newName + "'",
  });
  await browser.pause(150);
}

// Close a terminal row by its display name (the per-row "×"). Verify the caller-side disappearance.
async function closeTerminalRow(name) {
  await clickAria("Close terminal " + name);
  await browser.pause(400);
}

// -------------------------------------------------------------------------------------------
// Per-terminal CPU/RAM stats tooltip (non-inline, hover-only)
// -------------------------------------------------------------------------------------------

// Hover a terminal row's SELECT button (the label area, which the Tooltip wraps) and wait for the
// `CPU … · RAM …` tooltip to appear, returning its text. Retries the hover in a loop because the
// tooltip wrapper only exists once the first `terminal://stats` reading arrives (~1.5s cadence),
// and Base UI opens the tooltip on hover after a short delay. `name` must be the row's displayed
// name (rename the terminal first so the XPath text match is unambiguous).
async function waitForStatsTooltip(name, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 30000);
  const selectBtn = await $('//button[.//span[normalize-space()="' + name + '"]]');
  let lastSeen = null;
  while (Date.now() < deadline) {
    if (await selectBtn.isExisting()) {
      // Park the pointer AWAY first, then move ONTO the trigger, so a fresh `pointerenter` fires —
      // Base UI's Tooltip opens on hover via floating-ui `useHover`, and re-issuing `moveTo` while
      // the pointer already sits on the trigger does not re-trigger the enter (so the tooltip would
      // never open). The pause exceeds the 350ms open delay.
      await browser
        .action("pointer", { parameters: { pointerType: "mouse" } })
        .move({ x: 2, y: 2 })
        .pause(30)
        .perform();
      await selectBtn.moveTo();
      await browser.pause(500);
      const text = await browser.execute(function () {
        // AT REST the row text is JUST the name (no CPU/RAM anywhere on screen — the non-inline
        // constraint), so ANY element carrying the `CPU … · RAM …` reading is the portaled tooltip
        // popup. Pick the SHORTEST such textContent = the innermost carrier (the Base UI popup DIV,
        // which also holds an EMPTY arrow element, so it is NOT a childless leaf — the old
        // `children.length === 0` filter wrongly rejected it).
        var re = /CPU\s.+·\sRAM\s.+/;
        var best = null;
        Array.from(document.querySelectorAll("*")).forEach(function (el) {
          var t = (el.textContent || "").trim();
          if (re.test(t) && (best === null || t.length < best.length)) best = t;
        });
        return best;
      });
      if (text) return text;
      lastSeen = text;
    }
    await browser.pause(400);
  }
  throw new Error(
    "stats tooltip (CPU … · RAM …) never appeared for terminal '" + name + "' (last: " + lastSeen + ")",
  );
}

// The trimmed textContent of the terminal ROW (`[data-terminal-row]`) whose displayed name is
// `name`, located via its "Rename terminal <name>" control. It backs the NON-INLINE negative
// assertion: the CPU/RAM reading lives in a Base UI Tooltip that is PORTALED to document.body, so
// the row's own text is JUST the name — a regression to INLINE stats (the v1 behaviour the PRD
// forbids) would surface `CPU … · RAM …` in this text and fail the gate. Returns null when no such
// row exists (so the caller can assert the row is actually present).
async function terminalRowText(name) {
  return browser.execute(function (n) {
    var rows = Array.from(document.querySelectorAll("[data-terminal-row]"));
    var row = rows.find(function (r) {
      return !!r.querySelector('[aria-label="Rename terminal ' + n + '"]');
    });
    return row ? (row.textContent || "").trim() : null;
  }, name);
}

// -------------------------------------------------------------------------------------------
// Drag-reorder (dnd-kit pointer sensor, 5px activation)
// -------------------------------------------------------------------------------------------

// Drive a real pointer DRAG from `sourceEl` onto `targetEl`: press, nudge past the 5px activation
// distance, glide over the target with intermediate moves (dnd-kit collision needs the pointer
// inside the target droppable), then release. This is the genuine gesture the reorder relies on.
async function dragOnto(sourceEl, targetEl) {
  await sourceEl.scrollIntoView();
  await browser
    .action("pointer", { parameters: { pointerType: "mouse" } })
    .move({ origin: sourceEl, x: 0, y: 0 })
    .down({ button: 0 })
    .pause(60)
    // Exceed the 5px activation threshold to start the drag.
    .move({ origin: sourceEl, x: 0, y: -10, duration: 60 })
    .pause(60)
    // Two-step glide onto the target so dnd-kit registers the hover + reorder.
    .move({ origin: targetEl, x: 0, y: 0, duration: 120 })
    .pause(60)
    .move({ origin: targetEl, x: 0, y: 4, duration: 60 })
    .pause(80)
    .up({ button: 0 })
    .perform();
  await browser.pause(500);
}

// Drive a real pointer drag with SYNTHETIC `PointerEvent`s dispatched IN THE RENDERER (via
// `browser.execute`), from viewport point `start` to `end`. This bypasses the WebDriver Actions
// pointer API — which is unusable in this Electron/CDP harness (`Browser.getWindowForTarget`
// unsupported), so `browser.action("pointer")` never reaches the page — and instead fires the exact
// DOM events dnd-kit's `PointerSensor` binds: a `pointerdown` on the element under `start` (bubbles
// to the sortable's listener), a move past the 5px activation threshold, a glide to `end`, then a
// `pointerup`. It is split into phases with pauses so dnd-kit's rAF-driven collision detection runs
// between the threshold-cross, the hover-over-target, and the drop. Returns the pointerdown target's
// tag (or null if nothing was under `start`) so the caller can sanity-check the press landed.
async function synthPointerDrag(start, end) {
  // Phase 1 — press at `start` (on the real element there so it bubbles to the sortable) and cross
  // the 5px activation distance so the sensor promotes the gesture to a drag.
  const landed = await browser.execute(
    function (sx, sy) {
      function pe(type, x, y, buttons, button) {
        return new PointerEvent(type, {
          bubbles: true,
          cancelable: true,
          composed: true,
          pointerId: 1,
          pointerType: "mouse",
          isPrimary: true,
          button: button,
          buttons: buttons,
          clientX: x,
          clientY: y,
        });
      }
      var target = document.elementFromPoint(sx, sy) || document.body;
      target.dispatchEvent(pe("pointerdown", sx, sy, 1, 0));
      // Move UP past the 5px threshold (still near the source) to activate the sensor.
      document.dispatchEvent(pe("pointermove", sx, sy - 12, 1, -1));
      return target ? target.tagName : null;
    },
    start.x,
    start.y,
  );
  await browser.pause(100);
  // Phase 2 — glide onto the target droppable so dnd-kit registers the hover + computes the reorder.
  await browser.execute(
    function (sx, sy, ex, ey) {
      function pe(type, x, y) {
        return new PointerEvent(type, {
          bubbles: true,
          cancelable: true,
          composed: true,
          pointerId: 1,
          pointerType: "mouse",
          isPrimary: true,
          button: -1,
          buttons: 1,
          clientX: x,
          clientY: y,
        });
      }
      document.dispatchEvent(pe("pointermove", (sx + ex) / 2, (sy + ey) / 2));
      document.dispatchEvent(pe("pointermove", ex, ey));
      document.dispatchEvent(pe("pointermove", ex, ey + 3));
    },
    start.x,
    start.y,
    end.x,
    end.y,
  );
  await browser.pause(100);
  // Phase 3 — release over the target (commits the drop → onDragEnd → reorder callback).
  await browser.execute(
    function (ex, ey) {
      var ev = new PointerEvent("pointerup", {
        bubbles: true,
        cancelable: true,
        composed: true,
        pointerId: 1,
        pointerType: "mouse",
        isPrimary: true,
        button: 0,
        buttons: 0,
        clientX: ex,
        clientY: ey,
      });
      document.dispatchEvent(ev);
    },
    end.x,
    end.y,
  );
  await browser.pause(500);
  return landed;
}

// The viewport-center of the Nth `[data-terminal-row]` (or null when absent).
async function terminalRowCenter(index) {
  return browser.execute(function (i) {
    var rows = document.querySelectorAll("[data-terminal-row]");
    var el = rows[i];
    if (!el) return null;
    var r = el.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  }, index);
}

// The viewport-center of a rail project tile (by its `Project <name>` aria-label), or null.
async function projectTileCenter(name) {
  return browser.execute(function (n) {
    var el = document.querySelector('[aria-label="Project ' + n + '"]');
    if (!el) return null;
    var r = el.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  }, name);
}

// REAL drag of terminal row `from` onto row `to` via synthetic pointer events (see
// `synthPointerDrag`). Throws if either row is missing. Returns the pointerdown target tag.
async function synthDragTerminalRow(from, to) {
  const start = await terminalRowCenter(from);
  const end = await terminalRowCenter(to);
  if (!start || !end) throw new Error("synthDragTerminalRow: missing row " + from + " or " + to);
  return synthPointerDrag(start, end);
}

// REAL drag of project tile `nameFrom` onto `nameTo` via synthetic pointer events.
async function synthDragProject(nameFrom, nameTo) {
  const start = await projectTileCenter(nameFrom);
  const end = await projectTileCenter(nameTo);
  if (!start || !end) throw new Error("synthDragProject: missing tile " + nameFrom + " or " + nameTo);
  return synthPointerDrag(start, end);
}

// The `[data-terminal-row]` wrappers in DOM order (dnd-kit owns these as the sortable elements).
async function terminalRowEls() {
  return $$("[data-terminal-row]");
}

// Reorder the terminal rows by dragging the row at index `from` onto the row at index `to`.
async function dragTerminalRow(from, to) {
  const rows = await terminalRowEls();
  await dragOnto(rows[from], rows[to]);
}

// Reorder projects by dragging project `nameFrom`'s rail tile onto `nameTo`'s tile.
async function dragProject(nameFrom, nameTo) {
  const src = await waitAria("Project " + nameFrom);
  const dst = await waitAria("Project " + nameTo);
  await dragOnto(src, dst);
}

// -------------------------------------------------------------------------------------------
// Section navigation (Terminals / Work / Knowledge) — v2.2 shell non-regression
// -------------------------------------------------------------------------------------------

async function gotoSection(label) {
  await clickAria(label);
  await browser.pause(400);
}

// Fire a global terminal-nav chord (Ctrl+<key>) as a REAL `keydown` on `document` — the element the
// app's TanStack hotkey manager binds (hotkey-manager listens on the registration target, which
// defaults to `document`; it matches on `event.key` + `event.ctrlKey`). We dispatch the event
// instead of using `browser.keys` because Chromedriver/Chromium SWALLOWS a synthetic Ctrl+W (it is
// the reserved "close window/tab" browser command) before it ever reaches the page — proven here:
// under `browser.keys`, Ctrl+T fires `onNew` (a terminal opens) but the SAME-path Ctrl+W never
// reaches `document`, so `onClose` never runs, even with focus parked on app chrome. In a real
// (non-WebDriver) Electron session the chord DOES reach the page (xterm yields it → it bubbles to
// document). Dispatching here drives the SAME handler chain (manager → onNew/onClose → the real
// create/close IPC), so the assertions still fail honestly if the binding or create/close regresses.
async function sendNavChord(key) {
  await browser.execute(function (k) {
    document.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: k,
        code: "Key" + k.toUpperCase(),
        ctrlKey: true,
        bubbles: true,
        cancelable: true,
      }),
    );
  }, key);
  await browser.pause(150);
}

// -------------------------------------------------------------------------------------------
// Native folder-picker override (main process) — drives kebab "Add workspace" without an OS dialog
// -------------------------------------------------------------------------------------------

// Monkeypatch `electron.dialog.showOpenDialog` in the MAIN process so the folder picker resolves to
// `pickedPath` instead of opening a real OS dialog (which WebDriver cannot drive). The window IPC
// handler imports the SAME `dialog` object, so patching the shared reference reroutes its picker.
// Restored by `restoreFolderPicker()` (or a relaunch). Requires wdio-electron-service's
// `browser.electron.execute` (main-process bridge).
// NOTE: `browser.electron.execute` serializes the injected function and re-parses it with
// `@babel/parser` in MODULE mode (wdio-electron-service `removeFirstArg`), then strips the first
// param (`electron`, provided as a main-process global). An ANONYMOUS function expression
// (`function (electron, p) {…}`) parses at top level as a NAMELESS FunctionDeclaration → the parser
// throws `SyntaxError: Unexpected token (1:9)` (col 9 = the `(` after `function `). So every
// `electron.execute` callback here is an ARROW function (parsed as an ExpressionStatement), the same
// shape wdio-electron-service uses internally. Do NOT switch these back to `function` expressions.
async function patchFolderPicker(pickedPath) {
  await browser.electron.execute((electron, p) => {
    const d = electron.dialog;
    if (!d.__nyxOrigShowOpenDialog) d.__nyxOrigShowOpenDialog = d.showOpenDialog;
    d.showOpenDialog = () => Promise.resolve({ canceled: false, filePaths: [p] });
  }, pickedPath);
}

async function restoreFolderPicker() {
  await browser.electron.execute((electron) => {
    const d = electron.dialog;
    if (d.__nyxOrigShowOpenDialog) {
      d.showOpenDialog = d.__nyxOrigShowOpenDialog;
      delete d.__nyxOrigShowOpenDialog;
    }
  });
}

// -------------------------------------------------------------------------------------------
// Close-warning chrome — trigger the OS close-requested guard from the main process
// -------------------------------------------------------------------------------------------

// Send the renderer the SAME `close-requested` push the frameless window's `close` withhold emits,
// so the real close-guard chain runs (onCloseRequested → agent_close_warnings → dialog) WITHOUT
// tearing the app down. This is the deterministic e2e equivalent of the WM close gesture.
async function requestOsClose() {
  // Arrow function (see the note on `patchFolderPicker`): an anonymous `function` expression here
  // trips wdio-electron-service's Babel re-parse with `SyntaxError: Unexpected token (1:9)`.
  await browser.electron.execute((electron, channel) => {
    const wins = electron.BrowserWindow.getAllWindows();
    if (wins[0]) wins[0].webContents.send(channel);
  }, CLOSE_REQUESTED_CHANNEL);
  await browser.pause(400);
}

// Whether the app window is still alive (used to prove "Keep open" did NOT close it).
async function windowStillAlive() {
  return browser.execute(function () {
    return !!window.__nyxWork;
  });
}

module.exports = {
  CLOSE_REQUESTED_CHANNEL,
  // readiness
  waitForWorkShell,
  // fs
  makeTempDir,
  // seam
  seedProject,
  listProjectTrees,
  // aria/dom
  ariaSel,
  elByAria,
  waitAria,
  clickAria,
  existsAria,
  waitAriaGone,
  clickButtonByText,
  bodyIncludes,
  waitForText,
  looseBandRendered,
  // rail
  enterProject,
  openProjectContextMenu,
  railProjectOrder,
  // terminals
  deckTerminalIds,
  waitForNewTerminalId,
  newWorkspaceTerminal,
  readDeck,
  typeDeck,
  waitForDeck,
  terminalRowNames,
  renameTerminalRow,
  closeTerminalRow,
  // stats
  waitForStatsTooltip,
  terminalRowText,
  // reorder
  dragOnto,
  terminalRowEls,
  dragTerminalRow,
  dragProject,
  synthPointerDrag,
  terminalRowCenter,
  projectTileCenter,
  synthDragTerminalRow,
  synthDragProject,
  reorderProjectsSeam,
  reorderTerminalsSeam,
  // nav
  gotoSection,
  sendNavChord,
  // picker
  patchFolderPicker,
  restoreFolderPicker,
  // close-warning
  requestOsClose,
  windowStillAlive,
  // re-exported shell-agnostic primitives (MCP loopback, git repo, handoff, agent session)
  mcpCall: cockpit.mcpCall,
  waitForMcp: cockpit.waitForMcp,
  startAgentSession: cockpit.startAgentSession,
  ensureGitRepo: cockpit.ensureGitRepo,
  gitRepoDir: cockpit.gitRepoDir,
  makeHandoff: restore.makeHandoff,
};
