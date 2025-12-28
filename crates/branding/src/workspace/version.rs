//! Version parsing and validation.
//!
//! This module provides centralized version handling for the workspace.
//! Versions follow the format `x.y.z[-rust]` where each component is a
//! number that may have leading zeros.

use std::fmt;

use thiserror::Error;

/// Semantic version number with optional Rust branding suffix.
///
/// Versions follow the format `x.y.z[-rust]` where:
/// - Each component (x, y, z) is a non-negative integer
/// - Components may have leading zeros (e.g., "03.04.01" is valid)
/// - The `-rust` suffix is optional for Rust-branded builds
///
/// # Examples
///
/// ```
/// use branding::workspace::Version;
///
/// let v1: Version = "3.4.1".parse().unwrap();
/// assert_eq!(v1.major(), 3);
/// assert_eq!(v1.minor(), 4);
/// assert_eq!(v1.patch(), 1);
/// assert!(!v1.is_rust_branded());
///
/// let v2: Version = "3.4.1-rust".parse().unwrap();
/// assert_eq!(v2.major(), 3);
/// assert!(v2.is_rust_branded());
///
/// // Leading zeros are accepted
/// let v3: Version = "03.04.01".parse().unwrap();
/// assert_eq!(v3.major(), 3);
/// assert_eq!(v3.minor(), 4);
/// assert_eq!(v3.patch(), 1);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
    rust_branded: bool,
}

impl Version {
    /// Creates a new version from individual components.
    ///
    /// # Examples
    ///
    /// ```
    /// use branding::workspace::Version;
    ///
    /// let upstream = Version::new(3, 4, 1, false);
    /// assert_eq!(upstream.to_string(), "3.4.1");
    ///
    /// let rust = Version::new(3, 4, 1, true);
    /// assert_eq!(rust.to_string(), "3.4.1-rust");
    /// ```
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32, rust_branded: bool) -> Self {
        Self {
            major,
            minor,
            patch,
            rust_branded,
        }
    }

    /// Returns the major version component.
    #[must_use]
    pub const fn major(self) -> u32 {
        self.major
    }

    /// Returns the minor version component.
    #[must_use]
    pub const fn minor(self) -> u32 {
        self.minor
    }

    /// Returns the patch version component.
    #[must_use]
    pub const fn patch(self) -> u32 {
        self.patch
    }

    /// Returns `true` if this version carries the Rust branding suffix.
    #[must_use]
    pub const fn is_rust_branded(self) -> bool {
        self.rust_branded
    }

    /// Returns the base version tuple without the Rust branding suffix.
    ///
    /// # Examples
    ///
    /// ```
    /// use branding::workspace::Version;
    ///
    /// let v: Version = "3.4.1-rust".parse().unwrap();
    /// assert_eq!(v.base_triple(), (3, 4, 1));
    /// ```
    #[must_use]
    pub const fn base_triple(self) -> (u32, u32, u32) {
        (self.major, self.minor, self.patch)
    }

    /// Converts this version to its Rust-branded equivalent.
    ///
    /// If already branded, returns self unchanged.
    ///
    /// # Examples
    ///
    /// ```
    /// use branding::workspace::Version;
    ///
    /// let upstream: Version = "3.4.1".parse().unwrap();
    /// let rust = upstream.with_rust_branding();
    /// assert_eq!(rust.to_string(), "3.4.1-rust");
    /// ```
    #[must_use]
    pub const fn with_rust_branding(mut self) -> Self {
        self.rust_branded = true;
        self
    }

    /// Removes the Rust branding suffix if present.
    ///
    /// # Examples
    ///
    /// ```
    /// use branding::workspace::Version;
    ///
    /// let rust: Version = "3.4.1-rust".parse().unwrap();
    /// let base = rust.without_rust_branding();
    /// assert_eq!(base.to_string(), "3.4.1");
    /// ```
    #[must_use]
    pub const fn without_rust_branding(mut self) -> Self {
        self.rust_branded = false;
        self
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if self.rust_branded {
            f.write_str("-rust")?;
        }
        Ok(())
    }
}

/// Error returned when version parsing fails.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("invalid version '{input}': {reason}")]
pub struct VersionParseError {
    input: String,
    reason: &'static str,
}

impl VersionParseError {
    fn new(input: &str, reason: &'static str) -> Self {
        Self {
            input: input.to_owned(),
            reason,
        }
    }

    /// Returns the input string that failed to parse.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Returns a human-readable description of the parse failure.
    #[must_use]
    pub const fn reason(&self) -> &str {
        self.reason
    }
}

impl std::str::FromStr for Version {
    type Err = VersionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(VersionParseError::new(s, "empty version string"));
        }

        // Split on optional -rust suffix
        let (base, rust_branded) = match trimmed.rsplit_once("-rust") {
            Some((base, "")) => (base, true),
            Some(_) => {
                return Err(VersionParseError::new(
                    s,
                    "-rust suffix must appear at the end",
                ));
            }
            None => (trimmed, false),
        };

        // Parse x.y.z components
        let parts: Vec<&str> = base.split('.').collect();
        if parts.len() != 3 {
            return Err(VersionParseError::new(
                s,
                "version must have exactly three components (x.y.z)",
            ));
        }

        let major = parse_component(parts[0], s, "major")?;
        let minor = parse_component(parts[1], s, "minor")?;
        let patch = parse_component(parts[2], s, "patch")?;

        Ok(Self {
            major,
            minor,
            patch,
            rust_branded,
        })
    }
}

