#![deny(unsafe_code)]

//! # Overview
//!
//! `rsync_core::version` centralises the workspace version constants and
//! feature-detection helpers that drive the `--version` output of the Rust
//! `rsync` binaries. The module mirrors upstream rsync 3.4.1 by exposing the
//! canonical base version while appending the `-rust` suffix that brands this
//! implementation.
//!
//! # Design
//!
//! The module publishes lightweight enums and helper functions:
//!
//! - [`RUST_VERSION`](crate::version::RUST_VERSION) holds the `3.4.1-rust`
//!   identifier rendered by
//!   user-visible banners.
//! - [`compiled_features`](crate::version::compiled_features) inspects Cargo
//!   feature flags and returns the set of optional capabilities enabled at build
//!   time.
//! - [`compiled_features_static`](crate::version::compiled_features_static)
//!   exposes a zero-allocation view for repeated
//!   inspections of the compiled feature set.
//! - [`CompiledFeature`](crate::version::CompiledFeature) enumerates optional
//!   capabilities and provides label helpers such as
//!   [`crate::version::CompiledFeature::label`] and
//!   [`crate::version::CompiledFeature::from_label`] for parsing user-provided
//!   strings.
//! - [`VersionInfoReport`](crate::version::VersionInfoReport) renders the full
//!   `--version` text, including capability sections and checksum/compressor
//!   listings, so the CLI can display upstream-identical banners branded for
//!   `rsync`.
//!
//! This structure keeps other crates free of conditional compilation logic
//! while avoiding string duplication across the workspace.
//!
//! # Invariants
//!
//! - [`RUST_VERSION`](crate::version::RUST_VERSION) always embeds the upstream
//!   base release so diagnostics and
//!   CLI output remain aligned with rsync 3.4.1.
//! - [`compiled_features`](crate::version::compiled_features) never invents
//!   capabilities: it only reports flags that were explicitly enabled when
//!   compiling `rsync-core`.
//!
//! # Errors
//!
//! The module exposes
//! [`ParseCompiledFeatureError`](crate::version::ParseCompiledFeatureError)
//! when parsing a [`crate::version::CompiledFeature`] from a string fails. All
//! other helpers return constants or eagerly evaluate into owned collections.
//!
//! # Examples
//!
//! Retrieve the compiled feature list for the current build. Optional
//! capabilities appear when their corresponding Cargo features are enabled at
//! compile time.
//!
//! ```
//! use rsync_core::version::{compiled_features, CompiledFeature, RUST_VERSION};
//!
//! assert_eq!(RUST_VERSION, env!("CARGO_PKG_VERSION"));
//! let features = compiled_features();
//! #[cfg(feature = "xattr")]
//! assert!(features.contains(&CompiledFeature::Xattr));
//! #[cfg(not(feature = "xattr"))]
//! assert!(features.is_empty());
//! ```
//!
//! # See also
//!
//! - [`crate::message`] uses [`crate::version::RUST_VERSION`] when rendering
//!   error trailers.
//! - Future CLI modules rely on [`crate::version::compiled_features`] and
//!   [`crate::version::VersionInfoReport`] to mirror upstream `--version`
//!   capability listings while advertising the Rust-branded binary name.

use crate::{
    branding::{self, Brand},
    workspace,
};
use core::{
    fmt::{self, Write as FmtWrite},
    iter::{FromIterator, FusedIterator},
    mem,
    str::FromStr,
};
use libc::{ino_t, off_t, time_t};
use rsync_protocol::ProtocolVersion;
use std::{borrow::Cow, string::String};

const COMPILED_FEATURE_COUNT: usize = CompiledFeature::ALL.len();

const ACL_FEATURE_BIT: u8 = 1 << 0;
const XATTR_FEATURE_BIT: u8 = 1 << 1;
const ZSTD_FEATURE_BIT: u8 = 1 << 2;
const ICONV_FEATURE_BIT: u8 = 1 << 3;
const SD_NOTIFY_FEATURE_BIT: u8 = 1 << 4;

const _: () = {
    let workspace_version = workspace::metadata().protocol_version();
    let crate_version = ProtocolVersion::NEWEST.as_u8() as u32;
    if workspace_version != crate_version {
        panic!("workspace protocol version must match ProtocolVersion::NEWEST");
    }
};

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

/// Program name rendered by the `rsync` client when displaying version banners.
pub const PROGRAM_NAME: &str = branding::UPSTREAM_CLIENT_PROGRAM_NAME;

/// Program name rendered by the `rsyncd` daemon when displaying version banners.
pub const DAEMON_PROGRAM_NAME: &str = branding::UPSTREAM_DAEMON_PROGRAM_NAME;

/// Program name used by the standalone `oc-rsync` client wrapper.
pub const OC_PROGRAM_NAME: &str = branding::OC_CLIENT_PROGRAM_NAME;

/// Program name used by the standalone `oc-rsyncd` daemon wrapper.
pub const OC_DAEMON_PROGRAM_NAME: &str = branding::OC_DAEMON_PROGRAM_NAME;

