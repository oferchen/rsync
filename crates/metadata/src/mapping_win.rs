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
}
