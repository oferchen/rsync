#![deny(unsafe_code)]

//! # Overview
//!
//! `rsync_core::version` centralises the workspace version constants and
//! feature-detection helpers that will eventually drive the `--version` output
//! of the Rust `rsync` binaries. The module mirrors upstream rsync 3.4.1 by
//! exposing the canonical base version while appending the `-rust` suffix that
//! brands this reimplementation.
//!
//! # Design
//!
//! The module publishes lightweight enums and helper functions:
//!
//! - [`RUST_VERSION`] holds the `3.4.1-rust` identifier rendered by
//!   user-visible banners.
//! - [`compiled_features`] inspects Cargo feature flags and returns the set of
//!   optional capabilities enabled at build time.
//! - [`CompiledFeature`] enumerates optional capabilities and provides label
//!   helpers such as [`CompiledFeature::label`] and
//!   [`CompiledFeature::from_label`] for parsing user-provided strings.
//!
//! This structure keeps other crates free of conditional compilation logic
//! while avoiding string duplication across the workspace.
//!
//! # Invariants
//!
//! - [`RUST_VERSION`] always embeds the upstream base release so diagnostics and
//!   CLI output remain aligned with rsync 3.4.1.
//! - [`compiled_features`] never invents capabilities: it only reports flags
//!   that were explicitly enabled when compiling `rsync-core`.
//!
//! # Errors
//!
//! The module exposes [`ParseCompiledFeatureError`] when parsing a
//! [`CompiledFeature`] from a string fails. All other helpers return constants
//! or eagerly evaluate into owned collections.
//!
//! # Examples
//!
//! Retrieve the compiled feature list for the current build. The default test
//! configuration does not enable optional features, so the returned slice is
//! empty.
//!
//! ```
//! use rsync_core::version::{compiled_features, RUST_VERSION};
//!
//! assert_eq!(RUST_VERSION, "3.4.1-rust");
//! assert!(compiled_features().is_empty());
//! ```
//!
//! # See also
//!
//! - [`rsync_core::message`] uses [`RUST_VERSION`] when rendering error trailers.
//! - Future CLI modules rely on [`compiled_features`] to mirror upstream
//!   `--version` capability listings.

use core::{
    fmt,
    iter::{FromIterator, FusedIterator},
    str::FromStr,
};

const ACL_FEATURE_BIT: u8 = 1 << 0;
const XATTR_FEATURE_BIT: u8 = 1 << 1;
const ZSTD_FEATURE_BIT: u8 = 1 << 2;
const ICONV_FEATURE_BIT: u8 = 1 << 3;
const SD_NOTIFY_FEATURE_BIT: u8 = 1 << 4;

/// Bitmap describing the optional features compiled into this build.
///
/// Each bit corresponds to one of the [`CompiledFeature`] variants, ordered according to
/// [`CompiledFeature::ALL`]. Exposing the bitmap allows higher layers to perform constant-time
/// membership checks, pre-size lookup tables, or cache whether any optional capabilities were
/// enabled without materialising the full vector returned by [`compiled_features`]. The value is
/// computed using `cfg!(feature = "...")`, ensuring the bits reflect the compile-time feature
/// set even in `const` contexts.
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

/// Upstream base version that the Rust implementation tracks.
#[doc(alias = "3.4.1")]
pub const UPSTREAM_BASE_VERSION: &str = "3.4.1";

