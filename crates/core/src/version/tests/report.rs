use super::*;
use crate::branding::Brand;
use crate::version::report::{
    default_checksum_algorithms, default_compress_algorithms, default_daemon_auth_algorithms,
};
use libc::{ino_t, off_t, time_t};
use std::mem;

#[test]
fn version_info_report_renders_default_report() {
    let config = VersionInfoConfig::default();
    let report = VersionInfoReport::new(config);
    let actual = report.human_readable();

    let bit_files = mem::size_of::<off_t>() * 8;
    let bit_inums = mem::size_of::<ino_t>() * 8;
    let bit_timestamps = mem::size_of::<time_t>() * 8;
    let bit_long_ints = mem::size_of::<i64>() * 8;
    let compiled_features_display = compiled_features_display();
    let compiled_features_text = if compiled_features_display.is_empty() {
        "none".to_owned()
    } else {
        compiled_features_display.to_string()
    };

    let build_info = build_info_line();
    assert!(actual.starts_with(&format!("rsync  version {RUST_VERSION}")));
    assert!(actual.contains(&format!(
        "    {bit_files}-bit files, {bit_inums}-bit inums, {bit_timestamps}-bit timestamps, {bit_long_ints}-bit long ints,"
    )));
    assert!(actual.contains(", symlinks,"));
    assert!(actual.contains(", symtimes,"));
    assert!(actual.contains(", hardlinks"));
    assert!(!actual.contains("no symlinks"));
    assert!(!actual.contains("no symtimes"));
    assert!(!actual.contains("no hardlinks"));
    assert!(actual.contains("IPv6, atimes"));
    assert!(actual.contains("optional secluded-args"));
    let compiled_line = format!("Compiled features:\n    {compiled_features_text}\n");
    assert!(actual.contains(&compiled_line));
    let build_info_line = format!("Build info:\n    {build_info}\n");
    assert!(actual.contains(&build_info_line));
    let checksum_algorithms = default_checksum_algorithms()
        .iter()
        .map(|algo| algo.as_ref())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(actual.contains(&format!("Checksum list:\n    {checksum_algorithms}\n")));

    let compress_algorithms = default_compress_algorithms()
        .iter()
        .map(|algo| algo.as_ref())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(actual.contains(&format!("Compress list:\n    {compress_algorithms}\n")));

    let daemon_auth_algorithms = default_daemon_auth_algorithms()
        .iter()
        .map(|algo| algo.as_ref())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(actual.contains(&format!(
        "Daemon auth list:\n    {daemon_auth_algorithms}\n"
    )));
    assert!(actual.ends_with(
        "rsync comes with ABSOLUTELY NO WARRANTY.  This is free software, and you\nare welcome to redistribute it under certain conditions.  See the GNU\nGeneral Public Licence for details.\n"
    ));
}

#[test]
fn version_info_report_allows_custom_lists() {
    let report = VersionInfoReport::new(VersionInfoConfig::default())
        .with_checksum_algorithms(["alpha"])
        .with_compress_algorithms(["beta"])
        .with_daemon_auth_algorithms(["gamma"]);

    let rendered = report.human_readable();

    assert!(rendered.contains("Checksum list:\n    alpha\n"));
    assert!(rendered.contains("Compress list:\n    beta\n"));
    assert!(rendered.contains("Daemon auth list:\n    gamma\n"));
    assert!(rendered.contains("Compiled features:\n"));
    let build_info = build_info_line();
    assert!(rendered.contains(&format!("Build info:\n    {build_info}\n")));
}

#[test]
fn version_info_report_with_program_name_updates_banner() {
    let report =
        VersionInfoReport::new(VersionInfoConfig::default()).with_program_name(DAEMON_PROGRAM_NAME);
    let banner = report.metadata().standard_banner();

    assert!(banner.starts_with("rsyncd  version"));
}

#[test]
fn version_info_report_with_client_brand_updates_banner() {
    let report = VersionInfoReport::new(VersionInfoConfig::default()).with_client_brand(Brand::Oc);
    let banner = report.metadata().standard_banner();

    assert!(banner.starts_with("oc-rsync  version"));
}

#[test]
fn version_info_report_with_daemon_brand_updates_banner() {
    let report = VersionInfoReport::new(VersionInfoConfig::default()).with_daemon_brand(Brand::Oc);
    let banner = report.metadata().standard_banner();

    assert!(banner.starts_with("oc-rsyncd  version"));
}

