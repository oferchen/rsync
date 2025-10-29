use super::{Brand, *};

#[test]
fn version_metadata_matches_expected_constants() {
    let metadata = version_metadata();

    assert_eq!(metadata.program_name(), PROGRAM_NAME);
    assert_eq!(metadata.upstream_version(), UPSTREAM_BASE_VERSION);
    assert_eq!(metadata.rust_version(), RUST_VERSION);
    assert_eq!(metadata.protocol_version(), ProtocolVersion::NEWEST);
    assert_eq!(metadata.subprotocol_version(), SUBPROTOCOL_VERSION);
    assert_eq!(metadata.copyright_notice(), COPYRIGHT_NOTICE);
    assert_eq!(metadata.web_site(), WEB_SITE);
    assert_eq!(HIGHEST_PROTOCOL_VERSION, ProtocolVersion::NEWEST.as_u8());
}

#[test]
fn workspace_version_matches_package_version() {
    assert_eq!(RUST_VERSION, env!("CARGO_PKG_VERSION"));
}

#[test]
fn workspace_protocol_matches_latest() {
    assert_eq!(
        workspace::PROTOCOL_VERSION,
        u32::from(ProtocolVersion::NEWEST.as_u8())
    );
}

#[test]
fn sanitize_build_revision_trims_and_filters_values() {
    assert_eq!(sanitize_build_revision(Some(" 1a2b3c ")), "1a2b3c");
    assert_eq!(sanitize_build_revision(Some("\n\t")), "unknown");
    assert_eq!(sanitize_build_revision(Some("rev\nnext")), "rev");
    assert_eq!(sanitize_build_revision(Some("rev\r\nnext")), "rev");
    assert_eq!(sanitize_build_revision(Some("rev\u{7f}")), "unknown");
    assert_eq!(sanitize_build_revision(Some("rev\0")), "unknown");
    assert_eq!(sanitize_build_revision(None), "unknown");
}

#[test]
fn version_metadata_for_program_overrides_program_name() {
    let metadata = daemon_version_metadata();
    assert_eq!(metadata.program_name(), DAEMON_PROGRAM_NAME);
    assert_eq!(metadata.protocol_version(), ProtocolVersion::NEWEST);

    let branded = oc_version_metadata();
    assert_eq!(branded.program_name(), OC_PROGRAM_NAME);
    assert_eq!(branded.protocol_version(), ProtocolVersion::NEWEST);

    let branded_daemon = oc_daemon_version_metadata();
    assert_eq!(branded_daemon.program_name(), OC_DAEMON_PROGRAM_NAME);
    assert_eq!(branded_daemon.protocol_version(), ProtocolVersion::NEWEST);

    let via_brand = version_metadata_for_client_brand(Brand::Oc);
    assert_eq!(via_brand.program_name(), OC_PROGRAM_NAME);

    let via_brand_daemon = version_metadata_for_daemon_brand(Brand::Oc);
    assert_eq!(via_brand_daemon.program_name(), OC_DAEMON_PROGRAM_NAME);

    let custom = version_metadata_for_program("custom-rsync");
    assert_eq!(custom.program_name(), "custom-rsync");
    assert_eq!(custom.protocol_version(), ProtocolVersion::NEWEST);
}

#[test]
fn version_metadata_renders_standard_banner() {
    let metadata = version_metadata();
    let mut rendered = String::new();

    metadata
        .write_standard_banner(&mut rendered)
        .expect("writing to String cannot fail");

    let expected = format!(
        concat!(
            "rsync  version {rust_version} (revision/build #{build_revision})  protocol version {protocol}\n",
            "Copyright {copyright}\n",
            "Web site: {web_site}\n"
        ),
        rust_version = RUST_VERSION,
        build_revision = build_revision(),
        protocol = ProtocolVersion::NEWEST.as_u8(),
        copyright = COPYRIGHT_NOTICE,
        web_site = WEB_SITE,
    );

    assert_eq!(rendered, expected);
}
