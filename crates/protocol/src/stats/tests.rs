use std::io::Cursor;

use crate::stats::{DeleteStats, TransferStats};
use crate::version::ProtocolVersion;

#[test]
fn test_transfer_stats_roundtrip_proto30() {
    let stats = TransferStats {
        total_read: 1024,
        total_written: 2048,
        total_size: 10000,
        flist_buildtime: 500000,
        flist_xfertime: 100000,
        ..Default::default()
    };

    let protocol = ProtocolVersion::V30;
    let mut buf = Vec::new();
    stats.write_to(&mut buf, protocol).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

    assert_eq!(decoded.total_read, stats.total_read);
    assert_eq!(decoded.total_written, stats.total_written);
    assert_eq!(decoded.total_size, stats.total_size);
    assert_eq!(decoded.flist_buildtime, stats.flist_buildtime);
    assert_eq!(decoded.flist_xfertime, stats.flist_xfertime);
}

#[test]
fn test_transfer_stats_roundtrip_proto28() {
    let stats = TransferStats {
        total_read: 5000,
        total_written: 3000,
        total_size: 50000,
        ..Default::default()
    };

    let protocol = ProtocolVersion::V28;
    let mut buf = Vec::new();
    stats.write_to(&mut buf, protocol).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

    assert_eq!(decoded.total_read, stats.total_read);
    assert_eq!(decoded.total_written, stats.total_written);
    assert_eq!(decoded.total_size, stats.total_size);
    assert_eq!(decoded.flist_buildtime, 0);
    assert_eq!(decoded.flist_xfertime, 0);
}

/// A real protocol-29 peer (rsync 2.6.9, or upstream `--protocol=29`) reads the
/// end-of-run sender stats with `read_longint()` (io.h:29 `read_varlong30` ->
/// `read_longint` for protocol < 30), i.e. 4 bytes per small value. Emitting the
/// protocol >= 30 varlong form (3 bytes) leaves the peer's `handle_stats()` read
/// short, so it never relays its final MSG_DONE and the goodbye exchange
/// deadlocks. This pins the wire bytes to the legacy `write_longint` encoding so
/// that regression cannot recur.
#[test]
fn test_transfer_stats_proto29_uses_legacy_longint_encoding() {
    let stats = TransferStats {
        total_read: 1024,
        total_written: 2048,
        total_size: 10000,
        flist_buildtime: 500000,
        flist_xfertime: 100000,
        ..Default::default()
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf, ProtocolVersion::V29).unwrap();

    // 5 fields x write_longint (4 bytes each for values that fit in i32).
    let expected: Vec<u8> = [1024i32, 2048, 10000, 500000, 100000]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert_eq!(
        buf, expected,
        "proto-29 stats must be legacy 4-byte write_longint values"
    );
    assert_eq!(buf.len(), 20, "proto-29 stats: 5 fields x 4 bytes");
}

/// Protocol 28 omits the two flist-time fields (added at 29), so a proto-28 peer
/// reads exactly 3 legacy `write_longint` values.
#[test]
fn test_transfer_stats_proto28_legacy_three_fields() {
    let stats = TransferStats {
        total_read: 5000,
        total_written: 3000,
        total_size: 50000,
        flist_buildtime: 12345,
        flist_xfertime: 67890,
        ..Default::default()
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf, ProtocolVersion::V28).unwrap();

    let expected: Vec<u8> = [5000i32, 3000, 50000]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert_eq!(buf, expected, "proto-28 omits flist times; 3 legacy fields");
    assert_eq!(buf.len(), 12);
}

/// Protocol 30+ must remain byte-unchanged: the fix only redirects protocol < 30
/// to the legacy encoding. Values that fit in `min_bytes=3` varlong stay 3 bytes.
#[test]
fn test_transfer_stats_proto30_varlong_bytes_unchanged() {
    let stats = TransferStats {
        total_read: 1024,
        total_written: 2048,
        total_size: 10000,
        flist_buildtime: 500000,
        flist_xfertime: 100000,
        ..Default::default()
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf, ProtocolVersion::V30).unwrap();

    // 5 fields x varlong(min_bytes=3) = 15 bytes for these values.
    assert_eq!(buf.len(), 15, "proto-30 stays on the varlong encoding");
    assert_ne!(buf.len(), 20, "proto-30 must not use the legacy encoding");
}

