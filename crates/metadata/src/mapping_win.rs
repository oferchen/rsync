// crates/metadata/src/mapping_win.rs

use std::path::Path;

/// Kind of mapping requested (stubbed on Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingKind {
    /// No mapping supported on this platform.
    None,
}

/// Error returned when a mapping string cannot be parsed (always on Windows).
#[derive(Debug)]
pub struct MappingParseError;

/// User mapping placeholder for Windows.
#[derive(Debug, Clone)]
pub struct UserMapping;

/// Group mapping placeholder for Windows.
#[derive(Debug, Clone)]
pub struct GroupMapping;

/// Parse a user mapping string — unsupported on Windows.
pub fn parse_user_mapping(_s: &str) -> Result<UserMapping, MappingParseError> {
    Err(MappingParseError)
}

/// Parse a group mapping string — unsupported on Windows.
pub fn parse_group_mapping(_s: &str) -> Result<GroupMapping, MappingParseError> {
    Err(MappingParseError)
}

/// On Windows we don’t read xattrs here, so always return `None`.
pub fn read_xattr_as_string(_path: &Path, _name: &str) -> Option<String> {
    None
}