#[test]
fn version_info_report_for_client_brand_matches_builder() {
    let expected =
        VersionInfoReport::new(VersionInfoConfig::default()).with_client_brand(Brand::Oc);
    let actual = VersionInfoReport::for_client_brand(Brand::Oc);

    assert_eq!(actual.human_readable(), expected.human_readable());
}

#[test]
fn version_info_report_for_daemon_brand_matches_builder() {
    let expected =
        VersionInfoReport::new(VersionInfoConfig::default()).with_daemon_brand(Brand::Oc);
    let actual = VersionInfoReport::for_daemon_brand(Brand::Oc);

    assert_eq!(actual.human_readable(), expected.human_readable());
}

#[test]
fn version_info_report_for_brand_with_config_matches_builder() {
    let config = VersionInfoConfig {
        supports_ipv6: false,
        supports_symlinks: false,
        ..VersionInfoConfig::default()
    };
    let expected = VersionInfoReport::new(config).with_client_brand(Brand::Upstream);

    let alternate = VersionInfoConfig {
        supports_ipv6: false,
        supports_symlinks: false,
        ..VersionInfoConfig::default()
    };
    let actual = VersionInfoReport::for_client_brand_with_config(alternate, Brand::Upstream);

    assert_eq!(actual.human_readable(), expected.human_readable());
}

#[test]
fn version_info_report_includes_compiled_feature_section() {
    let report = VersionInfoReport::new(VersionInfoConfig::default());
    let rendered = report.human_readable();

    let compiled_features_display = compiled_features_display();
    let expected_line = if compiled_features_display.is_empty() {
        "Compiled features:\n    none\n".to_owned()
    } else {
        format!("Compiled features:\n    {compiled_features_display}\n")
    };

    assert!(rendered.contains(&expected_line));
    let build_info = build_info_line();
    assert!(rendered.contains(&format!("Build info:\n    {build_info}\n")));
}

#[test]
fn version_info_config_builder_supports_chaining() {
    let config = VersionInfoConfig::builder()
        .supports_socketpairs(true)
        .supports_symlinks(true)
        .supports_symtimes(true)
        .supports_hardlinks(true)
        .supports_hardlink_specials(true)
        .supports_hardlink_symlinks(true)
        .supports_ipv6(true)
        .supports_atimes(true)
        .supports_batchfiles(true)
        .supports_inplace(true)
        .supports_append(true)
        .supports_acls(true)
        .supports_xattrs(true)
        .secluded_args_mode(SecludedArgsMode::Default)
        .supports_iconv(true)
        .supports_prealloc(true)
        .supports_stop_at(true)
        .supports_crtimes(true)
        .supports_simd_roll(true)
        .supports_asm_roll(true)
        .supports_openssl_crypto(true)
        .supports_asm_md5(true)
        .build();

    assert!(config.supports_socketpairs);
    assert!(config.supports_symlinks);
    assert!(config.supports_symtimes);
    assert!(config.supports_hardlinks);
    assert!(config.supports_hardlink_specials);
    assert!(config.supports_hardlink_symlinks);
    assert!(config.supports_ipv6);
    assert!(config.supports_atimes);
    assert!(config.supports_batchfiles);
    assert!(config.supports_inplace);
    assert!(config.supports_append);
    assert_eq!(config.supports_acls, cfg!(feature = "acl"));
    assert_eq!(config.supports_xattrs, cfg!(feature = "xattr"));
    assert_eq!(config.secluded_args_mode, SecludedArgsMode::Default);
    assert_eq!(config.supports_iconv, cfg!(feature = "iconv"));
    assert!(config.supports_prealloc);
    assert!(config.supports_stop_at);
    assert!(config.supports_crtimes);
    assert!(config.supports_simd_roll);
    assert!(config.supports_asm_roll);
    assert!(config.supports_openssl_crypto);
    assert!(config.supports_asm_md5);
}

#[test]
fn version_info_config_to_builder_round_trips() {
    let original = VersionInfoConfig::builder()
        .supports_socketpairs(true)
        .supports_ipv6(true)
        .supports_prealloc(true)
        .build();

    let updated = original
        .to_builder()
        .supports_socketpairs(false)
        .supports_ipv6(false)
        .build();

    assert!(original.supports_socketpairs);
    assert!(original.supports_ipv6);
    assert!(original.supports_prealloc);
    assert!(!updated.supports_socketpairs);
    assert!(!updated.supports_ipv6);
    assert!(updated.supports_prealloc);
}