/// Large values (> i32) exercise the `write_longint` 12-byte marker form on the
/// legacy path and must round-trip against `read_from` at protocol 29.
#[test]
fn test_transfer_stats_proto29_large_value_roundtrip() {
    let stats = TransferStats {
        total_read: 5_000_000_000,
        total_written: 3000,
        total_size: 50000,
        flist_buildtime: 12345,
        flist_xfertime: 67890,
        ..Default::default()
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf, ProtocolVersion::V29).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = TransferStats::read_from(&mut cursor, ProtocolVersion::V29).unwrap();
    assert_eq!(decoded.total_read, 5_000_000_000);
    assert_eq!(decoded.total_written, 3000);
    assert_eq!(decoded.total_size, 50000);
    assert_eq!(decoded.flist_buildtime, 12345);
    assert_eq!(decoded.flist_xfertime, 67890);
}

#[test]
fn test_transfer_stats_swap_perspective() {
    let stats = TransferStats {
        total_read: 100,
        total_written: 200,
        total_size: 1000,
        flist_buildtime: 50,
        flist_xfertime: 25,
        entries_received: 10,
        directories_created: 5,
        directories_failed: 2,
        files_skipped: 3,
        symlinks_created: 1,
        specials_created: 0,
        num_files: 5,
        num_reg_files: 3,
        ..Default::default()
    };

    let swapped = stats.swap_perspective();

    assert_eq!(swapped.total_read, 200);
    assert_eq!(swapped.total_written, 100);
    assert_eq!(swapped.total_size, 1000);
    assert_eq!(swapped.flist_buildtime, 50);
    assert_eq!(swapped.flist_xfertime, 25);
    assert_eq!(swapped.entries_received, 10);
    assert_eq!(swapped.directories_created, 5);
    assert_eq!(swapped.directories_failed, 2);
    assert_eq!(swapped.files_skipped, 3);
    assert_eq!(swapped.symlinks_created, 1);
    assert_eq!(swapped.specials_created, 0);
    assert_eq!(swapped.num_files, 5);
    assert_eq!(swapped.num_reg_files, 3);
}

#[test]
fn test_transfer_stats_with_builders() {
    let stats = TransferStats::with_bytes(100, 200, 1000).with_flist_times(50000, 25000);

    assert_eq!(stats.total_read, 100);
    assert_eq!(stats.total_written, 200);
    assert_eq!(stats.total_size, 1000);
    assert_eq!(stats.flist_buildtime, 50000);
    assert_eq!(stats.flist_xfertime, 25000);
}

#[test]
fn test_transfer_stats_with_incremental_stats() {
    let stats = TransferStats::new().with_incremental_stats(100, 10, 2, 5, 3, 1);

    assert_eq!(stats.entries_received, 100);
    assert_eq!(stats.directories_created, 10);
    assert_eq!(stats.directories_failed, 2);
    assert_eq!(stats.files_skipped, 5);
    assert_eq!(stats.symlinks_created, 3);
    assert_eq!(stats.specials_created, 1);
}

#[test]
fn test_transfer_stats_display_basic() {
    let stats = TransferStats {
        total_read: 456,
        total_written: 789,
        total_size: 12345,
        num_files: 5,
        num_reg_files: 3,
        num_dirs: 2,
        num_created_files: 2,
        num_deleted_files: 0,
        num_transferred_files: 1,
        total_transferred_size: 4567,
        literal_data: 4567,
        matched_data: 0,
        flist_size: 123,
        flist_buildtime: 1000,
        flist_xfertime: 0,
        ..Default::default()
    };

    let output = format!("{stats}");

    assert!(output.contains("Number of files: 5 (reg: 3, dir: 2)"));
    assert!(output.contains("Number of created files: 2"));
    assert!(output.contains("Number of regular files transferred: 1"));
    assert!(output.contains("Total file size: 12,345 bytes"));
    assert!(output.contains("Total transferred file size: 4,567 bytes"));
    assert!(output.contains("Literal data: 4,567 bytes"));
    assert!(output.contains("Matched data: 0 bytes"));
    assert!(output.contains("File list size: 123"));
    assert!(output.contains("File list generation time: 0.001 seconds"));
    assert!(output.contains("Total bytes sent: 789"));
    assert!(output.contains("Total bytes received: 456"));
    assert!(output.contains("sent 789 bytes  received 456 bytes"));
    assert!(output.contains("total size is 12,345  speedup is"));
}

