//! The remote-shell server sets its inherited stderr blocking after setup.
//!
//! upstream: main.c:1356-1364 `start_server()` calls `set_blocking(STDERR)`
//! once `setup_protocol()` is done, because ssh may have made fd 2
//! non-blocking. `O_NONBLOCK` lives on the open file description, which the
//! client, the remote shell and the server all share here, so the client's
//! stderr must come out of the transfer blocking again.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// `CARGO_BIN_EXE_oc-rsync` is compile-time, so `env!` pins the build under test.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}",
        built.display()
    );
    built
}

fn nonblocking(fd: RawFd) -> bool {
    // SAFETY: F_GETFL reads the flags of a descriptor this test owns.
    #[allow(unsafe_code)]
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(
        flags >= 0,
        "fcntl(F_GETFL): {}",
        std::io::Error::last_os_error()
    );
    flags & libc::O_NONBLOCK != 0
}

fn set_nonblocking(fd: RawFd) {
    // SAFETY: F_GETFL/F_SETFL on a descriptor this test owns; only O_NONBLOCK changes.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::fcntl(
            fd,
            libc::F_SETFL,
            libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
        )
    };
    assert_eq!(rc, 0, "fcntl(F_SETFL): {}", std::io::Error::last_os_error());
}

#[test]
fn remote_shell_server_leaves_shared_stderr_blocking() {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&dest).expect("mkdir dest");
    fs::write(src.join("file"), b"payload").expect("write source");

    // A remote shell that drops the host token and runs the server locally.
    let rsh = temp.path().join("fake-rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");

    let (mut reader, writer) = std::io::pipe().expect("stderr pipe");
    let probe = writer.try_clone().expect("dup stderr write end");
    set_nonblocking(probe.as_raw_fd());
    assert!(
        nonblocking(probe.as_raw_fd()),
        "precondition: stderr starts non-blocking"
    );

    let mut child = Command::new(oc_rsync_binary())
        .arg("-r")
        .arg(format!("-e{}", rsh.display()))
        .arg(format!("--rsync-path={}", oc_rsync_binary().display()))
        .arg(format!("localhost:{}/", src.display()))
        .arg(format!("{}/", dest.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(writer)
        .spawn()
        .expect("spawn oc-rsync");
    let drain = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = reader.read_to_end(&mut text);
        text
    });
    let status = child.wait().expect("wait oc-rsync");
    let blocking_after = !nonblocking(probe.as_raw_fd());
    drop(probe);
    let stderr = drain.join().expect("stderr drain");

    assert!(
        status.success(),
        "pull failed: {status:?}: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(fs::read(dest.join("file")).expect("read dest"), b"payload");
    assert!(
        blocking_after,
        "the server must set the shared stderr blocking after setup (main.c:1364)"
    );
}