/// First copyright year advertised by the Rust implementation.
pub const COPYRIGHT_START_YEAR: &str = "2025";

/// Latest copyright year recorded by the Rust implementation.
pub const LATEST_COPYRIGHT_YEAR: &str = "2025";

/// Copyright notice rendered by `rsync`.
pub const COPYRIGHT_NOTICE: &str = "(C) 2025 by Ofer Chen.";

/// Web site advertised by `rsync` in `--version` output.
pub const WEB_SITE: &str = workspace::SOURCE_URL;

/// Repository URL advertised by version banners and documentation.
pub const SOURCE_URL: &str = WEB_SITE;

/// Human-readable toolchain description rendered in `--version` output.
pub const BUILD_TOOLCHAIN: &str = "Built in Rust 2024";

fn sanitize_build_revision(raw: Option<&'static str>) -> &'static str {
    let Some(value) = raw else {
        return "unknown";
    };

    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "unknown";
    }

    let head = trimmed.split(['\r', '\n']).next().unwrap_or("");
    let cleaned = head.trim();

    if cleaned.is_empty() || cleaned.chars().any(char::is_control) {
        "unknown"
    } else {
        cleaned
    }
}

/// Returns the Git revision baked into the build, if available.
///
/// Whitespace surrounding the revision string is trimmed so the value can be embedded in version
/// banners without introducing stray spaces or newlines. When the environment variable is unset or
/// only contains whitespace the function returns `"unknown"`, mirroring upstream rsync's
/// behaviour when revision metadata is unavailable. Embedded newlines are ignored by taking the
/// first non-empty line, and control characters cause the revision to be reported as
/// `"unknown"` to avoid rendering artifacts in version banners.
#[must_use]
pub fn build_revision() -> &'static str {
    sanitize_build_revision(option_env!("OC_RSYNC_BUILD_REV"))
}

/// Returns the build information line rendered in the capability section.
#[must_use]
pub fn build_info_line() -> String {
    format!(
        "Rust rsync implementation supporting protocol version {};\n    {};\n    source: {};\n    revision/build: #{}",
        HIGHEST_PROTOCOL_VERSION,
        BUILD_TOOLCHAIN,
        SOURCE_URL,
        build_revision()
    )
}

/// Subprotocol version appended to the negotiated protocol when non-zero.
pub const SUBPROTOCOL_VERSION: u8 = 0;

/// Upstream base version that the Rust implementation tracks.
#[doc(alias = "3.4.1")]
pub const UPSTREAM_BASE_VERSION: &str = workspace::UPSTREAM_VERSION;

/// Full version string rendered by user-visible banners.
#[doc(alias = "3.4.1-rust")]
pub const RUST_VERSION: &str = workspace::RUST_VERSION;

/// Highest protocol version supported by this build.
pub const HIGHEST_PROTOCOL_VERSION: u8 = workspace::protocol_version_u8();

/// Static metadata describing the standard version banner rendered by `rsync`.
///
/// The structure mirrors upstream `print_rsync_version()` so higher layers can
/// render byte-identical banners without hard-coding strings at the call site
/// while honouring the Rust-specific branding.
/// It captures the program name, version identifiers, protocol numbers, and the
/// canonical copyright notice.
///
/// # Examples
///
/// ```
/// use rsync_core::version::{version_metadata, RUST_VERSION};
///
/// let metadata = version_metadata();
/// let banner = metadata.standard_banner();
///
/// assert!(banner.starts_with(&format!(
///     "rsync  version {} ",
///     RUST_VERSION
/// )));
/// assert!(banner.contains("protocol version 32"));
/// assert!(banner.contains("revision/build #"));
/// assert!(banner.contains("https://github.com/oferchen/rsync"));
/// ```
#[doc(alias = "--version")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionMetadata {
    program_name: &'static str,
    upstream_version: &'static str,
    rust_version: &'static str,
    protocol_version: ProtocolVersion,
    subprotocol_version: u8,
    copyright_notice: &'static str,
    web_site: &'static str,
}

