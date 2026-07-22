//! Per-file dispatch helpers for batch replay.
//!
//! This module contains the routines that the main replay loop calls for
//! each transferred file:
//!
//! - [`read_iflags_and_skip_meta`] reads the per-file iflags word and
//!   consumes any `ITEM_BASIS_TYPE_FOLLOWS` / `ITEM_XNAME_FOLLOWS` trailers.
//! - [`read_sum_head`] reads the 16-byte sum_head block geometry.
//! - [`read_compressed_deltas_streaming`] runs the CPRES_ZLIB token loop
//!   with dictionary synchronization via `see_token()`.
//! - [`apply_file_delta`] applies a decoded delta sequence to a destination
//!   path, using a temp file + rename for the basis-present path.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;

use protocol::wire::{CompressedToken, CompressedTokenDecoder};

use crate::error::{BatchError, BatchResult};

use super::delta::{apply_delta_ops, write_literals_to_file};

/// ITEM_BASIS_TYPE_FOLLOWS - 1-byte fnamecmp_type follows iflags.
/// upstream: rsync.c:403-418
pub(super) const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11;
/// ITEM_XNAME_FOLLOWS - vstring (1-2 byte length + data) follows iflags.
/// upstream: rsync.c:403-418
pub(super) const ITEM_XNAME_FOLLOWS: u16 = 1 << 12;
/// ITEM_TRANSFER - delta data follows after the per-file metadata block.
/// upstream: rsync.c
pub(super) const ITEM_TRANSFER: u16 = 0x8000;

/// Read the per-file iflags word and consume any optional trailing fields.
///
/// upstream: rsync.c:383 - read iflags (u16) for protocol >= 29.
/// iflags MUST be read before the entry lookup because the stream contains
/// iflags for every positive NDX, including directory metadata updates where
/// NDX < ndx_start (INC_RECURSE).
///
/// upstream: rsync.c:403-418 - consume optional trailing fields after iflags.
/// `ITEM_BASIS_TYPE_FOLLOWS` (0x0800): 1 byte fnamecmp_type.
/// `ITEM_XNAME_FOLLOWS` (0x1000): vstring (1-2 byte length + data).
pub(super) fn read_iflags_and_skip_meta(
    stream: &mut BufReader<File>,
    proto: i32,
) -> BatchResult<u16> {
    let iflags = if proto >= 29 {
        let mut buf = [0u8; 2];
        stream.read_exact(&mut buf).map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to read iflags: {e}"),
            ))
        })?;
        u16::from_le_bytes(buf)
    } else {
        // upstream: rsync.c:384 - default to ITEM_TRANSFER | ITEM_MISSING_DATA
        0x8000 | 0x0400
    };

    if iflags & ITEM_BASIS_TYPE_FOLLOWS != 0 {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to read fnamecmp_type: {e}"),
            ))
        })?;
    }

    if iflags & ITEM_XNAME_FOLLOWS != 0 {
        // upstream: io.c:read_vstring() - 1-byte length, or 2-byte if high bit set
        let mut len_byte = [0u8; 1];
        stream.read_exact(&mut len_byte).map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to read xname length: {e}"),
            ))
        })?;
        let mut xname_len = len_byte[0] as usize;
        if xname_len & 0x80 != 0 {
            let mut hi = [0u8; 1];
            stream.read_exact(&mut hi).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to read xname extended length: {e}"),
                ))
            })?;
            xname_len = (xname_len & !0x80) * 0x100 + hi[0] as usize;
        }
        if xname_len > 0 {
            let mut xname_buf = vec![0u8; xname_len];
            stream.read_exact(&mut xname_buf).map_err(|e| {
                BatchError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to read xname data: {e}"),
                ))
            })?;
        }
    }

    Ok(iflags)
}

/// Read the 16-byte `sum_head` and return `(block_count, block_length_wire, remainder_wire)`.
///
/// upstream: receiver.c:338 - `read_sum_head()` reads 4 x i32.
/// The `s2length` field is read and discarded - oc-rsync derives the strong
/// checksum length from the negotiated checksum algorithm, not from the wire.
pub(super) fn read_sum_head(stream: &mut BufReader<File>) -> BatchResult<(i32, i32, i32)> {
    let mut sum_buf = [0u8; 16];
    stream.read_exact(&mut sum_buf).map_err(|e| {
        BatchError::Io(std::io::Error::new(
            e.kind(),
            format!("failed to read sum_head: {e}"),
        ))
    })?;
    let block_count = i32::from_le_bytes([sum_buf[0], sum_buf[1], sum_buf[2], sum_buf[3]]);
    let block_length_wire = i32::from_le_bytes([sum_buf[4], sum_buf[5], sum_buf[6], sum_buf[7]]);
    let _s2length = i32::from_le_bytes([sum_buf[8], sum_buf[9], sum_buf[10], sum_buf[11]]);
    let remainder_wire = i32::from_le_bytes([sum_buf[12], sum_buf[13], sum_buf[14], sum_buf[15]]);
    Ok((block_count, block_length_wire, remainder_wire))
}

