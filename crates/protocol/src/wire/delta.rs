#![deny(unsafe_code)]
//! Delta token wire format for file reconstruction.
//!
//! This module implements serialization for delta operations used to reconstruct
//! files from a basis file. Delta streams consist of literal data writes and
//! copy operations that reference blocks in the basis file.

use std::io::{self, Read, Write};

use crate::varint::{read_varint, write_varint};

/// Delta operation for file reconstruction.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DeltaOp {
    /// Write literal bytes to output.
    Literal(Vec<u8>),
    /// Copy bytes from basis file at given block index.
    Copy {
        /// Block index in basis file.
        block_index: u32,
        /// Number of bytes to copy.
        length: u32,
    },
}

/// Writes a delta operation to the wire format.
///
/// Format:
/// - Op code (1 byte):
///   - 0x00 = Literal
///   - 0x01 = Copy
/// - For Literal:
///   - Length (varint)
///   - Data bytes
/// - For Copy:
///   - Block index (varint)
///   - Length (varint)
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

/// Reads a delta operation from the wire format.
pub fn read_delta_op<R: Read>(reader: &mut R) -> io::Result<DeltaOp> {
    let mut opcode = [0u8; 1];
    reader.read_exact(&mut opcode)?;

    match opcode[0] {
        0x00 => {
            let length = read_varint(reader)? as usize;
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
            format!("invalid delta opcode: 0x{:02X}", other),
        )),
    }
}

/// Writes a complete delta stream to the wire format.
///
/// Format:
/// - Operation count (varint)
/// - For each operation:
///   - Delta operation (see `write_delta_op`)
pub fn write_delta<W: Write>(writer: &mut W, ops: &[DeltaOp]) -> io::Result<()> {
    write_varint(writer, ops.len() as i32)?;
    for op in ops {
        write_delta_op(writer, op)?;
    }
    Ok(())
}

/// Reads a complete delta stream from the wire format.
pub fn read_delta<R: Read>(reader: &mut R) -> io::Result<Vec<DeltaOp>> {
    let count = read_varint(reader)? as usize;
    let mut ops = Vec::with_capacity(count);

    for _ in 0..count {
        ops.push(read_delta_op(reader)?);
    }

    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_op_roundtrip_literal() {
        let op = DeltaOp::Literal(vec![0x01, 0x02, 0x03, 0x04, 0x05]);

        let mut buf = Vec::new();
        write_delta_op(&mut buf, &op).unwrap();

        let decoded = read_delta_op(&mut &buf[..]).unwrap();

        assert_eq!(decoded, op);
    }

    #[test]
    fn delta_op_roundtrip_copy() {
        let op = DeltaOp::Copy {
            block_index: 42,
            length: 4096,
        };

        let mut buf = Vec::new();
        write_delta_op(&mut buf, &op).unwrap();

        let decoded = read_delta_op(&mut &buf[..]).unwrap();

        assert_eq!(decoded, op);
    }

    #[test]
    fn delta_stream_roundtrip_mixed_ops() {
        let ops = vec![
            DeltaOp::Literal(vec![0x01, 0x02, 0x03]),
            DeltaOp::Copy {
                block_index: 0,
                length: 1024,
            },
            DeltaOp::Literal(vec![0x04, 0x05]),
            DeltaOp::Copy {
                block_index: 5,
                length: 2048,
            },
            DeltaOp::Literal(vec![0x06]),
        ];

        let mut buf = Vec::new();
        write_delta(&mut buf, &ops).unwrap();

        let decoded = read_delta(&mut &buf[..]).unwrap();

        assert_eq!(decoded.len(), ops.len());
        for (i, (decoded_op, expected_op)) in decoded.iter().zip(ops.iter()).enumerate() {
            assert_eq!(decoded_op, expected_op, "mismatch at op {}", i);
        }
    }

    #[test]
    fn delta_stream_empty() {
        let ops: Vec<DeltaOp> = vec![];

        let mut buf = Vec::new();
        write_delta(&mut buf, &ops).unwrap();

        let decoded = read_delta(&mut &buf[..]).unwrap();

        assert_eq!(decoded.len(), 0);
    }

    #[test]
    fn delta_op_rejects_invalid_opcode() {
        let buf = vec![0xFF, 0x00, 0x00, 0x00];
        let result = read_delta_op(&mut &buf[..]);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid delta opcode")
        );
    }

    #[test]
    fn delta_stream_single_large_literal() {
        let data = vec![0x42; 65536];
        let ops = vec![DeltaOp::Literal(data.clone())];

        let mut buf = Vec::new();
        write_delta(&mut buf, &ops).unwrap();

        let decoded = read_delta(&mut &buf[..]).unwrap();

        assert_eq!(decoded.len(), 1);
        if let DeltaOp::Literal(decoded_data) = &decoded[0] {
            assert_eq!(decoded_data.len(), 65536);
            assert_eq!(decoded_data, &data);
        } else {
            panic!("expected Literal operation");
        }
    }
}
