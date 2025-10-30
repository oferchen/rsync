#[test]
fn builder_allows_brand_override() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .brand(Brand::Upstream)
        .arguments([OsString::from("--daemon")])
        .build();

    assert_eq!(config.brand(), Brand::Upstream);
    assert_eq!(config.arguments(), &[OsString::from("--daemon")]);
}