/// Full version string rendered by user-visible banners.
#[doc(alias = "3.4.1-rust")]
pub const RUST_VERSION: &str = "3.4.1-rust";

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

    const fn bit(self) -> u8 {
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
    ///
    /// The helper accepts the canonical labels produced by [`CompiledFeature::label`]
    /// and used in `--version` output. It runs in constant time because the
    /// feature set is fixed and small, making it suitable for validating user
    /// supplied capability lists or regenerating [`CompiledFeature`] values from
    /// documentation tables without allocating intermediate collections. The
    /// function is `const`, enabling compile-time validation of documentation
    /// tables and other static metadata.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::version::CompiledFeature;
    ///
    /// const ACL: Option<CompiledFeature> = CompiledFeature::from_label("ACLs");
    /// const UNKNOWN: Option<CompiledFeature> = CompiledFeature::from_label("unknown");
    ///
    /// assert_eq!(ACL, Some(CompiledFeature::Acl));
    /// assert!(UNKNOWN.is_none());
    /// ```
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
///
/// The iterator preserves the canonical ordering defined by
/// [`CompiledFeature::ALL`] while skipping capabilities that were not enabled at
/// compile time. It is primarily useful for callers that only need to iterate
/// over the active feature set without allocating an intermediate [`Vec`]. When
/// the collected representation is required, use [`compiled_features`], which
/// delegates to this iterator.
///
/// # Examples
///
/// ```
/// use rsync_core::version::{compiled_features, compiled_features_iter};
///
/// let collected: Vec<_> = compiled_features_iter().collect();
/// assert_eq!(collected, compiled_features());
///
/// let mut expected = collected.clone();
/// expected.reverse();
/// let reversed: Vec<_> = compiled_features_iter().rev().collect();
/// assert_eq!(reversed, expected);
/// ```
#[must_use]
pub fn compiled_features_iter() -> CompiledFeaturesIter {
    CompiledFeaturesIter::new()
}

/// Returns the set of optional features compiled into the current build.
///
/// The helper collects [`compiled_features_iter`], preserving the deterministic
/// priority order used by upstream rsync when printing capability lists.
#[must_use]
pub fn compiled_features() -> Vec<CompiledFeature> {
    compiled_features_iter().collect()
}

/// Convenience helper that exposes the labels for each compiled feature.
#[must_use]
pub fn compiled_feature_labels() -> Vec<&'static str> {
    compiled_features_iter()
        .map(CompiledFeature::label)
        .collect()
}

/// Iterator over [`CompiledFeature`] values that are enabled for the current build.
///
/// The iterator caches the number of remaining enabled features so [`ExactSizeIterator::len`]
/// and [`Iterator::size_hint`] both run in `O(1)` time without repeatedly scanning the
/// static [`CompiledFeature::ALL`] table. It also implements [`DoubleEndedIterator`],
/// allowing callers to traverse the active feature set in reverse order when generating
/// diagnostics that list the newest capabilities first.
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

impl Default for CompiledFeaturesIter {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience formatter for the compiled feature list.
///
/// The wrapper retains the feature ordering produced by [`compiled_features`] and implements
/// [`Display`](fmt::Display) so callers can render the list into user-facing banners without
/// duplicating join logic. Upstream rsync prints optional capabilities as a space-separated
/// string, which this helper reproduces exactly. The type also implements [`IntoIterator`]
/// for owned and borrowed values together with [`FromIterator`] and [`Extend`], making it easy
/// to reuse the collected feature set when rendering additional diagnostics, building the
/// wrapper from iterator pipelines, or appending capabilities incrementally.
///
/// # Examples
///
/// Format two features into the canonical `--version` string layout:
///
/// ```
/// use rsync_core::version::{CompiledFeature, CompiledFeaturesDisplay};
///
/// let display = CompiledFeaturesDisplay::new(vec![
///     CompiledFeature::Acl,
///     CompiledFeature::Xattr,
/// ]);
///
/// assert_eq!(display.to_string(), "ACLs xattrs");
/// assert_eq!(display.features(), &[CompiledFeature::Acl, CompiledFeature::Xattr]);
/// ```
///
/// Iterate over the features using the [`IntoIterator`] implementations:
///
/// ```
/// use rsync_core::version::{CompiledFeature, CompiledFeaturesDisplay};
///
/// let display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
///
/// for feature in &display {
///     assert_eq!(*feature, CompiledFeature::Acl);
/// }
///
/// let mut owned = display.clone().into_iter();
/// assert_eq!(owned.next(), Some(CompiledFeature::Acl));
/// assert!(owned.next().is_none());
/// ```
///
/// Collect a display from an iterator of features:
///
/// ```
/// use rsync_core::version::{CompiledFeature, CompiledFeaturesDisplay};
///
/// let display: CompiledFeaturesDisplay = [CompiledFeature::Acl, CompiledFeature::Xattr]
///     .into_iter()
///     .collect();
///
/// assert_eq!(display.features(), &[CompiledFeature::Acl, CompiledFeature::Xattr]);
/// ```
///
/// Extend an existing display with additional features:
///
/// ```
/// use rsync_core::version::{CompiledFeature, CompiledFeaturesDisplay};
///
/// let mut display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
/// display.extend([CompiledFeature::Xattr]);
///
/// let extra = [CompiledFeature::Zstd, CompiledFeature::Iconv];
/// display.extend(extra.iter());
///
/// assert_eq!(
///     display.features(),
///     &[
///         CompiledFeature::Acl,
///         CompiledFeature::Xattr,
///         CompiledFeature::Zstd,
///         CompiledFeature::Iconv,
///     ]
/// );
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompiledFeaturesDisplay {
    features: Vec<CompiledFeature>,
}

impl CompiledFeaturesDisplay {
    /// Creates a display wrapper from an explicit feature list.
    ///
    /// The input order is preserved so higher layers can render capability groups in the same
    /// sequence they would appear in upstream rsync output.
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
    ///
    /// The helper mirrors [`Vec::len`], allowing callers to treat the wrapper as a
    /// lightweight view over the collected feature list without reaching into the
    /// backing vector explicitly. This is useful when rendering capability
    /// summaries that need to branch on the feature count while still preserving
    /// the ordering guarantees provided by [`CompiledFeaturesDisplay`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::version::{CompiledFeature, CompiledFeaturesDisplay};
    ///
    /// let display = CompiledFeaturesDisplay::new(vec![
    ///     CompiledFeature::Acl,
    ///     CompiledFeature::Xattr,
    /// ]);
    ///
    /// assert_eq!(display.len(), 2);
    /// ```
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
    ///
    /// This is a convenience wrapper around [`features`](Self::features) that
    /// makes it straightforward to traverse the capability list without
    /// importing [`IntoIterator`] for references. The iterator yields the same
    /// sequence as [`CompiledFeaturesDisplay::features`], ensuring callers can
    /// rely on the canonical upstream ordering.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::version::{CompiledFeature, CompiledFeaturesDisplay};
    ///
    /// let display = CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl]);
    /// let mut iter = display.iter();
    /// assert_eq!(iter.next(), Some(&CompiledFeature::Acl));
    /// assert!(iter.next().is_none());
    /// ```
    #[must_use = "inspect the iterator to observe compiled feature ordering"]
    pub fn iter(&self) -> std::slice::Iter<'_, CompiledFeature> {
        self.features.iter()
    }

