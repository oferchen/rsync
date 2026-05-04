use super::*;
use std::io;

fn encode(flags: CompatibilityFlags) -> Vec<u8> {
    let mut out = Vec::new();
    flags.encode_to_vec(&mut out).expect("encoding succeeds");
    out
}

#[test]
fn bit_constants_match_expected_values() {
    assert_eq!(CompatibilityFlags::INC_RECURSE.bits(), 1);
    assert_eq!(CompatibilityFlags::SYMLINK_TIMES.bits(), 1 << 1);
    assert_eq!(CompatibilityFlags::SYMLINK_ICONV.bits(), 1 << 2);
    assert_eq!(CompatibilityFlags::SAFE_FILE_LIST.bits(), 1 << 3);
    assert_eq!(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION.bits(), 1 << 4);
    assert_eq!(CompatibilityFlags::CHECKSUM_SEED_FIX.bits(), 1 << 5);
    assert_eq!(CompatibilityFlags::INPLACE_PARTIAL_DIR.bits(), 1 << 6);
    assert_eq!(CompatibilityFlags::VARINT_FLIST_FLAGS.bits(), 1 << 7);
    assert_eq!(CompatibilityFlags::ID0_NAMES.bits(), 1 << 8);
}

#[test]
fn encode_and_decode_round_trip_known_sets() {
    let sets = [
        CompatibilityFlags::EMPTY,
        CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SYMLINK_TIMES,
        CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::VARINT_FLIST_FLAGS,
        CompatibilityFlags::from_bits(1 << 30),
    ];

    for flags in sets {
        let encoded = encode(flags);
        let (decoded, remainder) =
            CompatibilityFlags::decode_from_slice(&encoded).expect("decoding succeeds");
        assert_eq!(decoded, flags);
        assert!(remainder.is_empty());
    }
}

#[test]
fn decode_from_slice_mut_advances_input_on_success() {
    let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;
    let mut encoded = encode(flags);
    encoded.extend_from_slice(&[0x42]);
    let mut slice: &[u8] = &encoded;

    let decoded =
        CompatibilityFlags::decode_from_slice_mut(&mut slice).expect("decoding should succeed");

    assert_eq!(decoded, flags);
    assert_eq!(slice, &[0x42]);
}

#[test]
fn decode_from_slice_mut_preserves_input_on_error() {
    let original: &[u8] = &[];
    let mut slice = original;
    let err =
        CompatibilityFlags::decode_from_slice_mut(&mut slice).expect_err("empty slice must error");

    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert!(slice.is_empty());
}

#[test]
fn iter_known_yields_flags_in_bit_order() {
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::VARINT_FLIST_FLAGS
        | CompatibilityFlags::CHECKSUM_SEED_FIX;

    let collected: Vec<_> = flags.iter_known().collect();
    assert_eq!(
        collected,
        vec![
            KnownCompatibilityFlag::IncRecurse,
            KnownCompatibilityFlag::ChecksumSeedFix,
            KnownCompatibilityFlag::VarintFlistFlags,
        ]
    );

    let mut iter = flags.iter_known();
    assert_eq!(iter.size_hint(), (3, Some(3)));
    assert_eq!(iter.len(), 3);
    assert_eq!(iter.next(), Some(KnownCompatibilityFlag::IncRecurse));
    assert_eq!(iter.size_hint(), (2, Some(2)));
    assert_eq!(iter.len(), 2);
}

#[test]
fn iter_known_skips_unknown_bits() {
    let flags = CompatibilityFlags::from_bits(1 << 15)
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::ID0_NAMES;

    let collected: Vec<_> = flags.iter_known().collect();
    assert_eq!(
        collected,
        vec![
            KnownCompatibilityFlag::SafeFileList,
            KnownCompatibilityFlag::Id0Names,
        ]
    );
}

#[test]
fn iter_known_supports_double_ended_iteration() {
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::ID0_NAMES;

    let mut iter = flags.iter_known();
    assert_eq!(iter.len(), 3);
    assert_eq!(iter.next_back(), Some(KnownCompatibilityFlag::Id0Names));
    assert_eq!(iter.len(), 2);
    assert_eq!(iter.next(), Some(KnownCompatibilityFlag::IncRecurse));
    assert_eq!(iter.len(), 1);
    assert_eq!(iter.next_back(), Some(KnownCompatibilityFlag::SafeFileList));
    assert_eq!(iter.len(), 0);
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next_back(), None);
}

