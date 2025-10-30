use super::prelude::*;


#[test]
fn resolve_connect_timeout_prefers_explicit_setting() {
    let explicit = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
    let resolved =
        resolve_connect_timeout(explicit, TransferTimeout::Default, Duration::from_secs(30));
    assert_eq!(resolved, Some(Duration::from_secs(5)));
}


#[test]
fn resolve_connect_timeout_uses_transfer_timeout_when_default() {
    let transfer = TransferTimeout::Seconds(NonZeroU64::new(8).unwrap());
    let resolved =
        resolve_connect_timeout(TransferTimeout::Default, transfer, Duration::from_secs(30));
    assert_eq!(resolved, Some(Duration::from_secs(8)));
}


#[test]
fn resolve_connect_timeout_disables_when_requested() {
    let resolved = resolve_connect_timeout(
        TransferTimeout::Disabled,
        TransferTimeout::Seconds(NonZeroU64::new(9).unwrap()),
        Duration::from_secs(30),
    );
    assert!(resolved.is_none());

    let resolved_default = resolve_connect_timeout(
        TransferTimeout::Default,
        TransferTimeout::Disabled,
        Duration::from_secs(30),
    );
    assert!(resolved_default.is_none());
}

