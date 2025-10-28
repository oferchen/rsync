#![cfg(test)]

use super::*;

#[test]
fn program_names_are_consistent() {
    assert_eq!(client_program_name(), upstream_client_program_name());
    assert_eq!(daemon_program_name(), upstream_daemon_program_name());
    assert_eq!(oc_client_program_name(), OC_CLIENT_PROGRAM_NAME);
    assert_eq!(oc_daemon_program_name(), OC_DAEMON_PROGRAM_NAME);
}

#[test]
fn oc_paths_match_expected_locations() {
    assert_eq!(oc_daemon_config_dir(), Path::new(OC_DAEMON_CONFIG_DIR));
    assert_eq!(oc_daemon_config_path(), Path::new(OC_DAEMON_CONFIG_PATH));
    assert_eq!(oc_daemon_secrets_path(), Path::new(OC_DAEMON_SECRETS_PATH));
    assert_eq!(
        legacy_daemon_config_path(),
        Path::new(LEGACY_DAEMON_CONFIG_PATH)
    );
    assert_eq!(
        legacy_daemon_config_dir(),
        Path::new(LEGACY_DAEMON_CONFIG_DIR)
    );
    assert_eq!(
        legacy_daemon_secrets_path(),
        Path::new(LEGACY_DAEMON_SECRETS_PATH)
    );
}

#[test]
fn profile_helpers_align_with_functions() {
    assert_eq!(
        upstream_profile().client_program_name(),
        upstream_client_program_name()
    );
    assert_eq!(
        upstream_profile().daemon_program_name(),
        upstream_daemon_program_name()
    );
    assert_eq!(oc_profile().client_program_name(), oc_client_program_name());
    assert_eq!(oc_profile().daemon_program_name(), oc_daemon_program_name());
    assert_eq!(oc_profile().daemon_config_dir(), oc_daemon_config_dir());
    assert_eq!(oc_profile().daemon_config_path(), oc_daemon_config_path());
    assert_eq!(oc_profile().daemon_secrets_path(), oc_daemon_secrets_path());
    assert_eq!(
        upstream_profile().daemon_config_path(),
        legacy_daemon_config_path()
    );
    assert_eq!(
        upstream_profile().daemon_config_dir(),
        legacy_daemon_config_dir()
    );
    assert_eq!(
        upstream_profile().daemon_secrets_path(),
        legacy_daemon_secrets_path()
    );
}

#[test]
fn brand_detection_matches_program_names() {
    assert_eq!(brand_for_program_name("rsync"), Brand::Upstream);
    assert_eq!(brand_for_program_name("rsyncd"), Brand::Upstream);
    assert_eq!(brand_for_program_name("oc-rsync"), Brand::Oc);
    assert_eq!(brand_for_program_name("oc-rsyncd"), Brand::Oc);
    assert_eq!(brand_for_program_name("oc-rsync-3.4.1"), Brand::Oc);
    assert_eq!(brand_for_program_name("OC-RSYNCD_v2"), Brand::Oc);
    assert_eq!(brand_for_program_name("rsync-3.4.1"), Brand::Upstream);
}

#[test]
fn brand_profiles_expose_program_names() {
    let upstream = Brand::Upstream.profile();
    assert_eq!(upstream.client_program_name(), UPSTREAM_CLIENT_PROGRAM_NAME);
    assert_eq!(upstream.daemon_program_name(), UPSTREAM_DAEMON_PROGRAM_NAME);
    assert_eq!(
        upstream.daemon_config_dir(),
        Path::new(LEGACY_DAEMON_CONFIG_DIR)
    );

    let oc = Brand::Oc.profile();
    assert_eq!(oc.client_program_name(), OC_CLIENT_PROGRAM_NAME);
    assert_eq!(oc.daemon_program_name(), OC_DAEMON_PROGRAM_NAME);
    assert_eq!(oc.daemon_config_dir(), Path::new(OC_DAEMON_CONFIG_DIR));
}

#[test]
fn detect_brand_matches_invocation_argument() {
    let expected = std::env::current_exe()
        .ok()
        .and_then(|path| brand_for_program_path(&path))
        .unwrap_or_else(default_brand);
    assert_eq!(detect_brand(None), expected);
    assert_eq!(detect_brand(Some(OsStr::new("rsync"))), Brand::Upstream);
    assert_eq!(
        detect_brand(Some(OsStr::new("/usr/bin/oc-rsync"))),
        Brand::Oc
    );
    assert_eq!(detect_brand(Some(OsStr::new("oc-rsyncd"))), Brand::Oc);
    assert_eq!(detect_brand(Some(OsStr::new("OC-RSYNCD"))), Brand::Oc);
    assert_eq!(
        detect_brand(Some(OsStr::new("/usr/bin/oc-rsync-3.4.1"))),
        Brand::Oc
    );
    assert_eq!(
        detect_brand(Some(OsStr::new("/usr/local/bin/rsync-3.4.1"))),
        Brand::Upstream
    );
}

