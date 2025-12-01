//! Remote operand parsing for SSH transport.
//!
//! This module provides utilities to parse remote operand strings (e.g.,
//! `user@host:path`, `[::1]:path`) into structured components for SSH invocation.

use std::ffi::OsStr;
use std::fmt;

/// Parsed remote operand with extracted components.
///
/// # Examples
///
/// ```
/// use std::ffi::OsStr;
/// use transport::ssh::parse_ssh_operand;
///
/// let operand = OsStr::new("user@example.com:/path/to/file");
/// let parsed = parse_ssh_operand(operand).unwrap();
/// assert_eq!(parsed.user(), Some("user"));
/// assert_eq!(parsed.host(), "example.com");
/// assert_eq!(parsed.path(), "/path/to/file");
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteOperand {
    user: Option<String>,
    host: String,
    port: Option<u16>,
    path: String,
}

impl RemoteOperand {
    /// Creates a new remote operand with the specified components.
    #[must_use]
    pub fn new(user: Option<String>, host: String, port: Option<u16>, path: String) -> Self {
        Self {
            user,
            host,
            port,
            path,
        }
    }

    /// Returns the username component, if present.
    #[must_use]
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// Returns the hostname or IP address.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the port number, if specified.
    #[must_use]
    pub const fn port(&self) -> Option<u16> {
        self.port
    }

    /// Returns the remote path component.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// Errors that can occur when parsing remote operands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteOperandParseError {
    /// The operand string was empty.
    Empty,
    /// The operand format was invalid or could not be parsed.
    InvalidFormat,
    /// The port number was invalid or out of range.
    InvalidPort,
}

impl fmt::Display for RemoteOperandParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "remote operand is empty"),
            Self::InvalidFormat => write!(f, "invalid remote operand format"),
            Self::InvalidPort => write!(f, "invalid port number in remote operand"),
        }
    }
}

impl std::error::Error for RemoteOperandParseError {}

/// Parses an SSH-style remote operand into structured components.
///
/// This function handles various remote operand formats:
/// - `host:path` - Simple host and path
/// - `user@host:path` - With username
/// - `[::1]:path` - IPv6 literal
/// - `user@[2001:db8::1]:path` - IPv6 with username
///
/// # Errors
///
/// Returns [`RemoteOperandParseError`] if:
/// - The operand is empty
/// - The format is invalid (no colon separator)
/// - The port number is malformed
///
/// # Examples
///
/// ```
/// use std::ffi::OsStr;
/// use transport::ssh::parse_ssh_operand;
///
/// let operand = OsStr::new("example.com:/remote/path");
/// let parsed = parse_ssh_operand(operand).unwrap();
/// assert_eq!(parsed.host(), "example.com");
/// assert_eq!(parsed.path(), "/remote/path");
/// ```
pub fn parse_ssh_operand(operand: &OsStr) -> Result<RemoteOperand, RemoteOperandParseError> {
    if operand.is_empty() {
        return Err(RemoteOperandParseError::Empty);
    }

    let text = operand.to_string_lossy();
    if text.is_empty() {
        return Err(RemoteOperandParseError::Empty);
    }

    // Parse the operand: [user@][host]:path
    // IPv6 hosts may be in brackets: [::1]:path or user@[::1]:path

    let (user, rest) = extract_user(&text);
    let (host, path) = extract_host_and_path(rest)?;

    if host.is_empty() {
        return Err(RemoteOperandParseError::InvalidFormat);
    }

    if path.is_empty() {
        return Err(RemoteOperandParseError::InvalidFormat);
    }

    Ok(RemoteOperand {
        user: user.map(String::from),
        host: host.to_string(),
        port: None, // Port is extracted from -e option, not the operand
        path: path.to_string(),
    })
}

