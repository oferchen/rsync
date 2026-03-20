//! Batch file header containing protocol negotiation information.
//!
//! The header is written at the very beginning of the batch file and
//! contains the information needed to replay the batch. The format
//! matches upstream rsync's `batch.c:write_stream_flags()` +
//! `io.c:start_write_batch()`.

use std::io::{self, Read, Write};

use super::flags::BatchFlags;
use super::wire::{read_i32, read_varint, write_i32, write_varint};

/// Batch file header containing protocol negotiation information.
///
/// Written at the start of every batch file with this layout:
///
/// 1. Stream flags bitmap (i32) - `write_stream_flags(batch_fd)`
/// 2. Protocol version (i32) - `write_int(batch_fd, protocol_version)`
/// 3. Compat flags (varint, protocol >= 30) - `write_varint(batch_fd, compat_flags)`
/// 4. Checksum seed (i32) - `write_int(batch_fd, checksum_seed)`
///
/// After the header, the batch file body is a raw tee of the protocol stream
/// (file list + delta operations), followed by trailing [`super::BatchStats`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchHeader {
    /// Protocol version (i32).
    pub protocol_version: i32,
    /// Compatibility flags (varint for protocol >= 30, None otherwise).
    pub compat_flags: Option<i32>,
    /// Checksum seed for this transfer (i32).
    pub checksum_seed: i32,
    /// Stream flags bitmap (i32).
    pub stream_flags: BatchFlags,
}

impl BatchHeader {
    /// Create a new batch header.
    pub fn new(protocol_version: i32, checksum_seed: i32) -> Self {
        Self {
            protocol_version,
            compat_flags: if protocol_version >= 30 {
                Some(0)
            } else {
                None
            },
            checksum_seed,
            stream_flags: BatchFlags::default(),
        }
    }

    /// Write the header to a writer.
    ///
    /// Format matches upstream rsync batch.c:
    /// 1. Stream flags bitmap (i32)
    /// 2. Protocol version (i32)
    /// 3. Compat flags (varint, if protocol >= 30)
    /// 4. Checksum seed (i32)
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        // upstream: batch.c:write_stream_flags()
        self.stream_flags
            .write_to_versioned(writer, self.protocol_version)?;

        // upstream: io.c batch initialization
        write_i32(writer, self.protocol_version)?;

        if let Some(flags) = self.compat_flags {
            write_varint(writer, flags)?;
        }

        write_i32(writer, self.checksum_seed)?;

        Ok(())
    }

    /// Read the header from a reader.
    ///
    /// Format matches upstream rsync (same order as write_to):
    /// 1. Stream flags bitmap (i32)
    /// 2. Protocol version (i32)
    /// 3. Compat flags (varint, if protocol >= 30)
    /// 4. Checksum seed (i32)
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        let raw_bitmap = BatchFlags::read_raw(reader)?;

        let protocol_version = read_i32(reader)?;

        // Reconstruct flags with the correct protocol version mask
        let stream_flags = BatchFlags::from_bitmap(raw_bitmap, protocol_version);

        let compat_flags = if protocol_version >= 30 {
            Some(read_varint(reader)?)
        } else {
            None
        };

        let checksum_seed = read_i32(reader)?;

        Ok(Self {
            protocol_version,
            compat_flags,
            checksum_seed,
            stream_flags,
        })
    }
}