impl VersionMetadata {
    /// Returns the program name rendered at the start of the banner.
    #[must_use]
    pub const fn program_name(&self) -> &'static str {
        self.program_name
    }

    /// Returns the upstream version string without the Rust suffix.
    #[must_use]
    pub const fn upstream_version(&self) -> &'static str {
        self.upstream_version
    }

    /// Returns the Rust-flavoured version string (`3.4.1-rust`).
    #[must_use]
    pub const fn rust_version(&self) -> &'static str {
        self.rust_version
    }

    /// Returns the negotiated protocol version advertised by the banner.
    #[must_use]
    pub const fn protocol_version(&self) -> ProtocolVersion {
        self.protocol_version
    }

    /// Returns the optional subprotocol used for pre-release builds.
    #[must_use]
    pub const fn subprotocol_version(&self) -> u8 {
        self.subprotocol_version
    }

    /// Returns the canonical copyright notice rendered by upstream rsync.
    #[must_use]
    pub const fn copyright_notice(&self) -> &'static str {
        self.copyright_notice
    }

    /// Returns the web site advertised by the banner.
    #[must_use]
    pub const fn web_site(&self) -> &'static str {
        self.web_site
    }

    /// Writes the standard textual banner into the provided [`fmt::Write`] sink.
    ///
    /// The formatting mirrors `print_rsync_version()` for the human-readable
    /// path. Callers that require an owned [`String`] can use
    /// [`VersionMetadata::standard_banner`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::version::version_metadata;
    ///
    /// let metadata = version_metadata();
    /// let mut rendered = String::new();
    /// metadata
    ///     .write_standard_banner(&mut rendered)
    ///     .expect("writing to a String never fails");
    ///
    /// assert!(rendered.starts_with("rsync  version"));
    /// assert!(rendered.ends_with("\n"));
    /// ```
    pub fn write_standard_banner<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        write!(
            writer,
            "{}  version {} (revision/build #{})  protocol version {}",
            self.program_name(),
            self.rust_version(),
            build_revision(),
            self.protocol_version().as_u8()
        )?;

        if self.subprotocol_version() != 0 {
            write!(writer, ".PR{}", self.subprotocol_version())?;
        }

        writer.write_char('\n')?;
        writer.write_str("Copyright ")?;
        writer.write_str(self.copyright_notice())?;
        writer.write_char('\n')?;
        writer.write_str("Web site: ")?;
        writer.write_str(self.web_site())?;
        writer.write_char('\n')
    }

    /// Returns the standard banner rendered into an owned [`String`].
    #[must_use]
    pub fn standard_banner(&self) -> String {
        let mut banner = String::new();
        self.write_standard_banner(&mut banner)
            .expect("writing to String cannot fail");
        banner
    }
}

impl Default for VersionMetadata {
    fn default() -> Self {
        version_metadata()
    }
}

/// Returns the canonical metadata used to render `--version` output.
///
/// # Examples
///
/// ```
/// use rsync_core::{branding::Brand, version::version_metadata};
///
/// let metadata = version_metadata();
/// assert_eq!(metadata.protocol_version().as_u8(), 32);
/// assert_eq!(metadata.program_name(), "rsync");
/// assert_eq!(
///     rsync_core::version::version_metadata_for_client_brand(Brand::Oc).program_name(),
///     "oc-rsync"
/// );
/// ```
#[doc(alias = "--version")]
#[must_use]
pub const fn version_metadata() -> VersionMetadata {
    version_metadata_for_client_brand(Brand::Upstream)
}

/// Returns metadata configured for the upstream-compatible `rsync` daemon banner.
#[must_use]
pub const fn daemon_version_metadata() -> VersionMetadata {
    version_metadata_for_daemon_brand(Brand::Upstream)
}

/// Returns metadata configured for the branded `oc-rsync` client banner.
#[must_use]
pub const fn oc_version_metadata() -> VersionMetadata {
    version_metadata_for_client_brand(Brand::Oc)
}

/// Returns metadata configured for the branded `oc-rsyncd` daemon banner.
#[must_use]
pub const fn oc_daemon_version_metadata() -> VersionMetadata {
    version_metadata_for_daemon_brand(Brand::Oc)
}

/// Returns version metadata tailored to the client program associated with `brand`.
#[must_use]
pub const fn version_metadata_for_client_brand(brand: Brand) -> VersionMetadata {
    version_metadata_for_program(brand.client_program_name())
}

/// Returns version metadata tailored to the daemon program associated with `brand`.
#[must_use]
pub const fn version_metadata_for_daemon_brand(brand: Brand) -> VersionMetadata {
    version_metadata_for_program(brand.daemon_program_name())
}

/// Returns version metadata that renders a banner for the supplied program name.
#[must_use]
pub const fn version_metadata_for_program(program_name: &'static str) -> VersionMetadata {
    VersionMetadata {
        program_name,
        upstream_version: UPSTREAM_BASE_VERSION,
        rust_version: RUST_VERSION,
        protocol_version: ProtocolVersion::NEWEST,
        subprotocol_version: SUBPROTOCOL_VERSION,
        copyright_notice: COPYRIGHT_NOTICE,
        web_site: WEB_SITE,
    }
}

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

