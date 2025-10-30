use super::prelude::*;


#[test]
fn builder_defaults_disable_compression() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.compress());
    assert!(config.compression_setting().is_disabled());
}

