//! Brand enumeration and parsing utilities.

use core::str::FromStr;
use std::fmt;
use std::path::Path;

use super::constants::{
    OC_CLIENT_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME, UPSTREAM_CLIENT_PROGRAM_NAME,
    UPSTREAM_DAEMON_PROGRAM_NAME,
};
use super::profile::{
    BrandProfile, config_path_candidate_strs, config_path_candidates, oc_profile,
    secrets_path_candidate_strs, secrets_path_candidates, upstream_profile,
};
use crate::workspace;
use serde::ser::{Serialize, Serializer};

/// Identifies the brand associated with an executable name.
///
/// The workspace recognises both upstream-compatible names (`rsync`/`rsyncd`),
/// typically provided via symlinks or remote invocations, and the branded
/// single binary (`oc-rsync`). Centralising the mapping keeps higher layers free
/// from string comparisons and ensures configuration paths, help banners, and
/// diagnostics stay consistent across entry points. The [`Brand::profile`]
/// method exposes the corresponding [`BrandProfile`], which in turn provides
/// program names and filesystem locations for the selected distribution.
///
/// `Brand` implements [`FromStr`], allowing environment variables such as
/// [`OC_RSYNC_BRAND`][super::brand_override_env_var] to accept human-readable aliases.
/// The parser tolerates ASCII case differences, leading/trailing whitespace, and
/// versioned program names.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Brand {
    /// Upstream-compatible binaries (`rsync` and `rsyncd`).
    Upstream,
    /// Branded binaries installed as the single `oc-rsync` entry point.
    Oc,
}

/// Error returned when parsing a [`Brand`] from an unrecognised string fails.
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

        if s.eq_ignore_ascii_case(Self::Oc.label()) {
            return Ok(Self::Oc);
        }

        if s.eq_ignore_ascii_case(Self::Upstream.label()) {
            return Ok(Self::Upstream);
        }

        if matches_any_program_alias(s, &[OC_CLIENT_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME]) {
            return Ok(Self::Oc);
        }

        const OC_LEGACY_DAEMON_ALIAS: &str = "oc-rsyncd";
        if matches_program_alias(s, OC_LEGACY_DAEMON_ALIAS) {
            return Ok(Self::Oc);
        }

        if matches_any_program_alias(
            s,
            &[UPSTREAM_CLIENT_PROGRAM_NAME, UPSTREAM_DAEMON_PROGRAM_NAME],
        ) {
            return Ok(Self::Upstream);
        }

        Err(BrandParseError)
    }
}

impl Brand {
    /// Returns the canonical, human-readable label associated with the brand.
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
            Self::Upstream => upstream_profile(),
            Self::Oc => oc_profile(),
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

    /// Returns the preferred daemon configuration directory as a [`Path`].
    #[must_use]
    pub fn daemon_config_dir(self) -> &'static Path {
        self.profile().daemon_config_dir()
    }

    /// Returns the preferred daemon configuration directory as a string slice.
    #[must_use]
    pub const fn daemon_config_dir_str(self) -> &'static str {
        self.profile().daemon_config_dir_str()
    }

    /// Returns the canonical daemon configuration path for this brand.
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
    #[must_use]
    pub const fn config_path_candidate_strs(self) -> [&'static str; 2] {
        config_path_candidate_strs(self)
    }

    /// Returns the preferred daemon configuration search order as [`Path`]s.
    #[must_use]
    pub fn config_path_candidates(self) -> [&'static Path; 2] {
        config_path_candidates(self)
    }

    /// Returns the preferred secrets-file search order for this brand.
    #[must_use]
    pub const fn secrets_path_candidate_strs(self) -> [&'static str; 2] {
        secrets_path_candidate_strs(self)
    }

    /// Returns the canonical daemon secrets path for this brand as a string slice.
    #[must_use]
    pub const fn daemon_secrets_path_str(self) -> &'static str {
        self.profile().daemon_secrets_path_str()
    }

    /// Returns the canonical daemon secrets path for this brand as a [`Path`].
    #[must_use]
    pub fn daemon_secrets_path(self) -> &'static Path {
        self.profile().daemon_secrets_path()
    }

    /// Returns the preferred secrets-file search order as [`Path`]s.
    #[must_use]
    pub fn secrets_path_candidates(self) -> [&'static Path; 2] {
        secrets_path_candidates(self)
    }
}

