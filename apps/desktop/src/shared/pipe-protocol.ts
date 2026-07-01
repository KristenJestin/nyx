/**
 * TypeScript side of the internal main <-> sidecar pipe transport (PDR-0007).
 *
 * This is the EXACT mirror of `crates/nyx-sidecar/src/pipe/{codec,wire}.rs`:
 *   - frames: u32 big-endian length prefix + MessagePack payload, with a hard
 *     max size and an incremental decoder;
 *   - messages: versioned, POSITIONAL MessagePack arrays (never struct-as-map),
 *     so the bytes match rmp-serde on the Rust side byte-for-byte (verified by
 *     `contract/fixtures.json` + `contract/contract.test.ts`).
 *
 * The envelope is `[protocol_version, category, kind, body]`; see the Rust
 * `wire.rs` module docs for the full shape.
 */
import { decode as mpDecode, encode as mpEncode } from "./msgpack-mini";

/** Protocol version stamped on every message. Bump on any breaking wire change. */
export const PROTOCOL_VERSION = 1;

/** Maximum payload length of a single frame (16 MiB) — mirrors `MAX_FRAME_LEN`. */
export const MAX_FRAME_LEN = 16 * 1024 * 1024;

/** Capabilities advertised at handshake (mirrors Rust `CAPABILITIES`). `pty` (HHC) is
 * the interactive-terminal surface; `commands` + `mcp` (HHD) are the managed-command
 * runtime + the in-sidecar MCP HTTP server. */
export const CAPABILITIES = [
  "handshake",
  "ping",
  "shutdown",
  "events",
  "pty",
  "commands",
  "mcp",
] as const;

// Envelope categories.
export const CAT_REQUEST = 0;
export const CAT_RESPONSE = 1;
export const CAT_EVENT = 2;

// Request kinds.
export const REQ_HANDSHAKE = 0;
export const REQ_PING = 1;
export const REQ_SHUTDOWN = 2;
// PTY request kinds (HHC).
export const REQ_PTY_SPAWN = 3;
export const REQ_PTY_WRITE = 4;
export const REQ_PTY_RESIZE = 5;
export const REQ_PTY_CLOSE = 6;
export const REQ_PTY_ACK = 7;
// Managed-command + MCP request kinds (HHD).
export const REQ_COMMAND_START = 8;
export const REQ_COMMAND_STOP = 9;
export const REQ_COMMAND_RELAUNCH = 10;
export const REQ_MCP_START = 11;
// The GENERIC non-PTY backend command (G0): a contract command NAME + JSON args. The
// supervisor replacement for the v1 napi `db_command` — every DB-backed + integrations +
// live-PTY-introspection long-tail command rides this one kind.
export const REQ_DB_COMMAND = 12;

// Response kinds.
export const RES_HANDSHAKE_OK = 0;
export const RES_PONG = 1;
export const RES_SHUTDOWN_ACK = 2;
export const RES_ERROR = 3;
// PTY response kinds (HHC).
export const RES_PTY_SPAWNED = 4;
export const RES_OK = 5;
// Managed-command + MCP response kinds (HHD).
export const RES_COMMAND_STATUS = 6;
export const RES_MCP_STARTED = 7;
// The reply to a db_command (G0): the result already serialized to a JSON string.
export const RES_DB_RESULT = 8;

// Event kinds.
export const EVT_TERMINAL_OUTPUT = 0;
export const EVT_LOG = 1;
// PTY lifecycle/observation events (HHC).
export const EVT_PTY_EXIT = 2;
export const EVT_PTY_CWD = 3;
export const EVT_PTY_EXEC_STATE = 4;
// Managed-command lifecycle events (HHD).
export const EVT_COMMAND_STATE = 5;
export const EVT_COMMAND_OUTPUT = 6;
export const EVT_CHANGED = 7;

/** Stable, machine-matchable error codes (mirror Rust `ErrorCode`). */
export type ErrorCode =
  | "version_incompatible"
  | "malformed_request"
  | "frame_too_large"
  | "quota_exceeded";

// --- Logical message types (the in-memory shape the rest of main uses) --------

