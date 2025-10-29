#![deny(unsafe_code)]

//! Workspace branding manifest exposed as a cached snapshot.
//!
//! The [`manifest()`] accessor returns a [`BrandManifest`] struct that captures
//! the canonical program names, configuration paths, and version metadata for
//! the workspace. Callers can retrieve brand-specific program names or
//! filesystem locations via [`BrandManifest::profile_for`] and its convenience
//! accessors. Higher layers use the manifest to render banners, build help
//! text, and locate configuration files without duplicating string literals or
//! re-parsing the workspace manifest at runtime. The manifest is constructed on
//! first use from [`crate::workspace::metadata()`] and cached for the lifetime of
//! the process so callers can obtain references to the data at negligible cost.

use std::sync::OnceLock;

use super::brand::{self, Brand};
use super::profile::{BrandProfile, oc_profile, upstream_profile};
use crate::version::{self, BUILD_TOOLCHAIN};
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
    build_revision: &'static str,
    build_toolchain: &'static str,
}

impl BrandManifest {
    /// Returns the default [`Brand`] configured for this workspace build.
    #[must_use]
    pub const fn default_brand(self) -> Brand {
        self.default_brand
    }

    /// Returns the [`BrandProfile`] associated with `brand`.
    #[must_use]
    pub const fn profile_for(self, brand: Brand) -> BrandProfile {
        match brand {
            Brand::Oc => self.oc,
            Brand::Upstream => self.upstream,
        }
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

    /// Returns the client program name for the requested `brand`.
    #[must_use]
    pub const fn client_program_name_for(self, brand: Brand) -> &'static str {
        let profile = self.profile_for(brand);
        profile.client_program_name()
    }

    /// Returns the daemon program name for the requested `brand`.
    #[must_use]
    pub const fn daemon_program_name_for(self, brand: Brand) -> &'static str {
        let profile = self.profile_for(brand);
        profile.daemon_program_name()
    }

    /// Returns the daemon configuration directory for the requested `brand`.
    #[must_use]
    pub const fn daemon_config_dir_for(self, brand: Brand) -> &'static str {
        let profile = self.profile_for(brand);
        profile.daemon_config_dir_str()
    }

    /// Returns the daemon configuration file path for the requested `brand`.
    #[must_use]
    pub const fn daemon_config_path_for(self, brand: Brand) -> &'static str {
        let profile = self.profile_for(brand);
        profile.daemon_config_path_str()
    }

    /// Returns the daemon secrets file path for the requested `brand`.
    #[must_use]
    pub const fn daemon_secrets_path_for(self, brand: Brand) -> &'static str {
        let profile = self.profile_for(brand);
        profile.daemon_secrets_path_str()
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

    /// Returns the sanitized build revision embedded in the binaries.
    #[must_use]
    pub const fn build_revision(self) -> &'static str {
        self.build_revision
    }

    /// Returns the human-readable toolchain description rendered by banners.
    #[must_use]
    pub const fn build_toolchain(self) -> &'static str {
        self.build_toolchain
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
        build_revision: version::build_revision(),
        build_toolchain: BUILD_TOOLCHAIN,
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

        assert_eq!(manifest.profile_for(Brand::Oc), manifest.oc());
        assert_eq!(manifest.profile_for(Brand::Upstream), manifest.upstream());
        assert_eq!(
            manifest.client_program_name_for(Brand::Oc),
            metadata.client_program_name()
        );
        assert_eq!(
            manifest.client_program_name_for(Brand::Upstream),
            metadata.legacy_client_program_name()
        );
        assert_eq!(
            manifest.daemon_program_name_for(Brand::Oc),
            metadata.daemon_program_name()
        );
        assert_eq!(
            manifest.daemon_program_name_for(Brand::Upstream),
            metadata.legacy_daemon_program_name()
        );
        assert_eq!(
            manifest.daemon_config_dir_for(Brand::Oc),
            metadata.daemon_config_dir()
        );
        assert_eq!(
            manifest.daemon_config_dir_for(Brand::Upstream),
            metadata.legacy_daemon_config_dir()
        );
        assert_eq!(
            manifest.daemon_config_path_for(Brand::Oc),
            metadata.daemon_config_path()
        );
        assert_eq!(
            manifest.daemon_config_path_for(Brand::Upstream),
            metadata.legacy_daemon_config_path()
        );
        assert_eq!(
            manifest.daemon_secrets_path_for(Brand::Oc),
            metadata.daemon_secrets_path()
        );
        assert_eq!(
            manifest.daemon_secrets_path_for(Brand::Upstream),
            metadata.legacy_daemon_secrets_path()
        );
        assert_eq!(manifest.default_brand(), brand::default_brand());
        assert_eq!(manifest.oc(), oc_profile());
        assert_eq!(manifest.upstream(), upstream_profile());
        assert_eq!(manifest.rust_version(), metadata.rust_version());
        assert_eq!(manifest.upstream_version(), metadata.upstream_version());
        assert_eq!(manifest.protocol_version(), metadata.protocol_version());
        assert_eq!(manifest.source_url(), metadata.source_url());
        assert_eq!(manifest.build_revision(), version::build_revision());
        assert_eq!(manifest.build_toolchain(), BUILD_TOOLCHAIN);
    }
}
