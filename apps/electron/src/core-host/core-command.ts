/**
 * The core-host ROUTER for the non-PTY nyxBridge request surface (PRD-5 review — the
 * full contract end-to-end over the real Electron IPC). Main forwards every contract
 * `BackendCommand` that is not a `pty_*` as a `core-command` host request; this router
 * sends it to the right AUTHORITY the host owns:
 *
 *   - the managed-command RUNTIME (`command_start` / `command_stop` / `command_relaunch`
 *     / `command_output` / `command_acknowledge`) → the live {@link CommandManager}
 *     runner that owns the off-screen command PTYs (parity with the Tauri
 *     `ManagedCommandRunner`-backed commands);
 *   - everything else DB-backed (terminals / projects / workspaces / templates /
 *     instances / agent sessions CRUD) → the napi `NyxCore.dbCommand` dispatcher, an
 *     AsyncTask off the Node loop that resolves a JSON result string we parse back to the
 *     contract shape.
 *
 * The split mirrors Tauri exactly: there the runtime commands take the
 * `ManagedCommandRunner` state while the rest are thin `db` wrappers. Here the runner is
 * the host's `CommandManager` and the `db` wrappers live behind `dbCommand`.
 *
 * A third authority is the host-owned LIVE-PTY / record↔PTY state (NOT the DB): the
 * `terminal_info` auto-label poll (`/proc` via the live `NyxPty` the PTY manager owns),
 * `register_terminal_pty` (the synchronous record↔live-PTY liveness registry the MCP
 * dispatcher consults), and `auto_attach_terminal` (the shared nyx-core resolver). The
 * review found these three had been STUBBED to benign defaults in main, silently killing
 * the auto-label, the cwd auto-attach, and the MCP liveness binding; they are routed here
 * now onto the real logic. Any remaining command with NO authority (a not-yet-ported
 * `integration_*`) falls through to `dbCommand`, which returns a readable "not available
 * over this transport" error — the allowlist stays honest.
 */
import type { HostServices } from "./services";

/** The runtime (runner-backed) commands — routed onto the live `CommandManager`. */
type RuntimeCommand =
  | "command_start"
  | "command_stop"
  | "command_relaunch"
  | "command_output"
  | "command_acknowledge";

function isRuntimeCommand(command: string): command is RuntimeCommand {
  return (
    command === "command_start" ||
    command === "command_stop" ||
    command === "command_relaunch" ||
    command === "command_output" ||
    command === "command_acknowledge"
  );
}

/**
 * Route a `core-command` to its authority and resolve the result ALREADY in the contract
 * shape (the value the renderer's `invoke` casts to its expected type). Throws a readable
 * error for an unknown/unsupported command or a backend failure — main maps that into the
 * adapter's `command` BridgeError.
 *
 * `argsJson` is the contract args serialized to JSON (`"{}"` for the no-arg commands).
 */
