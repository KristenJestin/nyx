/**
 * Electron host adapter of FRONTIER 3 — the SERVICE / STATE CONTAINER (the mirror of
 * the Tauri adapter's `app.manage` / `tauri::State`).
 *
 * `nyx-core` defines the long-lived runtime state types; the SHELL owns the
 * container that holds them and hands the core typed access. On the Tauri side this
 * is Tauri's managed-state registry; on the Electron host it is this plain object —
 * the host process owns the `nyx-napi` module handle, the resolved `AppPaths`, the
 * `EventSink`, and (phase-2 skeleton) the single active PTY. Phase 3/5 grow it with
 * the PTY manager, command runner, DB pool and MCP server — all OWNED here, off the
 * UI thread, exactly as the POC mandates.
 */
import type { ElectronAppPaths } from "./app-paths";
import type { ElectronEventSink } from "./event-sink";
import type { NyxCoreInstance, NyxNapi } from "./napi";
import { PtyManager } from "./pty-manager";
import { ExecStatePersister } from "./exec-state";
import { ResumeParks } from "./resume-parks";
import { CommandManager } from "./command-manager";
import type { BusyStatePoller } from "./busy-state";

export class HostServices {
  /** The OS busy-state poll loop (set by the lifecycle at boot; PRD-5 #1). */
  busyPoller: BusyStatePoller | null = null;

  /**
   * The shared-core handle: the r2d2 DB pool (PRD-5 #2) + the MCP server (PRD-5 #3).
   * Opened ONCE at construction (migrations run here); every DB call is an
   * AsyncTask (off the Node loop), and the MCP server shares this exact pool.
   */
  readonly core: NyxCoreInstance;

  /** Persists OSC 133 exec-state transitions to the DB and re-emits with the stamped
   * timestamp (PRD-5 #1, parity with the Tauri `persist_and_emit_exec_state`). */
  readonly execState: ExecStatePersister;

  /** Boot agent-session resume parks, injected at each terminal's first respawn
   * (PRD-5 #5, parity with the Tauri `PendingResumes`). */
  readonly resumeParks: ResumeParks;

  /** The keyed PTY manager — owns every live `NyxPty` + the lossless flow control. */
  readonly ptys: PtyManager;

  /**
   * The managed-command MANAGER (the runner the host owns, parity with the Tauri
   * `ManagedCommandRunner`). Built over the shared pool; the SAME runner backs the MCP
   * runtime command tools. Drives boot-restore + shutdown-snapshot/reap.
   */
  readonly commands: CommandManager;

  constructor(
    /** The loaded `nyx-napi` addon — loaded ONLY in this host, never in main/renderer. */
    readonly napi: NyxNapi,
    /** Frontier 2 — resolved paths. */
    readonly paths: ElectronAppPaths,
    /** Frontier 1 — events out to main/renderer. */
    readonly events: ElectronEventSink,
  ) {
    // Open the DB pool under the resolved data dir (the SAME `nyx.db` name + location
    // the Tauri shell uses). Migrations run inside `NyxCore.open`.
    this.core = new napi.NyxCore(paths.dataDir());
    this.execState = new ExecStatePersister(this.core, events);
    this.resumeParks = new ResumeParks(this.core);
    // The PTY manager forwards each OSC 133 exec-state transition through the persister
    // (DB is the authority for the badge after a restart) and injects any parked agent
    // resume at a terminal's first respawn (parity). It also holds the shared-core handle so
    // it can RETRACT the record↔live-PTY liveness binding on PTY exit (Finding C parity).
    this.ptys = new PtyManager(napi, events, this.execState, this.resumeParks, this.core);
    // Build the managed-command runner over the shared pool. Constructing it BEFORE
    // `mcpStart` (the lifecycle order) means the MCP runtime command tools route onto
    // this runner from the first request (no `mcp_unavailable`).
    this.commands = new CommandManager(this.core, events);
  }
}
