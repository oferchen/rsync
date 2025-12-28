use std::fmt;
use std::iter::{FromIterator, FusedIterator};
use std::str::FromStr;
use std::vec::Vec;

use thiserror::Error;

const COMPILED_FEATURE_COUNT: usize = CompiledFeature::ALL.len();

const ACL_FEATURE_BIT: u8 = 1 << 0;
const XATTR_FEATURE_BIT: u8 = 1 << 1;
const ZSTD_FEATURE_BIT: u8 = 1 << 2;
const ICONV_FEATURE_BIT: u8 = 1 << 3;
const SD_NOTIFY_FEATURE_BIT: u8 = 1 << 4;

/// Bitmap describing the optional features compiled into this build.
#[doc(alias = "--version")]
pub const COMPILED_FEATURE_BITMAP: u8 = {
    let mut bitmap = 0u8;

    if cfg!(feature = "acl") {
        bitmap |= ACL_FEATURE_BIT;
    }

    if cfg!(feature = "xattr") {
        bitmap |= XATTR_FEATURE_BIT;
    }

    if cfg!(feature = "zstd") {
        bitmap |= ZSTD_FEATURE_BIT;
    }

    if cfg!(feature = "iconv") {
        bitmap |= ICONV_FEATURE_BIT;
    }

    if cfg!(feature = "sd-notify") {
        bitmap |= SD_NOTIFY_FEATURE_BIT;
    }

    bitmap
};

/// Optional capabilities that may be compiled into the binary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompiledFeature {
    /// POSIX ACL support negotiated via `-A/--acls`.
    #[doc(alias = "--acls")]
    #[doc(alias = "-A")]
    Acl,
    /// Extended attribute propagation negotiated via `-X/--xattrs`.
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    Xattr,
    /// Zstandard compression available through `--compress` variants.
    #[doc(alias = "--compress")]
    #[doc(alias = "--zstd")]
    Zstd,
    /// Iconv-based character-set conversion support.
    #[doc(alias = "--iconv")]
    Iconv,
    /// `sd_notify` integration for the daemon systemd unit.
    #[doc(alias = "sd_notify")]
    SdNotify,
}

impl CompiledFeature {
    const fn label_eq(label: &str, expected: &str) -> bool {
        let lhs = label.as_bytes();
        let rhs = expected.as_bytes();

        if lhs.len() != rhs.len() {
            return false;
        }

        let mut index = 0;
        while index < lhs.len() {
            if lhs[index] != rhs[index] {
                return false;
            }
            index += 1;
        }

        true
    }

    /// Canonical ordering of optional capabilities as rendered in `--version` output.
    pub const ALL: [CompiledFeature; 5] = [
        CompiledFeature::Acl,
        CompiledFeature::Xattr,
        CompiledFeature::Zstd,
        CompiledFeature::Iconv,
        CompiledFeature::SdNotify,
    ];

    pub(crate) const fn bit(self) -> u8 {
        match self {
            Self::Acl => ACL_FEATURE_BIT,
            Self::Xattr => XATTR_FEATURE_BIT,
            Self::Zstd => ZSTD_FEATURE_BIT,
            Self::Iconv => ICONV_FEATURE_BIT,
            Self::SdNotify => SD_NOTIFY_FEATURE_BIT,
        }
    }

    /// Returns the canonical label used when listing the feature in `--version` output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Acl => "ACLs",
            Self::Xattr => "xattrs",
            Self::Zstd => "zstd",
            Self::Iconv => "iconv",
            Self::SdNotify => "sd-notify",
        }
    }

    /// Parses a feature label back into its [`CompiledFeature`] variant.
    #[must_use]
    pub const fn from_label(label: &str) -> Option<Self> {
        if Self::label_eq(label, "ACLs") {
            Some(Self::Acl)
        } else if Self::label_eq(label, "xattrs") {
            Some(Self::Xattr)
        } else if Self::label_eq(label, "zstd") {
            Some(Self::Zstd)
        } else if Self::label_eq(label, "iconv") {
            Some(Self::Iconv)
        } else if Self::label_eq(label, "sd-notify") {
            Some(Self::SdNotify)
        } else {
            None
        }
    }

    /// Reports whether the feature was compiled into the current build.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        match COMPILED_FEATURE_BITMAP {
            0 => false,
            bitmap => (bitmap & self.bit()) != 0,
        }
    }

    /// Returns a human-readable description of the feature for tooling output.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Acl => "POSIX ACL support",
            Self::Xattr => "Extended attribute support",
            Self::Zstd => "Zstandard compression",
            Self::Iconv => "Character-set conversion via iconv",
            Self::SdNotify => "systemd sd_notify integration",
        }
    }
}

/// Zero-allocation view of the compiled feature list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticCompiledFeatures {
    features: [CompiledFeature; COMPILED_FEATURE_COUNT],
    len: usize,
    bitmap: u8,
}