/// Zero-allocation view of the compiled feature list.
///
/// [`StaticCompiledFeatures`] materialises the active capabilities at compile
/// time using the [`COMPILED_FEATURE_BITMAP`] so lookups and iterations avoid
/// allocating intermediate vectors. The structure retains the canonical
/// upstream ordering exposed by [`CompiledFeature::ALL`], making the slice view
/// suitable for rendering `--version` banners or feeding pre-sized lookup
/// tables.
///
/// # Examples
///
/// Inspect the statically computed feature slice without allocating:
///
/// ```
/// use rsync_core::version::{compiled_features_static, COMPILED_FEATURE_BITMAP};
///
/// let static_view = compiled_features_static();
/// assert_eq!(static_view.len(), static_view.as_slice().len());
/// assert_eq!(static_view.is_empty(), static_view.as_slice().is_empty());
/// assert_eq!(static_view.bitmap(), COMPILED_FEATURE_BITMAP);
/// ```
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
    ///
    /// The bitmap mirrors [`COMPILED_FEATURE_BITMAP`], allowing callers to perform constant-time
    /// membership tests or combine the static view with other pre-computed masks without
    /// re-deriving the enabled set from the slice. This keeps the helper aligned with upstream
    /// rsync's `--version` output, which prints capability labels in a deterministic order while
    /// still permitting fast bitwise comparisons when generating diagnostics.
    #[must_use]
    pub const fn bitmap(&self) -> u8 {
        self.bitmap
    }

    /// Reports whether the provided feature is part of the compiled set.
    ///
    /// The check runs in constant time by consulting the cached bitmap instead of scanning the
    /// slice, ensuring lookups stay inexpensive even if future versions expand the capability
    /// matrix. This matches upstream rsync, where feature availability is represented as bitmasks
    /// for quick diagnostics and logging decisions.
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
    compiled_features_static().as_slice().to_vec()
}

/// Returns a zero-allocation view over the compiled feature set.
///
/// The view is backed by a `static` array constructed at compile time, making
/// repeated lookups inexpensive when rendering `--version` output or producing
/// diagnostics that need to inspect optional capabilities multiple times.
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

/// Describes how secluded argument mode is advertised in `--version` output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecludedArgsMode {
    /// Secluded arguments are available when explicitly requested.
    Optional,
    /// Secluded arguments are enabled by default, matching upstream's maintainer builds.
    Default,
}

impl SecludedArgsMode {
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

    /// Returns the canonical label rendered in `--version` output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Optional => "optional secluded-args",
            Self::Default => "default secluded-args",
        }
    }

    /// Parses a label produced by [`Self::label`] back into its variant.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::version::SecludedArgsMode;
    ///
    /// const OPTIONAL: Option<SecludedArgsMode> =
    ///     SecludedArgsMode::from_label("optional secluded-args");
    /// const UNKNOWN: Option<SecludedArgsMode> =
    ///     SecludedArgsMode::from_label("disabled secluded-args");
    ///
    /// assert_eq!(OPTIONAL, Some(SecludedArgsMode::Optional));
    /// assert!(UNKNOWN.is_none());
    /// ```
    #[must_use]
    pub const fn from_label(label: &str) -> Option<Self> {
        if Self::label_eq(label, "optional secluded-args") {
            Some(Self::Optional)
        } else if Self::label_eq(label, "default secluded-args") {
            Some(Self::Default)
        } else {
            None
        }
    }
}

/// Error returned when parsing a [`SecludedArgsMode`] from text fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseSecludedArgsModeError {
    _private: (),
}

impl fmt::Display for ParseSecludedArgsModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unrecognised secluded-args mode")
    }
}

impl std::error::Error for ParseSecludedArgsModeError {}

impl fmt::Display for SecludedArgsMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for SecludedArgsMode {
    type Err = ParseSecludedArgsModeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::from_label(input).ok_or(ParseSecludedArgsModeError { _private: () })
    }
}

/// Configuration describing which capabilities the current build exposes.
///
/// The structure mirrors the feature toggles used by upstream `print_rsync_version()` when it
/// prints the capabilities and optimisation sections. Higher layers populate the fields based on
/// actual runtime support so `VersionInfoReport` can render an accurate report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionInfoConfig {
    /// Whether socketpair-based transports are available.
    pub supports_socketpairs: bool,
    /// Whether symbolic links are preserved.
    pub supports_symlinks: bool,
    /// Whether symbolic link timestamps are propagated.
    pub supports_symtimes: bool,
    /// Whether hard links are preserved.
    pub supports_hardlinks: bool,
    /// Whether hard links to special files are preserved.
    pub supports_hardlink_specials: bool,
    /// Whether hard links to symbolic links are preserved.
    pub supports_hardlink_symlinks: bool,
    /// Whether IPv6 transports are supported.
    pub supports_ipv6: bool,
    /// Whether access times are preserved.
    pub supports_atimes: bool,
    /// Whether batch file generation and replay are implemented.
    pub supports_batchfiles: bool,
    /// Whether in-place updates are supported.
    pub supports_inplace: bool,
    /// Whether append mode is supported.
    pub supports_append: bool,
    /// Whether POSIX ACL propagation is implemented.
    pub supports_acls: bool,
    /// Whether extended attribute propagation is implemented.
    pub supports_xattrs: bool,
    /// How secluded-argument support is advertised.
    pub secluded_args_mode: SecludedArgsMode,
    /// Whether iconv-based charset conversion is implemented.
    pub supports_iconv: bool,
    /// Whether preallocation is implemented.
    pub supports_prealloc: bool,
    /// Whether `--stop-at` style cut-offs are supported.
    pub supports_stop_at: bool,
    /// Whether change-time preservation is implemented.
    pub supports_crtimes: bool,
    /// Whether SIMD acceleration is used for the rolling checksum.
    pub supports_simd_roll: bool,
    /// Whether assembly acceleration is used for the rolling checksum.
    pub supports_asm_roll: bool,
    /// Whether OpenSSL-backed cryptography is available.
    pub supports_openssl_crypto: bool,
    /// Whether assembly acceleration is used for MD5.
    pub supports_asm_md5: bool,
}

