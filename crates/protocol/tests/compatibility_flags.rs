//! Compatibility flags validation tests.
//!
//! These tests validate the wire-level encoding, negotiation semantics, and
//! bitfield operations for rsync compatibility flags (CF_*) as defined by
//! upstream rsync 3.4.1. The tests ensure our implementation produces
//! byte-identical varint-encoded payloads and handles all 9 known flags
//! correctly.

use protocol::{CompatibilityFlags, KnownCompatibilityFlag};
use std::io::Cursor;

// ============================================================================
// Individual Flag Encoding Tests
// ============================================================================

#[test]
#[ignore]
fn print_all_flag_encodings() {
    // Helper test to discover actual varint encodings
    let flags = [
        ("INC_RECURSE", CompatibilityFlags::INC_RECURSE),
        ("SYMLINK_TIMES", CompatibilityFlags::SYMLINK_TIMES),
        ("SYMLINK_ICONV", CompatibilityFlags::SYMLINK_ICONV),
        ("SAFE_FILE_LIST", CompatibilityFlags::SAFE_FILE_LIST),
        (
            "AVOID_XATTR_OPTIMIZATION",
            CompatibilityFlags::AVOID_XATTR_OPTIMIZATION,
        ),
        ("CHECKSUM_SEED_FIX", CompatibilityFlags::CHECKSUM_SEED_FIX),
        (
            "INPLACE_PARTIAL_DIR",
            CompatibilityFlags::INPLACE_PARTIAL_DIR,
        ),
        ("VARINT_FLIST_FLAGS", CompatibilityFlags::VARINT_FLIST_FLAGS),
        ("ID0_NAMES", CompatibilityFlags::ID0_NAMES),
    ];

    for (name, flag) in &flags {
        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).unwrap();
        eprintln!(
            "{:30} (bit {:3} = 0x{:x}): {:?}",
            name,
            flag.bits(),
            flag.bits(),
            buf
        );
    }
}

#[test]
fn test_inc_recurse_flag_encoding() {
    let flag = CompatibilityFlags::INC_RECURSE;
    assert_eq!(flag.bits(), 1 << 0, "CF_INC_RECURSE must be bit 0");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encoding must succeed");

    assert_eq!(buf, vec![1], "CF_INC_RECURSE encodes as [1]");
}

#[test]
fn test_symlink_times_flag_encoding() {
    let flag = CompatibilityFlags::SYMLINK_TIMES;
    assert_eq!(flag.bits(), 1 << 1, "CF_SYMLINK_TIMES must be bit 1");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encoding must succeed");

    assert_eq!(buf, vec![2], "CF_SYMLINK_TIMES encodes as [2]");
}

#[test]
fn test_symlink_iconv_flag_encoding() {
    let flag = CompatibilityFlags::SYMLINK_ICONV;
    assert_eq!(flag.bits(), 1 << 2, "CF_SYMLINK_ICONV must be bit 2");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encoding must succeed");

    assert_eq!(buf, vec![4], "CF_SYMLINK_ICONV encodes as [4]");
}

#[test]
fn test_safe_file_list_flag_encoding() {
    let flag = CompatibilityFlags::SAFE_FILE_LIST;
    assert_eq!(flag.bits(), 1 << 3, "CF_SAFE_FLIST must be bit 3");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encoding must succeed");

    assert_eq!(buf, vec![8], "CF_SAFE_FLIST encodes as [8]");
}

#[test]
fn test_avoid_xattr_optimization_flag_encoding() {
    let flag = CompatibilityFlags::AVOID_XATTR_OPTIMIZATION;
    assert_eq!(flag.bits(), 1 << 4, "CF_AVOID_XATTR_OPTIM must be bit 4");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encoding must succeed");

    assert_eq!(buf, vec![16], "CF_AVOID_XATTR_OPTIM encodes as [16]");
}

#[test]
fn test_checksum_seed_fix_flag_encoding() {
    let flag = CompatibilityFlags::CHECKSUM_SEED_FIX;
    assert_eq!(flag.bits(), 1 << 5, "CF_CHKSUM_SEED_FIX must be bit 5");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encoding must succeed");

    assert_eq!(buf, vec![32], "CF_CHKSUM_SEED_FIX encodes as [32]");
}

#[test]
fn test_inplace_partial_dir_flag_encoding() {
    let flag = CompatibilityFlags::INPLACE_PARTIAL_DIR;
    assert_eq!(flag.bits(), 1 << 6, "CF_INPLACE_PARTIAL_DIR must be bit 6");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encoding must succeed");

    assert_eq!(buf, vec![64], "CF_INPLACE_PARTIAL_DIR encodes as [64]");
}

