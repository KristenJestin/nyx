/**
 * Electron host adapter of FRONTIER 4 — the boot / shutdown LIFECYCLE (the mirror of
 * the Tauri adapter's `setup`/`on_window_event` sequence in `apps/tauri/.../lib.rs`).
 *
 * `boot` brings the core up IN ORDER; `shutdown` takes it down idempotently. Phase-2
 * scope wires the seams that exist now: load the `.node`, resolve the paths, build
 * the EventSink + service container. The DB-open/restore/normalize/resume/MCP-start
 * steps the Tauri `setup` runs are phase 5 — their slots are named here so the host
 * boots the SAME core in the SAME order once those modules are exposed over napi.
 *
 * Boot is FALLIBLE and BOUNDED: a missing/ABI-mismatched `.node` throws a readable
 * error rather than hanging, which the host entry turns into a `fatal` event +
 * non-zero exit (task #25's "never an infinite load").
 */
import { ElectronAppPaths } from "./app-paths";
import { ElectronEventSink, type EmitEvent } from "./event-sink";
import { loadNapi } from "./napi";
import { HostServices } from "./services";
import { BusyStatePoller } from "./busy-state";
import { StatsPoller } from "./stats-state";
import type { HostBootConfig, HostEventPayload, PingResult } from "../shared/host-protocol";

/** True iff this process is pure Node (no Chromium browser/renderer runtime). */
export function isNodePure(): boolean {
  // In full Electron, `process.type` is "browser" (main) or "renderer". Under
  // ELECTRON_RUN_AS_NODE it is undefined, and `require('electron').app` is absent.
  // Either alone is conclusive; we assert both for a belt-and-braces check.
  if ((process as { type?: string }).type !== undefined) return false;
  try {
    // eslint-disable-next-line @typescript-eslint/no-var-requires
    const electron = require("electron") as { app?: unknown };
    return electron.app === undefined;
  } catch {
    // No electron module at all → definitely a plain Node context.
    return true;
  }
}

/**
 * Boot the host: load napi (proves the ABI), resolve paths, assemble the container.
 * Returns the live services + the proof bundle main logs / ships as `ready`.
 */
