#![allow(clippy::module_name_repetitions)]

//! Helpers for parsing remote shell specifications supplied via `-e/--rsh`.

use std::borrow::Cow;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// Errors returned when parsing remote shell specifications fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteShellParseError {
    /// The specification was empty or consisted solely of whitespace.
    Empty,
    /// A backslash escape reached the end of the specification.
    UnterminatedEscape,
    /// A single-quoted string was not terminated.
    UnterminatedSingleQuote,
    /// A double-quoted string was not terminated.
    UnterminatedDoubleQuote,
    /// The specification contained an interior NUL byte.
    InteriorNull,
    /// Non-Unicode data was supplied on a platform that requires Unicode.
    InvalidEncoding,
}

impl fmt::Display for RemoteShellParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("remote shell specification is empty"),
            Self::UnterminatedEscape => {
                f.write_str("remote shell specification ended after escape")
            }
            Self::UnterminatedSingleQuote => {
                f.write_str("remote shell specification has an unterminated single quote")
            }
            Self::UnterminatedDoubleQuote => {
                f.write_str("remote shell specification has an unterminated double quote")
            }
            Self::InteriorNull => f.write_str("remote shell specification contains a NUL byte"),
            Self::InvalidEncoding => {
                f.write_str("remote shell specification contains invalid Unicode for this platform")
            }
        }
    }
}

impl Error for RemoteShellParseError {}

/// Parses a remote shell specification using rsync's quoting rules.
pub fn parse_remote_shell(specification: &OsStr) -> Result<Vec<OsString>, RemoteShellParseError> {
    let bytes = specification_bytes(specification)?;
    let mut args = Vec::new();
    let mut current = Vec::new();
    let mut token_started = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;

    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'\0' {
            return Err(RemoteShellParseError::InteriorNull);
        }

        if !in_single && !in_double && is_ascii_whitespace(byte) {
            if token_started {
                finish_token(&mut args, &mut current, &mut token_started);
            }
            i += 1;
            while i < bytes.len() && is_ascii_whitespace(bytes[i]) {
                i += 1;
            }
            continue;
        }

        match byte {
            b'\'' => {
                if in_double {
                    current.push(byte);
                } else {
                    in_single = !in_single;
                }
                token_started = true;
                i += 1;
            }
            b'"' => {
                if in_single {
                    current.push(byte);
                } else {
                    in_double = !in_double;
                }
                token_started = true;
                i += 1;
            }
            b'\\' => {
                if in_single {
                    current.push(byte);
                    token_started = true;
                    i += 1;
                    continue;
                }

                i += 1;
                if i == bytes.len() {
                    return Err(RemoteShellParseError::UnterminatedEscape);
                }

                let next = bytes[i];
                if next == b'\0' {
                    return Err(RemoteShellParseError::InteriorNull);
                }

                if next == b'\n' {
                    i += 1;
                    continue;
                }

                if in_double {
                    match next {
                        b'"' | b'\\' | b'$' | b'`' => {
                            current.push(next);
                        }
                        _ => {
                            current.push(b'\\');
                            current.push(next);
                        }
                    }
                } else {
                    current.push(next);
                }
                token_started = true;
                i += 1;
            }
            _ => {
                current.push(byte);
                token_started = true;
                i += 1;
            }
        }
    }

    if in_single {
        return Err(RemoteShellParseError::UnterminatedSingleQuote);
    }
    if in_double {
        return Err(RemoteShellParseError::UnterminatedDoubleQuote);
    }

    if token_started {
        finish_token(&mut args, &mut current, &mut token_started);
    }

    if args.is_empty() {
        return Err(RemoteShellParseError::Empty);
    }

    Ok(args)
}

fn finish_token(args: &mut Vec<OsString>, current: &mut Vec<u8>, token_started: &mut bool) {
    args.push(os_string_from_bytes(std::mem::take(current)));
    *token_started = false;
}

fn is_ascii_whitespace(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c' // space, tab, newline, carriage return, vertical tab, form feed
    )
}

fn specification_bytes(spec: &OsStr) -> Result<Cow<'_, [u8]>, RemoteShellParseError> {
    #[cfg(unix)]
    {
        Ok(Cow::Borrowed(spec.as_bytes()))
    }

    #[cfg(not(unix))]
    {
        spec.to_str()
            .map(|s| Cow::Owned(s.as_bytes().to_vec()))
            .ok_or(RemoteShellParseError::InvalidEncoding)
    }
}

fn os_string_from_bytes(bytes: Vec<u8>) -> OsString {
    #[cfg(unix)]
    {
        OsString::from_vec(bytes)
    }

    #[cfg(not(unix))]
    {
        OsString::from(String::from_utf8(bytes).expect("validated UTF-8"))
    }
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
        assert_eq!(error, RemoteShellParseError::UnterminatedSingleQuote);
    }

    #[test]
    fn parser_rejects_unterminated_double_quote() {
        let error = parse_remote_shell(OsStr::new("ssh -o\"ProxyCommand")).unwrap_err();
        assert_eq!(error, RemoteShellParseError::UnterminatedDoubleQuote);
    }

    #[test]
    fn parser_rejects_trailing_escape() {
        let error = parse_remote_shell(OsStr::new("ssh -oProxyCommand=\\")).unwrap_err();
        assert_eq!(error, RemoteShellParseError::UnterminatedEscape);
    }

    #[test]
    fn parser_rejects_empty_specification() {
        let error = parse_remote_shell(OsStr::new("   ")).unwrap_err();
        assert_eq!(error, RemoteShellParseError::Empty);
    }

    #[test]
    fn parser_retains_backslash_inside_single_quotes() {
        let parsed = parse_remote_shell(OsStr::new("ssh -o'Proxy\\Command'"))
            .expect("parse succeeds");

        assert_eq!(parsed[0], OsString::from("ssh"));
        assert_eq!(parsed[1], OsString::from("-oProxy\\Command"));
    }

    #[test]
    fn parser_honours_double_quote_escape_rules() {
        let parsed = parse_remote_shell(OsStr::new(
            r#"ssh -o"ProxyCommand=echo \$HOME \a""#,
        ))
        .expect("parse succeeds");

        assert_eq!(parsed[0], OsString::from("ssh"));
        assert_eq!(parsed[1], OsString::from("-oProxyCommand=echo $HOME \\a"));
    }

    #[test]
    fn parser_strips_newline_after_escape_sequence() {
        let parsed = parse_remote_shell(OsStr::new(
            "ssh -oProxyCommand=echo\\\nbar",
        ))
        .expect("parse succeeds");

        assert_eq!(parsed[1], OsString::from("-oProxyCommand=echobar"));
    }

    #[cfg(unix)]
    #[test]
    fn parser_rejects_interior_null_byte() {
        use std::os::unix::ffi::OsStringExt;

        let spec = OsString::from_vec(b"ssh\0-p 22".to_vec());
        let error = parse_remote_shell(spec.as_os_str()).unwrap_err();

        assert_eq!(error, RemoteShellParseError::InteriorNull);
    }

    #[test]
    fn parser_treats_extended_ascii_whitespace_as_delimiters() {
        let spec = OsString::from("ssh\u{0B}-p\u{0C}2222");
        let parsed = parse_remote_shell(spec.as_os_str()).expect("parse succeeds");

        assert_eq!(
            parsed,
            vec![
                OsString::from("ssh"),
                OsString::from("-p"),
                OsString::from("2222"),
            ]
        );
    }
}
