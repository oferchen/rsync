use oc_rsync_core::branding::{
    BRAND_OVERRIDE_ENV, LEGACY_DAEMON_CONFIG_DIR, LEGACY_DAEMON_CONFIG_PATH,
    LEGACY_DAEMON_SECRETS_PATH, OC_CLIENT_PROGRAM_NAME, OC_DAEMON_CONFIG_DIR,
    OC_DAEMON_CONFIG_PATH, OC_DAEMON_PROGRAM_NAME, OC_DAEMON_SECRETS_PATH,
    UPSTREAM_CLIENT_PROGRAM_NAME, UPSTREAM_DAEMON_PROGRAM_NAME, brand_override_env_var,
};
use oc_rsync_core::workspace::{
    self, CLIENT_PROGRAM_NAME, DAEMON_CONFIG_DIR, DAEMON_CONFIG_PATH, DAEMON_PROGRAM_NAME,
    DAEMON_SECRETS_PATH, LEGACY_CLIENT_PROGRAM_NAME, LEGACY_DAEMON_PROGRAM_NAME, PROTOCOL_VERSION,
    RUST_VERSION, SOURCE_URL, UPSTREAM_VERSION,
};
use std::path::Path;

#[test]
fn workspace_metadata_matches_constants() {
    let metadata = workspace::metadata();

    assert_eq!(metadata.brand(), workspace::brand());
    assert_eq!(metadata.brand(), workspace::BRAND);
    assert_eq!(metadata.upstream_version(), UPSTREAM_VERSION);
    assert_eq!(metadata.rust_version(), RUST_VERSION);
    assert_eq!(metadata.protocol_version(), PROTOCOL_VERSION);
    assert_eq!(metadata.client_program_name(), CLIENT_PROGRAM_NAME);
    assert_eq!(metadata.daemon_program_name(), DAEMON_PROGRAM_NAME);
    assert_eq!(
        metadata.legacy_client_program_name(),
        LEGACY_CLIENT_PROGRAM_NAME
    );
    assert_eq!(
        metadata.legacy_daemon_program_name(),
        LEGACY_DAEMON_PROGRAM_NAME
    );
    assert_eq!(metadata.daemon_config_dir(), DAEMON_CONFIG_DIR);
    assert_eq!(metadata.daemon_config_path(), DAEMON_CONFIG_PATH);
    assert_eq!(metadata.daemon_secrets_path(), DAEMON_SECRETS_PATH);
    assert_eq!(
        metadata.legacy_daemon_config_dir(),
        workspace::LEGACY_DAEMON_CONFIG_DIR
    );
    assert_eq!(
        metadata.legacy_daemon_config_path(),
        workspace::LEGACY_DAEMON_CONFIG_PATH
    );
    assert_eq!(
        metadata.legacy_daemon_secrets_path(),
        workspace::LEGACY_DAEMON_SECRETS_PATH
    );
    assert_eq!(metadata.source_url(), SOURCE_URL);
}

#[test]
fn branding_constants_align_with_workspace_metadata() {
    let metadata = workspace::metadata();

    assert_eq!(
        UPSTREAM_CLIENT_PROGRAM_NAME,
        metadata.legacy_client_program_name()
    );
    assert_eq!(
        UPSTREAM_DAEMON_PROGRAM_NAME,
        metadata.legacy_daemon_program_name()
    );
    assert_eq!(OC_CLIENT_PROGRAM_NAME, metadata.client_program_name());
    assert_eq!(OC_DAEMON_PROGRAM_NAME, metadata.daemon_program_name());
    assert_eq!(OC_DAEMON_CONFIG_DIR, metadata.daemon_config_dir());
    assert_eq!(OC_DAEMON_CONFIG_PATH, metadata.daemon_config_path());
    assert_eq!(OC_DAEMON_SECRETS_PATH, metadata.daemon_secrets_path());
    assert_eq!(
        LEGACY_DAEMON_CONFIG_DIR,
        metadata.legacy_daemon_config_dir()
    );
    assert_eq!(
        LEGACY_DAEMON_CONFIG_PATH,
        metadata.legacy_daemon_config_path()
    );
    assert_eq!(
        LEGACY_DAEMON_SECRETS_PATH,
        metadata.legacy_daemon_secrets_path()
    );
    assert_eq!(brand_override_env_var(), BRAND_OVERRIDE_ENV);
}

#[test]
fn workspace_path_helpers_return_expected_paths() {
    assert_eq!(workspace::daemon_config_dir(), Path::new(DAEMON_CONFIG_DIR));
    assert_eq!(
        workspace::daemon_config_path(),
        Path::new(DAEMON_CONFIG_PATH)
    );
    assert_eq!(
        workspace::daemon_secrets_path(),
        Path::new(DAEMON_SECRETS_PATH)
    );
    assert_eq!(
        workspace::legacy_daemon_config_dir(),
        Path::new(workspace::LEGACY_DAEMON_CONFIG_DIR)
    );
    assert_eq!(
        workspace::legacy_daemon_config_path(),
        Path::new(workspace::LEGACY_DAEMON_CONFIG_PATH)
    );
    assert_eq!(
        workspace::legacy_daemon_secrets_path(),
        Path::new(workspace::LEGACY_DAEMON_SECRETS_PATH)
    );
}
