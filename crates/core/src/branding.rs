#![deny(unsafe_code)]

//! Branding constants shared across the workspace.
//!
//! The `branding` module centralises the program names and filesystem
//! locations that the workspace exposes publicly. Higher-level crates rely on
//! these constants when rendering banners or searching for configuration files
//! so that packaging, documentation, and runtime behaviour remain aligned. The
//! module records both the upstream-compatible `rsync`/`rsyncd` names (used by
//! symlinks and remote invocations) and the branded `oc-rsync`/`oc-rsyncd`
//! binaries together with convenience accessors
//! that allow the CLI and daemon crates to select the correct identity for a
//! given execution mode. By funnelling branding details through this module we
//! keep string literals out of business logic and make it trivial to update
//! paths or names in one place.
//!
//! # Examples
//!
//! Retrieve the canonical daemon configuration directory and secrets paths that
//! `oc-rsyncd` uses when launched without explicit overrides:
//!
//! ```rust
//! use std::path::Path;
//!
//! let config_dir = rsync_core::branding::oc_daemon_config_dir();
//! let config = rsync_core::branding::oc_daemon_config_path();
//! let secrets = rsync_core::branding::oc_daemon_secrets_path();
//!
//! assert_eq!(config_dir, Path::new("/etc/oc-rsyncd"));
//! assert_eq!(config, Path::new("/etc/oc-rsyncd/oc-rsyncd.conf"));
//! assert_eq!(secrets, Path::new("/etc/oc-rsyncd/oc-rsyncd.secrets"));
//! ```

use core::str::FromStr;
use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::path::Path;

/// Identifies the brand associated with an executable name.
///
/// The workspace recognises both upstream-compatible names (`rsync`/`rsyncd`),
/// typically provided via symlinks or remote invocations, and the branded
/// binaries (`oc-rsync`/`oc-rsyncd`). Centralising the mapping keeps
/// higher layers free from string comparisons and ensures configuration paths,
/// help banners, and diagnostics stay consistent across entry points. The
/// [`Brand::profile`] method exposes the corresponding [`BrandProfile`], which in
/// turn provides program names and filesystem locations for the selected
/// distribution.
///
/// `Brand` implements [`FromStr`], allowing environment variables such as
/// [`OC_RSYNC_BRAND`][brand_override_env_var] to accept human-readable aliases.
/// The parser tolerates ASCII case differences, leading/trailing whitespace, and
/// versioned program names:
///
/// ```
/// use core::str::FromStr;
/// use rsync_core::branding::Brand;
///
/// assert_eq!(Brand::from_str(" oc-rsync-3.4.1 ").unwrap(), Brand::Oc);
/// assert_eq!(Brand::from_str("RSYNCD").unwrap(), Brand::Upstream);
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Brand {
    /// Upstream-compatible binaries (`rsync` and `rsyncd`).
    Upstream,
    /// Branded binaries installed as `oc-rsync` and `oc-rsyncd`.
    Oc,
}

/// Error returned when parsing a [`Brand`] from an unrecognised string fails.
///
/// Parsing accepts ASCII case-insensitive aliases for both the upstream and
/// branded binaries. Accepted values include `"oc"`, `"oc-rsync"`,
/// `"oc-rsyncd"`, `"upstream"`, `"rsync"`, and `"rsyncd"`, as well as
/// versioned variants such as `"oc-rsync-3.4.1"`. Whitespace surrounding the
/// input is ignored. Any other value triggers `BrandParseError` so callers can
/// fall back to defaults or surface a diagnostic to the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrandParseError;

impl fmt::Display for BrandParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unrecognised brand; expected oc or upstream aliases")
    }
}

impl std::error::Error for BrandParseError {}

impl FromStr for Brand {
    type Err = BrandParseError;

    fn from_str(mut s: &str) -> Result<Self, Self::Err> {
        s = s.trim();
        if s.is_empty() {
            return Err(BrandParseError);
        }

        if s.eq_ignore_ascii_case(Brand::Oc.label()) {
            return Ok(Brand::Oc);
        }

        if s.eq_ignore_ascii_case(Brand::Upstream.label()) {
            return Ok(Brand::Upstream);
        }

        if matches_any_program_alias(s, &[OC_CLIENT_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME]) {
            return Ok(Brand::Oc);
        }

        if matches_any_program_alias(
            s,
            &[UPSTREAM_CLIENT_PROGRAM_NAME, UPSTREAM_DAEMON_PROGRAM_NAME],
        ) {
            return Ok(Brand::Upstream);
        }

        Err(BrandParseError)
    }
}

