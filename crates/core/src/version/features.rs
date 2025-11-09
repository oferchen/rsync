use ::core::fmt;
use ::core::iter::{FromIterator, FusedIterator};
use ::core::str::FromStr;
use std::vec::Vec;

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
    pub fn iter(&self) -> StaticCompiledFeaturesIter<'_> {
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseCompiledFeatureError;

impl fmt::Display for ParseCompiledFeatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown compiled feature label")
    }
}

impl std::error::Error for ParseCompiledFeatureError {}

impl FromStr for CompiledFeature {
    type Err = ParseCompiledFeatureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_label(s).ok_or(ParseCompiledFeatureError)
    }
}

/// Returns an iterator over the optional features compiled into the current build.
#[must_use]
pub fn compiled_features_iter() -> CompiledFeaturesIter {
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

    fn consume(&mut self, feature: CompiledFeature) -> CompiledFeature {
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
    pub fn new(features: Vec<CompiledFeature>) -> Self {
        Self { features }
    }

    /// Returns the underlying feature slice.
    #[must_use]
    pub fn features(&self) -> &[CompiledFeature] {
        &self.features
    }

    /// Returns the number of compiled features captured by the display.
    #[must_use]
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Reports whether the feature list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
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
