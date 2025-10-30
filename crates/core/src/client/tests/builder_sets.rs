use super::prelude::*;


#[test]
fn builder_sets_compression_setting() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compression_setting(CompressionSetting::level(CompressionLevel::Best))
        .build();

    assert_eq!(
        config.compression_setting(),
        CompressionSetting::level(CompressionLevel::Best)
    );
}


#[test]
fn builder_sets_min_file_size_limit() {
    let config = ClientConfig::builder().min_file_size(Some(1_024)).build();

    assert_eq!(config.min_file_size(), Some(1_024));

    let cleared = ClientConfig::builder()
        .min_file_size(Some(2048))
        .min_file_size(None)
        .build();

    assert_eq!(cleared.min_file_size(), None);
}


#[test]
fn builder_sets_max_file_size_limit() {
    let config = ClientConfig::builder().max_file_size(Some(8_192)).build();

    assert_eq!(config.max_file_size(), Some(8_192));

    let cleared = ClientConfig::builder()
        .max_file_size(Some(4_096))
        .max_file_size(None)
        .build();

    assert_eq!(cleared.max_file_size(), None);
}


#[test]
fn builder_sets_max_delete_limit() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .max_delete(Some(4))
        .build();

    assert_eq!(config.max_delete(), Some(4));
    assert_eq!(ClientConfig::default().max_delete(), None);
}


#[test]
fn builder_sets_bandwidth_limit() {
    let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(4096).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .bandwidth_limit(Some(limit))
        .build();

    assert_eq!(config.bandwidth_limit(), Some(limit));
}


#[test]
fn builder_sets_compression_level() {
    let level = NonZeroU8::new(7).unwrap();
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .compress(true)
        .compression_level(Some(CompressionLevel::precise(level)))
        .build();

    assert!(config.compress());
    assert_eq!(
        config.compression_level(),
        Some(CompressionLevel::precise(level))
    );
    assert_eq!(ClientConfig::default().compression_level(), None);
}


#[test]
fn builder_sets_timeout() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(30).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .timeout(timeout)
        .build();

    assert_eq!(config.timeout(), timeout);
    assert_eq!(ClientConfig::default().timeout(), TransferTimeout::Default);
}


#[test]
fn builder_sets_connect_timeout() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(12).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .connect_timeout(timeout)
        .build();

    assert_eq!(config.connect_timeout(), timeout);
    assert_eq!(
        ClientConfig::default().connect_timeout(),
        TransferTimeout::Default
    );
}


#[test]
fn builder_sets_numeric_ids() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .numeric_ids(true)
        .build();

    assert!(config.numeric_ids());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.numeric_ids());
}


#[test]
fn builder_sets_chmod_modifiers() {
    let modifiers = ChmodModifiers::parse("a+rw").expect("chmod parses");
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .chmod(Some(modifiers.clone()))
        .build();

    assert_eq!(config.chmod(), Some(&modifiers));
}


#[test]
fn builder_sets_mkpath_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .mkpath(true)
        .build();

    assert!(config.mkpath());
    assert!(!ClientConfig::default().mkpath());
}


#[test]
fn builder_sets_inplace() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .inplace(true)
        .build();

    assert!(config.inplace());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.inplace());
}


#[test]
fn builder_sets_delay_updates() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delay_updates(true)
        .build();

    assert!(config.delay_updates());

    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!config.delay_updates());
}


#[test]
fn builder_sets_copy_dirlinks() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .copy_dirlinks(true)
        .build();

    assert!(enabled.copy_dirlinks());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.copy_dirlinks());
}


#[test]
fn builder_sets_copy_unsafe_links() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .copy_unsafe_links(true)
        .build();

    assert!(enabled.copy_unsafe_links());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.copy_unsafe_links());
}


#[test]
fn builder_sets_keep_dirlinks() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .keep_dirlinks(true)
        .build();

    assert!(enabled.keep_dirlinks());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.keep_dirlinks());
}