impl Brand {
    /// Returns the canonical, human-readable label associated with the brand.
    ///
    /// The label matches the identifiers accepted by [`Brand::from_str`] and
    /// rendered by [`Display`](fmt::Display). Higher-level components can
    /// surface the selected brand without duplicating string constants by
    /// delegating to this accessor.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::branding::Brand;
    ///
    /// assert_eq!(Brand::Oc.label(), "oc");
    /// assert_eq!(Brand::Upstream.label(), "upstream");
    /// ```
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Upstream => "upstream",
            Self::Oc => "oc",
        }
    }

    /// Returns the [`BrandProfile`] describing this brand.
    #[must_use]
    pub const fn profile(self) -> BrandProfile {
        match self {
            Self::Upstream => UPSTREAM_PROFILE,
            Self::Oc => OC_PROFILE,
        }
    }

    /// Returns the canonical client program name for this brand.
    #[must_use]
    pub const fn client_program_name(self) -> &'static str {
        self.profile().client_program_name()
    }

    /// Returns the canonical daemon program name for this brand.
    #[must_use]
    pub const fn daemon_program_name(self) -> &'static str {
        self.profile().daemon_program_name()
    }

    /// Returns the preferred daemon configuration directory for this brand.
    #[must_use]
    pub const fn daemon_config_dir_str(self) -> &'static str {
        self.profile().daemon_config_dir_str()
    }

    /// Returns the preferred daemon configuration directory as a [`Path`].
    #[must_use]
    pub fn daemon_config_dir(self) -> &'static Path {
        self.profile().daemon_config_dir()
    }

    /// Returns the canonical daemon configuration path for this brand.
    ///
    /// The value reflects the configuration file packaged with the binary
    /// distribution. Branded invocations (`oc-rsyncd`) resolve to
    /// `/etc/oc-rsyncd/oc-rsyncd.conf` while upstream-compatible invocations
    /// default to `/etc/rsyncd.conf`.
    #[must_use]
    pub const fn daemon_config_path_str(self) -> &'static str {
        self.profile().daemon_config_path_str()
    }

    /// Returns the canonical daemon configuration path as a [`Path`].
    #[must_use]
    pub fn daemon_config_path(self) -> &'static Path {
        self.profile().daemon_config_path()
    }

    /// Returns the preferred daemon configuration search order for this brand.
    ///
    /// The branded `oc-` binaries consult `/etc/oc-rsyncd/oc-rsyncd.conf`
    /// first and only fall back to the legacy `/etc/rsyncd.conf` when the
    /// branded path is absent. Invocations that use the upstream names
    /// (`rsync`/`rsyncd`) invert that order so existing deployments keep
    /// working without configuration changes.
    #[must_use]
    pub const fn config_path_candidate_strs(self) -> [&'static str; 2] {
        match self {
            Self::Oc => [OC_DAEMON_CONFIG_PATH, LEGACY_DAEMON_CONFIG_PATH],
            Self::Upstream => [LEGACY_DAEMON_CONFIG_PATH, OC_DAEMON_CONFIG_PATH],
        }
    }

    /// Returns the preferred daemon configuration search order as [`Path`]s.
    #[must_use]
    pub fn config_path_candidates(self) -> [&'static Path; 2] {
        let [primary, secondary] = self.config_path_candidate_strs();
        [Path::new(primary), Path::new(secondary)]
    }

    /// Returns the preferred secrets-file search order for this brand.
    ///
    /// Similar to [`Self::config_path_candidate_strs`], the branded binaries
    /// prefer `/etc/oc-rsyncd/oc-rsyncd.secrets` while invocations that use the
    /// upstream names continue to read `/etc/rsyncd.secrets` by default.
    #[must_use]
    pub const fn secrets_path_candidate_strs(self) -> [&'static str; 2] {
        match self {
            Self::Oc => [OC_DAEMON_SECRETS_PATH, LEGACY_DAEMON_SECRETS_PATH],
            Self::Upstream => [LEGACY_DAEMON_SECRETS_PATH, OC_DAEMON_SECRETS_PATH],
        }
    }

    /// Returns the canonical daemon secrets path for this brand.
    ///
    /// Branded invocations resolve to `/etc/oc-rsyncd/oc-rsyncd.secrets` while
    /// upstream-compatible invocations default to `/etc/rsyncd.secrets`.
    #[must_use]
    pub const fn daemon_secrets_path_str(self) -> &'static str {
        self.profile().daemon_secrets_path_str()
    }

    /// Returns the canonical daemon secrets path as a [`Path`].
    #[must_use]
    pub fn daemon_secrets_path(self) -> &'static Path {
        self.profile().daemon_secrets_path()
    }

    /// Returns the preferred secrets-file search order as [`Path`]s.
    #[must_use]
    pub fn secrets_path_candidates(self) -> [&'static Path; 2] {
        let [primary, secondary] = self.secrets_path_candidate_strs();
        [Path::new(primary), Path::new(secondary)]
    }
}

