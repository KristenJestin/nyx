/**
 * Entry of the dedicated CORE-HOST — the Node-pure process that owns `nyx-napi` and
 * the PTY. Spawned by the Electron main via the Electron binary with
 * `ELECTRON_RUN_AS_NODE=1` (PRD frozen decision: NOT `utilityProcess`; a PTY fork
 * from the Chromium main process SIGSEGVs — POC §B.1/§J). Running the SAME Electron
 * binary as plain Node guarantees the `.node` ABI matches (the addon is built
 * against this Electron's embedded Node ABI).
 *
 * Transport: Node's built-in IPC channel (`process.send` / `process.on('message')`),
 * available under `ELECTRON_RUN_AS_NODE` when main spawns us with an `'ipc'` stdio
 * slot. Messages are the typed `HostMessage` union (`../shared/host-protocol`);
 * requests are correlated by `id`, events are fire-and-forget.
 *
 * Boot: read the `HostBootConfig` (data/resource dirs resolved by main, where `app`
 * exists), run the lifecycle `boot` (load napi → resolve paths → assemble the
 * frontiers), then announce `ready`. A boot failure (e.g. the `.node` won't load)
 * emits `fatal` and exits non-zero — never an infinite load (task #25).
 */
import { boot, isNodePure, shutdown } from "./lifecycle";
import { handleCoreCommand as handleCoreCommandRouted } from "./core-command";
import type { HostServices } from "./services";
import type {
  CoreCommandRequest,
  HostBootConfig,
  HostEventPayload,
  HostRequest,
  HostResponse,
  PingResult,
  PtyAckRequest,
  PtyCloseRequest,
  PtyResizeRequest,
  PtySpawnRequest,
  PtyWriteRequest,
} from "../shared/host-protocol";

/** Send one event toward main (no-op if the channel is gone — main may have died). */
function emit(payload: HostEventPayload): void {
  process.send?.({ type: "evt", payload });
}

/** Reply to a correlated request. */
function reply(res: HostResponse): void {
  process.send?.(res);
}

/** Parse the boot config main passed via env (a single JSON blob). */
function readBootConfig(): HostBootConfig {
  const raw = process.env.NYX_HOST_CONFIG;
  if (!raw) {
    // Allow a bare/dev spawn: default to a cwd-local data dir, no resources.
    return { dataDir: process.cwd(), resourceDir: null };
  }
  return JSON.parse(raw) as HostBootConfig;
}

