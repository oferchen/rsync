mod bitmap;
mod compiled;
mod display;
mod iter;
mod static_set;

pub use bitmap::COMPILED_FEATURE_BITMAP;
pub use compiled::{CompiledFeature, ParseCompiledFeatureError};
pub use display::CompiledFeaturesDisplay;
pub use iter::CompiledFeaturesIter;
pub use static_set::{
    COMPILED_FEATURES_STATIC, StaticCompiledFeatures, StaticCompiledFeaturesIter,
};

/// Returns an iterator over the optional features compiled into the current build.
#[must_use]
pub const fn compiled_features_iter() -> CompiledFeaturesIter {
    CompiledFeaturesIter::new()
}

/// Returns the set of optional features compiled into the current build.
///
/// This function inspects the Cargo feature flags that were enabled at compile
/// time and returns a vector of all optional capabilities available in this
/// binary.
///
/// # Returns
///
/// A vector containing all [`CompiledFeature`] variants that were enabled at
/// build time. The list may be empty if no optional features are compiled in.
///
/// # Examples
///
/// ```
/// use core::version::{compiled_features, CompiledFeature};
///
/// let features = compiled_features();
/// #[cfg(feature = "xattr")]
/// assert!(features.contains(&CompiledFeature::Xattr));
/// ```
#[must_use]
pub fn compiled_features() -> Vec<CompiledFeature> {
    compiled_features_static().as_slice().to_vec()
}

/// Returns a zero-allocation view over the compiled feature set.
///
/// This function provides a const-friendly way to access the compiled features
/// without allocating a vector. Useful for performance-sensitive contexts or
/// const evaluation.
///
/// # Returns
///
/// A static reference to the compiled features that can be used in const
/// contexts.
///
/// # Examples
///
/// ```
/// use core::version::compiled_features_static;
///
/// const FEATURES: &core::version::StaticCompiledFeatures = compiled_features_static();
/// ```
#[must_use]
pub const fn compiled_features_static() -> &'static StaticCompiledFeatures {
    &COMPILED_FEATURES_STATIC
}

/// Convenience helper that exposes the labels for each compiled feature.
///
/// Returns a vector of human-readable feature labels suitable for display in
/// version banners and diagnostic messages.
///
/// # Returns
///
/// A vector of string labels (e.g., `"ACLs"`, `"xattrs"`, `"zstd"`) for each
/// compiled feature.
///
/// # Examples
///
/// ```
/// use core::version::compiled_feature_labels;
///
/// let labels = compiled_feature_labels();
/// for label in &labels {
///     println!("Feature available: {}", label);
/// }
/// ```
#[must_use]
pub fn compiled_feature_labels() -> Vec<&'static str> {
    compiled_features_iter()
        .map(CompiledFeature::label)
        .collect()
}

