use super::prelude::*;


#[test]
fn builder_safe_links_round_trip() {
    let enabled = ClientConfig::builder().safe_links(true).build();
    assert!(enabled.safe_links());

    let disabled = ClientConfig::builder().safe_links(false).build();
    assert!(!disabled.safe_links());

    let default_config = ClientConfig::builder().build();
    assert!(!default_config.safe_links());
}

