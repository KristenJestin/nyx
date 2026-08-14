/**
 * MAIN-side manager of the supervised `nyx-sidecar` Rust binary (Phase 3, G0).
 *
 * This REPLACES the transitional TS `core-host` (which loaded `nyx-napi`): per PDR-0006 /
 * PDR-0007 the runtime authority is now a single Rust sidecar — a direct, supervised CHILD
 * of Electron main, NOT a daemon, never surviving the app. Main owns the local
 * `stdin`/`stdout` PIPE to it (versioned, length-prefixed MessagePack frames — the
 * `../shared/pipe-protocol.ts` codec, the byte-exact mirror of the sidecar's Rust wire);
 * `stderr` carries only the sidecar's logs.
 *
 * Responsibilities:
 *   - SPAWN the sidecar binary with the resolved data/resource dirs (env), wire its three
 *     stdio pipes, and own its lifetime relative to the app.
 *   - drive the BOOT HANDSHAKE (a versioned `handshake` request → `handshake_ok`), BOUNDED
 *     by a timeout so a broken sidecar is a readable `fatal` state, never an infinite load.
 *   - correlate REQUEST/RESPONSE by a monotonic id, and demux unsolicited EVENTS
 *     (terminal output / pty lifecycle / command lifecycle / changed invalidations) to the
 *     registered event listeners.
 *   - translate the renderer's CONTRACT command names (`pty_spawn`, `list_terminals`, …)
 *     into the right typed pipe request, and the pipe events into the contract
 *     `{ event, payload }` envelopes — so the renderer's nyxBridge speaks the SAME surface
 *     it spoke to the core-host; only the transport underneath changed.
 *   - ordered SHUTDOWN (a `shutdown` request, then force-kill if it lingers) so no orphan
 *     sidecar / PTY survives.
 *
 * The pipe is BINARY: PTY output rides as MessagePack `bin` (no base64 per chunk), so the
 * relay decodes the bytes once and hands the renderer a `number[]` (the contract's JSON
 * wire shape the adapter normalizes to `Uint8Array`).
 */
import { spawn, type ChildProcess } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { app } from "electron";

import {
  decodeMessage,
  encodeMessageFrame,
  FrameDecoder,
  MAX_FRAME_LEN,
  PROTOCOL_VERSION,
  type Event as PipeEvent,
  type Message,
  type Response as PipeResponse,
} from "../shared/pipe-protocol";
import type { CoreEventEnvelope } from "../shared/ipc";
import { windowControlsVisible } from "./env";

/** Re-export the max frame length so a smoke / inspector can read the cap. */
export { MAX_FRAME_LEN };

/**
 * PURE data-dir resolution (testable without an Electron `app`): the `NYX_DATA_DIR` override
 * (the e2e harness pins it) else the Electron `userData` dir. SAFETY (review #37): `userData`
 * is the value main PINNED in code via `applyAppIdentity` (`<appData>/nyx-v2`, e.g.
 * `~/.config/nyx-v2`), NEVER the live v1 dir `~/.config/@nyx/electron`. So even with
 * `NYX_DATA_DIR` UNSET, this resolves to a `nyx-v2`-namespaced dir — the override is no longer
 * load-bearing for safety. Factored out so a test can assert the unset-override path yields the
 * `nyx-v2` userData WITHOUT touching the real Electron app / the real userData.
 */
export function resolveDataDirFrom(override: string | undefined, userData: string): string {
  if (override && override.length > 0) return override;
  return userData;
}

/**
 * Resolve the writable data dir for the sidecar. Thin wrapper over the pure
 * {@link resolveDataDirFrom} with the live `NYX_DATA_DIR` + Electron's `userData` (which main
 * pinned to `<appData>/nyx-v2` in `applyAppIdentity`). Mirrors the v1 `resolve_data_dir`; the
 * sidecar itself resolves the same precedence, but main passes it explicitly so a dev/packaged
 * run and a test pin agree.
 */
export function resolveDataDir(): string {
  return resolveDataDirFrom(process.env.NYX_DATA_DIR, app.getPath("userData"));
}

/** The platform-specific sidecar executable file name (`.exe` on Windows). */
export function sidecarExeName(platform: NodeJS.Platform = process.platform): string {
  return platform === "win32" ? "nyx-sidecar.exe" : "nyx-sidecar";
}

