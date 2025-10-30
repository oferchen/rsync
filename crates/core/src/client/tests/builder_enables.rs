use super::prelude::*;


#[test]
fn builder_enables_dry_run() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .dry_run(true)
        .build();

    assert!(config.dry_run());
}


#[test]
fn builder_enables_list_only() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .list_only(true)
        .build();

    assert!(config.list_only());
}


#[test]
fn builder_enables_delete() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete(true)
        .build();

    assert!(config.delete());
    assert_eq!(config.delete_mode(), DeleteMode::During);
}


#[test]
fn builder_enables_delete_after() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_after(true)
        .build();

    assert!(config.delete());
    assert!(config.delete_after());
    assert_eq!(config.delete_mode(), DeleteMode::After);
}


#[test]
fn builder_enables_delete_delay() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_delay(true)
        .build();

    assert!(config.delete());
    assert!(config.delete_delay());
    assert_eq!(config.delete_mode(), DeleteMode::Delay);
}


#[test]
fn builder_enables_delete_before() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_before(true)
        .build();

    assert!(config.delete());
    assert!(config.delete_before());
    assert_eq!(config.delete_mode(), DeleteMode::Before);
}


#[test]
fn builder_enables_delete_excluded() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .delete_excluded(true)
        .build();

    assert!(config.delete_excluded());
    assert!(!ClientConfig::default().delete_excluded());
}


#[test]
fn builder_enables_checksum() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .checksum(true)
        .build();

    assert!(config.checksum());
}


#[test]
fn builder_enables_update() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .update(true)
        .build();

    assert!(config.update());
    assert!(!ClientConfig::default().update());
}


#[test]
fn builder_enables_sparse() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .sparse(true)
        .build();

    assert!(config.sparse());
}


#[test]
fn builder_enables_size_only() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .size_only(true)
        .build();

    assert!(config.size_only());
    assert!(!ClientConfig::default().size_only());
}


#[test]
fn builder_enables_stats() {
    let enabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .stats(true)
        .build();

    assert!(enabled.stats());

    let disabled = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .build();

    assert!(!disabled.stats());
}

