use super::{ACL_FROM_LABEL, UNKNOWN_FROM_LABEL, *};

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
