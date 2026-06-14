/* eslint-disable */
// PRD-2 scenario e2e (ZE3): projects / workspaces / auto-attach, driven against
// the REAL nyx app via tauri-driver + WebKitWebDriver — the same harness the
// PRD-1 restore specs use. Where the restore specs script terminals through the
// inert `window.__nyx` seam, this spec scripts the PROJECT/WORKSPACE surface the
// sidebar drives: `createProject` / `createWorkspace` (the same `useProjects`
// hook mutations the manual-add flow calls — only the OS-native folder PICKER,
// which a WebDriver cannot operate, is bypassed) and the REAL backend
// `auto_attach_terminal` (Linux `/proc` cwd → longest-ancestor workspace match).
//
// Covered PRD-2 done-criteria:
//   1. create a project + add a manual folder as a workspace (real UI hooks);
//   2. Linux `/proc` auto-attach: `cd` into a known workspace from a terminal →
//      the terminal auto-attaches to that workspace;
//   3. two ARBITRARY folders (a real working dir + a fake feature-start-style
//      dir) are treated IDENTICALLY once registered as workspaces.
//
// (The OSC7 provider off-`/proc` is covered by ZE0's Rust unit tests:
//  src-tauri/src/osc7.rs parser tests + src-tauri/src/resolve.rs provider/routing
//  tests, incl. `provider_normalizes_raw_cwd` exercising `CwdProvider::Osc7`.
//  The Windows OSC7 dogfood is the phase-4 manual gate — not run here.)

const assert = require("assert");
const fs = require("fs");
const os = require("os");
const path = require("path");

// Stable, REAL directories on the Linux host the app shares (the spec process
// runs on the same machine as the app under tauri-driver). They must be real,
// non-symlinked dirs so the shell's `/proc/<pid>/cwd` reading equals the
// lexically-normalized path stored for the workspace (pathnorm is lexical — it
// does NOT resolve symlinks — so a symlinked tmp would not match).
const ROOT = fs.realpathSync(os.tmpdir()); // resolve any /tmp symlink up front
const RUN = "nyx-ws-e2e-" + process.pid;

function mkdir(...segs) {
  const p = path.join(ROOT, RUN, ...segs);
  fs.mkdirSync(p, { recursive: true });
  return fs.realpathSync(p); // canonical, symlink-free absolute path
}

// One project root, plus a manually-added workspace, plus the two "arbitrary
// folders" of criterion 3: a real working dir and a fake feature-start-style dir.
const PROJECT_ROOT = mkdir("project-root");
const MANUAL_WS = mkdir("manual-workspace");
const REAL_WORKDIR = mkdir("arbitrary", "real-working-dir");
const FAKE_FEATURE_DIR = mkdir("arbitrary", "feature-start-xyz");

async function waitForApp() {
  await browser.waitUntil(
    async () => browser.execute(() => !!(window.__nyx && window.__nyx.activeId() != null)),
    { timeout: 30000, timeoutMsg: "window.__nyx active terminal never appeared" },
  );
  await browser.pause(1000); // let the first shell print its prompt
}

const activeId = () => browser.execute(() => (window.__nyx ? window.__nyx.activeId() : null));

const listRecords = () => browser.execute(() => (window.__nyx ? window.__nyx.list() : []));

const listProjects = () => browser.execute(() => (window.__nyx ? window.__nyx.listProjects() : []));

const createProject = (name, rootPath, rootName) =>
  browser.execute((n, r, rn) => window.__nyx.createProject(n, r, rn), name, rootPath, rootName);

const createWorkspace = (projectId, name, p) =>
  browser.execute((pid, n, pp) => window.__nyx.createWorkspace(pid, n, pp), projectId, name, p);

const createTerminalAt = (cwd) => browser.execute((c) => window.__nyx.create(c), cwd);

const typeInto = (id, text) => browser.execute((i, t) => window.__nyx.typeInto(i, t), id, text);

const terminalInfo = (id) => browser.execute((i) => window.__nyx.terminalInfo(i), id);

