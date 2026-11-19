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
//!
//! Both cells pass `--timeout=5`, which reaches the server. A client that
//! failed to close its end would leave the server waiting for input until
//! that timeout ends it with an `io timeout after 5 seconds` line
//! (upstream: io.c:264-266 check_timeout()). The client still exits with
//! its own 12 (cleanup.c:113-117), so the cells assert that no side reported
//! the timeout, not only the exit code.
//!
//! The receiver's report is the codec's own line, then log_exit()'s trailer:
//! `rprintf(FERROR, ...)` before `exit_cleanup(RERR_STREAMIO)` (token.c:667,
//! token.c:902). The pull cells pin both lines and, when a 3.5.1 binary is
//! installed, compare them with 3.5.1 at both ends.
//!
//! On a push the dying server sends the client nothing but the end of its
//! stream: its receiver exits without flushing (cleanup.c:242-257) and its
//! generator's report goes to its own stderr. So the pushing client's code
//! depends only on what it meets first - the end of the stream while it waits
//! for input (`whine_about_eof()`, 12) or a write that fails (io.c:961-967,
//! 10) - and rsync itself goes either way with timing. The push cells fix the
//! order with `order-rsh` and pin each.
//!
//! The oracle is 3.5.1, not 3.5.2: the cells need a peer that speaks zstd. The
//! interop job builds both with zstd (tools/ci/run_interop.sh), but a 3.5.2
//! built without libzstd refuses the cells' option set with `unknown compress
//! name: zstd`, and these cells were measured against such a build.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use test_support::{
    CliOutput, LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, UpstreamVersion, create_tempdir,
    require_binaries, require_upstream_rsync, workspace_bin_path,
};

/// The trampoline that fixes the order a dying server is met in.
const ORDER_RSH_BIN: &str = "order-rsh";

/// A 300 KB pseudo-random file, so the stream holds several compressed blocks.
///
/// Written once per root: a rewrite would change the mtime, whose nanoseconds
/// go on the wire as a varint (flist.c:907-908), and with them the byte count
/// the generator reports.
fn source(root: &Path) -> PathBuf {
    let src = root.join("src");
    if src.exists() {
        return src;
    }
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

/// A 16 MB pseudo-random file: more than a pushing client can hand a dying
/// server before the server is gone, so the client is still writing then.
fn large_source(root: &Path) -> PathBuf {
    let src = root.join("large");
    if src.exists() {
        return src;
    }
    fs::create_dir_all(&src).expect("mkdir large");
    let mut state: u32 = 0x9e37_79b9;
    let data: Vec<u8> = (0..16_000_000)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect();
    fs::write(src.join("f"), data).expect("write f");
    src
}

/// Writes an rsh wrapper named `name` that replaces the server argument
/// `from` with `to` and runs `program` with the environment in `env`.
///
/// The server's stderr goes to `<name>.stderr` beside the script, not to the
/// client's. A pull's server is still alive while the client reports, and its
/// own failed-write line could land between the client's cause line and its
/// trailer; with the streams apart the client's stderr holds its lines only.
fn rsh_script(root: &Path, name: &str, from: &str, to: &str, program: &Path, env: &str) -> PathBuf {
    let script = root.join(name);
    let server_stderr = root.join(format!("{name}.stderr"));
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nn=$#\nwhile [ $n -gt 0 ]; do\n  a=$1; shift; n=$((n-1))\n  \
             [ \"$a\" = '{from}' ] && a='{to}'\n  set -- \"$@\" \"$a\"\ndone\n\
             exec env {env} '{}' \"$@\" 2>>'{}'\n",
            program.display(),
            server_stderr.display()
        ),
    )
    .expect("write rsh");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    script
}

/// Writes an rsh wrapper that replaces the server argument `from` with `to`
/// and runs the lsh stub.
fn rewriting_rsh(root: &Path, from: &str, to: &str) -> PathBuf {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    rsh_script(root, "rsh", from, to, stub.path(), "")
}