#[test]
fn iter_known_rev_collects_descending_order() {
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::VARINT_FLIST_FLAGS
        | CompatibilityFlags::ID0_NAMES;

    let collected: Vec<_> = flags.iter_known().rev().collect();
    assert_eq!(
        collected,
        vec![
            KnownCompatibilityFlag::Id0Names,
            KnownCompatibilityFlag::VarintFlistFlags,
            KnownCompatibilityFlag::ChecksumSeedFix,
            KnownCompatibilityFlag::IncRecurse,
        ]
    );
}

#[test]
fn collecting_known_flags_produces_expected_bitfield() {
    let flags: CompatibilityFlags = [
        KnownCompatibilityFlag::IncRecurse,
        KnownCompatibilityFlag::ChecksumSeedFix,
        KnownCompatibilityFlag::IncRecurse,
    ]
    .into_iter()
    .collect();

    assert_eq!(
        flags,
        CompatibilityFlags::INC_RECURSE | CompatibilityFlags::CHECKSUM_SEED_FIX
    );
}

#[test]
fn extend_adds_flags_without_clearing_existing_bits() {
    let mut flags = CompatibilityFlags::INC_RECURSE;
    flags.extend([
        KnownCompatibilityFlag::SafeFileList,
        KnownCompatibilityFlag::SafeFileList,
        KnownCompatibilityFlag::Id0Names,
    ]);

    assert!(flags.contains(CompatibilityFlags::INC_RECURSE));
    assert!(flags.contains(CompatibilityFlags::SAFE_FILE_LIST));
    assert!(flags.contains(CompatibilityFlags::ID0_NAMES));
    assert_eq!(flags.unknown_bits(), 0);
}

#[test]
fn all_constant_exposes_canonical_ordering() {
    let expected: Vec<_> = KnownCompatibilityFlag::ALL.into_iter().collect();
    let iterated: Vec<_> = CompatibilityFlags::ALL_KNOWN.iter_known().collect();

    assert_eq!(iterated, expected);

    let combined = KnownCompatibilityFlag::ALL
        .into_iter()
        .fold(CompatibilityFlags::EMPTY, |flags, flag| {
            flags | flag.as_flag()
        });

    assert_eq!(combined, CompatibilityFlags::ALL_KNOWN);
}

#[test]
fn from_known_flag_promotes_variant_to_bitfield() {
    let promoted = CompatibilityFlags::from(KnownCompatibilityFlag::SafeFileList);

    assert_eq!(promoted, CompatibilityFlags::SAFE_FILE_LIST);
    assert!(promoted.contains(CompatibilityFlags::SAFE_FILE_LIST));
    assert!(
        promoted
            .iter_known()
            .eq([KnownCompatibilityFlag::SafeFileList])
    );
}

#[test]
fn into_iter_collects_known_flags() {
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::VARINT_FLIST_FLAGS;

    let collected: Vec<_> = flags.into_iter().collect();
    assert_eq!(
        collected,
        vec![
            KnownCompatibilityFlag::IncRecurse,
            KnownCompatibilityFlag::ChecksumSeedFix,
            KnownCompatibilityFlag::VarintFlistFlags,
        ]
    );
}

#[test]
fn into_iter_for_references_matches_owned_iteration() {
    let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;

    let owned: Vec<_> = flags.into_iter().collect();
    let shared: Vec<_> = (&flags).into_iter().collect();
    let mut clone = flags;
    let mut shared_mut: Vec<_> = (&mut clone).into_iter().collect();

    assert_eq!(owned, shared);
    assert_eq!(shared, shared_mut);
    shared_mut.reverse();
    assert_eq!(
        shared_mut,
        vec![
            KnownCompatibilityFlag::SafeFileList,
            KnownCompatibilityFlag::IncRecurse
        ]
    );
}

#[test]
fn read_from_preserves_high_bit_flags() {
    let flags = CompatibilityFlags::from_bits(0x8000_0001);
    let mut encoded = Vec::new();
    flags
        .encode_to_vec(&mut encoded)
        .expect("encoding succeeds");

    let mut cursor = io::Cursor::new(&encoded[..]);
    let decoded = CompatibilityFlags::read_from(&mut cursor)
        .expect("two's-complement values must round-trip");
    assert_eq!(decoded.bits(), flags.bits());

    let (from_slice, remainder) =
        CompatibilityFlags::decode_from_slice(&encoded).expect("slice decoding succeeds");
    assert_eq!(from_slice.bits(), flags.bits());
    assert!(remainder.is_empty());
}

#[test]
fn unknown_bits_reports_future_flags() {
    let flags = CompatibilityFlags::from_bits(0x1FF | (1 << 12));
    assert_eq!(flags.unknown_bits(), 1 << 12);
}

#[test]
fn has_unknown_bits_detects_future_flags() {
    let future = CompatibilityFlags::from_bits(CompatibilityFlags::ID0_NAMES.bits() | (1 << 17));

    assert!(future.has_unknown_bits());
    assert!(!CompatibilityFlags::ID0_NAMES.has_unknown_bits());
}

