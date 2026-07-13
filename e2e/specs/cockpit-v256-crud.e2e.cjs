/* eslint-disable */
// v2.5.6 cockpit gate — PROJECT / WORKSPACE RIGHT-CLICK CRUD (task 01KXBNX51EPQ69MVKYG4W960HE, PRD-B).
//
// Drives the rail project RIGHT-CLICK context menu end-to-end against the REAL Electron app
// (v2.5.6 finding #24: the ⋮ kebab trigger was removed — right-click is the sole entry point):
//   - RENAME project   : right-click → "Project settings" → Global pane rename → Save → rail tile updates.
//   - ADD workspace    : right-click → "Add workspace"    → (picker patched) → name dialog → accordion appears.
//   - REMOVE workspace : right-click → "Remove <ws>"       → destructive confirm → accordion gone.
//   - DELETE project   : right-click → "Delete project"    → destructive confirm → rail tile gone.
// Plus the "no visible kebab" guard and the section-nav (Terminals/Work/Knowledge) v2.2 non-regression.
//
// Isolation: its own NYX_DATA_DIR (wdio.conf.cjs keys non-pair specs by basename). The native folder
// picker (an OS dialog WebDriver cannot drive) is neutralized by patching main's
// `dialog.showOpenDialog` to a real temp dir, so "Add workspace" runs fully through the UI.

const assert = require("assert");
const h = require("./cockpit-v256-helpers.cjs");

describe("v2.5.6 cockpit — project/workspace right-click CRUD", function () {
  const PROJECT = "CrudProj";
  const RENAMED = "CrudProjRenamed";
  let rootDir;
  let wsDir;

  before(async function () {
    rootDir = h.makeTempDir("nyx-v256-crud-root-");
    wsDir = h.makeTempDir("nyx-v256-crud-ws-");
    await h.waitForWorkShell();
    // Seed the project through the inert seam (no OS folder dialog) and enter it.
    await h.seedProject(PROJECT, rootDir, "main");
    await h.waitAria("Project " + PROJECT, 15000);
    await h.enterProject(PROJECT);
  });

  it("opens the project actions via RIGHT-CLICK, with NO visible kebab (⋮) trigger", async function () {
    // finding #24: the 3-dot kebab button is gone — the tile carries no visible actions trigger.
    assert(
      !(await h.existsAria("Project actions for " + PROJECT)),
      "the rail tile must NOT carry a visible kebab (⋮) actions button anymore",
    );
    // Right-click is the sole entry point and surfaces the full project menu.
    await h.openProjectContextMenu(PROJECT);
    assert(
      await h.existsAria("Project settings for " + PROJECT),
      "the right-click menu must expose Project settings",
    );
    assert(
      await h.existsAria("Delete project " + PROJECT),
      "the right-click menu must expose Delete project",
    );
    await browser.keys("Escape"); // dismiss the context menu
    await browser.pause(200);
  });

  it("renames the project via right-click → Project settings → Global pane", async function () {
    await h.openProjectContextMenu(PROJECT);
    await h.clickAria("Project settings for " + PROJECT);
    // The project-settings modal's Global pane owns the rename (update_project).
    const nameField = await h.waitAria("Project name");
    await nameField.click();
    await nameField.setValue(RENAMED);
    await h.clickButtonByText("Save");
    await browser.pause(400);
    await browser.keys("Escape"); // close the settings modal
    await browser.pause(300);
    // The rail tile (optimistically) reflects the new name.
    await h.waitAria("Project " + RENAMED, 10000);
    assert(
      await h.existsAria("Project " + RENAMED),
      "rail tile must show the renamed project after Save",
    );
    assert(
      !(await h.existsAria("Project " + PROJECT)),
      "the old project name must no longer label a tile",
    );
  });

  it("adds a workspace via right-click (folder picker → name dialog)", async function () {
    // Neutralize the native folder picker so the "Add workspace" flow runs through the UI.
    await h.patchFolderPicker(wsDir);
    await h.openProjectContextMenu(RENAMED);
    await h.clickAria("Add workspace to " + RENAMED);
    // The name dialog (AddWorkspaceDialog) pre-fills the folder basename; confirm with a known name.
    const wsField = await h.waitAria("Workspace name");
    await wsField.click();
    await wsField.setValue("extra-ws");
    await h.clickButtonByText("Add workspace");
    await h.restoreFolderPicker();
    // The new workspace surfaces as an accordion in the entered project's sidebar.
    await h.waitForText("extra-ws", 10000);
    const tree = (await h.listProjectTrees()).find((t) => t.project.name === RENAMED);
    assert(tree, "the renamed project must still be present");
    assert(
      tree.workspaces.some((w) => w.name === "extra-ws"),
      "the added workspace must appear in the project tree",
    );
  });

  it("removes the (non-root) workspace via the right-click destructive confirm", async function () {
    await h.openProjectContextMenu(RENAMED);
    // The menu now lists a per-workspace remove for the non-root workspace.
    await h.clickAria("Remove workspace extra-ws from " + RENAMED);
    // Destructive confirm dialog → "Remove workspace".
    await h.waitForText("Remove workspace", 8000);
    await h.clickButtonByText("Remove workspace");
    await browser.pause(600);
    await browser.waitUntil(
      async () => {
        const tree = (await h.listProjectTrees()).find((t) => t.project.name === RENAMED);
        return tree && !tree.workspaces.some((w) => w.name === "extra-ws");
      },
      { timeout: 10000, timeoutMsg: "the removed workspace never left the project tree" },
    );
  });

  it("deletes the whole project via the right-click destructive confirm", async function () {
    await h.openProjectContextMenu(RENAMED);
    await h.clickAria("Delete project " + RENAMED);
    await h.waitForText("Delete project", 8000);
    await h.clickButtonByText("Delete");
    await h.waitAriaGone("Project " + RENAMED, 10000);
    const trees = await h.listProjectTrees();
    assert(
      !trees.some((t) => t.project.name === RENAMED),
      "the deleted project must be gone from the rail",
    );
  });

  it("keeps the v2.2 section-nav (Terminals/Work/Knowledge) reachable (non-regression)", async function () {
    // The section-nav lives on the always-mounted shell; navigating between pages must work even
    // with no project entered (after the delete above).
    await h.gotoSection("Work");
    assert(await h.existsAria("Work"), "Work section-nav entry must exist");
    await h.gotoSection("Knowledge");
    assert(await h.existsAria("Knowledge"), "Knowledge section-nav entry must exist");
    await h.gotoSection("Terminals");
    assert(await h.existsAria("Terminals"), "Terminals section-nav entry must exist");
    // The "+ Add project" rail affordance is still present (v2.1 capability, rebranched).
    assert(await h.existsAria("Add project"), "the rail must keep the Add project affordance");
  });
});
