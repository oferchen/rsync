//! The remote-shell child must see sockets on its stdin and stdout.
//!
//! upstream: pipe.c:57 `piped_child()` wires the child through two
//! `fd_pair()`s, and util1.c:84 `fd_pair()` is `socketpair(AF_UNIX,
//! SOCK_STREAM, 0)`. A regression to anonymous pipes would still move bytes,
//! so only an assertion made from inside the child - where `/proc/self/fd`
//! names the descriptor type - can tell the two apart.

#![cfg(target_os = "linux")]

use std::ffi::OsString;
use std::io::{Read, Write};

use rsync_io::SshCommand;
use rsync_io::ssh::WirePrimitive;

/// Builds an `SshCommand` that runs `sh -c <script>` with no ssh target.
fn shell(script: &str) -> SshCommand {
    let mut builder = SshCommand::new("");
    builder
        .set_program("/bin/sh")
        .set_remote_command([OsString::from("-c"), OsString::from(script)]);
    builder
}

#[test]
fn child_stdin_and_stdout_are_sockets() {
    let connection =
        shell("for fd in 0 1; do [ -S /proc/self/fd/$fd ] && printf 'sock%s ' $fd; done")
            .spawn()
            .expect("spawn /bin/sh child");
    assert_eq!(connection.wire_primitive(), WirePrimitive::Socketpair);

    let (mut reader, writer, handle) = connection.split().expect("split");
    drop(writer);
    let mut seen = String::new();
    reader.read_to_string(&mut seen).expect("read child report");
    let (status, _) = handle.wait_with_stderr().expect("reap child");
    assert!(status.success());
    assert_eq!(seen, "sock0 sock1 ", "child stdio must be socketpair ends");
}

/// The split halves - io_uring-driven where the kernel offers it - carry a
/// multi-MiB stream through the child and back without loss or reordering.
///
/// `cat` does not retry `EAGAIN`, so like any such `-e` program under
/// upstream it needs `--blocking-io` to keep its stdout blocking.
#[test]
fn split_halves_round_trip_through_the_child() {
    let mut builder = shell("exec cat");
    builder.set_blocking_io(Some(true));
    let connection = builder.spawn().expect("spawn cat child");
    let (mut reader, mut writer, handle) = connection.split().expect("split");

    let payload: Vec<u8> = (0..4 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let expected = payload.clone();
    let feeder = std::thread::spawn(move || {
        writer.write_all(&payload).expect("write payload");
        writer.close().expect("close stdin");
    });

    let mut echoed = Vec::with_capacity(expected.len());
    reader.read_to_end(&mut echoed).expect("read echo");
    feeder.join().expect("feeder thread");
    let (status, _) = handle.wait_with_stderr().expect("reap child");
    assert!(status.success());
    assert_eq!(echoed.len(), expected.len());
    assert!(echoed == expected, "echoed stream differs from the payload");
}
