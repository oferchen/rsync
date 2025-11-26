#![deny(unsafe_code)]
//! Server-side Receiver role implementation.
//!
//! When the native server operates as a Receiver, it:
//! 1. Receives the file list from the client (sender)
//! 2. Generates signatures for existing local files
//! 3. Receives delta data and applies it to create/update files
//! 4. Sets metadata (permissions, times, ownership) on received files

use std::io::{self, Read, Write};

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListReader};

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

        // For now, just report what we received
        // Delta receiving and application will be implemented next
        Ok(TransferStats {
            files_listed: file_count,
            files_transferred: 0,
            bytes_received: 0,
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
