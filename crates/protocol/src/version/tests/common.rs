use crate::error::NegotiationError;
use std::collections::BTreeSet;

use super::{ProtocolVersion, ProtocolVersionAdvertisement, SUPPORTED_PROTOCOL_BITMAP};

pub(super) fn reference_negotiation(
    peer_versions: &[u32],
) -> Result<ProtocolVersion, NegotiationError> {
    let mut recognized = BTreeSet::new();
    let mut supported_bitmap: u64 = 0;
    let mut oldest_rejection: Option<u32> = None;

    for &advertised in peer_versions {
        match ProtocolVersion::from_peer_advertisement(advertised) {
            Ok(proto) => {
                let value = proto.as_u8();
                recognized.insert(value);

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
                oldest_rejection = Some(match oldest_rejection {
                    Some(current) if value >= current => current,
                    _ => value,
                });
            }
            Err(err) => return Err(err),
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

pub(super) fn collect_advertised<I, T>(inputs: I) -> Vec<u32>
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
    use super::super::ProtocolVersion;
    use super::{NegotiationError, collect_advertised, reference_negotiation};

    #[test]
    fn reference_negotiation_prefers_highest_supported_version() {
        let negotiated = reference_negotiation(&[27u32, 32, 30]).expect("must select newest");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn reference_negotiation_reports_oldest_rejection() {
        let err = reference_negotiation(&[27u32, 26]).unwrap_err();
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
        let negotiated = reference_negotiation(&[40u32]).expect("future versions clamp");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn collect_advertised_preserves_supported_sequence() {
        let expected: Vec<u32> = ProtocolVersion::supported_versions_iter()
            .map(|version| u32::from(version.as_u8()))
            .collect();
        let collected = collect_advertised(expected.iter().copied());
        assert_eq!(collected, expected);
    }

    #[test]
    fn collect_advertised_bounds_future_and_negative_inputs() {
        let future = u16::from(u8::MAX) + 10;
        let collected = collect_advertised([future]);
        assert_eq!(collected, vec![u32::from(future)]);

        let collected = collect_advertised([-5i16, 31i16]);
        assert_eq!(collected, vec![0, 31]);
    }
}
