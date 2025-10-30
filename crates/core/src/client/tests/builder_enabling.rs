use super::prelude::*;


#[test]
fn builder_enabling_compress_sets_default_level() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compress(true)
        .build();

    assert!(config.compress());
    assert!(config.compression_setting().is_enabled());
    assert_eq!(
        config.compression_setting().level_or_default(),
        CompressionLevel::Default
    );
}

