use super::constants::UPSTREAM_PROTOCOL_RANGE;
use super::*;
use crate::error::NegotiationError;
use core::convert::TryFrom;
use core::iter::FusedIterator;
use core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize, Wrapping,
};
use core::str::FromStr;
use proptest::prelude::*;

fn reference_negotiation(peer_versions: &[u8]) -> Result<ProtocolVersion, NegotiationError> {
    use std::collections::BTreeSet;

    let mut recognized = BTreeSet::new();
    let mut supported_bitmap: u64 = 0;
    let mut oldest_rejection: Option<u8> = None;

    for &advertised in peer_versions {
        if advertised < ProtocolVersion::OLDEST.as_u8() {
            oldest_rejection = Some(match oldest_rejection {
                Some(current) if advertised >= current => current,
                _ => advertised,
            });
            continue;
        }

        let clamped = advertised.min(ProtocolVersion::NEWEST.as_u8());
        recognized.insert(clamped);

        let bit = 1u64 << clamped;
        if SUPPORTED_PROTOCOL_BITMAP & bit != 0 {
            supported_bitmap |= bit;

            if clamped == ProtocolVersion::NEWEST.as_u8() {
                return Ok(ProtocolVersion::NEWEST);
            }
        }
    }

    if supported_bitmap != 0 {
        let highest_bit = (u64::BITS - 1) - supported_bitmap.leading_zeros();
        let highest = highest_bit as u8;
        return Ok(ProtocolVersion::new_const(highest));
    }

    if let Some(rejected) = oldest_rejection {
        return Err(NegotiationError::UnsupportedVersion(rejected));
    }

    let peer_versions = recognized.into_iter().collect();
    Err(NegotiationError::NoMutualProtocol { peer_versions })
}

fn collect_advertised<I, T>(inputs: I) -> Vec<u8>
where
    I: IntoIterator<Item = T>,
    T: ProtocolVersionAdvertisement,
{
    inputs
        .into_iter()
        .map(|value| value.into_advertised_version())
        .collect()
}

#[test]
fn newest_protocol_is_preferred() {
    let result = select_highest_mutual([32, 31, 30]).expect("must succeed");
    assert_eq!(result, ProtocolVersion::NEWEST);
}

#[test]
fn downgrades_when_peer_lacks_newest() {
    let result = select_highest_mutual([31]).expect("must succeed");
    assert_eq!(result.as_u8(), 31);
}

#[test]
fn reports_no_mutual_protocol() {
    let err = select_highest_mutual(core::iter::empty::<u8>()).unwrap_err();
    assert_eq!(
        err,
        NegotiationError::NoMutualProtocol {
            peer_versions: vec![]
        }
    );
}