/// Extracts the optional user prefix from a remote operand.
///
/// Returns (user, rest) where user is Some if found, and rest is the remainder.
fn extract_user(text: &str) -> (Option<&str>, &str) {
    // Find the last '@' before any '[' (to handle user@[::1]:path correctly)
    if let Some(bracket_pos) = text.find('[') {
        // If there's a bracket, only look for @ before it
        let before_bracket = &text[..bracket_pos];
        if let Some(at_pos) = before_bracket.rfind('@') {
            return (Some(&text[..at_pos]), &text[at_pos + 1..]);
        }
    } else {
        // No bracket, look for @ anywhere before the first ':'
        if let Some(colon_pos) = text.find(':') {
            let before_colon = &text[..colon_pos];
            if let Some(at_pos) = before_colon.rfind('@') {
                return (Some(&text[..at_pos]), &text[at_pos + 1..]);
            }
        }
    }

    (None, text)
}

/// Extracts host and path from the operand after user has been removed.
///
/// Handles both bracketed IPv6 ([::1]:path) and regular (host:path) formats.
fn extract_host_and_path(text: &str) -> Result<(&str, &str), RemoteOperandParseError> {
    if text.starts_with('[') {
        // IPv6 literal: [host]:path
        if let Some(close_bracket) = text.find(']') {
            let host = &text[1..close_bracket]; // Extract without brackets
            let after_bracket = &text[close_bracket + 1..];

            if let Some(stripped) = after_bracket.strip_prefix(':') {
                return Ok((host, stripped));
            }
            return Err(RemoteOperandParseError::InvalidFormat);
        }
        return Err(RemoteOperandParseError::InvalidFormat);
    }

    // Regular format: host:path
    // Find the first ':' that's not part of an IPv6 address
    if let Some(colon_pos) = text.find(':') {
        let host = &text[..colon_pos];
        let path = &text[colon_pos + 1..];
        return Ok((host, path));
    }

    Err(RemoteOperandParseError::InvalidFormat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_host_path() {
        let operand = OsStr::new("example.com:/remote/path");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.user(), None);
        assert_eq!(result.host(), "example.com");
        assert_eq!(result.port(), None);
        assert_eq!(result.path(), "/remote/path");
    }

    #[test]
    fn parses_user_at_host_path() {
        let operand = OsStr::new("alice@example.com:/home/alice/file.txt");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.user(), Some("alice"));
        assert_eq!(result.host(), "example.com");
        assert_eq!(result.port(), None);
        assert_eq!(result.path(), "/home/alice/file.txt");
    }

    #[test]
    fn parses_ipv6_literal_with_brackets() {
        let operand = OsStr::new("[::1]:/path");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.user(), None);
        assert_eq!(result.host(), "::1");
        assert_eq!(result.port(), None);
        assert_eq!(result.path(), "/path");
    }

    #[test]
    fn parses_ipv6_with_user() {
        let operand = OsStr::new("bob@[2001:db8::1]:/remote/dir/");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.user(), Some("bob"));
        assert_eq!(result.host(), "2001:db8::1");
        assert_eq!(result.port(), None);
        assert_eq!(result.path(), "/remote/dir/");
    }

    #[test]
    fn parses_ipv6_full_address() {
        let operand = OsStr::new("[2001:0db8:85a3:0000:0000:8a2e:0370:7334]:/data");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.user(), None);
        assert_eq!(result.host(), "2001:0db8:85a3:0000:0000:8a2e:0370:7334");
        assert_eq!(result.path(), "/data");
    }

    #[test]
    fn parses_localhost_ipv6() {
        let operand = OsStr::new("user@[::ffff:127.0.0.1]:/tmp/file");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.user(), Some("user"));
        assert_eq!(result.host(), "::ffff:127.0.0.1");
        assert_eq!(result.path(), "/tmp/file");
    }

    #[test]
    fn parses_relative_path() {
        let operand = OsStr::new("host:relative/path");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.host(), "host");
        assert_eq!(result.path(), "relative/path");
    }

    #[test]
    fn parses_path_with_colon() {
        let operand = OsStr::new("host:/path/with:colon/in:name");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.host(), "host");
        assert_eq!(result.path(), "/path/with:colon/in:name");
    }

    #[test]
    fn parses_path_with_at_symbol() {
        let operand = OsStr::new("user@host:/path/with@symbol");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.user(), Some("user"));
        assert_eq!(result.host(), "host");
        assert_eq!(result.path(), "/path/with@symbol");
    }

    #[test]
    fn parses_hostname_with_dots() {
        let operand = OsStr::new("files.example.co.uk:/backup/data");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.host(), "files.example.co.uk");
        assert_eq!(result.path(), "/backup/data");
    }

    #[test]
    fn parses_hostname_with_dash() {
        let operand = OsStr::new("backup-server:/data");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.host(), "backup-server");
        assert_eq!(result.path(), "/data");
    }

    #[test]
    fn parses_numeric_ip() {
        let operand = OsStr::new("192.168.1.100:/files");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.host(), "192.168.1.100");
        assert_eq!(result.path(), "/files");
    }

    #[test]
    fn parses_user_with_numeric_ip() {
        let operand = OsStr::new("admin@10.0.0.1:/etc/config");
        let result = parse_ssh_operand(operand).unwrap();

        assert_eq!(result.user(), Some("admin"));
        assert_eq!(result.host(), "10.0.0.1");
        assert_eq!(result.path(), "/etc/config");
    }

    #[test]
    fn rejects_empty_operand() {
        let operand = OsStr::new("");
        let result = parse_ssh_operand(operand);

        assert_eq!(result, Err(RemoteOperandParseError::Empty));
    }

    #[test]
    fn rejects_no_colon() {
        let operand = OsStr::new("hostnamewithoutcolon");
        let result = parse_ssh_operand(operand);

        assert_eq!(result, Err(RemoteOperandParseError::InvalidFormat));
    }

    #[test]
    fn rejects_empty_host() {
        let operand = OsStr::new(":/path");
        let result = parse_ssh_operand(operand);

        assert_eq!(result, Err(RemoteOperandParseError::InvalidFormat));
    }

    #[test]
    fn rejects_empty_path() {
        let operand = OsStr::new("host:");
        let result = parse_ssh_operand(operand);

        assert_eq!(result, Err(RemoteOperandParseError::InvalidFormat));
    }

    #[test]
    fn rejects_unclosed_bracket() {
        let operand = OsStr::new("[::1:/path");
        let result = parse_ssh_operand(operand);

        assert_eq!(result, Err(RemoteOperandParseError::InvalidFormat));
    }

    #[test]
    fn rejects_bracket_without_colon() {
        let operand = OsStr::new("[::1]");
        let result = parse_ssh_operand(operand);

        assert_eq!(result, Err(RemoteOperandParseError::InvalidFormat));
    }

    #[test]
    fn rejects_user_only() {
        let operand = OsStr::new("user@");
        let result = parse_ssh_operand(operand);

        assert_eq!(result, Err(RemoteOperandParseError::InvalidFormat));
    }

    #[test]
    fn remote_operand_display() {
        let operand = RemoteOperand::new(
            Some("user".to_string()),
            "example.com".to_string(),
            Some(2222),
            "/path".to_string(),
        );

        assert_eq!(operand.user(), Some("user"));
        assert_eq!(operand.host(), "example.com");
        assert_eq!(operand.port(), Some(2222));
        assert_eq!(operand.path(), "/path");
    }

    #[test]
    fn error_display_messages() {
        assert_eq!(
            RemoteOperandParseError::Empty.to_string(),
            "remote operand is empty"
        );
        assert_eq!(
            RemoteOperandParseError::InvalidFormat.to_string(),
            "invalid remote operand format"
        );
        assert_eq!(
            RemoteOperandParseError::InvalidPort.to_string(),
            "invalid port number in remote operand"
        );
    }
}
