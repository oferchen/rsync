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

use serde::Serialize;
use std::fmt;
use std::sync::OnceLock;

use super::brand::{self, Brand};
use super::profile::{BrandProfile, oc_profile, upstream_profile};
use crate::version::{self, BUILD_TOOLCHAIN};
use crate::workspace;

/// Cached branding snapshot derived from the workspace manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
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

/// Concise, brand-specific view of the workspace metadata.
///
/// Instances of this structure are produced via
/// [`BrandManifest::summary_for`] and expose the canonical program names,
/// configuration paths, version metadata, and optional compatibility wrapper
/// names associated with a given brand.
/// The summary keeps branding details and release identifiers in one place so
/// packaging automation, documentation generators, and entry points can surface
/// consistent human-readable descriptions without duplicating string literals.
///
/// ```
/// use oc_rsync_core::branding::{manifest, Brand};
///
/// let manifest = manifest();
/// let oc = manifest.summary_for(Brand::Oc);
/// assert_eq!(oc.client_program_name(), "oc-rsync");
/// assert_eq!(oc.daemon_config_path(), "/etc/oc-rsyncd/oc-rsyncd.conf");
/// assert!(oc.to_string().contains("3.4.1-rust"));
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BrandSummary {
    brand: Brand,
    profile: BrandProfile,
    daemon_wrapper_program_name: Option<&'static str>,
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

    /// Returns a [`BrandSummary`] describing the metadata associated with
    /// `brand`.
    #[must_use]
    pub const fn summary_for(self, brand: Brand) -> BrandSummary {
        BrandSummary {
            brand,
            profile: self.profile_for(brand),
            daemon_wrapper_program_name: self.profile_for(brand).daemon_wrapper_program_name(),
            rust_version: self.rust_version,
            upstream_version: self.upstream_version,
            protocol_version: self.protocol_version,
            source_url: self.source_url,
            build_revision: self.build_revision,
            build_toolchain: self.build_toolchain,
        }
    }

    /// Returns the [`BrandSummary`] describing the branded `oc-rsync` binaries.
    #[must_use]
    pub const fn oc_summary(self) -> BrandSummary {
        self.summary_for(Brand::Oc)
    }

    /// Returns the [`BrandSummary`] describing the upstream-compatible binaries.
    #[must_use]
    pub const fn upstream_summary(self) -> BrandSummary {
        self.summary_for(Brand::Upstream)
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

impl BrandSummary {
    /// Returns the [`Brand`] covered by the summary.
    #[must_use]
    pub const fn brand(self) -> Brand {
        self.brand
    }

    /// Returns the canonical client program name for the brand.
    #[must_use]
    pub const fn client_program_name(self) -> &'static str {
        self.profile.client_program_name()
    }

    /// Returns the canonical daemon program name for the brand.
    #[must_use]
    pub const fn daemon_program_name(self) -> &'static str {
        self.profile.daemon_program_name()
    }

    /// Returns the compatibility wrapper program name, if one exists.
    #[must_use]
    pub const fn daemon_wrapper_program_name(self) -> Option<&'static str> {
        self.daemon_wrapper_program_name
    }

    /// Returns the canonical daemon configuration directory for the brand.
    #[must_use]
    pub const fn daemon_config_dir(self) -> &'static str {
        self.profile.daemon_config_dir_str()
    }

    /// Returns the canonical daemon configuration file path for the brand.
    #[must_use]
    pub const fn daemon_config_path(self) -> &'static str {
        self.profile.daemon_config_path_str()
    }

    /// Returns the canonical daemon secrets file path for the brand.
    #[must_use]
    pub const fn daemon_secrets_path(self) -> &'static str {
        self.profile.daemon_secrets_path_str()
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

    /// Returns the source repository URL advertised in version banners.
    #[must_use]
    pub const fn source_url(self) -> &'static str {
        self.source_url
    }

    /// Returns the sanitized build revision embedded in binaries.
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

impl fmt::Display for BrandSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let wrapper = self
            .daemon_wrapper_program_name()
            .unwrap_or_else(|| self.daemon_program_name());

        write!(
            f,
            "brand={} client={} daemon={} wrapper={} config={} secrets={} version={} (upstream {}) protocol={} source={} revision={} toolchain={}",
            self.brand.label(),
            self.client_program_name(),
            self.daemon_program_name(),
            wrapper,
            self.daemon_config_path(),
            self.daemon_secrets_path(),
            self.rust_version(),
            self.upstream_version(),
            self.protocol_version(),
            self.source_url(),
            self.build_revision(),
            self.build_toolchain(),
        )
    }
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
            manifest.oc().daemon_wrapper_program_name(),
            Some(metadata.daemon_wrapper_program_name())
        );
        assert_eq!(
            manifest.upstream().daemon_wrapper_program_name(),
            Some(metadata.legacy_daemon_program_name())
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
        assert_eq!(manifest.oc_summary(), manifest.summary_for(Brand::Oc));
        assert_eq!(
            manifest.upstream_summary(),
            manifest.summary_for(Brand::Upstream)
        );
    }

    #[test]
    fn summary_for_oc_brand_exposes_expected_metadata() {
        let manifest = manifest();
        let summary = manifest.oc_summary();

        assert_eq!(summary.brand(), Brand::Oc);
        assert_eq!(summary.client_program_name(), "oc-rsync");
        assert_eq!(summary.daemon_program_name(), "oc-rsync");
        assert_eq!(summary.daemon_wrapper_program_name(), Some("oc-rsync"));
        assert_eq!(
            summary.daemon_config_path(),
            "/etc/oc-rsyncd/oc-rsyncd.conf"
        );
        assert_eq!(
            summary.daemon_secrets_path(),
            "/etc/oc-rsyncd/oc-rsyncd.secrets"
        );
        assert_eq!(summary.rust_version(), manifest.rust_version());
        assert_eq!(summary.upstream_version(), manifest.upstream_version());
        assert_eq!(summary.protocol_version(), manifest.protocol_version());
        assert_eq!(summary.source_url(), manifest.source_url());
        assert_eq!(summary.build_revision(), manifest.build_revision());
        assert_eq!(summary.build_toolchain(), manifest.build_toolchain());

        let rendered = summary.to_string();
        assert!(rendered.contains("brand=oc"));
        assert!(rendered.contains("client=oc-rsync"));
        assert!(rendered.contains("version=3.4.1-rust"));
    }

    #[test]
    fn summary_for_upstream_brand_reflects_legacy_names() {
        let manifest = manifest();
        let summary = manifest.upstream_summary();

        assert_eq!(summary.brand(), Brand::Upstream);
        assert_eq!(summary.client_program_name(), "rsync");
        assert_eq!(summary.daemon_program_name(), "rsyncd");
        assert_eq!(summary.daemon_wrapper_program_name(), Some("rsyncd"));
        assert_eq!(summary.daemon_config_path(), "/etc/rsyncd.conf");
        assert_eq!(summary.daemon_secrets_path(), "/etc/rsyncd.secrets");
        assert_eq!(summary.rust_version(), manifest.rust_version());
        assert_eq!(summary.upstream_version(), manifest.upstream_version());
    }

    #[test]
    fn summary_display_includes_wrapper_alias() {
        let manifest = manifest();
        let summary = manifest.oc_summary();
        let rendered = summary.to_string();

        assert!(rendered.contains("wrapper=oc-rsync"));
    }
}