#[test]
fn select_highest_mutual_deduplicates_peer_versions() {
    let negotiated = select_highest_mutual([32, 32, 31, 31]).expect("must select 32");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn select_highest_mutual_handles_unsorted_peer_versions() {
    let negotiated = select_highest_mutual([29, 32, 30, 31]).expect("must select newest");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn select_highest_mutual_short_circuits_after_newest() {
    #[derive(Default)]
    struct PanicAfterNewest {
        state: u8,
    }

    impl Iterator for PanicAfterNewest {
        type Item = u8;

        fn next(&mut self) -> Option<Self::Item> {
            match self.state {
                0 => {
                    self.state = 1;
                    Some(ProtocolVersion::OLDEST.as_u8().saturating_sub(1))
                }
                1 => {
                    self.state = 2;
                    Some(ProtocolVersion::NEWEST.as_u8())
                }
                _ => panic!(
                    "select_highest_mutual should return immediately after seeing the newest protocol"
                ),
            }
        }
    }

    let negotiated = select_highest_mutual(PanicAfterNewest::default())
        .expect("must select newest without exhausting iterator");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn select_highest_mutual_accepts_slice_iterators() {
    let peers = [31u8, 29, 32];
    let negotiated = select_highest_mutual(peers.iter()).expect("slice iter works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn select_highest_mutual_accepts_mut_slice_iterators() {
    let mut peers = [31u8, 29, 32];
    let negotiated = select_highest_mutual(peers.iter_mut()).expect("mut slice iter works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
    assert_eq!(peers, [31u8, 29, 32]);
}

#[test]
fn select_highest_mutual_accepts_protocol_version_references() {
    let peers = ProtocolVersion::supported_versions();
    let negotiated = select_highest_mutual(peers.iter()).expect("refs work");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn named_version_constants_match_supported_protocols() {
    let expected = [
        ProtocolVersion::V32,
        ProtocolVersion::V31,
        ProtocolVersion::V30,
        ProtocolVersion::V29,
        ProtocolVersion::V28,
    ];

    assert_eq!(
        expected,
        *ProtocolVersion::supported_versions_array(),
        "named constants must match the canonical supported ordering",
    );

    let expected_numbers: Vec<u8> = expected.iter().map(|version| version.as_u8()).collect();
    assert_eq!(
        expected_numbers,
        ProtocolVersion::supported_protocol_numbers(),
        "named constants must mirror the exported numeric list",
    );
    assert_eq!(expected.len(), SUPPORTED_PROTOCOL_COUNT);
}

#[test]
fn negotiation_style_predicates_match_protocol_boundaries() {
    assert!(ProtocolVersion::V32.uses_binary_negotiation());
    assert!(ProtocolVersion::V31.uses_binary_negotiation());
    assert!(ProtocolVersion::V30.uses_binary_negotiation());

    assert!(ProtocolVersion::V29.uses_legacy_ascii_negotiation());
    assert!(ProtocolVersion::V28.uses_legacy_ascii_negotiation());

    assert!(!ProtocolVersion::V29.uses_binary_negotiation());
    assert!(!ProtocolVersion::V28.uses_binary_negotiation());
}

#[test]
fn select_highest_mutual_accepts_non_zero_u8_advertisements() {
    let peers = [
        NonZeroU8::new(32).expect("non-zero"),
        NonZeroU8::new(31).expect("non-zero"),
    ];
    let negotiated = select_highest_mutual(peers).expect("non-zero values work");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn select_highest_mutual_accepts_non_zero_unsigned_advertisements() {
    fn check<T: ProtocolVersionAdvertisement + Copy>(advertised: T) {
        let negotiated = select_highest_mutual([advertised]).expect("non-zero unsigned works");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    check(NonZeroU16::new(32).expect("non-zero"));
    check(NonZeroU32::new(32).expect("non-zero"));
    check(NonZeroU64::new(32).expect("non-zero"));
    check(NonZeroU128::new(32).expect("non-zero"));
    check(NonZeroUsize::new(32).expect("non-zero"));

    let future = NonZeroU64::new(200).expect("non-zero");
    let negotiated = select_highest_mutual([future]).expect("future values clamp");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn select_highest_mutual_accepts_non_zero_signed_advertisements() {
    fn check<T: ProtocolVersionAdvertisement + Copy>(advertised: T, expected: ProtocolVersion) {
        let negotiated = select_highest_mutual([advertised]).expect("non-zero signed works");
        assert_eq!(negotiated, expected);
    }

    check(
        NonZeroI8::new(32).expect("non-zero"),
        ProtocolVersion::NEWEST,
    );
    check(
        NonZeroI16::new(i16::from(ProtocolVersion::NEWEST.as_u8())).expect("non-zero"),
        ProtocolVersion::NEWEST,
    );
    check(
        NonZeroI32::new(i32::from(ProtocolVersion::OLDEST.as_u8())).expect("non-zero"),
        ProtocolVersion::OLDEST,
    );
    check(
        NonZeroI64::new(i64::from(ProtocolVersion::NEWEST.as_u8())).expect("non-zero"),
        ProtocolVersion::NEWEST,
    );
    check(
        NonZeroI128::new(i128::from(ProtocolVersion::NEWEST.as_u8())).expect("non-zero"),
        ProtocolVersion::NEWEST,
    );
    check(
        NonZeroIsize::new(isize::from(ProtocolVersion::NEWEST.as_u8())).expect("non-zero"),
        ProtocolVersion::NEWEST,
    );

    let future = NonZeroI128::new(200).expect("non-zero");
    let negotiated = select_highest_mutual([future]).expect("future values clamp");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn protocol_version_converts_to_wider_unsigned_primitives() {
    for version in ProtocolVersion::supported_versions_iter() {
        let expected = version.as_u8();
        assert_eq!(u16::from(version), u16::from(expected));
        assert_eq!(u32::from(version), u32::from(expected));
        assert_eq!(u64::from(version), u64::from(expected));
        assert_eq!(u128::from(version), u128::from(expected));
        assert_eq!(usize::from(version), usize::from(expected));
        assert_eq!(version.as_usize(), usize::from(expected));
    }
}

#[test]
fn protocol_version_converts_to_nonzero_wider_unsigned_primitives() {
    for version in ProtocolVersion::supported_versions_iter() {
        let expected = version.as_u8();
        assert_eq!(NonZeroU16::from(version).get(), u16::from(expected));
        assert_eq!(NonZeroU32::from(version).get(), u32::from(expected));
        assert_eq!(NonZeroU64::from(version).get(), u64::from(expected));
        assert_eq!(NonZeroU128::from(version).get(), u128::from(expected));
        assert_eq!(NonZeroUsize::from(version).get(), usize::from(expected));
    }
}

#[test]
fn protocol_version_lookup_by_supported_index() {
    for (index, &expected) in ProtocolVersion::supported_versions().iter().enumerate() {
        assert_eq!(ProtocolVersion::from_supported_index(index), Some(expected));
    }

    assert_eq!(
        ProtocolVersion::from_supported_index(SUPPORTED_PROTOCOL_COUNT),
        None,
    );
    assert_eq!(ProtocolVersion::from_supported_index(usize::MAX), None);
}

#[test]
fn select_highest_mutual_accepts_wrapping_unsigned_advertisements() {
    let negotiated = select_highest_mutual([
        Wrapping::<u16>(u16::from(ProtocolVersion::NEWEST.as_u8())),
        Wrapping::<u16>(u16::from(ProtocolVersion::OLDEST.as_u8())),
    ])
    .expect("wrapping u16 works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let negotiated = select_highest_mutual([
        Wrapping::<u32>(u32::from(ProtocolVersion::NEWEST.as_u8())),
        Wrapping::<u32>(u32::from(ProtocolVersion::OLDEST.as_u8())),
    ])
    .expect("wrapping u32 works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let negotiated = select_highest_mutual([
        Wrapping::<u64>(u64::from(ProtocolVersion::NEWEST.as_u8())),
        Wrapping::<u64>(u64::from(ProtocolVersion::OLDEST.as_u8())),
    ])
    .expect("wrapping u64 works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let negotiated = select_highest_mutual([
        Wrapping::<u128>(u128::from(ProtocolVersion::NEWEST.as_u8())),
        Wrapping::<u128>(u128::from(ProtocolVersion::OLDEST.as_u8())),
    ])
    .expect("wrapping u128 works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let negotiated = select_highest_mutual([
        Wrapping::<usize>(usize::from(ProtocolVersion::NEWEST.as_u8())),
        Wrapping::<usize>(usize::from(ProtocolVersion::OLDEST.as_u8())),
    ])
    .expect("wrapping usize works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let future = Wrapping::<u128>(200);
    let negotiated = select_highest_mutual([future]).expect("wrapping future clamps");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn select_highest_mutual_accepts_wrapping_signed_advertisements() {
    let negotiated = select_highest_mutual([
        Wrapping::<i16>(i16::from(ProtocolVersion::NEWEST.as_u8())),
        Wrapping::<i16>(i16::from(ProtocolVersion::OLDEST.as_u8())),
    ])
    .expect("wrapping i16 works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let negotiated = select_highest_mutual([
        Wrapping::<i32>(i32::from(ProtocolVersion::NEWEST.as_u8())),
        Wrapping::<i32>(i32::from(ProtocolVersion::OLDEST.as_u8())),
    ])
    .expect("wrapping i32 works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let negotiated = select_highest_mutual([
        Wrapping::<i64>(i64::from(ProtocolVersion::NEWEST.as_u8())),
        Wrapping::<i64>(i64::from(ProtocolVersion::OLDEST.as_u8())),
    ])
    .expect("wrapping i64 works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let negotiated = select_highest_mutual([
        Wrapping::<i128>(i128::from(ProtocolVersion::NEWEST.as_u8())),
        Wrapping::<i128>(i128::from(ProtocolVersion::OLDEST.as_u8())),
    ])
    .expect("wrapping i128 works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let negotiated = select_highest_mutual([
        Wrapping::<isize>(isize::from(ProtocolVersion::NEWEST.as_u8())),
        Wrapping::<isize>(isize::from(ProtocolVersion::OLDEST.as_u8())),
    ])
    .expect("wrapping isize works");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let negative = Wrapping::<i64>(-12);
    let err = select_highest_mutual([negative]).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(0));
}

#[test]
fn select_highest_mutual_clamps_negative_non_zero_signed_advertisements() {
    let negative = NonZeroI16::new(-5).expect("non-zero");
    let err = select_highest_mutual([negative]).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(0));

    let negative_isize = NonZeroIsize::new(-12).expect("non-zero");
    let err = select_highest_mutual([negative_isize]).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(0));
}

#[test]
fn supported_protocol_helpers_remain_consistent() {
    let numbers_slice = ProtocolVersion::supported_protocol_numbers();
    assert_eq!(numbers_slice, &SUPPORTED_PROTOCOLS);

    let numbers_from_iter: Vec<u8> = ProtocolVersion::supported_protocol_numbers_iter().collect();
    assert_eq!(numbers_from_iter, SUPPORTED_PROTOCOLS);

    let versions_slice = ProtocolVersion::supported_versions();
    let versions_slice_numbers: Vec<u8> = versions_slice
        .iter()
        .map(|version| version.as_u8())
        .collect();
    assert_eq!(versions_slice_numbers, SUPPORTED_PROTOCOLS);

    let versions_from_iter: Vec<u8> = ProtocolVersion::supported_versions_iter()
        .map(|version| version.as_u8())
        .collect();
    assert_eq!(versions_from_iter, SUPPORTED_PROTOCOLS);

    let range = ProtocolVersion::supported_range();
    assert_eq!(*range.start(), ProtocolVersion::OLDEST.as_u8());
    assert_eq!(*range.end(), ProtocolVersion::NEWEST.as_u8());

    let expected_display = SUPPORTED_PROTOCOLS
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        ProtocolVersion::supported_protocol_numbers_display(),
        expected_display
    );
}

