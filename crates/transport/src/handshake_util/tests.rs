use super::*;
use proptest::prelude::*;

macro_rules! const_assert {
    ($condition:expr $(,)?) => {
        const_assert!($condition, stringify!($condition));
    };
    ($condition:expr, $message:expr $(,)?) => {
        const _: () = {
            if !$condition {
                panic!("{}", $message);
            }
        };
    };
}

fn negotiated_version_strategy() -> impl Strategy<Value = ProtocolVersion> {
    let versions: Vec<ProtocolVersion> = ProtocolVersion::supported_versions_array().to_vec();
    prop::sample::select(versions)
}

#[test]
fn detects_future_versions_encoded_in_u32() {
    assert!(remote_advertisement_was_clamped(40));
    assert!(remote_advertisement_was_clamped(0x0001_0200));
}

#[test]
fn ignores_advertisements_within_supported_range() {
    assert!(!remote_advertisement_was_clamped(30));
    assert!(!remote_advertisement_was_clamped(
        ProtocolVersion::OLDEST.as_u8().into()
    ));
}

#[test]
fn local_cap_reductions_do_not_appear_as_remote_clamps() {
    let remote = ProtocolVersion::NEWEST.as_u8();

    assert!(!remote_advertisement_was_clamped(u32::from(remote)));
}

#[test]
fn remote_advertisement_helpers_are_const_evaluable() {
    const CLAMPED: bool =
        remote_advertisement_was_clamped(ProtocolVersion::NEWEST.as_u8() as u32 + 1);
    const NOT_CLAMPED: bool =
        remote_advertisement_was_clamped(ProtocolVersion::NEWEST.as_u8() as u32);
    const FUTURE: RemoteProtocolAdvertisement =
        RemoteProtocolAdvertisement::from_raw(40, ProtocolVersion::NEWEST);
    const SUPPORTED: RemoteProtocolAdvertisement = RemoteProtocolAdvertisement::from_raw(
        ProtocolVersion::V30.as_u8() as u32,
        ProtocolVersion::V30,
    );
    const_assert!(CLAMPED, "remote advertisements must be clamped");
    const_assert!(!NOT_CLAMPED, "remote advertisement unexpectedly clamped");
    const_assert!(
        FUTURE.was_clamped(),
        "future advertisement should be clamped",
    );
    const_assert!(
        !SUPPORTED.was_clamped(),
        "supported advertisement should not be clamped",
    );

    let clamped = CLAMPED;
    let not_clamped = NOT_CLAMPED;
    let future = FUTURE;
    let supported = SUPPORTED;

    assert!(clamped);
    assert!(!not_clamped);
    assert!(matches!(
        future,
        RemoteProtocolAdvertisement::Future {
            advertised: 40,
            clamped
        } if clamped == ProtocolVersion::NEWEST
    ));
    assert!(matches!(
        supported,
        RemoteProtocolAdvertisement::Supported(ProtocolVersion::V30)
    ));
    assert_eq!(future.negotiated(), ProtocolVersion::NEWEST);
    assert_eq!(supported.negotiated(), ProtocolVersion::V30);
    assert!(future.was_clamped());
    assert!(!supported.was_clamped());
}

#[test]
fn local_cap_detection_is_const_evaluable() {
    const WAS_CAPPED: bool = local_cap_reduced_protocol(ProtocolVersion::V31, ProtocolVersion::V29);
    const NOT_CAPPED: bool = local_cap_reduced_protocol(ProtocolVersion::V29, ProtocolVersion::V29);

    const_assert!(WAS_CAPPED, "local cap reduction must be detected");
    const_assert!(!NOT_CAPPED, "local cap should not be detected");

    let was_capped = WAS_CAPPED;
    let not_capped = NOT_CAPPED;

    assert!(was_capped);
    assert!(!not_capped);
}

#[test]
fn future_remote_versions_are_detected_even_with_local_caps() {
    assert!(remote_advertisement_was_clamped(40));
}

