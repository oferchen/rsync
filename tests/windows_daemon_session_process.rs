//! A Windows daemon serves each connection in its own session process.
//!
//! upstream: socket.c:755-780 - `start_accept_loop()` forks a child for every
//! accepted connection. Windows has no `fork`, so the daemon starts
//! `oc-rsync --daemon-session` and hands it the connection; these tests pin
//! that the session really runs in that child, and that the child refuses to
//! run without a hand-off.

use std::process::{Command, Stdio};

/// `--daemon-session` is only ever started by a daemon, with the hand-off on a
/// pipe. Started by hand with any other standard input it must fail closed,
/// never wait for or act on whatever it was given.
#[cfg(windows)]
#[test]
fn a_session_child_without_a_handoff_pipe_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let not_a_pipe = std::fs::File::create(dir.path().join("stdin")).expect("stdin file");
    let output = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
        .arg("--daemon-session")
        .stdin(not_a_pipe)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run oc-rsync");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // RERR_IPC: the hand-off is inter-process communication that failed.
    assert_eq!(output.status.code(), Some(14), "stderr: {stderr}");
    assert!(
        stderr.contains("could not create child server process"),
        "stderr: {stderr}"
    );
}

/// A Unix daemon forks its session children and never starts this mode.
#[cfg(unix)]
#[test]
fn the_session_mode_is_refused_outside_windows() {
    let output = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
        .arg("--daemon-session")
        .stdin(Stdio::null())
        .output()
        .expect("run oc-rsync");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("--daemon-session is internal to the Windows daemon"),
        "stderr: {stderr}"
    );
}

/// The daemon serves a module listing through a session process: the
/// connection is answered, and the `connect from` line is logged by a process
/// other than the one running the accept loop.
#[cfg(windows)]
#[test]
fn each_connection_is_served_by_its_own_session_process() {
    use std::ffi::OsString;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    let dir = tempfile::tempdir().expect("tempdir");
    let module = dir.path().join("module");
    std::fs::create_dir(&module).expect("module dir");
    let log = dir.path().join("daemon.log");
    let conf = dir.path().join("rsyncd.conf");
    std::fs::write(
        &conf,
        format!(
            "log file = {}\n[files]\npath = {}\nuse chroot = false\n",
            log.display(),
            module.display()
        ),
    )
    .expect("write config");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let config = daemon::DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            conf.clone().into_os_string(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
            OsString::from("--no-detach"),
        ])
        .pre_bound_listener(listener)
        .session_executable(env!("CARGO_BIN_EXE_oc-rsync"))
        .build();
    let daemon = std::thread::spawn(move || daemon::run_daemon(config));

    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(line.starts_with("@RSYNCD: "), "greeting: {line:?}");
    // Protocol 30 needs no digest list in the client greeting.
    writer
        .write_all(b"@RSYNCD: 30.0\n#list\n")
        .expect("request listing");
    let mut listing = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).expect("listing line") == 0 {
            break;
        }
        listing.push_str(&line);
        if line.starts_with("@RSYNCD: EXIT") {
            break;
        }
    }
    assert!(listing.contains("files"), "listing: {listing:?}");
    daemon
        .join()
        .expect("daemon thread")
        .expect("daemon exits cleanly after --once");

    let text = std::fs::read_to_string(&log).expect("read log");
    let connect = text
        .lines()
        .find(|line| line.contains("connect from"))
        .unwrap_or_else(|| panic!("no connect line in log:\n{text}"));
    let pid: u32 = connect
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .and_then(|(pid, _)| pid.parse().ok())
        .unwrap_or_else(|| panic!("no pid in {connect:?}"));
    assert_ne!(
        pid,
        std::process::id(),
        "the session must run in its own process, not the accept loop's"
    );
}

/// A module with `use chroot` left unset is served from the inner path of its
/// `/./` split, anchored below the outer one. Under Cygwin the
/// `chroot("/")` probe succeeds, so the chroot applies, and Cygwin's chroot is
/// a path prefix rather than a jail (syscalls.cc:3836-3859). A file beside the
/// inner directory must therefore not be served, and neither may anything at
/// the drive root that the post-chroot "/" would name natively.
///
/// upstream: clientserver.c:883-918 `rsync_module()`.
#[cfg(windows)]
#[test]
fn an_unset_use_chroot_serves_the_inner_path_below_the_outer_one() {
    use std::ffi::OsString;
    use std::net::TcpListener;

    let dir = tempfile::tempdir().expect("tempdir");
    let outer = dir.path().join("outer");
    let inner = outer.join("inner");
    std::fs::create_dir_all(&inner).expect("module dirs");
    std::fs::write(outer.join("outside.txt"), b"outer").expect("outer file");
    std::fs::write(inner.join("inside.txt"), b"inner").expect("inner file");
    let conf = dir.path().join("rsyncd.conf");
    std::fs::write(
        &conf,
        format!("[files]\npath = {}/./inner\n", outer.display()),
    )
    .expect("write config");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let config = daemon::DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            conf.into_os_string(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
            OsString::from("--no-detach"),
        ])
        .pre_bound_listener(listener)
        .session_executable(env!("CARGO_BIN_EXE_oc-rsync"))
        .build();
    let daemon = std::thread::spawn(move || daemon::run_daemon(config));

    let dest = dir.path().join("dest");
    let output = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
        .arg("-r")
        .arg(format!("rsync://127.0.0.1:{port}/files/"))
        .arg(&dest)
        .stdin(Stdio::null())
        .output()
        .expect("run client");
    assert!(
        output.status.success(),
        "pull failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    daemon
        .join()
        .expect("daemon thread")
        .expect("daemon exits cleanly after --once");

    assert_eq!(
        std::fs::read(dest.join("inside.txt")).expect("inner file pulled"),
        b"inner"
    );
    assert!(
        !dest.join("outside.txt").exists(),
        "the outer directory must not be served"
    );
}
