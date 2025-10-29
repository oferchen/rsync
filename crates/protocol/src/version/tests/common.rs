use crate::error::NegotiationError;
use std::collections::BTreeSet;

use super::{ProtocolVersion, ProtocolVersionAdvertisement, SUPPORTED_PROTOCOL_BITMAP};

pub(super) fn reference_negotiation(
    peer_versions: &[u8],
) -> Result<ProtocolVersion, NegotiationError> {
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

pub(super) fn collect_advertised<I, T>(inputs: I) -> Vec<u8>
where
    I: IntoIterator<Item = T>,
    T: ProtocolVersionAdvertisement,
{
    inputs
        .into_iter()
        .map(|value| value.into_advertised_version())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{collect_advertised, reference_negotiation, NegotiationError};
    use super::super::ProtocolVersion;

    #[test]
    fn reference_negotiation_prefers_highest_supported_version() {
        let negotiated = reference_negotiation(&[27, 32, 30]).expect("must select newest");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn reference_negotiation_reports_oldest_rejection() {
        let err = reference_negotiation(&[27, 26]).unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(26));
    }

    #[test]
    fn reference_negotiation_reports_no_mutual_protocol() {
        let err = reference_negotiation(&[]).unwrap_err();
        assert_eq!(
            err,
            NegotiationError::NoMutualProtocol {
                peer_versions: vec![],
            }
        );
    }

    #[test]
    fn reference_negotiation_clamps_future_versions_to_newest() {
        let negotiated = reference_negotiation(&[40]).expect("future versions clamp");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn collect_advertised_preserves_supported_sequence() {
        let expected: Vec<u8> = ProtocolVersion::supported_versions_iter()
            .map(ProtocolVersion::as_u8)
            .collect();
        let collected = collect_advertised(expected.iter().copied());
        assert_eq!(collected, expected);
    }

    #[test]
    fn collect_advertised_bounds_future_and_negative_inputs() {
        let future = u16::from(u8::MAX) + 10;
        let collected = collect_advertised([future]);
        assert_eq!(collected, vec![u8::MAX]);

        let collected = collect_advertised([-5i16, 31i16]);
        assert_eq!(collected, vec![0, 31]);
    }
}
