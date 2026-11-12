//! `TestDaemon` serves every connection, and a daemon that dies before its
//! greeting fails the start once, loudly, with its stderr.
//!
//! The harness owns the listener, so a client never races the daemon for a
//! port. The first test drives the forked oc-rsync daemon; the second drives
//! the inetd path that upstream binaries take, with a stand-in binary that
//! dies at once.

#![cfg(unix)]

mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;

use common::{DaemonBinary, TestDaemon};

/// Reads the `@RSYNCD:` greeting of one new connection to `daemon`.
fn greeting(daemon: &TestDaemon) -> String {
    let stream = TcpStream::connect(("127.0.0.1", daemon.port())).expect("connect to daemon");
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .expect("read greeting");
    line
}

#[test]
fn every_connection_is_served_by_its_own_session() {
    let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("daemon should start");
    for _ in 0..2 {
        let line = greeting(&daemon);
        assert!(
            line.starts_with("@RSYNCD: "),
            "each connection must get a daemon greeting, got {line:?}"
        );
    }
}

#[test]
fn a_daemon_that_dies_before_its_greeting_fails_the_start_once_with_its_stderr() {
    let scratch = test_support::create_tempdir();
    let starts = scratch.path().join("starts");
    let script = scratch.path().join("dying-daemon");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho start >> '{}'\necho 'refusing to serve' >&2\nexit 1\n",
            starts.display()
        ),
    )
    .expect("write dying daemon");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it executable");
    let script: &'static str = Box::leak(script.display().to_string().into_boxed_str());

    let error = match TestDaemon::start(DaemonBinary::Upstream(script)) {
        Ok(_) => panic!("a daemon that exits before its greeting must fail the start"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    assert!(
        error.to_string().contains("refusing to serve"),
        "the failure must carry the daemon's stderr: {error}"
    );
    assert_eq!(
        fs::read_to_string(&starts).expect("read start count"),
        "start\n",
        "a failed start must not be retried"
    );
}