impl fmt::Display for Brand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Describes the public-facing identity used by a binary distribution.
///
/// The structure captures the canonical client and daemon program names
/// together with the configuration directory, configuration file, and secrets
/// file that ship with the distribution. Higher layers select the appropriate
/// [`BrandProfile`] to render banners, locate configuration files, or display
/// diagnostic messages without duplicating string literals across the
/// codebase. The profiles are intentionally lightweight and `Copy` so they can
/// be used in constant contexts such as rustdoc examples and compile-time
/// assertions.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BrandProfile {
    client_program_name: &'static str,
    daemon_program_name: &'static str,
    daemon_config_dir: &'static str,
    daemon_config_path: &'static str,
    daemon_secrets_path: &'static str,
}

/// Environment variable that forces a specific [`Brand`] at runtime.
#[doc(alias = "OC_RSYNC_BRAND")]
pub const BRAND_OVERRIDE_ENV: &str = "OC_RSYNC_BRAND";

/// Returns the environment variable that forces a specific [`Brand`] at runtime.
#[must_use]
pub const fn brand_override_env_var() -> &'static str {
    BRAND_OVERRIDE_ENV
}

impl BrandProfile {
    /// Creates a new [`BrandProfile`] describing a branded distribution.
    #[must_use]
    pub const fn new(
        client_program_name: &'static str,
        daemon_program_name: &'static str,
        daemon_config_dir: &'static str,
        daemon_config_path: &'static str,
        daemon_secrets_path: &'static str,
    ) -> Self {
        Self {
            client_program_name,
            daemon_program_name,
            daemon_config_dir,
            daemon_config_path,
            daemon_secrets_path,
        }
    }

    /// Returns the client program name associated with the profile.
    #[must_use]
    pub const fn client_program_name(&self) -> &'static str {
        self.client_program_name
    }

    /// Returns the daemon program name associated with the profile.
    #[must_use]
    pub const fn daemon_program_name(&self) -> &'static str {
        self.daemon_program_name
    }

    /// Returns the daemon configuration directory as a string slice.
    #[must_use]
    pub const fn daemon_config_dir_str(&self) -> &'static str {
        self.daemon_config_dir
    }

    /// Returns the daemon configuration directory as a [`Path`].
    #[must_use]
    pub fn daemon_config_dir(&self) -> &'static Path {
        Path::new(self.daemon_config_dir)
    }

    /// Returns the daemon configuration path as a string slice.
    #[must_use]
    pub const fn daemon_config_path_str(&self) -> &'static str {
        self.daemon_config_path
    }

    /// Returns the daemon secrets path as a string slice.
    #[must_use]
    pub const fn daemon_secrets_path_str(&self) -> &'static str {
        self.daemon_secrets_path
    }

    /// Returns the daemon configuration path as a [`Path`].
    #[must_use]
    pub fn daemon_config_path(&self) -> &'static Path {
        Path::new(self.daemon_config_path)
    }

    /// Returns the daemon secrets path as a [`Path`].
    #[must_use]
    pub fn daemon_secrets_path(&self) -> &'static Path {
        Path::new(self.daemon_secrets_path)
    }
}

const UPSTREAM_PROFILE: BrandProfile = BrandProfile::new(
    UPSTREAM_CLIENT_PROGRAM_NAME,
    UPSTREAM_DAEMON_PROGRAM_NAME,
    LEGACY_DAEMON_CONFIG_DIR,
    LEGACY_DAEMON_CONFIG_PATH,
    LEGACY_DAEMON_SECRETS_PATH,
);

const OC_PROFILE: BrandProfile = BrandProfile::new(
    OC_CLIENT_PROGRAM_NAME,
    OC_DAEMON_PROGRAM_NAME,
    OC_DAEMON_CONFIG_DIR,
    OC_DAEMON_CONFIG_PATH,
    OC_DAEMON_SECRETS_PATH,
);