/**
 * PURE resource-dir derivation (per-OS, testable without an Electron `app`). In a packaged
 * build electron-builder unpacks the `asarUnpack` globs to `<resources>/app.asar.unpacked`,
 * preserving the `dist/` prefix, so the resource ROOT is `…/app.asar.unpacked/dist` — the dir
 * whose `resources/claude-plugin` subpath nyx-core resolves. In dev (`isPackaged=false`) there
 * is no packaged resource dir → null (the sidecar falls back to its source tree). `resourcesPath`
 * is `process.resourcesPath` (on macOS `Foo.app/Contents/Resources`, on Linux/Windows
 * `<install>/resources`), so this is correct on all three OS. `sep` lets tests pin the
 * separator; defaults to the host's.
 */
export function deriveResourceDir(
  isPackaged: boolean,
  resourcesPath: string,
  sep: string = path.sep,
): string | null {
  if (!isPackaged) return null;
  return [resourcesPath, "app.asar.unpacked", "dist"].join(sep);
}

/**
 * Collapse `.` / `..` segments in `p`, honouring the EXPLICIT `sep` (so a Windows `\` path can
 * be normalized on a POSIX host and vice-versa). We CANNOT use `path.normalize` here: it always
 * resolves against the host's `path.sep`, so on a POSIX CI box it would leave a Windows
 * `…\main\..\sidecar` un-collapsed (and on Windows it would mangle a POSIX `/` path). This pure
 * collapse keeps the function correct for BOTH separators — the whole point of the injectable
 * `sep`. A leading separator (absolute POSIX path) and a drive prefix (`C:`) are preserved; a
 * `..` that has no parent to pop is kept verbatim (defensive — should not happen for our inputs).
 */
function collapseDotSegments(p: string, sep: string): string {
  const hasLeadingSep = p.startsWith(sep);
  const out: string[] = [];
  for (const seg of p.split(sep)) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") {
      const top = out[out.length - 1];
      // Pop the parent unless there is none, or the top is itself an un-poppable `..`.
      if (out.length > 0 && top !== "..") out.pop();
      else if (!hasLeadingSep) out.push("..");
      continue;
    }
    out.push(seg);
  }
  return (hasLeadingSep ? sep : "") + out.join(sep);
}

/**
 * PURE sidecar-binary path derivation (per-OS, testable). Given the dir the bundled
 * `sidecar.js` runs from (`__dirname`), the packaged flag, the exe name, and an optional
 * override, returns the resolved binary path. Precedence:
 *   1. `override` (`NYX_SIDECAR_BIN`) — the e2e harness / a dev run point it at the cargo
 *      target binary. Wins unconditionally.
 *   2. packaged: the unpacked `app.asar.unpacked/dist/sidecar/nyx-sidecar[.exe]` (outside the
 *      asar, signed with the app where required — PDR-0007). The bundled main lives at
 *      `app.asar/dist/main/sidecar.js`, so `<dirname>/../sidecar` lands at
 *      `app.asar/dist/sidecar`; we REDIRECT that to the `app.asar.unpacked` sibling (a real
 *      file — a native binary cannot be spawned from inside the archive). This is the
 *      production resolution from `process.resourcesPath`, NOT a dev/source path.
 *   3. dev: `dist/sidecar/nyx-sidecar[.exe]` next to the bundled main.
 * `sep` lets tests pin the separator (e.g. assert the Windows `\` rewrite on a POSIX host).
 *
 * CRITICAL (review `01KWD96WAD7MAPWM9Y5S56G138`): the returned path is NORMALIZED — the `..`
 * from the `<dirname>/../sidecar` join is COLLAPSED before returning, so the path contains NO
 * `..` segment (and thus no `dist/main` segment in the packaged case). Without this, the
 * packaged path was `…/app.asar.unpacked/dist/main/../sidecar/nyx-sidecar`: under
 * `app.asar.unpacked/` only `dist/sidecar/**` and `dist/resources/**` are unpacked, so the
 * `dist/main` segment does NOT exist there and the OS cannot traverse `dist/main/..` → ENOENT
 * → "sidecar binary not found". Collapsing the `..` yields the real
 * `…/app.asar.unpacked/dist/sidecar/<exe>`, which exists on disk and is spawnable WITHOUT the
 * `NYX_SIDECAR_BIN` override.
 */