#[test]
fn test_varint_flist_flags_flag_encoding() {
    let flag = CompatibilityFlags::VARINT_FLIST_FLAGS;
    assert_eq!(flag.bits(), 1 << 7, "CF_VARINT_FLIST_FLAGS must be bit 7");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encoding must succeed");

    // Rsync varint encoding of 128
    assert_eq!(
        buf,
        vec![128, 128],
        "CF_VARINT_FLIST_FLAGS encodes as [128, 128]"
    );
}

#[test]
fn test_id0_names_flag_encoding() {
    let flag = CompatibilityFlags::ID0_NAMES;
    assert_eq!(flag.bits(), 1 << 8, "CF_ID0_NAMES must be bit 8");

    let mut buf = Vec::new();
    flag.encode_to_vec(&mut buf).expect("encoding must succeed");

    // Rsync varint encoding of 256
    assert_eq!(buf, vec![129, 0], "CF_ID0_NAMES encodes as [129, 0]");
}

// ============================================================================
// All Known Flags Validation
// ============================================================================

#[test]
fn test_all_known_flags_have_unique_bits() {
    let all_flags = [
        CompatibilityFlags::INC_RECURSE,
        CompatibilityFlags::SYMLINK_TIMES,
        CompatibilityFlags::SYMLINK_ICONV,
        CompatibilityFlags::SAFE_FILE_LIST,
        CompatibilityFlags::AVOID_XATTR_OPTIMIZATION,
        CompatibilityFlags::CHECKSUM_SEED_FIX,
        CompatibilityFlags::INPLACE_PARTIAL_DIR,
        CompatibilityFlags::VARINT_FLIST_FLAGS,
        CompatibilityFlags::ID0_NAMES,
    ];

    // Verify no duplicate bits
    for (i, &flag_a) in all_flags.iter().enumerate() {
        for &flag_b in &all_flags[i + 1..] {
            assert_eq!(
                flag_a.intersection(flag_b),
                CompatibilityFlags::EMPTY,
                "flags must have non-overlapping bits"
            );
        }
    }
}

#[test]
fn test_all_known_flags_round_trip_encode_decode() {
    let all_flags = [
        CompatibilityFlags::INC_RECURSE,
        CompatibilityFlags::SYMLINK_TIMES,
        CompatibilityFlags::SYMLINK_ICONV,
        CompatibilityFlags::SAFE_FILE_LIST,
        CompatibilityFlags::AVOID_XATTR_OPTIMIZATION,
        CompatibilityFlags::CHECKSUM_SEED_FIX,
        CompatibilityFlags::INPLACE_PARTIAL_DIR,
        CompatibilityFlags::VARINT_FLIST_FLAGS,
        CompatibilityFlags::ID0_NAMES,
    ];

    for &flag in &all_flags {
        let mut buf = Vec::new();
        flag.encode_to_vec(&mut buf).expect("encode must succeed");

        let (decoded, remainder) =
            CompatibilityFlags::decode_from_slice(&buf).expect("decode must succeed");

        assert_eq!(decoded, flag, "round-trip encode/decode must preserve flag");
        assert_eq!(remainder.len(), 0, "must consume entire buffer");
    }
}

#[test]
fn test_all_known_constant_matches_union() {
    let manual_union = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SYMLINK_TIMES
        | CompatibilityFlags::SYMLINK_ICONV
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::INPLACE_PARTIAL_DIR
        | CompatibilityFlags::VARINT_FLIST_FLAGS
        | CompatibilityFlags::ID0_NAMES;

    assert_eq!(
        CompatibilityFlags::ALL_KNOWN,
        manual_union,
        "ALL_KNOWN constant must match union of all flags"
    );
}

#[test]
fn test_known_compatibility_flag_enum_all_array() {
    assert_eq!(
        KnownCompatibilityFlag::ALL.len(),
        9,
        "ALL array must contain all 9 known flags"
    );

    // Verify ordering matches bit positions
    let expected_order = [
        KnownCompatibilityFlag::IncRecurse,
        KnownCompatibilityFlag::SymlinkTimes,
        KnownCompatibilityFlag::SymlinkIconv,
        KnownCompatibilityFlag::SafeFileList,
        KnownCompatibilityFlag::AvoidXattrOptimization,
        KnownCompatibilityFlag::ChecksumSeedFix,
        KnownCompatibilityFlag::InplacePartialDir,
        KnownCompatibilityFlag::VarintFlistFlags,
        KnownCompatibilityFlag::Id0Names,
    ];

    assert_eq!(
        KnownCompatibilityFlag::ALL,
        expected_order,
        "ALL array must be in ascending bit order"
    );
}