fn parse_component(
    s: &str,
    original: &str,
    component_name: &'static str,
) -> Result<u32, VersionParseError> {
    if s.is_empty() {
        return Err(VersionParseError::new(original, component_name));
    }

    // Allow leading zeros but reject non-numeric characters
    s.parse::<u32>().map_err(|_| {
        VersionParseError::new(original, "version components must be non-negative integers")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_version() {
        let v: Version = "3.4.1".parse().unwrap();
        assert_eq!(v.major(), 3);
        assert_eq!(v.minor(), 4);
        assert_eq!(v.patch(), 1);
        assert!(!v.is_rust_branded());
        assert_eq!(v.to_string(), "3.4.1");
    }

    #[test]
    fn parse_rust_branded_version() {
        let v: Version = "3.4.1-rust".parse().unwrap();
        assert_eq!(v.major(), 3);
        assert_eq!(v.minor(), 4);
        assert_eq!(v.patch(), 1);
        assert!(v.is_rust_branded());
        assert_eq!(v.to_string(), "3.4.1-rust");
    }

    #[test]
    fn parse_version_with_leading_zeros() {
        let v: Version = "03.04.01".parse().unwrap();
        assert_eq!(v.major(), 3);
        assert_eq!(v.minor(), 4);
        assert_eq!(v.patch(), 1);
        assert_eq!(v.to_string(), "3.4.1");
    }

    #[test]
    fn parse_rust_branded_with_leading_zeros() {
        let v: Version = "03.04.01-rust".parse().unwrap();
        assert_eq!(v.major(), 3);
        assert_eq!(v.minor(), 4);
        assert_eq!(v.patch(), 1);
        assert!(v.is_rust_branded());
        assert_eq!(v.to_string(), "3.4.1-rust");
    }

    #[test]
    fn parse_zero_components() {
        let v: Version = "0.0.0".parse().unwrap();
        assert_eq!(v.base_triple(), (0, 0, 0));

        let v2: Version = "00.00.00-rust".parse().unwrap();
        assert_eq!(v2.base_triple(), (0, 0, 0));
        assert!(v2.is_rust_branded());
    }

    #[test]
    fn parse_large_numbers() {
        let v: Version = "999.888.777".parse().unwrap();
        assert_eq!(v.base_triple(), (999, 888, 777));
    }

    #[test]
    fn parse_rejects_empty_string() {
        assert!("".parse::<Version>().is_err());
        assert!("   ".parse::<Version>().is_err());
    }

    #[test]
    fn parse_rejects_incomplete_version() {
        assert!("3".parse::<Version>().is_err());
        assert!("3.4".parse::<Version>().is_err());
        assert!("3.4.".parse::<Version>().is_err());
    }

    #[test]
    fn parse_rejects_too_many_components() {
        assert!("3.4.1.0".parse::<Version>().is_err());
    }

    #[test]
    fn parse_rejects_non_numeric() {
        assert!("a.b.c".parse::<Version>().is_err());
        assert!("3.4.x".parse::<Version>().is_err());
        assert!("3.4.1-beta".parse::<Version>().is_err());
    }

    #[test]
    fn parse_rejects_negative_numbers() {
        assert!("-1.0.0".parse::<Version>().is_err());
        assert!("1.-2.0".parse::<Version>().is_err());
        assert!("1.0.-3".parse::<Version>().is_err());
    }

    #[test]
    fn parse_rejects_malformed_rust_suffix() {
        assert!("3.4.1-rustx".parse::<Version>().is_err());
        assert!("3.4.1-rust-extra".parse::<Version>().is_err());
    }

    #[test]
    fn with_rust_branding_adds_suffix() {
        let v: Version = "3.4.1".parse().unwrap();
        let branded = v.with_rust_branding();
        assert!(branded.is_rust_branded());
        assert_eq!(branded.to_string(), "3.4.1-rust");
    }

    #[test]
    fn with_rust_branding_idempotent() {
        let v: Version = "3.4.1-rust".parse().unwrap();
        let branded = v.with_rust_branding();
        assert_eq!(v, branded);
    }

    #[test]
    fn without_rust_branding_removes_suffix() {
        let v: Version = "3.4.1-rust".parse().unwrap();
        let base = v.without_rust_branding();
        assert!(!base.is_rust_branded());
        assert_eq!(base.to_string(), "3.4.1");
    }

    #[test]
    fn without_rust_branding_idempotent() {
        let v: Version = "3.4.1".parse().unwrap();
        let base = v.without_rust_branding();
        assert_eq!(v, base);
    }

    #[test]
    fn base_triple_excludes_rust_branding() {
        let v1: Version = "3.4.1".parse().unwrap();
        let v2: Version = "3.4.1-rust".parse().unwrap();
        assert_eq!(v1.base_triple(), v2.base_triple());
    }

    #[test]
    fn new_constructor_works() {
        let v = Version::new(3, 4, 1, false);
        assert_eq!(v.to_string(), "3.4.1");

        let v2 = Version::new(3, 4, 1, true);
        assert_eq!(v2.to_string(), "3.4.1-rust");
    }

    #[test]
    fn error_display_includes_input_and_reason() {
        let err = "invalid".parse::<Version>().unwrap_err();
        let display = err.to_string();
        assert!(display.contains("invalid"));
        assert!(!display.is_empty());
    }
}