export type Request =
  | { category: "request"; kind: "handshake"; id: number; protocolVersion: number }
  | { category: "request"; kind: "ping"; id: number; nonce: number }
  | { category: "request"; kind: "shutdown"; id: number }
  | {
      category: "request";
      kind: "pty_spawn";
      id: number;
      cols: number;
      rows: number;
      /** Working dir; empty string -> the sidecar's cwd. */
      cwd: string;
      /** Persistent terminal RECORD id exported as NYX_TERMINAL_ID; empty -> none. */
      recordId: string;
    }
  | {
      category: "request";
      kind: "pty_write";
      id: number;
      terminalId: number;
      /** Raw bytes (binary, no base64) written verbatim to the child's stdin. */
      data: Uint8Array;
    }
  | {
      category: "request";
      kind: "pty_resize";
      id: number;
      terminalId: number;
      cols: number;
      rows: number;
    }
  | { category: "request"; kind: "pty_close"; id: number; terminalId: number }
  | {
      category: "request";
      kind: "pty_ack";
      id: number;
      terminalId: number;
      /** Output bytes the renderer has now consumed (the flow-control credit). */
      bytes: number;
    }
  | { category: "request"; kind: "command_start"; id: number; instanceId: string }
  | { category: "request"; kind: "command_stop"; id: number; instanceId: string }
  | { category: "request"; kind: "command_relaunch"; id: number; instanceId: string }
  | { category: "request"; kind: "mcp_start"; id: number }
  | {
      category: "request";
      kind: "db_command";
      id: number;
      /** The contract command name (a `BackendCommand`). */
      command: string;
      /** The contract args, serialized to JSON (`"{}"` for the no-arg commands). */
      argsJson: string;
    };

export type Response =
  | {
      category: "response";
      kind: "handshake_ok";
      id: number;
      protocolVersion: number;
      capabilities: string[];
      maxFrameLen: number;
    }
  | { category: "response"; kind: "pong"; id: number; nonce: number }
  | { category: "response"; kind: "shutdown_ack"; id: number }
  | { category: "response"; kind: "error"; id: number; code: ErrorCode; message: string }
  | { category: "response"; kind: "pty_spawned"; id: number; terminalId: number }
  | { category: "response"; kind: "ok"; id: number }
  | {
      category: "response";
      kind: "command_status";
      id: number;
      instanceId: string;
      /** `idle|running|success|error`. */
      state: string;
      running: boolean;
      exitCode: number | null;
      unread: boolean;
      wasRunning: boolean;
      restarted: boolean;
    }
  | { category: "response"; kind: "mcp_started"; id: number; port: number }
  | {
      category: "response";
      kind: "db_result";
      id: number;
      /** The command's result, ALREADY serialized to JSON (`"null"` for void). The
       *  supervisor `JSON.parse`s it back to the contract shape. */
      resultJson: string;
    };

export type Event =
  | {
      category: "event";
      kind: "terminal_output";
      terminalId: number;
      seq: number;
      data: Uint8Array;
    }
  | { category: "event"; kind: "log"; level: string; message: string }
  | { category: "event"; kind: "pty_exit"; terminalId: number; code: number | null }
  | { category: "event"; kind: "pty_cwd"; terminalId: number; cwd: string }
  | {
      category: "event";
      kind: "pty_exec_state";
      terminalId: number;
      state: string;
      exitCode: number | null;
    }
  | {
      category: "event";
      kind: "command_state";
      /** `"state"` | `"ack"` | `"output-cleared"`. */
      event: string;
      instanceId: string;
      /** `idle|running|success|error` for a `"state"` event; empty otherwise. */
      state: string;
      exitCode: number | null;
    }
  | { category: "event"; kind: "command_output"; instanceId: string; data: Uint8Array }
  | {
      category: "event";
      kind: "changed";
      /** `terminals` | `workspaces` | `commands` | `agent-sessions`. */
      topic: string;
    };

export type Message = Request | Response | Event;

/** Raised when a decoded envelope's stamped version differs from ours. */
export class VersionMismatchError extends Error {
  constructor(
    public readonly theirs: number,
    public readonly ours: number,
  ) {
    super(
      `protocol version mismatch: peer speaks v${theirs}, this build speaks v${ours}`,
    );
    this.name = "VersionMismatchError";
  }
}

/** Raised when a length prefix exceeds {@link MAX_FRAME_LEN}. */
export class FrameTooLargeError extends Error {
  constructor(
    public readonly len: number,
    public readonly max: number,
  ) {
    super(`frame length ${len} exceeds maximum ${max}`);
    this.name = "FrameTooLargeError";
  }
}

// `nonce`/`seq` can exceed 2^53; rmp-serde sends them as MessagePack u64. The TS
// fixtures keep them within the safe-integer range, and @msgpack/msgpack decodes
// in-range u64 as `number`. We normalize bigint -> number defensively.
function asNumber(v: unknown): number {
  return typeof v === "bigint" ? Number(v) : (v as number);
}