#[test]
fn supported_protocol_bitmap_matches_expected_bits() {
    let bitmap = ProtocolVersion::supported_protocol_bitmap();
    assert_eq!(bitmap, SUPPORTED_PROTOCOL_BITMAP);
    assert_eq!(bitmap.count_ones() as usize, SUPPORTED_PROTOCOL_COUNT);

    for &version in &SUPPORTED_PROTOCOLS {
        let mask = 1u64 << version;
        assert_ne!(bitmap & mask, 0, "bit for protocol {version} must be set");
    }

    let lower_mask = (1u64 << ProtocolVersion::OLDEST.as_u8()) - 1;
    assert_eq!(
        bitmap & lower_mask,
        0,
        "bitmap must not contain bits below oldest"
    );

    let upper_shift = usize::from(ProtocolVersion::NEWEST.as_u8()) + 1;
    assert_eq!(
        bitmap >> upper_shift,
        0,
        "bitmap must not contain bits above newest"
    );
}

#[test]
fn select_highest_mutual_accepts_wider_integer_advertisements() {
    let peers = [u16::from(ProtocolVersion::NEWEST.as_u8()), 0u16];
    let negotiated = select_highest_mutual(peers).expect("wider integers supported");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let peers = [usize::from(ProtocolVersion::OLDEST.as_u8())];
    let negotiated = select_highest_mutual(peers).expect("usize conversions work");
    assert_eq!(negotiated, ProtocolVersion::OLDEST);
}

#[test]
fn select_highest_mutual_accepts_signed_integer_advertisements() {
    let peers = [32i16, 29i16];
    let negotiated = select_highest_mutual(peers).expect("signed integers supported");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let peers = [-5isize, 31isize];
    let negotiated = select_highest_mutual(peers).expect("negative values do not prevent success");
    assert_eq!(negotiated.as_u8(), 31);
}

