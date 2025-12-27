//! Validation utilities for branding configuration.
//!
//! This module provides testable validation functions used by build.rs
//! and other parts of the branding infrastructure.

use std::path::Path;

/// Validation error types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Value is empty or whitespace-only.
    Empty,
    /// Value contains invalid characters.
    InvalidCharacters(&'static str),
    /// Value has invalid format.
    InvalidFormat(&'static str),
    /// Value is out of expected range.
    OutOfRange(&'static str),
    /// Path validation failed.
    InvalidPath(&'static str),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "value must not be empty"),
            Self::InvalidCharacters(msg) => write!(f, "invalid characters: {msg}"),
            Self::InvalidFormat(msg) => write!(f, "invalid format: {msg}"),
            Self::OutOfRange(msg) => write!(f, "out of range: {msg}"),
            Self::InvalidPath(msg) => write!(f, "invalid path: {msg}"),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validates that a string is non-empty after trimming.
pub fn validate_non_empty(value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        Err(ValidationError::Empty)
    } else {
        Ok(())
    }
}

/// Validates a binary name (no whitespace, no path separators).
pub fn validate_binary_name(value: &str) -> Result<(), ValidationError> {
    validate_non_empty(value)?;

    if value.chars().any(char::is_whitespace) {
        return Err(ValidationError::InvalidCharacters("contains whitespace"));
    }

    if value.chars().any(std::path::is_separator) {
        return Err(ValidationError::InvalidCharacters(
            "contains path separator",
        ));
    }

    Ok(())
}

/// Validates that a path is absolute (Unix or native).
pub fn validate_absolute_path(value: &str) -> Result<(), ValidationError> {
    let path = Path::new(value);
    let is_native_abs = path.is_absolute();
    let is_unix_abs = value.starts_with('/');

    if is_native_abs || is_unix_abs {
        Ok(())
    } else {
        Err(ValidationError::InvalidPath("must be absolute"))
    }
}

/// Validates that a path has a file name component.
pub fn validate_has_file_name(value: &str) -> Result<(), ValidationError> {
    let path = Path::new(value);
    if path.file_name().is_some() {
        Ok(())
    } else {
        Err(ValidationError::InvalidPath("must have file name"))
    }
}

/// Validates that child path is under parent directory.
pub fn validate_path_under_dir(child: &str, parent: &str) -> Result<(), ValidationError> {
    let child_path = Path::new(child);
    let parent_path = Path::new(parent);

    if !child_path.starts_with(parent_path) {
        return Err(ValidationError::InvalidPath(
            "must be under parent directory",
        ));
    }

    if child_path == parent_path {
        return Err(ValidationError::InvalidPath(
            "must not equal parent directory",
        ));
    }

    Ok(())
}

/// Parsed semantic version components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVersion {
    /// Major.minor.patch tuple.
    pub base: (u32, u32, u32),
    /// Whether the version has a -rust suffix.
    pub is_rust_branded: bool,
}

/// Validates and parses a semantic version string (x.y.z or x.y.z-rust).
pub fn validate_version(value: &str) -> Result<ParsedVersion, ValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ValidationError::Empty);
    }

    let (base, is_rust_branded) = match trimmed.rsplit_once("-rust") {
        Some((base, "")) => (base, true),
        Some(_) => return Err(ValidationError::InvalidFormat("-rust must be at end")),
        None => (trimmed, false),
    };

    let parts: Vec<&str> = base.split('.').collect();
    if parts.len() != 3 {
        return Err(ValidationError::InvalidFormat("expected x.y.z format"));
    }

    let major = parts[0]
        .parse::<u32>()
        .map_err(|_| ValidationError::InvalidFormat("invalid major version"))?;
    let minor = parts[1]
        .parse::<u32>()
        .map_err(|_| ValidationError::InvalidFormat("invalid minor version"))?;
    let patch = parts[2]
        .parse::<u32>()
        .map_err(|_| ValidationError::InvalidFormat("invalid patch version"))?;

    Ok(ParsedVersion {
        base: (major, minor, patch),
        is_rust_branded,
    })
}

/// Validates protocol version is in supported range.
pub fn validate_protocol_version(version: u32) -> Result<(), ValidationError> {
    if (28..=40).contains(&version) {
        Ok(())
    } else {
        Err(ValidationError::OutOfRange("protocol must be 28-40"))
    }
}

/// Validates brand name is recognized.
pub fn validate_brand_name(value: &str) -> Result<(), ValidationError> {
    match value {
        "oc" | "upstream" => Ok(()),
        _ => Err(ValidationError::InvalidFormat("must be 'oc' or 'upstream'")),
    }
}

