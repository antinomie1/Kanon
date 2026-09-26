//! Milky 1.3 protocol data model.
//!
//! The module re-exports the vendored bindings from [`generated`], which are generated from the
//! official Milky IR and carry the complete wire vocabulary of the protocol: all 21 event types,
//! both segment unions, every entity, and the request/response pair of each of the 65 endpoints.
//!
//! Nothing in this module performs I/O. It describes the wire format and nothing else, so a
//! protocol version bump is a change to generated code plus, at most, the two mapping functions
//! in [`crate::mapping`] that decide how the new vocabulary reaches the pipeline.

mod event_ext;
mod generated;

pub use generated::*;
