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

use std::path::Path;

/// Kind of mapping requested (Windows stub).
///
/// On Windows, no mapping types are supported since the platform lacks
/// POSIX-style user/group ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingKind {
    /// No mapping supported on this platform.
    None,
}

/// Error returned when a mapping string cannot be parsed.
///
/// On Windows, this is always returned since user/group mapping
/// is not supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingParseError;

/// User mapping placeholder for Windows.
///
/// This type exists for API compatibility but cannot be constructed
/// since user mapping is not supported on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMapping;

/// Group mapping placeholder for Windows.
///
/// This type exists for API compatibility but cannot be constructed
/// since group mapping is not supported on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMapping;

/// Name mapping placeholder for Windows.
///
/// This type exists for API compatibility but cannot be constructed
/// since name mapping is not supported on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameMapping;

/// Parses a user mapping string (unsupported on Windows).
///
/// Always returns [`MappingParseError`] since Windows lacks POSIX user databases.
#[allow(dead_code)]
pub fn parse_user_mapping(_s: &str) -> Result<UserMapping, MappingParseError> {
    Err(MappingParseError)
}

/// Parses a group mapping string (unsupported on Windows).
///
/// Always returns [`MappingParseError`] since Windows lacks POSIX group databases.
#[allow(dead_code)]
pub fn parse_group_mapping(_s: &str) -> Result<GroupMapping, MappingParseError> {
    Err(MappingParseError)
}

/// Parses a name mapping string (unsupported on Windows).
///
/// Always returns [`MappingParseError`] since Windows lacks POSIX ownership concepts.
#[allow(dead_code)]
pub fn parse_name_mapping(_s: &str) -> Result<NameMapping, MappingParseError> {
    Err(MappingParseError)
}

/// Reads an extended attribute as a string (unsupported on Windows).
///
/// Always returns `None` since Windows uses a different xattr model
/// that is not compatible with POSIX extended attributes.
#[allow(dead_code)]
pub fn read_xattr_as_string(_path: &Path, _name: &str) -> Option<String> {
    None
}