impl StaticCompiledFeatures {
    const fn new() -> Self {
        let mut features = [CompiledFeature::Acl; COMPILED_FEATURE_COUNT];
        let mut len = 0usize;
        let mut index = 0usize;

        if COMPILED_FEATURE_BITMAP != 0 {
            while index < COMPILED_FEATURE_COUNT {
                let feature = CompiledFeature::ALL[index];
                if feature.is_enabled() {
                    features[len] = feature;
                    len += 1;
                }

                index += 1;
            }
        }

        Self {
            features,
            len,
            bitmap: COMPILED_FEATURE_BITMAP,
        }
    }

    /// Returns the number of compiled features captured by the view.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Reports whether any optional features were compiled in.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Exposes the canonical slice describing the compiled feature list.
    #[must_use]
    pub fn as_slice(&self) -> &[CompiledFeature] {
        &self.features[..self.len]
    }

    /// Returns the bitmap describing which optional capabilities were compiled in.
    #[must_use]
    pub const fn bitmap(&self) -> u8 {
        self.bitmap
    }

    /// Reports whether the provided feature is part of the compiled set.
    #[must_use]
    pub const fn contains(&self, feature: CompiledFeature) -> bool {
        (self.bitmap & feature.bit()) != 0
    }

    /// Returns an iterator over the compiled features without allocating.
    #[must_use]
    pub const fn iter(&self) -> StaticCompiledFeaturesIter<'_> {
        StaticCompiledFeaturesIter::new(&self.features, self.len)
    }
}

impl Default for StaticCompiledFeatures {
    fn default() -> Self {
        Self::new()
    }
}

impl AsRef<[CompiledFeature]> for StaticCompiledFeatures {
    fn as_ref(&self) -> &[CompiledFeature] {
        self.as_slice()
    }
}

/// Iterator over the statically computed feature set.
#[derive(Clone, Debug)]
pub struct StaticCompiledFeaturesIter<'a> {
    slice: &'a [CompiledFeature; COMPILED_FEATURE_COUNT],
    start: usize,
    end: usize,
}

impl<'a> StaticCompiledFeaturesIter<'a> {
    const fn new(slice: &'a [CompiledFeature; COMPILED_FEATURE_COUNT], len: usize) -> Self {
        Self {
            slice,
            start: 0,
            end: len,
        }
    }
}

impl<'a> Iterator for StaticCompiledFeaturesIter<'a> {
    type Item = CompiledFeature;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start >= self.end {
            return None;
        }

        let feature = self.slice[self.start];
        self.start += 1;
        Some(feature)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end.saturating_sub(self.start);
        (remaining, Some(remaining))
    }
}

impl<'a> DoubleEndedIterator for StaticCompiledFeaturesIter<'a> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.start >= self.end {
            return None;
        }

        self.end -= 1;
        Some(self.slice[self.end])
    }
}

impl<'a> ExactSizeIterator for StaticCompiledFeaturesIter<'a> {
    fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

impl<'a> FusedIterator for StaticCompiledFeaturesIter<'a> {}

impl<'a> IntoIterator for &'a StaticCompiledFeatures {
    type Item = CompiledFeature;
    type IntoIter = StaticCompiledFeaturesIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Statically computed compiled feature view used by helper accessors.
pub const COMPILED_FEATURES_STATIC: StaticCompiledFeatures = StaticCompiledFeatures::new();

impl fmt::Display for CompiledFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Error returned when parsing a [`CompiledFeature`] from a string fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error("unknown compiled feature label")]
pub struct ParseCompiledFeatureError;

impl FromStr for CompiledFeature {
    type Err = ParseCompiledFeatureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_label(s).ok_or(ParseCompiledFeatureError)
    }
}

/// Returns an iterator over the optional features compiled into the current build.
#[must_use]
pub const fn compiled_features_iter() -> CompiledFeaturesIter {
    CompiledFeaturesIter::new()
}

/// Returns the set of optional features compiled into the current build.
#[must_use]
pub fn compiled_features() -> Vec<CompiledFeature> {
    compiled_features_static().as_slice().to_vec()
}

/// Returns a zero-allocation view over the compiled feature set.
#[must_use]
pub const fn compiled_features_static() -> &'static StaticCompiledFeatures {
    &COMPILED_FEATURES_STATIC
}

/// Convenience helper that exposes the labels for each compiled feature.
#[must_use]
pub fn compiled_feature_labels() -> Vec<&'static str> {
    compiled_features_iter()
        .map(CompiledFeature::label)
        .collect()
}

/// Iterator over [`CompiledFeature`] values that are enabled for the current build.
#[derive(Clone, Debug)]
pub struct CompiledFeaturesIter {
    index: usize,
    back: usize,
    remaining_bitmap: u8,
    remaining: usize,
}