impl VersionInfoConfig {
    /// Creates a configuration reflecting the currently implemented workspace capabilities.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            supports_socketpairs: false,
            supports_symlinks: cfg!(unix),
            supports_symtimes: cfg!(unix),
            supports_hardlinks: cfg!(unix),
            supports_hardlink_specials: false,
            supports_hardlink_symlinks: false,
            supports_ipv6: true,
            supports_atimes: true,
            supports_batchfiles: false,
            supports_inplace: true,
            supports_append: false,
            supports_acls: cfg!(feature = "acl"),
            supports_xattrs: cfg!(feature = "xattr"),
            secluded_args_mode: SecludedArgsMode::Optional,
            supports_iconv: cfg!(feature = "iconv"),
            supports_prealloc: true,
            supports_stop_at: false,
            supports_crtimes: false,
            supports_simd_roll: false,
            supports_asm_roll: false,
            supports_openssl_crypto: false,
            supports_asm_md5: false,
        }
    }

    /// Returns a builder for constructing customised capability configurations.
    ///
    /// The builder follows the fluent style used across the workspace, making it
    /// straightforward to toggle capabilities while reusing the compile-time
    /// defaults produced by [`VersionInfoConfig::new`]. Feature-gated entries
    /// (ACLs, xattrs, and iconv) are automatically clamped so callers cannot
    /// advertise support for capabilities that were not compiled in.
    ///
    /// # Examples
    ///
    /// Build a configuration that reports socketpair availability while keeping
    /// the ACL flag consistent with the compiled feature set.
    ///
    /// ```
    /// use rsync_core::version::{VersionInfoConfig, VersionInfoConfigBuilder};
    ///
    /// let config = VersionInfoConfig::builder()
    ///     .supports_socketpairs(true)
    ///     .supports_acls(true)
    ///     .build();
    ///
    /// assert!(config.supports_socketpairs);
    /// assert_eq!(config.supports_acls, cfg!(feature = "acl"));
    /// ```
    #[must_use]
    pub const fn builder() -> VersionInfoConfigBuilder {
        VersionInfoConfigBuilder::new()
    }

    /// Converts the configuration into a builder so individual fields can be
    /// tweaked fluently.
    #[must_use]
    pub const fn to_builder(self) -> VersionInfoConfigBuilder {
        VersionInfoConfigBuilder::from_config(self)
    }
}

impl Default for VersionInfoConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Fluent builder for [`VersionInfoConfig`].
///
/// The builder starts from the compile-time defaults exposed by
/// [`VersionInfoConfig::new`] and provides chainable setters for each capability
/// flag. It clamps ACL, xattr, and iconv support to the compiled feature set so
/// higher layers cannot misreport unavailable functionality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionInfoConfigBuilder {
    config: VersionInfoConfig,
}

