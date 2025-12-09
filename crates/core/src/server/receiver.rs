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

use engine::delta::{apply_delta, calculate_signature_layout, DeltaScript, DeltaSignatureIndex, DeltaToken, SignatureLayoutParams};
use engine::signature::{generate_file_signature, FileSignature, SignatureAlgorithm};

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
            let basis_path = file_entry.path();

            // Step 1 & 2: Generate signature if basis file exists
            let signature_opt: Option<FileSignature> = 'sig: {
                let basis_file = match fs::File::open(basis_path) {
                    Ok(f) => f,
                    Err(_) => break 'sig None,
                };

                let file_size = match basis_file.metadata() {
                    Ok(meta) => meta.len(),
                    Err(_) => break 'sig None,
                };

                let params = SignatureLayoutParams::new(
                    file_size,
                    None, // Use default block size heuristic
                    self.protocol,
                    checksum_length,
                );

                let layout = match calculate_signature_layout(params) {
                    Ok(layout) => layout,
                    Err(_) => break 'sig None,
                };

                match generate_file_signature(basis_file, layout, SignatureAlgorithm::Md5) {
                    Ok(signature) => Some(signature),
                    Err(_) => None,
                }
            };

            // Step 3: Send signature or no-basis marker
            if let Some(ref signature) = signature_opt {
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
            } else {
                // No basis, request whole file
                write_signature(&mut &mut *writer, 0, 0, 0, &[])?;
            }
            writer.flush()?;

            // Step 4: Receive delta operations from generator
            let wire_delta = read_delta(&mut &mut *reader)?;
            let delta_script = wire_delta_to_script(wire_delta);

            // Step 5: Apply delta to reconstruct file
            let temp_path = basis_path.with_extension("oc-rsync.tmp");

            if let Some(signature) = signature_opt {
                // Delta transfer: apply delta using basis file
                let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md5);

                if let Some(index) = index {
                    // Open basis file for reading
                    let basis = fs::File::open(basis_path)?;
                    let mut output = fs::File::create(&temp_path)?;

                    // Apply the delta
                    apply_delta(basis, &mut output, &index, &delta_script)?;
                    output.sync_all()?;

                    // Atomic rename
                    fs::rename(&temp_path, basis_path)?;
                } else {
                    // Index creation failed (file too small?), fall back to whole-file
                    apply_whole_file_delta(&temp_path, &delta_script)?;
                    fs::rename(&temp_path, basis_path)?;
                }
            } else {
                // Whole-file transfer: no basis, all literals
                apply_whole_file_delta(&temp_path, &delta_script)?;
                fs::rename(&temp_path, basis_path)?;
            }

            // Step 6: Track stats
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

/// Applies a delta script to create a new file (whole-file transfer, no basis).
///
/// All tokens must be Literal; Copy operations indicate a protocol error.
fn apply_whole_file_delta(path: &std::path::Path, script: &DeltaScript) -> io::Result<()> {
    let mut output = fs::File::create(path)?;

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

    #[test]
    fn wire_delta_to_script_converts_literals() {
        use protocol::wire::DeltaOp;

        let wire_ops = vec![
            DeltaOp::Literal(vec![1, 2, 3, 4]),
            DeltaOp::Literal(vec![5, 6, 7, 8]),
        ];

        let script = wire_delta_to_script(wire_ops);

        assert_eq!(script.tokens().len(), 2);
        assert_eq!(script.total_bytes(), 8);
        assert_eq!(script.literal_bytes(), 8);

        match &script.tokens()[0] {
            DeltaToken::Literal(data) => assert_eq!(data, &vec![1, 2, 3, 4]),
            _ => panic!("expected literal token"),
        }
    }

    #[test]
    fn wire_delta_to_script_converts_copy_operations() {
        use protocol::wire::DeltaOp;

        let wire_ops = vec![
            DeltaOp::Copy {
                block_index: 0,
                length: 1024,
            },
            DeltaOp::Literal(vec![9, 10]),
            DeltaOp::Copy {
                block_index: 1,
                length: 512,
            },
        ];

        let script = wire_delta_to_script(wire_ops);

        assert_eq!(script.tokens().len(), 3);
        assert_eq!(script.total_bytes(), 1024 + 2 + 512);
        assert_eq!(script.literal_bytes(), 2);

        match &script.tokens()[0] {
            DeltaToken::Copy { index, len } => {
                assert_eq!(*index, 0);
                assert_eq!(*len, 1024);
            }
            _ => panic!("expected copy token"),
        }
    }

    #[test]
    fn apply_whole_file_delta_accepts_only_literals() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let output_path = temp_dir.path().join("output.txt");

        // Create a delta script with only literals
        let tokens = vec![
            DeltaToken::Literal(b"Hello, ".to_vec()),
            DeltaToken::Literal(b"world!".to_vec()),
        ];
        let script = DeltaScript::new(tokens, 13, 13);

        apply_whole_file_delta(&output_path, &script).unwrap();

        let result = std::fs::read(&output_path).unwrap();
        assert_eq!(result, b"Hello, world!");
    }

    #[test]
    fn apply_whole_file_delta_rejects_copy_operations() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let output_path = temp_dir.path().join("output.txt");

        // Create a delta script with a copy operation (invalid for whole-file transfer)
        let tokens = vec![
            DeltaToken::Literal(b"data".to_vec()),
            DeltaToken::Copy { index: 0, len: 1024 },
        ];
        let script = DeltaScript::new(tokens, 1028, 4);

        let result = apply_whole_file_delta(&output_path, &script);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
