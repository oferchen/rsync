#![deny(unsafe_code)]
//! Server-side Receiver role implementation.
//!
//! When the native server operates as a Receiver, it:
//! 1. Receives the file list from the client (sender)
//! 2. Generates signatures for existing local files
//! 3. Receives delta data and applies it to create/update files
//! 4. Sets metadata (permissions, times, ownership) on received files

use std::fs;
use std::io::{self, Read, Write};
use std::num::NonZeroU8;

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListReader};
use protocol::wire::{read_delta, write_signature, DeltaOp};

use engine::delta::{calculate_signature_layout, DeltaScript, DeltaToken, SignatureLayoutParams};
use engine::signature::{generate_file_signature, SignatureAlgorithm};

use super::config::ServerConfig;
use super::handshake::HandshakeResult;

/// Context for the receiver role during a transfer.
#[derive(Debug)]
pub struct ReceiverContext {
    /// Negotiated protocol version.
    protocol: ProtocolVersion,
    /// Server configuration.
    config: ServerConfig,
    /// List of files to receive.
    file_list: Vec<FileEntry>,
}

impl ReceiverContext {
    /// Creates a new receiver context from handshake result and config.
    pub fn new(handshake: &HandshakeResult, config: ServerConfig) -> Self {
        Self {
            protocol: handshake.protocol,
            config,
            file_list: Vec::new(),
        }
    }

    /// Returns the negotiated protocol version.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns a reference to the server configuration.
    #[must_use]
    pub const fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Returns the received file list.
    #[must_use]
    pub fn file_list(&self) -> &[FileEntry] {
        &self.file_list
    }

    /// Receives the file list from the sender.
    ///
    /// The file list is sent by the client in the rsync wire format with
    /// path compression and conditional fields based on flags.
    pub fn receive_file_list<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<usize> {
        let mut flist_reader = FileListReader::new(self.protocol);
        let mut count = 0;

        while let Some(entry) = flist_reader.read_entry(reader)? {
            self.file_list.push(entry);
            count += 1;
        }

        Ok(count)
    }

    /// Runs the receiver role to completion.
    ///
    /// This orchestrates the full receive operation:
    /// 1. Receive file list
    /// 2. For each file: generate signature, receive delta, apply
    /// 3. Set final metadata
    pub fn run<R: Read + ?Sized, W: Write + ?Sized>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
    ) -> io::Result<TransferStats> {
        // Receive file list from sender
        let file_count = self.receive_file_list(reader)?;

        // Send NDX_DONE (-1) to signal we're ready for transfer phase
        // This is CRITICAL - the sender is blocked waiting for this!
        // Mirrors upstream's write_ndx(f_out, NDX_DONE) in io.c:2259-2262
        // For protocol >= 30, NDX_DONE is encoded as a single byte 0x00
        writer.write_all(&[0])?;
        writer.flush()?;

        // Transfer loop: for each file, generate signature, receive delta, apply
        let mut files_transferred = 0;
        let mut bytes_received = 0u64;

        // Use MD5 for strong checksums (default for protocol >= 30)
        let checksum_length = NonZeroU8::new(16).expect("checksum length must be non-zero");

        for file_entry in &self.file_list {
            // Step 1: Try to open existing basis file
            let basis_path = file_entry.path();
            let basis_file_opt = fs::File::open(basis_path).ok();

            // Step 2: Generate signature if basis exists
            if let Some(basis_file) = basis_file_opt {
                let file_size = basis_file.metadata()?.len();

                let params = SignatureLayoutParams::new(
                    file_size,
                    None, // Use default block size heuristic
                    self.protocol,
                    checksum_length,
                );

                match calculate_signature_layout(params) {
                    Ok(layout) => {
                        // Generate the signature
                        match generate_file_signature(basis_file, layout, SignatureAlgorithm::Md5) {
                            Ok(signature) => {
                                // Send signature to generator (inline to avoid ?Sized issues)
                                use protocol::wire::signature::SignatureBlock as WireBlock;
                                let sig_layout = signature.layout();
                                let wire_blocks: Vec<WireBlock> = signature
                                    .blocks()
                                    .iter()
                                    .map(|block| WireBlock {
                                        index: block.index() as u32,
                                        rolling_sum: block.rolling().value(),
                                        strong_sum: block.strong().to_vec(),
                                    })
                                    .collect();
                                write_signature(
                                    &mut &mut *writer,
                                    sig_layout.block_count() as u32,
                                    sig_layout.block_length().get(),
                                    sig_layout.strong_sum_length().get(),
                                    &wire_blocks,
                                )?;
                            }
                            Err(_) => {
                                // If signature generation fails, fall back to whole-file transfer
                                write_signature(&mut &mut *writer, 0, 0, 0, &[])?;
                            }
                        }
                    }
                    Err(_) => {
                        // Layout calculation failed, use whole-file transfer
                        write_signature(&mut &mut *writer, 0, 0, 0, &[])?;
                    }
                }
            } else {
                // Step 3: No basis file, send marker to request whole file
                write_signature(&mut &mut *writer, 0, 0, 0, &[])?;
            }

            writer.flush()?;

            // Step 4: Receive delta operations from generator
            let wire_delta = read_delta(&mut &mut *reader)?;
            let delta_script = wire_delta_to_script(wire_delta);

            // Step 5: Apply delta to reconstruct file
            // TODO: Implement file reconstruction with temp files and metadata
            // For now, track stats
            bytes_received += delta_script.total_bytes();
            files_transferred += 1;
        }

        Ok(TransferStats {
            files_listed: file_count,
            files_transferred,
            bytes_received,
        })
    }
}