#[test]
fn classification_marks_supported_advertisements() {
    let version = ProtocolVersion::from_supported(31).expect("supported protocol");
    let advertised = u32::from(version.as_u8());
    let classification = RemoteProtocolAdvertisement::from_raw(advertised, version);

    assert!(classification.is_supported());
    assert_eq!(classification.supported(), Some(version));
    assert_eq!(classification.future(), None);
    assert_eq!(classification.clamped(), None);
    assert_eq!(classification.advertised(), advertised);
    assert_eq!(classification.negotiated(), version);
    assert!(!classification.was_clamped());
}

#[test]
fn classification_marks_future_advertisements() {
    let advertised = 40u32;
    let classification = RemoteProtocolAdvertisement::from_raw(advertised, ProtocolVersion::NEWEST);

    assert!(!classification.is_supported());
    assert_eq!(classification.supported(), None);
    assert_eq!(classification.future(), Some(advertised));
    assert_eq!(classification.clamped(), Some(ProtocolVersion::NEWEST));
    assert_eq!(classification.advertised(), advertised);
    assert_eq!(classification.negotiated(), ProtocolVersion::NEWEST);
    assert!(classification.was_clamped());
}

#[test]
fn classification_converts_into_protocol_version() {
    let supported = RemoteProtocolAdvertisement::Supported(ProtocolVersion::V31);
    let future = RemoteProtocolAdvertisement::from_raw(40, ProtocolVersion::NEWEST);

    let supported_version: ProtocolVersion = supported.into();
    let future_version: ProtocolVersion = future.into();

    assert_eq!(supported_version, ProtocolVersion::V31);
    assert_eq!(future_version, ProtocolVersion::NEWEST);
    assert_eq!(future.clamped(), Some(ProtocolVersion::NEWEST));
}

#[test]
fn classification_display_is_stable() {
    let supported = RemoteProtocolAdvertisement::Supported(ProtocolVersion::V31);
    let future = RemoteProtocolAdvertisement::from_raw(40, ProtocolVersion::NEWEST);

    assert_eq!(supported.to_string(), "protocol 31");
    assert_eq!(future.to_string(), "future protocol 40 (clamped to 32)");
}

proptest! {
    #[test]
    fn within_byte_range_matches_direct_comparison(
        advertised in 0u32..=u8::MAX as u32,
    ) {
        let newest = u32::from(ProtocolVersion::NEWEST.as_u8());
        let expected = advertised > newest;
        prop_assert_eq!(
            remote_advertisement_was_clamped(advertised),
            expected
        );
    }

    #[test]
    fn out_of_range_values_always_report_clamp(
        advertised in (u8::MAX as u32 + 1)..=u32::MAX,
    ) {
        prop_assert!(remote_advertisement_was_clamped(advertised));
    }

    #[test]
    fn local_cap_detection_matches_direct_comparison(
        remote in negotiated_version_strategy(),
        negotiated in negotiated_version_strategy(),
    ) {
        let expected = negotiated < remote;
        prop_assert_eq!(local_cap_reduced_protocol(remote, negotiated), expected);
    }

    #[test]
    fn advertised_round_trips_to_raw_value(
        advertised in 0u32..=u16::MAX as u32,
    ) {
        let negotiated = if remote_advertisement_was_clamped(advertised) {
            ProtocolVersion::NEWEST
        } else {
            let byte = u8::try_from(advertised).unwrap_or(ProtocolVersion::NEWEST.as_u8());
            ProtocolVersion::from_supported(byte).unwrap_or(ProtocolVersion::OLDEST)
        };

        let classification = RemoteProtocolAdvertisement::from_raw(advertised, negotiated);
        let expected = if remote_advertisement_was_clamped(advertised) {
            advertised
        } else {
            negotiated.as_u8() as u32
        };

        prop_assert_eq!(classification.advertised(), expected);
        prop_assert_eq!(classification.negotiated(), negotiated);
        prop_assert_eq!(
            classification.was_clamped(),
            remote_advertisement_was_clamped(advertised)
        );
    }
}
