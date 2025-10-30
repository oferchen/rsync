use super::prelude::*;


#[test]
fn builder_configures_implied_dirs_flag() {
    let default_config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(default_config.implied_dirs());
    assert!(ClientConfig::default().implied_dirs());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .implied_dirs(false)
        .build();

    assert!(!disabled.implied_dirs());

    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .implied_dirs(true)
        .build();

    assert!(enabled.implied_dirs());
}