const autoAttach = (id) => browser.execute((i) => window.__nyx.autoAttach(i), id);

// Poll the front record list until terminal `id` shows `workspaceId` as its
// binding. The backend attach already succeeded (the `autoAttach` result is the
// authority); this just waits for React to commit the reflected `workspace_id`
// into the `list()` snapshot the sidebar groups on — no fixed sleep.
async function waitForBinding(id, workspaceId) {
  let last;
  await browser.waitUntil(
    async () => {
      const rec = (await listRecords()).find((r) => r.id === id);
      last = rec && rec.workspace_id;
      return last === workspaceId;
    },
    {
      timeout: 10000,
      timeoutMsg:
        "record " + id + " workspace_id never became " + workspaceId + " (got " + last + ")",
    },
  );
}

// `cd` a terminal into `dir`, then poll the backend `/proc` cwd until it reports
// that dir (the shell has actually changed directory). Returns the record id.
async function cdInto(id, dir) {
  await typeInto(id, "cd " + dir + "\n");
  await browser.waitUntil(
    async () => {
      const info = await terminalInfo(id);
      return !!(info && info.cwd && info.cwd === dir);
    },
    {
      timeout: 15000,
      timeoutMsg: "terminal /proc cwd never became " + dir,
    },
  );
}

// Open a fresh terminal at `dir`, wait until its live `/proc` cwd resolves, and
// return its record id. A fresh terminal spawns its shell AT `dir`, so no `cd`
// is needed — but we still wait for the PTY id + the first `/proc` reading.
async function openTerminalAt(dir) {
  const before = (await listRecords()).map((r) => r.id);
  await createTerminalAt(dir);
  await browser.waitUntil(async () => (await listRecords()).length > before.length, {
    timeout: 15000,
    timeoutMsg: "new terminal record did not appear",
  });
  await browser.pause(600); // let the shell spawn + the PTY id surface
  const after = await listRecords();
  const fresh = after.filter((r) => before.indexOf(r.id) === -1);
  return fresh[fresh.length - 1].id;
}

