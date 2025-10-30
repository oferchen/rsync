use super::prelude::*;


#[test]
fn builder_preserves_owner_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .owner(true)
        .build();

    assert!(config.preserve_owner());
    assert!(!config.preserve_group());
}


#[test]
fn builder_preserves_group_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .group(true)
        .build();

    assert!(config.preserve_group());
    assert!(!config.preserve_owner());
}


#[test]
fn builder_preserves_permissions_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .permissions(true)
        .build();

    assert!(config.preserve_permissions());
    assert!(!config.preserve_times());
}


#[test]
fn builder_preserves_times_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .times(true)
        .build();

    assert!(config.preserve_times());
    assert!(!config.preserve_permissions());
}


#[test]
fn builder_preserves_devices_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .devices(true)
        .build();

    assert!(config.preserve_devices());
    assert!(!config.preserve_specials());
}


#[test]
fn builder_preserves_specials_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .specials(true)
        .build();

    assert!(config.preserve_specials());
    assert!(!config.preserve_devices());
}


#[test]
fn builder_preserves_hard_links_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .hard_links(true)
        .build();

    assert!(config.preserve_hard_links());
    assert!(!ClientConfig::default().preserve_hard_links());
}


#[cfg(feature = "acl")]
#[test]
fn builder_preserves_acls_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .acls(true)
        .build();

    assert!(config.preserve_acls());
    assert!(!ClientConfig::default().preserve_acls());
}


#[cfg(feature = "xattr")]
#[test]
fn builder_preserves_xattrs_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .xattrs(true)
        .build();

    assert!(config.preserve_xattrs());
    assert!(!ClientConfig::default().preserve_xattrs());
}


#[test]
fn builder_preserves_remove_source_files_flag() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .remove_source_files(true)
        .build();

    assert!(config.remove_source_files());
    assert!(!ClientConfig::default().remove_source_files());
}

