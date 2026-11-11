//! A server argv parsed the way 3.5.1's single popt pass parses it.
//!
//! upstream: options.c:1497 - one popt pass over the whole argv, so a short
//! option counts wherever it sits; options.c:1517-1524 - the first `--server`
//! restarts that pass, so an option ahead of it is parsed twice; options.c:
//! 2051-2054 and main.c:1909-1912 - an unknown option is `rsync: [on remote
//! machine: ]<opt>: unknown option` and RERR_SYNTAX, with the `on remote
//! machine` prefix and the `[server=...]` role only once `--server` was seen.
//! Each push runs under a deadline so a hang fails as a TIMEOUT.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use test_support::{Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined};

const BUDGET: Duration = Duration::from_secs(30);

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// The 3.5.1 binary, which drives every cell as the client and serves as the
/// reference server.
fn upstream() -> Option<PathBuf> {
    require_upstream_rsync(UpstreamVersion::V3_5_1).map(|u| u.binary().to_path_buf())
}

struct Push {
    _temp: tempfile::TempDir,
    status: Option<i32>,
    /// The client's stdout then stderr; a server's FINFO reaches stdout.
    output: String,
    dest: PathBuf,
}

/// Pushes `src/` with `client` to `server`, whose argv gets `before` ahead of
/// `--server` and `after` right behind it.
fn push(client: &Path, server: &Path, before: &[&str], after: &[&str]) -> Push {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("src/d")).unwrap();
    fs::create_dir_all(root.join("dst")).unwrap();
    fs::write(root.join("src/d/f"), "x\n").unwrap();
    let quote = |tokens: &[&str]| -> String { tokens.iter().map(|t| format!("'{t}' ")).collect() };
    // A remote shell that runs the server locally, splicing the extra tokens
    // around the `--server` the client put first.
    let rsh = root.join("rsh");
    fs::write(
        &rsh,
        format!(
            "#!/bin/sh\nshift\nprog=$1\nshift\nshift\nexec \"$prog\" {}--server {}\"$@\"\n",
            quote(before),
            quote(after)
        ),
    )
    .unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();
    let dest = root.join("dst");
    let mut command = Command::new(client);
    command
        .arg("-r")
        .arg("-e")
        .arg(&rsh)
        .arg(format!("--rsync-path={}", server.display()))
        .arg(format!("{}/", root.join("src").display()))
        .arg(format!("localhost:{}/", dest.display()));
    match run_deadlined(&mut command, BUDGET).unwrap() {
        Deadlined::Finished {
            status,
            stdout,
            stderr,
        } => Push {
            _temp: temp,
            status: status.code(),
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            ),
            dest,
        },
        Deadlined::Expired { budget, stderr, .. } => panic!(
            "TIMEOUT: push with server argv {before:?} --server {after:?} still running after \
             {budget:?}: {}",
            String::from_utf8_lossy(&stderr)
        ),
    }
}

/// The server's own refusal lines: the program name and the trailer's
/// version and source location are each implementation's own, so only the
/// text after the name and the role tag are compared.
fn refusal(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|line| line.contains("unknown option") || line.contains("syntax or usage"))
        .map(|line| {
            let text = line.split_once(": ").map_or(line, |(_, rest)| rest);
            match text.find(" at ") {
                Some(at) => {
                    let role = text.rfind('[').map_or("", |open| {
                        let tag = &text[open..];
                        &tag[..tag.find('=').unwrap_or(tag.len())]
                    });
                    format!("{}{role}", &text[..at])
                }
                None => text.to_owned(),
            }
        })
        .collect()
}

/// A stray `-v` ahead of `--server` is honoured, and counts twice as 3.5.1's
/// restarted pass counts it: at that level the server reports its delta mode.
#[test]
fn verbose_ahead_of_server_is_honoured_twice() {
    let Some(upstream) = upstream() else { return };
    for server in [&upstream, &oc_rsync()] {
        let push = push(&upstream, server, &["-v"], &[]);
        assert_eq!(
            push.status,
            Some(0),
            "{}: {}",
            server.display(),
            push.output
        );
        assert_eq!(fs::read_to_string(push.dest.join("d/f")).unwrap(), "x\n");
        assert!(
            push.output.contains("delta-transmission enabled"),
            "{}: {}",
            server.display(),
            push.output
        );
    }
}

/// Unknown short and long options, on either side of `--server`, are refused
/// with 3.5.1's text and role.
#[test]
fn unknown_options_are_refused_like_upstream() {
    let Some(upstream) = upstream() else { return };
    let cases: [(&[&str], &[&str]); 4] = [
        (&["-j"], &[]),
        (&[], &["-vj"]),
        (&["--bogus"], &[]),
        (&[], &["--bogus"]),
    ];
    for (before, after) in cases {
        let expected = push(&upstream, &upstream, before, after);
        let actual = push(&upstream, &oc_rsync(), before, after);
        assert_eq!(expected.status, Some(12), "oracle: {}", expected.output);
        assert_eq!(actual.status, expected.status, "{before:?} {after:?}");
        assert_eq!(
            refusal(&actual.output),
            refusal(&expected.output),
            "{before:?} --server {after:?}\noc: {}\n3.5.1: {}",
            actual.output,
            expected.output
        );
        assert!(!actual.dest.join("d").exists(), "{before:?} {after:?}");
    }
}