/// Returns a [`CompiledFeaturesDisplay`] reflecting the active feature set.
///
/// This provides a Display-friendly wrapper around the compiled features for
/// easy rendering in version output and diagnostics.
///
/// # Returns
///
/// A [`CompiledFeaturesDisplay`] that implements [`std::fmt::Display`] and can
/// be formatted directly into strings.
///
/// # Examples
///
/// ```
/// use core::version::compiled_features_display;
///
/// let display = compiled_features_display();
/// println!("Compiled features: {}", display);
/// ```
#[must_use]
pub fn compiled_features_display() -> CompiledFeaturesDisplay {
    CompiledFeaturesDisplay::new(compiled_features())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_feature_all_has_five_variants() {
        assert_eq!(CompiledFeature::ALL.len(), 5);
    }

    #[test]
    fn compiled_feature_labels_are_correct() {
        assert_eq!(CompiledFeature::Acl.label(), "ACLs");
        assert_eq!(CompiledFeature::Xattr.label(), "xattrs");
        assert_eq!(CompiledFeature::Zstd.label(), "zstd");
        assert_eq!(CompiledFeature::Iconv.label(), "iconv");
        assert_eq!(CompiledFeature::SdNotify.label(), "sd-notify");
    }

    #[test]
    fn compiled_feature_from_label_parses_correctly() {
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
    }

    #[test]
    fn compiled_feature_from_label_returns_none_for_unknown() {
        assert!(CompiledFeature::from_label("unknown").is_none());
        assert!(CompiledFeature::from_label("").is_none());
        assert!(CompiledFeature::from_label("acl").is_none()); // case-sensitive
    }

    #[test]
    fn compiled_feature_descriptions_are_not_empty() {
        for feature in CompiledFeature::ALL {
            assert!(!feature.description().is_empty());
        }
    }

    #[test]
    fn compiled_feature_display_shows_label() {
        assert_eq!(format!("{}", CompiledFeature::Acl), "ACLs");
        assert_eq!(format!("{}", CompiledFeature::Zstd), "zstd");
    }

    #[test]
    fn compiled_feature_from_str_parses_correctly() {
        let acl: CompiledFeature = "ACLs".parse().unwrap();
        assert_eq!(acl, CompiledFeature::Acl);
        let zstd: CompiledFeature = "zstd".parse().unwrap();
        assert_eq!(zstd, CompiledFeature::Zstd);
    }

    #[test]
    fn compiled_feature_from_str_returns_error_for_unknown() {
        let result: Result<CompiledFeature, _> = "unknown".parse();
        assert!(result.is_err());
    }

    #[test]
    fn compiled_feature_bits_are_distinct() {
        let mut seen = 0u8;
        for feature in CompiledFeature::ALL {
            let bit = feature.bit();
            assert_eq!(seen & bit, 0, "bit should not be reused");
            seen |= bit;
        }
    }

    #[test]
    fn compiled_feature_clone_equals_original() {
        let feature = CompiledFeature::Acl;
        assert_eq!(feature.clone(), feature);
    }

    #[test]
    fn compiled_feature_hash_consistency() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(CompiledFeature::Acl);
        assert!(set.contains(&CompiledFeature::Acl));
        assert!(!set.contains(&CompiledFeature::Xattr));
    }

    #[test]
    fn static_compiled_features_default_same_as_new() {
        let default = StaticCompiledFeatures::default();
        let new = *compiled_features_static();
        assert_eq!(default, new);
    }

    #[test]
    fn static_compiled_features_bitmap_matches_global() {
        let static_features = compiled_features_static();
        assert_eq!(static_features.bitmap(), COMPILED_FEATURE_BITMAP);
    }

    #[test]
    fn static_compiled_features_len_matches_bitmap_population() {
        let static_features = compiled_features_static();
        assert_eq!(
            static_features.len(),
            COMPILED_FEATURE_BITMAP.count_ones() as usize
        );
    }

    #[test]
    fn static_compiled_features_as_slice_length_matches_len() {
        let static_features = compiled_features_static();
        assert_eq!(static_features.as_slice().len(), static_features.len());
    }

    #[test]
    fn static_compiled_features_as_ref_equals_as_slice() {
        let static_features = compiled_features_static();
        let as_ref: &[CompiledFeature] = static_features.as_ref();
        assert_eq!(as_ref, static_features.as_slice());
    }

    #[test]
    fn static_compiled_features_is_empty_consistent_with_len() {
        let static_features = compiled_features_static();
        assert_eq!(static_features.is_empty(), static_features.is_empty());
    }

    #[test]
    fn static_compiled_features_contains_matches_is_enabled() {
        let static_features = compiled_features_static();
        for feature in CompiledFeature::ALL {
            assert_eq!(static_features.contains(feature), feature.is_enabled());
        }
    }

    #[test]
    fn static_compiled_features_iter_yields_correct_count() {
        let static_features = compiled_features_static();
        let count = static_features.iter().count();
        assert_eq!(count, static_features.len());
    }

    #[test]
    fn static_compiled_features_iter_is_exact_size() {
        let static_features = compiled_features_static();
        let iter = static_features.iter();
        assert_eq!(iter.len(), static_features.len());
    }

    #[test]
    fn static_compiled_features_iter_size_hint_is_exact() {
        let static_features = compiled_features_static();
        let iter = static_features.iter();
        let (lower, upper) = iter.size_hint();
        assert_eq!(lower, static_features.len());
        assert_eq!(upper, Some(static_features.len()));
    }

    #[test]
    fn static_compiled_features_iter_double_ended() {
        let static_features = compiled_features_static();
        if !static_features.is_empty() {
            let mut iter = static_features.iter();
            let first = iter.next();
            let last = iter.next_back();
            if static_features.len() > 1 {
                assert_ne!(first, last);
            } else {
                assert!(last.is_none());
            }
        }
    }

    #[test]
    fn static_compiled_features_into_iter() {
        let static_features = compiled_features_static();
        let collected: Vec<_> = static_features.into_iter().collect();
        assert_eq!(collected.len(), static_features.len());
    }

    #[test]
    fn compiled_features_iter_matches_static() {
        let iter_count = compiled_features_iter().count();
        let static_count = compiled_features_static().len();
        assert_eq!(iter_count, static_count);
    }

    #[test]
    fn compiled_features_iter_is_exact_size() {
        let iter = compiled_features_iter();
        let expected_len = iter.len();
        let actual_len = iter.count();
        assert_eq!(expected_len, actual_len);
    }

    #[test]
    fn compiled_features_iter_size_hint_is_exact() {
        let iter = compiled_features_iter();
        let len = iter.len();
        let (lower, upper) = iter.size_hint();
        assert_eq!(lower, len);
        assert_eq!(upper, Some(len));
    }

    #[test]
    fn compiled_features_iter_double_ended() {
        let mut iter = compiled_features_iter();
        if iter.len() > 0 {
            let first = iter.next();
            let last = iter.next_back();
            if iter.len() > 0 {
                assert!(first.is_some());
                assert!(last.is_some());
            }
        }
    }

    #[test]
    fn compiled_features_vec_matches_iter() {
        let vec = compiled_features();
        let from_iter: Vec<_> = compiled_features_iter().collect();
        assert_eq!(vec, from_iter);
    }

    #[test]
    fn compiled_feature_labels_matches_features() {
        let labels = compiled_feature_labels();
        let features = compiled_features();
        assert_eq!(labels.len(), features.len());
        for (label, feature) in labels.iter().zip(features.iter()) {
            assert_eq!(*label, feature.label());
        }
    }

    #[test]
    fn compiled_features_display_new_stores_features() {
        let features = vec![CompiledFeature::Acl, CompiledFeature::Zstd];
        let display = CompiledFeaturesDisplay::new(features.clone());
        assert_eq!(display.features(), features.as_slice());
    }

    #[test]
    fn compiled_features_display_len_matches_features() {
        let display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
        assert_eq!(display.len(), 1);
    }

    #[test]
    fn compiled_features_display_is_empty_for_empty() {
        let display = CompiledFeaturesDisplay::new(vec![]);
        assert!(display.is_empty());
    }

    #[test]
    fn compiled_features_display_not_empty_with_features() {
        let display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
        assert!(!display.is_empty());
    }

    #[test]
    fn compiled_features_display_iter() {
        let features = vec![CompiledFeature::Acl, CompiledFeature::Xattr];
        let display = CompiledFeaturesDisplay::new(features.clone());
        let collected: Vec<_> = display.iter().copied().collect();
        assert_eq!(collected, features);
    }

    #[test]
    fn compiled_features_display_retain() {
        let mut display = CompiledFeaturesDisplay::new(vec![
            CompiledFeature::Acl,
            CompiledFeature::Xattr,
            CompiledFeature::Zstd,
        ]);
        display.retain(|f| *f != CompiledFeature::Xattr);
        assert_eq!(display.len(), 2);
        assert!(!display.features().contains(&CompiledFeature::Xattr));
    }

    #[test]
    fn compiled_features_display_format_single() {
        let display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
        assert_eq!(format!("{display}"), "ACLs");
    }

    #[test]
    fn compiled_features_display_format_multiple() {
        let display =
            CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl, CompiledFeature::Zstd]);
        assert_eq!(format!("{display}"), "ACLs zstd");
    }

    #[test]
    fn compiled_features_display_format_empty() {
        let display = CompiledFeaturesDisplay::new(vec![]);
        assert_eq!(format!("{display}"), "");
    }

    #[test]
    fn compiled_features_display_into_iter() {
        let display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
        let collected: Vec<_> = display.into_iter().collect();
        assert_eq!(collected, vec![CompiledFeature::Acl]);
    }

    #[test]
    fn compiled_features_display_ref_into_iter() {
        let display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
        let collected: Vec<_> = (&display).into_iter().copied().collect();
        assert_eq!(collected, vec![CompiledFeature::Acl]);
    }

    #[test]
    fn compiled_features_display_mut_ref_into_iter() {
        let mut display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
        let collected: Vec<_> = (&mut display).into_iter().map(|f| *f).collect();
        assert_eq!(collected, vec![CompiledFeature::Acl]);
    }

    #[test]
    fn compiled_features_display_from_iter() {
        let features = vec![CompiledFeature::Acl, CompiledFeature::Zstd];
        let display: CompiledFeaturesDisplay = features.clone().into_iter().collect();
        assert_eq!(display.features(), features.as_slice());
    }

    #[test]
    fn compiled_features_display_extend() {
        let mut display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
        display.extend(vec![CompiledFeature::Zstd]);
        assert_eq!(display.len(), 2);
    }

    #[test]
    fn compiled_features_display_extend_ref() {
        let mut display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
        display.extend(&[CompiledFeature::Zstd]);
        assert_eq!(display.len(), 2);
    }

    #[test]
    fn compiled_features_display_default_is_empty() {
        let display = CompiledFeaturesDisplay::default();
        assert!(display.is_empty());
    }

    #[test]
    fn compiled_features_display_clone_equals() {
        let display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
        assert_eq!(display.clone(), display);
    }

    #[test]
    fn compiled_features_display_function_returns_active_set() {
        let display = compiled_features_display();
        let features = compiled_features();
        assert_eq!(display.features(), features.as_slice());
    }

    #[test]
    fn parse_compiled_feature_error_display() {
        let error = ParseCompiledFeatureError;
        let msg = format!("{error}");
        assert!(msg.contains("unknown"));
    }
}