/// An rsh wrapper that runs a zstd server through `order-rsh` in `order`.
fn ordered_rsh(root: &Path, order: &str) -> PathBuf {
    rsh_script(
        root,
        &format!("rsh-{order}"),
        "--old-compress",
        "--compress-choice=zstd",
        &workspace_bin_path(ORDER_RSH_BIN),
        &format!("OC_ORDER_RSH_ORDER={order}"),
    )
}

/// Runs `bin` at both ends over the 300 KB source; the destination is
/// recreated for each run.
fn run(bin: &Path, root: &Path, client_choice: &str, rsh: &Path, pull: bool) -> CliOutput {
    run_from(bin, root, &source(root), client_choice, rsh, pull)
}

/// [`run`] over the source directory `src`.
fn run_from(
    bin: &Path,
    root: &Path,
    src: &Path,
    client_choice: &str,
    rsh: &Path,
    pull: bool,
) -> CliOutput {
    let dest = root.join("dest");
    if dest.exists() {
        fs::remove_dir_all(&dest).expect("clear dest");
    }
    fs::create_dir_all(&dest).expect("mkdir dest");
    let runner = OcRsyncCliRunner::new()
        .binary(bin)
        .args(["-r", "-z", "--timeout=5"])
        .arg(format!("--compress-choice={client_choice}"))
        .arg(format!("--rsh={}", rsh.display()))
        .arg(format!("--rsync-path={}", bin.display()));
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

/// Asserts the run ended on the codec failure with exit 12, not by a peer
/// waiting out `--timeout` for input the client never closed.
fn assert_streamio_without_timeout(out: &CliOutput) {
    out.assert_exit(12);
    assert!(
        !out.stderr_contains("io timeout after"),
        "a side waited for input instead of seeing EOF, stderr:\n{}",
        out.stderr_str()
    );
}

/// The report `role` closes with: its cause line, then the trailer's message
/// with the source position and version left out. The cause keeps its program
/// prefix in upstream's spelling, since oc names itself `oc-rsync`.
///
/// The client's stderr carries no server lines (see [`rsh_script`]), so the
/// line before the trailer is always the client's own cause.
fn report(out: &CliOutput, role: &str) -> (String, String) {
    let stderr = out.stderr_str();
    let lines: Vec<&str> = stderr.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.contains(&format!("[{role}=")))
        .unwrap_or_else(|| panic!("no {role} trailer, stderr:\n{stderr}"));
    assert!(
        at > 0,
        "no cause line before the trailer, stderr:\n{stderr}"
    );
    let message = lines[at]
        .split_once(" error: ")
        .and_then(|(_, rest)| rest.split_once(" at "))
        .map(|(message, _)| message)
        .unwrap_or_else(|| panic!("malformed trailer: {}", lines[at]));
    let cause = lines[at - 1];
    let cause = cause.strip_prefix("oc-").unwrap_or(cause);
    (cause.to_owned(), message.to_owned())
}

/// Pulls with the client decompressing `client_choice` while the server,
/// told `server_option`, compresses with another codec. The client receiver
/// must exit 12 at once with `expected` as its cause line, the same report
/// 3.5.1 gives for the same desync.
fn assert_pull_desync_report(client_choice: &str, server_option: &str, expected: &str) {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    // upstream: options.c:3059-3064 server_options() - a zlib choice is sent
    // as --old-compress, any other codec as --compress-choice=NAME.
    let sent = if client_choice == "zlib" {
        "--old-compress".to_owned()
    } else {
        format!("--compress-choice={client_choice}")
    };
    let rsh = rewriting_rsh(tmp.path(), &sent, server_option);
    let oc = run(
        &test_support::oc_rsync_bin(),
        tmp.path(),
        client_choice,
        &rsh,
        true,
    );
    assert_streamio_without_timeout(&oc);
    let report = report(&oc, "receiver");
    assert_eq!(
        report,
        (
            expected.to_owned(),
            "error in rsync protocol data stream (code 12)".to_owned()
        )
    );
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) {
        let reference = run(upstream.binary(), tmp.path(), client_choice, &rsh, true);
        reference.assert_exit(12);
        assert_eq!(
            self::report(&reference, "receiver"),
            report,
            "oc pull against 3.5.1 pull"
        );
    }
}