impl VersionInfoConfigBuilder {
    /// Creates a builder initialised with [`VersionInfoConfig::new`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            config: VersionInfoConfig::new(),
        }
    }

    /// Creates a builder seeded with an existing configuration.
    #[must_use]
    pub const fn from_config(config: VersionInfoConfig) -> Self {
        Self { config }
    }

    /// Enables or disables socketpair support.
    #[must_use]
    pub fn supports_socketpairs(mut self, enabled: bool) -> Self {
        self.config.supports_socketpairs = enabled;
        self
    }

    /// Enables or disables symbolic link preservation.
    #[must_use]
    pub fn supports_symlinks(mut self, enabled: bool) -> Self {
        self.config.supports_symlinks = enabled;
        self
    }

    /// Enables or disables symbolic link timestamp preservation.
    #[must_use]
    pub fn supports_symtimes(mut self, enabled: bool) -> Self {
        self.config.supports_symtimes = enabled;
        self
    }

    /// Enables or disables hard link preservation.
    #[must_use]
    pub fn supports_hardlinks(mut self, enabled: bool) -> Self {
        self.config.supports_hardlinks = enabled;
        self
    }

    /// Enables or disables hard link support for special files.
    #[must_use]
    pub fn supports_hardlink_specials(mut self, enabled: bool) -> Self {
        self.config.supports_hardlink_specials = enabled;
        self
    }

    /// Enables or disables hard link support for symbolic links.
    #[must_use]
    pub fn supports_hardlink_symlinks(mut self, enabled: bool) -> Self {
        self.config.supports_hardlink_symlinks = enabled;
        self
    }

    /// Enables or disables IPv6 transport support.
    #[must_use]
    pub fn supports_ipv6(mut self, enabled: bool) -> Self {
        self.config.supports_ipv6 = enabled;
        self
    }

    /// Enables or disables access-time preservation.
    #[must_use]
    pub fn supports_atimes(mut self, enabled: bool) -> Self {
        self.config.supports_atimes = enabled;
        self
    }

    /// Enables or disables batch file support.
    #[must_use]
    pub fn supports_batchfiles(mut self, enabled: bool) -> Self {
        self.config.supports_batchfiles = enabled;
        self
    }

    /// Enables or disables in-place update support.
    #[must_use]
    pub fn supports_inplace(mut self, enabled: bool) -> Self {
        self.config.supports_inplace = enabled;
        self
    }

    /// Enables or disables append mode support.
    #[must_use]
    pub fn supports_append(mut self, enabled: bool) -> Self {
        self.config.supports_append = enabled;
        self
    }

    /// Enables or disables ACL propagation, clamped to the compiled feature set.
    #[must_use]
    pub fn supports_acls(mut self, enabled: bool) -> Self {
        self.config.supports_acls = enabled && cfg!(feature = "acl");
        self
    }

    /// Enables or disables extended attribute propagation, clamped to the compiled feature set.
    #[must_use]
    pub fn supports_xattrs(mut self, enabled: bool) -> Self {
        self.config.supports_xattrs = enabled && cfg!(feature = "xattr");
        self
    }

    /// Sets the advertised secluded-argument mode.
    #[must_use]
    pub fn secluded_args_mode(mut self, mode: SecludedArgsMode) -> Self {
        self.config.secluded_args_mode = mode;
        self
    }

    /// Enables or disables iconv charset conversion, clamped to the compiled feature set.
    #[must_use]
    pub fn supports_iconv(mut self, enabled: bool) -> Self {
        self.config.supports_iconv = enabled && cfg!(feature = "iconv");
        self
    }

    /// Enables or disables preallocation support.
    #[must_use]
    pub fn supports_prealloc(mut self, enabled: bool) -> Self {
        self.config.supports_prealloc = enabled;
        self
    }

    /// Enables or disables `--stop-at` style cut-off support.
    #[must_use]
    pub fn supports_stop_at(mut self, enabled: bool) -> Self {
        self.config.supports_stop_at = enabled;
        self
    }

    /// Enables or disables change-time preservation.
    #[must_use]
    pub fn supports_crtimes(mut self, enabled: bool) -> Self {
        self.config.supports_crtimes = enabled;
        self
    }

    /// Enables or disables SIMD-accelerated rolling checksums.
    #[must_use]
    pub fn supports_simd_roll(mut self, enabled: bool) -> Self {
        self.config.supports_simd_roll = enabled;
        self
    }

    /// Enables or disables assembly-accelerated rolling checksums.
    #[must_use]
    pub fn supports_asm_roll(mut self, enabled: bool) -> Self {
        self.config.supports_asm_roll = enabled;
        self
    }

    /// Enables or disables OpenSSL-backed cryptography support.
    #[must_use]
    pub fn supports_openssl_crypto(mut self, enabled: bool) -> Self {
        self.config.supports_openssl_crypto = enabled;
        self
    }

    /// Enables or disables assembly-accelerated MD5.
    #[must_use]
    pub fn supports_asm_md5(mut self, enabled: bool) -> Self {
        self.config.supports_asm_md5 = enabled;
        self
    }

    /// Finalises the builder and returns the constructed configuration.
    #[must_use]
    pub const fn build(self) -> VersionInfoConfig {
        self.config
    }
}

impl Default for VersionInfoConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Human-readable `--version` output renderer.
///
/// Instances of this type use [`VersionMetadata`] together with [`VersionInfoConfig`] to reproduce
/// upstream rsync's capability report. Callers may override the checksum, compression, and daemon
/// authentication lists to match the negotiated feature set of the final binary. When rendering
/// banners for a different binary (for example, `rsyncd`), construct the report with
/// [`with_program_name`](Self::with_program_name) so the prologue reflects the appropriate binary
/// name while retaining all other metadata.
#[derive(Clone, Debug)]
pub struct VersionInfoReport {
    metadata: VersionMetadata,
    config: VersionInfoConfig,
    checksum_algorithms: Vec<Cow<'static, str>>,
    compress_algorithms: Vec<Cow<'static, str>>,
    daemon_auth_algorithms: Vec<Cow<'static, str>>,
}

impl Default for VersionInfoReport {
    fn default() -> Self {
        Self::new(VersionInfoConfig::default())
    }
}

impl VersionInfoReport {
    /// Creates a report using the supplied configuration and default algorithm lists.
    #[must_use]
    pub fn new(config: VersionInfoConfig) -> Self {
        Self::with_metadata(version_metadata(), config)
    }

    /// Creates a report using explicit version metadata and default algorithm lists.
    #[must_use]
    pub fn with_metadata(metadata: VersionMetadata, config: VersionInfoConfig) -> Self {
        Self {
            metadata,
            config,
            checksum_algorithms: default_checksum_algorithms(),
            compress_algorithms: default_compress_algorithms(),
            daemon_auth_algorithms: default_daemon_auth_algorithms(),
        }
    }

    /// Returns the configuration associated with the report.
    #[must_use]
    pub const fn config(&self) -> &VersionInfoConfig {
        &self.config
    }