#[test]
fn select_highest_mutual_clamps_negative_signed_advertisements() {
    let err = select_highest_mutual([-1i8, -12i8]).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(0));
}

#[test]
fn select_highest_mutual_saturates_large_signed_advertisements() {
    let peers = [i32::MAX];
    let negotiated = select_highest_mutual(peers).expect("large signed values clamp to newest");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let peers = [i128::MAX];
    let negotiated = select_highest_mutual(peers).expect("i128 advertisements clamp");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn select_highest_mutual_saturates_wider_integer_advertisements() {
    let peers = [u32::MAX];
    let negotiated = select_highest_mutual(peers).expect("future versions clamp");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);

    let peers = [u128::MAX];
    let negotiated = select_highest_mutual(peers).expect("u128 advertisements clamp");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn display_for_no_mutual_protocol_mentions_filtered_list() {
    let err = NegotiationError::NoMutualProtocol {
        peer_versions: vec![29, 30],
    };
    let rendered = err.to_string();
    assert!(rendered.contains("peer offered [29, 30]"));
    assert!(rendered.contains("we support"));
}

#[test]
fn rejects_zero_protocol_version() {
    let err = select_highest_mutual([0]).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(0));
}

#[test]
fn clamps_future_versions_in_peer_advertisements_directly() {
    let negotiated = ProtocolVersion::from_peer_advertisement(40).expect("future versions clamp");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn rejects_peer_advertisements_older_than_supported_range() {
    let err = ProtocolVersion::from_peer_advertisement(27).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(27));
}

#[test]
fn rejects_zero_peer_advertisement() {
    let err = ProtocolVersion::from_peer_advertisement(0).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(0));
}

#[test]
fn clamps_future_peer_versions_in_selection() {
    let negotiated = select_highest_mutual([35, 31]).expect("must clamp to newest");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn ignores_versions_older_than_supported_when_newer_exists() {
    let negotiated = select_highest_mutual([27, 29, 27]).expect("29 should be selected");
    assert_eq!(negotiated.as_u8(), 29);
}

#[test]
fn reports_unsupported_when_only_too_old_versions_are_offered() {
    let err = select_highest_mutual([27, 26]).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(26));
}

#[test]
fn supported_versions_constant_matches_u8_list() {
    let expected: Vec<u8> = ProtocolVersion::SUPPORTED_VERSIONS
        .into_iter()
        .map(ProtocolVersion::as_u8)
        .collect();
    assert_eq!(expected, SUPPORTED_PROTOCOLS);
}

#[test]
fn supported_versions_method_matches_constant_slice() {
    assert_eq!(
        ProtocolVersion::supported_versions(),
        ProtocolVersion::SUPPORTED_VERSIONS.as_slice()
    );
    assert_eq!(
        ProtocolVersion::supported_versions_array(),
        &ProtocolVersion::SUPPORTED_VERSIONS,
    );
}

#[test]
fn supported_protocol_number_guard_matches_constants() {
    for &value in &SUPPORTED_PROTOCOLS {
        assert!(ProtocolVersion::is_supported_protocol_number(value));
    }

    assert!(!ProtocolVersion::is_supported_protocol_number(
        ProtocolVersion::OLDEST.as_u8() - 1
    ));
    assert!(!ProtocolVersion::is_supported_protocol_number(
        ProtocolVersion::NEWEST.as_u8() + 1
    ));
}

#[test]
fn supported_protocol_count_matches_helpers() {
    assert_eq!(
        SUPPORTED_PROTOCOL_COUNT,
        SUPPORTED_PROTOCOLS.len(),
        "count constant must match numeric list length",
    );
    assert_eq!(
        SUPPORTED_PROTOCOL_COUNT,
        ProtocolVersion::supported_versions().len(),
        "count constant must match cached ProtocolVersion list",
    );
    assert_eq!(
        SUPPORTED_PROTOCOL_COUNT,
        ProtocolVersion::supported_versions_array().len(),
        "count constant must match cached ProtocolVersion array",
    );
}

#[test]
fn supported_protocol_numbers_matches_constant_slice() {
    assert_eq!(
        ProtocolVersion::supported_protocol_numbers(),
        &SUPPORTED_PROTOCOLS
    );
}

#[test]
fn supported_protocol_numbers_array_matches_constant_slice() {
    assert_eq!(
        ProtocolVersion::supported_protocol_numbers_array(),
        &SUPPORTED_PROTOCOLS
    );
}

#[test]
fn supported_versions_iterator_matches_constants() {
    let via_iterator: Vec<u8> = ProtocolVersion::supported_versions_iter()
        .map(ProtocolVersion::as_u8)
        .collect();
    assert_eq!(via_iterator, SUPPORTED_PROTOCOLS);
}

#[test]
fn supported_protocol_numbers_iter_matches_constant_slice() {
    let iterated: Vec<u8> = ProtocolVersion::supported_protocol_numbers_iter().collect();
    assert_eq!(iterated.as_slice(), &SUPPORTED_PROTOCOLS);
}

#[test]
fn supported_protocol_numbers_iter_is_sorted_descending() {
    let iterated: Vec<u8> = ProtocolVersion::supported_protocol_numbers_iter().collect();
    assert!(iterated.windows(2).all(|pair| pair[0] >= pair[1]));
}

