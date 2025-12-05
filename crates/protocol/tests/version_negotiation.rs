//! Comprehensive protocol version negotiation validation tests.
//!
//! Validates wire-level protocol correctness across versions 28-32 with
//! exhaustive testing of negotiation rules, boundary conditions, and
//! version selection logic against upstream rsync semantics.

use protocol::{
    NegotiationError, ProtocolVersion, ProtocolVersionAdvertisement, select_highest_mutual,
};

/// Wrapper for testing negotiation with arbitrary advertised versions.
#[derive(Clone, Copy, Debug)]
struct AdvertisedVersion(u32);

impl ProtocolVersionAdvertisement for AdvertisedVersion {
    #[inline]
    fn into_advertised_version(self) -> u32 {
        self.0
    }
}

/// Test all 25 combinations of peer-advertised protocols 28-32.
///
/// This exhaustive matrix ensures that negotiation logic correctly selects
/// the highest mutual version when the peer advertises any combination of
/// supported protocol versions.
#[test]
fn test_mutual_version_selection_exhaustive() {
    const SUPPORTED: [u8; 5] = [28, 29, 30, 31, 32];

    // Test each single version advertisement
    for &peer_version in &SUPPORTED {
        let peer_versions = [AdvertisedVersion(u32::from(peer_version))];
        let negotiated = select_highest_mutual(peer_versions)
            .expect("negotiation with single supported version must succeed");

        assert_eq!(
            negotiated.as_u8(),
            peer_version,
            "negotiation with peer advertising {peer_version} must select {peer_version}"
        );
    }

    // Test pairwise combinations
    for (i, &version_a) in SUPPORTED.iter().enumerate() {
        for &version_b in &SUPPORTED[i..] {
            let peer_versions = [
                AdvertisedVersion(u32::from(version_a)),
                AdvertisedVersion(u32::from(version_b)),
            ];
            let negotiated = select_highest_mutual(peer_versions)
                .expect("negotiation with two versions must succeed");

            let expected = std::cmp::max(version_a, version_b);
            assert_eq!(
                negotiated.as_u8(),
                expected,
                "negotiation with peer advertising [{version_a}, {version_b}] must select {expected}"
            );
        }
    }
}

/// Test all pairwise combinations where one peer supports multiple versions.
///
/// Validates that the negotiation algorithm correctly identifies the highest
/// common version when one or both peers advertise multiple protocol versions.
#[test]
fn test_multiple_version_advertisements() {
    const SUPPORTED: [u8; 5] = [28, 29, 30, 31, 32];

    for i in 0..SUPPORTED.len() {
        for j in i..SUPPORTED.len() {
            let range_start = SUPPORTED[i];
            let range_end = SUPPORTED[j];

            // Build advertisement list for versions from range_start to range_end
            let mut advertisements = Vec::new();
            for &version in &SUPPORTED[i..=j] {
                advertisements.push(AdvertisedVersion(u32::from(version)));
            }

            let negotiated = select_highest_mutual(advertisements.clone())
                .expect("negotiation must succeed within supported range");

            assert_eq!(
                negotiated.as_u8(),
                range_end,
                "negotiation of range [{range_start}..={range_end}] must select highest ({range_end})"
            );
        }
    }
}

/// Test that protocol 30 is correctly recognized as the binary negotiation boundary.
///
/// Protocol 30 introduced the binary handshake. This test verifies that:
/// - Protocol 30 and above use binary negotiation
/// - Protocol 29 and below use legacy ASCII negotiation
#[test]
fn test_protocol_30_boundary() {
    // Protocol 30 and above use binary negotiation
    assert!(
        ProtocolVersion::V30.uses_binary_negotiation(),
        "protocol 30 must use binary negotiation"
    );
    assert!(
        ProtocolVersion::V31.uses_binary_negotiation(),
        "protocol 31 must use binary negotiation"
    );
    assert!(
        ProtocolVersion::V32.uses_binary_negotiation(),
        "protocol 32 must use binary negotiation"
    );

    // Protocol 29 and below use legacy ASCII negotiation
    assert!(
        !ProtocolVersion::V29.uses_binary_negotiation(),
        "protocol 29 must not use binary negotiation"
    );
    assert!(
        ProtocolVersion::V29.uses_legacy_ascii_negotiation(),
        "protocol 29 must use legacy ASCII negotiation"
    );
    assert!(
        !ProtocolVersion::V28.uses_binary_negotiation(),
        "protocol 28 must not use binary negotiation"
    );
    assert!(
        ProtocolVersion::V28.uses_legacy_ascii_negotiation(),
        "protocol 28 must use legacy ASCII negotiation"
    );
}