export function boot(config: HostBootConfig, emit: EmitEvent): {
  services: HostServices;
  info: PingResult;
} {
  // TEST-ONLY: simulate a boot failure (e.g. a `.node` load error) deterministically,
  // so the lifecycle's "readable fatal, never an infinite load" path is verifiable.
  // Guarded by an env flag; a production host ignores it.
  if (process.env.NYX_HOST_FORCE_BOOT_FAIL === "1") {
    throw new Error("simulated .node load failure (NYX_HOST_FORCE_BOOT_FAIL)");
  }
  // 1. Load the native addon — ONLY in this host. A failure here is the readable
  //    boot error (not a hang).
  const napi = loadNapi();
  const coreVersion = napi.version(); // proves the `.node` actually loaded.

  // 2. Frontier 2 — resolve + create the data dir (honors NYX_DATA_DIR).
  const paths = new ElectronAppPaths(config);
  const dataDir = paths.dataDir();

  // 3. Frontier 1 — events out.
  const events = new ElectronEventSink(emit);

  // 4. Frontier 3 — the service / state container. Constructing it OPENS the DB pool
  //    (NyxCore.open → migrate) under the resolved data dir (PRD-5 #2).
  const services = new HostServices(napi, paths, events);

  // 5. Phase-5 boot steps, in the SAME order as the Tauri `setup` (lib.rs):
  //   a. Terminal boot normalization (PRD-5 #5 / #2): settle any terminal left at a
  //      persisted `exec_state = running` (force-quit artefact) to idle, so no phantom
  //      running badge survives a restart. Best-effort + off the Node loop (AsyncTask).
  void services.core.normalizePhantomTerminals().catch(() => {
    /* best-effort: the UI must still come up */
  });
  //   a1. Managed-command BOOT RESTORE (PRD-5 #18 criterion 2, parity with the Tauri
  //      `restore_commands_on_boot`): relaunch every instance whose template
  //      `restart_on_startup` is ON and that was running at the last shutdown, normalize
  //      orphaned `running` to idle, and reset the snapshots. Synchronous on the runner
  //      (it spawns off-screen command PTYs on its own threads); best-effort so a single
  //      restore failure cannot block boot. Emits `commands` so the band re-pulls.
  try {
    const relaunched = services.commands.restoreOnBoot();
    if (relaunched.length > 0) events.changed("commands");
  } catch (e) {
    process.stderr.write(`[core-host] command restore-on-boot failed: ${(e as Error).message}\n`);
  }
  //   a2. Boot agent-session RESUME scan (PRD-5 #5): sweep stale sessions to `unknown`,
  //      then PARK a `claude --resume <id>` for every alive terminal whose project opts
  //      in and whose session is resumable — injected when the front remounts each
  //      restored terminal's PTY. Runs off the loop; parks are populated before the
  //      front (which mounts only after `ready`) can spawn a terminal.
  void services.core
    .resumeScanOnBoot()
    .then((parks) => {
      services.resumeParks.setAll(parks);
      // The boot resume-scan also RETIRES provably-dead sessions (boot-cleanup): a session
      // we did NOT resume is gone, so it is marked `ended`. Nudge the renderer to re-pull
      // `agent_active_sessions` so a STALE agent icon clears on relaunch — e.g. a claude
      // started-but-never-talked-in (no transcript → not resumed) then killed without a
      // SessionEnd: its row is now `ended`, but without this signal the sidebar icon would
      // survive forever because nothing told the UI the session is gone.
      events.changed("agent-sessions");
    })
    .catch(() => {
      /* best-effort: a resume failure must not block boot */
    });
  //   b. Busy/idle authority loop (PRD-5 #1, decision 1-B): poll the foreground
  //      process group of every open PTY ~300ms, emit `terminal://busy-state` on
  //      TRANSITION only. The OS-derived running-dot signal that replaces OSC 133.
  const busyPoller = new BusyStatePoller(services.ptys, events);
  services.ptys.onPtyExit((terminalId) => busyPoller.forget(terminalId));
  busyPoller.start();
  services.busyPoller = busyPoller;
  //   b2. Per-terminal CPU%/RAM loop (FEEDBACK #28): sample each live terminal's process
  //      tree (shell + descendants) ~1.5s via the single host-owned `NyxProcStats` (one
  //      live `sysinfo::System`, kept alive for CPU% deltas), emit `terminal://stats` on a
  //      visible change. Cross-platform (Linux/macOS/Windows). Shares the PTY-exit forget
  //      hook so a closed terminal's tracked reading is dropped (alongside busy-state).
  const statsPoller = new StatsPoller(services.ptys, events, new napi.NyxProcStats());
  services.ptys.onPtyExit((terminalId) => statsPoller.forget(terminalId));
  statsPoller.start();
  services.statsPoller = statsPoller;
  //   c. Local MCP server (PRD-5 #3): start on the SHARED pool. A bind failure (port
  //      taken) is a WARNING, never a hard boot failure — the UI must still come up
  //      (parity with the Tauri `setup` which logs and continues).
  try {
    const port = services.core.mcpStart(
      // onChanged: a mutating MCP tool produced a coarse `changed` invalidation — relay it
      // so the renderer re-pulls the named collection (the SAME seam the host emits
      // elsewhere; e.g. a `create_terminal` fires `terminals` so the renderer mounts the
      // xterm + spawns the PTY, exactly as a UI-created terminal).
      (...args: unknown[]) => {
        const ev = args[args.length - 1] as { topic?: string } | undefined;
        const topic = ev?.topic;
        if (
          topic === "terminals" ||
          topic === "workspaces" ||
          topic === "commands" ||
          topic === "agent-sessions"
        ) {
          events.changed(topic);
        }
      },
      // onTerminalOp: the live-PTY half of the interactive-terminal tools — park an opening
      // command, write into a terminal's live shell, or kill its PTY (the half nyx-core
      // cannot do: it owns the records, not the live PTY). Dispatched on the Node loop.
      (...args: unknown[]) => {
        const op = args[args.length - 1] as
          | { op?: string; terminalId?: string; command?: string }
          | undefined;
        if (!op || typeof op.terminalId !== "string") return;
        switch (op.op) {
          case "park":
            services.ptys.parkOpeningCommand(op.terminalId, op.command ?? "");
            break;
          case "send":
            services.ptys.writeToTerminal(op.terminalId, Buffer.from(`${op.command ?? ""}\r`, "utf8"));
            break;
          case "send_raw":
            // FEEDBACK #31 (send_keys): the payload is the RAW bytes hex-encoded by nyx-napi
            // (named keys + literal text already resolved). Decode and write them VERBATIM —
            // NO appended `\r` (unlike "send"), so the agent can drive a raw-mode TUI.
            services.ptys.writeToTerminal(op.terminalId, Buffer.from(op.command ?? "", "hex"));
            break;
          case "close":
            services.ptys.closeTerminal(op.terminalId);
            break;
        }
      },
    );
    emit({ kind: "changed", topic: "commands" }); // observable "MCP up" marker (best-effort).
    process.stderr.write(`[core-host] nyx MCP server listening on http://127.0.0.1:${port}/mcp\n`);
    //   d. Boot reconcile (PRD-5 #4): re-template/re-register installed providers'
    //      plugins (NEVER install on boot). Detached + best-effort; the `claude` CLI it
    //      runs is wall-clock bounded so a hung CLI cannot freeze the host.
    services.core.mcpReconcile(paths.dataDir(), paths.resourceDir());
  } catch (e) {
    process.stderr.write(`[core-host] nyx MCP server did not start: ${(e as Error).message}\n`);
  }

  const info: PingResult = {
    coreVersion,
    electron: process.versions.electron ?? "unknown",
    node: process.versions.node,
    abi: process.versions.modules,
    nodePure: isNodePure(),
    dataDir,
    resourceDir: paths.resourceDir(),
  };

  return { services, info };
}

