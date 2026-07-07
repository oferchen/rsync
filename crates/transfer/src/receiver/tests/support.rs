//! Shared fixtures and helpers used across the receiver test modules.
//!
//! Re-export everything so each per-surface test module can pull in the
//! helpers with a single `use super::support::*` line.

use std::ffi::OsString;
use std::io::{self, Write};

use engine::delta::{DeltaScript, DeltaToken};
use protocol::ProtocolVersion;
use protocol::flist::FileEntry;
use protocol::wire::DeltaOp;

use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

#[cfg(feature = "incremental-flist")]
pub(super) use super::super::PHASE1_CHECKSUM_LENGTH;
pub(super) use super::super::REDO_CHECKSUM_LENGTH;
use super::super::ReceiverContext;

/// Default test config: protocol 32 receiver with no flags set.
pub(super) fn test_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    }
}

/// Default handshake (protocol 32).
pub(super) fn test_handshake() -> HandshakeResult {
    test_handshake_with_protocol(32)
}

/// Creates a [`HandshakeResult`] with a specific protocol version for testing.
pub(super) fn test_handshake_with_protocol(protocol_version: u8) -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

/// Builds an id-list test config with the requested owner/group/numeric-ids flags.
pub(super) fn config_with_flags(owner: bool, group: bool, numeric_ids: bool) -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            owner,
            group,
            numeric_ids,
            ..ParsedServerFlags::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    }
}

/// Applies a delta script to create a new file (whole-file transfer, no basis).
///
/// All tokens must be Literal; Copy operations indicate a protocol error.
pub(super) fn apply_whole_file_delta(
    path: &std::path::Path,
    script: &DeltaScript,
) -> io::Result<()> {
    let mut output = std::fs::File::create(path)?;

    for token in script.tokens() {
        match token {
            DeltaToken::Literal(data) => {
                output.write_all(data)?;
            }
            DeltaToken::Copy { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Copy operation in whole-file transfer (no basis exists)",
                ));
            }
        }
    }

    output.sync_all()?;
    Ok(())
}

/// Converts wire protocol delta operations to engine delta script.
pub(super) fn wire_delta_to_script(ops: Vec<DeltaOp>) -> DeltaScript {
    let mut tokens = Vec::with_capacity(ops.len());
    let mut total_bytes = 0u64;
    let mut literal_bytes = 0u64;

    for op in ops {
        match op {
            DeltaOp::Literal(data) => {
                let len = data.len() as u64;
                total_bytes += len;
                literal_bytes += len;
                tokens.push(DeltaToken::Literal(data));
            }
            DeltaOp::Copy {
                block_index,
                length,
            } => {
                total_bytes += length as u64;
                tokens.push(DeltaToken::Copy {
                    index: block_index as u64,
                    len: length as usize,
                });
            }
        }
    }

    DeltaScript::new(tokens, total_bytes, literal_bytes)
}

/// Creates a `ReceiverContext` configured for hardlink testing with protocol 32.
pub(super) fn receiver_with_hardlinks(entries: Vec<FileEntry>) -> ReceiverContext {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpHre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = entries;
    ctx
}

/// Helper to create a hardlink leader entry with appropriate flags.
pub(super) fn make_hlink_leader(name: &str, size: u64, gnum: u32) -> FileEntry {
    let mut entry = FileEntry::new_file(name.into(), size, 0o644);
    entry.set_hlinked(true);
    entry.set_hlink_first(true);
    entry.set_hardlink_idx(gnum);
    entry
}

/// Helper to create a hardlink follower entry with appropriate flags.
pub(super) fn make_hlink_follower(name: &str, size: u64, gnum: u32) -> FileEntry {
    let mut entry = FileEntry::new_file(name.into(), size, 0o644);
    entry.set_hlinked(true);
    entry.set_hardlink_idx(gnum);
    entry
}

/// Minimal writer that discards output and provides a no-op `MsgInfoSender`.
pub(super) struct TestDeletionWriter;

impl Write for TestDeletionWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl crate::writer::MsgInfoSender for TestDeletionWriter {
    fn send_msg_info(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

/// Writer that records every `MSG_INFO` frame so tests can assert the
/// emitted `*deleting` itemize order.
#[cfg(unix)]
#[derive(Default)]
pub(super) struct CapturingDeletionWriter {
    pub(super) lines: Vec<String>,
}

#[cfg(unix)]
impl Write for CapturingDeletionWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
impl crate::writer::MsgInfoSender for CapturingDeletionWriter {
    fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
        self.lines
            .push(String::from_utf8_lossy(data).trim_end().to_owned());
        Ok(())
    }
}
