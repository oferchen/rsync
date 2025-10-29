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
    let mut oldest_rejection: Option<u8> = None;

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
                if value < ProtocolVersion::OLDEST.as_u8() =>
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

/// Ensures compile-time guards stay aligned with the advertised protocol list.
pub(crate) const fn validate_protocol_tables() {
    let protocols = super::SUPPORTED_PROTOCOLS;
    assert!(
        !protocols.is_empty(),
        "supported protocol list must not be empty"
    );
    assert!(
        protocols.len() == SUPPORTED_PROTOCOL_COUNT,
        "supported protocol count must match list length",
    );
    assert!(
        protocols[0] == ProtocolVersion::NEWEST.as_u8(),
        "newest supported protocol must lead the list",
    );
    assert!(
        protocols[protocols.len() - 1] == ProtocolVersion::OLDEST.as_u8(),
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
}
