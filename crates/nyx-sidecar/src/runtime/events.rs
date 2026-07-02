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

/// The PRODUCTION [`nyx_core::usecase::EventSink`] (v2.1 Phase 4, task #10): it bridges the
/// use-case layer's POST-COMMIT domain-event seam to the sidecar's real-time PUSH. When a
/// mutating identity use case emits a [`nyx_core::usecase::DomainEvent`] (after its
/// transaction committed — the `domain_events` row is durably present), this sink pushes the
/// v2.0 UNSOLICITED [`Event::Changed`] out over the pipe, so the renderer re-pulls the
/// affected list WITHOUT a reload and WITHOUT polling.
///
/// It REUSES the existing v2.0 unsolicited-event channel (the coarse "X changed → re-fetch"
/// invalidation) rather than inventing a new transport: every structuring identity (project /
/// workspace / repository) lives in the sidebar tree the renderer refreshes on
/// `workspaces://changed`, so ALL identity domain events map to that ONE topic — including
/// the archive / rename / set_optional / set_primary families that the ad-hoc
/// `workspaces://changed` broadcast in the UI bridge does not cover on its own. This makes
/// the DOMAIN event (not a string-matched command name) the thing that drives the push.
///
/// The [`nyx_core::usecase::EventSink`] trait is not `Send`/`Sync`-bound and its `emit` takes
/// `&self`, but a use case runs on ONE thread holding `&mut SqliteConnection`, so a plain
/// clone of the underlying pipe [`EventSink`] is all this needs.
pub struct PushEventSink {
    events: EventSink,
}

impl PushEventSink {
    /// Build the push sink over the sidecar's pipe event sink.
    pub fn new(events: EventSink) -> Self {
        Self { events }
    }
}

impl nyx_core::usecase::EventSink for PushEventSink {
    fn emit(&self, event: nyx_core::usecase::DomainEvent) {
        // Every identity domain event (project / workspace / repository) invalidates the
        // sidebar tree the renderer refreshes on `workspaces://changed`. Coarse by design
        // (the renderer re-pulls the authoritative list), and it is the SAME unsolicited
        // channel the pre-Phase-4 broadcast already used — no new transport.
        let _ = event;
        self.events.emit(Event::Changed {
            topic: "workspaces".to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::codec::FrameDecoder;
    use crate::pipe::wire::{decode_message, Event, Message};
    use nyx_core::db::Db;
    use nyx_core::usecase::{self, DomainEventSource};
    use std::sync::{Arc, Mutex};

    /// A fresh on-disk migrated `Db` under a unique temp dir (the sidecar cannot use
    /// nyx-core's `#[cfg(test)]` `Db::in_memory`, which is not compiled into this crate).
    fn temp_db() -> Db {
        let dir = std::env::temp_dir().join(format!(
            "nyx-push-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("nyx.db")).expect("open db")
    }

    /// A `Write` that appends to a shared buffer, so a test can read back exactly the frames
    /// the sink pushed onto the pipe.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Decode every complete `Event` frame the shared buffer captured.
    fn captured_events(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<Event> {
        let bytes = buf.lock().unwrap().clone();
        let mut dec = FrameDecoder::new();
        dec.push(&bytes);
        let mut events = Vec::new();
        while let Some(payload) = dec.next_frame().expect("decode frame") {
            if let Ok(Message::Event(e)) = decode_message(&payload) {
                events.push(e);
            }
        }
        events
    }

    /// done_criterion #10 — "Une mutation (UI ou MCP) se reflète dans l'UI via un event
    /// poussé, sans rechargement ni polling." The FULL Rust chain: a mutation
    /// (`usecase::register_workspace`) → its `domain_events` row (task #9) → a PUSHED
    /// unsolicited `Event::Changed { topic: "workspaces" }` on the pipe, driven by the
    /// POST-COMMIT domain-event seam through the production [`PushEventSink`]. The assertion
    /// is that the push CHANNEL received the invalidation — there is NO poll loop anywhere
    /// (the renderer re-pulls on receipt of this pushed event, never on a timer).
    #[test]
    fn a_mutation_persists_a_domain_event_and_pushes_a_changed_invalidation() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let pipe = EventSink::new(Box::new(SharedBuf(buf.clone())));
        let push = PushEventSink::new(pipe);

        let db = temp_db();
        // Seed a project + its root workspace (through the same shared use case), then add a
        // non-root workspace — the mutation under test.
        let (project, _root) = db
            .with_conn(|c| {
                usecase::create_project(
                    c,
                    &push,
                    "Nyx",
                    "/tmp/push-p",
                    None,
                    DomainEventSource::Mcp,
                )
            })
            .unwrap();
        let ws = db
            .with_conn(|c| {
                usecase::register_workspace(
                    c,
                    &push,
                    &project.id,
                    "wt",
                    "/tmp/push-wt",
                    DomainEventSource::Mcp,
                )
            })
            .unwrap();

        // task #9 side: the mutations persisted correlated `domain_events` rows.
        let events = db
            .with_conn(nyx_core::db::list_domain_events)
            .expect("list domain_events");
        assert!(
            events.iter().any(|e| e.event_type == "workspace.created"
                && e.workspace_id.as_deref() == Some(ws.id.as_str())
                && e.project_id == project.id),
            "a correlated workspace.created domain_event was persisted"
        );

        // task #10 side: each committed mutation PUSHED a `workspaces://changed` invalidation
        // on the pipe (the v2.0 unsolicited-event channel) — no reload, no poll.
        let pushed = captured_events(&buf);
        assert!(
            pushed
                .iter()
                .all(|e| matches!(e, Event::Changed { topic } if topic == "workspaces")),
            "every pushed event is a `workspaces://changed` invalidation"
        );
        assert_eq!(
            pushed.len(),
            2,
            "the two committed mutations (create_project + register_workspace) each pushed once"
        );
    }
}