/// Read the per-file transfer checksum and discard it.
///
/// upstream: receiver.c:515 - `read_buf(f_in, sender_file_sum, xfer_sum_len)`.
/// The sender ALWAYS writes `xfer_sum_len` bytes of file checksum after the
/// delta stream, regardless of `sum_head.s2length`. For protocol 32 the
/// default xfer checksum is XXH3-128 or MD5 - both 16 bytes. For protocol
/// 28-31 it is MD4 or MD5 - also 16 bytes.
pub(super) fn read_and_discard_file_checksum(
    stream: &mut BufReader<File>,
    xfer_sum_len: usize,
) -> BatchResult<()> {
    let mut checksum_buf = vec![0u8; xfer_sum_len];
    stream.read_exact(&mut checksum_buf).map_err(|e| {
        BatchError::Io(std::io::Error::new(
            e.kind(),
            format!("failed to read file checksum ({xfer_sum_len} bytes): {e}"),
        ))
    })?;
    Ok(())
}

/// CPRES_ZLIB streaming read with dictionary synchronization.
///
/// After each token, `see_token()` feeds the data into the decompressor
/// dictionary so subsequent tokens can reference it via back-references.
/// Without this, inflate fails with "invalid distance too far back".
///
/// upstream: receiver.c:receive_data() + token.c:see_deflate_token()
pub(super) fn read_compressed_deltas_streaming(
    decoder: &mut CompressedTokenDecoder,
    stream: &mut BufReader<File>,
    basis_data: &[u8],
    entry_name: &str,
    block_length: usize,
    block_count: i32,
    remainder: usize,
) -> BatchResult<Vec<protocol::wire::DeltaOp>> {
    let mut ops = Vec::new();
    loop {
        let token = decoder.recv_token(stream).map_err(|e| {
            BatchError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to read compressed delta token for '{entry_name}': {e}"),
            ))
        })?;
        match token {
            CompressedToken::End => break,
            CompressedToken::Literal(data) => {
                // Literals already pass through inflate in recv_token(),
                // which updates the decompressor dictionary. Do NOT call
                // see_token() here - upstream receiver.c only calls
                // see_token() for block matches, not literals.
                ops.push(protocol::wire::DeltaOp::Literal(data));
            }
            CompressedToken::BlockMatch(block_index) => {
                // Feed matched block's basis data into dictionary.
                // upstream: receiver.c - see_token(map, len) after block match
                let offset = block_index as usize * block_length;
                let len = if block_index == block_count as u32 - 1 {
                    remainder
                } else {
                    block_length
                };
                let end = (offset + len).min(basis_data.len());
                if offset < basis_data.len() {
                    decoder
                        .see_token(&basis_data[offset..end])
                        .map_err(BatchError::Io)?;
                }
                ops.push(protocol::wire::DeltaOp::Copy {
                    block_index,
                    length: 0,
                });
            }
        }
    }
    Ok(ops)
}

/// Apply a decoded delta sequence to `dest_path`.
///
/// When `basis_exists` is false, writes literals directly to the destination
/// path. Otherwise, applies the deltas against the existing basis file using
/// a temp file (`<dest>.~batch-tmp`) and atomically renames the result into
/// place to avoid corrupting the basis on partial failure.
pub(super) fn apply_file_delta(
    dest_path: &Path,
    basis_exists: bool,
    delta_ops: Vec<protocol::wire::DeltaOp>,
    block_length: usize,
    block_count: u32,
    remainder: usize,
) -> BatchResult<()> {
    if !basis_exists {
        write_literals_to_file(dest_path, &delta_ops)?;
        return Ok(());
    }
    let temp_path = dest_path.with_extension("~batch-tmp");
    apply_delta_ops(
        dest_path,
        &temp_path,
        delta_ops,
        block_length,
        block_count,
        remainder,
    )?;
    fs::rename(&temp_path, dest_path).map_err(|e| {
        BatchError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "failed to rename temp file '{}' to '{}': {e}",
                temp_path.display(),
                dest_path.display()
            ),
        ))
    })?;
    Ok(())
}
