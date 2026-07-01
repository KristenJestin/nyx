//! nyx-sidecar library surface.
//!
//! The supervised sidecar lifecycle ([`lifecycle`]) and the internal main <->
//! sidecar [`pipe`] transport are exposed as a library so integration and
//! contract tests can drive the real codec without `#[path]` re-includes, and so
//! later phases can reuse the protocol types. The binary (`src/main.rs`) wires
//! these into the actual stdin/stdout process loop.

pub mod lifecycle;
pub mod pipe;
pub mod protocol;
pub mod runtime;
