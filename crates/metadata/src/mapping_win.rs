//! User/group mapping stubs for Windows.
//!
//! Windows lacks POSIX user/group concepts, so ownership mapping options
//! (`--usermap`, `--groupmap`, `--chown`) are not supported. This module
//! provides placeholder types that return errors when parsing is attempted.
//!
//! # Platform Behavior
//!
//! This matches upstream rsync behavior where ownership-related options
//! are unavailable on platforms without POSIX user databases.

use std::fmt;

/// Kind of mapping requested (Windows stub).
///
/// On Windows, no mapping types are supported since the platform lacks
/// POSIX-style user/group ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingKind {
    /// No mapping supported on this platform.
    None,
}

impl fmt::Display for MappingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "none (unsupported on Windows)")
    }
}

/// Error returned when a mapping string cannot be parsed.
///
/// On Windows, this is always returned since user/group mapping
/// is not supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingParseError;

impl fmt::Display for MappingParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "user/group mapping (--usermap/--groupmap/--chown) is not supported on Windows"
        )
    }
}

impl std::error::Error for MappingParseError {}

/// User mapping placeholder for Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMapping;

impl UserMapping {
    /// Always returns an error on Windows.
    ///
    /// # Errors
    /// Always returns [`MappingParseError`].
    pub fn parse(_spec: &str) -> Result<Self, MappingParseError> {
        Err(MappingParseError)
    }

    /// Returns an empty string on Windows.
    ///
    /// The Unix variant returns the original `--usermap` specification so
    /// callers can forward it verbatim to a remote server. On Windows the
    /// type cannot be constructed, so this method is provided only to keep
    /// the cross-platform API surface uniform.
    #[must_use]
    pub fn spec(&self) -> &str {
        ""
    }

    /// Reports whether the mapping contains any rules.
    ///
    /// Always `true` on Windows: the type cannot be constructed, so this is
    /// only provided to keep the cross-platform API surface uniform.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        true
    }

    /// Never maps on Windows (no POSIX user database).
    ///
    /// Provided so the receiver's id-list remap compiles cross-platform; the
    /// value can never be constructed, so this is never actually called.
    ///
    /// # Errors
    /// Never returns an error.
    pub fn map_uid_named(
        &self,
        _uid: u32,
        _name: Option<&[u8]>,
        _numeric_ids: bool,
    ) -> std::io::Result<Option<u32>> {
        Ok(None)
    }
}

/// Group mapping placeholder for Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMapping;

impl GroupMapping {
    /// Always returns an error on Windows.
    ///
    /// # Errors
    /// Always returns [`MappingParseError`].
    pub fn parse(_spec: &str) -> Result<Self, MappingParseError> {
        Err(MappingParseError)
    }

    /// Returns an empty string on Windows.
    ///
    /// The Unix variant returns the original `--groupmap` specification so
    /// callers can forward it verbatim to a remote server. On Windows the
    /// type cannot be constructed, so this method is provided only to keep
    /// the cross-platform API surface uniform.
    #[must_use]
    pub fn spec(&self) -> &str {
        ""
    }

    /// Reports whether the mapping contains any rules.
    ///
    /// Always `true` on Windows: the type cannot be constructed, so this is
    /// only provided to keep the cross-platform API surface uniform.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        true
    }

    /// Never maps on Windows (no POSIX group database).
    ///
    /// Provided so the receiver's id-list remap compiles cross-platform; the
    /// value can never be constructed, so this is never actually called.
    ///
    /// # Errors
    /// Never returns an error.
    pub fn map_gid_named(
        &self,
        _gid: u32,
        _name: Option<&[u8]>,
        _numeric_ids: bool,
    ) -> std::io::Result<Option<u32>> {
        Ok(None)
    }
}

/// Name mapping placeholder for Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameMapping;

impl NameMapping {
    /// Always returns an error on Windows.
    ///
    /// # Errors
    /// Always returns [`MappingParseError`].
    pub fn parse(_spec: &str) -> Result<Self, MappingParseError> {
        Err(MappingParseError)
    }

    /// Returns an empty string on Windows.
    ///
    /// Provided so the cross-platform API surface stays uniform; the Unix
    /// variant exposes the original mapping specification string.
    #[must_use]
    pub fn spec(&self) -> &str {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_kind_none_display() {
        let kind = MappingKind::None;
        let display = format!("{kind}");
        assert!(display.contains("unsupported"));
        assert!(display.contains("Windows"));
    }

    #[test]
    fn mapping_parse_error_display() {
        let err = MappingParseError;
        let display = format!("{err}");
        assert!(display.contains("not supported on Windows"));
    }

    #[test]
    fn mapping_parse_error_is_std_error() {
        let err = MappingParseError;
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn user_mapping_parse_always_fails() {
        assert!(UserMapping::parse("root:0").is_err());
        assert!(UserMapping::parse("*:65534").is_err());
        assert!(UserMapping::parse("").is_err());
    }

    #[test]
    fn group_mapping_parse_always_fails() {
        assert!(GroupMapping::parse("wheel:0").is_err());
        assert!(GroupMapping::parse("*:65534").is_err());
        assert!(GroupMapping::parse("").is_err());
    }

    #[test]
    fn name_mapping_parse_always_fails() {
        assert!(NameMapping::parse("user:group").is_err());
        assert!(NameMapping::parse("root:root").is_err());
        assert!(NameMapping::parse("").is_err());
    }

    #[test]
    fn mapping_kind_debug() {
        let kind = MappingKind::None;
        let debug = format!("{kind:?}");
        assert!(debug.contains("None"));
    }

    #[test]
    fn mapping_parse_error_equality() {
        assert_eq!(MappingParseError, MappingParseError);
    }

    #[test]
    fn user_mapping_equality() {
        assert_eq!(UserMapping, UserMapping);
    }

    #[test]
    fn group_mapping_equality() {
        assert_eq!(GroupMapping, GroupMapping);
    }

    #[test]
    fn name_mapping_equality() {
        assert_eq!(NameMapping, NameMapping);
    }
}
