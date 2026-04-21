#![no_main]

//! Fuzz target for varint encode/decode roundtrip verification.
//!
//! Tests that encoding a value and decoding the result produces the
//! original value, for all varint/varlong/longint formats. Uses
//! structured input via `arbitrary` to generate values across the
//! full range of each encoding.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

/// Structured input for varint roundtrip testing.
#[derive(Arbitrary, Debug)]
struct VarintInput {
    /// Value for varint (i32) roundtrip.
    varint_value: i32,
    /// Value for varlong (i64) roundtrip.
    varlong_value: i64,
    /// Value for longint (i64) roundtrip.
    longint_value: i64,
    /// Min bytes selector for varlong (mapped to 1-8).
    min_bytes_selector: u8,
    /// Protocol version selector for varint30 (mapped to 28-32).
    proto_selector: u8,
    /// Raw bytes for unstructured decode testing.
    raw_bytes: Vec<u8>,
    /// Stats roundtrip fields.
    total_read: u64,
    total_written: u64,
    total_size: u64,
    flist_buildtime: u64,
    flist_xfertime: u64,
    /// Delete stats fields.
    del_files: u32,
    del_dirs: u32,
    del_symlinks: u32,
    del_devices: u32,
    del_specials: u32,
}

impl VarintInput {
    /// Maps min_bytes_selector to a valid min_bytes value (1-8).
    fn min_bytes(&self) -> u8 {
        1 + (self.min_bytes_selector % 8)
    }

    /// Maps proto_selector to a valid protocol version (28-32).
    fn protocol_version(&self) -> u8 {
        28 + (self.proto_selector % 5)
    }
}

