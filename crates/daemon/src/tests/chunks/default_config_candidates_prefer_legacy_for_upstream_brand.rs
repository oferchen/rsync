#[test]
fn default_config_candidates_prefer_legacy_for_upstream_brand() {
    assert_eq!(
        Brand::Upstream.config_path_candidate_strs(),
        [
            branding::LEGACY_DAEMON_CONFIG_PATH,
            branding::OC_DAEMON_CONFIG_PATH,
        ]
    );
}