export function deriveSidecarBin(
  dirname: string,
  isPackaged: boolean,
  exe: string,
  override: string | undefined,
  sep: string = path.sep,
): string {
  if (override && override.length > 0) return override;
  let bin = [dirname, "..", "sidecar", exe].join(sep);
  const asarSeg = `app.asar${sep}`;
  const unpackedSeg = `app.asar.unpacked${sep}`;
  if (isPackaged && bin.includes(asarSeg) && !bin.includes(unpackedSeg)) {
    bin = bin.replace(asarSeg, unpackedSeg);
  }
  // Collapse the `..` (and any `.`) so the RETURNED path is the real on-disk path the OS will
  // spawn — never an un-normalized `…/dist/main/../sidecar/…` that ENOENTs under the unpacked
  // layout. Done for BOTH the packaged and dev branches (a clean dev path is harmless).
  return collapseDotSegments(bin, sep);
}

/**
 * Resolve the read-only resource dir (the unpacked, outside-asar resources). Thin wrapper over
 * the pure {@link deriveResourceDir} with the live Electron `app.isPackaged` +
 * `process.resourcesPath`.
 */
export function resolveResourceDir(): string | null {
  return deriveResourceDir(app.isPackaged, process.resourcesPath);
}

/**
 * Resolve the sidecar BINARY path from the Electron RESOURCES in a packaged build (or the dev
 * staging otherwise). Thin wrapper over the pure {@link deriveSidecarBin} with the live
 * `__dirname` (where the bundled `sidecar.js` runs), `app.isPackaged`, and the
 * `NYX_SIDECAR_BIN` override.
 */
export function resolveSidecarBin(): string {
  return deriveSidecarBin(__dirname, app.isPackaged, sidecarExeName(), process.env.NYX_SIDECAR_BIN);
}

