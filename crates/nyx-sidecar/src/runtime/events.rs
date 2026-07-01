//! The sidecar's UNSOLICITED-event sink — the wire side of the
//! [`nyx_core::frontier::EventSink`] frontier.
//!
//! The runtime (PTY pump threads, the managed-command runner, the MCP dispatcher)
//! pushes events OUT to the supervisor over the pipe. They all share ONE
//! [`EventSink`]: a thread-safe handle around the sidecar's framed `stdout`. Frames
//! are written under a mutex so a PTY-output frame from one thread and a `pty-exit`
//! frame from another never interleave their bytes on the wire (each frame is
//! length-prefixed and written atomically while the lock is held).

use std::io::Write;
use std::sync::{Arc, Mutex};

use crate::pipe::wire::{self, Event, Message};

/// A clone-able, thread-safe sink for unsolicited events. Every clone writes framed
/// MessagePack to the SAME underlying `stdout`, serialized by the inner mutex so
/// frames never tear.
#[derive(Clone)]
pub struct EventSink {
    out: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl EventSink {
    /// Build a sink over a writer (the sidecar's `stdout` in production; a buffer in
    /// tests).
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self {
            out: Arc::new(Mutex::new(out)),
        }
    }

    /// Frame-encode and write one event to the pipe, flushing so the supervisor sees
    /// it promptly. A write error means the supervisor's pipe is gone (parent death);
    /// it is swallowed here — the parent-death watch in `main` tears the process down.
    pub fn emit(&self, event: Event) {
        let frame = match wire::encode_message_frame(&Message::Event(event)) {
            Ok(f) => f,
            // An event larger than MAX_FRAME_LEN is a producer bug; PTY output is
            // chunked well under the cap (see `flow`), so this never fires in practice.
            Err(_) => return,
        };
        let mut out = self.out.lock().unwrap();
        let _ = out.write_all(&frame);
        let _ = out.flush();
    }

    /// Convenience: emit a structured log line (the same `EVT_LOG` the boot `ready`
    /// event uses).
    pub fn log(&self, level: &str, message: impl Into<String>) {
        self.emit(Event::Log {
            level: level.to_string(),
            message: message.into(),
        });
    }

    /// Write an already-framed message (a request/response reply) through the SAME
    /// serialized `stdout` the events use, so a response frame and a concurrent PTY
    /// event frame never tear. Used by the binary's run loop to send responses on the
    /// one shared writer.
    pub fn write_frame(&self, frame: &[u8]) {
        let mut out = self.out.lock().unwrap();
        let _ = out.write_all(frame);
        let _ = out.flush();
    }
}
