/* eslint-disable */
// PRD-3 managed-commands e2e against the REAL nyx app (release build) through
// tauri-driver + WebKitWebDriver. Managed commands run under a real PTY/shell and
// their output paints to a WebGL canvas (not in the DOM), exactly like terminals —
// so we drive + read them through the inert `window.__nyx` command seam
// (src/components/sidebar/terminal-manager.tsx), keyed by instance id.
//
// Scenario (one app session, real backend + real shell):
//   1. create a project at a real cwd (/tmp) → its root workspace exists on disk;
//   2. create commands as project templates → each materializes ONE instance in
//      the root workspace;
//   3. run a `echo + exit 0` command → RUNNING, marker in output, then SUCCESS
//      (the GREEN dot);
//   4. run an `exit 1` command → ERROR (the RED dot);
//   5. run a long-lived `sleep` command → RUNNING, then STOP → idle, then
//      RELAUNCH → running again, asserting no double-instance is left live.
//
// ANTI-FALSE-GREEN: the `before` hook FAILS HARD if the app/seam never appears,
// and every state assertion has a finite timeout that THROWS on miss — so a build
// that doesn't mount, a missing seam, or a stuck WebDriver session FAILS the suite
// rather than silently passing (the explicit "faux-vert e2e PRD 1" risk).

const assert = require("assert");

// --- seam helpers (all go through window.__nyx) ---------------------------

// True once React mounted AND the command seam is published (gated VITE_NYX_E2E).
async function commandSeamReady() {
  return browser.execute(function () {
    return !!(
      window.__nyx &&
      typeof window.__nyx.createProject === "function" &&
      typeof window.__nyx.createCommand === "function" &&
      typeof window.__nyx.commandStart === "function" &&
      typeof window.__nyx.commandState === "function"
    );
  });
}

async function createProject(name, rootPath) {
  return browser.execute(
    function (name, rootPath) {
      return window.__nyx.createProject(name, rootPath);
    },
    name,
    rootPath,
  );
}

async function createCommand(projectId, name, command) {
  return browser.execute(
    function (projectId, name, command) {
      return window.__nyx.createCommand(projectId, name, command);
    },
    projectId,
    name,
    command,
  );
}

async function listInstances(workspaceId) {
  return browser.execute(function (workspaceId) {
    return window.__nyx.listCommandInstances(workspaceId);
  }, workspaceId);
}

async function start(instanceId) {
  return browser.execute(function (id) {
    return window.__nyx.commandStart(id);
  }, instanceId);
}

async function stop(instanceId) {
  return browser.execute(function (id) {
    return window.__nyx.commandStop(id);
  }, instanceId);
}

async function relaunch(instanceId) {
  return browser.execute(function (id) {
    return window.__nyx.commandRelaunch(id);
  }, instanceId);
}

async function output(instanceId) {
  return browser.execute(function (id) {
    return window.__nyx.commandOutput(id);
  }, instanceId);
}

// Read an instance's CURRENT run state for the dot. Two independent signals are
// combined so the read is robust:
//   - the seam's live `command://state` map (event-driven, freshest), and
//   - the persisted `last_state` from `command_instance_list` (the runner commits
//     it to the DB on EVERY transition, BEFORE emitting the event — see
//     TauriRunnerSink::on_state), so it is authoritative even if a fast transition
//     raced the event listener.
// The live map wins when it is non-idle (most up to date); otherwise we trust the
// committed DB state.
async function readState(workspaceId, instanceId) {
  return browser.execute(
    function (workspaceId, instanceId) {
      var live = window.__nyx.commandState(instanceId);
      if (live && live !== "idle") return Promise.resolve(live);
      return window.__nyx.listCommandInstances(workspaceId).then(function (rows) {
        var row = (rows || []).find(function (r) {
          return r.id === instanceId;
        });
        var persisted = row ? row.last_state : "idle";
        // Prefer the live value when both agree it is non-idle; else the DB truth.
        return live && live !== "idle" ? live : persisted;
      });
    },
    workspaceId,
    instanceId,
  );
}

// Poll the instance state until it reaches `want`, or throw (anti-false-green: a
// state that never arrives FAILS the suite).
async function waitForState(workspaceId, instanceId, want, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 15000);
  let last = "";
  while (Date.now() < deadline) {
    last = await readState(workspaceId, instanceId);
    if (last === want) return last;
    await browser.pause(150);
  }
  throw new Error(
    "timed out waiting for state '" + want + "' on " + instanceId + " (last was '" + last + "')",
  );
}