/// The client receiver fails to decode the server's zlib stream as zstd.
///
/// upstream: token.c:899-903 - ZSTD_decompressStream() rejects the zlib
/// bytes as an unknown frame descriptor (ZSTD_error_prefix_unknown, 10).
#[test]
fn pull_zstd_client_of_a_zlib_stream_reports_the_zstd_failure() {
    assert_pull_desync_report(
        "zstd",
        "--old-compress",
        "ZSTD decomp returned -10 (0 bytes)",
    );
}

/// The client receiver fails to inflate the server's zstd stream.
///
/// upstream: token.c:664-668 - inflate() returns Z_DATA_ERROR (-3) before
/// producing any output.
#[test]
fn pull_zlib_client_of_a_zstd_stream_reports_the_inflate_failure() {
    assert_pull_desync_report(
        "zlib",
        "--compress-choice=zstd",
        "inflate returned -3 (0 bytes)",
    );
}

/// A push client still writing when the dying server's stream ends: its next
/// write fails, and it reports that write and exits `RERR_SOCKETIO`.
///
/// upstream: io.c:961-967 perform_io() - `rsyserr(FERROR_SOCKET, errno,
/// "write error")`, then `exit_cleanup(RERR_SOCKETIO)`. While it writes,
/// perform_io() only marks the end of the input (io.c:905-911); the drain of
/// io.c:966 finds no message, since the server sent none.
#[test]
fn push_whose_write_fails_first_reports_the_write_error() {
    require_binaries!("oc-rsync", ORDER_RSH_BIN);
    let tmp = create_tempdir();
    let src = large_source(tmp.path());
    let rsh = ordered_rsh(tmp.path(), "write-first");
    let ours = run_from(
        &test_support::oc_rsync_bin(),
        tmp.path(),
        &src,
        "zlib",
        &rsh,
        false,
    );
    ours.assert_exit(10);
    let (cause, message) = report(&ours, "sender");
    assert_eq!(cause, "rsync: [sender] write error: Broken pipe (32)");
    assert_eq!(message, "error in socket IO (code 10)");
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) {
        let reference = run_from(upstream.binary(), tmp.path(), &src, "zlib", &rsh, false);
        reference.assert_exit(10);
        assert_eq!(
            report(&reference, "sender"),
            (cause, message),
            "oc push against 3.5.1 push"
        );
    }
}

/// A push client whose writes never fail meets the end of the dying server's
/// stream when it next waits for input, and exits `RERR_STREAMIO`.
///
/// upstream: io.c:282-304 whine_about_eof() - `connection unexpectedly closed
/// (N bytes received so far)`, then `exit_cleanup(RERR_STREAMIO)`.
#[test]
fn push_that_meets_the_end_of_the_stream_first_reports_it() {
    require_binaries!("oc-rsync", ORDER_RSH_BIN);
    let tmp = create_tempdir();
    let src = large_source(tmp.path());
    let rsh = ordered_rsh(tmp.path(), "eof-first");
    let ours = run_from(
        &test_support::oc_rsync_bin(),
        tmp.path(),
        &src,
        "zlib",
        &rsh,
        false,
    );
    assert_streamio_without_timeout(&ours);
    let (cause, message) = report(&ours, "sender");
    assert!(
        cause.starts_with("rsync: connection unexpectedly closed (")
            && cause.ends_with(" bytes received so far) [sender]"),
        "not an end-of-stream cause: {cause}"
    );
    assert_eq!(message, "error in rsync protocol data stream (code 12)");
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) {
        let reference = run_from(upstream.binary(), tmp.path(), &src, "zlib", &rsh, false);
        reference.assert_exit(12);
        // The byte count is compared too: both servers run with --timeout
        // flush their first entry's record on its own (generator.c:2904-2913,
        // io.c:1670-1672), so the client has received the same frames.
        assert_eq!(
            report(&reference, "sender"),
            (cause, message),
            "oc push against 3.5.1 push"
        );
    }
}
