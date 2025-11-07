#[test]
fn builder_collects_arguments() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            OsString::from("/tmp/rsyncd.conf"),
        ])
        .build();

    assert_eq!(
        config.arguments(),
        &[
            OsString::from("--config"),
            OsString::from("/tmp/rsyncd.conf")
        ]
    );
    assert!(config.has_runtime_request());
    assert_eq!(config.brand(), Brand::Oc);
}