/** A pending correlated request awaiting its reply. */
interface Pending {
  resolve: (value: PipeResponse) => void;
  reject: (err: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

/** Listener for sidecar→main events, already mapped to the contract envelope. */
export type CoreEventListener = (envelope: CoreEventEnvelope) => void;

/**
 * The observable sidecar lifecycle state. Mirrors the core-host's so `index.ts`'s
 * surfacing logic is unchanged.
 *   - `starting`   — spawned, boot handshake in flight.
 *   - `ready`      — handshake succeeded; serving.
 *   - `recovering` — a crash was detected and the SINGLE automatic relaunch is in flight.
 *   - `degraded`   — an unexpected exit we did NOT (or no longer) auto-restart. Surfaced;
 *                    NOT a hang and NOT a restart loop.
 *   - `fatal`      — boot failed (timeout / spawn error / handshake error). Terminal.
 *   - `stopping`   — ordered shutdown in progress; new requests refused.
 *   - `stopped`    — fully down.
 */
export type SidecarState =
  | "idle"
  | "starting"
  | "ready"
  | "recovering"
  | "degraded"
  | "fatal"
  | "stopping"
  | "stopped";

export interface SidecarStateChange {
  state: SidecarState;
  reason?: string;
}

export type SidecarStateListener = (change: SidecarStateChange) => void;

/** How long the boot handshake may take before we declare `fatal`. Overridable via
 *  `NYX_HOST_BOOT_TIMEOUT_MS` (operational knob + lets tests shorten the wait); 10s. */
function bootTimeoutMs(): number {
  const raw = Number(process.env.NYX_HOST_BOOT_TIMEOUT_MS);
  return Number.isFinite(raw) && raw > 0 ? raw : 10_000;
}

export class Sidecar {
  private child: ChildProcess | null = null;
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private readonly eventListeners = new Set<CoreEventListener>();
  private readonly stateListeners = new Set<SidecarStateListener>();
  private readonly decoder = new FrameDecoder();

  private state: SidecarState = "idle";
  private stateReason: string | undefined;
  /** Latched true while WE initiated the shutdown (so its exit is not a crash). */
  private intentionalStop = false;
  /**
   * CRASH-RECOVERY relaunch latch (Phase 4 — `01KWCJPFP9JSJMRDZGW2E0N905`): how many times
   * we have already relaunched the sidecar after an UNEXPECTED crash. We relaunch EXACTLY
   * ONCE (never a blind/looping restart): the first crash sets this to 1 and triggers one
   * relaunch; a second crash (this already ≥ the cap) leaves the sidecar `degraded`, never
   * looping. Reset on a successful `start()` so a later, unrelated crash gets its own single
   * relaunch.
   */
  private crashRelaunchCount = 0;
  /** Hard cap on automatic post-crash relaunches: a SINGLE relaunch, never a restart loop. */
  private static readonly MAX_CRASH_RELAUNCHES = 1;
  /** Set true while a post-crash relaunch is in flight, so concurrent triggers don't double. */
  private relaunching = false;
  /** The MCP port the LIVE sidecar bound (see {@link mcpPort}). */
  private lastMcpPort = 0;
  /** Resolver for the in-flight boot handshake, settled by `handshake_ok`/timeout/error. */
  private bootSettle: { resolve: () => void; reject: (e: Error) => void } | null = null;
  private bootTimer: ReturnType<typeof setTimeout> | null = null;
  /** The id of the in-flight handshake request, so its reply drives the boot. */
  private handshakeId = 0;

  /** Subscribe to mapped sidecar events (contract `{ event, payload }` envelopes). */
  onEvent(listener: CoreEventListener): () => void {
    this.eventListeners.add(listener);
    return () => this.eventListeners.delete(listener);
  }

  /** Subscribe to lifecycle state changes (main surfaces these). */
  onState(listener: SidecarStateListener): () => void {
    this.stateListeners.add(listener);
    return () => this.stateListeners.delete(listener);
  }

  get currentState(): SidecarState {
    return this.state;
  }

  get currentStateReason(): string | undefined {
    return this.stateReason;
  }

  /** Whether the sidecar child is alive (spawned and not exited). */
  get alive(): boolean {
    return this.child !== null;
  }

  /** The live child's PID (for the smoke / lifecycle to assert identity). */
  get pid(): number | undefined {
    return this.child?.pid;
  }

  private setState(state: SidecarState, reason?: string): void {
    this.state = state;
    this.stateReason = reason;
    for (const l of this.stateListeners) l({ state, reason });
  }

  /**
   * Spawn the sidecar AND await its boot handshake, BOUNDED by the boot timeout. Resolves
   * once the sidecar replies `handshake_ok`; rejects (state `fatal`) on a boot timeout, a
   * spawn error, or a handshake error (e.g. a protocol-version mismatch). Never hangs.
   *
   * An EXPLICIT (app-lifecycle) start clears the crash-relaunch latch, so a later, unrelated
   * crash gets its OWN single relaunch. The automatic post-crash relaunch goes through
   * `relaunchAfterCrash()` instead, which does NOT clear the latch (so the cap of a single
   * relaunch holds across the crash → relaunch → crash-again sequence).
   */
  start(): Promise<void> {
    this.crashRelaunchCount = 0;
    return this.spawnAndBoot();
  }

  /** The single automatic relaunch after a crash — preserves the relaunch latch so a second
   *  crash is NOT auto-relaunched (no restart loop). */
  private async relaunchAfterCrash(): Promise<void> {
    await this.spawnAndBoot();
    // …and START THE MCP SERVER AGAIN. `mcp_start` is issued once at boot by the caller of
    // `start()`, and a relaunched sidecar is a FRESH process that never received it: it comes
    // back serving the pipe (the UI works, so nothing looks wrong) with NO HTTP server bound.
    // Every agent then loses the nyx tools until the app is restarted by hand — observed live.
    // The supervisor owns the relaunch, so it owns restoring what the relaunch dropped.
    //
    // A bind failure stays a WARNING here exactly as it is at boot: a sidecar serving the pipe
    // without its MCP surface is degraded, not dead, and killing the recovery over it would
    // trade a partial loss for a total one.
    const port = await this.startMcp().catch((e) => {
      console.error(`[sidecar] MCP server did not restart after the crash relaunch: ${e}`);
      return 0;
    });
    if (port > 0) console.log(`[sidecar] MCP server re-bound on 127.0.0.1:${port} after relaunch`);
  }

  /** Spawn the sidecar + await the bounded boot handshake. Shared by `start()` (which first
   *  clears the latch) and the post-crash `relaunchAfterCrash()` (which preserves it). */
  private spawnAndBoot(): Promise<void> {
    if (this.child) throw new Error("sidecar already started");
    this.intentionalStop = false;
    this.setState("starting");

    const bin = resolveSidecarBin();
    if (!process.env.NYX_SIDECAR_BIN && !fs.existsSync(bin)) {
      const reason = `sidecar binary not found at ${bin}`;
      this.setState("fatal", reason);
      return Promise.reject(new Error(reason));
    }

    const dataDir = resolveDataDir();
    const resourceDir = resolveResourceDir();
    this.child = spawn(bin, [], {
      env: {
        ...process.env,
        NYX_DATA_DIR: dataDir,
        ...(resourceDir ? { NYX_RESOURCE_DIR: resourceDir } : {}),
        // OPT IN the sidecar's boot-time Claude-plugin reconcile: when the MCP server binds
        // (on `startMcp()` below), the sidecar re-templates the installed plugin's `.mcp.json`
        // to the live MCP port + re-registers it via the `claude` CLI, so Claude Code connects
        // the nyx MCP (finding #44/#45/#47). This is a REAL-`~/.claude` mutation, so it is
        // opt-in — the app (which owns the integration) turns it on here; bare sidecar test
        // spawns leave it off and stay hermetic. Honor an explicit user override if present.
        NYX_RECONCILE_CLAUDE_PLUGIN: process.env.NYX_RECONCILE_CLAUDE_PLUGIN ?? "1",
      },
      // 0=stdin (frames in), 1=stdout (frames out), 2=stderr (logs to our stderr).
      stdio: ["pipe", "pipe", "inherit"],
    });

    this.child.on("error", (err) => {
      // Spawn-level failure (binary missing / not executable): readable fatal, never a hang.
      this.setState("fatal", `sidecar spawn error: ${err.message}`);
      this.failAllPending(`sidecar spawn error: ${err.message}`);
      this.settleBoot(err);
    });
    this.child.stdout?.on("data", (chunk: Buffer) => this.onStdout(chunk));
    this.child.on("exit", (code, signal) => this.onExit(code, signal));

    const timeout = bootTimeoutMs();
    const boot = new Promise<void>((resolve, reject) => {
      this.bootSettle = { resolve, reject };
      this.bootTimer = setTimeout(() => {
        const reason = `boot handshake timed out after ${timeout}ms`;
        this.setState("fatal", reason);
        this.intentionalStop = true;
        this.child?.kill();
        this.settleBoot(new Error(reason));
      }, timeout);
    });

    // Send the versioned handshake to open the pipe (the sidecar validates the version and
    // replies `handshake_ok` with its capabilities + max frame len, or an `error`).
    this.handshakeId = this.nextId++;
    this.send({ category: "request", kind: "handshake", id: this.handshakeId, protocolVersion: PROTOCOL_VERSION });
    return boot;
  }

  /** Frame-encode + write a logical message to the sidecar's stdin. */
  private send(message: Message): void {
    const stdin = this.child?.stdin;
    if (!stdin) return;
    stdin.write(encodeMessageFrame(message));
  }

  /** Drain whole frames out of the incremental decoder and dispatch each. */
  private onStdout(chunk: Buffer): void {
    this.decoder.push(new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength));
    for (;;) {
      let payload: Uint8Array | null;
      try {
        payload = this.decoder.nextFrame();
      } catch (e) {
        // An over-sized / malformed frame is unrecoverable: tear the pipe down.
        this.setState("fatal", `pipe decode error: ${(e as Error).message}`);
        this.intentionalStop = true;
        this.child?.kill();
        return;
      }
      if (!payload) break;
      let message: Message;
      try {
        message = decodeMessage(payload);
      } catch (e) {
        // A single bad message is logged but does not kill the pipe.
        console.error(`[main] sidecar message decode error: ${(e as Error).message}`);
        continue;
      }
      this.dispatch(message);
    }
  }

  /** Route a decoded message: settle a pending request, or fan an event out. */
  private dispatch(message: Message): void {
    if (message.category === "response") {
      // The handshake reply drives the boot.
      if (message.id === this.handshakeId && this.state === "starting") {
        if (message.kind === "handshake_ok") {
          this.setState("ready");
          this.settleBoot();
        } else if (message.kind === "error") {
          this.setState("fatal", message.message);
          this.settleBoot(new Error(message.message));
        }
        // The handshake has no caller pending entry; nothing else to do.
        if (!this.pending.has(message.id)) return;
      }
      this.settle(message);
    } else if (message.category === "event") {
      this.onPipeEvent(message);
    }
    // Requests FROM the sidecar are not acted on.
  }

  private settle(res: PipeResponse): void {
    const p = this.pending.get(res.id);
    if (!p) return;
    this.pending.delete(res.id);
    clearTimeout(p.timer);
    p.resolve(res);
  }

  private settleBoot(err?: Error): void {
    if (this.bootTimer) {
      clearTimeout(this.bootTimer);
      this.bootTimer = null;
    }
    const s = this.bootSettle;
    this.bootSettle = null;
    if (!s) return;
    if (err) s.reject(err);
    else s.resolve();
  }

  private onExit(code: number | null, signal: NodeJS.Signals | null): void {
    this.child = null;
    this.failAllPending(`sidecar exited (code=${code}, signal=${signal})`);
    if (this.intentionalStop) {
      this.setState("stopped");
      return;
    }
    // An exit we did NOT initiate is a CRASH (a panic / SIGKILL / OOM). The boot promise, if
    // still in flight, rejects so `start()`'s caller is never left hanging.
    const detail = `sidecar crashed (code=${code}, signal=${signal})`;
    this.settleBoot(new Error(detail));

    // CRASH RECOVERY (Phase 4): relaunch EXACTLY ONCE — never a blind/looping restart. The
    // relaunched sidecar reopens SQLite and rebuilds the persisted state; its boot crash
    // reconcile flags doubtful `running` instances `interrupted` and active sessions
    // `unknown` (the user chooses what to resume). A SECOND crash (the cap is reached) is
    // left `degraded` so the renderer surfaces it and we do not thrash.
    if (this.crashRelaunchCount < Sidecar.MAX_CRASH_RELAUNCHES && !this.relaunching) {
      this.crashRelaunchCount += 1;
      this.relaunching = true;
      this.setState("recovering", detail);
      // Defer to the next tick so the exit handler unwinds before we respawn.
      setTimeout(() => {
        this.relaunching = false;
        this.relaunchAfterCrash().catch((e) => {
          // A relaunch that itself fails to boot is terminal — surface it, do not loop.
          this.setState("fatal", `crash-relaunch failed: ${(e as Error).message}`);
        });
      }, 0);
      return;
    }
    // No relaunch left (already relaunched once, or a relaunch is in flight): surface
    // `degraded` so the renderer is told — never an infinite restart loop.
    this.setState("degraded", detail);
  }

  private failAllPending(reason: string): void {
    for (const [, p] of this.pending) {
      clearTimeout(p.timer);
      p.reject(new Error(reason));
    }
    this.pending.clear();
  }

  /**
   * Send a correlated request and resolve with the sidecar's response. New requests are
   * REFUSED once a shutdown is in progress (or the sidecar isn't running) — EXCEPT the
   * `shutdown` request itself, which `stop()` issues to drive the ordered teardown.
   */
  private requestRaw(
    build: (id: number) => Message,
    timeoutMs = 5000,
    allowDuringStop = false,
  ): Promise<PipeResponse> {
    if (!allowDuringStop && (this.state === "stopping" || this.state === "stopped")) {
      return Promise.reject(new Error(`sidecar is ${this.state}; new requests are refused`));
    }
    if (!this.child) {
      return Promise.reject(new Error(`sidecar is not running (state=${this.state})`));
    }
    const id = this.nextId++;
    return new Promise<PipeResponse>((resolve, reject) => {
      const timer = setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`request id=${id} timed out`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.send(build(id));
    });
  }

  // -------------------------------------------------------------------------
  // The renderer-facing surface (the relay calls these).
  // -------------------------------------------------------------------------

  /**
   * Invoke a CONTRACT command by name with its args — the single entry the renderer's
   * `nyxCore.invoke` reaches through the relay. Maps each command onto the right typed pipe
   * request and resolves the result in the shape the contract expects (the PTY id for
   * `pty_spawn`, the parsed JSON for a db-command, the state string for a command lifecycle
   * call). A sidecar `error` response rejects with the readable message.
   */
  async invoke(command: string, args: Record<string, unknown> = {}): Promise<unknown> {
    // `window_controls_visible` is a window-chrome concern, answered from the OS env (parity
    // with the `nyxWindow.controlsVisible` bridge) — never a sidecar round-trip.
    if (command === "window_controls_visible") return windowControlsVisible();

    switch (command) {
      case "pty_spawn": {
        const res = await this.requestRaw((id) => ({
          category: "request",
          kind: "pty_spawn",
          id,
          cols: Number(args.cols),
          rows: Number(args.rows),
          cwd: (args.cwd as string | undefined) ?? "",
          recordId: (args.terminalId as string | undefined) ?? "",
        }));
        if (res.kind === "pty_spawned") return res.terminalId;
        return this.throwError(res, command);
      }
      case "pty_write": {
        const res = await this.requestRaw((id) => ({
          category: "request",
          kind: "pty_write",
          id,
          terminalId: Number(args.id),
          data: Uint8Array.from((args.data as number[]) ?? []),
        }));
        return this.expectOk(res, command);
      }
      case "pty_resize": {
        const res = await this.requestRaw((id) => ({
          category: "request",
          kind: "pty_resize",
          id,
          terminalId: Number(args.id),
          cols: Number(args.cols),
          rows: Number(args.rows),
        }));
        return this.expectOk(res, command);
      }
      case "pty_close": {
        const res = await this.requestRaw((id) => ({
          category: "request",
          kind: "pty_close",
          id,
          terminalId: Number(args.id),
        }));
        return this.expectOk(res, command);
      }
      case "command_start":
      case "command_stop":
      case "command_relaunch": {
        const res = await this.requestRaw((id) => ({
          category: "request",
          kind: command,
          id,
          instanceId: String(args.instanceId),
        }));
        if (res.kind === "command_status") return res.state;
        return this.throwError(res, command);
      }
      default: {
        // The whole non-PTY long tail (DB CRUD / integrations / live-PTY introspection /
        // activity / command output+acknowledge) rides the generic `db_command`: the
        // contract command name + JSON args. The sidecar replies a JSON result string the
        // relay parses back to the contract shape.
        const res = await this.requestRaw((id) => ({
          category: "request",
          kind: "db_command",
          id,
          command,
          argsJson: JSON.stringify(args ?? {}),
        }));
        if (res.kind === "db_result") {
          return res.resultJson.length === 0 ? null : JSON.parse(res.resultJson);
        }
        return this.throwError(res, command);
      }
    }
  }

  /** Resolve a void-OK response, or reject on an error response. */
  private expectOk(res: PipeResponse, command: string): undefined {
    if (res.kind === "ok") return undefined;
    return this.throwError(res, command);
  }

  /** Throw the readable error from an `error` response (or an unexpected kind). */
  private throwError(res: PipeResponse, command: string): never {
    if (res.kind === "error") throw new Error(res.message);
    throw new Error(`${command}: unexpected response kind '${res.kind}'`);
  }

  /**
   * FLOW-CONTROL ACK (fire-and-forget): credit `bytes` of `ptyId`'s output back to the
   * sidecar so the paused reader resumes below the low-water mark. Registers NO pending
   * entry and awaits NO reply, so a high-rate ack stream never grows the pending map nor
   * blocks the renderer's write path. Dropped when the sidecar isn't serving (a missed ack
   * only delays a resume — never loses a byte; the data sits in the kernel PTY buffer).
   */
  ackPty(ptyId: number, bytes: number): void {
    if (this.state !== "ready" || !this.child) return;
    this.send({
      category: "request",
      kind: "pty_ack",
      id: this.nextId++,
      terminalId: ptyId,
      bytes,
    });
  }

  /** Start the in-sidecar MCP HTTP server (idempotent). Resolves the bound port. */
  async startMcp(): Promise<number> {
    const res = await this.requestRaw((id) => ({ category: "request", kind: "mcp_start", id }));
    if (res.kind === "mcp_started") {
      this.lastMcpPort = res.port;
      return res.port;
    }
    return this.throwError(res, "mcp_start");
  }

  /** The MCP port bound by the LIVE sidecar (0 before the first bind, or if it failed). A crash
   *  relaunch re-binds and updates this, so it is the one place that knows where the agents'
   *  tools currently answer. */
  get mcpPort(): number {
    return this.lastMcpPort;
  }

  /**
   * Translate a sidecar EVENT into the contract `{ event, payload }` envelope the renderer
   * demuxes, and fan it out to every listener. Bytes ride as MessagePack `bin`; the relay
   * hands the renderer a plain `number[]` (the contract's JSON wire shape).
   */
  private onPipeEvent(event: PipeEvent): void {
    const envelope = toEnvelope(event);
    if (!envelope) return;
    for (const l of this.eventListeners) l(envelope);
  }

  /**
   * Ordered shutdown. Transitions to `stopping` (REFUSING new requests), asks the sidecar to
   * shut down in order (snapshot → stop PTYs/commands → exit), then FORCE-kills if it
   * lingers past `timeoutMs` so no orphan survives. Idempotent.
   */
  async stop(timeoutMs = 3000): Promise<void> {
    if (this.state === "stopping" || this.state === "stopped" || this.state === "idle") return;
    const child = this.child;
    this.intentionalStop = true;
    this.setState("stopping");
    if (!child) {
      this.setState("stopped");
      return;
    }
    const exited = new Promise<void>((resolve) => child.once("exit", () => resolve()));
    try {
      // The shutdown request bypasses the `stopping` guard (it IS the shutdown). Ignore a
      // failure — the forced kill below covers an unresponsive sidecar.
      await this.requestRaw((id) => ({ category: "request", kind: "shutdown", id }), timeoutMs, true);
    } catch {
      // ignore — forced kill below.
    }
    const timer = setTimeout(() => {
      if (this.child) child.kill();
    }, timeoutMs);
    await exited;
    clearTimeout(timer);
    this.setState("stopped");
  }
}

/**
 * Map a sidecar EVENT to the contract event envelope the renderer demuxes. Returns `null`
 * for events with no renderer-facing mapping (the `log` event drives main's console, not a
 * contract event). The byte payloads (`terminal_output`, `command_output`) are converted to
 * a plain `number[]` (the JSON wire shape the renderer adapter normalizes to `Uint8Array`).
 */
export function toEnvelope(event: PipeEvent): CoreEventEnvelope | null {
  switch (event.kind) {
    case "terminal_output":
      return {
        event: "pty://output",
        payload: { id: event.terminalId, bytes: Array.from(event.data) },
      };
    case "pty_exit":
      return { event: "pty://exit", payload: { id: event.terminalId, code: event.code } };
    case "pty_cwd":
      // Surfaced for the portable cwd source (auto-attach). The sidecar keys it by the live
      // pty id; the renderer's raw cwd subscriber reads `id` + `cwd`.
      return { event: "pty://cwd", payload: { id: event.terminalId, cwd: event.cwd } };
    case "pty_exec_state":
      // The contract's `terminal://exec-state` is keyed by the live pty id here (the sidecar
      // emits the raw OSC 133 transition keyed by the pty id). The renderer's raw subscriber
      // reads the snake_case wire shape.
      return {
        event: "terminal://exec-state",
        payload: {
          terminal_id: event.terminalId,
          state: event.state,
          exit_code: event.exitCode,
        },
      };
    case "pty_busy_state":
      // The OS busy-state transition (v2.5.5 review T1). The sidecar emits it keyed by the
      // persistent RECORD id, so the renderer folds it straight onto the terminal record and
      // the runtime tree re-pulls the snapshot (whose overlaid `busy` bit lights the line).
      return {
        event: "terminal://busy-state",
        payload: { terminal_id: event.recordId, busy: event.busy },
      };
    case "terminal_stats":
      // The per-terminal CPU%/RAM reading (FEEDBACK #28 / v2.5.6). Keyed by the persistent RECORD
      // id. The wire carries CPU as an integer `cpuMilli` (thousandths of a percent) to stay
      // float-free; divide back to the float `cpu_pct` the renderer hook (`use-terminal-stats`)
      // formats. The renderer shows it on hover of a terminal LINE — never inline (non-inline
      // constraint: the CPU%/RAM must not mask/truncate the terminal name).
      return {
        event: "terminal://stats",
        payload: {
          terminal_id: event.recordId,
          cpu_pct: event.cpuMilli / 1000,
          mem_bytes: event.memBytes,
        },
      };
    case "command_state":
      switch (event.event) {
        case "ack":
          return { event: "command://ack", payload: { instanceId: event.instanceId } };
        case "output-cleared":
          return {
            event: "command://output-cleared",
            payload: { instanceId: event.instanceId },
          };
        default:
          return {
            event: "command://state",
            payload: {
              instanceId: event.instanceId,
              state: event.state,
              code: event.exitCode,
            },
          };
      }
    case "command_output":
      return {
        event: "command://output",
        payload: { instanceId: event.instanceId, bytes: Array.from(event.data) },
      };
    case "changed":
      return { event: `${event.topic}://changed`, payload: undefined };
    case "log":
      // Lifecycle log — main's console, not a contract event.
      return null;
  }
}
