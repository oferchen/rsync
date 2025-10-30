use super::prelude::*;


#[test]
fn local_copy_options_apply_explicit_timeout() {
    let timeout = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .timeout(timeout)
        .build();

    let options = build_local_copy_options(&config, None);
    assert_eq!(options.timeout(), Some(Duration::from_secs(5)));
}


#[test]
fn local_copy_options_apply_modify_window() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .modify_window(Some(3))
        .build();

    let options = build_local_copy_options(&config, None);
    assert_eq!(options.modify_window(), Duration::from_secs(3));

    let default_config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();
    assert_eq!(
        build_local_copy_options(&default_config, None).modify_window(),
        Duration::ZERO
    );
}


#[test]
fn local_copy_options_omit_timeout_when_unset() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    let options = build_local_copy_options(&config, None);
    assert!(options.timeout().is_none());
}


#[test]
fn local_copy_options_delay_updates_enable_partial_transfers() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delay_updates(true)
        .build();

    let enabled_options = build_local_copy_options(&enabled, None);
    assert!(enabled_options.delay_updates_enabled());
    assert!(enabled_options.partial_enabled());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    let disabled_options = build_local_copy_options(&disabled, None);
    assert!(!disabled_options.delay_updates_enabled());
    assert!(!disabled_options.partial_enabled());
}


#[test]
fn local_copy_options_honour_temp_directory_setting() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .temp_directory(Some(PathBuf::from(".staging")))
        .build();

    let options = build_local_copy_options(&config, None);
    assert_eq!(options.temp_directory_path(), Some(Path::new(".staging")));

    let default_config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(
        build_local_copy_options(&default_config, None)
            .temp_directory_path()
            .is_none()
    );
}

