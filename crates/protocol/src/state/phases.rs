//! Protocol phase marker types for the typestate pattern.
//!
//! Each phase captures the data accumulated during that stage of the protocol lifecycle.

use std::fmt;

/// Protocol phase marker trait for type-safe transitions.
pub trait ProtocolPhase: fmt::Debug + Send + Sync {
    /// Human-readable name of this phase.
    fn name(&self) -> &'static str;
}

/// Negotiation phase - protocol version and capability exchange.
#[derive(Debug, Clone, Default)]
pub struct Negotiation {
    /// The negotiated protocol version, if set.
    pub protocol_version: Option<u32>,
    /// The checksum seed, if set.
    ///
    /// upstream: `options.c:151` declares `int checksum_seed`, so the seed is
    /// signed end to end and half its domain is negative.
    /// `options.c:861` parses it with POPT_ARG_INT.
    /// `compat.c:825` puts it on the wire with `write_int(f_out, checksum_seed)`.
    pub checksum_seed: Option<i32>,
}

/// FileList phase - file list exchange between sender and receiver.
#[derive(Debug, Clone)]
pub struct FileList {
    /// The negotiated protocol version.
    pub protocol_version: u32,
    /// The checksum seed for the session.
    ///
    /// upstream: `options.c:151` `int checksum_seed` - signed.
    pub checksum_seed: i32,
    /// The number of files in the list, if known.
    pub file_count: Option<usize>,
}

/// Transfer phase - delta transfer of file contents.
#[derive(Debug, Clone)]
pub struct Transfer {
    /// The negotiated protocol version.
    pub protocol_version: u32,
    /// The checksum seed for the session.
    ///
    /// upstream: `options.c:151` `int checksum_seed` - signed.
    pub checksum_seed: i32,
    /// The total number of files to transfer.
    pub file_count: usize,
    /// The number of files transferred so far.
    pub files_transferred: usize,
}

/// Finalize phase - statistics exchange and cleanup.
#[derive(Debug, Clone)]
pub struct Finalize {
    /// The negotiated protocol version.
    pub protocol_version: u32,
    /// The total number of files.
    pub total_files: usize,
    /// The number of files transferred.
    pub files_transferred: usize,
}

impl ProtocolPhase for Negotiation {
    fn name(&self) -> &'static str {
        "negotiation"
    }
}

impl ProtocolPhase for FileList {
    fn name(&self) -> &'static str {
        "file_list"
    }
}

impl ProtocolPhase for Transfer {
    fn name(&self) -> &'static str {
        "transfer"
    }
}

impl ProtocolPhase for Finalize {
    fn name(&self) -> &'static str {
        "finalize"
    }
}