impl CompiledFeaturesIter {
    const fn new() -> Self {
        let bitmap = COMPILED_FEATURE_BITMAP;

        Self {
            index: 0,
            back: CompiledFeature::ALL.len(),
            remaining_bitmap: bitmap,
            remaining: bitmap.count_ones() as usize,
        }
    }

    const fn consume(&mut self, feature: CompiledFeature) -> CompiledFeature {
        self.remaining_bitmap &= !feature.bit();
        self.remaining = self.remaining.saturating_sub(1);
        feature
    }
}

impl Iterator for CompiledFeaturesIter {
    type Item = CompiledFeature;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            self.index = self.back;
            return None;
        }

        while self.index < self.back {
            let feature = CompiledFeature::ALL[self.index];
            self.index += 1;

            if (self.remaining_bitmap & feature.bit()) != 0 {
                return Some(self.consume(feature));
            }
        }

        self.remaining = 0;
        self.remaining_bitmap = 0;
        self.index = self.back;
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for CompiledFeaturesIter {
    fn len(&self) -> usize {
        self.remaining
    }
}

impl DoubleEndedIterator for CompiledFeaturesIter {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            self.back = self.index;
            return None;
        }

        while self.index < self.back {
            self.back -= 1;
            let feature = CompiledFeature::ALL[self.back];

            if (self.remaining_bitmap & feature.bit()) != 0 {
                return Some(self.consume(feature));
            }
        }

        self.remaining = 0;
        self.remaining_bitmap = 0;
        self.back = self.index;
        None
    }
}

impl FusedIterator for CompiledFeaturesIter {}

/// Display helper for rendering capability lists.
/// Display helper for rendering capability lists.
///
/// The wrapper preserves the upstream ordering of compiled features while offering convenient
/// iterators and formatting helpers for rendering `--version` output.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompiledFeaturesDisplay {
    features: Vec<CompiledFeature>,
}

impl CompiledFeaturesDisplay {
    /// Creates a display wrapper from an explicit feature list.
    #[must_use]
    pub const fn new(features: Vec<CompiledFeature>) -> Self {
        Self { features }
    }

    /// Returns the underlying feature slice.
    #[must_use]
    pub fn features(&self) -> &[CompiledFeature] {
        &self.features
    }

    /// Returns the number of compiled features captured by the display.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.features.len()
    }

    /// Reports whether the feature list is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Returns an iterator over the compiled features in display order.
    #[must_use = "inspect the iterator to observe compiled feature ordering"]
    pub fn iter(&self) -> std::slice::Iter<'_, CompiledFeature> {
        self.features.iter()
    }

    /// Retains only the features that satisfy the provided predicate.
    pub fn retain<F>(&mut self, mut predicate: F)
    where
        F: FnMut(&CompiledFeature) -> bool,
    {
        self.features.retain(|feature| predicate(feature));
    }
}

impl fmt::Display for CompiledFeaturesDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut iter = self.features.iter();

        if let Some(first) = iter.next() {
            fmt::Display::fmt(first, f)?;
            for feature in iter {
                f.write_str(" ")?;
                fmt::Display::fmt(feature, f)?;
            }
        }

        Ok(())
    }
}

impl IntoIterator for CompiledFeaturesDisplay {
    type Item = CompiledFeature;
    type IntoIter = std::vec::IntoIter<CompiledFeature>;

    fn into_iter(self) -> Self::IntoIter {
        self.features.into_iter()
    }
}

impl<'a> IntoIterator for &'a CompiledFeaturesDisplay {
    type Item = &'a CompiledFeature;
    type IntoIter = std::slice::Iter<'a, CompiledFeature>;

    fn into_iter(self) -> Self::IntoIter {
        self.features.iter()
    }
}

impl<'a> IntoIterator for &'a mut CompiledFeaturesDisplay {
    type Item = &'a mut CompiledFeature;
    type IntoIter = std::slice::IterMut<'a, CompiledFeature>;

    fn into_iter(self) -> Self::IntoIter {
        self.features.iter_mut()
    }
}

impl FromIterator<CompiledFeature> for CompiledFeaturesDisplay {
    fn from_iter<T: IntoIterator<Item = CompiledFeature>>(iter: T) -> Self {
        Self::new(iter.into_iter().collect())
    }
}

impl Extend<CompiledFeature> for CompiledFeaturesDisplay {
    fn extend<T: IntoIterator<Item = CompiledFeature>>(&mut self, iter: T) {
        self.features.extend(iter);
    }
}

impl<'a> Extend<&'a CompiledFeature> for CompiledFeaturesDisplay {
    fn extend<T: IntoIterator<Item = &'a CompiledFeature>>(&mut self, iter: T) {
        self.features.extend(iter.into_iter().copied());
    }
}

/// Returns a [`CompiledFeaturesDisplay`] reflecting the active feature set.
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
