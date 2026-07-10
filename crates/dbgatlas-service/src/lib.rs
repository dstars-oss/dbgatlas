//! DbgAtlas service facade.
//!
//! Implementation details live in focused internal modules while the crate-root
//! API remains stable for the CLI and existing Rust callers.

mod service;

pub use service::*;