/// Test that binary negotiation boundary is consistent across all methods.
#[test]
fn test_binary_negotiation_boundary_consistency() {
    assert_eq!(
        ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED,
        ProtocolVersion::V30,
        "binary negotiation must be introduced at protocol 30"
    );

    // Verify the boundary is exactly at protocol 30
    assert!(
        ProtocolVersion::V30.uses_binary_negotiation(),
        "protocol 30 is the first to use binary negotiation"
    );
    assert!(
        ProtocolVersion::V29.uses_legacy_ascii_negotiation(),
        "protocol 29 is the last to use ASCII negotiation"
    );

    // Ensure mutual exclusivity
    for version in ProtocolVersion::supported_versions_iter() {
        let uses_binary = version.uses_binary_negotiation();
        let uses_ascii = version.uses_legacy_ascii_negotiation();
        assert_ne!(
            uses_binary, uses_ascii,
            "protocol {version} must use exactly one negotiation method"
        );
    }
}

/// Test that future version advertisements are clamped to the newest supported version.
///
/// Upstream rsync clamps future protocol versions (within MAX_PROTOCOL_VERSION=40)
/// down to the newest supported version (32) to maintain forward compatibility.
#[test]
fn test_future_version_clamping() {
    // Future version 35 should clamp to 32
    let result = select_highest_mutual([AdvertisedVersion(35)])
        .expect("future version must clamp to newest supported");
    assert_eq!(
        result,
        ProtocolVersion::V32,
        "version 35 must clamp to protocol 32"
    );

    // Version at the maximum advertisement threshold (40) should clamp to 32
    let result = select_highest_mutual([AdvertisedVersion(40)])
        .expect("maximum advertisement version must clamp to newest supported");
    assert_eq!(
        result,
        ProtocolVersion::V32,
        "version 40 must clamp to protocol 32"
    );

    // Test clamping with mixed future and supported versions
    let result = select_highest_mutual([
        AdvertisedVersion(35),
        AdvertisedVersion(31),
        AdvertisedVersion(29),
    ])
    .expect("mixed future and supported versions must negotiate");
    assert_eq!(
        result,
        ProtocolVersion::V32,
        "mixed versions including 35 must clamp to 32"
    );

    // Versions above MAX_PROTOCOL_VERSION (40) are rejected, not clamped
    let result = select_highest_mutual([AdvertisedVersion(100)]);
    assert!(
        result.is_err(),
        "versions above MAX_PROTOCOL_VERSION must be rejected"
    );
}

/// Test that version 0 is correctly rejected as reserved.
#[test]
fn test_version_zero_rejected() {
    let result = select_highest_mutual([AdvertisedVersion(0)]);
    assert!(
        result.is_err(),
        "protocol version 0 must be rejected as reserved"
    );

    match result {
        Err(NegotiationError::UnsupportedVersion(0)) => {
            // Expected error
        }
        other => panic!("unexpected result for version 0: {other:?}"),
    }
}

/// Test that versions below the oldest supported (28) are rejected.
#[test]
fn test_ancient_versions_rejected() {
    for version in 1..28 {
        let result = select_highest_mutual([AdvertisedVersion(version)]);
        assert!(
            result.is_err(),
            "protocol version {version} below oldest (28) must be rejected"
        );

        match result {
            Err(NegotiationError::UnsupportedVersion(v)) => {
                assert_eq!(v, version, "error must report the unsupported version");
            }
            other => panic!("unexpected result for version {version}: {other:?}"),
        }
    }
}