/** Encode a logical message to its MessagePack envelope bytes (no frame prefix). */
export function encodeMessage(msg: Message): Uint8Array {
  let category: number;
  let kind: number;
  let body: unknown[];

  switch (msg.category) {
    case "request":
      category = CAT_REQUEST;
      switch (msg.kind) {
        case "handshake":
          kind = REQ_HANDSHAKE;
          body = [msg.id, msg.protocolVersion];
          break;
        case "ping":
          kind = REQ_PING;
          body = [msg.id, msg.nonce];
          break;
        case "shutdown":
          kind = REQ_SHUTDOWN;
          body = [msg.id];
          break;
        case "pty_spawn":
          kind = REQ_PTY_SPAWN;
          body = [msg.id, msg.cols, msg.rows, msg.cwd, msg.recordId];
          break;
        case "pty_write":
          kind = REQ_PTY_WRITE;
          body = [msg.id, msg.terminalId, msg.data];
          break;
        case "pty_resize":
          kind = REQ_PTY_RESIZE;
          body = [msg.id, msg.terminalId, msg.cols, msg.rows];
          break;
        case "pty_close":
          kind = REQ_PTY_CLOSE;
          body = [msg.id, msg.terminalId];
          break;
        case "pty_ack":
          kind = REQ_PTY_ACK;
          body = [msg.id, msg.terminalId, msg.bytes];
          break;
        case "command_start":
          kind = REQ_COMMAND_START;
          body = [msg.id, msg.instanceId];
          break;
        case "command_stop":
          kind = REQ_COMMAND_STOP;
          body = [msg.id, msg.instanceId];
          break;
        case "command_relaunch":
          kind = REQ_COMMAND_RELAUNCH;
          body = [msg.id, msg.instanceId];
          break;
        case "mcp_start":
          kind = REQ_MCP_START;
          body = [msg.id];
          break;
        case "db_command":
          kind = REQ_DB_COMMAND;
          body = [msg.id, msg.command, msg.argsJson];
          break;
      }
      break;
    case "response":
      category = CAT_RESPONSE;
      switch (msg.kind) {
        case "handshake_ok":
          kind = RES_HANDSHAKE_OK;
          body = [msg.id, msg.protocolVersion, msg.capabilities, msg.maxFrameLen];
          break;
        case "pong":
          kind = RES_PONG;
          body = [msg.id, msg.nonce];
          break;
        case "shutdown_ack":
          kind = RES_SHUTDOWN_ACK;
          body = [msg.id];
          break;
        case "error":
          kind = RES_ERROR;
          body = [msg.id, msg.code, msg.message];
          break;
        case "pty_spawned":
          kind = RES_PTY_SPAWNED;
          body = [msg.id, msg.terminalId];
          break;
        case "ok":
          kind = RES_OK;
          body = [msg.id];
          break;
        case "command_status":
          kind = RES_COMMAND_STATUS;
          body = [
            msg.id,
            msg.instanceId,
            msg.state,
            msg.running,
            msg.exitCode,
            msg.unread,
            msg.wasRunning,
            msg.restarted,
          ];
          break;
        case "mcp_started":
          kind = RES_MCP_STARTED;
          body = [msg.id, msg.port];
          break;
        case "db_result":
          kind = RES_DB_RESULT;
          body = [msg.id, msg.resultJson];
          break;
      }
      break;
    case "event":
      category = CAT_EVENT;
      switch (msg.kind) {
        case "terminal_output":
          kind = EVT_TERMINAL_OUTPUT;
          body = [msg.terminalId, msg.seq, msg.data];
          break;
        case "log":
          kind = EVT_LOG;
          body = [msg.level, msg.message];
          break;
        case "pty_exit":
          kind = EVT_PTY_EXIT;
          // `code: null` encodes as MessagePack nil (mirrors the Rust `opt_i32`).
          body = [msg.terminalId, msg.code];
          break;
        case "pty_cwd":
          kind = EVT_PTY_CWD;
          body = [msg.terminalId, msg.cwd];
          break;
        case "pty_exec_state":
          kind = EVT_PTY_EXEC_STATE;
          body = [msg.terminalId, msg.state, msg.exitCode];
          break;
        case "command_state":
          kind = EVT_COMMAND_STATE;
          body = [msg.event, msg.instanceId, msg.state, msg.exitCode];
          break;
        case "command_output":
          kind = EVT_COMMAND_OUTPUT;
          body = [msg.instanceId, msg.data];
          break;
        case "changed":
          kind = EVT_CHANGED;
          body = [msg.topic];
          break;
      }
      break;
  }

  const envelope = [PROTOCOL_VERSION, category!, kind!, body!];
  // The positional array shape + minimal int/str/bin encodings of msgpack-mini
  // match rmp-serde exactly (verified by contract/fixtures.json).
  return mpEncode(envelope);
}

