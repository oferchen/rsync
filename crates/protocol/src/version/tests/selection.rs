use crate::error::NegotiationError;
use ::core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize, Wrapping,
};

use super::{ProtocolVersion, select_highest_mutual};

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
    let err = select_highest_mutual(::core::iter::empty::<u8>()).unwrap_err();
    assert_eq!(
        err,
        NegotiationError::NoMutualProtocol {
            peer_versions: vec![],
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
    fn check<T: super::ProtocolVersionAdvertisement + Copy>(advertised: T) {
        let negotiated = select_highest_mutual([advertised]).expect("non-zero unsigned works");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    check(NonZeroU16::new(32).expect("non-zero"));
    check(NonZeroU32::new(32).expect("non-zero"));
    check(NonZeroU64::new(32).expect("non-zero"));
    check(NonZeroU128::new(32).expect("non-zero"));
    check(NonZeroUsize::new(32).expect("non-zero"));

    let future = NonZeroU64::new(200).expect("non-zero");
    let err = select_highest_mutual([future]).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(200));
}

#[test]
fn select_highest_mutual_accepts_non_zero_signed_advertisements() {
    fn check<T: super::ProtocolVersionAdvertisement + Copy>(
        advertised: T,
        expected: ProtocolVersion,
    ) {
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
    let err = select_highest_mutual([future]).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(200));
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
    let err = select_highest_mutual([future]).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(200));
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
    let err = select_highest_mutual(peers).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(i32::MAX as u32));

    let peers = [i128::MAX];
    let err = select_highest_mutual(peers).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(u32::MAX));
}

#[test]
fn select_highest_mutual_saturates_wider_integer_advertisements() {
    let peers = [u32::MAX];
    let err = select_highest_mutual(peers).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(u32::MAX));

    let peers = [u128::MAX];
    let err = select_highest_mutual(peers).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(u32::MAX));
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
fn rejects_advertisements_beyond_upstream_cap() {
    let err = select_highest_mutual([41u8]).unwrap_err();
    assert_eq!(err, NegotiationError::UnsupportedVersion(41));

    let err =
        select_highest_mutual([u32::from(ProtocolVersion::NEWEST.as_u8()) + 100]).unwrap_err();
    assert_eq!(
        err,
        NegotiationError::UnsupportedVersion(ProtocolVersion::NEWEST.as_u8() as u32 + 100)
    );
}