/// Sanitizes a build revision string for embedding.
#[must_use]
pub fn sanitize_revision(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "unknown".to_owned();
    }

    let head = trimmed.split(['\r', '\n']).next().unwrap_or("");
    let cleaned = head.trim();
    if cleaned.is_empty() || cleaned.chars().any(char::is_control) {
        "unknown".to_owned()
    } else {
        cleaned.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Non-empty validation
    #[test]
    fn validate_non_empty_accepts_content() {
        assert!(validate_non_empty("hello").is_ok());
    }

    #[test]
    fn validate_non_empty_rejects_empty() {
        assert_eq!(validate_non_empty(""), Err(ValidationError::Empty));
    }

    #[test]
    fn validate_non_empty_rejects_whitespace() {
        assert_eq!(validate_non_empty("   "), Err(ValidationError::Empty));
    }

    // Binary name validation
    #[test]
    fn validate_binary_name_accepts_simple() {
        assert!(validate_binary_name("oc-rsync").is_ok());
    }

    #[test]
    fn validate_binary_name_rejects_empty() {
        assert_eq!(validate_binary_name(""), Err(ValidationError::Empty));
    }

    #[test]
    fn validate_binary_name_rejects_whitespace() {
        assert!(matches!(
            validate_binary_name("oc rsync"),
            Err(ValidationError::InvalidCharacters(_))
        ));
    }

    #[test]
    fn validate_binary_name_rejects_path_separator() {
        assert!(matches!(
            validate_binary_name("bin/rsync"),
            Err(ValidationError::InvalidCharacters(_))
        ));
    }

    // Absolute path validation
    #[test]
    fn validate_absolute_path_accepts_unix() {
        assert!(validate_absolute_path("/etc/rsyncd.conf").is_ok());
    }

    #[test]
    fn validate_absolute_path_rejects_relative() {
        assert!(matches!(
            validate_absolute_path("etc/rsyncd.conf"),
            Err(ValidationError::InvalidPath(_))
        ));
    }

    // File name validation
    #[test]
    fn validate_has_file_name_accepts_file() {
        assert!(validate_has_file_name("/etc/rsyncd.conf").is_ok());
    }

    #[test]
    fn validate_has_file_name_rejects_dir_only() {
        assert!(matches!(
            validate_has_file_name("/"),
            Err(ValidationError::InvalidPath(_))
        ));
    }

    // Path under dir validation
    #[test]
    fn validate_path_under_dir_accepts_child() {
        assert!(validate_path_under_dir("/etc/oc-rsync/config", "/etc/oc-rsync").is_ok());
    }

    #[test]
    fn validate_path_under_dir_rejects_outside() {
        assert!(matches!(
            validate_path_under_dir("/var/config", "/etc"),
            Err(ValidationError::InvalidPath(_))
        ));
    }

    #[test]
    fn validate_path_under_dir_rejects_equal() {
        assert!(matches!(
            validate_path_under_dir("/etc", "/etc"),
            Err(ValidationError::InvalidPath(_))
        ));
    }

    // Version validation
    #[test]
    fn validate_version_parses_simple() {
        let v = validate_version("3.4.1").unwrap();
        assert_eq!(v.base, (3, 4, 1));
        assert!(!v.is_rust_branded);
    }

    #[test]
    fn validate_version_parses_rust_branded() {
        let v = validate_version("3.4.1-rust").unwrap();
        assert_eq!(v.base, (3, 4, 1));
        assert!(v.is_rust_branded);
    }

    #[test]
    fn validate_version_rejects_empty() {
        assert_eq!(validate_version(""), Err(ValidationError::Empty));
    }

    #[test]
    fn validate_version_rejects_invalid_format() {
        assert!(matches!(
            validate_version("3.4"),
            Err(ValidationError::InvalidFormat(_))
        ));
    }

    #[test]
    fn validate_version_rejects_non_numeric() {
        assert!(matches!(
            validate_version("3.x.1"),
            Err(ValidationError::InvalidFormat(_))
        ));
    }

    #[test]
    fn validate_version_rejects_rust_not_at_end() {
        assert!(matches!(
            validate_version("3.4.1-rust-extra"),
            Err(ValidationError::InvalidFormat(_))
        ));
    }

    // Protocol validation
    #[test]
    fn validate_protocol_version_accepts_valid() {
        assert!(validate_protocol_version(28).is_ok());
        assert!(validate_protocol_version(32).is_ok());
        assert!(validate_protocol_version(40).is_ok());
    }

    #[test]
    fn validate_protocol_version_rejects_too_low() {
        assert!(matches!(
            validate_protocol_version(27),
            Err(ValidationError::OutOfRange(_))
        ));
    }

    #[test]
    fn validate_protocol_version_rejects_too_high() {
        assert!(matches!(
            validate_protocol_version(41),
            Err(ValidationError::OutOfRange(_))
        ));
    }

    // Brand validation
    #[test]
    fn validate_brand_name_accepts_oc() {
        assert!(validate_brand_name("oc").is_ok());
    }

    #[test]
    fn validate_brand_name_accepts_upstream() {
        assert!(validate_brand_name("upstream").is_ok());
    }

    #[test]
    fn validate_brand_name_rejects_unknown() {
        assert!(matches!(
            validate_brand_name("other"),
            Err(ValidationError::InvalidFormat(_))
        ));
    }

    // Revision sanitization
    #[test]
    fn sanitize_revision_trims_whitespace() {
        assert_eq!(sanitize_revision("  abc123  "), "abc123");
    }

    #[test]
    fn sanitize_revision_returns_unknown_for_empty() {
        assert_eq!(sanitize_revision(""), "unknown");
        assert_eq!(sanitize_revision("   "), "unknown");
    }

    #[test]
    fn sanitize_revision_takes_first_line() {
        assert_eq!(sanitize_revision("abc\ndef"), "abc");
    }

    #[test]
    fn sanitize_revision_rejects_control_chars() {
        assert_eq!(sanitize_revision("abc\x00def"), "unknown");
    }
}