#[test]
fn without_unknown_bits_masks_future_flags() {
    let mixed = CompatibilityFlags::from_bits(CompatibilityFlags::INC_RECURSE.bits() | (1 << 29));
    let masked = mixed.without_unknown_bits();

    assert_eq!(masked, CompatibilityFlags::INC_RECURSE);
    assert_eq!(masked.unknown_bits(), 0);
    assert!(mixed.has_unknown_bits());
}

#[test]
fn bitwise_operators_behave_like_bitfields() {
    let mut flags = CompatibilityFlags::INC_RECURSE;
    flags |= CompatibilityFlags::SYMLINK_TIMES;
    assert!(flags.contains(CompatibilityFlags::SYMLINK_TIMES));

    flags &= CompatibilityFlags::SYMLINK_TIMES;
    assert_eq!(flags, CompatibilityFlags::SYMLINK_TIMES);

    flags ^= CompatibilityFlags::SYMLINK_TIMES;
    assert!(flags.is_empty());

    flags |= CompatibilityFlags::SYMLINK_ICONV;
    assert!(flags.contains(CompatibilityFlags::SYMLINK_ICONV));
    assert!(!flags.contains(CompatibilityFlags::SYMLINK_TIMES));
}

#[test]
fn known_flag_name_matches_display_output() {
    for flag in [
        KnownCompatibilityFlag::IncRecurse,
        KnownCompatibilityFlag::SymlinkTimes,
        KnownCompatibilityFlag::SymlinkIconv,
        KnownCompatibilityFlag::SafeFileList,
        KnownCompatibilityFlag::AvoidXattrOptimization,
        KnownCompatibilityFlag::ChecksumSeedFix,
        KnownCompatibilityFlag::InplacePartialDir,
        KnownCompatibilityFlag::VarintFlistFlags,
        KnownCompatibilityFlag::Id0Names,
    ] {
        assert_eq!(flag.name(), flag.to_string());
    }
}

#[test]
fn known_flag_from_str_accepts_canonical_names() {
    use std::str::FromStr;

    for (name, expected) in [
        ("CF_INC_RECURSE", KnownCompatibilityFlag::IncRecurse),
        ("CF_SYMLINK_TIMES", KnownCompatibilityFlag::SymlinkTimes),
        ("CF_SYMLINK_ICONV", KnownCompatibilityFlag::SymlinkIconv),
        ("CF_SAFE_FLIST", KnownCompatibilityFlag::SafeFileList),
        (
            "CF_AVOID_XATTR_OPTIM",
            KnownCompatibilityFlag::AvoidXattrOptimization,
        ),
        (
            "CF_CHKSUM_SEED_FIX",
            KnownCompatibilityFlag::ChecksumSeedFix,
        ),
        (
            "CF_INPLACE_PARTIAL_DIR",
            KnownCompatibilityFlag::InplacePartialDir,
        ),
        (
            "CF_VARINT_FLIST_FLAGS",
            KnownCompatibilityFlag::VarintFlistFlags,
        ),
        ("CF_ID0_NAMES", KnownCompatibilityFlag::Id0Names),
    ] {
        assert_eq!(KnownCompatibilityFlag::from_str(name).unwrap(), expected);
        assert_eq!(KnownCompatibilityFlag::from_name(name).unwrap(), expected);
    }
}

#[test]
fn known_flag_from_str_rejects_unknown_identifiers() {
    use std::str::FromStr;

    let err = KnownCompatibilityFlag::from_str("CF_UNKNOWN").expect_err("unknown flag");
    assert_eq!(err.identifier(), "CF_UNKNOWN");
    assert_eq!(
        err.to_string(),
        "unrecognized compatibility flag identifier: CF_UNKNOWN"
    );
}

#[test]
fn parse_known_flag_error_implements_std_error() {
    fn assert_error<E: std::error::Error>() {}

    assert_error::<ParseKnownCompatibilityFlagError>();
}

#[test]
fn compatibility_flags_display_lists_known_and_unknown_bits() {
    let known = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;
    assert_eq!(known.to_string(), "CF_INC_RECURSE | CF_SAFE_FLIST");

    let unknown_only = CompatibilityFlags::from_bits(1 << 12);
    assert_eq!(unknown_only.to_string(), "unknown(0x1000)");

    let mixed = known | CompatibilityFlags::from_bits(1 << 20);
    assert_eq!(
        mixed.to_string(),
        "CF_INC_RECURSE | CF_SAFE_FLIST | unknown(0x100000)"
    );

    assert_eq!(CompatibilityFlags::EMPTY.to_string(), "CF_NONE");
}
