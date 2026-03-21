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
