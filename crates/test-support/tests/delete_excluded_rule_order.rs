//! Wire oracle: the filter rules a client sends under `--delete-excluded`.
//!
//! upstream: exclude.c:1478-1481 parse_rule_tok() adds the implicit
//! `FILTRULE_SENDER_SIDE` only to a rule parsed once `delete_excluded` is
//! already set, and exclude.c:1864-1866 get_rule_prefix() writes `s` from
//! that flag alone. So for `--exclude=a --delete-excluded --exclude=b` a
//! pulling client sends `- a` then `-s b`. send_rules() (exclude.c:1903-1913)
//! elides every unsided rule as sender-only under `--delete-excluded`, so a
//! pushing client sends neither.
//!
//! Each cell captures the client-to-server stream through `capture-rsh` and
//! looks for each rule's `write_int(len)` + text framing (exclude.c:1936-1937).
//! The pinned bytes were recorded from rsync 3.5.1; when that release is
//! installed, its client is captured too and must send the same bytes.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use test_support::transcript::TranscriptRecorder;
use test_support::upstream_compat::{UpstreamVersion, require_upstream_rsync};

const ARGS: [&str; 4] = ["-r", "--exclude=a", "--delete-excluded", "--exclude=b"];

/// One rule as send_rules() frames it: a 4-byte little-endian length, then
/// the prefix and pattern.
fn framed(rule: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(rule.len()).unwrap().to_le_bytes().to_vec();
    bytes.extend_from_slice(rule);
    bytes
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Runs `client` against `server` through the capture trampoline and returns
/// the client-to-server bytes. `pull` picks which side holds the source.
fn client_to_server(client: &Path, server: &Path, pull: bool) -> Vec<u8> {
    let workdir = test_support::create_tempdir();
    let src = workdir.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("f"), b"payload\n").unwrap();
    let dest = workdir.path().join("dest");
    let (from, to) = if pull {
        (
            format!("transcripthost:{}/", src.display()),
            format!("{}/", dest.display()),
        )
    } else {
        (
            format!("{}/", src.display()),
            format!("transcripthost:{}/", dest.display()),
        )
    };
    let recorder = TranscriptRecorder::new(workdir.path());
    let capture_rsh = Path::new(env!("CARGO_BIN_EXE_capture-rsh"));
    let output = recorder
        .command(client, server, capture_rsh)
        .args(ARGS)
        .arg(from)
        .arg(to)
        .output()
        .unwrap();
    assert!(output.status.success(), "{client:?}: {output:?}");
    recorder.finish().expect("transcript").client_to_server
}

/// The clients to capture: oc always, and 3.5.1 when it is installed. Both
/// talk to the same server so only the client differs.
fn clients() -> (Vec<(&'static str, PathBuf)>, PathBuf) {
    let oc = test_support::oc_rsync_bin();
    match require_upstream_rsync(UpstreamVersion::V3_5_1) {
        Some(upstream) => {
            let upstream = upstream.binary().to_path_buf();
            (vec![("oc", oc), ("upstream", upstream.clone())], upstream)
        }
        None => (vec![("oc", oc.clone())], oc),
    }
}

#[test]
fn pull_sends_s_only_on_rules_after_delete_excluded() {
    let (clients, server) = clients();
    for (name, client) in clients {
        let c2s = client_to_server(&client, &server, true);
        let expected = [framed(b"- a"), framed(b"-s b"), vec![0; 4]].concat();
        assert!(
            contains(&c2s, &expected),
            "{name}: rule list `- a`, `-s b` not found in the client stream"
        );
    }
}

#[test]
fn push_elides_every_rule_under_delete_excluded() {
    let (clients, server) = clients();
    for (name, client) in clients {
        let c2s = client_to_server(&client, &server, false);
        for rule in [&b"- a"[..], b"-s a", b"- b", b"-s b"] {
            assert!(
                !contains(&c2s, &framed(rule)),
                "{name}: a pushing client sent {:?}",
                String::from_utf8_lossy(rule)
            );
        }
    }
}
