//! A daemon receiver anchors attribute applies on the module root, never on
//! the client-supplied destination path.
//!
//! The daemon's operator named only the module; every component below it
//! comes from the peer. Upstream confines that tail (`clientserver.c:1093`
//! `use_secure_symlinks = am_daemon && ...`), and its `chdir-symlink-race`
//! cell plants `<module>/subdir -> <outside>` and pushes a single file to
//! `subdir/target.txt` with `-tp --size-only`: no data moves, only the
//! attribute apply runs, and the outside file's mode must not change.
//!
//! Treating the destination operand as operator-trusted let that apply follow
//! the planted link and chmod the outside file from 600 to 666 wherever no
//! kernel sandbox (Landlock) stood in the way, e.g. on macOS, where the
//! destination anchor has no `RESOLVE_BENEATH` walk to refuse the link first.
//!
//! Skip conditions (the test passes with a printed reason): loopback TCP is
//! unavailable, or the test runs as root, where the daemon drops to `nobody`
//! and cannot reach the private temp tree.
#![cfg(unix)]

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use test_support::ReapOnDrop;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// Starts a daemon serving `module` read-write as `m` and waits for its port.
fn spawn_daemon(root: &Path, port: u16, module: &Path) -> ReapOnDrop {
    let conf = root.join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "port = {port}\n\
             use chroot = no\n\
             log file = {log}\n\
             reverse lookup = no\n\
             \n\
             [m]\n\
             \tpath = {module}\n\
             \tread only = no\n",
            log = root.join("daemon.log").display(),
            module = module.display(),
        ),
    )
    .expect("write config");
    let child = ReapOnDrop::new(
        Command::new(oc_binary())
            .arg("--daemon")
            .arg("--no-detach")
            .arg(format!("--config={}", conf.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && TcpStream::connect(("127.0.0.1", port)).is_err() {
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

#[test]
fn a_symlinked_tail_component_cannot_redirect_the_attribute_apply() {
    // SAFETY: geteuid() has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        println!("SKIP: running as root, the daemon drops to nobody");
        return;
    }
    let Some(port) = free_port() else {
        println!("SKIP: no loopback port available");
        return;
    };
    let temp = tempfile::tempdir().expect("tempdir");
    let root = fs::canonicalize(temp.path()).expect("canonicalize tempdir");
    let module = root.join("module");
    let outside = root.join("outside");
    fs::create_dir(&module).expect("create module dir");
    fs::create_dir(&outside).expect("create outside dir");
    symlink(&outside, module.join("subdir")).expect("plant symlink");

    // Same size on both sides, so --size-only skips the data and only the
    // attribute apply touches the destination.
    let victim = outside.join("target.txt");
    fs::write(&victim, b"outside\n").expect("write outside file");
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o600)).expect("chmod victim");
    let src = root.join("src.txt");
    fs::write(&src, b"payload\n").expect("write source");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o666)).expect("chmod source");

    let _daemon = spawn_daemon(&root, port, &module);
    let out = Command::new(oc_binary())
        .args(["-tp", "--size-only"])
        .arg(&src)
        .arg(format!("rsync://127.0.0.1:{port}/m/subdir/target.txt"))
        .stdin(Stdio::null())
        .output()
        .expect("run push client");

    let mode = fs::metadata(&victim)
        .expect("stat victim")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(
        mode,
        0o600,
        "the outside file's mode must not change through the planted link\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