/** Decode a MessagePack envelope (no frame prefix) into a logical message. */
export function decodeMessage(bytes: Uint8Array): Message {
  const env = mpDecode(bytes) as unknown[];
  if (!Array.isArray(env) || env.length !== 4) {
    throw new Error("bad message envelope: not a 4-element array");
  }
  const version = asNumber(env[0]);
  const category = asNumber(env[1]);
  const kind = asNumber(env[2]);
  const body = env[3] as unknown[];
  if (version !== PROTOCOL_VERSION) {
    throw new VersionMismatchError(version, PROTOCOL_VERSION);
  }

  if (category === CAT_REQUEST) {
    if (kind === REQ_HANDSHAKE)
      return {
        category: "request",
        kind: "handshake",
        id: asNumber(body[0]),
        protocolVersion: asNumber(body[1]),
      };
    if (kind === REQ_PING)
      return { category: "request", kind: "ping", id: asNumber(body[0]), nonce: asNumber(body[1]) };
    if (kind === REQ_SHUTDOWN)
      return { category: "request", kind: "shutdown", id: asNumber(body[0]) };
    if (kind === REQ_PTY_SPAWN)
      return {
        category: "request",
        kind: "pty_spawn",
        id: asNumber(body[0]),
        cols: asNumber(body[1]),
        rows: asNumber(body[2]),
        cwd: body[3] as string,
        recordId: body[4] as string,
      };
    if (kind === REQ_PTY_WRITE)
      return {
        category: "request",
        kind: "pty_write",
        id: asNumber(body[0]),
        terminalId: asNumber(body[1]),
        data: body[2] as Uint8Array,
      };
    if (kind === REQ_PTY_RESIZE)
      return {
        category: "request",
        kind: "pty_resize",
        id: asNumber(body[0]),
        terminalId: asNumber(body[1]),
        cols: asNumber(body[2]),
        rows: asNumber(body[3]),
      };
    if (kind === REQ_PTY_CLOSE)
      return {
        category: "request",
        kind: "pty_close",
        id: asNumber(body[0]),
        terminalId: asNumber(body[1]),
      };
    if (kind === REQ_PTY_ACK)
      return {
        category: "request",
        kind: "pty_ack",
        id: asNumber(body[0]),
        terminalId: asNumber(body[1]),
        bytes: asNumber(body[2]),
      };
    if (kind === REQ_COMMAND_START)
      return {
        category: "request",
        kind: "command_start",
        id: asNumber(body[0]),
        instanceId: body[1] as string,
      };
    if (kind === REQ_COMMAND_STOP)
      return {
        category: "request",
        kind: "command_stop",
        id: asNumber(body[0]),
        instanceId: body[1] as string,
      };
    if (kind === REQ_COMMAND_RELAUNCH)
      return {
        category: "request",
        kind: "command_relaunch",
        id: asNumber(body[0]),
        instanceId: body[1] as string,
      };
    if (kind === REQ_MCP_START)
      return { category: "request", kind: "mcp_start", id: asNumber(body[0]) };
    if (kind === REQ_DB_COMMAND)
      return {
        category: "request",
        kind: "db_command",
        id: asNumber(body[0]),
        command: body[1] as string,
        argsJson: body[2] as string,
      };
  } else if (category === CAT_RESPONSE) {
    if (kind === RES_HANDSHAKE_OK)
      return {
        category: "response",
        kind: "handshake_ok",
        id: asNumber(body[0]),
        protocolVersion: asNumber(body[1]),
        capabilities: (body[2] as string[]).slice(),
        maxFrameLen: asNumber(body[3]),
      };
    if (kind === RES_PONG)
      return {
        category: "response",
        kind: "pong",
        id: asNumber(body[0]),
        nonce: asNumber(body[1]),
      };
    if (kind === RES_SHUTDOWN_ACK)
      return { category: "response", kind: "shutdown_ack", id: asNumber(body[0]) };
    if (kind === RES_ERROR)
      return {
        category: "response",
        kind: "error",
        id: asNumber(body[0]),
        code: body[1] as ErrorCode,
        message: body[2] as string,
      };
    if (kind === RES_PTY_SPAWNED)
      return {
        category: "response",
        kind: "pty_spawned",
        id: asNumber(body[0]),
        terminalId: asNumber(body[1]),
      };
    if (kind === RES_OK)
      return { category: "response", kind: "ok", id: asNumber(body[0]) };
    if (kind === RES_COMMAND_STATUS)
      return {
        category: "response",
        kind: "command_status",
        id: asNumber(body[0]),
        instanceId: body[1] as string,
        state: body[2] as string,
        running: body[3] as boolean,
        exitCode: body[4] == null ? null : asNumber(body[4]),
        unread: body[5] as boolean,
        wasRunning: body[6] as boolean,
        restarted: body[7] as boolean,
      };
    if (kind === RES_MCP_STARTED)
      return {
        category: "response",
        kind: "mcp_started",
        id: asNumber(body[0]),
        port: asNumber(body[1]),
      };
    if (kind === RES_DB_RESULT)
      return {
        category: "response",
        kind: "db_result",
        id: asNumber(body[0]),
        resultJson: body[1] as string,
      };
  } else if (category === CAT_EVENT) {
    if (kind === EVT_TERMINAL_OUTPUT)
      return {
        category: "event",
        kind: "terminal_output",
        terminalId: asNumber(body[0]),
        seq: asNumber(body[1]),
        data: body[2] as Uint8Array,
      };
    if (kind === EVT_LOG)
      return {
        category: "event",
        kind: "log",
        level: body[0] as string,
        message: body[1] as string,
      };
    if (kind === EVT_PTY_EXIT)
      return {
        category: "event",
        kind: "pty_exit",
        terminalId: asNumber(body[0]),
        // MessagePack nil decodes to null/undefined -> normalize to null.
        code: body[1] == null ? null : asNumber(body[1]),
      };
    if (kind === EVT_PTY_CWD)
      return {
        category: "event",
        kind: "pty_cwd",
        terminalId: asNumber(body[0]),
        cwd: body[1] as string,
      };
    if (kind === EVT_PTY_EXEC_STATE)
      return {
        category: "event",
        kind: "pty_exec_state",
        terminalId: asNumber(body[0]),
        state: body[1] as string,
        exitCode: body[2] == null ? null : asNumber(body[2]),
      };
    if (kind === EVT_COMMAND_STATE)
      return {
        category: "event",
        kind: "command_state",
        event: body[0] as string,
        instanceId: body[1] as string,
        state: body[2] as string,
        exitCode: body[3] == null ? null : asNumber(body[3]),
      };
    if (kind === EVT_COMMAND_OUTPUT)
      return {
        category: "event",
        kind: "command_output",
        instanceId: body[0] as string,
        data: body[1] as Uint8Array,
      };
    if (kind === EVT_CHANGED)
      return { category: "event", kind: "changed", topic: body[0] as string };
  }
  throw new Error(`unknown category/kind ${category}/${kind}`);
}

