use super::*;
use core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize, Wrapping,
};
use proptest::prelude::*;

fn reference_negotiation(peer_versions: &[u8]) -> Result<ProtocolVersion, NegotiationError> {
    use std::collections::BTreeSet;

    let mut recognized = BTreeSet::new();
    let mut seen_any = false;
    let mut seen_max = ProtocolVersion::OLDEST.as_u8();
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
        if recognized.insert(clamped) {
            seen_any = true;
            if clamped > seen_max {
                seen_max = clamped;
            }
        }
    }

    if let Some(&newest) = recognized.iter().next_back() {
        return Ok(ProtocolVersion::new_const(newest));
    }

    if let Some(rejected) = oldest_rejection {
        return Err(NegotiationError::UnsupportedVersion(rejected));
    }

    let peer_versions = if seen_any {
        let start = ProtocolVersion::OLDEST.as_u8();
        let span = usize::from(seen_max.saturating_sub(start)) + 1;
        let mut versions = Vec::with_capacity(span);
        for version in start..=seen_max {
            if recognized.contains(&version) {
                versions.push(version);
            }
        }
        versions
    } else {
        Vec::new()
    };

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
fn protocol_versions_are_hashable() {
    use std::collections::HashSet;

    let mut set = HashSet::new();
    assert!(set.insert(ProtocolVersion::NEWEST));
    assert!(set.contains(&ProtocolVersion::NEWEST));
    assert!(!set.insert(ProtocolVersion::NEWEST));
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
    fn select_highest_mutual_matches_reference_for_signed(
        peer_versions in proptest::collection::vec(any::<i16>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.clone());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }
}