#[test]
fn offset_from_oldest_counts_up_across_supported_versions() {
    for (expected_offset, &version) in ProtocolVersion::supported_versions()
        .iter()
        .rev()
        .enumerate()
    {
        assert_eq!(version.offset_from_oldest(), expected_offset);
    }
}

#[test]
fn offset_from_newest_matches_descending_index() {
    for (index, &version) in ProtocolVersion::supported_versions().iter().enumerate() {
        assert_eq!(version.offset_from_newest(), index);
        assert_eq!(
            version.offset_from_oldest() + version.offset_from_newest(),
            SUPPORTED_PROTOCOL_COUNT - 1,
            "offsets should mirror the supported protocol span",
        );
    }
}

#[test]
fn offset_conversions_round_trip_supported_versions() {
    for &version in ProtocolVersion::supported_versions().iter() {
        assert_eq!(
            ProtocolVersion::from_oldest_offset(version.offset_from_oldest()),
            Some(version),
            "offset_from_oldest should invert from_oldest_offset",
        );
        assert_eq!(
            ProtocolVersion::from_newest_offset(version.offset_from_newest()),
            Some(version),
            "offset_from_newest should invert from_newest_offset",
        );
    }

    let max_oldest_offset = ProtocolVersion::NEWEST.offset_from_oldest();
    assert_eq!(
        ProtocolVersion::from_oldest_offset(max_oldest_offset + 1),
        None,
        "offsets past the supported span must be rejected",
    );

    assert_eq!(
        ProtocolVersion::from_newest_offset(ProtocolVersion::supported_protocol_numbers().len()),
        None,
        "offsets beyond the descending span must be rejected",
    );

    assert_eq!(
        ProtocolVersion::from_oldest_offset(usize::MAX),
        None,
        "large offsets should saturate to None without panicking",
    );
    assert_eq!(
        ProtocolVersion::from_newest_offset(usize::MAX),
        None,
        "large offsets relative to newest should also be rejected",
    );
}

#[test]
fn next_newer_walks_towards_newest_within_bounds() {
    let mut current = ProtocolVersion::OLDEST;
    let mut expected = ProtocolVersion::OLDEST.as_u8();

    while let Some(next) = current.next_newer() {
        expected += 1;
        assert_eq!(next.as_u8(), expected);
        current = next;
    }

    assert_eq!(current, ProtocolVersion::NEWEST);
    assert!(ProtocolVersion::NEWEST.next_newer().is_none());
}

#[test]
fn next_older_walks_towards_oldest_within_bounds() {
    let mut current = ProtocolVersion::NEWEST;
    let mut expected = ProtocolVersion::NEWEST.as_u8();

    while let Some(next) = current.next_older() {
        expected -= 1;
        assert_eq!(next.as_u8(), expected);
        current = next;
    }

    assert_eq!(current, ProtocolVersion::OLDEST);
    assert!(ProtocolVersion::OLDEST.next_older().is_none());
}

#[test]
fn supported_versions_iter_reports_length() {
    let mut iter = ProtocolVersion::supported_versions_iter();

    assert_eq!(iter.len(), SUPPORTED_PROTOCOL_COUNT);
    assert_eq!(
        iter.size_hint(),
        (SUPPORTED_PROTOCOL_COUNT, Some(SUPPORTED_PROTOCOL_COUNT))
    );

    assert_eq!(iter.next(), Some(ProtocolVersion::NEWEST));
    assert_eq!(iter.len(), SUPPORTED_PROTOCOL_COUNT - 1);
    assert_eq!(
        iter.size_hint(),
        (
            SUPPORTED_PROTOCOL_COUNT - 1,
            Some(SUPPORTED_PROTOCOL_COUNT - 1)
        )
    );
}

#[test]
fn supported_versions_iter_supports_double_ended_iteration() {
    let mut iter = ProtocolVersion::supported_versions_iter();

    assert_eq!(iter.next_back(), Some(ProtocolVersion::OLDEST));
    assert_eq!(iter.next(), Some(ProtocolVersion::NEWEST));
    assert_eq!(iter.len(), SUPPORTED_PROTOCOL_COUNT.saturating_sub(2));
    assert_eq!(
        iter.size_hint(),
        (
            SUPPORTED_PROTOCOL_COUNT.saturating_sub(2),
            Some(SUPPORTED_PROTOCOL_COUNT.saturating_sub(2)),
        )
    );
}

#[test]
fn supported_versions_iter_is_fused() {
    fn assert_fused<I: FusedIterator>(_iter: I) {}

    assert_fused(ProtocolVersion::supported_versions_iter());
}

#[test]
fn supported_versions_iter_supports_nth_and_nth_back() {
    let mut iter = ProtocolVersion::supported_versions_iter();

    let second =
        ProtocolVersion::from_supported_index(1).expect("second supported protocol should exist");
    assert_eq!(iter.nth(1), Some(second));

    let third =
        ProtocolVersion::from_supported_index(2).expect("third supported protocol should exist");
    assert_eq!(iter.next(), Some(third));

    let oldest = ProtocolVersion::from_supported_index(SUPPORTED_PROTOCOL_COUNT - 1)
        .expect("oldest supported protocol should exist");
    assert_eq!(iter.nth_back(0), Some(oldest));

    let penultimate = ProtocolVersion::from_supported_index(SUPPORTED_PROTOCOL_COUNT - 2)
        .expect("penultimate supported protocol should exist");
    assert_eq!(iter.next_back(), Some(penultimate));
}

