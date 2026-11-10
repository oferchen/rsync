//! A remote-shell transfer whose two sides use different compressors ends with
//! `RERR_STREAMIO` (12) on both directions, and the client never hangs.
//!
//! upstream: token.c - an inflate or ZSTD failure on the receiver is
//! `exit_cleanup(RERR_STREAMIO)` (token.c:664-668, token.c:902-903), and
//! `_exit_cleanup()` reaps the remote-shell child with `WNOHANG` before
//! exiting (cleanup.c:146-153), so the closed descriptors let the remote side
//! see EOF. Measured on 3.5.1 with the same rsh rewrite: exit 12 at once for a
//! pull and for a push.
//!
//! The desync is forced by an rsh wrapper that rewrites the server's
//! compression option, so the server compresses with one codec while the
//! client decompresses with another.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use test_support::{
    CliOutput, LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

/// A 300 KB pseudo-random file, so the stream holds several compressed blocks.
fn source(root: &Path) -> PathBuf {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("mkdir src");
    let mut state: u32 = 0x2545_f491;
    let data: Vec<u8> = (0..300_000)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect();
    fs::write(src.join("f"), data).expect("write f");
    src
}

/// Writes an rsh wrapper that replaces the server argument `from` with `to`
/// and runs the lsh stub.
fn rewriting_rsh(root: &Path, from: &str, to: &str) -> PathBuf {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let script = root.join("rsh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nn=$#\nwhile [ $n -gt 0 ]; do\n  a=$1; shift; n=$((n-1))\n  \
             [ \"$a\" = '{from}' ] && a='{to}'\n  set -- \"$@\" \"$a\"\ndone\nexec '{}' \"$@\"\n",
            stub.path().display()
        ),
    )
    .expect("write rsh");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    script
}

fn run(root: &Path, client_choice: &str, rsh: &Path, pull: bool) -> CliOutput {
    let src = source(root);
    let dest = root.join("dest");
    fs::create_dir_all(&dest).expect("mkdir dest");
    let runner = OcRsyncCliRunner::new()
        .args(["-r", "-z"])
        .arg(format!("--compress-choice={client_choice}"))
        .arg(format!("--rsh={}", rsh.display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ));
    let runner = if pull {
        runner
            .arg(format!("localhost:{}/", src.display()))
            .arg(format!("{}/", dest.display()))
    } else {
        runner
            .arg(format!("{}/", src.display()))
            .arg(format!("localhost:{}/", dest.display()))
    };
    runner.run().expect("transfer finishes instead of hanging")
}

/// The client receiver fails to decode the server's zlib stream as zstd. It
/// must exit 12 instead of waiting forever on a server that is still waiting
/// for input.
#[test]
fn pull_with_mismatched_compressors_exits_streamio() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    // upstream: options.c:2994-2999 server_options() - a zlib choice is sent
    // as --old-compress, any other codec as --compress-choice=NAME.
    let rsh = rewriting_rsh(tmp.path(), "--compress-choice=zstd", "--old-compress");
    run(tmp.path(), "zstd", &rsh, true).assert_exit(12);
}

/// The server receiver fails to decode the client's zlib stream as zstd and
/// exits 12, which the client reports.
#[test]
fn push_with_mismatched_compressors_exits_streamio() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let rsh = rewriting_rsh(tmp.path(), "--old-compress", "--compress-choice=zstd");
    run(tmp.path(), "zlib", &rsh, false).assert_exit(12);
}