    /// Retains only the features that satisfy the provided predicate.
    ///
    /// The helper mirrors [`Vec::retain`] while preserving the deterministic
    /// ordering expected by upstream `--version` output. Callers can use this to
    /// drop capabilities that should not be rendered in a particular context
    /// (for example, when the daemon configuration restricts advertised
    /// features) without reallocating the backing vector. The predicate receives
    /// each feature in sequence and retains it when returning `true`.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::version::{CompiledFeature, CompiledFeaturesDisplay};
    ///
    /// let mut display = CompiledFeaturesDisplay::new(vec![
    ///     CompiledFeature::Acl,
    ///     CompiledFeature::Xattr,
    ///     CompiledFeature::Iconv,
    /// ]);
    ///
    /// display.retain(|feature| !matches!(feature, CompiledFeature::Xattr));
    /// assert_eq!(display.features(), &[CompiledFeature::Acl, CompiledFeature::Iconv]);
    /// ```
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

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.features.into_iter()
    }
}

impl<'a> IntoIterator for &'a CompiledFeaturesDisplay {
    type Item = &'a CompiledFeature;
    type IntoIter = std::slice::Iter<'a, CompiledFeature>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.features.iter()
    }
}

impl<'a> IntoIterator for &'a mut CompiledFeaturesDisplay {
    type Item = &'a mut CompiledFeature;
    type IntoIter = std::slice::IterMut<'a, CompiledFeature>;

    #[inline]
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
///
/// This helper is intended for rendering `--version` banners and other user-visible diagnostics
/// where upstream rsync prints a space-separated capability list. The returned wrapper can be
/// formatted directly or inspected programmatically.
///
/// # Examples
///
/// ```
/// use rsync_core::version::compiled_features_display;
///
/// let display = compiled_features_display();
/// let rendered = display.to_string();
///
/// if display.is_empty() {
///     assert!(rendered.is_empty());
/// } else {
///     let words: Vec<_> = rendered.split_whitespace().collect();
///     assert_eq!(words.len(), display.features().len());
/// }
/// ```
#[must_use]
pub fn compiled_features_display() -> CompiledFeaturesDisplay {
    CompiledFeaturesDisplay::new(compiled_features())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACL_FROM_LABEL: Option<CompiledFeature> = CompiledFeature::from_label("ACLs");
    const UNKNOWN_FROM_LABEL: Option<CompiledFeature> = CompiledFeature::from_label("unknown");

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
    fn compiled_features_display_reflects_active_features() {
        let display = compiled_features_display();
        assert_eq!(display.features(), compiled_features().as_slice());
        assert_eq!(display.is_empty(), compiled_features().is_empty());
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
        let display =
            CompiledFeaturesDisplay::new(vec![CompiledFeature::Acl, CompiledFeature::Xattr]);

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
}
