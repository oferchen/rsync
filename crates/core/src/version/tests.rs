use super::*;
use crate::branding::Brand;
use core::str::FromStr;

const ACL_FROM_LABEL: Option<CompiledFeature> = CompiledFeature::from_label("ACLs");
const UNKNOWN_FROM_LABEL: Option<CompiledFeature> = CompiledFeature::from_label("unknown");

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

#[test]
fn compiled_features_match_cfg_flags() {
    let features = compiled_features();
    let mut bitmap_from_features = 0u8;

    for feature in &features {
        bitmap_from_features |= feature.bit();
        assert!(feature.is_enabled());
    }

    for feature in CompiledFeature::ALL {
        assert_eq!(features.contains(&feature), feature.is_enabled());
    }

    assert_eq!(bitmap_from_features, COMPILED_FEATURE_BITMAP);
    assert_eq!(
        features.len(),
        COMPILED_FEATURE_BITMAP.count_ones() as usize
    );
}

#[test]
fn secluded_args_mode_labels_round_trip() {
    assert_eq!(
        SecludedArgsMode::from_label(SecludedArgsMode::Optional.label()),
        Some(SecludedArgsMode::Optional)
    );
    assert_eq!(
        SecludedArgsMode::from_label(SecludedArgsMode::Default.label()),
        Some(SecludedArgsMode::Default)
    );
    assert!(SecludedArgsMode::from_label("custom secluded-args").is_none());
}

#[test]
fn secluded_args_mode_display_matches_label() {
    assert_eq!(
        SecludedArgsMode::Optional.to_string(),
        SecludedArgsMode::Optional.label()
    );
    assert_eq!(
        SecludedArgsMode::Default.to_string(),
        SecludedArgsMode::Default.label()
    );
}