/// Test negotiation with empty peer version list.
#[test]
fn test_empty_version_list() {
    let result = select_highest_mutual::<Vec<AdvertisedVersion>, _>(vec![]);
    assert!(
        result.is_err(),
        "negotiation with empty version list must fail"
    );

    match result {
        Err(NegotiationError::NoMutualProtocol { peer_versions }) => {
            assert!(
                peer_versions.is_empty(),
                "no mutual protocol error must report empty peer list"
            );
        }
        other => panic!("unexpected result for empty list: {other:?}"),
    }
}

/// Test negotiation when peer only advertises unsupported versions.
#[test]
fn test_no_mutual_protocol() {
    // Peer advertises only ancient versions
    let result = select_highest_mutual([AdvertisedVersion(20), AdvertisedVersion(25)]);
    assert!(
        result.is_err(),
        "no mutual protocol when peer only advertises unsupported versions"
    );

    match result {
        Err(NegotiationError::UnsupportedVersion(v)) => {
            assert!(
                v < 28,
                "error must report an unsupported version below oldest supported"
            );
        }
        other => panic!("unexpected result for ancient versions: {other:?}"),
    }
}

/// Test that negotiation selects highest when peer advertises multiple versions.
#[test]
fn test_selects_highest_from_multiple_peer_versions() {
    let result = select_highest_mutual([
        AdvertisedVersion(28),
        AdvertisedVersion(30),
        AdvertisedVersion(31),
        AdvertisedVersion(29),
    ])
    .expect("negotiation with multiple versions must succeed");

    assert_eq!(
        result,
        ProtocolVersion::V31,
        "must select highest mutual version (31) from unordered peer list"
    );
}

/// Test that duplicate version advertisements don't affect negotiation.
#[test]
fn test_duplicate_versions_handled_correctly() {
    let result = select_highest_mutual([
        AdvertisedVersion(30),
        AdvertisedVersion(30),
        AdvertisedVersion(30),
        AdvertisedVersion(29),
    ])
    .expect("duplicates must not prevent negotiation");

    assert_eq!(
        result,
        ProtocolVersion::V30,
        "duplicates must not affect highest version selection"
    );
}

/// Test negotiation with only the oldest supported version.
#[test]
fn test_negotiation_with_oldest_only() {
    let result = select_highest_mutual([AdvertisedVersion(28)])
        .expect("negotiation with oldest version must succeed");

    assert_eq!(
        result,
        ProtocolVersion::V28,
        "must successfully negotiate protocol 28"
    );
}

/// Test negotiation with only the newest supported version.
#[test]
fn test_negotiation_with_newest_only() {
    let result = select_highest_mutual([AdvertisedVersion(32)])
        .expect("negotiation with newest version must succeed");

    assert_eq!(
        result,
        ProtocolVersion::V32,
        "must successfully negotiate protocol 32"
    );
}

/// Test that negotiation short-circuits when newest version is found.
///
/// This test verifies the optimization where negotiation returns immediately
/// upon finding protocol 32 in the peer's advertisement list.
#[test]
fn test_short_circuit_on_newest_version() {
    // Even with older versions after newest, should return 32
    let result = select_highest_mutual([
        AdvertisedVersion(32),
        AdvertisedVersion(31),
        AdvertisedVersion(30),
    ])
    .expect("negotiation must succeed");

    assert_eq!(
        result,
        ProtocolVersion::V32,
        "must short-circuit and return protocol 32"
    );

    // Newest version in middle of list
    let result = select_highest_mutual([
        AdvertisedVersion(29),
        AdvertisedVersion(32),
        AdvertisedVersion(28),
    ])
    .expect("negotiation must succeed");

    assert_eq!(
        result,
        ProtocolVersion::V32,
        "must find and return protocol 32 regardless of position"
    );
}