#[test]
fn test_transfer_stats_display_large_numbers() {
    let stats = TransferStats {
        total_read: 1_234_567,
        total_written: 7_654_321,
        total_size: 123_456_789,
        num_files: 1000,
        num_reg_files: 950,
        num_dirs: 50,
        flist_buildtime: 500000,
        flist_xfertime: 100000,
        ..Default::default()
    };

    let output = format!("{stats}");

    assert!(output.contains("Number of files: 1,000 (reg: 950, dir: 50)"));
    assert!(output.contains("Total file size: 123,456,789 bytes"));
    assert!(output.contains("Total bytes sent: 7,654,321"));
    assert!(output.contains("Total bytes received: 1,234,567"));
    assert!(output.contains("sent 7,654,321 bytes  received 1,234,567 bytes"));
    assert!(output.contains("total size is 123,456,789"));
}

#[test]
fn test_transfer_stats_display_with_all_file_types() {
    let stats = TransferStats {
        total_read: 1000,
        total_written: 2000,
        total_size: 50000,
        num_files: 25,
        num_reg_files: 10,
        num_dirs: 5,
        num_symlinks: 7,
        num_devices: 2,
        num_specials: 1,
        flist_buildtime: 100000,
        flist_xfertime: 50000,
        ..Default::default()
    };

    let output = format!("{stats}");

    assert!(output.contains("Number of files: 25 (reg: 10, dir: 5, link: 7, dev: 2, special: 1)"));
}

#[test]
fn test_transfer_stats_display_speedup_calculation() {
    let stats = TransferStats {
        total_read: 500,
        total_written: 500,
        total_size: 10000,
        flist_buildtime: 1000000,
        flist_xfertime: 1000000,
        ..Default::default()
    };

    let output = format!("{stats}");

    assert!(output.contains("speedup is 10.00"));
    assert!(output.contains("500.00 bytes/sec"));
}

#[test]
fn test_transfer_stats_display_minimal() {
    let stats = TransferStats {
        total_read: 100,
        total_written: 200,
        total_size: 0,
        ..Default::default()
    };

    let output = format!("{stats}");

    assert!(output.contains("Total bytes sent: 200"));
    assert!(output.contains("Total bytes received: 100"));
    assert!(output.contains("sent 200 bytes  received 100 bytes"));
    assert!(output.contains("total size is 0"));
}

#[test]
fn test_transfer_stats_format_number() {
    assert_eq!(TransferStats::format_number(0), "0");
    assert_eq!(TransferStats::format_number(999), "999");
    assert_eq!(TransferStats::format_number(1000), "1,000");
    assert_eq!(TransferStats::format_number(1234), "1,234");
    assert_eq!(TransferStats::format_number(12345), "12,345");
    assert_eq!(TransferStats::format_number(123456), "123,456");
    assert_eq!(TransferStats::format_number(1234567), "1,234,567");
    assert_eq!(TransferStats::format_number(1234567890), "1,234,567,890");
}

#[test]
fn test_transfer_stats_bytes_per_sec_zero_time() {
    let stats = TransferStats {
        total_read: 1000,
        total_written: 2000,
        flist_buildtime: 0,
        flist_xfertime: 0,
        ..Default::default()
    };

    let output = format!("{stats}");
    assert!(output.contains("0.00 bytes/sec"));
}

#[test]
fn test_transfer_stats_speedup_zero_bytes() {
    let stats = TransferStats {
        total_read: 0,
        total_written: 0,
        total_size: 1000,
        ..Default::default()
    };

    let output = format!("{stats}");
    assert!(output.contains("speedup is 0.00"));
}

