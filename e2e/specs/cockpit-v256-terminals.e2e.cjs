/* eslint-disable */
// v2.5.6 cockpit gate — TERMINAL ROW AFFORDANCES (task 01KXBNX51EPQ69MVKYG4W960HE).
//
// Against the REAL Electron app, entered into a seeded project, this exercises the per-terminal
// cockpit affordances the rebranch added, plus the v2.2 terminal non-regression:
//   - LIVE deck terminal      : create a terminal in the workspace, type a marker, read it back
//                               through `__nyxDeck` (proves a real PTY/xterm pane — v2.2 workbench).
//   - INLINE RENAME           : pencil → inline <Input> → Enter pins a manual label.
//   - CPU/RAM STATS (hover)    : the non-inline `CPU … · RAM …` tooltip on the row.
//   - PER-ROW CLOSE            : the row "×" closes THAT terminal by id.
//   - Ctrl+T / Ctrl+W          : the restored global terminal shortcuts (v2.2 non-regression).
// (v2.5.6 finding #26 removed the "loose terminals" band — no unattached terminals are surfaced.)

const assert = require("assert");
const h = require("./cockpit-v256-helpers.cjs");

describe("v2.5.6 cockpit — terminal rows (stats / rename / close / libres)", function () {
  const PROJECT = "TermProj";
  let rootDir;
  let t1Id;
  let t1Name;

  before(async function () {
    rootDir = h.makeTempDir("nyx-v256-term-root-");
    await h.waitForWorkShell();
    await h.seedProject(PROJECT, rootDir, "main");
    await h.waitAria("Project " + PROJECT, 15000);
    await h.enterProject(PROJECT);
  });

  it("opens a terminal in the workspace and drives it LIVE through the deck (v2.2)", async function () {
    t1Id = await h.newWorkspaceTerminal();
    assert(t1Id, "workspace 'New terminal' must create a terminal pane");
    const marker = "TERM_LIVE_" + Date.now();
    assert(await h.typeDeck(t1Id, 'echo "' + marker + '"\n'), "deck input seam must accept keystrokes");
    const buf = await h.waitForDeck(t1Id, marker, 15000);
    assert(buf.indexOf(marker) !== -1, "the typed marker must echo back through the live pane");
    // Capture the row's current display name for the rename step (it is the only terminal now).
    const names = await h.terminalRowNames();
    assert.strictEqual(names.length, 1, "exactly one terminal row expected after the first create");
    t1Name = names[0];
  });

  it("renames a terminal INLINE (pencil → input → Enter)", async function () {
    const newName = "renamed-term-e2e";
    await h.renameTerminalRow(t1Name, newName);
    const names = await h.terminalRowNames();
    assert(
      names.indexOf(newName) !== -1,
      "the row must show the manual label after the inline rename (got " + JSON.stringify(names) + ")",
    );
    t1Name = newName;
  });

  it("surfaces per-terminal CPU/RAM as a NON-INLINE hover tooltip", async function () {
    // NEGATIVE assertion (the NON-INLINE guard) — AT REST, before any hover: the terminal ROW must
    // NOT carry the `CPU … · RAM …` reading on its own line. The stats live in a Base UI tooltip
    // PORTALED to document.body, so a healthy row's text is JUST the name; if the stats regressed to
    // INLINE (the v1 behaviour this PRD forbids), the reading would appear in the row's own text and
    // this fails — so the gate honestly catches an inline regression, not just a missing producer.
    const rowText = await h.terminalRowText(t1Name);
    assert(rowText != null, "the renamed terminal row must be present in the tree (got null)");
    assert(
      !/CPU\s.+·\sRAM\s/.test(rowText),
      "stats must NOT render inline on the terminal row at rest (non-inline constraint); row text: " +
        JSON.stringify(rowText),
    );
    // The name stays a distinct, fully-present token in the row — never masked/consumed by the stats.
    assert(
      rowText.indexOf(t1Name) !== -1,
      "the terminal name must remain visible in the row; row text: " + JSON.stringify(rowText),
    );

    // POSITIVE assertion: the stats come from the sidecar `terminal://stats` producer (~1.5s cadence)
    // and show ONLY on hover of the row (never inline). Retries the hover until the tooltip opens.
    const tip = await h.waitForStatsTooltip(t1Name, 30000);
    assert(/CPU\s/.test(tip), "tooltip must carry a CPU reading, got: " + tip);
    assert(/RAM\s/.test(tip), "tooltip must carry a RAM reading, got: " + tip);
  });

  it("has NO 'Loose terminals' band (finding #26 — no unattached terminals surfaced)", async function () {
    // The band + its "New unattached terminal" affordance were removed: a terminal attached to
    // nothing is no longer displayed. Neither the section label nor its "+" must be on screen.
    assert(!(await h.looseBandRendered()), "the Loose terminals band must NOT be rendered");
    assert(
      !(await h.existsAria("New unattached terminal")),
      "the 'New unattached terminal' affordance must be gone",
    );
  });

  it("closes a terminal from its per-row ×", async function () {
    // Close the renamed workspace terminal by its row's × (targets THIS terminal by id).
    await h.closeTerminalRow(t1Name);
    await browser.waitUntil(
      async () => (await h.terminalRowNames()).indexOf(t1Name) === -1,
      { timeout: 10000, timeoutMsg: "the closed terminal row never disappeared" },
    );
    assert(
      (await h.deckTerminalIds()).indexOf(t1Id) === -1,
      "the closed terminal's deck pane must be gone",
    );
  });

  it("keeps the Ctrl+T / Ctrl+W terminal shortcuts (v2.2 non-regression)", async function () {
    // The chords are GLOBAL (TanStack Hotkeys bound on `document`). We fire them as real `keydown`
    // events on `document` (see `sendNavChord`) rather than `browser.keys`, because Chromedriver
    // SWALLOWS a synthetic Ctrl+W (the reserved "close" browser command) before it reaches the page.
    // This still drives the app's real handler chain (manager → onNew/onClose → create/close IPC),
    // so the assertions honestly fail if the binding or the create/close wiring regresses.
    const before = (await h.deckTerminalIds()).length;
    await h.sendNavChord("t");
    await browser.pause(1200);
    const afterNew = (await h.deckTerminalIds()).length;
    assert(afterNew > before, "Ctrl+T must open a new terminal (got " + before + " → " + afterNew + ")");
    await h.sendNavChord("w");
    await browser.pause(1200);
    const afterClose = (await h.deckTerminalIds()).length;
    assert(
      afterClose < afterNew,
      "Ctrl+W must close the selected terminal (got " + afterNew + " → " + afterClose + ")",
    );
  });
});
