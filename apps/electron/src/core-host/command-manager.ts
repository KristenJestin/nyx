/**
 * The core-host's MANAGED-COMMAND MANAGER — the Electron mirror of the Tauri
 * `ManagedCommandRunner` + its lifecycle wiring (`manage_command_runner`,
 * `restore_commands_on_boot`, `snapshot_commands_on_shutdown`, the shutdown reap).
 *
 * It builds ONE `NyxCommandRunner` over the shared r2d2 pool (via
 * `NyxCore.createCommandRunner`), forwards the runner's two Node callbacks to the
 * EventSink (so the renderer's command band sees `command://state` / `command://ack` /
 * `command://output-cleared` / `command://output` at parity), and exposes the lifecycle
 * surface the host's IPC handlers + boot/shutdown sequence drive. The SAME runner backs
 * the MCP runtime command tools (the napi dispatcher captured it at `createCommandRunner`
 * time), so an agent and the UI pilot ONE runtime.
 *
 * This module runs ONLY in the dedicated Node-pure host (it touches the `.node`),
 * never in main/renderer.
 */
import type { ElectronEventSink } from "./event-sink";
import type {
  CommandOutputEvent,
  CommandStateEvent,
  CommandStatus,
  NyxCommandRunnerInstance,
  NyxCoreInstance,
} from "./napi";

export class CommandManager {
  private readonly runner: NyxCommandRunnerInstance;

  constructor(core: NyxCoreInstance, events: ElectronEventSink) {
    // Build the runner over the shared pool + Node callbacks. The addon stashes the
    // runner so the MCP runtime command tools route onto this SAME instance.
    this.runner = core.createCommandRunner(
      // on_state: a run-state transition, an unread ack, or an output-cleared tick.
      // `ErrorStrategy::Fatal` → the value is the LAST argument.
      (...args: unknown[]) => {
        const ev = args[args.length - 1] as CommandStateEvent | undefined;
        if (!ev) return;
        const event = ev.kind; // "state" | "ack" | "output-cleared"
        events.commandState(event, ev.instanceId, ev.state, ev.exitCode ?? null);
      },
      // on_output: a coalesced output chunk for an instance.
      (...args: unknown[]) => {
        const ev = args[args.length - 1] as CommandOutputEvent | undefined;
        if (!ev) return;
        events.commandOutput(ev.instanceId, ev.bytes);
      },
    );
  }

  /** Start an instance (idempotent on a running one). Resolves cmd+cwd from the DB. */
  start(instanceId: string): CommandStatus {
    return this.runner.start(instanceId);
  }

  /** Stop an instance (tree-kill). Idempotent on a non-running one. */
  stop(instanceId: string): CommandStatus {
    return this.runner.stop(instanceId);
  }

  /** Relaunch an instance (the explicit restart; never two live processes). */
  relaunch(instanceId: string): CommandStatus {
    return this.runner.relaunch(instanceId);
  }

  /** Read an instance's captured output (live tail while running, else persisted). */
  getOutput(instanceId: string): string {
    return this.runner.getOutput(instanceId);
  }

  /** Acknowledge a finished one-shot's unseen result: clear `unread` + emit
   * `command://ack` (parity with the Tauri `command_acknowledge`), never touching the
   * factual outcome. Returns the factual `last_state` string after the call. */
  acknowledge(instanceId: string): string {
    return this.runner.acknowledge(instanceId);
  }

  /** The live run status of an instance (no mutation). */
  status(instanceId: string): CommandStatus {
    return this.runner.status(instanceId);
  }

  /**
   * BOOT RESTORE (parity with the Tauri `setup`): relaunch every instance whose
   * template `restart_on_startup` is ON and whose `was_running_on_shutdown` snapshot is
   * true, normalize orphaned `running` to idle, and reset the snapshots. Returns the
   * relaunched ids. Drives the shell-agnostic `nyx_core::command::restore_commands_on_boot`.
   */
  restoreOnBoot(): string[] {
    return this.runner.restoreOnBoot();
  }

  /**
   * SHUTDOWN (parity with the Tauri close hook): LATCH so this runs exactly once, then
   * SNAPSHOT which instances are running (so the next boot relaunches exactly those),
   * then tree-kill EVERY live process so nothing is orphaned past exit. Safe to call on
   * both the close-request AND the destroy event (the latch guards the double-event).
   */
  snapshotAndReap(): void {
    // The latch (begin_shutdown) ensures the snapshot is taken BEFORE the reap and only
    // once — a second snapshot after the reap would see every instance idle and wrongly
    // clear `was_running_on_shutdown`, breaking restart-on-startup.
    if (!this.runner.beginShutdown()) return;
    this.runner.snapshotOnShutdown();
    this.runner.killAllRunning();
  }
}
