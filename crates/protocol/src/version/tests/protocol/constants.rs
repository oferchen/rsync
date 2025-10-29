use super::super::{
    ProtocolVersion, SUPPORTED_PROTOCOL_BITMAP, SUPPORTED_PROTOCOL_BOUNDS,
    SUPPORTED_PROTOCOL_COUNT, SUPPORTED_PROTOCOL_RANGE, SUPPORTED_PROTOCOLS,
    UPSTREAM_PROTOCOL_RANGE,
};

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
