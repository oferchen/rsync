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

use core::fmt;

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

/// Returns the set of optional features compiled into the current build.
///
/// The helper inspects Cargo feature flags exposed to the `rsync-core` crate and
/// yields the matching [`CompiledFeature`] values in a deterministic order. The
/// resulting vector is sorted by priority rather than lexicographically to
/// mirror the user-facing expectation: ACLs and xattrs appear before
/// compression and auxiliary integrations.
#[must_use]
pub fn compiled_features() -> Vec<CompiledFeature> {
    let mut features = Vec::with_capacity(5);

    if cfg!(feature = "acl") {
        features.push(CompiledFeature::Acl);
    }
    if cfg!(feature = "xattr") {
        features.push(CompiledFeature::Xattr);
    }
    if cfg!(feature = "zstd") {
        features.push(CompiledFeature::Zstd);
    }
    if cfg!(feature = "iconv") {
        features.push(CompiledFeature::Iconv);
    }
    if cfg!(feature = "sd-notify") {
        features.push(CompiledFeature::SdNotify);
    }

    features
}

/// Convenience helper that exposes the labels for each compiled feature.
#[must_use]
pub fn compiled_feature_labels() -> Vec<&'static str> {
    compiled_features()
        .into_iter()
        .map(CompiledFeature::label)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_features_match_cfg_flags() {
        let features = compiled_features();

        assert_eq!(
            features.contains(&CompiledFeature::Acl),
            cfg!(feature = "acl")
        );
        assert_eq!(
            features.contains(&CompiledFeature::Xattr),
            cfg!(feature = "xattr")
        );
        assert_eq!(
            features.contains(&CompiledFeature::Zstd),
            cfg!(feature = "zstd")
        );
        assert_eq!(
            features.contains(&CompiledFeature::Iconv),
            cfg!(feature = "iconv")
        );
        assert_eq!(
            features.contains(&CompiledFeature::SdNotify),
            cfg!(feature = "sd-notify")
        );
    }

    #[test]
    fn feature_labels_align_with_display() {
        for feature in [
            CompiledFeature::Acl,
            CompiledFeature::Xattr,
            CompiledFeature::Zstd,
            CompiledFeature::Iconv,
            CompiledFeature::SdNotify,
        ] {
            assert_eq!(feature.label(), feature.to_string());
        }
    }

    #[test]
    fn compiled_feature_labels_reflect_active_features() {
        let labels = compiled_feature_labels();

        assert_eq!(labels.contains(&"ACLs"), cfg!(feature = "acl"));
        assert_eq!(labels.contains(&"xattrs"), cfg!(feature = "xattr"));
        assert_eq!(labels.contains(&"zstd"), cfg!(feature = "zstd"));
        assert_eq!(labels.contains(&"iconv"), cfg!(feature = "iconv"));
        assert_eq!(labels.contains(&"sd-notify"), cfg!(feature = "sd-notify"));
    }
}