/// Canonical program name used by upstream `rsync` releases.
#[doc(alias = "rsync")]
pub const UPSTREAM_CLIENT_PROGRAM_NAME: &str = "rsync";

/// Canonical program name used by upstream `rsyncd` daemon releases.
#[doc(alias = "rsyncd")]
pub const UPSTREAM_DAEMON_PROGRAM_NAME: &str = "rsyncd";

/// Canonical binary name exposed by the client wrapper packaged as `oc-rsync`.
#[doc(alias = "oc-rsync")]
pub const OC_CLIENT_PROGRAM_NAME: &str = "oc-rsync";

/// Canonical binary name exposed by the branded daemon wrapper packaged as `oc-rsyncd`.
#[doc(alias = "oc-rsyncd")]
pub const OC_DAEMON_PROGRAM_NAME: &str = "oc-rsyncd";

/// Directory that packages install for daemon configuration snippets.
#[doc(alias = "/etc/oc-rsyncd")]
pub const OC_DAEMON_CONFIG_DIR: &str = "/etc/oc-rsyncd";

/// Default configuration file path consumed by the daemon when no override is provided.
#[doc(alias = "/etc/oc-rsyncd/oc-rsyncd.conf")]
pub const OC_DAEMON_CONFIG_PATH: &str = "/etc/oc-rsyncd/oc-rsyncd.conf";

/// Default secrets file path consumed by the daemon when no override is provided.
#[doc(alias = "/etc/oc-rsyncd/oc-rsyncd.secrets")]
pub const OC_DAEMON_SECRETS_PATH: &str = "/etc/oc-rsyncd/oc-rsyncd.secrets";

/// Legacy configuration file path supported for backwards compatibility with upstream deployments.
#[doc(alias = "/etc/rsyncd.conf")]
pub const LEGACY_DAEMON_CONFIG_PATH: &str = "/etc/rsyncd.conf";

/// Legacy configuration directory that hosts upstream-compatible configuration files.
#[doc(alias = "/etc")]
pub const LEGACY_DAEMON_CONFIG_DIR: &str = "/etc";

/// Legacy secrets file path supported for backwards compatibility with upstream deployments.
#[doc(alias = "/etc/rsyncd.secrets")]
pub const LEGACY_DAEMON_SECRETS_PATH: &str = "/etc/rsyncd.secrets";

/// Returns the canonical upstream client program name (`rsync`).
#[must_use]
pub const fn upstream_client_program_name() -> &'static str {
    UPSTREAM_PROFILE.client_program_name()
}

/// Returns the canonical upstream daemon program name (`rsyncd`).
#[must_use]
pub const fn upstream_daemon_program_name() -> &'static str {
    UPSTREAM_PROFILE.daemon_program_name()
}

/// Returns the canonical client program name for upstream-compatible binaries.
#[must_use]
pub const fn client_program_name() -> &'static str {
    upstream_client_program_name()
}

/// Returns the canonical client program name as an [`OsStr`].
///
/// The helper avoids repeating `OsStr::new(client_program_name())` at call sites,
/// keeping conversions centralised alongside the raw string constant. The
/// returned slice borrows the compile-time string and therefore lives for the
/// duration of the program.
#[must_use]
pub fn client_program_name_os_str() -> &'static OsStr {
    OsStr::new(client_program_name())
}

/// Returns the canonical daemon program name for upstream-compatible binaries.
#[must_use]
pub const fn daemon_program_name() -> &'static str {
    upstream_daemon_program_name()
}

/// Returns the canonical daemon program name as an [`OsStr`].
#[must_use]
pub fn daemon_program_name_os_str() -> &'static OsStr {
    OsStr::new(daemon_program_name())
}

/// Returns the branded client program name exposed as `oc-rsync`.
#[must_use]
pub const fn oc_client_program_name() -> &'static str {
    OC_CLIENT_PROGRAM_NAME
}

/// Returns the branded client program name as an [`OsStr`].
#[must_use]
pub fn oc_client_program_name_os_str() -> &'static OsStr {
    OsStr::new(oc_client_program_name())
}

/// Returns the branded daemon program name exposed as `oc-rsyncd`.
#[must_use]
pub const fn oc_daemon_program_name() -> &'static str {
    OC_DAEMON_PROGRAM_NAME
}

