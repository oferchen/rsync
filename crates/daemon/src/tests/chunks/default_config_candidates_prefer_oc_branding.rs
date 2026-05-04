#[test]
fn default_config_candidates_prefer_oc_branding() {
    assert_eq!(
        Brand::Oc.config_path_candidate_strs(),
        [
            branding::OC_DAEMON_CONFIG_PATH,
            branding::LEGACY_DAEMON_CONFIG_PATH,
        ]
    );
}