#[test]
fn supported_protocol_numbers_iter_reports_length() {
    let mut iter = ProtocolVersion::supported_protocol_numbers_iter();

    assert_eq!(iter.len(), SUPPORTED_PROTOCOL_COUNT);
    assert_eq!(
        iter.size_hint(),
        (SUPPORTED_PROTOCOL_COUNT, Some(SUPPORTED_PROTOCOL_COUNT))
    );

    assert_eq!(iter.next(), Some(ProtocolVersion::NEWEST.as_u8()));
    assert_eq!(iter.len(), SUPPORTED_PROTOCOL_COUNT - 1);
    assert_eq!(
        iter.size_hint(),
        (
            SUPPORTED_PROTOCOL_COUNT - 1,
            Some(SUPPORTED_PROTOCOL_COUNT - 1)
        )
    );
}

#[test]
fn supported_protocol_numbers_iter_supports_double_ended_iteration() {
    let mut iter = ProtocolVersion::supported_protocol_numbers_iter();

    assert_eq!(iter.next_back(), Some(ProtocolVersion::OLDEST.as_u8()));
    assert_eq!(iter.next(), Some(ProtocolVersion::NEWEST.as_u8()));
    assert_eq!(iter.len(), SUPPORTED_PROTOCOL_COUNT.saturating_sub(2));
    assert_eq!(
        iter.size_hint(),
        (
            SUPPORTED_PROTOCOL_COUNT.saturating_sub(2),
            Some(SUPPORTED_PROTOCOL_COUNT.saturating_sub(2)),
        )
    );
}

#[test]
fn supported_protocol_numbers_iter_is_fused() {
    fn assert_fused<I: FusedIterator>(_iter: I) {}

    assert_fused(ProtocolVersion::supported_protocol_numbers_iter());
}

#[test]
fn supported_protocol_numbers_iter_supports_nth_and_nth_back() {
    let mut iter = ProtocolVersion::supported_protocol_numbers_iter();

    assert_eq!(iter.nth(1), Some(SUPPORTED_PROTOCOLS[1]));
    assert_eq!(iter.next(), Some(SUPPORTED_PROTOCOLS[2]));

    assert_eq!(
        iter.nth_back(0),
        Some(SUPPORTED_PROTOCOLS[SUPPORTED_PROTOCOL_COUNT - 1])
    );

    assert_eq!(
        iter.next_back(),
        Some(SUPPORTED_PROTOCOLS[SUPPORTED_PROTOCOL_COUNT - 2])
    );
}

#[test]
fn supported_range_matches_upstream_bounds() {
    assert_eq!(ProtocolVersion::supported_range(), SUPPORTED_PROTOCOL_RANGE);
    assert_eq!(SUPPORTED_PROTOCOL_RANGE, UPSTREAM_PROTOCOL_RANGE);
}

#[test]
fn supported_range_bounds_match_upstream_constants() {
    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
    assert_eq!(oldest, ProtocolVersion::OLDEST.as_u8());
    assert_eq!(newest, ProtocolVersion::NEWEST.as_u8());
}

#[test]
fn supported_version_bounds_match_constants() {
    let (oldest, newest) = ProtocolVersion::supported_version_bounds();
    assert_eq!(oldest, ProtocolVersion::OLDEST);
    assert_eq!(newest, ProtocolVersion::NEWEST);
}

#[test]
fn supported_protocol_range_constant_matches_bounds() {
    assert_eq!(
        *SUPPORTED_PROTOCOL_RANGE.start(),
        ProtocolVersion::OLDEST.as_u8()
    );
    assert_eq!(
        *SUPPORTED_PROTOCOL_RANGE.end(),
        ProtocolVersion::NEWEST.as_u8()
    );
}

#[test]
fn supported_protocol_bounds_constant_matches_helpers() {
    assert_eq!(
        SUPPORTED_PROTOCOL_BOUNDS,
        (
            ProtocolVersion::OLDEST.as_u8(),
            ProtocolVersion::NEWEST.as_u8()
        )
    );
    assert_eq!(
        SUPPORTED_PROTOCOL_BOUNDS,
        ProtocolVersion::supported_range_bounds()
    );
}

#[test]
fn supported_version_range_matches_bounds() {
    let range = ProtocolVersion::supported_version_range();
    assert_eq!(range.start(), &ProtocolVersion::OLDEST);
    assert_eq!(range.end(), &ProtocolVersion::NEWEST);
}

#[test]
fn detects_supported_versions() {
    for version in SUPPORTED_PROTOCOLS {
        assert!(ProtocolVersion::is_supported(version));
    }
}

#[test]
fn rejects_unsupported_versions_in_helper() {
    assert!(!ProtocolVersion::is_supported(0));
    assert!(!ProtocolVersion::is_supported(27));
    assert!(!ProtocolVersion::is_supported(33));
}

#[test]
fn rejects_out_of_range_non_zero_u8() {
    let value = NonZeroU8::new(27).expect("non-zero");
    let err = ProtocolVersion::try_from(value).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(27));
}

#[test]
fn from_supported_accepts_values_within_range() {
    let newest = ProtocolVersion::from_supported(ProtocolVersion::NEWEST.as_u8());
    assert_eq!(newest, Some(ProtocolVersion::NEWEST));

    let oldest = ProtocolVersion::from_supported(ProtocolVersion::OLDEST.as_u8());
    assert_eq!(oldest, Some(ProtocolVersion::OLDEST));
}

