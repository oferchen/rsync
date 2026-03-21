use super::*;
use std::io::Cursor;

#[test]
fn test_write_read_i32() {
    let values = [0, 1, -1, i32::MAX, i32::MIN, 12345, -67890];
    for &val in &values {
        let mut buf = Vec::new();
        wire::write_i32(&mut buf, val).unwrap();
        let mut cursor = Cursor::new(buf);
        let read_val = wire::read_i32(&mut cursor).unwrap();
        assert_eq!(val, read_val);
    }
}

#[test]
fn test_write_read_varint() {
    let values: &[i32] = &[0, 1, 127, 128, 255, 256, 16383, 16384, i32::MAX];
    for &val in values {
        let mut buf = Vec::new();
        wire::write_varint(&mut buf, val).unwrap();
        let mut cursor = Cursor::new(buf);
        let read_val = wire::read_varint(&mut cursor).unwrap();
        assert_eq!(val, read_val);
    }
}

#[test]
fn test_batch_flags_bitmap_roundtrip() {
    let flags = BatchFlags {
        recurse: true,
        preserve_uid: true,
        preserve_links: true,
        preserve_hard_links: true,
        always_checksum: true,
        ..Default::default()
    };

    let bitmap = flags.to_bitmap(30);
    let restored = BatchFlags::from_bitmap(bitmap, 30);
    assert_eq!(flags, restored);
}

#[test]
fn test_batch_flags_protocol_29() {
    let flags = BatchFlags {
        xfer_dirs: true,
        do_compression: true,
        ..Default::default()
    };

    let bitmap = flags.to_bitmap(29);
    let restored = BatchFlags::from_bitmap(bitmap, 29);
    assert_eq!(flags, restored);

    // Protocol 28 should not include these flags
    let bitmap_28 = flags.to_bitmap(28);
    let restored_28 = BatchFlags::from_bitmap(bitmap_28, 28);
    assert!(!restored_28.xfer_dirs);
    assert!(!restored_28.do_compression);
}

#[test]
fn test_batch_header_write_read() {
    let mut header = BatchHeader::new(30, 12345);
    header.compat_flags = Some(42);
    header.stream_flags.recurse = true;
    header.stream_flags.preserve_uid = true;

    let mut buf = Vec::new();
    header.write_to(&mut buf).unwrap();

    let mut cursor = Cursor::new(buf);
    let restored = BatchHeader::read_from(&mut cursor).unwrap();

    assert_eq!(header.protocol_version, restored.protocol_version);
    assert_eq!(header.compat_flags, restored.compat_flags);
    assert_eq!(header.checksum_seed, restored.checksum_seed);
    assert_eq!(header.stream_flags.recurse, restored.stream_flags.recurse);
    assert_eq!(
        header.stream_flags.preserve_uid,
        restored.stream_flags.preserve_uid
    );
}

#[test]
fn test_batch_header_protocol_28() {
    let header = BatchHeader::new(28, 99999);
    assert!(header.compat_flags.is_none());

    let mut buf = Vec::new();
    header.write_to(&mut buf).unwrap();

    let mut cursor = Cursor::new(buf);
    let restored = BatchHeader::read_from(&mut cursor).unwrap();

    assert_eq!(28, restored.protocol_version);
    assert!(restored.compat_flags.is_none());
    assert_eq!(99999, restored.checksum_seed);
}

#[test]
fn test_batch_flags_default() {
    let flags = BatchFlags::default();
    assert!(!flags.recurse);
    assert!(!flags.preserve_uid);
    assert!(!flags.preserve_gid);
    assert!(!flags.preserve_links);
    assert!(!flags.preserve_devices);
    assert!(!flags.preserve_hard_links);
    assert!(!flags.always_checksum);
}

#[test]
fn test_batch_flags_protocol_30_features() {
    let flags = BatchFlags {
        iconv: true,
        preserve_acls: true,
        preserve_xattrs: true,
        inplace: true,
        append: true,
        append_verify: true,
        ..Default::default()
    };

    let bitmap = flags.to_bitmap(30);
    let restored = BatchFlags::from_bitmap(bitmap, 30);

    assert_eq!(flags.iconv, restored.iconv);
    assert_eq!(flags.preserve_acls, restored.preserve_acls);
    assert_eq!(flags.preserve_xattrs, restored.preserve_xattrs);
    assert_eq!(flags.inplace, restored.inplace);
    assert_eq!(flags.append, restored.append);
    assert_eq!(flags.append_verify, restored.append_verify);
}

