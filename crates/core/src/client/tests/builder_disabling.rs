use super::prelude::*;


#[test]
fn builder_disabling_compress_clears_override() {
    let level = NonZeroU8::new(5).unwrap();
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compression_level(Some(CompressionLevel::precise(level)))
        .compress(false)
        .build();

    assert!(!config.compress());
    assert!(config.compression_setting().is_disabled());
    assert_eq!(config.compression_level(), None);
}

