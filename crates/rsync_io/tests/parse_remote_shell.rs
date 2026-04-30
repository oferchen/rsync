//! Integration tests for [`rsync_io::parse_remote_shell`].
//!
//! These tests guard the contract that the public wrapper around
//! [`shell_words::split`] preserves the historical corpus of remote-shell
//! specifications accepted by the bespoke tokenizer it replaced, and that
//! it remains in lockstep with `shell_words::split` for arbitrary inputs.
//!
//! The unit tests below exercise specific syntactic shapes (quoting,
//! escapes, multi-word arguments, surrounding whitespace) that callers of
//! `-e/--rsh` and `RSYNC_RSH` rely on. The property tests fuzz arbitrary
//! ASCII inputs - including the metacharacters relevant to a POSIX shell
//! tokenizer - to assert that for any input either both implementations
//! produce identical token vectors or both reject the input.
//!
//! Tasks: #1877 (replace bespoke tokenizer), #1878 (parity property tests).

use std::ffi::{OsStr, OsString};

use proptest::prelude::*;
use rsync_io::{RemoteShellParseError, parse_remote_shell};

fn assert_parses_to(input: &str, expected: &[&str]) {
    let parsed = parse_remote_shell(OsStr::new(input))
        .unwrap_or_else(|err| panic!("parsing {input:?} should succeed but got {err:?}"));
    let expected_owned: Vec<OsString> = expected
        .iter()
        .map(|token| OsString::from(*token))
        .collect();
    assert_eq!(
        parsed, expected_owned,
        "tokens mismatch for input {input:?}"
    );
}

#[test]
fn parses_plain_command_without_arguments() {
    assert_parses_to("ssh", &["ssh"]);
}

#[test]
fn parses_command_with_short_flags() {
    assert_parses_to("ssh -p 2222", &["ssh", "-p", "2222"]);
}

#[test]
fn parses_command_with_user_and_port() {
    assert_parses_to(
        "ssh -l backup -p 2222",
        &["ssh", "-l", "backup", "-p", "2222"],
    );
}

#[test]
fn parses_double_quoted_argument_with_spaces() {
    assert_parses_to(
        r#"ssh -oProxyCommand="ssh -W %h:%p gateway""#,
        &["ssh", "-oProxyCommand=ssh -W %h:%p gateway"],
    );
}

#[test]
fn parses_single_quoted_argument_with_spaces() {
    assert_parses_to(
        "ssh -i '/path/to my key'",
        &["ssh", "-i", "/path/to my key"],
    );
}

#[test]
fn parses_mixed_quoting_in_one_token() {
    assert_parses_to(
        r#"ssh -oProxyCommand="ssh -W %h:%p gw" -i'/path/to key'"#,
        &["ssh", "-oProxyCommand=ssh -W %h:%p gw", "-i/path/to key"],
    );
}

#[test]
fn collapses_runs_of_whitespace_between_tokens() {
    assert_parses_to("ssh    -v   --   host", &["ssh", "-v", "--", "host"]);
}

#[test]
fn trims_leading_and_trailing_whitespace() {
    assert_parses_to("   ssh -v   ", &["ssh", "-v"]);
}

#[test]
fn parses_backslash_escaped_space() {
    assert_parses_to(r"ssh /path/with\ space", &["ssh", "/path/with space"]);
}

#[test]
fn preserves_backslash_inside_single_quotes() {
    assert_parses_to(r"ssh -o'Proxy\Command'", &["ssh", r"-oProxy\Command"]);
}

#[test]
fn rejects_empty_specification() {
    let error = parse_remote_shell(OsStr::new("")).unwrap_err();
    assert_eq!(error, RemoteShellParseError::Empty);
}

#[test]
fn rejects_whitespace_only_specification() {
    let error = parse_remote_shell(OsStr::new("   \t  ")).unwrap_err();
    assert_eq!(error, RemoteShellParseError::Empty);
}

#[test]
fn rejects_unterminated_single_quote() {
    let error = parse_remote_shell(OsStr::new("ssh -o'ProxyCommand")).unwrap_err();
    assert!(matches!(error, RemoteShellParseError::Parse(_)));
}