#[test]
fn test_transfer_stats_display_matches_upstream_format() {
    let stats = TransferStats {
        total_read: 456,
        total_written: 789,
        total_size: 12345,
        num_files: 5,
        num_reg_files: 3,
        num_dirs: 2,
        num_created_files: 2,
        num_transferred_files: 1,
        total_transferred_size: 4567,
        literal_data: 4567,
        matched_data: 0,
        flist_size: 123,
        flist_buildtime: 1000,
        flist_xfertime: 0,
        ..Default::default()
    };

    let output = format!("{stats}");
    let lines: Vec<&str> = output.lines().collect();

    assert_eq!(lines[0], "Number of files: 5 (reg: 3, dir: 2)");
    assert_eq!(lines[1], "Number of created files: 2");
    assert_eq!(lines[2], "Number of regular files transferred: 1");
    assert_eq!(lines[3], "Total file size: 12,345 bytes");
    assert_eq!(lines[4], "Total transferred file size: 4,567 bytes");
    assert_eq!(lines[5], "Literal data: 4,567 bytes");
    assert_eq!(lines[6], "Matched data: 0 bytes");
    assert_eq!(lines[7], "File list size: 123");
    assert_eq!(lines[8], "File list generation time: 0.001 seconds");
    assert_eq!(lines[9], "Total bytes sent: 789");
    assert_eq!(lines[10], "Total bytes received: 456");
    assert!(lines[11].starts_with("sent 789 bytes  received 456 bytes"));
    assert!(lines[12].starts_with("total size is 12,345  speedup is"));
}

#[test]
fn test_transfer_stats_large_values() {
    let stats = TransferStats {
        total_read: 100_000_000_000_000,
        total_written: 50_000_000_000_000,
        total_size: 200_000_000_000_000,
        flist_buildtime: 1_000_000_000,
        flist_xfertime: 500_000_000,
        ..Default::default()
    };

    let protocol = ProtocolVersion::V32;
    let mut buf = Vec::new();
    stats.write_to(&mut buf, protocol).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

    assert_eq!(decoded.total_read, stats.total_read);
    assert_eq!(decoded.total_written, stats.total_written);
    assert_eq!(decoded.total_size, stats.total_size);
    assert_eq!(decoded.flist_buildtime, stats.flist_buildtime);
    assert_eq!(decoded.flist_xfertime, stats.flist_xfertime);
}