#[test]
fn config_search_orders_match_brand_expectations() {
    assert_eq!(
        Brand::Oc.config_path_candidate_strs(),
        [OC_DAEMON_CONFIG_PATH, LEGACY_DAEMON_CONFIG_PATH]
    );
    assert_eq!(
        Brand::Upstream.config_path_candidate_strs(),
        [LEGACY_DAEMON_CONFIG_PATH, OC_DAEMON_CONFIG_PATH]
    );

    let oc_paths = Brand::Oc.config_path_candidates();
    assert_eq!(oc_paths[0], Path::new(OC_DAEMON_CONFIG_PATH));
    assert_eq!(oc_paths[1], Path::new(LEGACY_DAEMON_CONFIG_PATH));
    let upstream_paths = Brand::Upstream.config_path_candidates();
    assert_eq!(upstream_paths[0], Path::new(LEGACY_DAEMON_CONFIG_PATH));
    assert_eq!(upstream_paths[1], Path::new(OC_DAEMON_CONFIG_PATH));
}

#[test]
fn config_directories_match_brand_profiles() {
    assert_eq!(
        Brand::Oc.daemon_config_dir(),
        Path::new(OC_DAEMON_CONFIG_DIR)
    );
    assert_eq!(Brand::Oc.daemon_config_dir_str(), OC_DAEMON_CONFIG_DIR);
    assert_eq!(
        Brand::Upstream.daemon_config_dir(),
        Path::new(LEGACY_DAEMON_CONFIG_DIR)
    );
    assert_eq!(
        Brand::Upstream.daemon_config_dir_str(),
        LEGACY_DAEMON_CONFIG_DIR
    );
}

#[test]
fn config_paths_match_brand_profiles() {
    assert_eq!(
        Brand::Oc.daemon_config_path(),
        Path::new(OC_DAEMON_CONFIG_PATH)
    );
    assert_eq!(
        Brand::Oc.daemon_config_path_str(),
        OC_DAEMON_CONFIG_PATH
    );
    assert_eq!(
        Brand::Upstream.daemon_config_path(),
        Path::new(LEGACY_DAEMON_CONFIG_PATH)
    );
    assert_eq!(
        Brand::Upstream.daemon_config_path_str(),
        LEGACY_DAEMON_CONFIG_PATH
    );
}

#[test]
fn secrets_search_orders_match_brand_expectations() {
    assert_eq!(
        Brand::Oc.secrets_path_candidate_strs(),
        [OC_DAEMON_SECRETS_PATH, LEGACY_DAEMON_SECRETS_PATH]
    );
    assert_eq!(
        Brand::Upstream.secrets_path_candidate_strs(),
        [LEGACY_DAEMON_SECRETS_PATH, OC_DAEMON_SECRETS_PATH]
    );

    let oc_paths = Brand::Oc.secrets_path_candidates();
    assert_eq!(oc_paths[0], Path::new(OC_DAEMON_SECRETS_PATH));
    assert_eq!(oc_paths[1], Path::new(LEGACY_DAEMON_SECRETS_PATH));
    let upstream_paths = Brand::Upstream.secrets_path_candidates();
    assert_eq!(upstream_paths[0], Path::new(LEGACY_DAEMON_SECRETS_PATH));
    assert_eq!(upstream_paths[1], Path::new(OC_DAEMON_SECRETS_PATH));

    assert_eq!(
        Brand::Oc.daemon_secrets_path(),
        Path::new(OC_DAEMON_SECRETS_PATH)
    );
    assert_eq!(
        Brand::Oc.daemon_secrets_path_str(),
        OC_DAEMON_SECRETS_PATH
    );
    assert_eq!(
        Brand::Upstream.daemon_secrets_path(),
        Path::new(LEGACY_DAEMON_SECRETS_PATH)
    );
    assert_eq!(
        Brand::Upstream.daemon_secrets_path_str(),
        LEGACY_DAEMON_SECRETS_PATH
    );
}

#[test]
fn brand_from_str_accepts_aliases() {
    assert_eq!(Brand::from_str("oc").unwrap(), Brand::Oc);
    assert_eq!(Brand::from_str("OC-RSYNCD").unwrap(), Brand::Oc);
    assert_eq!(Brand::from_str(" rsync-3.4.1 ").unwrap(), Brand::Upstream);
    assert_eq!(Brand::from_str("RSYNCD").unwrap(), Brand::Upstream);
}

#[test]
fn brand_from_str_rejects_unknown_values() {
    assert!(Brand::from_str("").is_err());
    assert!(Brand::from_str("unknown").is_err());
    assert!(Brand::from_str("ocrsync").is_err());
}

#[test]
fn brand_label_matches_expected() {
    assert_eq!(Brand::Oc.label(), "oc");
    assert_eq!(Brand::Upstream.label(), "upstream");
}

#[test]
fn brand_display_renders_label() {
    assert_eq!(Brand::Oc.to_string(), "oc");
    assert_eq!(Brand::Upstream.to_string(), "upstream");
}
