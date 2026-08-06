//! Regression tests pinning that oc-rsync's remote server-arg quoting survives
//! re-tokenization by a remote SSH server, including a Windows OpenSSH host.
//!
//! # Why this matters
//!
//! When oc-rsync runs a transfer over SSH to `user@host:path`, it hands the
//! `--server` invocation to the local `ssh` client as a vector of argv
//! elements (`ssh_transfer::connection::build_ssh_connection` ->
//! `rsync_io::ssh::SshCommand::set_remote_command`). The OpenSSH client joins
//! that vector into a single command string with single ASCII spaces and sends
//! it to the remote `sshd`, which hands it to the remote user's shell. That
//! shell RE-TOKENIZES the string back into argv before the remote `rsync
//! --server` sees it. The quoting oc applies must survive that split so the
//! reconstructed server argv is byte-identical to the intended one.
//!
//! The re-tokenizer depends on the remote:
//!
//! - A POSIX remote runs `/bin/sh -c "<cmd>"`; the split is `/bin/sh`
//!   word-splitting with backslash escapes. This is what upstream rsync's
//!   `options.c:safe_arg()` backslash-escaping targets, and what oc mirrors in
//!   `invocation::builder::shell_safe_filename_arg`.
//! - A **Windows OpenSSH** remote runs the command through the native Windows
//!   argv split (`CommandLineToArgvW` / the MSVCRT C runtime), whose backslash
//!   and quote rules differ fundamentally from POSIX: a backslash is literal
//!   unless it precedes a `"`, and whitespace is never escaped by a backslash.
//!   POSIX backslash-escaping therefore does NOT survive a Windows-native
//!   re-tokenizer.
//!
//! upstream rsync solves this for BOTH remotes the same way it always has:
//! `--secluded-args` (`-s`, formerly `--protect-args`), which stops emitting the
//! path/option arguments on the command line entirely and ships them
//! null-separated over the protocol stream after connection
//! (`rsync.c:283-320 send_protected_args()`, `options.c:2744` NULL cutoff). The
//! null-separated stream is immune to ANY shell/argv re-tokenizer.
//!
//! These tests pin, as a black-box contract over the invocation builder:
//!
//! 1. The pure-Rust `CommandLineToArgvW` model is faithful to the documented
//!    Microsoft algorithm (validated against MSDN reference vectors).
//! 2. Default (non-secluded) path: oc's shell-escaped command line reconstructs
//!    the intended server argv under a POSIX re-tokenizer (matches upstream
//!    `safe_arg`), and demonstrably does NOT under the Windows re-tokenizer -
//!    the documented reason `--secluded-args` exists for Windows remotes.
//! 3. `--secluded-args` path: no path/option argument reaches the command line;
//!    the residual command-line head carries no re-tokenization-hazard byte, so
//!    it reconstructs identically under BOTH the POSIX and Windows tokenizers,
//!    and the paths ride the stdin stream verbatim (byte-exact), immune to
//!    re-tokenization.

use std::ffi::{OsStr, OsString};

use super::RemoteRole;
use super::builder::RemoteInvocationBuilder;
use crate::client::config::ClientConfig;

/// Returns the raw bytes backing an `OsStr`, mirroring the byte view that
/// `ssh_transfer::connection` ships (Unix: verbatim filesystem bytes; other
/// targets: the WTF-8 encoding, exact for the Unicode operands they carry).
fn os_bytes(s: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        s.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        s.as_encoded_bytes().to_vec()
    }
}

/// Joins argv elements the way the OpenSSH client joins a remote command before
/// sending it to the remote `sshd`: single ASCII spaces, no added quoting.
fn ssh_join(args: &[OsString]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        out.extend_from_slice(&os_bytes(a));
    }
    out
}