#[test]
fn secluded_args_mode_from_str_rejects_unknown_values() {
    assert_eq!(
        SecludedArgsMode::from_str("default secluded-args"),
        Ok(SecludedArgsMode::Default)
    );
    assert_eq!(
        SecludedArgsMode::from_str("optional secluded-args"),
        Ok(SecludedArgsMode::Optional)
    );
    assert!(SecludedArgsMode::from_str("disabled secluded-args").is_err());
}

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
    assert!(actual.starts_with(&format!("rsync  version {}", RUST_VERSION)));
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
    let compiled_line = format!("Compiled features:\n    {}\n", compiled_features_text);
    assert!(actual.contains(&compiled_line));
    let build_info_line = format!("Build info:\n    {}\n", build_info);
    assert!(actual.contains(&build_info_line));
    let checksum_algorithms = default_checksum_algorithms()
        .iter()
        .map(|algo| algo.as_ref())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(actual.contains(&format!("Checksum list:\n    {}\n", checksum_algorithms)));

    let compress_algorithms = default_compress_algorithms()
        .iter()
        .map(|algo| algo.as_ref())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(actual.contains(&format!("Compress list:\n    {}\n", compress_algorithms)));

    let daemon_auth_algorithms = default_daemon_auth_algorithms()
        .iter()
        .map(|algo| algo.as_ref())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(actual.contains(&format!(
        "Daemon auth list:\n    {}\n",
        daemon_auth_algorithms
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
    assert!(rendered.contains(&format!("Build info:\n    {}\n", build_info)));
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
fn version_info_report_includes_compiled_feature_section() {
    let report = VersionInfoReport::new(VersionInfoConfig::default());
    let rendered = report.human_readable();

    let compiled_features_display = compiled_features_display();
    let expected_line = if compiled_features_display.is_empty() {
        "Compiled features:\n    none\n".to_owned()
    } else {
        format!("Compiled features:\n    {}\n", compiled_features_display)
    };

    assert!(rendered.contains(&expected_line));
    let build_info = build_info_line();
    assert!(rendered.contains(&format!("Build info:\n    {}\n", build_info)));
}

#[test]
fn feature_labels_align_with_display() {
    for feature in CompiledFeature::ALL {
        assert_eq!(feature.label(), feature.to_string());
    }
}

#[test]
fn compiled_feature_labels_reflect_active_features() {
    let labels = compiled_feature_labels();

    for feature in CompiledFeature::ALL {
        assert_eq!(labels.contains(&feature.label()), feature.is_enabled());
    }
}

#[test]
fn compiled_features_display_reflects_active_features() {
    let display = compiled_features_display();
    assert_eq!(display.features(), compiled_features().as_slice());
    assert_eq!(display.is_empty(), compiled_features().is_empty());
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

#[test]
fn compiled_features_display_formats_space_separated_list() {
    let display = CompiledFeaturesDisplay::new(vec![
        CompiledFeature::Acl,
        CompiledFeature::Xattr,
        CompiledFeature::Iconv,
    ]);

    assert_eq!(display.to_string(), "ACLs xattrs iconv");
}

#[test]
fn compiled_features_display_into_iter_exposes_features() {
    let mut display = CompiledFeaturesDisplay::new(vec![
        CompiledFeature::Acl,
        CompiledFeature::Xattr,
        CompiledFeature::Iconv,
    ]);

    let from_ref: Vec<_> = (&display).into_iter().copied().collect();
    assert_eq!(from_ref, display.features());

    let from_mut: Vec<_> = (&mut display).into_iter().map(|feature| *feature).collect();
    assert_eq!(from_mut, display.features());

    let owned: Vec<_> = display.clone().into_iter().collect();
    assert_eq!(owned, display.features());
}

#[test]
fn compiled_features_display_handles_empty_list() {
    let display = CompiledFeaturesDisplay::new(Vec::new());

    assert!(display.is_empty());
    assert!(display.to_string().is_empty());
}

#[test]
fn compiled_features_display_len_and_iter_match_features() {
    let display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl, CompiledFeature::Xattr]);

    assert_eq!(display.len(), display.features().len());
    let collected: Vec<_> = display.iter().copied().collect();
    assert_eq!(collected, display.features());

    let empty = CompiledFeaturesDisplay::new(Vec::new());
    assert_eq!(empty.len(), 0);
    assert!(empty.iter().next().is_none());
}

#[test]
fn compiled_features_display_collect_from_iterator() {
    let display: CompiledFeaturesDisplay = [CompiledFeature::Acl, CompiledFeature::Iconv]
        .into_iter()
        .collect();

    assert_eq!(
        display.features(),
        &[CompiledFeature::Acl, CompiledFeature::Iconv]
    );
}

#[test]
fn compiled_features_display_extend_supports_owned_and_borrowed() {
    let mut display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
    display.extend([CompiledFeature::Xattr]);

    assert_eq!(
        display.features(),
        &[CompiledFeature::Acl, CompiledFeature::Xattr]
    );

    let borrowed = [CompiledFeature::Zstd, CompiledFeature::SdNotify];
    display.extend(borrowed.iter());

    assert_eq!(
        display.features(),
        &[
            CompiledFeature::Acl,
            CompiledFeature::Xattr,
            CompiledFeature::Zstd,
            CompiledFeature::SdNotify,
        ]
    );
}

#[test]
fn compiled_features_display_retain_filters_in_place() {
    let mut display = CompiledFeaturesDisplay::new(vec![
        CompiledFeature::Acl,
        CompiledFeature::Xattr,
        CompiledFeature::Iconv,
    ]);

    display.retain(|feature| !matches!(feature, CompiledFeature::Xattr));

    assert_eq!(
        display.features(),
        &[CompiledFeature::Acl, CompiledFeature::Iconv]
    );
}

#[test]
fn compiled_features_display_retain_can_drop_all_features() {
    let mut display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
    display.retain(|_| false);

    assert!(display.is_empty());
    assert!(display.features().is_empty());
}

#[test]
fn compiled_feature_from_label_matches_variants() {
    assert_eq!(
        CompiledFeature::from_label("ACLs"),
        Some(CompiledFeature::Acl)
    );
    assert_eq!(
        CompiledFeature::from_label("xattrs"),
        Some(CompiledFeature::Xattr)
    );
    assert_eq!(
        CompiledFeature::from_label("zstd"),
        Some(CompiledFeature::Zstd)
    );
    assert_eq!(
        CompiledFeature::from_label("iconv"),
        Some(CompiledFeature::Iconv)
    );
    assert_eq!(
        CompiledFeature::from_label("sd-notify"),
        Some(CompiledFeature::SdNotify)
    );
    assert_eq!(CompiledFeature::from_label("unknown"), None);
}

#[test]
fn from_label_const_results_match_runtime() {
    assert_eq!(ACL_FROM_LABEL, Some(CompiledFeature::Acl));
    assert!(UNKNOWN_FROM_LABEL.is_none());
}

#[test]
fn compiled_feature_from_str_uses_canonical_labels() {
    for feature in CompiledFeature::ALL {
        let parsed = feature
            .label()
            .parse::<CompiledFeature>()
            .expect("label parses into feature");
        assert_eq!(parsed, feature);
    }

    let err = "invalid".parse::<CompiledFeature>().unwrap_err();
    assert_eq!(err, ParseCompiledFeatureError);
    assert_eq!(err.to_string(), "unknown compiled feature label");
}

#[test]
fn compiled_features_iter_matches_collected_set() {
    let via_iter: Vec<_> = compiled_features_iter().collect();
    assert_eq!(via_iter, compiled_features());
}

#[test]
fn compiled_features_iter_rev_matches_reverse_order() {
    let forward: Vec<_> = compiled_features_iter().collect();
    let mut expected = forward.clone();
    expected.reverse();

    let backward: Vec<_> = compiled_features_iter().rev().collect();
    assert_eq!(backward, expected);
}

#[test]
fn compiled_features_iter_is_fused_and_updates_len() {
    let mut iter = compiled_features_iter();
    let (lower, upper) = iter.size_hint();
    assert_eq!(Some(lower), upper);
    let expected = compiled_features();
    assert_eq!(lower, expected.len());
    assert_eq!(iter.len(), expected.len());
    assert_eq!(iter.len(), lower);

    while iter.next().is_some() {
        let (lower, upper) = iter.size_hint();
        assert_eq!(Some(lower), upper);
        assert_eq!(iter.len(), lower);
    }

    assert_eq!(iter.next(), None);
    assert_eq!(iter.next(), None);
    assert_eq!(iter.len(), 0);

    let mut rev_iter = compiled_features_iter();
    while rev_iter.next_back().is_some() {
        let (lower, upper) = rev_iter.size_hint();
        assert_eq!(Some(lower), upper);
        assert_eq!(rev_iter.len(), lower);
    }

    assert_eq!(rev_iter.next_back(), None);
    assert_eq!(rev_iter.len(), 0);
}

#[test]
fn compiled_features_iter_next_back_matches_reverse_collection() {
    let mut iter = compiled_features_iter();
    let mut reversed = Vec::new();

    while let Some(feature) = iter.next_back() {
        reversed.push(feature);
    }

    let expected: Vec<_> = compiled_features().into_iter().rev().collect();
    assert_eq!(reversed, expected);
}

#[test]
fn compiled_features_iter_supports_mixed_direction_iteration() {
    let expected = compiled_features();
    let mut iter = compiled_features_iter();

    let front = iter.next();
    let back = iter.next_back();
    let mut remainder: Vec<_> = iter.collect();

    let mut reconstructed = Vec::new();
    if let Some(feature) = front {
        reconstructed.push(feature);
    }

    reconstructed.append(&mut remainder);

    if let Some(feature) = back {
        reconstructed.push(feature);
    }

    assert_eq!(reconstructed, expected);
}

#[test]
fn compiled_features_static_matches_dynamic_collection() {
    let static_view = compiled_features_static();
    let collected = compiled_features();

    assert_eq!(static_view.as_slice(), collected.as_slice());
    assert_eq!(static_view.len(), collected.len());
    assert_eq!(static_view.is_empty(), collected.is_empty());
    assert_eq!(static_view.bitmap(), COMPILED_FEATURE_BITMAP);

    for feature in CompiledFeature::ALL {
        assert_eq!(static_view.contains(feature), feature.is_enabled());
    }
}

#[test]
fn compiled_features_static_iterator_preserves_ordering() {
    let static_view = compiled_features_static();
    let from_iter: Vec<_> = static_view.iter().collect();

    assert_eq!(from_iter.as_slice(), static_view.as_slice());

    let mut iter = static_view.iter();
    let front = iter.next();
    let back = iter.next_back();
    let mut remainder: Vec<_> = iter.collect();

    let mut reconstructed = Vec::new();
    if let Some(feature) = front {
        reconstructed.push(feature);
    }

    reconstructed.append(&mut remainder);

    if let Some(feature) = back {
        reconstructed.push(feature);
    }

    assert_eq!(reconstructed.as_slice(), static_view.as_slice());
}