/// Returns the branded daemon program name as an [`OsStr`].
#[must_use]
pub fn oc_daemon_program_name_os_str() -> &'static OsStr {
    OsStr::new(oc_daemon_program_name())
}

/// Returns the canonical configuration directory used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_config_dir() -> &'static Path {
    oc_profile().daemon_config_dir()
}

/// Returns the canonical configuration path used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_config_path() -> &'static Path {
    oc_profile().daemon_config_path()
}

/// Returns the canonical secrets path used by `oc-rsyncd`.
#[must_use]
pub fn oc_daemon_secrets_path() -> &'static Path {
    oc_profile().daemon_secrets_path()
}

/// Returns the branding profile that matches the provided program name.
///
/// The helper inspects the supplied stem (for example the output of
/// [`Path::file_stem`]) and returns [`Brand::Oc`] when the binary belongs to the
/// branded `oc-` family. The comparison tolerates versioned wrapper names such
/// as `oc-rsync-3.4.1` or `oc-rsyncd_v2` so distribution-specific symlinks keep
/// their branded behaviour without additional configuration. All other names
/// fall back to the upstream-compatible profile so symlinked invocations using
/// the upstream names keep their semantics aligned with the reference
/// implementation.
///
/// # Examples
///
/// ```
/// use rsync_core::branding;
///
/// assert_eq!(
///     branding::brand_for_program_name("oc-rsync"),
///     branding::Brand::Oc
/// );
/// assert_eq!(
///     branding::brand_for_program_name("OC-RSYNC"),
///     branding::Brand::Oc
/// );
/// assert_eq!(
///     branding::brand_for_program_name("rsync"),
///     branding::Brand::Upstream
/// );
/// ```
/// The comparison is ASCII case-insensitive so that binaries launched on
/// case-preserving filesystems (for example Windows) still select the correct
/// brand even when the executable name was uppercased.
#[must_use]
pub fn brand_for_program_name(program: &str) -> Brand {
    if matches_program_alias(program, OC_CLIENT_PROGRAM_NAME)
        || matches_program_alias(program, OC_DAEMON_PROGRAM_NAME)
    {
        Brand::Oc
    } else {
        Brand::Upstream
    }
}

fn matches_program_alias(program: &str, canonical: &str) -> bool {
    if program.eq_ignore_ascii_case(canonical) {
        return true;
    }

    let Some(prefix) = program.get(..canonical.len()) else {
        return false;
    };

    if !prefix.eq_ignore_ascii_case(canonical) {
        return false;
    }

    program
        .get(canonical.len()..)
        .and_then(|suffix| suffix.chars().next())
        .is_some_and(|separator| matches!(separator, '-' | '_' | '.'))
}

fn matches_any_program_alias(value: &str, programs: &[&str]) -> bool {
    programs
        .iter()
        .any(|canonical| matches_program_alias(value, canonical))
}

/// Detects the [`Brand`] associated with an invocation argument.
///
/// The helper mirrors the logic used by the client and daemon front-ends when
/// determining whether the binary was invoked as `rsync`/`rsyncd` or via the
/// branded binaries (`oc-rsync`/`oc-rsyncd`). Calls first consult the
/// [`OC_RSYNC_BRAND` environment variable][brand_override_env_var] so packaging
/// scripts and integration tests can force a specific identity regardless of
/// the executable name. When no override is present, the function inspects the
/// stem of the first argument (commonly `argv[0]`), stripping directory
/// prefixes and filename extensions before delegating to
/// [`brand_for_program_name`]. If the program name is unavailable the
/// upstream-compatible brand is assumed, matching the behaviour expected by
/// remote invocations and compatibility symlinks.
///
/// # Examples
///
/// ```
/// use std::ffi::OsStr;
///
/// use rsync_core::branding::{self, Brand};
///
/// assert_eq!(
///     branding::detect_brand(Some(OsStr::new("/usr/bin/oc-rsync"))),
///     Brand::Oc
/// );
/// assert_eq!(
///     branding::detect_brand(Some(OsStr::new("rsync"))),
///     Brand::Upstream
/// );
/// assert_eq!(branding::detect_brand(None), Brand::Upstream);
/// ```
#[must_use]
pub fn detect_brand(program: Option<&OsStr>) -> Brand {
    if let Some(brand) = brand_override_from_env() {
        return brand;
    }

    program
        .and_then(|arg| Path::new(arg).file_stem())
        .and_then(|stem| stem.to_str())
        .map(brand_for_program_name)
        .unwrap_or(Brand::Upstream)
}

