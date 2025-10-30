use super::prelude::*;


#[test]
fn builder_backup_round_trip() {
    let enabled = ClientConfig::builder().backup(true).build();
    assert!(enabled.backup());

    let disabled = ClientConfig::builder().build();
    assert!(!disabled.backup());
}


#[test]
fn builder_backup_directory_implies_backup() {
    let config = ClientConfig::builder()
        .backup_directory(Some(PathBuf::from("backups")))
        .build();

    assert!(config.backup());
    assert_eq!(
        config.backup_directory(),
        Some(std::path::Path::new("backups"))
    );

    let cleared = ClientConfig::builder()
        .backup_directory(None::<PathBuf>)
        .build();
    assert!(!cleared.backup());
    assert!(cleared.backup_directory().is_none());
}


#[test]
fn builder_backup_suffix_implies_backup() {
    let config = ClientConfig::builder()
        .backup_suffix(Some(OsString::from(".bak")))
        .build();

    assert!(config.backup());
    assert_eq!(config.backup_suffix(), Some(OsStr::new(".bak")));

    let cleared = ClientConfig::builder()
        .backup(true)
        .backup_suffix(None::<OsString>)
        .build();
    assert!(cleared.backup());
    assert_eq!(cleared.backup_suffix(), None);
}

