#![allow(clippy::module_name_repetitions)]

//! Helpers for parsing remote shell specifications supplied via `-e/--rsh`.
//!
//! Tokenization is delegated to the `shell-words` crate so that quoting
//! semantics match a standard POSIX shell. The wrapper here adds three
//! pieces of behaviour that callers of `RSYNC_RSH`-style strings rely on
//! and that `shell_words::split` does not provide directly:
//!
//! 1. Empty/whitespace-only specifications return [`RemoteShellParseError::Empty`]
//!    so that downstream callers (`SshCommand::configure_remote_shell`) can
//!    rely on the parsed argv being non-empty.
//! 2. Specifications containing an interior NUL byte are rejected with
//!    [`RemoteShellParseError::InteriorNull`], since NUL is meaningless in a
//!    `Command` argv and would otherwise be silently accepted.
//! 3. Non-UTF-8 input is rejected with
//!    [`RemoteShellParseError::InvalidEncoding`], since `shell_words::split`
//!    operates on `&str`.

use std::ffi::{OsStr, OsString};

use thiserror::Error;

/// Errors returned when parsing remote shell specifications fails.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum RemoteShellParseError {
    /// The specification was empty or consisted solely of whitespace.
    #[error("remote shell specification is empty")]
    Empty,
    /// The specification contained an interior NUL byte.
    #[error("remote shell specification contains a NUL byte")]
    InteriorNull,
    /// The specification was not valid UTF-8 and therefore cannot be tokenized.
    #[error("remote shell specification contains invalid Unicode")]
    InvalidEncoding,
    /// The specification could not be tokenized because of unbalanced quotes
    /// or a trailing escape. The contained message is the human-readable
    /// description produced by `shell_words::ParseError`.
    #[error("remote shell specification is malformed: {0}")]
    Parse(String),
}

/// Parses a remote shell specification using POSIX shell tokenization.
///
/// This is a thin wrapper around [`shell_words::split`] that adapts the
/// signature to `&OsStr` and applies the additional validation described in
/// this module's documentation. The returned `Vec<OsString>` always contains
/// at least one element on success, ensuring callers can safely treat
/// `parts[0]` as the program name.
///
/// # Errors
///
/// - [`RemoteShellParseError::Empty`] when the specification has no tokens
///   (e.g. `""` or `"   "`).
/// - [`RemoteShellParseError::InteriorNull`] when the input contains a `\0`
///   byte, which cannot be passed to `Command::arg` cleanly.
/// - [`RemoteShellParseError::InvalidEncoding`] when the input is not valid
///   UTF-8.
/// - [`RemoteShellParseError::Parse`] when `shell-words` rejects the input
///   because of unbalanced quotes or a dangling escape.
pub fn parse_remote_shell(specification: &OsStr) -> Result<Vec<OsString>, RemoteShellParseError> {
    let text = specification
        .to_str()
        .ok_or(RemoteShellParseError::InvalidEncoding)?;

    if text.as_bytes().contains(&b'\0') {
        return Err(RemoteShellParseError::InteriorNull);
    }

    let parts =
        shell_words::split(text).map_err(|err| RemoteShellParseError::Parse(err.to_string()))?;

    if parts.is_empty() {
        return Err(RemoteShellParseError::Empty);
    }

    Ok(parts.into_iter().map(OsString::from).collect())
}

#[cfg(test)]
mod tests {
    use super::{RemoteShellParseError, parse_remote_shell};
    use std::ffi::{OsStr, OsString};

    #[test]
    fn parses_basic_remote_shell_sequence() {
        let parsed =
            parse_remote_shell(OsStr::new("ssh -l backup -p 2222")).expect("parse succeeds");
        assert_eq!(
            parsed,
            vec![
                OsString::from("ssh"),
                OsString::from("-l"),
                OsString::from("backup"),
                OsString::from("-p"),
                OsString::from("2222"),
            ]
        );
    }

    #[test]
    fn parses_remote_shell_with_quotes_and_escapes() {
        let parsed = parse_remote_shell(OsStr::new(
            r#"ssh -oProxyCommand="ssh -W %h:%p gateway" -i'/path/to key'"#,
        ))
        .expect("parse succeeds");

        assert_eq!(parsed[0], OsString::from("ssh"));
        assert_eq!(
            parsed[1],
            OsString::from("-oProxyCommand=ssh -W %h:%p gateway")
        );
        assert_eq!(parsed[2], OsString::from("-i/path/to key"));
    }

    #[test]
    fn parser_rejects_unterminated_single_quote() {
        let error = parse_remote_shell(OsStr::new("ssh -o'ProxyCommand")).unwrap_err();
        assert!(matches!(error, RemoteShellParseError::Parse(_)));
    }

    #[test]
    fn parser_rejects_unterminated_double_quote() {
        let error = parse_remote_shell(OsStr::new("ssh -o\"ProxyCommand")).unwrap_err();
        assert!(matches!(error, RemoteShellParseError::Parse(_)));
    }

    #[test]
    fn parser_rejects_trailing_escape() {
        let error = parse_remote_shell(OsStr::new("ssh -oProxyCommand=\\")).unwrap_err();
        assert!(matches!(error, RemoteShellParseError::Parse(_)));
    }

    #[test]
    fn parser_rejects_empty_specification() {
        let error = parse_remote_shell(OsStr::new("   ")).unwrap_err();
        assert_eq!(error, RemoteShellParseError::Empty);
    }

    #[test]
    fn parser_retains_backslash_inside_single_quotes() {
        let parsed =
            parse_remote_shell(OsStr::new("ssh -o'Proxy\\Command'")).expect("parse succeeds");

        assert_eq!(parsed[0], OsString::from("ssh"));
        assert_eq!(parsed[1], OsString::from("-oProxy\\Command"));
    }

    #[cfg(unix)]
    #[test]
    fn parser_rejects_interior_null_byte() {
        use std::os::unix::ffi::OsStringExt;

        let spec = OsString::from_vec(b"ssh\0-p 22".to_vec());
        let error = parse_remote_shell(spec.as_os_str()).unwrap_err();

        assert_eq!(error, RemoteShellParseError::InteriorNull);
    }

    #[cfg(unix)]
    #[test]
    fn parser_rejects_invalid_unicode() {
        use std::os::unix::ffi::OsStringExt;

        let spec = OsString::from_vec(b"ssh \xff\xfe".to_vec());
        let error = parse_remote_shell(spec.as_os_str()).unwrap_err();

        assert_eq!(error, RemoteShellParseError::InvalidEncoding);
    }
}
