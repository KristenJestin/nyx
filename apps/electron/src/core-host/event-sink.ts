/**
 * Electron host adapter of FRONTIER 1 — `EventSink` (the mirror of the Tauri
 * adapter's `TauriRunnerSink` / `app.emit`).
 *
 * `nyx-core` pushes UI/observer notifications OUT through this sink; the host maps
 * each to a `HostEvent` on the host→main channel (where main relays it to the
 * renderer over the allowlisted IPC). The napi `ThreadsafeFunction` is what marshals
 * the Rust reader thread's chunks onto the Node event loop before they reach here —
 * so this sink always runs on the host's Node loop, never a foreign thread.
 *
 * Phase-3 surface: the full interactive-PTY EventSink — `pty_output` (ORDERED per
 * `ptyId`, carrying its byte length for the flow-control accounting), `pty_exit`,
 * `pty_cwd` (OSC 7) and `pty_exec_state` (OSC 133) — plus the coarse `changed`
 * invalidation. Bytes are base64-encoded for JSON transport (decoded back to a
 * Buffer by main).
 */
import type { HostEventPayload } from "../shared/host-protocol";

/** A function that ships one event payload toward main (process.send). */
export type EmitEvent = (payload: HostEventPayload) => void;

export class ElectronEventSink {
  constructor(private readonly emit: EmitEvent) {}

  /**
   * A chunk of interactive-terminal output for `ptyId` (ORDERED per pty). Carries the
   * decoded byte length so main/renderer can credit the flow-control loop without
   * decoding the base64 first. The host has already capped this at 64 KiB.
   */
  ptyOutput(ptyId: number, bytes: Buffer): void {
    this.emit({
      kind: "pty-output",
      ptyId,
      dataB64: bytes.toString("base64"),
      bytes: bytes.length,
    });
  }

  /** The interactive terminal `ptyId` exited with `code`. */
  ptyExit(ptyId: number, code: number | null): void {
    this.emit({ kind: "pty-exit", ptyId, code });
  }

  /** A decoded OSC 7 cwd for `ptyId` (portable cwd source). */
  ptyCwd(ptyId: number, terminalId: string | null, cwd: string): void {
    this.emit({ kind: "pty-cwd", ptyId, terminalId, cwd });
  }

  /**
   * A PERSISTED exec-state transition for a terminal RECORD (PRD-5 task #1). Emitted
   * AFTER the DB write, carrying the persisted `unread` + stamped `updatedAt` (parity
   * with the Tauri `persist_and_emit_exec_state`). Keyed by the durable `terminalId`.
   */
  ptyExecState(
    terminalId: string,
    state: string,
    exitCode: number | null,
    unread: boolean,
    updatedAt: number,
  ): void {
    this.emit({ kind: "pty-exec-state", terminalId, state, exitCode, unread, updatedAt });
  }

  /** An OS-derived busy/idle TRANSITION for a terminal RECORD (PRD-5 task #1). */
  ptyBusyState(terminalId: string, busy: boolean): void {
    this.emit({ kind: "pty-busy-state", terminalId, busy });
  }

  /**
   * A managed-command run-state / ack / output-cleared transition (parity with the
   * Tauri `command://state` / `command://ack` / `command://output-cleared`). Keyed by
   * the persistent `instanceId`; `event` is `"state" | "ack" | "output-cleared"`.
   */
  commandState(event: string, instanceId: string, state: string, exitCode: number | null): void {
    this.emit({ kind: "command-state", event, instanceId, state, exitCode });
  }

  /** A coalesced managed-command output chunk (parity with `command://output`). */
  commandOutput(instanceId: string, bytes: Buffer): void {
    this.emit({ kind: "command-output", instanceId, dataB64: bytes.toString("base64") });
  }

  /** A coarse "this collection changed, re-fetch it" invalidation. */
  changed(topic: ChangedEventTopic): void {
    this.emit({ kind: "changed", topic });
  }
}

/** The closed set of change topics (mirrors `nyx_core::frontier::ChangedTopic`). */
export type ChangedEventTopic = "terminals" | "workspaces" | "commands" | "agent-sessions";
