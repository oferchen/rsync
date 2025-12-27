//! Helpers for selecting a mutual protocol version with a peer.

use super::advertisement::ProtocolVersionAdvertisement;
use super::constants::UPSTREAM_PROTOCOL_RANGE;
use super::recognized::RecognizedVersions;
use super::{ProtocolVersion, SUPPORTED_PROTOCOL_BITMAP, SUPPORTED_PROTOCOL_COUNT};
use crate::error::NegotiationError;

/// Selects the highest mutual protocol version between the Rust implementation and a peer.
#[must_use = "the negotiation outcome must be checked"]
pub fn select_highest_mutual<I, T>(peer_versions: I) -> Result<ProtocolVersion, NegotiationError>
where
    I: IntoIterator<Item = T>,
    T: ProtocolVersionAdvertisement,
{
    let mut supported_bitmap: u64 = 0;
    let mut recognized_versions = RecognizedVersions::new();
    let mut oldest_rejection: Option<u32> = None;

    for version in peer_versions {
        let advertised = version.into_advertised_version();

        match ProtocolVersion::from_peer_advertisement(advertised) {
            Ok(proto) => {
                let value = proto.as_u8();
                if value as u32 >= u64::BITS {
                    continue;
                }

                recognized_versions.insert(value);

                let bit = 1u64 << value;
                if SUPPORTED_PROTOCOL_BITMAP & bit != 0 {
                    supported_bitmap |= bit;

                    if value == ProtocolVersion::NEWEST.as_u8() {
                        return Ok(ProtocolVersion::NEWEST);
                    }
                }
            }
            Err(NegotiationError::UnsupportedVersion(value))
                if value < u32::from(ProtocolVersion::OLDEST.as_u8()) =>
            {
                if oldest_rejection.is_none_or(|current| value < current) {
                    oldest_rejection = Some(value);
                }
            }
            Err(err) => return Err(err),
        }
    }

    if supported_bitmap != 0 {
        let highest_bit = (u64::BITS - 1) - supported_bitmap.leading_zeros();
        debug_assert!(highest_bit < u64::BITS);

        let highest = highest_bit as u8;
        debug_assert!(ProtocolVersion::is_supported_protocol_number(highest));

        return Ok(ProtocolVersion::new_const(highest));
    }

    if let Some(value) = oldest_rejection {
        return Err(NegotiationError::UnsupportedVersion(value));
    }

    Err(NegotiationError::NoMutualProtocol {
        peer_versions: recognized_versions.into_vec(),
    })
}