/// Pure-Rust model of Windows `CommandLineToArgvW` / MSVCRT argv
/// re-tokenization.
///
/// Implements the documented Microsoft algorithm ("Parsing C++ Command-Line
/// Arguments"), identical to the parser the Rust standard library uses to split
/// `lpCmdLine` on Windows (`library/std/src/sys/args/windows.rs`):
///
/// - Whitespace (space or tab) delimits arguments outside quotes.
/// - `2n` backslashes followed by `"` emit `n` backslashes and toggle the
///   quoted state (the `"` is an unescaped delimiter-quote).
/// - `2n+1` backslashes followed by `"` emit `n` backslashes and a literal `"`.
/// - Backslashes not followed by `"` are literal.
/// - Inside quotes, `""` emits one literal `"` and stays quoted; otherwise `"`
///   toggles the quoted state.
///
/// Operates on bytes: every metacharacter (space, tab, `"`, `\`) is ASCII, so
/// byte-level handling is faithful and non-ASCII argument bytes pass through
/// verbatim, exactly as `CommandLineToArgvW` treats opaque non-metacharacter
/// UTF-16 code units.
fn command_line_to_argv(cmd: &[u8]) -> Vec<Vec<u8>> {
    let mut args = Vec::new();
    let mut i = 0;
    let n = cmd.len();
    while i < n {
        while i < n && (cmd[i] == b' ' || cmd[i] == b'\t') {
            i += 1;
        }
        if i >= n {
            break;
        }
        let mut cur = Vec::new();
        let mut in_quotes = false;
        while i < n {
            let c = cmd[i];
            if !in_quotes && (c == b' ' || c == b'\t') {
                break;
            }
            if c == b'\\' {
                let mut nslash = 0usize;
                while i < n && cmd[i] == b'\\' {
                    nslash += 1;
                    i += 1;
                }
                if i < n && cmd[i] == b'"' {
                    cur.resize(cur.len() + nslash / 2, b'\\');
                    if nslash % 2 == 1 {
                        cur.push(b'"');
                        i += 1;
                    }
                    // Even count: leave the `"` for the quote branch below.
                } else {
                    cur.resize(cur.len() + nslash, b'\\');
                }
                continue;
            }
            if c == b'"' {
                if in_quotes && i + 1 < n && cmd[i + 1] == b'"' {
                    cur.push(b'"');
                    i += 2;
                } else {
                    in_quotes = !in_quotes;
                    i += 1;
                }
                continue;
            }
            cur.push(c);
            i += 1;
        }
        args.push(cur);
    }
    args
}

/// Pure-Rust model of a POSIX `/bin/sh` re-tokenizer restricted to the escape
/// vocabulary oc-rsync actually emits: a backslash escapes the following byte
/// (dropping the backslash), and unescaped whitespace delimits arguments.
///
/// oc never emits single- or double-quoted words (its `safe_arg` mirror uses
/// backslash escapes exclusively), so this narrow model is exact for the
/// command lines under test. It is the inverse of
/// `invocation::builder::shell_safe_filename_arg`.
fn posix_sh_split(cmd: &[u8]) -> Vec<Vec<u8>> {
    let mut args = Vec::new();
    let mut i = 0;
    let n = cmd.len();
    while i < n {
        while i < n && (cmd[i] == b' ' || cmd[i] == b'\t') {
            i += 1;
        }
        if i >= n {
            break;
        }
        let mut cur = Vec::new();
        while i < n {
            let c = cmd[i];
            if c == b' ' || c == b'\t' {
                break;
            }
            if c == b'\\' && i + 1 < n {
                cur.push(cmd[i + 1]);
                i += 2;
                continue;
            }
            cur.push(c);
            i += 1;
        }
        args.push(cur);
    }
    args
}