/// Statistics from a receiver transfer operation.
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    /// Number of files in the received file list.
    pub files_listed: usize,
    /// Number of files actually transferred.
    pub files_transferred: usize,
    /// Total bytes received.
    pub bytes_received: u64,
}

// Helper functions for delta transfer

/// Converts wire protocol delta operations to engine delta script.
fn wire_delta_to_script(ops: Vec<DeltaOp>) -> DeltaScript {
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

#[cfg(test)]
mod tests {
    use super::super::flags::ParsedServerFlags;
    use super::super::role::ServerRole;
    use super::*;
    use std::ffi::OsString;
    use std::io::Cursor;

    fn test_config() -> ServerConfig {
        ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_string(),
            flags: ParsedServerFlags::default(),
            args: vec![OsString::from(".")],
        }
    }

    fn test_handshake() -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
        }
    }

    #[test]
    fn receiver_context_creation() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        assert_eq!(ctx.protocol().as_u8(), 32);
        assert!(ctx.file_list().is_empty());
    }

    #[test]
    fn receiver_empty_file_list() {
        let handshake = test_handshake();
        let config = test_config();
        let mut ctx = ReceiverContext::new(&handshake, config);

        // Empty file list (just the end marker)
        let data = [0u8];
        let mut cursor = Cursor::new(&data[..]);

        let count = ctx.receive_file_list(&mut cursor).unwrap();
        assert_eq!(count, 0);
        assert!(ctx.file_list().is_empty());
    }

    #[test]
    fn receiver_single_file() {
        let handshake = test_handshake();
        let config = test_config();
        let mut ctx = ReceiverContext::new(&handshake, config);

        // Single file entry followed by end marker
        // flags: XMIT_SAME_TIME | XMIT_SAME_MODE = 0x60
        let mut data = Vec::new();
        data.push(0x60); // flags
        data.push(8); // name length
        data.extend_from_slice(b"test.txt"); // name
        data.push(100); // size
        data.push(0); // end marker

        let mut cursor = Cursor::new(&data[..]);
        let count = ctx.receive_file_list(&mut cursor).unwrap();

        assert_eq!(count, 1);
        assert_eq!(ctx.file_list().len(), 1);
        assert_eq!(ctx.file_list()[0].name(), "test.txt");
    }
}