describe("PRD-2 workspaces + auto-attach (tauri-driver, Linux /proc)", function () {
  before(async function () {
    await waitForApp();
  });

  it("creates a project and adds a manual folder as a workspace (real UI hooks)", async function () {
    const created = await createProject("proj", PROJECT_ROOT, "root");
    assert(created && created.project && created.root, "create_project returns project + root");
    assert.strictEqual(
      created.root.path,
      PROJECT_ROOT,
      "the root workspace stores the (normalized) project root path",
    );
    assert.strictEqual(created.root.is_root, true, "the auto-created workspace is the root");
    const projectId = created.project.id;

    // Add a SECOND, manual folder as a (non-root) workspace of that project —
    // the exact `create_workspace` the add-workspace dialog calls on confirm.
    const ws = await createWorkspace(projectId, "manual", MANUAL_WS);
    assert.strictEqual(ws.path, MANUAL_WS, "manual workspace stores the normalized path");
    assert.strictEqual(ws.is_root, false, "a manually-added workspace is not the root");

    // The tree the sidebar renders now has the project with BOTH workspaces.
    // `createWorkspace` re-lists the project's workspaces into React state
    // (`useProjects.refreshWorkspaces`), so poll the seam until that render has
    // committed — the backend create already succeeded (above), this just lets
    // the front tree settle, exactly as a UI observer would wait for the spine
    // to repaint.
    const want = [MANUAL_WS, PROJECT_ROOT].sort();
    let wsPaths = [];
    await browser.waitUntil(
      async () => {
        const projects = await listProjects();
        const tree = projects.find((t) => t.project.id === projectId);
        if (!tree) return false;
        wsPaths = tree.workspaces.map((w) => w.path).sort();
        return JSON.stringify(wsPaths) === JSON.stringify(want);
      },
      {
        timeout: 10000,
        timeoutMsg:
          "project tree never listed root + manual workspace (got " + JSON.stringify(wsPaths) + ")",
      },
    );
    assert.deepStrictEqual(
      wsPaths,
      want,
      "the project lists exactly its root + the manually-added workspace",
    );
  });

  it("auto-attaches a terminal to a known workspace after a `cd` into it (Linux /proc)", async function () {
    // Reuse the project's ROOT workspace (created in the first test) as the known
    // target. Poll the seam until that workspace is in the front tree (decoupled
    // from render timing / test order). Open a fresh terminal, `cd` it into the
    // workspace, and run the REAL auto-attach pass (reads /proc cwd → resolves
    // the longest-ancestor known workspace).
    let rootWs = null;
    await browser.waitUntil(
      async () => {
        const projects = await listProjects();
        for (const t of projects) {
          const w = t.workspaces.find((w) => w.path === PROJECT_ROOT);
          if (w) {
            rootWs = w;
            return true;
          }
        }
        return false;
      },
      { timeout: 10000, timeoutMsg: "the project with the root workspace must exist" },
    );

    const termId = await openTerminalAt(ROOT); // start OUTSIDE the workspace
    // Sanity: a terminal outside every workspace must NOT auto-attach.
    const outside = await autoAttach(termId);
    assert.strictEqual(
      outside.changed,
      false,
      "a terminal outside every known workspace must not attach (no guessing)",
    );

    // Now `cd` into the known workspace and auto-attach: it must bind to it.
    await cdInto(termId, PROJECT_ROOT);
    const res = await autoAttach(termId);
    assert.strictEqual(res.changed, true, "cd into a known workspace must auto-attach");
    assert.strictEqual(
      res.workspace_id,
      rootWs.id,
      "the terminal attaches to the workspace whose path is its (ancestor) cwd",
    );

    // The front list reflects the new binding (what the sidebar groups on).
    await waitForBinding(termId, rootWs.id);
  });

  it("treats two arbitrary folders identically once registered as workspaces", async function () {
    // Register BOTH arbitrary folders as workspaces of one project: a real
    // working dir and a fake feature-start-style dir. Nothing about them differs
    // to the backend once registered — same create path, same auto-attach path.
    const proj = await createProject("arbitrary-proj", REAL_WORKDIR, "real");
    const projectId = proj.project.id;
    const realWs = proj.root; // root workspace == the real working dir
    const fakeWs = await createWorkspace(projectId, "feature", FAKE_FEATURE_DIR);

    assert.strictEqual(realWs.path, REAL_WORKDIR);
    assert.strictEqual(fakeWs.path, FAKE_FEATURE_DIR);

    // Drive a terminal into EACH and auto-attach; assert IDENTICAL handling:
    // both attach (changed), each to its own workspace id, via the same call.
    const results = [];
    for (const [dir, ws] of [
      [REAL_WORKDIR, realWs],
      [FAKE_FEATURE_DIR, fakeWs],
    ]) {
      const termId = await openTerminalAt(ROOT);
      await cdInto(termId, dir);
      const res = await autoAttach(termId);
      results.push({ dir, ws, termId, res });
    }

    for (const { dir, ws, termId, res } of results) {
      assert.strictEqual(
        res.changed,
        true,
        "folder " + dir + " must auto-attach once registered (same as any other)",
      );
      assert.strictEqual(
        res.workspace_id,
        ws.id,
        "folder " + dir + " attaches to its own registered workspace",
      );
      // The front list reflects the binding (settle-poll, no fixed sleep).
      await waitForBinding(termId, ws.id);
    }

    // The decisive equality: the real working dir and the fake feature dir
    // produced the SAME outcome shape (both changed, both bound to their ws) —
    // the backend made no distinction between "real" and "fake" folders.
    assert.deepStrictEqual(
      results.map((r) => r.res.changed),
      [true, true],
      "both arbitrary folders are handled identically (both auto-attach)",
    );
  });

  after(function () {
    // Best-effort cleanup of the per-run scratch dirs.
    try {
      fs.rmSync(path.join(ROOT, RUN), { recursive: true, force: true });
    } catch {}
  });
});
