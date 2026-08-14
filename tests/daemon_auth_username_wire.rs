//! The username a client puts on the wire in its `@RSYNCD` auth response.
//!
//! Upstream resolves it in two stages, and the daemon matches the result
//! against `auth users`, so every stage is observable and wire-affecting:
//!
//! ```c
//! /* clientserver.c:289-292, start_inband_exchange() */
//! if (!user)
//!     user = getenv("USER");
//! if (!user)
//!     user = getenv("LOGNAME");
//! /* authenticate.c:451-452 + :473, auth_client() */
//! if (!user || !*user)
//!     user = "nobody";
//! io_printf(fd, "%s %s\n", user, pass2);
//! ```
//!
//! oc consulted `USERNAME` (a Windows convention upstream never reads) instead
//! of `LOGNAME`, and fell back to `rsync` instead of `nobody`. On a host that
//! sets only `LOGNAME` - a plain `su`, cron, or a login shell that does not
//! export `USER` - `auth users = alice` therefore rejected a correctly
//! configured client.
//!
//! The daemon side here is a stub: it speaks just enough of the greeting to
//! reach `AUTHREQD` and then captures the response line, so the assertion is on
//! the actual bytes rather than on a success/failure proxy. The environment is
//! set on the CHILD only - never on the test process - so these cases stay
//! correct under parallel nextest.
//!
//! Skip conditions (test passes with a printed reason):
//! - Loopback TCP is unavailable.
//! - The cross-implementation cell additionally needs a built upstream 3.5.0
//!   binary; without it that cell reports why it did not run.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Locates the built upstream 3.5.0 oracle, or `None` when the interop tree has
/// not been fetched and built.
fn upstream_binary() -> Option<PathBuf> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/interop/upstream-src/rsync-3.5.0/rsync");
    path.is_file().then_some(path)
}

/// Serves one stub daemon session and returns the username field of the auth
/// response the client sent.
fn capture_auth_username(client: &Path, env: &[(&str, &str)]) -> Option<String> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();

    let server = thread::spawn(move || -> Option<String> {
        let (stream, _) = listener.accept().ok()?;
        let mut writer = stream.try_clone().ok()?;
        let mut reader = BufReader::new(stream);
        writer.write_all(b"@RSYNCD: 32.0 md5 md4\n").ok()?;
        writer.flush().ok()?;

        let mut line = String::new();
        reader.read_line(&mut line).ok()?; // client greeting
        line.clear();
        reader.read_line(&mut line).ok()?; // module request

        writer
            .write_all(b"@RSYNCD: AUTHREQD 0123456789abcdef\n")
            .ok()?;
        writer.flush().ok()?;

        line.clear();
        reader.read_line(&mut line).ok()?; // "<user> <hash>"
        let _ = writer.write_all(b"@ERROR: probe complete\n");

        line.split_whitespace().next().map(str::to_owned)
    });

    let mut command = Command::new(client);
    command
        .args(["-q", &format!("rsync://127.0.0.1:{port}/m/f"), "/dev/null"])
        .env_remove("USER")
        .env_remove("LOGNAME")
        .env_remove("USERNAME")
        // A password source keeps the client from blocking on a prompt; the
        // stub never checks it.
        .env("RSYNC_PASSWORD", "probe")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    let _ = command.status();

    server.join().ok().flatten()
}

/// Runs the six-row table against `client`.
///
/// An empty auth line would mean the client never authenticated, so every row
/// asserts a concrete username rather than merely "not the old value".
fn assert_table(client: &Path, label: &str) {
    let rows: [(&[(&str, &str)], &str); 6] = [
        (&[("USER", "alice")], "alice"),
        (&[("LOGNAME", "bob")], "bob"),
        (&[("USER", "alice"), ("LOGNAME", "bob")], "alice"),
        (&[("USER", ""), ("LOGNAME", "bob")], "nobody"),
        (&[], "nobody"),
        (&[("USERNAME", "win")], "nobody"),
    ];

    for (env, expected) in rows {
        let Some(sent) = capture_auth_username(client, env) else {
            println!("SKIP: loopback TCP unavailable");
            return;
        };
        assert_eq!(
            sent, expected,
            "{label}: env {env:?} must authenticate as {expected:?}"
        );
    }
}

/// The headline case and its five siblings, measured on oc.
///
/// `LOGNAME`-only and the two unset rows are the ones that were wrong; the
/// `USER` rows are the over-correction guard - they passed before the fix and
/// must keep passing, so a change that simply always sent `nobody` fails here.
#[test]
fn oc_sends_the_upstream_username_for_every_environment() {
    assert_table(&oc_binary(), "oc");
}

/// CROSS-IMPLEMENTATION: the real 3.5.0 client is the source of the expected
/// column, so the same table is asserted against it directly. If upstream ever
/// changed this resolution, this cell would fail rather than oc silently
/// encoding a stale rule.
#[test]
fn upstream_sends_the_same_username_for_every_environment() {
    let Some(upstream) = upstream_binary() else {
        println!(
            "SKIP: upstream 3.5.0 oracle not built \
             (target/interop/upstream-src/rsync-3.5.0/rsync)"
        );
        return;
    };
    assert_table(&upstream, "upstream 3.5.0");
}