fuzz_target!(|input: VarintInput| {
    // Roundtrip: write_varint / read_varint
    {
        let mut buf = Vec::new();
        if protocol::write_varint(&mut buf, input.varint_value).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) = protocol::read_varint(&mut cursor) {
                assert_eq!(
                    decoded, input.varint_value,
                    "varint roundtrip mismatch for {}",
                    input.varint_value
                );
            }
        }
    }

    // Roundtrip: encode_varint_to_vec / decode_varint
    {
        let mut buf = Vec::new();
        protocol::encode_varint_to_vec(input.varint_value, &mut buf);
        if let Ok((decoded, remainder)) = protocol::decode_varint(&buf) {
            assert!(remainder.is_empty(), "trailing bytes after decode_varint");
            assert_eq!(
                decoded, input.varint_value,
                "encode/decode_varint roundtrip mismatch"
            );
        }
    }

    // Roundtrip: write_varlong / read_varlong
    {
        let min_bytes = input.min_bytes();
        let mut buf = Vec::new();
        if protocol::write_varlong(&mut buf, input.varlong_value, min_bytes).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) = protocol::read_varlong(&mut cursor, min_bytes) {
                assert_eq!(
                    decoded, input.varlong_value,
                    "varlong roundtrip mismatch for {} (min_bytes={})",
                    input.varlong_value, min_bytes
                );
            }
        }
    }

    // Roundtrip: write_varlong30 / read_varlong30
    {
        let min_bytes = input.min_bytes();
        let mut buf = Vec::new();
        if protocol::write_varlong30(&mut buf, input.varlong_value, min_bytes).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) = protocol::read_varlong30(&mut cursor, min_bytes) {
                assert_eq!(
                    decoded, input.varlong_value,
                    "varlong30 roundtrip mismatch for {} (min_bytes={})",
                    input.varlong_value, min_bytes
                );
            }
        }
    }

    // Roundtrip: write_longint / read_longint
    {
        let mut buf = Vec::new();
        if protocol::write_longint(&mut buf, input.longint_value).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) = protocol::read_longint(&mut cursor) {
                assert_eq!(
                    decoded, input.longint_value,
                    "longint roundtrip mismatch for {}",
                    input.longint_value
                );
            }
        }
    }

    // Roundtrip: write_int / read_int
    {
        let mut buf = Vec::new();
        if protocol::write_int(&mut buf, input.varint_value).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) = protocol::read_int(&mut cursor) {
                assert_eq!(
                    decoded, input.varint_value,
                    "int roundtrip mismatch for {}",
                    input.varint_value
                );
            }
        }
    }

    // Roundtrip: write_varint30_int / read_varint30_int
    {
        let proto = input.protocol_version();
        let mut buf = Vec::new();
        if protocol::write_varint30_int(&mut buf, input.varint_value, proto).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) = protocol::read_varint30_int(&mut cursor, proto) {
                assert_eq!(
                    decoded, input.varint_value,
                    "varint30_int roundtrip mismatch for {} (proto={})",
                    input.varint_value, proto
                );
            }
        }
    }

    // Roundtrip: TransferStats write_to / read_from
    {
        // Clamp values to non-negative i64 range for wire format compatibility
        let stats = protocol::TransferStats::with_bytes(
            input.total_read & 0x7FFF_FFFF_FFFF_FFFF,
            input.total_written & 0x7FFF_FFFF_FFFF_FFFF,
            input.total_size & 0x7FFF_FFFF_FFFF_FFFF,
        )
        .with_flist_times(
            input.flist_buildtime & 0x7FFF_FFFF_FFFF_FFFF,
            input.flist_xfertime & 0x7FFF_FFFF_FFFF_FFFF,
        );

        // Test with protocol versions that support flist times (>= 29) and those that do not
        for proto_num in [28u8, 29, 30, 31, 32] {
            if let Ok(proto) = protocol::ProtocolVersion::try_from(proto_num) {
                let mut buf = Vec::new();
                if stats.write_to(&mut buf, proto).is_ok() {
                    let mut cursor = Cursor::new(&buf);
                    if let Ok(decoded) = protocol::TransferStats::read_from(&mut cursor, proto) {
                        assert_eq!(
                            decoded.total_read, stats.total_read,
                            "stats total_read mismatch (proto={})",
                            proto_num
                        );
                        assert_eq!(
                            decoded.total_written, stats.total_written,
                            "stats total_written mismatch (proto={})",
                            proto_num
                        );
                        assert_eq!(
                            decoded.total_size, stats.total_size,
                            "stats total_size mismatch (proto={})",
                            proto_num
                        );
                        if proto.supports_flist_times() {
                            assert_eq!(
                                decoded.flist_buildtime, stats.flist_buildtime,
                                "stats flist_buildtime mismatch (proto={})",
                                proto_num
                            );
                            assert_eq!(
                                decoded.flist_xfertime, stats.flist_xfertime,
                                "stats flist_xfertime mismatch (proto={})",
                                proto_num
                            );
                        }
                    }
                }
            }
        }
    }

    // Roundtrip: DeleteStats write_to / read_from
    {
        let del_stats = protocol::DeleteStats {
            files: input.del_files,
            dirs: input.del_dirs,
            symlinks: input.del_symlinks,
            devices: input.del_devices,
            specials: input.del_specials,
        };

        let mut buf = Vec::new();
        if del_stats.write_to(&mut buf).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) = protocol::DeleteStats::read_from(&mut cursor) {
                assert_eq!(decoded, del_stats, "delete stats roundtrip mismatch");
            }
        }
    }

    // Unstructured: parse arbitrary bytes through all decode functions
    {
        let mut cursor = Cursor::new(&input.raw_bytes);
        let _ = protocol::read_varint(&mut cursor);
    }
    for min_bytes in [1u8, 2, 3, 4, 5, 6, 7, 8] {
        let mut cursor = Cursor::new(&input.raw_bytes);
        let _ = protocol::read_varlong(&mut cursor, min_bytes);
    }
    {
        let mut cursor = Cursor::new(&input.raw_bytes);
        let _ = protocol::read_longint(&mut cursor);
    }
    {
        let _ = protocol::decode_varint(&input.raw_bytes);
    }
    {
        let mut cursor = Cursor::new(&input.raw_bytes);
        let _ = protocol::DeleteStats::read_from(&mut cursor);
    }
    for proto_num in [28u8, 29, 30, 31, 32] {
        if let Ok(proto) = protocol::ProtocolVersion::try_from(proto_num) {
            let mut cursor = Cursor::new(&input.raw_bytes);
            let _ = protocol::TransferStats::read_from(&mut cursor, proto);
        }
    }
});