fn resolve_default_brand(label: &str) -> Brand {
    label.parse::<Brand>().unwrap_or_else(|_| {
        panic!("unsupported workspace brand '{label}'; expected 'oc' or 'upstream' aliases")
    })
}

/// Returns the brand configured for this workspace build.
#[must_use]
pub fn default_brand() -> Brand {
    resolve_default_brand(workspace::BRAND)
}

impl fmt::Display for Brand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl Serialize for Brand {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.label())
    }
}

pub(super) fn matches_program_alias(program: &str, canonical: &str) -> bool {
    if normalized_program_match(program, canonical) {
        return true;
    }

    let Some(suffix) = program_suffix(program, canonical) else {
        return false;
    };

    version_suffix_is_allowed(suffix)
}

fn normalized_program_match(candidate: &str, canonical: &str) -> bool {
    if candidate.len() != canonical.len() {
        return false;
    }

    candidate
        .bytes()
        .zip(canonical.bytes())
        .all(|(candidate_byte, canonical_byte)| {
            program_alias_byte_eq(candidate_byte, canonical_byte)
        })
}

fn program_alias_byte_eq(candidate: u8, canonical: u8) -> bool {
    let candidate_lower = candidate.to_ascii_lowercase();
    let canonical_lower = canonical.to_ascii_lowercase();

    if candidate_lower == canonical_lower {
        return true;
    }

    canonical_lower == b'-' && matches!(candidate_lower, b'-' | b'_' | b'.')
}

fn program_suffix<'a>(program: &'a str, canonical: &str) -> Option<&'a str> {
    if program.len() <= canonical.len() || !program.is_char_boundary(canonical.len()) {
        return None;
    }

    let prefix = &program[..canonical.len()];
    if !normalized_program_match(prefix, canonical) {
        return None;
    }

    program.get(canonical.len()..)
}

fn version_suffix_is_allowed(suffix: &str) -> bool {
    if suffix.is_empty() {
        return true;
    }

    let bytes = suffix.as_bytes();
    let Some((&first, rest)) = bytes.split_first() else {
        return true;
    };

    if !matches!(first, b'-' | b'_' | b'.') {
        return false;
    }

    let mut has_digit = false;

    for &byte in rest {
        if !byte.is_ascii_alphanumeric() && byte != b'-' && byte != b'_' && byte != b'.' {
            return false;
        }

        if byte.is_ascii_digit() {
            has_digit = true;
        }
    }

    if has_digit {
        return true;
    }

    const WINDOWS_EXECUTABLE_EXTENSIONS: [&[u8]; 4] = [b".exe", b".com", b".bat", b".cmd"];
    if WINDOWS_EXECUTABLE_EXTENSIONS.iter().any(|ext| {
        bytes.len() >= ext.len() && bytes[bytes.len() - ext.len()..].eq_ignore_ascii_case(ext)
    }) {
        return true;
    }

    let mut start = 0;
    while let Some(&byte) = rest.get(start) {
        if matches!(byte, b'-' | b'_' | b'.') {
            start += 1;
        } else {
            break;
        }
    }

    let trimmed = &rest[start..];
    if trimmed.is_empty() {
        return false;
    }

    if trimmed.iter().all(u8::is_ascii_alphabetic) {
        const ALLOWED_ALPHA_SUFFIXES: [&[u8]; 4] = [b"debug", b"dbg", b"devel", b"dev"];
        if ALLOWED_ALPHA_SUFFIXES
            .iter()
            .any(|suffix| trimmed.eq_ignore_ascii_case(suffix))
        {
            return true;
        }
    }

    false
}

fn matches_any_program_alias(value: &str, programs: &[&str]) -> bool {
    programs
        .iter()
        .any(|canonical| matches_program_alias(value, canonical))
}