// ============================================================================
// Combined Flags Encoding Tests
// ============================================================================

#[test]
fn test_combined_flags_encoding() {
    let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;
    assert_eq!(flags.bits(), 0b1001, "combined flags must OR bits");

    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).expect("encode must succeed");

    // Varint encoding of 9
    assert_eq!(buf, vec![0x09], "combined flags encode as varint 9");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).expect("decode must succeed");
    assert_eq!(decoded, flags, "round-trip must preserve combined flags");
}

#[test]
fn test_all_known_flags_combined_encoding() {
    let flags = CompatibilityFlags::ALL_KNOWN;
    assert_eq!(
        flags.bits(),
        0b111111111,
        "ALL_KNOWN must have all 9 bits set"
    );

    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).expect("encode must succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).expect("decode must succeed");
    assert_eq!(
        decoded, flags,
        "round-trip encode/decode must preserve ALL_KNOWN"
    );
}

#[test]
fn test_empty_flags_encoding() {
    let flags = CompatibilityFlags::EMPTY;
    assert_eq!(flags.bits(), 0, "EMPTY must have no bits set");

    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).expect("encode must succeed");

    // Varint encoding of 0
    assert_eq!(buf, vec![0x00], "EMPTY encodes as varint 0");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).expect("decode must succeed");
    assert_eq!(decoded, flags, "round-trip must preserve EMPTY");
}

// ============================================================================
// Read/Write I/O Tests
// ============================================================================

#[test]
fn test_write_to_and_read_from_io() {
    let flags = CompatibilityFlags::SYMLINK_TIMES
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::ID0_NAMES;

    let mut buf = Vec::new();
    flags.write_to(&mut buf).expect("write_to must succeed");

    let mut cursor = Cursor::new(buf);
    let decoded = CompatibilityFlags::read_from(&mut cursor).expect("read_from must succeed");

    assert_eq!(
        decoded, flags,
        "read_from must decode what write_to encoded"
    );
}

#[test]
fn test_decode_from_slice_mut() {
    let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).expect("encode must succeed");
    buf.extend_from_slice(&[0x42, 0x43]); // Extra trailing bytes

    let mut slice: &[u8] = &buf;
    let decoded = CompatibilityFlags::decode_from_slice_mut(&mut slice)
        .expect("decode_from_slice_mut must succeed");

    assert_eq!(decoded, flags, "must decode the flags");
    assert_eq!(slice, &[0x42, 0x43], "must advance slice past encoded data");
}

// ============================================================================
// Unknown Bits Handling Tests
// ============================================================================

#[test]
fn test_unknown_bits_detection() {
    let flags_with_unknown = CompatibilityFlags::from_bits(0b1_0000_0000_1111);

    assert!(
        flags_with_unknown.has_unknown_bits(),
        "must detect unknown bits"
    );
    assert_eq!(
        flags_with_unknown.unknown_bits(),
        0b1_0000_0000_0000,
        "must isolate unknown bits"
    );
}

#[test]
fn test_without_unknown_bits_mask() {
    let flags_with_unknown = CompatibilityFlags::from_bits(0b1_0000_0000_1111);
    let masked = flags_with_unknown.without_unknown_bits();

    assert_eq!(masked.bits(), 0b1111, "must mask out unknown bits");
    assert!(
        !masked.has_unknown_bits(),
        "masked flags must have no unknown bits"
    );
}

#[test]
fn test_unknown_bits_round_trip() {
    // Upstream rsync tolerates unknown bits by preserving them
    let flags_with_unknown = CompatibilityFlags::from_bits(0xFFFF_FFFF);

    let mut buf = Vec::new();
    flags_with_unknown
        .encode_to_vec(&mut buf)
        .expect("encode must succeed");

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).expect("decode must succeed");

    assert_eq!(
        decoded, flags_with_unknown,
        "unknown bits must survive round-trip"
    );
}

// ============================================================================
// Iterator Tests
// ============================================================================