#[test]
fn from_supported_rejects_values_outside_range() {
    assert_eq!(ProtocolVersion::from_supported(0), None);
    assert_eq!(ProtocolVersion::from_supported(27), None);
    assert_eq!(ProtocolVersion::from_supported(33), None);
}

#[test]
fn from_supported_matches_supported_protocol_list() {
    let supported = ProtocolVersion::supported_protocol_numbers();

    for value in 0..=u8::MAX {
        let parsed = ProtocolVersion::from_supported(value);
        let expected = supported.contains(&value);

        assert_eq!(
            parsed.is_some(),
            expected,
            "value {value} should match membership"
        );

        if let Some(version) = parsed {
            assert_eq!(
                version.as_u8(),
                value,
                "returned version must preserve numeric value"
            );
        }
    }
}

#[test]
fn converts_from_non_zero_u8() {
    let value = NonZeroU8::new(31).expect("non-zero");
    let version = ProtocolVersion::try_from(value).expect("valid");
    assert_eq!(version.as_u8(), 31);
}

#[test]
fn select_highest_mutual_accepts_only_future_versions() {
    let negotiated = select_highest_mutual([40]).expect("future-only handshake clamps");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn select_highest_mutual_prefers_newest_when_future_and_too_old_mix() {
    let negotiated = select_highest_mutual([0, 40])
        .expect("unsupported low versions must not mask clamped future ones");
    assert_eq!(negotiated, ProtocolVersion::NEWEST);
}

#[test]
fn converts_protocol_version_to_non_zero_u8() {
    let value = NonZeroU8::new(28).expect("non-zero");
    let version = ProtocolVersion::try_from(value).expect("valid");
    let round_trip: NonZeroU8 = version.into();
    assert_eq!(round_trip, value);
}

#[test]
fn exposes_non_zero_protocol_byte() {
    let version = ProtocolVersion::try_from(30).expect("valid");
    let non_zero = version.as_non_zero();
    assert_eq!(non_zero.get(), 30);

    let via_from: NonZeroU8 = version.into();
    assert_eq!(via_from, non_zero);
}

#[test]
fn compares_protocol_version_with_non_zero_u8() {
    let newest = NonZeroU8::new(ProtocolVersion::NEWEST.as_u8()).expect("non-zero");
    assert_eq!(ProtocolVersion::NEWEST, newest);
    assert_eq!(newest, ProtocolVersion::NEWEST);

    let oldest = NonZeroU8::new(ProtocolVersion::OLDEST.as_u8()).expect("non-zero");
    assert_ne!(ProtocolVersion::NEWEST, oldest);
    assert_ne!(oldest, ProtocolVersion::NEWEST);
}

#[test]
fn converts_protocol_version_to_u8() {
    let version = ProtocolVersion::try_from(32).expect("valid");
    let value: u8 = version.into();
    assert_eq!(value, 32);
}

#[test]
fn converts_protocol_version_to_signed_integers() {
    let newest = ProtocolVersion::NEWEST;
    let oldest = ProtocolVersion::OLDEST;

    let newest_i8: i8 = newest.into();
    assert_eq!(newest_i8, i8::try_from(newest.as_u8()).expect("fits in i8"));

    let newest_i16: i16 = newest.into();
    assert_eq!(newest_i16, i16::from(newest.as_u8()));

    let oldest_i32: i32 = oldest.into();
    assert_eq!(oldest_i32, i32::from(oldest.as_u8()));

    let newest_i64: i64 = newest.into();
    assert_eq!(newest_i64, i64::from(newest.as_u8()));

    let oldest_i128: i128 = oldest.into();
    assert_eq!(oldest_i128, i128::from(oldest.as_u8()));

    let newest_isize: isize = newest.into();
    assert_eq!(newest_isize, isize::from(newest.as_u8()));
}

#[test]
fn converts_protocol_version_to_non_zero_signed_integers() {
    let newest = ProtocolVersion::NEWEST;
    let oldest = ProtocolVersion::OLDEST;

    let newest_i8: NonZeroI8 = newest.into();
    assert_eq!(
        newest_i8.get(),
        i8::try_from(newest.as_u8()).expect("fits in i8")
    );

    let newest_i16: NonZeroI16 = newest.into();
    assert_eq!(newest_i16.get(), i16::from(newest.as_u8()));

    let oldest_i32: NonZeroI32 = oldest.into();
    assert_eq!(oldest_i32.get(), i32::from(oldest.as_u8()));

    let newest_i64: NonZeroI64 = newest.into();
    assert_eq!(newest_i64.get(), i64::from(newest.as_u8()));

    let oldest_i128: NonZeroI128 = oldest.into();
    assert_eq!(oldest_i128.get(), i128::from(oldest.as_u8()));

    let newest_isize: NonZeroIsize = newest.into();
    assert_eq!(newest_isize.get(), isize::from(newest.as_u8()));
}

#[test]
fn compares_directly_with_u8() {
    let version = ProtocolVersion::try_from(30).expect("valid");
    assert_eq!(version, 30);
    assert_eq!(30, version);
}

#[test]
fn compares_directly_with_non_zero_u8() {
    let version = ProtocolVersion::try_from(31).expect("valid");
    let non_zero = NonZeroU8::new(31).expect("non-zero");

    assert_eq!(version, non_zero);
    assert_eq!(non_zero, version);

    let different = NonZeroU8::new(ProtocolVersion::OLDEST.as_u8()).expect("non-zero");
    assert_ne!(version, different);
    assert_ne!(different, version);
}

#[test]
fn supported_versions_are_sorted_descending() {
    let mut sorted = SUPPORTED_PROTOCOLS;
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(sorted, SUPPORTED_PROTOCOLS);
}

#[test]
fn protocol_version_display_matches_numeric_value() {
    let version = ProtocolVersion::try_from(32).expect("valid");
    assert_eq!(version.to_string(), "32");
}

#[test]
fn protocol_version_from_str_accepts_supported_values() {
    assert_eq!(
        ProtocolVersion::from_str("32").expect("32 is supported"),
        ProtocolVersion::NEWEST
    );
    assert_eq!(
        ProtocolVersion::from_str(" 29 ")
            .expect("whitespace should be ignored")
            .as_u8(),
        29
    );
    assert_eq!(
        ProtocolVersion::from_str("+30")
            .expect("leading plus is accepted")
            .as_u8(),
        30
    );
}

#[test]
fn protocol_version_from_str_reports_error_kinds() {
    let empty = ProtocolVersion::from_str("").unwrap_err();
    assert_eq!(empty.kind(), ParseProtocolVersionErrorKind::Empty);

    let invalid = ProtocolVersion::from_str("abc").unwrap_err();
    assert_eq!(invalid.kind(), ParseProtocolVersionErrorKind::InvalidDigit);

    let double_sign = ProtocolVersion::from_str("+-31").unwrap_err();
    assert_eq!(
        double_sign.kind(),
        ParseProtocolVersionErrorKind::InvalidDigit
    );

    let negative = ProtocolVersion::from_str("-31").unwrap_err();
    assert_eq!(negative.kind(), ParseProtocolVersionErrorKind::Negative);

    let overflow = ProtocolVersion::from_str("256").unwrap_err();
    assert_eq!(overflow.kind(), ParseProtocolVersionErrorKind::Overflow);

    let unsupported = ProtocolVersion::from_str("27").unwrap_err();
    assert_eq!(
        unsupported.kind(),
        ParseProtocolVersionErrorKind::UnsupportedRange(27)
    );
    assert_eq!(unsupported.unsupported_value(), Some(27));
}

#[test]
fn parse_protocol_version_error_display_matches_variants() {
    let empty = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Empty);
    assert_eq!(empty.to_string(), "protocol version string is empty");

    let invalid = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::InvalidDigit);
    assert_eq!(
        invalid.to_string(),
        "protocol version must be an unsigned integer"
    );

    let negative = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Negative);
    assert_eq!(negative.to_string(), "protocol version cannot be negative");

    let overflow = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Overflow);
    assert_eq!(
        overflow.to_string(),
        "protocol version value exceeds u8::MAX"
    );

    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
    let unsupported =
        ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::UnsupportedRange(27));
    assert_eq!(
        unsupported.to_string(),
        format!("protocol version 27 is outside the supported range {oldest}-{newest}")
    );
}