export async function handleCoreCommand(
  services: HostServices,
  command: string,
  argsJson: string,
): Promise<unknown> {
  if (isRuntimeCommand(command)) {
    // The runtime commands take a single `instanceId` arg and return a string (the
    // factual state, or — for `command_output` — the captured output). Parity with the
    // Tauri command bodies that return `Result<String, String>`.
    const args = parseArgs(command, argsJson);
    const instanceId = requireString(command, args, "instanceId");
    switch (command) {
      case "command_start":
        return services.commands.start(instanceId).state;
      case "command_stop":
        return services.commands.stop(instanceId).state;
      case "command_relaunch":
        return services.commands.relaunch(instanceId).state;
      case "command_output":
        return services.commands.getOutput(instanceId);
      case "command_acknowledge":
        return services.commands.acknowledge(instanceId);
    }
  }

  // LIVE-PTY / record↔PTY commands — they touch host-owned PTY state, not the DB
  // dispatcher. Routed here so the auto-label poll, auto-attach, and the MCP liveness
  // registry are REAL features again (the review found these stubbed to benign defaults).
  if (command === "terminal_info") {
    // `{ id: ptyId }` → live `{ cwd, foreground }`. Resolved by the PTY manager (it owns the
    // live `NyxPty`), read straight from the kernel (Linux `/proc`); `{ null, null }` on
    // Windows / an exited pty WITHOUT erroring (so the per-second poll never spams).
    const args = parseArgs(command, argsJson);
    const ptyId = Number(args.id);
    return services.ptys.terminalInfo(ptyId);
  }
  if (command === "register_terminal_pty") {
    // `{ recordId, ptyId }` → feed the SYNCHRONOUS record↔live-PTY liveness registry the
    // MCP dispatcher reads (so `send_to_terminal` / `list_terminals` see live shells). A
    // null `ptyId` retracts the join (the Tauri `TerminalPtyMap::clear` behaviour).
    const args = parseArgs(command, argsJson);
    const recordId = requireString(command, args, "recordId");
    const ptyId = args.ptyId;
    if (typeof ptyId === "number") {
      services.core.registerTerminalPty(recordId, ptyId);
    } else {
      services.core.unregisterTerminalPty(recordId);
    }
    return null;
  }
  if (command === "auto_attach_terminal") {
    // `{ terminalId, cwd }` → run the shared nyx-core auto-attach resolver + persist the
    // decided binding (off the Node loop); `{ workspaceId, changed }` back to the front,
    // shaped to the snake_case the renderer reads.
    const args = parseArgs(command, argsJson);
    const terminalId = requireString(command, args, "terminalId");
    const cwd = typeof args.cwd === "string" ? args.cwd : null;
    const res = await services.core.autoAttachTerminal(terminalId, cwd);
    // Coerce a `None` workspace to an explicit `null` (the contract shape the renderer
    // reads): napi maps `Option::None` to `undefined`, which JSON drops — so the front
    // would see a missing key. The Tauri command always serializes `workspace_id` present.
    return { workspace_id: res.workspaceId ?? null, changed: res.changed };
  }
  // DB-backed: the napi dispatcher returns a JSON string we parse back to the contract
  // shape (a `Terminal[]`, `Project[]`, `null`, …). Runs off the Node loop (AsyncTask).
  const resultJson = await services.core.dbCommand(command, argsJson || "{}");
  const result = JSON.parse(resultJson);
  // Parity with the Tauri `emit_commands_changed`: a TEMPLATE mutation (create / update /
  // delete / resync / unlink / import) broadcasts `commands://changed` so every
  // command-band surface re-pulls — including a surface (e.g. an MCP-driven mutation, or
  // the modal that never invoked this) that did not refresh locally. Emitted ONLY after a
  // successful mutation (a throw above skips it). The napi dispatcher runs off the Node
  // loop with no EventSink, so the broadcast lives here on the Node loop.
  if (mutatesCommandTemplates(command)) {
    services.events.changed("commands");
  } else if (mutatesWorkspaceTree(command)) {
    services.events.changed("workspaces");
  }
  return result;
}

/** The template-MUTATING DB commands — each broadcasts `commands://changed` on success
 *  (parity with the Tauri commands that call `emit_commands_changed`). The reads
 *  (`command_list` / `command_instance_list` / `command_import_scripts`) and the runtime
 *  commands do NOT. */
function mutatesCommandTemplates(command: string): boolean {
  switch (command) {
    case "command_create":
    case "command_update":
    case "command_delete":
    case "command_resync_source":
    case "command_unlink_source":
    case "command_import_create":
      return true;
    default:
      return false;
  }
}

/** The project/workspace TREE-MUTATING DB commands that broadcast `workspaces://changed`
 *  on success — parity with the Tauri commands that call `emit_workspaces_changed`
 *  (`create_project` / `delete_project` / `create_workspace`). The collapse/rename/resume
 *  toggles refresh locally and do NOT broadcast (matching Tauri). */
function mutatesWorkspaceTree(command: string): boolean {
  switch (command) {
    case "create_project":
    case "delete_project":
    case "create_workspace":
      return true;
    default:
      return false;
  }
}

/** Parse the contract args blob to an object, with a readable error on a bad blob. */
function parseArgs(command: string, argsJson: string): Record<string, unknown> {
  if (!argsJson) return {};
  try {
    const v = JSON.parse(argsJson) as unknown;
    return v && typeof v === "object" ? (v as Record<string, unknown>) : {};
  } catch (e) {
    throw new Error(`${command}: bad args json: ${(e as Error).message}`);
  }
}

/** Read a required string arg, or throw a readable error. */
function requireString(command: string, args: Record<string, unknown>, name: string): string {
  const v = args[name];
  if (typeof v !== "string") throw new Error(`${command}: missing string arg '${name}'`);
  return v;
}