fn brand_override_from_env() -> Option<Brand> {
    let value = env::var_os(BRAND_OVERRIDE_ENV)?;
    if value.is_empty() {
        return None;
    }

    let value = value.to_string_lossy();
    value.trim().parse::<Brand>().ok()
}

/// Returns the legacy configuration path recognised for compatibility with upstream deployments.
#[must_use]
pub fn legacy_daemon_config_path() -> &'static Path {
    UPSTREAM_PROFILE.daemon_config_path()
}

/// Returns the legacy configuration directory recognised for upstream deployments.
#[must_use]
pub fn legacy_daemon_config_dir() -> &'static Path {
    UPSTREAM_PROFILE.daemon_config_dir()
}

/// Returns the legacy secrets path recognised for compatibility with upstream deployments.
#[must_use]
pub fn legacy_daemon_secrets_path() -> &'static Path {
    UPSTREAM_PROFILE.daemon_secrets_path()
}

/// Returns the upstream-compatible branding profile used by invocations that
/// employ the legacy program names.
#[must_use]
pub const fn upstream_profile() -> BrandProfile {
    UPSTREAM_PROFILE
}

/// Returns the oc-branded profile used by the canonical binaries.
///
/// # Examples
///
/// ```
/// use rsync_core::branding;
///
/// let profile = branding::oc_profile();
/// assert_eq!(profile.client_program_name(), "oc-rsync");
/// assert_eq!(profile.daemon_program_name(), "oc-rsyncd");
/// assert_eq!(profile.daemon_config_dir(), branding::oc_daemon_config_dir());
/// assert_eq!(profile.daemon_config_path(), branding::oc_daemon_config_path());
/// assert_eq!(profile.daemon_secrets_path(), branding::oc_daemon_secrets_path());
/// ```
#[must_use]
pub const fn oc_profile() -> BrandProfile {
    OC_PROFILE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::ffi::{OsStr, OsString};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn acquire_env_lock() -> MutexGuard<'static, ()> {
        env_lock()
            .lock()
            .expect("environment lock poisoned during test")
    }

    #[allow(unsafe_code)]
    fn set_env_var(key: &'static str, value: impl AsRef<OsStr>) {
        unsafe {
            env::set_var(key, value);
        }
    }

    #[allow(unsafe_code)]
    fn remove_env_var(key: &'static str) {
        unsafe {
            env::remove_var(key);
        }
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set<V>(key: &'static str, value: V) -> Self
        where
            V: AsRef<OsStr>,
        {
            let lock = acquire_env_lock();
            let previous = env::var_os(key);
            set_env_var(key, value);
            Self {
                key,
                previous,
                _lock: lock,
            }
        }

        fn remove(key: &'static str) -> Self {
            let lock = acquire_env_lock();
            let previous = env::var_os(key);
            remove_env_var(key);
            Self {
                key,
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => set_env_var(self.key, value),
                None => remove_env_var(self.key),
            }
        }
    }

    #[test]
    fn program_names_are_consistent() {
        assert_eq!(client_program_name(), upstream_client_program_name());
        assert_eq!(daemon_program_name(), upstream_daemon_program_name());
        assert_eq!(oc_client_program_name(), OC_CLIENT_PROGRAM_NAME);
        assert_eq!(oc_daemon_program_name(), OC_DAEMON_PROGRAM_NAME);
    }

    #[test]
    fn oc_paths_match_expected_locations() {
        assert_eq!(oc_daemon_config_dir(), Path::new(OC_DAEMON_CONFIG_DIR));
        assert_eq!(oc_daemon_config_path(), Path::new(OC_DAEMON_CONFIG_PATH));
        assert_eq!(oc_daemon_secrets_path(), Path::new(OC_DAEMON_SECRETS_PATH));
        assert_eq!(
            legacy_daemon_config_path(),
            Path::new(LEGACY_DAEMON_CONFIG_PATH)
        );
        assert_eq!(
            legacy_daemon_config_dir(),
            Path::new(LEGACY_DAEMON_CONFIG_DIR)
        );
        assert_eq!(
            legacy_daemon_secrets_path(),
            Path::new(LEGACY_DAEMON_SECRETS_PATH)
        );
    }

    #[test]
    fn profile_helpers_align_with_functions() {
        assert_eq!(
            upstream_profile().client_program_name(),
            upstream_client_program_name()
        );
        assert_eq!(
            upstream_profile().daemon_program_name(),
            upstream_daemon_program_name()
        );
        assert_eq!(oc_profile().client_program_name(), oc_client_program_name());
        assert_eq!(oc_profile().daemon_program_name(), oc_daemon_program_name());
        assert_eq!(oc_profile().daemon_config_dir(), oc_daemon_config_dir());
        assert_eq!(oc_profile().daemon_config_path(), oc_daemon_config_path());
        assert_eq!(oc_profile().daemon_secrets_path(), oc_daemon_secrets_path());
        assert_eq!(
            upstream_profile().daemon_config_path(),
            legacy_daemon_config_path()
        );
        assert_eq!(
            upstream_profile().daemon_config_dir(),
            legacy_daemon_config_dir()
        );
        assert_eq!(
            upstream_profile().daemon_secrets_path(),
            legacy_daemon_secrets_path()
        );
    }

    #[test]
    fn brand_detection_matches_program_names() {
        assert_eq!(brand_for_program_name("rsync"), Brand::Upstream);
        assert_eq!(brand_for_program_name("rsyncd"), Brand::Upstream);
        assert_eq!(brand_for_program_name("oc-rsync"), Brand::Oc);
        assert_eq!(brand_for_program_name("oc-rsyncd"), Brand::Oc);
        assert_eq!(brand_for_program_name("oc-rsync-3.4.1"), Brand::Oc);
        assert_eq!(brand_for_program_name("OC-RSYNCD_v2"), Brand::Oc);
        assert_eq!(brand_for_program_name("rsync-3.4.1"), Brand::Upstream);
    }

    #[test]
    fn brand_profiles_expose_program_names() {
        let upstream = Brand::Upstream.profile();
        assert_eq!(upstream.client_program_name(), UPSTREAM_CLIENT_PROGRAM_NAME);
        assert_eq!(upstream.daemon_program_name(), UPSTREAM_DAEMON_PROGRAM_NAME);
        assert_eq!(
            upstream.daemon_config_dir(),
            Path::new(LEGACY_DAEMON_CONFIG_DIR)
        );

        let oc = Brand::Oc.profile();
        assert_eq!(oc.client_program_name(), OC_CLIENT_PROGRAM_NAME);
        assert_eq!(oc.daemon_program_name(), OC_DAEMON_PROGRAM_NAME);
        assert_eq!(oc.daemon_config_dir(), Path::new(OC_DAEMON_CONFIG_DIR));
    }

    #[test]
    fn detect_brand_matches_invocation_argument() {
        let _guard = EnvGuard::remove(BRAND_OVERRIDE_ENV);
        assert_eq!(detect_brand(None), Brand::Upstream);
        assert_eq!(detect_brand(Some(OsStr::new("rsync"))), Brand::Upstream);
        assert_eq!(
            detect_brand(Some(OsStr::new("/usr/bin/oc-rsync"))),
            Brand::Oc
        );
        assert_eq!(detect_brand(Some(OsStr::new("oc-rsyncd"))), Brand::Oc);
        assert_eq!(detect_brand(Some(OsStr::new("OC-RSYNCD"))), Brand::Oc);
        assert_eq!(
            detect_brand(Some(OsStr::new("/usr/bin/oc-rsync-3.4.1"))),
            Brand::Oc
        );
        assert_eq!(
            detect_brand(Some(OsStr::new("/usr/local/bin/rsync-3.4.1"))),
            Brand::Upstream
        );
    }

    #[test]
    fn config_search_orders_match_brand_expectations() {
        assert_eq!(
            Brand::Oc.config_path_candidate_strs(),
            [OC_DAEMON_CONFIG_PATH, LEGACY_DAEMON_CONFIG_PATH]
        );
        assert_eq!(
            Brand::Upstream.config_path_candidate_strs(),
            [LEGACY_DAEMON_CONFIG_PATH, OC_DAEMON_CONFIG_PATH]
        );

        let oc_paths = Brand::Oc.config_path_candidates();
        assert_eq!(oc_paths[0], Path::new(OC_DAEMON_CONFIG_PATH));
        assert_eq!(oc_paths[1], Path::new(LEGACY_DAEMON_CONFIG_PATH));
        let upstream_paths = Brand::Upstream.config_path_candidates();
        assert_eq!(upstream_paths[0], Path::new(LEGACY_DAEMON_CONFIG_PATH));
        assert_eq!(upstream_paths[1], Path::new(OC_DAEMON_CONFIG_PATH));
    }

    #[test]
    fn config_directories_match_brand_profiles() {
        assert_eq!(
            Brand::Oc.daemon_config_dir(),
            Path::new(OC_DAEMON_CONFIG_DIR)
        );
        assert_eq!(Brand::Oc.daemon_config_dir_str(), OC_DAEMON_CONFIG_DIR);
        assert_eq!(
            Brand::Upstream.daemon_config_dir(),
            Path::new(LEGACY_DAEMON_CONFIG_DIR)
        );
        assert_eq!(
            Brand::Upstream.daemon_config_dir_str(),
            LEGACY_DAEMON_CONFIG_DIR
        );
    }

    #[test]
    fn config_paths_match_brand_profiles() {
        assert_eq!(
            Brand::Oc.daemon_config_path(),
            Path::new(OC_DAEMON_CONFIG_PATH)
        );
        assert_eq!(Brand::Oc.daemon_config_path_str(), OC_DAEMON_CONFIG_PATH);
        assert_eq!(
            Brand::Upstream.daemon_config_path(),
            Path::new(LEGACY_DAEMON_CONFIG_PATH)
        );
        assert_eq!(
            Brand::Upstream.daemon_config_path_str(),
            LEGACY_DAEMON_CONFIG_PATH
        );
    }

    #[test]
    fn secrets_search_orders_match_brand_expectations() {
        assert_eq!(
            Brand::Oc.secrets_path_candidate_strs(),
            [OC_DAEMON_SECRETS_PATH, LEGACY_DAEMON_SECRETS_PATH]
        );
        assert_eq!(
            Brand::Upstream.secrets_path_candidate_strs(),
            [LEGACY_DAEMON_SECRETS_PATH, OC_DAEMON_SECRETS_PATH]
        );

        let oc_paths = Brand::Oc.secrets_path_candidates();
        assert_eq!(oc_paths[0], Path::new(OC_DAEMON_SECRETS_PATH));
        assert_eq!(oc_paths[1], Path::new(LEGACY_DAEMON_SECRETS_PATH));
        let upstream_paths = Brand::Upstream.secrets_path_candidates();
        assert_eq!(upstream_paths[0], Path::new(LEGACY_DAEMON_SECRETS_PATH));
        assert_eq!(upstream_paths[1], Path::new(OC_DAEMON_SECRETS_PATH));

        assert_eq!(
            Brand::Oc.daemon_secrets_path(),
            Path::new(OC_DAEMON_SECRETS_PATH)
        );
        assert_eq!(Brand::Oc.daemon_secrets_path_str(), OC_DAEMON_SECRETS_PATH);
        assert_eq!(
            Brand::Upstream.daemon_secrets_path(),
            Path::new(LEGACY_DAEMON_SECRETS_PATH)
        );
        assert_eq!(
            Brand::Upstream.daemon_secrets_path_str(),
            LEGACY_DAEMON_SECRETS_PATH
        );
    }

    #[test]
    fn detect_brand_respects_oc_override_environment_variable() {
        let _guard = EnvGuard::set(BRAND_OVERRIDE_ENV, OsStr::new("oc"));
        assert_eq!(detect_brand(Some(OsStr::new("rsync"))), Brand::Oc);
        assert_eq!(detect_brand(None), Brand::Oc);
    }

    #[test]
    fn detect_brand_respects_upstream_override_environment_variable() {
        let _guard = EnvGuard::set(BRAND_OVERRIDE_ENV, OsStr::new("upstream"));
        assert_eq!(detect_brand(Some(OsStr::new("oc-rsync"))), Brand::Upstream);
        assert_eq!(detect_brand(None), Brand::Upstream);
    }

    #[test]
    fn detect_brand_ignores_invalid_override_environment_variable() {
        let _guard = EnvGuard::set(BRAND_OVERRIDE_ENV, OsStr::new("invalid"));
        assert_eq!(detect_brand(Some(OsStr::new("oc-rsync"))), Brand::Oc);
    }

    #[test]
    fn brand_label_matches_expected() {
        assert_eq!(Brand::Oc.label(), "oc");
        assert_eq!(Brand::Upstream.label(), "upstream");
    }

    #[test]
    fn brand_display_renders_label() {
        assert_eq!(Brand::Oc.to_string(), "oc");
        assert_eq!(Brand::Upstream.to_string(), "upstream");
    }
}
