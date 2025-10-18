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
//! The module does not expose error types. All helpers either return constants
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

use core::{fmt, iter::FusedIterator};

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
    /// Canonical ordering of optional capabilities as rendered in `--version` output.
    pub const ALL: [CompiledFeature; 5] = [
        CompiledFeature::Acl,
        CompiledFeature::Xattr,
        CompiledFeature::Zstd,
        CompiledFeature::Iconv,
        CompiledFeature::SdNotify,
    ];

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

    /// Reports whether the feature was compiled into the current build.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        match self {
            Self::Acl => cfg!(feature = "acl"),
            Self::Xattr => cfg!(feature = "xattr"),
            Self::Zstd => cfg!(feature = "zstd"),
            Self::Iconv => cfg!(feature = "iconv"),
            Self::SdNotify => cfg!(feature = "sd-notify"),
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
#[derive(Clone, Debug, Default)]
pub struct CompiledFeaturesIter {
    index: usize,
}

impl CompiledFeaturesIter {
    const fn new() -> Self {
        Self { index: 0 }
    }

    fn remaining_enabled(&self) -> usize {
        CompiledFeature::ALL[self.index..]
            .iter()
            .filter(|feature| feature.is_enabled())
            .count()
    }
}

impl Iterator for CompiledFeaturesIter {
    type Item = CompiledFeature;

    fn next(&mut self) -> Option<Self::Item> {
        while self.index < CompiledFeature::ALL.len() {
            let feature = CompiledFeature::ALL[self.index];
            self.index += 1;

            if feature.is_enabled() {
                return Some(feature);
            }
        }

        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.remaining_enabled();
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for CompiledFeaturesIter {
    fn len(&self) -> usize {
        self.remaining_enabled()
    }
}

impl FusedIterator for CompiledFeaturesIter {}

/// Convenience formatter for the compiled feature list.
///
/// The wrapper retains the feature ordering produced by [`compiled_features`] and implements
/// [`Display`](fmt::Display) so callers can render the list into user-facing banners without
/// duplicating join logic. Upstream rsync prints optional capabilities as a space-separated
/// string, which this helper reproduces exactly.
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

    /// Reports whether the feature list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
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

    #[test]
    fn compiled_features_match_cfg_flags() {
        let features = compiled_features();

        for feature in CompiledFeature::ALL {
            assert_eq!(features.contains(&feature), feature.is_enabled());
        }
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
    fn compiled_features_display_handles_empty_list() {
        let display = CompiledFeaturesDisplay::new(Vec::new());

        assert!(display.is_empty());
        assert!(display.to_string().is_empty());
    }

    #[test]
    fn compiled_features_iter_matches_collected_set() {
        let via_iter: Vec<_> = compiled_features_iter().collect();
        assert_eq!(via_iter, compiled_features());
    }

    #[test]
    fn compiled_features_iter_is_fused_and_updates_len() {
        let mut iter = compiled_features_iter();
        let (lower, upper) = iter.size_hint();
        assert_eq!(Some(lower), upper);
        assert_eq!(iter.len(), lower);

        while iter.next().is_some() {
            let (lower, upper) = iter.size_hint();
            assert_eq!(Some(lower), upper);
            assert_eq!(iter.len(), lower);
        }

        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
        assert_eq!(iter.len(), 0);
    }
}