/// Test protocol version ordering and bounds.
#[test]
fn test_protocol_version_ordering() {
    assert!(ProtocolVersion::V28 < ProtocolVersion::V29);
    assert!(ProtocolVersion::V29 < ProtocolVersion::V30);
    assert!(ProtocolVersion::V30 < ProtocolVersion::V31);
    assert!(ProtocolVersion::V31 < ProtocolVersion::V32);

    assert_eq!(
        ProtocolVersion::OLDEST,
        ProtocolVersion::V28,
        "V28 must be the oldest supported version"
    );
    assert_eq!(
        ProtocolVersion::NEWEST,
        ProtocolVersion::V32,
        "V32 must be the newest supported version"
    );
}

/// Test that all protocol version constants are properly defined and consistent.
#[test]
fn test_protocol_version_constants_consistency() {
    assert_eq!(ProtocolVersion::V32.as_u8(), 32);
    assert_eq!(ProtocolVersion::V31.as_u8(), 31);
    assert_eq!(ProtocolVersion::V30.as_u8(), 30);
    assert_eq!(ProtocolVersion::V29.as_u8(), 29);
    assert_eq!(ProtocolVersion::V28.as_u8(), 28);

    // Verify supported versions array matches constants
    let supported = ProtocolVersion::supported_versions();
    assert_eq!(
        supported.len(),
        5,
        "must support exactly 5 protocol versions"
    );
    assert_eq!(supported[0], ProtocolVersion::V32);
    assert_eq!(supported[1], ProtocolVersion::V31);
    assert_eq!(supported[2], ProtocolVersion::V30);
    assert_eq!(supported[3], ProtocolVersion::V29);
    assert_eq!(supported[4], ProtocolVersion::V28);
}

/// Test that protocol version range helpers work correctly.
#[test]
fn test_protocol_version_range_helpers() {
    let range = ProtocolVersion::supported_range();
    assert_eq!(*range.start(), 28);
    assert_eq!(*range.end(), 32);

    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
    assert_eq!(oldest, 28);
    assert_eq!(newest, 32);

    let (oldest_ver, newest_ver) = ProtocolVersion::supported_version_bounds();
    assert_eq!(oldest_ver, ProtocolVersion::V28);
    assert_eq!(newest_ver, ProtocolVersion::V32);
}

/// Test that supported protocol bitmap is correctly constructed.
#[test]
fn test_supported_protocol_bitmap() {
    let bitmap = ProtocolVersion::supported_protocol_bitmap();

    // Verify each supported version has its bit set
    for &version in ProtocolVersion::supported_protocol_numbers() {
        let mask = 1u64 << version;
        assert_ne!(
            bitmap & mask,
            0,
            "bit for protocol {version} must be set in bitmap"
        );
    }

    // Verify unsupported versions don't have their bits set
    for version in 0..28 {
        let mask = 1u64 << version;
        assert_eq!(
            bitmap & mask,
            0,
            "bit for unsupported protocol {version} must not be set"
        );
    }

    // Verify no bits above newest supported version
    let upper_shift = usize::from(ProtocolVersion::NEWEST.as_u8()) + 1;
    assert_eq!(
        bitmap >> upper_shift,
        0,
        "no bits above newest supported version must be set"
    );
}

/// Test that protocol version lookup by index works correctly.
#[test]
fn test_protocol_version_index_lookup() {
    for (index, &version) in ProtocolVersion::supported_versions().iter().enumerate() {
        let looked_up = ProtocolVersion::from_supported_index(index)
            .expect("lookup by index must succeed for valid indices");
        assert_eq!(
            looked_up, version,
            "index {index} must map to protocol {version}"
        );
    }

    // Test out-of-bounds index
    assert!(
        ProtocolVersion::from_supported_index(5).is_none(),
        "out-of-bounds index must return None"
    );
    assert!(
        ProtocolVersion::from_supported_index(100).is_none(),
        "large out-of-bounds index must return None"
    );
}