    /// Returns the metadata associated with the report.
    #[must_use]
    pub const fn metadata(&self) -> VersionMetadata {
        self.metadata
    }

    /// Returns a report with the supplied program name.
    #[must_use]
    pub fn with_program_name(mut self, program_name: &'static str) -> Self {
        self.metadata = version_metadata_for_program(program_name);
        self
    }

    /// Returns a report using the client program name associated with `brand`.
    ///
    /// The helper preserves capability listings and other banner metadata while
    /// swapping the program identifier so wrappers such as `oc-rsync` can
    /// present branded banners without re-implementing the formatting logic.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{branding::Brand, version::VersionInfoReport};
    ///
    /// let report = VersionInfoReport::default().with_client_brand(Brand::Oc);
    /// let banner = report.metadata().standard_banner();
    /// assert!(banner.starts_with("oc-rsync  version"));
    /// ```
    #[must_use]
    pub fn with_client_brand(self, brand: Brand) -> Self {
        self.with_program_name(brand.client_program_name())
    }

    /// Returns a report using the daemon program name associated with `brand`.
    ///
    /// This mirrors [`with_client_brand`](Self::with_client_brand) but targets
    /// the daemon wrapper so the same version metadata code path renders both
    /// `rsyncd` and `oc-rsyncd` banners without manual string handling in the
    /// caller.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{branding::Brand, version::VersionInfoReport};
    ///
    /// let report = VersionInfoReport::default().with_daemon_brand(Brand::Oc);
    /// let banner = report.metadata().standard_banner();
    /// assert!(banner.starts_with("oc-rsyncd  version"));
    /// ```
    #[must_use]
    pub fn with_daemon_brand(self, brand: Brand) -> Self {
        self.with_program_name(brand.daemon_program_name())
    }