#[test]
fn test_batch_flags_protocol_30_not_in_28() {
    let flags = BatchFlags {
        iconv: true,
        preserve_acls: true,
        preserve_xattrs: true,
        ..Default::default()
    };

    let bitmap = flags.to_bitmap(28);
    let restored = BatchFlags::from_bitmap(bitmap, 28);

    assert!(!restored.iconv);
    assert!(!restored.preserve_acls);
    assert!(!restored.preserve_xattrs);
}

#[test]
fn test_batch_flags_write_read_roundtrip() {
    let flags = BatchFlags {
        recurse: true,
        preserve_uid: false,
        preserve_gid: true,
        preserve_links: false,
        preserve_devices: true,
        preserve_hard_links: false,
        always_checksum: true,
        xfer_dirs: false,
        do_compression: true,
        ..Default::default()
    };

    let mut buf = Vec::new();
    flags.write_to_versioned(&mut buf, 30).unwrap();

    let mut cursor = Cursor::new(&buf);
    let raw = BatchFlags::read_raw(&mut cursor).unwrap();
    let restored = BatchFlags::from_bitmap(raw, 30);

    assert_eq!(flags.to_bitmap(30), restored.to_bitmap(30));
}

#[test]
fn test_batch_header_new_creates_compat_flags_for_protocol_30() {
    let header = BatchHeader::new(30, 42);
    assert_eq!(header.compat_flags, Some(0));
}

#[test]
fn test_batch_header_new_no_compat_flags_for_protocol_29() {
    let header = BatchHeader::new(29, 42);
    assert!(header.compat_flags.is_none());
}

#[test]
fn test_batch_flags_clone() {
    let flags = BatchFlags {
        recurse: true,
        preserve_uid: true,
        ..Default::default()
    };
    let cloned = flags;
    assert_eq!(flags, cloned);
}

#[test]
fn test_batch_header_clone() {
    let header = BatchHeader::new(30, 12345);
    let cloned = header.clone();
    assert_eq!(header.protocol_version, cloned.protocol_version);
    assert_eq!(header.checksum_seed, cloned.checksum_seed);
}

#[test]
fn test_varint_edge_cases() {
    let values: &[i32] = &[0, 127, 128, 16383, 16384, 2097151, 2097152];
    for &val in values {
        let mut buf = Vec::new();
        wire::write_varint(&mut buf, val).unwrap();
        let mut cursor = Cursor::new(buf);
        let read_val = wire::read_varint(&mut cursor).unwrap();
        assert_eq!(val, read_val, "Failed for value {val}");
    }
}

#[test]
fn test_batch_flags_all_set() {
    let flags = BatchFlags {
        recurse: true,
        preserve_uid: true,
        preserve_gid: true,
        preserve_links: true,
        preserve_devices: true,
        preserve_hard_links: true,
        always_checksum: true,
        xfer_dirs: true,
        do_compression: true,
        iconv: true,
        preserve_acls: true,
        preserve_xattrs: true,
        inplace: true,
        append: true,
        append_verify: true,
    };

    let bitmap = flags.to_bitmap(30);
    let restored = BatchFlags::from_bitmap(bitmap, 30);
    assert_eq!(flags, restored);
}

#[test]
fn test_batch_flags_debug_format() {
    let flags = BatchFlags::default();
    let debug = format!("{flags:?}");
    assert!(debug.contains("BatchFlags"));
}

#[test]
fn test_batch_header_debug_format() {
    let header = BatchHeader::new(30, 42);
    let debug = format!("{header:?}");
    assert!(debug.contains("BatchHeader"));
    assert!(debug.contains("30"));
}

#[test]
fn test_batch_header_protocol28_masks_high_bits() {
    let mut header = BatchHeader::new(28, 42);
    header.stream_flags.xfer_dirs = true; // bit 7 - protocol 29+
    header.stream_flags.preserve_acls = true; // bit 10 - protocol 30+

    let mut buf = Vec::new();
    header.write_to(&mut buf).unwrap();

    let mut cursor = Cursor::new(buf);
    let restored = BatchHeader::read_from(&mut cursor).unwrap();

    assert!(!restored.stream_flags.xfer_dirs);
    assert!(!restored.stream_flags.preserve_acls);
    assert_eq!(restored.protocol_version, 28);
}