// Evaluate the validation routine at compile time to guard against drift between
// the advertised protocol list and the supporting constants.
const _: () = {
    let protocols = super::SUPPORTED_PROTOCOLS;
    let Some(&declared_newest) = protocols.first() else {
        panic!("supported protocol list must not be empty");
    };
    assert!(
        protocols.len() == SUPPORTED_PROTOCOL_COUNT,
        "supported protocol count must match list length",
    );
    assert!(
        declared_newest == ProtocolVersion::NEWEST.as_u8(),
        "newest supported protocol must lead the list",
    );
    let declared_oldest = protocols[protocols.len() - 1];
    assert!(
        declared_oldest == ProtocolVersion::OLDEST.as_u8(),
        "oldest supported protocol must terminate the list",
    );

    let newest = ProtocolVersion::NEWEST.as_u8() as u32;
    assert!(
        newest < u64::BITS,
        "supported protocol bitmap must accommodate newest protocol",
    );

    let mut index = 1usize;
    while index < SUPPORTED_PROTOCOL_COUNT {
        assert!(
            protocols[index - 1] > protocols[index],
            "supported protocols must be strictly descending",
        );
        assert!(
            ProtocolVersion::OLDEST.as_u8() <= protocols[index]
                && protocols[index] <= ProtocolVersion::NEWEST.as_u8(),
            "each supported protocol must fall within the upstream range",
        );
        index += 1;
    }

    let versions = ProtocolVersion::SUPPORTED_VERSIONS;
    assert!(
        versions.len() == SUPPORTED_PROTOCOL_COUNT,
        "cached ProtocolVersion list must mirror numeric protocols",
    );

    let mut v_index = 0usize;
    while v_index < versions.len() {
        assert!(
            versions[v_index].as_u8() == protocols[v_index],
            "cached ProtocolVersion must match numeric protocol at each index",
        );
        v_index += 1;
    }

    let mut bitmap = 0u64;
    index = 0usize;
    while index < SUPPORTED_PROTOCOL_COUNT {
        bitmap |= 1u64 << protocols[index];
        index += 1;
    }
    assert!(
        bitmap == super::SUPPORTED_PROTOCOL_BITMAP,
        "supported protocol bitmap must mirror numeric protocol list",
    );
    assert!(
        super::SUPPORTED_PROTOCOL_BITMAP.count_ones() as usize == SUPPORTED_PROTOCOL_COUNT,
        "supported protocol bitmap must contain one bit per protocol version",
    );
    assert!(
        super::SUPPORTED_PROTOCOL_BITMAP >> (ProtocolVersion::NEWEST.as_u8() as usize + 1) == 0,
        "supported protocol bitmap must not include bits above the newest supported version",
    );
    assert!(
        super::SUPPORTED_PROTOCOL_BITMAP & ((1u64 << ProtocolVersion::OLDEST.as_u8()) - 1) == 0,
        "supported protocol bitmap must not include bits below the oldest supported version",
    );

    let range_oldest = *super::SUPPORTED_PROTOCOL_RANGE.start();
    let range_newest = *super::SUPPORTED_PROTOCOL_RANGE.end();
    assert!(
        range_oldest == ProtocolVersion::OLDEST.as_u8(),
        "supported protocol range must begin at the oldest supported version",
    );
    assert!(
        range_newest == ProtocolVersion::NEWEST.as_u8(),
        "supported protocol range must end at the newest supported version",
    );

    let (bounds_oldest, bounds_newest) = super::SUPPORTED_PROTOCOL_BOUNDS;
    assert!(
        bounds_oldest == range_oldest,
        "supported protocol bounds tuple must begin at the oldest supported version",
    );
    assert!(
        bounds_newest == range_newest,
        "supported protocol bounds tuple must end at the newest supported version",
    );

    let binary_intro = ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.as_u8();
    assert!(
        ProtocolVersion::OLDEST.as_u8() <= binary_intro
            && binary_intro <= ProtocolVersion::NEWEST.as_u8(),
        "binary negotiation threshold must fall within the supported range",
    );
    assert!(
        ProtocolVersion::BINARY_NEGOTIATION_INTRODUCED.uses_binary_negotiation(),
        "binary negotiation threshold must classify as binary",
    );
    assert!(
        binary_intro > ProtocolVersion::OLDEST.as_u8(),
        "binary negotiation threshold must exceed oldest supported version",
    );
    assert!(
        ProtocolVersion::new_const(binary_intro - 1).uses_legacy_ascii_negotiation(),
        "protocol immediately preceding binary threshold must use legacy negotiation",
    );

    let upstream_oldest = *UPSTREAM_PROTOCOL_RANGE.start();
    let upstream_newest = *UPSTREAM_PROTOCOL_RANGE.end();
    assert!(
        range_oldest == upstream_oldest && range_newest == upstream_newest,
        "supported protocol range must match upstream rsync's protocol span",
    );
};

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for select_highest_mutual with empty input
    #[test]
    fn select_highest_mutual_empty_iterator_returns_no_mutual_protocol() {
        let empty: Vec<u8> = vec![];
        let result = select_highest_mutual(empty);
        assert!(result.is_err());
        match result.unwrap_err() {
            NegotiationError::NoMutualProtocol { peer_versions } => {
                assert!(peer_versions.is_empty());
            }
            _ => panic!("expected NoMutualProtocol error"),
        }
    }

    // Tests for select_highest_mutual with single version
    #[test]
    fn select_highest_mutual_single_supported_version() {
        let versions = vec![30_u8];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::V30);
    }

    #[test]
    fn select_highest_mutual_single_newest_version_returns_early() {
        let versions = vec![ProtocolVersion::NEWEST.as_u8()];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::NEWEST);
    }

    // Tests for select_highest_mutual with multiple versions
    #[test]
    fn select_highest_mutual_multiple_versions_returns_highest() {
        let versions = vec![28_u8, 29, 30];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::V30);
    }

    #[test]
    fn select_highest_mutual_unsorted_versions_returns_highest() {
        let versions = vec![29_u8, 31, 28, 30];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
    }

    #[test]
    fn select_highest_mutual_all_supported_returns_newest() {
        let versions = vec![28_u8, 29, 30, 31, 32];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::NEWEST);
    }

    // Tests for select_highest_mutual with NEWEST version early return
    #[test]
    fn select_highest_mutual_newest_first_returns_immediately() {
        // When NEWEST is first, we return early without processing the rest
        let versions = vec![ProtocolVersion::NEWEST.as_u8(), 28, 29];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::NEWEST);
    }

    // Tests for select_highest_mutual with unsupported versions
    #[test]
    fn select_highest_mutual_only_too_old_versions() {
        // Versions below OLDEST (28) should trigger UnsupportedVersion
        let versions = vec![27_u8, 26, 25];
        let result = select_highest_mutual(versions);
        assert!(result.is_err());
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                // Should report the oldest rejection
                assert_eq!(v, 25);
            }
            _ => panic!("expected UnsupportedVersion error"),
        }
    }

    #[test]
    fn select_highest_mutual_reports_oldest_rejection() {
        let versions = vec![27_u8, 20, 26];
        let result = select_highest_mutual(versions);
        assert!(result.is_err());
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                assert_eq!(v, 20); // Oldest rejected version
            }
            _ => panic!("expected UnsupportedVersion error"),
        }
    }

    #[test]
    fn select_highest_mutual_versions_above_newest_are_clamped() {
        // Versions between NEWEST and MAXIMUM_PROTOCOL_ADVERTISEMENT are clamped to NEWEST
        let versions = vec![35_u8, 36, 37];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::NEWEST);
    }

    #[test]
    fn select_highest_mutual_mixed_supported_and_clamped() {
        let versions = vec![35_u8, 30, 29]; // 35 is clamped to NEWEST (32)
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::NEWEST);
    }

    #[test]
    fn select_highest_mutual_version_above_maximum_fails() {
        // Versions above MAXIMUM_PROTOCOL_ADVERTISEMENT (40) should fail
        let versions = vec![50_u32];
        let result = select_highest_mutual(versions);
        assert!(result.is_err());
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                assert_eq!(v, 50);
            }
            _ => panic!("expected UnsupportedVersion error"),
        }
    }

    // Tests with different input types
    #[test]
    fn select_highest_mutual_accepts_u8_slice() {
        let versions: &[u8] = &[30, 31];
        let result = select_highest_mutual(versions.iter()).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
    }

    #[test]
    fn select_highest_mutual_accepts_u32_values() {
        let versions: Vec<u32> = vec![30, 31, 32];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::NEWEST);
    }

    #[test]
    fn select_highest_mutual_accepts_protocol_version() {
        let versions = vec![ProtocolVersion::V29, ProtocolVersion::V30];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::V30);
    }

    // Tests for edge cases
    #[test]
    fn select_highest_mutual_zero_version_is_ignored_but_valid_versions_win() {
        // Zero is rejected by from_peer_advertisement, but if there are valid versions, they win
        let versions: Vec<u32> = vec![0, 30];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::V30);
    }

    #[test]
    fn select_highest_mutual_only_zero_version() {
        let versions: Vec<u32> = vec![0];
        let result = select_highest_mutual(versions);
        assert!(result.is_err());
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                assert_eq!(v, 0);
            }
            _ => panic!("expected UnsupportedVersion error"),
        }
    }

    #[test]
    fn select_highest_mutual_version_at_bitmap_boundary() {
        // Version 63 is at the boundary of u64 bitmap (bit 63)
        // But it's above MAXIMUM_PROTOCOL_ADVERTISEMENT
        let versions: Vec<u32> = vec![63];
        let result = select_highest_mutual(versions);
        assert!(result.is_err());
    }

    #[test]
    fn select_highest_mutual_version_above_bitmap_boundary() {
        // Version 64 would overflow the u64 bitmap
        // But it's above MAXIMUM_PROTOCOL_ADVERTISEMENT so it's rejected first
        let versions: Vec<u32> = vec![64];
        let result = select_highest_mutual(versions);
        assert!(result.is_err());
    }

    #[test]
    fn select_highest_mutual_duplicate_versions_handled() {
        let versions = vec![30_u8, 30, 30, 31, 31];
        let result = select_highest_mutual(versions).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
    }

    #[test]
    fn select_highest_mutual_mixed_valid_and_invalid() {
        // Mix of too-old, valid, and clamped versions
        let versions: Vec<u32> = vec![27, 30, 35];
        let result = select_highest_mutual(versions).unwrap();
        // 27 is too old (ignored for bitmap, tracked as rejection)
        // 30 is valid
        // 35 is clamped to NEWEST (32)
        assert_eq!(result, ProtocolVersion::NEWEST);
    }

    #[test]
    fn select_highest_mutual_no_mutual_with_unrecognized_versions() {
        // Versions that parse successfully but are above our bitmap range
        // Actually, versions 33-40 are clamped to NEWEST, so they work
        // We need versions that are recognized but not in bitmap
        // All versions 28-32 are in our bitmap, so we can't test this easily
        // Let's test NoMutualProtocol with versions that get filtered out differently

        // This scenario is hard to trigger since all recognized versions 28-32 are supported
        // But we can verify the behavior when only versions above MAXIMUM are given
        let versions: Vec<u32> = vec![50, 60, 70];
        let result = select_highest_mutual(versions);
        // All are UnsupportedVersion errors, first one (50) should be returned
        match result.unwrap_err() {
            NegotiationError::UnsupportedVersion(v) => {
                assert_eq!(v, 50);
            }
            _ => panic!("expected UnsupportedVersion error"),
        }
    }

    // Test with references
    #[test]
    fn select_highest_mutual_accepts_references() {
        let versions = vec![30_u8, 31];
        let result = select_highest_mutual(&versions).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
    }

    #[test]
    fn select_highest_mutual_accepts_mutable_references() {
        let mut versions = vec![30_u8, 31];
        let result = select_highest_mutual(&mut versions).unwrap();
        assert_eq!(result, ProtocolVersion::V31);
    }
}
