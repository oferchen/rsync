#![deny(unsafe_code)]

//! Workspace branding manifest exposed as a cached snapshot.
//!
//! The [`manifest()`] accessor returns a [`BrandManifest`] struct that captures
//! the canonical program names, configuration paths, and version metadata for
//! the workspace. Higher layers use the manifest to render banners, build help
//! text, and locate configuration files without duplicating string literals or
//! re-parsing the workspace manifest at runtime. The manifest is constructed on
//! first use from [`crate::workspace::metadata()`] and cached for the lifetime of
//! the process so callers can obtain references to the data at negligible cost.

use std::sync::OnceLock;

use super::brand::{self, Brand};
use super::profile::{BrandProfile, oc_profile, upstream_profile};
use crate::workspace;

/// Cached branding snapshot derived from the workspace manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrandManifest {
    default_brand: Brand,
    oc: BrandProfile,
    upstream: BrandProfile,
    rust_version: &'static str,
    upstream_version: &'static str,
    protocol_version: u32,
    source_url: &'static str,
}

impl BrandManifest {
    /// Returns the default [`Brand`] configured for this workspace build.
    #[must_use]
    pub const fn default_brand(self) -> Brand {
        self.default_brand
    }

    /// Returns the branded [`BrandProfile`] used by the canonical binaries.
    #[must_use]
    pub const fn oc(self) -> BrandProfile {
        self.oc
    }

    /// Returns the upstream-compatible [`BrandProfile`].
    #[must_use]
    pub const fn upstream(self) -> BrandProfile {
        self.upstream
    }

    /// Returns the Rust-branded version string advertised by binaries.
    #[must_use]
    pub const fn rust_version(self) -> &'static str {
        self.rust_version
    }

    /// Returns the upstream base version targeted by this build.
    #[must_use]
    pub const fn upstream_version(self) -> &'static str {
        self.upstream_version
    }

    /// Returns the highest rsync protocol version supported by the workspace.
    #[must_use]
    pub const fn protocol_version(self) -> u32 {
        self.protocol_version
    }

    /// Returns the source repository URL advertised by `--version` output.
    #[must_use]
    pub const fn source_url(self) -> &'static str {
        self.source_url
    }
}

fn build_manifest() -> BrandManifest {
    let metadata = workspace::metadata();

    BrandManifest {
        default_brand: brand::default_brand(),
        oc: oc_profile(),
        upstream: upstream_profile(),
        rust_version: metadata.rust_version(),
        upstream_version: metadata.upstream_version(),
        protocol_version: metadata.protocol_version(),
        source_url: metadata.source_url(),
    }
}

/// Returns the cached [`BrandManifest`] describing this workspace build.
#[must_use]
pub fn manifest() -> &'static BrandManifest {
    static MANIFEST: OnceLock<BrandManifest> = OnceLock::new();
    MANIFEST.get_or_init(build_manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_matches_workspace_metadata() {
        let manifest = manifest();
        let metadata = workspace::metadata();

        assert_eq!(manifest.default_brand(), brand::default_brand());
        assert_eq!(manifest.oc(), oc_profile());
        assert_eq!(manifest.upstream(), upstream_profile());
        assert_eq!(manifest.rust_version(), metadata.rust_version());
        assert_eq!(manifest.upstream_version(), metadata.upstream_version());
        assert_eq!(manifest.protocol_version(), metadata.protocol_version());
        assert_eq!(manifest.source_url(), metadata.source_url());
    }
}
