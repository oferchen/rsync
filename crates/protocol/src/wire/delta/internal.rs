#![deny(unsafe_code)]
//! Internal opcode-based delta format.
//!
//! This format is used for backward compatibility with earlier versions of this
//! implementation. For upstream rsync compatibility, use the token-based functions
//! in the [`super::token`] module instead.

use std::io::{self, Read, Write};

use crate::varint::{read_varint, write_varint};

use super::types::DeltaOp;

/// Writes a delta operation to the internal wire format.
///
/// For upstream rsync compatibility, use [`super::write_token_stream`] instead.
///
/// # Wire Format
///
/// **Opcode** (1 byte):
/// - `0x00` = Literal
/// - `0x01` = Copy
///
/// **For Literal** (`0x00`):
/// - Length (varint)
/// - Data bytes
///
/// **For Copy** (`0x01`):
/// - Block index (varint)
/// - Length (varint)
///
/// # Errors
///
/// Returns an error if writing to the underlying stream fails.
pub fn write_delta_op<W: Write>(writer: &mut W, op: &DeltaOp) -> io::Result<()> {
    match op {
        DeltaOp::Literal(data) => {
            writer.write_all(&[0x00])?;
            write_varint(writer, data.len() as i32)?;
            writer.write_all(data)?;
        }
        DeltaOp::Copy {
            block_index,
            length,
        } => {
            writer.write_all(&[0x01])?;
            write_varint(writer, *block_index as i32)?;
            write_varint(writer, *length as i32)?;
        }
    }
    Ok(())
}

/// Reads a delta operation from the internal wire format.
///
/// This is the counterpart to [`write_delta_op`], decoding the opcode-based
/// format. For upstream rsync compatibility, use [`super::read_token`] instead.
///
/// # Errors
///
/// Returns an error if:
/// - Reading from the underlying stream fails
/// - An invalid opcode is encountered (not 0x00 or 0x01)
pub fn read_delta_op<R: Read>(reader: &mut R) -> io::Result<DeltaOp> {
    let mut opcode = [0u8; 1];
    reader.read_exact(&mut opcode)?;

    match opcode[0] {
        0x00 => {
            let raw_length = read_varint(reader)?;
            if raw_length < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "negative literal length in delta op",
                ));
            }
            let length = raw_length as usize;
            let mut data = vec![0u8; length];
            reader.read_exact(&mut data)?;
            Ok(DeltaOp::Literal(data))
        }
        0x01 => {
            let block_index = read_varint(reader)? as u32;
            let length = read_varint(reader)? as u32;
            Ok(DeltaOp::Copy {
                block_index,
                length,
            })
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid delta opcode: 0x{other:02X}"),
        )),
    }
}

/// Writes a complete delta stream to the internal wire format.
///
/// For upstream rsync compatibility, use [`super::write_token_stream`] instead.
///
/// # Wire Format
///
/// - Operation count (varint)
/// - For each operation: delta operation (see [`write_delta_op`])
///
/// # Errors
///
/// Returns an error if writing to the underlying stream fails.
pub fn write_delta<W: Write>(writer: &mut W, ops: &[DeltaOp]) -> io::Result<()> {
    write_varint(writer, ops.len() as i32)?;
    for op in ops {
        write_delta_op(writer, op)?;
    }
    Ok(())
}

/// Reads a complete delta stream from the internal wire format.
///
/// This is the counterpart to [`write_delta`], decoding the opcode-based format.
/// For upstream rsync compatibility, use [`super::read_token`] instead.
///
/// # Errors
///
/// Returns an error if:
/// - Reading from the underlying stream fails
/// - An invalid opcode is encountered in any delta operation
pub fn read_delta<R: Read>(reader: &mut R) -> io::Result<Vec<DeltaOp>> {
    let raw_count = read_varint(reader)?;
    if raw_count < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "negative delta operation count",
        ));
    }
    let count = raw_count as usize;
    // Cap pre-allocation to avoid OOM on malformed input; the loop still
    // iterates `count` times but will hit EOF naturally if the data is short.
    let mut ops = Vec::with_capacity(count.min(1024));

    for _ in 0..count {
        ops.push(read_delta_op(reader)?);
    }

    Ok(ops)
}