// --- Framing ------------------------------------------------------------------

/** Encode a payload into a length-prefixed frame: `[u32 BE len][payload]`. */
export function encodeFrame(payload: Uint8Array): Uint8Array {
  if (payload.length > MAX_FRAME_LEN) {
    throw new FrameTooLargeError(payload.length, MAX_FRAME_LEN);
  }
  const out = new Uint8Array(4 + payload.length);
  const view = new DataView(out.buffer);
  view.setUint32(0, payload.length, false); // big-endian
  out.set(payload, 4);
  return out;
}

/** Encode a logical message straight into a length-prefixed frame. */
export function encodeMessageFrame(msg: Message): Uint8Array {
  return encodeFrame(encodeMessage(msg));
}

/**
 * Incremental frame decoder mirroring the Rust `FrameDecoder`. Feed it bytes as
 * they arrive (`push`); drain whole frame payloads with `nextFrame`.
 */
export class FrameDecoder {
  private buf: Uint8Array = new Uint8Array(0);

  push(bytes: Uint8Array): void {
    const merged = new Uint8Array(this.buf.length + bytes.length);
    merged.set(this.buf, 0);
    merged.set(bytes, this.buf.length);
    this.buf = merged;
  }

  /** Pull the next complete frame payload, or `null` if not enough bytes yet. */
  nextFrame(): Uint8Array | null {
    if (this.buf.length < 4) return null;
    const view = new DataView(this.buf.buffer, this.buf.byteOffset, this.buf.byteLength);
    const len = view.getUint32(0, false);
    if (len > MAX_FRAME_LEN) {
      throw new FrameTooLargeError(len, MAX_FRAME_LEN);
    }
    const total = 4 + len;
    if (this.buf.length < total) return null;
    const payload = this.buf.slice(4, total);
    this.buf = this.buf.slice(total);
    return payload;
  }
}