#[test]
fn protocol_versions_are_hashable() {
    use std::collections::HashSet;

    let mut set = HashSet::new();
    assert!(set.insert(ProtocolVersion::NEWEST));
    assert!(set.contains(&ProtocolVersion::NEWEST));
    assert!(!set.insert(ProtocolVersion::NEWEST));
}

#[test]
fn binary_negotiation_threshold_matches_protocol_30() {
    let expected = ProtocolVersion::from_supported(30).expect("protocol 30 is supported");
    assert_eq!(ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED, expected);
    assert!(ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.uses_binary_negotiation());
    assert!(!ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.uses_legacy_ascii_negotiation());
}

#[test]
fn binary_negotiation_threshold_exceeds_oldest_supported_version() {
    let threshold = ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.as_u8();
    let oldest = ProtocolVersion::OLDEST.as_u8();

    assert!(
        threshold > oldest,
        "binary negotiation threshold must be newer than the oldest supported protocol",
    );

    let preceding = ProtocolVersion::from_supported(threshold - 1)
        .expect("protocol immediately preceding the binary threshold is supported");
    assert!(preceding.uses_legacy_ascii_negotiation());
}

#[test]
fn negotiation_style_helpers_match_protocol_cutoff() {
    for version in ProtocolVersion::supported_versions() {
        if version.as_u8() >= ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.as_u8() {
            assert!(
                version.uses_binary_negotiation(),
                "version {} should be binary",
                version
            );
            assert!(
                !version.uses_legacy_ascii_negotiation(),
                "version {} should not be legacy",
                version
            );
        } else {
            assert!(
                !version.uses_binary_negotiation(),
                "version {} should not be binary",
                version
            );
            assert!(
                version.uses_legacy_ascii_negotiation(),
                "version {} should be legacy",
                version
            );
        }
    }
}

proptest! {
    #[test]
    fn select_highest_mutual_matches_reference(peer_versions in proptest::collection::vec(0u8..=255, 0..=16)) {
        let expected = reference_negotiation(&peer_versions);
        let actual = select_highest_mutual(peer_versions.iter().copied());
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_wider_unsigned(
        peer_versions in proptest::collection::vec(any::<u16>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.clone());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_widest_unsigned(
        peer_versions in proptest::collection::vec(any::<u128>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.iter().copied());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_native_usize(
        peer_versions in proptest::collection::vec(any::<usize>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.iter().copied());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_signed(
        peer_versions in proptest::collection::vec(any::<i16>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.clone());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_widest_signed(
        peer_versions in proptest::collection::vec(any::<i128>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.iter().copied());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_native_isize(
        peer_versions in proptest::collection::vec(any::<isize>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.iter().copied());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }
}