#[test]
fn test_delete_stats_roundtrip() {
    let stats = DeleteStats {
        files: 10,
        dirs: 3,
        symlinks: 2,
        devices: 1,
        specials: 0,
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = DeleteStats::read_from(&mut cursor).unwrap();

    assert_eq!(stats, decoded);
}

#[test]
fn test_delete_stats_total() {
    let stats = DeleteStats {
        files: 10,
        dirs: 5,
        symlinks: 3,
        devices: 2,
        specials: 1,
    };

    assert_eq!(stats.total(), 21);
}

#[test]
fn test_delete_stats_empty() {
    let stats = DeleteStats::new();

    assert_eq!(stats.total(), 0);

    let mut buf = Vec::new();
    stats.write_to(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = DeleteStats::read_from(&mut cursor).unwrap();

    assert_eq!(stats, decoded);
}

#[test]
fn delete_stats_wire_roundtrip_zeros() {
    let stats = DeleteStats::new();
    let mut buf = Vec::new();
    stats.write_to(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = DeleteStats::read_from(&mut cursor).unwrap();
    assert_eq!(stats, decoded);
}

#[test]
fn delete_stats_wire_roundtrip_realistic() {
    let stats = DeleteStats {
        files: 42,
        dirs: 5,
        symlinks: 3,
        devices: 1,
        specials: 2,
    };
    let mut buf = Vec::new();
    stats.write_to(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = DeleteStats::read_from(&mut cursor).unwrap();
    assert_eq!(stats, decoded);
}

#[test]
fn delete_stats_wire_roundtrip_large_values() {
    let stats = DeleteStats {
        files: 100_000,
        dirs: 50_000,
        symlinks: 10_000,
        devices: 500,
        specials: 200,
    };
    let mut buf = Vec::new();
    stats.write_to(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = DeleteStats::read_from(&mut cursor).unwrap();
    assert_eq!(stats, decoded);
}

#[test]
fn delete_stats_total() {
    let stats = DeleteStats {
        files: 10,
        dirs: 5,
        symlinks: 3,
        devices: 2,
        specials: 1,
    };
    assert_eq!(stats.total(), 21);

    let empty = DeleteStats::new();
    assert_eq!(empty.total(), 0);
}

/// upstream: io.c - MAX_WIRE_DEL_STAT defence-in-depth (3.4.3)
#[test]
fn delete_stats_rejects_oversized_wire_value() {
    use crate::varint::write_varint;

    // Encode a value just above MAX_WIRE_DEL_STAT (0x3FFF_FFFF = 1_073_741_823)
    let oversized: i32 = 0x3FFF_FFFF + 1;
    let mut buf = Vec::new();
    write_varint(&mut buf, oversized).unwrap();
    // Pad with four more zero varints so read_from can attempt all 5 fields.
    for _ in 0..4 {
        write_varint(&mut buf, 0).unwrap();
    }

    let mut cursor = Cursor::new(&buf);
    let err = DeleteStats::read_from(&mut cursor).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.to_string().contains("MAX_WIRE_DEL_STAT"),
        "error should mention MAX_WIRE_DEL_STAT, got: {err}"
    );
    // WHY: upstream main.c reads these via read_varint_bounded, exiting
    // RERR_PROTOCOL (2) on an out-of-range value; the tag must survive so the
    // core exit-code mapper reproduces exit 2, not RERR_STREAMIO (12).
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "oversized delete-stat must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

/// upstream: io.c - MAX_WIRE_DEL_STAT defence-in-depth (3.4.3)
#[test]
fn delete_stats_rejects_negative_wire_value() {
    use crate::varint::write_varint;

    let mut buf = Vec::new();
    write_varint(&mut buf, -1).unwrap();
    for _ in 0..4 {
        write_varint(&mut buf, 0).unwrap();
    }

    let mut cursor = Cursor::new(&buf);
    let err = DeleteStats::read_from(&mut cursor).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.to_string().contains("MAX_WIRE_DEL_STAT"),
        "negative value should be rejected, got: {err}"
    );
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
        "negative delete-stat must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
    );
}

/// Values at exactly MAX_WIRE_DEL_STAT must be accepted.
#[test]
fn delete_stats_accepts_value_at_cap() {
    use crate::varint::write_varint;

    let at_cap: i32 = 0x3FFF_FFFF;
    let mut buf = Vec::new();
    write_varint(&mut buf, at_cap).unwrap();
    for _ in 0..4 {
        write_varint(&mut buf, 0).unwrap();
    }

    let mut cursor = Cursor::new(&buf);
    let decoded = DeleteStats::read_from(&mut cursor).unwrap();
    assert_eq!(decoded.files, at_cap as u32);
}

/// Oversized value in a non-first field is also rejected.
#[test]
fn delete_stats_rejects_oversized_in_middle_field() {
    use crate::varint::write_varint;

    let oversized: i32 = 0x3FFF_FFFF + 1;
    let mut buf = Vec::new();
    // files=0, dirs=oversized
    write_varint(&mut buf, 0).unwrap();
    write_varint(&mut buf, oversized).unwrap();
    for _ in 0..3 {
        write_varint(&mut buf, 0).unwrap();
    }

    let mut cursor = Cursor::new(&buf);
    let err = DeleteStats::read_from(&mut cursor).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.to_string().contains("dirs"),
        "error should name the 'dirs' field, got: {err}"
    );
}

#[cfg(feature = "serde")]
mod serde_tests {
    use crate::stats::{DeleteStats, TransferStats};

    #[test]
    fn test_transfer_stats_serde_roundtrip() {
        let stats = TransferStats {
            total_read: 1024,
            total_written: 2048,
            total_size: 10000,
            flist_buildtime: 500000,
            flist_xfertime: 100000,
            entries_received: 50,
            directories_created: 10,
            directories_failed: 2,
            files_skipped: 5,
            symlinks_created: 3,
            specials_created: 1,
            num_files: 100,
            num_reg_files: 80,
            num_dirs: 15,
            num_symlinks: 5,
            num_devices: 0,
            num_specials: 0,
            num_created_files: 25,
            num_deleted_files: 5,
            num_transferred_files: 20,
            total_transferred_size: 8000,
            literal_data: 6000,
            matched_data: 2000,
            flist_size: 500,
        };

        let json = serde_json::to_string(&stats).unwrap();
        let decoded: TransferStats = serde_json::from_str(&json).unwrap();
        assert_eq!(stats, decoded);
    }

    #[test]
    fn test_delete_stats_serde_roundtrip() {
        let stats = DeleteStats {
            files: 10,
            dirs: 3,
            symlinks: 2,
            devices: 1,
            specials: 0,
        };

        let json = serde_json::to_string(&stats).unwrap();
        let decoded: DeleteStats = serde_json::from_str(&json).unwrap();
        assert_eq!(stats, decoded);
    }
}