function main(): void {
  // Refuse to run if we somehow ended up in a Chromium process — this code must
  // NEVER load the `.node` outside a pure-Node context (the whole point of the host).
  if (!isNodePure()) {
    emit({ kind: "fatal", error: "core-host started in a non-Node (Chromium) process" });
    process.exit(2);
  }

  let services: HostServices | null = null;
  let bootInfo: PingResult | null = null;
  try {
    const result = boot(readBootConfig(), emit);
    services = result.services;
    bootInfo = result.info;
    // TEST-ONLY: simulate a boot HANG (the host loads but never readies), so main's
    // BOUNDED boot handshake (timeout → fatal, never an infinite load) is verifiable.
    // We deliberately do NOT emit `ready`; main times out and kills us.
    if (process.env.NYX_HOST_FORCE_BOOT_HANG === "1") {
      return;
    }
    emit({ kind: "ready", info: result.info });
  } catch (e) {
    emit({ kind: "fatal", error: `boot failed: ${(e as Error).message}` });
    process.exit(1);
  }

  // --- request handlers ------------------------------------------------------
  function handlePtySpawn(req: PtySpawnRequest): unknown {
    if (!services) throw new Error("host not booted");
    const ptyId = services.ptys.spawn({
      cols: req.cols,
      rows: req.rows,
      cwd: req.cwd,
      terminalId: req.terminalId,
    });
    return { ptyId };
  }

  function handlePtyWrite(req: PtyWriteRequest): unknown {
    if (!services) throw new Error("host not booted");
    services.ptys.write(req.ptyId, Buffer.from(req.dataB64, "base64"));
    return null;
  }

  function handlePtyResize(req: PtyResizeRequest): unknown {
    if (!services) throw new Error("host not booted");
    services.ptys.resize(req.ptyId, req.cols, req.rows);
    return null;
  }

  function handlePtyClose(req: PtyCloseRequest): unknown {
    if (!services) throw new Error("host not booted");
    services.ptys.close(req.ptyId);
    return null;
  }

  function handlePtyAck(req: PtyAckRequest): void {
    // Fire-and-forget flow-control credit (no reply). A missing host or unknown
    // pty id is a harmless no-op.
    services?.ptys.ack(req.ptyId, req.bytes);
  }

  /**
   * Handle a `core-command` (the non-PTY nyxBridge surface). ASYNC: the DB dispatcher is
   * a napi AsyncTask (off the Node loop), so we reply when it settles — a correlated
   * reply with the same `id`, ok+result or a readable error. Routing (runtime runner vs
   * DB) lives in `handleCoreCommand`.
   */
  function handleCoreCommand(id: number, req: CoreCommandRequest): void {
    if (!services) {
      reply({ type: "res", id, ok: false, error: "host not booted" });
      return;
    }
    handleCoreCommandRouted(services, req.command, req.argsJson)
      .then((result) => reply({ type: "res", id, ok: true, result }))
      .catch((e) => reply({ type: "res", id, ok: false, error: (e as Error).message }));
  }

  process.on("message", (msg: HostRequest) => {
    if (!msg || msg.type !== "req") return;
    const { id, payload } = msg;
    try {
      let result: unknown = null;
      switch (payload.kind) {
        case "ping":
          // Return the cached boot proof bundle (same shape as `ready.info`); no
          // re-boot — the addon is already loaded and paths resolved.
          result = bootInfo;
          break;
        case "pty-spawn":
          result = handlePtySpawn(payload);
          break;
        case "pty-write":
          result = handlePtyWrite(payload);
          break;
        case "pty-resize":
          result = handlePtyResize(payload);
          break;
        case "pty-close":
          result = handlePtyClose(payload);
          break;
        case "pty-ack":
          // Fire-and-forget: credit the flow-control loop, send NO reply (the
          // renderer does not await it, so a reply would just be a stray message).
          handlePtyAck(payload);
          return;
        case "core-command":
          // The non-PTY surface (DB-backed + managed-command runtime). ASYNC — it
          // replies itself when the routed work settles, so return WITHOUT the
          // synchronous reply below (which would double-reply this id).
          handleCoreCommand(id, payload);
          return;
        case "shutdown":
          // TEST-ONLY: ignore the shutdown request entirely, so main's FORCED
          // (kill-after-timeout) cleanup path is verifiable (no orphan must survive).
          if (process.env.NYX_HOST_IGNORE_SHUTDOWN === "1") {
            return; // no reply, no exit — main will time out and kill us.
          }
          // Ordered teardown: snapshot (emits the `changed` marker) → stop PTY → exit.
          shutdown(services, emit);
          reply({ type: "res", id, ok: true, result: null });
          // Give the reply + snapshot event a tick to flush, then exit cleanly.
          setImmediate(() => process.exit(0));
          return;
        case "__crash":
          // TEST-ONLY abrupt crash (no ordered shutdown), guarded by an env flag so a
          // production host can never be crashed via IPC.
          if (process.env.NYX_HOST_ALLOW_CRASH === "1") {
            process.exit(101); // non-zero, no `stopped` — looks like a real crash.
          }
          throw new Error("__crash is disabled (set NYX_HOST_ALLOW_CRASH=1 to allow)");
      }
      reply({ type: "res", id, ok: true, result });
    } catch (e) {
      reply({ type: "res", id, ok: false, error: (e as Error).message });
    }
  });

  // If the parent (main) goes away, tear down so no orphan host survives (task #25's
  // forced-cleanup path; full tree-kill of children is phase 3/5). The channel is
  // gone, so no events can flush — just stop the PTY and exit.
  process.on("disconnect", () => {
    shutdown(services);
    process.exit(0);
  });
}

main();