#[test]
fn test_batch_header_protocol29_includes_bits_7_8() {
    let mut header = BatchHeader::new(29, 100);
    header.stream_flags.xfer_dirs = true; // bit 7
    header.stream_flags.do_compression = true; // bit 8
    header.stream_flags.preserve_acls = true; // bit 10 - protocol 30+

    let mut buf = Vec::new();
    header.write_to(&mut buf).unwrap();

    let mut cursor = Cursor::new(buf);
    let restored = BatchHeader::read_from(&mut cursor).unwrap();

    assert!(restored.stream_flags.xfer_dirs);
    assert!(restored.stream_flags.do_compression);
    assert!(!restored.stream_flags.preserve_acls); // masked out for proto 29
}

#[test]
fn test_batch_flags_write_versioned_roundtrip() {
    let flags = BatchFlags {
        recurse: true,
        preserve_uid: true,
        xfer_dirs: true,
        preserve_acls: true,
        ..Default::default()
    };

    // Write with protocol 30 - all bits preserved
    let mut buf30 = Vec::new();
    flags.write_to_versioned(&mut buf30, 30).unwrap();
    let raw30 = BatchFlags::read_raw(&mut Cursor::new(&buf30)).unwrap();
    let restored30 = BatchFlags::from_bitmap(raw30, 30);
    assert!(restored30.xfer_dirs);
    assert!(restored30.preserve_acls);

    // Write with protocol 28 - bits 7+ masked
    let mut buf28 = Vec::new();
    flags.write_to_versioned(&mut buf28, 28).unwrap();
    let raw28 = BatchFlags::read_raw(&mut Cursor::new(&buf28)).unwrap();
    let restored28 = BatchFlags::from_bitmap(raw28, 28);
    assert!(!restored28.xfer_dirs);
    assert!(!restored28.preserve_acls);
}

#[test]
fn test_batch_stats_roundtrip_protocol_30() {
    let stats = BatchStats {
        total_read: 1024,
        total_written: 2048,
        total_size: 10_000_000,
        flist_buildtime: Some(42),
        flist_xfertime: Some(100),
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf, 30).unwrap();

    let mut cursor = Cursor::new(buf);
    let restored = BatchStats::read_from(&mut cursor, 30).unwrap();

    assert_eq!(stats, restored);
}

#[test]
fn test_batch_stats_roundtrip_protocol_28() {
    let stats = BatchStats {
        total_read: 500,
        total_written: 1000,
        total_size: 5_000,
        flist_buildtime: None,
        flist_xfertime: None,
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf, 28).unwrap();

    let mut cursor = Cursor::new(buf);
    let restored = BatchStats::read_from(&mut cursor, 28).unwrap();

    assert_eq!(stats.total_read, restored.total_read);
    assert_eq!(stats.total_written, restored.total_written);
    assert_eq!(stats.total_size, restored.total_size);
    assert!(restored.flist_buildtime.is_none());
    assert!(restored.flist_xfertime.is_none());
}

#[test]
fn test_batch_stats_default() {
    let stats = BatchStats::default();
    assert_eq!(stats.total_read, 0);
    assert_eq!(stats.total_written, 0);
    assert_eq!(stats.total_size, 0);
    assert!(stats.flist_buildtime.is_none());
    assert!(stats.flist_xfertime.is_none());
}

#[test]
fn test_batch_stats_large_values() {
    let stats = BatchStats {
        total_read: 10_000_000_000_000,   // ~10 TB
        total_written: 5_000_000_000_000, // ~5 TB
        total_size: 50_000_000_000_000,   // ~50 TB
        flist_buildtime: Some(3_600_000), // 1 hour in ms
        flist_xfertime: Some(86_400_000), // 1 day in ms
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf, 31).unwrap();

    let mut cursor = Cursor::new(buf);
    let restored = BatchStats::read_from(&mut cursor, 31).unwrap();

    assert_eq!(stats, restored);
}

#[test]
fn test_batch_stats_zero_values() {
    let stats = BatchStats {
        total_read: 0,
        total_written: 0,
        total_size: 0,
        flist_buildtime: Some(0),
        flist_xfertime: Some(0),
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf, 30).unwrap();

    let mut cursor = Cursor::new(buf);
    let restored = BatchStats::read_from(&mut cursor, 30).unwrap();

    assert_eq!(stats, restored);
}
