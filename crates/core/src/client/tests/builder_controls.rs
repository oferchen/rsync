use super::prelude::*;


#[test]
fn builder_controls_omit_dir_times_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .omit_dir_times(true)
        .build();

    assert!(config.omit_dir_times());
    assert!(!ClientConfig::default().omit_dir_times());
}


#[test]
fn builder_controls_omit_link_times_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .omit_link_times(true)
        .build();

    assert!(config.omit_link_times());
    assert!(!ClientConfig::default().omit_link_times());
}