#[test]
fn rejects_unterminated_double_quote() {
    let error = parse_remote_shell(OsStr::new("ssh -o\"ProxyCommand")).unwrap_err();
    assert!(matches!(error, RemoteShellParseError::Parse(_)));
}

#[test]
fn rejects_trailing_backslash() {
    let error = parse_remote_shell(OsStr::new("ssh -oProxyCommand=\\")).unwrap_err();
    assert!(matches!(error, RemoteShellParseError::Parse(_)));
}

#[cfg(unix)]
#[test]
fn rejects_interior_nul_byte() {
    use std::os::unix::ffi::OsStringExt;

    let spec = OsString::from_vec(b"ssh\0-p 22".to_vec());
    let error = parse_remote_shell(spec.as_os_str()).unwrap_err();
    assert_eq!(error, RemoteShellParseError::InteriorNull);
}

#[cfg(unix)]
#[test]
fn rejects_invalid_unicode() {
    use std::os::unix::ffi::OsStringExt;

    let spec = OsString::from_vec(b"ssh \xff\xfe".to_vec());
    let error = parse_remote_shell(spec.as_os_str()).unwrap_err();
    assert_eq!(error, RemoteShellParseError::InvalidEncoding);
}

/// Generates strings drawn from a curated alphabet that exercises every
/// metacharacter relevant to POSIX shell tokenization (single and double
/// quotes, backslash escapes, embedded newlines, runs of whitespace, plus
/// printable ASCII payload bytes). Lengths up to 64 keep proptest cases
/// fast while still triggering deeply nested quote/escape interactions.
fn shell_input_strategy() -> impl Strategy<Value = String> {
    let alphabet: Vec<char> = "abcXYZ09 \t\n\\\"'-_/=:.%h%p#".chars().collect();
    proptest::collection::vec(proptest::sample::select(alphabet), 0..64)
        .prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    /// `parse_remote_shell` must produce exactly the same token vector as
    /// `shell_words::split` for any input that contains no NUL byte and
    /// tokenizes to a non-empty argv. Inputs containing a NUL byte or that
    /// fail to tokenize are validated against their respective error
    /// classes in the companion property tests below.
    #[test]
    fn parity_with_shell_words_for_valid_inputs(input in shell_input_strategy()) {
        prop_assume!(!input.as_bytes().contains(&b'\0'));

        match (parse_remote_shell(OsStr::new(&input)), shell_words::split(&input)) {
            (Ok(parsed), Ok(expected)) => {
                prop_assume!(!expected.is_empty());
                let expected_os: Vec<OsString> =
                    expected.into_iter().map(OsString::from).collect();
                prop_assert_eq!(parsed, expected_os);
            }
            (Err(RemoteShellParseError::Empty), Ok(expected)) => {
                prop_assert!(expected.is_empty());
            }
            (Err(RemoteShellParseError::Parse(_)), Err(_)) => {
                // Both rejected the same input; nothing else to assert.
            }
            (lhs, rhs) => {
                prop_assert!(
                    false,
                    "divergent classification for {input:?}: parse_remote_shell={lhs:?}, shell_words={rhs:?}"
                );
            }
        }
    }

    /// Any input containing an interior NUL byte must be rejected with
    /// [`RemoteShellParseError::InteriorNull`], regardless of whether
    /// `shell_words::split` would otherwise accept it.
    #[test]
    fn nul_bytes_are_always_rejected(prefix in "[a-z ]{0,16}", suffix in "[a-z ]{0,16}") {
        let mut input = prefix;
        input.push('\0');
        input.push_str(&suffix);
        let error = parse_remote_shell(OsStr::new(&input)).unwrap_err();
        prop_assert_eq!(error, RemoteShellParseError::InteriorNull);
    }

    /// Whitespace-only inputs (any combination of spaces, tabs, and
    /// newlines) must be reported as [`RemoteShellParseError::Empty`].
    #[test]
    fn whitespace_only_inputs_are_empty(input in "[ \t\n]{0,32}") {
        let error = parse_remote_shell(OsStr::new(&input)).unwrap_err();
        prop_assert_eq!(error, RemoteShellParseError::Empty);
    }
}