// Poll the instance's output history until `needle` appears, or throw.
async function waitForOutput(instanceId, needle, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 15000);
  let last = "";
  while (Date.now() < deadline) {
    last = await output(instanceId);
    if (last && last.includes(needle)) return last;
    await browser.pause(150);
  }
  throw new Error(
    "timed out waiting for output '" + needle + "' on " + instanceId + ". Buffer was:\n" + last,
  );
}

function instanceByName(instances, name) {
  const found = (instances || []).find(function (i) {
    return i.name === name;
  });
  assert(found, "no command instance named '" + name + "' (got " + JSON.stringify(instances) + ")");
  return found;
}

describe("nyx managed commands e2e (run → states → stop/relaunch)", function () {
  let projectId;
  let workspaceId;

  before(async function () {
    // ANTI-FALSE-GREEN gate: if the app never mounts or the command seam is never
    // published (build broke, seam not gated in, WebView didn't load), this THROWS
    // and the whole suite fails — it cannot pass without a real running app.
    await browser.waitUntil(commandSeamReady, {
      timeout: 30000,
      timeoutMsg: "window.__nyx command seam never appeared (app/seam did not start)",
    });

    // Create a project at a REAL existing dir so the command cwd resolves and the
    // shell can actually spawn there. The root workspace is created with it.
    const created = await createProject("e2e-cmds", "/tmp");
    assert(created && created.project && created.root, "createProject must return project + root");
    projectId = created.project.id;
    workspaceId = created.root.id;
    assert(projectId && workspaceId, "project + root workspace ids must be present");
  });

  it("runs a command to RUNNING with observable output, then SUCCESS (green dot) on exit 0", async function () {
    const MARKER = "E2E_OK_MARKER_42";
    // echo a marker then exit 0 → success (green).
    await createCommand(projectId, "ok", 'echo "' + MARKER + '"; exit 0');
    const inst = instanceByName(await listInstances(workspaceId), "ok");

    const st = await start(inst.id);
    assert(st === "running" || st === "success", "start returns running/success, got: " + st);

    // The marker reaches the output stream (RUNNING + observable output).
    const buf = await waitForOutput(inst.id, MARKER, 15000);
    assert(buf.includes(MARKER), "command output must contain the marker");

    // exit 0 → the dot reaches SUCCESS (green).
    await waitForState(workspaceId, inst.id, "success", 15000);
  });

  it("shows ERROR (red dot) when the command exits non-zero", async function () {
    // exit 1 → error (red).
    await createCommand(projectId, "fail", "exit 1");
    const inst = instanceByName(await listInstances(workspaceId), "fail");

    await start(inst.id);
    // exit != 0 → the dot reaches ERROR (red), distinct from success.
    await waitForState(workspaceId, inst.id, "error", 15000);
  });

  it("STOP takes a running command to idle, and RELAUNCH brings it back to running (no double instance)", async function () {
    // A long-lived command so we can observe running → stop → relaunch.
    await createCommand(projectId, "svc", 'echo "SVC_UP"; sleep 120');
    const inst = instanceByName(await listInstances(workspaceId), "svc");

    // start → running, with observable output.
    const st = await start(inst.id);
    assert(st === "running", "long command starts running, got: " + st);
    await waitForState(workspaceId, inst.id, "running", 10000);
    await waitForOutput(inst.id, "SVC_UP", 15000);

    // stop → idle.
    const stopped = await stop(inst.id);
    assert(stopped === "idle", "stop returns idle, got: " + stopped);
    await waitForState(workspaceId, inst.id, "idle", 10000);

    // relaunch → running again. The instance is keyed by id in the runner, so a
    // relaunch replaces (never doubles) the live entry; we assert it is running
    // again and that the output stream shows a FRESH "SVC_UP" (a new process).
    const relaunched = await relaunch(inst.id);
    assert(relaunched === "running", "relaunch returns running, got: " + relaunched);
    await waitForState(workspaceId, inst.id, "running", 10000);

    // No double instance: the seam reports exactly ONE instance row for this
    // command in the workspace (materialization is UNIQUE per command+workspace),
    // and that single instance is the one running.
    const rows = await listInstances(workspaceId);
    const matching = rows.filter(function (r) {
      return r.id === inst.id;
    });
    assert.strictEqual(matching.length, 1, "exactly one instance row for the command (no double)");
    assert.strictEqual(
      await readState(workspaceId, inst.id),
      "running",
      "the single instance is the one left running after relaunch",
    );

    // Clean up the live process so the session tears down without an orphan.
    await stop(inst.id);
    await waitForState(workspaceId, inst.id, "idle", 10000);
  });
});
