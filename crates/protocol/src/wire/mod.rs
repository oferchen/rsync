#![deny(unsafe_code)]
//! Wire protocol serialization for file lists, signatures, and deltas.
//!
//! This module provides the serialization and deserialization logic for the
//! rsync protocol's data structures. The formats mirror upstream rsync 3.4.1
//! to ensure interoperability.

pub mod delta;
pub mod file_entry;
pub mod signature;

pub use self::delta::{DeltaOp, read_delta, write_delta};
pub use self::file_entry::{FileEntry, FileEntryFlags, FileType};
pub use self::signature::{SignatureBlock, read_signature, write_signature};