#[test]
fn windows_argv_model_matches_msdn_reference_vectors() {
    // Reference vectors from Microsoft's "Parsing C++ Command-Line Arguments".
    // Encoding WHY: the whole Windows-safety argument rests on this model being
    // the real CommandLineToArgvW algorithm, so it is pinned against the
    // authoritative examples before it is trusted to judge oc's output.
    let cases: &[(&str, &[&str])] = &[
        (r#""abc" d e"#, &["abc", "d", "e"]),
        (r#"a\\\b d"e f"g h"#, &[r"a\\\b", "de fg", "h"]),
        (r#"a\\\"b c d"#, &[r#"a\"b"#, "c", "d"]),
        (r#"a\\\\"b c" d e"#, &[r"a\\b c", "d", "e"]),
    ];
    for (input, expected) in cases {
        let got = command_line_to_argv(input.as_bytes());
        let got_str: Vec<String> = got
            .iter()
            .map(|a| String::from_utf8(a.clone()).unwrap())
            .collect();
        assert_eq!(
            got_str,
            expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "CommandLineToArgvW model diverged on {input:?}"
        );
    }
}

#[test]
fn posix_split_inverts_oc_backslash_escaping() {
    // Sanity-pin the POSIX model against the exact escape shape oc emits so it
    // is trustworthy as the reference re-tokenizer for a POSIX remote.
    assert_eq!(
        posix_sh_split(br"dir\ with\ space/file"),
        vec![b"dir with space/file".to_vec()]
    );
    assert_eq!(posix_sh_split(br#"a\"b\\c"#), vec![br#"a"b\c"#.to_vec()]);
    assert_eq!(
        posix_sh_split(b"one two three"),
        vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]
    );
}

/// A remote path whose bytes exercise every re-tokenization hazard shared by
/// the POSIX and Windows splitters: a space, a backslash, and a double quote.
const HOSTILE_PATH: &str = r#"/remote/dir with\slash and"quote/file"#;

#[test]
fn default_command_line_survives_posix_but_not_windows_retokenization() {
    // Default config: protect_args unset -> oc emits the paths on the command
    // line, backslash-escaped exactly like upstream `options.c:safe_arg()`
    // (`options.c:1983-1993`: both oc and upstream default protect_args to 0).
    let config = ClientConfig::builder().build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let cmd_args = builder.build_with_paths(&[OsStr::new(HOSTILE_PATH)]);

    // The path is present on the command line, and in escaped (not verbatim)
    // form - so it is exposed to whatever the remote shell does with it.
    let last = cmd_args.last().unwrap();
    assert_ne!(
        os_bytes(last),
        HOSTILE_PATH.as_bytes(),
        "default path must be shell-escaped on the command line, not verbatim"
    );

    let wire = ssh_join(&cmd_args);

    // POSIX remote (`/bin/sh -c`): oc's escaping round-trips, reconstructing the
    // intended server argv byte-for-byte, with the final token the ORIGINAL
    // unescaped path. This is the contract oc shares with upstream rsync.
    let posix = posix_sh_split(&wire);
    let mut intended: Vec<Vec<u8>> = cmd_args[..cmd_args.len() - 1]
        .iter()
        .map(|a| os_bytes(a))
        .collect();
    intended.push(HOSTILE_PATH.as_bytes().to_vec());
    assert_eq!(
        posix, intended,
        "oc's backslash escaping must reconstruct the intended argv under a POSIX remote"
    );

    // Windows OpenSSH remote (CommandLineToArgvW): POSIX backslash escaping does
    // NOT survive - the escaped spaces split the single path into multiple argv
    // tokens, corrupting the server argv. This is precisely why a Windows remote
    // requires `--secluded-args` (asserted below). We pin the corruption so a
    // future change that silently routed this path differently is caught.
    let windows = command_line_to_argv(&wire);
    assert_ne!(
        windows, intended,
        "POSIX-escaped command line must NOT be assumed safe under Windows re-tokenization"
    );
    assert!(
        !windows.iter().any(|tok| tok == HOSTILE_PATH.as_bytes()),
        "the intended path must not reconstruct as a single Windows argv token"
    );
    assert!(
        windows.len() > intended.len(),
        "the escaped spaces must over-split the path under Windows re-tokenization"
    );
}

#[test]
fn secluded_args_head_is_retokenization_invariant_on_every_remote() {
    // `--secluded-args` (-s / --protect-args): the re-tokenization-immune path.
    let config = ClientConfig::builder().protect_args(Some(true)).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let paths = [
        OsStr::new(HOSTILE_PATH),
        OsStr::new(r#"second path/with"quote"#),
    ];
    let secluded = builder.build_secluded(&paths);

    // 1. NO path argument reaches the command line, and every command-line token
    //    is non-empty and free of the bytes any re-tokenizer treats specially
    //    (space, tab, backslash, double-quote). This is the property that makes
    //    the head split-invariant.
    for tok in &secluded.command_line_args {
        let b = os_bytes(tok);
        assert!(!b.is_empty(), "no command-line token may be empty");
        assert!(
            !b.iter()
                .any(|&c| c == b' ' || c == b'\t' || c == b'\\' || c == b'"'),
            "command-line head must carry no re-tokenization-hazard byte: {tok:?}"
        );
        assert!(
            b != HOSTILE_PATH.as_bytes(),
            "no path argument may appear on the command line under secluded-args"
        );
    }

    // 2. The head reconstructs IDENTICALLY under BOTH the POSIX and Windows
    //    re-tokenizers - the contract that lets a Windows OpenSSH remote work.
    let wire = ssh_join(&secluded.command_line_args);
    let expected: Vec<Vec<u8>> = secluded
        .command_line_args
        .iter()
        .map(|a| os_bytes(a))
        .collect();
    assert_eq!(
        posix_sh_split(&wire),
        expected,
        "secluded head must reconstruct under a POSIX remote"
    );
    assert_eq!(
        command_line_to_argv(&wire),
        expected,
        "secluded head must reconstruct under a Windows OpenSSH remote"
    );

    // 3. The paths travel VERBATIM in the stdin stream (null-separated,
    //    re-tokenizer-immune), byte-exact - never shell-escaped.
    let stdin: Vec<Vec<u8>> = secluded.stdin_args.iter().map(|a| os_bytes(a)).collect();
    for p in &paths {
        assert!(
            stdin.iter().any(|a| a == &os_bytes(p)),
            "each path must ride the stdin stream verbatim: {p:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn secluded_args_ship_non_utf8_path_verbatim_off_the_command_line() {
    // A non-UTF-8 remote path is a legal Unix filename that rsync carries as a
    // raw `char*`. Under secluded-args it must ship verbatim over stdin and
    // never touch the re-tokenized command line.
    use std::os::unix::ffi::OsStrExt;
    let raw = b"/remote/na\xffme with space/\xfe";
    let path = OsStr::from_bytes(raw);

    let config = ClientConfig::builder().protect_args(Some(true)).build();
    let builder = RemoteInvocationBuilder::new(&config, RemoteRole::Sender);
    let secluded = builder.build_secluded(&[path]);

    assert!(
        secluded.stdin_args.iter().any(|a| a.as_bytes() == raw),
        "non-UTF-8 path must ride the stdin stream verbatim"
    );
    for tok in &secluded.command_line_args {
        assert!(
            !tok.as_bytes().windows(raw.len()).any(|w| w == raw),
            "non-UTF-8 path bytes must never appear on the command line"
        );
    }
}
