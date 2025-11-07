use std::path::Path;

use super::*;

#[test]
fn parse_protocol_matches_env() {
    assert_eq!(metadata().protocol_version(), 32);
    assert_eq!(protocol_version_u8(), 32);
    assert_eq!(protocol_version_nonzero_u8().get(), 32);
    assert_eq!(daemon_config_dir(), Path::new(DAEMON_CONFIG_DIR));
    assert_eq!(daemon_config_path(), Path::new(DAEMON_CONFIG_PATH));
    assert_eq!(daemon_secrets_path(), Path::new(DAEMON_SECRETS_PATH));
    assert_eq!(
        legacy_daemon_config_dir(),
        Path::new(LEGACY_DAEMON_CONFIG_DIR)
    );
    assert_eq!(
        legacy_daemon_config_path(),
        Path::new(LEGACY_DAEMON_CONFIG_PATH)
    );
    assert_eq!(
        legacy_daemon_secrets_path(),
        Path::new(LEGACY_DAEMON_SECRETS_PATH)
    );
}

#[test]
fn const_accessors_match_metadata() {
    let snapshot = metadata();

    assert_eq!(brand(), snapshot.brand());
    assert_eq!(upstream_version(), snapshot.upstream_version());
    assert_eq!(rust_version(), snapshot.rust_version());
    assert_eq!(client_program_name(), snapshot.client_program_name());
    assert_eq!(daemon_program_name(), snapshot.daemon_program_name());
    assert_eq!(
        legacy_client_program_name(),
        snapshot.legacy_client_program_name()
    );
    assert_eq!(
        legacy_daemon_program_name(),
        snapshot.legacy_daemon_program_name()
    );
    assert_eq!(
        legacy_daemon_config_dir(),
        Path::new(snapshot.legacy_daemon_config_dir())
    );
    assert_eq!(
        legacy_daemon_config_path(),
        Path::new(snapshot.legacy_daemon_config_path())
    );
    assert_eq!(
        legacy_daemon_secrets_path(),
        Path::new(snapshot.legacy_daemon_secrets_path())
    );
    assert_eq!(source_url(), snapshot.source_url());
    assert_eq!(web_site(), snapshot.web_site());
}

#[test]
fn metadata_matches_manifest() {
    let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"));
    let value: toml::Table = manifest.parse().expect("parse manifest");
    let workspace = value
        .get("workspace")
        .and_then(toml::Value::as_table)
        .expect("workspace table");
    let metadata_table = workspace
        .get("metadata")
        .and_then(toml::Value::as_table)
        .expect("metadata table");
    let oc = metadata_table
        .get("oc_rsync")
        .and_then(toml::Value::as_table)
        .expect("oc_rsync table");

    let snapshot = metadata();

    assert_eq!(snapshot.brand(), oc["brand"].as_str().expect("brand"));
    assert_eq!(
        snapshot.upstream_version(),
        oc["upstream_version"].as_str().expect("upstream_version")
    );
    assert_eq!(
        snapshot.rust_version(),
        oc["rust_version"].as_str().expect("rust_version")
    );
    assert_eq!(
        snapshot.protocol_version(),
        oc["protocol"].as_integer().expect("protocol") as u32
    );
    assert_eq!(
        snapshot.client_program_name(),
        oc["client_bin"].as_str().expect("client_bin")
    );
    assert_eq!(
        snapshot.daemon_program_name(),
        oc["daemon_bin"].as_str().expect("daemon_bin")
    );
    assert_eq!(
        snapshot.legacy_client_program_name(),
        oc["legacy_client_bin"].as_str().expect("legacy_client_bin")
    );
    assert_eq!(
        snapshot.legacy_daemon_program_name(),
        oc["legacy_daemon_bin"].as_str().expect("legacy_daemon_bin")
    );
    assert_eq!(
        snapshot.daemon_config_dir(),
        oc["daemon_config_dir"].as_str().expect("daemon_config_dir")
    );
    assert_eq!(
        snapshot.daemon_config_path(),
        oc["daemon_config"].as_str().expect("daemon_config")
    );
    assert_eq!(
        snapshot.daemon_secrets_path(),
        oc["daemon_secrets"].as_str().expect("daemon_secrets")
    );
    assert_eq!(
        snapshot.legacy_daemon_config_dir(),
        oc["legacy_daemon_config_dir"]
            .as_str()
            .expect("legacy_daemon_config_dir")
    );
    assert_eq!(
        snapshot.legacy_daemon_config_path(),
        oc["legacy_daemon_config"]
            .as_str()
            .expect("legacy_daemon_config")
    );
    assert_eq!(
        snapshot.legacy_daemon_secrets_path(),
        oc["legacy_daemon_secrets"]
            .as_str()
            .expect("legacy_daemon_secrets")
    );
    assert_eq!(
        snapshot.source_url(),
        oc["source"].as_str().expect("source")
    );
    assert_eq!(
        snapshot.web_site(),
        oc["web_site"].as_str().expect("web_site")
    );
}