#[test]
fn test_iter_known_flags() {
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SYMLINK_ICONV
        | CompatibilityFlags::ID0_NAMES;

    let collected: Vec<_> = flags.iter_known().collect();

    assert_eq!(
        collected,
        vec![
            KnownCompatibilityFlag::IncRecurse,
            KnownCompatibilityFlag::SymlinkIconv,
            KnownCompatibilityFlag::Id0Names,
        ],
        "iterator must yield flags in ascending bit order"
    );
}

#[test]
fn test_iter_known_skips_unknown_bits() {
    // Bit 11 (unknown) + bit 3 (SAFE_FILE_LIST) + bit 0 (INC_RECURSE)
    let flags_with_unknown = CompatibilityFlags::from_bits(0b1_0000_0000_1001);

    let collected: Vec<_> = flags_with_unknown.iter_known().collect();

    assert_eq!(
        collected,
        vec![
            KnownCompatibilityFlag::IncRecurse,
            KnownCompatibilityFlag::SafeFileList,
        ],
        "iterator must skip unknown bits"
    );
}

#[test]
fn test_from_iterator() {
    let flags_vec = vec![
        KnownCompatibilityFlag::SymlinkTimes,
        KnownCompatibilityFlag::ChecksumSeedFix,
    ];

    let flags: CompatibilityFlags = flags_vec.into_iter().collect();

    assert_eq!(
        flags,
        CompatibilityFlags::SYMLINK_TIMES | CompatibilityFlags::CHECKSUM_SEED_FIX,
        "FromIterator must OR all flag bits"
    );
}

// ============================================================================
// Bitwise Operations Tests
// ============================================================================

#[test]
fn test_union_operation() {
    let a = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SYMLINK_TIMES;
    let b = CompatibilityFlags::SYMLINK_TIMES | CompatibilityFlags::SAFE_FILE_LIST;

    let union = a.union(b);

    assert_eq!(
        union,
        CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SYMLINK_TIMES
            | CompatibilityFlags::SAFE_FILE_LIST,
        "union must combine all bits"
    );
}

#[test]
fn test_intersection_operation() {
    let a = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SYMLINK_TIMES;
    let b = CompatibilityFlags::SYMLINK_TIMES | CompatibilityFlags::SAFE_FILE_LIST;

    let intersection = a.intersection(b);

    assert_eq!(
        intersection,
        CompatibilityFlags::SYMLINK_TIMES,
        "intersection must keep only common bits"
    );
}

#[test]
fn test_difference_operation() {
    let a = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SYMLINK_TIMES;
    let b = CompatibilityFlags::SYMLINK_TIMES | CompatibilityFlags::SAFE_FILE_LIST;

    let difference = a.difference(b);

    assert_eq!(
        difference,
        CompatibilityFlags::INC_RECURSE,
        "difference must keep bits in a but not in b"
    );
}

#[test]
fn test_contains_operation() {
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SYMLINK_TIMES
        | CompatibilityFlags::SAFE_FILE_LIST;

    assert!(
        flags.contains(CompatibilityFlags::SYMLINK_TIMES),
        "must contain single flag"
    );
    assert!(
        flags.contains(CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST),
        "must contain subset of flags"
    );
    assert!(
        !flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX),
        "must not contain unset flag"
    );
}

// ============================================================================
// Display and Debug Tests
// ============================================================================

#[test]
fn test_display_empty_flags() {
    let flags = CompatibilityFlags::EMPTY;
    assert_eq!(
        flags.to_string(),
        "CF_NONE",
        "EMPTY must display as CF_NONE"
    );
}

#[test]
fn test_display_single_flag() {
    let flags = CompatibilityFlags::INC_RECURSE;
    assert_eq!(
        flags.to_string(),
        "CF_INC_RECURSE",
        "single flag must display its name"
    );
}

#[test]
fn test_display_multiple_flags() {
    let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;
    assert_eq!(
        flags.to_string(),
        "CF_INC_RECURSE | CF_SAFE_FLIST",
        "multiple flags must display with | separator"
    );
}

#[test]
fn test_display_with_unknown_bits() {
    // Bit 9 (unknown) + bit 0 (INC_RECURSE, known)
    let flags = CompatibilityFlags::from_bits(0x201);
    let display = flags.to_string();

    assert!(
        display.contains("CF_INC_RECURSE"),
        "display must include known flags"
    );
    assert!(
        display.contains("unknown(0x200)"),
        "display must indicate unknown bits"
    );
}

#[test]
fn test_debug_format() {
    let flags = CompatibilityFlags::from_bits(0x1AB);
    let debug = format!("{flags:?}");

    assert!(debug.contains("0x1ab"), "debug format must show hex bits");
}
