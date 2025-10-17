#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Core utilities shared by the Rust rsync implementation.
//!
//! The crate provides foundational building blocks that are reused by the
//! client, daemon, and transport layers. Only functionality that already
//! exists in this repository is documented here to keep the mission brief's
//! "code is truth" requirement intact.

/// Message formatting utilities shared across workspace binaries.
pub mod message;