/**
 * Shut the host down IN ORDER (the Tauri shutdown sequence, mirrored): SNAPSHOT what
 * is running so the next boot can restore it, THEN stop PTY/commands (phase 3/5 add
 * MCP + DB), THEN let the entry exit. Idempotent / latched: safe on an explicit
 * `shutdown` request AND on a parent-disconnect.
 *
 * The snapshot step runs FIRST and only when the host is booted (state permits) — it
 * emits a `changed` signal as the observable "snapshot taken" marker. Phase 5 makes
 * it persist the real running-command set; here it is the ordered placeholder so the
 * close-warning/snapshot-before-shutdown ordering (task #25 criterion 4) is wired.
 */
let shuttingDown = false;
export function shutdown(
  services: HostServices | null,
  emit?: (payload: HostEventPayload) => void,
): void {
  if (shuttingDown) return;
  shuttingDown = true;
  // 1. SNAPSHOT + REAP the managed commands FIRST (before tearing anything down), only
  //    if booted (PRD-5 #18 criterion 2, parity with the Tauri close hook): latch, persist
  //    `was_running_on_shutdown` for every instance so the next boot relaunches exactly the
  //    ones that were running, then tree-kill EVERY live process so nothing is orphaned past
  //    exit. The latch makes this run exactly once across close-request + destroy. Then emit
  //    the ordered `commands` marker so an observer sees the snapshot was taken.
  if (services) {
    try {
      services.commands.snapshotAndReap();
    } catch (e) {
      process.stderr.write(`[core-host] command shutdown snapshot/reap failed: ${(e as Error).message}\n`);
    }
  }
  if (services && emit) {
    emit({ kind: "changed", topic: "commands" });
  }
  // 2. Stop the busy-state (PRD-5 #1) + per-terminal stats (FEEDBACK #28) poll loops
  //    before tearing down the PTYs.
  if (services) {
    services.busyPoller?.stop();
    services.statsPoller?.stop();
  }
  // 3. Stop the PTYs. The keyed manager kills every live `NyxPty` (each kill EOFs
  //    its Rust reader and reaps the child) in the same Tauri shutdown order. The MCP
  //    server + DB pool are owned by the core (NyxCore) and torn down with the process
  //    exit (the same lifetime model as the Tauri server/Db managed state).
  if (services) services.ptys.killAll();
}
