use super::prelude::*;


#[test]
fn builder_append_round_trip() {
    let enabled = ClientConfig::builder().append(true).build();
    assert!(enabled.append());
    assert!(!enabled.append_verify());

    let disabled = ClientConfig::builder().append(false).build();
    assert!(!disabled.append());
    assert!(!disabled.append_verify());
}


#[test]
fn builder_append_verify_implies_append() {
    let verified = ClientConfig::builder().append_verify(true).build();
    assert!(verified.append());
    assert!(verified.append_verify());

    let cleared = ClientConfig::builder()
        .append(true)
        .append_verify(true)
        .append_verify(false)
        .build();
    assert!(cleared.append());
    assert!(!cleared.append_verify());
}