    /// Replaces the checksum algorithm list used in the rendered report.
    #[must_use]
    pub fn with_checksum_algorithms<I, S>(mut self, algorithms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Cow<'static, str>>,
    {
        self.checksum_algorithms = algorithms.into_iter().map(Into::into).collect();
        self
    }

    /// Replaces the compression algorithm list used in the rendered report.
    #[must_use]
    pub fn with_compress_algorithms<I, S>(mut self, algorithms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Cow<'static, str>>,
    {
        self.compress_algorithms = algorithms.into_iter().map(Into::into).collect();
        self
    }

    /// Replaces the daemon authentication algorithm list used in the rendered report.
    #[must_use]
    pub fn with_daemon_auth_algorithms<I, S>(mut self, algorithms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Cow<'static, str>>,
    {
        self.daemon_auth_algorithms = algorithms.into_iter().map(Into::into).collect();
        self
    }

    /// Writes the full human-readable `--version` output into the provided writer.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::version::{VersionInfoConfig, VersionInfoReport};
    ///
    /// let report = VersionInfoReport::new(VersionInfoConfig::default());
    /// let mut rendered = String::new();
    /// report
    ///     .write_human_readable(&mut rendered)
    ///     .expect("writing to String cannot fail");
    ///
    /// assert!(rendered.contains("Capabilities:"));
    /// assert!(rendered.contains("Checksum list:"));
    /// ```
    pub fn write_human_readable<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        self.metadata.write_standard_banner(writer)?;
        self.write_info_sections(writer)?;
        self.write_named_list(writer, "Checksum list", &self.checksum_algorithms)?;
        self.write_named_list(writer, "Compress list", &self.compress_algorithms)?;
        self.write_named_list(writer, "Daemon auth list", &self.daemon_auth_algorithms)?;
        writer.write_char('\n')?;
        writer.write_str(
            "rsync comes with ABSOLUTELY NO WARRANTY.  This is free software, and you\n",
        )?;
        writer
            .write_str("are welcome to redistribute it under certain conditions.  See the GNU\n")?;
        writer.write_str("General Public Licence for details.\n")
    }

    /// Returns the rendered report as an owned string.
    #[must_use]
    pub fn human_readable(&self) -> String {
        let mut rendered = String::new();
        self.write_human_readable(&mut rendered)
            .expect("writing to String cannot fail");
        rendered
    }

    fn write_info_sections<W: FmtWrite>(&self, writer: &mut W) -> fmt::Result {
        let mut buffer = String::new();
        let mut items = self.info_items().into_iter().peekable();

        while let Some(item) = items.next() {
            match item {
                InfoItem::Section(name) => {
                    if !buffer.is_empty() {
                        writeln!(writer, "   {}", buffer)?;
                        buffer.clear();
                    }
                    writeln!(writer, "{}:", name)?;
                }
                InfoItem::Entry(text) => {
                    let needs_comma = matches!(items.peek(), Some(InfoItem::Entry(_)));
                    let mut formatted = String::with_capacity(text.len() + 3);
                    formatted.push(' ');
                    formatted.push_str(text.as_ref());
                    if needs_comma {
                        formatted.push(',');
                    }

                    if !buffer.is_empty() && buffer.len() + formatted.len() >= 75 {
                        writeln!(writer, "   {}", buffer)?;
                        buffer.clear();
                    }

                    buffer.push_str(&formatted);
                }
            }
        }

        if !buffer.is_empty() {
            writeln!(writer, "   {}", buffer)?;
        }

        Ok(())
    }

    fn write_named_list<W: FmtWrite>(
        &self,
        writer: &mut W,
        name: &str,
        entries: &[Cow<'static, str>],
    ) -> fmt::Result {
        writeln!(writer, "{}:", name)?;

        if entries.is_empty() {
            writeln!(writer, "    none")
        } else {
            writer.write_str("    ")?;
            for (index, entry) in entries.iter().enumerate() {
                if index > 0 {
                    writer.write_char(' ')?;
                }
                writer.write_str(entry.as_ref())?;
            }
            writer.write_char('\n')
        }
    }

    fn info_items(&self) -> Vec<InfoItem> {
        const BASE_CAPACITY: usize = 32;

        let config = self.config;
        let mut items = Vec::with_capacity(BASE_CAPACITY);

        items.push(InfoItem::Section("Capabilities"));
        items.push(bits_entry::<off_t>("files"));
        items.push(bits_entry::<ino_t>("inums"));
        items.push(bits_entry::<time_t>("timestamps"));
        items.push(bits_entry::<i64>("long ints"));
        items.push(capability_entry("socketpairs", config.supports_socketpairs));
        items.push(capability_entry("symlinks", config.supports_symlinks));
        items.push(capability_entry("symtimes", config.supports_symtimes));
        items.push(capability_entry("hardlinks", config.supports_hardlinks));
        items.push(capability_entry(
            "hardlink-specials",
            config.supports_hardlink_specials,
        ));
        items.push(capability_entry(
            "hardlink-symlinks",
            config.supports_hardlink_symlinks,
        ));
        items.push(capability_entry("IPv6", config.supports_ipv6));
        items.push(capability_entry("atimes", config.supports_atimes));
        items.push(capability_entry("batchfiles", config.supports_batchfiles));
        items.push(capability_entry("inplace", config.supports_inplace));
        items.push(capability_entry("append", config.supports_append));
        items.push(capability_entry("ACLs", config.supports_acls));
        items.push(capability_entry("xattrs", config.supports_xattrs));
        items.push(InfoItem::Entry(Cow::Borrowed(
            config.secluded_args_mode.label(),
        )));
        items.push(capability_entry("iconv", config.supports_iconv));
        items.push(capability_entry("prealloc", config.supports_prealloc));
        items.push(capability_entry("stop-at", config.supports_stop_at));
        items.push(capability_entry("crtimes", config.supports_crtimes));
        items.push(InfoItem::Section("Optimizations"));
        items.push(capability_entry("SIMD-roll", config.supports_simd_roll));
        items.push(capability_entry("asm-roll", config.supports_asm_roll));
        items.push(capability_entry(
            "openssl-crypto",
            config.supports_openssl_crypto,
        ));
        items.push(capability_entry("asm-MD5", config.supports_asm_md5));

        items.push(InfoItem::Section("Compiled features"));
        let compiled_features = compiled_features_display();
        if compiled_features.is_empty() {
            items.push(InfoItem::Entry(Cow::Borrowed("none")));
        } else {
            items.push(InfoItem::Entry(Cow::Owned(compiled_features.to_string())));
        }

        items.push(InfoItem::Section("Build info"));
        items.push(InfoItem::Entry(Cow::Owned(build_info_line())));

        debug_assert!(items.capacity() >= BASE_CAPACITY);
        items
    }
}

fn default_checksum_algorithms() -> Vec<Cow<'static, str>> {
    vec![
        Cow::Borrowed("xxh128"),
        Cow::Borrowed("xxh3"),
        Cow::Borrowed("xxh64"),
        Cow::Borrowed("md5"),
        Cow::Borrowed("md4"),
        Cow::Borrowed("none"),
    ]
}

fn default_compress_algorithms() -> Vec<Cow<'static, str>> {
    let mut algorithms = Vec::new();

    if cfg!(feature = "zstd") {
        algorithms.push(Cow::Borrowed("zstd"));
    }

    algorithms.push(Cow::Borrowed("none"));
    algorithms
}

fn default_daemon_auth_algorithms() -> Vec<Cow<'static, str>> {
    vec![Cow::Borrowed("md5"), Cow::Borrowed("md4")]
}

#[derive(Clone, Debug)]
enum InfoItem {
    Section(&'static str),
    Entry(Cow<'static, str>),
}

fn bits_entry<T>(label: &'static str) -> InfoItem {
    let bits = mem::size_of::<T>() * 8;
    InfoItem::Entry(Cow::Owned(format!("{}-bit {}", bits, label)))
}

fn capability_entry(label: &'static str, supported: bool) -> InfoItem {
    if supported {
        InfoItem::Entry(Cow::Borrowed(label))
    } else {
        InfoItem::Entry(Cow::Owned(format!("no {}", label)))
    }
}

#[cfg(test)]
mod tests;
